use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
//use tracing::{info};
//use bitcoin_hashes::{sha256d, Hash};
use bitcoincore_rpc::RpcApi;
//use bitcoin::Script;
use bitcoin_hashes::Hash as _;

use crate::{node::Node, storage::Storage};

pub const EXTRANONCE1_LEN: usize = 4;
pub const EXTRANONCE2_LEN: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StratumJob {
    pub job_id: String,
    pub prevhash: String,
    pub prevhash_be: String,
    pub coinbase1: String,
    pub coinbase2: String,
    pub merkle_branch: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub clean_jobs: bool,
    pub coinbase_value: u64,
    pub extranonce1: String,
    pub witness_commitment: Option<String>,
    pub coinbasetxn_hex: String,
    pub txs_hex: Vec<String>,
    pub ex1_len: usize,
    pub ex2_len: usize,
    pub ex1_hex: String,
}

#[derive(Clone)]
pub struct JobManager {
    pub node: Arc<Node>,
    #[allow(dead_code)]
    pub storage: Storage,
    pub current_job: Arc<RwLock<Option<StratumJob>>>,
    pub sse_tx: tokio::sync::broadcast::Sender<String>,
    pub stratum_tx: tokio::sync::broadcast::Sender<String>,
    pub last_publish: Arc<RwLock<std::time::Instant>>,   // ✅ new
}

impl JobManager {
    pub fn new(
        node: Arc<Node>,
        storage: Storage,
        sse_tx: tokio::sync::broadcast::Sender<String>,
        stratum_tx: tokio::sync::broadcast::Sender<String>,
    ) -> Self {
        Self {
            node,
            storage,
            current_job: Arc::new(RwLock::new(None)),
            sse_tx,
            stratum_tx,
            last_publish: Arc::new(RwLock::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            )),
        }
    }

    pub async fn get_current_job(&self) -> Option<StratumJob> {
        let guard = self.current_job.read().await;
        guard.clone()
    }

    pub async fn run_block_tx_watcher(&self) -> Result<()> {
        let me = self.clone();
        self.node
            .run_zmq_watch(move |_| {
                let me = me.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.refresh_job().await {
                        tracing::warn!("refresh_job failed: {e:?}");
                    }
                });
            })
            .await?;
        Ok(())
    }

    pub async fn submit_from_header_and_template(
        &self,
        header_bytes: Vec<u8>,
        coinbase_bytes: Vec<u8>,
        job: &StratumJob,
    ) -> anyhow::Result<()> {
        use bitcoin::{
            block::Header,
            consensus::encode::deserialize,
            transaction::Transaction,
            Block,
        };

        // Decode the typed header and coinbase tx
        let header: Header = deserialize(&header_bytes).context("deserialize header")?;
        let coinbase: Transaction = deserialize(&coinbase_bytes).context("deserialize coinbase")?;

        // Decode the rest of the transactions from the job/template
        let mut txdata: Vec<Transaction> = Vec::with_capacity(1 + job.txs_hex.len());
        txdata.push(coinbase);
        for hex_tx in &job.txs_hex {
            let raw = hex::decode(hex_tx).context("decode tx hex")?;
            let tx: Transaction = deserialize(&raw).context("deserialize tx")?;
            txdata.push(tx);
        }

        // Build full block and submit to bitcoind
        let block = Block { header, txdata };
        self.node.rpc.submit_block(&block).context("submitblock")?;
        Ok(())
    }

    pub async fn refresh_job(&self) -> Result<()> {
        use bitcoin::{
            absolute::LockTime,
            amount::Amount,
            opcodes::all::OP_PUSHBYTES_0,
            transaction::{Transaction, TxIn, TxOut, Version as TxVersion},
            OutPoint, ScriptBuf, Sequence, Witness,
        };

        // 1) Fetch template
        let gbt = self.node.get_block_template_raw().await.context("GBT")?;

        {
            let mut lp = self.last_publish.write().await;
            let now = std::time::Instant::now();
            if now.duration_since(*lp) < std::time::Duration::from_millis(200) {
                // Skip refresh if a job was just published very recently
                return Ok(());
            }
            *lp = now;
        }

        // 2) Extract basics
        let height_u32   = gbt.height as u32;
        let prevhash_be  = gbt.previous_block_hash.to_string();
        let version_hex  = format!("{:08x}", gbt.version as u32);
        let bits_hex     = hex::encode(&gbt.bits);
        let ntime_hex    = format!("{:08x}", gbt.current_time as u32);
        let reward_sat   = gbt.coinbase_value.to_sat();

        // 3) Non-coinbase txs (hex)
        let mut txs_hex = Vec::with_capacity(gbt.transactions.len());
        for t in &gbt.transactions {
            txs_hex.push(hex::encode(&t.raw_tx));
        }

        // --- DE-DUPE GUARD ---
        // If nothing changed (same tip and same tx count), skip republishing.
        if let Some(cur) = self.current_job.read().await.as_ref() {
            if cur.prevhash_be == prevhash_be && cur.txs_hex.len() == txs_hex.len() {
                // same tip & same number of txs → don't spam miners
                return Ok(());
            }
        }

        // 4) Build a coinbase template with ex1 + placeholder ex2 (so miners can rebuild)
        //    ex1 is per-job (we also expose it to clients via subscribe result).
        let ex1 = rand::random::<[u8; EXTRANONCE1_LEN]>();
        let ex1_hex = hex::encode(ex1);
        let ex2_placeholder = [0u8; EXTRANONCE2_LEN];

        // scriptSig: BIP34-height || ex1 || <ex2 placeholder>
        let scriptsig = ScriptBuf::builder()
            .push_int(height_u32 as i64)
            .push_slice(&ex1)
            .push_slice(&ex2_placeholder)
            .into_script();

        // Payout script (dummy anyone-can-spend OP_0 for regtest; replace later)
        let payout = ScriptBuf::builder()
            .push_opcode(OP_PUSHBYTES_0)
            .into_script();

        let coinbase = Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: scriptsig.clone(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(reward_sat),
                script_pubkey: payout,
            }],
        };

        // 5) Find the scriptSig in the serialized coinbase to split coinbase1/coinbase2
        use bitcoin::consensus::Encodable;
        let mut cb_ser = Vec::with_capacity(128);
        coinbase.consensus_encode(&mut cb_ser).expect("encode coinbase");

        let ss_bytes = scriptsig.as_bytes();
        let ss_pos = cb_ser
            .windows(ss_bytes.len())
            .position(|w| w == ss_bytes)
            .context("scriptSig not found in serialized coinbase")?;

        // length of just the height push (to locate where ex2 starts inside scriptSig)
        let height_only = ScriptBuf::builder()
            .push_int(height_u32 as i64)
            .into_script();
        let height_len = height_only.as_bytes().len();

        let ex2_start_in_ss = height_len + EXTRANONCE1_LEN;              // after height + ex1
        let ex2_abs_start   = ss_pos + ex2_start_in_ss;                   // absolute offset in tx
        let coinbase1       = hex::encode(&cb_ser[..ex2_abs_start]);      // up to ex2 start
        let coinbase2       = hex::encode(&cb_ser[ex2_abs_start + EXTRANONCE2_LEN..]); // after ex2

        // 6) Convert witness commitment (ScriptBuf) → Option<String> (hex) for UI/debug
        let witness_commitment_hex: Option<String> = {
            // Your GetBlockTemplateResult exposes `default_witness_commitment: ScriptBuf` (not Option).
            // Convert it to hex; treat empty script as None.
            let bytes = gbt.default_witness_commitment.as_bytes();
            if bytes.is_empty() {
                None
            } else {
                Some(hex::encode(bytes))
            }
        };

        // 7) Build job
        let job = StratumJob {
            job_id: format!("{:x}", rand::random::<u64>()),
            prevhash: prevhash_be.clone(),   // (display)
            prevhash_be: prevhash_be.clone(),// (typed)
            coinbase1,
            coinbase2,
            merkle_branch: vec![],           // we submit full tx list
            version: version_hex,
            nbits: bits_hex,
            ntime: ntime_hex,
            clean_jobs: true,
            coinbase_value: reward_sat,
            extranonce1: String::new(),      // informational; we publish ex1_hex separately
            witness_commitment: witness_commitment_hex,
            coinbasetxn_hex: String::new(),  // not used in this sliced mode
            txs_hex,
            ex1_len: EXTRANONCE1_LEN,
            ex2_len: EXTRANONCE2_LEN,
            ex1_hex,
        };

        // 8) Swap current job
        {
            let mut guard = self.current_job.write().await;
            *guard = Some(job.clone());
        }

        // 9) Notify web UI
        let _ = self.sse_tx.send(
            serde_json::json!({
                "type":"job",
                "prevhash": job.prevhash,
                "job_id": job.job_id
            }).to_string()
        );

        // 10) Broadcast mining.notify to all stratum clients
        let notify = serde_json::json!({
            "id": null,
            "method": "mining.notify",
            "params": [
                job.job_id,
                job.prevhash_be,  // big-endian hex
                job.coinbase1,
                job.coinbase2,
                job.txs_hex,      // full tx list (non-coinbase)
                job.version,
                job.nbits,
                job.ntime,
                true              // clean_jobs
            ]
        });
        let _ = self.stratum_tx.send(notify.to_string());

        tracing::info!("New job – prevhash {}", job.prevhash);
        Ok(())
    }
}

// Expose to other modules (stratum.rs)
pub(crate) fn merkle_root_from_txids(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let mut layer = leaves.to_vec();
    while layer.len() > 1 {
        let mut next = Vec::with_capacity((layer.len() + 1) / 2);
        let mut i = 0;
        while i < layer.len() {
            let a = layer[i];
            let b = if i + 1 < layer.len() { layer[i + 1] } else { layer[i] };
            let mut both = Vec::with_capacity(64);
            both.extend_from_slice(&a);
            both.extend_from_slice(&b);
            let h = bitcoin_hashes::sha256d::Hash::hash(&both).to_byte_array();
            next.push(h);
            i += 2;
        }
        layer = next;
    }
    layer[0]
}

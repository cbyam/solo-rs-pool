use crate::job::merkle_root_from_txids;
use anyhow::{Context, Result};
use bitcoin::{
    block::{Header, Version},
    consensus::{deserialize, Encodable},
    hash_types::{BlockHash, TxMerkleNode},
    pow, Transaction,
};
use bitcoin_hashes::{sha256d, Hash};
use bitcoincore_rpc::RpcApi;
use serde_json::{json, Value};
use std::str::FromStr;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::{broadcast, Mutex},
};
use tracing::{info, warn};

use crate::{job::JobManager, storage::Storage};

#[derive(Clone)]
pub struct StratumServer {
    jobman: Arc<JobManager>,
    storage: Storage,
    #[allow(dead_code)]
    sse_tx: broadcast::Sender<String>,
    stratum_tx: broadcast::Sender<String>,
}

impl StratumServer {
    pub fn new(
        jobman: Arc<JobManager>,
        storage: Storage,
        sse_tx: broadcast::Sender<String>,
        stratum_tx: broadcast::Sender<String>,
    ) -> Self {
        Self {
            jobman,
            storage,
            sse_tx,
            stratum_tx,
        }
    }

    pub async fn run(&self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await.context("bind")?;
        loop {
            let (socket, peer) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.handle_client(socket, peer).await {
                    warn!("client {peer} error: {e:#}");
                }
            });
        }
    }

    pub async fn handle_client(
        &self,
        socket: tokio::net::TcpStream,
        peer: SocketAddr,
    ) -> Result<()> {
        info!("Stratum client {peer}");
        let (r, w) = socket.into_split();
        let mut r = BufReader::new(r).lines();

        let writer = Arc::new(Mutex::new(w));

        // Forward mining.notify from ZMQ
        {
            let mut rx = self.stratum_tx.subscribe();
            let writer = writer.clone();
            tokio::spawn(async move {
                while let Ok(msg) = rx.recv().await {
                    let mut w = writer.lock().await;
                    let _ = w.write_all(format!("{msg}\n").as_bytes()).await;
                }
            });
        }

        while let Some(line) = r.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => {
                    let err = json!({"id":null,"error":{"code":-32700,"message":"parse error"},"result":null});
                    let mut w = writer.lock().await;
                    let _ = w.write_all(format!("{err}\n").as_bytes()).await;
                    continue;
                }
            };

            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

            match method {
                "mining.subscribe" => {
                    let job = self
                        .jobman
                        .get_current_job()
                        .await
                        .ok_or_else(|| anyhow::anyhow!("no current job"))?;

                    let res = json!({
                        "id": msg.get("id").cloned().unwrap_or(json!(1)),
                        "result": [
                            [["mining.set_difficulty", "1"], ["mining.notify", "2"]],
                            job.ex1_hex,
                            job.ex2_len
                        ],
                        "error": null
                    });

                    let mut w = writer.lock().await;
                    w.write_all(format!("{res}\n").as_bytes()).await?;

                    // Send difficulty
                    let diff = json!({"id":null,"method":"mining.set_difficulty","params":[1.0]});
                    w.write_all(format!("{diff}\n").as_bytes()).await?;

                    // Send initial job
                    let notify = json!({
                        "id": null,
                        "method": "mining.notify",
                        "params": [
                            job.job_id,
                            job.prevhash_be,
                            job.coinbase1,
                            job.coinbase2,
                            job.txs_hex,
                            job.version,
                            job.nbits,
                            job.ntime,
                            true
                        ]
                    });
                    w.write_all(format!("{notify}\n").as_bytes()).await?;
                }

                "mining.authorize" => {
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(2)), "result": true, "error": null});
                    let mut w = writer.lock().await;
                    w.write_all(format!("{res}\n").as_bytes()).await?;
                }

                "mining.submit" => {
                    let id_val = msg.get("id").cloned().unwrap_or(json!(3));
                    let params: Vec<Value> = msg
                        .get("params")
                        .and_then(|p| p.as_array())
                        .cloned()
                        .unwrap_or_default();

                    let user = params.first().and_then(|v| v.as_str()).unwrap_or("unknown");
                    let job_id = params.get(1).and_then(|v| v.as_str()).unwrap_or("");
                    let ex2_hex = params.get(2).and_then(|v| v.as_str()).unwrap_or("");
                    let ntime_hex = params.get(3).and_then(|v| v.as_str()).unwrap_or("");
                    let nonce_hex = params.get(4).and_then(|v| v.as_str()).unwrap_or("");

                    let Some(job) = self.jobman.get_current_job().await else {
                        let err = json!({"id":id_val,"result":false,"error":{"code":21,"message":"Stale job"}});
                        let mut w = writer.lock().await;
                        w.write_all(format!("{err}\n").as_bytes()).await?;
                        continue;
                    };
                    if job.job_id != job_id {
                        let err = json!({"id":id_val,"result":false,"error":{"code":21,"message":"Stale job"}});
                        let mut w = writer.lock().await;
                        w.write_all(format!("{err}\n").as_bytes()).await?;
                        continue;
                    }

                    let ex2 = match hex::decode(ex2_hex) {
                        Ok(b) if b.len() == job.ex2_len => b,
                        _ => {
                            let err = json!({"id":id_val,"result":false,"error":{"code":25,"message":"bad ex2 size"}});
                            let mut w = writer.lock().await;
                            w.write_all(format!("{err}\n").as_bytes()).await?;
                            continue;
                        }
                    };

                    let coinbase_hex =
                        format!("{}{}{}", job.coinbase1, hex::encode(&ex2), job.coinbase2);

                    let coinbase_bytes = match hex::decode(&coinbase_hex) {
                        Ok(b) => b,
                        Err(e) => {
                            warn!("bad coinbase hex: {e}");
                            let err = json!({"id":id_val,"result":false,"error":{"code":23,"message":"bad coinbase"}});
                            let mut w = writer.lock().await;
                            w.write_all(format!("{err}\n").as_bytes()).await?;
                            continue;
                        }
                    };

                    let coinbase_tx: Transaction = match deserialize(&coinbase_bytes) {
                        Ok(tx) => tx,
                        Err(e) => {
                            warn!("bad coinbase deserialize: {e}");
                            let err = json!({"id":id_val,"result":false,"error":{"code":23,"message":"bad coinbase"}});
                            let mut w = writer.lock().await;
                            w.write_all(format!("{err}\n").as_bytes()).await?;
                            continue;
                        }
                    };

                    let mut txids: Vec<[u8; 32]> = Vec::with_capacity(1 + job.txs_hex.len());
                    txids.push(coinbase_tx.compute_txid().as_raw_hash().to_byte_array());
                    for hex_tx in &job.txs_hex {
                        if let Ok(raw) = hex::decode(hex_tx) {
                            if let Ok(t) = deserialize::<Transaction>(&raw) {
                                txids.push(t.compute_txid().as_raw_hash().to_byte_array());
                            }
                        }
                    }

                    let merkle_root = merkle_root_from_txids(&txids);
                    let merkle_node =
                        TxMerkleNode::from_raw_hash(sha256d::Hash::from_byte_array(merkle_root));
                    let version_u32 = u32::from_str_radix(&job.version, 16).unwrap_or(0);
                    let prev_hash = BlockHash::from_str(&job.prevhash_be)
                        .unwrap_or_else(|_| BlockHash::all_zeros());
                    let nbits_u32 = u32::from_str_radix(&job.nbits, 16).unwrap_or(0);
                    let ct = pow::CompactTarget::from_consensus(nbits_u32);
                    let ntime_u32 = u32::from_str_radix(ntime_hex, 16).unwrap_or(0);
                    let nonce_u32 = u32::from_str_radix(nonce_hex, 16).unwrap_or(0);

                    // Fetch current tip (best prev for a new block)
                    let best_prev = self
                        .jobman
                        .node
                        .rpc
                        .get_best_block_hash()
                        .unwrap_or_else(|_| bitcoin::BlockHash::all_zeros());

                    // If the job's prev doesn't match the current tip, reject as stale
                    if best_prev.to_string() != job.prevhash_be {
                        let err = json!({"id": id_val, "result": false, "error": {"code": 21, "message": "Stale: tip changed"}});
                        let mut w = writer.lock().await;
                        w.write_all(format!("{err}\n").as_bytes()).await?;
                        continue;
                    }

                    tracing::info!(
                        "submit: job.prevhash_be={} (header.prev will be {})",
                        job.prevhash_be,
                        prev_hash
                    );
                    let header = Header {
                        version: Version::from_consensus(version_u32 as i32),
                        prev_blockhash: prev_hash,
                        merkle_root: merkle_node,
                        time: ntime_u32,
                        bits: ct,
                        nonce: nonce_u32,
                    };

                    let mut header_bytes = Vec::with_capacity(80);
                    header
                        .consensus_encode(&mut header_bytes)
                        .expect("header encode");

                    let header_hash = sha256d::Hash::hash(&header_bytes);
                    let net_target = pow::Target::from_compact(ct);
                    let meets_block = net_target.is_met_by(header_hash.into());

                    if let Ok(wid) = self.storage.upsert_worker(user, true).await {
                        let _ = self.storage.insert_share(wid, 1.0, true).await;
                    }

                    let ok = json!({"id":id_val,"result":true,"error":null});
                    let mut w = writer.lock().await;
                    w.write_all(format!("{ok}\n").as_bytes()).await?;

                    if meets_block {
                        match self
                            .jobman
                            .submit_from_header_and_template(
                                header_bytes.clone(),
                                coinbase_bytes.clone(),
                                &job,
                            )
                            .await
                        {
                            Err(e) => {
                                warn!("submitblock failed: {e}");
                                // record rejected attempt (no height yet)
                                let _ = self
                                    .storage
                                    .record_block(
                                        None,
                                        &hex::encode(sha256d::Hash::hash(&header_bytes)),
                                        "rejected",
                                    )
                                    .await;
                            }
                            Ok(()) => {
                                info!("Block submitted to bitcoind!");
                                // fetch tip + height and record "accepted"
                                if let Ok(best) = self.jobman.node.rpc.get_best_block_hash() {
                                    let worker_id: String = params
                                        .first()
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    if let Ok(hdr) =
                                        self.jobman.node.rpc.get_block_header_info(&best)
                                    {
                                        let _ = self
                                            .storage
                                            .record_block(
                                                Some(hdr.height as i64),
                                                &best.to_string(),
                                                "accepted",
                                            )
                                            .await;
                                        let _ = self.storage.record_share(&worker_id, 1.0).await;
                                    } else {
                                        let _ = self
                                            .storage
                                            .record_block(None, &best.to_string(), "accepted")
                                            .await;
                                        let _ = self.storage.record_share(&worker_id, 1.0).await;
                                    }
                                }
                            }
                        }
                    }
                }

                "mining.extranonce.subscribe" => {
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(4)), "result": true, "error": null});
                    let mut w = writer.lock().await;
                    w.write_all(format!("{res}\n").as_bytes()).await?;
                }

                _ => {
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(0)), "result": null, "error": {"code": -32601, "message": "Method not found"}});
                    let mut w = writer.lock().await;
                    w.write_all(format!("{res}\n").as_bytes()).await?;
                }
            }
        }
        Ok(())
    }
}

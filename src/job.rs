use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::node::Node;
use crate::storage::Storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StratumJob {
    pub job_id: String,
    pub prevhash: String,
    pub coinbase1: String,
    pub coinbase2: String,
    pub merkle_branch: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub clean_jobs: bool,
}

#[derive(Clone)]
pub struct JobManager {
    node: Node,
    storage: Storage,
    pub current_job: Arc<RwLock<Option<StratumJob>>>,
    sse_tx: tokio::sync::broadcast::Sender<String>,
}

impl JobManager {
    pub fn new(node: Node, storage: Storage, sse_tx: tokio::sync::broadcast::Sender<String>) -> Self {
        Self {
            node,
            storage,
            current_job: Arc::new(RwLock::new(None)),
            sse_tx,
        }
    }

    pub async fn run_block_tx_watcher(&self) -> Result<()> {
        let me = self.clone();
        self.node.run_zmq_watch(move |topic| {
            let me = me.clone();
            tokio::spawn(async move {
                if topic == "block" {
                    if let Err(e) = me.refresh_job().await {
                        warn!("refresh_job on block failed: {e:?}");
                    }
                } else if topic == "tx" {
                    // In a real pool you might re-template on tx too
                }
            });
        }).await?;
        Ok(())
    }

    /// Build or refresh a GBT-based job (very simplified/placeholder).
    pub async fn refresh_job(&self) -> Result<()> {
        let gbt = self.node.get_block_template_raw().await
            .context("getblocktemplate failed")?;

        // Simplified fields: prevhash, version, nbits, curtime (ntime)
        let prevhash = gbt.get("previousblockhash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let version = gbt.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
        let bits = gbt.get("bits").and_then(|v| v.as_str()).unwrap_or("1d00ffff").to_string();
        let curtime = gbt.get("curtime").and_then(|v| v.as_i64()).unwrap_or(0);

        // Extremely simplified coinbase stubs (pool needs real coinbase construction)
        let mut nonce_tail = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut nonce_tail);

        let job = StratumJob {
            job_id: format!("{:x?}", nonce_tail),
            prevhash,
            coinbase1: "02000000010000000000000000000000000000000000000000000000000000000000000000".into(),
            coinbase2: "ffffffff0100f2052a010000001976a914000000000000000000000000000000000000000088ac00000000".into(),
            merkle_branch: vec![], // real pool: build from tx list
            version: format!("{:x}", version as u32),
            nbits: bits,
            ntime: format!("{:x}", curtime as u32),
            clean_jobs: true,
        };

        {
            let mut guard = self.current_job.write().await;
            *guard = Some(job.clone());
        }

        // Notify UI listeners
        let _ = self.sse_tx.send(json!({
            "type": "job",
            "prevhash": job.prevhash,
            "job_id": job.job_id
        }).to_string());

        info!("Refreshed job (prevhash={})", job.prevhash);
        Ok(())
    }

    pub async fn get_current_job(&self) -> Option<StratumJob> {
        self.current_job.read().await.clone()
    }
}
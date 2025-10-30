use crate::{job::JobManager, storage::Storage};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::TcpListener, sync::broadcast};
use tracing::{info, warn};

#[derive(Clone)]
pub struct StratumServer {
    jobman: Arc<JobManager>,
    storage: Storage,
    sse_tx: broadcast::Sender<String>,
}

impl StratumServer {
    pub fn new(jobman: Arc<JobManager>, storage: Storage, sse_tx: broadcast::Sender<String>) -> Self {
        Self { jobman, storage, sse_tx }
    }

    pub async fn run(&self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await
            .context("bind stratum")?;
        loop {
            let (socket, peer) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.handle_client(socket, peer).await {
                    warn!("stratum client {peer} error: {e:#}");
                }
            });
        }
    }

    async fn handle_client(&self, socket: tokio::net::TcpStream, peer: SocketAddr) -> Result<()> {
        info!("Stratum client connected: {peer}");
        let (r, mut w) = socket.into_split();
        let mut r = BufReader::new(r).lines();

        // Send a mining.notify upon connect (optional)
        if let Some(job) = self.jobman.get_current_job().await {
            let notify = json!({
                "id": null,
                "method": "mining.notify",
                "params": [
                    job.job_id,
                    job.prevhash,
                    job.coinbase1,
                    job.coinbase2,
                    job.merkle_branch,
                    job.version,
                    job.nbits,
                    job.ntime,
                    job.clean_jobs
                ]
            });
            w.write_all(format!("{}\n", notify).as_bytes()).await?;
        }

        let mut _worker_name: Option<String> = None;

        while let Some(line) = r.next_line().await? {
            if line.trim().is_empty() { continue; }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => {
                    // Send error
                    let err = json!({"id": null, "error": {"code": -32700, "message": "parse error"}, "result": null});
                    w.write_all(format!("{}\n", err).as_bytes()).await?;
                    continue;
                }
            };

            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
            match method {
                "mining.subscribe" => {
                    let res = json!({
                        "id": msg.get("id").cloned().unwrap_or(json!(1)),
                        "result": [
                            [["mining.set_difficulty","1"],["mining.notify","1"]],
                            "deadbeef", // subscription id
                            4 // extranonce1 size (stub)
                        ],
                        "error": null
                    });
                    w.write_all(format!("{}\n", res).as_bytes()).await?;
                }

                "mining.authorize" => {
                    let name = msg.get("params").and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("unknown");
                    _worker_name = Some(name.to_string());
                    let _ = self.storage.upsert_worker(name, true).await;

                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(2)), "result": true, "error": null});
                    w.write_all(format!("{}\n", res).as_bytes()).await?;
                    let _ = self.sse_tx.send(json!({"type":"worker_seen","name":name}).to_string());
                }

                "mining.submit" => {
                    // params: [ "user.worker", "job_id", "extranonce2", "ntime", "nonce" ]
                    let valid = true; // stub; real implementation does PoW validation against target
                    if let Some(name) = msg.get("params").and_then(|p| p.get(0)).and_then(|v| v.as_str()) {
                        if let Ok(id) = self.storage.upsert_worker(name, true).await {
                            let _ = self.storage.insert_share(id, 1.0, valid).await;
                        }
                    }
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(3)), "result": true, "error": null});
                    w.write_all(format!("{}\n", res).as_bytes()).await?;
                }
                
                "mining.extranonce.subscribe" => {
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(4)), "result": true, "error": null});
                    w.write_all(format!("{}\n", res).as_bytes()).await?;
                }
                _ => {
                    let res = json!({"id": msg.get("id").cloned().unwrap_or(json!(0)), "result": null, "error": {"code": -32601, "message": "Method not found"}});
                    w.write_all(format!("{}\n", res).as_bytes()).await?;
                }
            }
        }

        Ok(())
    }
}
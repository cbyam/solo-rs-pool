use anyhow::{Context, Result};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::json;
use std::sync::Arc;
use tokio::task;
use tracing::info;

#[derive(Clone)]
pub struct Node {
    pub rpc: Arc<Client>,
    pub zmq_block: String,
    pub zmq_tx: String,
}

impl Node {
    pub async fn connect(rpc_url: &str, user: &str, pass: &str, zmq_block: &str, zmq_tx: &str) -> Result<Self> {
        let rpc = Client::new(rpc_url, Auth::UserPass(user.to_string(), pass.to_string()))
            .context("bitcoincore-rpc connect failed")?;
        Ok(Self {
            rpc: Arc::new(rpc),
            zmq_block: zmq_block.to_string(),
            zmq_tx: zmq_tx.to_string(),
        })
    }

    pub async fn best_block_hash(&self) -> Result<String> {
        Ok(self.rpc.get_best_block_hash()?.to_string())
    }

    pub async fn get_block_template_raw(&self) -> Result<serde_json::Value> {
        let req = json!({ "rules": ["segwit"] });
        let v = self.rpc.call::<serde_json::Value>("getblocktemplate", &[req])?;
        Ok(v)
    }

    /// Blocking ZMQ listener stub that wakes the job manager when blocks/tx arrive.
    pub async fn run_zmq_watch<F>(&self, mut on_event: F) -> Result<()>
    where
        F: FnMut(String) + Send + 'static,
    {
        let block_ep = self.zmq_block.clone();
        let tx_ep = self.zmq_tx.clone();

        task::spawn_blocking(move || -> Result<()> {
            let ctx = zmq::Context::new();
            let block_sub = ctx.socket(zmq::SUB)?;
            block_sub.connect(&block_ep)?;
            block_sub.set_subscribe(b"hashblock")?;

            let tx_sub = ctx.socket(zmq::SUB)?;
            tx_sub.connect(&tx_ep)?;
            tx_sub.set_subscribe(b"hashtx")?;

            info!("ZMQ connected: {block_ep} (blocks), {tx_ep} (tx)");

            let poll_items = &mut [
                block_sub.as_poll_item(zmq::POLLIN),
                tx_sub.as_poll_item(zmq::POLLIN),
            ];

            loop {
                zmq::poll(poll_items, 1000)?;
                if poll_items[0].is_readable() {
                    let _topic = block_sub.recv_bytes(0)?;
                    let _hash = block_sub.recv_bytes(0)?;
                    on_event("block".into());
                }
                if poll_items[1].is_readable() {
                    let _topic = tx_sub.recv_bytes(0)?;
                    let _hash = tx_sub.recv_bytes(0)?;
                    on_event("tx".into());
                }
            }
        });

        Ok(())
    }
}
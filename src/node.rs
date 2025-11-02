use anyhow::Result;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use bitcoincore_rpc::json::{GetBlockTemplateModes, GetBlockTemplateResult, GetBlockTemplateRules};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use zmq::{Context as ZmqContext, SocketType};
use bitcoin::BlockHash;

#[derive(Clone, Copy, Debug)]
pub enum ZmqEvent { Block, Tx }

pub struct Node {
    pub rpc: Arc<Client>,
    zmq_block: String,
    zmq_tx: String,
}

impl Node {
    pub fn new(
        rpc_url: &str,
        rpc_user: &str,
        rpc_pass: &str,
        zmq_block: &str,
        zmq_tx: &str,
    ) -> Result<Self> {
        let auth = Auth::UserPass(rpc_user.to_string(), rpc_pass.to_string());
        let rpc = Client::new(rpc_url, auth)?;
        Ok(Self {
            rpc: Arc::new(rpc),
            zmq_block: zmq_block.to_string(),
            zmq_tx: zmq_tx.to_string(),
        })
    }

    pub async fn get_block_template_raw(&self) -> Result<GetBlockTemplateResult> {
        self.rpc
            .get_block_template(
                GetBlockTemplateModes::Template,
                &[GetBlockTemplateRules::SegWit],
                &[],
            )
            .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub async fn get_current_tip(&self) -> Result<BlockHash> {
        Ok(self.rpc.get_best_block_hash()?)
    }

    pub async fn run_zmq_watch<F>(&self, mut on_event: F) -> Result<()>
    where
        F: FnMut(ZmqEvent) + Send + 'static,
    {
        let ctx = ZmqContext::new();
        let block_sock = ctx.socket(SocketType::SUB)?;
        let tx_sock    = ctx.socket(SocketType::SUB)?;

        block_sock.connect(&self.zmq_block)?;
        tx_sock.connect(&self.zmq_tx)?;

        // subscribe to everything on each endpoint (these endpoints are already segregated)
        block_sock.set_subscribe(&[])?;
        tx_sock.set_subscribe(&[])?;

        loop {
            let mut msg = zmq::Message::new();
            // fire only one callback per poll-iteration; prefer block over tx
            if block_sock.recv(&mut msg, zmq::DONTWAIT).is_ok() {
                on_event(ZmqEvent::Block);
            } else if tx_sock.recv(&mut msg, zmq::DONTWAIT).is_ok() {
                on_event(ZmqEvent::Tx);
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}
#![allow(clippy::needless_return)]
mod node;
mod stratum;
mod job;
mod storage;
mod web;

use anyhow::Result;
use axum::Router;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::{broadcast};
use tracing_subscriber::EnvFilter;

use crate::{
    job::JobManager,
    storage::Storage,
    node::Node,
};

#[derive(Clone)]
pub struct AppState {
    pub storage: Storage,
    pub jobman: Arc<JobManager>,
    pub sse_tx: broadcast::Sender<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
            .add_directive("solo_rs_pool=info".parse().unwrap())
            .add_directive("axum::rejection=warn".parse().unwrap()))
        .init();

    // Config (env)
    let rpc_url = std::env::var("BITCOIN_RPC_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18443".into());
    let rpc_user = std::env::var("BITCOIN_RPC_USER").unwrap_or_else(|_| "user".into());
    let rpc_pass = std::env::var("BITCOIN_RPC_PASS").unwrap_or_else(|_| "pass".into());
    let zmq_block = std::env::var("BITCOIN_ZMQ_BLOCK").unwrap_or_else(|_| "tcp://127.0.0.1:28332".into());
    let zmq_tx = std::env::var("BITCOIN_ZMQ_TX").unwrap_or_else(|_| "tcp://127.0.0.1:28333".into());
    let db_path = std::env::var("POOL_DB").unwrap_or_else(|_| "pool.sqlite".into());
    let stratum_addr: SocketAddr = std::env::var("STRATUM_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3333".into())
        .parse()?;
    let web_addr: SocketAddr = std::env::var("WEB_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()?;

    // Storage
    let storage = Storage::connect(&db_path).await?;
    storage.ensure_schema().await?;

    // SSE broadcast
    let (sse_tx, _sse_rx) = broadcast::channel::<String>(1024);

    // Node + Job manager
    let node = Node::connect(&rpc_url, &rpc_user, &rpc_pass, &zmq_block, &zmq_tx).await?;
    let jobman = Arc::new(JobManager::new(node.clone(), storage.clone(), sse_tx.clone()));

    // Preload job
    jobman.refresh_job().await?;

    // Spawn ZMQ watcher
    {
        let jm = jobman.clone();
        tokio::spawn(async move {
            if let Err(e) = jm.run_block_tx_watcher().await {
                tracing::error!("ZMQ watcher exited: {e:?}");
            }
        });
    }

    // Spawn Stratum
    {
        let jm = jobman.clone();
        let st = storage.clone();
        let sse = sse_tx.clone();
        tokio::spawn(async move {
            let server = stratum::StratumServer::new(jm, st, sse);
            if let Err(e) = server.run(stratum_addr).await {
                tracing::error!("Stratum server exited: {e:?}");
            }
        });
    }

    // Web
    let state = AppState { storage: storage.clone(), jobman: jobman.clone(), sse_tx: sse_tx.clone() };
    let app: Router = web::build_router(state);

    tracing::info!("Web UI on http://{web_addr}");
    tracing::info!("Stratum listening on {stratum_addr}");
    axum::serve(tokio::net::TcpListener::bind(web_addr).await?, app).await?;

    Ok(())
}
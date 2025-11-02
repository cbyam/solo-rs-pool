use anyhow::{Context, Result};
use std::{env, net::SocketAddr, sync::Arc};
use tokio::time::sleep;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use dotenvy::dotenv; // ✅ load .env automatically

mod job;
mod node;
mod storage;
mod stratum;
mod web;

use crate::job::JobManager;
use crate::node::Node;
use crate::storage::Storage;
use crate::stratum::StratumServer;
use crate::web::AppState;

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    // ---------- env + logging ----------
    dotenv().ok(); // ✅ load .env
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();

    // ---------- configuration ----------
    let rpc_url = env_or("BITCOIN_RPC_URL", "http://127.0.0.1:18443");
    let rpc_user = env_or("BITCOIN_RPC_USER", "solo");
    let rpc_pass = env_or("BITCOIN_RPC_PASS", "solo123");
    let zmq_block = env_or("BITCOIN_ZMQ_BLOCK", "tcp://127.0.0.1:28342");
    let zmq_tx = env_or("BITCOIN_ZMQ_TX", "tcp://127.0.0.1:28343");
    let stratum_addr: SocketAddr = env_or("STRATUM_ADDR", "0.0.0.0:3335")
        .parse()
        .context("parse STRATUM_ADDR")?;
    let web_addr: SocketAddr = env_or("WEB_ADDR", "0.0.0.0:8080")
        .parse()
        .context("parse WEB_ADDR")?;
    let db_path = env_or("POOL_DB", "./data/pool.sqlite");
    tracing::info!("RPC URL={}, RPC USER={}", rpc_url, rpc_user);

    // ---------- subsystems ----------
    let node = Arc::new(Node::new(
        &rpc_url, &rpc_user, &rpc_pass, &zmq_block, &zmq_tx,
    )?);
    let storage = Storage::connect(&db_path).await.context("storage init")?;
    storage.ensure_schema().await?;

    let (sse_tx, _sse_rx) = tokio::sync::broadcast::channel::<String>(64);
    let (stratum_tx, _) = tokio::sync::broadcast::channel::<String>(100);

    let jobman = Arc::new(JobManager::new(
        node,
        storage.clone(),
        sse_tx.clone(),
        stratum_tx.clone(),
    ));

    let stratum = StratumServer::new(
        jobman.clone(),
        storage.clone(),
        sse_tx.clone(),
        stratum_tx.clone(),
    );

    // ---- initial job ----
    let mut tries = 0usize;
    loop {
        match jobman.refresh_job().await {
            Ok(_) => break,
            Err(e) => {
                tries += 1;
                error!("initial job failed (attempt {tries}): {e:#}");
                if tries > 10 {
                    anyhow::bail!("failed to fetch initial job after 10 attempts: {e:#}");
                }
                sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }

    info!(
        "Job ready – prevhash {}",
        jobman.get_current_job().await.unwrap().prevhash
    );

    // ---- web UI ----
    {
        let state = AppState {
            storage: storage.clone(),
            jobman: jobman.clone(),
            sse_tx: sse_tx.clone(),
        };
        let app = web::build_router(state);
        let web = tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::bind(web_addr).await.unwrap(), app)
                .await
                .unwrap();
        });
        info!("Web UI → http://{web_addr}");
        tokio::task::spawn(web);
    }

    // ---- stratum ----
    {
        let stratum = stratum.clone();
        let stratum_task = tokio::spawn(async move {
            stratum.run(stratum_addr).await.unwrap();
        });
        info!("Stratum → {stratum_addr}");
        tokio::task::spawn(stratum_task);
    }

    // ---- ZMQ watcher ----
    {
        let jm = jobman.clone();
        tokio::spawn(async move {
            if let Err(e) = jm.run_block_tx_watcher().await {
                error!("ZMQ watcher died: {e:#}");
            }
        });
    }

    // keep alive forever
    futures_util::future::pending::<()>().await;
    #[allow(unreachable_code)]
    Ok(())
}

pub mod templates;

use crate::{job::JobManager, storage::Storage};
use askama_axum::IntoResponse;
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use axum::{
    extract::{Path, Query},
    Json,
};
use serde::Deserialize;
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

#[derive(Clone)]
pub struct AppState {
    pub storage: Storage,
    pub jobman: Arc<JobManager>,
    pub sse_tx: broadcast::Sender<String>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/workers", get(workers))
        .route("/blocks", get(blocks))
        .route("/events", get(events))
        .route("/api/worker/:name/metrics", get(worker_metrics))
        // ✅ Serve static files (CSS/JS) from src/web/static
        .nest_service("/static", ServeDir::new("src/web/static"))
        .with_state(state)
}

async fn dashboard(State(state): State<AppState>) -> impl IntoResponse {
    let workers = state.storage.list_workers().await.unwrap_or_default();
    let job = state.jobman.get_current_job().await;
    templates::DashboardTemplate { workers, job }.into_response()
}

async fn workers(State(state): State<AppState>) -> impl IntoResponse {
    let workers = state.storage.list_workers().await.unwrap_or_default();
    templates::WorkersTemplate { workers }.into_response()
}

async fn blocks(State(state): State<AppState>) -> impl IntoResponse {
    let list = state.storage.recent_blocks(50).await.unwrap_or_default();
    let blocks = list
        .into_iter()
        .map(|(height, hash, status, ts)| templates::BlockRow {
            height: height.map(|h| h.to_string()).unwrap_or_else(|| "-".into()),
            hash: hash.unwrap_or_else(|| "-".into()),
            status,
            timestamp: ts,
        })
        .collect::<Vec<_>>();

    templates::BlocksTemplate { blocks }.into_response()
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.sse_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(s) => yield Ok(Event::default().event("msg").data(s)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}

#[derive(Deserialize)]
struct Win {
    window: Option<i64>,
    minutes: Option<i64>,
}

async fn worker_metrics(
    State(state): State<AppState>,
    Path(worker): Path<String>,
    Query(win): Query<Win>,
) -> impl IntoResponse {
    let window = win.window.unwrap_or(300); // 5m
    let minutes = win.minutes.unwrap_or(60); // sparkline 60m
    let hps = state
        .storage
        .worker_hashrate_hps(&worker, window)
        .await
        .unwrap_or(0.0);
    let buckets = state
        .storage
        .share_buckets(&worker, minutes)
        .await
        .unwrap_or_default();

    // normalize buckets -> [ [ts, count], ... ]
    let series: Vec<(i64, i64)> = buckets;
    Json(serde_json::json!({
        "worker": worker,
        "window_secs": window,
        "hashrate_hps": hps,
        "spark": series
    }))
}

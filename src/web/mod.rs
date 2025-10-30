pub mod templates;

use crate::AppState;
use askama_axum::IntoResponse;
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use std::{convert::Infallible, time::Duration};
use tower_http::services::ServeDir;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/workers", get(workers))
        .route("/blocks", get(blocks))
        .route("/events", get(events))
        .nest_service("/static", ServeDir::new("templates"))
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
    // Map DB tuples into display strings so templates stay simple
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
                Ok(s) => {
                    yield Ok(Event::default().event("msg").data(s));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Drop lagged messages and keep going
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    )
}
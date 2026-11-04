//! HTTP surface: static UI, snapshot, SSE stream and event ingestion.

use std::{convert::Infallible, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::header,
    response::{
        Html, IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::stream::{Stream, StreamExt};
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

use crate::state::{DashboardState, TraceEvent, now_ms};

pub fn build_router(state: Arc<DashboardState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/style.css", get(style))
        .route("/app.js", get(script))
        .route("/health", get(health))
        .route("/api/state", get(snapshot))
        .route("/api/events", get(stream).post(ingest))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../assets/style.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../assets/app.js"),
    )
}

async fn health(State(state): State<Arc<DashboardState>>) -> Json<Value> {
    Json(json!({"status": "ok", "service": "dashboard", "metrics": state.metrics()}))
}

async fn snapshot(State(state): State<Arc<DashboardState>>) -> Json<Value> {
    Json(state.snapshot())
}

async fn stream(
    State(state): State<Arc<DashboardState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.subscribe();
    let stream = BroadcastStream::new(receiver).filter_map(|item| async move {
        let event = item.ok()?;
        Event::default()
            .event("trace")
            .json_data(event)
            .ok()
            .map(Ok)
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn ingest(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let values = match payload {
        Value::Array(items) => items,
        other => vec![other],
    };
    let mut accepted = 0;
    let mut errors = Vec::new();
    for value in values {
        match serde_json::from_value::<TraceEvent>(value) {
            Ok(mut event) => {
                if event.ts_ms == 0 {
                    event.ts_ms = now_ms();
                }
                state.emit(event);
                accepted += 1;
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    Json(json!({"accepted": accepted, "errors": errors}))
}

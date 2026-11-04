//! Management API on `API_PORT`: devices, push and supported directives.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{connections::ConnectionRegistry, storage::SqliteStorage};

pub struct AppState {
    pub storage: Arc<SqliteStorage>,
    pub connections: Arc<ConnectionRegistry>,
    pub glagol_jwt_key: String,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/devices", get(list_devices))
        .route("/devices/{duid}", patch(update_device))
        .route("/devices/{duid}/push/text", post(push_text))
        .route("/devices/{duid}/push/raw", post(push_raw))
        .route("/devices/{duid}/push", post(push_directives))
        .route("/devices/{duid}/directives", get(supported_directives))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "quasar", "mode": "management-api"}))
}

async fn list_devices(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.storage.list() {
        Ok(stations) => Json(Value::Array(
            stations
                .into_iter()
                .map(|station| {
                    json!({
                        "connected": state.connections.is_connected(&station.duid),
                        "duid": station.duid,
                        "name": station.name,
                        "quasarConfig": station.quasar_config,
                    })
                })
                .collect(),
        ))
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to list stations");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct UpdateDevice {
    name: Option<String>,
    #[serde(rename = "quasarConfig")]
    quasar_config: Option<Value>,
}

async fn update_device(
    Path(duid): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateDevice>,
) -> impl IntoResponse {
    match state.storage.update_name_and_config(
        &duid,
        body.name.as_deref(),
        body.quasar_config.as_ref(),
    ) {
        Ok(Some(station)) => Json(json!({
            "duid": station.duid,
            "name": station.name,
            "quasarConfig": station.quasar_config,
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"code": "DEVICE_NOT_FOUND"})),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, %duid, "failed to update station");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct PushText {
    #[serde(rename = "eventText")]
    event_text: String,
}

async fn push_text(
    Path(duid): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PushText>,
) -> Json<Value> {
    state.connections.push_tts(&duid, &body.event_text).await;
    Json(json!({}))
}

async fn push_raw(
    Path(duid): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(directive): Json<Value>,
) -> Json<Value> {
    state.connections.push_raw(&duid, &directive).await;
    Json(json!({}))
}

async fn push_directives(
    Path(duid): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(directives): Json<Vec<Value>>,
) -> Json<Value> {
    state.connections.push_directives(&duid, &directives).await;
    Json(json!({}))
}

async fn supported_directives(
    Path(duid): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.connections.supported_directives(&duid) {
        Some(directives) => Json(directives).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"code": "DEVICE_NOT_CONNECTED"})),
        )
            .into_response(),
    }
}

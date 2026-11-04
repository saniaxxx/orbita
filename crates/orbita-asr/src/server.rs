//! WebSocket front end: one config message, then PCM16 binary frames.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures_util::SinkExt;
use serde_json::{Value, json};

use crate::{
    config::{Config, SAMPLE_RATE},
    recognizer::Recognizer,
    streaming::{AsrEvent, StreamingAsrSession},
    vad::VadDetector,
};

pub struct AppState {
    pub config: Arc<Config>,
    pub recognizer: Arc<dyn Recognizer>,
    pub vad_factory: Arc<dyn Fn() -> Box<dyn VadDetector> + Send + Sync>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(websocket))
        .route("/health", get(health))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "asr",
        "sample_rate": SAMPLE_RATE,
        "language": state.config.language,
        "engine": state.config.engine,
        "vad": state.config.vad,
    }))
}

async fn websocket(
    websocket: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    websocket.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let Some(Ok(Message::Text(config_message))) = socket.recv().await else {
        tracing::warn!("ASR session did not start with a config message");
        let _ = socket.close().await;
        return;
    };

    let sample_rate = serde_json::from_str::<Value>(config_message.as_str())
        .ok()
        .and_then(|value| value.get("sample_rate").and_then(Value::as_u64));
    if sample_rate != Some(SAMPLE_RATE as u64) {
        tracing::warn!(?sample_rate, "unsupported ASR sample rate");
        let _ = socket.close().await;
        return;
    }

    let mut session = new_session(&state);
    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "ASR websocket receive failed");
                break;
            }
        };

        match message {
            Message::Binary(bytes) => {
                let (pairs, _) = bytes[..].as_chunks::<2>();
                let pcm: Vec<i16> = pairs.iter().map(|pair| i16::from_le_bytes(*pair)).collect();
                let mut final_seen = false;
                for event in session.add_audio(&pcm).await {
                    let AsrEvent::Transcript(transcript) = event else {
                        continue;
                    };
                    let Ok(encoded) = serde_json::to_string(&transcript) else {
                        continue;
                    };
                    if socket.send(Message::Text(encoded.into())).await.is_err() {
                        return;
                    }
                    final_seen |= transcript.end_of_utt;
                }
                if final_seen {
                    session = new_session(&state);
                }
            }
            Message::Text(_) => {}
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
}

fn new_session(state: &AppState) -> StreamingAsrSession {
    StreamingAsrSession::new(
        Arc::clone(&state.config),
        Arc::clone(&state.recognizer),
        (state.vad_factory)(),
    )
}

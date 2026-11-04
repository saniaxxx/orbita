//! HTTP/WebSocket front end. quasar connects to `ws://host:port/process`.

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

use crate::{processor::Processor, protocol::ClientMessage, protocol::ServerMessage};

pub fn build_router(processor: Arc<Processor>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/process", get(websocket))
        .with_state(processor)
}

async fn health(State(processor): State<Arc<Processor>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "processor",
        "model": processor.config.openai_model,
    }))
}

async fn websocket(
    websocket: WebSocketUpgrade,
    State(processor): State<Arc<Processor>>,
) -> impl IntoResponse {
    websocket.on_upgrade(move |socket| handle_socket(socket, processor))
}

async fn handle_socket(mut socket: WebSocket, processor: Arc<Processor>) {
    let mut session = Arc::clone(&processor).new_session();
    tracing::info!(session_id = %session.session_id(), "websocket opened");

    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "websocket receive failed");
                break;
            }
        };

        match message {
            Message::Text(text) => {
                let parsed: ClientMessage = match serde_json::from_str(text.as_str()) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        tracing::warn!(%error, "invalid processor message");
                        continue;
                    }
                };

                match parsed {
                    ClientMessage::Prepare(data) => {
                        tracing::info!(session_id = %session.session_id(), "prepare");
                        if let Err(error) = session.prepare(data.directive_functions).await {
                            tracing::error!(%error, "prepare failed");
                            let _ = socket.close().await;
                            return;
                        }
                    }
                    ClientMessage::Process(data) => {
                        tracing::info!(
                            session_id = %session.session_id(),
                            text = %data.text,
                            "process"
                        );
                        match session.process(&data.text, &data.metadata).await {
                            Ok(responses) => {
                                let should_close = responses
                                    .last()
                                    .map(|response| response.finished && !response.should_listen)
                                    .unwrap_or(true);
                                for response in responses {
                                    let message = ServerMessage::PartialResponse(response);
                                    let Ok(encoded) = serde_json::to_string(&message) else {
                                        tracing::error!("failed to encode a partial response");
                                        return;
                                    };
                                    if socket.send(Message::Text(encoded.into())).await.is_err() {
                                        return;
                                    }
                                }
                                if should_close {
                                    let _ = socket.close().await;
                                    return;
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "process failed");
                                let _ = socket.close().await;
                                return;
                            }
                        }
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }

    tracing::info!(session_id = %session.session_id(), "websocket closed");
}

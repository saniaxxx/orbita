//! WebSocket front end compatible with quasar's processor protocol.
//!
//! `prepare` is accepted for compatibility. `process` handles one utterance and
//! returns a single `partialResponse`; the session key is the station duid when
//! quasar passes it in `metadata.internal.deviceId`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::{get, post},
};
use chrono::Utc;
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::action::{Action, Outcome, SessionKey};
use crate::agent::Agent;
use crate::channel::station::{Directive, StationSession};

pub struct AppState {
    pub agent: Arc<Agent>,
    pub model: String,
    pub default_duid: Option<String>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/process", get(websocket))
        .route("/pending", get(pending_actions))
        .route("/confirm", post(confirm_action))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "agent",
        "model": state.model,
    }))
}

async fn pending_actions(State(state): State<Arc<AppState>>) -> Json<Value> {
    match state.agent.home() {
        Some(home) => Json(home.pending().await),
        None => Json(json!([])),
    }
}

#[derive(Debug, Deserialize)]
struct ConfirmBody {
    code: String,
}

async fn confirm_action(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConfirmBody>,
) -> Json<Value> {
    let Some(home) = state.agent.home() else {
        return Json(json!({"status": "error", "error": "home control is disabled"}));
    };
    match home.confirm(&body.code).await {
        Ok(description) => Json(json!({"status": "ok", "done": description})),
        Err(error) => Json(json!({"status": "error", "error": error})),
    }
}

async fn websocket(
    websocket: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    websocket.on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "data")]
enum ClientMessage {
    #[serde(rename = "prepare")]
    Prepare {
        #[serde(default, rename = "directiveFunctions")]
        directive_functions: Value,
    },
    #[serde(rename = "process")]
    Process(ProcessData),
}

#[derive(Debug, Deserialize)]
struct ProcessData {
    #[serde(default)]
    metadata: Metadata,
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct Metadata {
    #[serde(default)]
    internal: Value,
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // quasar sends the offered station functions once per connection, before
    // the first `process`.
    let mut channel = StationSession::default();
    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "agent websocket receive failed");
                break;
            }
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let parsed: ClientMessage = match serde_json::from_str(text.as_str()) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn!(%error, "invalid agent message");
                continue;
            }
        };

        match parsed {
            ClientMessage::Prepare {
                directive_functions,
            } => match StationSession::from_prepare(&directive_functions) {
                Ok(parsed) => {
                    info!(functions = parsed.len(), "prepare");
                    channel = parsed;
                }
                Err(error) => warn!(%error, "prepare ignored"),
            },
            ClientMessage::Process(data) => {
                let duid = session_id(&data.metadata, state.default_duid.as_deref());
                let session = SessionKey::station(duid.clone());
                let context = channel.context();
                info!(%duid, text = %data.text, functions = context.functions.len(), "process");
                let started = std::time::Instant::now();
                let (outcome, ok) = match state.agent.process(&data.text, &session, &context).await
                {
                    Ok(outcome) => (outcome, true),
                    Err(error) => {
                        warn!(%error, %duid, "agent request failed");
                        (Outcome::text("Извини, не получилось."), false)
                    }
                };
                info!(
                    %duid,
                    ms = started.elapsed().as_millis(),
                    reply = %outcome.text,
                    actions = outcome.actions.len(),
                    "process done"
                );
                let directives = render_directives(&state, &channel, &outcome, &duid, &data.text);
                let response = json!({
                    "type": "partialResponse",
                    "data": {
                        "directives": directives,
                        "text": outcome.text,
                        "finished": true,
                        "shouldListen": false,
                    }
                });
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
                if !ok {
                    let _ = socket.close().await;
                    return;
                }
                // quasar opens a fresh processor connection per utterance.
                let _ = socket.close().await;
                return;
            }
        }
    }
}

/// Turn the actions into station directives. The channel validates names and
/// arguments; anything it cannot render is dropped with a warning. Native
/// timers are mirrored in SQLite for audit and cancellation.
fn render_directives(
    state: &AppState,
    channel: &StationSession,
    outcome: &Outcome,
    duid: &str,
    text: &str,
) -> Vec<Directive> {
    let mut directives = Vec::new();
    for action in &outcome.actions {
        if matches!(action, Action::ToolCall { .. }) {
            // Server-side tools are executed inside the agent core.
            continue;
        }
        let Some(directive) = channel.directive(action) else {
            warn!(
                ?action,
                "channel did not offer the function; directive dropped"
            );
            continue;
        };
        if let Action::SetTimer { seconds, label } = action {
            let due = Utc::now() + chrono::Duration::seconds(*seconds as i64);
            let label = label.clone().unwrap_or_else(|| text.to_owned());
            if let Err(error) =
                state
                    .agent
                    .store()
                    .insert_task("native_timer", &label, Some(duid), due)
            {
                warn!(%error, "mirroring the native timer failed");
            }
        }
        directives.push(directive);
    }
    directives
}

fn session_id(metadata: &Metadata, default_duid: Option<&str>) -> String {
    metadata
        .internal
        .get("deviceId")
        .and_then(Value::as_str)
        .or_else(|| metadata.internal.get("sessionId").and_then(Value::as_str))
        .or(default_duid)
        .unwrap_or("anonymous")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_uses_device_id() {
        let metadata = Metadata {
            internal: json!({"deviceId": "duid-1"}),
        };
        assert_eq!(session_id(&metadata, None), "duid-1");
    }

    #[test]
    fn session_falls_back_to_anonymous() {
        assert_eq!(session_id(&Metadata::default(), None), "anonymous");
        assert_eq!(session_id(&Metadata::default(), Some("default")), "default");
    }
}

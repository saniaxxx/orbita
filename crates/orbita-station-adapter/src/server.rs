//! quasar-compatible `/process` WebSocket in front of the agent core.
//!
//! quasar opens one connection per utterance: `prepare` (offered station
//! functions), then `process` (recognised text). The adapter builds a contract
//! [`InboundEvent`], answers deterministic intents locally (D-25), sends
//! everything else to the core and renders device actions as directives. The
//! reply is one final `partialResponse`; with `stream_replies` (D-19) a core
//! reply may come as two: the first sentence (`finished: false`) as soon as it
//! is ready, then the rest. TS-quasar must run with `PROCESSOR_STREAMING=true`
//! to play them as one reply.

use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::Query,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use orbita_contracts::{CONTRACT_VERSION, DeviceAction, InboundEvent, InputKind};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::core::Core;
use crate::schedule::{Commitment, Notifier, Scheduler, choose_delivery, split_channel};
use crate::speech;
use crate::station::{Directive, StationFunctions, intent_action};

pub const FALLBACK_REPLY: &str = "Извини, не получилось.";
pub const EMPTY_REPLY: &str = "Я вас не расслышала. Повторите, пожалуйста.";

#[derive(Debug, Clone)]
pub struct Identity {
    pub household: String,
    pub default_duid: Option<String>,
    /// duid → room, from configuration.
    pub rooms: Vec<(String, String)>,
    pub locale: String,
    pub timezone: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            household: "home".into(),
            default_duid: None,
            rooms: Vec::new(),
            locale: "ru-RU".into(),
            timezone: "Europe/Moscow".into(),
        }
    }
}

pub struct AppState {
    pub core: Arc<dyn Core>,
    pub identity: Identity,
    pub core_name: String,
    /// Core scheduler client (D-26); `None` keeps native station timers.
    pub scheduler: Option<Arc<dyn Scheduler>>,
    /// Push channel for deliveries arriving on `/outbound`.
    pub notifier: Option<Arc<dyn Notifier>>,
    /// Shared secret in the webhook URL; `/outbound` is closed without it.
    pub outbound_key: Option<String>,
    pub telegram_chat: Option<String>,
    /// Send the first sentence of a core reply early (D-19).
    pub stream_replies: bool,
}

impl AppState {
    pub fn new(core: Arc<dyn Core>, identity: Identity, core_name: impl Into<String>) -> Self {
        Self {
            core,
            identity,
            core_name: core_name.into(),
            scheduler: None,
            notifier: None,
            outbound_key: None,
            telegram_chat: None,
            stream_replies: false,
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/process", get(websocket))
        .route("/outbound", post(outbound))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct OutboundQuery {
    device: String,
    #[serde(default)]
    key: String,
}

/// Delivery from the core scheduler (OpenClaw cron webhook): speak the job
/// summary on the station. 2xx tells the scheduler it was delivered.
async fn outbound(
    State(state): State<Arc<AppState>>,
    Query(query): Query<OutboundQuery>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let Some(expected) = state.outbound_key.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "outbound is not configured"})),
        );
    };
    if !constant_time_eq(expected.as_bytes(), query.key.as_bytes()) {
        warn!(device = %query.device, "outbound with a wrong key");
        return (StatusCode::FORBIDDEN, Json(json!({"error": "forbidden"})));
    }
    let job = body.get("jobId").and_then(Value::as_str).unwrap_or("");
    let status = body.get("status").and_then(Value::as_str).unwrap_or("");
    let summary = body
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if status != "ok" || summary.is_empty() {
        warn!(%job, status, "outbound without a deliverable summary");
        return (StatusCode::OK, Json(json!({"delivered": false})));
    }
    let Some(notifier) = state.notifier.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "no push channel"})),
        );
    };
    match notifier.push(&query.device, summary).await {
        Ok(()) => {
            info!(%job, device = %query.device, text = summary, "outbound delivered");
            (StatusCode::OK, Json(json!({"delivered": true})))
        }
        Err(error) => {
            warn!(%job, device = %query.device, error = format!("{error:#}"), "outbound push failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "push failed"})),
            )
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "station-adapter",
        "core": state.core_name,
        "contract_version": CONTRACT_VERSION,
    }))
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
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct Metadata {
    #[serde(default)]
    internal: Value,
}

/// The outcome of one utterance, before it is rendered for quasar.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    /// What is left to say; after an early part only the rest of the reply.
    pub text: String,
    pub directives: Vec<Directive>,
    pub route: &'static str,
    /// When the first sentence went out early, its latency in ms.
    pub first_part_ms: Option<u64>,
    /// The reply ends with a question: the station keeps the dialog open and
    /// listens for the answer.
    pub should_listen: bool,
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut functions = StationFunctions::default();
    while let Some(message) = socket.recv().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(error) => {
                warn!(%error, "adapter websocket receive failed");
                break;
            }
        };
        let parsed: ClientMessage = match serde_json::from_str(text.as_str()) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn!(%error, "invalid /process message");
                continue;
            }
        };
        match parsed {
            ClientMessage::Prepare {
                directive_functions,
            } => match StationFunctions::from_prepare(&directive_functions) {
                Ok(parsed) => {
                    info!(functions = parsed.len(), "prepare");
                    functions = parsed;
                }
                Err(error) => warn!(%error, "prepare ignored"),
            },
            ClientMessage::Process(data) => {
                let duid = device_id(&data.metadata, state.identity.default_duid.as_deref());
                let (early_tx, mut early_rx) = mpsc::unbounded_channel();
                let early = state.stream_replies.then_some(early_tx);
                let turn = handle_turn(&state, &functions, &duid, &data.text, early);
                tokio::pin!(turn);
                let mut early_open = true;
                let result = loop {
                    tokio::select! {
                        result = &mut turn => break result,
                        part = early_rx.recv(), if early_open => match part {
                            Some(part) => {
                                let response = json!({
                                    "type": "partialResponse",
                                    "data": {"directives": [], "text": part, "finished": false}
                                });
                                let _ = socket
                                    .send(Message::Text(response.to_string().into()))
                                    .await;
                            }
                            None => early_open = false,
                        },
                    }
                };
                let response = json!({
                    "type": "partialResponse",
                    "data": {
                        "directives": result.directives,
                        "text": result.text,
                        "finished": true,
                        "shouldListen": result.should_listen,
                    }
                });
                let _ = socket
                    .send(Message::Text(response.to_string().into()))
                    .await;
                if result.should_listen {
                    // quasar keeps this processor session for the answer
                    // and sends the next `process` on the same connection.
                    continue;
                }
                // Otherwise quasar opens a fresh connection per utterance.
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    }
}

/// One utterance end to end: contract event → intent layer or core →
/// directives. Never fails; errors become the fallback phrase.
pub async fn handle_turn(
    state: &AppState,
    functions: &StationFunctions,
    duid: &str,
    text: &str,
    early: Option<mpsc::UnboundedSender<String>>,
) -> TurnResult {
    let started = Instant::now();
    let event = inbound_event(&state.identity, functions, duid, text);
    if let Err(error) = event.validate() {
        warn!(%error, %duid, "invalid inbound event");
        return TurnResult {
            text: FALLBACK_REPLY.into(),
            directives: Vec::new(),
            route: "invalid",
            first_part_ms: None,
            should_listen: false,
        };
    }
    if event.text.is_empty() {
        // ASR produced nothing (cold start, noise): answer politely, as the
        // legacy agent does, instead of waking the core.
        let result = TurnResult {
            text: EMPTY_REPLY.into(),
            directives: Vec::new(),
            route: "empty",
            first_part_ms: None,
            should_listen: false,
        };
        log_turn(&event, &result, started);
        return result;
    }

    // Device settings (D-26): volume is the station's own business.
    if let Some(change) = orbita_intents::volume(&event.text) {
        let intent = orbita_intents::Intent::Volume(change);
        if functions.supports_intent(&intent) {
            let result = TurnResult {
                text: intent.reply(),
                directives: render(functions, &[intent_action(&intent)]),
                route: "intent",
                first_part_ms: None,
                should_listen: false,
            };
            log_turn(&event, &result, started);
            return result;
        }
    }

    // Time commitments (D-26): the core scheduler owns them.
    if let Some(result) = commitment_turn(state, functions, &event).await {
        log_turn(&event, &result, started);
        return result;
    }

    let (outcome, first) = match early {
        Some(early) => core_streaming(state, &event, &early, started).await,
        None => (state.core.turn(&event).await, None),
    };
    let result = match outcome {
        Ok(turn) => {
            let directives = render(functions, &turn.actions);
            match &first {
                Some((part, ms)) => {
                    let rest = speech::remainder(&turn.text, part).unwrap_or_else(|| {
                        warn!(turn_id = %event.turn_id, "streamed reply does not start with the first part");
                        ""
                    });
                    TurnResult {
                        text: speech::spoken(rest),
                        directives,
                        route: "core",
                        first_part_ms: Some(*ms),
                        should_listen: speech::asks(&turn.text),
                    }
                }
                None => {
                    let text = speech::spoken(&turn.text);
                    let text = if text.is_empty() && directives.is_empty() {
                        FALLBACK_REPLY.to_owned()
                    } else {
                        text
                    };
                    TurnResult {
                        should_listen: speech::asks(&text),
                        text,
                        directives,
                        route: "core",
                        first_part_ms: None,
                    }
                }
            }
        }
        Err(error) => {
            warn!(error = format!("{error:#}"), turn_id = %event.turn_id, "core turn failed");
            TurnResult {
                // After an early part the station is already speaking; do
                // not append an apology to half a sentence.
                text: if first.is_some() {
                    String::new()
                } else {
                    FALLBACK_REPLY.into()
                },
                directives: Vec::new(),
                route: "core_error",
                first_part_ms: first.map(|(_, ms)| ms),
                should_listen: false,
            }
        }
    };
    log_turn(&event, &result, started);
    result
}

/// Run a streamed core turn, forwarding the first sentence to `early` while
/// the core is still talking. Returns the first part (as sent) and its latency.
async fn core_streaming(
    state: &AppState,
    event: &InboundEvent,
    early: &mpsc::UnboundedSender<String>,
    started: Instant,
) -> (anyhow::Result<crate::core::CoreTurn>, Option<(String, u64)>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let turn = state.core.turn_streaming(event, tx);
    tokio::pin!(turn);
    let mut first: Option<(String, u64)> = None;
    let mut open = true;
    let outcome = loop {
        tokio::select! {
            outcome = &mut turn => break outcome,
            part = rx.recv(), if open => match part {
                Some(part) if first.is_none() => {
                    let ms = started.elapsed().as_millis() as u64;
                    let text = speech::spoken(&part);
                    let _ = early.send(text);
                    first = Some((part, ms));
                }
                Some(_) => {}
                None => open = false,
            },
        }
    };
    (outcome, first)
}

/// Deterministic reminders and timers. With a scheduler they become core
/// scheduler jobs delivered here or to Telegram; without one a timer falls
/// back to the native station timer (offline/dev only).
async fn commitment_turn(
    state: &AppState,
    functions: &StationFunctions,
    event: &InboundEvent,
) -> Option<TurnResult> {
    let (text, wants_telegram) = split_channel(&event.text);
    let now = chrono::Utc::now();
    let (commitment_text, due_at, reply) =
        if let Some(reminder) = orbita_intents::reminder::parse(&text, now) {
            (
                format!("Напоминание: {}", reminder.text),
                reminder.due_at,
                reminder.reply,
            )
        } else if let Some((seconds, label)) = orbita_intents::timer(&text) {
            let intent = orbita_intents::Intent::Timer {
                seconds,
                label: label.clone(),
            };
            let Some(_) = state.scheduler else {
                // No scheduler: native timer, as before D-26.
                if !functions.supports_intent(&intent) {
                    return None;
                }
                return Some(TurnResult {
                    text: intent.reply(),
                    directives: render(functions, &[intent_action(&intent)]),
                    route: "intent",
                    first_part_ms: None,
                    should_listen: false,
                });
            };
            let duration = orbita_intents::russian::duration_phrase(seconds);
            let fired = match label {
                Some(label) => format!("Таймер {duration} для {label} истёк."),
                None => format!("Таймер {duration} истёк."),
            };
            (
                fired,
                now + chrono::Duration::seconds(seconds as i64),
                intent.reply(),
            )
        } else {
            return None;
        };
    let scheduler = state.scheduler.as_deref()?;
    let (delivery, note) = choose_delivery(
        wants_telegram,
        state.telegram_chat.as_deref(),
        &event.device_id,
    );
    let commitment = Commitment {
        due_at,
        text: commitment_text,
        delivery,
    };
    Some(match scheduler.schedule(&commitment).await {
        Ok(job) => {
            info!(%job, %due_at, delivery = ?commitment.delivery, "commitment scheduled");
            TurnResult {
                text: match note {
                    Some(note) => format!("{note} {reply}"),
                    None => reply,
                },
                directives: Vec::new(),
                route: "schedule",
                first_part_ms: None,
                should_listen: false,
            }
        }
        Err(error) => {
            warn!(error = format!("{error:#}"), "scheduling failed");
            TurnResult {
                text: "Не получилось поставить напоминание.".into(),
                directives: Vec::new(),
                route: "schedule_error",
                first_part_ms: None,
                should_listen: false,
            }
        }
    })
}

fn render(functions: &StationFunctions, actions: &[DeviceAction]) -> Vec<Directive> {
    actions
        .iter()
        .filter_map(|action| {
            let directive = functions.directive(action);
            if directive.is_none() {
                warn!(?action, "station cannot render the action; dropped");
            }
            directive
        })
        .collect()
}

fn log_turn(event: &InboundEvent, result: &TurnResult, started: Instant) {
    info!(
        turn_id = %event.turn_id,
        device = %event.device_id,
        route = result.route,
        ms = started.elapsed().as_millis() as u64,
        directives = result.directives.len(),
        first_part_ms = result.first_part_ms,
        should_listen = result.should_listen,
        text = %event.text,
        reply = %result.text,
        "turn"
    );
}

pub fn inbound_event(
    identity: &Identity,
    functions: &StationFunctions,
    duid: &str,
    text: &str,
) -> InboundEvent {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let room = identity
        .rooms
        .iter()
        .find(|(device, _)| device == duid)
        .map(|(_, room)| room.clone());
    InboundEvent {
        contract_version: CONTRACT_VERSION,
        request_id: turn_id.clone(),
        turn_id,
        timestamp_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        input_kind: InputKind::Voice,
        text: strip_wake_word(text).to_owned(),
        stt_confidence: None,
        device_id: duid.to_owned(),
        room,
        // D-23: voice devices speak for the household, not for a person.
        principal: format!("household:{}", identity.household),
        household: identity.household.clone(),
        // One core session per station per Moscow day: follow-ups work, but
        // history (and prefill) does not grow without bound.
        session_key: station_session_key(duid, chrono::Utc::now()),
        locale: identity.locale.clone(),
        timezone: identity.timezone.clone(),
        capabilities: functions.capabilities(),
        shadow: false,
    }
}

pub fn station_session_key(duid: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let moscow = now.with_timezone(&chrono::FixedOffset::east_opt(3 * 3600).expect("valid offset"));
    format!("station:{duid}:{}", moscow.format("%Y-%m-%d"))
}

/// The wake word is an interface detail: "Алиса, сделай громче" → "сделай громче".
pub fn strip_wake_word(text: &str) -> &str {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    for form in ["алиса", "алекса"] {
        if let Some(rest) = lower.strip_prefix(form) {
            let rest = rest.trim_start_matches([' ', ',', '!', '.', ':', ';']);
            if rest.is_empty() || lower.len() != trimmed.len() {
                continue;
            }
            return trimmed[trimmed.len() - rest.len()..].trim();
        }
    }
    trimmed
}

fn device_id(metadata: &Metadata, default_duid: Option<&str>) -> String {
    metadata
        .internal
        .get("deviceId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or(default_duid)
        .unwrap_or("anonymous")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_wake_word_only_as_a_prefix() {
        assert_eq!(strip_wake_word("Алиса, сделай громче"), "сделай громче");
        assert_eq!(
            strip_wake_word("  алекса! таймер на 5 минут "),
            "таймер на 5 минут"
        );
        assert_eq!(strip_wake_word("Алиса"), "Алиса");
        assert_eq!(strip_wake_word("позови Алису"), "позови Алису");
        assert_eq!(strip_wake_word("   "), "");
    }

    #[test]
    fn device_id_prefers_metadata() {
        let metadata = Metadata {
            internal: json!({"deviceId": "duid-1"}),
        };
        assert_eq!(device_id(&metadata, Some("default")), "duid-1");
        assert_eq!(device_id(&Metadata::default(), Some("default")), "default");
        assert_eq!(device_id(&Metadata::default(), None), "anonymous");
    }

    #[test]
    fn inbound_event_is_contract_valid_and_protocol_free() {
        let identity = Identity {
            rooms: vec![("duid-1".into(), "кухня".into())],
            ..Default::default()
        };
        let event = inbound_event(
            &identity,
            &StationFunctions::default(),
            "duid-1",
            " привет ",
        );
        event.validate().unwrap();
        assert_eq!(event.text, "привет");
        assert_eq!(event.room.as_deref(), Some("кухня"));
        assert_eq!(event.principal, "household:home");
        assert!(event.session_key.starts_with("station:duid-1:"));
        let late = chrono::DateTime::parse_from_rfc3339("2026-11-08T21:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // 00:30 in Moscow is already the next day.
        assert_eq!(station_session_key("d", late), "station:d:2026-11-09");
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("smart_speaker"));
    }
}

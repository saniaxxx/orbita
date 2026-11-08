//! MCP facade for home state and Wiren Board commands.
//!
//! The real backend (`HOME_MCP_SIMULATED=0`) subscribes to Wiren Board MQTT,
//! keeps a state cache and publishes commands through `orbita-home`; the agent
//! never sees MQTT credentials or raw topics. `HOME_MCP_SIMULATED=1` runs the
//! same contract against an in-memory fake for shadow runs.

mod fake;
mod real;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{Json, Router, routing::get};
use orbita_home::Risk;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use fake::FakeHome;
pub use real::RealHome;

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub risk: Risk,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub room: String,
    pub kind: String,
    /// Capability name to its last reported value, typed by capability kind.
    pub state: Value,
    pub updated_ms: u64,
    /// No message within the staleness window. Retained Wiren Board state goes
    /// quiet when nothing changes, so this is an age signal, not availability.
    pub stale: bool,
    /// A value has been reported and the device has no `meta/error`.
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub writable: Vec<String>,
    pub capabilities: Vec<CapabilityInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HomeEvent {
    pub cursor: u64,
    pub kind: String,
    pub device: String,
    pub capability: String,
    pub value: Value,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Executed,
    Simulated,
    Denied,
    Failed,
    Conflict,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub status: CommandStatus,
    pub device: String,
    pub capability: String,
    pub state: Option<Value>,
    pub reason: Option<String>,
    pub simulated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventsPage {
    pub epoch: u64,
    pub cursor_reset: bool,
    pub events: Vec<HomeEvent>,
    pub next_cursor: u64,
}

/// What the MCP tools need from a home backend. `FakeHome` is used for shadow
/// runs, `RealHome` for Wiren Board over MQTT.
#[async_trait]
pub trait Backend: Send + Sync {
    fn simulated(&self) -> bool;
    async fn devices(&self, room: Option<&str>, kind: Option<&str>) -> Vec<Device>;
    async fn state(&self, device_id: &str) -> Result<Device, String>;
    async fn command(&self, params: &CommandParams) -> CommandResult;
    async fn scenes(&self) -> Vec<String>;
    async fn scene_run(&self, params: &SceneParams) -> CommandResult;
    async fn events(&self, params: &EventsParams) -> EventsPage;
    async fn health(&self) -> Value;
}

#[derive(Debug, Clone)]
struct CachedResponse {
    fingerprint: String,
    result: CommandResult,
}

#[derive(Debug, Default)]
struct CommandCache {
    responses: Mutex<HashMap<String, CachedResponse>>,
}

impl CommandCache {
    fn check(&self, params: &CommandParams, fingerprint: &str) -> Option<CommandResult> {
        let responses = self.responses.lock().unwrap_or_else(|e| e.into_inner());
        let cached = responses.get(&params.request_id)?;
        if cached.fingerprint == fingerprint {
            return Some(cached.result.clone());
        }
        Some(CommandResult {
            status: CommandStatus::Conflict,
            device: params.device_id.clone(),
            capability: params.capability.clone(),
            state: None,
            reason: Some("request_id reused with different arguments".into()),
            simulated: cached.result.simulated,
        })
    }

    fn remember(&self, params: &CommandParams, fingerprint: String, result: &CommandResult) {
        if result.status == CommandStatus::Failed {
            return;
        }
        let mut responses = self.responses.lock().unwrap_or_else(|e| e.into_inner());
        responses.insert(
            params.request_id.clone(),
            CachedResponse {
                fingerprint,
                result: result.clone(),
            },
        );
        while responses.len() > 1024 {
            if let Some(key) = responses.keys().next().cloned() {
                responses.remove(&key);
            }
        }
    }
}

#[derive(Clone)]
pub struct HomeServer {
    tool_router: ToolRouter<Self>,
    backend: Arc<dyn Backend>,
    cache: Arc<CommandCache>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct DevicesParams {
    pub room: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct StateParams {
    pub device_id: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CommandParams {
    pub device_id: String,
    pub capability: String,
    pub value: Value,
    /// Idempotency key from the core.
    pub request_id: String,
    /// Reserved for out-of-band approval. Dangerous capabilities are denied
    /// regardless of this value until the approval channel exists.
    pub confirm: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SceneParams {
    pub name: String,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct EventsParams {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub epoch: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn room_matches(actual: &str, query: &str) -> bool {
    let stem = |value: &str| {
        value
            .to_lowercase()
            .trim_end_matches(['а', 'я', 'е', 'у', 'ю', 'ы', 'и', 'о', 'й', 'ь'])
            .to_owned()
    };
    stem(actual) == stem(query)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Parse a JSON value from a tool call into a domain action.
pub(crate) fn parse_action(kind: &str, value: &Value) -> Result<orbita_home::Action, String> {
    match kind {
        "bool" => parse_bool(value)
            .map(|on| orbita_home::Action::OnOff { on })
            .ok_or_else(|| "value must be a boolean (true/false, on/off, 1/0)".to_owned()),
        "number" => parse_number(value)
            .map(|value| orbita_home::Action::Number { value })
            .ok_or_else(|| "value must be a finite number".to_owned()),
        "enum" => value
            .as_str()
            .or_else(|| value.get("value").and_then(Value::as_str))
            .map(|value| orbita_home::Action::Enum {
                value: value.to_owned(),
            })
            .ok_or_else(|| "value must be a string".to_owned()),
        other => Err(format!("unsupported capability type {other}")),
    }
}

/// The JSON form of an action, used when the fake backend updates its cache.
pub(crate) fn action_value(action: &orbita_home::Action) -> Value {
    match action {
        orbita_home::Action::OnOff { on } => Value::Bool(*on),
        orbita_home::Action::Number { value } => json!(value),
        orbita_home::Action::Enum { value } => Value::String(value.clone()),
        orbita_home::Action::Status => Value::Null,
    }
}

fn parse_bool(value: &Value) -> Option<bool> {
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    if let Some(value) = value.as_f64() {
        return Some(value != 0.0);
    }
    if let Some(value) = value.as_str() {
        return parse_bool_str(value);
    }
    value.get("on").and_then(parse_bool)
}

pub(crate) fn parse_bool_str(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("on")
        || value.eq_ignore_ascii_case("вкл")
    {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("выкл")
    {
        return Some(false);
    }
    match value.parse::<f64>() {
        Ok(value) if (value - 1.0).abs() <= 1e-6 => Some(true),
        Ok(value) if value.abs() <= 1e-6 => Some(false),
        _ => None,
    }
}

fn parse_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
        .or_else(|| value.get("value").and_then(parse_number))
        .filter(|value| value.is_finite())
}

impl HomeServer {
    /// In-memory backend for shadow runs and offline tests.
    pub fn new(simulated: bool) -> Self {
        Self::with_backend(Arc::new(FakeHome::seeded(simulated)))
    }

    pub fn with_backend(backend: Arc<dyn Backend>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            backend,
            cache: Arc::new(CommandCache::default()),
        }
    }

    /// Real Wiren Board backend over MQTT.
    pub async fn connect(config_path: &str, ack_timeout: Duration) -> Result<Self, String> {
        let backend = RealHome::start(config_path, ack_timeout).await?;
        Ok(Self::with_backend(Arc::new(backend)))
    }

    async fn run_command(&self, params: CommandParams) -> CommandResult {
        let fingerprint = format!(
            "{}\0{}\0{}",
            params.device_id,
            params.capability,
            serde_json::to_string(&params.value).unwrap_or_default()
        );
        if let Some(result) = self.cache.check(&params, &fingerprint) {
            return result;
        }
        let result = self.backend.command(&params).await;
        self.cache.remember(&params, fingerprint, &result);
        result
    }
}

#[tool_router(router = tool_router)]
impl HomeServer {
    #[tool(description = "Список устройств дома, опционально по комнате или типу.")]
    pub async fn devices(&self, Parameters(params): Parameters<DevicesParams>) -> String {
        let devices = self
            .backend
            .devices(params.room.as_deref(), params.kind.as_deref())
            .await;
        serde_json::to_string(&devices).unwrap_or_else(|_| "[]".into())
    }

    #[tool(description = "Состояние одного устройства дома.")]
    pub async fn state(&self, Parameters(params): Parameters<StateParams>) -> String {
        match self.backend.state(&params.device_id).await {
            Ok(device) => serde_json::to_string(&device).unwrap_or_else(|_| "{}".into()),
            Err(error) => format!("Ошибка: {error}"),
        }
    }

    #[tool(
        description = "Команда устройству. Опасные capability запрещены (deny by default); request_id делает команду идемпотентной."
    )]
    pub async fn command(&self, Parameters(params): Parameters<CommandParams>) -> String {
        let result = self.run_command(params).await;
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "Список именованных сцен. Сейчас сцен нет.")]
    pub async fn scenes(&self) -> String {
        serde_json::to_string(&self.backend.scenes().await).unwrap_or_else(|_| "[]".into())
    }

    #[tool(description = "Запустить именованную сцену. Сцены ещё не настроены.")]
    pub async fn scene_run(&self, Parameters(params): Parameters<SceneParams>) -> String {
        let result = self.backend.scene_run(&params).await;
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "События дома после cursor.")]
    pub async fn events(&self, Parameters(params): Parameters<EventsParams>) -> String {
        let page = self.backend.events(&params).await;
        serde_json::to_string(&page).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "Здоровье home-MCP: backend, broker, устройства.")]
    pub async fn health(&self) -> String {
        self.backend.health().await.to_string()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HomeServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("home", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Дом: состояние и команды через Wiren Board MQTT. Никаких прямых топиков; опасные действия запрещены без подтверждения.",
            )
    }
}

pub fn build_router(server: HomeServer) -> Router {
    let service: StreamableHttpService<HomeServer, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let server = server.clone();
                move || Ok(server.clone())
            },
            Default::default(),
            orbita_mcp_http::server_config(),
        );
    Router::new()
        .route("/health", get(http_health))
        .nest_service("/mcp", service)
}

async fn http_health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "home-mcp"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(device_id: &str, value: Value, request_id: &str) -> CommandParams {
        CommandParams {
            device_id: device_id.into(),
            capability: "on_off".into(),
            value,
            request_id: request_id.into(),
            confirm: None,
        }
    }

    #[tokio::test]
    async fn commands_update_state_and_are_idempotent() {
        let server = HomeServer::new(false);
        let params = command("light_hall", json!(true), "req-1");
        let first = server.command(Parameters(params.clone())).await;
        assert!(first.contains("\"executed\""));
        let second = server.command(Parameters(params)).await;
        assert!(second.contains("\"executed\""));
        let events = server
            .events(Parameters(EventsParams {
                cursor: 0,
                epoch: None,
                limit: None,
            }))
            .await;
        assert_eq!(events.matches("home.state_changed").count(), 1);
        let state = server
            .state(Parameters(StateParams {
                device_id: "light_hall".into(),
            }))
            .await;
        assert!(state.contains("\"on_off\":true"));
    }

    #[tokio::test]
    async fn same_request_id_with_other_value_conflicts() {
        let server = HomeServer::new(false);
        let first = server
            .command(Parameters(command(
                "light_hall",
                json!(true),
                "req-conflict",
            )))
            .await;
        assert!(first.contains("\"executed\""));
        let second = server
            .command(Parameters(command(
                "light_hall",
                json!(false),
                "req-conflict",
            )))
            .await;
        assert!(second.contains("\"conflict\""));
    }

    #[tokio::test]
    async fn dangerous_lock_is_denied_even_with_confirm() {
        let server = HomeServer::new(false);
        let result = server
            .command(Parameters(CommandParams {
                device_id: "lock_front".into(),
                capability: "lock".into(),
                value: json!(false),
                request_id: "req-lock".into(),
                confirm: Some("approved".into()),
            }))
            .await;
        assert!(result.contains("\"denied\""));
    }

    #[tokio::test]
    async fn simulated_commands_do_not_mutate_state() {
        let server = HomeServer::new(true);
        let result = server
            .command(Parameters(command("light_hall", json!(true), "req-shadow")))
            .await;
        assert!(result.contains("\"simulated\""));
        let state = server
            .state(Parameters(StateParams {
                device_id: "light_hall".into(),
            }))
            .await;
        assert!(state.contains("\"on_off\":false"));
    }

    #[tokio::test]
    async fn scenes_are_not_configured() {
        let server = HomeServer::new(false);
        assert_eq!(server.scenes().await, "[]");
        let result = server
            .scene_run(Parameters(SceneParams {
                name: "утро".into(),
                request_id: None,
            }))
            .await;
        assert!(result.contains("\"denied\""));
    }

    #[tokio::test]
    async fn devices_match_russian_case_forms() {
        let server = HomeServer::new(false);
        let devices = server
            .devices(Parameters(DevicesParams {
                room: Some("детской".into()),
                kind: Some("LIGHT".into()),
            }))
            .await;
        assert!(devices.contains("light_kids"));
        assert!(!devices.contains("light_hall"));
    }

    #[tokio::test]
    async fn events_advance_and_detect_epoch_reset() {
        let server = HomeServer::new(false);
        server
            .command(Parameters(command("light_hall", json!(true), "req-evt")))
            .await;
        let events = server
            .events(Parameters(EventsParams {
                cursor: 0,
                epoch: None,
                limit: None,
            }))
            .await;
        assert!(events.contains("home.state_changed"));
        assert!(events.contains("\"next_cursor\":1"));

        let reset = server
            .events(Parameters(EventsParams {
                cursor: 1,
                epoch: Some(1),
                limit: None,
            }))
            .await;
        assert!(reset.contains("\"cursor_reset\":true"));
    }
}

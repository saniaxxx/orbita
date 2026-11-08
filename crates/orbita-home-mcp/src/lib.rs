//! MCP facade for home state and wb-rules commands.
//!
//! This is the offline skeleton: it owns the agent-facing contract and policy
//! checks, but its backend is an in-memory fake. The real backend will publish
//! commands to wb-rules and confirm them through MQTT state topics; the agent
//! still never talks MQTT directly.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, Router, routing::get};
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

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub room: String,
    pub kind: String,
    pub state: Value,
    pub updated_ms: u64,
    pub stale: bool,
    pub writable: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Safe,
    Confirm,
    Deny,
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

#[derive(Debug, Clone)]
struct CachedResponse {
    fingerprint: String,
    result: CommandResult,
}

#[derive(Debug)]
pub struct FakeHome {
    devices: BTreeMap<String, Device>,
    events: VecDeque<HomeEvent>,
    next_cursor: u64,
    epoch: u64,
    scene_counter: u64,
    responses: HashMap<String, CachedResponse>,
    simulated: bool,
}

impl FakeHome {
    pub fn seeded(simulated: bool) -> Self {
        let now = now_ms();
        let mut devices = BTreeMap::new();
        for (id, room, kind, writable, state) in [
            (
                "light_hall",
                "коридор",
                "light",
                vec!["on_off".to_owned()],
                json!({"on": false}),
            ),
            (
                "light_kids",
                "детская",
                "light",
                vec!["on_off".to_owned()],
                json!({"on": false}),
            ),
            (
                "climate_hall",
                "коридор",
                "temperature",
                vec![],
                json!({"temperature": 21.5}),
            ),
            (
                "lock_front",
                "прихожая",
                "lock",
                vec![],
                json!({"locked": true}),
            ),
        ] {
            devices.insert(
                id.to_owned(),
                Device {
                    id: id.to_owned(),
                    room: room.to_owned(),
                    kind: kind.to_owned(),
                    state,
                    updated_ms: now,
                    stale: false,
                    writable,
                },
            );
        }
        Self {
            devices,
            events: VecDeque::new(),
            next_cursor: 1,
            epoch: now_ms(),
            scene_counter: 0,
            responses: HashMap::new(),
            simulated,
        }
    }

    fn command(
        &mut self,
        device_id: &str,
        capability: &str,
        value: Value,
        request_id: &str,
        confirm: Option<&str>,
    ) -> CommandResult {
        let fingerprint = format!("{device_id}\0{capability}\0{value}");
        if let Some(cached) = self.responses.get(request_id) {
            if cached.fingerprint == fingerprint {
                return cached.result.clone();
            }
            return CommandResult {
                status: CommandStatus::Conflict,
                device: device_id.into(),
                capability: capability.into(),
                state: None,
                reason: Some("request_id reused with different arguments".into()),
                simulated: self.simulated,
            };
        }
        let Some(device) = self.devices.get(device_id) else {
            return CommandResult {
                status: CommandStatus::Failed,
                device: device_id.into(),
                capability: capability.into(),
                state: None,
                reason: Some("unknown device".into()),
                simulated: self.simulated,
            };
        };
        let policy = policy_for(&device.kind, capability);
        if policy == Policy::Deny {
            return self.finish(
                request_id,
                fingerprint,
                CommandResult {
                    status: CommandStatus::Denied,
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("dangerous capability is deny-by-default".into()),
                    simulated: self.simulated,
                },
            );
        }
        if policy == Policy::Confirm && confirm != Some("approved") {
            return self.finish(
                request_id,
                fingerprint,
                CommandResult {
                    status: CommandStatus::Denied,
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("out-of-band confirmation is required".into()),
                    simulated: self.simulated,
                },
            );
        }
        if !device.writable.iter().any(|item| item == capability) {
            return self.finish(
                request_id,
                fingerprint,
                CommandResult {
                    status: CommandStatus::Denied,
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("capability is not writable".into()),
                    simulated: self.simulated,
                },
            );
        }
        let Some(state) = merged_state(&device.state, capability, &value) else {
            return self.finish(
                request_id,
                fingerprint,
                CommandResult {
                    status: CommandStatus::Denied,
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("value does not match the capability schema".into()),
                    simulated: self.simulated,
                },
            );
        };
        if self.simulated {
            return self.finish(
                request_id,
                fingerprint,
                CommandResult {
                    status: CommandStatus::Simulated,
                    device: device_id.into(),
                    capability: capability.into(),
                    state: Some(state),
                    reason: None,
                    simulated: true,
                },
            );
        }
        let timestamp_ms = now_ms();
        if let Some(device) = self.devices.get_mut(device_id) {
            device.state = state.clone();
            device.updated_ms = timestamp_ms;
            device.stale = false;
        }
        let event = HomeEvent {
            cursor: self.next_cursor,
            kind: "home.state_changed".into(),
            device: device_id.into(),
            capability: capability.into(),
            value: value.clone(),
            timestamp_ms,
        };
        self.next_cursor += 1;
        self.events.push_back(event);
        while self.events.len() > 1024 {
            self.events.pop_front();
        }
        self.finish(
            request_id,
            fingerprint,
            CommandResult {
                status: CommandStatus::Executed,
                device: device_id.into(),
                capability: capability.into(),
                state: Some(state),
                reason: None,
                simulated: false,
            },
        )
    }

    fn finish(
        &mut self,
        request_id: &str,
        fingerprint: String,
        result: CommandResult,
    ) -> CommandResult {
        if result.status != CommandStatus::Failed {
            self.responses.insert(
                request_id.to_owned(),
                CachedResponse {
                    fingerprint,
                    result: result.clone(),
                },
            );
            while self.responses.len() > 1024 {
                if let Some(key) = self.responses.keys().next().cloned() {
                    self.responses.remove(&key);
                }
            }
        }
        result
    }
}

fn policy_for(kind: &str, capability: &str) -> Policy {
    if kind == "lock" || capability == "lock" {
        return Policy::Deny;
    }
    if matches!(kind, "boiler" | "gate" | "intercom") {
        return Policy::Confirm;
    }
    Policy::Safe
}

fn merged_state(current: &Value, capability: &str, value: &Value) -> Option<Value> {
    let mut state = current.as_object()?.clone();
    if capability == "on_off" {
        let on = value
            .as_bool()
            .or_else(|| value.get("on").and_then(Value::as_bool))?;
        state.insert("on".into(), Value::Bool(on));
    } else if let Some(fields) = value.as_object() {
        for (key, value) in fields {
            state.insert(key.clone(), value.clone());
        }
    } else {
        state.insert(capability.to_owned(), value.clone());
    }
    Some(Value::Object(state))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn scene(name: &str) -> Option<(&'static str, Value)> {
    match name {
        "ухожу" => Some(("light_hall", json!({"on": false}))),
        "кино" => Some(("light_kids", json!({"on": true}))),
        "утро" => Some(("light_hall", json!({"on": true}))),
        _ => None,
    }
}

fn scene_names() -> Vec<&'static str> {
    vec!["ухожу", "кино", "утро"]
}

#[derive(Clone)]
pub struct HomeServer {
    tool_router: ToolRouter<Self>,
    home: Arc<Mutex<FakeHome>>,
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
    /// Out-of-band confirmation token; `approved` is the fake test token.
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

impl HomeServer {
    pub fn new(simulated: bool) -> Self {
        Self {
            tool_router: Self::tool_router(),
            home: Arc::new(Mutex::new(FakeHome::seeded(simulated))),
        }
    }

    pub fn shared(mut self, home: Arc<Mutex<FakeHome>>) -> Self {
        self.home = home;
        self
    }
}

#[tool_router(router = tool_router)]
impl HomeServer {
    #[tool(description = "Список устройств дома, опционально по комнате или типу.")]
    pub async fn devices(&self, Parameters(params): Parameters<DevicesParams>) -> String {
        let home = self.home.lock().expect("home lock");
        let devices: Vec<_> = home
            .devices
            .values()
            .filter(|device| {
                params
                    .room
                    .as_deref()
                    .is_none_or(|room| room_matches(&device.room, room))
                    && params
                        .kind
                        .as_deref()
                        .is_none_or(|kind| device.kind.eq_ignore_ascii_case(kind))
            })
            .collect();
        serde_json::to_string(&devices).unwrap_or_else(|_| "[]".into())
    }

    #[tool(description = "Состояние одного устройства дома.")]
    pub async fn state(&self, Parameters(params): Parameters<StateParams>) -> String {
        let home = self.home.lock().expect("home lock");
        match home.devices.get(&params.device_id) {
            Some(device) => serde_json::to_string(device).unwrap_or_else(|_| "{}".into()),
            None => format!("Ошибка: неизвестное устройство {}", params.device_id),
        }
    }

    #[tool(
        description = "Команда устройству. Для опасных capability нужен out-of-band confirm; в fake lock запрещён."
    )]
    pub async fn command(&self, Parameters(params): Parameters<CommandParams>) -> String {
        let mut home = self.home.lock().expect("home lock");
        let result = home.command(
            &params.device_id,
            &params.capability,
            params.value,
            &params.request_id,
            params.confirm.as_deref(),
        );
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "Список именованных сцен wb-rules.")]
    pub async fn scenes(&self) -> String {
        serde_json::to_string(&scene_names()).unwrap_or_else(|_| "[]".into())
    }

    #[tool(description = "Запустить именованную сцену wb-rules.")]
    pub async fn scene_run(&self, Parameters(params): Parameters<SceneParams>) -> String {
        let Some((device, value)) = scene(&params.name) else {
            return format!("Ошибка: неизвестная сцена {}", params.name);
        };
        let mut home = self.home.lock().expect("home lock");
        let request_id = params.request_id.unwrap_or_else(|| {
            home.scene_counter += 1;
            format!(
                "scene:{}:{}:{}",
                home.epoch, home.scene_counter, params.name
            )
        });
        let result = home.command(device, "on_off", value, &request_id, None);
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "События дома после cursor.")]
    pub async fn events(&self, Parameters(params): Parameters<EventsParams>) -> String {
        let home = self.home.lock().expect("home lock");
        if params.epoch.is_some_and(|epoch| epoch != home.epoch) {
            return json!({
                "cursor_reset": true,
                "epoch": home.epoch,
                "events": [],
                "next_cursor": home.next_cursor,
            })
            .to_string();
        }
        let limit = params.limit.unwrap_or(50).min(200);
        let events: Vec<_> = home
            .events
            .iter()
            .filter(|event| event.cursor > params.cursor)
            .take(limit)
            .collect();
        json!({
            "epoch": home.epoch,
            "cursor_reset": false,
            "events": events,
            "next_cursor": events.last().map(|event| event.cursor).unwrap_or(params.cursor),
        })
        .to_string()
    }

    #[tool(description = "Здоровье home-MCP: backend, broker, rules.")]
    pub async fn health(&self) -> String {
        let home = self.home.lock().expect("home lock");
        json!({
            "status": "ok",
            "backend": "fake",
            "broker": "fake",
            "rules": "fake",
            "epoch": home.epoch,
            "next_cursor": home.next_cursor,
        })
        .to_string()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HomeServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("home", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Дом: состояние и команды через wb-rules. Никаких прямых MQTT-топиков; опасные действия запрещены без подтверждения.",
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
        let params = command("light_hall", json!({"on": true}), "req-1");
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
        assert!(state.contains("\"on\":true"));
    }

    #[tokio::test]
    async fn same_request_id_with_other_value_conflicts() {
        let server = HomeServer::new(false);
        let first = server
            .command(Parameters(command(
                "light_hall",
                json!({"on": true}),
                "req-conflict",
            )))
            .await;
        assert!(first.contains("\"executed\""));
        let second = server
            .command(Parameters(command(
                "light_hall",
                json!({"on": false}),
                "req-conflict",
            )))
            .await;
        assert!(second.contains("\"conflict\""));
    }

    #[tokio::test]
    async fn dangerous_lock_is_denied() {
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
            .command(Parameters(command(
                "light_hall",
                json!({"on": true}),
                "req-shadow",
            )))
            .await;
        assert!(result.contains("\"simulated\""));
        let state = server
            .state(Parameters(StateParams {
                device_id: "light_hall".into(),
            }))
            .await;
        assert!(state.contains("\"on\":false"));
    }

    #[tokio::test]
    async fn scene_runs_again_with_a_fresh_request() {
        let server = HomeServer::new(false);
        for _ in 0..2 {
            let result = server
                .scene_run(Parameters(SceneParams {
                    name: "утро".into(),
                    request_id: None,
                }))
                .await;
            assert!(result.contains("\"executed\""));
        }
        let events = server
            .events(Parameters(EventsParams {
                cursor: 0,
                epoch: None,
                limit: None,
            }))
            .await;
        assert_eq!(events.matches("home.state_changed").count(), 2);
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
            .command(Parameters(command(
                "light_hall",
                json!({"on": true}),
                "req-evt",
            )))
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

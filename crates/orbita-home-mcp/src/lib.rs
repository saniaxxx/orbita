//! MCP facade for home state and wb-rules commands.
//!
//! This is the offline skeleton: it owns the agent-facing contract and policy
//! checks, but its backend is an in-memory fake. The real backend will publish
//! commands to wb-rules and confirm them through MQTT state topics; the agent
//! still never talks MQTT directly.

use std::{
    collections::{BTreeMap, HashMap},
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
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
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

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub status: String,
    pub device: String,
    pub capability: String,
    pub state: Option<Value>,
    pub reason: Option<String>,
    pub simulated: bool,
}

#[derive(Debug)]
pub struct FakeHome {
    devices: BTreeMap<String, Device>,
    events: Vec<HomeEvent>,
    next_cursor: u64,
    responses: HashMap<String, CommandResult>,
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
            events: Vec::new(),
            next_cursor: 1,
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
        if let Some(result) = self.responses.get(request_id) {
            return result.clone();
        }
        let Some(device) = self.devices.get_mut(device_id) else {
            return self.finish(
                request_id,
                CommandResult {
                    status: "failed".into(),
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("unknown device".into()),
                    simulated: self.simulated,
                },
            );
        };
        if device.kind == "lock" || capability == "lock" {
            return self.finish(
                request_id,
                CommandResult {
                    status: "denied".into(),
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("dangerous capability requires out-of-band approval".into()),
                    simulated: self.simulated,
                },
            );
        }
        if !device.writable.iter().any(|item| item == capability) {
            return self.finish(
                request_id,
                CommandResult {
                    status: "denied".into(),
                    device: device_id.into(),
                    capability: capability.into(),
                    state: None,
                    reason: Some("capability is not writable".into()),
                    simulated: self.simulated,
                },
            );
        }
        if self.simulated {
            return self.finish(
                request_id,
                CommandResult {
                    status: "simulated".into(),
                    device: device_id.into(),
                    capability: capability.into(),
                    state: Some(value),
                    reason: None,
                    simulated: true,
                },
            );
        }
        let _ = confirm;
        device.state = value.clone();
        device.updated_ms = now_ms();
        device.stale = false;
        let event = HomeEvent {
            cursor: self.next_cursor,
            kind: "home.state_changed".into(),
            device: device_id.into(),
            capability: capability.into(),
            value: value.clone(),
            timestamp_ms: device.updated_ms,
        };
        self.next_cursor += 1;
        self.events.push(event);
        self.finish(
            request_id,
            CommandResult {
                status: "executed".into(),
                device: device_id.into(),
                capability: capability.into(),
                state: Some(value),
                reason: None,
                simulated: false,
            },
        )
    }

    fn finish(&mut self, request_id: &str, result: CommandResult) -> CommandResult {
        self.responses.insert(request_id.to_owned(), result.clone());
        result
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
    pub device: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CommandParams {
    pub device: String,
    pub capability: String,
    pub value: Value,
    /// Idempotency key from the core.
    pub request_id: String,
    /// Out-of-band confirmation token; not implemented in the fake.
    pub confirm: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SceneParams {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct EventsParams {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub limit: Option<usize>,
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
                    .is_none_or(|room| device.room == room)
                    && params
                        .kind
                        .as_deref()
                        .is_none_or(|kind| device.kind == kind)
            })
            .collect();
        serde_json::to_string(&devices).unwrap_or_else(|_| "[]".into())
    }

    #[tool(description = "Состояние одного устройства дома.")]
    pub async fn state(&self, Parameters(params): Parameters<StateParams>) -> String {
        let home = self.home.lock().expect("home lock");
        match home.devices.get(&params.device) {
            Some(device) => serde_json::to_string(device).unwrap_or_else(|_| "{}".into()),
            None => format!("Ошибка: неизвестное устройство {}", params.device),
        }
    }

    #[tool(
        description = "Команда устройству. Для опасных capability нужен out-of-band confirm; в fake lock запрещён."
    )]
    pub async fn command(&self, Parameters(params): Parameters<CommandParams>) -> String {
        let mut home = self.home.lock().expect("home lock");
        let result = home.command(
            &params.device,
            &params.capability,
            params.value,
            &params.request_id,
            params.confirm.as_deref(),
        );
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "Список именованных сцен wb-rules.")]
    pub async fn scenes(&self) -> String {
        r#"["ухожу","кино","утро"]"#.into()
    }

    #[tool(description = "Запустить именованную сцену wb-rules.")]
    pub async fn scene_run(&self, Parameters(params): Parameters<SceneParams>) -> String {
        let mut home = self.home.lock().expect("home lock");
        let (device, value) = match params.name.as_str() {
            "ухожу" => ("light_hall", json!({"on": false})),
            "кино" => ("light_kids", json!({"on": true})),
            "утро" => ("light_hall", json!({"on": true})),
            _ => return format!("Ошибка: неизвестная сцена {}", params.name),
        };
        let result = home.command(
            device,
            "on_off",
            value,
            &format!("scene:{}", params.name),
            None,
        );
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    #[tool(description = "События дома после cursor.")]
    pub async fn events(&self, Parameters(params): Parameters<EventsParams>) -> String {
        let home = self.home.lock().expect("home lock");
        let limit = params.limit.unwrap_or(50).min(200);
        let events: Vec<_> = home
            .events
            .iter()
            .filter(|event| event.cursor > params.cursor)
            .take(limit)
            .collect();
        json!({
            "events": events,
            "next_cursor": events.last().map(|event| event.cursor).unwrap_or(params.cursor),
        })
        .to_string()
    }

    #[tool(description = "Здоровье home-MCP: backend, broker, rules.")]
    pub async fn health(&self) -> String {
        json!({"status": "ok", "backend": "fake", "broker": "fake", "rules": "fake"}).to_string()
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
            StreamableHttpServerConfig::default(),
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

    #[tokio::test]
    async fn commands_update_state_and_are_idempotent() {
        let server = HomeServer::new(false);
        let params = CommandParams {
            device: "light_hall".into(),
            capability: "on_off".into(),
            value: json!({"on": true}),
            request_id: "req-1".into(),
            confirm: None,
        };
        let first = server.command(Parameters(params.clone())).await;
        assert!(first.contains("\"executed\""));
        let second = server.command(Parameters(params)).await;
        assert!(second.contains("\"executed\""));
        let state = server
            .state(Parameters(StateParams {
                device: "light_hall".into(),
            }))
            .await;
        assert!(state.contains("\"on\":true"));
    }

    #[tokio::test]
    async fn dangerous_lock_is_denied() {
        let server = HomeServer::new(false);
        let result = server
            .command(Parameters(CommandParams {
                device: "lock_front".into(),
                capability: "lock".into(),
                value: json!(false),
                request_id: "req-lock".into(),
                confirm: Some("1234".into()),
            }))
            .await;
        assert!(result.contains("\"denied\""));
    }

    #[tokio::test]
    async fn simulated_commands_do_not_mutate_state() {
        let server = HomeServer::new(true);
        let result = server
            .command(Parameters(CommandParams {
                device: "light_hall".into(),
                capability: "on_off".into(),
                value: json!({"on": true}),
                request_id: "req-shadow".into(),
                confirm: None,
            }))
            .await;
        assert!(result.contains("\"simulated\""));
        let state = server
            .state(Parameters(StateParams {
                device: "light_hall".into(),
            }))
            .await;
        assert!(state.contains("\"on\":false"));
    }

    #[tokio::test]
    async fn events_advance_cursor() {
        let server = HomeServer::new(false);
        server
            .command(Parameters(CommandParams {
                device: "light_hall".into(),
                capability: "on_off".into(),
                value: json!({"on": true}),
                request_id: "req-evt".into(),
                confirm: None,
            }))
            .await;
        let events = server
            .events(Parameters(EventsParams {
                cursor: 0,
                limit: None,
            }))
            .await;
        assert!(events.contains("home.state_changed"));
        assert!(events.contains("\"next_cursor\":1"));
    }
}

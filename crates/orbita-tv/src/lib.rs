//! MCP server for Android TV / Google TV.
//!
//! The agent joins the server name with the tool name, so these tools appear
//! as `tv_state`, `tv_power`, `tv_key`, `tv_volume`, `tv_app` and `tv_input`.
//! Pairing is an operator step; the server only uses the client certificate
//! issued for a TV.

pub mod config;
pub mod proto;
pub mod protocol;

use std::{collections::HashMap, sync::Arc, time::Duration};

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
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::{TvConfig, TvDevice};
use crate::protocol::TvLink;

const KEY_NOT_ALLOWED: &str =
    "POWER через tv_key запрещён: используй tv_power, он проверяет состояние";

#[derive(Clone)]
pub struct TvServer {
    tool_router: ToolRouter<Self>,
    config: Arc<TvConfig>,
    locks: Arc<HashMap<String, Arc<Mutex<()>>>>,
}

impl TvServer {
    pub fn new(config: TvConfig) -> Self {
        let locks = config
            .devices
            .iter()
            .map(|device| (device.id.clone(), Arc::new(Mutex::new(()))))
            .collect();
        Self {
            tool_router: Self::tool_router(),
            config: Arc::new(config),
            locks: Arc::new(locks),
        }
    }

    fn device(&self, query: Option<&str>) -> Result<&TvDevice, String> {
        self.config.resolve(query)
    }

    fn lock(&self, device: &TvDevice) -> Result<Arc<Mutex<()>>, String> {
        self.locks
            .get(&device.id)
            .cloned()
            .ok_or_else(|| format!("нет блокировки для {}", device.id))
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct DeviceParams {
    /// Телевизор: комната, id или алиас. Можно не указывать, если он один.
    pub device: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct PowerParams {
    /// Телевизор: комната, id или алиас. Можно не указывать, если он один.
    pub device: Option<String>,
    /// true — включить, false — выключить. Не toggle: состояние проверяется.
    pub on: bool,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct KeyParams {
    /// Телевизор: комната, id или алиас.
    pub device: Option<String>,
    /// Клавиша: HOME, BACK, UP, DOWN, LEFT, RIGHT, OK, PLAY_PAUSE, NEXT,
    /// PREVIOUS, STOP, HDMI1–HDMI4, TV, INPUT. POWER запрещён — используй tv_power.
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct VolumeParams {
    /// Телевизор: комната, id или алиас.
    pub device: Option<String>,
    /// up, down, mute, unmute или set.
    pub action: String,
    /// Уровень для action=set, 0–100.
    pub level: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AppParams {
    /// Телевизор: комната, id или алиас.
    pub device: Option<String>,
    /// Приложение или deep link: youtube, netflix, kinopoisk, ivi, okko или URL.
    pub app: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Телевизор: комната, id или алиас.
    pub device: Option<String>,
    /// Что искать на YouTube: «котики», «клип про кота», «как починить кран».
    pub query: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct InputParams {
    /// Телевизор: комната, id или алиас.
    pub device: Option<String>,
    /// hdmi1, hdmi2, hdmi3, hdmi4 или tv.
    pub input: String,
}

#[tool_router(router = tool_router)]
impl TvServer {
    #[tool(
        description = "Список настроенных телевизоров: id, комната, алиасы, протокол, включён ли в конфиге."
    )]
    pub async fn list(&self) -> String {
        let devices: Vec<_> = self
            .config
            .devices
            .iter()
            .map(|device| {
                json!({
                    "id": device.id,
                    "room": device.room,
                    "aliases": device.aliases,
                    "host": device.host,
                    "protocol": device.protocol,
                    "enabled": device.enabled,
                })
            })
            .collect();
        serde_json::to_string_pretty(&devices).unwrap_or_else(|_| "[]".to_owned())
    }

    #[tool(
        description = "Состояние телевизора: включён ли, громкость, текущее приложение. Не будит выключенный телевизор."
    )]
    pub async fn state(&self, Parameters(params): Parameters<DeviceParams>) -> String {
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        match TvLink::open(device).await {
            Ok(mut link) => {
                let _ = link.drain_for(Duration::from_millis(700)).await;
                let state = link.state();
                format!("{}: {}", device.name(), state.summary())
            }
            Err(error) => format!(
                "{}: не удалось подключиться ({error}); если он выключен, это нормально",
                device.name()
            ),
        }
    }

    #[tool(
        description = "Включить или выключить телевизор. Всегда указывай on=true/false, не toggle. Для включения из ждущего режима используется Wake-on-LAN, если задан MAC."
    )]
    pub async fn power(&self, Parameters(params): Parameters<PowerParams>) -> String {
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        if !device.is_remote_v2() {
            return format!(
                "{}: протокол {} пока не поддерживает включение/выключение",
                device.name(),
                device.protocol
            );
        }
        if params.on {
            let mut link = match open_with_wake(device).await {
                Ok(link) => link,
                Err(error) => return format!("{}: не включился ({error})", device.name()),
            };
            match link.set_power(true).await {
                Ok(state) => format!("{}: {}", device.name(), state.summary()),
                Err(error) => format!("{}: ошибка включения ({error})", device.name()),
            }
        } else {
            let mut link = match TvLink::open(device).await {
                Ok(link) => link,
                Err(error) => {
                    return format!("{}: уже выключен или недоступен ({error})", device.name());
                }
            };
            match link.set_power(false).await {
                Ok(state) => format!("{}: {}", device.name(), state.summary()),
                Err(error) => format!("{}: ошибка выключения ({error})", device.name()),
            }
        }
    }

    #[tool(
        description = "Нажать клавишу на телевизоре: HOME, BACK, UP, DOWN, LEFT, RIGHT, OK, PLAY_PAUSE, NEXT, PREVIOUS, STOP, HDMI1–HDMI4, TV, INPUT. POWER запрещён, для питания используй tv_power."
    )]
    pub async fn key(&self, Parameters(params): Parameters<KeyParams>) -> String {
        if params.key.trim().eq_ignore_ascii_case("power")
            || params.key.trim().eq_ignore_ascii_case("keycode_power")
        {
            return format!("Ошибка: {KEY_NOT_ALLOWED}");
        }
        let Some(code) = proto::key_code(&params.key) else {
            return format!("Ошибка: неизвестная клавиша {}", params.key);
        };
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        let mut link = match TvLink::open(device).await {
            Ok(link) => link,
            Err(error) => return format!("{}: не удалось подключиться ({error})", device.name()),
        };
        match link.send_key(code).await {
            Ok(()) => format!("{}: нажала {}", device.name(), params.key),
            Err(error) => format!("{}: ошибка клавиши ({error})", device.name()),
        }
    }

    #[tool(
        description = "Громкость телевизора: action=up/down/mute/unmute/set. Для set укажи level 0–100."
    )]
    pub async fn volume(&self, Parameters(params): Parameters<VolumeParams>) -> String {
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        let mut link = match TvLink::open(device).await {
            Ok(link) => link,
            Err(error) => return format!("{}: не удалось подключиться ({error})", device.name()),
        };
        let _ = link.drain_for(Duration::from_millis(500)).await;
        let action = params.action.to_lowercase();
        let result = match action.as_str() {
            "up" => link.volume_step(true).await,
            "down" => link.volume_step(false).await,
            "mute" | "unmute" => {
                let want_muted = action == "mute";
                if link.state().muted == Some(want_muted) {
                    Ok(link.state())
                } else {
                    match link.send_key(proto::KEYCODE_MUTE).await {
                        Ok(()) => {
                            let _ = link.drain_for(Duration::from_millis(500)).await;
                            Ok(link.state())
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            "set" => match params.level {
                Some(level) => link.set_volume(level).await,
                None => Err("для action=set нужен level".to_owned()),
            },
            _ => Err(format!("неизвестное действие громкости {}", params.action)),
        };
        match result {
            Ok(state) => format!("{}: {}", device.name(), state.summary()),
            Err(error) => format!("{}: ошибка громкости ({error})", device.name()),
        }
    }

    #[tool(
        description = "Запустить приложение на телевизоре по имени или deep link: youtube, netflix, kinopoisk, ivi, okko или URL."
    )]
    pub async fn app(&self, Parameters(params): Parameters<AppParams>) -> String {
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        let link = app_link(&params.app);
        let mut client = match TvLink::open(device).await {
            Ok(link) => link,
            Err(error) => return format!("{}: не удалось подключиться ({error})", device.name()),
        };
        match client.send_app_link(&link).await {
            Ok(()) => format!("{}: запускаю {}", device.name(), params.app),
            Err(error) => format!("{}: ошибка запуска ({error})", device.name()),
        }
    }

    #[tool(
        description = "Открыть результаты поиска YouTube на телевизоре. Возвращает экран поиска, видео само не запускается — пользователь выбирает пультом. Для конкретного клипа используй tv_app с watch-ссылкой."
    )]
    pub async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        let query = params.query.trim();
        if query.is_empty() {
            return "Ошибка: пустой поисковый запрос".to_owned();
        }
        let link = format!(
            "https://www.youtube.com/results?search_query={}",
            percent_encode(query)
        );
        let mut client = match TvLink::open(device).await {
            Ok(link) => link,
            Err(error) => return format!("{}: не удалось подключиться ({error})", device.name()),
        };
        match client.send_app_link(&link).await {
            Ok(()) => format!("{}: открываю поиск на YouTube: {query}", device.name()),
            Err(error) => format!("{}: ошибка поиска ({error})", device.name()),
        }
    }

    #[tool(description = "Переключить вход телевизора: hdmi1, hdmi2, hdmi3, hdmi4 или tv.")]
    pub async fn input(&self, Parameters(params): Parameters<InputParams>) -> String {
        let Some(code) = proto::key_code(&params.input) else {
            return format!("Ошибка: неизвестный вход {}", params.input);
        };
        let device = match self.device(params.device.as_deref()) {
            Ok(device) => device,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let lock = match self.lock(device) {
            Ok(lock) => lock,
            Err(error) => return format!("Ошибка: {error}"),
        };
        let _guard = lock.lock().await;
        let mut link = match TvLink::open(device).await {
            Ok(link) => link,
            Err(error) => return format!("{}: не удалось подключиться ({error})", device.name()),
        };
        match link.send_key(code).await {
            Ok(()) => format!("{}: переключила на {}", device.name(), params.input),
            Err(error) => format!("{}: ошибка входа ({error})", device.name()),
        }
    }
}

/// Percent-encode a YouTube query; spaces become %20, UTF-8 bytes are escaped.
fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn app_link(app: &str) -> String {
    match app.to_lowercase().as_str() {
        "youtube" | "ютуб" => "https://www.youtube.com".to_owned(),
        "netflix" | "нетфликс" => "https://www.netflix.com".to_owned(),
        "kinopoisk" | "кинопоиск" => "https://www.kinopoisk.ru".to_owned(),
        "ivi" | "иви" => "https://www.ivi.ru".to_owned(),
        "okko" | "окко" => "https://okko.tv".to_owned(),
        _ => app.to_owned(),
    }
}

async fn open_with_wake(device: &TvDevice) -> Result<TvLink, String> {
    match TvLink::open(device).await {
        Ok(link) => Ok(link),
        Err(error) => {
            let Some(mac) = &device.mac else {
                return Err(error);
            };
            protocol::wake_on_lan(mac, &device.host, device.broadcast.as_deref())?;
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Ok(link) = TvLink::open(device).await {
                    return Ok(link);
                }
            }
            Err(format!(
                "{error}; Wake-on-LAN отправлен, но телевизор не ответил"
            ))
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TvServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tv", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Управление Android TV / Google TV: состояние, питание, клавиши, громкость, приложения и входы. Питание — только через tv_power.",
            )
    }
}

pub fn build_router(config: TvConfig) -> Router {
    let server = TvServer::new(config);
    let service: StreamableHttpService<TvServer, LocalSessionManager> = StreamableHttpService::new(
        {
            let server = server.clone();
            move || Ok(server.clone())
        },
        Default::default(),
        StreamableHttpServerConfig::default(),
    );
    Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", service)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "tv"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_apps_to_deep_links() {
        assert_eq!(app_link("YouTube"), "https://www.youtube.com");
        assert_eq!(app_link("кинопоиск"), "https://www.kinopoisk.ru");
        assert_eq!(app_link("https://example.com"), "https://example.com");
    }

    #[test]
    fn percent_encodes_youtube_queries() {
        assert_eq!(
            percent_encode("котики"),
            "%D0%BA%D0%BE%D1%82%D0%B8%D0%BA%D0%B8"
        );
        assert_eq!(percent_encode("cat clips"), "cat%20clips");
    }

    #[test]
    fn summary_mentions_power_and_volume() {
        let state = protocol::TvState {
            is_on: Some(true),
            volume_level: Some(17),
            volume_max: Some(100),
            muted: Some(false),
            current_app: Some("com.google.android.apps.tv.launcherx".to_owned()),
            device_info: None,
        };
        let summary = state.summary();
        assert!(summary.contains("включён"));
        assert!(summary.contains("17 из 100"));
    }
}

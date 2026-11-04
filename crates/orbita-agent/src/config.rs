//! Environment configuration for `orbita-agent`.
//!
//! The voice interface keeps the same port and WebSocket protocol as the
//! processor, so quasar only needs `PROCESSOR_BASIC_URL` pointed at the agent.

use orbita_core::{env_or, env_parse};

const DEFAULT_PROMPT: &str = "Ты — домашний голосовой агент orbita. Отвечай по-русски, \
кратко и по делу: одно-два предложения, без Markdown и списков. Пиши так, чтобы текст \
можно было произнести вслух: числа и единицы словами, без символов (градусы, проценты, \
дроби), без сокращений и без ссылок на источники. Если не знаешь — скажи об этом. \
Не выдумывай действия, которых не выполнял.";

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub openai_model: String,
    pub db_path: String,
    pub quasar_api_url: String,
    pub default_duid: Option<String>,
    pub system_prompt: String,
    pub history_limit: usize,
    pub scheduler_interval_ms: u64,
    pub mcp_server_urls: Vec<String>,
    /// Local llama.cpp used when the primary provider is slow or fails.
    pub fallback_base_url: String,
    pub fallback_model: String,
    pub fallback_api_key: String,
    pub primary_timeout_ms: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let history_limit: usize = env_parse("HISTORY_LIMIT", 12)?;
        if history_limit == 0 {
            return Err("HISTORY_LIMIT must be greater than zero".to_owned());
        }
        let scheduler_interval_ms: u64 = env_parse("SCHEDULER_INTERVAL_MS", 1000)?;
        if scheduler_interval_ms == 0 {
            return Err("SCHEDULER_INTERVAL_MS must be greater than zero".to_owned());
        }
        Ok(Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_parse("PORT", 8080)?,
            openai_base_url: env_or("OPENAI_BASE_URL", "http://127.0.0.1:8090/v1"),
            openai_api_key: env_or("OPENAI_API_KEY", ""),
            openai_model: env_or("OPENAI_MODEL", "qwen3.5-9b-q4km"),
            db_path: env_or("AGENT_DB", "/var/lib/orbita-agent/agent.db"),
            quasar_api_url: env_or("QUASAR_API_URL", "http://127.0.0.1:31121"),
            default_duid: std::env::var("DEFAULT_DUID")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            system_prompt: env_or("AGENT_SYSTEM_PROMPT", DEFAULT_PROMPT),
            history_limit,
            scheduler_interval_ms,
            mcp_server_urls: split_urls(&env_or("AGENT_MCP_SERVER_URLS", "")),
            fallback_base_url: env_or("AGENT_FALLBACK_BASE_URL", "http://192.168.250.102:8090/v1"),
            fallback_model: env_or("AGENT_FALLBACK_MODEL", "qwen3.5-9b-q4km"),
            fallback_api_key: env_or("AGENT_FALLBACK_API_KEY", ""),
            primary_timeout_ms: env_parse("AGENT_PRIMARY_TIMEOUT_MS", 2500)?,
        })
    }
}

fn split_urls(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

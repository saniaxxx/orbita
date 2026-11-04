//! Environment contract of the upstream quasar-server.

use orbita_core::{env_bool, env_or, env_parse};

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub api_port: u16,
    pub sqlite_path: String,
    pub allow_updates: bool,
    pub updates_config_path: String,
    pub processor_basic_url: String,
    pub stt_gigaam_url: String,
    pub tts_openai_base_url: String,
    pub tts_openai_api_key: String,
    pub tts_openai_model: String,
    pub tts_openai_speed: f32,
    pub tts_openai_voice: String,
    pub audio_metadata_urls: Vec<String>,
    pub function_state_server_port: u16,
    pub glagol_jwt_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_parse("PORT", 31120)?,
            api_port: env_parse("API_PORT", 31121)?,
            sqlite_path: env_or("SQLITE_PATH", "/var/lib/quasar-server/database.sqlite"),
            allow_updates: env_bool("ALLOW_UPDATES", false)?,
            updates_config_path: env_or("UPDATES_CONFIG_PATH", "updates-config.yml"),
            processor_basic_url: env_or("PROCESSOR_BASIC_URL", "http://localhost:8080"),
            stt_gigaam_url: env_or("STT_GIGAAM_URL", "ws://10.0.3.137:8080"),
            tts_openai_base_url: env_or("TTS_OPENAI_BASE_URL", "http://10.0.3.137:8000"),
            tts_openai_api_key: env_or("TTS_OPENAI_API_KEY", ""),
            tts_openai_model: env_or("TTS_OPENAI_MODEL", ""),
            tts_openai_speed: env_parse("TTS_OPENAI_SPEED", 1.0)?,
            tts_openai_voice: env_or("TTS_OPENAI_VOICE", "IVONA 2 Tatyana OEM"),
            audio_metadata_urls: split_urls(&env_or("AUDIO_METADATA_URLS", "")),
            function_state_server_port: env_parse("FUNCTION_STATE_SERVER_PORT", 31114)?,
            glagol_jwt_key: env_or("GLAGOL_JWT_KEY", &uuid::Uuid::new_v4().simple().to_string()),
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

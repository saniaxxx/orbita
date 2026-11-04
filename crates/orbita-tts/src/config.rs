//! Environment contract of the Python `tts-gpu` service.

use orbita_core::{env_or, env_parse};

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub engine: String,
    pub assets_path: String,
    pub default_voice: String,
    pub default_speed: f32,
    pub max_text_length: usize,
    pub cache_size: usize,
    pub total_steps: u32,
    pub silence_duration: f32,
    pub gain_db: f32,
    pub providers: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let default_speed: f32 = env_parse("DEFAULT_SPEED", 1.05)?;
        let max_text_length: usize = env_parse("MAX_TEXT_LENGTH", 12000)?;
        let cache_size: usize = env_parse("CACHE_SIZE", 256)?;
        let total_steps: u32 = env_parse("TOTAL_STEPS", 8)?;
        let silence_duration: f32 = env_parse("SILENCE_DURATION", 0.3)?;
        let gain_db: f32 = env_parse("GAIN_DB", 9.0)?;

        if !default_speed.is_finite() || default_speed <= 0.0 {
            return Err(format!(
                "DEFAULT_SPEED must be a positive number, got {default_speed}"
            ));
        }
        if max_text_length == 0 {
            return Err("MAX_TEXT_LENGTH must be greater than zero".to_owned());
        }
        if cache_size == 0 {
            return Err("CACHE_SIZE must be greater than zero".to_owned());
        }
        if total_steps == 0 {
            return Err("TOTAL_STEPS must be greater than zero".to_owned());
        }
        if !silence_duration.is_finite() || silence_duration < 0.0 {
            return Err(format!(
                "SILENCE_DURATION must be a non-negative number, got {silence_duration}"
            ));
        }
        if !gain_db.is_finite() {
            return Err(format!("GAIN_DB must be finite, got {gain_db}"));
        }

        Ok(Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_parse("PORT", 9160)?,
            engine: env_or("ENGINE", "fake"),
            assets_path: env_or("ASSETS_PATH", "/opt/supertonic-tts/assets"),
            default_voice: env_or("DEFAULT_VOICE", "F4"),
            default_speed,
            max_text_length,
            cache_size,
            total_steps,
            silence_duration,
            gain_db,
            providers: env_or("PROVIDERS", "auto"),
        })
    }
}

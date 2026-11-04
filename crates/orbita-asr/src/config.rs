//! Environment contract of the upstream quasistreaming-gigam service.

use orbita_core::{env_or, env_parse};

pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingMode {
    Streaming,
    Quasistreaming,
}

impl StreamingMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_lowercase().as_str() {
            "streaming" => Ok(Self::Streaming),
            "quasistreaming" => Ok(Self::Quasistreaming),
            other => Err(format!(
                "ASR_STREAMING_MODE must be streaming or quasistreaming, got {other}"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub input_gain: f32,
    pub vad_threshold: f32,
    pub eou_silence_ms: u64,
    pub min_utterance_ms: u64,
    pub max_utterance_ms: u64,
    pub speech_pad_ms: u64,
    pub partial_results: bool,
    pub partial_interval_ms: u64,
    pub min_partial_audio_ms: u64,
    pub emit_vad_events: bool,
    pub streaming_mode: StreamingMode,
    pub quasistreaming_stable_repeats: u32,
    pub language: String,
    pub engine: String,
    pub vad: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let input_gain: f32 = env_parse("INPUT_GAIN", 1.0)?;
        let vad_threshold: f32 = env_parse("VAD_THRESHOLD", 0.3)?;
        let min_silence_seconds: f32 = env_parse("VAD_MIN_SILENCE_DURATION", 0.8)?;
        let min_speech_seconds: f32 = env_parse("VAD_MIN_SPEECH_DURATION", 1.5)?;
        let max_speech_seconds: f32 = env_parse("VAD_MAX_SPEECH_DURATION", 8.0)?;

        if !input_gain.is_finite() || input_gain <= 0.0 {
            return Err(format!("INPUT_GAIN must be positive, got {input_gain}"));
        }
        if !vad_threshold.is_finite() || !(0.0..=1.0).contains(&vad_threshold) {
            return Err(format!(
                "VAD_THRESHOLD must be between 0 and 1, got {vad_threshold}"
            ));
        }
        if !min_silence_seconds.is_finite() || min_silence_seconds < 0.0 {
            return Err("VAD_MIN_SILENCE_DURATION must be non-negative".to_owned());
        }
        if !min_speech_seconds.is_finite() || min_speech_seconds < 0.0 {
            return Err("VAD_MIN_SPEECH_DURATION must be non-negative".to_owned());
        }
        if !max_speech_seconds.is_finite() || max_speech_seconds <= 0.0 {
            return Err("VAD_MAX_SPEECH_DURATION must be positive".to_owned());
        }

        Ok(Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_parse("PORT", 8006)?,
            input_gain,
            vad_threshold,
            eou_silence_ms: (min_silence_seconds * 1000.0).round() as u64,
            min_utterance_ms: (min_speech_seconds * 1000.0).round() as u64,
            max_utterance_ms: (max_speech_seconds * 1000.0).round() as u64,
            speech_pad_ms: env_parse("ASR_STREAM_SPEECH_PAD_MS", 200)?,
            partial_results: orbita_core::env_bool("ASR_STREAM_PARTIAL_RESULTS", true)?,
            partial_interval_ms: env_parse("ASR_STREAM_PARTIAL_INTERVAL_MS", 1000)?,
            min_partial_audio_ms: env_parse("ASR_STREAM_MIN_PARTIAL_AUDIO_MS", 1800)?,
            emit_vad_events: orbita_core::env_bool("ASR_STREAM_EMIT_VAD_EVENTS", false)?,
            streaming_mode: StreamingMode::parse(&env_or("ASR_STREAMING_MODE", "streaming"))?,
            quasistreaming_stable_repeats: env_parse("ASR_QUASISTREAMING_STABLE_REPEATS", 3)?,
            language: env_or("ASR_LANGUAGE", "ru"),
            engine: env_or("ENGINE", "gigaam"),
            vad: env_or("VAD", "energy"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes() {
        assert_eq!(
            StreamingMode::parse("streaming").unwrap(),
            StreamingMode::Streaming
        );
        assert_eq!(
            StreamingMode::parse("QUASISTREAMING").unwrap(),
            StreamingMode::Quasistreaming
        );
        assert!(StreamingMode::parse("x").is_err());
    }
}

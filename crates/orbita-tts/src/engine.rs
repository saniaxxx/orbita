//! Synthesis engine boundary.
//!
//! The ONNX implementation lands in a separate step and is tested on the GPU
//! host. Everything else talks to this trait, so the HTTP contract can be
//! verified locally with [`FakeEngine`].

use std::{collections::BTreeMap, f32::consts::TAU, fmt};

pub const MODEL_NAMES: [&str; 4] = [
    "duration_predictor",
    "text_encoder",
    "vector_estimator",
    "vocoder",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    UnknownVoice(String),
    Failed(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownVoice(voice) => write!(formatter, "unknown voice: {voice}"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Providers = BTreeMap<&'static str, Vec<String>>;

pub trait SynthesisEngine: Send + Sync {
    fn sample_rate(&self) -> u32;
    fn steps(&self) -> u32;
    fn providers(&self) -> Providers;

    /// Synthesize mono f32 samples. The caller serialises access, matching the
    /// Python engine lock.
    fn synthesize(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        steps: u32,
    ) -> Result<Vec<f32>, EngineError>;
}

/// Deterministic engine for contract tests and local runs.
#[derive(Debug, Clone)]
pub struct FakeEngine {
    sample_rate: u32,
    steps: u32,
    providers: Vec<String>,
}

impl FakeEngine {
    pub fn new(sample_rate: u32, steps: u32) -> Self {
        Self {
            sample_rate,
            steps,
            providers: vec!["CPUExecutionProvider".to_owned()],
        }
    }
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new(44_100, 8)
    }
}

impl SynthesisEngine for FakeEngine {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn steps(&self) -> u32 {
        self.steps
    }

    fn providers(&self) -> Providers {
        MODEL_NAMES
            .into_iter()
            .map(|name| (name, self.providers.clone()))
            .collect()
    }

    fn synthesize(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        _steps: u32,
    ) -> Result<Vec<f32>, EngineError> {
        if voice == "XX" {
            return Err(EngineError::UnknownVoice(voice.to_owned()));
        }

        let characters = text.chars().count().max(1) as f32;
        let seconds = (characters * 0.02).min(2.0) / speed.clamp(0.25, 4.0);
        let samples = ((self.sample_rate as f32 * seconds) as usize).max(1);
        let mut output = Vec::with_capacity(samples);
        for index in 0..samples {
            let time = index as f32 / self.sample_rate as f32;
            output.push(0.1 * (TAU * 440.0 * time).sin());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_engine_is_deterministic() {
        let mut engine = FakeEngine::default();
        let first = engine.synthesize("привет", "F4", 1.0, 8).unwrap();
        let second = engine.synthesize("привет", "F4", 1.0, 8).unwrap();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn fake_engine_rejects_unknown_voice() {
        let mut engine = FakeEngine::default();
        assert_eq!(
            engine.synthesize("привет", "XX", 1.0, 8),
            Err(EngineError::UnknownVoice("XX".to_owned()))
        );
    }

    #[test]
    fn reports_all_four_providers() {
        let engine = FakeEngine::default();
        assert_eq!(engine.providers().len(), 4);
    }
}

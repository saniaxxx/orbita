//! GigaAM v3 recogniser: log-mel features, ONNX encoder, greedy decoding.
//!
//! Pure parts (features, vocabulary, decoding loops) build without ONNX
//! Runtime and are unit-tested offline; the ONNX engine is behind the
//! `gigaam` feature.

pub mod decoding;
pub mod features;

#[cfg(feature = "gigaam")]
mod engine;

use orbita_core::{env_or, env_parse};

#[cfg(feature = "gigaam")]
pub use engine::GigaamEngine;

pub const DEFAULT_MODEL_DIR: &str = "/opt/orbita/models/gigaam-v3-onnx";
pub const DEFAULT_MODEL: &str = "v3_e2e_rnnt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    Rnnt,
    Ctc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantization {
    None,
    Int8,
}

impl Quantization {
    pub fn file_suffix(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Int8 => ".int8",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Cuda,
}

/// Engine settings from the environment.
///
/// `GIGAAM_DEVICE` keeps the name used by the Python `qsg-asr` unit.
#[derive(Debug, Clone, PartialEq)]
pub struct GigaamConfig {
    pub model_dir: String,
    pub model: String,
    pub quantization: Quantization,
    pub device: Device,
    pub cuda_device: i32,
    pub threads: usize,
}

impl GigaamConfig {
    pub fn from_env() -> Result<Self, String> {
        let model = env_or("GIGAAM_MODEL", DEFAULT_MODEL);
        let quantization = parse_quantization(&env_or("GIGAAM_QUANTIZATION", "none"))?;
        let device = parse_device(&env_or("GIGAAM_DEVICE", "auto"))?;
        let threads: usize = env_parse("ASR_THREADS", 4)?;
        if threads == 0 {
            return Err("ASR_THREADS must be greater than zero".to_owned());
        }
        let config = Self {
            model_dir: env_or("GIGAAM_MODEL_DIR", DEFAULT_MODEL_DIR),
            model,
            quantization,
            device,
            cuda_device: env_parse("GIGAAM_CUDA_DEVICE", 0)?,
            threads,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.model.starts_with("v3_") {
            return Err(format!(
                "GIGAAM_MODEL must be a v3 model (v3_e2e_rnnt, v3_e2e_ctc, v3_rnnt, v3_ctc), got {}",
                self.model
            ));
        }
        Ok(())
    }

    pub fn head(&self) -> Head {
        if self.model.ends_with("_ctc") {
            Head::Ctc
        } else {
            Head::Rnnt
        }
    }

    pub fn provider_names(&self) -> Vec<String> {
        match self.device {
            Device::Cpu => vec!["CPUExecutionProvider".to_owned()],
            Device::Cuda => vec!["CUDAExecutionProvider".to_owned()],
        }
    }
}

fn parse_quantization(value: &str) -> Result<Quantization, String> {
    match value.trim().to_lowercase().as_str() {
        "" | "none" | "fp32" => Ok(Quantization::None),
        "int8" => Ok(Quantization::Int8),
        other => Err(format!(
            "GIGAAM_QUANTIZATION must be none or int8, got {other}"
        )),
    }
}

fn parse_device(value: &str) -> Result<Device, String> {
    match value.trim().to_lowercase().as_str() {
        "auto" => Ok(if cfg!(feature = "cuda") {
            Device::Cuda
        } else {
            Device::Cpu
        }),
        "cpu" => Ok(Device::Cpu),
        "cuda" | "gpu" => Ok(Device::Cuda),
        other => Err(format!(
            "GIGAAM_DEVICE must be auto, cpu or cuda, got {other}"
        )),
    }
}

#[cfg(feature = "gigaam")]
mod recognizer {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::{GigaamConfig, GigaamEngine};
    use crate::{
        config::SAMPLE_RATE,
        recognizer::{Recognizer, TranscriptSegment},
    };

    /// [`Recognizer`] over one engine; calls are serialised like the Python
    /// service's model lock and run on the blocking pool.
    pub struct GigaamRecognizer {
        engine: Arc<Mutex<GigaamEngine>>,
    }

    impl GigaamRecognizer {
        pub fn load(config: &GigaamConfig) -> Result<Self, String> {
            Ok(Self {
                engine: Arc::new(Mutex::new(GigaamEngine::load(config)?)),
            })
        }

        /// The first ONNX Runtime calls are several times slower; run them
        /// before the port opens.
        pub fn warm_up(&self) -> Result<(), String> {
            let silence = vec![0.0f32; SAMPLE_RATE as usize];
            let mut engine = self.lock();
            for _ in 0..2 {
                engine.recognize(&silence)?;
            }
            Ok(())
        }

        pub fn recognize_blocking(&self, samples: &[f32]) -> Result<String, String> {
            self.lock().recognize(samples)
        }

        pub fn providers(&self) -> Vec<String> {
            self.lock().providers().to_vec()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, GigaamEngine> {
            self.engine
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    #[async_trait]
    impl Recognizer for GigaamRecognizer {
        async fn transcribe(
            &self,
            samples: &[f32],
            offset_s: f64,
            _language: &str,
        ) -> Result<Vec<TranscriptSegment>, String> {
            if samples.is_empty() {
                return Ok(Vec::new());
            }
            let engine = Arc::clone(&self.engine);
            let audio = samples.to_vec();
            let duration = audio.len() as f64 / SAMPLE_RATE as f64;
            let text = tokio::task::spawn_blocking(move || {
                engine
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recognize(&audio)
            })
            .await
            .map_err(|error| format!("recognizer task failed: {error}"))??;

            if text.is_empty() {
                return Ok(Vec::new());
            }
            Ok(vec![TranscriptSegment {
                start_s: offset_s,
                end_s: offset_s + duration,
                text,
            }])
        }
    }
}

#[cfg(feature = "gigaam")]
pub use recognizer::GigaamRecognizer;

#[cfg(test)]
mod tests {
    use super::*;

    fn config(model: &str) -> GigaamConfig {
        GigaamConfig {
            model_dir: DEFAULT_MODEL_DIR.to_owned(),
            model: model.to_owned(),
            quantization: Quantization::None,
            device: Device::Cpu,
            cuda_device: 0,
            threads: 4,
        }
    }

    #[test]
    fn picks_the_head_from_the_model_name() {
        assert_eq!(config("v3_e2e_rnnt").head(), Head::Rnnt);
        assert_eq!(config("v3_rnnt").head(), Head::Rnnt);
        assert_eq!(config("v3_e2e_ctc").head(), Head::Ctc);
        assert_eq!(config("v3_ctc").head(), Head::Ctc);
    }

    #[test]
    fn only_v3_models_are_accepted() {
        assert!(config("v3_e2e_rnnt").validate().is_ok());
        assert!(config("v2_rnnt").validate().is_err());
    }

    #[test]
    fn parses_quantization_and_device() {
        assert_eq!(parse_quantization("INT8").unwrap(), Quantization::Int8);
        assert_eq!(parse_quantization("fp32").unwrap(), Quantization::None);
        assert!(parse_quantization("int4").is_err());
        assert_eq!(parse_device("cpu").unwrap(), Device::Cpu);
        assert_eq!(parse_device("GPU").unwrap(), Device::Cuda);
        assert!(parse_device("tpu").is_err());
        assert_eq!(Quantization::Int8.file_suffix(), ".int8");
    }
}

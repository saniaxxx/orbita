//! Vosk TTS ru 0.10 (multistream VITS + BERT, 57 voices, Apache-2.0) in Rust.
//!
//! Port of the `vosk-tts` Python package by Alpha Cephei (Apache-2.0): text
//! clean-up, BERT WordPiece, multistream g2p and the ONNX calls. Model files
//! are not part of the repository; `VOSK_MODEL_DIR` points at them. Both the
//! original `vosk-model-tts-ru-0.10-multi` layout (`model.onnx`,
//! `bert/model.onnx`, `bert/vocab.txt`, `dictionary`) and the flat Hugging
//! Face mirrors (`model_fp32.onnx`, `bert_u8.onnx`, `vocab.txt`,
//! `dictionary.txt.gz`) are accepted.

pub mod g2p;
pub mod normalize;
pub mod tokenizer;

#[cfg(feature = "vosk")]
pub mod bench;
#[cfg(feature = "vosk")]
mod engine;
#[cfg(feature = "vosk")]
pub mod model;

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
};

use orbita_core::{env_or, env_parse};
use serde::Deserialize;

#[cfg(feature = "vosk")]
pub use engine::VoskEngine;

use tokenizer::WordPiece;

pub const DEFAULT_MODEL_DIR: &str = "/opt/orbita/models/vosk-tts-ru-0.10";
pub const SAMPLE_RATE: u32 = 22_050;
/// Provisional default voice until one is chosen by ear.
pub const DEFAULT_SPEAKER: &str = "nizhegorodova_aleksandra";

#[derive(Debug, Clone, PartialEq)]
pub struct ModelFiles {
    pub synthesizer: PathBuf,
    pub bert: PathBuf,
    pub vocab: PathBuf,
    pub dictionary: PathBuf,
    pub config: PathBuf,
}

impl ModelFiles {
    /// Resolve files in `dir`, original layout first, then the flat mirrors.
    pub fn resolve(dir: &Path) -> Result<Self, String> {
        let pick = |what: &str, candidates: &[&str]| -> Result<PathBuf, String> {
            candidates
                .iter()
                .map(|name| dir.join(name))
                .find(|path| path.is_file())
                .ok_or_else(|| {
                    format!(
                        "{what} not found in {}: tried {}",
                        dir.display(),
                        candidates.join(", ")
                    )
                })
        };
        Ok(Self {
            synthesizer: pick("synthesizer", &["model.onnx", "model_fp32.onnx"])?,
            bert: pick("BERT", &["bert/model.onnx", "bert_u8.onnx"])?,
            vocab: pick("BERT vocab", &["bert/vocab.txt", "vocab.txt"])?,
            dictionary: pick("dictionary", &["dictionary", "dictionary.txt.gz"])?,
            config: pick("config", &["config.json"])?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct VoskConfig {
    pub model_dir: String,
    pub files: ModelFiles,
    pub default_speaker: String,
    pub cuda: bool,
    /// Run the (uint8) BERT session on the GPU too; `VOSK_BERT_DEVICE`.
    pub bert_cuda: bool,
    /// cuDNN convolution search; `VOSK_CUDNN_SEARCH`.
    pub cudnn_search: Option<CudnnSearch>,
    /// cuDNN TF32 for fp32 convolutions; `VOSK_TF32`.
    pub tf32: Option<bool>,
    /// `true` = preallocate exactly (same as requested), `false` = next power
    /// of two; `VOSK_ARENA`.
    pub arena: Option<bool>,
    /// Log per-stage timings for every request; `VOSK_TIMINGS`.
    pub timings: bool,
    pub threads: usize,
    /// Pause between pieces of long input, seconds.
    pub silence: f32,
    /// Pieces longer than this are split at sentence ends.
    pub max_chars: usize,
}

impl VoskConfig {
    /// `providers` and `silence` come from the service config (`PROVIDERS`,
    /// `SILENCE_DURATION`).
    pub fn from_env(providers: &str, silence: f32) -> Result<Self, String> {
        let model_dir = env_or("VOSK_MODEL_DIR", DEFAULT_MODEL_DIR);
        let threads: usize = env_parse("TTS_THREADS", 4)?;
        if threads == 0 {
            return Err("TTS_THREADS must be greater than zero".to_owned());
        }
        let max_chars: usize = env_parse("VOSK_MAX_CHARS", 400)?;
        if max_chars < 50 {
            return Err("VOSK_MAX_CHARS must be at least 50".to_owned());
        }
        let cuda = match providers.trim().to_lowercase().as_str() {
            "auto" => cfg!(feature = "cuda"),
            "cpu" => false,
            "cuda" | "gpu" => true,
            other => return Err(format!("PROVIDERS must be auto, cpu or cuda, got {other}")),
        };
        let bert_cuda = match env_or("VOSK_BERT_DEVICE", "auto")
            .trim()
            .to_lowercase()
            .as_str()
        {
            "auto" => cuda,
            "cpu" => false,
            "cuda" | "gpu" => true,
            other => {
                return Err(format!(
                    "VOSK_BERT_DEVICE must be auto, cpu or cuda, got {other}"
                ));
            }
        };
        let cudnn_search = match env_or("VOSK_CUDNN_SEARCH", "heuristic")
            .trim()
            .to_lowercase()
            .as_str()
        {
            "heuristic" => Some(CudnnSearch::Heuristic),
            "exhaustive" => Some(CudnnSearch::Exhaustive),
            "default" => Some(CudnnSearch::Default),
            "auto" => None,
            other => {
                return Err(format!(
                    "VOSK_CUDNN_SEARCH must be auto, default, exhaustive or heuristic, got {other}"
                ));
            }
        };
        let tf32 = match env_or("VOSK_TF32", "auto").trim().to_lowercase().as_str() {
            "auto" => None,
            "on" | "1" | "true" => Some(true),
            "off" | "0" | "false" => Some(false),
            other => return Err(format!("VOSK_TF32 must be auto, on or off, got {other}")),
        };
        let arena = match env_or("VOSK_ARENA", "auto").trim().to_lowercase().as_str() {
            "auto" => None,
            "same" | "same_as_requested" => Some(true),
            "power" | "next_power_of_two" => Some(false),
            other => {
                return Err(format!(
                    "VOSK_ARENA must be auto, same or power, got {other}"
                ));
            }
        };
        Ok(Self {
            files: ModelFiles::resolve(Path::new(&model_dir))?,
            model_dir,
            default_speaker: env_or("VOSK_VOICE", DEFAULT_SPEAKER),
            cuda,
            bert_cuda,
            cudnn_search,
            tf32,
            arena,
            timings: orbita_core::env_bool("VOSK_TIMINGS", false)?,
            threads,
            silence,
            max_chars,
        })
    }

    /// Providers of the two sessions: (synthesizer, BERT).
    pub fn provider_names(&self) -> (Vec<String>, Vec<String>) {
        let name = |cuda: bool| {
            vec![
                if cuda {
                    "CUDAExecutionProvider"
                } else {
                    "CPUExecutionProvider"
                }
                .to_owned(),
            ]
        };
        (name(self.cuda), name(self.bert_cuda))
    }
}

/// cuDNN convolution search strategy (`VOSK_CUDNN_SEARCH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CudnnSearch {
    Heuristic,
    Exhaustive,
    Default,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub model_type: String,
    pub phoneme_id_map: HashMap<String, i64>,
    pub speaker_id_map: HashMap<String, i64>,
    pub inference: Inference,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Inference {
    #[serde(default = "default_noise")]
    pub noise_level: f32,
    #[serde(default = "default_rate")]
    pub speech_rate: f32,
    #[serde(default = "default_noise")]
    pub duration_noise_level: f32,
    #[serde(default = "default_rate")]
    pub scale: f32,
}

fn default_noise() -> f32 {
    0.8
}

fn default_rate() -> f32 {
    1.0
}

/// Everything text-side: tokenizer, pronunciation dictionary, symbol tables.
#[derive(Debug, Clone)]
pub struct Lexicon {
    pub tokenizer: WordPiece,
    pub dictionary: HashMap<String, String>,
    pub phoneme_ids: HashMap<String, i64>,
    pub speakers: HashMap<String, i64>,
    pub inference: Inference,
}

impl Lexicon {
    pub fn load(files: &ModelFiles) -> Result<Self, String> {
        let config: ModelConfig = serde_json::from_str(&read_text(&files.config)?)
            .map_err(|error| format!("parsing {}: {error}", files.config.display()))?;
        if config.model_type != "multistream_v1" {
            return Err(format!(
                "model_type {} is not supported (only multistream_v1)",
                config.model_type
            ));
        }
        Ok(Self {
            tokenizer: WordPiece::from_vocab(&read_text(&files.vocab)?)?,
            dictionary: parse_dictionary(&read_text(&files.dictionary)?),
            phoneme_ids: config.phoneme_id_map,
            speakers: config.speaker_id_map,
            inference: config.inference,
        })
    }

    /// Speaker id for a voice name or numeric id.
    pub fn speaker(&self, voice: &str) -> Option<i64> {
        let voice = voice.trim();
        if let Some(id) = self.speakers.get(voice) {
            return Some(*id);
        }
        let id: i64 = voice.parse().ok()?;
        self.speakers
            .values()
            .any(|known| *known == id)
            .then_some(id)
    }
}

/// Read a text file, transparently un-gzipping `*.gz`.
fn read_text(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    if path.extension().is_some_and(|ext| ext == "gz") {
        let mut text = String::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_string(&mut text)
            .map_err(|error| format!("un-gzipping {}: {error}", path.display()))?;
        Ok(text)
    } else {
        String::from_utf8(bytes)
            .map_err(|error| format!("{} is not UTF-8: {error}", path.display()))
    }
}

/// Two formats: the original `word prob phonemes…` (highest probability wins,
/// first one on ties, like the Python loader) and the flattened
/// `word<TAB>phonemes`.
pub fn parse_dictionary(text: &str) -> HashMap<String, String> {
    let mut dictionary: HashMap<String, String> = HashMap::new();
    let mut best: HashMap<String, f64> = HashMap::new();
    for line in text.lines() {
        if let Some((word, phonemes)) = line.split_once('\t') {
            dictionary.insert(word.to_owned(), phonemes.to_owned());
            continue;
        }
        let mut items = line.splitn(3, char::is_whitespace);
        let (Some(word), Some(prob), Some(phonemes)) = (items.next(), items.next(), items.next())
        else {
            continue;
        };
        let Ok(prob) = prob.parse::<f64>() else {
            continue;
        };
        if best.get(word).copied().unwrap_or(0.0) < prob {
            dictionary.insert(word.to_owned(), phonemes.trim().to_owned());
            best.insert(word.to_owned(), prob);
        }
    }
    dictionary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_dictionary_formats() {
        let original = "мука 0.4 m u1 k a0\nмука 0.6 m u0 k a1\nдом 1.0 d o1 m\n";
        let dictionary = parse_dictionary(original);
        assert_eq!(dictionary["мука"], "m u0 k a1");
        assert_eq!(dictionary["дом"], "d o1 m");

        let flat = "!SIL\tSIL\n1с\to0 dj i0 n e1 s\n";
        let dictionary = parse_dictionary(flat);
        assert_eq!(dictionary["1с"], "o0 dj i0 n e1 s");
    }
}

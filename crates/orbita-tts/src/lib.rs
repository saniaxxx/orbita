//! Speech synthesis service with an OpenAI-compatible HTTP API.
//!
//! The engine is Vosk TTS ru 0.10 on ONNX Runtime (`ENGINE=vosk`, module
//! [`vosk`]); the HTTP and audio contracts are also verified with
//! [`FakeEngine`]. `text` is the supertonic text port kept for its parity tests.

pub mod audio;
pub mod cache;
pub mod config;
pub mod engine;
pub mod http;
pub mod text;
pub mod vosk;

pub use config::Config;
pub use engine::{FakeEngine, SynthesisEngine};
pub use http::{AppState, build_router};

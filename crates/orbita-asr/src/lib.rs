//! Rust port of the streaming GigaAM service.
//!
//! The VAD buffer, endpointing and WebSocket contract follow `qsg-asr`;
//! recognition is GigaAM v3 on ONNX Runtime (`gigaam` feature). TEN VAD is
//! not ported yet: `VAD=energy` is the only detector.

pub mod config;
pub mod gigaam;
pub mod recognizer;
pub mod server;
pub mod streaming;
pub mod vad;

pub use config::{Config, StreamingMode};
pub use gigaam::GigaamConfig;
#[cfg(feature = "gigaam")]
pub use gigaam::GigaamRecognizer;
pub use recognizer::{FakeRecognizer, Recognizer, TranscriptSegment};
pub use server::{AppState, build_router};
pub use streaming::{AsrEvent, StreamingAsrSession, TranscriptEvent};
pub use vad::{EnergyVad, VadDetector};

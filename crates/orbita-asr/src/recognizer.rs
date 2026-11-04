//! Speech recognition boundary.
//!
//! The real GigaAM backend needs PyTorch or an ONNX export and is a separate
//! step; the streaming and WebSocket contracts are verified with a fake.

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
}

#[async_trait]
pub trait Recognizer: Send + Sync {
    async fn transcribe(
        &self,
        samples: &[f32],
        offset_s: f64,
        language: &str,
    ) -> Result<Vec<TranscriptSegment>, String>;
}

#[derive(Debug, Clone)]
pub struct FakeRecognizer {
    text: String,
}

impl FakeRecognizer {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl Default for FakeRecognizer {
    fn default() -> Self {
        Self::new("тест")
    }
}

#[async_trait]
impl Recognizer for FakeRecognizer {
    async fn transcribe(
        &self,
        samples: &[f32],
        offset_s: f64,
        _language: &str,
    ) -> Result<Vec<TranscriptSegment>, String> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let duration = samples.len() as f64 / 16_000.0;
        Ok(vec![TranscriptSegment {
            start_s: offset_s,
            end_s: offset_s + duration,
            text: self.text.clone(),
        }])
    }
}

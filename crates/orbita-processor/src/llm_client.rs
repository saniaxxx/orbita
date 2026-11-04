//! OpenAI-compatible streaming chat client.

use std::time::Instant;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::llm::LlmMessage;

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Return the full `delta.content` stream. The first token timestamp is
    /// logged, which keeps the same TTFT signal as the upstream processor.
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct OpenAiClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
}

impl OpenAiClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort,
        })
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut payload = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(reasoning_effort) = &self.reasoning_effort {
            payload["reasoning_effort"] = json!(reasoning_effort);
        }

        let mut request = self.client.post(url).json(&payload);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("LLM request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("LLM returned {status}: {}", body.trim()));
        }

        let started = Instant::now();
        let mut first_token = true;
        let mut buffer = Vec::new();
        let mut content = String::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("LLM stream failed: {error}"))?;
            buffer.extend_from_slice(&chunk);

            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = buffer.drain(..=position).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(part) = value["choices"][0]["delta"]["content"].as_str() {
                    if first_token {
                        tracing::info!("TTFT: {}ms", started.elapsed().as_millis());
                        first_token = false;
                    }
                    content.push_str(part);
                }
            }
        }

        tracing::info!("Full query time: {}ms", started.elapsed().as_millis());
        Ok(content)
    }
}

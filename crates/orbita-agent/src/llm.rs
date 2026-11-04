//! Minimal OpenAI-compatible streaming client.
//!
//! The agent deliberately does not depend on the processor crate: it needs
//! only `chat/completions` with SSE and a plain-text answer.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: content.into(),
        }
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, messages: &[Message]) -> Result<String, String>;
}

pub struct OpenAiClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| format!("building LLM client: {error}"))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        })
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut request = self.client.post(&url).json(&json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
        }));
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

        let mut content = String::new();
        let mut buffer = Vec::new();
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
                    content.push_str(part);
                }
            }
        }
        Ok(content)
    }
}

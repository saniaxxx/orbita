//! Outgoing voice notifications through quasar.

use async_trait::async_trait;
use serde_json::json;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn push(&self, device_id: &str, text: &str) -> Result<(), String>;
}

pub struct QuasarNotifier {
    client: reqwest::Client,
    base_url: String,
}

impl QuasarNotifier {
    pub fn new(base_url: impl Into<String>) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| format!("building notifier client: {error}"))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }
}

#[async_trait]
impl Notifier for QuasarNotifier {
    async fn push(&self, device_id: &str, text: &str) -> Result<(), String> {
        let url = format!(
            "{}/devices/{device_id}/push/text",
            self.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .json(&json!({"eventText": text}))
            .send()
            .await
            .map_err(|error| format!("push to {device_id}: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "push to {device_id} returned {}",
                response.status()
            ));
        }
        Ok(())
    }
}

//! In-memory registry of connected stations.
//!
//! The uniproxy will register its live connections here; the management API
//! pushes TTS and directives through the same trait.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait DeviceConnection: Send + Sync {
    async fn push_tts(&self, text: &str) -> Result<(), String>;
    async fn push_raw(&self, directive: &Value) -> Result<(), String>;
    async fn push_directives(&self, directives: &[Value]) -> Result<(), String>;
    fn supported_directives(&self) -> Value;
}

#[derive(Default)]
pub struct ConnectionRegistry {
    connections: Mutex<HashMap<String, Vec<Arc<dyn DeviceConnection>>>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, duid: &str, connection: Arc<dyn DeviceConnection>) {
        self.lock()
            .entry(duid.to_owned())
            .or_default()
            .push(connection);
    }

    pub fn is_connected(&self, duid: &str) -> bool {
        self.lock()
            .get(duid)
            .is_some_and(|connections| !connections.is_empty())
    }

    pub fn supported_directives(&self, duid: &str) -> Option<Value> {
        self.lock()
            .get(duid)
            .and_then(|connections| connections.first())
            .map(|connection| connection.supported_directives())
    }

    pub async fn push_tts(&self, duid: &str, text: &str) {
        for connection in self.connections_for(duid) {
            if let Err(error) = connection.push_tts(text).await {
                tracing::warn!(%duid, %error, "push_tts failed");
            }
        }
    }

    pub async fn push_raw(&self, duid: &str, directive: &Value) {
        for connection in self.connections_for(duid) {
            if let Err(error) = connection.push_raw(directive).await {
                tracing::warn!(%duid, %error, "push_raw failed");
            }
        }
    }

    pub async fn push_directives(&self, duid: &str, directives: &[Value]) {
        for connection in self.connections_for(duid) {
            if let Err(error) = connection.push_directives(directives).await {
                tracing::warn!(%duid, %error, "push_directives failed");
            }
        }
    }

    fn connections_for(&self, duid: &str) -> Vec<Arc<dyn DeviceConnection>> {
        let connections = self.lock();
        if duid == "all" {
            connections
                .values()
                .flat_map(|items| items.iter().cloned())
                .collect()
        } else {
            connections
                .get(duid)
                .map(|items| items.to_vec())
                .unwrap_or_default()
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<Arc<dyn DeviceConnection>>>> {
        self.connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

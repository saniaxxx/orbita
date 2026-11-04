//! JSON configuration for the home service.

use std::{collections::HashSet, path::Path};

use serde::{Deserialize, Serialize};

use crate::model::{DeviceConfig, HomeError};

fn default_stale_after_ms() -> u64 {
    30_000
}

fn default_rate_limit_per_min() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttConfig {
    pub url: String,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

fn default_client_id() -> String {
    "orbita-home".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeConfig {
    pub mqtt: MqttConfig,
    pub devices: Vec<DeviceConfig>,
    #[serde(default = "default_stale_after_ms")]
    pub stale_after_ms: u64,
    #[serde(default = "default_rate_limit_per_min")]
    pub rate_limit_per_min: u32,
}

impl HomeConfig {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, HomeError> {
        let text = std::fs::read_to_string(path.as_ref()).map_err(|error| {
            HomeError::Config(format!("reading {}: {error}", path.as_ref().display()))
        })?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|error| HomeError::Config(format!("parsing config: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), HomeError> {
        if self.devices.is_empty() {
            return Err(HomeError::Config("no devices configured".to_owned()));
        }
        if self.mqtt.url.is_empty() {
            return Err(HomeError::Config("mqtt.url is empty".to_owned()));
        }
        let mut ids = HashSet::new();
        for device in &self.devices {
            if device.id.is_empty() {
                return Err(HomeError::Config("device id is empty".to_owned()));
            }
            if !ids.insert(device.id.as_str()) {
                return Err(HomeError::Config(format!(
                    "duplicate device id {}",
                    device.id
                )));
            }
            let mut capabilities = HashSet::new();
            for capability in &device.capabilities {
                if capability.name.is_empty() {
                    return Err(HomeError::Config(format!(
                        "{} has a capability with an empty name",
                        device.id
                    )));
                }
                if !capabilities.insert(capability.name.as_str()) {
                    return Err(HomeError::Config(format!(
                        "{} has duplicate capability {}",
                        device.id, capability.name
                    )));
                }
                if capability.writable && capability.command_topic.is_none() {
                    return Err(HomeError::Config(format!(
                        "{}/{} is writable but has no command_topic",
                        device.id, capability.name
                    )));
                }
                if capability.readable && capability.state_topic.is_none() {
                    return Err(HomeError::Config(format!(
                        "{}/{} is readable but has no state_topic",
                        device.id, capability.name
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> HomeConfig {
        HomeConfig {
            mqtt: MqttConfig {
                url: "mqtt://127.0.0.1:1883".to_owned(),
                client_id: "test".to_owned(),
                username: None,
                password: None,
            },
            devices: vec![DeviceConfig {
                id: "light".to_owned(),
                room: Some("living".to_owned()),
                kind: "light".to_owned(),
                integration: "wirenboard".to_owned(),
                external_id: "wb/1".to_owned(),
                aliases: vec!["свет".to_owned()],
                meta_error_topic: None,
                capabilities: vec![crate::model::Capability {
                    name: "on_off".to_owned(),
                    kind: "bool".to_owned(),
                    min: None,
                    max: None,
                    unit: None,
                    values: vec![],
                    readable: true,
                    writable: true,
                    risk: crate::model::Risk::Safe,
                    state_topic: Some("/devices/wb/controls/1".to_owned()),
                    command_topic: Some("/devices/wb/controls/1/on".to_owned()),
                    payload_on: None,
                    payload_off: None,
                    stale_after_ms: None,
                    feedback: None,
                }],
            }],
            stale_after_ms: 30_000,
            rate_limit_per_min: 60,
        }
    }

    #[test]
    fn accepts_minimal_config() {
        assert!(minimal().validate().is_ok());
    }

    #[test]
    fn rejects_writable_without_topic() {
        let mut config = minimal();
        config.devices[0].capabilities[0].command_topic = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_device() {
        let mut config = minimal();
        config.devices.push(config.devices[0].clone());
        assert!(config.validate().is_err());
    }
}

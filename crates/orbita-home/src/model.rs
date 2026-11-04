//! Domain model for `orbita-home`.
//!
//! Devices are described in JSON. The model is deliberately independent from
//! MQTT: the home service validates actions and builds payloads, while the
//! transport only publishes them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    #[default]
    Safe,
    Sensitive,
    Dangerous,
}

impl Risk {
    pub fn requires_grant(self) -> bool {
        !matches!(self, Self::Safe)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackBinding {
    pub topic: String,
    pub expect: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default = "default_true")]
    pub readable: bool,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub risk: Risk,
    #[serde(default)]
    pub state_topic: Option<String>,
    #[serde(default)]
    pub command_topic: Option<String>,
    #[serde(default)]
    pub payload_on: Option<String>,
    #[serde(default)]
    pub payload_off: Option<String>,
    #[serde(default)]
    pub stale_after_ms: Option<u64>,
    #[serde(default)]
    pub feedback: Option<FeedbackBinding>,
}

impl Capability {
    pub fn validate(&self, action: &Action) -> Result<(), HomeError> {
        if matches!(action, Action::Status) {
            return if self.readable {
                Ok(())
            } else {
                Err(HomeError::Denied(format!(
                    "{}/{} is not readable",
                    self.name, self.kind
                )))
            };
        }
        if !self.writable {
            return Err(HomeError::Denied(format!("{} is read-only", self.name)));
        }
        if self.command_topic.is_none() {
            return Err(HomeError::Config(format!(
                "{} has no command topic",
                self.name
            )));
        }

        match (self.kind.as_str(), action) {
            ("bool", Action::OnOff { .. }) => Ok(()),
            ("number", Action::Number { value }) => {
                if !value.is_finite() {
                    return Err(HomeError::Denied(format!(
                        "{} must be a finite number",
                        self.name
                    )));
                }
                if let Some(min) = self.min
                    && *value < min
                {
                    return Err(HomeError::Denied(format!(
                        "{} below minimum {min}",
                        self.name
                    )));
                }
                if let Some(max) = self.max
                    && *value > max
                {
                    return Err(HomeError::Denied(format!(
                        "{} above maximum {max}",
                        self.name
                    )));
                }
                Ok(())
            }
            ("enum", Action::Enum { value }) => {
                if self.values.iter().any(|allowed| allowed == value) {
                    Ok(())
                } else {
                    Err(HomeError::Denied(format!(
                        "{} is not one of {:?}",
                        value, self.values
                    )))
                }
            }
            (kind, action) => Err(HomeError::Denied(format!(
                "cannot apply {action:?} to {kind} capability {}",
                self.name
            ))),
        }
    }

    pub fn payload(&self, action: &Action) -> Result<Vec<u8>, HomeError> {
        let value = match action {
            Action::OnOff { on } => {
                if *on {
                    self.payload_on.clone().unwrap_or_else(|| "1".to_owned())
                } else {
                    self.payload_off.clone().unwrap_or_else(|| "0".to_owned())
                }
            }
            Action::Number { value } => value.to_string(),
            Action::Enum { value } => value.clone(),
            Action::Status => {
                return Err(HomeError::Denied("status is not a write action".to_owned()));
            }
        };
        Ok(value.into_bytes())
    }

    pub fn stale_after_ms(&self, default_ms: u64) -> u64 {
        self.stale_after_ms.unwrap_or(default_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    OnOff { on: bool },
    Number { value: f64 },
    Enum { value: String },
    Status,
}

impl Action {
    pub fn canonical(&self) -> String {
        match self {
            Self::OnOff { on } => format!("on_off:{on}"),
            Self::Number { value } => format!("number:{value}"),
            Self::Enum { value } => format!("enum:{value}"),
            Self::Status => "status".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub id: String,
    #[serde(default)]
    pub room: Option<String>,
    pub kind: String,
    #[serde(default = "default_integration")]
    pub integration: String,
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub meta_error_topic: Option<String>,
    pub capabilities: Vec<Capability>,
}

fn default_integration() -> String {
    "wirenboard".to_owned()
}

impl DeviceConfig {
    pub fn capability(&self, name: &str) -> Option<&Capability> {
        self.capabilities.iter().find(|cap| cap.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub device_id: String,
    pub capability: String,
    pub action: Action,
}

impl ActionRequest {
    pub fn new(
        device_id: impl Into<String>,
        capability: impl Into<String>,
        action: Action,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            capability: capability.into(),
            action,
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.device_id,
            self.capability,
            self.action.canonical()
        )
    }
}

/// A grant issued by the agent's policy layer. `orbita-home` only checks that
/// the grant matches the action and has not expired; it never decides policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub device_id: String,
    pub capability: String,
    pub action_hash: String,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
}

impl Grant {
    pub fn matches(&self, request: &ActionRequest, now: DateTime<Utc>) -> bool {
        self.expires_at > now
            && self.device_id == request.device_id
            && self.capability == request.capability
            && self.action_hash == request.key()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Published {
    pub device_id: String,
    pub capability: String,
    pub topic: String,
    pub payload: String,
    pub risk: Risk,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityView {
    pub name: String,
    pub kind: String,
    pub risk: Risk,
    pub readable: bool,
    pub writable: bool,
    pub value: Option<String>,
    pub received_at: Option<DateTime<Utc>>,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub id: String,
    pub room: Option<String>,
    pub kind: String,
    pub error: Option<String>,
    pub stale: bool,
    pub capabilities: Vec<CapabilityView>,
}

#[derive(Debug, Error)]
pub enum HomeError {
    #[error("unknown device {0}")]
    UnknownDevice(String),
    #[error("unknown capability {device}/{capability}")]
    UnknownCapability { device: String, capability: String },
    #[error("denied: {0}")]
    Denied(String),
    #[error("confirmation required for {0}")]
    NeedsGrant(String),
    #[error("rate limited for {0}")]
    RateLimited(String),
    #[error("publish failed: {0}")]
    Publish(String),
    #[error("invalid config: {0}")]
    Config(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(kind: &str) -> Capability {
        Capability {
            name: "value".to_owned(),
            kind: kind.to_owned(),
            min: Some(0.0),
            max: Some(100.0),
            unit: None,
            values: vec!["auto".to_owned(), "off".to_owned()],
            readable: true,
            writable: true,
            risk: Risk::Safe,
            state_topic: None,
            command_topic: Some("/devices/x/controls/y".to_owned()),
            payload_on: None,
            payload_off: None,
            stale_after_ms: None,
            feedback: None,
        }
    }

    #[test]
    fn bool_payloads_default_to_one_and_zero() {
        let cap = capability("bool");
        assert_eq!(
            cap.payload(&Action::OnOff { on: true }).unwrap(),
            b"1".to_vec()
        );
        assert_eq!(
            cap.payload(&Action::OnOff { on: false }).unwrap(),
            b"0".to_vec()
        );
        assert!(cap.validate(&Action::OnOff { on: true }).is_ok());
    }

    #[test]
    fn number_is_range_checked() {
        let cap = capability("number");
        assert!(cap.validate(&Action::Number { value: 42.0 }).is_ok());
        assert!(cap.validate(&Action::Number { value: -1.0 }).is_err());
        assert!(cap.validate(&Action::Number { value: 101.0 }).is_err());
        assert!(cap.validate(&Action::Number { value: f64::NAN }).is_err());
    }

    #[test]
    fn enum_is_checked() {
        let cap = capability("enum");
        assert!(
            cap.validate(&Action::Enum {
                value: "auto".to_owned()
            })
            .is_ok()
        );
        assert!(
            cap.validate(&Action::Enum {
                value: "turbo".to_owned()
            })
            .is_err()
        );
    }

    #[test]
    fn read_only_capability_rejects_writes() {
        let mut cap = capability("bool");
        cap.writable = false;
        assert!(cap.validate(&Action::OnOff { on: true }).is_err());
        assert!(cap.validate(&Action::Status).is_ok());
    }

    #[test]
    fn grant_matches_action_only() {
        let now = Utc::now();
        let request = ActionRequest::new("light", "on_off", Action::OnOff { on: true });
        let grant = Grant {
            device_id: "light".to_owned(),
            capability: "on_off".to_owned(),
            action_hash: request.key(),
            expires_at: now + chrono::Duration::seconds(30),
            nonce: "n".to_owned(),
        };
        assert!(grant.matches(&request, now));
        assert!(!grant.matches(
            &ActionRequest::new("light", "on_off", Action::OnOff { on: false }),
            now
        ));
        assert!(!grant.matches(&request, now + chrono::Duration::seconds(31)));
    }
}

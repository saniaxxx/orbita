//! Versioned contracts between the orbita core and its interfaces.
//!
//! These types deliberately contain no Yandex/AAPI/quasar details. A station
//! adapter is the only place where a `Reply` becomes directives and TtsSpeak
//! frames. The contract is v0: it is revised after the OpenClaw/Hermes POC
//! proves how device actions and capabilities are exposed as tools.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONTRACT_VERSION: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Voice,
    Text,
    Event,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Normal,
    Important,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Principal(String),
    Device(String),
    Room(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    #[default]
    None,
    Once,
    UntilAck,
}

/// What an interface can do. The core must not assume station directives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub audio_out: bool,
    #[serde(default)]
    pub volume: bool,
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub directives: bool,
    #[serde(default)]
    pub media: bool,
    /// Forward-compatible interface-specific flags.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundEvent {
    pub contract_version: u32,
    pub turn_id: String,
    pub request_id: String,
    /// Unix milliseconds; the transport owns clock semantics.
    pub timestamp_ms: i64,
    pub input_kind: InputKind,
    pub text: String,
    #[serde(default)]
    pub stt_confidence: Option<f32>,
    pub device_id: String,
    pub room: String,
    pub principal: String,
    pub household: String,
    pub session_key: String,
    pub locale: String,
    pub timezone: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Shadow turns must not produce side effects.
    #[serde(default)]
    pub shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    pub seq: u64,
    pub text: String,
    #[serde(rename = "final")]
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub contract_version: u32,
    pub turn_id: String,
    pub blocks: Vec<TextBlock>,
    #[serde(default)]
    pub should_listen: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundEvent {
    pub contract_version: u32,
    pub event_id: String,
    pub target: Target,
    pub urgency: Urgency,
    pub text: String,
    /// Unix milliseconds; zero means no expiry.
    #[serde(default)]
    pub ttl_ms: u64,
    #[serde(default)]
    pub require_ack: bool,
    #[serde(default)]
    pub retry: RetryPolicy,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("unsupported contract version {0}")]
    Version(u32),
    #[error("text blocks must be ordered, unique and end with final=true")]
    Blocks,
    #[error("reply must contain text blocks or an error")]
    EmptyReply,
    #[error("stt_confidence must be between 0 and 1")]
    Confidence,
    #[error("outbound event must have a non-empty target")]
    Target,
}

impl InboundEvent {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ContractError::Version(self.contract_version));
        }
        for (name, value) in [
            ("turn_id", self.turn_id.as_str()),
            ("request_id", self.request_id.as_str()),
            ("device_id", self.device_id.as_str()),
            ("principal", self.principal.as_str()),
            ("household", self.household.as_str()),
            ("session_key", self.session_key.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::Empty(name));
            }
        }
        if let Some(confidence) = self.stt_confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            return Err(ContractError::Confidence);
        }
        Ok(())
    }
}

impl Reply {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ContractError::Version(self.contract_version));
        }
        if self.blocks.is_empty() && self.error.is_none() {
            return Err(ContractError::EmptyReply);
        }
        let mut previous = None;
        for (index, block) in self.blocks.iter().enumerate() {
            if Some(block.seq) == previous || (previous.is_some() && block.seq < previous.unwrap())
            {
                return Err(ContractError::Blocks);
            }
            if block.is_final != (index + 1 == self.blocks.len()) {
                return Err(ContractError::Blocks);
            }
            previous = Some(block.seq);
        }
        Ok(())
    }
}

impl OutboundEvent {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ContractError::Version(self.contract_version));
        }
        if self.event_id.trim().is_empty() || self.text.trim().is_empty() {
            return Err(ContractError::Empty("event_id/text"));
        }
        match &self.target {
            Target::Principal(value) | Target::Device(value) | Target::Room(value)
                if value.trim().is_empty() =>
            {
                Err(ContractError::Target)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound() -> InboundEvent {
        InboundEvent {
            contract_version: CONTRACT_VERSION,
            turn_id: "turn-1".into(),
            request_id: "req-1".into(),
            timestamp_ms: 1_700_000_000_000,
            input_kind: InputKind::Voice,
            text: "включи мультик".into(),
            stt_confidence: Some(0.94),
            device_id: "station-1".into(),
            room: "детская".into(),
            principal: "household".into(),
            household: "home".into(),
            session_key: "station:station-1".into(),
            locale: "ru-RU".into(),
            timezone: "Europe/Moscow".into(),
            capabilities: Capabilities {
                audio_out: true,
                volume: true,
                directives: true,
                ..Default::default()
            },
            shadow: false,
        }
    }

    #[test]
    fn inbound_round_trips_and_validates() {
        let event = inbound();
        event.validate().unwrap();
        let json = serde_json::to_string(&event).unwrap();
        let decoded: InboundEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn rejects_bad_confidence_and_empty_ids() {
        let mut event = inbound();
        event.stt_confidence = Some(1.2);
        assert_eq!(event.validate(), Err(ContractError::Confidence));

        let mut event = inbound();
        event.turn_id.clear();
        assert_eq!(event.validate(), Err(ContractError::Empty("turn_id")));
    }

    #[test]
    fn reply_stream_requires_one_final_block() {
        let reply = Reply {
            contract_version: CONTRACT_VERSION,
            turn_id: "turn-1".into(),
            blocks: vec![
                TextBlock {
                    seq: 0,
                    text: "Сейчас в Москве".into(),
                    is_final: false,
                },
                TextBlock {
                    seq: 1,
                    text: "облачно.".into(),
                    is_final: true,
                },
            ],
            should_listen: false,
            error: None,
        };
        reply.validate().unwrap();

        let mut bad = reply.clone();
        bad.blocks[0].is_final = true;
        assert_eq!(bad.validate(), Err(ContractError::Blocks));
    }

    #[test]
    fn outbound_target_and_ttl_are_typed() {
        let event = OutboundEvent {
            contract_version: CONTRACT_VERSION,
            event_id: "evt-1".into(),
            target: Target::Room("детская".into()),
            urgency: Urgency::Important,
            text: "Протечка".into(),
            ttl_ms: 60_000,
            require_ack: true,
            retry: RetryPolicy::UntilAck,
        };
        event.validate().unwrap();
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"room\":\"детская\""));
    }
}

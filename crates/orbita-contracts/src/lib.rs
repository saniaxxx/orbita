//! Versioned contracts between the orbita core and its interfaces.
//!
//! These types deliberately contain no Yandex/AAPI/quasar details. A station
//! adapter is the only place where a `Reply` becomes directives and TtsSpeak
//! frames. The contract is v0: it is revised after the OpenClaw/Hermes POC
//! proves how device actions and capabilities are exposed as tools.

use serde::{Deserialize, Serialize};

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
    Household(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    #[default]
    None,
    Once,
    UntilAck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundKind {
    Reminder,
    Alert,
    Announce,
}

/// What an interface can do. The core must not assume station directives;
/// station-specific capabilities are translated by its adapter into this list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub audio_out: bool,
    #[serde(default)]
    pub volume: bool,
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub media: bool,
    /// Actions the interface can perform, e.g. `set_volume`, `set_timer`,
    /// `media_play`, `play_audio`, `show_text`.
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundEvent {
    pub contract_version: u32,
    pub turn_id: String,
    pub request_id: String,
    /// Unix milliseconds; the transport owns clock semantics.
    pub timestamp_ms: u64,
    pub input_kind: InputKind,
    pub text: String,
    #[serde(default)]
    pub stt_confidence: Option<f32>,
    pub device_id: String,
    /// Telegram/mobile may have no room.
    #[serde(default)]
    pub room: Option<String>,
    pub principal: String,
    pub household: String,
    pub session_key: String,
    pub locale: String,
    pub timezone: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Only the adapter may set this; it must not be the only shadow guard.
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
pub struct ReplyBlock {
    pub contract_version: u32,
    pub turn_id: String,
    pub seq: u64,
    pub text: String,
    #[serde(rename = "final")]
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyErrorKind {
    ToolFailed,
    Timeout,
    LlmUnavailable,
    Denied,
    InvalidRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyError {
    pub kind: ReplyErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub contract_version: u32,
    pub turn_id: String,
    /// Aggregate form for storage/tests; live streaming uses `ReplyBlock`.
    pub blocks: Vec<TextBlock>,
    #[serde(default)]
    pub should_listen: bool,
    #[serde(default)]
    pub error: Option<ReplyError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAction {
    /// Absolute level on the device scale (station: 1..=10).
    SetVolume {
        level: u8,
    },
    /// Relative step: louder or quieter by the device's own increment.
    StepVolume {
        direction: VolumeDirection,
    },
    SetTimer {
        seconds: u64,
        label: Option<String>,
    },
    MediaPlay {
        url: String,
    },
    PlayAudio,
    ShowText {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancel {
    pub turn_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundEvent {
    pub contract_version: u32,
    pub event_id: String,
    pub kind: OutboundKind,
    pub target: Target,
    pub household: String,
    pub urgency: Urgency,
    pub text: String,
    pub created_at_ms: u64,
    /// Absolute expiry; zero means no expiry.
    #[serde(default)]
    pub expires_at_ms: u64,
    #[serde(default)]
    pub require_ack: bool,
    #[serde(default)]
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundAckStatus {
    Delivered,
    Failed,
    Expired,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundAck {
    pub event_id: String,
    pub status: OutboundAckStatus,
    pub timestamp_ms: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("unsupported contract version {0}")]
    Version(u32),
    #[error("text blocks must be ordered, contiguous and end with final=true")]
    Blocks,
    #[error("reply must contain text blocks or an error")]
    EmptyReply,
    #[error("stt_confidence must be between 0 and 1")]
    Confidence,
    #[error("outbound event must have a non-empty target")]
    Target,
    #[error("require_ack needs an explicit retry policy")]
    AckRetry,
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
            ("locale", self.locale.as_str()),
            ("timezone", self.timezone.as_str()),
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

impl ReplyBlock {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ContractError::Version(self.contract_version));
        }
        if self.turn_id.trim().is_empty() || self.text.trim().is_empty() {
            return Err(ContractError::Empty("turn_id/text"));
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
        for (index, block) in self.blocks.iter().enumerate() {
            if block.seq != index as u64 || block.text.trim().is_empty() {
                return Err(ContractError::Blocks);
            }
            if block.is_final != (index + 1 == self.blocks.len()) {
                return Err(ContractError::Blocks);
            }
        }
        Ok(())
    }
}

impl OutboundEvent {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ContractError::Version(self.contract_version));
        }
        if self.event_id.trim().is_empty()
            || self.text.trim().is_empty()
            || self.household.trim().is_empty()
        {
            return Err(ContractError::Empty("event_id/text/household"));
        }
        match &self.target {
            Target::Principal(value)
            | Target::Device(value)
            | Target::Room(value)
            | Target::Household(value)
                if value.trim().is_empty() =>
            {
                Err(ContractError::Target)
            }
            _ => {
                if self.require_ack && self.retry == RetryPolicy::None {
                    return Err(ContractError::AckRetry);
                }
                Ok(())
            }
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
            room: Some("детская".into()),
            principal: "person:owner".into(),
            household: "home".into(),
            session_key: "station:station-1".into(),
            locale: "ru-RU".into(),
            timezone: "Europe/Moscow".into(),
            capabilities: Capabilities {
                audio_out: true,
                volume: true,
                actions: vec!["set_volume".into(), "set_timer".into()],
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
    fn reply_stream_requires_contiguous_one_final_block() {
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
        bad.blocks[1].seq = 3;
        assert_eq!(bad.validate(), Err(ContractError::Blocks));
    }

    #[test]
    fn reply_with_only_error_is_valid() {
        let reply = Reply {
            contract_version: CONTRACT_VERSION,
            turn_id: "turn-1".into(),
            blocks: vec![],
            should_listen: false,
            error: Some(ReplyError {
                kind: ReplyErrorKind::ToolFailed,
                message: "home unavailable".into(),
            }),
        };
        reply.validate().unwrap();
    }

    #[test]
    fn outbound_target_ack_and_expiry_are_typed() {
        let event = OutboundEvent {
            contract_version: CONTRACT_VERSION,
            event_id: "evt-1".into(),
            kind: OutboundKind::Alert,
            target: Target::Room("детская".into()),
            household: "home".into(),
            urgency: Urgency::Important,
            text: "Протечка".into(),
            created_at_ms: 1_700_000_000_000,
            expires_at_ms: 1_700_000_060_000,
            require_ack: true,
            retry: RetryPolicy::UntilAck,
        };
        event.validate().unwrap();
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"room\":\"детская\""));

        let mut bad = event;
        bad.retry = RetryPolicy::None;
        assert_eq!(bad.validate(), Err(ContractError::AckRetry));
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut event = inbound();
        event.contract_version = 99;
        assert_eq!(event.validate(), Err(ContractError::Version(99)));
    }
}

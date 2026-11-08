//! Station protocol details: the functions quasar offers in `prepare` and the
//! directives that carry device actions back.
//!
//! quasar sends `prepare.data.directiveFunctions` as a map of
//! `smart_speaker_<name>` → `{description, arguments}` (JSON Schema). It drops
//! unknown functions and invalid arguments silently, so the adapter validates
//! both before a directive leaves the process. Nothing here reaches the core:
//! the core sees only contract [`Capabilities`] and [`DeviceAction`]s.

use std::collections::BTreeMap;

use orbita_contracts::{Capabilities, DeviceAction, VolumeDirection};
use orbita_intents::{Intent, VolumeChange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const FUNCTION_PREFIX: &str = "smart_speaker_";

/// Contract action names derived from the offered station functions.
pub const ACTION_SET_VOLUME: &str = "set_volume";
pub const ACTION_STEP_VOLUME: &str = "step_volume";
pub const ACTION_SET_TIMER: &str = "set_timer";

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct FunctionInfo {
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub description: String,
}

/// What quasar offered to this connection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StationFunctions {
    functions: BTreeMap<String, FunctionInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Directive {
    pub name: String,
    pub arguments: Value,
}

fn normalize(name: &str) -> &str {
    name.strip_prefix(FUNCTION_PREFIX).unwrap_or(name)
}

impl StationFunctions {
    pub fn from_prepare(value: &Value) -> Result<Self, String> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let functions = serde_json::from_value(value.clone())
            .map_err(|error| format!("parsing directiveFunctions: {error}"))?;
        Ok(Self { functions })
    }

    pub fn len(&self) -> usize {
        self.functions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    fn actual_name(&self, normalized: &str) -> Option<&String> {
        self.functions
            .keys()
            .find(|name| normalize(name) == normalized)
    }

    pub fn has(&self, name: &str) -> bool {
        self.actual_name(normalize(name)).is_some()
    }

    /// Channel-independent capabilities for the core.
    pub fn capabilities(&self) -> Capabilities {
        let mut actions = Vec::new();
        if self.has("set_sound_volume") {
            actions.push(ACTION_SET_VOLUME.to_owned());
        }
        if self.has("set_sound_volume_louder") && self.has("set_sound_volume_quieter") {
            actions.push(ACTION_STEP_VOLUME.to_owned());
        }
        if self.has("timer_set") {
            actions.push(ACTION_SET_TIMER.to_owned());
        }
        let volume = !actions.is_empty() && actions.iter().any(|action| action.contains("volume"));
        Capabilities {
            audio_out: true,
            volume,
            display: false,
            media: self.has("player_play"),
            actions,
        }
    }

    /// Whether a deterministic intent can be executed on this connection.
    pub fn supports_intent(&self, intent: &Intent) -> bool {
        self.directive(&intent_action(intent)).is_some()
    }

    /// Render a device action as a station directive, or `None` when the
    /// function was not offered or its arguments do not fit the schema.
    pub fn directive(&self, action: &DeviceAction) -> Option<Directive> {
        let (name, arguments) = match action {
            DeviceAction::SetVolume { level } => {
                ("set_sound_volume", json!({"volume": (*level).clamp(1, 10)}))
            }
            DeviceAction::StepVolume {
                direction: VolumeDirection::Up,
            } => ("set_sound_volume_louder", json!({})),
            DeviceAction::StepVolume {
                direction: VolumeDirection::Down,
            } => ("set_sound_volume_quieter", json!({})),
            DeviceAction::SetTimer { seconds, .. } if *seconds > 0 => {
                ("timer_set", json!({"durationSeconds": seconds}))
            }
            _ => return None,
        };
        let actual = self.actual_name(name)?;
        let info = &self.functions[actual];
        if !arguments_fit(&info.arguments, &arguments) {
            tracing::warn!(name = %actual, %arguments, "arguments do not fit the offered schema");
            return None;
        }
        Some(Directive {
            name: actual.clone(),
            arguments,
        })
    }
}

/// The contract action for a deterministic intent.
pub fn intent_action(intent: &Intent) -> DeviceAction {
    match intent {
        Intent::Volume(VolumeChange::Set(level)) => DeviceAction::SetVolume { level: *level },
        Intent::Volume(VolumeChange::Louder) => DeviceAction::StepVolume {
            direction: VolumeDirection::Up,
        },
        Intent::Volume(VolumeChange::Quieter) => DeviceAction::StepVolume {
            direction: VolumeDirection::Down,
        },
        Intent::Timer { seconds, label } => DeviceAction::SetTimer {
            seconds: *seconds,
            label: label.clone(),
        },
    }
}

/// Minimal JSON Schema check: object shape and `required` keys. quasar runs
/// the full validation afterwards.
fn arguments_fit(schema: &Value, arguments: &Value) -> bool {
    if schema.is_null() || schema.as_object().is_none_or(|schema| schema.is_empty()) {
        return true;
    }
    if schema.get("type").and_then(Value::as_str) == Some("object") && !arguments.is_object() {
        return false;
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let Some(object) = arguments.as_object() else {
            return required.is_empty();
        };
        return required
            .iter()
            .filter_map(Value::as_str)
            .all(|key| object.contains_key(key));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn functions() -> StationFunctions {
        StationFunctions::from_prepare(
            &serde_json::from_str(include_str!("../tests/fixtures/prepare_station.json")).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn real_prepare_map_becomes_contract_capabilities() {
        let functions = functions();
        assert_eq!(functions.len(), 18);
        let capabilities = functions.capabilities();
        assert_eq!(
            capabilities.actions,
            vec!["set_volume", "step_volume", "set_timer"]
        );
        assert!(capabilities.volume && capabilities.audio_out && capabilities.media);
        // No station vocabulary leaks into the contract.
        assert!(
            capabilities
                .actions
                .iter()
                .all(|action| !action.starts_with(FUNCTION_PREFIX))
        );
    }

    #[test]
    fn device_actions_render_as_directives() {
        let functions = functions();
        let set = functions
            .directive(&DeviceAction::SetVolume { level: 4 })
            .unwrap();
        assert_eq!(set.name, "smart_speaker_set_sound_volume");
        assert_eq!(set.arguments, json!({"volume": 4}));
        let clamped = functions
            .directive(&DeviceAction::SetVolume { level: 40 })
            .unwrap();
        assert_eq!(clamped.arguments, json!({"volume": 10}));
        let up = functions
            .directive(&DeviceAction::StepVolume {
                direction: VolumeDirection::Up,
            })
            .unwrap();
        assert_eq!(up.name, "smart_speaker_set_sound_volume_louder");
        let timer = functions
            .directive(&DeviceAction::SetTimer {
                seconds: 120,
                label: None,
            })
            .unwrap();
        assert_eq!(timer.name, "smart_speaker_timer_set");
        assert_eq!(timer.arguments, json!({"durationSeconds": 120}));
    }

    #[test]
    fn unsupported_or_invalid_actions_are_dropped() {
        let functions = functions();
        assert!(
            functions
                .directive(&DeviceAction::SetTimer {
                    seconds: 0,
                    label: None
                })
                .is_none()
        );
        assert!(
            functions
                .directive(&DeviceAction::ShowText { text: "x".into() })
                .is_none()
        );
        let empty = StationFunctions::default();
        assert!(
            empty
                .directive(&DeviceAction::SetVolume { level: 3 })
                .is_none()
        );
        assert!(empty.capabilities().actions.is_empty());
        assert!(!empty.supports_intent(&Intent::Volume(VolumeChange::Louder)));
    }
}

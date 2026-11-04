//! Station channel: the functions quasar offers in `prepare` and the
//! directives that carry them back.
//!
//! quasar sends `prepare.data.directiveFunctions` as a map of
//! `smart_speaker_<name>` → `{description, arguments}` (JSON Schema from Zod).
//! It strips the prefix, validates the arguments and silently drops anything
//! unknown, so this adapter validates names and arguments before they leave the
//! process.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::action::{Action, FunctionSpec, SessionContext, VolumeChange, normalize_function_name};

pub const FUNCTION_PREFIX: &str = "smart_speaker_";

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct DirectiveFunctionInfo {
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub description: String,
}

/// What quasar offered to this connection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StationSession {
    functions: BTreeMap<String, DirectiveFunctionInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Directive {
    pub name: String,
    pub arguments: Value,
}

impl StationSession {
    pub fn from_prepare(value: &Value) -> Result<Self, String> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let functions = serde_json::from_value(value.clone())
            .map_err(|error| format!("parsing directiveFunctions: {error}"))?;
        Ok(Self { functions })
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.functions.len()
    }

    /// Functions as the core sees them: channel-independent specs.
    pub fn context(&self) -> SessionContext {
        SessionContext {
            functions: self
                .functions
                .iter()
                .map(|(name, info)| FunctionSpec {
                    name: name.clone(),
                    description: info.description.clone(),
                    arguments: info.arguments.clone(),
                })
                .collect(),
        }
    }

    /// Accepts both `timer_set` and `smart_speaker_timer_set`.
    pub fn has(&self, name: &str) -> bool {
        self.actual_name(normalize_function_name(name)).is_some()
    }

    fn actual_name(&self, normalized: &str) -> Option<&String> {
        self.functions
            .keys()
            .find(|name| normalize_function_name(name) == normalized)
    }

    /// Render an action as a station directive, or `None` when the function
    /// was not offered or its arguments do not fit the schema.
    pub fn directive(&self, action: &Action) -> Option<Directive> {
        let (name, arguments) = match action {
            Action::SetVolume(change) => match change {
                VolumeChange::Louder => ("set_sound_volume_louder", json!({})),
                VolumeChange::Quieter => ("set_sound_volume_quieter", json!({})),
                VolumeChange::Set(level) => ("set_sound_volume", json!({"volume": level})),
            },
            Action::SetTimer { seconds, .. } => ("timer_set", json!({"durationSeconds": seconds})),
            Action::Directive { name, arguments } => {
                (normalize_function_name(name), arguments.clone())
            }
            // Server-side MCP tools are executed by the agent, not the station.
            Action::ToolCall { .. } => return None,
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

/// Minimal JSON Schema check: the arguments must be an object when the schema
/// describes one, and every `required` property must be present. quasar runs
/// the full Zod validation afterwards.
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

    fn session() -> StationSession {
        StationSession::from_prepare(&json!({
            "smart_speaker_set_sound_volume_louder": {
                "description": "Atomically make smart speaker volume louder",
                "arguments": {}
            },
            "smart_speaker_set_sound_volume_quieter": {
                "description": "quieter",
                "arguments": {}
            },
            "smart_speaker_set_sound_volume": {
                "description": "set",
                "arguments": {
                    "type": "object",
                    "required": ["volume"],
                    "properties": {"volume": {"type": "integer", "minimum": 1, "maximum": 10}}
                }
            },
            "smart_speaker_timer_set": {
                "description": "timer",
                "arguments": {
                    "type": "object",
                    "required": ["durationSeconds"],
                    "properties": {"durationSeconds": {"type": "integer"}}
                }
            },
            "smart_speaker_bluetooth": {
                "description": "bt",
                "arguments": {"type": "object", "required": ["enable"]}
            }
        }))
        .unwrap()
    }

    #[test]
    fn parses_the_prepare_map() {
        let session = session();
        assert_eq!(session.len(), 5);
        assert!(session.has("timer_set"));
        assert!(session.has("smart_speaker_timer_set"));
        assert!(!session.has("alarm_add"));
        assert_eq!(session.context().functions.len(), 5);
    }

    #[test]
    fn maps_volume_and_timer() {
        let session = session();
        assert_eq!(
            session
                .directive(&Action::SetVolume(VolumeChange::Louder))
                .unwrap()
                .name,
            "smart_speaker_set_sound_volume_louder"
        );
        let set = session
            .directive(&Action::SetVolume(VolumeChange::Set(4)))
            .unwrap();
        assert_eq!(set.arguments, json!({"volume": 4}));
        let timer = session
            .directive(&Action::SetTimer {
                seconds: 300,
                label: None,
            })
            .unwrap();
        assert_eq!(timer.arguments, json!({"durationSeconds": 300}));
    }

    #[test]
    fn drops_functions_that_were_not_offered() {
        let session = session();
        assert!(
            session
                .directive(&Action::Directive {
                    name: "alarm_add".into(),
                    arguments: json!({"time": "07:00"}),
                })
                .is_none()
        );
    }

    #[test]
    fn validates_required_arguments() {
        let session = session();
        assert!(
            session
                .directive(&Action::Directive {
                    name: "set_sound_volume".into(),
                    arguments: json!({"volume": 3}),
                })
                .is_some()
        );
        assert!(
            session
                .directive(&Action::Directive {
                    name: "set_sound_volume".into(),
                    arguments: json!({}),
                })
                .is_none()
        );
        assert!(
            session
                .directive(&Action::Directive {
                    name: "bluetooth".into(),
                    arguments: json!({"enable": true}),
                })
                .is_some()
        );
    }
}

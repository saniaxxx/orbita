//! WebSocket messages between quasar-server and the processor.
//!
//! Mirrors `@v3rt3p/types/processor` and is deliberately a small serde model:
//! metadata is free-form, directive schemas are passed through as JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    #[serde(rename = "prepare")]
    Prepare(PrepareData),
    #[serde(rename = "process")]
    Process(ProcessData),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrepareData {
    #[serde(rename = "directiveFunctions")]
    pub directive_functions: BTreeMap<String, DirectiveFunctionInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DirectiveFunctionInfo {
    pub arguments: Value,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessData {
    pub metadata: Metadata,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Metadata {
    #[serde(default)]
    pub internal: Value,
    #[serde(default, rename = "readyToUse")]
    pub ready_to_use: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    #[serde(rename = "partialResponse")]
    PartialResponse(PartialResponseData),
}

#[derive(Debug, Clone, Serialize)]
pub struct PartialResponseData {
    pub directives: Vec<Directive>,
    pub text: String,
    pub finished: bool,
    #[serde(rename = "shouldListen")]
    pub should_listen: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Directive {
    pub name: String,
    pub arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_prepare() {
        let message: ClientMessage = serde_json::from_value(json!({
            "type": "prepare",
            "data": {
                "directiveFunctions": {
                    "set_lights": {
                        "arguments": {"type": "object"},
                        "description": "Turn lights on or off"
                    }
                }
            }
        }))
        .unwrap();
        let ClientMessage::Prepare(prepare) = message else {
            panic!("expected prepare");
        };
        assert_eq!(
            prepare.directive_functions["set_lights"].description,
            "Turn lights on or off"
        );
    }

    #[test]
    fn parses_process_with_free_form_metadata() {
        let message: ClientMessage = serde_json::from_value(json!({
            "type": "process",
            "data": {
                "metadata": {
                    "internal": {"deviceId": "duid"},
                    "readyToUse": {"inputPersonVoiceLabel": {"description": "name", "value": "Аня"}}
                },
                "text": "привет"
            }
        }))
        .unwrap();
        let ClientMessage::Process(process) = message else {
            panic!("expected process");
        };
        assert_eq!(process.text, "привет");
        assert_eq!(process.metadata.internal["deviceId"], "duid");
    }

    #[test]
    fn serialises_partial_response() {
        let message = ServerMessage::PartialResponse(PartialResponseData {
            directives: vec![Directive {
                name: "set_lights".to_owned(),
                arguments: json!({"state": 1}),
            }],
            text: "Включаю".to_owned(),
            finished: true,
            should_listen: false,
        });
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            json!({
                "type": "partialResponse",
                "data": {
                    "directives": [{"name": "set_lights", "arguments": {"state": 1}}],
                    "text": "Включаю",
                    "finished": true,
                    "shouldListen": false
                }
            })
        );
    }
}

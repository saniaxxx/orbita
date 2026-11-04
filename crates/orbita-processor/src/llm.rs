//! LLM message shape, response parsing and the `schedule` duration syntax.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: content.into(),
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: content.into(),
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: content.into(),
            name: None,
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_owned(),
            content: content.into(),
            name: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawLlmResponse {
    #[serde(default)]
    function_calls: Vec<RawFunctionCall>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    continue_dialog: bool,
    #[serde(default)]
    can_cache: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    schedule: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: Value,
    pub schedule: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredResponse {
    pub function_calls: Vec<FunctionCall>,
    pub text: String,
    pub require_more_input: bool,
    pub can_cache: bool,
}

pub fn parse_llm_response(raw: &str) -> Result<StructuredResponse, serde_json::Error> {
    let response: RawLlmResponse = serde_json::from_str(raw)?;
    let function_calls = response
        .function_calls
        .into_iter()
        .map(|call| FunctionCall {
            name: call.name,
            arguments: call.args,
            schedule: call.schedule.as_deref().map(parse_schedule),
        })
        .collect();
    Ok(StructuredResponse {
        function_calls,
        text: response.text,
        require_more_input: response.continue_dialog,
        can_cache: response.can_cache,
    })
}

/// Port of the upstream `schedule` parser: space-separated parts such as
/// `1h 20m`, each a decimal number followed by `ms`, `s`, `m`, `h` or `d`.
pub fn parse_schedule(value: &str) -> Duration {
    let mut milliseconds = 0.0_f64;
    for part in value.split_whitespace() {
        let (number, coefficient) = if let Some(number) = part.strip_suffix("ms") {
            (number, 1.0)
        } else if let Some(number) = part.strip_suffix('s') {
            (number, 1_000.0)
        } else if let Some(number) = part.strip_suffix('m') {
            (number, 60_000.0)
        } else if let Some(number) = part.strip_suffix('h') {
            (number, 3_600_000.0)
        } else if let Some(number) = part.strip_suffix('d') {
            (number, 86_400_000.0)
        } else {
            tracing::warn!(%part, "failed to parse schedule part");
            continue;
        };

        match number.parse::<f64>() {
            Ok(number) if number.is_finite() && number >= 0.0 => {
                milliseconds += coefficient * number;
            }
            _ => tracing::warn!(%part, "failed to parse schedule part"),
        }
    }
    Duration::from_secs_f64(milliseconds / 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_plain_text_answer() {
        let parsed = parse_llm_response(
            &json!({
                "function_calls": [],
                "text": "Привет",
                "continue_dialog": false,
                "can_cache": true
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(parsed.text, "Привет");
        assert!(parsed.function_calls.is_empty());
        assert!(!parsed.require_more_input);
        assert!(parsed.can_cache);
    }

    #[test]
    fn parses_function_calls_and_schedules() {
        let parsed = parse_llm_response(
            &json!({
                "function_calls": [
                    {"name": "search_search", "args": {"query": "погода"}},
                    {"name": "set_lights", "args": {"state": 0}, "schedule": "5m"}
                ],
                "text": "Сейчас посмотрю",
                "continue_dialog": false
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(parsed.function_calls.len(), 2);
        assert_eq!(parsed.function_calls[0].arguments["query"], "погода");
        assert_eq!(
            parsed.function_calls[1].schedule,
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn parses_schedule_units_and_sums_them() {
        assert_eq!(parse_schedule("5m"), Duration::from_secs(300));
        assert_eq!(parse_schedule("1h 20m"), Duration::from_secs(4_800));
        assert_eq!(parse_schedule("500ms"), Duration::from_millis(500));
        assert_eq!(parse_schedule("1.5s"), Duration::from_millis(1_500));
        assert_eq!(parse_schedule("nonsense"), Duration::ZERO);
    }
}

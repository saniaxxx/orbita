//! Channel-independent actions and the outcome of one turn.
//!
//! The agent core never builds station protocol messages. It decides what
//! should happen ([`Action`]) and what to say; a channel adapter turns actions
//! into its own primitives: directives for the station, text and notifications
//! for Telegram or the web.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelId {
    Station,
    Telegram,
    Web,
    Cli,
}

impl ChannelId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Station => "station",
            Self::Telegram => "telegram",
            Self::Web => "web",
            Self::Cli => "cli",
        }
    }
}

/// Who is talking, through which channel. Station sessions key on the duid,
/// Telegram on the chat/user id, the web on the session owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
    pub channel: ChannelId,
    pub principal: String,
}

impl SessionKey {
    pub fn new(channel: ChannelId, principal: impl Into<String>) -> Self {
        Self {
            channel,
            principal: principal.into(),
        }
    }

    pub fn station(duid: impl Into<String>) -> Self {
        Self::new(ChannelId::Station, duid)
    }

    /// Dialogue history key. Station keeps the bare duid so existing history
    /// stays reachable; other channels are namespaced.
    pub fn history_id(&self) -> String {
        match self.channel {
            ChannelId::Station => self.principal.clone(),
            _ => format!("{}:{}", self.channel.as_str(), self.principal),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Safe,
    Sensitive,
    Dangerous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeChange {
    Louder,
    Quieter,
    /// Absolute level, 1..=10 (the station scale).
    Set(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SetVolume(VolumeChange),
    /// Native timer on the target device; the station adapter mirrors it in
    /// SQLite for audit and cancellation.
    SetTimer {
        seconds: u64,
        label: Option<String>,
    },
    /// A directive-callable function offered by the channel (from `prepare`).
    /// Arguments are validated by the adapter before they leave the process.
    Directive {
        name: String,
        arguments: Value,
    },
    /// A server-side MCP tool. The agent executes it and feeds the result back
    /// to the model; it never reaches a channel.
    ToolCall {
        name: String,
        arguments: Value,
    },
}

impl Action {
    pub fn risk(&self) -> Risk {
        match self {
            Self::SetVolume(_) | Self::SetTimer { .. } => Risk::Safe,
            Self::Directive { name, .. } => {
                let name = name.strip_prefix("smart_speaker_").unwrap_or(name);
                match name {
                    "bluetooth" => Risk::Sensitive,
                    _ => Risk::Safe,
                }
            }
            Self::ToolCall { .. } => Risk::Safe,
        }
    }
}

/// One turn's result: what to say and what to do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    pub text: String,
    pub actions: Vec<Action>,
}

impl Outcome {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            actions: Vec::new(),
        }
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }
}

/// A function the channel offers to the model for this session.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments, as the channel sent it.
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionContext {
    pub functions: Vec<FunctionSpec>,
}

impl SessionContext {
    pub fn has(&self, normalized_name: &str) -> bool {
        self.functions
            .iter()
            .any(|function| normalize_function_name(&function.name) == normalized_name)
    }
}

/// Station functions arrive as `smart_speaker_*`; the prefix is protocol
/// noise, the name is the part after it.
pub fn normalize_function_name(name: &str) -> &str {
    name.strip_prefix("smart_speaker_").unwrap_or(name)
}

/// `durationSeconds` or `amountMs, direction`, not the full JSON Schema.
fn compact_arguments(schema: &Value) -> String {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// Build the JSON-grammar reply the model is asked for when functions exist.
pub fn function_prompt(context: &SessionContext) -> String {
    let mut block =
        String::from("Доступные функции устройства. Вызывай их, когда просьба им соответствует:\n");
    for function in &context.functions {
        block.push_str(&format!(
            "- {}({}): {}\n",
            normalize_function_name(&function.name),
            compact_arguments(&function.arguments),
            function.description
        ));
    }
    block.push_str(
        "Ответ верни строго одним JSON-объектом: {\"text\": \"короткий ответ\", \
         \"function_calls\": [{\"name\": \"имя\", \"arguments\": {}}]}. \
         Если функция не нужна, оставь function_calls пустым. \
         Не вызывай функцию, если в этом не было явной просьбы.",
    );
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_the_station_prefix() {
        assert_eq!(
            normalize_function_name("smart_speaker_timer_set"),
            "timer_set"
        );
        assert_eq!(normalize_function_name("timer_set"), "timer_set");
    }

    #[test]
    fn risk_follows_the_function() {
        assert_eq!(
            Action::Directive {
                name: "smart_speaker_set_sound_volume_louder".into(),
                arguments: json!({}),
            }
            .risk(),
            Risk::Safe
        );
        assert_eq!(
            Action::Directive {
                name: "smart_speaker_bluetooth".into(),
                arguments: json!({"enable": true}),
            }
            .risk(),
            Risk::Sensitive
        );
    }

    #[test]
    fn station_history_keeps_the_bare_duid() {
        assert_eq!(SessionKey::station("duid-1").history_id(), "duid-1");
        assert_eq!(
            SessionKey::new(ChannelId::Telegram, "42").history_id(),
            "telegram:42"
        );
    }
}

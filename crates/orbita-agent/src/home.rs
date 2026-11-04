//! Home control inside the agent: Wiren Board through `orbita-home`.
//!
//! Lights are controlled deterministically: the fast path resolves the device
//! by alias, checks the capability risk and publishes through `orbita-home`.
//! Sensitive and dangerous actions are refused here until the grant policy and
//! a second confirmation channel exist.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbita_home::{
    Action, ActionRequest, DeviceConfig, DeviceView, Grant, Home, HomeConfig, Risk, SystemClock,
    mqtt,
};
use tokio::sync::Mutex;

pub struct HomeService {
    home: Arc<Home>,
    pending: Mutex<Vec<PendingAction>>,
}

struct PendingAction {
    request: ActionRequest,
    description: String,
    code: String,
    expires_at: DateTime<Utc>,
}

/// Result of a home command.
pub enum HomeOutcome {
    Done(String),
    /// Sensitive or dangerous action: a code went to the log, the owner must
    /// confirm it through another channel (`POST /confirm`).
    NeedsConfirmation {
        reply: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomeCommand {
    pub device_id: String,
    pub capability: String,
    pub action: Action,
    pub reply: String,
}

impl HomeService {
    pub async fn start(path: &str) -> Result<Self, String> {
        let config = Arc::new(HomeConfig::from_path(path).map_err(|e| e.to_string())?);
        let home = mqtt::start(config, Arc::new(SystemClock::new()))
            .await
            .map_err(|e| e.to_string())?;
        // Retained state arrives right after subscribing.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(Self {
            home,
            pending: Mutex::new(Vec::new()),
        })
    }

    pub fn list(&self) -> serde_json::Value {
        serde_json::to_value(self.home.list()).unwrap_or(serde_json::Value::Null)
    }

    /// "включи свет в детской", "выключи кухню", "яркость 40 в спальне".
    pub fn parse(&self, text: &str) -> Option<HomeCommand> {
        let text = text.to_lowercase();
        let brightness = brightness_value(&text);
        let on = text.contains("включи") || text.contains("зажги") || text.contains("включить");
        let off = text.contains("выключи")
            || text.contains("погаси")
            || text.contains("выключить")
            || text.contains("потуши");
        if !on && !off && brightness.is_none() {
            return None;
        }
        let device = self.resolve(&text)?;
        let name = device.name.clone();
        let (capability, action, reply) = match brightness {
            Some(value) if device.capability("brightness").is_some() => (
                "brightness".to_owned(),
                Action::Number {
                    value: value as f64,
                },
                format!("Яркость {value} процентов, {name}."),
            ),
            _ if off => (
                "on_off".to_owned(),
                Action::OnOff { on: false },
                format!("Выключила: {name}."),
            ),
            _ => (
                "on_off".to_owned(),
                Action::OnOff { on: true },
                format!("Включила: {name}."),
            ),
        };
        Some(HomeCommand {
            device_id: device.id.clone(),
            capability,
            action,
            reply,
        })
    }

    fn resolve(&self, text: &str) -> Option<DeviceChoice> {
        let mut best: Option<((usize, usize, usize), DeviceChoice)> = None;
        for view in self.home.list() {
            for device in &self.home.config().devices {
                if device.id != view.id {
                    continue;
                }
                for alias in &device.aliases {
                    let alias = alias.to_lowercase();
                    let Some(score) = alias_match_score(text, &alias) else {
                        continue;
                    };
                    if best
                        .as_ref()
                        .is_none_or(|(best_score, _)| score > *best_score)
                    {
                        best = Some((score, device_choice(&view, device)));
                    }
                }
            }
        }
        best.map(|(_, choice)| choice)
    }

    pub async fn execute(&self, command: &HomeCommand) -> Result<HomeOutcome, String> {
        let device = self
            .home
            .config()
            .devices
            .iter()
            .find(|device| device.id == command.device_id)
            .ok_or_else(|| format!("unknown device {}", command.device_id))?;
        let capability = device
            .capability(&command.capability)
            .ok_or_else(|| format!("unknown capability {}", command.capability))?;
        let request = ActionRequest {
            device_id: command.device_id.clone(),
            capability: command.capability.clone(),
            action: command.action.clone(),
        };
        if capability.risk != Risk::Safe {
            let code = confirmation_code();
            let expires_at = Utc::now() + Duration::minutes(2);
            tracing::warn!(
                device = %command.device_id,
                capability = %command.capability,
                %code,
                "home action needs confirmation; code is not spoken"
            );
            self.pending.lock().await.push(PendingAction {
                request,
                description: command.reply.clone(),
                code: code.clone(),
                expires_at,
            });
            return Ok(HomeOutcome::NeedsConfirmation {
                reply: format!(
                    "{} Это действие требует подтверждения вторым каналом: код записан в журнале агента.",
                    command.reply
                ),
            });
        }
        self.home
            .apply(&request, None)
            .await
            .map(|_| HomeOutcome::Done(command.reply.clone()))
            .map_err(|e| e.to_string())
    }

    /// Confirm a pending sensitive/dangerous action by its code.
    pub async fn confirm(&self, code: &str) -> Result<String, String> {
        let now = Utc::now();
        let mut pending = self.pending.lock().await;
        pending.retain(|action| action.expires_at > now);
        let Some(index) = pending.iter().position(|action| action.code == code) else {
            return Err("unknown or expired confirmation code".to_owned());
        };
        let action = pending.remove(index);
        let grant = Grant {
            device_id: action.request.device_id.clone(),
            capability: action.request.capability.clone(),
            action_hash: action.request.key(),
            expires_at: action.expires_at,
            nonce: action.code.clone(),
        };
        self.home
            .apply(&action.request, Some(&grant))
            .await
            .map(|_| action.description)
            .map_err(|e| e.to_string())
    }

    pub async fn pending(&self) -> serde_json::Value {
        let now = Utc::now();
        let pending = self.pending.lock().await;
        serde_json::json!(
            pending
                .iter()
                .filter(|action| action.expires_at > now)
                .map(|action| serde_json::json!({
                    "device": action.request.device_id,
                    "capability": action.request.capability,
                    "action": action.request.action.canonical(),
                    "expires_at": action.expires_at,
                }))
                .collect::<Vec<_>>()
        )
    }

    /// Compact tool description for the LLM prompt.
    pub fn prompt(&self) -> String {
        let mut block = String::from(
            "Дом (Wiren Board). Инструменты: home_list() — устройства; home_get(device) — состояние;              home_set(device, capability, value) — включить/выключить/яркость.              Управлять светом можно и обычной фразой, инструменты — для сложных формулировок.\n",
        );
        for device in &self.home.config().devices {
            let aliases = device.aliases.join(", ");
            block.push_str(&format!("- {}: {aliases}\n", device.id));
        }
        block
    }

    /// Execute a model tool call; `None` means the name is not a home tool.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Option<Result<String, String>> {
        match name {
            "home_list" => Some(Ok(self.list().to_string())),
            "home_get" => {
                let device = arguments.get("device").and_then(|v| v.as_str())?;
                Some(
                    self.home
                        .get(device)
                        .map(|view| serde_json::to_string(&view).unwrap_or_default())
                        .map_err(|e| e.to_string()),
                )
            }
            "home_set" => {
                let device = arguments.get("device").and_then(|v| v.as_str())?;
                let capability = arguments
                    .get("capability")
                    .and_then(|v| v.as_str())
                    .unwrap_or("on_off");
                let value = arguments.get("value").and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_f64().map(|n| n.to_string()))
                })?;
                let action = match capability {
                    "brightness" => Action::Number {
                        value: value.parse().ok()?,
                    },
                    _ => Action::OnOff {
                        on: matches!(value.as_str(), "1" | "true" | "on" | "вкл" | "включить"),
                    },
                };
                let command = HomeCommand {
                    device_id: device.to_owned(),
                    capability: capability.to_owned(),
                    action,
                    reply: format!("Готово: {device}."),
                };
                Some(self.execute(&command).await.map(|outcome| match outcome {
                    HomeOutcome::Done(reply) => reply,
                    HomeOutcome::NeedsConfirmation { reply } => reply,
                }))
            }
            _ => None,
        }
    }

    /// "какой свет включен" — a short spoken summary.
    pub fn status_line(&self) -> String {
        let on: Vec<String> = self
            .home
            .list()
            .into_iter()
            .filter(|view| {
                view.capabilities
                    .iter()
                    .any(|cap| cap.name == "on_off" && cap.value.as_deref() == Some("1"))
            })
            .map(|view| view.room.unwrap_or(view.id))
            .collect();
        if on.is_empty() {
            "Сейчас весь свет выключен.".to_owned()
        } else {
            format!("Сейчас включён свет: {}.", on.join(", "))
        }
    }
}

struct DeviceChoice {
    id: String,
    /// Human name for replies: the room, not the first configured alias.
    name: String,
    capabilities: Vec<String>,
}

fn device_choice(view: &DeviceView, device: &DeviceConfig) -> DeviceChoice {
    DeviceChoice {
        id: device.id.clone(),
        name: view
            .room
            .clone()
            .or_else(|| device.room.clone())
            .filter(|room| !room.trim().is_empty())
            .or_else(|| device.aliases.first().cloned())
            .unwrap_or_else(|| device.id.clone()),
        capabilities: device
            .capabilities
            .iter()
            .map(|cap| cap.name.clone())
            .collect(),
    }
}

/// Rank an alias against the utterance: more words first (the specific
/// «туалет спальня» beats the generic «туалет»), then an exact substring, then
/// the longer alias.
fn alias_match_score(text: &str, alias: &str) -> Option<(usize, usize, usize)> {
    let exact = text.contains(alias);
    let alias_stems = stems(alias);
    if !exact && !contains_stems(&stems(text), &alias_stems) {
        return None;
    }
    Some((alias_stems.len(), usize::from(exact), alias.chars().count()))
}

fn contains_stems(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Russian noun and adjective endings, longest first so «-ого» wins over «-о».
const ENDINGS: &[&str] = &[
    "ыми", "ими", "ого", "его", "ому", "ему", "ая", "яя", "ое", "ее", "ые", "ие", "ый", "ий", "ых",
    "их", "ой", "ей", "ою", "ею", "ую", "юю", "ом", "ем", "ам", "ям", "ах", "ях", "а", "я", "у",
    "ю", "е", "и", "ы", "о", "ь",
];

/// Crude Russian stemmer: enough to match a room name in any case form.
fn stem(word: &str) -> &str {
    for ending in ENDINGS {
        if let Some(base) = word.strip_suffix(ending)
            && base.chars().count() >= 4
        {
            return base;
        }
    }
    word
}

/// Stemmed words of a phrase. One-letter words (prepositions) are dropped so
/// «свет в гостиной» matches «свет гостиной».
fn stems(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.replace('ё', "е"))
        .filter(|token| {
            let mut chars = token.chars();
            !matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphabetic())
        })
        .map(|token| stem(&token).to_owned())
        .collect()
}

impl DeviceChoice {
    fn capability(&self, name: &str) -> Option<()> {
        self.capabilities
            .iter()
            .any(|cap| cap == name)
            .then_some(())
    }
}

fn brightness_value(text: &str) -> Option<u32> {
    let bytes: Vec<char> = text.chars().collect();
    for (index, _) in text.match_indices("яркост") {
        let rest: String = bytes[index..].iter().collect();
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(value) = digits.parse::<u32>() {
            return Some(value.min(100));
        }
    }
    for marker in [" на ", "до "] {
        if let Some(index) = text.find(marker) {
            let rest = &text[index + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(value) = digits.parse::<u32>()
                && text.contains("процент")
            {
                return Some(value.min(100));
            }
        }
    }
    None
}

fn confirmation_code() -> String {
    // Six digits from the system clock and address entropy; not suitable as a
    // secret, enough against a mistaken voice confirmation.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    format!("{:06}", (nanos ^ std::process::id()) % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_russian_case_forms() {
        for (text, alias) in [
            ("включи свет в спальне", "спальня"),
            ("выключи спальню", "спальня"),
            ("включи свет в спальне", "свет спальня"),
            ("свет на кухне", "кухня"),
            ("включи свет в кабинете", "кабинет"),
            ("свет в детской", "детская"),
            ("свет в гостиной", "гостиная"),
            ("свет в прихожей", "прихожая"),
            ("свет в тамбуре", "тамбур"),
            ("свет в туалете спальни", "туалет спальня"),
            ("включи прикроватный 1", "прикроватный 1"),
        ] {
            assert!(
                alias_match_score(&text.to_lowercase(), alias).is_some(),
                "{text} / {alias}"
            );
        }
    }

    #[test]
    fn does_not_match_other_rooms() {
        for (text, alias) in [
            ("включи свет в спальне", "кухня"),
            ("выключи кухню", "спальня"),
            ("свет в гостиной", "прихожая"),
            ("включи свет", "кухня"),
        ] {
            assert!(
                alias_match_score(&text.to_lowercase(), alias).is_none(),
                "{text} / {alias}"
            );
        }
    }

    #[test]
    fn prefers_the_more_specific_alias() {
        let text = "включи свет в туалете спальни";
        let bedroom_toilet = alias_match_score(text, "туалет спальня").unwrap();
        let toilet = alias_match_score(text, "туалет").unwrap();
        assert!(bedroom_toilet > toilet);

        let text = "включи свет в спальне";
        let with_light = alias_match_score(text, "свет спальня").unwrap();
        let room = alias_match_score(text, "спальня").unwrap();
        assert!(with_light > room);
    }
}

//! Home control inside the agent: Wiren Board through `orbita-home`.
//!
//! Lights are controlled deterministically: the fast path resolves the device
//! by alias, checks the capability risk and publishes through `orbita-home`.
//! Sensitive and dangerous actions are refused here until the grant policy and
//! a second confirmation channel exist.

use std::sync::Arc;

use orbita_home::{Action, ActionRequest, Home, HomeConfig, Risk, SystemClock, mqtt};

pub struct HomeService {
    home: Arc<Home>,
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
        Ok(Self { home })
    }

    pub fn list(&self) -> serde_json::Value {
        serde_json::to_value(self.home.list()).unwrap_or(serde_json::Value::Null)
    }

    /// "включи свет в детской", "выключи кухню", "яркость 40 в спальне".
    pub fn parse(&self, text: &str) -> Option<HomeCommand> {
        let text = text.to_lowercase();
        let brightness = brightness_value(&text);
        let on = text.contains("включи")
            || text.contains("зажги")
            || text.contains("включить");
        let off = text.contains("выключи")
            || text.contains("погаси")
            || text.contains("выключить")
            || text.contains("потуши");
        if !on && !off && brightness.is_none() {
            return None;
        }
        let device = self.resolve(&text)?;
        let name = device
            .aliases
            .first()
            .cloned()
            .unwrap_or_else(|| device.id.clone());
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
        let mut best: Option<(usize, DeviceChoice)> = None;
        for view in self.home.list() {
            for device in &self.home.config().devices {
                if device.id != view.id {
                    continue;
                }
                for alias in &device.aliases {
                    let alias = alias.to_lowercase();
                    if !text.contains(&alias) {
                        continue;
                    }
                    let score = alias.len();
                    if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
                        best = Some((
                            score,
                            DeviceChoice {
                                id: device.id.clone(),
                                aliases: device.aliases.clone(),
                                capabilities: device
                                    .capabilities
                                    .iter()
                                    .map(|cap| cap.name.clone())
                                    .collect(),
                            },
                        ));
                    }
                }
            }
        }
        best.map(|(_, choice)| choice)
    }

    pub async fn execute(&self, command: &HomeCommand) -> Result<(), String> {
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
        if capability.risk != Risk::Safe {
            return Err(format!(
                "{} requires confirmation; voice control is not an authentication",
                command.capability
            ));
        }
        let request = ActionRequest {
            device_id: command.device_id.clone(),
            capability: command.capability.clone(),
            action: command.action.clone(),
        };
        self.home
            .apply(&request, None)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
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
    aliases: Vec<String>,
    capabilities: Vec<String>,
}

impl DeviceChoice {
    fn capability(&self, name: &str) -> Option<()> {
        self.capabilities.iter().any(|cap| cap == name).then_some(())
    }
}

fn brightness_value(text: &str) -> Option<u32> {
    let bytes: Vec<char> = text.chars().collect();
    for (index, _) in text.match_indices("яркост") {
        let rest: String = bytes[index..].iter().collect();
        let digits: String = rest.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
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

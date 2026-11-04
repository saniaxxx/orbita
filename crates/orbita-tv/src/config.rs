//! TV inventory loaded from a small JSON file.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TvConfig {
    #[serde(default)]
    pub devices: Vec<TvDevice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvDevice {
    pub id: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub host: String,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub cert: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// Broadcast address for Wake-on-LAN; defaults to 255.255.255.255.
    #[serde(default)]
    pub broadcast: Option<String>,
}

fn default_protocol() -> String {
    "remote_v2".to_owned()
}

fn default_true() -> bool {
    true
}

fn default_api_port() -> u16 {
    6466
}

impl TvConfig {
    pub fn from_path(path: &str) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("reading TV config {path}: {error}"))?;
        let config: Self = serde_json::from_str(&raw)
            .map_err(|error| format!("parsing TV config {path}: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_env() -> Result<Self, String> {
        let path = orbita_core::env_or("TV_CONFIG", "/etc/orbita/tv.json");
        Self::from_path(&path)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for device in &self.devices {
            if device.id.trim().is_empty() {
                return Err("TV device id must not be empty".to_owned());
            }
            if !ids.insert(device.id.as_str()) {
                return Err(format!("duplicate TV device id {}", device.id));
            }
            if device.host.trim().is_empty() {
                return Err(format!("TV {} has an empty host", device.id));
            }
            if device.enabled && device.protocol == "remote_v2" {
                for (name, path) in [("cert", &device.cert), ("key", &device.key)] {
                    let Some(path) = path else {
                        return Err(format!("TV {} runs remote_v2 without {name}", device.id));
                    };
                    if !Path::new(path).is_file() {
                        return Err(format!(
                            "TV {} {name} file does not exist: {path}",
                            device.id
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn enabled(&self) -> impl Iterator<Item = &TvDevice> {
        self.devices.iter().filter(|device| device.enabled)
    }

    /// Resolve a spoken or typed device reference: id, room or alias.
    pub fn resolve(&self, query: Option<&str>) -> Result<&TvDevice, String> {
        let enabled: Vec<&TvDevice> = self.enabled().collect();
        let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) else {
            return match enabled.as_slice() {
                [device] => Ok(device),
                [] => Err("нет включённых телевизоров в конфиге".to_owned()),
                _ => Err(format!(
                    "телевизоров несколько ({}), укажи какой",
                    enabled
                        .iter()
                        .map(|device| device.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            };
        };
        let needle = query.to_lowercase();
        // A room in the utterance is decisive: it must select that TV or
        // report that it is not controllable. Otherwise a generic «телевизор»
        // alias could pick the wrong room.
        let by_room: Vec<&TvDevice> = self
            .devices
            .iter()
            .filter(|device| {
                device
                    .room
                    .as_deref()
                    .is_some_and(|room| phrase_matches(&needle, room))
            })
            .collect();
        if !by_room.is_empty() {
            let controllable: Vec<&TvDevice> = by_room
                .iter()
                .copied()
                .filter(|device| device.enabled)
                .collect();
            return match controllable.as_slice() {
                [device] => Ok(device),
                [] => Err(format!(
                    "телевизор в комнате «{}» есть, но не управляется ({})",
                    by_room[0].name(),
                    by_room[0].protocol
                )),
                _ => Err("в этой комнате несколько телевизоров".to_owned()),
            };
        }
        let matches: Vec<&TvDevice> = enabled
            .into_iter()
            .filter(|device| device.matches(&needle))
            .collect();
        match matches.as_slice() {
            [device] => Ok(device),
            [] => Err(format!("не нашла телевизор «{query}»")),
            _ => Err(format!("«{query}» подходит к нескольким телевизорам")),
        }
    }
}

impl TvDevice {
    pub fn name(&self) -> &str {
        self.room.as_deref().unwrap_or(&self.id)
    }

    pub fn is_remote_v2(&self) -> bool {
        self.protocol == "remote_v2"
    }

    /// Match a spoken reference: exact id/room/alias or a Russian case form.
    fn matches(&self, query: &str) -> bool {
        if self.id.to_lowercase() == query {
            return true;
        }
        if self
            .room
            .as_deref()
            .is_some_and(|room| phrase_matches(query, room))
        {
            return true;
        }
        self.aliases
            .iter()
            .any(|alias| phrase_matches(query, alias))
    }
}

/// Russian noun/adjective endings, longest first; stems shorter than 4
/// characters are left alone to avoid silly matches.
const ENDINGS: &[&str] = &[
    "ыми", "ими", "ого", "его", "ому", "ему", "ая", "яя", "ое", "ее", "ые", "ие", "ый", "ий", "ых",
    "их", "ой", "ей", "ою", "ею", "ую", "юю", "ом", "ем", "ам", "ям", "ах", "ях", "а", "я", "у",
    "ю", "е", "и", "ы", "о", "ь",
];

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
/// «телевизор в гостиной» matches «телевизор гостиная».
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

fn phrase_matches(query: &str, candidate: &str) -> bool {
    let query = stems(query);
    let candidate = stems(candidate);
    !candidate.is_empty()
        && candidate.len() <= query.len()
        && query
            .windows(candidate.len())
            .any(|window| window == candidate.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TvConfig {
        TvConfig {
            devices: vec![
                TvDevice {
                    id: "tcl".to_owned(),
                    room: Some("гостиная".to_owned()),
                    aliases: vec!["телевизор".to_owned(), "телик".to_owned()],
                    host: "192.168.250.109".to_owned(),
                    mac: None,
                    cert: None,
                    key: None,
                    protocol: "remote_v2".to_owned(),
                    enabled: true,
                    api_port: 6466,
                    broadcast: None,
                },
                TvDevice {
                    id: "sber".to_owned(),
                    room: Some("детская".to_owned()),
                    aliases: vec!["сбер".to_owned(), "телевизор в детской".to_owned()],
                    host: "192.168.250.111".to_owned(),
                    mac: None,
                    cert: None,
                    key: None,
                    protocol: "cast".to_owned(),
                    enabled: false,
                    api_port: 6466,
                    broadcast: None,
                },
            ],
        }
    }

    #[test]
    fn resolves_by_id_room_and_alias() {
        let config = config();
        assert_eq!(config.resolve(Some("tcl")).unwrap().id, "tcl");
        assert_eq!(config.resolve(Some("Гостиная")).unwrap().id, "tcl");
        assert_eq!(config.resolve(Some("телик")).unwrap().id, "tcl");
    }

    #[test]
    fn defaults_to_the_only_enabled_device() {
        assert_eq!(config().resolve(None).unwrap().id, "tcl");
    }

    #[test]
    fn rejects_unknown_and_ambiguous_references() {
        let config = config();
        assert!(config.resolve(Some("кухня")).is_err());
        let mut config = config;
        config.devices[1].enabled = true;
        assert!(config.resolve(None).is_err());
    }

    #[test]
    fn matches_russian_case_forms_and_room_phrases() {
        let config = config();
        assert_eq!(config.resolve(Some("в гостиной")).unwrap().id, "tcl");
        assert_eq!(
            config.resolve(Some("телевизор в гостиной")).unwrap().id,
            "tcl"
        );
        assert_eq!(config.resolve(Some("телик в гостиной")).unwrap().id, "tcl");
    }

    #[test]
    fn a_room_does_not_fall_back_to_another_tv() {
        let config = config();
        assert!(config.resolve(Some("включи телевизор в детской")).is_err());
    }
}

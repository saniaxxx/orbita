//! Deterministic fast paths for the most common device commands.
//!
//! Volume and timers are answered without the LLM: they are unambiguous, and
//! the station must react immediately. Free-form phrasing falls through to the
//! model with the offered functions.

use std::sync::OnceLock;

use regex::Regex;

use crate::action::{Action, VolumeChange};

/// Parse a volume command: "громче", "сделай потише", "громкость 5".
pub fn volume(text: &str) -> Option<VolumeChange> {
    let text = normalize(text);
    if text.is_empty() || text.contains("напомни") {
        return None;
    }
    if let Some(level) = volume_level(&text) {
        return Some(VolumeChange::Set(level.clamp(1, 10)));
    }
    if text.contains("громче")
        || text.contains("погромче")
        || text.contains("прибавь громкость")
        || text.contains("увеличь громкость")
    {
        return Some(VolumeChange::Louder);
    }
    if text.contains("тише")
        || text.contains("потише")
        || text.contains("убавь громкость")
        || text.contains("уменьши громкость")
        || text.contains("сбавь громкость")
    {
        return Some(VolumeChange::Quieter);
    }
    None
}

fn volume_level(text: &str) -> Option<u8> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let regex = RE.get_or_init(|| {
        Regex::new(
            r"(?:громкость|звук)\s*(?:на\s*)?(\d{1,3})|(\d{1,3})\s*(?:деления|делений|уровн)",
        )
        .expect("valid regex")
    });
    let captures = regex.captures(text)?;
    let value = captures
        .get(1)
        .or_else(|| captures.get(2))?
        .as_str()
        .parse()
        .ok()?;
    Some(value)
}

/// Parse a native timer: "поставь таймер на 5 минут", "таймер 30 секунд",
/// "таймер на час".
pub fn timer(text: &str) -> Option<Action> {
    let text = normalize(text);
    if text.is_empty() || !text.contains("таймер") || text.contains("напомни") {
        return None;
    }
    if text.contains("отмен") || text.contains("стоп") || text.contains("останов") {
        return None;
    }
    let seconds = timer_seconds(&text)?;
    if seconds == 0 {
        return None;
    }
    let label = timer_label(&text);
    Some(Action::SetTimer { seconds, label })
}

fn timer_seconds(text: &str) -> Option<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let regex = RE.get_or_init(|| {
        Regex::new(
            r"(\d{1,3})\s*(секунд\w*|сек|минут\w*|мин|час\w*|ч)(?:\s+(\d{1,3})\s*(секунд\w*|сек))?",
        )
        .expect("valid regex")
    });
    if let Some(captures) = regex.captures(text) {
        let amount: u64 = captures.get(1)?.as_str().parse().ok()?;
        let unit = captures.get(2)?.as_str();
        let seconds = amount * unit_seconds(unit);
        // "5 минут 30 секунд"
        if let (Some(extra), Some(extra_unit)) = (captures.get(3), captures.get(4)) {
            let extra: u64 = extra.as_str().parse().ok()?;
            return Some(seconds + extra * unit_seconds(extra_unit.as_str()));
        }
        return Some(seconds);
    }
    if text.contains("полчаса") {
        return Some(1800);
    }
    if text.contains("на час") || text.contains("на один час") {
        return Some(3600);
    }
    words_timer_seconds(text)
}

/// Number word in the 1..=100 range, added to the running value.
pub(crate) fn number_value(token: &str) -> Option<u64> {
    Some(match token {
        "один" | "одну" | "одна" => 1,
        "два" | "две" => 2,
        "три" => 3,
        "четыре" => 4,
        "пять" => 5,
        "шесть" => 6,
        "семь" => 7,
        "восемь" => 8,
        "девять" => 9,
        "десять" => 10,
        "одиннадцать" => 11,
        "двенадцать" => 12,
        "тринадцать" => 13,
        "четырнадцать" => 14,
        "пятнадцать" => 15,
        "шестнадцать" => 16,
        "семнадцать" => 17,
        "восемнадцать" => 18,
        "девятнадцать" => 19,
        "двадцать" => 20,
        "тридцать" => 30,
        "сорок" => 40,
        "пятьдесят" => 50,
        "шестьдесят" => 60,
        "семьдесят" => 70,
        "восемьдесят" => 80,
        "девяносто" => 90,
        "сто" => 100,
        _ => return None,
    })
}

/// "двадцать две минуты" → (22, minutes), after a failed digit match.
fn words_timer_seconds(text: &str) -> Option<u64> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    // Skip the command words before the number: "поставь таймер на две минуты".
    let mut index = tokens
        .iter()
        .position(|token| number_value(token).is_some())?;
    let mut number = 0u64;
    while index < tokens.len() {
        match number_value(tokens[index]) {
            Some(value) => {
                number += value;
                index += 1;
            }
            None => break,
        }
    }
    let unit = tokens[index..].iter().find(|token| {
        token.starts_with("сек") || token.starts_with("мин") || token.starts_with("час")
    })?;
    Some(number * unit_seconds(unit))
}

fn unit_seconds(unit: &str) -> u64 {
    if unit.starts_with("сек") {
        1
    } else if unit.starts_with("мин") {
        60
    } else {
        3600
    }
}

fn timer_label(text: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let regex = RE.get_or_init(|| {
        Regex::new(r"(?:под\s+названием|называется|для)\s+(.+)$").expect("valid regex")
    });
    let label = regex.captures(text)?.get(1)?.as_str().trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_owned())
    }
}

/// Fast path for any supported action, in priority order.
pub fn action(text: &str) -> Option<Action> {
    if let Some(volume) = volume(text) {
        return Some(Action::SetVolume(volume));
    }
    timer(text)
}

fn normalize(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .replace('ё', "е")
        .trim_end_matches(['.', '!', '?', ','])
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_volume_commands() {
        assert_eq!(volume("громче"), Some(VolumeChange::Louder));
        assert_eq!(volume("Сделай погромче!"), Some(VolumeChange::Louder));
        assert_eq!(volume("потише"), Some(VolumeChange::Quieter));
        assert_eq!(volume("убавь громкость"), Some(VolumeChange::Quieter));
        assert_eq!(volume("громкость 5"), Some(VolumeChange::Set(5)));
        assert_eq!(
            volume("установи громкость на 7"),
            Some(VolumeChange::Set(7))
        );
        assert_eq!(volume("громкость 50"), Some(VolumeChange::Set(10)));
        assert_eq!(volume("напомни через 5 минут громче"), None);
        assert_eq!(volume("привет"), None);
    }

    #[test]
    fn reads_timers() {
        assert_eq!(
            timer("поставь таймер на 5 минут"),
            Some(Action::SetTimer {
                seconds: 300,
                label: None
            })
        );
        assert_eq!(
            timer("таймер 30 секунд"),
            Some(Action::SetTimer {
                seconds: 30,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на час"),
            Some(Action::SetTimer {
                seconds: 3600,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на 2 минуты 30 секунд"),
            Some(Action::SetTimer {
                seconds: 150,
                label: None
            })
        );
        assert_eq!(
            timer("поставь таймер на две минуты"),
            Some(Action::SetTimer {
                seconds: 120,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на двадцать пять минут"),
            Some(Action::SetTimer {
                seconds: 1500,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на полчаса"),
            Some(Action::SetTimer {
                seconds: 1800,
                label: None
            })
        );
        assert_eq!(timer("отмени таймер"), None);
        assert_eq!(timer("напомни через 5 минут"), None);
        assert_eq!(timer("привет"), None);
    }

    #[test]
    fn action_prefers_volume() {
        assert_eq!(
            action("громкость 3"),
            Some(Action::SetVolume(VolumeChange::Set(3)))
        );
        assert_eq!(
            action("таймер на 10 минут"),
            Some(Action::SetTimer {
                seconds: 600,
                label: None
            })
        );
        assert_eq!(action("расскажи сказку"), None);
    }
}

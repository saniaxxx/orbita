//! Deterministic reminder fast path.
//!
//! Voice reminders are frequent and time-sensitive, so they do not go through
//! the LLM: the agent parses "напомни через N минут" and "напомни в HH:MM"
//! itself. The LLM remains the fallback for everything else.

use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};
use regex::Regex;
use std::sync::OnceLock;

use crate::fastpath::number_value;

const MOSCOW_OFFSET_SECONDS: i32 = 3 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub text: String,
    pub due_at: DateTime<Utc>,
    pub reply: String,
}

pub fn parse(input: &str, now: DateTime<Utc>) -> Option<Reminder> {
    if let Some(captures) = relative_regex().captures(input) {
        let amount: i64 = captures.get(1)?.as_str().parse().ok()?;
        let unit = captures.get(2)?.as_str().to_lowercase();
        let text = clean_text(captures.get(3)?.as_str());
        let minutes = if unit.starts_with("час") {
            amount * 60
        } else {
            amount
        };
        let due_at = now + Duration::minutes(minutes);
        let reply = format!("Напомню через {}. {text}.", human_minutes(amount, &unit));
        return Some(Reminder {
            text,
            due_at,
            reply,
        });
    }

    if let Some(reminder) = parse_relative_words(input, now) {
        return Some(reminder);
    }

    let captures = absolute_regex().captures(input)?;
    let tomorrow = captures.get(1).is_some();
    let hour: u32 = captures.get(2)?.as_str().parse().ok()?;
    let minute: u32 = captures.get(3)?.as_str().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let text = clean_text(captures.get(4)?.as_str());
    let offset = FixedOffset::east_opt(MOSCOW_OFFSET_SECONDS)?;
    let now_moscow = now.with_timezone(&offset);
    let date = if tomorrow {
        now_moscow.date_naive() + chrono::Days::new(1)
    } else {
        now_moscow.date_naive()
    };
    let naive = date.and_hms_opt(hour, minute, 0)?;
    let candidate = offset.from_local_datetime(&naive).single()?;
    let due = if candidate <= now_moscow {
        candidate + chrono::Days::new(1)
    } else {
        candidate
    };
    let due_at = due.with_timezone(&Utc);
    let reply = format!("Напомню в {hour:02}:{minute:02}. {text}.");
    Some(Reminder {
        text,
        due_at,
        reply,
    })
}

fn clean_text(text: &str) -> String {
    let text = text.trim().trim_matches(|character: char| {
        matches!(character, ',' | '.' | '!' | '?' | ':' | ';' | ' ')
    });
    if text.is_empty() {
        "напоминание".to_owned()
    } else {
        text.to_owned()
    }
}

fn human_minutes(amount: i64, unit: &str) -> String {
    crate::russian::amount_phrase(amount, unit)
}

/// "напомни через одну минуту", "через двадцать пять минут", "через полчаса".
fn parse_relative_words(input: &str, now: DateTime<Utc>) -> Option<Reminder> {
    // "через полчаса" has no separate number word.
    if let Some(captures) = half_hour_regex().captures(input) {
        let text = clean_text(captures.get(1)?.as_str());
        return Some(Reminder {
            text: text.clone(),
            due_at: now + Duration::minutes(30),
            reply: format!("Напомню через 30 минут. {text}."),
        });
    }
    let captures = relative_words_regex().captures(input)?;
    let words = captures.get(1)?.as_str().to_lowercase();
    let unit = captures.get(2)?.as_str().to_lowercase();
    let text = clean_text(captures.get(3)?.as_str());
    let mut total = 0u64;
    for word in words.split_whitespace() {
        total += number_value(word)?;
    }
    let amount = total as i64;
    let minutes = if unit.starts_with("час") {
        amount * 60
    } else {
        amount
    };
    let due_at = now + Duration::minutes(minutes);
    let reply = format!("Напомню через {}. {text}.", human_minutes(amount, &unit));
    Some(Reminder {
        text,
        due_at,
        reply,
    })
}

fn half_hour_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?iu)^\s*(?:алиса[,.!\s]+)?напомни(?:\s+мне)?\s+через\s+полчаса\s*[,:]?\s*(.*)$",
        )
        .expect("half-hour reminder regex")
    })
}

fn relative_words_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?iu)^\s*(?:алиса[,.!\s]+)?напомни(?:\s+мне)?\s+через\s+((?:[а-яё]+\s*){1,2})\s*(минут(?:у|ы)?|час(?:а|ов)?|полчаса)\s*[,:]?\s*(.*)$",
        )
        .expect("relative word reminder regex")
    })
}

fn relative_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?iu)^\s*(?:алиса[,.!\s]+)?напомни(?:\s+мне)?\s+через\s+(\d+)\s*(минут(?:у|ы)?|час(?:а|ов)?)\s*[,:]?\s*(.*)$",
        )
        .expect("relative reminder regex")
    })
}

fn absolute_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?iu)^\s*(?:алиса[,.!\s]+)?напомни(?:\s+мне)?\s+(завтра\s+)?в\s+(\d{1,2}):(\d{2})\s*[,:]?\s*(.*)$",
        )
        .expect("absolute reminder regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-03T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_relative_reminder() {
        let reminder = parse("Алиса, напомни через 20 минут выключить духовку", now()).unwrap();
        assert_eq!(reminder.text, "выключить духовку");
        assert_eq!(reminder.due_at, now() + Duration::minutes(20));
        assert_eq!(reminder.reply, "Напомню через 20 минут. выключить духовку.");
    }

    #[test]
    fn parses_word_numbers() {
        for (phrase, minutes) in [
            ("напомни через одну минуту проверить чай", 1),
            ("напомни через две минуты позвонить", 2),
            ("напомни через двадцать пять минут выключить", 25),
            ("напомни через полчаса проверить духовку", 30),
        ] {
            let reminder = parse(phrase, now()).unwrap_or_else(|| panic!("{phrase}"));
            assert_eq!(
                reminder.due_at,
                now() + Duration::minutes(minutes),
                "{phrase}"
            );
        }
        assert_eq!(
            parse("напомни через одну минуту проверить чай", now())
                .unwrap()
                .reply,
            "Напомню через 1 минуту. проверить чай."
        );
    }

    #[test]
    fn parses_hours_and_plural() {
        let reminder = parse("напомни через 1 час позвонить", now()).unwrap();
        assert_eq!(reminder.reply, "Напомню через 1 час. позвонить.");
        assert_eq!(reminder.due_at, now() + Duration::hours(1));
    }

    #[test]
    fn parses_absolute_time_today() {
        let reminder = parse("напомни мне в 18:30 позвонить маме", now()).unwrap();
        assert_eq!(reminder.text, "позвонить маме");
        assert_eq!(
            reminder.due_at,
            Utc.with_ymd_and_hms(2026, 11, 3, 15, 30, 0).unwrap()
        );
        assert_eq!(reminder.reply, "Напомню в 18:30. позвонить маме.");
    }

    #[test]
    fn past_time_moves_to_tomorrow() {
        let reminder = parse("напомни в 09:00 зарядка", now()).unwrap();
        assert_eq!(
            reminder.due_at,
            Utc.with_ymd_and_hms(2026, 11, 4, 6, 0, 0).unwrap()
        );
    }

    #[test]
    fn explicit_tomorrow() {
        let reminder = parse("напомни завтра в 08:15 вода", now()).unwrap();
        assert_eq!(
            reminder.due_at,
            Utc.with_ymd_and_hms(2026, 11, 4, 5, 15, 0).unwrap()
        );
    }

    #[test]
    fn non_reminder_is_none() {
        assert!(parse("какая погода", now()).is_none());
    }
}

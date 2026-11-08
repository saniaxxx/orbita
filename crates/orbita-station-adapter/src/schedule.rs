//! Time commitments (D-26): reminders, alarms and timers belong to the core
//! scheduler, not to the station.
//!
//! The adapter only *creates* jobs in the OpenClaw scheduler (`openclaw cron
//! add`, a deterministic command payload, no LLM at fire time) and *receives*
//! station deliveries on `/outbound`. Delivery goes where the user asked:
//! back to this station (webhook → quasar push) or to Telegram (`announce`).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Push to a station through quasar.
    Station { device: String },
    /// Announce to a Telegram chat through the OpenClaw channel.
    Telegram { chat_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commitment {
    pub due_at: DateTime<Utc>,
    /// What the user hears or reads when it fires.
    pub text: String,
    pub delivery: Delivery,
}

#[async_trait]
pub trait Scheduler: Send + Sync {
    /// Store the commitment in the core scheduler; returns the job id.
    async fn schedule(&self, commitment: &Commitment) -> Result<String>;
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn push(&self, device: &str, text: &str) -> Result<()>;
}

/// Where the user wants the commitment delivered, from the utterance.
/// Returns the utterance without the channel words, so parsers see only the
/// time and the subject.
pub fn split_channel(text: &str) -> (String, bool) {
    const PHRASES: &[&str] = &[
        "в телеграмм",
        "в телеграм",
        "в телегу",
        "в telegram",
        "через телеграмм",
        "через телеграм",
        "телеграммом",
        "телеграмом",
    ];
    let lower = text.to_lowercase();
    for phrase in PHRASES {
        if let Some(start) = lower.find(phrase) {
            // Same byte offsets: lowercase Cyrillic/ASCII keeps lengths.
            if lower.len() == text.len() {
                let rest = format!("{} {}", &text[..start], &text[start + phrase.len()..]);
                return (rest.split_whitespace().collect::<Vec<_>>().join(" "), true);
            }
        }
    }
    (text.to_owned(), false)
}

/// Pick the delivery: Telegram when asked and configured, otherwise this
/// station. The note is prepended to the spoken reply when Telegram was asked
/// but is not connected.
pub fn choose_delivery(
    wants_telegram: bool,
    telegram_chat: Option<&str>,
    device: &str,
) -> (Delivery, Option<&'static str>) {
    match (wants_telegram, telegram_chat) {
        (true, Some(chat_id)) => (
            Delivery::Telegram {
                chat_id: chat_id.to_owned(),
            },
            None,
        ),
        (true, None) => (
            Delivery::Station {
                device: device.to_owned(),
            },
            Some("Телеграм пока не подключён, напомню здесь."),
        ),
        (false, _) => (
            Delivery::Station {
                device: device.to_owned(),
            },
            None,
        ),
    }
}

/// "HH:MM" Moscow wall time → the next such moment in UTC (today, or tomorrow
/// when it has passed). Russia has no DST, so the offset is fixed.
pub fn due_at_local(at: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use chrono::{FixedOffset, TimeZone};
    let (hour, minute) = at.trim().split_once(':')?;
    let (hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
    if hour > 23 || minute > 59 {
        return None;
    }
    let offset = FixedOffset::east_opt(3 * 3600)?;
    let local_now = now.with_timezone(&offset);
    let candidate = offset
        .from_local_datetime(&local_now.date_naive().and_hms_opt(hour, minute, 0)?)
        .single()?;
    let due = if candidate <= local_now {
        candidate + chrono::Days::new(1)
    } else {
        candidate
    };
    Some(due.with_timezone(&Utc))
}

/// `openclaw cron add` as the scheduler client.
pub struct OpenClawCliScheduler {
    pub bin: String,
    /// Adapter base the Gateway can reach, e.g. `http://127.0.0.1:8081`.
    pub outbound_base: String,
    pub outbound_key: String,
    pub timeout: Duration,
}

impl OpenClawCliScheduler {
    pub fn arguments(&self, commitment: &Commitment) -> Vec<String> {
        let at = commitment.due_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut arguments = vec![
            "cron".to_owned(),
            "add".to_owned(),
            "--name".to_owned(),
            format!("orbita: {}", truncate(&commitment.text, 60)),
            "--at".to_owned(),
            at,
            "--command-argv".to_owned(),
            json!(["printf", "%s", commitment.text]).to_string(),
            "--delete-after-run".to_owned(),
        ];
        match &commitment.delivery {
            Delivery::Station { device } => {
                arguments.push("--webhook".to_owned());
                arguments.push(format!(
                    "{}/outbound?device={}&key={}",
                    self.outbound_base.trim_end_matches('/'),
                    url_component(device),
                    url_component(&self.outbound_key)
                ));
            }
            Delivery::Telegram { chat_id } => {
                arguments.extend([
                    "--announce".to_owned(),
                    "--channel".to_owned(),
                    "telegram".to_owned(),
                    "--to".to_owned(),
                    chat_id.clone(),
                ]);
            }
        }
        arguments
    }
}

#[async_trait]
impl Scheduler for OpenClawCliScheduler {
    async fn schedule(&self, commitment: &Commitment) -> Result<String> {
        let output = tokio::time::timeout(
            self.timeout,
            tokio::process::Command::new(&self.bin)
                .args(self.arguments(commitment))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("openclaw cron add timed out")?
        .with_context(|| format!("running {}", self.bin))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            bail!(
                "openclaw cron add failed ({}): {}",
                output.status,
                truncate(
                    &format!("{stdout} {}", String::from_utf8_lossy(&output.stderr)),
                    300
                )
            );
        }
        Ok(job_id(&stdout).unwrap_or_else(|| "unknown".to_owned()))
    }
}

/// The job id from `openclaw cron add` JSON output (the CLI may print a
/// banner line first).
fn job_id(stdout: &str) -> Option<String> {
    let start = stdout.find('{')?;
    let value: Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    value
        .get("id")
        .or_else(|| value.pointer("/job/id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// quasar `POST /devices/{duid}/push/text` (the legacy agent's channel).
pub struct QuasarNotifier {
    client: reqwest::Client,
    base_url: String,
}

impl QuasarNotifier {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("building the quasar client")?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }
}

#[async_trait]
impl Notifier for QuasarNotifier {
    async fn push(&self, device: &str, text: &str) -> Result<()> {
        let url = format!(
            "{}/devices/{}/push/text",
            self.base_url.trim_end_matches('/'),
            url_component(device)
        );
        let response = self
            .client
            .post(&url)
            .json(&json!({"eventText": text}))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !response.status().is_success() {
            bail!("quasar push returned {}", response.status());
        }
        Ok(())
    }
}

fn url_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_words_are_split_off() {
        assert_eq!(
            split_channel("напомни через час в телеграм купить хлеб"),
            ("напомни через час купить хлеб".to_owned(), true)
        );
        assert_eq!(
            split_channel("Напомни в телеграмм через 20 минут позвонить"),
            ("Напомни через 20 минут позвонить".to_owned(), true)
        );
        assert_eq!(
            split_channel("напомни через час купить хлеб"),
            ("напомни через час купить хлеб".to_owned(), false)
        );
    }

    #[test]
    fn telegram_falls_back_to_the_station_when_not_connected() {
        let (delivery, note) = choose_delivery(true, None, "duid-1");
        assert_eq!(
            delivery,
            Delivery::Station {
                device: "duid-1".into()
            }
        );
        assert!(note.is_some());
        let (delivery, note) = choose_delivery(true, Some("42"), "duid-1");
        assert_eq!(
            delivery,
            Delivery::Telegram {
                chat_id: "42".into()
            }
        );
        assert!(note.is_none());
    }

    #[test]
    fn cli_arguments_carry_time_payload_and_delivery() {
        let scheduler = OpenClawCliScheduler {
            bin: "openclaw".into(),
            outbound_base: "http://127.0.0.1:8081/".into(),
            outbound_key: "k e/y".into(),
            timeout: Duration::from_secs(5),
        };
        let due_at = DateTime::parse_from_rfc3339("2026-11-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let station = scheduler.arguments(&Commitment {
            due_at,
            text: "Напоминание: чай".into(),
            delivery: Delivery::Station {
                device: "LG00".into(),
            },
        });
        assert!(
            station
                .windows(2)
                .any(|pair| pair == ["--at", "2026-11-08T10:00:00Z"])
        );
        assert!(station.windows(2).any(|pair| pair[0] == "--command-argv"
            && pair[1] == r#"["printf","%s","Напоминание: чай"]"#));
        assert!(station.windows(2).any(|pair| pair[0] == "--webhook"
            && pair[1] == "http://127.0.0.1:8081/outbound?device=LG00&key=k%20e%2Fy"));
        let telegram = scheduler.arguments(&Commitment {
            due_at,
            text: "x".into(),
            delivery: Delivery::Telegram {
                chat_id: "42".into(),
            },
        });
        assert!(
            telegram
                .windows(2)
                .any(|pair| pair == ["--channel", "telegram"])
        );
        assert!(telegram.windows(2).any(|pair| pair == ["--to", "42"]));
        assert!(!telegram.iter().any(|argument| argument == "--webhook"));
    }

    #[test]
    fn job_id_survives_a_banner_line() {
        assert_eq!(
            job_id("OpenClaw 2026.11.1\n{\"id\":\"abc\",\"name\":\"x\"}"),
            Some("abc".into())
        );
        assert_eq!(job_id("{\"job\":{\"id\":\"def\"}}"), Some("def".into()));
        assert_eq!(job_id("no json"), None);
    }
}

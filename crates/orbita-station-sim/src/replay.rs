//! Replay a scenario of text and voice inputs with delays.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::{SessionReport, run_session, run_voice_session};

#[derive(Debug, Default, Deserialize)]
pub struct Device {
    pub duid: Option<String>,
    pub platform: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Step {
    Text {
        text: String,
        #[serde(default)]
        delay_ms: u64,
    },
    Voice {
        file: String,
        #[serde(default)]
        delay_ms: u64,
    },
}

#[derive(Debug, Deserialize)]
pub struct ReplayFile {
    #[serde(default)]
    pub device: Device,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ReplayInput {
    File(ReplayFile),
    Steps(Vec<Step>),
}

#[derive(Debug, Serialize)]
pub struct StepReport {
    pub index: usize,
    pub kind: &'static str,
    pub input: String,
    pub report: SessionReport,
}

pub struct ReplayOptions<'a> {
    pub url: &'a str,
    pub duid: &'a str,
    pub platform: &'a str,
    pub audio_dir: Option<&'a Path>,
    pub timeout: Duration,
}

pub async fn run_replay(path: &Path, options: ReplayOptions<'_>) -> Result<Vec<StepReport>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading replay file {}", path.display()))?;
    let input: ReplayInput = serde_json::from_str(&raw)
        .with_context(|| format!("parsing replay file {}", path.display()))?;
    let (device, steps) = match input {
        ReplayInput::File(file) => (file.device, file.steps),
        ReplayInput::Steps(steps) => (Device::default(), steps),
    };

    let duid = device.duid.as_deref().unwrap_or(options.duid);
    let platform = device.platform.as_deref().unwrap_or(options.platform);
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut reports = Vec::with_capacity(steps.len());

    for (index, step) in steps.into_iter().enumerate() {
        let (delay_ms, kind, input_text) = match &step {
            Step::Text { text, delay_ms } => (*delay_ms, "text", text.clone()),
            Step::Voice { file, delay_ms } => (*delay_ms, "voice", file.clone()),
        };
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let report = match step {
            Step::Text { text, .. } => {
                run_session(
                    options.url,
                    duid,
                    platform,
                    &text,
                    options.audio_dir,
                    options.timeout,
                )
                .await?
            }
            Step::Voice { file, .. } => {
                let path = base_dir.join(&file);
                let audio = std::fs::read(&path)
                    .with_context(|| format!("reading voice file {}", path.display()))?;
                run_voice_session(
                    options.url,
                    duid,
                    platform,
                    &audio,
                    options.audio_dir,
                    options.timeout,
                )
                .await?
            }
        };

        reports.push(StepReport {
            index,
            kind,
            input: input_text,
            report,
        });
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_scenario_object() {
        let scenario: ReplayInput = serde_json::from_str(
            r#"{
                "device": {"duid": "duid-1", "platform": "yandexmicro"},
                "steps": [
                    {"type": "text", "text": "привет"},
                    {"type": "voice", "file": "phrase.ogg", "delay_ms": 500}
                ]
            }"#,
        )
        .unwrap();
        let ReplayInput::File(file) = scenario else {
            panic!("expected object");
        };
        assert_eq!(file.device.duid.as_deref(), Some("duid-1"));
        assert_eq!(file.steps.len(), 2);
    }

    #[test]
    fn parses_a_top_level_step_array() {
        let scenario: ReplayInput =
            serde_json::from_str(r#"[{"type": "text", "text": "привет"}]"#).unwrap();
        assert!(matches!(scenario, ReplayInput::Steps(steps) if steps.len() == 1));
    }
}

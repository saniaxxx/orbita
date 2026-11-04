//! Reading JSONL files produced by `tests/recorders/ws_capture.py`.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Line {
    Meta {
        #[serde(default)]
        started_at: Option<f64>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Frame {
        ts_ms: f64,
        dir: String,
        kind: String,
        data: String,
    },
}

#[derive(Debug, Clone)]
pub struct RecordedFrame {
    pub ts_ms: f64,
    pub direction: String,
    pub kind: String,
    pub data: String,
}

impl RecordedFrame {
    pub fn is_client(&self) -> bool {
        self.direction == "c"
    }

    pub fn text(&self) -> Option<&str> {
        (self.kind == "text").then_some(self.data.as_str())
    }

    pub fn binary(&self) -> Result<Vec<u8>> {
        STANDARD
            .decode(&self.data)
            .context("invalid base64 frame data")
    }
}

#[derive(Debug, Default)]
pub struct RecordedSession {
    pub started_at: Option<f64>,
    pub path: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub frames: Vec<RecordedFrame>,
}

pub fn load(path: &Path) -> Result<RecordedSession> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading recorded session {}", path.display()))?;
    let mut session = RecordedSession::default();

    for (number, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Line = serde_json::from_str(line)
            .with_context(|| format!("parsing {} line {}", path.display(), number + 1))?;
        match parsed {
            Line::Meta {
                started_at,
                path,
                headers,
            } => {
                session.started_at = started_at;
                session.path = path;
                session.headers = headers;
            }
            Line::Frame {
                ts_ms,
                dir,
                kind,
                data,
            } => session.frames.push(RecordedFrame {
                ts_ms,
                direction: dir,
                kind,
                data,
            }),
        }
    }

    Ok(session)
}

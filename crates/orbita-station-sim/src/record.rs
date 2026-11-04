//! Writing real uniproxy sessions to the JSONL format read by `frames`.
//!
//! The format is the same one the old Python recorder produced:
//! a `meta` line followed by `frame` lines with `dir` (`c`/`s`), `kind`
//! (`text`/`binary`), base64 payloads for binary frames and milliseconds.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};

pub struct Recorder {
    start: Instant,
    out: BufWriter<File>,
}

impl Recorder {
    pub fn create(path: &Path, url: &str, duid: &str, platform: &str) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let file = File::create(path)
            .with_context(|| format!("creating recorded session {}", path.display()))?;
        let mut out = BufWriter::new(file);
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or_default();
        let meta = serde_json::json!({
            "type": "meta",
            "started_at": started_at,
            "path": url,
            "headers": {
                "x-uprx-device-id": duid,
                "x-uprx-device-model": platform,
            },
        });
        writeln!(out, "{meta}")?;
        out.flush()?;
        Ok(Self {
            start: Instant::now(),
            out,
        })
    }

    pub fn binary(&mut self, direction: &str, bytes: &[u8]) -> Result<()> {
        self.frame(direction, "binary", &STANDARD.encode(bytes))
    }

    pub fn text(&mut self, direction: &str, text: &str) -> Result<()> {
        self.frame(direction, "text", text)
    }

    fn frame(&mut self, direction: &str, kind: &str, data: &str) -> Result<()> {
        let line = serde_json::json!({
            "type": "frame",
            "ts_ms": self.start.elapsed().as_millis() as u64,
            "dir": direction,
            "kind": kind,
            "data": data,
        });
        writeln!(self.out, "{line}")?;
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::load;

    #[test]
    fn recorder_writes_loadable_jsonl() {
        let path = std::env::temp_dir().join(format!(
            "orbita-session-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let mut recorder =
                Recorder::create(&path, "ws://example/uni.ws", "duid-1", "yandexmicro").unwrap();
            recorder.binary("c", &[0, 1, 2, 3]).unwrap();
            recorder.text("s", "привет").unwrap();
        }

        let session = load(&path).unwrap();
        assert_eq!(session.path.as_deref(), Some("ws://example/uni.ws"));
        assert_eq!(
            session.headers.get("x-uprx-device-id").map(String::as_str),
            Some("duid-1")
        );
        assert_eq!(session.frames.len(), 2);
        assert_eq!(session.frames[0].direction, "c");
        assert_eq!(session.frames[0].binary().unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(session.frames[1].text(), Some("привет"));

        std::fs::remove_file(path).unwrap();
    }
}

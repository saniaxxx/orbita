//! Replay a recorded JSONL session: send its client frames with timings and
//! collect the server responses from a fresh connection.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio_tungstenite::tungstenite::Message;

use orbita_proto::n_alice::n_alice_api::{
    t_server_message::Message as ServerMessage, t_server_message::t_event::Event as ServerEventKind,
};

use crate::{
    frames::load,
    protocol::{audio_stream_id, decode_server, is_aapi, response_text},
    session::connect,
};

#[derive(Debug, Default, Serialize)]
pub struct FrameReplayReport {
    pub sent: usize,
    pub received: usize,
    pub text: Option<String>,
    pub asr_text: Option<String>,
    pub audio_chunks: usize,
    pub audio_bytes: usize,
}

pub async fn run_frame_replay(
    url: &str,
    duid: &str,
    platform: &str,
    session_path: &Path,
    frames_out: Option<&Path>,
    timeout: Duration,
) -> Result<FrameReplayReport> {
    let session = load(session_path)?;
    let mut socket = connect(url, duid, platform).await?;
    let mut report = FrameReplayReport::default();
    let started = Instant::now();
    let base_ts = session
        .frames
        .iter()
        .find(|frame| frame.is_client())
        .map(|frame| frame.ts_ms)
        .unwrap_or(0.0);

    let mut output = match frames_out {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("opening {}", path.display()))?,
        ),
        None => None,
    };

    for frame in session.frames.iter().filter(|frame| frame.is_client()) {
        let target = Duration::from_secs_f64((frame.ts_ms - base_ts).max(0.0) / 1000.0);
        let elapsed = started.elapsed();
        if target > elapsed {
            tokio::time::sleep(target - elapsed).await;
        }

        let message = if let Some(text) = frame.text() {
            Message::Text(text.to_owned().into())
        } else {
            Message::Binary(frame.binary()?.into())
        };
        socket
            .send(message)
            .await
            .context("sending a recorded client frame")?;
        report.sent += 1;
    }

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let message = match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) | Ok(None) => break,
            Ok(Some(Err(error))) => return Err(error).context("receiving a replayed frame"),
            Ok(Some(Ok(message))) => message,
        };

        match message {
            Message::Binary(bytes) => {
                report.received += 1;
                if is_aapi(&bytes) {
                    let server = decode_server(&bytes)
                        .map_err(anyhow::Error::msg)
                        .context("decoding a server frame")?;
                    if let Some(ServerMessage::Event(event)) = server.message
                        && let Some(event_kind) = event.event
                    {
                        match event_kind {
                            ServerEventKind::AliceResponse(response) => {
                                if let Some(text) = response_text(&response) {
                                    println!("ALICE: {text}");
                                    report.text = Some(text);
                                }
                            }
                            ServerEventKind::AsrResult(asr) => {
                                if let Some(text) = asr
                                    .recognition
                                    .first()
                                    .and_then(|recognition| recognition.normalized.clone())
                                {
                                    println!("ASR TEXT: {text}");
                                    report.asr_text = Some(text);
                                }
                            }
                            _ => {}
                        }
                    }
                } else if audio_stream_id(&bytes).is_some() {
                    report.audio_chunks += 1;
                    report.audio_bytes += bytes.len().saturating_sub(4);
                }

                if let Some(output) = output.as_mut() {
                    write_frame(
                        output,
                        started.elapsed().as_millis() as u64,
                        "s",
                        "binary",
                        &STANDARD.encode(&bytes),
                    )?;
                }
            }
            Message::Text(text) => {
                report.received += 1;
                if let Some(output) = output.as_mut() {
                    write_frame(
                        output,
                        started.elapsed().as_millis() as u64,
                        "s",
                        "text",
                        text.as_str(),
                    )?;
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }

    Ok(report)
}

fn write_frame(
    output: &mut std::fs::File,
    ts_ms: u64,
    direction: &str,
    kind: &str,
    data: &str,
) -> Result<()> {
    use std::io::Write as _;
    let line = serde_json::json!({
        "type": "frame",
        "ts_ms": ts_ms,
        "dir": direction,
        "kind": kind,
        "data": data,
    });
    writeln!(output, "{line}")?;
    output.flush()?;
    Ok(())
}

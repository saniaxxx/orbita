//! One request over AAPI: text input or an Opus/Ogg voice utterance.

use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
    },
};
use uuid::Uuid;

use orbita_proto::n_alice::n_alice_api::{
    t_server_message::Message as ServerMessage,
    t_server_message::t_event::Event as ServerEventKind, t_stream_control::Action as StreamAction,
};

use crate::{
    protocol::{
        audio_frame, audio_stream_id, decode_server, encode_client, is_aapi, response_text,
        stream_close, text_input, voice_input,
    },
    record::Recorder,
};

#[derive(Debug, Default, Serialize)]
pub struct SessionReport {
    pub text: Option<String>,
    pub asr_text: Option<String>,
    pub audio_chunks: usize,
    pub audio_bytes: usize,
    pub audio_files: Vec<String>,
    /// Directive names delivered by AliceResponse or Push.
    pub directives: Vec<String>,
    /// Number of Push events received on this connection.
    pub pushes: usize,
    /// Time from sending the input to the first AliceResponse text.
    pub response_ms: Option<u64>,
    /// Time from sending the input to the first audio frame.
    pub first_audio_ms: Option<u64>,
    pub total_ms: u64,
}

pub async fn run_session(
    url: &str,
    duid: &str,
    platform: &str,
    text: &str,
    audio_dir: Option<&Path>,
    timeout: Duration,
) -> Result<SessionReport> {
    run_recorded_session(url, duid, platform, text, audio_dir, timeout, None).await
}

pub async fn run_recorded_session(
    url: &str,
    duid: &str,
    platform: &str,
    text: &str,
    audio_dir: Option<&Path>,
    timeout: Duration,
    record_path: Option<&Path>,
) -> Result<SessionReport> {
    let mut socket = connect(url, duid, platform).await?;
    let mut recorder = record_path
        .map(|path| Recorder::create(path, url, duid, platform))
        .transpose()?;
    let dialog_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    send(
        &mut socket,
        &text_input(text, &dialog_id),
        recorder.as_mut(),
    )
    .await?;
    receive_loop(socket, started, audio_dir, timeout, recorder.as_mut()).await
}

pub async fn run_voice_session(
    url: &str,
    duid: &str,
    platform: &str,
    audio: &[u8],
    audio_dir: Option<&Path>,
    timeout: Duration,
) -> Result<SessionReport> {
    run_recorded_voice_session(url, duid, platform, audio, audio_dir, timeout, None).await
}

pub async fn run_recorded_voice_session(
    url: &str,
    duid: &str,
    platform: &str,
    audio: &[u8],
    audio_dir: Option<&Path>,
    timeout: Duration,
    record_path: Option<&Path>,
) -> Result<SessionReport> {
    let mut socket = connect(url, duid, platform).await?;
    let mut recorder = record_path
        .map(|path| Recorder::create(path, url, duid, platform))
        .transpose()?;
    let dialog_id = Uuid::new_v4().to_string();
    let stream_id = 1_i64;
    let started = Instant::now();

    send(
        &mut socket,
        &voice_input(&dialog_id, stream_id),
        recorder.as_mut(),
    )
    .await?;
    // quasar's OpusProcessor parses one complete Ogg page per binary frame.
    // Sending arbitrary 4096-byte slices splits pages and crashes the parser.
    let mut pages = ogg_pages(audio);
    if pages.is_empty() {
        pages = audio.chunks(4096).collect();
    }
    for chunk in pages {
        let frame = audio_frame(stream_id as u32, chunk);
        if let Some(recorder) = recorder.as_mut() {
            recorder.binary("c", &frame)?;
        }
        socket
            .send(Message::Binary(frame.into()))
            .await
            .context("sending voice audio")?;
    }
    // Give VAD/ASR a moment to emit the final transcript before closing the
    // stream; the real station speaks into silence, while the simulator sends
    // the whole file at once.
    let drain_ms = std::env::var("VOICE_DRAIN_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1500);
    if drain_ms > 0 {
        tokio::time::sleep(Duration::from_millis(drain_ms)).await;
    }
    send(&mut socket, &stream_close(stream_id), recorder.as_mut()).await?;
    receive_loop(socket, started, audio_dir, timeout, recorder.as_mut()).await
}

pub(crate) async fn connect(
    url: &str,
    duid: &str,
    platform: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let mut request = url
        .into_client_request()
        .with_context(|| format!("invalid WebSocket URL {url}"))?;
    request.headers_mut().insert(
        HeaderName::from_static("x-uprx-device-id"),
        HeaderValue::from_str(duid)?,
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-uprx-device-model"),
        HeaderValue::from_str(platform)?,
    );

    connect_async(request)
        .await
        .with_context(|| format!("connecting to {url}"))
        .map(|(socket, _)| socket)
}

/// Split an Ogg file into complete pages. The station sends Ogg pages, not
/// arbitrary byte slices, and quasar's parser relies on that.
fn ogg_pages(data: &[u8]) -> Vec<&[u8]> {
    let mut pages = Vec::new();
    let mut offset = 0;
    while offset + 27 <= data.len() && &data[offset..offset + 4] == b"OggS" {
        let segments = data[offset + 26] as usize;
        let table_end = offset + 27 + segments;
        if table_end > data.len() {
            break;
        }
        let body: usize = data[offset + 27..table_end]
            .iter()
            .map(|value| *value as usize)
            .sum();
        let end = table_end + body;
        if end > data.len() {
            break;
        }
        pages.push(&data[offset..end]);
        offset = end;
    }
    pages
}

async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    message: &orbita_proto::n_alice::n_alice_api::TClientMessage,
    recorder: Option<&mut Recorder>,
) -> Result<()> {
    let frame = encode_client(message);
    if let Some(recorder) = recorder {
        recorder.binary("c", &frame)?;
    }
    socket
        .send(Message::Binary(frame.into()))
        .await
        .context("sending AAPI frame")
}

async fn receive_loop(
    mut socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    started: Instant,
    audio_dir: Option<&Path>,
    timeout: Duration,
    mut recorder: Option<&mut Recorder>,
) -> Result<SessionReport> {
    let mut report = SessionReport::default();
    let mut audio: HashMap<u32, Vec<u8>> = HashMap::new();
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!("simulator timeout");
            break;
        }

        let next = tokio::time::timeout(remaining, socket.next()).await;
        let message = match next {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(Err(error))) => return Err(error).context("websocket receive failed"),
            Ok(Some(Ok(message))) => message,
        };

        if let Some(recorder) = recorder.as_deref_mut() {
            match &message {
                Message::Binary(bytes) => recorder.binary("s", bytes)?,
                Message::Text(text) => recorder.text("s", text.as_str())?,
                _ => {}
            }
        }

        match message {
            Message::Binary(bytes) => {
                if is_aapi(&bytes) {
                    let server = decode_server(&bytes)
                        .map_err(anyhow::Error::msg)
                        .context("decoding TServerMessage")?;
                    match server.message {
                        Some(ServerMessage::Event(event)) => {
                            let stream_id =
                                event.header.as_ref().and_then(|header| header.stream_id);
                            match event.event {
                                Some(ServerEventKind::AliceResponse(response)) => {
                                    if let Some(body) = &response.response {
                                        report.directives.extend(
                                            body.directives
                                                .iter()
                                                .map(|directive| directive.name.clone()),
                                        );
                                    }
                                    if let Some(text) = response_text(&response) {
                                        println!("ALICE: {text}");
                                        report.text = Some(text);
                                        report.response_ms.get_or_insert_with(|| {
                                            started.elapsed().as_millis() as u64
                                        });
                                    }
                                }
                                Some(ServerEventKind::TtsSpeak(tts)) => {
                                    println!(
                                        "TTS: stream={:?} format={} bytes=0",
                                        stream_id, tts.format
                                    );
                                }
                                Some(ServerEventKind::AsrResult(asr)) => {
                                    println!(
                                        "ASR: end_of_utt={:?} results={}",
                                        asr.end_of_utt,
                                        asr.recognition.len()
                                    );
                                    if let Some(recognition) = asr.recognition.first()
                                        && let Some(text) = &recognition.normalized
                                    {
                                        println!("ASR TEXT: {text}");
                                        report.asr_text = Some(text.clone());
                                    }
                                }
                                Some(ServerEventKind::Push(push)) => {
                                    report.pushes += 1;
                                    report.directives.extend(
                                        push.directives
                                            .iter()
                                            .map(|directive| directive.name.clone()),
                                    );
                                    println!("PUSH: directives={}", push.directives.len());
                                }
                                Some(ServerEventKind::GoAway(_)) => {
                                    println!("GOAWAY");
                                    break;
                                }
                                Some(_) => {}
                                None => {}
                            }
                        }
                        Some(ServerMessage::StreamControl(control)) => {
                            if matches!(control.action, Some(StreamAction::Close(_))) {
                                println!("STREAM CLOSE: {}", control.stream_id);
                                if report.text.is_some() {
                                    break;
                                }
                            }
                        }
                        None => {}
                    }
                } else if let Some(stream_id) = audio_stream_id(&bytes) {
                    let data = &bytes[4..];
                    report.audio_chunks += 1;
                    report.audio_bytes += data.len();
                    report
                        .first_audio_ms
                        .get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    audio.entry(stream_id).or_default().extend_from_slice(data);
                }
            }
            Message::Text(text) => {
                tracing::debug!(%text, "server text message");
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }

    report.total_ms = started.elapsed().as_millis() as u64;

    if let Some(directory) = audio_dir {
        std::fs::create_dir_all(directory)?;
        for (stream_id, data) in audio {
            let path = directory.join(format!("stream-{stream_id}.opus"));
            std::fs::write(&path, data)?;
            report.audio_files.push(path.display().to_string());
        }
    }

    Ok(report)
}

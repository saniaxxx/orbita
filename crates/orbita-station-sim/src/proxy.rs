//! Transparent WebSocket proxy that records a real station session.
//!
//! Replaces the deleted Python `ws_capture.py`: listen on a local port, forward
//! frames to quasar, and write both directions to the JSONL format read by
//! `frames`. Intended for a short manual capture, not for production traffic.

use std::path::PathBuf;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        handshake::server::{Request, Response},
        http::HeaderName,
    },
};

use crate::record::Recorder;

const DEVICE_HEADERS: [&str; 2] = ["x-uprx-device-id", "x-uprx-device-model"];

pub async fn run_proxy(listen: &str, upstream: &str, record: Option<PathBuf>) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding proxy listener {listen}"))?;
    tracing::info!(%listen, %upstream, "recording proxy listening");

    loop {
        let (stream, peer) = listener.accept().await.context("accepting a station")?;
        let upstream = upstream.to_owned();
        let record = record.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &upstream, record).await {
                tracing::warn!(%peer, %error, "recording proxy session failed");
            }
        });
    }
}

#[allow(clippy::result_large_err)] // tungstenite's handshake callback error type
async fn handle(stream: TcpStream, upstream: &str, record: Option<PathBuf>) -> Result<()> {
    let mut headers = None;
    let client = accept_hdr_async(stream, |request: &Request, response: Response| {
        headers = Some(request.headers().clone());
        Ok(response)
    })
    .await
    .context("accepting the station websocket")?;

    let headers = headers.unwrap_or_default();
    let duid = headers
        .get("x-uprx-device-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let platform = headers
        .get("x-uprx-device-model")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");

    let mut request = upstream
        .into_client_request()
        .with_context(|| format!("invalid upstream websocket URL {upstream}"))?;
    for name in DEVICE_HEADERS {
        if let Some(value) = headers.get(name) {
            request
                .headers_mut()
                .insert(HeaderName::from_static(name), value.clone());
        }
    }
    let (server, _) = connect_async(request)
        .await
        .with_context(|| format!("connecting to upstream {upstream}"))?;

    let mut recorder = record
        .as_deref()
        .map(|path| Recorder::create(path, upstream, duid, platform))
        .transpose()?;
    tracing::info!(%duid, %platform, "recording proxy session started");

    let mut client = client;
    let mut server = server;
    loop {
        tokio::select! {
            message = client.next() => {
                let Some(message) = message else { break };
                let message = message.context("receiving from the station")?;
                if let Some(recorder) = recorder.as_mut() {
                    log_message(recorder, "c", &message)?;
                }
                let close = matches!(message, Message::Close(_));
                server.send(message).await.context("forwarding to upstream")?;
                if close { break; }
            }
            message = server.next() => {
                let Some(message) = message else { break };
                let message = message.context("receiving from upstream")?;
                if let Some(recorder) = recorder.as_mut() {
                    log_message(recorder, "s", &message)?;
                }
                let close = matches!(message, Message::Close(_));
                client.send(message).await.context("forwarding to the station")?;
                if close { break; }
            }
        }
    }

    tracing::info!(%duid, %platform, "recording proxy session ended");
    Ok(())
}

fn log_message(recorder: &mut Recorder, direction: &str, message: &Message) -> Result<()> {
    match message {
        Message::Binary(bytes) => recorder.binary(direction, bytes),
        Message::Text(text) => recorder.text(direction, text.as_str()),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_) => Ok(()),
    }
}

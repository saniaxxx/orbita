//! Local Glagol WebSocket client.
//!
//! The station uses a self-signed certificate on its local port, so the spike
//! deliberately accepts any certificate. Keep this client on the trusted LAN.

use std::{
    net::IpAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::time::timeout;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config, tungstenite::Message,
};

use crate::protocol::{command, request_json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Ask our quasar for a Glagol token.
///
/// `quasar` is the HTTP base URL, for example `http://192.168.250.103:31120`.
/// The endpoint is the same one the real cloud exposes:
/// `/glagol/token?device_id=...&platform=...`.
pub async fn fetch_token(quasar: &str, device_id: &str, platform: &str) -> Result<String> {
    let url = format!("{}/glagol/token", quasar.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        // The stand proxy may present our own CA. The spike runs on loopback
        // or the trusted LAN, so certificate pinning is not its job.
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(5))
        .build()
        .context("building token client")?;
    let response = client
        .get(&url)
        .query(&[("device_id", device_id), ("platform", platform)])
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned an error"))?;
    let value: Value = response.json().await.context("parsing token response")?;
    if value.get("status").and_then(Value::as_str) != Some("ok") {
        bail!("glagol token endpoint returned {value}");
    }
    value
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("token response has no token")
}

pub struct GlagolClient {
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    token: String,
    request_timeout: Duration,
}

impl GlagolClient {
    pub async fn connect(host: IpAddr, port: u16, token: String) -> Result<Self> {
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .context("building TLS connector")?;
        let url = match host {
            IpAddr::V4(address) => format!("wss://{address}:{port}"),
            IpAddr::V6(address) => format!("wss://[{address}]:{port}"),
        };
        let (socket, _) = connect_async_tls_with_config(
            url.as_str(),
            None,
            false,
            Some(Connector::NativeTls(tls)),
        )
        .await
        .with_context(|| format!("connecting to {url}"))?;

        let mut client = Self {
            socket,
            token,
            request_timeout: REQUEST_TIMEOUT,
        };
        client.send_no_wait(command("softwareVersion")).await?;
        Ok(client)
    }

    pub fn with_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    /// Send a payload and wait for the response carrying the same `requestId`.
    /// Unsolicited state messages are skipped.
    pub async fn send(&mut self, payload: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_request(&id, payload).await?;

        let deadline = Instant::now() + self.request_timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("timed out waiting for response {id}");
            }
            let message = timeout(left, self.socket.next())
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for response {id}"))?;
            let Some(message) = message else {
                bail!("station closed the connection");
            };
            let message = message.context("reading station message")?;
            match message {
                Message::Text(text) => {
                    if let Some(response) = self.matches(&text, &id)? {
                        return Ok(response);
                    }
                }
                Message::Binary(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(response) = self.matches(&text, &id)? {
                        return Ok(response);
                    }
                }
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(frame) => {
                    bail!("station closed the connection: {frame:?}");
                }
                _ => {}
            }
        }
    }

    /// Send without waiting; the response is skipped by the next `send`.
    pub async fn send_no_wait(&mut self, payload: Value) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_request(&id, payload).await
    }

    /// Collect unsolicited messages for `duration`; useful for `state`.
    pub async fn watch(&mut self, duration: Duration) -> Result<Vec<Value>> {
        let deadline = Instant::now() + duration;
        let mut messages = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(messages);
            }
            match timeout(left, self.socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(value) = serde_json::from_str(&text) {
                        messages.push(value);
                    }
                }
                Ok(Some(Ok(Message::Binary(bytes)))) => {
                    if let Ok(value) = serde_json::from_slice(&bytes) {
                        messages.push(value);
                    }
                }
                Ok(Some(Ok(Message::Ping(payload)))) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) => return Ok(messages),
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => return Err(error).context("reading station message"),
                Err(_) => return Ok(messages),
            }
        }
    }

    pub async fn close(mut self) -> Result<()> {
        self.socket.close(None).await.context("closing WebSocket")?;
        Ok(())
    }

    async fn send_request(&mut self, id: &str, payload: Value) -> Result<()> {
        let sent_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let request = request_json(&self.token, id, payload, sent_time);
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .context("sending station request")
    }

    fn matches(&self, text: &str, id: &str) -> Result<Option<Value>> {
        let value: Value = serde_json::from_str(text).context("parsing station message")?;
        if value.get("requestId").and_then(Value::as_str) == Some(id) {
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }
}

//! Minimal uniproxy path: AAPI over WebSocket, text input, processor and TTS.
//!
//! This is the first vertical slice of the upstream `UniProxyConnection`:
//! it accepts a station WebSocket, decodes a `TextInput`, asks
//! `orbita-processor` for an answer and streams the synthesized Opus back as
//! `TtsSpeak` frames. Voice input, directives and sync will follow.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use orbita_proto::n_alice::{
    EEventType,
    n_alice_api::{
        TClientMessage, TEventHeader, TServerMessage, TStreamControl, TTtsSpeak,
        t_alice_response_proto::t_voice_response::TOutputSpeech,
        t_alice_response_proto::{THeader as ResponseHeader, TResponse, TVoiceResponse},
        t_client_message::{Message as ClientMessage, t_event::Event},
        t_server_message::{
            Message as ServerMessage, TEvent as ServerEvent, t_event::Event as ServerEventKind,
        },
        t_stream_control::{Action as StreamAction, TActionClose},
    },
};
use prost::Message as ProstMessage;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use crate::Config;

const AAPI: &[u8; 4] = b"AAPI";
const FIRST_AUDIO_STREAM: i64 = 1024;
const PROCESSOR_TIMEOUT: Duration = Duration::from_secs(25);
const TTS_TIMEOUT: Duration = Duration::from_secs(30);

pub fn router(config: Config) -> Router {
    Router::new()
        .route("/uniproxy.alice.yandex.net/uni.ws", get(ws_handler))
        .with_state(Arc::new(config))
}

async fn ws_handler(
    upgrade: WebSocketUpgrade,
    State(config): State<Arc<Config>>,
    headers: HeaderMap,
) -> Response {
    let device_id = headers
        .get("x-uprx-device-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();
    upgrade.on_upgrade(move |socket| handle_socket(socket, config, device_id))
}

async fn handle_socket(mut socket: WebSocket, config: Arc<Config>, device_id: String) {
    tracing::info!(%device_id, "uniproxy connected");
    let mut next_stream_id = FIRST_AUDIO_STREAM;

    while let Some(message) = socket.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%device_id, %error, "uniproxy receive failed");
                break;
            }
        };

        match message {
            Message::Binary(bytes) => {
                let Some(payload) = bytes.strip_prefix(AAPI) else {
                    continue;
                };
                let client = match TClientMessage::decode(payload) {
                    Ok(client) => client,
                    Err(error) => {
                        tracing::warn!(%device_id, %error, "uniproxy protobuf decode failed");
                        continue;
                    }
                };
                let Some(ClientMessage::Event(event)) = client.message else {
                    continue;
                };
                let Some(Event::TextInput(input)) = event.event else {
                    continue;
                };
                let Some(request) = input.request else {
                    continue;
                };
                let Some(alice_event) = request.event else {
                    continue;
                };
                if alice_event.r#type != Some(EEventType::TextInput as i32) {
                    continue;
                }
                let text = alice_event.text.unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }

                let request_id = input
                    .header
                    .as_ref()
                    .and_then(|header| header.request_id.clone())
                    .unwrap_or_default();
                let dialog_id = input
                    .header
                    .as_ref()
                    .and_then(|header| header.dialog_id.clone())
                    .unwrap_or_default();
                let sequence_number = input
                    .header
                    .as_ref()
                    .and_then(|header| header.sequence_number)
                    .unwrap_or(0);
                let reference_message_id = event
                    .header
                    .as_ref()
                    .map(|header| header.message_id.clone())
                    .unwrap_or_default();

                match answer(&config, &text, &device_id).await {
                    Ok((answer, _should_listen)) => {
                        if let Err(error) = send_answer(
                            &mut socket,
                            &answer,
                            &request_id,
                            &dialog_id,
                            sequence_number,
                            &reference_message_id,
                            &mut next_stream_id,
                            &config,
                        )
                        .await
                        {
                            tracing::warn!(%device_id, %error, "uniproxy answer failed");
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%device_id, %error, "processor failed");
                    }
                }
            }
            Message::Close(_) => break,
            Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    tracing::info!(%device_id, "uniproxy disconnected");
}

async fn answer(config: &Config, text: &str, device_id: &str) -> Result<(String, bool), String> {
    let url = processor_ws_url(&config.processor_basic_url);
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|error| format!("connecting to processor {url}: {error}"))?;

    let prepare = serde_json::json!({
        "type": "prepare",
        "data": {"directiveFunctions": {}}
    });
    send_json(&mut socket, prepare).await?;

    let process = serde_json::json!({
        "type": "process",
        "data": {
            "metadata": {"internal": {"deviceId": device_id}, "readyToUse": {}},
            "text": text
        }
    });
    send_json(&mut socket, process).await?;

    let deadline = tokio::time::Instant::now() + PROCESSOR_TIMEOUT;
    loop {
        let next = tokio::time::timeout_at(deadline, socket.next()).await;
        let message = match next {
            Err(_) => return Err("processor timeout".to_owned()),
            Ok(None) => return Err("processor closed the socket".to_owned()),
            Ok(Some(Err(error))) => return Err(format!("processor websocket: {error}")),
            Ok(Some(Ok(message))) => message,
        };
        let WsMessage::Text(text) = message else {
            continue;
        };
        let value: Value =
            serde_json::from_str(&text).map_err(|error| format!("processor JSON: {error}"))?;
        if value["type"] != "partialResponse" {
            continue;
        }
        let data = &value["data"];
        if data["finished"] == true {
            let text = data["text"].as_str().unwrap_or_default().to_owned();
            let should_listen = data["shouldListen"].as_bool().unwrap_or(false);
            return Ok((text, should_listen));
        }
    }
}

async fn send_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    value: Value,
) -> Result<(), String> {
    socket
        .send(WsMessage::Text(value.to_string().into()))
        .await
        .map_err(|error| format!("sending to processor: {error}"))
}

#[allow(clippy::too_many_arguments)]
async fn send_answer(
    socket: &mut WebSocket,
    text: &str,
    request_id: &str,
    dialog_id: &str,
    sequence_number: u32,
    reference_message_id: &str,
    next_stream_id: &mut i64,
    config: &Config,
) -> Result<(), String> {
    let response = TResponse {
        ..Default::default()
    };
    let alice = orbita_proto::n_alice::n_alice_api::TAliceResponseProto {
        header: Some(ResponseHeader {
            request_id: Some(request_id.to_owned()),
            response_id: Some(Uuid::new_v4().to_string()),
            sequence_number: Some(sequence_number),
            dialog_id: Some(dialog_id.to_owned()),
            parent_request_id: None,
        }),
        response: Some(response),
        voice_response: Some(TVoiceResponse {
            output_speech: Some(TOutputSpeech {
                text: Some(text.to_owned()),
            }),
            should_listen: false,
            has_voice_response: true,
        }),
        ..Default::default()
    };
    let message = TServerMessage {
        timings: None,
        streaming: None,
        message_group: None,
        message: Some(ServerMessage::Event(ServerEvent {
            header: Some(TEventHeader {
                message_id: Uuid::new_v4().to_string(),
                ref_message_id: reference_message_id.to_owned(),
                ..Default::default()
            }),
            event: Some(ServerEventKind::AliceResponse(alice)),
        })),
    };
    send_server_message(socket, &message).await?;

    if text.is_empty() {
        return Ok(());
    }

    // The station plays one TTS stream per AliceResponse: it stops after the
    // first StreamControl.Close. Keep the whole answer in one stream until
    // multi-stream playback is verified on the device.
    let audio = synthesize(config, text).await?;
    *next_stream_id += 1;
    send_tts_chunk(socket, &audio, *next_stream_id, reference_message_id).await?;
    Ok(())
}

async fn send_tts_chunk(
    socket: &mut WebSocket,
    audio: &[u8],
    stream_id: i64,
    reference_message_id: &str,
) -> Result<(), String> {
    let speak = TServerMessage {
        timings: None,
        streaming: None,
        message_group: None,
        message: Some(ServerMessage::Event(ServerEvent {
            header: Some(TEventHeader {
                message_id: Uuid::new_v4().to_string(),
                ref_message_id: reference_message_id.to_owned(),
                stream_id: Some(stream_id),
                ..Default::default()
            }),
            event: Some(ServerEventKind::TtsSpeak(TTtsSpeak {
                format: "audio/opus".to_owned(),
                lazy_tts_streaming: false,
                ..Default::default()
            })),
        })),
    };
    send_server_message(socket, &speak).await?;

    let mut frame = stream_id.to_be_bytes().to_vec();
    frame.extend_from_slice(audio);
    socket
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|error| format!("sending audio: {error}"))?;

    let close = TServerMessage {
        timings: None,
        streaming: None,
        message_group: None,
        message: Some(ServerMessage::StreamControl(TStreamControl {
            stream_id,
            message_id: Uuid::new_v4().to_string(),
            action: Some(StreamAction::Close(TActionClose::default())),
        })),
    };
    send_server_message(socket, &close).await
}

async fn send_server_message(
    socket: &mut WebSocket,
    message: &TServerMessage,
) -> Result<(), String> {
    let mut frame = AAPI.to_vec();
    frame.extend_from_slice(&message.encode_to_vec());
    socket
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|error| format!("sending AAPI frame: {error}"))
}

async fn synthesize(config: &Config, text: &str) -> Result<Vec<u8>, String> {
    let url = format!(
        "{}/audio/speech",
        config.tts_openai_base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": &config.tts_openai_model,
        "voice": &config.tts_openai_voice,
        "input": text,
        "response_format": "opus",
        "speed": config.tts_openai_speed,
    });
    let client = reqwest::Client::builder()
        .timeout(TTS_TIMEOUT)
        .build()
        .map_err(|error| format!("building TTS client: {error}"))?;
    let mut request = client.post(&url).json(&body);
    if !config.tts_openai_api_key.is_empty() {
        request = request.bearer_auth(&config.tts_openai_api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("TTS request {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("TTS returned {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| format!("reading TTS audio: {error}"))
}

fn processor_ws_url(base: &str) -> String {
    let mut url = base.trim_end_matches('/').to_owned();
    if let Some(rest) = url.strip_prefix("http://") {
        url = format!("ws://{rest}");
    } else if let Some(rest) = url.strip_prefix("https://") {
        url = format!("wss://{rest}");
    }
    if !url.ends_with("/process") {
        url.push_str("/process");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_processor_url() {
        assert_eq!(
            processor_ws_url("http://192.168.250.103:8080/process"),
            "ws://192.168.250.103:8080/process"
        );
        assert_eq!(
            processor_ws_url("http://127.0.0.1:8080"),
            "ws://127.0.0.1:8080/process"
        );
    }
}

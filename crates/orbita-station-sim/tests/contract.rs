//! Simulator against a fake quasar: send TextInput, receive text and TTS audio.

use std::time::Duration;

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::SinkExt;
use orbita_proto::n_alice::n_alice_api::{
    TAliceResponseProto, TAsrRecognition, TAsrResult, TClientMessage, TEventHeader, TServerMessage,
    TStreamControl, TTtsSpeak,
    t_alice_response_proto::{
        TResponse, TVoiceResponse, t_response::TCard, t_voice_response::TOutputSpeech,
    },
    t_client_message::{Message as ClientMessage, t_event::Event as ClientEventKind},
    t_server_message::{
        Message as ServerMessage, TEvent as ServerEvent, t_event::Event as ServerEventKind,
    },
    t_stream_control::{Action as StreamAction, TActionClose},
};
use orbita_station_sim::{
    frame_replay::run_frame_replay,
    protocol::{encode_client, text_input},
    replay::{ReplayOptions, run_replay},
    run_session, run_voice_session,
};
use prost::Message as ProstMessage;

async fn websocket(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(handle)
}

async fn handle(mut socket: WebSocket) {
    let message = socket
        .recv()
        .await
        .expect("client message")
        .expect("websocket");
    let Message::Binary(bytes) = message else {
        panic!("expected binary AAPI frame");
    };
    assert!(bytes.starts_with(b"AAPI"));
    let client = TClientMessage::decode(&bytes[4..]).expect("TClientMessage");
    let text = match client.message.expect("message") {
        ClientMessage::Event(event) => match event.event.expect("event") {
            ClientEventKind::TextInput(request) => request
                .request
                .expect("request")
                .event
                .expect("event")
                .text
                .expect("text"),
            _ => panic!("expected TextInput"),
        },
        _ => panic!("expected Event message"),
    };
    assert!(!text.is_empty());

    let response = TAliceResponseProto {
        response: Some(TResponse {
            cards: vec![TCard {
                r#type: "simple_text".to_owned(),
                text: Some("Ответ".to_owned()),
                card_id: Some("card".to_owned()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        voice_response: Some(TVoiceResponse {
            output_speech: Some(TOutputSpeech {
                text: Some("Ответ".to_owned()),
            }),
            should_listen: false,
            has_voice_response: true,
        }),
        ..Default::default()
    };
    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::Event(ServerEvent {
                header: Some(TEventHeader {
                    message_id: "message".to_owned(),
                    ref_message_id: "request".to_owned(),
                    ..Default::default()
                }),
                event: Some(ServerEventKind::AliceResponse(response)),
            })),
            ..Default::default()
        },
    )
    .await;

    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::Event(ServerEvent {
                header: Some(TEventHeader {
                    message_id: "tts".to_owned(),
                    ref_message_id: "request".to_owned(),
                    stream_id: Some(1),
                    ..Default::default()
                }),
                event: Some(ServerEventKind::TtsSpeak(TTtsSpeak {
                    format: "audio/opus".to_owned(),
                    lazy_tts_streaming: false,
                    sound_chunk_type: 0,
                    sub_stream_id: None,
                })),
            })),
            ..Default::default()
        },
    )
    .await;

    let mut audio = vec![0_u8, 0, 0, 1];
    audio.extend_from_slice(b"OggS-simulated-audio");
    socket
        .send(Message::Binary(audio.into()))
        .await
        .expect("audio frame");

    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::StreamControl(TStreamControl {
                stream_id: 1,
                message_id: "close".to_owned(),
                action: Some(StreamAction::Close(TActionClose { reason: 0 })),
            })),
            ..Default::default()
        },
    )
    .await;

    let _ = socket.close().await;
}

async fn voice_websocket(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(handle_voice)
}

async fn handle_voice(mut socket: WebSocket) {
    let message = socket
        .recv()
        .await
        .expect("voice message")
        .expect("websocket");
    let Message::Binary(bytes) = message else {
        panic!("expected voice AAPI frame");
    };
    assert!(bytes.starts_with(b"AAPI"));
    let client = TClientMessage::decode(&bytes[4..]).expect("TClientMessage");
    assert!(matches!(
        client.message,
        Some(ClientMessage::Event(event))
            if matches!(event.event, Some(ClientEventKind::VoiceInput(_)))
    ));

    let audio = socket.recv().await.expect("audio").expect("websocket");
    let Message::Binary(audio) = audio else {
        panic!("expected audio frame");
    };
    assert_eq!(&audio[..4], &1_u32.to_be_bytes());

    let close = socket.recv().await.expect("close").expect("websocket");
    let Message::Binary(close) = close else {
        panic!("expected stream close frame");
    };
    let close = TClientMessage::decode(&close[4..]).expect("close message");
    assert!(matches!(
        close.message,
        Some(ClientMessage::StreamControl(_))
    ));

    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::Event(ServerEvent {
                header: Some(TEventHeader {
                    message_id: "asr".to_owned(),
                    ref_message_id: "request".to_owned(),
                    ..Default::default()
                }),
                event: Some(ServerEventKind::AsrResult(TAsrResult {
                    end_of_utt: Some(true),
                    recognition: vec![TAsrRecognition {
                        normalized: Some("привет".to_owned()),
                        confidence: Some(0.9),
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
            })),
            ..Default::default()
        },
    )
    .await;

    let response = TAliceResponseProto {
        voice_response: Some(TVoiceResponse {
            output_speech: Some(TOutputSpeech {
                text: Some("Ответ".to_owned()),
            }),
            should_listen: false,
            has_voice_response: true,
        }),
        ..Default::default()
    };
    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::Event(ServerEvent {
                header: Some(TEventHeader {
                    message_id: "message".to_owned(),
                    ref_message_id: "request".to_owned(),
                    ..Default::default()
                }),
                event: Some(ServerEventKind::AliceResponse(response)),
            })),
            ..Default::default()
        },
    )
    .await;

    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::Event(ServerEvent {
                header: Some(TEventHeader {
                    message_id: "tts".to_owned(),
                    ref_message_id: "request".to_owned(),
                    stream_id: Some(1),
                    ..Default::default()
                }),
                event: Some(ServerEventKind::TtsSpeak(TTtsSpeak {
                    format: "audio/opus".to_owned(),
                    ..Default::default()
                })),
            })),
            ..Default::default()
        },
    )
    .await;

    let mut audio = vec![0_u8, 0, 0, 1];
    audio.extend_from_slice(b"OggS-simulated-audio");
    socket
        .send(Message::Binary(audio.into()))
        .await
        .expect("audio frame");

    send_server(
        &mut socket,
        TServerMessage {
            message: Some(ServerMessage::StreamControl(TStreamControl {
                stream_id: 1,
                message_id: "close".to_owned(),
                action: Some(StreamAction::Close(TActionClose { reason: 0 })),
            })),
            ..Default::default()
        },
    )
    .await;
    let _ = socket.close().await;
}

async fn send_server(socket: &mut WebSocket, message: TServerMessage) {
    let mut frame = b"AAPI".to_vec();
    frame.extend_from_slice(&message.encode_to_vec());
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("server frame");
}

#[tokio::test]
async fn sends_text_and_collects_audio() {
    let app = Router::new().route("/uniproxy.alice.yandex.net/uni.ws", get(websocket));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let report = run_session(
        &format!("ws://{addr}/uniproxy.alice.yandex.net/uni.ws"),
        "duid-1",
        "yandexmicro",
        "привет",
        None,
        Duration::from_secs(5),
    )
    .await
    .expect("session");

    assert_eq!(report.text.as_deref(), Some("Ответ"));
    assert_eq!(report.audio_chunks, 1);
    assert_eq!(report.audio_bytes, b"OggS-simulated-audio".len());
    assert!(report.response_ms.is_some());
    assert!(report.first_audio_ms.is_some());
    assert!(report.total_ms >= report.first_audio_ms.unwrap());
}

#[tokio::test]
async fn sends_voice_and_collects_transcript() {
    let app = Router::new().route("/uniproxy.alice.yandex.net/uni.ws", get(voice_websocket));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let report = run_voice_session(
        &format!("ws://{addr}/uniproxy.alice.yandex.net/uni.ws"),
        "duid-1",
        "yandexmicro",
        b"fake-ogg-opus",
        None,
        Duration::from_secs(5),
    )
    .await
    .expect("voice session");

    assert_eq!(report.asr_text.as_deref(), Some("привет"));
    assert_eq!(report.text.as_deref(), Some("Ответ"));
    assert_eq!(report.audio_chunks, 1);
}

#[tokio::test]
async fn replays_a_scenario_with_timings() {
    let app = Router::new().route("/uniproxy.alice.yandex.net/uni.ws", get(websocket));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let directory = std::env::temp_dir().join(format!("orbita-sim-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("scenario.json");
    std::fs::write(
        &path,
        r#"{"steps":[
            {"type":"text","text":"привет"},
            {"type":"text","text":"ещё","delay_ms":20}
        ]}"#,
    )
    .unwrap();

    let url = format!("ws://{addr}/uniproxy.alice.yandex.net/uni.ws");
    let reports = run_replay(
        &path,
        ReplayOptions {
            url: &url,
            duid: "duid-1",
            platform: "yandexmicro",
            audio_dir: None,
            timeout: Duration::from_secs(5),
        },
    )
    .await
    .expect("replay");
    let _ = std::fs::remove_dir_all(&directory);

    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].kind, "text");
    assert_eq!(reports[0].input, "привет");
    assert_eq!(reports[1].report.text.as_deref(), Some("Ответ"));
    assert!(reports[1].report.response_ms.is_some());
    assert!(reports[1].report.total_ms >= reports[1].report.response_ms.unwrap());
}

#[tokio::test]
async fn replays_a_recorded_frame_session() {
    let app = Router::new().route("/uniproxy.alice.yandex.net/uni.ws", get(websocket));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let directory = std::env::temp_dir().join(format!("orbita-frames-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let session_path = directory.join("session.jsonl");
    let output_path = directory.join("replayed.jsonl");
    let frame = encode_client(&text_input("привет", "dialog"));
    let content = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "meta",
            "started_at": 0,
            "path": "/uniproxy.alice.yandex.net/uni.ws",
            "headers": {"x-uprx-device-id": "duid-1"}
        }),
        serde_json::json!({
            "type": "frame",
            "ts_ms": 0,
            "dir": "c",
            "kind": "binary",
            "data": STANDARD.encode(frame)
        })
    );
    std::fs::write(&session_path, content).unwrap();

    let url = format!("ws://{addr}/uniproxy.alice.yandex.net/uni.ws");
    let report = run_frame_replay(
        &url,
        "duid-1",
        "yandexmicro",
        &session_path,
        Some(&output_path),
        Duration::from_secs(5),
    )
    .await
    .expect("frame replay");
    let output = std::fs::read_to_string(&output_path).unwrap();
    let _ = std::fs::remove_dir_all(&directory);

    assert_eq!(report.sent, 1);
    assert_eq!(report.text.as_deref(), Some("Ответ"));
    assert!(report.received >= 1);
    assert!(output.contains("\"dir\":\"s\""));
}

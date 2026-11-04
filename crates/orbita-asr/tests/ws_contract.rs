//! Black-box WebSocket contract for the streaming ASR service.

use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use orbita_asr::{AppState, Config, FakeRecognizer, StreamingMode, VadDetector, build_router};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct ScriptedVad {
    frame: usize,
    speech_to: usize,
}

impl VadDetector for ScriptedVad {
    fn process(&mut self, _frame: &[i16]) -> (f32, bool) {
        let speech = self.frame < self.speech_to;
        self.frame += 1;
        (if speech { 1.0 } else { 0.0 }, speech)
    }
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".to_owned(),
        port: 0,
        input_gain: 1.0,
        vad_threshold: 0.1,
        eou_silence_ms: 100,
        min_utterance_ms: 100,
        max_utterance_ms: 8_000,
        speech_pad_ms: 0,
        partial_results: false,
        partial_interval_ms: 1_000,
        min_partial_audio_ms: 1_800,
        emit_vad_events: false,
        streaming_mode: StreamingMode::Streaming,
        quasistreaming_stable_repeats: 3,
        language: "ru".to_owned(),
        engine: "fake".to_owned(),
        vad: "energy".to_owned(),
    }
}

async fn start_server() -> String {
    let state = Arc::new(AppState {
        config: Arc::new(config()),
        recognizer: Arc::new(FakeRecognizer::new("тест")),
        vad_factory: Arc::new(|| {
            Box::new(ScriptedVad {
                frame: 0,
                speech_to: 30,
            })
        }),
    });
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/")
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

async fn next_transcript(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("timed out waiting for a transcript")
            .expect("websocket closed")
            .expect("websocket error");
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(text.as_str()).expect("json transcript");
        if value.get("text").is_some() {
            return value;
        }
    }
}

#[tokio::test]
async fn sends_final_transcripts_and_restarts_the_session() {
    let url = start_server().await;
    let (mut socket, _) = connect_async(url.as_str()).await.unwrap();

    socket
        .send(Message::Text(
            json!({"sample_rate": 16000}).to_string().into(),
        ))
        .await
        .unwrap();

    for expected in 0..2 {
        socket
            .send(Message::Binary(pcm_bytes(&vec![1_000_i16; 8_000]).into()))
            .await
            .unwrap();
        socket
            .send(Message::Binary(pcm_bytes(&vec![0_i16; 8_000]).into()))
            .await
            .unwrap();

        let transcript = next_transcript(&mut socket).await;
        assert_eq!(transcript["text"], "тест", "utterance {expected}");
        assert_eq!(transcript["end_of_utt"], true, "utterance {expected}");
        assert_eq!(transcript["result_type"], "final", "utterance {expected}");
    }
}

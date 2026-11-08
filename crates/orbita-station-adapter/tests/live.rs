//! Live black-box run against a deployed adapter and a real core.
//!
//! `ADAPTER_WS_URL=ws://192.168.250.105:8081/process cargo test -p
//! orbita-station-adapter --test live -- --nocapture`. Without the variable
//! the test is skipped so CI stays offline. Prints route-independent timings.
//! Timers create real scheduler jobs for the fake device `live-test-duid`;
//! they fire silently (quasar has no such station) and delete themselves.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

async fn process(url: &str, text: &str) -> (Value, Duration) {
    let prepare: Value =
        serde_json::from_str(include_str!("fixtures/prepare_station.json")).unwrap();
    let started = Instant::now();
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    for message in [
        json!({"type": "prepare", "data": {"directiveFunctions": prepare}}),
        json!({"type": "process", "data": {
            "metadata": {"internal": {"deviceId": "live-test-duid"}},
            "text": text,
        }}),
    ] {
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
    loop {
        let message = tokio::time::timeout(Duration::from_secs(60), socket.next())
            .await
            .expect("adapter reply timeout")
            .expect("adapter closed the socket")
            .expect("websocket error");
        if let Message::Text(text) = message {
            return (
                serde_json::from_str::<Value>(text.as_str()).unwrap()["data"].clone(),
                started.elapsed(),
            );
        }
    }
}

fn names(data: &Value) -> Vec<String> {
    data["directives"]
        .as_array()
        .map(|directives| {
            directives
                .iter()
                .filter_map(|directive| directive["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn live_adapter_scenarios() {
    let Some(url) = std::env::var("ADAPTER_WS_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        eprintln!("ADAPTER_WS_URL is not set; skipping the live run");
        return;
    };

    // (utterance, expected directive or None for a spoken-only answer)
    let cases: [(&str, Option<&str>); 7] = [
        (
            "Алиса, сделай громче",
            Some("smart_speaker_set_sound_volume_louder"),
        ),
        // D-26: timers and reminders go to the core scheduler, not the
        // station; the answer is spoken, no directive.
        ("поставь таймер на 2 минуты", None),
        ("засеки три минутки, чай завариваю", None),
        // Free phrasing: the core must call reminder_create (check the log).
        ("через пять минуток скажи мне проверить духовку", None),
        (
            "мне плохо слышно, прибавь звук",
            Some("smart_speaker_set_sound_volume_louder"),
        ),
        ("привет, как дела?", None),
        ("какая сейчас погода в Москве?", None),
    ];
    let mut failures = Vec::new();
    for (text, expected) in cases {
        let (data, elapsed) = process(&url, text).await;
        let directives = names(&data);
        println!(
            "{:>6} ms | {text} → {:?} {:?}",
            elapsed.as_millis(),
            data["text"].as_str().unwrap_or(""),
            directives
        );
        let spoken = data["text"].as_str().unwrap_or("");
        let ok = match expected {
            Some(name) => directives.iter().any(|directive| directive == name),
            None => {
                directives.is_empty() && !spoken.is_empty() && spoken != "Извини, не получилось."
            }
        };
        if !ok {
            failures.push(text);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

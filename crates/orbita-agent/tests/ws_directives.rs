//! Black-box WebSocket contract for station directives.
//!
//! Set `AGENT_WS_URL` (for example `ws://127.0.0.1:8080/process`) to run
//! against a live agent; without it the tests are skipped so CI stays offline.
//! The `prepare` payload is the real map captured from TS quasar.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

fn prepare() -> Value {
    serde_json::from_str(include_str!("fixtures/prepare_station.json")).unwrap()
}

fn agent_url() -> Option<String> {
    std::env::var("AGENT_WS_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

async fn connect(
    url: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket
}

async fn send_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    value: Value,
) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn read_response(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .expect("agent response timeout")
            .expect("agent closed the socket")
            .expect("agent websocket error");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).expect("valid JSON response");
        }
    }
}

async fn process(url: &str, text: &str) -> Value {
    let mut socket = connect(url).await;
    send_json(
        &mut socket,
        json!({"type": "prepare", "data": {"directiveFunctions": prepare()}}),
    )
    .await;
    send_json(
        &mut socket,
        json!({
            "type": "process",
            "data": {
                "metadata": {"internal": {"deviceId": "test-duid"}},
                "text": text
            }
        }),
    )
    .await;
    read_response(&mut socket).await
}

fn directives(response: &Value) -> &[Value] {
    response["data"]["directives"]
        .as_array()
        .expect("directives array")
}

#[tokio::test]
async fn louder_maps_to_the_offered_directive() {
    let Some(url) = agent_url() else {
        eprintln!("AGENT_WS_URL is not set; skipping");
        return;
    };
    let response = process(&url, "громче").await;
    let directives = directives(&response);
    assert!(
        directives
            .iter()
            .any(|directive| { directive["name"] == "smart_speaker_set_sound_volume_louder" }),
        "unexpected response: {response}"
    );
    assert!(
        directives
            .iter()
            .all(|directive| directive.get("schedule").is_none()),
        "schedule must not leave the agent"
    );
}

#[tokio::test]
async fn absolute_volume_carries_the_level() {
    let Some(url) = agent_url() else {
        eprintln!("AGENT_WS_URL is not set; skipping");
        return;
    };
    let response = process(&url, "громкость 4").await;
    let directive = directives(&response)
        .iter()
        .find(|directive| directive["name"] == "smart_speaker_set_sound_volume")
        .unwrap_or_else(|| panic!("no volume directive: {response}"));
    assert_eq!(directive["arguments"]["volume"], 4);
}

#[tokio::test]
async fn timer_uses_native_timer_directive() {
    let Some(url) = agent_url() else {
        eprintln!("AGENT_WS_URL is not set; skipping");
        return;
    };
    let response = process(&url, "поставь таймер на 5 минут").await;
    let directive = directives(&response)
        .iter()
        .find(|directive| directive["name"] == "smart_speaker_timer_set")
        .unwrap_or_else(|| panic!("no timer directive: {response}"));
    assert_eq!(directive["arguments"]["durationSeconds"], 300);
    assert!(directive.get("schedule").is_none());
}

#[tokio::test]
async fn without_prepare_a_plain_question_stays_text() {
    let Some(url) = agent_url() else {
        eprintln!("AGENT_WS_URL is not set; skipping");
        return;
    };
    let mut socket = connect(&url).await;
    send_json(
        &mut socket,
        json!({
            "type": "process",
            "data": {"metadata": {"internal": {"deviceId": "test-duid"}}, "text": "привет"}
        }),
    )
    .await;
    let response = read_response(&mut socket).await;
    assert_eq!(response["type"], "partialResponse");
    assert!(directives(&response).is_empty(), "unexpected: {response}");
    assert!(
        response["data"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
}

#[tokio::test]
async fn prepare_alone_does_not_answer() {
    let Some(url) = agent_url() else {
        eprintln!("AGENT_WS_URL is not set; skipping");
        return;
    };
    let mut socket = connect(&url).await;
    send_json(
        &mut socket,
        json!({"type": "prepare", "data": {"directiveFunctions": prepare()}}),
    )
    .await;
    let next = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
    assert!(next.is_err(), "prepare must not produce a response");
}

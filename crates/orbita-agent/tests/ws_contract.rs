//! Black-box contract test for the quasar processor protocol.
//!
//! quasar connects to `/process`, sends `prepare` and `process`, and expects a
//! final `partialResponse`. The test runs the real axum server and a fake LLM.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use orbita_agent::{
    Agent, AppState, Store, build_router,
    llm::{LlmClient, Message},
};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

struct FakeLlm;

#[async_trait]
impl LlmClient for FakeLlm {
    async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
        Ok("ответ".to_owned())
    }
}

async fn start() -> (String, Arc<Store>) {
    let store = Arc::new(Store::open(":memory:").unwrap());
    let agent = Arc::new(Agent::new(
        Arc::clone(&store),
        Arc::new(FakeLlm),
        "system",
        10,
    ));
    let app = build_router(Arc::new(AppState {
        agent,
        model: "fake".to_owned(),
        default_duid: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://{addr}/process"), store)
}

async fn turn(url: &str, text: &str) -> Value {
    let (mut socket, _) = connect_async(url).await.unwrap();
    socket
        .send(WsMessage::Text(
            json!({"type": "prepare", "data": {"directiveFunctions": {}}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
        .send(WsMessage::Text(
            json!({
                "type": "process",
                "data": {
                    "metadata": {"internal": {"deviceId": "duid-1"}, "readyToUse": {}},
                    "text": text
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WsMessage::Text(text) = message else {
        panic!("expected text");
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn dialogue_turn_returns_partial_response() {
    let (url, store) = start().await;
    let response = turn(&url, "привет").await;
    assert_eq!(response["type"], "partialResponse");
    assert_eq!(response["data"]["text"], "ответ");
    assert_eq!(response["data"]["finished"], true);
    assert_eq!(store.messages("duid-1", 10).unwrap().len(), 2);
}

#[tokio::test]
async fn reminder_turn_stores_task() {
    let (url, store) = start().await;
    let response = turn(&url, "напомни через 20 минут чай").await;
    assert_eq!(response["data"]["text"], "Напомню через 20 минут. чай.");
    let due = store
        .due_tasks(Utc::now() + chrono::Duration::minutes(21), 10)
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].device_id.as_deref(), Some("duid-1"));
}

//! Black-box WebSocket contract: a fake OpenAI SSE server and quasar-shaped
//! `prepare` / `process` messages.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::Response,
    routing::post,
};
use futures_util::{SinkExt, StreamExt};
use orbita_processor::{
    Config, FunctionServer, LlmClient, OpenAiClient, Processor, PromptGenerator, StateServer,
    SystemStateServer, build_router,
    functions::{FunctionInfo, FunctionMap, ServerKind},
};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone)]
struct FakeLlm {
    responses: Arc<Mutex<VecDeque<String>>>,
}

async fn chat(State(state): State<FakeLlm>, Json(_request): Json<Value>) -> Response {
    let content = state
        .responses
        .lock()
        .expect("responses")
        .pop_front()
        .unwrap_or_else(|| {
            json!({
                "function_calls": [],
                "text": "Готово",
                "continue_dialog": false,
                "can_cache": false
            })
            .to_string()
        });

    let mut sse = String::new();
    let characters: Vec<char> = content.chars().collect();
    for chunk in characters.chunks(7) {
        let piece: String = chunk.iter().collect();
        let event = json!({"choices": [{"delta": {"content": piece}}]});
        sse.push_str("data: ");
        sse.push_str(&event.to_string());
        sse.push_str("\n\n");
    }
    sse.push_str("data: [DONE]\n\n");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(sse))
        .expect("response")
}

async fn start_fake_llm(responses: Vec<String>) -> String {
    let state = FakeLlm {
        responses: Arc::new(Mutex::new(responses.into())),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/v1")
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".to_owned(),
        port: 0,
        cache_size: 10,
        prompt_template_path: String::new(),
        functions_prompt_template_path: String::new(),
        state_prompt_template_path: String::new(),
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        openai_model: "test".to_owned(),
        openai_reasoning_effort: None,
        function_server_urls: Vec::new(),
        state_server_urls: Vec::new(),
        mcp_server_urls: Vec::new(),
        nlp_service_url: String::new(),
        nlp_service_timeout_ms: 500,
    }
}

fn prompt() -> Arc<PromptGenerator> {
    Arc::new(
        PromptGenerator::new("SYS", "FUNCTIONS:{{functionsText}}", "STATE:{{stateText}}").unwrap(),
    )
}

async fn start_processor(llm_base_url: String, servers: Vec<Arc<dyn FunctionServer>>) -> String {
    let llm: Arc<dyn LlmClient> =
        Arc::new(OpenAiClient::new(llm_base_url, "", "test", None).unwrap());
    let state_servers: Vec<Arc<dyn StateServer>> = vec![Arc::new(SystemStateServer)];
    let processor = Arc::new(Processor::new(
        config(),
        llm,
        prompt(),
        servers,
        state_servers,
    ));
    let app = build_router(processor);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/process")
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

async fn read_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("timed out waiting for a processor message")
        .expect("websocket closed")
        .expect("websocket error");
    let Message::Text(text) = message else {
        panic!("expected a text message, got {message:?}");
    };
    serde_json::from_str(text.as_str()).expect("json response")
}

#[tokio::test]
async fn answers_a_plain_request() {
    let llm = start_fake_llm(vec![
        json!({
            "function_calls": [],
            "text": "Привет",
            "continue_dialog": false,
            "can_cache": true
        })
        .to_string(),
    ])
    .await;
    let url = start_processor(llm, Vec::new()).await;
    let (mut socket, _) = connect_async(url.as_str()).await.unwrap();

    send_json(
        &mut socket,
        json!({"type": "prepare", "data": {"directiveFunctions": {}}}),
    )
    .await;
    send_json(
        &mut socket,
        json!({
            "type": "process",
            "data": {
                "metadata": {"internal": {}, "readyToUse": {}},
                "text": "привет"
            }
        }),
    )
    .await;

    let response = read_json(&mut socket).await;
    assert_eq!(response["type"], "partialResponse");
    assert_eq!(response["data"]["text"], "Привет");
    assert_eq!(response["data"]["finished"], true);
    assert_eq!(response["data"]["shouldListen"], false);
    assert_eq!(response["data"]["directives"], json!([]));
}

#[tokio::test]
async fn returns_directive_calls() {
    let llm = start_fake_llm(vec![
        json!({
            "function_calls": [{"name": "set_lights", "args": {"state": 1}}],
            "text": "Включаю",
            "continue_dialog": false,
            "can_cache": false
        })
        .to_string(),
    ])
    .await;
    let url = start_processor(llm, Vec::new()).await;
    let (mut socket, _) = connect_async(url.as_str()).await.unwrap();

    send_json(
        &mut socket,
        json!({
            "type": "prepare",
            "data": {
                "directiveFunctions": {
                    "set_lights": {
                        "arguments": {"type": "object"},
                        "description": "Turn lights on or off"
                    }
                }
            }
        }),
    )
    .await;
    send_json(
        &mut socket,
        json!({
            "type": "process",
            "data": {
                "metadata": {"internal": {}, "readyToUse": {}},
                "text": "включи свет"
            }
        }),
    )
    .await;

    let response = read_json(&mut socket).await;
    assert_eq!(response["data"]["text"], "Включаю");
    assert_eq!(
        response["data"]["directives"],
        json!([{"name": "set_lights", "arguments": {"state": 1}}])
    );
    assert_eq!(response["data"]["finished"], true);
}

struct EchoServer {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl FunctionServer for EchoServer {
    fn name(&self) -> String {
        "echo".to_owned()
    }

    fn kind(&self) -> ServerKind {
        ServerKind::Remote
    }

    async fn get_independent_functions(&self) -> Result<FunctionMap, String> {
        Ok(BTreeMap::from([(
            "echo".to_owned(),
            FunctionInfo {
                arguments: json!({"type": "object"}),
                description: "Echo a value".to_owned(),
                has_response: true,
            },
        )]))
    }

    async fn call_function(
        &self,
        _session_id: &str,
        _metadata: &Value,
        _name: &str,
        arguments: &Value,
    ) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!(
            "echo:{}",
            arguments["value"].as_str().unwrap_or("")
        ))
    }
}

#[tokio::test]
async fn makes_a_second_llm_pass_after_a_response_function() {
    let llm = start_fake_llm(vec![
        json!({
            "function_calls": [{"name": "echo", "args": {"value": "hi"}}],
            "text": "Проверяю",
            "continue_dialog": false,
            "can_cache": false
        })
        .to_string(),
        json!({
            "function_calls": [],
            "text": "Готово",
            "continue_dialog": false,
            "can_cache": false
        })
        .to_string(),
    ])
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let echo: Arc<dyn FunctionServer> = Arc::new(EchoServer {
        calls: Arc::clone(&calls),
    });
    let url = start_processor(llm, vec![echo]).await;
    let (mut socket, _) = connect_async(url.as_str()).await.unwrap();

    send_json(
        &mut socket,
        json!({"type": "prepare", "data": {"directiveFunctions": {}}}),
    )
    .await;
    send_json(
        &mut socket,
        json!({
            "type": "process",
            "data": {
                "metadata": {"internal": {}, "readyToUse": {}},
                "text": "проверь"
            }
        }),
    )
    .await;

    let first = read_json(&mut socket).await;
    assert_eq!(first["data"]["text"], "Проверяю");
    assert_eq!(first["data"]["finished"], false);

    let second = read_json(&mut socket).await;
    assert_eq!(second["data"]["text"], "Готово");
    assert_eq!(second["data"]["finished"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

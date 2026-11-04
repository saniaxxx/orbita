//! Processor -> MCP -> SearXNG, entirely in-process.
//!
//! The fake LLM calls `search_search`; the real `orbita-mcp-search` server
//! speaks to a mock SearXNG; the second LLM pass must receive the search text.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::Response,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use orbita_processor::{
    Config, LlmClient, McpFunctionServer, OpenAiClient, Processor, PromptGenerator, StateServer,
    SystemStateServer, build_router,
};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone)]
struct FakeLlm {
    responses: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn chat(State(state): State<FakeLlm>, Json(request): Json<Value>) -> Response {
    state.requests.lock().expect("requests").push(request);
    let content = state
        .responses
        .lock()
        .expect("responses")
        .pop_front()
        .unwrap_or_else(|| {
            json!({"function_calls": [], "text": "Готово", "continue_dialog": false}).to_string()
        });

    let mut sse = String::new();
    let characters: Vec<char> = content.chars().collect();
    for chunk in characters.chunks(7) {
        let piece: String = chunk.iter().collect();
        sse.push_str("data: ");
        sse.push_str(&json!({"choices": [{"delta": {"content": piece}}]}).to_string());
        sse.push_str("\n\n");
    }
    sse.push_str("data: [DONE]\n\n");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(sse))
        .expect("response")
}

async fn searx() -> Json<Value> {
    Json(json!({
        "results": [{
            "title": "Погода в Москве",
            "url": "https://weather.example/moscow",
            "content": "Сейчас в Москве ясно, +5 градусов."
        }]
    }))
}

async fn start_fake_llm(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = FakeLlm {
        responses: Arc::new(Mutex::new(responses.into())),
        requests: Arc::clone(&requests),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), requests)
}

async fn start_mcp_search() -> String {
    let app = Router::new().route("/search", get(searx));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let searx_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = orbita_mcp_search::Config {
        host: "127.0.0.1".to_owned(),
        port: 0,
        searx_url: format!("http://{searx_addr}"),
        max_results: 5,
        snippet_chars: 300,
        timeout: Duration::from_secs(10),
    };
    let app = orbita_mcp_search::build_router(&config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{mcp_addr}/mcp")
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

async fn start_processor(llm_base_url: String, mcp_url: String) -> String {
    let llm: Arc<dyn LlmClient> =
        Arc::new(OpenAiClient::new(llm_base_url, "", "test", None).unwrap());
    let prompt = Arc::new(
        PromptGenerator::new("SYS", "FUNCTIONS:{{functionsText}}", "STATE:{{stateText}}").unwrap(),
    );
    let servers: Vec<Arc<dyn orbita_processor::FunctionServer>> =
        vec![Arc::new(McpFunctionServer::new("search", mcp_url))];
    let state_servers: Vec<Arc<dyn StateServer>> = vec![Arc::new(SystemStateServer)];
    let processor = Arc::new(Processor::new(
        config(),
        llm,
        prompt,
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
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
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
async fn search_goes_through_mcp_and_back_to_the_llm() {
    let llm = start_fake_llm(vec![
        json!({
            "function_calls": [{"name": "search_search", "args": {"query": "погода в москве"}}],
            "text": "Сейчас посмотрю",
            "continue_dialog": false,
            "can_cache": false
        })
        .to_string(),
        json!({
            "function_calls": [],
            "text": "В Москве ясно, плюс пять",
            "continue_dialog": false,
            "can_cache": false
        })
        .to_string(),
    ])
    .await;
    let (llm_url, requests) = llm;
    let mcp_url = start_mcp_search().await;
    let processor_url = start_processor(llm_url, mcp_url).await;

    let (mut socket, _) = connect_async(processor_url.as_str()).await.unwrap();
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
                "text": "какая погода в москве"
            }
        }),
    )
    .await;

    let first = read_json(&mut socket).await;
    assert_eq!(first["data"]["text"], "Сейчас посмотрю");
    assert_eq!(first["data"]["finished"], false);

    let second = read_json(&mut socket).await;
    assert_eq!(second["data"]["text"], "В Москве ясно, плюс пять");
    assert_eq!(second["data"]["finished"], true);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two LLM passes");
    let second_request = requests[1].to_string();
    assert!(
        second_request.contains("Погода в Москве"),
        "{second_request}"
    );
    assert!(
        second_request.contains("https://weather.example/moscow"),
        "{second_request}"
    );
}

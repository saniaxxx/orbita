//! MCP client and the two-pass search loop against a fake server.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use axum::{Json, Router, http::HeaderMap, routing::post};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use orbita_agent::{
    Agent, LlmClient, McpRegistry, SessionContext, SessionKey, Store, llm::Message,
};

#[derive(Clone, Default)]
struct FakeMcp {
    methods: Arc<Mutex<Vec<String>>>,
}

async fn rpc(
    axum::extract::State(state): axum::extract::State<FakeMcp>,
    Json(body): Json<Value>,
) -> (HeaderMap, Json<Value>) {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    state.methods.lock().unwrap().push(method.clone());
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "1"}
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "search",
                "description": "Search the web",
                "inputSchema": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {"query": {"type": "string"}}
                }
            }]
        }),
        "tools/call" => json!({
            "content": [{"type": "text", "text": "RESULT: +5, ясно"}]
        }),
        _ => Value::Null,
    };
    let mut headers = HeaderMap::new();
    if method == "initialize" {
        headers.insert("mcp-session-id", "test-session".parse().unwrap());
    }
    (
        headers,
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})),
    )
}

async fn fake_server() -> (String, FakeMcp) {
    let state = FakeMcp::default();
    let app = Router::new()
        .route("/mcp", post(rpc))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/mcp"), state)
}

fn specs(url: &str) -> Vec<String> {
    vec![format!("search#{url}")]
}

#[tokio::test]
async fn initializes_lists_and_calls_a_tool() {
    let (url, state) = fake_server().await;
    let mut registry = McpRegistry::from_specs(&specs(&url)).unwrap();
    registry.initialize().await;

    assert!(registry.has_tool("search_search"));
    assert_eq!(registry.tools().count(), 1);
    let result = registry
        .call("search_search", &json!({"query": "погода"}))
        .await
        .unwrap();
    assert_eq!(result, "RESULT: +5, ясно");
    assert_eq!(
        state.methods.lock().unwrap().as_slice(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
}

#[tokio::test]
async fn rejects_a_malformed_server_spec() {
    assert!(McpRegistry::from_specs(&["http://127.0.0.1:1/mcp".into()]).is_err());
}

#[tokio::test]
async fn real_search_server_if_configured() {
    let Ok(url) = std::env::var("AGENT_MCP_URL") else {
        eprintln!("AGENT_MCP_URL is not set; skipping");
        return;
    };
    let mut registry = McpRegistry::from_specs(&specs(&url)).unwrap();
    registry.initialize().await;
    assert!(registry.has_tool("search_search"));
    let result = registry
        .call("search_search", &json!({"query": "погода в Москве"}))
        .await
        .unwrap();
    assert!(!result.trim().is_empty());
}

struct ToolLlm {
    calls: AtomicUsize,
    seen: Mutex<Vec<Message>>,
}

#[async_trait]
impl LlmClient for ToolLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.seen.lock().unwrap().extend_from_slice(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if call == 0 {
            json!({
                "text": "Сейчас проверю погоду.",
                "function_calls": [{
                    "name": "search_search",
                    "arguments": {"query": "погода в Москве"}
                }]
            })
            .to_string()
        } else {
            json!({
                "text": "В Москве плюс пять, ясно.",
                "function_calls": []
            })
            .to_string()
        })
    }
}

#[tokio::test]
async fn agent_runs_the_tool_and_answers_again() {
    let (url, _state) = fake_server().await;
    let mut registry = McpRegistry::from_specs(&specs(&url)).unwrap();
    registry.initialize().await;

    let llm = Arc::new(ToolLlm {
        calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    });
    let store = Arc::new(Store::open(":memory:").unwrap());
    let agent =
        Agent::new(Arc::clone(&store), llm.clone(), "system", 10).with_mcp(Arc::new(registry));

    let outcome = agent
        .process(
            "какая погода в москве",
            &SessionKey::station("duid"),
            &SessionContext::default(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.text, "В Москве плюс пять, ясно.");
    assert!(outcome.actions.is_empty());
    assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
    assert!(
        llm.seen
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.role == "tool" && message.content.contains("RESULT"))
    );
}

//! Streamable HTTP MCP client for server-side tools.
//!
//! The agent owns these tools (search, later home automation). They are not
//! station directives: the model calls them, the agent executes them, and the
//! result goes back through the LLM for the final answer.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    /// `<server>_<tool>`, the name the LLM sees.
    pub exposed_name: String,
    pub tool_name: String,
    pub server_index: usize,
    pub description: String,
    pub arguments: Value,
}

#[derive(Debug)]
struct McpServer {
    name: String,
    url: String,
    client: reqwest::Client,
    session: Mutex<Option<String>>,
}

impl McpServer {
    fn new(name: String, url: String) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("building MCP client: {error}"))?;
        Ok(Self {
            name,
            url,
            client,
            session: Mutex::new(None),
        })
    }

    async fn reset(&self) {
        *self.session.lock().await = None;
    }

    async fn session(&self) -> Result<String, String> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let (session, _) = self
            .post(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": {
                            "name": "orbita-agent",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                }),
                None,
            )
            .await?;
        let session =
            session.ok_or_else(|| "MCP initialize returned no Mcp-Session-Id".to_owned())?;
        self.post(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            Some(&session),
        )
        .await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn rpc(
        &self,
        session: &str,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let (_, response) = self
            .post(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                Some(session),
            )
            .await?;
        if let Some(error) = response.get("error") {
            return Err(format!("MCP {method} failed: {error}"));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("MCP {method} returned no result"))
    }

    async fn post(
        &self,
        body: Value,
        session: Option<&str>,
    ) -> Result<(Option<String>, Value), String> {
        let mut request = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(&body);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("MCP request to {} failed: {error}", self.url))?;
        let status = response.status();
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let text = response
            .text()
            .await
            .map_err(|error| format!("MCP response read failed: {error}"))?;
        if !status.is_success() {
            return Err(format!("MCP returned {status}: {}", text.trim()));
        }
        Ok((session, parse_mcp_body(&text)?))
    }

    async fn list_tools(&self) -> Result<Vec<Value>, String> {
        let session = self.session().await?;
        let result = self.rpc(&session, 2, "tools/list", json!({})).await?;
        result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "MCP tools/list returned no tools array".to_owned())
    }

    async fn call_tool(&self, tool: &str, arguments: &Value) -> Result<String, String> {
        for attempt in 0..2 {
            let session = self.session().await?;
            let result = self
                .rpc(
                    &session,
                    3,
                    "tools/call",
                    json!({"name": tool, "arguments": arguments}),
                )
                .await;
            match result {
                Ok(result) => {
                    return Ok(result
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|content| content.first())
                        .and_then(|block| block.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("N/A")
                        .to_owned());
                }
                Err(error) if attempt == 0 && error.contains("404") => {
                    warn!(server = %self.name, "MCP session expired; reinitializing");
                    self.reset().await;
                }
                Err(error) => return Err(error),
            }
        }
        Err("MCP call failed after reinitialization".to_owned())
    }
}

/// All configured MCP servers and the tools they currently offer.
#[derive(Debug)]
pub struct McpRegistry {
    servers: Vec<McpServer>,
    tools: BTreeMap<String, McpTool>,
}

impl McpRegistry {
    pub fn from_specs(specs: &[String]) -> Result<Self, String> {
        let mut servers = Vec::new();
        for spec in specs {
            let (name, url) = spec
                .split_once('#')
                .ok_or_else(|| format!("MCP server spec must be name#url, got {spec:?}"))?;
            if name.is_empty() || url.is_empty() {
                return Err(format!("MCP server spec must be name#url, got {spec:?}"));
            }
            servers.push(McpServer::new(name.to_owned(), url.to_owned())?);
        }
        Ok(Self {
            servers,
            tools: BTreeMap::new(),
        })
    }

    /// Fetch `tools/list` from every server. A dead server is logged and
    /// skipped; the agent keeps working without its tools.
    pub async fn initialize(&mut self) {
        let mut tools = BTreeMap::new();
        for (index, server) in self.servers.iter().enumerate() {
            match server.list_tools().await {
                Ok(list) => {
                    for tool in list {
                        let Some(tool_name) = tool.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        let exposed_name = format!("{}_{}", server.name, tool_name);
                        tools.insert(
                            exposed_name.clone(),
                            McpTool {
                                exposed_name,
                                tool_name: tool_name.to_owned(),
                                server_index: index,
                                description: tool
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("???")
                                    .to_owned(),
                                arguments: tool
                                    .get("inputSchema")
                                    .cloned()
                                    .unwrap_or_else(|| json!({"type": "object"})),
                            },
                        );
                    }
                }
                Err(error) => warn!(server = %server.name, %error, "MCP tools/list failed"),
            }
        }
        self.tools = tools;
    }

    pub fn tools(&self) -> impl Iterator<Item = &McpTool> {
        self.tools.values()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub async fn call(&self, name: &str, arguments: &Value) -> Result<String, String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("unknown MCP tool {name}"))?;
        let server = self
            .servers
            .get(tool.server_index)
            .ok_or_else(|| format!("MCP server {} is missing", tool.server_index))?;
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments.clone()
        };
        server.call_tool(&tool.tool_name, &arguments).await
    }

    pub fn prompt(&self) -> String {
        if self.tools.is_empty() {
            return String::new();
        }
        let mut block = String::from(
            "Серверные инструменты (вызывай их через function_calls, результат вернётся тебе):\n",
        );
        for tool in self.tools() {
            let arguments = serde_json::to_string(&tool.arguments).unwrap_or_else(|_| "{}".into());
            block.push_str(&format!(
                "- {}: {} (аргументы: {})\n",
                tool.exposed_name, tool.description, arguments
            ));
        }
        block
    }
}

fn parse_mcp_body(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    for line in text.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        return serde_json::from_str(data)
            .map_err(|error| format!("invalid MCP event JSON: {error}"));
    }
    serde_json::from_str(text).map_err(|error| format!("invalid MCP JSON: {error}"))
}

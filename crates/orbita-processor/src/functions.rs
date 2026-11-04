//! Function and state servers.
//!
//! Directive functions are executed in-process (they become directives for
//! quasar). Remote servers speak the HTTP contract from `@v3rt3p/types`.
//! MCP servers are a separate step.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::protocol::{Directive, DirectiveFunctionInfo};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FunctionInfo {
    pub arguments: Value,
    pub description: String,
    #[serde(rename = "hasResponse")]
    pub has_response: bool,
}

pub type FunctionMap = BTreeMap<String, FunctionInfo>;

#[derive(Clone)]
pub struct FunctionEntry {
    pub info: FunctionInfo,
    pub server: Arc<dyn FunctionServer>,
}

pub type Functions = BTreeMap<String, FunctionEntry>;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct StateEntry {
    pub description: String,
    pub value: String,
}

pub type State = BTreeMap<String, StateEntry>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Directive,
    Remote,
}

#[async_trait]
pub trait FunctionServer: Send + Sync {
    fn name(&self) -> String;
    fn kind(&self) -> ServerKind;

    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    async fn get_independent_functions(&self) -> Result<FunctionMap, String> {
        Ok(BTreeMap::new())
    }

    async fn get_functions(
        &self,
        _session_id: &str,
        _metadata: &Value,
    ) -> Result<FunctionMap, String> {
        Ok(BTreeMap::new())
    }

    async fn call_function(
        &self,
        _session_id: &str,
        _metadata: &Value,
        _name: &str,
        _arguments: &Value,
    ) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct DirectiveFunctionServer {
    functions: BTreeMap<String, DirectiveFunctionInfo>,
}

impl DirectiveFunctionServer {
    pub fn new(functions: BTreeMap<String, DirectiveFunctionInfo>) -> Self {
        Self { functions }
    }

    pub fn directive(&self, name: &str, arguments: &Value) -> Option<Directive> {
        self.functions.get(name).map(|_| Directive {
            name: name.to_owned(),
            arguments: arguments.clone(),
        })
    }
}

#[async_trait]
impl FunctionServer for DirectiveFunctionServer {
    fn name(&self) -> String {
        "directive".to_owned()
    }

    fn kind(&self) -> ServerKind {
        ServerKind::Directive
    }

    async fn get_independent_functions(&self) -> Result<FunctionMap, String> {
        Ok(self
            .functions
            .iter()
            .map(|(name, function)| {
                (
                    name.clone(),
                    FunctionInfo {
                        arguments: function.arguments.clone(),
                        description: function.description.clone(),
                        has_response: false,
                    },
                )
            })
            .collect())
    }

    async fn call_function(
        &self,
        _session_id: &str,
        _metadata: &Value,
        _name: &str,
        _arguments: &Value,
    ) -> Result<String, String> {
        Err("directive functions have no async result".to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct RemoteFunctionServer {
    name: String,
    url: String,
    client: reqwest::Client,
}

impl RemoteFunctionServer {
    pub fn new(url: impl Into<String>, client: reqwest::Client) -> Self {
        let url = url.into();
        Self {
            name: format!("remote{{{url}}}"),
            url,
            client,
        }
    }
}

#[async_trait]
impl FunctionServer for RemoteFunctionServer {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> ServerKind {
        ServerKind::Remote
    }

    async fn get_independent_functions(&self) -> Result<FunctionMap, String> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response.json().await.map_err(|error| error.to_string())
    }

    async fn get_functions(
        &self,
        session_id: &str,
        metadata: &Value,
    ) -> Result<FunctionMap, String> {
        let response = self
            .client
            .put(&self.url)
            .json(&json!({"metadata": metadata, "sessionId": session_id}))
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response.json().await.map_err(|error| error.to_string())
    }

    async fn call_function(
        &self,
        session_id: &str,
        metadata: &Value,
        name: &str,
        arguments: &Value,
    ) -> Result<String, String> {
        self.client
            .post(&self.url)
            .json(&json!({
                "arguments": arguments,
                "metadata": metadata,
                "name": name,
                "sessionId": session_id,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        // The upstream HTTP contract does not use the response body.
        Ok(String::new())
    }
}

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// MCP tools are exposed to the LLM as `<server>_<tool>` and are always
/// `hasResponse: true`, matching the upstream `RemoteMCPServer`.
///
/// The client speaks the Streamable HTTP transport directly: initialize once,
/// keep `Mcp-Session-Id`, then ordinary JSON-RPC POSTs. This avoids a second
/// HTTP stack and keeps the wire contract visible in one place.
pub struct McpFunctionServer {
    name: String,
    url: String,
    client: reqwest::Client,
    session: Mutex<Option<String>>,
}

impl McpFunctionServer {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("building the MCP HTTP client");
        Self {
            name: name.into(),
            url: url.into(),
            client,
            session: Mutex::new(None),
        }
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
                            "name": "orbita-processor",
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

#[async_trait]
impl FunctionServer for McpFunctionServer {
    fn name(&self) -> String {
        format!("mcp{{{}#{}}}", self.name, self.url)
    }

    fn kind(&self) -> ServerKind {
        ServerKind::Remote
    }

    async fn initialize(&self) -> Result<(), String> {
        self.session().await.map(|_| ())
    }

    async fn get_independent_functions(&self) -> Result<FunctionMap, String> {
        let session = self.session().await?;
        let result = self.rpc(&session, 2, "tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "MCP tools/list returned no tools array".to_owned())?;

        let mut functions = BTreeMap::new();
        for tool in tools {
            let Some(tool_name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let name = format!("{}_{}", self.name, tool_name);
            functions.insert(
                name,
                FunctionInfo {
                    arguments: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("???")
                        .to_owned(),
                    has_response: true,
                },
            );
        }
        Ok(functions)
    }

    async fn call_function(
        &self,
        _session_id: &str,
        _metadata: &Value,
        name: &str,
        arguments: &Value,
    ) -> Result<String, String> {
        let tool_name = name
            .strip_prefix(&format!("{}_", self.name))
            .unwrap_or(name)
            .to_owned();
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments.clone()
        };

        let session = self.session().await?;
        let result = self
            .rpc(
                &session,
                3,
                "tools/call",
                json!({"name": tool_name, "arguments": arguments}),
            )
            .await?;
        Ok(result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("N/A")
            .to_owned())
    }
}

#[async_trait]
pub trait StateServer: Send + Sync {
    fn name(&self) -> String;

    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    async fn get_independent_state(&self) -> Result<State, String> {
        Ok(BTreeMap::new())
    }

    async fn get_state(&self, _session_id: &str, _metadata: &Value) -> Result<State, String> {
        Ok(BTreeMap::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct SystemStateServer;

#[async_trait]
impl StateServer for SystemStateServer {
    fn name(&self) -> String {
        "system".to_owned()
    }

    async fn get_state(&self, _session_id: &str, _metadata: &Value) -> Result<State, String> {
        let now = Local::now().format("%d-%m-%Y %H:%M:%S").to_string();
        Ok(BTreeMap::from([(
            "date_time".to_owned(),
            StateEntry {
                description: "current time and date in DD-MM-YYYY HH:MM:SS format".to_owned(),
                value: now,
            },
        )]))
    }
}

#[derive(Debug, Clone)]
pub struct RemoteStateServer {
    name: String,
    url: String,
    client: reqwest::Client,
}

impl RemoteStateServer {
    pub fn new(url: impl Into<String>, client: reqwest::Client) -> Self {
        let url = url.into();
        Self {
            name: format!("remote{{{url}}}"),
            url,
            client,
        }
    }
}

#[async_trait]
impl StateServer for RemoteStateServer {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn get_independent_state(&self) -> Result<State, String> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response.json().await.map_err(|error| error.to_string())
    }

    async fn get_state(&self, session_id: &str, metadata: &Value) -> Result<State, String> {
        let response = self
            .client
            .put(&self.url)
            .json(&json!({"metadata": metadata, "sessionId": session_id}))
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response.json().await.map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn directive_server_exposes_functions_without_response() {
        let server = DirectiveFunctionServer::new(BTreeMap::from([(
            "set_lights".to_owned(),
            DirectiveFunctionInfo {
                arguments: json!({"type": "object"}),
                description: "Turn lights on or off".to_owned(),
            },
        )]));
        let functions = server.get_independent_functions().await.unwrap();
        assert_eq!(functions["set_lights"].description, "Turn lights on or off");
        assert!(!functions["set_lights"].has_response);
        assert_eq!(
            server.directive("set_lights", &json!({"state": 1})),
            Some(Directive {
                name: "set_lights".to_owned(),
                arguments: json!({"state": 1})
            })
        );
    }

    #[tokio::test]
    async fn system_state_contains_the_date() {
        let state = SystemStateServer
            .get_state("session", &Value::Null)
            .await
            .unwrap();
        let date = &state["date_time"].value;
        assert_eq!(date.len(), 19);
        assert_eq!(&date[2..3], "-");
        assert_eq!(&date[5..6], "-");
    }
}

//! MCP server that exposes SearXNG web search to the processor.
//!
//! This is a Rust port of `services/mcp-search/server.py`: one tool named
//! `search`, compact numbered results, no API keys. The processor joins the
//! MCP server name from `PROCESSOR_MCP_SERVER_URLS` with the tool name, so the
//! station sees `search_search`.

use std::time::Duration;

use anyhow::Context;
use axum::{Json, Router, routing::get};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::json;

const USER_AGENT: &str = "orbita-search/1.0";
const MAX_LIMIT: u32 = 10;

/// Settings read from the environment, matching the Python service.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub searx_url: String,
    pub max_results: u32,
    pub snippet_chars: usize,
    pub timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let timeout_secs: f64 = orbita_core::env_parse("SEARCH_TIMEOUT", 10.0)?;
        if !timeout_secs.is_finite() || timeout_secs <= 0.0 {
            return Err(format!(
                "SEARCH_TIMEOUT must be a positive number, got {timeout_secs}"
            ));
        }

        Ok(Self {
            host: orbita_core::env_or("HOST", "127.0.0.1"),
            port: orbita_core::env_parse("PORT", 8899)?,
            searx_url: orbita_core::env_or("SEARX_URL", "http://127.0.0.1:8888"),
            max_results: orbita_core::env_parse("MAX_RESULTS", 5)?,
            snippet_chars: orbita_core::env_parse("SNIPPET_CHARS", 300)?,
            timeout: Duration::from_secs_f64(timeout_secs),
        })
    }
}

/// Arguments accepted by the `search` tool.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Поисковый запрос на языке пользователя (обычно по-русски), например
    /// «погода в Москве сегодня».
    pub query: String,
    /// Сколько результатов вернуть (1–10). По умолчанию MAX_RESULTS.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearxResult>,
}

#[derive(Debug, Deserialize)]
struct SearxResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// The MCP server. Cheap to clone; the router and HTTP client are shared.
#[derive(Debug, Clone)]
pub struct SearchServer {
    tool_router: ToolRouter<Self>,
    client: reqwest::Client,
    searx_url: String,
    max_results: u32,
    snippet_chars: usize,
}

impl SearchServer {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .context("building the SearXNG HTTP client")?;
        Ok(Self {
            tool_router: Self::tool_router(),
            client,
            searx_url: config.searx_url.trim_end_matches('/').to_owned(),
            max_results: config.max_results,
            snippet_chars: config.snippet_chars,
        })
    }

    async fn fetch(&self, query: &str) -> Result<Vec<SearxResult>, String> {
        let url = format!("{}/search", self.searx_url);
        let response = self
            .client
            .get(url)
            .query(&[("q", query), ("format", "json")])
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let body: SearxResponse = response.json().await.map_err(|error| error.to_string())?;
        Ok(body.results)
    }
}

#[tool_router(router = tool_router)]
impl SearchServer {
    #[tool(
        description = "Веб-поиск актуальной информации в интернете. ВЫЗЫВАЙ ВСЕГДА, когда вопрос касается того, что меняется со временем: погода, новости, курс валют, цены, пробки, события, расписания, результаты матчей, должности, возраст, «сейчас» и «сегодня». Не отвечай на такие вопросы по памяти, даже если кажется, что знаешь ответ. Возвращает заголовки, URL и короткие сниппеты — опирайся на них и называй источник."
    )]
    pub async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        let limit = params.limit.unwrap_or(self.max_results).clamp(1, MAX_LIMIT);
        match self.fetch(&params.query).await {
            Ok(results) => {
                format_results(&params.query, &results, limit as usize, self.snippet_chars)
            }
            Err(error) => format!("Search failed: {error}"),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SearchServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("search", env!("CARGO_PKG_VERSION")))
            .with_instructions("Локальный веб-поиск через SearXNG.")
    }
}

fn format_results(
    query: &str,
    results: &[SearxResult],
    limit: usize,
    snippet_chars: usize,
) -> String {
    if results.is_empty() {
        return format!("No results for '{query}'.");
    }

    let lines: Vec<String> = results
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, result)| {
            let title = collapse_whitespace(result.title.as_deref().unwrap_or(""));
            let link = result.url.as_deref().unwrap_or("");
            let content = collapse_whitespace(result.content.as_deref().unwrap_or(""));
            let snippet: String = content.chars().take(snippet_chars).collect();
            format!("{}. {}\n{}\n{}", index + 1, title, link, snippet)
        })
        .collect();

    lines.join("\n\n")
}

/// Same as Python's `re.sub(r"\s+", " ", value).strip()`.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// HTTP router used by the binary and by in-process integration tests.
pub fn build_router(config: &Config) -> anyhow::Result<Router> {
    let server = SearchServer::new(config)?;
    let service: StreamableHttpService<SearchServer, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let server = server.clone();
                move || Ok(server.clone())
            },
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    Ok(Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", service))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "mcp-search"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(title: &str, url: &str, content: &str) -> SearxResult {
        SearxResult {
            title: Some(title.to_owned()),
            url: Some(url.to_owned()),
            content: Some(content.to_owned()),
        }
    }

    #[test]
    fn formats_the_same_shape_as_the_python_service() {
        let results = vec![
            result(
                "  Первый   заголовок ",
                "https://a.example",
                "  сниппет\n с  пробелами ",
            ),
            result("Второй", "https://b.example", "другой"),
        ];
        assert_eq!(
            format_results("запрос", &results, 5, 300),
            "1. Первый заголовок\nhttps://a.example\nсниппет с пробелами\n\n2. Второй\nhttps://b.example\nдругой"
        );
    }

    #[test]
    fn respects_the_limit() {
        let results: Vec<_> = (0..5)
            .map(|index| result(&format!("t{index}"), "u", "c"))
            .collect();
        let text = format_results("q", &results, 2, 300);
        assert!(text.contains("2. t1"));
        assert!(!text.contains("3. t2"));
    }

    #[test]
    fn truncates_snippets_by_characters() {
        let results = vec![result("t", "u", "абвгд")];
        assert_eq!(format_results("q", &results, 5, 3), "1. t\nu\nабв");
    }

    #[test]
    fn reports_an_empty_result_set() {
        assert_eq!(
            format_results("погода", &[], 5, 300),
            "No results for 'погода'."
        );
    }

    #[test]
    fn collapses_unicode_whitespace() {
        assert_eq!(collapse_whitespace(" a\u{00a0}b\tc\n"), "a b c");
    }
}

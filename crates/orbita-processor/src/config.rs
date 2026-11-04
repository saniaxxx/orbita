//! Environment contract of the upstream processor.

use orbita_core::{env_or, env_parse};

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub cache_size: usize,
    pub prompt_template_path: String,
    pub functions_prompt_template_path: String,
    pub state_prompt_template_path: String,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub openai_model: String,
    pub openai_reasoning_effort: Option<String>,
    pub function_server_urls: Vec<String>,
    pub state_server_urls: Vec<String>,
    pub mcp_server_urls: Vec<String>,
    pub nlp_service_url: String,
    pub nlp_service_timeout_ms: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let cache_size: usize = env_parse("CACHE_SIZE", 1000)?;
        if cache_size == 0 {
            return Err("CACHE_SIZE must be greater than zero".to_owned());
        }

        let openai_reasoning_effort = std::env::var("OPENAI_REASONING_EFFORT")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        Ok(Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_parse("PORT", 8080)?,
            cache_size,
            prompt_template_path: env_or("PROMPT_TEMPLATE_PATH", "prompt.handlebars"),
            functions_prompt_template_path: env_or(
                "FUNCTIONS_PROMPT_TEMPLATE_PATH",
                "prompt-functions.handlebars",
            ),
            state_prompt_template_path: env_or(
                "STATE_PROMPT_TEMPLATE_PATH",
                "prompt-state.handlebars",
            ),
            openai_base_url: env_or("OPENAI_BASE_URL", "https://llm.bksp.in"),
            openai_api_key: env_or("OPENAI_API_KEY", ""),
            openai_model: env_or("OPENAI_MODEL", "qwen2.5-coder-7b-instruct"),
            openai_reasoning_effort,
            function_server_urls: split_urls(&env_or("PROCESSOR_FUNCTION_SERVER_URLS", "")),
            state_server_urls: split_urls(&env_or("PROCESSOR_STATE_SERVER_URLS", "")),
            mcp_server_urls: split_urls(&env_or("PROCESSOR_MCP_SERVER_URLS", "")),
            nlp_service_url: env_or("NLP_SERVICE_URL", ""),
            nlp_service_timeout_ms: env_parse("NLP_SERVICE_TIMEOUT_MS", 500)?,
        })
    }
}

fn split_urls(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

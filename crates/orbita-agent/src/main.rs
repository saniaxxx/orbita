//! Agent service: replaces the processor on the voice path.
//!
//! It keeps the processor WebSocket contract (`/process`), stores dialogue and
//! reminders in SQLite, handles reminders without the LLM and calls the local
//! OpenAI-compatible model for everything else.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use orbita_agent::llm::{FallbackLlm, LlmClient};
use orbita_agent::{
    Agent, AppState, Config, McpRegistry, OpenAiClient, QuasarNotifier, Store, build_router,
    reminder_scheduler,
};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let store = Arc::new(Store::open(&config.db_path).map_err(anyhow::Error::msg)?);
    let recovered = store
        .recover_running(Utc::now())
        .map_err(anyhow::Error::msg)?;
    if recovered > 0 {
        warn!(recovered, "recovered running tasks after restart");
    }

    let primary = OpenAiClient::new(
        &config.openai_base_url,
        &config.openai_api_key,
        &config.openai_model,
    )
    .map_err(anyhow::Error::msg)?;
    let llm: Arc<dyn LlmClient> = if config.fallback_base_url.trim().is_empty() {
        Arc::new(primary)
    } else {
        let fallback = OpenAiClient::new(
            &config.fallback_base_url,
            &config.fallback_api_key,
            &config.fallback_model,
        )
        .map_err(anyhow::Error::msg)?;
        Arc::new(FallbackLlm::new(
            Arc::new(primary),
            Arc::new(fallback),
            std::time::Duration::from_millis(config.primary_timeout_ms),
        ))
    };
    let mut agent = Agent::new(
        Arc::clone(&store),
        llm,
        config.system_prompt.clone(),
        config.history_limit,
    );
    if !config.mcp_server_urls.is_empty() {
        let mut mcp =
            McpRegistry::from_specs(&config.mcp_server_urls).map_err(anyhow::Error::msg)?;
        mcp.initialize().await;
        info!(tools = mcp.tools().count(), "MCP tools loaded");
        agent = agent.with_mcp(Arc::new(mcp));
    }
    if !config.home_config.trim().is_empty() {
        match orbita_agent::home::HomeService::start(&config.home_config).await {
            Ok(home) => {
                info!(config = %config.home_config, "home control ready");
                agent = agent.with_home(Arc::new(home));
            }
            Err(error) => warn!(%error, "home control disabled"),
        }
    }
    let agent = Arc::new(agent);
    let notifier =
        Arc::new(QuasarNotifier::new(&config.quasar_api_url).map_err(anyhow::Error::msg)?);
    tokio::spawn(reminder_scheduler::run(
        Arc::clone(&store),
        notifier,
        Duration::from_millis(config.scheduler_interval_ms),
    ));

    tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.warm_up().await }
    });
    let state = Arc::new(AppState {
        agent,
        model: config.openai_model.clone(),
        default_duid: config.default_duid.clone(),
    });
    let app = build_router(state);
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", config.host, config.port))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(
        %addr,
        db = %config.db_path,
        model = %config.openai_model,
        "orbita-agent listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving agent")?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("installing a SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }

    tracing::info!("shutting down");
}

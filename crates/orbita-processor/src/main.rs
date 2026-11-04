use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use orbita_processor::{
    Config, McpFunctionServer, OpenAiClient, Processor, PromptGenerator, RemoteFunctionServer,
    RemoteStateServer, SystemStateServer, build_router,
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let prompt = Arc::new(PromptGenerator::from_files(
        &config.prompt_template_path,
        &config.functions_prompt_template_path,
        &config.state_prompt_template_path,
    )?);

    let llm = Arc::new(OpenAiClient::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.openai_model.clone(),
        config.openai_reasoning_effort.clone(),
    )?);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let function_servers = config
        .function_server_urls
        .iter()
        .map(|url| {
            Arc::new(RemoteFunctionServer::new(url, client.clone()))
                as Arc<dyn orbita_processor::FunctionServer>
        })
        .collect::<Vec<_>>();

    let mut function_servers = function_servers;
    for spec in &config.mcp_server_urls {
        let Some((name, url)) = spec.split_once('#') else {
            tracing::warn!(%spec, "MCP server must be specified as name#url; ignoring");
            continue;
        };
        function_servers.push(Arc::new(McpFunctionServer::new(name, url)));
    }

    let mut state_servers =
        vec![Arc::new(SystemStateServer) as Arc<dyn orbita_processor::StateServer>];
    for url in &config.state_server_urls {
        state_servers.push(Arc::new(RemoteStateServer::new(url, client.clone())));
    }

    if !config.mcp_server_urls.is_empty() {
        tracing::info!(
            count = config.mcp_server_urls.len(),
            "MCP function servers configured"
        );
    }
    if !config.nlp_service_url.is_empty() {
        tracing::warn!("NLP_SERVICE_URL is not implemented yet; ignoring");
    }

    let processor = Arc::new(Processor::new(
        config.clone(),
        llm,
        prompt,
        function_servers,
        state_servers,
    ));
    let app = build_router(processor);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", config.host, config.port))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(
        %addr,
        model = %config.openai_model,
        base_url = %config.openai_base_url,
        "processor listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving HTTP")?;
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

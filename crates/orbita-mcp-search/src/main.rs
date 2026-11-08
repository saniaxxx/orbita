use std::net::SocketAddr;

use anyhow::{Context, Result};
use orbita_mcp_search::{Config, build_router};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", config.host, config.port))?;
    let token = orbita_mcp_http::token_from_env(&config.host).map_err(anyhow::Error::msg)?;
    let protected = token.is_some();
    let app = orbita_mcp_http::protect(build_router(&config)?, token);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(
        %addr,
        searx_url = %config.searx_url,
        snippet_chars = config.snippet_chars,
        protected,
        "mcp-search listening"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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

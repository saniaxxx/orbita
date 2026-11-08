//! `orbita-tv`: MCP server for Android TV / Google TV.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use orbita_tv::{build_router, config::TvConfig};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let host = orbita_core::env_or("HOST", "127.0.0.1");
    let port: u16 = orbita_core::env_parse("PORT", 8898).map_err(anyhow::Error::msg)?;
    let config = TvConfig::from_env().map_err(anyhow::Error::msg)?;
    let device_count = config.enabled().count();
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address {host}:{port}"))?;
    let token = orbita_mcp_http::token_from_env(&host).map_err(anyhow::Error::msg)?;
    let protected = token.is_some();
    let app = orbita_mcp_http::protect(build_router(config), token);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, devices = device_count, protected, "orbita-tv listening");

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

//! `orbita-home-mcp`: MCP facade for home state and wb-rules commands.

use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use orbita_home_mcp::{HomeServer, build_router};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let host = orbita_core::env_or("HOST", "127.0.0.1");
    let port: u16 = orbita_core::env_parse("PORT", 8897).map_err(anyhow::Error::msg)?;
    let simulated =
        orbita_core::env_bool("HOME_MCP_SIMULATED", false).map_err(anyhow::Error::msg)?;
    let config_path = orbita_core::env_or("HOME_CONFIG", "/etc/orbita/home.json");
    let ack_timeout_ms: u64 =
        orbita_core::env_parse("HOME_ACK_TIMEOUT_MS", 2000u64).map_err(anyhow::Error::msg)?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address {host}:{port}"))?;
    let token = orbita_mcp_http::token_from_env(&host).map_err(anyhow::Error::msg)?;
    let protected = token.is_some();

    let server = if simulated {
        HomeServer::new(true)
    } else {
        HomeServer::connect(&config_path, Duration::from_millis(ack_timeout_ms))
            .await
            .map_err(anyhow::Error::msg)?
    };
    let app = orbita_mcp_http::protect(build_router(server), token);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(
        %addr,
        simulated,
        protected,
        config = %config_path,
        ack_timeout_ms,
        "orbita-home-mcp listening"
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

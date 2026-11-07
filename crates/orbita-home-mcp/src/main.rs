//! `orbita-home-mcp`: MCP facade for home state and wb-rules commands.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use orbita_home_mcp::{HomeServer, build_router};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let host = orbita_core::env_or("HOST", "127.0.0.1");
    let port: u16 = orbita_core::env_parse("PORT", 8897).map_err(anyhow::Error::msg)?;
    let simulated =
        orbita_core::env_bool("HOME_MCP_SIMULATED", false).map_err(anyhow::Error::msg)?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address {host}:{port}"))?;
    let app = build_router(HomeServer::new(simulated));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, simulated, "orbita-home-mcp listening");

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

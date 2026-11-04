use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use orbita_dashboard::{DashboardState, build_router, demo};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let arguments: Vec<String> = std::env::args().collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("usage: orbita-dashboard [--listen 127.0.0.1:8901] [--demo | --no-demo]");
        return Ok(());
    }

    let listen = value(&arguments, "--listen")
        .or_else(|| std::env::var("LISTEN").ok())
        .unwrap_or_else(|| "127.0.0.1:8901".to_owned());
    let demo_enabled = !arguments.iter().any(|argument| argument == "--no-demo")
        && std::env::var("DEMO")
            .map(|value| value != "0")
            .unwrap_or(true);

    let state = DashboardState::shared();
    if demo_enabled {
        demo::spawn(Arc::clone(&state));
    }

    let app = build_router(Arc::clone(&state));
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid listen address {listen}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, demo = demo_enabled, "dashboard listening");

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

fn value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

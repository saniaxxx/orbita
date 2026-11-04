use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, Result};
use orbita_quasar::{
    AppState, Config, ConnectionRegistry, SqliteStorage, build_router, routers, uniproxy,
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    if std::env::var_os("GLAGOL_JWT_KEY").is_none() {
        tracing::warn!(
            "GLAGOL_JWT_KEY is not set; glagol tokens will stop working after a restart"
        );
    }
    if config.sqlite_path != ":memory:"
        && let Some(parent) = Path::new(&config.sqlite_path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let storage = Arc::new(SqliteStorage::open(&config.sqlite_path)?);
    let state = Arc::new(AppState {
        storage,
        connections: Arc::new(ConnectionRegistry::new()),
        glagol_jwt_key: config.glagol_jwt_key.clone(),
    });
    let api_app = build_router(Arc::clone(&state));
    let http_app =
        routers::build_router(Arc::clone(&state)).merge(uniproxy::router(config.clone()));

    let api_addr: SocketAddr = format!("{}:{}", config.host, config.api_port)
        .parse()
        .with_context(|| format!("invalid API address {}:{}", config.host, config.api_port))?;
    let http_addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid HTTP address {}:{}", config.host, config.port))?;

    let api_listener = tokio::net::TcpListener::bind(api_addr)
        .await
        .with_context(|| format!("binding {api_addr}"))?;
    let http_listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding {http_addr}"))?;

    tracing::info!(
        %http_addr,
        sqlite = %config.sqlite_path,
        "quasar HTTP routers and text uniproxy listening; voice input is not implemented yet"
    );
    tracing::info!(%api_addr, "quasar management API listening");

    tokio::spawn(async move {
        if let Err(error) = axum::serve(api_listener, api_app).await {
            tracing::error!(%error, "management API stopped");
        }
    });

    axum::serve(http_listener, http_app)
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

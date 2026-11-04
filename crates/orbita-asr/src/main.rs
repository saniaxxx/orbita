use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use orbita_asr::{
    AppState, Config, EnergyVad, FakeRecognizer, Recognizer, VadDetector, build_router,
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let recognizer: Arc<dyn Recognizer> = match config.engine.as_str() {
        "fake" => Arc::new(FakeRecognizer::default()),
        "gigaam" => load_gigaam()?,
        other => bail!("ENGINE={other} is not supported; use gigaam or fake"),
    };

    let threshold = config.vad_threshold;
    let vad_factory: Arc<dyn Fn() -> Box<dyn VadDetector> + Send + Sync> = match config.vad.as_str()
    {
        "energy" => Arc::new(move || Box::new(EnergyVad::new(threshold))),
        other => bail!("VAD={other} is not implemented yet; only 'energy' is available"),
    };

    let state = Arc::new(AppState {
        config: Arc::new(config.clone()),
        recognizer,
        vad_factory,
    });
    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", config.host, config.port))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(
        %addr,
        engine = %config.engine,
        vad = %config.vad,
        language = %config.language,
        "asr listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving HTTP")?;
    Ok(())
}

#[cfg(feature = "gigaam")]
fn load_gigaam() -> Result<Arc<dyn Recognizer>> {
    use orbita_asr::{GigaamConfig, GigaamRecognizer};

    let gigaam = GigaamConfig::from_env().map_err(anyhow::Error::msg)?;
    let started = std::time::Instant::now();
    let recognizer = GigaamRecognizer::load(&gigaam).map_err(anyhow::Error::msg)?;
    let loaded_ms = started.elapsed().as_millis();
    recognizer.warm_up().map_err(anyhow::Error::msg)?;
    tracing::info!(
        model = %gigaam.model,
        dir = %gigaam.model_dir,
        quantization = ?gigaam.quantization,
        providers = ?recognizer.providers(),
        threads = gigaam.threads,
        loaded_ms,
        warm_ms = started.elapsed().as_millis() - loaded_ms,
        "gigaam ready"
    );
    Ok(Arc::new(recognizer))
}

#[cfg(not(feature = "gigaam"))]
fn load_gigaam() -> Result<Arc<dyn Recognizer>> {
    bail!("ENGINE=gigaam needs a build with the `gigaam` feature")
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

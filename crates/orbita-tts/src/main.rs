use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use orbita_tts::{
    AppState, Config, FakeEngine, SynthesisEngine, audio::FfmpegEncoder, build_router,
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let mut args = std::env::args().skip(1);
    if let Some(command) = args.next()
        && command == "--bench"
    {
        let runs = args
            .next()
            .map(|runs| runs.parse::<usize>().context("--bench takes a run count"))
            .transpose()?
            .unwrap_or(5);
        return bench(runs);
    }

    let mut config = Config::from_env().map_err(anyhow::Error::msg)?;
    let engine: Box<dyn SynthesisEngine> = match config.engine.as_str() {
        "fake" => Box::new(FakeEngine::new(44_100, config.total_steps)),
        "vosk" => load_vosk(&mut config)?,
        other => bail!("ENGINE={other} is not supported; use vosk or fake"),
    };

    let encoder = Arc::new(FfmpegEncoder::new(config.gain_db));
    let state = AppState::new(engine, encoder, config.clone());
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
        voice = %config.default_voice,
        steps = config.total_steps,
        gain_db = config.gain_db,
        "tts listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving HTTP")?;
    Ok(())
}

#[cfg(feature = "vosk")]
fn bench(runs: usize) -> Result<()> {
    orbita_tts::vosk::bench::run(runs).map_err(anyhow::Error::msg)
}

#[cfg(not(feature = "vosk"))]
fn bench(_runs: usize) -> Result<()> {
    bail!("--bench needs a build with the `vosk` feature")
}

#[cfg(feature = "vosk")]
fn load_vosk(config: &mut Config) -> Result<Box<dyn SynthesisEngine>> {
    use orbita_tts::vosk::{VoskConfig, VoskEngine};

    let vosk = VoskConfig::from_env(&config.providers, config.silence_duration)
        .map_err(anyhow::Error::msg)?;
    if std::env::var_os("DEFAULT_VOICE").is_none() {
        config.default_voice = vosk.default_speaker.clone();
    }
    let started = std::time::Instant::now();
    let mut engine = VoskEngine::load(vosk.clone()).map_err(anyhow::Error::msg)?;
    let loaded_ms = started.elapsed().as_millis();
    engine.warm_up().map_err(anyhow::Error::msg)?;
    tracing::info!(
        dir = %vosk.model_dir,
        voice = %vosk.default_speaker,
        voices = engine.voices().len(),
        providers = ?vosk.provider_names(),
        threads = vosk.threads,
        loaded_ms,
        warm_ms = started.elapsed().as_millis() - loaded_ms,
        "vosk ready"
    );
    Ok(Box::new(engine))
}

#[cfg(not(feature = "vosk"))]
fn load_vosk(_config: &mut Config) -> Result<Box<dyn SynthesisEngine>> {
    bail!("ENGINE=vosk needs a build with the `vosk` feature")
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

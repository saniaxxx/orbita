//! `orbita-station-adapter`: quasar-compatible `/process` backed by OpenClaw.
//!
//! Environment:
//! - `HOST`/`PORT` — listen address (default `127.0.0.1:8081`);
//! - `OPENCLAW_URL` — Gateway base (default `http://127.0.0.1:18789`);
//! - `OPENCLAW_TOKEN` — Gateway bearer token (required);
//! - `OPENCLAW_MODEL` — agent target (default `openclaw/default`);
//! - `CORE_TIMEOUT_SECS` — per-request timeout (default 45);
//! - `HOUSEHOLD`, `DEFAULT_DUID`, `DEVICE_ROOMS` (`duid=room,duid=room`);
//! - scheduling (D-26): `OPENCLAW_BIN`, `OUTBOUND_BASE` (adapter URL the
//!   Gateway can reach), `OUTBOUND_KEY` (webhook secret), `QUASAR_API_URL`
//!   (station push), optional `TELEGRAM_CHAT_ID`. Without `OUTBOUND_KEY`
//!   scheduling is off and timers stay native.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use orbita_station_adapter::{
    AppState, Identity, OpenClawConfig, OpenClawCore, build_router,
    schedule::{OpenClawCliScheduler, QuasarNotifier, Scheduler},
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let host = orbita_core::env_or("HOST", "127.0.0.1");
    let port: u16 = orbita_core::env_parse("PORT", 8081).map_err(anyhow::Error::msg)?;
    let timeout: u64 =
        orbita_core::env_parse("CORE_TIMEOUT_SECS", 45).map_err(anyhow::Error::msg)?;
    let token = std::env::var("OPENCLAW_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .context("OPENCLAW_TOKEN is required")?;
    let config = OpenClawConfig {
        base_url: orbita_core::env_or("OPENCLAW_URL", "http://127.0.0.1:18789"),
        token,
        model: orbita_core::env_or("OPENCLAW_MODEL", "openclaw/default"),
        timeout: Duration::from_secs(timeout),
        max_tool_rounds: 2,
    };
    let core_name = format!("openclaw {}", config.base_url);
    let identity = Identity {
        household: orbita_core::env_or("HOUSEHOLD", "home"),
        default_duid: std::env::var("DEFAULT_DUID")
            .ok()
            .filter(|duid| !duid.is_empty()),
        rooms: parse_rooms(&orbita_core::env_or("DEVICE_ROOMS", "")),
        ..Default::default()
    };
    let non_empty = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let outbound_key = non_empty("OUTBOUND_KEY");
    let telegram_chat = non_empty("TELEGRAM_CHAT_ID");
    let scheduler: Option<Arc<dyn Scheduler>> = outbound_key.as_ref().map(|key| {
        Arc::new(OpenClawCliScheduler {
            bin: orbita_core::env_or("OPENCLAW_BIN", "openclaw"),
            outbound_base: orbita_core::env_or(
                "OUTBOUND_BASE",
                &format!("http://127.0.0.1:{port}"),
            ),
            outbound_key: key.clone(),
            timeout: Duration::from_secs(20),
        }) as Arc<dyn Scheduler>
    });
    let mut core = OpenClawCore::new(config)?;
    if let Some(scheduler) = &scheduler {
        core = core.with_scheduler(scheduler.clone(), telegram_chat.clone());
    }
    let mut state = AppState::new(Arc::new(core), identity, core_name);
    state.scheduler = scheduler;
    state.outbound_key = outbound_key;
    state.telegram_chat = telegram_chat;
    if let Some(url) = non_empty("QUASAR_API_URL") {
        state.notifier = Some(Arc::new(QuasarNotifier::new(url)?));
    }
    tracing::info!(
        scheduling = state.scheduler.is_some(),
        push = state.notifier.is_some(),
        telegram = state.telegram_chat.is_some(),
        "adapter configured"
    );
    let state = Arc::new(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address {host}:{port}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "orbita-station-adapter listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving HTTP")
}

fn parse_rooms(value: &str) -> Vec<(String, String)> {
    value
        .split(',')
        .filter_map(|pair| {
            let (duid, room) = pair.split_once('=')?;
            let (duid, room) = (duid.trim(), room.trim());
            (!duid.is_empty() && !room.is_empty()).then(|| (duid.to_owned(), room.to_owned()))
        })
        .collect()
}

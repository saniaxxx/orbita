//! Synthetic turns so the dashboard is useful before live telemetry exists.

use std::sync::Arc;

use serde_json::json;
use tokio::time::{Duration, sleep};

use crate::state::{DashboardState, TraceEvent};

const QUESTION: &str = "какая сейчас погода в москве";
const PARTIAL: &str = "какая сейчас погода…";
const ANSWER: &str = "Сейчас в Москве ясно, плюс пять градусов. Ветер слабый.";

pub fn spawn(state: Arc<DashboardState>) {
    tokio::spawn(async move {
        loop {
            run_turn(&state).await;
            sleep(Duration::from_secs(8)).await;
        }
    });
}

async fn run_turn(state: &DashboardState) {
    let turn = state.next_turn_id();
    emit(state, &turn, "station", "turn.start", None, json!({}));
    sleep(Duration::from_millis(90)).await;

    emit(
        state,
        &turn,
        "proxy",
        "proxy.tls",
        Some(18),
        json!({"sni": "uniproxy.alice.yandex.net"}),
    );
    sleep(Duration::from_millis(90)).await;

    emit(
        state,
        &turn,
        "quasar",
        "quasar.session",
        Some(120),
        json!({"path": "/uni.ws"}),
    );
    sleep(Duration::from_millis(120)).await;

    emit(
        state,
        &turn,
        "asr",
        "asr.partial",
        Some(180),
        json!({"text": PARTIAL}),
    );
    sleep(Duration::from_millis(180)).await;

    emit(
        state,
        &turn,
        "asr",
        "asr.final",
        Some(220),
        json!({"text": QUESTION}),
    );
    sleep(Duration::from_millis(180)).await;

    emit(
        state,
        &turn,
        "processor",
        "processor.request",
        Some(210),
        json!({"text": QUESTION}),
    );
    sleep(Duration::from_millis(210)).await;

    emit(
        state,
        &turn,
        "llm",
        "llm.ttft",
        Some(360),
        json!({"model": "qwen3.5-9b", "ttft_ms": 360}),
    );
    sleep(Duration::from_millis(360)).await;

    emit(
        state,
        &turn,
        "llm",
        "llm.done",
        Some(1122),
        json!({"pass": 1, "llm_ms": 1122, "answer": ""}),
    );
    sleep(Duration::from_millis(120)).await;

    emit(
        state,
        &turn,
        "mcp",
        "mcp.request",
        Some(90),
        json!({"tool": "search_search", "query": "погода в Москве"}),
    );
    sleep(Duration::from_millis(90)).await;

    emit(
        state,
        &turn,
        "searxng",
        "search.results",
        Some(320),
        json!({"engine": "duckduckgo web", "count": 4}),
    );
    sleep(Duration::from_millis(320)).await;

    emit(
        state,
        &turn,
        "mcp",
        "mcp.response",
        Some(80),
        json!({"chars": 1072}),
    );
    sleep(Duration::from_millis(80)).await;

    emit(
        state,
        &turn,
        "llm",
        "llm.done",
        Some(1122),
        json!({"answer": ANSWER, "pass": 2, "llm_ms": 1122}),
    );
    sleep(Duration::from_millis(500)).await;

    emit(
        state,
        &turn,
        "tts",
        "tts.chunk",
        Some(440),
        json!({"index": 1, "total": 2, "bytes": 20_004}),
    );
    sleep(Duration::from_millis(440)).await;

    emit(
        state,
        &turn,
        "tts",
        "tts.chunk",
        Some(300),
        json!({"index": 2, "total": 2, "bytes": 38_867}),
    );
    sleep(Duration::from_millis(300)).await;

    emit(
        state,
        &turn,
        "station",
        "station.first_audio",
        Some(160),
        json!({"bytes": 58_871, "first_audio_ms": 1630}),
    );
    sleep(Duration::from_millis(160)).await;

    emit(
        state,
        &turn,
        "station",
        "station.play",
        None,
        json!({"finished": true}),
    );
    emit(
        state,
        &turn,
        "dashboard",
        "metrics",
        None,
        json!({"vram_gib": 7.3, "queue": 0}),
    );
}

fn emit(
    state: &DashboardState,
    turn: &str,
    service: &str,
    stage: &str,
    duration_ms: Option<u64>,
    data: serde_json::Value,
) {
    let mut event = TraceEvent::new(turn, service, stage).with_data(data);
    event.duration_ms = duration_ms;
    state.emit(event);
}

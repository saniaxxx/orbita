//! In-memory ring buffer of trace events plus live subscribers.

use std::{
    collections::VecDeque,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

const MAX_EVENTS: usize = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub ts_ms: u64,
    pub turn_id: String,
    pub service: String,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub data: Value,
}

impl TraceEvent {
    pub fn new(
        turn_id: impl Into<String>,
        service: impl Into<String>,
        stage: impl Into<String>,
    ) -> Self {
        Self {
            ts_ms: now_ms(),
            turn_id: turn_id.into(),
            service: service.into(),
            stage: stage.into(),
            duration_ms: None,
            data: Value::Null,
        }
    }

    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    pub first_audio_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub llm_ms: Option<u64>,
    pub tts_ms: Option<u64>,
    pub vram_gib: f32,
    pub queue: u32,
    pub errors: u32,
}

pub struct DashboardState {
    events: RwLock<VecDeque<TraceEvent>>,
    metrics: RwLock<Metrics>,
    tx: broadcast::Sender<TraceEvent>,
    turn_counter: AtomicU64,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl DashboardState {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            events: RwLock::new(VecDeque::with_capacity(MAX_EVENTS)),
            metrics: RwLock::new(Metrics {
                vram_gib: 7.3,
                ..Metrics::default()
            }),
            tx,
            turn_counter: AtomicU64::new(1283),
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn next_turn_id(&self) -> String {
        format!(
            "turn-{}",
            self.turn_counter.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    pub fn emit(&self, event: TraceEvent) -> TraceEvent {
        self.update_metrics(&event);
        {
            let mut events = self
                .events
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            events.push_back(event.clone());
            while events.len() > MAX_EVENTS {
                events.pop_front();
            }
        }
        let _ = self.tx.send(event.clone());
        event
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TraceEvent> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> Value {
        let events: Vec<TraceEvent> = self
            .events
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect();
        let metrics = self
            .metrics
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        json!({"events": events, "metrics": metrics})
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn update_metrics(&self, event: &TraceEvent) {
        let mut metrics = self
            .metrics
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match event.stage.as_str() {
            "llm.ttft" => {
                metrics.ttft_ms = event
                    .data
                    .get("ttft_ms")
                    .and_then(Value::as_u64)
                    .or(event.duration_ms);
            }
            "llm.done" => {
                metrics.llm_ms = event
                    .data
                    .get("llm_ms")
                    .and_then(Value::as_u64)
                    .or(event.duration_ms);
            }
            "station.first_audio" => {
                metrics.first_audio_ms = event
                    .data
                    .get("first_audio_ms")
                    .and_then(Value::as_u64)
                    .or(event.duration_ms);
            }
            "tts.chunk" => metrics.tts_ms = event.duration_ms,
            "metrics" => {
                if let Some(value) = event.data.get("vram_gib").and_then(Value::as_f64) {
                    metrics.vram_gib = value as f32;
                }
                if let Some(value) = event.data.get("queue").and_then(Value::as_u64) {
                    metrics.queue = value as u32;
                }
            }
            "error" => metrics.errors = metrics.errors.saturating_add(1),
            _ => {}
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

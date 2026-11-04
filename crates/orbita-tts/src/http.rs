//! OpenAI-compatible speech endpoint, matching `services/tts-gpu/server.py`.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::{
    audio::{AudioEncoder, Format, wav_bytes},
    cache::LruCache,
    config::Config,
    engine::{EngineError, SynthesisEngine},
    text::LANGUAGE,
};

const MAX_BODY_BYTES: usize = 1_000_000;

pub struct AppState {
    pub engine: Mutex<Box<dyn SynthesisEngine>>,
    pub encoder: Arc<dyn AudioEncoder>,
    pub cache: Mutex<LruCache>,
    pub config: Config,
}

impl AppState {
    pub fn new(
        engine: Box<dyn SynthesisEngine>,
        encoder: Arc<dyn AudioEncoder>,
        config: Config,
    ) -> Arc<Self> {
        let capacity = config.cache_size;
        Arc::new(Self {
            engine: Mutex::new(engine),
            encoder,
            cache: Mutex::new(LruCache::new(capacity)),
            config,
        })
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/v1/audio/speech", post(speech))
        .route("/audio/speech", post(speech))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let engine = lock(&state.engine);
    Json(json!({
        "status": "ok",
        "providers": engine.providers(),
        "steps": engine.steps(),
        "sample_rate": engine.sample_rate(),
        "voice": state.config.default_voice,
        "cached": lock(&state.cache).len(),
    }))
}

async fn speech(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if body.len() > MAX_BODY_BYTES {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
            "invalid_request_error",
        );
    }
    if body.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "empty request body",
            "invalid_request_error",
        );
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("invalid JSON: {error}"),
                "invalid_request_error",
            );
        }
    };

    let request = match validate(&payload, &state.config) {
        Ok(request) => request,
        Err(error) => {
            return error_response(StatusCode::BAD_REQUEST, error, "invalid_request_error");
        }
    };

    let key = cache_key(&request);
    if let Some(cached) = lock(&state.cache).get(&key) {
        tracing::info!(
            voice = %request.voice,
            format = %request.format,
            chars = request.input.chars().count(),
            speed = request.speed,
            bytes = cached.len(),
            cache = "hit",
            "speech"
        );
        return audio_response(cached, 0.0, true, request.format);
    }

    let render_state = Arc::clone(&state);
    let render_request = request.clone();
    let rendered =
        match tokio::task::spawn_blocking(move || render(&render_state, &render_request)).await {
            Ok(Ok(rendered)) => rendered,
            Ok(Err(RenderError::UnknownVoice(voice))) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    format!("unknown voice: {voice}"),
                    "invalid_request_error",
                );
            }
            Ok(Err(RenderError::Failed(error))) => {
                tracing::error!(%error, "synthesis failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Speech synthesis failed",
                    "server_error",
                );
            }
            Err(error) => {
                tracing::error!(%error, "synthesis task failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Speech synthesis failed",
                    "server_error",
                );
            }
        };

    lock(&state.cache).put(key, rendered.body.clone());
    tracing::info!(
        voice = %request.voice,
        format = %request.format,
        chars = request.input.chars().count(),
        speed = request.speed,
        bytes = rendered.body.len(),
        synth_ms = rendered.synth_ms,
        cache = "miss",
        "speech"
    );
    audio_response(rendered.body, rendered.synth_ms, false, request.format)
}

#[derive(Debug, Clone)]
struct SpeechRequest {
    input: String,
    voice: String,
    format: Format,
    speed: f32,
}

struct Rendered {
    body: Vec<u8>,
    synth_ms: f64,
}

enum RenderError {
    UnknownVoice(String),
    Failed(String),
}

fn render(state: &AppState, request: &SpeechRequest) -> Result<Rendered, RenderError> {
    let started = Instant::now();
    let (samples, sample_rate) = {
        let mut engine = lock(&state.engine);
        let sample_rate = engine.sample_rate();
        let steps = engine.steps();
        let samples = engine
            .synthesize(&request.input, &request.voice, request.speed, steps)
            .map_err(|error| match error {
                EngineError::UnknownVoice(voice) => RenderError::UnknownVoice(voice),
                EngineError::Failed(message) => RenderError::Failed(message),
            })?;
        (samples, sample_rate)
    };

    let wav = wav_bytes(&samples, sample_rate);
    let body = state
        .encoder
        .encode(&wav, request.format)
        .map_err(RenderError::Failed)?;
    let synth_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(Rendered { body, synth_ms })
}

fn validate(payload: &Value, config: &Config) -> Result<SpeechRequest, String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "request body must be a JSON object".to_owned())?;

    let input = object
        .get("input")
        .and_then(Value::as_str)
        .ok_or_else(|| "'input' must be a non-empty string".to_owned())?;
    if input.trim().is_empty() {
        return Err("'input' must be a non-empty string".to_owned());
    }
    if input.chars().count() > config.max_text_length {
        return Err(format!(
            "'input' exceeds {} characters",
            config.max_text_length
        ));
    }

    let voice = match object.get("voice") {
        None => config.default_voice.clone(),
        Some(value) => {
            let voice = value
                .as_str()
                .ok_or_else(|| "'voice' must be a non-empty string".to_owned())?;
            if voice.trim().is_empty() {
                return Err("'voice' must be a non-empty string".to_owned());
            }
            voice.trim().to_owned()
        }
    };

    let format = match object.get("response_format") {
        None => Format::Opus,
        Some(value) => value
            .as_str()
            .ok_or_else(|| "'response_format' must be a string".to_owned())?
            .parse::<Format>()?,
    };

    let speed = match object.get("speed") {
        None => config.default_speed,
        Some(value) => {
            if value.is_boolean() {
                return Err("'speed' must be a number".to_owned());
            }
            let speed = value
                .as_f64()
                .ok_or_else(|| "'speed' must be a number".to_owned())?;
            if !speed.is_finite() {
                return Err("'speed' must be a number".to_owned());
            }
            speed as f32
        }
    };
    if !(0.25..=4.0).contains(&speed) {
        return Err("'speed' must be between 0.25 and 4".to_owned());
    }

    if let Some(language) = object.get("language") {
        let language = language.as_str().unwrap_or("");
        if language.to_lowercase() != LANGUAGE {
            return Err("Only Russian (ru) is supported".to_owned());
        }
    }

    if let Some(model) = object.get("model")
        && !model.is_string()
    {
        return Err("'model' must be a string".to_owned());
    }

    Ok(SpeechRequest {
        input: input.to_owned(),
        voice,
        format,
        speed,
    })
}

fn cache_key(request: &SpeechRequest) -> String {
    serde_json::to_string(&json!([
        request.input,
        request.voice,
        LANGUAGE,
        request.format.as_str(),
        request.speed,
    ]))
    .expect("serialising the cache key cannot fail")
}

fn audio_response(body: Vec<u8>, synth_ms: f64, cached: bool, format: Format) -> Response {
    let content_length = body.len().to_string();
    let synth_ms = format!("{synth_ms:.0}");
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length).expect("content length is ASCII"),
    );
    headers.insert(
        HeaderName::from_static("x-synth-ms"),
        HeaderValue::from_str(&synth_ms).expect("synth time is ASCII"),
    );
    headers.insert(
        HeaderName::from_static("x-cache"),
        HeaderValue::from_static(if cached { "hit" } else { "miss" }),
    );
    response
}

fn error_response(status: StatusCode, message: impl Into<String>, kind: &str) -> Response {
    (
        status,
        Json(json!({"error": {"message": message.into(), "type": kind}})),
    )
        .into_response()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

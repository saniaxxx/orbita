//! HTTP contract, mirroring `services/tts-gpu/tests/test_http.py`.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use orbita_tts::{
    AppState, Config, FakeEngine, audio::FakeEncoder, build_router, engine::SynthesisEngine,
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn config() -> Config {
    Config {
        host: "127.0.0.1".to_owned(),
        port: 0,
        engine: "fake".to_owned(),
        assets_path: String::new(),
        default_voice: "F4".to_owned(),
        default_speed: 1.05,
        max_text_length: 12_000,
        cache_size: 2,
        total_steps: 8,
        silence_duration: 0.3,
        gain_db: 9.0,
        providers: "auto".to_owned(),
    }
}

fn app() -> Router {
    let engine: Box<dyn SynthesisEngine> = Box::new(FakeEngine::new(44_100, 8));
    let state = AppState::new(engine, Arc::new(FakeEncoder), config());
    build_router(state)
}

async fn post(router: &Router, path: &str, body: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

async fn get(router: &Router, path: &str) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, body)
}

#[tokio::test]
async fn speech_returns_audio_and_cache_headers() {
    let router = app();
    let body = json!({
        "model": "",
        "input": "Привет, станция!",
        "voice": "F4",
        "response_format": "opus",
        "speed": 1,
    })
    .to_string();

    let (status, headers, response) = post(&router, "/audio/speech", &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, b"AUDIO:opus");
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("audio/ogg; codecs=opus")
    );
    assert!(headers.contains_key("x-synth-ms"));
    assert_eq!(
        headers.get("x-cache").and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let (status, headers, response) = post(&router, "/audio/speech", &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, b"AUDIO:opus");
    assert_eq!(
        headers.get("x-cache").and_then(|value| value.to_str().ok()),
        Some("hit")
    );
}

#[tokio::test]
async fn accepts_the_v1_path_and_defaults() {
    let router = app();
    let (status, _, body) = post(&router, "/v1/audio/speech", r#"{"input":"тест"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"AUDIO:opus");
}

#[tokio::test]
async fn rejects_invalid_requests() {
    let router = app();

    let (status, _, body) = post(&router, "/audio/speech", r#"{"input":"   "}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["type"], "invalid_request_error");

    let (status, _, _) = post(
        &router,
        "/audio/speech",
        r#"{"input":"тест","response_format":"ogg"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = post(&router, "/audio/speech", r#"{"input":"тест","speed":9}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = post(
        &router,
        "/audio/speech",
        r#"{"input":"тест","language":"en"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = post(&router, "/audio/speech", r#"{"input":"тест","model":123}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = post(&router, "/audio/speech", r#"{"input":"тест","voice":"XX"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = post(&router, "/audio/speech", "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wav_is_passed_to_the_encoder() {
    let router = app();
    let (status, headers, body) = post(
        &router,
        "/audio/speech",
        r#"{"input":"тест","response_format":"wav"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"AUDIO:wav");
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("audio/wav")
    );
}

#[tokio::test]
async fn health_reports_the_engine() {
    let router = app();
    let (status, body) = get(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    let health: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["steps"], 8);
    assert_eq!(health["sample_rate"], 44_100);
    assert_eq!(health["voice"], "F4");
    assert_eq!(health["providers"]["vocoder"][0], "CPUExecutionProvider");
}

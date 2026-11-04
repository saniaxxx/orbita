//! Dashboard API: snapshot, ingestion and the static UI.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use orbita_dashboard::{DashboardState, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    let request_body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(request_body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn app() -> Router {
    build_router(Arc::new(DashboardState::new()))
}

#[tokio::test]
async fn serves_state_and_accepts_trace_events() {
    let router = app();

    let (status, state) = call(&router, "GET", "/api/state", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["events"], json!([]));

    let event = json!({
        "ts_ms": 1_700_000_000_000_u64,
        "turn_id": "turn-1",
        "service": "llm",
        "stage": "llm.ttft",
        "duration_ms": 360,
        "data": {"ttft_ms": 360}
    });
    let (status, result) = call(&router, "POST", "/api/events", Some(event)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["accepted"], 1);

    let (status, state) = call(&router, "GET", "/api/state", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["events"].as_array().unwrap().len(), 1);
    assert_eq!(state["metrics"]["ttft_ms"], 360);
}

#[tokio::test]
async fn serves_health_and_the_static_ui() {
    let router = app();

    let (status, body) = call(&router, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let request = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("ORBITA"));
    assert!(html.contains("/app.js"));
}

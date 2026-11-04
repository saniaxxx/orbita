//! Contract tests for the non-uniproxy HTTP routers.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use orbita_quasar::{AppState, ConnectionRegistry, SqliteStorage, routers};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> (Router, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = Arc::new(AppState {
        storage: Arc::clone(&storage),
        connections: Arc::new(ConnectionRegistry::new()),
        glagol_jwt_key: "test-glagol-key".to_owned(),
    });
    (routers::build_router(state), storage)
}

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Body,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

fn form(body: &str) -> Body {
    Body::from(body.to_owned())
}

fn json_body(value: Value) -> Body {
    Body::from(value.to_string())
}

#[tokio::test]
async fn serves_probe_and_analytics_routes() {
    let (router, _) = app();

    let (status, headers, body) = call(
        &router,
        "GET",
        "/connectivitycheck.gstatic.com/generate_204",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_eq!(
        headers.get("cross-origin-resource-policy").unwrap(),
        "cross-origin"
    );
    assert!(headers.get(header::DATE).is_some());

    let (status, _, _) = call(
        &router,
        "GET",
        "/connectivitycheck.gstatic.com/missing",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, headers, _) = call(
        &router,
        "GET",
        "/scbh.yandex.net/generate_204",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.get("x-start-time").is_some());

    let (status, _, body) = call(
        &router,
        "GET",
        "/startup.mobile.yandex.net/analytics/startup",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let startup: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(startup["uuid"]["value"].as_str().unwrap().len(), 32);
    assert_eq!(
        startup["query_hosts"]["list"]["startup"]["urls"][0],
        "https://startup.mobile.yandex.net"
    );

    let (status, _, body) = call(
        &router,
        "POST",
        "/report.appmetrica.yandex.net/report",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["status"],
        "accepted"
    );
}

#[tokio::test]
async fn serves_oauth_tokens() {
    let (router, _) = app();

    for path in [
        "/oauth.yandex.ru/token",
        "/mobileproxy.passport.yandex.net/1/token",
    ] {
        let (status, _, response) = call(
            &router,
            "POST",
            path,
            Some("application/x-www-form-urlencoded"),
            form("grant_type=authorization_code&code=abc&device_id=duid-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let token: Value = serde_json::from_slice(&response).unwrap();
        assert!(token["access_token"].as_str().unwrap().starts_with("VAT."));
        assert!(token["refresh_token"].as_str().unwrap().starts_with("VRT."));
        assert_eq!(token["expires_in"], 315_360_000);
    }

    let (status, _, response) = call(
        &router,
        "POST",
        "/oauth.yandex.ru/token",
        Some("application/x-www-form-urlencoded"),
        form("grant_type=x-token&access_token=TOKEN"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(token["access_token"], "TOKEN");
    assert_eq!(token["uid"], 10_146_228);

    let (status, _, _) = call(
        &router,
        "POST",
        "/oauth.yandex.ru/token",
        Some("application/x-www-form-urlencoded"),
        form("grant_type=other"),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _, response) = call(
        &router,
        "GET",
        "/mobileproxy.passport.yandex.net/1/bundle/account/short_info",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let info: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(info["display_login"], "YandexStationOwnerNo146");
    assert!(info["x_token_issued_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn serves_clck_and_rpc() {
    let (router, _) = app();

    let (status, _, response) = call(
        &router,
        "POST",
        "/clck.yandex.net/quasar_metrics/write/json/batch",
        Some("application/json"),
        json_body(json!([{}, {}])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let results: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 2);
    assert_eq!(results[0]["ResponseStatus"], "OK");

    let (status, _, _) = call(
        &router,
        "POST",
        "/clck.yandex.net/quasar_metrics/write/json/batch",
        Some("application/json"),
        json_body(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _, response) = call(
        &router,
        "POST",
        "/clck.yandex.net/quasar_metrics/write/batch",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, vec![0x0A, 0x02, 0x4F, 0x4B]);

    let (status, headers, response) = call(
        &router,
        "POST",
        "/rpc.alice.yandex.net/gproxy/push_events_batch",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/x-protobuf"
    );
    assert!(response.is_empty());
}

#[tokio::test]
async fn serves_quasar_device_endpoints() {
    let (router, storage) = app();

    let (status, _, response) = call(
        &router,
        "POST",
        "/quasar.yandex.net/update_device_state?device_id=duid-1&platform=yandexmicro",
        Some("application/json"),
        json_body(json!({"wifi": "home"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["status"],
        "ok"
    );
    assert_eq!(storage.list().unwrap()[0].network_info["wifi"], "home");

    let (status, _, response) = call(
        &router,
        "POST",
        "/quasar.yandex.net/register?platform=yandexmicro&firmware_version=1",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["hasCriticalUpdate"],
        false
    );

    let (status, _, response) = call(
        &router,
        "GET",
        "/quasar.yandex.net/check_updates?platform=yandexmicro&version=1",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["hasUpdate"],
        false
    );
}

#[tokio::test]
async fn signs_and_verifies_glagol_tokens() {
    let (router, _) = app();

    let (status, _, response) = call(
        &router,
        "GET",
        "/quasar.yandex.net/glagol/token?device_id=duid-1&platform=yandexmicro",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = serde_json::from_slice::<Value>(&response).unwrap()["token"]
        .as_str()
        .expect("token")
        .to_owned();

    let (status, _, response) = call(
        &router,
        "POST",
        "/quasar.yandex.net/glagol/check_token",
        None,
        Body::from(token.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["valid"],
        true
    );

    let (status, _, response) = call(
        &router,
        "POST",
        "/quasar.yandex.net/glagol/v2.0/check_token",
        None,
        Body::from(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["owner"],
        true
    );

    let (status, _, response) = call(
        &router,
        "POST",
        "/quasar.yandex.net/glagol/check_token",
        None,
        Body::from("not-a-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["valid"],
        false
    );
}

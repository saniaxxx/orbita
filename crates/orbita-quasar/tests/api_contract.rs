//! Management API contract: devices, config and push endpoints.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use orbita_quasar::{AppState, ConnectionRegistry, DeviceConnection, SqliteStorage, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct Recording {
    tts: Mutex<Vec<String>>,
    raw: Mutex<Vec<Value>>,
    directives: Mutex<Vec<Vec<Value>>>,
}

#[async_trait]
impl DeviceConnection for Recording {
    async fn push_tts(&self, text: &str) -> Result<(), String> {
        self.tts.lock().unwrap().push(text.to_owned());
        Ok(())
    }

    async fn push_raw(&self, directive: &Value) -> Result<(), String> {
        self.raw.lock().unwrap().push(directive.clone());
        Ok(())
    }

    async fn push_directives(&self, directives: &[Value]) -> Result<(), String> {
        self.directives.lock().unwrap().push(directives.to_vec());
        Ok(())
    }

    fn supported_directives(&self) -> Value {
        json!({
            "set_lights": {
                "arguments": {"type": "object"},
                "description": "Turn lights on or off"
            }
        })
    }
}

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

fn app() -> (Router, Arc<SqliteStorage>, Arc<ConnectionRegistry>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let connections = Arc::new(ConnectionRegistry::new());
    let router = build_router(Arc::new(AppState {
        storage: Arc::clone(&storage),
        connections: Arc::clone(&connections),
        glagol_jwt_key: "test-glagol-key".to_owned(),
    }));
    (router, storage, connections)
}

#[tokio::test]
async fn serves_devices_config_and_push() {
    let (router, storage, connections) = app();

    let (status, devices) = call(&router, "GET", "/devices", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices, json!([]));

    storage.get_or_create("duid-1", "yandexmicro").unwrap();
    let (status, devices) = call(&router, "GET", "/devices", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices[0]["duid"], "duid-1");
    assert_eq!(devices[0]["connected"], false);
    assert_eq!(
        devices[0]["quasarConfig"]["timezone"]["name"],
        "Europe/Moscow"
    );

    let (status, updated) = call(
        &router,
        "PATCH",
        "/devices/duid-1",
        Some(json!({
            "name": "Кухня",
            "quasarConfig": {"timezone": {"offsetSeconds": 7200}}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["name"], "Кухня");
    assert_eq!(updated["quasarConfig"]["timezone"]["offsetSeconds"], 7200);
    assert_eq!(updated["quasarConfig"]["timezone"]["name"], "Europe/Moscow");

    let recording = Arc::new(Recording::default());
    connections.register("duid-1", recording.clone());

    let (status, devices) = call(&router, "GET", "/devices", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices[0]["connected"], true);

    let (status, body) = call(
        &router,
        "POST",
        "/devices/duid-1/push/text",
        Some(json!({"eventText": "Привет"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({}));
    assert_eq!(recording.tts.lock().unwrap().as_slice(), ["Привет"]);

    let (status, _) = call(
        &router,
        "POST",
        "/devices/duid-1/push/raw",
        Some(json!({"Name": "sound_set_level", "Type": "client_action"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recording.raw.lock().unwrap().len(), 1);

    let (status, _) = call(
        &router,
        "POST",
        "/devices/duid-1/push",
        Some(json!([{"name": "set_lights", "arguments": {"state": 1}}])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recording.directives.lock().unwrap().len(), 1);

    let (status, directives) = call(&router, "GET", "/devices/duid-1/directives", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        directives["set_lights"]["description"],
        "Turn lights on or off"
    );

    let (status, error) = call(&router, "GET", "/devices/missing/directives", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "DEVICE_NOT_CONNECTED");

    storage.get_or_create("duid-2", "yandexmicro").unwrap();
    let second = Arc::new(Recording::default());
    connections.register("duid-2", second.clone());

    let (status, _) = call(
        &router,
        "POST",
        "/devices/all/push/text",
        Some(json!({"eventText": "Всем"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recording.tts.lock().unwrap().as_slice(), ["Привет", "Всем"]);
    assert_eq!(second.tts.lock().unwrap().as_slice(), ["Всем"]);
}

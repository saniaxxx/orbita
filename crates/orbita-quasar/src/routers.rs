//! HTTP routers that the station calls besides uniproxy.
//!
//! Ports the small upstream routers verbatim: OAuth/token, metrics,
//! connectivity probes, startup analytics and update stubs. The larger
//! `quasar.yandex.net` sync/glagol surface will follow with recorded requests.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::api::AppState;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .nest("/clck.yandex.net", clck_router())
        .nest("/connectivitycheck.gstatic.com", probe_router(true))
        .nest("/play.googleapis.com", probe_router(true))
        .nest("/www.google.com", probe_router(true))
        .nest("/mobileproxy.passport.yandex.net", passport_router())
        .nest("/oauth.yandex.ru", oauth_router())
        .nest("/report.appmetrica.yandex.net", report_router())
        .nest("/rpc.alice.yandex.net", rpc_router())
        .nest("/scbh.yandex.net", scbh_router())
        .nest("/startup.mobile.yandex.net", startup_router())
        .nest("/quasar.yandex.net", quasar_router())
        .with_state(state)
}

fn clck_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/quasar_metrics/write/batch", post(clck_ok_protobuf))
        .route("/quasar_metrics/write/json/batch", post(clck_ok_json))
        .route("/smart_display_errors/write/json/batch", post(clck_ok_json))
        .route("/quasar/write/json/batch", post(clck_ok_json))
}

async fn clck_ok_protobuf() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(vec![0x0A, 0x02, 0x4F, 0x4B]))
        .expect("valid response")
}

async fn clck_ok_json(Json(body): Json<Value>) -> Response {
    let Value::Array(items) = body else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let responses = items
        .iter()
        .map(|_| json!({"ResponseStatus": "OK"}))
        .collect::<Vec<_>>();
    Json(responses).into_response()
}

fn probe_router(include_date: bool) -> Router<Arc<AppState>> {
    Router::new()
        .route("/generate_204", get(move || probe(include_date)))
        .fallback(not_found)
}

async fn probe(include_date: bool) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Cross-Origin-Resource-Policy", "cross-origin");
    if include_date {
        builder = builder.header(header::DATE, http_date());
    }
    builder.body(Body::empty()).expect("valid response")
}

fn scbh_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/generate_204", get(scbh_generate_204))
        .fallback(not_found)
}

async fn scbh_generate_204() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(
            "X-Start-Time",
            (Utc::now().timestamp_millis() * 1000).to_string(),
        )
        .body(Body::empty())
        .expect("valid response")
}

fn report_router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/report",
        post(|| async { Json(json!({"status": "accepted"})) }),
    )
}

fn rpc_router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/gproxy/push_events_batch",
        post(|| async {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .body(Body::empty())
                .expect("valid response")
        }),
    )
}

fn passport_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/1/token", post(passport_token))
        .route("/1/bundle/account/short_info", get(short_info))
}

async fn passport_token(axum::Form(form): axum::Form<HashMap<String, String>>) -> Response {
    oauth_response(&form)
}

async fn oauth_token(axum::Form(form): axum::Form<HashMap<String, String>>) -> Response {
    oauth_response(&form)
}

fn oauth_router() -> Router<Arc<AppState>> {
    Router::new().route("/token", post(oauth_token))
}

fn oauth_response(form: &HashMap<String, String>) -> Response {
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let code = form.get("code").map(String::as_str).unwrap_or("");
            let device_id = form.get("device_id").map(String::as_str).unwrap_or("");
            Json(json!({
                "access_token": format!("VAT.{}", hash("access", code, device_id)),
                "expires_in": 365 * 10 * 24 * 60 * 60,
                "refresh_token": format!("VRT.{}", hash("refresh", code, device_id)),
                "token_type": "bearer"
            }))
            .into_response()
        }
        Some("x-token") => Json(json!({
            "access_token": form.get("access_token").map(String::as_str).unwrap_or(""),
            "expires_in": 365 * 10 * 24 * 60 * 60,
            "token_type": "bearer",
            "uid": 10_146_228
        }))
        .into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn hash(prefix: &str, code: &str, device_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(prefix.as_bytes());
    digest.update(code.as_bytes());
    digest.update(device_id.as_bytes());
    hex::encode(digest.finalize())
}

async fn short_info() -> Json<Value> {
    let issued_at = Utc::now().timestamp();
    Json(json!({
        "avatar_url": "https://avatars.mds.yandex.net/get-yapic/53031/0s-5/islands-300",
        "birthday": "1984-01-01",
        "display_login": "YandexStationOwnerNo146",
        "display_name": "YandexStationOwnerNo146",
        "filter_params": {"subtype": ["portal"]},
        "firstname": "Owner",
        "gender": "m",
        "has_cards": true,
        "has_family": true,
        "has_password": true,
        "has_plus": true,
        "has_telemost": true,
        "have_webauthn_credential": true,
        "is_browser_account": true,
        "is_rfc_2fa_enabled": true,
        "lastname": "No146",
        "location_id": 0,
        "machine_readable_login": "YandexStationOwnerNo146",
        "master_members": [{
            "avatar_url": "https://avatars.mds.yandex.net/get-yapic/53031/0s-5/islands-300",
            "display_login": "YandexStationOwnerNo146",
            "has_cards": true,
            "has_family": true,
            "has_plus": true,
            "has_telemost": true,
            "is_browser_account": true,
            "plus_card": true,
            "primary_alias_type": 1,
            "public_name": "Owner No146",
            "secure_phone_number": "+78005553535",
            "uid": 10_146_228
        }],
        "master_uid": 1_234_567_228,
        "native_default_email": "yandexowner@yandex.ru",
        "normalized_display_login": "YandexStationOwnerNo146",
        "partitions": ["default"],
        "plus_card": true,
        "primary_alias_type": 1,
        "public_id": "oj4bx3cwbowdyz4eb5lt1v0z7v",
        "public_name": "Owner No146",
        "secure_phone_number": "+78005553535",
        "status": "ok",
        "uid": 10_146_228,
        "x_token_client_id": "ef0e663c4b4d38debd41f76ed511e73f",
        "x_token_issued_at": issued_at,
        "xtoken_has_muid": true
    }))
}

fn startup_router() -> Router<Arc<AppState>> {
    Router::new().route("/analytics/startup", get(startup))
}

async fn startup() -> Json<Value> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let hash = format!(
        "{:010}",
        uuid.as_bytes().iter().fold(0_u64, |acc, byte| {
            acc.wrapping_mul(31).wrapping_add(*byte as u64)
        }) % 10_000_000_000
    );
    Json(json!({
        "device_id": {"hash": hash},
        "query_hosts": {
            "list": {
                "apple_tracking": {"urls": ["https://tracking.appmetrica.yandex.net"]},
                "check_updates": {"urls": ["https://analytics.mobile.yandex.net"]},
                "diagnostic": {"urls": ["https://report.appmetrica.yandex.net"]},
                "location": {"urls": ["https://rosenberg.appmetrica.yandex.net"]},
                "mediascope": {"urls": ["https://zen.report.appmetrica.yandex.net"]},
                "redirect": {"urls": ["https://redirect.appmetrica.yandex.com"]},
                "report": {"urls": ["https://report.appmetrica.yandex.net"]},
                "ssl_pinning": {"urls": ["https://certificate.mobile.yandex.net"]},
                "startup": {"urls": ["https://startup.mobile.yandex.net"]}
            }
        },
        "uuid": {"value": uuid}
    }))
}

fn quasar_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/check_updates", get(check_updates))
        .route("/register", post(register))
        .route("/update_device_state", post(update_device_state))
        .route(
            "/push_subscribe",
            post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/tv/api/v3/updates",
            post(|| async { Json(json!({"bundles": [], "updates": []})) }),
        )
        .route(
            "/tv/api/v4/updates",
            post(|| async { Json(json!({"updates": []})) }),
        )
        .route("/tv/api/v3/ota", get(|| async { Json(json!({})) }))
        .route("/glagol/token", get(glagol_token))
        .route("/glagol/check_token", post(glagol_check))
        .route("/glagol/v2.0/check_token", post(glagol_check_v2))
}

#[derive(Debug, Serialize, Deserialize)]
struct GlagolClaims {
    plt: String,
    sub: String,
    iss: String,
    aud: String,
    iat: usize,
    exp: usize,
}

async fn glagol_token(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(duid) = query.get("device_id") else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = query
        .get("platform")
        .map(String::as_str)
        .unwrap_or("unknown");
    let now = Utc::now().timestamp().max(0) as usize;
    let claims = GlagolClaims {
        plt: platform.to_owned(),
        sub: duid.clone(),
        iss: "quasar-backend".to_owned(),
        aud: "glagol".to_owned(),
        iat: now,
        exp: now + 86_400,
    };
    match encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(state.glagol_jwt_key.as_bytes()),
    ) {
        Ok(token) => Json(json!({"status": "ok", "token": token})).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to sign glagol token");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn glagol_check(State(state): State<Arc<AppState>>, body: Bytes) -> Json<Value> {
    let token = String::from_utf8_lossy(&body);
    let valid = verify_glagol(&state.glagol_jwt_key, token.trim()).is_ok();
    Json(json!({"status": "ok", "valid": valid}))
}

async fn glagol_check_v2(State(state): State<Arc<AppState>>, body: Bytes) -> Json<Value> {
    let token = String::from_utf8_lossy(&body);
    let owner = verify_glagol(&state.glagol_jwt_key, token.trim()).is_ok();
    Json(json!({"guest": false, "owner": owner, "status": "ok"}))
}

fn verify_glagol(key: &str, token: &str) -> Result<GlagolClaims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&["glagol"]);
    validation.set_issuer(&["quasar-backend"]);
    decode::<GlagolClaims>(
        token,
        &DecodingKey::from_secret(key.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
}

async fn check_updates() -> Json<Value> {
    Json(json!({"hasUpdate": false}))
}

async fn register(Query(_query): Query<HashMap<String, String>>) -> Json<Value> {
    Json(json!({"hasCriticalUpdate": false, "status": "ok"}))
}

async fn update_device_state(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    let Some(duid) = query.get("device_id") else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = query
        .get("platform")
        .map(String::as_str)
        .unwrap_or("unknown");
    match state.storage.update_network_info(duid, platform, &body) {
        Ok(_) => Json(json!({"status": "ok"})).into_response(),
        Err(error) => {
            tracing::error!(%error, "update_device_state failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn http_date() -> HeaderValue {
    HeaderValue::from_str(&Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .expect("HTTP date is ASCII")
}

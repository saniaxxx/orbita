//! HTTP hardening shared by orbita MCP servers (`orbita-tv`,
//! `orbita-mcp-search`, `orbita-home-mcp`), spec §7: private network plus a
//! token.
//!
//! - `MCP_TOKEN` — bearer token required from network callers on every path
//!   except `/health`; loopback callers on the same host (the legacy agent)
//!   stay trusted, as before. Serve with
//!   `into_make_service_with_connect_info::<SocketAddr>()` so the peer address
//!   is known; without it every caller must present the token;
//! - `MCP_ALLOWED_HOSTS` — extra `Host` values rmcp accepts besides loopback
//!   (comma-separated, e.g. `192.168.250.103`);
//! - a server bound to a non-loopback address refuses to start without a
//!   token, so a tool server is never open on the LAN by accident.

use std::net::SocketAddr;

use axum::{
    Router,
    extract::{ConnectInfo, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;

/// rmcp server config: loopback hosts plus `MCP_ALLOWED_HOSTS`.
pub fn server_config() -> StreamableHttpServerConfig {
    server_config_with(std::env::var("MCP_ALLOWED_HOSTS").ok().as_deref())
}

pub fn server_config_with(extra_hosts: Option<&str>) -> StreamableHttpServerConfig {
    let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    hosts.extend(
        extra_hosts
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned),
    );
    StreamableHttpServerConfig::default().with_allowed_hosts(hosts)
}

/// `MCP_TOKEN`, checked against the bind address.
pub fn token_from_env(host: &str) -> Result<Option<String>, String> {
    token_for(host, std::env::var("MCP_TOKEN").ok())
}

pub fn token_for(host: &str, token: Option<String>) -> Result<Option<String>, String> {
    let token = token
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());
    if token.is_none() && !is_loopback(host) {
        return Err(format!(
            "MCP_TOKEN is required when listening on {host}: tool servers must not be open on the network"
        ));
    }
    Ok(token)
}

fn is_loopback(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Require `Authorization: Bearer <token>` from network callers on everything
/// but `/health`.
/// With `None` the router is returned unchanged (loopback-only servers).
pub fn protect(router: Router, token: Option<String>) -> Router {
    match token {
        Some(token) => router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let token = token.clone();
            async move { check(request, next, &token).await }
        })),
        None => router,
    }
}

async fn check(request: Request, next: Next, token: &str) -> Response {
    let loopback_peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
    if request.uri().path() == "/health" || loopback_peer {
        return next.run(request).await;
    }
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(presented.as_bytes(), token.as_bytes()) {
        next.run(request).await
    } else {
        tracing::warn!(path = %request.uri().path(), "MCP request without a valid token");
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get};
    use tower::ServiceExt;

    fn app() -> Router {
        protect(
            Router::new()
                .route("/health", get(|| async { "ok" }))
                .route("/mcp", get(|| async { "tools" })),
            Some("s3cret".into()),
        )
    }

    async fn status_from(peer: Option<&str>, path: &str, auth: Option<&str>) -> StatusCode {
        let mut request = Request::builder().uri(path);
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, auth);
        }
        let mut request = request.body(Body::empty()).unwrap();
        if let Some(peer) = peer {
            let peer: SocketAddr = peer.parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
        }
        app().oneshot(request).await.unwrap().status()
    }

    async fn status(path: &str, auth: Option<&str>) -> StatusCode {
        status_from(Some("192.168.250.105:40000"), path, auth).await
    }

    #[tokio::test]
    async fn loopback_callers_stay_trusted() {
        assert_eq!(
            status_from(Some("127.0.0.1:5000"), "/mcp", None).await,
            StatusCode::OK
        );
        // Unknown peer (no ConnectInfo) is treated as the network.
        assert_eq!(
            status_from(None, "/mcp", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn bearer_token_guards_everything_but_health() {
        assert_eq!(status("/health", None).await, StatusCode::OK);
        assert_eq!(status("/mcp", None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            status("/mcp", Some("Bearer nope")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("/mcp", Some("s3cret")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(status("/mcp", Some("Bearer s3cret")).await, StatusCode::OK);
    }

    #[test]
    fn network_bind_requires_a_token() {
        assert_eq!(token_for("127.0.0.1", None), Ok(None));
        assert!(token_for("192.168.250.103", None).is_err());
        assert!(token_for("0.0.0.0", Some("  ".into())).is_err());
        assert_eq!(
            token_for("192.168.250.103", Some("t".into())),
            Ok(Some("t".into()))
        );
    }

    #[test]
    fn allowed_hosts_keep_loopback_and_add_extra() {
        let config = server_config_with(Some("192.168.250.103, orbita-hub"));
        assert_eq!(
            config.allowed_hosts,
            vec![
                "localhost",
                "127.0.0.1",
                "::1",
                "192.168.250.103",
                "orbita-hub"
            ]
        );
        assert_eq!(server_config_with(None).allowed_hosts.len(), 3);
    }
}

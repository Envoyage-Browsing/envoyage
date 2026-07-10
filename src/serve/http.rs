//! MCP Streamable HTTP transport (`envoyage serve --http-port N`).
//!
//! Mirrors the shape immorterm-memory uses: a single POST endpoint that parses
//! one JSON-RPC 2.0 request, dispatches it through the SAME [`mcp::handle_request`]
//! the stdio transport uses (same tools, same browser), and replies with either
//! plain JSON or an SSE `event: message` frame depending on the client's Accept
//! header. GET/DELETE are 405 — this is a request/response transport, no long-
//! lived server→client SSE stream (envoyage's live view is the WS frame stream,
//! not the MCP channel).
//!
//! Remote = untrusted network, so a bearer token (`ENVOYAGE_AUTH_TOKEN`) gates
//! every request when set. Unset → no auth (local dev).

use crate::serve::mcp;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use std::sync::Arc;

/// Shared HTTP state: the optional bearer token required on every request.
struct HttpState {
    auth_token: Option<String>,
}

/// Bind `127.0.0.1:port` (or `0.0.0.0` when `ENVOYAGE_HTTP_HOST` is set) and serve
/// MCP over Streamable HTTP until the process exits. `auth_token` is `Some` when
/// `ENVOYAGE_AUTH_TOKEN` is set — every request must then carry
/// `Authorization: Bearer <token>`.
pub async fn run(port: u16, auth_token: Option<String>) -> std::io::Result<()> {
    let state = Arc::new(HttpState { auth_token });
    let app = Router::new()
        .route(
            "/mcp",
            post(mcp_http_handler)
                .get(method_not_allowed)
                .delete(method_not_allowed),
        )
        .with_state(state);

    // Default to loopback; opt into 0.0.0.0 for a hosted deployment.
    let host = std::env::var("ENVOYAGE_HTTP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("envoyage: MCP Streamable HTTP on http://{addr}/mcp");
    axum::serve(listener, app).await
}

/// Bearer check: `Authorization: Bearer <token>` must match the configured
/// token. Returns `None` when auth is disabled or the token matches; `Some(401)`
/// otherwise. (A short-circuit compare is fine — this is a bearer secret over a
/// network round-trip, not a timing oracle worth hardening.)
fn check_auth(state: &HttpState, headers: &HeaderMap) -> Option<Response> {
    let expected = state.auth_token.as_deref()?; // None → auth disabled
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented == Some(expected) {
        None
    } else {
        Some(
            (
                StatusCode::UNAUTHORIZED,
                [("content-type", "application/json")],
                r#"{"error":"missing or invalid bearer token"}"#,
            )
                .into_response(),
        )
    }
}

async fn mcp_http_handler(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(resp) = check_auth(&state, &headers) {
        return resp;
    }

    let request: mcp::JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => {
            let error = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("Parse error: {e}") }
            });
            return (
                StatusCode::OK,
                [("content-type", "application/json")],
                error.to_string(),
            )
                .into_response();
        }
    };

    // Notifications (no id) get no body — 202 Accepted, matching the spec and
    // the immorterm-memory handler.
    if request.id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }

    // Same dispatch as stdio: one browser, one tool surface, new transport.
    let response = mcp::handle_request(&request);
    let json = serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"Internal serialization error"}}"#
            .to_string()
    });

    // SSE if the client asked for it (MCP Streamable HTTP negotiates via Accept).
    let wants_sse = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));

    if wants_sse {
        (
            StatusCode::OK,
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
            ],
            format!("event: message\ndata: {json}\n\n"),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            json,
        )
            .into_response()
    }
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [("content-type", "application/json")],
        r#"{"error":"Method not allowed. POST JSON-RPC to /mcp."}"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    fn headers_with_auth(val: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = val {
            h.insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn auth_disabled_lets_everything_through() {
        let state = HttpState { auth_token: None };
        assert!(check_auth(&state, &headers_with_auth(None)).is_none());
        assert!(check_auth(&state, &headers_with_auth(Some("Bearer whatever"))).is_none());
    }

    #[test]
    fn auth_enabled_requires_exact_bearer_match() {
        let state = HttpState { auth_token: Some("s3cret".into()) };
        // Correct token → allowed (None).
        assert!(check_auth(&state, &headers_with_auth(Some("Bearer s3cret"))).is_none());
        // Wrong token, missing header, and wrong scheme → 401 (Some).
        for bad in [None, Some("Bearer nope"), Some("s3cret"), Some("Basic s3cret")] {
            let resp = check_auth(&state, &headers_with_auth(bad));
            let resp = resp.unwrap_or_else(|| panic!("expected 401 for {bad:?}"));
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }
}

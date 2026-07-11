//! The remote surface (`envoyage serve --http-port N`): MCP over Streamable
//! HTTP **plus** a per-session live-view SSE stream and input channel. This is
//! the Workers-safe brain the SDK talks to — plain fetch + Server-Sent Events,
//! no WebSocket anywhere on this surface.
//!
//! Routes (all bearer-gated when `ENVOYAGE_AUTH_TOKEN` is set):
//! - `POST /mcp` — one JSON-RPC 2.0 MCP request, dispatched through the SAME
//!   [`mcp::handle_request`] the stdio transport uses (same tools, same
//!   registry). Session routing via the `Mcp-Session-Id` header (standard MCP).
//!   Replies plain JSON or an SSE `event: message` frame per the Accept header.
//! - `GET /sessions/:id/events` — the live view: a `text/event-stream` that
//!   pushes this session's Frame + Cursor + Narration + HumanRequest + State
//!   envelopes as SSE `data:` events (frames ride as base64 PNG inside the
//!   frame envelope). Replays the current frame + handoff banner on connect,
//!   then streams. Auto-reconnects (native EventSource), Workers-native.
//! - `POST /sessions/:id/input` — the human's clicks/keys/scroll + pause/resume
//!   control during a handoff, as one [`protocol::Input`] JSON body. Plain
//!   fetch; feeds the session's pump exactly like the WS input path.
//!
//! PASSWORD-BLIND boundary: enforced in Rust, NOT here in wire code. While a
//! session is paused/handed-off, its MCP tool responses already carry no
//! screenshot/AX bytes (see `mcp::handle_browser_shot`). The SSE stream IS the
//! human's live view and DOES carry frames during a handoff — that is the only
//! path to the screen, and only the human watching the stream sees it.
//!
//! Remote = untrusted network, so a bearer token (`ENVOYAGE_AUTH_TOKEN`) gates
//! every request when set. Unset → no auth (local dev).

use crate::protocol::Input;
use crate::serve::{mcp, state};
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::{self, StreamExt};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

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
        .route("/sessions/{id}/events", get(sse_events_handler))
        .route("/sessions/{id}/input", post(input_handler))
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

    // Multi-session key: a remote agent picks its own browser by sending an
    // `Mcp-Session-Id` header (the standard MCP session header). Each distinct id
    // gets its own BrowserSession (its own CDP connection) in the registry, so
    // one serve process multiplexes many agents. No header → the single default
    // session (single-session HTTP, same as before).
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::serve::state::DEFAULT_SESSION);

    // Same dispatch as stdio: registry-keyed browser, same tool surface. Run it
    // on a BLOCKING thread: handle_request is fully synchronous — it takes
    // blocking mutexes, sleeps (settle/wait_for_human), and — on the remote-CDP
    // path — BrowserSession::connect builds its own tokio runtime and blocks on
    // it. Calling any of that inline on this axum async worker panics with
    // "Cannot start a runtime from within a runtime" (a nested block_on). So we
    // hop to spawn_blocking, matching the stdio transport's dedicated thread.
    let session_id = session_id.to_string();
    let response = match tokio::task::spawn_blocking(move || {
        mcp::handle_request(&session_id, &request)
    })
    .await
    {
        Ok(resp) => resp,
        Err(join_err) => {
            // The tool panicked on the blocking thread — surface a clean JSON-RPC
            // error instead of dropping the connection (which is what shows up as
            // an empty/EOF body on the client).
            let error = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32603, "message": format!("tool panicked: {join_err}") }
            });
            return (
                StatusCode::OK,
                [("content-type", "application/json")],
                error.to_string(),
            )
                .into_response();
        }
    };
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

/// `GET /sessions/:id/events` — the per-session live-view SSE stream.
///
/// On connect we replay the session's cached envelopes (current frame + any
/// active handoff banner + pause state) so a viewer joining a STATIC page sees
/// the picture immediately, then forward every new envelope as it's broadcast.
/// Each SSE event names its protocol type (`event: browser_frame` etc.) with the
/// full JSON envelope as `data:` — the SDK dispatches on the event name and the
/// frame's base64 PNG rides inside the `browser_frame` envelope.
async fn sse_events_handler(
    State(state): State<Arc<HttpState>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = check_auth(&state, &headers) {
        return resp;
    }

    // Replay-first, then live: chain the cached envelopes ahead of the broadcast
    // receiver so a mid-session joiner is caught up before the next frame.
    let replay = state::replay_envelopes_of(&session_id);
    let rx = state::subscribe_to(&session_id);

    let replay_stream = stream::iter(replay.into_iter().map(sse_event));
    let live_stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(env) => return Some((sse_event(env), rx)),
                // Slow client: skip missed frames (coalescing), keep streaming.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None, // bus dropped → end stream
            }
        }
    });

    let events = replay_stream.chain(live_stream);
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

/// Turn a broadcast envelope (`{"type":"browser_frame",...}`) into an SSE event
/// named by its `type` so the client can dispatch on the event name. Falls back
/// to an unnamed `message` event if the type can't be read.
fn sse_event(env: String) -> Result<Event, Infallible> {
    let name = envelope_type(&env).unwrap_or("message");
    Ok(Event::default().event(name).data(env))
}

/// Extract the `"type":"..."` discriminant from a serialized envelope without a
/// full parse (the envelopes are small, flat, and we control their shape).
fn envelope_type(env: &str) -> Option<&'static str> {
    const TYPES: [&str; 5] = [
        "browser_frame",
        "browser_cursor",
        "browser_narration",
        "browser_human_request",
        "browser_state",
    ];
    TYPES
        .into_iter()
        .find(|t| env.contains(&format!("\"type\":\"{t}\"")))
}

/// `POST /sessions/:id/input` — the human's live-view input during a handoff
/// (click/key/scroll) or a pause/resume control toggle. Body is one
/// [`protocol::Input`] JSON object (same wire shape the WS surface accepts); we
/// queue it onto the session's input channel for its pump to dispatch.
async fn input_handler(
    State(state): State<Arc<HttpState>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(resp) = check_auth(&state, &headers) {
        return resp;
    }
    match serde_json::from_str::<Input>(&body) {
        Ok(ev) => {
            state::push_input_to(&session_id, ev);
            StatusCode::ACCEPTED.into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            format!(r#"{{"error":"invalid input event: {e}"}}"#),
        )
            .into_response(),
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

    // The SSE event name is derived from the envelope's `type` so the client can
    // dispatch by event name. Every protocol envelope must map to its own name;
    // an unknown/garbage envelope falls back to the generic `message` event.
    #[test]
    fn envelope_type_names_each_protocol_event() {
        for t in [
            "browser_frame",
            "browser_cursor",
            "browser_narration",
            "browser_human_request",
            "browser_state",
        ] {
            let env = format!(r#"{{"type":"{t}","x":1}}"#);
            assert_eq!(envelope_type(&env), Some(t), "{t} should name itself");
        }
        assert_eq!(envelope_type(r#"{"nope":true}"#), None, "unknown → fallback");
    }

    // The input body must deserialize from the exact protocol wire shape the WS
    // surface accepts (the SDK's POST /input body). If this contract drifts, the
    // human's live-view clicks/keys/controls silently 400.
    #[test]
    fn input_body_parses_all_wire_kinds() {
        for body in [
            r#"{"kind":"click","x":10,"y":20}"#,
            r#"{"kind":"key","key":"Enter"}"#,
            r#"{"kind":"scroll","dy":-5}"#,
            r#"{"kind":"control","action":"pause"}"#,
        ] {
            assert!(
                serde_json::from_str::<Input>(body).is_ok(),
                "POST /input must accept {body}"
            );
        }
        assert!(serde_json::from_str::<Input>(r#"{"kind":"bogus"}"#).is_err());
    }
}

//! `envoyage serve` — the runnable surface: an MCP stdio server + an optional WS
//! frame stream + an optional MCP-over-HTTP endpoint. ONE serve process holds a
//! registry of N independent in-process browsers, keyed by session id (the stdio
//! loop uses one implicit session; HTTP agents key by their `Mcp-Session-Id`
//! header), so a cloud deployment can multiplex many agents — each its own CDP
//! connection — through a single process.
//!
//! Layout:
//! - [`state`] — the per-session browser registry, per-session pause flag,
//!   WS<->pump channels.
//! - [`pump`] — the screencast pump + `with_browser` access + ownership lock.
//! - [`mcp`] — the JSON-RPC MCP server and `browser_*` tool handlers.
//! - [`http`] — the remote surface: MCP over Streamable HTTP + a per-session
//!   live-view SSE stream (`GET /sessions/:id/events`) + input channel
//!   (`POST /sessions/:id/input`), opt-in for remote agents + the SDK.
//! - [`recorder`] — the `browser_gif` recording buffer + annotated-GIF export.
//! - [`ws`] — the WS frame-stream server.

mod http;
mod mcp;
mod pump;
mod recorder;
mod state;
mod ws;

/// How `envoyage serve` was invoked.
pub struct Options {
    /// Serve MCP over stdio (default true when no other transport is given).
    pub mcp: bool,
    /// If set, also serve the WS frame stream on `127.0.0.1:<port>`.
    pub ws_port: Option<u16>,
    /// If set, also serve the remote surface on `<host>:<port>`: MCP over
    /// Streamable HTTP (`POST /mcp`, session-routed via `Mcp-Session-Id`) plus a
    /// per-session live-view SSE stream (`GET /sessions/:id/events`) and input
    /// channel (`POST /sessions/:id/input`) — the Workers-safe surface the SDK
    /// talks to. Bearer auth via `ENVOYAGE_AUTH_TOKEN` when that env var is set.
    pub http_port: Option<u16>,
    /// If set, drive a REMOTE browser over this CDP WebSocket URL instead of
    /// spawning a local Chromium (e.g. Cloudflare Browser Run).
    pub cdp_url: Option<String>,
}

/// Run the server. Blocks until stdin EOF (MCP) or forever (WS/HTTP-only).
pub fn run(opts: Options) -> std::io::Result<()> {
    // Remote-CDP selection is process-global: record it before any browser
    // launch so `with_browser` connects instead of spawning.
    state::set_cdp_url(opts.cdp_url.clone());

    // WS server on its own tokio runtime + thread, if requested. Start the pump
    // so frames flow to WS clients even before the first MCP tool call.
    if let Some(port) = opts.ws_port {
        pump::ensure_pump();
        std::thread::Builder::new()
            .name("envoyage-ws".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build ws runtime");
                if let Err(e) = rt.block_on(ws::run(port)) {
                    eprintln!("envoyage: WS server error: {e}");
                }
            })?;
    }

    // MCP-over-HTTP server on its own runtime + thread, if requested. Bearer
    // auth is on iff ENVOYAGE_AUTH_TOKEN is set (remote = untrusted network).
    if let Some(port) = opts.http_port {
        let auth_token = std::env::var("ENVOYAGE_AUTH_TOKEN").ok().filter(|t| !t.is_empty());
        std::thread::Builder::new()
            .name("envoyage-http".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build http runtime");
                if let Err(e) = rt.block_on(http::run(port, auth_token)) {
                    eprintln!("envoyage: HTTP server error: {e}");
                }
            })?;
    }

    let network_only = opts.ws_port.is_some() || opts.http_port.is_some();
    if opts.mcp {
        // MCP owns the main thread (blocking stdio loop until EOF).
        mcp::serve_stdio()
    } else if network_only {
        // WS/HTTP-only: park the main thread; the server threads do the work.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    } else {
        // Nothing requested → default to MCP.
        mcp::serve_stdio()
    }
}

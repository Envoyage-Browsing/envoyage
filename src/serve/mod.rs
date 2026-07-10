//! `rudder serve` — the runnable surface: an MCP stdio server + an optional WS
//! frame stream, both driving one in-process browser.
//!
//! Layout:
//! - [`state`] — process-global browser slot, pause flag, WS<->pump channels.
//! - [`pump`] — the screencast pump + `with_browser` access + ownership lock.
//! - [`mcp`] — the JSON-RPC MCP server and `browser_*` tool handlers.
//! - [`http`] — MCP over Streamable HTTP (opt-in, for remote agents).
//! - [`recorder`] — the `browser_gif` recording buffer + annotated-GIF export.
//! - [`ws`] — the WS frame-stream server.

mod http;
mod mcp;
mod pump;
mod recorder;
mod state;
mod ws;

/// How `rudder serve` was invoked.
pub struct Options {
    /// Serve MCP over stdio (default true when no other transport is given).
    pub mcp: bool,
    /// If set, also serve the WS frame stream on `127.0.0.1:<port>`.
    pub ws_port: Option<u16>,
    /// If set, also serve MCP over Streamable HTTP on `<host>:<port>` (remote
    /// agents). Bearer auth via `RUDDER_AUTH_TOKEN` when that env var is set.
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
            .name("rudder-ws".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build ws runtime");
                if let Err(e) = rt.block_on(ws::run(port)) {
                    eprintln!("rudder: WS server error: {e}");
                }
            })?;
    }

    // MCP-over-HTTP server on its own runtime + thread, if requested. Bearer
    // auth is on iff RUDDER_AUTH_TOKEN is set (remote = untrusted network).
    if let Some(port) = opts.http_port {
        let auth_token = std::env::var("RUDDER_AUTH_TOKEN").ok().filter(|t| !t.is_empty());
        std::thread::Builder::new()
            .name("rudder-http".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build http runtime");
                if let Err(e) = rt.block_on(http::run(port, auth_token)) {
                    eprintln!("rudder: HTTP server error: {e}");
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

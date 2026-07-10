//! `rudder serve` — the runnable surface: an MCP stdio server + an optional WS
//! frame stream, both driving one in-process browser.
//!
//! Layout:
//! - [`state`] — process-global browser slot, pause flag, WS<->pump channels.
//! - [`pump`] — the screencast pump + `with_browser` access + ownership lock.
//! - [`mcp`] — the JSON-RPC MCP server and `browser_*` tool handlers.
//! - [`ws`] — the WS frame-stream server.

mod mcp;
mod pump;
mod state;
mod ws;

/// How `rudder serve` was invoked.
pub struct Options {
    /// Serve MCP over stdio (default true when no `--ws-port` is given).
    pub mcp: bool,
    /// If set, also serve the WS frame stream on `127.0.0.1:<port>`.
    pub ws_port: Option<u16>,
}

/// Run the server. Blocks until stdin EOF (MCP) or forever (WS-only).
pub fn run(opts: Options) -> std::io::Result<()> {
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

    if opts.mcp {
        // MCP owns the main thread (blocking stdio loop until EOF).
        mcp::serve_stdio()
    } else if opts.ws_port.is_some() {
        // WS-only: park the main thread; the WS thread does the work.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    } else {
        // Neither requested → default to MCP.
        mcp::serve_stdio()
    }
}

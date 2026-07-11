//! `envoyage` CLI. One subcommand: `serve`.
//!
//!   envoyage serve                     MCP over stdio (default)
//!   envoyage serve --mcp               MCP over stdio (explicit)
//!   envoyage serve --ws-port 8787      also stream frames over WS
//!   envoyage serve --http-port 8788    also serve MCP over Streamable HTTP
//!   envoyage serve --cdp-url wss://…   drive a REMOTE browser (no local spawn)
//!   envoyage serve --ws-port 8787 --mcp   both

use envoyage::serve::{self, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => {
            let opts = match parse_serve(&args[1..]) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("envoyage: {e}\n\n{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = serve::run(opts) {
                eprintln!("envoyage: {e}");
                std::process::exit(1);
            }
        }
        Some("-h") | Some("--help") | None => {
            println!("{USAGE}");
        }
        Some("-V") | Some("--version") => {
            println!("envoyage {}", env!("CARGO_PKG_VERSION"));
        }
        Some(other) => {
            eprintln!("envoyage: unknown command '{other}'\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn parse_serve(args: &[String]) -> Result<Options, String> {
    let mut mcp = false;
    let mut ws_port: Option<u16> = None;
    let mut http_port: Option<u16> = None;
    let mut cdp_url: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mcp" => mcp = true,
            "--ws-port" => {
                let v = it.next().ok_or("--ws-port needs a port number")?;
                ws_port = Some(v.parse().map_err(|_| format!("invalid port '{v}'"))?);
            }
            "--http-port" => {
                let v = it.next().ok_or("--http-port needs a port number")?;
                http_port = Some(v.parse().map_err(|_| format!("invalid port '{v}'"))?);
            }
            "--cdp-url" => {
                cdp_url = Some(it.next().ok_or("--cdp-url needs a ws:// or wss:// URL")?.clone());
            }
            other => return Err(format!("unknown flag '{other}'")),
        }
    }
    // Default to stdio MCP unless the user asked ONLY for a network transport.
    if !mcp && ws_port.is_none() && http_port.is_none() {
        mcp = true;
    }
    Ok(Options { mcp, ws_port, http_port, cdp_url })
}

const USAGE: &str = "\
envoyage — drive a real browser from any AI agent, live.

USAGE:
    envoyage serve [--mcp] [--ws-port <PORT>] [--http-port <PORT>] [--cdp-url <URL>]

OPTIONS:
    --mcp               Serve MCP (JSON-RPC 2.0) over stdio. Default when no
                        --ws-port/--http-port is given.
    --ws-port <PORT>    Also stream frames + accept input over WS on
                        127.0.0.1:<PORT>. Pass with --mcp to serve both.
    --http-port <PORT>  Also serve the remote surface for a remote agent + the
                        SDK: MCP over Streamable HTTP (POST /mcp, session-routed
                        via the Mcp-Session-Id header), a per-session live-view
                        SSE stream (GET /sessions/<id>/events), and an input
                        channel (POST /sessions/<id>/input). Bind host via
                        ENVOYAGE_HTTP_HOST (default 127.0.0.1). Bearer auth via
                        ENVOYAGE_AUTH_TOKEN.
    --cdp-url <URL>     Drive a REMOTE browser at this CDP WebSocket URL
                        (ws://…/wss://…) instead of spawning a local Chromium —
                        e.g. a Cloudflare Browser Run endpoint.
    -h, --help          Show this help.
    -V, --version       Show version.

ENV:
    ENVOYAGE_HOME          Base dir for browser.lock + profile (default ~/.envoyage).
    ENVOYAGE_BROWSER_BIN   Path to a Chromium/Chrome/Brave/Edge binary override.
    ENVOYAGE_BROWSER_EVAL  Set to 1 to expose the gated browser_eval tool.
    ENVOYAGE_AUTH_TOKEN    If set, --http-port requires Authorization: Bearer it.
    ENVOYAGE_HTTP_HOST     Bind host for --http-port (default 127.0.0.1).";

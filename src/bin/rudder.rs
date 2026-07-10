//! `rudder` CLI. One subcommand: `serve`.
//!
//!   rudder serve                  MCP over stdio (default)
//!   rudder serve --mcp            MCP over stdio (explicit)
//!   rudder serve --ws-port 8787   also stream frames over WS
//!   rudder serve --ws-port 8787 --mcp   both

use rudder::serve::{self, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => {
            let opts = match parse_serve(&args[1..]) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("rudder: {e}\n\n{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = serve::run(opts) {
                eprintln!("rudder: {e}");
                std::process::exit(1);
            }
        }
        Some("-h") | Some("--help") | None => {
            println!("{USAGE}");
        }
        Some("-V") | Some("--version") => {
            println!("rudder {}", env!("CARGO_PKG_VERSION"));
        }
        Some(other) => {
            eprintln!("rudder: unknown command '{other}'\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn parse_serve(args: &[String]) -> Result<Options, String> {
    let mut mcp = false;
    let mut ws_port: Option<u16> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mcp" => mcp = true,
            "--ws-port" => {
                let v = it.next().ok_or("--ws-port needs a port number")?;
                ws_port = Some(v.parse().map_err(|_| format!("invalid port '{v}'"))?);
            }
            other => return Err(format!("unknown flag '{other}'")),
        }
    }
    // Default to MCP unless the user asked ONLY for WS.
    if !mcp && ws_port.is_none() {
        mcp = true;
    }
    Ok(Options { mcp, ws_port })
}

const USAGE: &str = "\
rudder — drive a real browser from any AI agent, live.

USAGE:
    rudder serve [--mcp] [--ws-port <PORT>]

OPTIONS:
    --mcp              Serve MCP (JSON-RPC 2.0) over stdio. Default when no
                       --ws-port is given.
    --ws-port <PORT>   Also stream frames + accept input over WS on
                       127.0.0.1:<PORT>. Pass with --mcp to serve both.
    -h, --help         Show this help.
    -V, --version      Show version.

ENV:
    RUDDER_HOME          Base dir for browser.lock + profile (default ~/.rudder).
    RUDDER_BROWSER_BIN   Path to a Chromium/Chrome/Brave/Edge binary override.
    RUDDER_BROWSER_EVAL  Set to 1 to expose the gated browser_eval tool.";

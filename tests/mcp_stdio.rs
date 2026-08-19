//! Black-box test of the `envoyage serve --mcp` JSON-RPC surface.
//!
//! Drives the compiled binary over stdin/stdout — no real browser needed, so it
//! runs in CI. Locks the handshake, the neutral `browser_*` tool names, the
//! eval gate, and an unknown-method error. (Tools that actually launch a
//! browser are covered by the ignored `screencast_live_smoke` in the lib.)

use std::io::Write;
use std::process::{Command, Stdio};

/// Feed `lines` to `envoyage serve --mcp` and collect the JSON responses.
fn run_mcp(lines: &[&str], eval: bool) -> Vec<serde_json::Value> {
    let bin = env!("CARGO_BIN_EXE_envoyage");
    let mut cmd = Command::new(bin);
    cmd.args(["serve", "--mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if eval {
        cmd.env("ENVOYAGE_BROWSER_EVAL", "1");
    }
    let mut child = cmd.spawn().expect("spawn envoyage");
    {
        let mut stdin = child.stdin.take().unwrap();
        for l in lines {
            writeln!(stdin, "{l}").unwrap();
        }
        // Drop stdin → EOF → server loop exits.
    }
    let out = child.wait_with_output().expect("wait");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse response json"))
        .collect()
}

#[test]
fn initialize_and_list_tools() {
    let resps = run_mcp(
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        ],
        false,
    );
    assert_eq!(resps.len(), 2, "one response per request with an id");

    // Handshake identifies the server as envoyage.
    assert_eq!(resps[0]["result"]["serverInfo"]["name"], "envoyage");
    assert!(resps[0]["result"]["capabilities"]["tools"].is_object());
    let instructions = resps[0]["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("browser_read_page/browser_find"));
    assert!(instructions.contains("Playwright tests first"));
    assert!(instructions.contains("captures nothing by default"));

    // Every tool is neutrally named by its Envoyage capability (no consumer
    // product prefix such as `immorterm_` or `flam_` may leak here).
    let tools = resps[1]["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            name.starts_with("browser_") || name.starts_with("crawl_"),
            "leaked tool name: {name}"
        );
        assert!(t["inputSchema"]["type"] == "object");
    }
    // The core surface is present.
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "crawl_start",
        "crawl_read",
        "crawl_cancel",
        "browser_open",
        "browser_read_page",
        "browser_click",
        "browser_request_human",
        "browser_gif",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    let screenshot = tools
        .iter()
        .find(|tool| tool["name"] == "browser_screenshot")
        .expect("browser_screenshot definition");
    assert_eq!(
        screenshot["inputSchema"]["properties"]["inline"]["type"],
        "boolean"
    );
    assert!(screenshot["description"]
        .as_str()
        .unwrap()
        .contains("OFF by default"));
    // eval is gated OFF by default.
    assert!(!names.contains(&"browser_eval"));
}

#[test]
fn eval_tool_appears_only_when_enabled() {
    let resps = run_mcp(
        &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#],
        true,
    );
    let names: Vec<&str> = resps[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"browser_eval"),
        "eval tool should appear with ENVOYAGE_BROWSER_EVAL=1"
    );
}

#[test]
fn unknown_method_is_json_rpc_error() {
    let resps = run_mcp(
        &[r#"{"jsonrpc":"2.0","id":9,"method":"does/not/exist","params":{}}"#],
        false,
    );
    assert_eq!(resps[0]["id"], 9);
    assert_eq!(resps[0]["error"]["code"], -32601);
}

#[test]
fn notification_without_id_gets_no_response() {
    // A notification (no id) followed by a real request: only the request replies.
    let resps = run_mcp(
        &[
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"initialize","params":{}}"#,
        ],
        false,
    );
    assert_eq!(resps.len(), 1);
    assert_eq!(resps[0]["id"], 5);
}

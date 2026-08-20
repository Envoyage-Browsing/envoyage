//! CI-safe end-to-end tests against a MOCK CDP WebSocket server — no live
//! browser. Drives the compiled `envoyage serve` binary with `--cdp-url
//! ws://127.0.0.1:<mock>` so the engine's real stack (mcp handlers → pump →
//! state → BrowserSession::connect → WsTransport) runs against a fake CDP target
//! we script from the test. Cloudflare Browser Run exposes the same
//! CDP-over-WebSocket shape, so green here means the remote path is exercised
//! end to end without a cloud account or a real Chromium.
//!
//! Four scenarios the spec requires:
//!  1. handoff state machine — drive → human-needed → pause → resume.
//!  2. active-tab following — a new tab becomes the followed target for drive.
//!  3. password-blindness — no screenshot bytes leak to the client while a wall
//!     is up (the mock records whether Page.captureScreenshot was ever called).
//!  4. multi-session isolation — two concurrent sessions don't cross-talk.

#![cfg(test)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

// ─── Mock CDP server ────────────────────────────────────────────────────────

/// Scriptable state shared with every connection the mock serves. Each field is
/// a knob a test sets before driving the engine; the connection handler reads
/// them when answering CDP commands.
#[derive(Default)]
struct MockScript {
    /// Page targets returned by `Target.getTargets`. `(targetId, title, url)`.
    /// Starts with one page; a test can push a second to simulate a popup/new
    /// tab and assert the engine follows + attaches to it.
    targets: Vec<(String, String, String)>,
    /// The `{kind}` the HUMAN_NEEDED_JS probe should report (`""` = none).
    handoff_kind: String,
    /// document.title reported by current_title_url / the AX snapshot.
    title: String,
    /// location.href reported by current_title_url / the AX snapshot.
    url: String,
    /// Set true whenever `Page.captureScreenshot` is invoked — the
    /// password-blindness assertion: it must STAY false while a wall is up.
    screenshot_called: bool,
    /// The `targetId` the engine most recently attached to (proves tab-follow).
    last_attached: String,
    /// Target lifecycle telemetry: switching must activate the selected page,
    /// detach the old flattened session, and avoid duplicate attachments.
    last_activated: String,
    attach_count: usize,
    detach_count: usize,
    /// When set, the next `Input.dispatchMouseEvent` "opens" this popup: the
    /// mock appends it to `targets` so it's a target that appeared AFTER the
    /// click (exactly what follow_new_target diffs for). `(id, title, url)`.
    popup_on_click: Option<(String, String, String)>,
}

impl MockScript {
    /// A script pre-seeded with one page target + its title/url, optionally with
    /// a handoff wall already up. Keeps the tests' setup to one line and avoids
    /// clippy's field-reassign-after-default lint.
    fn page(id: &str, title: &str, url: &str, handoff_kind: &str) -> MockScript {
        MockScript {
            targets: vec![(id.into(), title.into(), url.into())],
            title: title.into(),
            url: url.into(),
            handoff_kind: handoff_kind.into(),
            ..MockScript::default()
        }
    }
}

type SharedScript = Arc<Mutex<MockScript>>;

/// A running mock CDP server: its `ws://` URL + the shared script + a handle to
/// stop it. Dropping `stop` aborts the accept loop.
struct MockCdp {
    url: String,
    script: SharedScript,
    _rt: tokio::runtime::Runtime,
    stop: Arc<AtomicBool>,
}

impl MockCdp {
    /// Bind an OS-assigned port and start accepting CDP WS connections on a
    /// dedicated runtime. Each connection is handled independently (so a
    /// two-session test gets two isolated conversations against one script, or —
    /// with `per_conn` — its own script).
    fn start(initial: MockScript) -> MockCdp {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("mock cdp runtime");
        let script: SharedScript = Arc::new(Mutex::new(initial));
        let stop = Arc::new(AtomicBool::new(false));

        // Bind synchronously so we know the port before returning.
        let (listener, addr) = rt.block_on(async {
            let l = TcpListener::bind("127.0.0.1:0").await.expect("bind mock cdp");
            let a = l.local_addr().unwrap();
            (l, a)
        });
        let url = format!("ws://{addr}");

        let script_bg = script.clone();
        let stop_bg = stop.clone();
        rt.spawn(async move {
            loop {
                let accept = listener.accept().await;
                if stop_bg.load(Ordering::SeqCst) {
                    break;
                }
                let Ok((stream, _)) = accept else { continue };
                let script = script_bg.clone();
                tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                        serve_conn(ws, script).await;
                    }
                });
            }
        });

        MockCdp { url, script, _rt: rt, stop }
    }
}

impl Drop for MockCdp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Handle one CDP connection: answer each command by `id`, and push a
/// screencast frame after `Page.startScreencast` so the pump has something to
/// broadcast. Reads the shared script for the scriptable answers.
async fn serve_conn<S>(ws: tokio_tungstenite::WebSocketStream<S>, script: SharedScript)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = ws.split();
    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = v.get("params").cloned().unwrap_or_else(|| json!({}));

        let result = reply_for(method, &params, &script);
        let reply = json!({ "id": id, "result": result });
        if sink.send(Message::Text(reply.to_string())).await.is_err() {
            break;
        }

        // A navigate blocks in the engine until Page.loadEventFired; emit it so
        // navigate() returns promptly instead of waiting out LOAD_TIMEOUT.
        if method == "Page.navigate" {
            let ev = json!({ "method": "Page.loadEventFired", "params": {} });
            let _ = sink.send(Message::Text(ev.to_string())).await;
        }
        // After the screencast is armed, emit one frame so the pump broadcasts.
        if method == "Page.startScreencast" {
            let frame = json!({
                "method": "Page.screencastFrame",
                "params": { "data": "aGVsbG8=", "sessionId": 1 }
            });
            let _ = sink.send(Message::Text(frame.to_string())).await;
        }
    }
}

/// The `result` object for one CDP command, reading the script where the answer
/// is scriptable. Only the methods the engine actually issues are handled;
/// anything else gets an empty object (the engine treats most as best-effort).
fn reply_for(method: &str, params: &Value, script: &SharedScript) -> Value {
    let mut s = script.lock().unwrap();
    match method {
        "Target.getTargets" => {
            let infos: Vec<Value> = s
                .targets
                .iter()
                .map(|(id, title, url)| {
                    json!({ "targetId": id, "type": "page", "title": title, "url": url })
                })
                .collect();
            json!({ "targetInfos": infos })
        }
        "Target.attachToTarget" => {
            if let Some(tid) = params.get("targetId").and_then(|x| x.as_str()) {
                s.last_attached = tid.to_string();
            }
            s.attach_count += 1;
            json!({ "sessionId": "mock-session" })
        }
        "Target.activateTarget" => {
            if let Some(tid) = params.get("targetId").and_then(|x| x.as_str()) {
                s.last_activated = tid.to_string();
            }
            json!({})
        }
        "Target.detachFromTarget" => {
            s.detach_count += 1;
            json!({})
        }
        "Page.captureScreenshot" => {
            s.screenshot_called = true;
            // A 1x1 transparent PNG (base64) — real bytes so the engine's
            // "no data" guard passes; the point is that it was CALLED at all.
            json!({ "data": ONE_PX_PNG })
        }
        "Runtime.evaluate" => {
            let expr = params.get("expression").and_then(|e| e.as_str()).unwrap_or("");
            evaluate(expr, &s)
        }
        "Page.getLayoutMetrics" => json!({
            "cssLayoutViewport": { "clientWidth": 1280, "clientHeight": 800 }
        }),
        "Input.dispatchMouseEvent" => {
            // A click that opens a popup: append the new target so it appears
            // AFTER the action (what follow_new_target diffs `known_before`
            // against). Fire once, on the press half of the click.
            if params.get("type").and_then(|t| t.as_str()) == Some("mousePressed")
                && let Some(popup) = s.popup_on_click.take()
            {
                s.title = popup.1.clone();
                s.url = popup.2.clone();
                s.targets.push(popup);
            }
            json!({})
        }
        // Handshake / input / lifecycle commands: an empty result is enough.
        _ => json!({}),
    }
}

/// Emulate `Runtime.evaluate { returnByValue:true }`: the engine only inspects a
/// few well-known expressions, each expecting a specific `result.value`.
fn evaluate(expr: &str, s: &MockScript) -> Value {
    // detect_human_needed: the probe returns a JSON string `{"kind": "..."}`.
    if expr.contains("challenges.cloudflare.com") || expr.contains("input[type=password]") {
        let payload = json!({ "kind": s.handoff_kind }).to_string();
        return json!({ "result": { "value": payload } });
    }
    // AX snapshot (shared/ax-snapshot.js): return a canned listing with one text
    // input carrying a value, so tests can assert the paused value-strip. The
    // password floor is exercised at the JS layer (sdk test) + render layer
    // (browser.rs unit test); here we prove the server-side paused suppression.
    // NOTE: must precede the title/url branch — the snapshot JS also references
    // document.title + location.href, so it would otherwise match that first.
    if expr.contains("getBoundingClientRect") && expr.contains("data-immorterm-ref") {
        let payload = json!({
            "title": s.title,
            "url": s.url,
            "items": [{
                "role": "textbox", "name": "Card number", "value": "4111111111111111",
                "idx": 0, "interactive": true, "cx": 5, "cy": 5,
            }],
        })
        .to_string();
        return json!({ "result": { "value": payload } });
    }
    // current_title_url: `JSON.stringify({t:document.title,u:location.href})`.
    if expr.contains("document.title") && expr.contains("location.href") {
        let payload = json!({ "t": s.title, "u": s.url }).to_string();
        return json!({ "result": { "value": payload } });
    }
    // devicePixelRatio (used by screenshot()).
    if expr.contains("devicePixelRatio") {
        return json!({ "result": { "value": 1.0 } });
    }
    // Anything else → null value (harmless for the paths we drive) — includes the
    // `window.__ENVOYAGE_AX_MASK=…` config assignment the engine prepends.
    json!({ "result": { "value": Value::Null } })
}

/// 1x1 transparent PNG, base64 — a valid image payload for the mock screenshot.
const ONE_PX_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

// ─── Engine driver (spawns `envoyage serve --mcp --cdp-url <mock>`) ──────────

/// A running `envoyage serve --mcp` child wired to the mock CDP URL, driven over
/// stdin/stdout as line-delimited JSON-RPC. One implicit (stdio) session.
struct Engine {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: AtomicU64,
}

impl Engine {
    fn spawn_mcp(cdp_url: &str) -> Engine {
        let bin = env!("CARGO_BIN_EXE_envoyage");
        let mut child = Command::new(bin)
            .args(["serve", "--mcp", "--cdp-url", cdp_url])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn envoyage serve");
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        Engine { child, stdin, reader, next_id: AtomicU64::new(1) }
    }

    /// Send one JSON-RPC request and read the single response line back.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).expect("read engine stdout");
            assert_ne!(n, 0, "engine closed stdout before replying to {method}");
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(line.trim()).expect("parse engine response");
        }
    }

    /// A `tools/call` convenience returning the `result` object.
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let resp = self.call("tools/call", json!({ "name": name, "arguments": args }));
        assert!(resp.get("error").is_none(), "tool {name} errored: {resp}");
        resp["result"].clone()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The concatenated text of a tool result's `content[]` text parts.
fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Whether a tool result carries any `image` content part.
fn has_image(result: &Value) -> bool {
    result["content"]
        .as_array()
        .is_some_and(|arr| arr.iter().any(|c| c["type"] == "image"))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// [1] Handoff state machine — drive → human-needed → pause → resume — over the
/// REAL deployed resume path: the human's `control:continue` posted to
/// `/sessions/:id/input`, which the pump applies to flip the pause off. (The
/// resume control is a live-view input, not an MCP tool, so this must run on the
/// HTTP surface where `/input` exists.)
#[test]
fn handoff_drive_human_needed_pause_resume() {
    // The wall is up: the cloudflare handoff kind fires on the first drive.
    let mock = MockCdp::start(MockScript::page(
        "t1", "Login", "https://site.test/login", "cloudflare",
    ));

    let (port, _guard) = spawn_http_engine(&mock.url);
    wait_for_http(port);
    let base = format!("http://127.0.0.1:{port}");
    let sid = "handoff-session";

    // DRIVE into the wall: browser_open detects cloudflare → hands off.
    let open = http_tool(&base, sid, "browser_open", json!({ "url": "https://site.test/login" }));
    let t = text_of(&open);
    assert!(t.contains("Human needed"), "handoff message expected, got: {t}");
    assert!(!has_image(&open), "PASSWORD-BLIND: no screenshot while handing off");

    // PAUSED: wait_for_human must NOT report done yet (still paused).
    let waiting = http_tool(&base, sid, "browser_wait_for_human", json!({ "timeout_secs": 1 }));
    assert!(text_of(&waiting).contains("Still waiting"), "should still be paused");

    // RESUME: the human clears the wall and clicks ▶ Continue in the live view.
    // Drop the wall in the mock, then POST the exact control the UI sends.
    mock.script.lock().unwrap().handoff_kind.clear();
    http_input(&base, sid, json!({ "kind": "control", "action": "continue" }));

    // The pump drains the input each ~66ms tick and calls set_paused(false).
    let resumed = wait_until_resumed(&base, sid);
    assert!(resumed, "after control:continue, wait_for_human should report done");

    // And a fresh screenshot now returns an image again (wall down, not paused).
    let shot = http_tool(&base, sid, "browser_screenshot", json!({ "inline": true }));
    assert!(has_image(&shot), "after resume the screen returns to the model");
}

/// Poll `browser_wait_for_human` until it reports the human finished (pump has
/// applied the resume), or give up. Returns whether it resumed.
fn wait_until_resumed(base: &str, sid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let r = http_tool(base, sid, "browser_wait_for_human", json!({ "timeout_secs": 1 }));
        if text_of(&r).contains("Human finished") {
            return true;
        }
    }
    false
}

/// [2] Active-tab following: after a drive opens a new tab (a target that wasn't
/// present before the action), the engine follows it — the mock records the
/// engine attaching to the NEW target id, and subsequent reads reflect the new
/// tab's title/url.
#[test]
fn drive_follows_a_new_tab() {
    let mock = MockCdp::start(MockScript::page("opener", "Home", "https://site.test/", ""));

    let mut engine = Engine::spawn_mcp(&mock.url);

    // Open the home page (one target). The engine attaches to `opener`.
    engine.tool("browser_open", json!({ "url": "https://site.test/" }));
    assert_eq!(mock.script.lock().unwrap().last_attached, "opener");

    // Arm a popup: the click's mousePressed will open a NEW tab (a target that
    // appears AFTER the action). follow_new_target must attach to it.
    mock.script.lock().unwrap().popup_on_click =
        Some(("popup".into(), "Auth".into(), "https://auth.test/".into()));
    engine.tool("browser_click", json!({ "x": 10, "y": 10 }));

    assert_eq!(
        mock.script.lock().unwrap().last_attached, "popup",
        "the engine must follow the newly-opened tab for driving"
    );
    let script = mock.script.lock().unwrap();
    assert_eq!(
        script.last_activated, "popup",
        "the followed popup must also become Chrome's foreground target"
    );
    assert_eq!(
        script.detach_count, 1,
        "following a popup must detach the opener's CDP session"
    );
    assert_eq!(
        script.attach_count, 2,
        "one opener attachment plus one popup attachment"
    );
}

#[test]
fn tab_switch_reuses_current_attachment_and_replaces_it_once_for_another_target() {
    let mock = MockCdp::start(MockScript::page("opener", "Home", "https://site.test/", ""));
    let mut engine = Engine::spawn_mcp(&mock.url);

    engine.tool("browser_open", json!({ "url": "https://site.test/" }));
    mock.script.lock().unwrap().targets.push((
        "settings".into(),
        "Settings".into(),
        "chrome://settings/help".into(),
    ));
    engine.tool("browser_tabs_switch", json!({ "targetId": "opener" }));
    {
        let state = mock.script.lock().unwrap();
        assert_eq!(
            state.attach_count, 1,
            "re-selecting the current target must reuse its attachment"
        );
        assert_eq!(
            state.detach_count, 0,
            "re-selecting the current target must not detach it"
        );
        assert_eq!(state.last_activated, "opener");
    }

    engine.tool("browser_tabs_switch", json!({ "targetId": "settings" }));
    let state = mock.script.lock().unwrap();
    assert_eq!(
        state.attach_count, 2,
        "switching targets creates exactly one replacement attachment"
    );
    assert_eq!(
        state.detach_count, 1,
        "switching targets detaches exactly one old session"
    );
    assert_eq!(
        state.last_activated, "settings",
        "the selected target must be foregrounded"
    );
}

/// [3] Password-blindness: while a wall is up (handoff active), NO
/// Page.captureScreenshot is ever issued — the mock's `screenshot_called` stays
/// false — so no screenshot bytes can leak to the client. Then, with the wall
/// down, a screenshot IS taken (proving the guard is state-driven, not a blanket
/// disable).
#[test]
fn no_screenshot_bytes_while_wall_is_up() {
    // A password field → the wall is up from the first drive.
    let mock = MockCdp::start(MockScript::page(
        "t1", "Bank", "https://bank.test/login", "password",
    ));

    let mut engine = Engine::spawn_mcp(&mock.url);

    // Drive into the password wall.
    let open = engine.tool("browser_open", json!({ "url": "https://bank.test/login" }));
    assert!(!has_image(&open), "no image in the handoff response");
    assert!(
        !mock.script.lock().unwrap().screenshot_called,
        "PASSWORD-BLIND: captureScreenshot must NOT be called while the wall is up"
    );

    // A plain browser_screenshot while PAUSED must also stay blind.
    let shot_paused = engine.tool("browser_screenshot", json!({}));
    assert!(!has_image(&shot_paused), "paused screenshot returns text-only");
    assert!(
        !mock.script.lock().unwrap().screenshot_called,
        "PASSWORD-BLIND: still no captureScreenshot while paused"
    );

    // Now clear the wall + pause (close resets pause), reconnect with the wall
    // down, and confirm the default call still captures nothing.
    mock.script.lock().unwrap().handoff_kind.clear();
    let _ = engine.tool("browser_close", json!({}));
    let shot = engine.tool("browser_screenshot", json!({}));
    assert!(!has_image(&shot), "default screenshot call is text-only");
    assert!(
        !mock.script.lock().unwrap().screenshot_called,
        "default screenshot call must not issue captureScreenshot"
    );

    // Only an explicit inline opt-in captures pixels.
    let shot = engine.tool("browser_screenshot", json!({ "inline": true }));
    assert!(has_image(&shot), "explicit inline preview returns an image");
    assert!(
        mock.script.lock().unwrap().screenshot_called,
        "captureScreenshot runs once the wall is down"
    );
}

/// While the wall is up (session paused / handoff), `read_page` and `find` must
/// strip EVERY input value from the AX listing before returning to the model —
/// the parallel of the screenshot suppression. The mock returns an AX listing
/// with a card number typed into a text field; with the wall up that value must
/// NOT appear. This catches secrets typed into non-`password` fields during a
/// handoff.
#[test]
fn no_input_values_in_ax_listing_while_wall_is_up() {
    let mock = MockCdp::start(MockScript::page(
        "t1", "Checkout", "https://shop.test/pay", "password",
    ));
    let mut engine = Engine::spawn_mcp(&mock.url);

    // Drive into the wall → paused.
    let _ = engine.tool("browser_open", json!({ "url": "https://shop.test/pay" }));

    // read_page while paused: field is still listed, its value is withheld.
    let read = engine.tool("browser_read_page", json!({ "interactive_only": false }));
    let read_txt = text_of(&read);
    assert!(read_txt.contains("Card number"), "the field is still enumerated");
    assert!(!read_txt.contains("4111"), "the typed value must be stripped while paused");
    assert!(!read_txt.contains("value:"), "no value: field at all while paused");

    // find while paused: same guarantee.
    let found = engine.tool("browser_find", json!({ "query": "card" }));
    let found_txt = text_of(&found);
    assert!(!found_txt.contains("4111"), "find must also strip values while paused");

    // Wall down (close resets pause): the value flows again (guard is state-driven).
    mock.script.lock().unwrap().handoff_kind.clear();
    let _ = engine.tool("browser_close", json!({}));
    let _ = engine.tool("browser_open", json!({ "url": "https://shop.test/pay" }));
    let read2 = engine.tool("browser_read_page", json!({ "interactive_only": false }));
    assert!(text_of(&read2).contains("4111"), "with the wall down, values return");
}

/// [4] Multi-session isolation: two agents pointed at the SAME engine over the
/// HTTP surface with DISTINCT Mcp-Session-Id headers get independent browsers —
/// a handoff (pause) in session A never pauses session B, and each session's
/// browser is its own CDP connection to the mock (no cross-talk). Driven over
/// HTTP (the only multi-session transport); the mock serves each connection
/// independently.
#[test]
fn two_sessions_do_not_cross_talk() {
    // Session A hits a wall; session B is clean. One process-global cdp-url, but
    // each session opens its OWN connection to the mock — the mock answers the
    // handoff probe from the shared script, so we flip the wall per phase.
    let mock = MockCdp::start(MockScript::page("t1", "Page", "https://x.test/", ""));

    let (port, _guard) = spawn_http_engine(&mock.url);
    wait_for_http(port);
    let base = format!("http://127.0.0.1:{port}");

    // Phase 1: wall UP. Session A drives into it → paused. Session B is not
    // touched yet, so it must be unpaused.
    mock.script.lock().unwrap().handoff_kind = "cloudflare".into();
    let a_open = http_tool(&base, "A", "browser_open", json!({ "url": "https://x.test/" }));
    assert!(text_of(&a_open).contains("Human needed"), "session A hits the wall");

    // Phase 2: wall DOWN. Session B drives cleanly and can explicitly request
    // an image — proof A's pause did not bleed into B (per-session pause map).
    mock.script.lock().unwrap().handoff_kind.clear();
    let b_open = http_tool(&base, "B", "browser_open", json!({ "url": "https://x.test/" }));
    assert!(
        !has_image(&b_open),
        "drive actions must not duplicate Workshop frames into agent context"
    );
    let b_shot = http_tool(&base, "B", "browser_screenshot", json!({ "inline": true }));
    assert!(
        has_image(&b_shot),
        "session B must drive normally despite A being paused: {}",
        text_of(&b_shot)
    );

    // And A is still paused (its wait_for_human still waits) while B never was.
    let a_wait = http_tool(&base, "A", "browser_wait_for_human", json!({ "timeout_secs": 1 }));
    assert!(text_of(&a_wait).contains("Still waiting"), "A stays paused independently");
    let b_wait = http_tool(&base, "B", "browser_wait_for_human", json!({ "timeout_secs": 1 }));
    assert!(text_of(&b_wait).contains("Human finished"), "B was never paused");
}

// ─── HTTP helpers for the multi-session test ────────────────────────────────

/// Spawn `envoyage serve --http-port <free> --cdp-url <mock>`. Returns the port
/// + a guard that kills + reaps the child on drop. The caller waits for the port.
fn spawn_http_engine(cdp_url: &str) -> (u16, ChildGuard) {
    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_envoyage");
    let child = Command::new(bin)
        .args(["serve", "--http-port", &port.to_string(), "--cdp-url", cdp_url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // ponytail: ENVOYAGE_TEST_STDERR=1 surfaces the child engine's stderr —
        // the only way to see a panic inside the spawned process when a test hangs.
        .stderr(if std::env::var("ENVOYAGE_TEST_STDERR").is_ok() {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
        .expect("spawn envoyage serve --http-port");
    (port, ChildGuard(child))
}

/// POST one JSON-RPC tools/call to `<base>/mcp` with an Mcp-Session-Id, over raw
/// TCP (no HTTP-client dep). Returns the `result` object.
fn http_tool(base: &str, session: &str, name: &str, args: Value) -> Value {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
    .to_string();
    let resp = http_post(&format!("{base}/mcp"), session, &body);
    let v: Value = serde_json::from_str(&resp)
        .unwrap_or_else(|e| panic!("parse http response ({e}): {resp}"));
    assert!(v.get("error").is_none(), "http tool {name} errored: {v}");
    v["result"].clone()
}

/// POST one live-view input (the human's click/key/scroll/control) to
/// `<base>/sessions/:id/input` — the real resume path.
fn http_input(base: &str, session: &str, event: Value) {
    let path = format!("{base}/sessions/{session}/input");
    let _ = http_post(&path, session, &event.to_string());
}

/// Minimal HTTP/1.1 POST over raw TCP. Reads headers, then exactly
/// Content-Length bytes of body — hyper keeps the connection alive (ignores our
/// `Connection: close`), so we must NOT read to EOF (that would block until the
/// idle timeout). A 202 Accepted (the /input route) has no body → returns "".
fn http_post(url: &str, session: &str, body: &str) -> String {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let (host, port, path) = parse_url(url);
    let mut stream = TcpStream::connect((host.as_str(), port)).expect("connect engine http");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nMcp-Session-Id: {session}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).unwrap();

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut body_start: Option<usize> = None;
    let mut content_len: Option<usize> = None;
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if body_start.is_none()
            && let Some(idx) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            body_start = Some(idx + 4);
            let head = String::from_utf8_lossy(&buf[..idx]).to_lowercase();
            content_len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok());
        }
        match (body_start, content_len) {
            // Full body arrived.
            (Some(start), Some(len)) if buf.len() >= start + len => {
                return String::from_utf8_lossy(&buf[start..start + len]).into_owned();
            }
            // Headers done, no Content-Length (e.g. 202 Accepted) → empty body.
            (Some(start), None) => return String::from_utf8_lossy(&buf[start..]).into_owned(),
            _ => {}
        }
    }
    body_start
        .map(|s| String::from_utf8_lossy(&buf[s..]).into_owned())
        .unwrap_or_default()
}

fn parse_url(url: &str) -> (String, u16, String) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let (host, port) = authority.split_once(':').unwrap_or((authority, "80"));
    (host.to_string(), port.parse().unwrap_or(80), path)
}

/// Grab a free TCP port by binding :0, then releasing it. A short race window
/// remains before the engine binds it, acceptable for a test.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Poll the engine's /mcp until it accepts a TCP connection (server is up).
fn wait_for_http(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            // Give axum a beat to finish wiring routes.
            std::thread::sleep(Duration::from_millis(150));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("engine http server never came up on port {port}");
}

/// Owns an engine child process, killing + reaping it on scope exit.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

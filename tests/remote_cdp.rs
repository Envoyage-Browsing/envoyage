//! Remote-CDP transport smoke: prove `BrowserSession::connect(ws_url)` can drive
//! a browser rudder did NOT spawn, over a CDP WebSocket.
//!
//! We host the CDP endpoint LOCALLY so the test needs no cloud account: spawn a
//! headless Chromium with `--remote-debugging-port=0` (TCP, so it exposes a
//! DevTools HTTP+WS endpoint), read the browser-level WS URL from
//! `GET /json/version`, then connect to it and drive it (navigate + one
//! screencast frame). Cloudflare Browser Run exposes the SAME shape — a plain
//! CDP-over-WebSocket endpoint — so a green run here means the remote path works
//! against CF too (only the URL + auth differ; the consumer supplies those).
//!
//! Ignored by default (needs a real Chromium). Run with:
//!   cargo test -p rudder --test remote_cdp -- --ignored

#![cfg(test)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rudder::BrowserSession;

/// Locate a Chromium-engine browser the same way the library does (env override
/// first, then the usual macOS bundle / PATH names). Kept minimal — this is
/// test scaffolding, not the production locator.
fn find_browser() -> Option<String> {
    if let Ok(bin) = std::env::var("RUDDER_BROWSER_BIN")
        && !bin.is_empty()
        && std::path::Path::new(&bin).exists()
    {
        return Some(bin);
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    candidates.iter().find(|p| std::path::Path::new(p).exists()).map(|s| s.to_string())
}

/// Spawn a headless Chromium with a TCP debug port (0 = OS-assigned) and a
/// throwaway profile. Returns the child + the port it actually bound.
fn spawn_debug_chromium(bin: &str) -> (Child, u16) {
    // Port 0 lets Chromium pick a free port; it writes the real one to
    // <profile>/DevToolsActivePort. We poll that file for the port.
    let profile = std::env::temp_dir().join(format!("rudder-remote-cdp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).unwrap();

    let child = Command::new(bin)
        .arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn debug chromium");

    // Read the assigned port from DevToolsActivePort (line 1 is the port).
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(10);
    let port = loop {
        if let Ok(s) = std::fs::read_to_string(&port_file)
            && let Some(first) = s.lines().next()
            && let Ok(p) = first.trim().parse::<u16>()
        {
            break p;
        }
        assert!(Instant::now() < deadline, "Chromium never wrote DevToolsActivePort");
        std::thread::sleep(Duration::from_millis(100));
    };
    (child, port)
}

/// Minimal `GET {path}` over raw TCP → the response body. Avoids adding an
/// HTTP-client dep just for the test. Chrome's DevTools HTTP keeps the socket
/// alive (ignores `Connection: close`), so we must NOT `read_to_string` (that
/// blocks forever waiting for EOF) — instead read exactly Content-Length bytes.
fn http_get(port: u16, path: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
            stream.write_all(req.as_bytes()).unwrap();

            // Read until we have the full headers + Content-Length bytes of body.
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
                    && let Some(idx) = find_subslice(&buf, b"\r\n\r\n")
                {
                    body_start = Some(idx + 4);
                    let head = String::from_utf8_lossy(&buf[..idx]).to_lowercase();
                    content_len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok());
                }
                if let (Some(start), Some(len)) = (body_start, content_len)
                    && buf.len() >= start + len
                {
                    return String::from_utf8_lossy(&buf[start..start + len]).into_owned();
                }
            }
            // Fell out (timeout / EOF): return whatever body we have.
            if let Some(start) = body_start {
                return String::from_utf8_lossy(&buf[start..]).into_owned();
            }
        }
        assert!(Instant::now() < deadline, "DevTools HTTP endpoint never responded");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// First index of `needle` in `hay`, or `None`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[test]
#[ignore = "needs a real Chromium; run explicitly"]
fn connect_drives_remote_cdp_ws() {
    let Some(bin) = find_browser() else {
        eprintln!("skipping: no Chromium-engine browser found");
        return;
    };
    let (mut child, port) = spawn_debug_chromium(&bin);

    // The browser-level CDP WS URL comes from /json/version.
    let body = http_get(port, "/json/version");
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parse /json/version ({e}): {body}"));
    let ws_url = v
        .get("webSocketDebuggerUrl")
        .and_then(|u| u.as_str())
        .expect("webSocketDebuggerUrl in /json/version")
        .to_string();
    assert!(ws_url.starts_with("ws://"), "expected a ws:// URL, got {ws_url}");

    // The runtime handle is accepted for signature parity; connect builds its own.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut b = BrowserSession::connect(&rt, &ws_url).expect("connect to remote CDP WS");

    // Drive it: navigate (proves the CDP conversation flows over WS), then pull a
    // screencast frame (proves the frame-drain works over WS, not just the pipe).
    b.navigate("about:blank").expect("navigate over WS");
    let _ = b.eval("document.body.style.background='#135'; true");

    b.ensure_screencast().expect("startScreencast over WS");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_frame = false;
    while Instant::now() < deadline {
        if let Ok(Some(png)) = b.poll_screencast_frame() {
            assert!(!png.is_empty(), "empty screencast frame over WS");
            got_frame = true;
            break;
        }
        let _ = b.eval(&format!(
            "document.body.style.background = '#{:03x}'; true",
            (Instant::now().elapsed().as_millis() % 4096) as u32
        ));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(got_frame, "no screencast frame over the remote WS within 5s");

    // close() must NOT kill the remote browser (we didn't spawn it) — the child
    // we spawned separately is still alive afterward.
    b.close();
    assert!(
        child.try_wait().unwrap().is_none(),
        "remote browser was killed by close() — it must not touch a browser it didn't spawn"
    );

    // Tear down the test-owned Chromium ourselves.
    let _ = child.kill();
    let _ = child.wait();
}

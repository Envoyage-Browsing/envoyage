//! Verify the REMOTE/cloud path (BrowserSession::connect over CDP-over-WebSocket,
//! e.g. Cloudflare Browser Rendering) gets the same CDP-layer stealth as local —
//! launch flags can't reach a browser we didn't spawn, so this proves the UA
//! override + injected shim applied in attach_target do the job on connect() too.
//!
//!   cargo run --example connect-probe -- <browser-ws-url> [url]
//!
//! Get the ws url from a Chrome started with --remote-debugging-port:
//!   curl -s localhost:9500/json/version | jq -r .webSocketDebuggerUrl

use envoyage::BrowserSession;
use std::time::Duration;

fn main() {
    let ws = std::env::args().nth(1).expect("usage: connect-probe <ws-url> [url]");
    let url = std::env::args().nth(2).unwrap_or_else(|| "https://example.com".to_string());
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut b = BrowserSession::connect(&rt, &ws).expect("connect");
    b.navigate(&url).expect("navigate");
    std::thread::sleep(Duration::from_millis(1200));
    let probe = r#"JSON.stringify({
        webdriver: navigator.webdriver,
        headless: /headless/i.test(navigator.userAgent),
        ua: navigator.userAgent,
        brands: (navigator.userAgentData ? navigator.userAgentData.brands.map(b=>b.brand) : 'n/a'),
        screen: [screen.width, screen.height],
        innerW: window.innerWidth
    })"#;
    println!("CONNECT_PROBE {}", b.eval(probe).expect("eval"));
    // Do NOT close(): connect() must never kill a browser it didn't spawn. Drop
    // just closes the WS.
}

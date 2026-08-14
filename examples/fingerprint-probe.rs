//! Baseline stealth probe: launch the engine, navigate to a URL, dump the
//! classic bot-detection tells + a screenshot. Not shipped — a measurement
//! harness for the stealth-hardening work.
//!
//!   cargo run --example fingerprint-probe -- [url] [out.png]
//!
//! Default url = https://bot.sannysoft.com (the puppeteer-stealth pass/fail table).

use envoyage::browser::decode_png_width;
use envoyage::BrowserSession;
use std::time::Duration;

const PROBE_JS: &str = r#"
(() => {
  const g = {};
  try { g.webdriver = navigator.webdriver; } catch(e) { g.webdriver = 'err:'+e; }
  g.userAgent = navigator.userAgent;
  g.uaContainsHeadless = /headless/i.test(navigator.userAgent);
  g.appVersion = navigator.appVersion;
  g.platform = navigator.platform;
  g.languages = navigator.languages;
  g.vendor = navigator.vendor;
  g.pluginsLength = (navigator.plugins||[]).length;
  g.mimeTypesLength = (navigator.mimeTypes||[]).length;
  g.hardwareConcurrency = navigator.hardwareConcurrency;
  g.deviceMemory = navigator.deviceMemory;
  g.hasChrome = typeof window.chrome !== 'undefined';
  g.hasChromeRuntime = !!(window.chrome && window.chrome.runtime);
  // Client hints (Sec-CH-UA brands) — headless leaks "HeadlessChrome" here even
  // when the UA string is overridden by the --user-agent flag.
  try {
    g.uaDataBrands = (navigator.userAgentData && navigator.userAgentData.brands)
      ? navigator.userAgentData.brands.map(b => b.brand) : 'no-uaData';
    g.uaDataMobile = navigator.userAgentData ? navigator.userAgentData.mobile : 'no-uaData';
  } catch(e) { g.uaDataBrands = 'err:'+e; }
  // CDP Runtime.enable tell (rebrowser 'runtimeEnableLeak'): with the Runtime
  // domain enabled, Chrome eagerly serializes console args for consoleAPICalled,
  // firing a getter on the argument. Try every documented variant.
  const rtTell = {};
  const mk = (label, fn) => { let hit=false; try { const o={}; Object.defineProperty(o,'id',{get(){hit=true;return 1;}}); fn(o); } catch(e){} rtTell[label]=hit; };
  mk('console.log',   o => console.log(o));
  mk('console.debug', o => console.debug(o));
  mk('console.dir',   o => console.dir(o));
  mk('console.table', o => console.table([o]));
  // Error.stack getter variant
  { let hit=false; try { const e=new Error(); Object.defineProperty(e,'stack',{get(){hit=true;return '';}}); console.debug(e); } catch(_){} rtTell['error.stack'] = hit; }
  // console timing side-channel (enabled Runtime is measurably slower on big args)
  try {
    const big = new Array(2000).fill({x:1});
    const t0 = performance.now();
    for (let i=0;i<50;i++) console.debug(big);
    rtTell['timing_ms_50x'] = Math.round((performance.now()-t0)*10)/10;
  } catch(e) { rtTell['timing_ms_50x'] = 'err'; }
  g.cdpRuntimeEnableTell = rtTell;
  g.notificationPermission = (typeof Notification !== 'undefined') ? Notification.permission : 'no-Notification';
  g.devicePixelRatio = window.devicePixelRatio;
  g.screen = { w: screen.width, h: screen.height, availW: screen.availWidth, availH: screen.availHeight, colorDepth: screen.colorDepth };
  g.outer = { w: window.outerWidth, h: window.outerHeight, innerW: window.innerWidth, innerH: window.innerHeight };
  // permissions/notification mismatch tell
  try {
    // synchronous best-effort; the async form is checked below
    g.permissionsQueryType = (navigator.permissions && navigator.permissions.query) ? 'fn' : 'missing';
  } catch(e) { g.permissionsQueryType = 'err'; }
  // WebGL vendor/renderer
  try {
    const c = document.createElement('canvas');
    const gl = c.getContext('webgl') || c.getContext('experimental-webgl');
    const ext = gl && gl.getExtension('WEBGL_debug_renderer_info');
    g.webglVendor = ext ? gl.getParameter(ext.UNMASKED_VENDOR_WEBGL) : (gl ? gl.getParameter(gl.VENDOR) : 'no-gl');
    g.webglRenderer = ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : (gl ? gl.getParameter(gl.RENDERER) : 'no-gl');
  } catch(e) { g.webglVendor = 'err:'+e; }
  // native-function patch tell
  const nat = (f) => { try { return Function.prototype.toString.call(f); } catch(e) { return 'err'; } };
  g.toStringPermissions = nat(navigator.permissions && navigator.permissions.query);
  g.toStringWebdriverGetter = (() => {
    try { const d = Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver'); return d ? String(d.get) : 'no-descriptor'; } catch(e) { return 'err'; }
  })();
  return JSON.stringify(g);
})()
"#;

fn main() {
    let url = std::env::args().nth(1).unwrap_or_else(|| "https://bot.sannysoft.com".to_string());
    let out = std::env::args().nth(2).unwrap_or_else(|| "/tmp/envoyage-probe.png".to_string());
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    eprintln!("launching → {url}");
    let mut b = BrowserSession::launch(&rt, &url).expect("launch");
    // let the page settle (sannysoft/creepjs run async detection)
    std::thread::sleep(Duration::from_millis(3500));
    b.ensure_live_target().ok();
    match b.eval(PROBE_JS) {
        Ok(json) => println!("PROBE {json}"),
        Err(e) => println!("PROBE_ERR {e}"),
    }
    // sannysoft prints a results table; scrape its text for a quick pass/fail read.
    if let Ok(txt) = b.eval("document.body ? document.body.innerText.slice(0,4000) : ''") {
        println!("--- page innerText (truncated) ---\n{txt}");
    }
    match b.screenshot() {
        Ok(b64) => {
            let bytes = envoyage_decode(&b64);
            std::fs::write(&out, &bytes).ok();
            let w = decode_png_width(&b64);
            println!("SCREENSHOT {out} bytes={} width_px={w}", bytes.len());
        }
        Err(e) => println!("SCREENSHOT_ERR {e}"),
    }
    b.close();
}

fn envoyage_decode(b64: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default()
}

//! Anti-detection hardening for the CDP-driven browser.
//!
//! Applied at the shared `attach_target` choke point so BOTH the local-spawn and
//! the remote-connect (Cloudflare Browser Rendering) paths — and every popup /
//! new tab — inherit it.
//!
//! Deliberately NARROW. Every JS shim is itself a detectable tell (a detector can
//! probe `Function.prototype.toString`, property descriptors, and iframe realms),
//! so we fix the high-signal leaks with launch flags + CDP overrides (which change
//! the *real* value with no JS trace) and inject the MINIMUM JS for what those
//! can't reach. Verified 2026 against Chrome 149.
//!
//! Fixed here:
//!  * `navigator.webdriver` → `--disable-blink-features=AutomationControlled`
//!    (local flag, C++-level, no trace). The JS shim re-nulls it too, but ONLY as
//!    the fallback for the remote CF browser, which cannot take that flag.
//!  * UA string "HeadlessChrome" (leaks in `navigator.userAgent` AND the UA request
//!    header) → `Network.setUserAgentOverride` with a matching `userAgentMetadata`
//!    so the string, the UA header, `Sec-CH-UA`, and `navigator.userAgentData` all
//!    stay consistent. Metadata versions are derived from the live binary so they
//!    never drift.
//!  * Headless screen/window envelope (`screen` == viewport, 800×600 default,
//!    `avail*` == `screen`, `outer*` == `inner*`) → a small injected shim gives a
//!    realistic desktop screen larger than the window with a menubar/chrome gap.
//!
//! Deliberately NOT done (verified non-issues on Chrome 149, or cost > benefit):
//!  * The `Runtime.enable` getter-trap does NOT fire on Chrome 149 (measured across
//!    console.log/debug/dir/table + Error.stack), so the console-capture rebuild it
//!    would require is deferred. Re-verify on Chrome upgrades.
//!  * `navigator.plugins`, `window.chrome`, WebGL vendor: headless=new already
//!    matches headful with a real GPU; faking them adds toString tells for no gain
//!    (a GPU-less host is a deployment concern — document a GPU requirement).
//!  * TLS / HTTP2 fingerprint is authentic real-Chrome; never proxy or rewrite it.

use serde_json::{json, Value};

/// Launch flags that harden the LOCALLY spawned browser (order-independent).
pub const LAUNCH_FLAGS: &[&str] = &[
    // navigator.webdriver becomes genuinely false at the C++ level (no JS tell).
    "--disable-blink-features=AutomationControlled",
    // Coherent locale for navigator.languages + the Accept-Language header.
    "--lang=en-US",
];

/// Strip the "Headless" token from a UA string ("HeadlessChrome/…" → "Chrome/…").
pub fn clean_user_agent(raw: &str) -> String {
    raw.replace("HeadlessChrome", "Chrome").replace("Headless", "")
}

/// `(platform, platformVersion, architecture)` for the host build, matching the
/// values `navigator.userAgentData.getHighEntropyValues` would report.
fn host_platform() -> (&'static str, &'static str, &'static str) {
    let arch = if cfg!(target_arch = "aarch64") { "arm" } else { "x86" };
    if cfg!(target_os = "macos") {
        ("macOS", "15.0.0", arch)
    } else if cfg!(target_os = "windows") {
        ("Windows", "15.0.0", arch)
    } else {
        ("Linux", "6.0.0", arch)
    }
}

/// A common desktop screen size for the host platform, guaranteed to exceed the
/// default 1280×800 window so `screen >= window` holds. macOS logical Retina vs a
/// typical Linux/Windows 1080p panel.
fn host_screen() -> (u32, u32) {
    if cfg!(target_os = "macos") { (1512, 982) } else { (1920, 1080) }
}

/// Build a `userAgentMetadata` block consistent with the running binary. `product`
/// is `Browser.getVersion`'s `product`, e.g. "Chrome/149.0.7827.201" — the version
/// is derived from it (page-independent, so it can never drift from the binary),
/// the platform/arch from the host build. Brands mirror a real Chrome (with the
/// GREASE entry) and contain NO "Headless".
pub fn user_agent_metadata(product: &str) -> Value {
    let full = product.rsplit('/').next().unwrap_or("149.0.0.0").to_string();
    let major = full.split('.').next().unwrap_or("149").to_string();
    // GREASE brand: Chrome varies this per release and it is low-signal by design
    // (a detector keying on it false-positives on real Chrome). Tracks Chrome 149;
    // refresh if a client-hints probe shows a mismatch.
    let grease = "Not)A;Brand";
    let (platform, platform_version, arch) = host_platform();
    json!({
        "brands": [
            { "brand": "Google Chrome", "version": major },
            { "brand": "Chromium", "version": major },
            { "brand": grease, "version": "24" },
        ],
        "fullVersionList": [
            { "brand": "Google Chrome", "version": full },
            { "brand": "Chromium", "version": full },
            { "brand": grease, "version": "24.0.0.0" },
        ],
        "fullVersion": full,
        "platform": platform,
        "platformVersion": platform_version,
        "architecture": arch,
        "bitness": "64",
        "model": "",
        "mobile": false,
        "wow64": false,
    })
}

/// The document-start shim injected via `Page.addScriptToEvaluateOnNewDocument`.
/// Runs in every frame before page scripts. Kept minimal and prototype-level so it
/// survives iframe re-derivation; each override is `configurable` and native-ish.
pub fn new_document_script() -> String {
    let (sw, sh) = host_screen();
    // Menubar/taskbar gap and browser-chrome height (macOS-ish, close enough).
    let avail_top = 38u32; // menubar
    let chrome_h = 85u32; // tab strip + toolbar
    format!(
        r#"(() => {{
  const nativeDef = (obj, prop, getter) => {{
    try {{ Object.defineProperty(obj, prop, {{ get: getter, configurable: true, enumerable: true }}); }} catch (e) {{}}
  }};
  // navigator.webdriver — belt-and-suspenders for the REMOTE CF browser, which
  // can't take --disable-blink-features. On the local path the flag already made
  // it false, so this is a no-op there. Prototype-level so iframes inherit it.
  try {{
    if (navigator.webdriver === true) {{
      nativeDef(Navigator.prototype, 'webdriver', () => false);
    }}
  }} catch (e) {{}}
  // Realistic desktop screen envelope. Headless reports screen==viewport (~800x600),
  // avail==screen (no menubar/taskbar), outer==inner (no browser chrome) — all tells.
  const SW = {sw}, SH = {sh}, AVAIL_TOP = {avail_top}, CHROME_H = {chrome_h};
  nativeDef(Screen.prototype, 'width', () => SW);
  nativeDef(Screen.prototype, 'height', () => SH);
  nativeDef(Screen.prototype, 'availWidth', () => SW);
  nativeDef(Screen.prototype, 'availHeight', () => SH - AVAIL_TOP);
  nativeDef(Screen.prototype, 'availLeft', () => 0);
  nativeDef(Screen.prototype, 'availTop', () => AVAIL_TOP);
  nativeDef(window, 'screenX', () => 0);
  nativeDef(window, 'screenY', () => 0);
  nativeDef(window, 'screenLeft', () => 0);
  nativeDef(window, 'screenTop', () => 0);
  // outerWidth/Height track the inner viewport plus browser chrome, and never 0.
  nativeDef(window, 'outerWidth', () => window.innerWidth || SW);
  nativeDef(window, 'outerHeight', () => (window.innerHeight ? window.innerHeight + CHROME_H : SH));
}})()"#,
        sw = sw,
        sh = sh,
        avail_top = avail_top,
        chrome_h = chrome_h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_ua_strips_headless() {
        let raw = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                   (KHTML, like Gecko) HeadlessChrome/149.0.0.0 Safari/537.36";
        let clean = clean_user_agent(raw);
        assert!(!clean.to_lowercase().contains("headless"), "clean UA still has Headless: {clean}");
        assert!(clean.contains("Chrome/149.0.0.0"));
    }

    #[test]
    fn metadata_derives_version_and_has_no_headless() {
        let m = user_agent_metadata("Chrome/149.0.7827.201");
        assert_eq!(m["fullVersion"], "149.0.7827.201");
        let brands = m["brands"].as_array().unwrap();
        assert!(brands.iter().any(|b| b["brand"] == "Google Chrome" && b["version"] == "149"));
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.to_lowercase().contains("headless"), "metadata leaks Headless: {s}");
        // architecture is mandatory for the CDP override.
        assert!(m["architecture"].is_string() && !m["architecture"].as_str().unwrap().is_empty());
    }

    #[test]
    fn new_document_script_is_self_contained() {
        let js = new_document_script();
        assert!(js.contains("webdriver"));
        assert!(js.contains("Screen.prototype"));
        // Must be a single evaluable expression (IIFE), no template leftovers.
        assert!(js.trim_start().starts_with("(()"));
        assert!(!js.contains("{sw}"));
    }
}

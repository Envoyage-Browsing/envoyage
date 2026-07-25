# Anti-detection (stealth)

envoyage drives a **real** Chrome/Chromium binary over CDP. That is a structural
advantage: the TLS (JA3/JA4) and HTTP/2 fingerprints are authentically Chrome's,
so the whole class of network-layer bot detection that defeats HTTP-client
scrapers does not apply — **as long as we never interpose a proxy or rewrite
requests.** What *does* leak is the automation/headless surface: `navigator.
webdriver`, a `HeadlessChrome` User-Agent, and headless screen/window defaults.
envoyage normalizes those.

Everything here is applied at the shared `attach_target` choke point
([`src/browser.rs`](../src/browser.rs)), so it covers **both** the local-spawn
(`launch`) and remote-connect (`connect`, e.g. Cloudflare Browser Rendering)
paths, and every followed popup / new tab. The rationale and the (deliberately
narrow) scope live in [`src/stealth.rs`](../src/stealth.rs).

## What we fix

| Tell | Fix | Reaches the cloud path? |
|------|-----|-------------------------|
| `navigator.webdriver === true` | `--disable-blink-features=AutomationControlled` launch flag (C++-level, no JS trace) | Flag is local-only; a prototype-level JS shim re-nulls it on the remote path |
| UA string / header contains `HeadlessChrome` | `Network.setUserAgentOverride` with a matching `userAgentMetadata` (versions derived from the live binary so they never drift) | ✅ CDP-level |
| Sec-CH-UA / `navigator.userAgentData` consistency | same override supplies brands + fullVersionList + platform | ✅ |
| `Accept-Language` / `navigator.languages` | `--lang=en-US` + `acceptLanguage: "en-US,en"` (bare — Chrome appends the q-weights) | ✅ (acceptLanguage) |
| Headless screen/window (`screen` == viewport ~800×600, `avail*` == `screen`, `outer*` == `inner*`) | injected document-start shim: realistic desktop screen > window, menubar gap, non-zero chrome | ✅ CDP-injected |

Input is also humanized so behavioral scoring (DataDome / Kasada / PerimeterX)
has a real trajectory to read: clicks glide the pointer along a jittered eased
path and land off dead-center with a press→release dwell; typing fires real
per-character `keydown`/`keyup` (not a single paste-like `insertText`); scrolling
is a burst of eased wheel ticks off-center. See the input primitives in
`src/browser.rs`.

## Verifying

The repo ships measurement harnesses (not published):

```bash
cargo run --example fingerprint-probe -- https://bot.sannysoft.com out.png
cargo run --example fingerprint-probe -- https://bot-detector.rebrowser.net/ out.png
cargo run --example input-realism            # counts the DOM events input produces
cargo run --example connect-probe -- <ws-url> # proves the remote/connect path is covered
```

As of Chrome 149 the engine passes the full bot.sannysoft.com table and every
rebrowser-bot-detector check (`runtimeEnableLeak`, `navigatorWebdriver`,
`viewport`, `useragent`, … all green).

## Deliberately NOT done

Every JS shim is itself probeable (`Function.prototype.toString`, descriptor
shape, iframe realms), so more spoofing past a point *lowers* the trust score.
We fix the high-signal leaks with flags/CDP and inject the minimum JS.

- **`Runtime.enable` getter-trap** — the famous rebrowser leak does **not** fire
  on Chrome 149 (measured across console.log/debug/dir/table + Error.stack; the
  rebrowser `runtimeEnableLeak` test reads green). The console-capture rebuild it
  would require is deferred. Re-verify on Chrome upgrades.
- **plugins / `window.chrome` / WebGL vendor shims** — `--headless=new` with a
  real GPU already matches headful; faking them adds tells for no gain.
- **TLS / HTTP2** — authentic real-Chrome fingerprint. Never add an HTTP proxy or
  `Network.setExtraHTTPHeaders` for stealth — either would *degrade* it.

## Known limitations

- **Hosted (Cloudflare Browser Rendering) IP reputation is a hard ceiling.** The
  JS/UA/screen stealth above applies over the `connect` path, but Cloudflare
  Browser Rendering egresses from datacenter IPs that anti-bot vendors — including
  Cloudflare's own Turnstile / Bot Management — do not trust. No CDP or JS fix
  overcomes IP reputation. Route anti-bot-protected sites to the **local** path
  with a residential `--proxy-server`, or hand off to a human on challenge.
- **GPU-less hosts** (e.g. a headless Linux server with no GPU) fall back to
  SwiftShader, whose WebGL renderer string is a bot signal. Provision a GPU, or
  accept the tell. On the Cloudflare path the GPU is theirs.
- **Injected-marker surface.** The accessibility snapshot still stamps a
  `data-immorterm-ref` attribute and reads a `window.__ENVOYAGE_AX_MASK` global in
  the page's main world during `read_page`. rebrowser rates the related
  `mainWorldExecution` surface as inert, but a determined detector could probe for
  these post-snapshot. Moving the snapshot/ref bookkeeping into an isolated world
  (`Page.createIsolatedWorld` + a `WeakMap`) is the clean fix — tracked, deferred
  (the file is shared verbatim with the SDK, so it needs cross-repo coordination).
- **Trace-free fingerprint spoofing requires a patched binary** (Camoufox et al.),
  which is out of scope for a stock-Chrome engine.

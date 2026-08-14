# Changelog

All notable changes to the **envoyage engine** (the Rust crate + `envoyage` CLI)
are documented here. The `@envoyage/browser` SDK has its own changelog at
[`sdk/CHANGELOG.md`](sdk/CHANGELOG.md).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Bounded website crawling** behind one Envoyage contract: durable exact
  idempotency, start/read/cancel over REST and MCP, normalized page sections,
  links and ordered media, opaque pagination, public-host and limit checks, and
  job-scoped raster downloads with redirect/DNS checks and exact cached replay.
  The first provider adapter targets an unmodified private Firecrawl v2 service;
  its credentials, job IDs and response shape stay server-side. See
  [`docs/crawling.md`](docs/crawling.md).
- **Anti-detection layer** ([`src/stealth.rs`](src/stealth.rs), applied in
  `attach_target` so both the local-spawn and remote-`connect` paths + popups
  inherit it): `--disable-blink-features=AutomationControlled` +
  `--lang=en-US` launch flags; a `Network.setUserAgentOverride` that strips the
  `HeadlessChrome` token and supplies a `userAgentMetadata` derived from the live
  binary (UA string, UA header, Sec-CH-UA and `navigator.userAgentData` all stay
  consistent); and a document-start shim giving a realistic screen/window
  envelope. Passes the full bot.sannysoft.com table and every rebrowser-bot-
  detector check on Chrome 149. See [`docs/stealth.md`](docs/stealth.md).
- **Humanized input**: clicks trace a jittered eased pointer path and land
  off-center with a press→release dwell; typing fires real per-character
  `keydown`/`keyup` (was a single paste-like `Input.insertText`); scrolling is a
  burst of eased off-center wheel ticks. Defeats behavioral bot scoring.
- **Per-session remote browsers**: the `x-envoyage-cdp-url` request header is now
  honored (via `state::set_session_cdp_url`/`cdp_url_for`), so each remote session
  drives its OWN Cloudflare Browser Rendering browser instead of every session
  collapsing onto the one process-global `--cdp-url` (or silently spawning local).

### Fixed
- **GIF export was completely broken** since the screencast switched to JPEG: the
  recorder decoded frames as PNG and the `image` crate lacked the `jpeg` feature,
  so every `browser_gif` export failed. Enabled `jpeg`, switched to
  format-sniffing decode, and corrected the replay frame delay (33 → 66 ms; GIFs
  were playing ~2× speed).
- **Input latency**: human input is now dispatched the instant it arrives (a wake
  channel parks the pump) instead of waiting out the up-to-66 ms frame tick.
- The `about:blank`-first boot + navigate ensures the stealth pre-load script
  covers the FIRST page (a script added via `addScriptToEvaluateOnNewDocument`
  only affects future documents).

### Changed
- Live screencast JPEG quality 75 → 88 (sharper small text / 1px borders at 1:1
  CSS-px capture); `Page.screencastFrameAck` is now fire-and-forget (no blocking
  round-trip per frame while the pump holds the browser mutex).

## [0.1.1] - 2026-07-11

First `@envoyage/cli` release published tokenlessly via npm Trusted Publishing (OIDC). Carries the serve/security work below.

### Added
- **Multi-session remote serve surface** (`envoyage serve --http-port N`): one
  process multiplexes N independent browsers, keyed by the client's
  `Mcp-Session-Id` header. MCP-over-HTTP driving (`POST /mcp`) plus a per-session
  live view — `GET /sessions/{id}/events` (SSE) and `POST /sessions/{id}/input`.
- **Keyframe-on-connect replay**: a viewer joining a session's SSE stream is
  immediately caught up with the current frame + narration + pause state + any
  active handoff banner, instead of a blank view until the next visual change.
- Bearer auth on the remote surface via `ENVOYAGE_AUTH_TOKEN` (unset → no auth,
  local dev); `ENVOYAGE_HTTP_HOST` to bind beyond loopback.
- `@envoyage/cli` `0.1.0` published (npm meta package + per-platform binaries).

### Fixed
- **Remote CDP over HTTP**: MCP-over-HTTP tool dispatch runs on a blocking
  thread, so the remote-CDP path (which builds its own tokio runtime and blocks
  on it) no longer panics with "Cannot start a runtime from within a runtime".
- Wired the live-view SSE surface end-to-end (per-session routes, fetch bind,
  CORS reflecting the caller's Origin for the browser/Worker SDK).

### Security
- **AX-listing password-blindness**: password values are never emitted from
  `browser_read_page` / `browser_find`. An `<input type="password">` is always
  masked (`masked: true`, no value). Configurable masking via
  `ENVOYAGE_MASK_ALL_INPUTS`, `ENVOYAGE_MASK_SELECTOR`, and the
  `[data-envoyage-mask]` attribute. All input values are additionally suppressed
  while a session is paused/handed-off, so a secret typed into a non-password
  field can't leak either. (Enforced server-side in Rust.)

## [0.1.0]

Initial engine release: `BrowserSession` crate + `envoyage` CLI — drive a real
headless browser from any AI agent over MCP (stdio), with a mascot-neutral
cursor/narration/handoff protocol and a WS live-view surface.

[Unreleased]: https://github.com/Envoyage-Browsing/envoyage/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Envoyage-Browsing/envoyage/releases/tag/v0.1.0

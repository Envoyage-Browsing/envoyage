# Changelog

All notable changes to the **envoyage engine** (the Rust crate + `envoyage` CLI)
are documented here. The `@envoyage/browser` SDK has its own changelog at
[`sdk/CHANGELOG.md`](sdk/CHANGELOG.md).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

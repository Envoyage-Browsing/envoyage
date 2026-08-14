# Changelog

All notable changes to `@envoyage/browser` are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] — 2026-08-14

### Added
- Optional crawl adapter selection and normalized Product/canonical/gallery
  fields for verified commerce-site inventories.

## [0.2.0] — 2026-08-14

### Added
- `createCrawlClient()` and `CrawlClient.start/read/cancel/downloadAsset` for
  Envoyage's bounded website crawl contract.
- Typed crawl requests, limits, normalized page/section/media results, progress,
  opaque cursors and exact raster downloads. The core remains Workers-safe and
  uses only `fetch`.

> **API stability:** the public API — `createSession()` / `BrowserSession` and
> `createCrawlClient()` / `CrawlClient` — is **stable across patch versions**.
> A patch never changes the API surface; any
> breaking change gets a minor/major bump **and** its own CHANGELOG entry. Pin to
> `~0.2.0` (or an exact version) and patch-upgrade safely.

## [0.1.2] — 2026-07-11

Security. **No API change** (adds an optional `PageElement.masked` field).

### Security
- Never expose password-input values: `AX_SNAPSHOT_JS` now always drops the
  typed value of an `<input type="password">` (emits `masked: true`, no value) —
  the model can no longer read a password via `readPage()`/`find()`.
- Configurable input masking (PostHog-session-replay style): mask a field's
  value via `maskAllInputs`, a `maskSelector` CSS selector, or the
  `[data-envoyage-mask]` attribute convention. The engine drives these from the
  `ENVOYAGE_MASK_ALL_INPUTS` / `ENVOYAGE_MASK_SELECTOR` env vars.
- AX values are suppressed while a session is paused: `readPage()`/`find()` now
  strip ALL input values during a handoff (matching the existing screenshot
  suppression), so a secret typed into a non-password field can't leak either.

### Added
- Optional `masked?: boolean` on `PageElement` for consumers parsing
  `AX_SNAPSHOT_JS` output directly.

## [0.1.1] — 2026-07-11

Packaging / CI only. **No API change.**

### Changed
- `publishConfig.access: "public"` so the scoped `@envoyage/browser` package
  publishes public.
- `prepack` builds `dist/` and strips test files, so the tarball ships
  compiled JS + `.d.ts` only (no `src/`, no `*.test.js`).
- Published via npm **Trusted Publishing** (OIDC) — no long-lived `NPM_TOKEN`.

## [0.1.0] — 2026-07-11

Initial release.

### Added
- `createSession({ endpoint, cdpUrl, token?, sessionId?, fetch? })` — create a
  session against a running `envoyage serve` engine. Points the engine at a CDP
  browser (Cloudflare Browser Run or any Chrome); the SDK never touches the
  browser directly.
- **Driving methods** on `BrowserSession` (all over `POST /mcp`, JSON-RPC
  `tools/call`): `open`, `click`, `formInput`, `type`, `key`, `scroll`,
  `screenshot`, `readPage` (alias `snapshot`), `find`, `waitFor`, `tabsList`,
  `tabsSwitch`, `console`, `network`, `upload`, `requestHuman`, `waitForHuman`
  (alias `resume`), and `close`.
- **SSE live-view event stream** (`GET /sessions/:id/events`): `frame`,
  `cursor`, `narration`, `human-needed`, `state`, and a synthetic `close`.
  Consume via `on()`/`off()`, `frames()`, or `events()`. The engine replays the
  current frame + any active handoff banner on connect (keyframe-on-connect).
- `sendInput()` — forward one human input event (`click` | `key` | `scroll` |
  `control`) to the engine over `POST /sessions/:id/input`, for driving during a
  handoff.
- **Shared detection files** re-exported verbatim from the engine:
  `AX_SNAPSHOT_JS`, `HUMAN_NEEDED_JS`, plus `classifyHandoff()` /
  `parseHumanNeeded()` — byte-identical to the engine's `src/shared/*.js`.
- Node-only `launch()` (`@envoyage/browser/launch`) for the local OSS case where
  the engine spawns and owns a local Chromium. Kept out of the Worker bundle.

[0.1.1]: https://github.com/Envoyage-Browsing/envoyage/releases/tag/sdk-v0.1.1
[0.1.0]: https://github.com/Envoyage-Browsing/envoyage/releases/tag/sdk-v0.1.0

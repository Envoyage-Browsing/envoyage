# AGENTS.md — envoyage engine

Policy for anyone (human or AI agent) changing this repo. Vendor-neutral; this
is public OSS. The full dev + release runbook lives in
[CONTRIBUTING.md](CONTRIBUTING.md) — this file is the *documentation-discipline*
contract, not a duplicate of it.

## Documentation is part of the change, not a follow-up

No commit or PR that adds or changes behavior is complete until the docs move
with it. Before you consider a change done:

- **README** — updated if the change touches user-facing usage, quickstart,
  tools, config, or env vars. Specifically: a new env var → the **Configuration**
  table; a new MCP tool → the **tool surface** table; a new WS/SSE event → the
  **protocol** table.
- **CHANGELOG.md** — updated (Keep a Changelog + semver). Engine / crate / CLI
  changes go in the **root** [`CHANGELOG.md`](CHANGELOG.md); SDK changes go in
  [`sdk/CHANGELOG.md`](sdk/CHANGELOG.md). Two separate release lanes, two
  changelogs — don't cross them.
- **docs/** — updated for the affected area.
- **API collections** — whenever the change touches the HTTP / MCP / SSE serve
  surface, the OpenAPI spec ([`docs/api/openapi.yaml`](docs/api/openapi.yaml))
  **and** the Bruno collection ([`api-collections/envoyage/`](api-collections/envoyage/))
  are source-of-truth and must stay in sync. A new route, param, tool, or event
  → update **both**. See [`docs/api/README.md`](docs/api/README.md).

A PR touching behavior without the matching doc change is incomplete, not
"docs later."

## Repo map

| Path | What |
|------|------|
| `src/` | Rust engine + crate (`BrowserSession`, CDP transport, serve surface). |
| `src/serve/` | Multi-session remote surface: MCP-over-HTTP + per-session SSE + `POST /input`. |
| `src/shared/*.js` | Detection heuristics — single source, `include_str!`'d into Rust **and** imported by the SDK. |
| `sdk/` | `@envoyage/browser` — thin Workers-safe TS client. |
| `npm/` | `@envoyage/cli` meta package + per-platform binaries. |
| `docs/api/` | OpenAPI spec + serve-surface API README (source of truth). |
| `api-collections/envoyage/` | Bruno collection for the serve surface. |

Full dev setup, green-before-commit gates, and the tokenless publish process →
[CONTRIBUTING.md](CONTRIBUTING.md).

## Hard rules (also enforced in CONTRIBUTING)

- **Never break the crate's public API** (`BrowserSession`) without a major bump
  — ImmorTerm links it directly.
- **Detection heuristics live once** in `src/shared/*.js`. Never inline a copy in
  Rust or TS; edit the shared file.
- **Password-blindness is server-side.** While a session is paused/handed-off the
  serve surface returns no screenshot/AX bytes and no field values. Enforce in
  Rust, never rely on the JS.
- **No `git stash`** — parallel agents share the tree. Commit named paths:
  `git commit <file> -m "…"`.
- **Read before you edit.** Trace the real flow first.

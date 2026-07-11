---
name: dev-up
description: Start the envoyage umbrella (engine + cloud) — one Tilt that boots BOTH repos together on UI port 10442. The engine has no standalone Tilt; the umbrella scripts live in the sibling envoyage-cloud. ALWAYS use ./dev_up.sh, never `tilt up` directly.
allowed-tools: Bash, Read
---

# Dev Up — start the envoyage umbrella (engine + cloud)

**This engine repo has NO standalone Tilt.** It's a Rust headless-Chromium driver (`Cargo.toml`, no Tiltfile of its own). It runs locally only as a resource inside the **whole-stack umbrella**: one Tilt that serves the cloud stack (dashboard + payment + landing + blog + storybook) *and* this engine together. The umbrella scripts live in the sibling **envoyage-cloud** (`dev.Tiltfile` includes the cloud Tiltfile and picks up this engine's prebuilt binary via `../envoyage`), so this skill `cd`s there.

**CRITICAL**: NEVER run `tilt up` directly and NEVER `kill` Tilt / portless by hand. Always use `./dev_up.sh`. Multiple Tilt projects share portless on `:1355` — a stray `tilt up` fights over portless routes and orphans a dashboard.

## Coexisting Tilt UI ports

| Project | Tilt UI port(s) |
|---|---|
| delulus | 10370 |
| builders-stack | 10380 |
| krispyai (cloud / umbrella) | 10440 · 10441 · 10442 |
| ringtail (tool / site / umbrella) | 10450 · 10451 · 10452 |
| envoyage-cloud | 10441 |
| **envoyage umbrella (engine + cloud)** | **10442** |

> ⚠ **Known collision**: envoyage's 10441/10442 clash with krispyai's cloud/umbrella (10441/10442) — **run one at a time**.

## Usage

```bash
cd ~/Development/envoyage-org/envoyage-cloud
./dev_up.sh            # Tilt UI on http://localhost:10442
```

The script exports the right PATH (`/opt/homebrew/bin` for portless + `~/.bun/bin` for bun), so it works from a non-interactive agent shell. It `exec`s a long-running `tilt up` — from the agent shell run it **in the background**. `dev_up.sh` also starts a local Docker Postgres, applies migrations, and `cargo build --release`s this engine if the binary is missing, before bringing Tilt up.

## Services (all via portless, no pinned ports)

| Resource | URL | What |
|---|---|---|
| engine | ws://localhost:9223 · http://localhost:9224/mcp | **this repo** — Rust headless-Chromium driver; the dashboard drives + watches it over MCP-over-HTTP + SSE |
| dashboard | http://localhost:5747 · http://dashboard.envoyage.localhost:1355 | cloud — Next.js cockpit |
| payment · landing · blog · storybook | `*.envoyage.localhost:1355` | cloud — billing · marketing · MDX blog · design system |

Tilt UI: http://localhost:10442

## Pre-flight

- **`../envoyage` (this repo) must stay a sibling of `envoyage-cloud`** — the umbrella picks up this engine's binary by relative path; the two repos MUST stay siblings inside `envoyage-org/`. If absent, `dev.Tiltfile` skips the engine with a warning and the cloud stack boots alone (the live-browser view won't stream).
- **portless must be up** (shared on `:1355` across all projects — `portless --version` should print). If missing: `npm install -g portless`.
- **Docker must be running** for the cloud's local Postgres + migrations.
- **Check for a stray umbrella Tilt first**: `ps aux | grep "[t]ilt up"` matching `--port 10442`; don't start a second.

## Teardown

`./dev_down.sh` (in envoyage-cloud) — stops the umbrella, which covers both the engine and the cloud. (See the `dev-down` skill.)

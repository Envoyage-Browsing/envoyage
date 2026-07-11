---
name: dev-restart
description: Restart the envoyage umbrella (engine + cloud) — down then up on the same UI port (10462). The engine has no standalone Tilt; the umbrella scripts live in the sibling envoyage-cloud. Use this to pick up dependency/config changes across the whole stack. ALWAYS use the scripts, never `tilt up`/`tilt down` directly.
allowed-tools: Bash, Read
---

# Dev Restart — restart the envoyage umbrella (engine + cloud)

**This engine repo has NO standalone Tilt** — it runs only as a resource inside the whole-stack umbrella. `dev_*` is that umbrella (engine + cloud in one Tilt, UI port **10462**), and its scripts live in the sibling **envoyage-cloud**, so this skill `cd`s there.

**CRITICAL**: NEVER run `tilt down` / `tilt up` directly and NEVER `kill` Tilt / portless / dev-server process groups by hand. Always go through the scripts. Multiple Tilt projects share portless on `:1355` — a stray `tilt up` fights over portless routes and orphans a dashboard.

## Coexisting Tilt UI ports

| Project | Tilt UI port(s) |
|---|---|
| delulus | 10370 |
| builders-stack | 10380 |
| krispyai (cloud / umbrella) | 10440 · 10441 · 10442 |
| ringtail (tool / site / umbrella) | 10450 · 10451 · 10452 |
| envoyage-cloud | 10461 |
| **envoyage umbrella (engine + cloud)** | **10462** |

## Usage

```bash
cd ~/Development/envoyage-org/envoyage-cloud
./dev_down.sh && ./dev_up.sh       # same UI port 10462
```

Both scripts export the right PATH (`/opt/homebrew/bin` for portless + `~/.bun/bin` for bun), so they work from a non-interactive agent shell. `dev_up.sh` `exec`s a long-running `tilt up` — from the agent shell run the pair **in the background**.

## When to use

The clean way to pick up **dependency or Tiltfile/config changes** across the whole stack — a plain live edit won't reload them. A restart tears both the cloud stack and this engine down and brings them back on the same routes.

**Engine code changes need a rebuild**: `dev_up.sh` reuses an existing `../envoyage/target/release/envoyage` binary as-is (it only builds when the binary is missing). To pick up Rust changes, rebuild first: `cargo build --release` here in this repo, then restart the umbrella.

## Pre-flight & stray Tilts

- **`../envoyage` (this repo) must stay a sibling of `envoyage-cloud`** — the umbrella picks up this engine's binary by relative path; keep both repos siblings inside `envoyage-org/`.
- **portless must be up** (shared on `:1355`). If missing: `npm install -g portless`.
- **Docker must be running** for the cloud's local Postgres + migrations.
- `dev_down.sh` only stops the Tilt `dev_up.sh` started. An umbrella Tilt launched some other way is **untracked** and survives the down step, leaving two Tilts fighting over port 10462 — check first with `ps aux | grep "[t]ilt up"` and match `--port 10462`.

---
name: dev-down
description: Stop the envoyage umbrella (engine + cloud) cleanly — one Tilt on UI port 10462 that covers BOTH repos. The engine has no standalone Tilt; the umbrella scripts live in the sibling envoyage-cloud. Use this instead of killing tilt/portless by hand.
allowed-tools: Bash, Read
---

# Dev Down — stop the envoyage umbrella (engine + cloud)

**This engine repo has NO standalone Tilt** — it runs only as a resource inside the whole-stack umbrella. `dev_*` is that umbrella (engine + cloud in one Tilt), and its scripts live in the sibling **envoyage-cloud**, so this skill `cd`s there.

**CRITICAL**: NEVER `kill` Tilt / portless / dev-server process groups by hand, and never run `tilt down` directly. Use `./dev_down.sh`.

## Usage

```bash
cd ~/Development/envoyage-org/envoyage-cloud
./dev_down.sh
```

## What it does

- Stops the umbrella's tracked Tilt (its UI on port **10462**), which brings down **both** the cloud stack and this engine together (the Rust engine runs as a Tilt resource, so it dies with Tilt). Other projects' Tilts (delulus 10370, builders-stack 10380, ringtail 10450/10451/10452, krispyai 10440 · 10441 · 10442) and the standalone envoyage-cloud 10461 Tilt are untouched.
- **Never** stops portless — it's the shared `:1355` proxy used by every project.
- The cloud's local Docker Postgres (`envoyage-postgres`) is **left running** — stop it yourself with `docker stop envoyage-postgres` if you want it down.

## Note on stray Tilts

`dev_down.sh` only knows the Tilt `dev_up.sh` started. An umbrella Tilt launched some other way (a manual `tilt up -f dev.Tiltfile`, an old session) is **untracked** and survives this — check with `ps aux | grep "[t]ilt up"` and match `--port 10462` before assuming a clean slate.

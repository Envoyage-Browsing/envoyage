# Contributing to envoyage

Thanks for hacking on envoyage — the vendor-neutral browser-driving engine. This
doc is the maintainer runbook (humans **and** AI agents): dev setup, the rules,
and the exact publish process.

## Layout

| Path | What |
|------|------|
| `src/` | The Rust engine + crate (`BrowserSession`, CDP transport, serve surface). Consumed directly by ImmorTerm — **never break the crate's public API without a major bump.** |
| `src/serve/` | The multi-session remote surface: MCP-over-HTTP (driving) + per-session SSE (`/sessions/:id/events` frames/cursor/narration/handoff) + `POST /sessions/:id/input`. |
| `src/shared/*.js` | Detection heuristics (human-needed, AX snapshot), `include_str!`'d into Rust **and** imported by the SDK — single source, so they can't diverge. |
| `sdk/` | `@envoyage/browser` — the thin, Workers-safe TS client (`fetch` + SSE only in the core path). |
| `npm/` | The `@envoyage/cli` meta package + per-platform binary packages. |
| `examples/` | Consumer references. |

## Dev setup

**Engine (Rust):**
```bash
cargo build
cargo clippy --all-targets -- -D warnings
cargo test                     # unit + mock-CDP e2e (no live browser needed)
cargo test -- --ignored screencast --test-threads=1   # live-browser smoke (needs Chrome + net)
```

**SDK (TypeScript):**
```bash
cd sdk
npm install
npm run build                  # runs gen-shared (mirrors src/shared/*) + tsc
npm test                       # node --test on the built output
```

## Rules

- **Read before you edit.** Trace the real flow first.
- **Never break the crate's public API** (`src/browser.rs` `BrowserSession`) without a major-version bump — ImmorTerm links it directly.
- **Detection heuristics live once** in `src/shared/*.js`. Don't inline copies in Rust or TS; edit the shared file and let `include_str!` / the SDK's `gen-shared` pick it up.
- **Password-blindness is server-side.** While a session is in a human-needed/paused state, the serve surface must return no screenshot/AX bytes and no field values to the client. Enforce in Rust, never rely on the JS.
- Green before commit: `cargo clippy -D warnings` + `cargo test`, and for SDK changes `npm run build && npm test`.
- **No `git stash`** in this repo (parallel agents share the tree — stash can clobber). Commit named paths instead: `git commit <file> -m "…"`.

## Releasing

Both npm lanes are **tokenless** via [npm Trusted Publishing](https://docs.npmjs.com/trusted-publishers) (GitHub Actions OIDC + provenance). See the [README → Releasing](README.md#releasing) table for triggers. Short version:

- **SDK:** bump `sdk/package.json` → `git tag sdk-vX.Y.Z && git push origin main sdk-vX.Y.Z` → `publish-sdk.yml` publishes (guarded: tag must equal package.json version).
- **CLI:** bump `Cargo.toml` → `gh workflow run release.yml -f version=X.Y.Z`.

### First publish of a NEW package name (one-time bootstrap)

npm won't let you configure a Trusted Publisher until the package exists, so each
new name needs exactly **one** credentialed publish, then it's tokenless forever.

1. **Generate a Classic → Automation token** on npmjs.com (Access Tokens →
   Generate New Token → Classic → **Automation**). Classic-Automation is the only
   type that (a) bypasses 2FA and (b) can create a brand-new package — a
   *granular* token scoped to "select packages" **cannot** create one.
2. **Publish once** with it. npm's CLI reads auth from `.npmrc`, **not** from
   `NODE_AUTH_TOKEN` — so use a temporary registry line:
   ```bash
   cd sdk   # or the package dir under npm/
   printf '//registry.npmjs.org/:_authToken=${NPM_TOKEN}\n' > .npmrc
   NPM_TOKEN=npm_yourAutomationToken npx -y npm@latest publish --provenance --access public
   rm .npmrc
   ```
3. **Configure the Trusted Publisher** (npmjs.com → package → Settings → Trusted
   Publisher → GitHub Actions): org `Envoyage-Browsing`, repo `envoyage`,
   workflow file `publish-sdk.yml` (SDK) or `release.yml` (CLI).
4. **Revoke the token** — CI never needs a standing token again.

### Gotchas (all real, all cost us time)

- Scoped packages default to **private**; set `publishConfig.access: "public"`.
- `dist/` is gitignored → the package's `prepack` must rebuild it at pack time.
- Trusted Publishing needs npm ≥ 11.5.1 → CI runs `npx -y npm@latest publish`.
- The org (`@envoyage`) must exist first; publishing to a missing scope is a
  `404 on PUT`, not a permissions error.

## Filing issues / PRs

Keep PRs focused. Include a test for non-trivial logic (the mock-CDP harness in
`tests/` needs no live browser). Note any crate-API or event-shape change in the
PR description so downstream consumers (ImmorTerm, ringtail) know when a pin bump
is safe.

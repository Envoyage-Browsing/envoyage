# envoyage consumer example

A small, self-contained example of consuming [envoyage](../../README.md) from a
TypeScript product **via the published `@envoyage/browser` SDK** — the shape a
consumer copies. It mirrors the real dashboard consumer,
[`envoyage-cloud`'s `LiveView.tsx`](https://github.com/Envoyage-Browsing/envoyage-cloud):
`createSession({ endpoint, cdpUrl })`, drive, subscribe to the live-view events,
handle a human handoff, `close()`.

Two halves:

- **`driver.ts`** — the **agent side** (headless). `createSession` against a
  running `envoyage serve`, then drive by hand: `open` → `readPage` → `find` →
  `click`. Subscribes to `frame | cursor | narration | state` (logged to the
  console) and sketches the `human-needed` handoff (`sendInput` takeover). It's
  headless so it runs in a plain terminal — the *wiring* is the point.
- **`index.html` + `viewer.ts`** — the **UI side**. The framework-free version of
  `LiveView.tsx`: it `createSession`s, paints each `frame` onto a `<canvas>`,
  glides a placeholder mascot to each `cursor`, shows the handoff banner on
  `human-needed`, and forwards the human's clicks/keys via `sendInput()` during a
  takeover. The mascot is an orange dot — search for `consumer seam:` to see
  exactly where you swap in your own sprite.

Both use the **same** SDK and the **same** events; one just renders to a canvas.

## Run it

```bash
npm install                    # @envoyage/browser + tsx + typescript

# 1. Start an engine, pointed at a browser:
#      envoyage serve --http-port 8788 --cdp-url wss://your-browser/cdp
#    or local-spawn (engine owns a local Chrome, cdpUrl:"local"):
#      envoyage serve --http-port 8788

# 2a. Agent side — drive a task and watch the events stream:
npx tsx driver.ts

# 2b. UI side — open the live view (any TS-aware dev server):
npx vite .                     # then open the printed URL → index.html
```

Env for `driver.ts`: `ENVOYAGE_ENDPOINT` (default `http://127.0.0.1:8788`),
`ENVOYAGE_CDP_URL` (default `local`), `ENVOYAGE_AUTH_TOKEN` (if the engine sets a
bearer), `TARGET_URL` (default `https://example.com`).

## What it demonstrates (and what it doesn't)

- **The SDK is the whole consumer surface** — no MCP client, no framework, just
  `import { createSession } from "@envoyage/browser"`. The engine is the only
  thing you host; the SDK is a `fetch` + SSE client.
- **The live-view event set** — `frame | cursor | narration | human-needed |
  state`, wired exactly as `LiveView.tsx` wires them.
- **Bring-your-own-mascot** — envoyage emits `cursor {x,y,action}`; the consumer
  maps page px → display px and places its own sprite. Swap the dot and nothing
  else changes.
- **The handoff** — `human-needed` + `sendInput()` during a takeover; a
  password/OTP/CAPTCHA/OAuth wall pauses the engine server-side and the model
  never sees the screen or the secret.

Honesty notes: the handoff in `driver.ts` is **scripted** (a canned `Enter`),
not a real human — a real consumer shows the live view and lets the human drive.
The example doesn't run in CI, but the TS typechecks (`npm run typecheck` after
`npm install`).

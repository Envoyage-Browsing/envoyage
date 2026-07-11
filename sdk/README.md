# @envoyage/browser

A **thin, Workers-safe** client for the [Envoyage](../README.md) browser-driving
engine. Point it at a running `envoyage serve` and drive a real remote browser
(Cloudflare Browser Run, or any Chrome you expose over CDP) — navigate, read,
click, type, watch it live, and hand off to a human — all over `fetch` + SSE.

The engine is the single source of driving, detection, and tab-following. This
SDK **marshals to it**: no NAPI, no WASM, no port of the driving logic. The core
path (`@envoyage/browser`) imports **zero** node built-ins and uses **only**
`fetch` and an SSE reader over a streamed `Response` — so it bundles into a
Cloudflare Worker unchanged. A separate `@envoyage/browser/launch` entry uses
`node:child_process` for the local OSS case and is kept out of the Worker
bundle.

## Topology

```
                        ┌──────────────────────────────────────────┐
  Cloudflare Worker     │        envoyage serve (the engine)        │
  ┌────────────────┐    │  ┌────────────┐   CDP wss   ┌───────────┐ │
  │ @envoyage/     │    │  │ MCP tools  │────────────▶│  browser  │ │
  │   browser      │───▶│  │ /mcp       │             │ (CF Run / │ │
  │ (fetch + SSE)  │◀───│  │ /events    │◀── frames ──│  Chrome)  │ │
  └────────────────┘    │  │ /input     │             └───────────┘ │
       points at ─────▶ │  └────────────┘                           │
       the engine URL   └──────────────────────────────────────────┘
     runs on: k8s / VPS / a CF Container (anywhere that can spawn or reach Chrome)
```

The Worker (or any Node service) holds **no browser** — it points the SDK at an
engine URL. Run the engine wherever a browser lives: a k8s pod, a VPS, or a
Cloudflare Container. For the fully-hosted case the engine drives a **Cloudflare
Browser Run** endpoint; pass its CDP `wss://` URL as `cdpUrl`.

## Install

```bash
# Git dependency (no npm publish needed yet):
npm install "git+https://github.com/Envoyage-Browsing/envoyage.git#browser-sdk&path:/sdk"
# or in package.json:
#   "@envoyage/browser": "github:Envoyage-Browsing/envoyage#browser-sdk"
```

## Quickstart — drive, handle a human wall, close

```ts
import { createSession } from "@envoyage/browser";

const session = createSession({
  cdpUrl: "wss://browser.cloudflare.example/cdp", // CF Browser Run or any Chrome
  endpoint: "https://envoyage.example.com",        // your running `envoyage serve`
  token: process.env.ENVOYAGE_AUTH_TOKEN,          // if the engine sets a bearer
});

// 1) Drive — every driving call returns { text, image?, isError, raw }.
await session.open("https://example.com/login");

const page = await session.readPage();            // parsed ref listing
const emailField = page.elements.find((e) => e.role === "textbox");
if (emailField) await session.formInput(emailField.ref, "mort@example.com");

// 2) Handle a human wall. The engine PAUSES server-side and emits `human-needed`.
//    A password/OTP or a screenshot can NEVER reach you while the wall is up —
//    the boundary is enforced by the engine; the SDK only surfaces the event.
session.on("human-needed", async ({ kind, reason, instructions }) => {
  console.log(`Human needed (${kind}): ${reason}. ${instructions ?? ""}`);
  // Show the live view to your user; they solve it and drive via sendInput().
});

const signIn = (await session.find("Sign in")).elements[0];
if (signIn) await session.click({ ref: signIn.ref }); // may trigger a handoff

// 3) After the human finishes (clicks Continue in your live view), resume:
await session.resume();

// 4) Close — closes the SSE connection + the engine-side session. NEVER kills a
//    browser the SDK didn't launch (CF owns that lifecycle).
await session.close();
```

## Live view — `frames()` + `sendInput()`

Frames of the **active** target, narration bubbles, cursor points, handoff
banners, and pause state all arrive on **one** SSE stream. Read frames as an
async iterator and forward the human's clicks/keys with `sendInput()`:

```ts
// Stream frames to your UI (base64 PNG of the active tab):
(async () => {
  for await (const frame of session.frames()) {
    ui.paint(`data:image/png;base64,${frame.pngBase64}`, frame.seq);
  }
})();

// When the human clicks on your live view during a handoff, un-letterbox the
// point to PAGE CSS pixels and forward it:
canvas.onclick = (e) => {
  const { x, y } = toPageCoords(e);
  session.sendInput({ kind: "click", x, y });
};
// keys / scroll / pause-continue too:
session.sendInput({ kind: "key", key: "Enter" });
session.sendInput({ kind: "scroll", dy: 400 });
session.sendInput({ kind: "control", action: "continue" });
```

Or consume every event type in one loop:

```ts
for await (const ev of session.events()) {
  switch (ev.type) {
    case "frame":        ui.paint(ev.frame); break;
    case "cursor":       mascot.glideTo(ev.cursor.x, ev.cursor.y); break;
    case "narration":    balloon.show(ev.narration.text); break;
    case "human-needed": ui.banner(ev.human); break;
    case "state":        ui.setPaused(ev.state.paused); break;
  }
}
```

Typed subscriptions with `on()` work too:
`on("frame" | "cursor" | "narration" | "human-needed" | "state" | "close")`.

## Local OSS case — `launch()` (Node only)

For a single machine where the engine spawns and **owns** a local Chromium:

```ts
import { launch } from "@envoyage/browser/launch"; // node:child_process — NOT in a Worker

const { session, stop } = await launch({ httpPort: 8788 });
await session.open("https://example.com");
await stop(); // closes the session AND the engine's local browser
```

Never import `@envoyage/browser/launch` from a Worker — use
`createSession({ cdpUrl })` against a remote engine instead.

## Many sessions, one process

Each `createSession()` is independent — its own `Mcp-Session-Id`, its own SSE
connection, its own remote browser. There are no global singletons in the SDK,
so a Worker can serve many concurrent users from one module instance.

## The human boundary (why this is safe)

When the engine hits a Cloudflare/CAPTCHA bot-check, an OAuth/sign-in screen, or
a password/OTP field, it **pauses** and returns text-only to the model — no
screenshot. The human solves the step in the live view (you stream frames + send
their input); the model sees nothing sensitive. This is enforced **server-side**
in the engine — the SDK cannot bypass it, it only surfaces the `human-needed`
event and the paused `state`.

## Detection stays in sync with the engine

`HUMAN_NEEDED_JS` and `AX_SNAPSHOT_JS` are re-exported **verbatim** from the
engine's `src/shared/*.js` (the same files the Rust engine `include_str!`s), so
any client-side probe you run uses byte-identical heuristics. `classifyHandoff()`
mirrors the engine's password > captcha > cloudflare > oauth priority.

## Engine routes this SDK expects

| Method + route | Purpose |
|---|---|
| `POST /mcp` | JSON-RPC `tools/call` for all driving methods (exists today). |
| `GET /events` | SSE stream of `browser_frame`/`cursor`/`narration`/`human_request`/`state` envelopes. |
| `POST /input` | Forward one `Input` event (`click`/`key`/`scroll`/`control`). |

Headers: `Mcp-Session-Id` (multi-session key), `Authorization: Bearer <token>`
(when the engine sets `ENVOYAGE_AUTH_TOKEN`), and `X-Envoyage-Cdp-Url` (per-session
remote browser to connect on lazy launch).

> Note: `/mcp` ships today. `/events` (SSE) and `/input` are the HTTP mirror of
> the engine's existing WebSocket live-view surface (`src/serve/ws.rs`) — the SSE
> envelopes are byte-for-byte the same `src/protocol.rs` shapes. The Workers-safe
> core path deliberately uses SSE, not WebSocket.
```

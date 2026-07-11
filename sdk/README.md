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

## Reading values (secrets) back to the consumer

Some consumers need to read a value **off the page** and file it somewhere (e.g.
the browser creates an API key, and you write that key to a secret store) —
**without** the value reaching the model, the narration, or the screencast.

**The value-return op is `readPage()` / `find()`.** Each element in the returned
listing carries a `value` field:

```ts
const page = await session.readPage(/* interactiveOnly */ false);
const keyField = page.elements.find((e) => e.name.includes("API key"));
const secret = keyField?.value;          // the element's live value, returned to YOU
if (secret) await mySecretStore.put("api-key", secret);
```

Call path (engine side):

- `browser_read_page` → `handle_read_page` (`src/serve/mcp.rs:388`) →
  `BrowserSession::snapshot` (`src/browser.rs:717`) → in-page
  `AX_SNAPSHOT_JS` (`src/shared/ax-snapshot.js:54`, `value = String(el.value)`) →
  `render_ax_listing` emits `value:"…"` (`src/browser.rs:1316`).
- The SDK parses that line into `PageElement.value`
  (`sdk/src/index.ts:513`, `sdk/src/types.ts:146`) and hands it to you.
- `find()` is the same path via `handle_find` (`src/serve/mcp.rs:396`).

This value is returned in the **tool result of the `POST /mcp` call the consumer
made** — it is a direct request/response to the caller. It is **not** pushed onto
the SSE live-view bus, so it does **not** appear in:

- `narration` events — those carry only the field *name*, never the value
  (`src/serve/mcp.rs:308–310`), and are truncated to 60 chars
  (`truncate_narration`, `src/serve/mcp.rs:220`).
- `frame` events — the screencast PNGs are pumped independently
  (`src/serve/pump.rs:196–212`) and never encode AX text.
- `cursor` / `state` / `human-needed` events — none carry element values.

So the returned value goes to the **caller that asked for it** and nowhere else
on the live view. Whoever can read the model's tool results can read the value —
if the model is your LLM, treat `readPage()`/`find()` output as
secret-bearing and don't echo it back into a prompt.

### ⚠️ Gap: `readPage()`/`find()` are NOT password-blind

The password-blindness guarantee (the model never sees a password) is enforced
**only on the screenshot-returning tools** (`open`/`click`/`type`/`key`/`scroll`
→ `handle_browser_shot`, which suppresses the screenshot while paused/handed-off,
`src/serve/mcp.rs:338`). **`readPage()` and `find()` do not consult the pause
flag and do not run handoff detection** — they always return the full AX listing,
and `AX_SNAPSHOT_JS` captures `el.value` for **every** input including
`type="password"` (no password exclusion, `src/shared/ax-snapshot.js:54`).

Consequences for a consumer:

- ✅ **Reading a non-secret value** (an API key the browser just *displayed*, a
  generated token in a visible field) works cleanly — `readPage()` returns it.
- ⚠️ **A password the human typed during a handoff IS readable** via `readPage()`
  while the wall is up. The screen is hidden from the model, but the AX value is
  not. If your model (not just your trusted daemon) can call `readPage()`, it can
  read a password field's contents. Don't route `readPage()`/`find()` output to
  an untrusted model on a page that has a live password field.

If you need a hard guarantee, gate `readPage()`/`find()` yourself while
`state.paused === true`, or file an engine change to redact
`type="password"` values in `AX_SNAPSHOT_JS` and/or skip the value in
`render_ax_listing` while paused. As of `@envoyage/browser@0.1.1` this is **not**
done for you.

## Deploying & consuming

### Topology — where the engine runs

There is exactly **one** moving piece to place: the `envoyage serve` engine. The
SDK (and therefore your Cloudflare Worker) is just a `fetch` client pointed at
it.

Two common shapes:

1. **Consumer self-hosts the engine.** You run `envoyage serve --http-port N`
   yourself (k8s pod, VPS, CF Container) and point it at a browser — either a
   remote CDP endpoint via `--cdp-url wss://…` (Cloudflare Browser Run) or a
   local Chrome. **One engine process multiplexes N sessions**: each distinct
   `Mcp-Session-Id` gets its own `BrowserSession` (its own CDP connection) in the
   registry (`src/serve/state.rs:39–51`), so a slow navigate in session A never
   blocks session B. Your Worker sets `endpoint` to this engine's URL and a
   `sessionId` (or lets the SDK generate one) per user.

2. **SDK points at a provided endpoint.** If someone else runs the engine, you
   only ever set `createSession({ endpoint, cdpUrl })`. The Worker holds no
   browser and no engine — it is a thin `fetch` + SSE client.

Per-session remote browser: the SDK sends `x-envoyage-cdp-url` per request
(`sdk/src/index.ts:403`), so a single engine started **without** a process-global
`--cdp-url` can point each session at a **different** remote browser. An engine
started **with** `--cdp-url` ignores the header and uses its configured endpoint
(`src/serve/pump.rs:54`).

### Auth — bearer token

The engine gates every HTTP route with a bearer token **when the env var
`ENVOYAGE_AUTH_TOKEN` is set** (verified in code: `src/serve/http.rs:56` reads it
into `HttpState.auth_token`; `check_auth` at `src/serve/http.rs:86` requires
`Authorization: Bearer <token>` on `/mcp`, `/sessions/:id/events`, and
`/sessions/:id/input`). If the var is **unset, auth is disabled** — every request
is allowed (intended for local dev only; `src/serve/http.rs:87`).

Pass the same token to the SDK as `token`; it's sent as
`Authorization: Bearer <token>` on every call (`sdk/src/index.ts:404`).

```bash
# engine host
ENVOYAGE_AUTH_TOKEN=$(openssl rand -hex 32) \
ENVOYAGE_HTTP_HOST=0.0.0.0 \
  envoyage serve --http-port 8788 --cdp-url wss://browser.example/cdp
```

```ts
// worker
createSession({ endpoint: "https://engine.example.com", cdpUrl: "…", token: env.ENVOYAGE_AUTH_TOKEN });
```

Two more deployment notes verified in code:

- The engine binds **loopback (`127.0.0.1`) by default**; set
  `ENVOYAGE_HTTP_HOST=0.0.0.0` to expose it (`src/serve/http.rs:75`). Only expose
  it with `ENVOYAGE_AUTH_TOKEN` set.
- CORS reflects any `Origin` and uses **no credentials mode** — the bearer token,
  not the origin, is the gate (`src/serve/http.rs:301–342`). So the token is the
  entire security boundary; treat it as a secret and rotate it.

### Isolation — can one session read another's?

**No — sessions are isolated, and this is enforced in code, not by convention.**

- **Browsers:** the registry keys each browser by session id
  (`src/serve/state.rs:46`). Distinct ids get distinct `Arc<Mutex<…>>` slots —
  asserted in `state.rs:298` (`registry_and_pause_are_per_session`:
  `assert!(!Arc::ptr_eq(&a1, &b1))`). One session's tool calls only ever touch
  its own browser (`with_browser` clones the slot for `session_id` only,
  `src/serve/pump.rs:39`).
- **Frames / AX / live view:** each session has its own broadcast bus, keyed by
  id (`src/serve/state.rs:149`). `broadcast_envelope_to(sid, …)` only reaches
  that id's subscribers — asserted in `state.rs:285`
  (`buses_are_isolated_per_session`: session B's replay cache is empty after a
  frame is broadcast to session A).
- **Input:** `POST /sessions/:id/input` queues onto that id's input channel only
  (`src/serve/http.rs:289` → `push_input_to`, `src/serve/state.rs:191`); the
  session's own pump drains only its own queue (`src/serve/pump.rs:177`).
- **Pause state:** per-session map (`src/serve/state.rs:80`); pausing A does not
  pause B (asserted, `state.rs:307`).

**The caveat — the boundary is the token + the session id, not the network.**
The engine does **not** bind a session id to a caller identity: any request
carrying `Mcp-Session-Id: X` (and the bearer token, if set) drives session X and
subscribes to its live view. So a consumer that lets clients choose their own
`sessionId` must treat the id as a **capability** — issue unguessable ids
(`randomId()` uses `crypto.randomUUID`, `sdk/src/index.ts:556`) and never leak
one user's id to another. Within one trusted consumer that mints per-user ids,
cross-session reads are impossible; across untrusted callers sharing one engine +
token, the session id is the only thing standing between them, so keep it secret.

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

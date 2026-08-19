# Consuming Envoyage

**envoyage gives an AI agent a live, human-watchable browser. You bring the
mascot.**

This guide is for TypeScript products that want to embed envoyage: let a coding
agent (Claude, Gemini, whatever) drive a real browser, show that browser
*live* in your own UI, and glide *your* mascot to exactly where the agent is
about to act. ImmorTerm draws **Mort** (coral axolotl); ringtail draws
**Rocco** (its ringtail mascot). envoyage draws nothing — it hands you the
frame, the cursor point, and the intent string; you skin them.

For a full walkthrough wired to a specific stack, see
[`ringtail.md`](ringtail.md). The runnable example lives in
[`../../examples/ringtail-consumer/`](../../examples/ringtail-consumer/).

## The two surfaces

You run `envoyage serve` as a subprocess. It exposes two independent surfaces —
use one or both:

| Surface | Transport | Purpose | Who talks to it |
|---------|-----------|---------|-----------------|
| **MCP** | JSON-RPC 2.0 over **stdio** | The agent's tool surface: `browser_open`, `read_page`, `click`, … | Your **agent runtime** (MCP client → LLM tool loop) |
| **WS frame stream** | WebSocket on `--ws-port` | The **live view** + human input back | Your **UI** (dashboard `<img>`/`<canvas>` + your mascot) |

```
                 ┌──────────── envoyage serve --ws-port 8787 ────────────┐
                 │                                                     │
   your agent ──stdio (MCP JSON-RPC)──▶  browser_* tools  ──drives──▶  │
   runtime    ◀──tool results──────────                    Chromium    │
                 │                                            │        │
   your UI    ◀──ws (browser_frame / cursor / narration)─────┘        │
   (mascot)   ──ws (click / key / scroll / control)──▶ human input     │
                 └─────────────────────────────────────────────────────┘
```

Start both:

```bash
envoyage serve --mcp --ws-port 8787
# or via npm:  npx @envoyage/cli serve --mcp --ws-port 8787
```

`--mcp` alone → stdio only (no live view). `--ws-port N` alone → live view
only (no agent). Default (no flags) → `--mcp`.

## Surface 1 — MCP (the agent's tools)

envoyage speaks MCP over **stdio**: newline-delimited JSON-RPC 2.0. The handshake
is the standard three steps:

1. `initialize` → returns `{protocolVersion, capabilities:{tools:{}}, serverInfo}`
2. `tools/list` → the `browser_*` tool array (schemas below)
3. `tools/call` → `{name, arguments}` → `{content: [...]}`

You do **not** hand-roll this. Use `@modelcontextprotocol/sdk`'s stdio client
(`StdioClientTransport`) — spawn `envoyage serve` as the command, and the SDK
does the handshake, `listTools`, and `callTool` for you. Then expose those
tools to your LLM's tool-calling loop. See [`ringtail.md`](ringtail.md) for the
exact wiring (raw MCP SDK **and** the Vercel AI SDK path).

### The tool surface

Names are neutral `browser_*`; schemas mirror ImmorTerm's set, so a model that
knows one knows both.

| Tool | Args | Returns | Notes |
|------|------|---------|-------|
| `browser_open` | `url` | compact caption | Opens/reuses the browser and navigates; visual frames stay on the live-view transport. `http`/`https`/`about:blank` only. |
| `browser_read_page` | `interactive_only?` | text: `[ref_N] role "name"` listing | The cheap way to understand a page (no image tokens). **Untrusted content.** |
| `browser_find` | `query` | text: ranked `[ref_N]` listing | Search a long page; same ref shape. |
| `browser_click` | `ref` \| `x`,`y` | compact caption | Prefer `ref`; coords are a fallback. Visuals stream separately. |
| `browser_form_input` | `ref`, `value` | compact caption | Set a field/checkbox/dropdown. Visuals stream separately. |
| `browser_key` | `key` | compact caption | `Enter`/`Tab`/`Escape`/`Backspace`/`Arrow*`. Visuals stream separately. |
| `browser_scroll` | `dy` | compact caption | CSS px; positive = down. Visuals stream separately. |
| `browser_screenshot` | — | caption + bounded image preview, or omission notice | Visual judgment only; large captures become scaled JPEGs, so click by ref. Oversized inline images never enter model context. |
| `browser_tabs_list` | — | text: tab list | Popups/OAuth windows. **Untrusted content.** |
| `browser_tabs_switch` | `index` \| `targetId` | read_page listing | Switch tab, then read it. |
| `browser_upload` | `ref`, `path` | text | Attach a local file to a `<input type=file>`. |
| `browser_console` | — | text: recent console | Debug page JS. **Untrusted content.** |
| `browser_network` | — | text: recent responses | Did the API call fire? **Untrusted content.** |
| `browser_wait_for` | `selector?`, `text?`, `timeout_secs?` | text | Wait for a selector/text — no blind sleeps. |
| `browser_request_human` | `reason?`, `instructions?` | text: wait cue | Hand off (pauses, banners the UI). |
| `browser_wait_for_human` | `timeout_secs?` | text | Wait for the human to click ▶ Continue. |
| `browser_gif` | `action`, `filename?`, `options?` | text: GIF path | Record a session (`start_recording`/`stop_recording`) and `export` an annotated animated GIF (`clear` drops it). Parity with claude-in-chrome's `gif_creator`; writes to `${ENVOYAGE_HOME:-~/.envoyage}/gif/` — serve/download that file. Vendor-neutral: no logo; `showWatermark` defaults false. |
| `browser_close` | — | text | Kill the exact spawned browser. |
| `browser_eval` | `js` | text | **Gated** — only if `ENVOYAGE_BROWSER_EVAL=1`. |

**Trust boundary:** every listing (`read_page`, `find`, `tabs_list`, `console`,
`network`) is framed as untrusted web-page content — data, not instructions.
Pass it to the model as tool output, never as a system directive.

MCP output is context-safe by construction: every serialized tool result is
hard-capped at 128 KiB, with aggregate text capped at 24 KiB and inline image
base64 at 96 KiB. The limits cannot be raised with environment configuration.
Drive-action visuals belong on the WS/SSE live-view surface, not in the LLM
transcript.

## Surface 2 — WS (the live view + input)

Connect a plain WebSocket to `ws://127.0.0.1:<ws-port>`. envoyage pushes JSON
event envelopes (tagged by `type`) and accepts input events (tagged by `kind`)
back. This is what makes the browser **human-watchable**.

### Events envoyage → your UI

| `type` | Payload | Render as |
|--------|---------|-----------|
| `browser_frame` | `png_base64`, `title`, `url`, `seq` | The live view: `<img src="data:image/png;base64,…">` or `drawImage` on a `<canvas>`. Drop any frame whose `seq` ≤ the last shown. |
| `browser_cursor` | `x`, `y`, `action` (`move`/`click`/`type`/`scroll`) | **Your mascot**, glided to `(x,y)` in page CSS px. |
| `browser_narration` | `text` | A speech balloon / caption near the mascot ("Clicking \"Sign in\""). |
| `browser_human_request` | `reason`, `instructions?` | A handoff banner ("Human needed: Cloudflare check"). |
| `browser_state` | `paused` (bool) | Toggle your pause/continue control. |

### Input your UI → envoyage

Send JSON with a `kind` field. Coordinates are **page CSS pixels** (see
mapping below).

| `kind` | Payload | When |
|--------|---------|------|
| `click` | `x`, `y` | Human clicks on the live view. |
| `key` | `key` | Human types (`Enter`, a char, …). |
| `scroll` | `dy` | Human scrolls the live view. |
| `control` | `action` (`pause` / `continue`) | Human takes over / hands back. |

## Coordinate mapping (frame ↔ display ↔ mascot)

Everything envoyage emits is in **page CSS pixels**, and by envoyage's scale-1
design **page px → CSS px is 1:1** (screenshots line up 1:1 with click
coordinates, even on Retina). So there are only two spaces you juggle:

- **Page space** — the coordinates in `browser_cursor` and the coordinates you
  send in a `click`. This is the frame's native pixel space.
- **Display space** — where the frame ends up in your UI's `<img>`/`<canvas>`
  rect, which is usually a different size and **letterboxed** (the frame keeps
  its aspect ratio, so there are bars on one axis).

The frame PNG has an intrinsic width/height (`img.naturalWidth/Height`, or the
PNG header). Fit it into your display rect preserving aspect ratio:

```ts
// One scale, centered → letterboxed. `dispW/dispH` = your <img>/<canvas> box.
function fit(frameW: number, frameH: number, dispW: number, dispH: number) {
  const scale = Math.min(dispW / frameW, dispH / frameH);
  const w = frameW * scale, h = frameH * scale;
  const offX = (dispW - w) / 2, offY = (dispH - h) / 2; // letterbox bars
  return { scale, offX, offY };
}

// page px  → display px  (place the mascot at a browser_cursor point)
const toDisplay = (x: number, y: number, f: ReturnType<typeof fit>) => ({
  x: f.offX + x * f.scale,
  y: f.offY + y * f.scale,
});

// display px → page px  (a human click on the live view → an Input.click)
const toPage = (x: number, y: number, f: ReturnType<typeof fit>) => ({
  x: (x - f.offX) / f.scale,
  y: (y - f.offY) / f.scale,
});
```

So: **place your mascot** with `toDisplay(cursor.x, cursor.y)`; **send a human
click** with `toPage(evt.offsetX, evt.offsetY)`. That's the whole seam.

## Human handoff (secrets never reach the model)

When envoyage hits a Cloudflare/CAPTCHA check, an OAuth/sign-in screen, or a
password/one-time-code field — or the agent calls `browser_request_human` — it:

1. **pauses** (sets `browser_state {paused:true}`),
2. broadcasts `browser_human_request {reason, instructions?}` to your UI,
3. returns **text only** to the model (no screenshot — the paused screen is
   never sent to the LLM).

Your UI: show the banner, keep rendering `browser_frame` (the human still sees
the live view), and let the human drive by forwarding their `click`/`key`/
`scroll` input. When they're done, send `{kind:"control", action:"continue"}`.
The agent, meanwhile, should call `browser_wait_for_human` (which returns when
`paused` flips back to false). Passwords the human types go straight into the
browser over the WS input channel — they never pass through the model.

## Bring-your-own-mascot, concretely

envoyage ships **no** cursor sprite. The `browser_cursor` event is the seam:

```ts
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.type === "browser_cursor") {
    const { x, y } = toDisplay(msg.x, msg.y, fit(frameW, frameH, dispW, dispH));
    // ImmorTerm: glide Mort here.  ringtail: glide Rocco here.  you: your sprite.
    placeMascot(x, y, msg.action); // msg.action: move | click | type | scroll
  }
  if (msg.type === "browser_narration") showBalloon(msg.text);
};
```

Animate the transition (a CSS transform transition on the mascot element is
enough) so it *glides* to the point rather than teleporting. Use `action` to
pick the mascot's pose (a "click" tap, a "type" lean-in, a "scroll" drift).

## Configuration knobs

| Env | Default | Purpose |
|-----|---------|---------|
| `ENVOYAGE_HOME` | `~/.envoyage` | Base dir for `browser.lock` + the persistent browser profile. |
| `ENVOYAGE_BROWSER_BIN` | auto-detect | Path to a Chromium/Chrome/Brave/Edge binary. |
| `ENVOYAGE_BROWSER_MEMORY_LIMIT_MB` | `4096` | Combined RSS ceiling for Envoyage's local Chromium process group. Values are clamped to 256–131072 MiB. |
| `ENVOYAGE_BROWSER_EVAL` | unset | `1` exposes the gated `browser_eval` tool. |
| `ENVOYAGE_GITHUB_REPO` | `Envoyage-Browsing/envoyage` | Release source for the npm wrapper. |

One real browser drives the shared profile at a time (a cross-process lock).
The first `serve` owns it; a later one that finds a live owner refuses rather
than corrupting the profile.

The local process tree has a hard memory guard. Envoyage measures the process
group it created, closes only that group when it crosses the limit, and bounds
pending screencast frames, viewer backlog, input backlog, closed-session pumps,
and replay state. Hosted browsers connected with `--cdp-url` are owned and
measured by their provider instead.

# Hosted Envoyage — browser-driving as a service

> Status: **design / RFC** (not built). Envoyage-the-package (this repo) is the
> engine; this doc sketches the hosted product on top of it.

## Why

Cloud AI products (ringtail-cloud, and any agent that runs server-side or in a
browser tab) can't spawn a local headless Chromium. Today that means their
agent stalls the moment a task needs the web — or they bolt on a brittle
Playwright box. Hosted Envoyage is the paid drop-in: **"your agent operates a
real browser with zero setup, and your user watches it happen live with your
mascot."** That last clause is the wedge — it's a demo-able, shareable feature,
not just plumbing.

## Shape

```
 consumer app (ringtail)                 hosted Envoyage (multi-tenant)
 ┌──────────────────────┐   MCP/WSS      ┌───────────────────────────────┐
 │ Gemini + AI SDK      │◄──────────────►│  edge gateway (auth, routing) │
 │  → browser_* tools   │  tools/call    │        │                      │
 │ dashboard <canvas>   │◄──────────────►│  session broker → pool        │
 │  + Rocco cursor      │  frame/cursor  │   ┌─────┴─────┐                │
 └──────────────────────┘  /narration   │   │ envoyage    │ one headless  │
                                         │   │ serve ×N  │ Chromium each │
                                         │   └───────────┘ (isolated)    │
                                         └───────────────────────────────┘
```

- **Per tenant/session**: one `envoyage serve` process + one Chromium, isolated
  via `Target.createBrowserContext` (the isolation envoyage deferred — this is
  where it gets built). Warm pool to hide the ~300ms cold launch.
- **Two surfaces, unchanged from the package**: MCP-over-WSS for the agent's
  tool calls; the frame/cursor/narration WS for the consumer's live view. The
  service is a thin multi-tenant wrapper around the exact protocol this repo
  already ships — so a consumer that works against local `envoyage serve` works
  against hosted Envoyage by swapping a URL.
- **Auth**: per-tenant API key at the gateway → maps to a session lease.
- **Mascot stays client-side**: the service never renders a cursor. It emits
  coordinates; the consumer draws Rocco/Mort. Zero mascot coupling server-side.

## The security boundary is the product's spine

Envoyage already guarantees passwords/CAPTCHA/OAuth never reach the model (frame
suppression while paused; human-handoff returns text-only). Hosted, that
becomes a *compliance* selling point: the human solves the sensitive step in
their own browser tab against the streamed view; the model sees nothing. Make
this explicit in the pitch and the SOC2 story.

## MVP path (smallest thing that earns money)

1. Single-region, single-tenant-per-box (no pool yet): a container that runs
   `envoyage serve`, fronted by an authenticated WSS gateway. ringtail points its
   MCP client + viewer at it. Proves the remote path end-to-end.
2. Add `Target.createBrowserContext` isolation + a warm pool → multi-session
   per box.
3. Metering (session-minutes / actions) + per-tenant keys → billing.
4. Regions + autoscaling.

Stop at each rung until the next is demanded — don't build the pool before a
second tenant exists.

## Open questions (for the founder)

- Pricing unit: session-minutes, actions, or seats? (leans session-minutes —
  matches the cost driver: a live Chromium held open.)
- Does hosted Envoyage live in `immorterm-cloud` infra (Hetzner k8s, already
  there) or its own deploy? Reusing immorterm-cloud is the lazy-correct start.
- Isolation depth: `BrowserContext` per session is cheap but shares a browser
  process; full process-per-tenant is safer but costlier. Start with context,
  offer process-isolation as an enterprise tier.

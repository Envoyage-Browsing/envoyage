# serve-surface API collections

These are the **source of truth** for the `envoyage serve --http-port` remote
surface (the HTTP/MCP/SSE routes in `src/serve/http.rs`):

- [`openapi.yaml`](openapi.yaml) — OpenAPI 3.1 spec.
- [`../../api-collections/envoyage/`](../../api-collections/envoyage/) — a
  runnable [Bruno](https://www.usebruno.com/) collection.

**The rule (from [AGENTS.md](../../AGENTS.md)):** any change to the serve API —
a new route, param, header, MCP tool, or SSE event — must update **both** files
in the same change. They are kept in sync deliberately; a drift between them is
a bug.

## Routes documented

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/mcp` | JSON-RPC 2.0 MCP request (driving). Session via `Mcp-Session-Id`. |
| GET | `/sessions/{id}/events` | Per-session live-view SSE stream (frame/cursor/narration/handoff/state). |
| POST | `/sessions/{id}/input` | Push one human input event (click/key/scroll/control). |

There is no `/health` route. Auth is `Authorization: Bearer <ENVOYAGE_AUTH_TOKEN>`
on every route when the token is set.

## Running the Bruno collection

1. Start the surface: `envoyage serve --http-port 8788`
   (set `ENVOYAGE_AUTH_TOKEN` if you want to exercise auth).
2. Open `api-collections/envoyage/` in Bruno, pick the `local` environment, and
   set `port` / `authToken` to match.

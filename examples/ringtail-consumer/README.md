# ringtail-consumer example

A small, self-contained example of consuming [envoyage](../../README.md) from a
TypeScript product — the shape a ringtail engineer copies. Two halves:

- **`driver.ts`** — the **agent side**. Spawns `envoyage serve --mcp --ws-port 8787`
  as an MCP server (stdio), connects the `@modelcontextprotocol/sdk` client,
  lists the `browser_*` tools, and drives a task by hand: `browser_open` →
  `browser_read_page` → `browser_find` → `browser_click`. A commented block at
  the bottom shows the Vercel AI SDK path (Gemini drives the tools itself via
  `experimental_createMCPClient`).
- **`index.html` + `viewer.ts`** — the **UI side**. Connects to envoyage's WS
  frame stream, paints each `browser_frame` onto a `<canvas>`, and **glides a
  placeholder "Rocco" marker** to every `browser_cursor`. The mascot is an
  orange dot — search `viewer.ts`/`index.html` for `ringtail:` to see exactly
  where you swap in the real Rocco sprite.

## Run it

```bash
# 1. Have `envoyage` on PATH (cargo build --release -p envoyage) or use npm:
#      export ENVOYAGE_CMD="npx -y @envoyage/cli"
npm install            # @modelcontextprotocol/sdk, ai, @ai-sdk/google, tsx

# 2. Open the live view (any TS-aware dev server, e.g. vite):
npx vite .             # then open the printed URL → index.html

# 3. Drive the browser (in another terminal):
npx tsx driver.ts
```

`driver.ts` launches envoyage with `--ws-port 8787`, so the viewer shows the live
browser and the mascot gliding as the driver clicks. Watch the dot move to the
"More information" link, then tap on the click.

## What it proves

- **MCP is native to a TS product** — no framework, just the MCP SDK stdio
  client spawning `envoyage serve`.
- **Bring-your-own-mascot** — envoyage emits `browser_cursor {x,y,action}`; the
  consumer maps page px → display px and places its own sprite. Swap the dot for
  Rocco (or Mort) and nothing else changes.
- **The frame→display mapping** — `viewer.ts` letterboxes the frame and maps
  both directions (mascot placement + human-click forwarding).

The point is the *shape*, not full coverage — it doesn't run in CI, but the TS
is valid (`npm run typecheck`).

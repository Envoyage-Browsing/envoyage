/**
 * driver.ts — the consumer side, using the published @envoyage/browser SDK.
 *
 * This mirrors the real dashboard consumer (envoyage-cloud LiveView.tsx): it
 * `createSession({ endpoint, cdpUrl })` against a running `envoyage serve`,
 * drives over the SDK's methods (open/readPage/find/click), subscribes to the
 * live-view events (frame | cursor | narration | human-needed | state), and
 * handles a human handoff (requestHuman/waitForHuman + sendInput during the
 * takeover), then close()s.
 *
 * It is intentionally headless (logs events to the console instead of painting
 * a canvas) so it runs in a plain terminal — the *shape* of consuming the SDK is
 * the point, not the UI. For the real UI shape (frames → <img>, mascot on the
 * cursor, handoff banner), read envoyage-cloud's LiveView.tsx; the event wiring
 * below is identical.
 *
 * Run:
 *   1. Start an engine and point it at a browser (CF Browser Run or any Chrome):
 *        envoyage serve --http-port 8788 --cdp-url wss://your-browser/cdp
 *      (or local-spawn: `envoyage serve --http-port 8788` with cdpUrl:"local")
 *   2. npm install && npx tsx driver.ts
 *
 * Env:
 *   ENVOYAGE_ENDPOINT   engine base URL          (default http://127.0.0.1:8788)
 *   ENVOYAGE_CDP_URL    CDP wss:// to drive       (default "local" = engine-spawned Chrome)
 *   ENVOYAGE_AUTH_TOKEN bearer, if the engine sets one
 *   TARGET_URL          page to drive             (default https://example.com)
 */

import { createSession } from "@envoyage/browser";

const endpoint = process.env.ENVOYAGE_ENDPOINT ?? "http://127.0.0.1:8788";
const cdpUrl = process.env.ENVOYAGE_CDP_URL ?? "local";
const token = process.env.ENVOYAGE_AUTH_TOKEN;
const targetUrl = process.env.TARGET_URL ?? "https://example.com";

async function main(): Promise<void> {
  const session = createSession({ endpoint, cdpUrl, token });
  console.log(`consuming @envoyage/browser → ${endpoint} (session ${session.sessionId})`);

  // ── Subscribe to the live view. This is the SAME event set LiveView.tsx maps
  //    onto its UI; here we just log. Subscribing starts the SSE stream. ──
  session.on("frame", (f) =>
    console.log(`  [frame #${f.seq}] ${f.title} — ${f.url} (${f.pngBase64.length}B png)`),
  );
  session.on("cursor", (c) => console.log(`  [cursor] ${c.action} @ (${c.x}, ${c.y})`));
  session.on("narration", (n) => console.log(`  [narration] ${n.text}`));
  session.on("state", (s) => console.log(`  [state] ${s.paused ? "paused (human driving)" : "running"}`));

  // ── The handoff. The engine PAUSES server-side and emits `human-needed` when
  //    it hits a password/OTP/CAPTCHA/OAuth wall. The model never sees the
  //    screen or the secret — the human solves it in the live view and drives
  //    via sendInput(). Here we sketch a scripted takeover so the example runs
  //    end-to-end without a real human. ──
  session.on("human-needed", async ({ kind, reason, instructions }) => {
    console.log(`\n🙋 human needed (${kind}): ${reason}${instructions ? ` — ${instructions}` : ""}`);
    console.log("   (in a real consumer: show the live view; the human takes over here)");
    // Take over: pause, then drive the paused browser with human-style input.
    session.sendInput({ kind: "control", action: "pause" });
    // e.g. the human types + submits in the frame — coords are PAGE CSS pixels:
    session.sendInput({ kind: "key", key: "Enter" });
    // Hand it back so the agent's waitForHuman() resolves.
    session.sendInput({ kind: "control", action: "continue" });
  });

  try {
    // 1) Drive to a page. `open` returns { text, image?, isError, raw }.
    console.log(`\n[open] ${targetUrl}`);
    const opened = await session.open(targetUrl);
    console.log("  ", opened.text.split("\n")[0]); // "🌐 <title> — <url>"

    // 2) Read the page as ref handles (cheap; no image tokens). Elements carry a
    //    `value` field — the value-return path a consumer uses to read a field's
    //    contents back (see README "Reading values (secrets) back to the consumer").
    console.log("\n[readPage]");
    const page = await session.readPage();
    for (const el of page.elements) {
      console.log(`   ${el.ref}  ${el.role}  "${el.name}"${el.value ? `  value:"${el.value}"` : ""}`);
    }

    // 3) Find the element we want, ranked best-first, and click it by ref.
    console.log("\n[find] 'more information'");
    const found = await session.find("more information");
    const target = found.elements[0] ?? page.elements.find((e) => e.name.toLowerCase().includes("more"));
    if (target) {
      console.log(`\n[click] ${target.ref} ("${target.name}")`);
      const clicked = await session.click({ ref: target.ref });
      console.log("  ", clicked.text.split("\n")[0]);
    } else {
      console.log("\n[click] no matching element — skipping (page shape changed?)");
    }

    // 4) If the agent knows a step needs a human, it can ask explicitly (this
    //    fires the same handoff path as auto-detection), then wait for the human.
    //    Uncomment to demo the handoff wiring above:
    // await session.requestHuman("finish the login", "type your password in the frame");
    // await session.waitForHuman();

    console.log("\n✓ drove open → readPage → find → click via @envoyage/browser.");
  } finally {
    // Closes the SSE stream + the engine-side session. NEVER kills a browser the
    // SDK didn't launch (a cdpUrl/CF Browser Run browser is owned by CF).
    await session.close();
  }
}

main().catch((e) => {
  console.error("driver failed:", e);
  process.exit(1);
});

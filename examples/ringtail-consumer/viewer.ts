/**
 * viewer.ts — the UI side, using @envoyage/browser in the browser.
 *
 * This is the minimal, framework-free version of envoyage-cloud's LiveView.tsx:
 * `createSession({ endpoint, cdpUrl })`, subscribe to the SSE live view, paint
 * each `frame` onto a canvas, glide a mascot marker to each `cursor`, show the
 * handoff banner on `human-needed`, and forward the human's clicks/keys with
 * `sendInput()` during a takeover. Same events, same SDK — just <canvas> instead
 * of React.
 *
 *   // consumer seam: replace the #mascot dot with your own sprite.
 *
 * The SDK talks fetch + SSE (no WebSocket), so this bundles into a Worker-served
 * page unchanged. Load via <script type="module"> (see index.html); serve with a
 * TS-aware dev server (e.g. `npx vite .`).
 */

import { createSession, type BrowserSession, type CursorAction } from "@envoyage/browser";

// Engine coordinates. In a real product these come from your backend (which
// mints a per-user sessionId + hands out the engine endpoint + a CDP url).
const ENDPOINT = "http://127.0.0.1:8788";
const CDP_URL = "local"; // or a CF Browser Run "wss://…/cdp"
const START_URL = "https://example.com";

const canvas = document.getElementById("view") as HTMLCanvasElement;
const mascot = document.getElementById("mascot") as HTMLDivElement;
const balloon = document.getElementById("balloon") as HTMLDivElement;
const banner = document.getElementById("banner") as HTMLDivElement;
const bannerText = document.getElementById("banner-text") as HTMLSpanElement;
const takeOverBtn = document.getElementById("takeover") as HTMLButtonElement;
const continueBtn = document.getElementById("continue") as HTMLButtonElement;
const ctx = canvas.getContext("2d")!;

// Letterbox fit of the current frame within the canvas.
let fit = { scale: 1, offX: 0, offY: 0 };
let driving = false; // the human has taken over during a handoff

/** page CSS px → display px (place the mascot). */
function toDisplay(x: number, y: number) {
  return { x: fit.offX + x * fit.scale, y: fit.offY + y * fit.scale };
}
/** display px → page CSS px (send a human click). */
function toPage(x: number, y: number) {
  return { x: (x - fit.offX) / fit.scale, y: (y - fit.offY) / fit.scale };
}

const session: BrowserSession = createSession({ endpoint: ENDPOINT, cdpUrl: CDP_URL });

session.on("frame", (f) => {
  const img = new Image();
  img.onload = () => {
    const scale = Math.min(canvas.width / img.width, canvas.height / img.height);
    const w = img.width * scale;
    const h = img.height * scale;
    fit = { scale, offX: (canvas.width - w) / 2, offY: (canvas.height - h) / 2 };
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(img, fit.offX, fit.offY, w, h);
  };
  img.src = `data:image/png;base64,${f.pngBase64}`;
});

session.on("cursor", (c) => {
  // ── THE MASCOT SEAM: glide the mascot to where the agent is about to act. ──
  const p = toDisplay(c.x, c.y);
  mascot.style.left = `${p.x}px`;
  mascot.style.top = `${p.y}px`;
  mascot.className = (c.action ?? "move") as CursorAction; // pose per action
});

session.on("narration", (n) => {
  balloon.textContent = n.text;
  balloon.style.left = mascot.style.left;
  balloon.style.top = mascot.style.top;
  balloon.classList.remove("hidden");
});

session.on("human-needed", (h) => {
  bannerText.textContent = `🙋 ${h.reason}${h.instructions ? " — " + h.instructions : ""}`;
  banner.style.display = "block";
});

session.on("state", (s) => {
  // Only a resume (paused:false) is authoritative: agent's back, clear the banner.
  if (!s.paused) {
    driving = false;
    banner.style.display = "none";
  }
});

// Drive to a real page so the frame isn't blank.
session.open(START_URL).catch((e) => console.error("open failed:", e));

// ── Handoff controls (the trust moment): take over → human drives; resume. ──
takeOverBtn.addEventListener("click", () => {
  session.sendInput({ kind: "control", action: "pause" });
  driving = true;
});
continueBtn.addEventListener("click", () => {
  session.sendInput({ kind: "control", action: "continue" });
  driving = false;
  banner.style.display = "none";
});

// The human clicks the live view → page CSS px → engine (used while driving).
canvas.addEventListener("click", (ev) => {
  if (!driving) return;
  const r = canvas.getBoundingClientRect();
  const px = ((ev.clientX - r.left) / r.width) * canvas.width;
  const py = ((ev.clientY - r.top) / r.height) * canvas.height;
  const { x, y } = toPage(px, py);
  session.sendInput({ kind: "click", x, y });
});

window.addEventListener("keydown", (ev) => {
  if (!driving) return;
  const named = ["Enter", "Tab", "Backspace", "Escape", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"];
  const key = named.includes(ev.key) ? ev.key : ev.key.length === 1 ? ev.key : null;
  if (key) {
    ev.preventDefault();
    session.sendInput({ kind: "key", key });
  }
});

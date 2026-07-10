/**
 * viewer.ts — the UI side of consuming rudder.
 *
 * Connects to rudder's WS frame stream, paints each browser_frame onto a canvas,
 * and glides a mascot marker to every browser_cursor. rudder ships NO cursor —
 * the mascot here (an orange dot) is the placeholder a consumer skins.
 *
 *   // ringtail: replace the #rocco marker with the real <Rocco> sprite.
 *
 * It also forwards human clicks (page CSS px) back to rudder, and handles the
 * pause/handoff banner. Pair with driver.ts, which launches
 * `rudder serve --ws-port 8787`.
 *
 * Plain browser module — load via <script type="module"> (see index.html).
 * Served through any dev server that transpiles TS (e.g. `vite`), or precompile.
 */

const WS_URL = "ws://127.0.0.1:8787";

const canvas = document.getElementById("view") as HTMLCanvasElement;
const rocco = document.getElementById("rocco") as HTMLDivElement;
const balloon = document.getElementById("balloon") as HTMLDivElement;
const banner = document.getElementById("banner") as HTMLDivElement;
const bannerText = document.getElementById("banner-text") as HTMLSpanElement;
const continueBtn = document.getElementById("continue") as HTMLButtonElement;
const ctx = canvas.getContext("2d")!;

// The letterbox fit of the current frame within the canvas (see consumers/README.md).
let fit = { scale: 1, offX: 0, offY: 0 };
let lastSeq = -1;

/** page CSS px → display px (place the mascot). */
function toDisplay(x: number, y: number) {
  return { x: fit.offX + x * fit.scale, y: fit.offY + y * fit.scale };
}
/** display px → page CSS px (send a human click). */
function toPage(x: number, y: number) {
  return { x: (x - fit.offX) / fit.scale, y: (y - fit.offY) / fit.scale };
}

const ws = new WebSocket(WS_URL);
ws.onopen = () => console.log("viewer: connected to rudder");
ws.onclose = () => console.log("viewer: rudder closed the stream");

ws.onmessage = (e: MessageEvent<string>) => {
  const msg = JSON.parse(e.data) as
    | { type: "browser_frame"; png_base64: string; title: string; url: string; seq: number }
    | { type: "browser_cursor"; x: number; y: number; action: string }
    | { type: "browser_narration"; text: string }
    | { type: "browser_human_request"; reason: string; instructions?: string }
    | { type: "browser_state"; paused: boolean };

  switch (msg.type) {
    case "browser_frame": {
      if (msg.seq <= lastSeq) return; // drop stale/out-of-order frames
      lastSeq = msg.seq;
      const img = new Image();
      img.onload = () => {
        const scale = Math.min(canvas.width / img.width, canvas.height / img.height);
        const w = img.width * scale, h = img.height * scale;
        fit = { scale, offX: (canvas.width - w) / 2, offY: (canvas.height - h) / 2 };
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        ctx.drawImage(img, fit.offX, fit.offY, w, h);
      };
      img.src = `data:image/png;base64,${msg.png_base64}`;
      break;
    }

    case "browser_cursor": {
      // ── THE MASCOT SEAM: glide Rocco to where the agent is about to act. ──
      const p = toDisplay(msg.x, msg.y);
      rocco.style.left = `${p.x}px`;
      rocco.style.top = `${p.y}px`;
      rocco.className = msg.action; // "click" | "type" | "move" | "scroll" → pose
      break;
    }

    case "browser_narration": {
      // ringtail: a Rocco speech balloon. Anchor it above the mascot.
      balloon.textContent = msg.text;
      balloon.style.left = rocco.style.left;
      balloon.style.top = rocco.style.top;
      balloon.classList.remove("hidden");
      break;
    }

    case "browser_human_request": {
      bannerText.textContent = `🙋 ${msg.reason}${msg.instructions ? " — " + msg.instructions : ""} — take over below, then press ▶ Continue.`;
      banner.style.display = "block";
      break;
    }

    case "browser_state": {
      if (!msg.paused) banner.style.display = "none";
      break;
    }
  }
};

// Human clicks the live view → page CSS px → rudder (used mainly while paused).
canvas.addEventListener("click", (ev) => {
  const r = canvas.getBoundingClientRect();
  // account for CSS scaling of the canvas element vs its pixel width.
  const px = ((ev.clientX - r.left) / r.width) * canvas.width;
  const py = ((ev.clientY - r.top) / r.height) * canvas.height;
  const { x, y } = toPage(px, py);
  ws.send(JSON.stringify({ kind: "click", x, y }));
});

continueBtn.addEventListener("click", () => {
  ws.send(JSON.stringify({ kind: "control", action: "continue" }));
});

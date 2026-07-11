// @envoyage/browser — a thin, Workers-safe client for the Envoyage engine.
//
// The engine (`envoyage serve --http-port N`) is the single source of driving,
// detection, and tab-following. This SDK marshals to it: NO NAPI, NO WASM, NO
// port of the driving logic. The core path uses ONLY `fetch` (POST /mcp for
// driving, POST /input for human input) and an SSE reader over a streamed fetch
// Response (GET /events for frames/cursor/narration/handoff/state). No
// WebSocket, no node built-ins here — this module bundles into a Cloudflare
// Worker unchanged. (The Node-only `launch()` helper lives in ./launch.)
//
// Wire contract mirrors the engine's src/protocol.rs envelopes and
// src/serve/tool_defs.rs tools — the engine is the source of truth.

import type {
  CreateSessionOptions,
  DriveResult,
  DriveState,
  EventName,
  Frame,
  HumanNeeded,
  InputEvent,
  Listener,
  PageElement,
  PageSnapshot,
  SessionEvents,
  TabInfo,
  ToolContent,
} from "./types.js";
import { classifyHandoff } from "./detection.js";
import { readSse } from "./sse.js";

export * from "./types.js";
export { classifyHandoff, parseHumanNeeded, HUMAN_NEEDED_JS, AX_SNAPSHOT_JS } from "./detection.js";

/** Routes on the engine. `/mcp` exists today; `/events` + `/input` are the live-view HTTP surface. */
const ROUTES = {
  mcp: "/mcp",
  events: "/events",
  input: "/input",
} as const;

let jsonRpcId = 0;

/**
 * Create a browser session against a running Envoyage engine. Tells the engine
 * to drive the browser at `cdpUrl` (a CF Browser Run wss, or any Chrome CDP
 * endpoint). Does NOT open the SSE stream until you call `.frames()` or
 * subscribe with `.on(...)`. Many independent sessions can coexist in one
 * process — there are no global singletons here.
 */
export function createSession(opts: CreateSessionOptions): BrowserSession {
  if (!opts.endpoint) throw new Error("createSession: `endpoint` is required");
  if (!opts.cdpUrl) throw new Error("createSession: `cdpUrl` is required");
  return new BrowserSession(opts);
}

export class BrowserSession {
  readonly sessionId: string;
  private readonly endpoint: string;
  private readonly token?: string;
  private readonly cdpUrl: string;
  private readonly fetchImpl: typeof fetch;

  /** Open SSE controller; null until the stream is started. */
  private streamAbort: AbortController | null = null;
  private streamStarted = false;
  private readonly listeners = new Map<EventName, Set<Listener<EventName>>>();
  private lastSeq = -1;
  private closed = false;

  constructor(opts: CreateSessionOptions) {
    this.endpoint = opts.endpoint.replace(/\/+$/, "");
    this.token = opts.token;
    this.cdpUrl = opts.cdpUrl;
    this.sessionId = opts.sessionId ?? randomId();
    this.fetchImpl = opts.fetch ?? globalThis.fetch;
    if (typeof this.fetchImpl !== "function") {
      throw new Error("createSession: no `fetch` available — pass one in options");
    }
  }

  // ─── Driving (POST /mcp, JSON-RPC tools/call) ─────────────────────────────

  /** Navigate to a URL. Returns caption text + the fresh screenshot PNG. */
  open(url: string): Promise<DriveResult> {
    return this.callShot("browser_open", { url });
  }

  /** Click by ref (from readPage/find) or by CSS-pixel coordinates. */
  click(target: { ref: string } | { x: number; y: number }): Promise<DriveResult> {
    return this.callShot("browser_click", target);
  }

  /** Set a text field / checkbox / dropdown by ref. */
  formInput(ref: string, value: string): Promise<DriveResult> {
    return this.callShot("browser_form_input", { ref, value });
  }

  /** Type into the focused element (alias of a form_input on a ref). */
  type(ref: string, value: string): Promise<DriveResult> {
    return this.formInput(ref, value);
  }

  /** Press a single named key (Enter/Tab/Escape/Backspace/Arrow*). */
  key(key: string): Promise<DriveResult> {
    return this.callShot("browser_key", { key });
  }

  /** Scroll vertically by `dy` CSS pixels (positive = down). */
  scroll(dy: number): Promise<DriveResult> {
    return this.callShot("browser_scroll", { dy });
  }

  /** Take a fresh screenshot without acting. */
  screenshot(): Promise<DriveResult> {
    return this.callShot("browser_screenshot", {});
  }

  /** Read the current page as a ref listing. `interactiveOnly` defaults true. */
  async readPage(interactiveOnly = true): Promise<PageSnapshot> {
    const r = await this.callText("browser_read_page", { interactive_only: interactiveOnly });
    return parseListing(r.text);
  }

  /** Alias for readPage() — the accessibility snapshot. */
  snapshot(interactiveOnly = true): Promise<PageSnapshot> {
    return this.readPage(interactiveOnly);
  }

  /** Search the page for elements matching a query, ranked best-first. */
  async find(query: string): Promise<PageSnapshot> {
    const r = await this.callText("browser_find", { query });
    return parseListing(r.text);
  }

  /** Wait for a CSS selector and/or visible text (server-side poll). */
  waitFor(opts: { selector?: string; text?: string; timeoutSecs?: number }): Promise<DriveResult> {
    return this.callText("browser_wait_for", {
      selector: opts.selector,
      text: opts.text,
      timeout_secs: opts.timeoutSecs,
    });
  }

  /** List open tabs (popups/new tabs included), each with active flag. */
  async tabsList(): Promise<TabInfo[]> {
    const r = await this.callText("browser_tabs_list", {});
    return parseTabs(r.text);
  }

  /** Switch to another tab by index or targetId; returns the new page listing. */
  async tabsSwitch(sel: { index: number } | { targetId: string }): Promise<PageSnapshot> {
    const r = await this.callText("browser_tabs_switch", sel);
    return parseListing(r.text);
  }

  /** Read recent console messages. */
  console(): Promise<DriveResult> {
    return this.callText("browser_console", {});
  }

  /** List recent network responses. */
  network(): Promise<DriveResult> {
    return this.callText("browser_network", {});
  }

  /** Attach a local file to a file input by ref (path is on the ENGINE host). */
  upload(ref: string, path: string): Promise<DriveResult> {
    return this.callText("browser_upload", { ref, path });
  }

  /**
   * Explicitly hand off to a human. The engine pauses, banners the live view,
   * and returns a text cue. Prefer letting auto-detection fire; use this when
   * you know a step needs a human.
   */
  requestHuman(reason?: string, instructions?: string): Promise<DriveResult> {
    return this.callText("browser_request_human", { reason, instructions });
  }

  /**
   * Wait for the human to finish the paused step and resume. Resolves when the
   * `browser_state paused:false` transition is seen (or on timeout). This is the
   * server-side wait; you can also await the `state` event from the SSE stream.
   */
  waitForHuman(timeoutSecs = 300): Promise<DriveResult> {
    return this.callText("browser_wait_for_human", { timeout_secs: timeoutSecs });
  }

  /** Alias for waitForHuman() — resume after a handoff wall is cleared. */
  resume(timeoutSecs = 300): Promise<DriveResult> {
    return this.waitForHuman(timeoutSecs);
  }

  // ─── Live view (SSE) ──────────────────────────────────────────────────────

  /**
   * Subscribe to a typed event: `frame` | `cursor` | `narration` |
   * `human-needed` | `state` | `close`. Starts the SSE stream on first
   * subscription. Returns an unsubscribe function.
   */
  on<E extends EventName>(event: E, listener: Listener<E>): () => void {
    let set = this.listeners.get(event);
    if (!set) {
      set = new Set();
      this.listeners.set(event, set);
    }
    set.add(listener as Listener<EventName>);
    this.ensureStream();
    return () => this.off(event, listener);
  }

  /** Remove a previously-registered listener. */
  off<E extends EventName>(event: E, listener: Listener<E>): void {
    this.listeners.get(event)?.delete(listener as Listener<EventName>);
  }

  /**
   * Async iterator of screencast frames of the ACTIVE target, read off the SSE
   * stream. Yields until the stream ends or `close()` is called. Frames with a
   * stale `seq` are dropped (monotonic).
   */
  async *frames(): AsyncGenerator<Frame> {
    for await (const ev of this.events()) {
      if (ev.type === "frame") yield ev.frame;
    }
  }

  /**
   * Async iterator over ALL live-view events (frame/cursor/narration/
   * human-needed/state). A single SSE connection feeds it. Use this instead of
   * on()/off() when you prefer a for-await loop.
   */
  async *events(): AsyncGenerator<LiveEvent> {
    const abort = this.openStream();
    try {
      const res = await this.fetchImpl(this.url(ROUTES.events), {
        method: "GET",
        headers: this.headers({ accept: "text/event-stream" }),
        signal: abort.signal,
      });
      if (!res.ok || !res.body) {
        throw new Error(`envoyage /events: HTTP ${res.status}`);
      }
      for await (const sse of readSse(res.body, abort.signal)) {
        const ev = decodeEnvelope(sse.data);
        if (!ev) continue;
        if (ev.type === "frame") {
          if (ev.frame.seq <= this.lastSeq) continue;
          this.lastSeq = ev.frame.seq;
        }
        this.dispatch(ev);
        yield ev;
      }
    } catch (err) {
      if (!this.closed) this.emitClose(String(err));
    } finally {
      this.emitClose("stream ended");
    }
  }

  /**
   * Forward a human input event (click/key/scroll/pause/continue) to the engine
   * so the human can drive during a handoff. POST /input. Coordinates are PAGE
   * CSS pixels (un-letterbox the live view first).
   */
  async sendInput(event: InputEvent): Promise<void> {
    const res = await this.fetchImpl(this.url(ROUTES.input), {
      method: "POST",
      headers: this.headers({ "content-type": "application/json" }),
      body: JSON.stringify(event),
    });
    if (!res.ok) throw new Error(`envoyage /input: HTTP ${res.status}`);
  }

  /**
   * Close the SSE connection and the engine-side session. NEVER kills a browser
   * the SDK did not launch — for a `cdpUrl` session (CF Browser Run, remote
   * Chrome) the engine only disconnects; CF owns the browser lifecycle.
   */
  async close(): Promise<void> {
    this.closed = true;
    this.streamAbort?.abort();
    this.streamAbort = null;
    try {
      await this.callText("browser_close", {});
    } catch {
      // Best-effort — the caller is tearing down regardless.
    }
    this.emitClose("closed");
    this.listeners.clear();
  }

  // ─── internals ────────────────────────────────────────────────────────────

  private ensureStream(): void {
    if (this.streamStarted || this.closed) return;
    // Kick off the events() loop in the background; on()/off() consumers just
    // want dispatch. Errors surface via the `close` event.
    void (async () => {
      for await (const _ of this.events()) {
        /* dispatch happens inside events() */
      }
    })();
  }

  private openStream(): AbortController {
    if (this.streamAbort) this.streamAbort.abort();
    const abort = new AbortController();
    this.streamAbort = abort;
    this.streamStarted = true;
    this.lastSeq = -1;
    return abort;
  }

  private dispatch(ev: LiveEvent): void {
    const name = liveEventName(ev);
    const set = this.listeners.get(name);
    if (!set) return;
    const payload = liveEventPayload(ev);
    for (const l of set) {
      try {
        l(payload as never);
      } catch {
        /* a listener throwing must not break the stream */
      }
    }
  }

  private emitClose(reason: string): void {
    const set = this.listeners.get("close");
    if (!set) return;
    for (const l of set) {
      try {
        (l as Listener<"close">)({ reason });
      } catch {
        /* ignore */
      }
    }
  }

  /** A screenshot-returning tool: text caption + optional image. */
  private async callShot(name: string, args: unknown): Promise<DriveResult> {
    return this.toolCall(name, args);
  }

  /** A text-only tool. */
  private async callText(name: string, args: unknown): Promise<DriveResult> {
    return this.toolCall(name, args);
  }

  private async toolCall(name: string, args: unknown): Promise<DriveResult> {
    const body = {
      jsonrpc: "2.0",
      id: ++jsonRpcId,
      method: "tools/call",
      params: { name, arguments: pruneUndefined(args) },
    };
    const res = await this.fetchImpl(this.url(ROUTES.mcp), {
      method: "POST",
      headers: this.headers({ "content-type": "application/json", accept: "application/json" }),
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const txt = await res.text().catch(() => "");
      throw new Error(`envoyage ${name}: HTTP ${res.status}${txt ? ` — ${txt}` : ""}`);
    }
    const parsed = (await res.json()) as {
      error?: { message?: string };
      result?: { content?: ToolContent[]; isError?: boolean };
    };
    if (parsed.error) throw new Error(`envoyage ${name}: ${parsed.error.message ?? "RPC error"}`);
    const content = parsed.result?.content ?? [];
    const text = content
      .filter((c): c is Extract<ToolContent, { type: "text" }> => c.type === "text")
      .map((c) => c.text)
      .join("\n");
    const image = content.find(
      (c): c is Extract<ToolContent, { type: "image" }> => c.type === "image",
    )?.data;
    return { text, image, isError: parsed.result?.isError === true, raw: content };
  }

  private url(route: string): string {
    return `${this.endpoint}${route}`;
  }

  private headers(extra: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { "mcp-session-id": this.sessionId, ...extra };
    // The engine connects THIS session's browser to this CDP endpoint on first
    // use (lazy launch). Sent per-request so a multi-session engine process can
    // point each Mcp-Session-Id at a different remote browser. Ignored by an
    // engine started with a process-global --cdp-url or in local-spawn mode.
    if (this.cdpUrl && this.cdpUrl !== "local") h["x-envoyage-cdp-url"] = this.cdpUrl;
    if (this.token) h["authorization"] = `Bearer ${this.token}`;
    return h;
  }
}

// ─── live-event decoding (protocol.rs envelopes → typed SDK events) ──────────

/** A decoded live-view event with a discriminant matching the SDK event map. */
export type LiveEvent =
  | { type: "frame"; frame: Frame }
  | { type: "cursor"; cursor: SessionEvents["cursor"] }
  | { type: "narration"; narration: SessionEvents["narration"] }
  | { type: "human-needed"; human: HumanNeeded }
  | { type: "state"; state: DriveState };

function liveEventName(ev: LiveEvent): EventName {
  return ev.type === "human-needed" ? "human-needed" : ev.type;
}

function liveEventPayload(ev: LiveEvent): SessionEvents[EventName] {
  switch (ev.type) {
    case "frame":
      return ev.frame;
    case "cursor":
      return ev.cursor;
    case "narration":
      return ev.narration;
    case "human-needed":
      return ev.human;
    case "state":
      return ev.state;
  }
}

/**
 * Decode one engine envelope (`{"type":"browser_*", ...}`) into a typed
 * LiveEvent. Field names match src/protocol.rs. Unknown types → null.
 */
export function decodeEnvelope(json: string): LiveEvent | null {
  let v: Record<string, unknown>;
  try {
    v = JSON.parse(json) as Record<string, unknown>;
  } catch {
    return null;
  }
  switch (v.type) {
    case "browser_frame":
      return {
        type: "frame",
        frame: {
          pngBase64: str(v.png_base64),
          title: str(v.title),
          url: str(v.url),
          seq: num(v.seq),
        },
      };
    case "browser_cursor":
      return {
        type: "cursor",
        cursor: { x: num(v.x), y: num(v.y), action: str(v.action) as never },
      };
    case "browser_narration":
      return { type: "narration", narration: { text: str(v.text) } };
    case "browser_human_request": {
      const reason = str(v.reason);
      return {
        type: "human-needed",
        human: {
          kind: classifyHandoff(reason),
          reason,
          instructions: v.instructions == null ? undefined : str(v.instructions),
        },
      };
    }
    case "browser_state":
      return { type: "state", state: { paused: v.paused === true } };
    default:
      return null;
  }
}

// ─── listing parsers (engine's untrusted-framed text → structured) ───────────

const REF_LINE = /^\[(ref_\d+)\]\s+(\S+)\s+"((?:[^"\\]|\\.)*)"(?:\s+value:"((?:[^"\\]|\\.)*)")?/;

/**
 * Parse the engine's render_ax_listing output (read_page/find/tabs_switch). The
 * text is untrusted-framed; we pull Title/URL and each `[ref_N] role "name"`
 * line. `raw` keeps the full framed text for the model.
 */
export function parseListing(text: string): PageSnapshot {
  let title = "";
  let url = "";
  const elements: PageElement[] = [];
  for (const line of text.split("\n")) {
    if (title === "" && line.startsWith("Title:")) {
      title = line.slice("Title:".length).trim();
      continue;
    }
    if (url === "" && line.startsWith("URL:")) {
      url = line.slice("URL:".length).trim();
      continue;
    }
    const m = REF_LINE.exec(line.trim());
    if (m) {
      elements.push({
        ref: m[1],
        role: m[2],
        name: m[3],
        value: m[4] === undefined ? undefined : m[4],
      });
    }
  }
  return { title, url, elements, raw: text };
}

const TAB_LINE = /^(\*|\s)?\s*\[(\d+)\]\s+(.*?)\s{2,}(\S+)\s+\(targetId (\S+)\)/;

/** Parse the engine's tabs_list output into structured TabInfo[]. */
export function parseTabs(text: string): TabInfo[] {
  const tabs: TabInfo[] = [];
  for (const line of text.split("\n")) {
    const m = TAB_LINE.exec(line);
    if (!m) continue;
    tabs.push({
      active: m[1] === "*",
      index: Number(m[2]),
      title: m[3].trim(),
      url: m[4],
      targetId: m[5],
    });
  }
  return tabs;
}

// ─── tiny helpers ─────────────────────────────────────────────────────────────

function str(v: unknown): string {
  return typeof v === "string" ? v : v == null ? "" : String(v);
}
function num(v: unknown): number {
  return typeof v === "number" ? v : Number(v) || 0;
}
function pruneUndefined<T>(obj: T): T {
  if (obj == null || typeof obj !== "object") return obj;
  const out: Record<string, unknown> = {};
  for (const [k, val] of Object.entries(obj)) {
    if (val !== undefined) out[k] = val;
  }
  return out as T;
}
function randomId(): string {
  // crypto.randomUUID exists in Workers and Node 18+.
  const c = (globalThis as { crypto?: Crypto }).crypto;
  if (c?.randomUUID) return c.randomUUID();
  return `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

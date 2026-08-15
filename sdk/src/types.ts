// Public wire + event types. These MIRROR the engine's src/protocol.rs
// envelopes (browser_frame / browser_cursor / browser_narration /
// browser_human_request / browser_state) and the browser_* MCP tools in
// src/serve/tool_defs.rs. The engine is the source of truth; keep these in sync
// with protocol.rs (the field names are the wire contract).

// ─── Live-view events (arrive on the SSE stream) ─────────────────────────────

/** A screencast frame of the ACTIVE target. Wire tag: `browser_frame`. */
export interface Frame {
  /** Base64 PNG (consumers build a `data:image/png;base64,` URI). */
  pngBase64: string;
  title: string;
  url: string;
  /** Monotonic; drop any frame whose `seq` <= the last shown. */
  seq: number;
}

/** The kind of action a cursor marks. Matches CursorAction in protocol.rs. */
export type CursorAction = "move" | "click" | "type" | "scroll";

/**
 * What the agent is about to act on, in PAGE CSS pixels. The consumer glides
 * its OWN mascot cursor there. Wire tag: `browser_cursor`.
 */
export interface Cursor {
  x: number;
  y: number;
  action: CursorAction;
}

/** A short intent string for a balloon UI ("Clicking \"Sign in\""). */
export interface Narration {
  text: string;
}

/** The four states where a human must take over. Matches HandoffReason in the engine. */
export type HandoffKind = "password" | "captcha" | "cloudflare" | "oauth";

/**
 * Handoff signal: the engine hit something a human must solve. The browser is
 * PAUSED server-side; a typed password/OTP or a screenshot can NEVER reach the
 * caller while a wall is up (the boundary is enforced by the engine — the SDK
 * only surfaces the event). Wire tag: `browser_human_request`.
 *
 * `kind` is the SDK's best-effort classification of the free-text `reason` (the
 * engine's wire envelope carries `reason` + `instructions`, not a typed kind).
 */
export interface HumanNeeded {
  kind: HandoffKind;
  reason: string;
  instructions?: string;
}

/** The driving pause state. `paused` = a human is driving. Wire tag: `browser_state`. */
export interface DriveState {
  paused: boolean;
}

// ─── Input events (POSTed to /input for the human to drive during handoff) ────

/**
 * Human input forwarded to the engine. Coordinates are PAGE CSS pixels (the
 * caller un-letterboxes the live view to page space first). Matches the
 * `#[serde(tag = "kind")]` Input enum in protocol.rs.
 */
export type InputEvent =
  | { kind: "click"; x: number; y: number }
  | { kind: "key"; key: string }
  | { kind: "scroll"; dy: number }
  | { kind: "control"; action: "pause" | "continue" };

// ─── SDK event map (for on()/off()) ──────────────────────────────────────────

export interface SessionEvents {
  frame: Frame;
  cursor: Cursor;
  narration: Narration;
  "human-needed": HumanNeeded;
  state: DriveState;
  /** The SSE stream ended or errored (network drop / close()). */
  close: { reason: string };
}

export type EventName = keyof SessionEvents;
export type Listener<E extends EventName> = (payload: SessionEvents[E]) => void;

// ─── Config ──────────────────────────────────────────────────────────────────

export interface CreateSessionOptions {
  /**
   * CDP WebSocket URL of the browser to drive — a Cloudflare Browser Run
   * endpoint or any Chrome started with a remote debugging endpoint. The ENGINE
   * connects to this (server-side); the SDK never touches it.
   */
  cdpUrl: string;
  /** Base URL of the running `envoyage serve` engine (e.g. https://envoyage.example.com). */
  endpoint: string;
  /** Bearer token if the engine has ENVOYAGE_AUTH_TOKEN set. */
  token?: string;
  /**
   * Session id sent as the `Mcp-Session-Id` header so one engine process can
   * multiplex many independent browsers. Auto-generated if omitted.
   */
  sessionId?: string;
  /**
   * Optional custom fetch (defaults to global `fetch`). Useful in a Worker to
   * pass a bound fetcher, or in tests to stub the transport.
   */
  fetch?: typeof fetch;
}

export interface LaunchOptions {
  /** Base URL of the running `envoyage serve` engine. */
  endpoint: string;
  token?: string;
  sessionId?: string;
  fetch?: typeof fetch;
}

// ─── Bounded crawling (POST/GET/DELETE /crawls) ─────────────────────────────

export type CrawlDiscovery = "sitemap_and_links" | "sitemap_only" | "links_only";
export type CrawlRenderPolicy = "auto" | "static" | "browser";
export type CrawlAdapter = "auto" | "generic" | "shopify_collection" | "shopify_product";
export type CrawlState = "queued" | "running" | "completed" | "failed" | "cancelled";

export interface CrawlLimits {
  maxPages?: number;
  maxDepth?: number;
  maxAssets?: number;
  maxContentBytes?: number;
  maxDurationSecs?: number;
  maxConcurrency?: number;
}

export interface CrawlCapture {
  sections?: boolean;
  links?: boolean;
  media?: boolean;
  markdown?: boolean;
  html?: boolean;
}

export interface CrawlRequest {
  url: string;
  adapter?: CrawlAdapter;
  allowedHosts?: string[];
  includePaths?: string[];
  excludePaths?: string[];
  discovery?: CrawlDiscovery;
  render?: CrawlRenderPolicy;
  capture?: CrawlCapture;
  limits?: CrawlLimits;
}

export interface CrawlSection {
  level: number;
  heading: string;
  text: string;
}

export interface CrawlMedia {
  id: string;
  position: number;
  url: string;
  alt?: string;
  width?: number;
  height?: number;
}

export interface CrawlPage {
  url: string;
  canonicalUrl?: string;
  pageType?: string;
  productKey?: string;
  breadcrumbs: string[];
  title?: string;
  description?: string;
  statusCode?: number;
  sections: CrawlSection[];
  links: string[];
  media: CrawlMedia[];
  markdown?: string;
  html?: string;
  contentSha256: string;
  truncated: boolean;
}

export interface CrawlProgress {
  completedPages: number;
  totalPages: number;
  returnedPages: number;
  returnedAssets: number;
  returnedContentBytes: number;
}

export interface CrawlJob {
  id: string;
  adapter: string;
  adapterVersion: string;
  state: CrawlState;
  requestFingerprint: string;
  createdAtMs: number;
  progress: CrawlProgress;
  pages: CrawlPage[];
  nextCursor?: string;
  warnings: string[];
  error?: string;
}

export interface CrawlAssetDownload {
  contentType: string;
  sha256: string;
  bytes: Uint8Array;
}

export interface CreateCrawlClientOptions {
  /** Base URL of the running Envoyage engine. */
  endpoint: string;
  /** Bearer token if the engine has ENVOYAGE_AUTH_TOKEN set. */
  token?: string;
  /** Optional Workers-safe fetch replacement. */
  fetch?: typeof fetch;
}

// ─── Driving results ──────────────────────────────────────────────────────────

/** One `content` item from a tool call (MCP shape). */
export type ToolContent =
  | { type: "text"; text: string }
  | { type: "image"; data: string; mimeType: string };

/**
 * The result of a driving call. `text` is the joined text captions; `image` is
 * the returned screenshot PNG (base64) when the tool returned one (open, click,
 * type, key, scroll, screenshot). `isError` is set when the engine flagged the
 * tool result as an error. `raw` is the untouched content array.
 */
export interface DriveResult {
  text: string;
  image?: string;
  isError: boolean;
  raw: ToolContent[];
}

/** One element from a read_page / find listing (parsed from the ref listing). */
export interface PageElement {
  ref: string;
  role: string;
  name: string;
  value?: string;
  /**
   * True when the engine masked this element's value (a `type="password"` field,
   * always; or a field matched by `maskAllInputs` / `maskSelector` /
   * `[data-envoyage-mask]`). Present on items parsed directly from
   * `AX_SNAPSHOT_JS`; the `render_ax_listing` text path simply omits the value,
   * so `parseListing()` leaves this `undefined`.
   */
  masked?: boolean;
}

/** A parsed read_page / find snapshot. `raw` keeps the untrusted-framed text. */
export interface PageSnapshot {
  title: string;
  url: string;
  elements: PageElement[];
  raw: string;
}

/** One open tab from tabsList(). */
export interface TabInfo {
  index: number;
  targetId: string;
  title: string;
  url: string;
  active: boolean;
}

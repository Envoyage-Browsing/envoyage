//! Process-global browser state, pause flag, and the WS<->pump channels.
//!
//! envoyage collapses ImmorTerm's two-process split (browser in the MCP process,
//! panel in the daemon): here the browser, the MCP tools, the screencast pump,
//! and the WS server all live in ONE `envoyage serve` process. So the pump reads
//! frames straight off the in-process [`BrowserSession`] (no IPC) and pushes
//! protocol events onto a broadcast channel; WS clients subscribe to it and
//! feed [`protocol::Input`] back through an input queue the pump drains.

use crate::browser::BrowserSession;
use crate::protocol::Input;
use crate::serve::recorder::Recording;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, mpsc};

/// The default session id for transports that carry no session id of their own
/// (the stdio MCP loop — one process per Claude session, so one session). The
/// HTTP transport keys by the client's `Mcp-Session-Id` header instead.
pub const DEFAULT_SESSION: &str = "stdio";

/// One session's browser slot: the exact `Mutex<Option<BrowserSession>>` the
/// single-browser design used, now one PER session so tool calls on different
/// sessions never serialize against each other (a slow navigate in session A
/// doesn't block session B). `Arc` so the caller can clone the slot out of the
/// registry and drop the (short-lived) registry lock before running the tool.
pub type BrowserSlot = Arc<Mutex<Option<BrowserSession>>>;

/// The registry of self-driven browsers, keyed by session id. ONE `envoyage
/// serve` process holds N independent [`BrowserSession`]s — each its own CDP
/// connection (local spawn or remote `connect`) — so a cloud deployment can
/// multiplex many agents through a single process. Each slot is launched lazily
/// on that session's first browser tool use and reused after. All multi-session
/// bookkeeping lives HERE in serve/; the crate's `BrowserSession` stays a single
/// unchanged object.
///
/// The registry `Mutex` is held only to get-or-create a session's slot; the
/// per-session `BrowserSlot` mutex is what serializes work within one session.
static BROWSERS: OnceLock<Mutex<HashMap<String, BrowserSlot>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, BrowserSlot>> {
    BROWSERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get (or create) the browser slot for `session_id`. Cheap: clones an `Arc`.
pub fn browser_slot(session_id: &str) -> BrowserSlot {
    let mut map = registry().lock().expect("browser registry poisoned");
    map.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Remove a session's slot entirely (on `browser_close`), returning it so the
/// caller can reap the browser. The slot's `Drop` closes the browser if the
/// caller doesn't take it out first.
pub fn remove_session(session_id: &str) -> Option<BrowserSlot> {
    registry().lock().ok().and_then(|mut m| m.remove(session_id))
}

/// A remote CDP WebSocket URL to drive INSTEAD of spawning a local browser.
/// Set once at startup from `--cdp-url`; `with_browser` reads it to decide
/// between `BrowserSession::connect` (remote) and `::launch` (local spawn).
static CDP_URL: OnceLock<Option<String>> = OnceLock::new();

/// Record the remote CDP URL (or `None` for local spawn). Call once at startup.
pub fn set_cdp_url(url: Option<String>) {
    let _ = CDP_URL.set(url);
}

/// The configured remote CDP URL, if any.
pub fn cdp_url() -> Option<&'static str> {
    CDP_URL.get().and_then(|o| o.as_deref())
}

/// Per-session paused flag toggled by the human via the WS UI. While paused
/// envoyage still streams frames + forwards the human's input for THAT session,
/// but its MCP tools return text-only (no screenshot to the model — passwords
/// never leave to the LLM). Per-session so a handoff in one session never pauses
/// another's driving.
static PAUSED: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

fn paused_map() -> &'static Mutex<HashMap<String, bool>> {
    PAUSED.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn is_paused(session_id: &str) -> bool {
    paused_map()
        .lock()
        .map(|m| m.get(session_id).copied().unwrap_or(false))
        .unwrap_or(false)
}

pub fn set_paused(session_id: &str, v: bool) {
    if let Ok(mut m) = paused_map().lock() {
        m.insert(session_id.to_string(), v);
    }
}

/// A serialized protocol envelope (`{"type":"browser_*", ...}`) broadcast to
/// every connected WS client. `String` (not the typed event) so a client that
/// joins late simply misses old frames — coalescing is inherent.
pub type WsEnvelope = String;

/// Lazily-created broadcast bus (pump -> WS clients) and input queue
/// (WS clients -> pump). Both are process-global so the MCP stdio loop, the
/// pump thread, and the WS accept loop share one browser.
struct Channels {
    tx: broadcast::Sender<WsEnvelope>,
    input_tx: mpsc::UnboundedSender<Input>,
    input_rx: Mutex<mpsc::UnboundedReceiver<Input>>,
}

static CHANNELS: OnceLock<Channels> = OnceLock::new();

fn channels() -> &'static Channels {
    CHANNELS.get_or_init(|| {
        let (tx, _) = broadcast::channel(256);
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        Channels { tx, input_tx, input_rx: Mutex::new(input_rx) }
    })
}

/// The last envelope of each replay-worthy type, so a WS client that connects
/// mid-session sees the CURRENT state immediately instead of a blank frame that
/// only fills once the page next changes. Without this, a late joiner on a
/// STATIC page (e.g. a login screen during a human handoff) sees nothing —
/// `Page.screencastFrame` only fires on visual change. Keyed by envelope `type`:
/// `browser_frame` (the last painted frame), `browser_narration`,
/// `browser_human_request`, and `browser_state` (so the handoff banner + pause
/// state re-appear for anyone joining during a handoff).
static REPLAY: OnceLock<Mutex<std::collections::HashMap<&'static str, WsEnvelope>>> = OnceLock::new();

fn replay() -> &'static Mutex<std::collections::HashMap<&'static str, WsEnvelope>> {
    REPLAY.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// The envelope types we cache + replay to a fresh subscriber (in this order).
const REPLAY_TYPES: [&str; 4] =
    ["browser_frame", "browser_narration", "browser_state", "browser_human_request"];

/// Broadcast one envelope to all WS clients (best-effort; no clients = no-op).
/// Caches replay-worthy envelopes so a late joiner can be caught up on subscribe.
pub fn broadcast_envelope(env: WsEnvelope) {
    if let Some(kind) = REPLAY_TYPES.iter().find(|k| env.contains(&format!("\"type\":\"{k}\"")))
        && let Ok(mut map) = replay().lock()
    {
        // A resume (browser_state paused:false) clears a stale handoff banner
        // so a fresh client doesn't get banner-ed after the human finished.
        if *kind == "browser_state" && env.contains("\"paused\":false") {
            map.remove("browser_human_request");
        }
        map.insert(kind, env.clone());
    }
    let _ = channels().tx.send(env);
}

/// Subscribe a new WS client to the envelope stream.
pub fn subscribe() -> broadcast::Receiver<WsEnvelope> {
    channels().tx.subscribe()
}

/// The cached envelopes to replay to a client that just connected, in a sane
/// order (frame first so the picture paints, then narration/state/handoff).
pub fn replay_envelopes() -> Vec<WsEnvelope> {
    let map = match replay().lock() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    REPLAY_TYPES.iter().filter_map(|k| map.get(*k).cloned()).collect()
}

/// A WS client sends one human input event toward the pump.
pub fn push_input(ev: Input) {
    let _ = channels().input_tx.send(ev);
}

/// Drain all queued human input (called by the pump each tick).
pub fn drain_input() -> Vec<Input> {
    let mut out = Vec::new();
    if let Ok(mut rx) = channels().input_rx.lock() {
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
    }
    out
}

// ─── GIF recording buffer ───────────────────────────────────────────
//
// The `browser_gif` tool buffers screencast frames while recording is on. The
// pump appends each broadcast frame's PNG (with whatever overlay hint the last
// action left pending); MCP tool dispatch sets that pending hint when it emits a
// cursor/narration. Both sides share this one mutex.

/// The recording buffer + captured frames. `None` = not recording (frames kept
/// after `stop_recording` until `clear`/new `start_recording`).
static RECORDING: OnceLock<Mutex<Recording>> = OnceLock::new();

/// Access the shared recording buffer.
pub fn recording() -> &'static Mutex<Recording> {
    RECORDING.get_or_init(|| Mutex::new(Recording::new()))
}

/// Pump hook: append a frame's PNG to the recording iff recording is active.
/// Attaches (and consumes) any pending overlay hint. No-op when not recording.
pub fn record_frame(png_base64: &str) {
    if let Ok(mut rec) = recording().lock() {
        rec.push_frame(png_base64);
    }
}

/// MCP hook: stamp the overlay hint (cursor + label) that the next captured
/// frame should carry. No-op when not recording.
pub fn record_overlay(cursor: Option<(f64, f64)>, label: Option<String>) {
    if let Ok(mut rec) = recording().lock() {
        rec.set_pending_overlay(cursor, label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The replay cache lets a late WS joiner see the current frame + any active
    // handoff banner instead of a blank view on a static page. Serialized under
    // one test since REPLAY is a process-global.
    #[test]
    fn replay_caches_last_frame_and_resume_clears_banner() {
        // A frame + a handoff both broadcast → both replay, frame first.
        broadcast_envelope(r#"{"type":"browser_frame","seq":9,"png_base64":"AA"}"#.into());
        broadcast_envelope(r#"{"type":"browser_human_request","reason":"login"}"#.into());
        broadcast_envelope(r#"{"type":"browser_state","paused":true}"#.into());
        let r = replay_envelopes();
        assert!(r[0].contains("\"type\":\"browser_frame\""), "frame replays first");
        assert!(r.iter().any(|e| e.contains("browser_human_request")), "handoff banner replays");

        // A resume (paused:false) must clear the stale banner so a fresh client
        // doesn't get banner-ed after the human finished.
        broadcast_envelope(r#"{"type":"browser_state","paused":false}"#.into());
        let r2 = replay_envelopes();
        assert!(
            !r2.iter().any(|e| e.contains("browser_human_request")),
            "resume clears the cached handoff banner"
        );
    }

    // The registry is the multi-session core: get-or-create returns the SAME slot
    // for one id and DISTINCT slots for distinct ids; remove drops it; pause state
    // is isolated per session.
    #[test]
    fn registry_and_pause_are_per_session() {
        let a1 = browser_slot("sess-a");
        let a2 = browser_slot("sess-a");
        let b1 = browser_slot("sess-b");
        // Same id → same underlying slot (Arc points at one Mutex).
        assert!(Arc::ptr_eq(&a1, &a2), "one slot per session id");
        assert!(!Arc::ptr_eq(&a1, &b1), "distinct sessions get distinct slots");

        // Pause is isolated: pausing A must not pause B.
        set_paused("sess-a", true);
        assert!(is_paused("sess-a"));
        assert!(!is_paused("sess-b"));

        // Remove drops the slot; a later get-or-create makes a fresh (distinct) one.
        assert!(remove_session("sess-a").is_some());
        let a3 = browser_slot("sess-a");
        assert!(!Arc::ptr_eq(&a1, &a3), "removed session is recreated fresh");

        // Cleanup so process-global registry doesn't leak into other tests.
        remove_session("sess-a");
        remove_session("sess-b");
        set_paused("sess-a", false);
    }
}

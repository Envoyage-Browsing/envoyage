//! Process-global browser state, pause flag, and the WS<->pump channels.
//!
//! envoyage collapses ImmorTerm's two-process split (browser in the MCP process,
//! panel in the daemon): here the browser, the MCP tools, the screencast pump,
//! and the WS server all live in ONE `envoyage serve` process. So the pump reads
//! frames straight off the in-process [`BrowserSession`] (no IPC) and pushes
//! protocol events onto a broadcast channel; WS clients subscribe to it and
//! feed [`protocol::Input`] back through an input queue the pump drains.

use crate::browser::BrowserSession;
use crate::agent_contract;
use crate::protocol::Input;
use crate::serve::recorder::Recording;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, mpsc};

/// The default session id for transports that carry no session id of their own
/// (the stdio MCP loop — one process per Claude session, so one session). The
/// HTTP transport keys by the client's `Mcp-Session-Id` header instead.
pub const DEFAULT_SESSION: &str = "stdio";

/// The live view only needs recent frames. A small bounded bus prevents a slow
/// or paused consumer from retaining hundreds of full base64 images.
const LIVE_VIEW_BUS_CAP: usize = 8;
/// Human input is also bounded so a disconnected or hostile viewer cannot grow
/// an unbounded queue while the browser is busy.
const INPUT_QUEUE_CAP: usize = 256;

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

/// Per-session remote CDP URLs from the `x-envoyage-cdp-url` request header. The
/// dashboard mints a FRESH Cloudflare Browser Rendering browser per session and
/// sends its connection URL here, so each session drives its OWN remote browser
/// (real isolation) instead of every session collapsing onto the one process-
/// global `--cdp-url` — or silently falling through to a local spawn.
static SESSION_CDP_URLS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn session_cdp_urls() -> &'static Mutex<HashMap<String, String>> {
    SESSION_CDP_URLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a per-session remote CDP URL (from the request header). Last write wins
/// so that after a session's remote browser expires and the dashboard mints a new
/// one, the next (re)connect targets the fresh URL. Consulted only at connect
/// time (`with_browser` caches the live browser), so this never yanks a live one.
pub fn set_session_cdp_url(session_id: &str, url: String) {
    session_cdp_urls()
        .lock()
        .expect("session cdp urls poisoned")
        .insert(session_id.to_string(), url);
}

/// The remote CDP URL to use for `session_id`: the per-session header value if
/// present, else the process-global `--cdp-url`. `None` ⇒ spawn a local browser.
pub fn cdp_url_for(session_id: &str) -> Option<String> {
    if let Ok(m) = session_cdp_urls().lock()
        && let Some(u) = m.get(session_id)
    {
        return Some(u.clone());
    }
    cdp_url().map(str::to_string)
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

/// Best bounded region associated with the most recent drive action. Visual
/// proof can use it for a changed-region or cursor fallback without returning
/// screenshots from the action itself.
pub type VisualHint = agent_contract::VisualRegion;

static VISUAL_HINTS: OnceLock<Mutex<HashMap<String, VisualHint>>> = OnceLock::new();

fn visual_hints() -> &'static Mutex<HashMap<String, VisualHint>> {
    VISUAL_HINTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn set_visual_hint(session_id: &str, hint: VisualHint) {
    if let Ok(mut hints) = visual_hints().lock() {
        hints.insert(session_id.to_string(), hint);
    }
}

pub fn visual_hint(session_id: &str) -> Option<VisualHint> {
    visual_hints().lock().ok().and_then(|hints| hints.get(session_id).cloned())
}

pub type ImageUsage = agent_contract::ImageUsage;

static IMAGE_USAGE: OnceLock<Mutex<HashMap<String, ImageUsage>>> = OnceLock::new();

fn image_usage() -> &'static Mutex<HashMap<String, ImageUsage>> {
    IMAGE_USAGE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn image_usage_for(session_id: &str) -> ImageUsage {
    image_usage().lock().ok().and_then(|usage| usage.get(session_id).copied()).unwrap_or_default()
}

pub fn add_image_usage(session_id: &str, pixels: u64, bytes: u64) -> ImageUsage {
    let mut usage = image_usage().lock().expect("image usage poisoned");
    let total = usage.entry(session_id.to_string()).or_default();
    total.count = total.count.saturating_add(1);
    total.pixels = total.pixels.saturating_add(pixels);
    total.bytes = total.bytes.saturating_add(bytes);
    *total
}

/// A serialized protocol envelope (`{"type":"browser_*", ...}`) broadcast to
/// every connected live-view client (WS or SSE) of one session. `String` (not
/// the typed event) so a client that joins late simply misses old frames —
/// coalescing is inherent.
pub type WsEnvelope = String;

/// The envelope types we cache + replay to a fresh subscriber (in this order:
/// frame first so the picture paints, then narration/state/handoff banner).
const REPLAY_TYPES: [&str; 4] =
    ["browser_frame", "browser_narration", "browser_state", "browser_human_request"];

/// One session's live-view bus: the broadcast channel (pump -> viewers), the
/// human-input queue (viewers -> pump), and the replay cache (last frame +
/// narration + pause state + handoff banner) so a client that connects
/// mid-session sees the CURRENT state instead of a blank frame that only fills
/// on the next visual change. Per session so many sessions multiplex on one
/// process, each with its own live view + input path.
struct Channels {
    tx: broadcast::Sender<WsEnvelope>,
    input_tx: mpsc::Sender<Input>,
    input_rx: Mutex<mpsc::Receiver<Input>>,
    /// Wakes the pump the instant input arrives so it dispatches without waiting
    /// out the frame tick (cuts perceived input latency from up to PUMP_TICK to
    /// ~0). A plain std channel so the std pump thread can block on it with a
    /// timeout; a `()` is sent per input, drained on wake.
    wake_tx: std::sync::mpsc::Sender<()>,
    wake_rx: Mutex<std::sync::mpsc::Receiver<()>>,
    /// Last envelope of each replay-worthy `type`. Without this a late joiner on
    /// a STATIC page (a login screen during a handoff) would see nothing —
    /// `Page.screencastFrame` only fires on visual change.
    replay: Mutex<HashMap<&'static str, WsEnvelope>>,
}

impl Channels {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(LIVE_VIEW_BUS_CAP);
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE_CAP);
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        Channels {
            tx,
            input_tx,
            input_rx: Mutex::new(input_rx),
            wake_tx,
            wake_rx: Mutex::new(wake_rx),
            replay: Mutex::new(HashMap::new()),
        }
    }
}

/// The registry of per-session live-view buses, created lazily on first use
/// (first frame broadcast or first subscribe). `Arc` so a caller can clone the
/// bus out and drop the registry lock before doing channel work.
static CHANNELS: OnceLock<Mutex<HashMap<String, Arc<Channels>>>> = OnceLock::new();

fn channels_registry() -> &'static Mutex<HashMap<String, Arc<Channels>>> {
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get (or create) `session_id`'s live-view bus.
fn channels(session_id: &str) -> Arc<Channels> {
    let mut map = channels_registry().lock().expect("channels registry poisoned");
    map.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Channels::new()))
        .clone()
}

/// Broadcast one envelope to all of `session_id`'s viewers (best-effort; no
/// viewers = no-op). Caches replay-worthy envelopes so a late joiner can be
/// caught up on subscribe.
pub fn broadcast_envelope_to(session_id: &str, env: WsEnvelope) {
    let ch = channels(session_id);
    if let Some(kind) = REPLAY_TYPES.iter().find(|k| env.contains(&format!("\"type\":\"{k}\"")))
        && let Ok(mut map) = ch.replay.lock()
    {
        // A resume (browser_state paused:false) clears a stale handoff banner
        // so a fresh client doesn't get banner-ed after the human finished.
        if *kind == "browser_state" && env.contains("\"paused\":false") {
            map.remove("browser_human_request");
        }
        map.insert(kind, env.clone());
    }
    let _ = ch.tx.send(env);
}

/// Subscribe a new viewer to `session_id`'s envelope stream.
pub fn subscribe_to(session_id: &str) -> broadcast::Receiver<WsEnvelope> {
    channels(session_id).tx.subscribe()
}

/// The cached envelopes to replay to a viewer that just connected to
/// `session_id`, in a sane order (frame first, then narration/state/handoff).
pub fn replay_envelopes_of(session_id: &str) -> Vec<WsEnvelope> {
    let ch = channels(session_id);
    let map = match ch.replay.lock() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    REPLAY_TYPES.iter().filter_map(|k| map.get(*k).cloned()).collect()
}

/// A viewer sends one human input event toward `session_id`'s pump.
pub fn push_input_to(session_id: &str, ev: Input) {
    let ch = channels(session_id);
    if ch.input_tx.try_send(ev).is_ok() {
        // Wake the pump so it dispatches this event now instead of on the next tick.
        let _ = ch.wake_tx.send(());
    }
}

/// Wake a session pump without queueing input (used to stop it promptly).
pub fn wake_pump(session_id: &str) {
    let ch = channels(session_id);
    let _ = ch.wake_tx.send(());
}

/// Drop all non-browser state retained for a closed session, including the last
/// base64 frame. The pump is stopped before this is called.
pub fn clear_session_state(session_id: &str) {
    if let Ok(mut map) = session_cdp_urls().lock() {
        map.remove(session_id);
    }
    if let Ok(mut map) = paused_map().lock() {
        map.remove(session_id);
    }
    if let Ok(mut map) = visual_hints().lock() {
        map.remove(session_id);
    }
    if let Ok(mut map) = image_usage().lock() {
        map.remove(session_id);
    }
    if let Ok(mut map) = channels_registry().lock() {
        map.remove(session_id);
    }
}

/// Block the pump thread until input arrives (wakes early) or `tick` elapses
/// (the regular frame cadence). Drains any extra wake signals so a burst of
/// inputs collapses into one wake cycle rather than spinning.
pub fn park_until_input_or(session_id: &str, tick: std::time::Duration) {
    let ch = channels(session_id);
    if let Ok(rx) = ch.wake_rx.lock() {
        // recv_timeout returns Ok on the first wake, Err(Timeout) after `tick`.
        let _ = rx.recv_timeout(tick);
        while rx.try_recv().is_ok() {} // coalesce a burst
    } else {
        std::thread::sleep(tick);
    }
}

/// Drain all queued human input for `session_id` (called by the pump each tick).
pub fn drain_input_of(session_id: &str) -> Vec<Input> {
    let ch = channels(session_id);
    let mut out = Vec::new();
    if let Ok(mut rx) = ch.input_rx.lock() {
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
    }
    out
}

// Back-compat single-session wrappers: the WS surface streams the DEFAULT_SESSION
// (the stdio/WS session), so ws.rs subscribes / replays / pushes input against it
// without threading a session id it doesn't have.
pub fn subscribe() -> broadcast::Receiver<WsEnvelope> {
    subscribe_to(DEFAULT_SESSION)
}
pub fn replay_envelopes() -> Vec<WsEnvelope> {
    replay_envelopes_of(DEFAULT_SESSION)
}
pub fn push_input(ev: Input) {
    push_input_to(DEFAULT_SESSION, ev);
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
        // Isolated per-session bus so this doesn't pollute another test's session.
        let sid = "replay-test";
        // A frame + a handoff both broadcast → both replay, frame first.
        broadcast_envelope_to(sid, r#"{"type":"browser_frame","seq":9,"png_base64":"AA"}"#.into());
        broadcast_envelope_to(sid, r#"{"type":"browser_human_request","reason":"login"}"#.into());
        broadcast_envelope_to(sid, r#"{"type":"browser_state","paused":true}"#.into());
        let r = replay_envelopes_of(sid);
        assert!(r[0].contains("\"type\":\"browser_frame\""), "frame replays first");
        assert!(r.iter().any(|e| e.contains("browser_human_request")), "handoff banner replays");

        // A resume (paused:false) must clear the stale banner so a fresh client
        // doesn't get banner-ed after the human finished.
        broadcast_envelope_to(sid, r#"{"type":"browser_state","paused":false}"#.into());
        let r2 = replay_envelopes_of(sid);
        assert!(
            !r2.iter().any(|e| e.contains("browser_human_request")),
            "resume clears the cached handoff banner"
        );
    }

    // Per-session isolation: an envelope broadcast to session A must NOT appear
    // in session B's replay cache — the multiplexing guarantee the SSE surface
    // relies on so one session's frames never leak into another's live view.
    #[test]
    fn buses_are_isolated_per_session() {
        broadcast_envelope_to("iso-a", r#"{"type":"browser_frame","seq":1,"png_base64":"AA"}"#.into());
        assert!(replay_envelopes_of("iso-a").iter().any(|e| e.contains("\"seq\":1")));
        assert!(
            replay_envelopes_of("iso-b").is_empty(),
            "session B's bus must not see session A's frame"
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

    #[test]
    fn live_view_bus_drops_old_frames_instead_of_retaining_them() {
        let sid = "bounded-bus-test";
        let mut rx = subscribe_to(sid);
        for seq in 0..(LIVE_VIEW_BUS_CAP + 5) {
            broadcast_envelope_to(
                sid,
                format!(r#"{{"type":"browser_frame","seq":{seq},"png_base64":"AA"}}"#),
            );
        }
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(5))
        ));
        clear_session_state(sid);
    }

    #[test]
    fn clearing_a_session_drops_its_replay_frame_and_pause_state() {
        let sid = "clear-session-test";
        broadcast_envelope_to(sid, r#"{"type":"browser_frame","png_base64":"large"}"#.into());
        set_paused(sid, true);
        clear_session_state(sid);
        assert!(!is_paused(sid));
        assert!(replay_envelopes_of(sid).is_empty());
        clear_session_state(sid);
    }
}

//! Process-global browser state, pause flag, and the WS<->pump channels.
//!
//! rudder collapses ImmorTerm's two-process split (browser in the MCP process,
//! panel in the daemon): here the browser, the MCP tools, the screencast pump,
//! and the WS server all live in ONE `rudder serve` process. So the pump reads
//! frames straight off the in-process [`BrowserSession`] (no IPC) and pushes
//! protocol events onto a broadcast channel; WS clients subscribe to it and
//! feed [`protocol::Input`] back through an input queue the pump drains.

use crate::browser::BrowserSession;
use crate::protocol::Input;
use crate::serve::recorder::Recording;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::{broadcast, mpsc};

/// The single self-driven browser for this process. Launched lazily on first
/// browser tool use, reused after.
static BROWSER: OnceLock<Mutex<Option<BrowserSession>>> = OnceLock::new();

pub fn browser_slot() -> &'static Mutex<Option<BrowserSession>> {
    BROWSER.get_or_init(|| Mutex::new(None))
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

/// Paused flag toggled by the human via the WS UI. While paused rudder still
/// streams frames + forwards the human's input, but MCP tools return text-only
/// (no screenshot to the model — passwords never leave to the LLM).
static PAUSED: AtomicBool = AtomicBool::new(false);

pub fn is_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

pub fn set_paused(v: bool) {
    PAUSED.store(v, Ordering::Relaxed);
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

/// Broadcast one envelope to all WS clients (best-effort; no clients = no-op).
pub fn broadcast_envelope(env: WsEnvelope) {
    let _ = channels().tx.send(env);
}

/// Subscribe a new WS client to the envelope stream.
pub fn subscribe() -> broadcast::Receiver<WsEnvelope> {
    channels().tx.subscribe()
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

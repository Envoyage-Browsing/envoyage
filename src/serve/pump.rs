//! The screencast pump + shared browser access.
//!
//! `with_browser` serializes all browser access behind the process mutex and
//! honors the cross-process ownership lock. The pump thread drives the live
//! screencast: each tick it dispatches any queued human input, arms the
//! screencast, and broadcasts the newest frame as a `browser_frame` envelope.

use super::state;
use crate::browser::BrowserSession;
use crate::browser_lock;
use crate::protocol::{Frame, Input};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Session ids whose screencast pump thread is already running (each pump idles
/// cheaply when its browser is closed, so spawn at most one per session).
static PUMPS_STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn pumps_started() -> &'static Mutex<HashSet<String>> {
    PUMPS_STARTED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// ~15fps. Frame coalescing means a slower tick just drops intermediate frames.
const PUMP_TICK: Duration = Duration::from_millis(66);

/// Ensure this session's browser exists (launch on first use), then run `f`.
/// Consults the ownership lock: refuse to launch over a live foreign owner's
/// shared profile. Auto-resets a dead-pipe session so the next call relaunches.
///
/// `session_id` keys the registry — ONE serve process holds N independent
/// browsers, one per session id. Different sessions never serialize against each
/// other: each has its own slot mutex, held only for the duration of ITS `f`.
pub fn with_browser<T>(
    session_id: &str,
    launch_url: Option<&str>,
    f: impl FnOnce(&mut BrowserSession) -> Result<T, String>,
) -> Result<T, String> {
    let slot = state::browser_slot(session_id);
    let mut guard = slot
        .lock()
        .map_err(|_| "browser lock poisoned".to_string())?;
    if guard.is_none() {
        // The core's launch/connect take a tokio runtime handle; the pipe path
        // never uses it, the WS path builds its own — a throwaway current-thread
        // runtime satisfies the signature.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|e| e.to_string())?;

        // Remote path: drive a browser we did NOT spawn (Cloudflare Browser Run
        // etc.). No broker lock — that's about a local shared profile dir, which
        // a remote connection has neither of.
        if let Some(cdp_url) = state::cdp_url() {
            let session = BrowserSession::connect(&rt, cdp_url)?;
            if let Some(url) = launch_url {
                // A remote browser is already on a page; honor an explicit open.
                let mut s = session;
                s.navigate(url)?;
                *guard = Some(s);
            } else {
                *guard = Some(session);
            }
        } else {
            // Local spawn path: honor the one-browser-per-user broker lock.
            let self_pid = std::process::id();
            if let browser_lock::Decision::RouteTo { owner_pid, .. } =
                browser_lock::decide(browser_lock::read().as_ref(), self_pid)
            {
                return Err(format!(
                    "envoyage's browser is already open and owned by another session \
                     (pid {owner_pid}). Use that session's browser, or close it first."
                ));
            }
            let url = launch_url.unwrap_or("about:blank");
            let session = BrowserSession::launch(&rt, url)?;
            if let Ok(nonce) = browser_lock::acquire(self_pid, 0, session.pid())
                && !browser_lock::confirm_nonce(&nonce)
            {
                drop(session);
                return Err("Lost a race to open the browser to another session — retry.".to_string());
            }
            *guard = Some(session);
        }
    }
    let session = guard.as_mut().unwrap();
    let result = session.ensure_live_target().and_then(|()| f(session));
    if let Err(e) = &result
        && is_dead_pipe(e)
    {
        *guard = None; // Drop -> close() reaps the exact pid.
        if browser_lock::read()
            .map(|l| l.owner_pid == std::process::id())
            .unwrap_or(false)
        {
            browser_lock::release();
        }
        return Err("The browser closed — call browser_open to start a fresh one.".to_string());
    }
    result
}

/// Does this error signal the CDP pipe is dead (crash / user closed the window)?
fn is_dead_pipe(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("pipe closed")
        || m.contains("broken pipe")
        || m.contains("cdp send")
        || m.contains("cdp flush")
        || m.contains("epipe")
        || m.contains("browser exited")
}

/// Apply one human input event to `session_id`'s live browser. Errors are
/// swallowed — the human can retry, and a dead pipe surfaces on the next tool
/// call.
fn dispatch_input(session_id: &str, b: &mut BrowserSession, ev: Input) {
    match ev {
        Input::Click { x, y } => {
            let _ = b.click(x, y);
        }
        Input::Key { key } => {
            if b.key(&key).is_err() && key.chars().count() == 1 {
                let _ = b.type_text(&key);
            }
        }
        Input::Scroll { dy } => {
            let _ = b.scroll(dy);
        }
        Input::Control { action } => state::set_paused(session_id, action == "pause"),
    }
}

/// Start `session_id`'s screencast pump if it isn't already running. Idempotent
/// per session: one process runs N pumps, one per session with a live browser,
/// each streaming ITS browser's frames + draining ITS input onto ITS bus.
pub fn ensure_pump_for(session_id: &str) {
    {
        let mut started = pumps_started().lock().expect("pumps registry poisoned");
        if !started.insert(session_id.to_string()) {
            return; // already running for this session
        }
    }
    let sid = session_id.to_string();
    std::thread::Builder::new()
        .name(format!("envoyage-pump-{session_id}"))
        .spawn(move || pump_loop(&sid))
        .ok();
}

/// Back-compat: the stdio/WS surface streams the DEFAULT_SESSION.
pub fn ensure_pump() {
    ensure_pump_for(state::DEFAULT_SESSION);
}

/// The pump body for ONE session: each tick, dispatch that session's queued
/// human input, arm its screencast, and broadcast the newest frame onto that
/// session's bus. Holds the browser mutex only briefly.
///
/// The DEFAULT_SESSION pump's `browser_frame` envelope wire shape is byte-
/// compatible with ImmorTerm's deployed WS panel; the per-session SSE surface
/// (`GET /sessions/:id/events`) subscribes to the SAME per-session bus, so each
/// remote session gets its own live view without any wire change.
fn pump_loop(session_id: &str) {
    let mut seq: u64 = 0;
    // Target ids we've already seen. Each tick we follow any target that appeared
    // since — a popup/new tab opened WITHOUT a tool call (async OAuth redirect,
    // "Sign in with Google" popup) — so the live view re-points to the newest
    // active target instead of streaming a stale opener. `attach_target` (inside
    // follow_new_target) resets screencast_on, so the ensure_screencast below
    // re-arms the screencast on the followed target. Switching to an EXISTING tab
    // (tabs_switch) never looks new, so a manual switch is never yanked back.
    let mut known: Vec<String> = Vec::new();
    let slot = state::browser_slot(session_id);
    loop {
        std::thread::sleep(PUMP_TICK);
        let inputs = state::drain_input_of(session_id);
        let frame = {
            let mut guard = match slot.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let Some(b) = guard.as_mut() else { continue };
            for ev in inputs {
                dispatch_input(session_id, b, ev);
            }
            // Auto-follow a genuinely-new target (popup/new tab) to both drive AND
            // stream it. Best-effort; refresh the baseline afterward.
            if !known.is_empty() {
                b.follow_new_target(&known);
            }
            known = b.page_target_ids();
            if b.ensure_screencast().is_err() {
                continue;
            }
            match b.poll_screencast_frame() {
                Ok(Some(png)) => {
                    let (title, url) = b.current_title_url();
                    Some((png, title, url))
                }
                _ => None,
            }
        };
        if let Some((png, title, url)) = frame {
            seq += 1;
            // Feed the GIF recorder (no-op unless recording); it takes the PNG
            // before broadcast moves it into the envelope.
            state::record_frame(&png);
            let f = Frame { png_base64: png, title, url, seq };
            if let Ok(env) = serde_json::to_string(&f.to_envelope()) {
                state::broadcast_envelope_to(session_id, env);
            }
        }
    }
}

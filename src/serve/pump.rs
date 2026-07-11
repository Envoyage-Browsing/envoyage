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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// True once the pump thread has been spawned (it idles cheaply when no browser
/// is open, so spawn it at most once).
static PUMP_STARTED: AtomicBool = AtomicBool::new(false);

/// ~15fps. Frame coalescing means a slower tick just drops intermediate frames.
const PUMP_TICK: Duration = Duration::from_millis(66);

/// Ensure the process-global browser exists (launch on first use), then run `f`.
/// Consults the ownership lock: refuse to launch over a live foreign owner's
/// shared profile. Auto-resets a dead-pipe session so the next call relaunches.
pub fn with_browser<T>(
    launch_url: Option<&str>,
    f: impl FnOnce(&mut BrowserSession) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = state::browser_slot()
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

/// Apply one human input event to the live browser. Errors are swallowed — the
/// human can retry, and a dead pipe surfaces on the next tool call.
fn dispatch_input(b: &mut BrowserSession, ev: Input) {
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
        Input::Control { action } => state::set_paused(action == "pause"),
    }
}

/// Start the screencast pump if it isn't already running. Idempotent.
pub fn ensure_pump() {
    if PUMP_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("envoyage-screencast-pump".into())
        .spawn(pump_loop)
        .ok();
}

/// The pump body: each tick, dispatch queued human input, arm the screencast,
/// and broadcast the newest frame. Holds the browser mutex only briefly.
fn pump_loop() {
    let mut seq: u64 = 0;
    // Target ids we've already seen. Each tick we follow any target that appeared
    // since — a popup/new tab opened WITHOUT a tool call (async OAuth redirect,
    // "Sign in with Google" popup) — so the live view re-points to the newest
    // active target instead of streaming a stale opener. `attach_target` (inside
    // follow_new_target) resets screencast_on, so the ensure_screencast below
    // re-arms the screencast on the followed target. Switching to an EXISTING tab
    // (tabs_switch) never looks new, so a manual switch is never yanked back.
    let mut known: Vec<String> = Vec::new();
    loop {
        std::thread::sleep(PUMP_TICK);
        let inputs = state::drain_input();
        let frame = {
            let mut guard = match state::browser_slot().lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let Some(b) = guard.as_mut() else { continue };
            for ev in inputs {
                dispatch_input(b, ev);
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
                state::broadcast_envelope(env);
            }
        }
    }
}

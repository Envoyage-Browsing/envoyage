//! # envoyage
//!
//! Drive a real browser from any AI agent, live.
//!
//! envoyage launches a headless Chromium over a private CDP pipe and exposes a
//! small, ref-based tool surface (navigate / read / find / click / type / key /
//! scroll / screenshot / tabs / upload / console / network / wait / human-handoff).
//! It streams a live screencast plus a **mascot-neutral** cursor/narration
//! protocol so a consumer can render its own animated cursor over the live view.
//! envoyage draws nothing itself — see [`protocol`] for the customization seam.
//!
//! ## Layout
//! - [`BrowserSession`] — the CDP driver (launch, navigate, refs, screencast,
//!   human-handoff detection, multi-tab).
//! - [`browser_lock`] — the one-browser-per-user broker lock.
//! - [`protocol`] — the vendor-neutral event types (`Frame`/`Cursor`/`Narration`
//!   /`HumanRequest`/`State`/`Input`) a consumer renders.

// The AX-snapshot APIs (`snapshot`/`find`/`tabs_list`) return tuple lists whose
// shape is load-bearing and kept verbatim from the extracted source; a newtype
// per return would only obscure them.
#![allow(clippy::type_complexity)]

pub mod browser;
pub mod browser_lock;
pub mod crawl;
pub mod protocol;
pub mod serve;
pub mod stealth;
pub mod transport;

pub use browser::{BrowserSession, HandoffReason};
pub use browser_lock::{BrowserLock, Decision};
pub use protocol::{Cursor, CursorAction, Frame, HumanRequest, Input, Narration, State};

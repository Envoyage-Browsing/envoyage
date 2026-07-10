//! The mascot-neutral event protocol — envoyage's customization seam.
//!
//! envoyage drives a real browser and streams what it sees + what it's about to
//! do. It draws NOTHING itself: no cursor sprite, no mascot, no balloon. A
//! consumer renders these events however it likes — ImmorTerm glides *Mort* the
//! axolotl to the cursor point and shows the narration in an axolotl balloon;
//! ringtail glides *Rocco* the ringtail. Bring your own mascot; envoyage gives you
//! the coordinates and the intent, you skin them.
//!
//! # Wire compatibility
//! The `#[serde(tag = "type")]` discriminants and field names below MATCH the
//! envelopes ImmorTerm's daemon already broadcasts to its browser panel
//! (`browser_frame` / `browser_state` / `browser_human_request` / `browser_cursor`
//! / `browser_narration`) and the input events its panel sends back
//! (`browser_input` kinds: click / key / scroll / control). This is deliberate:
//! ImmorTerm can drop envoyage in behind its existing renderer with no wire change.

use serde::{Deserialize, Serialize};

/// One screencast frame: a base64 PNG plus the tab's title/url and a monotonic
/// sequence. Consumers drop any frame whose `seq` is <= the last one shown.
///
/// Wire tag: `browser_frame`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    /// Base64-encoded PNG (consumers hardcode `data:image/png` — must be PNG).
    pub png_base64: String,
    pub title: String,
    pub url: String,
    /// Monotonic sequence; panels drop any frame `<=` the last shown.
    pub seq: u64,
}

impl Frame {
    /// The `{"type":"browser_frame", ...}` envelope ImmorTerm's panel expects.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "browser_frame",
            "png_base64": self.png_base64,
            "title": self.title,
            "url": self.url,
            "seq": self.seq,
        })
    }
}

/// What the agent is about to act on. Coordinates are PAGE CSS pixels — the
/// consumer glides its OWN mascot cursor there.
///
/// Wire tag: `browser_cursor` (with `action` as a lowercase string on the wire).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    pub x: f64,
    pub y: f64,
    pub action: CursorAction,
}

/// The kind of action a `Cursor` marks. Serializes lowercase to match the
/// panel's `action` string (`move` / `click` / `type` / `scroll`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorAction {
    Move,
    Click,
    Type,
    Scroll,
}

impl CursorAction {
    /// The lowercase wire string (`move`/`click`/`type`/`scroll`).
    pub fn as_str(self) -> &'static str {
        match self {
            CursorAction::Move => "move",
            CursorAction::Click => "click",
            CursorAction::Type => "type",
            CursorAction::Scroll => "scroll",
        }
    }
}

impl Cursor {
    /// The `{"type":"browser_cursor", ...}` envelope ImmorTerm's panel expects.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "browser_cursor",
            "x": self.x,
            "y": self.y,
            "action": self.action.as_str(),
        })
    }
}

/// A short intent string ("Clicking \"Sign in\"") for the consumer's balloon UI.
///
/// Wire tag: `browser_narration`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Narration {
    pub text: String,
}

impl Narration {
    /// The `{"type":"browser_narration", ...}` envelope.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({ "type": "browser_narration", "text": self.text })
    }
}

/// Handoff signal: envoyage hit something a human must solve (Cloudflare/CAPTCHA,
/// an OAuth/sign-in screen, a password or one-time-code field). The consumer
/// banners its UI and lets the human drive; passwords never reach the model.
///
/// Wire tag: `browser_human_request`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HumanRequest {
    pub reason: String,
    #[serde(default)]
    pub instructions: Option<String>,
}

impl HumanRequest {
    /// The `{"type":"browser_human_request", ...}` envelope.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "browser_human_request",
            "reason": self.reason,
            "instructions": self.instructions,
        })
    }
}

/// The AI-driving pause state. When `paused`, a human is driving; envoyage keeps
/// streaming frames to the human UI but returns text-only to the model.
///
/// Wire tag: `browser_state`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub paused: bool,
}

impl State {
    /// The `{"type":"browser_state", ...}` envelope.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({ "type": "browser_state", "paused": self.paused })
    }
}

/// Human input coming back FROM the consumer's UI (a click on the live view, a
/// key, a scroll, or a pause/continue toggle). Coordinates are PAGE CSS pixels
/// (the consumer un-letterboxes to page space before sending).
///
/// Wire shape matches ImmorTerm's `BrowserInputEvent`: `#[serde(tag = "kind")]`
/// with lowercase kinds (`click`/`key`/`scroll`/`control`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Input {
    /// Click at page CSS pixels.
    Click { x: f64, y: f64 },
    /// A single named key (Enter/Tab/Backspace/Escape/Arrow*) or printable char.
    Key { key: String },
    /// Vertical wheel scroll by `dy` CSS pixels (positive = down).
    Scroll { dy: f64 },
    /// `pause` / `continue` the AI's automation from the UI toggle.
    Control { action: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_envelope_tag_and_fields() {
        let f = Frame { png_base64: "QUJD".into(), title: "T".into(), url: "u".into(), seq: 3 };
        let v = f.to_envelope();
        assert_eq!(v["type"], "browser_frame");
        assert_eq!(v["png_base64"], "QUJD");
        assert_eq!(v["seq"], 3);
    }

    #[test]
    fn cursor_action_serializes_lowercase() {
        let c = Cursor { x: 1.0, y: 2.0, action: CursorAction::Click };
        assert_eq!(c.to_envelope()["action"], "click");
        // The struct itself also round-trips lowercase.
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(j["action"], "click");
    }

    #[test]
    fn input_deserializes_from_panel_wire_shape() {
        // Exactly what ImmorTerm's panel sends over WS.
        let click: Input = serde_json::from_str(r#"{"kind":"click","x":10,"y":20}"#).unwrap();
        assert_eq!(click, Input::Click { x: 10.0, y: 20.0 });
        let ctrl: Input = serde_json::from_str(r#"{"kind":"control","action":"pause"}"#).unwrap();
        assert_eq!(ctrl, Input::Control { action: "pause".into() });
        let key: Input = serde_json::from_str(r#"{"kind":"key","key":"Enter"}"#).unwrap();
        assert_eq!(key, Input::Key { key: "Enter".into() });
        let scroll: Input = serde_json::from_str(r#"{"kind":"scroll","dy":-5}"#).unwrap();
        assert_eq!(scroll, Input::Scroll { dy: -5.0 });
    }

    #[test]
    fn human_request_envelope() {
        let h = HumanRequest { reason: "Cloudflare".into(), instructions: Some("solve it".into()) };
        let v = h.to_envelope();
        assert_eq!(v["type"], "browser_human_request");
        assert_eq!(v["reason"], "Cloudflare");
        assert_eq!(v["instructions"], "solve it");
    }
}

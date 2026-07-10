//! The `browser_gif` recording buffer + annotated animated-GIF export.
//!
//! Mirrors claude-in-chrome's `gif_creator`: record a browser-automation session
//! (`start_recording` → buffer screencast frames → `stop_recording`) and
//! `export` an annotated animated GIF; `clear` drops the buffer. rudder is
//! **vendor-neutral**: there is NO baked-in logo. `showWatermark` defaults false;
//! when requested it renders a caller-supplied neutral text string (empty by
//! default) — the consumer brands it, never rudder.
//!
//! Overlays composited on export, each toggled by an `options` flag:
//! - `showClickIndicators` — an orange ring at the action's click point.
//! - `showActionLabels`    — a small text label (the narration) for the action.
//! - `showProgressBar`     — an orange bar along the bottom, scaled to progress.
//! - `showWatermark`       — neutral configurable text (empty ⇒ nothing drawn).
//! - `showDragPaths`       — TODO(rudder): rudder has no drag primitive yet.
//!
//! The frame source is the pump: it appends each broadcast PNG here while
//! recording, tagging it with whatever overlay hint the last MCP action left
//! pending (see `state::record_frame` / `state::record_overlay`).

use crate::browser_lock::rudder_home;
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame as GifFrame, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_hollow_circle_mut};
use imageproc::rect::Rect;
use std::path::PathBuf;
use std::time::Duration;

/// Hard cap on buffered frames (~40s at 15fps). When hit, oldest frames drop and
/// we LOG it — never a silent truncation.
const MAX_FRAMES: usize = 600;

/// The orange used for every overlay (matches claude-in-chrome's indicator hue).
const ORANGE: Rgba<u8> = Rgba([255, 138, 0, 255]);
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
const LABEL_BG: Rgba<u8> = Rgba([20, 20, 20, 220]);

/// Per-frame overlay hint captured alongside the PNG at record time.
#[derive(Clone, Default)]
struct Overlay {
    /// Click point in page CSS pixels (= screenshot pixels), if the action had one.
    cursor: Option<(f64, f64)>,
    /// Short narration label for the action ("Clicking \"Sign in\"").
    label: Option<String>,
}

/// One buffered frame: the base64 PNG the pump broadcast, plus its overlay hint.
struct Buffered {
    png_base64: String,
    overlay: Overlay,
}

/// The recording buffer. Frames accumulate while `recording`; `stop_recording`
/// flips it off but keeps frames for `export`; `clear` drops everything.
pub struct Recording {
    recording: bool,
    frames: Vec<Buffered>,
    /// Overlay hint to attach to the NEXT captured frame (set by MCP dispatch,
    /// consumed on the next pump frame so the indicator lands on the frame that
    /// actually shows the post-action page).
    pending: Overlay,
    /// Whether we've already logged a truncation for this recording (log once).
    truncated: bool,
    /// Monotonic export sequence for the default filename.
    export_seq: u64,
}

impl Recording {
    pub fn new() -> Self {
        Recording {
            recording: false,
            frames: Vec::new(),
            pending: Overlay::default(),
            truncated: false,
            export_seq: 0,
        }
    }

    // Used by tests to assert the start/stop transition; harmless as a public
    // accessor a consumer may also want.
    #[allow(dead_code)]
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// `start_recording`: begin buffering. Drops any previously buffered frames
    /// so a fresh recording starts clean.
    pub fn start(&mut self) {
        self.frames.clear();
        self.pending = Overlay::default();
        self.truncated = false;
        self.recording = true;
    }

    /// `stop_recording`: stop buffering but KEEP the frames for `export`.
    pub fn stop(&mut self) {
        self.recording = false;
    }

    /// `clear`: drop the buffer entirely.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.pending = Overlay::default();
        self.truncated = false;
        self.recording = false;
    }

    /// MCP hook: stamp the overlay hint the next captured frame should carry.
    /// A `None` cursor/label leaves that part of the pending hint unchanged so a
    /// narration-only or cursor-only action still enriches the frame.
    pub fn set_pending_overlay(&mut self, cursor: Option<(f64, f64)>, label: Option<String>) {
        if !self.recording {
            return;
        }
        if cursor.is_some() {
            self.pending.cursor = cursor;
        }
        if label.is_some() {
            self.pending.label = label;
        }
    }

    /// Pump hook: append a frame while recording, consuming the pending overlay.
    pub fn push_frame(&mut self, png_base64: &str) {
        if !self.recording {
            return;
        }
        if self.frames.len() >= MAX_FRAMES {
            // Drop the oldest, log once — never silently truncate.
            self.frames.remove(0);
            if !self.truncated {
                self.truncated = true;
                eprintln!(
                    "rudder: browser_gif buffer hit {MAX_FRAMES} frames — dropping oldest \
                     (the GIF will cover only the most recent ~{}s).",
                    MAX_FRAMES / 15
                );
            }
        }
        let overlay = std::mem::take(&mut self.pending);
        self.frames.push(Buffered { png_base64: png_base64.to_string(), overlay });
    }

    /// `export`: composite overlays and write an annotated animated GIF to
    /// `${RUDDER_HOME:-~/.rudder}/gif/<filename>`. Returns the written path.
    pub fn export(&mut self, filename: Option<&str>, opts: &ExportOptions) -> Result<PathBuf, String> {
        let dir = rudder_home().join("gif");
        self.export_to_dir(&dir, filename, opts)
    }

    /// Write the GIF into an explicit directory (env-free; used by tests and by
    /// `export`, which passes `${RUDDER_HOME}/gif`).
    fn export_to_dir(
        &mut self,
        dir: &std::path::Path,
        filename: Option<&str>,
        opts: &ExportOptions,
    ) -> Result<PathBuf, String> {
        if self.frames.is_empty() {
            return Err("no frames recorded — call browser_gif start_recording first, drive the browser, then export.".into());
        }
        self.export_seq += 1;
        let name = filename
            .map(sanitize_filename)
            .unwrap_or_else(|| format!("recording-{}.gif", self.export_seq));
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join(&name);

        let total = self.frames.len();
        let mut gif_frames: Vec<GifFrame> = Vec::with_capacity(total);
        for (i, buf) in self.frames.iter().enumerate() {
            let mut img = decode_png(&buf.png_base64)?;
            composite_overlays(&mut img, &buf.overlay, i, total, opts);
            let delay = Delay::from_saturating_duration(Duration::from_millis(frame_delay_ms(opts)));
            gif_frames.push(GifFrame::from_parts(img, 0, 0, delay));
        }

        let file = std::fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let writer = std::io::BufWriter::new(file);
        let mut encoder = GifEncoder::new_with_speed(writer, gif_speed(opts));
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|e| format!("gif repeat: {e}"))?;
        encoder
            .encode_frames(gif_frames)
            .map_err(|e| format!("gif encode: {e}"))?;
        drop(encoder);
        Ok(path)
    }
}

/// Export overlay toggles + quality, mirroring claude-in-chrome's `options`.
pub struct ExportOptions {
    pub show_click_indicators: bool,
    pub show_action_labels: bool,
    pub show_progress_bar: bool,
    pub show_drag_paths: bool,
    pub show_watermark: bool,
    /// Neutral watermark text; empty ⇒ nothing drawn even if `show_watermark`.
    pub watermark_text: String,
    /// 1–30, lower = better (claude-in-chrome semantics). Maps to GIF speed.
    pub quality: u8,
}

impl Default for ExportOptions {
    fn default() -> Self {
        // Bools default true EXCEPT show_watermark (vendor-neutral) and
        // show_drag_paths (unimplemented — off so we don't imply support).
        ExportOptions {
            show_click_indicators: true,
            show_action_labels: true,
            show_progress_bar: true,
            show_drag_paths: false,
            show_watermark: false,
            watermark_text: String::new(),
            quality: 10,
        }
    }
}

/// Map `quality` (1 best … 30 worst) to `image`'s GIF speed (1 best … 30 fast).
/// image's speed is 1..=30 where 30 is fastest/lowest-quality — same direction.
fn gif_speed(o: &ExportOptions) -> i32 {
    o.quality.clamp(1, 30) as i32
}

/// Per-frame delay in ms (~3/100s ≈ 33ms ≈ the 15fps capture cadence).
fn frame_delay_ms(_o: &ExportOptions) -> u64 {
    33
}

/// Decode a base64 PNG into an owned RGBA image.
fn decode_png(png_base64: &str) -> Result<RgbaImage, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png_base64)
        .map_err(|e| format!("frame base64 decode: {e}"))?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| format!("frame png decode: {e}"))?;
    Ok(img.to_rgba8())
}

/// Draw the requested overlays onto one frame in place.
fn composite_overlays(
    img: &mut RgbaImage,
    overlay: &Overlay,
    index: usize,
    total: usize,
    opts: &ExportOptions,
) {
    let (w, h) = (img.width(), img.height());

    if opts.show_click_indicators
        && let Some((cx, cy)) = overlay.cursor
    {
        draw_click_ring(img, cx as i32, cy as i32);
    }

    if opts.show_action_labels
        && let Some(label) = overlay.label.as_deref()
        && !label.is_empty()
    {
        draw_label(img, label);
    }

    if opts.show_progress_bar && total > 1 {
        let filled = (((index + 1) as f64 / total as f64) * w as f64).round() as i32;
        let bar_h = 6i32.min(h as i32);
        let y = h as i32 - bar_h;
        draw_filled_rect_mut(img, Rect::at(0, y).of_size(filled.max(0) as u32, bar_h as u32), ORANGE);
    }

    // showDragPaths: TODO(rudder) — no drag primitive in the tool surface yet.

    if opts.show_watermark && !opts.watermark_text.is_empty() {
        draw_watermark(img, &opts.watermark_text);
    }
}

/// An orange click ring (two concentric hollow circles for visibility over any
/// background).
fn draw_click_ring(img: &mut RgbaImage, cx: i32, cy: i32) {
    draw_hollow_circle_mut(img, (cx, cy), 16, ORANGE);
    draw_hollow_circle_mut(img, (cx, cy), 15, ORANGE);
    draw_hollow_circle_mut(img, (cx, cy), 10, WHITE);
}

/// Draw a narration label in a dark pill at the top-left of the frame.
fn draw_label(img: &mut RgbaImage, text: &str) {
    let text = clip_to_ascii(text, 48);
    if text.is_empty() {
        return;
    }
    let pad = 4i32;
    let scale = 2i32; // 5x7 glyph → 10x14 with 2px spacing
    let glyph_w = (FONT_W as i32 + 1) * scale;
    let text_w = glyph_w * text.chars().count() as i32;
    let box_w = text_w + pad * 2;
    let box_h = FONT_H as i32 * scale + pad * 2;
    let (x0, y0) = (6i32, 6i32);
    draw_filled_rect_mut(
        img,
        Rect::at(x0, y0).of_size(box_w.max(1) as u32, box_h as u32),
        LABEL_BG,
    );
    draw_text_5x7(img, x0 + pad, y0 + pad, &text, scale, WHITE);
}

/// Draw a neutral watermark at the bottom-right (consumer-supplied text).
fn draw_watermark(img: &mut RgbaImage, text: &str) {
    let text = clip_to_ascii(text, 40);
    if text.is_empty() {
        return;
    }
    let scale = 2i32;
    let glyph_w = (FONT_W as i32 + 1) * scale;
    let text_w = glyph_w * text.chars().count() as i32;
    let x = (img.width() as i32 - text_w - 8).max(0);
    let y = (img.height() as i32 - FONT_H as i32 * scale - 10).max(0);
    draw_text_5x7(img, x, y, &text, scale, WHITE);
}

/// Keep only printable ASCII (the 5x7 font's range), capping length.
fn clip_to_ascii(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .take(max)
        .collect()
}

fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    // Strip leading dots so a name can never become `..`-style traversal even in
    // isolation (slashes are already filtered, so this is belt-and-suspenders).
    let s = s.trim_start_matches('.').to_string();
    let mut s = if s.is_empty() { "recording".to_string() } else { s };
    if !s.to_ascii_lowercase().ends_with(".gif") {
        s.push_str(".gif");
    }
    s
}

// ─── Tiny embedded 5x7 ASCII bitmap font ────────────────────────────
//
// ponytail: a hand-rolled 5x7 bitmap font (printable ASCII only) instead of
// vendoring a 250KB+ TTF and pulling ab_glyph, for overlay labels that are short
// and crisper as pixels at this size. Upgrade path: swap draw_text_5x7 for
// imageproc's `text` feature + a bundled OFL font if rich typography is needed.

const FONT_W: usize = 5;
const FONT_H: usize = 7;

/// Look up a glyph's 7 row-bitmaps (5 low bits used, MSB = leftmost column).
fn glyph(c: char) -> [u8; FONT_H] {
    let idx = c as usize;
    if (0x20..0x80).contains(&idx) {
        FONT_5X7[idx - 0x20]
    } else {
        FONT_5X7['?' as usize - 0x20]
    }
}

/// Blit a string of 5x7 glyphs at (x,y), each pixel a `scale`×`scale` block.
fn draw_text_5x7(img: &mut RgbaImage, x: i32, y: i32, text: &str, scale: i32, color: Rgba<u8>) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let mut cx = x;
    for ch in text.chars() {
        let g = glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..FONT_W {
                if bits & (1 << (FONT_W - 1 - col)) != 0 {
                    let px = cx + col as i32 * scale;
                    let py = y + row as i32 * scale;
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (fx, fy) = (px + dx, py + dy);
                            if fx >= 0 && fy >= 0 && fx < iw && fy < ih {
                                img.put_pixel(fx as u32, fy as u32, color);
                            }
                        }
                    }
                }
            }
        }
        cx += (FONT_W as i32 + 1) * scale;
    }
}

/// Printable ASCII 0x20..0x7F, each glyph 7 rows of a 5-bit-wide bitmap.
/// Compact; readability over typographic finesse. Unknown chars fall back to '?'.
#[rustfmt::skip]
const FONT_5X7: [[u8; FONT_H]; 96] = [
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00], // ' '
    [0x04,0x04,0x04,0x04,0x00,0x00,0x04], // '!'
    [0x0A,0x0A,0x00,0x00,0x00,0x00,0x00], // '"'
    [0x0A,0x1F,0x0A,0x0A,0x1F,0x0A,0x00], // '#'
    [0x04,0x0F,0x14,0x0E,0x05,0x1E,0x04], // '$'
    [0x18,0x19,0x02,0x04,0x08,0x13,0x03], // '%'
    [0x08,0x14,0x08,0x15,0x12,0x0D,0x00], // '&'
    [0x04,0x04,0x00,0x00,0x00,0x00,0x00], // '\''
    [0x02,0x04,0x08,0x08,0x08,0x04,0x02], // '('
    [0x08,0x04,0x02,0x02,0x02,0x04,0x08], // ')'
    [0x00,0x04,0x15,0x0E,0x15,0x04,0x00], // '*'
    [0x00,0x04,0x04,0x1F,0x04,0x04,0x00], // '+'
    [0x00,0x00,0x00,0x00,0x00,0x04,0x08], // ','
    [0x00,0x00,0x00,0x1F,0x00,0x00,0x00], // '-'
    [0x00,0x00,0x00,0x00,0x00,0x0C,0x0C], // '.'
    [0x01,0x02,0x02,0x04,0x08,0x08,0x10], // '/'
    [0x0E,0x11,0x13,0x15,0x19,0x11,0x0E], // '0'
    [0x04,0x0C,0x04,0x04,0x04,0x04,0x0E], // '1'
    [0x0E,0x11,0x01,0x02,0x04,0x08,0x1F], // '2'
    [0x1F,0x02,0x04,0x02,0x01,0x11,0x0E], // '3'
    [0x02,0x06,0x0A,0x12,0x1F,0x02,0x02], // '4'
    [0x1F,0x10,0x1E,0x01,0x01,0x11,0x0E], // '5'
    [0x06,0x08,0x10,0x1E,0x11,0x11,0x0E], // '6'
    [0x1F,0x01,0x02,0x04,0x08,0x08,0x08], // '7'
    [0x0E,0x11,0x11,0x0E,0x11,0x11,0x0E], // '8'
    [0x0E,0x11,0x11,0x0F,0x01,0x02,0x0C], // '9'
    [0x00,0x0C,0x0C,0x00,0x0C,0x0C,0x00], // ':'
    [0x00,0x0C,0x0C,0x00,0x0C,0x04,0x08], // ';'
    [0x02,0x04,0x08,0x10,0x08,0x04,0x02], // '<'
    [0x00,0x00,0x1F,0x00,0x1F,0x00,0x00], // '='
    [0x08,0x04,0x02,0x01,0x02,0x04,0x08], // '>'
    [0x0E,0x11,0x01,0x02,0x04,0x00,0x04], // '?'
    [0x0E,0x11,0x17,0x15,0x17,0x10,0x0E], // '@'
    [0x0E,0x11,0x11,0x1F,0x11,0x11,0x11], // 'A'
    [0x1E,0x11,0x11,0x1E,0x11,0x11,0x1E], // 'B'
    [0x0E,0x11,0x10,0x10,0x10,0x11,0x0E], // 'C'
    [0x1C,0x12,0x11,0x11,0x11,0x12,0x1C], // 'D'
    [0x1F,0x10,0x10,0x1E,0x10,0x10,0x1F], // 'E'
    [0x1F,0x10,0x10,0x1E,0x10,0x10,0x10], // 'F'
    [0x0E,0x11,0x10,0x17,0x11,0x11,0x0F], // 'G'
    [0x11,0x11,0x11,0x1F,0x11,0x11,0x11], // 'H'
    [0x0E,0x04,0x04,0x04,0x04,0x04,0x0E], // 'I'
    [0x07,0x02,0x02,0x02,0x02,0x12,0x0C], // 'J'
    [0x11,0x12,0x14,0x18,0x14,0x12,0x11], // 'K'
    [0x10,0x10,0x10,0x10,0x10,0x10,0x1F], // 'L'
    [0x11,0x1B,0x15,0x15,0x11,0x11,0x11], // 'M'
    [0x11,0x11,0x19,0x15,0x13,0x11,0x11], // 'N'
    [0x0E,0x11,0x11,0x11,0x11,0x11,0x0E], // 'O'
    [0x1E,0x11,0x11,0x1E,0x10,0x10,0x10], // 'P'
    [0x0E,0x11,0x11,0x11,0x15,0x12,0x0D], // 'Q'
    [0x1E,0x11,0x11,0x1E,0x14,0x12,0x11], // 'R'
    [0x0F,0x10,0x10,0x0E,0x01,0x01,0x1E], // 'S'
    [0x1F,0x04,0x04,0x04,0x04,0x04,0x04], // 'T'
    [0x11,0x11,0x11,0x11,0x11,0x11,0x0E], // 'U'
    [0x11,0x11,0x11,0x11,0x11,0x0A,0x04], // 'V'
    [0x11,0x11,0x11,0x15,0x15,0x1B,0x11], // 'W'
    [0x11,0x11,0x0A,0x04,0x0A,0x11,0x11], // 'X'
    [0x11,0x11,0x0A,0x04,0x04,0x04,0x04], // 'Y'
    [0x1F,0x01,0x02,0x04,0x08,0x10,0x1F], // 'Z'
    [0x0E,0x08,0x08,0x08,0x08,0x08,0x0E], // '['
    [0x10,0x08,0x08,0x04,0x02,0x02,0x01], // '\\'
    [0x0E,0x02,0x02,0x02,0x02,0x02,0x0E], // ']'
    [0x04,0x0A,0x11,0x00,0x00,0x00,0x00], // '^'
    [0x00,0x00,0x00,0x00,0x00,0x00,0x1F], // '_'
    [0x08,0x04,0x00,0x00,0x00,0x00,0x00], // '`'
    [0x00,0x00,0x0E,0x01,0x0F,0x11,0x0F], // 'a'
    [0x10,0x10,0x16,0x19,0x11,0x11,0x1E], // 'b'
    [0x00,0x00,0x0E,0x10,0x10,0x11,0x0E], // 'c'
    [0x01,0x01,0x0D,0x13,0x11,0x11,0x0F], // 'd'
    [0x00,0x00,0x0E,0x11,0x1F,0x10,0x0E], // 'e'
    [0x06,0x09,0x08,0x1C,0x08,0x08,0x08], // 'f'
    [0x00,0x00,0x0F,0x11,0x0F,0x01,0x0E], // 'g'
    [0x10,0x10,0x16,0x19,0x11,0x11,0x11], // 'h'
    [0x04,0x00,0x0C,0x04,0x04,0x04,0x0E], // 'i'
    [0x02,0x00,0x06,0x02,0x02,0x12,0x0C], // 'j'
    [0x10,0x10,0x12,0x14,0x18,0x14,0x12], // 'k'
    [0x0C,0x04,0x04,0x04,0x04,0x04,0x0E], // 'l'
    [0x00,0x00,0x1A,0x15,0x15,0x11,0x11], // 'm'
    [0x00,0x00,0x16,0x19,0x11,0x11,0x11], // 'n'
    [0x00,0x00,0x0E,0x11,0x11,0x11,0x0E], // 'o'
    [0x00,0x00,0x1E,0x11,0x1E,0x10,0x10], // 'p'
    [0x00,0x00,0x0D,0x13,0x0F,0x01,0x01], // 'q'
    [0x00,0x00,0x16,0x19,0x10,0x10,0x10], // 'r'
    [0x00,0x00,0x0F,0x10,0x0E,0x01,0x1E], // 's'
    [0x08,0x08,0x1C,0x08,0x08,0x09,0x06], // 't'
    [0x00,0x00,0x11,0x11,0x11,0x13,0x0D], // 'u'
    [0x00,0x00,0x11,0x11,0x11,0x0A,0x04], // 'v'
    [0x00,0x00,0x11,0x11,0x15,0x15,0x0A], // 'w'
    [0x00,0x00,0x11,0x0A,0x04,0x0A,0x11], // 'x'
    [0x00,0x00,0x11,0x11,0x0F,0x01,0x0E], // 'y'
    [0x00,0x00,0x1F,0x02,0x04,0x08,0x1F], // 'z'
    [0x02,0x04,0x04,0x08,0x04,0x04,0x02], // '{'
    [0x04,0x04,0x04,0x04,0x04,0x04,0x04], // '|'
    [0x08,0x04,0x04,0x02,0x04,0x04,0x08], // '}'
    [0x00,0x00,0x08,0x15,0x02,0x00,0x00], // '~'
    [0x1F,0x1F,0x1F,0x1F,0x1F,0x1F,0x1F], // 0x7F (DEL) → solid block
];

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    /// Encode a solid-color synthetic frame to base64 PNG (what the pump buffers).
    fn synthetic_png(w: u32, h: u32, color: [u8; 4]) -> String {
        use base64::Engine;
        let img = RgbaImage::from_pixel(w, h, Rgba(color));
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(&png)
    }

    #[test]
    fn state_transitions_start_buffer_stop_export_clear() {
        let mut rec = Recording::new();
        // Not recording: pushes are ignored.
        rec.push_frame(&synthetic_png(8, 8, [0, 0, 0, 255]));
        assert_eq!(rec.frame_count(), 0);

        rec.start();
        assert!(rec.is_recording());
        for _ in 0..3 {
            rec.push_frame(&synthetic_png(8, 8, [10, 20, 30, 255]));
        }
        assert_eq!(rec.frame_count(), 3);

        rec.stop();
        assert!(!rec.is_recording());
        // Stopped keeps frames; further pushes ignored.
        rec.push_frame(&synthetic_png(8, 8, [0, 0, 0, 255]));
        assert_eq!(rec.frame_count(), 3);

        rec.clear();
        assert_eq!(rec.frame_count(), 0);
    }

    #[test]
    fn export_empty_errors() {
        let mut rec = Recording::new();
        assert!(rec.export(None, &ExportOptions::default()).is_err());
    }

    #[test]
    fn export_writes_valid_animated_gif() {
        // Env-free: write into an explicit temp dir (avoids racing the global
        // RUDDER_HOME with the browser_lock tests when run in parallel).
        let dir = std::env::temp_dir().join(format!(
            "rudder-gif-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let mut rec = Recording::new();
        rec.start();
        rec.set_pending_overlay(Some((30.0, 30.0)), Some("Clicking \"Go\"".into()));
        rec.push_frame(&synthetic_png(64, 48, [200, 50, 50, 255]));
        rec.push_frame(&synthetic_png(64, 48, [50, 200, 50, 255]));
        rec.push_frame(&synthetic_png(64, 48, [50, 50, 200, 255]));

        let opts = ExportOptions {
            show_watermark: true,
            watermark_text: "demo".into(),
            ..ExportOptions::default()
        };
        let path = rec.export_to_dir(&dir, Some("t.gif"), &opts).expect("export");
        assert!(path.starts_with(&dir), "GIF written under the given dir");

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..6], b"GIF89a", "GIF89a magic bytes");

        // Decodes to >1 frame.
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).unwrap();
        use image::AnimationDecoder;
        let n = decoder.into_frames().collect_frames().unwrap().len();
        assert!(n > 1, "animated GIF has >1 frame, got {n}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn buffer_truncates_at_cap_without_panicking() {
        let mut rec = Recording::new();
        rec.start();
        for _ in 0..(MAX_FRAMES + 5) {
            rec.push_frame(&synthetic_png(4, 4, [0, 0, 0, 255]));
        }
        assert_eq!(rec.frame_count(), MAX_FRAMES);
        assert!(rec.truncated);
    }

    /// Live smoke: launch a REAL headless browser, record ~1s of its screencast
    /// into a `Recording`, export, and assert a valid multi-frame GIF. Ignored by
    /// default — needs a Chromium-engine browser. Run with:
    ///   cargo test -p rudder -- --ignored gif_live_smoke
    #[test]
    #[ignore = "needs a real browser; run explicitly"]
    fn gif_live_smoke() {
        use crate::BrowserSession;
        use std::time::Instant;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut b = BrowserSession::launch(&rt, "about:blank").expect("headless browser");
        b.ensure_screencast().expect("startScreencast");

        let mut rec = Recording::new();
        rec.start();
        // Record ~1s, forcing a repaint each poll so a static page still emits
        // fresh frames (the scheme allowlist forbids navigating to a data: URL).
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let _ = b.eval_for_test("document.body.style.background = (Date.now()%2)?'#124':'#125'; true");
            if let Ok(Some(png)) = b.poll_screencast_frame() {
                rec.push_frame(&png);
            }
            std::thread::sleep(Duration::from_millis(66));
        }
        b.close();
        rec.stop();
        assert!(rec.frame_count() > 1, "expected >1 recorded frame, got {}", rec.frame_count());

        let dir = std::env::temp_dir().join(format!("rudder-gif-live-{}", std::process::id()));
        let path = rec.export_to_dir(&dir, Some("live.gif"), &ExportOptions::default()).expect("export");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..6], b"GIF89a", "GIF89a magic bytes");
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).unwrap();
        assert!(decoder.into_frames().collect_frames().unwrap().len() > 1, "multi-frame GIF");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filename_is_sanitized_and_gif_suffixed() {
        assert_eq!(sanitize_filename("../etc/passwd"), "etcpasswd.gif"); // slashes + leading dots stripped
        assert_eq!(sanitize_filename("my clip"), "myclip.gif");
        assert_eq!(sanitize_filename("keep.gif"), "keep.gif");
        assert_eq!(sanitize_filename(""), "recording.gif");
    }
}

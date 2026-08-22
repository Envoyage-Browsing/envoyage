//! Shared agent-facing browser contract used by Envoyage's native MCP server
//! and embedded consumers such as ImmorTerm.

use crate::browser::SemanticState;
use base64::Engine as _;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::GenericImageView;
use serde_json::{Value, json};

pub const ACCESSIBILITY_FIRST_INSTRUCTIONS: &str = "For repeatable product verification, use the repository's Playwright tests first: assertions and failure-only traces are more reliable and context-efficient than screenshots. Use Envoyage for compact exploratory control: browser_read_page/browser_find, then act by stable ref. Open, click, form input, key/focus, scroll, reload, read, and tab operations are accessibility-first and return zero image blocks plus compact semantic diffs. Visual proof is explicit: prefer a ref crop, then changed/action or cursor crop; full_viewport=true is required for a full viewport. Captures return bounded file receipts by default; inline=true is required to put pixels in context, and per-session count/pixel/byte budgets apply. Puppeteer is an acceptable fallback when already used by the project. Use human handoff for login, secrets, permissions, or user-browser state.";

pub const SCREENSHOT_DESCRIPTION: &str = "Explicit visual proof only. Prefer an element crop by ref; otherwise use the most recent changed/action region, then a bounded cursor crop. A full viewport requires full_viewport=true. Every capture is cropped/downscaled/compressed and returns a file receipt (path, dimensions, format, bytes) by default; image content enters context only with inline=true. Per-session image-count, pixel, and byte budgets are enforced. For routine navigation use accessibility refs and semantic action diffs; for repeatable product verification prefer Playwright tests.";

pub fn screenshot_properties() -> Value {
    json!({
        "ref": { "type": "string", "description": "Preferred: crop the element identified by a stable ref from read_page/find, with safe padding." },
        "full_viewport": { "type": "boolean", "default": false, "description": "Explicitly request the full visible viewport. Never implied." },
        "inline": { "type": "boolean", "default": false, "description": "Also return one bounded image content block. Default is a file receipt only." },
        "format": { "type": "string", "enum": ["jpeg", "png"], "default": "jpeg" },
        "max_width": { "type": "integer", "minimum": 64, "maximum": 1600, "default": 768 },
        "max_height": { "type": "integer", "minimum": 64, "maximum": 1600, "default": 768 },
        "quality": { "type": "integer", "minimum": 20, "maximum": 90, "default": 55, "description": "JPEG quality; cropping and dimensions are the primary vision-cost controls." }
    })
}

#[derive(Clone, Debug)]
pub struct VisualRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub source: &'static str,
}

#[derive(Debug)]
pub struct EncodedVisual {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub format: &'static str,
}

pub fn encode_visual(
    png_base64: &str,
    crop: Option<&VisualRegion>,
    max_width: u32,
    max_height: u32,
    quality: u8,
    format: &str,
) -> Result<EncodedVisual, String> {
    let source = base64::engine::general_purpose::STANDARD
        .decode(png_base64)
        .map_err(|error| format!("decode screenshot: {error}"))?;
    let image = image::load_from_memory(&source).map_err(|error| format!("decode screenshot pixels: {error}"))?;
    let source_width = image.width().max(1);
    let source_height = image.height().max(1);
    let cropped = if let Some(region) = crop {
        let padding = 24.0;
        let x = (region.x - padding).max(0.0).min(source_width.saturating_sub(1) as f64) as u32;
        let y = (region.y - padding).max(0.0).min(source_height.saturating_sub(1) as f64) as u32;
        let width = (region.width + padding * 2.0).max(1.0) as u32;
        let height = (region.height + padding * 2.0).max(1.0) as u32;
        image.crop_imm(x, y, width.min(source_width - x), height.min(source_height - y))
    } else {
        image
    };
    let (width, height) = cropped.dimensions();
    let scale = (max_width as f64 / width.max(1) as f64)
        .min(max_height as f64 / height.max(1) as f64)
        .min(1.0);
    let encoded_width = ((width as f64 * scale).round() as u32).max(1);
    let encoded_height = ((height as f64 * scale).round() as u32).max(1);
    let resized = cropped.resize(encoded_width, encoded_height, FilterType::Triangle);
    let mut bytes = Vec::new();
    let encoded_format = if format == "png" {
        let mut cursor = std::io::Cursor::new(Vec::new());
        resized.write_to(&mut cursor, image::ImageFormat::Png).map_err(|error| format!("encode PNG screenshot: {error}"))?;
        bytes = cursor.into_inner();
        "png"
    } else {
        JpegEncoder::new_with_quality(&mut bytes, quality).encode_image(&resized.to_rgb8()).map_err(|error| format!("encode JPEG screenshot: {error}"))?;
        "jpeg"
    };
    Ok(EncodedVisual { bytes, width: encoded_width, height: encoded_height, source_width, source_height, format: encoded_format })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ImageUsage {
    pub count: u64,
    pub pixels: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy)]
pub struct ImageLimits {
    pub count: u64,
    pub pixels: u64,
    pub bytes: u64,
}

impl ImageLimits {
    pub fn from_env() -> Self {
        fn limit(name: &str, default: u64) -> u64 {
            std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
        }
        Self {
            count: limit("ENVOYAGE_IMAGE_BUDGET_COUNT", 12),
            pixels: limit("ENVOYAGE_IMAGE_BUDGET_PIXELS", 8_000_000),
            bytes: limit("ENVOYAGE_IMAGE_BUDGET_BYTES", 4 * 1024 * 1024),
        }
    }
}

pub fn budget_warning(usage: ImageUsage, visual: &EncodedVisual, limits: ImageLimits) -> Option<String> {
    let next_count = usage.count + 1;
    let next_pixels = usage.pixels.saturating_add(visual.width as u64 * visual.height as u64);
    let next_bytes = usage.bytes.saturating_add(visual.bytes.len() as u64);
    if next_count > limits.count || next_pixels > limits.pixels || next_bytes > limits.bytes {
        return Some(format!("⚠️ Envoyage image budget exceeded; no image was returned or written. Session totals would be {next_count}/{} images, {next_pixels}/{} pixels, {next_bytes}/{} bytes. Continue with accessibility data or close/reopen the browser session to reset its visual-proof budget.", limits.count, limits.pixels, limits.bytes));
    }
    None
}

pub fn compact_text_change(before: &str, after: &str) -> Value {
    if before == after { return Value::Null; }
    let before_chars: Vec<char> = before.chars().collect();
    let after_chars: Vec<char> = after.chars().collect();
    let mut prefix = 0usize;
    while prefix < before_chars.len() && prefix < after_chars.len() && before_chars[prefix] == after_chars[prefix] { prefix += 1; }
    let mut suffix = 0usize;
    while suffix < before_chars.len().saturating_sub(prefix)
        && suffix < after_chars.len().saturating_sub(prefix)
        && before_chars[before_chars.len() - 1 - suffix] == after_chars[after_chars.len() - 1 - suffix] { suffix += 1; }
    let removed: String = before_chars[prefix..before_chars.len().saturating_sub(suffix)].iter().take(500).collect();
    let added: String = after_chars[prefix..after_chars.len().saturating_sub(suffix)].iter().take(500).collect();
    json!({ "removed": removed, "added": added })
}

pub fn semantic_diff(
    operation: &str,
    target_role: &str,
    target_name: &str,
    before: &SemanticState,
    after: &SemanticState,
    console_errors: Vec<String>,
    failed_requests: Vec<String>,
) -> String {
    json!({
        "operation": operation,
        "target": { "role": target_role, "name": target_name },
        "focus": { "before": { "role": before.focus_role, "name": before.focus_name }, "after": { "role": after.focus_role, "name": after.focus_name }, "changed": before.focus_role != after.focus_role || before.focus_name != after.focus_name },
        "url": { "before": before.url, "after": after.url, "changed": before.url != after.url },
        "title": { "before": before.title, "after": after.title, "changed": before.title != after.title },
        "changed_visible_text": compact_text_change(&before.visible_text, &after.visible_text),
        "console_errors": console_errors,
        "failed_requests": failed_requests,
        "image_blocks": 0,
    }).to_string()
}

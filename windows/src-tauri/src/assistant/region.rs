// Ask about a screen region (`ask-region`, P2).
//
// The user starts it from the chat. Each monitor gets an overlay window of
// its own (region.html), dimmed, where they drag a rectangle; Escape or a
// right click cancels. Nothing is captured until they let go: then the
// overlays close, Coucou's own windows are excluded from capture, and only
// that rectangle of that monitor is copied, once. There is no continuous
// capture and nothing is written to disk yet.
//
// The island then shows the picture with where it will go (the chat, with the
// next question, to the provider in the picker). The user can black out parts
// of it first. "Ask about it" hands Rust's grant back; Rust checks it against
// the `ask-region` action it kept (policy says `ask`), applies the masks and
// saves the PNG into Coucou's inbox, where a dropped file would land. From
// there the chat sends it with the first question, as it does a dropped image,
// and the inbox sweep deletes it after a week.
//
// Coordinates are physical pixels of one monitor, from its top-left corner.
// One overlay per monitor keeps mixed DPI simple: each page lives on a single
// monitor and multiplies its CSS pixels by its own devicePixelRatio.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::contracts::*;
use super::policy::{self, Grant};

/// How long the overlays wait for a pick, and the picture for "Ask about it".
pub const SESSION_TTL: Duration = Duration::from_secs(5 * 60);
/// How long the `ask-region` action stays valid once the picture is shown.
pub const PROPOSAL_TTL_MS: u64 = 120_000;
/// Smaller than this (physical pixels) is a click, not a region.
pub const MIN_SIDE: u32 = 8;
/// The preview on the island is at most this wide or tall.
pub const PREVIEW_MAX: u32 = 720;
/// At most this many masks on one picture.
pub const MAX_MASKS: usize = 64;

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A monitor as Windows places it on the virtual screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    /// The device name (`\\.\DISPLAY1`).
    pub name: String,
    /// Its rectangle on the virtual screen.
    pub bounds: Rect,
}

/// RGBA pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The rectangle the user dragged, clamped to its monitor. None when it is
/// too small to be a region (a click), or entirely off the monitor.
pub fn clamp(selection: Rect, monitor: (u32, u32)) -> Option<Rect> {
    let (mw, mh) = (monitor.0 as i64, monitor.1 as i64);
    let x0 = (selection.x as i64).clamp(0, mw);
    let y0 = (selection.y as i64).clamp(0, mh);
    let x1 = (selection.x as i64 + selection.width as i64).clamp(0, mw);
    let y1 = (selection.y as i64 + selection.height as i64).clamp(0, mh);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    (w >= MIN_SIDE && h >= MIN_SIDE).then_some(Rect { x: x0 as i32, y: y0 as i32, width: w, height: h })
}

/// Blacks out each mask (picture pixels), clipped to the picture.
pub fn apply_masks(image: &mut Image, masks: &[Rect]) {
    for mask in masks.iter().take(MAX_MASKS) {
        let Some(m) = clamp_any(*mask, (image.width, image.height)) else { continue };
        for y in m.y as u32..m.y as u32 + m.height {
            let row = (y * image.width) as usize * 4;
            for x in m.x as u32..m.x as u32 + m.width {
                let i = row + x as usize * 4;
                image.rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
}

/// Like `clamp`, without the minimum size: a mask may be thin.
fn clamp_any(rect: Rect, size: (u32, u32)) -> Option<Rect> {
    let x0 = (rect.x as i64).clamp(0, size.0 as i64);
    let y0 = (rect.y as i64).clamp(0, size.1 as i64);
    let x1 = (rect.x as i64 + rect.width as i64).clamp(0, size.0 as i64);
    let y1 = (rect.y as i64 + rect.height as i64).clamp(0, size.1 as i64);
    (x1 > x0 && y1 > y0).then_some(Rect { x: x0 as i32, y: y0 as i32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
}

/// A copy at most `max` pixels on its longer side (averaging each block),
/// or the picture itself when it is already that small.
pub fn shrink(image: &Image, max: u32) -> Image {
    let longer = image.width.max(image.height);
    if longer <= max {
        return image.clone();
    }
    let width = (image.width as u64 * max as u64 / longer as u64).max(1) as u32;
    let height = (image.height as u64 * max as u64 / longer as u64).max(1) as u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let (sy0, sy1) = span(y, height, image.height);
        for x in 0..width {
            let (sx0, sx1) = span(x, width, image.width);
            let mut sum = [0u64; 4];
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = ((sy * image.width + sx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += image.rgba[i + c] as u64;
                    }
                }
            }
            let n = ((sy1 - sy0) * (sx1 - sx0)).max(1) as u64;
            rgba.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    Image { width, height, rgba }
}

/// The source pixels [start, end) that destination pixel `i` of `n` covers.
fn span(i: u32, n: u32, source: u32) -> (u32, u32) {
    let start = (i as u64 * source as u64 / n as u64) as u32;
    let end = (((i + 1) as u64 * source as u64 / n as u64) as u32).max(start + 1).min(source);
    (start, end)
}

pub fn png(image: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&image.rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

pub fn data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", crate::claude::base64_for(png))
}

/// The file's name in the inbox: "Screen region 2026-10-14 09.41.07.png".
pub fn file_name(t: &crate::platform::LocalTime) -> String {
    format!(
        "Screen region {:04}-{:02}-{:02} {:02}.{:02}.{:02}.png",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

// ── One pick at a time ───────────────────────────────────────────────────────

struct Session {
    id: String,
    monitors: Vec<Monitor>,
    started: Instant,
    /// Once the user let go: the action Rust keeps and the picture.
    picked: Option<(AssistantAction, Image)>,
}

/// The current pick, if any. One per app.
#[derive(Default)]
pub struct Regions(Mutex<Option<Session>>);

/// What the island shows once a region is picked.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub session_id: String,
    /// The picture's size in physical pixels: masks are given in these.
    pub width: u32,
    pub height: u32,
    /// A PNG data URL, at most PREVIEW_MAX on its longer side.
    pub image: String,
    pub grant: Grant,
}

impl Regions {
    /// A new pick over these monitors; any previous one is dropped.
    pub fn start(&self, monitors: Vec<Monitor>) -> String {
        let id = policy::new_id("reg");
        *self.0.lock().unwrap() = Some(Session { id: id.clone(), monitors, started: Instant::now(), picked: None });
        id
    }

    /// The session still waiting for a pick, if any.
    pub fn waiting(&self) -> Option<String> {
        let guard = self.0.lock().unwrap();
        guard.as_ref().filter(|s| s.picked.is_none() && s.started.elapsed() < SESSION_TTL).map(|s| s.id.clone())
    }

    /// The monitor an overlay is on, for a session still waiting for a pick.
    pub fn monitor(&self, session_id: &str, index: usize) -> Option<Monitor> {
        let guard = self.0.lock().unwrap();
        let s = guard.as_ref().filter(|s| s.id == session_id && s.picked.is_none() && s.started.elapsed() < SESSION_TTL)?;
        s.monitors.get(index).cloned()
    }

    /// The picture of `region` on `monitor`, captured: Rust keeps the
    /// `ask-region` action and returns what the island shows, with its grant.
    pub fn picked(&self, session_id: &str, monitor: &Monitor, region: Rect, image: Image) -> Result<Preview, ErrorCode> {
        let mut guard = self.0.lock().unwrap();
        let Some(session) = guard.as_mut().filter(|s| s.id == session_id && s.picked.is_none()) else {
            return Err(ErrorCode::Expired);
        };
        let now = policy::now_ms();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: policy::new_id("act"),
            kind: ActionKind::AskRegion,
            args: serde_json::json!({
                "monitor": monitor.name, "x": region.x, "y": region.y,
                "width": region.width, "height": region.height,
            }),
            intent_id: session_id.to_string(),
            expires_at_ms: now + PROPOSAL_TTL_MS,
        };
        let grant = policy::grant_for(&action, now)
            .ok_or_else(|| policy::decide(&action, now).error.unwrap_or(ErrorCode::InvalidArgs))?;
        let preview = png(&shrink(&image, PREVIEW_MAX)).map_err(|_| ErrorCode::Failed)?;
        let (width, height) = (image.width, image.height);
        session.picked = Some((action, image));
        Ok(Preview { session_id: session_id.to_string(), width, height, image: data_url(&preview), grant })
    }

    /// "Ask about it": the grant must be the one Rust gave for the action it
    /// kept, still in time (policy::check_grant). The picture is taken out
    /// whatever the answer: it is used once.
    pub fn confirm(&self, session_id: &str, grant: &Grant) -> Result<(String, Image), ErrorCode> {
        let session = {
            let mut guard = self.0.lock().unwrap();
            if guard.as_ref().is_some_and(|s| s.id != session_id) {
                return Err(ErrorCode::GrantMismatch);
            }
            guard.take()
        };
        let Some((action, image)) = session.and_then(|s| s.picked) else { return Err(ErrorCode::Expired) };
        if action.id != grant.action_id {
            return Err(ErrorCode::GrantMismatch);
        }
        policy::check_grant(grant, &action, policy::now_ms())?;
        Ok((action.id, image))
    }

    /// Cancel, Escape, or the picture left unused: everything is dropped.
    pub fn cancel(&self, session_id: &str) -> bool {
        let mut guard = self.0.lock().unwrap();
        if guard.as_ref().is_some_and(|s| s.id == session_id) {
            *guard = None;
            return true;
        }
        false
    }
}

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod commands;

#[cfg(test)]
mod tests;

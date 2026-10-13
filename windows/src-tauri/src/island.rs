// Island window: placement on the chosen display, the two window sizes
// (full panel / invisible wake strip), click-through and the cursor poll.
//
// There is no notch on a PC, so the island is a black shape drawn at the top
// centre of the main display inside a borderless, transparent, always-on-top
// window that never takes focus. The user may drag it anywhere on a display
// (`island_spot` in the settings); dragged back near the top centre, it docks
// there again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::platform::{self, cursor_physical, left_button_down};
use crate::settings::IslandSpot;

/// Logical size of the full window — the largest island view, like the macOS panel.
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 320.0;
/// Logical size of the invisible strip that wakes the island when it is hidden.
pub const STRIP_W: f64 = 240.0;
pub const STRIP_H: f64 = 6.0;

pub const WINDOW_LABEL: &str = "island";

/// Margin around the island that still counts as "on the island", in logical px.
/// Wider than the macOS 6 pt because a click must never be swallowed.
const HIT_MARGIN: f64 = 14.0;

#[derive(Serialize, Clone)]
pub struct CursorPayload {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Clone)]
pub struct ScreenInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// The island shape in window-logical coordinates, pushed by the front end.
/// The poll thread owns the click-through decision so it lands in the same 16 ms
/// tick as the cursor read — an IPC round trip here loses clicks.
#[derive(Clone, Copy, Default)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Wakes / parks the cursor poll thread so a hidden island costs literally nothing.
pub struct PollGate {
    active: Mutex<bool>,
    cv: Condvar,
    pub collapsed: AtomicBool,
    pub rect: Mutex<IslandRect>,
    /// Mirrors the window flag so we only call into the OS when it changes.
    ignoring: AtomicBool,
    /// The user is dragging the island. Windows finishes from its button poll;
    /// Linux finishes from the captured page gesture, never the cursor poll.
    pub moving: AtomicBool,
    #[cfg(target_os = "linux")]
    page_move: Mutex<Option<PageMove>>,
}

impl PollGate {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            collapsed: AtomicBool::new(true),
            rect: Mutex::new(IslandRect::default()),
            ignoring: AtomicBool::new(false),
            moving: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            page_move: Mutex::new(None),
        }
    }

    pub fn set_rect(&self, rect: IslandRect) {
        *self.rect.lock().unwrap() = rect;
    }

    /// Forces the next poll tick to re-apply the flag (after a window resize).
    pub fn forget_ignore_state(&self) {
        self.ignoring.store(false, Ordering::Relaxed);
    }

    pub fn set_active(&self, on: bool) {
        let mut guard = self.active.lock().unwrap();
        *guard = on;
        self.cv.notify_all();
    }

    pub(crate) fn wait_until_active(&self) {
        let mut guard = self.active.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

fn monitor_contains(m: &Monitor, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && x < (p.x + s.width as i32) as f64
        && y >= p.y as f64
        && y < (p.y + s.height as i32) as f64
}

/// A display's logical origin, the key `at:<x>,<y>` preferences are matched on.
/// Names are no good for that: two monitors of the same model share one.
fn logical_origin(m: &Monitor) -> (i32, i32) {
    let scale = m.scale_factor();
    let p = m.position();
    ((p.x as f64 / scale).round() as i32, (p.y as f64 / scale).round() as i32)
}

/// One entry of the "Island lives on" list in Settings.
#[derive(Serialize, Clone)]
pub struct MonitorChoice {
    pub key: String,
    pub label: String,
}

pub fn monitor_choices(app: &AppHandle) -> Vec<MonitorChoice> {
    let Ok(monitors) = app.available_monitors() else { return Vec::new() };
    monitors
        .iter()
        .map(|m| {
            let d = describe(m);
            MonitorChoice {
                key: d.key(),
                label: crate::i18n::tf(
                    "{name} — {width}×{height} at {x},{y}",
                    &[
                        ("name", &d.name),
                        ("width", &d.w.to_string()),
                        ("height", &d.h.to_string()),
                        ("x", &d.x.to_string()),
                        ("y", &d.y.to_string()),
                    ],
                ),
            }
        })
        .collect()
}

/// What a display is remembered by: its logical origin, plus its name and
/// logical size, so it is still found after the layout is rearranged or the
/// resolution changes (the Mac keeps the display's UUID for the same reason;
/// Tauri has no stable ID).
#[derive(Debug, Clone, PartialEq)]
struct DisplayId {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl DisplayId {
    /// `at:<x>,<y>` stays first, so a preference saved before still matches.
    fn key(&self) -> String {
        format!("at:{},{}|{}|{}x{}", self.x, self.y, self.name.replace('|', " "), self.w, self.h)
    }
}

fn describe(m: &Monitor) -> DisplayId {
    let (x, y) = logical_origin(m);
    let scale = m.scale_factor();
    let s = m.size();
    DisplayId {
        name: m.name().cloned().unwrap_or_else(|| "Display".into()),
        x,
        y,
        w: (s.width as f64 / scale).round() as i32,
        h: (s.height as f64 / scale).round() as i32,
    }
}

/// Which display a saved `at:` preference points at, best match first: same
/// place and name; the same name and size elsewhere (layout rearranged); the
/// same name alone when unique (resolution changed); the same place. None
/// means unplugged, and the caller falls back to the primary display.
fn pick_display(pref: &str, displays: &[DisplayId]) -> Option<usize> {
    let rest = pref.strip_prefix("at:")?;
    let mut parts = rest.split('|');
    let (x, y) = parts.next()?.split_once(',')?;
    let (x, y) = (x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?);
    let name = parts.next();
    let size = parts.next().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse::<i32>().ok()?, h.parse::<i32>().ok()?))
    });
    let at = |d: &DisplayId| d.x == x && d.y == y;
    if let Some(name) = name {
        if let Some(i) = displays.iter().position(|d| at(d) && d.name == name) {
            return Some(i);
        }
        if let Some((w, h)) = size {
            if let Some(i) = displays.iter().position(|d| d.name == name && d.w == w && d.h == h) {
                return Some(i);
            }
        }
        let mut same_name = displays.iter().enumerate().filter(|(_, d)| d.name == name);
        if let (Some((i, _)), None) = (same_name.next(), same_name.next()) {
            return Some(i);
        }
    }
    displays.iter().position(at)
}

/// The display the island lives on: a chosen one, the primary one, or the one
/// under the cursor.
fn target_monitor(app: &AppHandle, pref: &str) -> Option<Monitor> {
    let monitors = app.available_monitors().ok()?;
    let ids: Vec<DisplayId> = monitors.iter().map(describe).collect();
    if let Some(i) = pick_display(pref, &ids) {
        return Some(monitors[i].clone());
    }
    if pref == "cursor" {
        if let Some((cx, cy)) = cursor_physical() {
            if let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)) {
                return Some(m.clone());
            }
        }
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next())
}

#[cfg(test)]
mod display_tests {
    use super::*;

    fn d(name: &str, x: i32, y: i32, w: i32, h: i32) -> DisplayId {
        DisplayId { name: name.into(), x, y, w, h }
    }

    const MON: (i32, i32) = (0, 0);
    const FHD: (u32, u32) = (1920, 1080);
    const PANEL: (u32, u32) = (720, 320);

    #[test]
    fn without_a_spot_the_island_sits_at_the_top_centre() {
        assert_eq!(panel_origin(MON, FHD, 1.0, PANEL, None), (600, 0));
        assert_eq!(panel_origin((-1920, 200), FHD, 1.0, PANEL, None), (-1320, 200));
    }

    #[test]
    fn a_spot_is_kept_whole_on_its_display() {
        let spot = |x, y| Some(IslandSpot { x, y });
        assert_eq!(panel_origin(MON, FHD, 1.0, PANEL, spot(100.0, 300.0)), (100, 300));
        // Scaled: logical spot → physical.
        assert_eq!(panel_origin(MON, (3840, 2160), 2.0, (1440, 640), spot(100.0, 300.0)), (200, 600));
        // Off any edge: pulled back so the open island still fits.
        assert_eq!(panel_origin(MON, FHD, 1.0, PANEL, spot(-50.0, 5000.0)), (0, 760));
        assert_eq!(panel_origin(MON, FHD, 1.0, PANEL, spot(5000.0, -3.0)), (1200, 0));
        // A display left of the primary one.
        assert_eq!(panel_origin((-1920, 0), FHD, 1.0, PANEL, spot(10.0, 10.0)), (-1910, 10));
    }

    #[test]
    fn a_drop_is_saved_where_it_shows_and_docks_near_the_top_centre() {
        assert_eq!(spot_after_move(MON, FHD, 1.0, PANEL, (100, 400)), Some(IslandSpot { x: 100.0, y: 400.0 }));
        assert_eq!(spot_after_move(MON, FHD, 1.0, PANEL, (1500, 900)), Some(IslandSpot { x: 1200.0, y: 760.0 }));
        assert_eq!(spot_after_move(MON, FHD, 1.0, PANEL, (620, 20)), None, "near the top centre");
        assert_eq!(spot_after_move(MON, FHD, 1.0, PANEL, (600, -40)), None, "pushed above the top");
        assert!(spot_after_move(MON, FHD, 1.0, PANEL, (100, 10)).is_some(), "at the top, but in a corner");
        assert_eq!(spot_after_move((1920, 0), (3840, 2160), 2.0, (1440, 640), (1920 + 400, 600)), Some(IslandSpot { x: 200.0, y: 300.0 }));
    }

    #[test]
    fn layer_drops_use_logical_margins_on_the_selected_output() {
        assert_eq!(layer_drop_origin((1920, -200), (3840, 2160), 2.0, (100.0, 300.0)), (2120, 400));
        // A captured pointer may leave the output; that is not a new global
        // position (in particular on a neighbouring output with another scale).
        assert_eq!(layer_drop_origin((1920, -200), (3840, 2160), 2.0, (5000.0, -100.0)), (4320, -200));
        assert_eq!(layer_drop_origin((1920, -200), (3840, 2160), 2.0, (f64::MAX, -f64::MAX)), (4320, -200));
        let at = layer_drop_origin((1920, -200), (3840, 2160), 2.0, (600.0, 10.0));
        assert_eq!(spot_after_move((1920, -200), (3840, 2160), 2.0, (1440, 640), at), None);
    }

    #[test]
    fn a_display_is_found_again_after_changes() {
        let dell = d("DELL U2720Q", 1920, 0, 2560, 1440);
        let lap = d("eDP-1", 0, 0, 1920, 1200);
        let key = dell.key();
        assert_eq!(pick_display(&key, &[lap.clone(), dell.clone()]), Some(1));
        // Rearranged: the Dell moved to the left of the laptop.
        let moved = [d("eDP-1", 2560, 0, 1920, 1200), d("DELL U2720Q", 0, 0, 2560, 1440)];
        assert_eq!(pick_display(&key, &moved), Some(1));
        // Resolution changed, still the only Dell.
        let rescaled = [lap.clone(), d("DELL U2720Q", 1920, 0, 1920, 1080)];
        assert_eq!(pick_display(&key, &rescaled), Some(1));
        // Unplugged: nothing, so the caller falls back to the primary display.
        assert_eq!(pick_display(&key, &[lap.clone()]), None);
    }

    #[test]
    fn two_identical_monitors_are_told_apart_by_place() {
        let a = d("LG 27UL500", 0, 0, 1920, 1080);
        let b = d("LG 27UL500", 1920, 0, 1920, 1080);
        assert_eq!(pick_display(&b.key(), &[a.clone(), b.clone()]), Some(1));
        assert_eq!(pick_display(&a.key(), &[a, b]), Some(0));
    }

    #[test]
    fn preferences_saved_before_still_match() {
        let lap = d("eDP-1", 0, 0, 1920, 1200);
        let ext = d("HDMI-1", 1920, 0, 1920, 1080);
        assert_eq!(pick_display("at:1920,0", &[lap.clone(), ext.clone()]), Some(1));
        assert_eq!(pick_display("primary", &[lap, ext]), None);
        assert_eq!(pick_display("at:nonsense", &[]), None);
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_linux_drag_never_enters_an_untracked_native_move() {
        let gate = PollGate::new();
        start_move(&gate, false,
            || panic!("Linux has no global button state"),
            || panic!("Linux uses the captured page lifecycle"));
        assert!(!gate.moving.load(Ordering::SeqCst));
    }

    #[test]
    fn page_drag_coordinates_are_bounded_before_integer_conversion() {
        assert!(valid_move_point(-20_000.0, 50_000.0));
        for bad in [f64::NAN, f64::INFINITY, -f64::INFINITY, f64::MAX, -f64::MAX, 1_000_001.0] {
            assert!(!valid_move_point(bad, 0.0));
            assert!(!valid_move_point(0.0, bad));
        }
    }

    #[test]
    fn a_windows_drag_requires_the_physical_left_button() {
        let gate = PollGate::new();
        start_move(&gate, true, || false, || panic!("a released button must not start a drag"));
        assert!(!gate.moving.load(Ordering::SeqCst));
    }

    #[test]
    fn a_windows_drag_arms_the_release_poll_before_entering_the_native_move_loop() {
        let gate = PollGate::new();
        let started = Cell::new(false);
        start_move(&gate, true, || true, || {
            assert!(gate.moving.load(Ordering::SeqCst));
            started.set(true);
            true
        });
        assert!(started.get());
        assert!(gate.moving.load(Ordering::SeqCst));
    }

    #[test]
    fn a_failed_native_drag_clears_the_release_poll() {
        for cursor_poll in [false, true] {
            let gate = PollGate::new();
            start_move(&gate, cursor_poll, || true, || false);
            assert!(!gate.moving.load(Ordering::SeqCst));
        }
    }
}

pub fn screen_info(app: &AppHandle, pref: &str) -> ScreenInfo {
    match target_monitor(app, pref) {
        Some(m) => {
            let scale = m.scale_factor();
            let p = m.position();
            let s = m.size();
            ScreenInfo {
                x: p.x as f64 / scale,
                y: p.y as f64 / scale,
                width: s.width as f64 / scale,
                height: s.height as f64 / scale,
                scale,
            }
        }
        None => ScreenInfo { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0, scale: 1.0 },
    }
}

/// Dropped this close to the top centre (logical px), the island docks there.
const DOCK_SNAP_X: f64 = 80.0;
const DOCK_SNAP_Y: f64 = 48.0;

/// Where the panel's top-left corner goes on a display, in physical pixels:
/// the top centre, or the user's spot kept whole on the display (so the
/// island can open downwards without leaving it).
pub fn panel_origin(mon: (i32, i32), mon_size: (u32, u32), scale: f64, panel: (u32, u32), spot: Option<IslandSpot>) -> (i32, i32) {
    let centred = mon.0 + (mon_size.0 as i32 - panel.0 as i32) / 2;
    let Some(spot) = spot else { return (centred, mon.1) };
    let clamp = |v: i32, lo: i32, room: i32| if room <= 0 { lo } else { v.clamp(lo, lo + room) };
    (
        clamp(mon.0 + (spot.x * scale).round() as i32, mon.0, mon_size.0 as i32 - panel.0 as i32),
        clamp(mon.1 + (spot.y * scale).round() as i32, mon.1, mon_size.1 as i32 - panel.1 as i32),
    )
}

/// Where a drag left the panel (`at`, physical), as a spot on that display.
/// None near the top centre: it docks there again.
pub fn spot_after_move(mon: (i32, i32), mon_size: (u32, u32), scale: f64, panel: (u32, u32), at: (i32, i32)) -> Option<IslandSpot> {
    let x = (at.0 - mon.0) as f64 / scale;
    let y = (at.1 - mon.1) as f64 / scale;
    let spot = IslandSpot { x: x.round(), y: y.round() };
    // The same clamping as placing it, so what is saved is where it shows.
    let (px, py) = panel_origin(mon, mon_size, scale, panel, Some(spot));
    let spot = IslandSpot { x: ((px - mon.0) as f64 / scale).round(), y: ((py - mon.1) as f64 / scale).round() };
    let dock_x = (mon_size.0 as f64 - panel.0 as f64) / 2.0 / scale;
    if spot.y < DOCK_SNAP_Y && (spot.x - dock_x).abs() < DOCK_SNAP_X {
        return None;
    }
    Some(spot)
}

/// Layer coordinates are local to one output, not global desktop coordinates.
/// Keep an out-of-surface release on that output, including mixed-DPI layouts.
#[cfg(any(target_os = "linux", test))]
fn layer_drop_origin(mon: (i32, i32), size: (u32, u32), scale: f64, at: (f64, f64)) -> (i32, i32) {
    let panel = ((PANEL_W * scale).round() as u32, (PANEL_H * scale).round() as u32);
    // Clamp while still floating point: a huge finite IPC coordinate must not
    // saturate to i32::MAX and overflow when the monitor origin is added.
    let x = at.0.clamp(0.0, size.0.saturating_sub(panel.0) as f64 / scale);
    let y = at.1.clamp(0.0, size.1.saturating_sub(panel.1) as f64 / scale);
    panel_origin(mon, size, scale, panel, Some(IslandSpot { x, y }))
}

/// Places and sizes the window. `collapsed` picks the wake strip instead of
/// the panel; `spot` is where the user dragged the island (None: top centre).
pub fn apply_geometry(app: &AppHandle, pref: &str, spot: Option<IslandSpot>, collapsed: bool) {
    // A captured Linux drag temporarily owns size/position. Background shape,
    // display and expansion updates must not fold its stationary overlay away.
    #[cfg(target_os = "linux")]
    if app.try_state::<crate::Shared>().is_some_and(|s| s.gate.moving.load(Ordering::SeqCst)) {
        return;
    }
    let Some(win) = window(app) else { return };
    let Some(m) = target_monitor(app, pref) else { return };

    let scale = m.scale_factor();
    let mp = *m.position();
    let ms = *m.size();

    let panel = ((PANEL_W * scale).round().max(1.0) as u32, (PANEL_H * scale).round().max(1.0) as u32);
    let (px, py) = panel_origin((mp.x, mp.y), (ms.width, ms.height), scale, panel, spot);
    let (pw, ph, x, y) = if collapsed {
        let sw = (STRIP_W * scale).round().max(1.0) as u32;
        let sh = (STRIP_H * scale).round().max(1.0) as u32;
        // The strip sits where the island's top edge is.
        (sw, sh, px + (panel.0 as i32 - sw as i32) / 2, py)
    } else {
        (panel.0, panel.1, px, py)
    };

    // GTK never sizes a non-resizable window below its natural size (200 px
    // here), so on Linux the 6 px wake strip would stay a 200 px block. tao
    // re-applies the config's `resizable: false` after the first configure, so
    // this is asked every time, just before the resize. Undecorated, the window
    // still offers the user nothing to resize it by. (Found by @YossiYad, #44.)
    #[cfg(target_os = "linux")]
    let _ = win.set_resizable(true);
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    #[cfg(windows)]
    let _ = win.set_position(PhysicalPosition::new(x, y));
    #[cfg(target_os = "linux")]
    platform::place_island(&win, (x, y), logical_origin(&m),
        ((x - mp.x) as f64 / scale, (y - mp.y) as f64 / scale));
    // Moving across displays can rescale the window: re-assert the physical size.
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_always_on_top(true);
}

/// Windows native move: the cursor poll detects release. Linux instead uses
/// a captured page gesture with explicit begin/move/end commands below.
pub fn start_move(
    gate: &PollGate,
    cursor_poll: bool,
    left_button_down: impl FnOnce() -> bool,
    start_dragging: impl FnOnce() -> bool,
) {
    if !cursor_poll || !left_button_down() {
        return;
    }
    gate.moving.store(true, Ordering::SeqCst);
    if !start_dragging() {
        gate.moving.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
pub struct MoveInfo {
    mode: &'static str,
    x: f64,
    y: f64,
    scale: f64,
}

#[cfg(target_os = "linux")]
struct PageMove {
    layer: bool,
    monitor: Monitor,
}

/// Linux keeps the implicit pointer grab in the page. On Wayland the surface
/// stays still over one output while the page paints the island under the
/// pointer. This is the same coordinate strategy as desktop Mochi.
pub fn begin_page_move(app: &AppHandle) -> Option<MoveInfo> {
    #[cfg(windows)]
    { let _ = app; None }
    #[cfg(target_os = "linux")]
    {
        let shared = app.try_state::<crate::Shared>()?;
        let win = window(app)?;
        if shared.gate.moving.load(Ordering::SeqCst) || shared.gate.collapsed.load(Ordering::Relaxed) {
            return None;
        }
        let mode = platform::island_move_mode();
        let layer = match mode {
            platform::DesktopMode::Layer => true,
            platform::DesktopMode::Window => false,
            _ => {
                crate::log::line("island drag needs layer-shell or X11 for saved placement");
                return None;
            }
        };
        let settings = shared.settings.lock().unwrap().clone();
        let m = target_monitor(app, &settings.screen)?;
        let scale = m.scale_factor();
        let at = if layer {
            panel_origin((m.position().x, m.position().y), (m.size().width, m.size().height),
                scale, ((PANEL_W * scale).round() as u32, (PANEL_H * scale).round() as u32), settings.island_spot)
        } else {
            let p = win.outer_position().ok()?;
            (p.x, p.y)
        };
        let (x, y) = if layer {
            ((at.0 - m.position().x) as f64 / scale, (at.1 - m.position().y) as f64 / scale)
        } else { (at.0 as f64, at.1 as f64) };
        *shared.gate.page_move.lock().unwrap() = Some(PageMove { layer, monitor: m });
        shared.gate.moving.store(true, Ordering::SeqCst);
        platform::set_input_region(&win, None);
        if layer { platform::set_layer_overlay(&win, true); }
        Some(MoveInfo { mode: if layer { "layer" } else { "window" }, x, y, scale })
    }
}

/// Far beyond any useful desktop, but safely inside i32 monitor arithmetic.
/// IPC numbers are not trusted merely because they are finite.
#[cfg(any(target_os = "linux", test))]
fn valid_move_point(x: f64, y: f64) -> bool {
    const LIMIT: f64 = 1_000_000.0;
    x.is_finite() && y.is_finite() && x.abs() <= LIMIT && y.abs() <= LIMIT
}

pub fn update_page_move(app: &AppHandle, x: f64, y: f64) -> Result<(), String> {
    #[cfg(windows)]
    { let _ = (app, x, y); Ok(()) }
    #[cfg(target_os = "linux")]
    {
        if !valid_move_point(x, y) { return Err("invalid island position".into()); }
        let Some(shared) = app.try_state::<crate::Shared>() else { return Ok(()) };
        let state = shared.gate.page_move.lock().unwrap();
        if state.as_ref().is_some_and(|m| !m.layer) {
            let win = window(app).ok_or("island window closed")?;
            win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// None cancels without saving. Every exit folds the overlay back and restores
/// the island's click-through region, even if the selected monitor disappeared.
pub fn end_page_move(app: &AppHandle, at: Option<(f64, f64)>) {
    #[cfg(windows)]
    let _ = (app, at);
    #[cfg(target_os = "linux")]
    {
        let Some(shared) = app.try_state::<crate::Shared>() else { return };
        let Some(drag) = shared.gate.page_move.lock().unwrap().take() else { return };
        shared.gate.moving.store(false, Ordering::SeqCst);
        if let Some(win) = window(app) {
            if drag.layer { platform::set_layer_overlay(&win, false); }
            // Restore the size even if monitor discovery fails during unplug.
            let scale = win.scale_factor().unwrap_or(1.0);
            let (w, h) = if shared.gate.collapsed.load(Ordering::Relaxed) { (STRIP_W, STRIP_H) } else { (PANEL_W, PANEL_H) };
            let _ = win.set_size(PhysicalSize::new((w * scale).round() as u32, (h * scale).round() as u32));
        }
        if let Some((x, y)) = at.filter(|(x, y)| valid_move_point(*x, *y)) {
            let scale = drag.monitor.scale_factor();
            let at = if drag.layer {
                layer_drop_origin((drag.monitor.position().x, drag.monitor.position().y),
                    (drag.monitor.size().width, drag.monitor.size().height), scale, (x, y))
            } else { (x.round() as i32, y.round() as i32) };
            if drag.layer {
                // GDK scales each monitor's origin by its own scale. Those
                // physical rectangles can overlap on mixed-DPI desktops; the
                // layer's explicit output is authoritative, not a hit-test.
                if let Ok(monitors) = app.available_monitors() {
                    if let Some(m) = monitors.iter().find(|m| m.position() == drag.monitor.position()
                        && m.size() == drag.monitor.size() && m.name() == drag.monitor.name()
                        && m.scale_factor() == scale) {
                        finish_move_on_monitor(app, m, at);
                    }
                }
            } else {
                finish_move_at(app, at, scale);
            }
        }
        let settings = shared.settings.lock().unwrap().clone();
        apply_geometry(app, &settings.screen, settings.island_spot, shared.gate.collapsed.load(Ordering::Relaxed));
        refresh_click_through(app, &shared.gate);
    }
}

/// The user let go of the island after dragging it: remember where (and on
/// which display), or dock it at the top again, then place it properly.
pub fn finish_move(app: &AppHandle) {
    let Some(win) = window(app) else { return };
    let Ok(at) = win.outer_position() else { return };
    finish_move_at(app, (at.x, at.y), win.scale_factor().unwrap_or(1.0));
}

/// Explicit coordinates also work for layer surfaces, whose outer_position is
/// not a desktop position on Wayland.
fn finish_move_at(app: &AppHandle, at: (i32, i32), scale_now: f64) {
    let Some(shared) = app.try_state::<crate::Shared>() else { return };
    let Ok(monitors) = app.available_monitors() else { return };
    let pref = shared.settings.lock().unwrap().screen.clone();
    // The display under the island's top centre, where the user sees it.
    let (cx, cy) = (at.0 as f64 + PANEL_W * scale_now / 2.0, at.1 as f64 + 16.0 * scale_now);
    let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)).cloned().or_else(|| target_monitor(app, &pref))
    else {
        return;
    };
    finish_move_on_monitor(app, &m, at);
}

fn finish_move_on_monitor(app: &AppHandle, m: &Monitor, at: (i32, i32)) {
    let Some(shared) = app.try_state::<crate::Shared>() else { return };
    let pref = shared.settings.lock().unwrap().screen.clone();
    let scale = m.scale_factor();
    let panel = ((PANEL_W * scale).round() as u32, (PANEL_H * scale).round() as u32);
    let spot = spot_after_move((m.position().x, m.position().y), (m.size().width, m.size().height), scale, panel, at);
    let same_display = target_monitor(app, &pref).is_some_and(|t| t.position() == m.position() && t.size() == m.size());
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        if !same_display {
            current.screen = describe(&m).key();
        }
        current.island_spot = spot;
        if let Err(err) = crate::settings::save(&current) {
            crate::log::line(format!("could not save settings: {err}"));
        }
        current.clone()
    };
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    apply_geometry(app, &updated.screen, updated.island_spot, collapsed);
    let _ = app.emit("settings-changed", updated);
}

/// Position, size and scale of the monitor the island lives on. Any change here
/// means the island has to be placed again.
fn current_screen_key(app: &AppHandle) -> Option<(i32, i32, u32, u32, u64)> {
    let pref = app
        .try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    let m = target_monitor(app, &pref)?;
    let p = m.position();
    let size = m.size();
    Some((p.x, p.y, size.width, size.height, m.scale_factor().to_bits()))
}

/// Emits `cursor` (window-logical coordinates) at ~60 Hz while the island is
/// visible. Parked on a condvar the rest of the time.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut was_down = false;
        // Remembered across wakes so a display change while hidden is noticed the
        // moment the island comes back.
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        // Without a cursor to read (Linux) the loop only watches the display
        // layout, and twice a second is plenty for that: waking at 60 Hz just to
        // find no cursor costs CPU for nothing.
        let (period, screen_every) = if platform::CURSOR_POLL { (16, 30) } else { (500, 1) };
        loop {
            gate.wait_until_active();
            let mut last = (f64::MIN, f64::MIN);
            let mut ticks: u32 = 0;
            while gate.is_active() {
                std::thread::sleep(Duration::from_millis(period));

                // Monitors get plugged in, unplugged, rearranged and rescaled, and
                // an island pinned to coordinates that no longer exist is an island
                // nobody can reach. Checked about twice a second — the cursor poll
                // is already running, so this costs one monitor query.
                ticks = ticks.wrapping_add(1);
                if ticks % screen_every == 0 {
                    let now = current_screen_key(&app);
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            crate::log::line("display layout changed — repositioning".to_string());
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }

                // A drag of the island ends when the button comes up. Checked
                // before anything else: the cursor doesn't move relative to a
                // window that moves with it.
                if platform::CURSOR_POLL && gate.moving.load(Ordering::SeqCst) && !left_button_down() {
                    gate.moving.store(false, Ordering::SeqCst);
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || finish_move(&handle));
                }

                let Some(win) = window(&app) else { continue };
                let Ok(origin) = win.outer_position() else { continue };
                let scale = win.scale_factor().unwrap_or(1.0);
                let Some((cx, cy)) = cursor_physical() else { continue };
                let x = (cx - origin.x as f64) / scale;
                let y = (cy - origin.y as f64) / scale;
                let size = match win.inner_size() {
                    Ok(s) => (s.width as f64 / scale, s.height as f64 / scale),
                    Err(_) => (PANEL_W, PANEL_H),
                };
                if (x - last.0).abs() < 1.0 && (y - last.1).abs() < 1.0 {
                    continue;
                }
                last = (x, y);

                // Click-through: the window only takes the mouse over the island
                // shape. A small entry margin means the flag is already off by the
                // time a moving cursor reaches a button.
                let r = *gate.rect.lock().unwrap();
                let on_island = r.w > 0.0
                    && x >= r.x - HIT_MARGIN
                    && x <= r.x + r.w + HIT_MARGIN
                    && y >= r.y - HIT_MARGIN
                    && y <= r.y + r.h + HIT_MARGIN;

                // A file being dragged has to be able to find us. WS_EX_TRANSPARENT
                // — what click-through is on Windows — hides the window from
                // WindowFromPoint, so OLE finds no drop target and shows the "no
                // drop" cursor. macOS has no such problem: AppKit delivers drags to
                // registered destinations whatever ignoresMouseEvents says. So while
                // a button is held anywhere over the panel, the whole panel takes
                // the mouse, which also makes the drop zone as forgiving as the Mac's.
                // A press may be the start of a drag: make sure the drop target is
                // ours before the file arrives.
                let down = left_button_down();
                if down && !was_down {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || platform::unblock_webview_drops(&handle));
                }
                was_down = down;

                let dragging = down
                    && x >= 0.0
                    && x <= size.0
                    && y >= 0.0
                    && y <= size.1;

                let accept = on_island || dragging;
                if gate.ignoring.load(Ordering::Relaxed) == accept {
                    gate.ignoring.store(!accept, Ordering::Relaxed);
                    let _ = win.set_ignore_cursor_events(!accept);
                }

                let _ = win.emit("cursor", CursorPayload { x, y });
            }
        }
    });
}

/// Re-applies click-through after the window or the island changed shape.
///
/// With the cursor poll (Windows) the window takes the mouse again and the next
/// tick decides from the cursor. Without it (Linux) the input region is set to
/// the island itself, or to the whole wake strip while collapsed.
pub fn refresh_click_through(app: &AppHandle, gate: &PollGate) {
    if platform::CURSOR_POLL {
        set_ignore_cursor(app, false);
        gate.forget_ignore_state();
        return;
    }
    let Some(win) = window(app) else { return };
    if gate.moving.load(Ordering::SeqCst) {
        platform::set_input_region(&win, None);
        return;
    }
    let region = if gate.collapsed.load(Ordering::Relaxed) {
        // The wake strip itself, never "the whole window": if the window ever
        // fails to shrink to the strip, the rest of it must not swallow clicks
        // meant for whatever sits under the top of the screen.
        Some((0.0, 0.0, STRIP_W, STRIP_H))
    } else {
        let r = *gate.rect.lock().unwrap();
        if r.w <= 0.0 {
            // Nothing drawn yet: nothing takes the mouse.
            Some((0.0, 0.0, 0.0, 0.0))
        } else {
            let x0 = (r.x - HIT_MARGIN).max(0.0);
            let y0 = (r.y - HIT_MARGIN).max(0.0);
            let x1 = r.x + r.w + HIT_MARGIN;
            let y1 = r.y + r.h + HIT_MARGIN;
            Some((x0, y0, x1 - x0, y1 - y0))
        }
    };
    platform::set_input_region(&win, region);
}

pub fn set_ignore_cursor(app: &AppHandle, ignore: bool) {
    if let Some(win) = window(app) {
        let _ = win.set_ignore_cursor_events(ignore);
    }
}

// The region commands: the island starts a pick and attaches or drops the
// picture; each overlay (region.html, one per monitor) reports the rectangle
// or a cancel. Every command checks which window called it.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, Webview, WebviewUrl, WebviewWindowBuilder};

use super::{apply_masks, capture, clamp, file_name, png, Monitor, Rect, Regions, MAX_MASKS};
use crate::assistant::contracts::{ActionReceipt, ErrorCode};
use crate::assistant::policy::Grant;
use crate::files::DroppedFile;
use crate::island::WINDOW_LABEL;

/// Overlay windows are `region-0`, `region-1`…, one per monitor.
const OVERLAY: &str = "region-";
/// To the island: the picture to confirm (`Preview`).
pub const PREVIEW_EVENT: &str = "region-preview";
/// To the island: the pick ended without a picture ("cancelled" or "failed").
pub const ENDED_EVENT: &str = "region-ended";
/// Long enough for the closed overlays to leave the screen before the copy.
const SETTLE: Duration = Duration::from_millis(220);

fn log(line: String) {
    crate::log::line(line);
}

fn from_island(webview: &Webview) -> Result<(), ErrorCode> {
    (webview.label() == WINDOW_LABEL).then_some(()).ok_or(ErrorCode::Unavailable)
}

fn overlay_index(webview: &Webview) -> Option<usize> {
    webview.label().strip_prefix(OVERLAY)?.parse().ok()
}

fn page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/region.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("region.html".into())
}

fn close_overlays(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with(OVERLAY) {
            let _ = win.close();
        }
    }
}

fn ended(app: &AppHandle, how: &str) {
    let _ = app.emit_to(WINDOW_LABEL, ENDED_EVENT, how);
}

/// The island's "Ask about a screen region": an overlay on every monitor.
/// Returns the session id.
#[tauri::command]
pub async fn region_start(app: AppHandle, webview: Webview, regions: State<'_, Regions>) -> Result<String, ErrorCode> {
    from_island(&webview)?;
    close_overlays(&app);
    let found = app.available_monitors().map_err(|_| ErrorCode::Failed)?;
    let monitors: Vec<Monitor> = found
        .iter()
        .enumerate()
        .map(|(i, m)| Monitor {
            name: m.name().cloned().unwrap_or_else(|| format!("monitor {}", i + 1)),
            bounds: Rect { x: m.position().x, y: m.position().y, width: m.size().width, height: m.size().height },
        })
        .collect();
    if monitors.is_empty() {
        return Err(ErrorCode::Failed);
    }
    let id = regions.start(monitors.clone());
    for (i, m) in monitors.iter().enumerate() {
        let built = WebviewWindowBuilder::new(&app, format!("{OVERLAY}{i}"), page_url(&app))
            .additional_browser_args(crate::BROWSER_ARGS)
            .title("Coucou — pick a region")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .disable_drag_drop_handler()
            .visible(false)
            .build();
        let Ok(win) = built else {
            close_overlays(&app);
            regions.cancel(&id);
            return Err(ErrorCode::Failed);
        };
        // Physical, so each overlay covers exactly its monitor at its own scale.
        let _ = win.set_position(PhysicalPosition::new(m.bounds.x, m.bounds.y));
        let _ = win.set_size(PhysicalSize::new(m.bounds.width, m.bounds.height));
        let _ = win.show();
        if i == 0 {
            let _ = win.set_focus();
        }
    }
    log(format!("region: pick started on {} monitor(s)", monitors.len()));
    Ok(id)
}

/// What an overlay needs: the session and which monitor it covers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayInfo {
    pub session_id: String,
    pub index: usize,
    /// Settings' interface language ("" = the system's).
    pub language: String,
}

#[tauri::command]
pub fn region_overlay_info(webview: Webview, regions: State<Regions>, shared: State<crate::Shared>) -> Option<OverlayInfo> {
    let index = overlay_index(&webview)?;
    let session_id = regions.waiting()?;
    regions.monitor(&session_id, index)?;
    let language = shared.settings.lock().unwrap().language.clone();
    Some(OverlayInfo { session_id, index, language })
}

/// The user let go of a rectangle on one overlay (its physical pixels).
/// Every overlay closes, then that rectangle alone is copied, without
/// Coucou's own windows, and the island shows it with its grant.
#[tauri::command]
pub async fn region_pick(
    app: AppHandle,
    webview: Webview,
    regions: State<'_, Regions>,
    session_id: String,
    rect: Rect,
) -> Result<(), ErrorCode> {
    let index = overlay_index(&webview).ok_or(ErrorCode::Unavailable)?;
    let monitor = regions.monitor(&session_id, index).ok_or(ErrorCode::Expired)?;
    let Some(region) = clamp(rect, (monitor.bounds.width, monitor.bounds.height)) else {
        // A click, not a region: the overlays stay for another try.
        return Err(ErrorCode::InvalidArgs);
    };
    close_overlays(&app);
    let ours: Vec<isize> = [crate::island::window(&app), app.get_webview_window(crate::desktop::LABEL)]
        .into_iter()
        .flatten()
        .filter_map(|w| crate::platform::hwnd_of(&w))
        .map(|h| h.0 as isize)
        .collect();
    let on_screen = Rect { x: monitor.bounds.x + region.x, y: monitor.bounds.y + region.y, ..region };
    let image = tauri::async_runtime::spawn_blocking(move || {
        let _hidden = capture::Excluded::new(ours.into_iter().map(|h| windows::Win32::Foundation::HWND(h as *mut _)));
        std::thread::sleep(SETTLE);
        capture::grab(on_screen)
    })
    .await
    .ok()
    .flatten();
    let Some(image) = image else {
        regions.cancel(&session_id);
        log("region: the screen could not be copied".into());
        ended(&app, "failed");
        return Err(ErrorCode::Failed);
    };
    match regions.picked(&session_id, &monitor, region, image) {
        Ok(preview) => {
            log(format!("region: {}x{} picked, waiting for confirmation", region.width, region.height));
            let _ = app.emit_to(WINDOW_LABEL, PREVIEW_EVENT, &preview);
            Ok(())
        }
        Err(code) => {
            regions.cancel(&session_id);
            ended(&app, "failed");
            Err(code)
        }
    }
}

/// Escape or a right click on an overlay, or Cancel on the island's card:
/// the overlays close and the picture, if any, is dropped.
#[tauri::command]
pub fn region_cancel(app: AppHandle, webview: Webview, regions: State<Regions>, session_id: String) -> Result<(), ErrorCode> {
    if webview.label() != WINDOW_LABEL && overlay_index(&webview).is_none() {
        return Err(ErrorCode::Unavailable);
    }
    close_overlays(&app);
    if regions.cancel(&session_id) {
        log("region: cancelled".into());
        ended(&app, "cancelled");
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub file: DroppedFile,
    pub receipt: ActionReceipt,
}

/// "Ask about it": the grant is checked against the action Rust kept, the
/// masks black out what the user covered, and the PNG goes into the inbox
/// for the chat to send with the next question.
#[tauri::command]
pub async fn region_attach(
    webview: Webview,
    regions: State<'_, Regions>,
    session_id: String,
    grant: Grant,
    masks: Vec<Rect>,
) -> Result<Attached, ErrorCode> {
    from_island(&webview)?;
    if masks.len() > MAX_MASKS {
        return Err(ErrorCode::InvalidArgs);
    }
    let (action_id, mut image) = regions.confirm(&session_id, &grant)?;
    let masked = masks.len();
    let stored = tauri::async_runtime::spawn_blocking(move || {
        apply_masks(&mut image, &masks);
        let bytes = png(&image)?;
        crate::files::store(&file_name(&crate::platform::local_time()), &bytes)
    })
    .await
    .map_err(|_| ErrorCode::Failed)?;
    let receipt_for = |r: &ActionReceipt| {
        let outcome = serde_json::to_value(r.outcome()).ok();
        log(format!(
            "assistant action {action_id} {} ({masked} mask(s))",
            outcome.as_ref().and_then(|v| v.as_str()).unwrap_or("?")
        ));
    };
    match stored {
        Ok(file) => {
            let receipt = ActionReceipt::succeeded(&action_id, "inbox:png").ok_or(ErrorCode::Failed)?;
            receipt_for(&receipt);
            Ok(Attached { file, receipt })
        }
        Err(_) => {
            receipt_for(&ActionReceipt::failed(&action_id, ErrorCode::Failed));
            Err(ErrorCode::Failed)
        }
    }
}

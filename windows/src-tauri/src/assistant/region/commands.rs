// The region commands: the island starts a pick and attaches or drops the
// picture; each overlay (region.html, one per monitor) reports the rectangle
// or a cancel. Every command checks which window called it.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, Webview, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use super::{apply_masks, capture, clamp, file_name, png, Monitor, Rect, Regions, MAX_MASKS};
use crate::assistant::contracts::{ActionReceipt, ErrorCode};
use crate::assistant::policy::Grant;
use crate::files::DroppedFile;
use crate::island::WINDOW_LABEL;

/// Overlay windows carry their session id and monitor index.
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

fn overlay_owner(label: &str) -> Option<(&str, usize)> {
    let (session, index) = label.strip_prefix(OVERLAY)?.rsplit_once('-')?;
    Some((session, index.parse().ok()?))
}

fn overlay_index(webview: &Webview, session_id: &str) -> Option<usize> {
    let (owner, index) = overlay_owner(webview.label())?;
    (owner == session_id).then_some(index)
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

fn close_overlays(app: &AppHandle, session_id: Option<&str>) {
    for (label, win) in app.webview_windows() {
        if overlay_owner(&label).is_some_and(|(owner, _)| session_id.is_none_or(|id| id == owner)) {
            // Intentional teardown must bypass CloseRequested: other monitors
            // have not picked, and their user-close handler would cancel it.
            let _ = win.destroy();
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ended<'a> {
    session_id: &'a str,
    reason: &'a str,
}

fn ended(app: &AppHandle, session_id: &str, reason: &str) {
    let _ = app.emit_to(WINDOW_LABEL, ENDED_EVENT, Ended { session_id, reason });
}

fn cancel_session(app: &AppHandle, regions: &Regions, session_id: &str) {
    if regions.cancel(session_id) {
        close_overlays(app, Some(session_id));
        log("region: cancelled".into());
        ended(app, session_id, "cancelled");
    }
}

/// The island's "Ask about a screen region": an overlay on every monitor.
/// Returns the session id.
#[tauri::command]
pub async fn region_start(app: AppHandle, webview: Webview, regions: State<'_, Regions>) -> Result<String, ErrorCode> {
    from_island(&webview)?;
    close_overlays(&app, None);
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
        let built = WebviewWindowBuilder::new(&app, format!("{OVERLAY}{id}-{i}"), page_url(&app))
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
            close_overlays(&app, Some(&id));
            regions.cancel(&id);
            return Err(ErrorCode::Failed);
        };
        // A user can close the first monitor while the others are building.
        // Do not bring back overlays for a canceled or replaced session.
        if regions.waiting().as_deref() != Some(id.as_str()) {
            let _ = win.destroy();
            close_overlays(&app, Some(&id));
            // A very fast pick may already own the capture; it still needs
            // its startup reply so the island can accept the queued preview.
            return if regions.owns(&id) { Ok(id) } else { Err(ErrorCode::Expired) };
        }
        let close_app = app.clone();
        let close_id = id.clone();
        win.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                cancel_session(&close_app, &close_app.state::<Regions>(), &close_id);
            }
        });
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
    let session_id = regions.waiting()?;
    let index = overlay_index(&webview, &session_id)?;
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
    let index = overlay_index(&webview, &session_id).ok_or(ErrorCode::Unavailable)?;
    let monitor = regions.monitor(&session_id, index).ok_or(ErrorCode::Expired)?;
    let Some(region) = clamp(rect, (monitor.bounds.width, monitor.bounds.height)) else {
        // A click, not a region: the overlays stay for another try.
        return Err(ErrorCode::InvalidArgs);
    };
    let monitor = regions.claim_pick(&session_id, index).ok_or(ErrorCode::Expired)?;
    close_overlays(&app, Some(&session_id));
    let ours: Vec<isize> = app.webview_windows()
        .into_iter()
        .filter(|(label, _)| !label.starts_with(OVERLAY))
        .filter_map(|(_, w)| crate::platform::hwnd_of(&w))
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
        ended(&app, &session_id, "failed");
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
            ended(&app, &session_id, "failed");
            Err(code)
        }
    }
}

/// Escape or a right click on an overlay, or Cancel on the island's card:
/// the overlays close and the picture, if any, is dropped.
#[tauri::command]
pub fn region_cancel(app: AppHandle, webview: Webview, regions: State<Regions>, session_id: String) -> Result<(), ErrorCode> {
    if webview.label() != WINDOW_LABEL && overlay_index(&webview, &session_id).is_none() {
        return Err(ErrorCode::Unavailable);
    }
    cancel_session(&app, &regions, &session_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_labels_bind_the_monitor_to_its_own_session() {
        assert_eq!(overlay_owner("region-reg-012345-0"), Some(("reg-012345", 0)));
        assert_eq!(overlay_owner("region-reg-6789ab-2"), Some(("reg-6789ab", 2)));
        assert_eq!(overlay_owner("settings"), None);
        assert_eq!(overlay_owner("region-0"), None);
        assert_eq!(overlay_owner("region-reg-012345-bad"), None);
    }

    #[test]
    fn terminal_events_keep_the_session_identity() {
        let event = serde_json::to_value(Ended { session_id: "reg-123", reason: "cancelled" }).unwrap();
        assert_eq!(event, serde_json::json!({ "sessionId": "reg-123", "reason": "cancelled" }));
    }
}

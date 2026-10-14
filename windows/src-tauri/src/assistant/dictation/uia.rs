// The dictation thread on Windows: UI Automation in a COM MTA of its own.
// Holds one capture's field, bounded by its native preview deadline once
// offered. Logs say what happened, never the words or the app.

use std::sync::mpsc;
use std::time::Instant;

use windows::core::{BSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationValuePattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, UIA_TextPatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow};

use super::*;

/// UI Automation's own limits per call, so a hung app answers "busy" rather
/// than holding the thread (well under REPLY_TIMEOUT).
const UIA_TIMEOUT_MS: u32 = 1500;

struct Field {
    window: HWND,
    pid: u32,
    element: IUIAutomationElement,
}

pub(super) fn run(jobs: mpsc::Receiver<Job>, log: fn(String)) {
    // Both S_OK and S_FALSE are balanced by the CoUninitialize at the end.
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        log("dictation: COM unavailable".into());
        return refuse_all(jobs);
    }
    match automation() {
        Ok(ua) => serve(&ua, jobs, log),
        Err(_) => {
            log("dictation: UI Automation unavailable".into());
            refuse_all(jobs);
        }
    }
    unsafe { CoUninitialize() };
}

fn automation() -> windows::core::Result<IUIAutomation2> {
    let ua: IUIAutomation2 = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
    unsafe {
        ua.SetConnectionTimeout(UIA_TIMEOUT_MS)?;
        ua.SetTransactionTimeout(UIA_TIMEOUT_MS)?;
    }
    Ok(ua)
}

fn refuse_all(jobs: mpsc::Receiver<Job>) {
    for job in jobs {
        match job {
            Job::Probe(_, reply) => drop(reply.send(false)),
            Job::Bind { .. } => {}
            Job::Offer(_, _, reply) => drop(reply.send(Err(Refusal::Unsupported.into()))),
            Job::CheckCopy(_, _, reply) => drop(reply.send(Err(Refusal::Busy))),
            Job::Write(_, _, _, reply) => drop(reply.send(Err(Refusal::Gone.into()))),
        }
    }
}

fn serve(ua: &IUIAutomation2, jobs: mpsc::Receiver<Job>, log: fn(String)) {
    let mut targets = CaptureTargets::new();
    for job in jobs {
        // Release the element at its original deadline, but keep its capture
        // so expiry remains a refusal rather than an absent initial target.
        if let Some(target) = targets.bound.as_mut() {
            target.expire(Instant::now());
        }
        match job {
            Job::Probe(id, reply) => targets.probe(id, reply, || focused_field(ua)),
            Job::Bind { probe, answered, capture } => {
                if targets.bind(probe, answered, capture) {
                    log("dictation: field remembered for the capture".into());
                }
            }
            Job::Offer(capture, expires, reply) => {
                let answer = offer_bound(&mut targets.bound, &capture, expires, Instant::now(), |field| {
                    check(field).map(|_| Offer {
                        label: app_label(field.pid),
                        target_window: field.window.0 as usize as u64,
                        target_pid: field.pid,
                    })
                });
                if let Err(why) = answer {
                    log(format!("dictation: no insertion offered ({})", name(why.reason)));
                }
                let _ = reply.send(answer);
            }
            Job::CheckCopy(capture, expires, reply) => {
                if reply.is_closed() { continue; }
                let answer = copy_bound(&mut targets.bound, &capture, expires, Instant::now, |field| {
                    copy_revalidated(
                        || unsafe { field.element.CurrentProcessId() }
                            .map(|pid| pid as u32 == field.pid).map_err(|_| Refusal::Busy),
                        || unsafe { field.element.CurrentIsPassword() }
                            .map(|password| password.as_bool()).map_err(|_| Refusal::Busy),
                        || window_state(field).0 == Some(field.pid),
                    )
                });
                let _ = reply.send(answer);
            }
            Job::Write(capture, args, permit, reply) => {
                let answer = write_bound(&mut targets.bound, |target| {
                    target.capture == capture
                        && target.field.as_ref().is_some_and(|field| {
                            field.window.0 as usize as u64 == args.target_window && field.pid == args.target_pid
                        })
                }, |target| {
                    let field = target.field.as_ref().ok_or(Refusal::Gone)?;
                    write(field, &args.text, &permit, &reply)
                });
                log(match answer {
                    Ok(Written::Verified) => "dictation: text inserted and read back".into(),
                    Ok(Written::Unconfirmed) => "dictation: text set but not read back".into(),
                    Ok(Written::Failed) => "dictation: the field refused the text".into(),
                    Err(why) => format!("dictation: nothing inserted ({})", name(why.reason)),
                });
                let _ = reply.send(answer);
            }
        }
    }
}

fn name(why: Refusal) -> String {
    serde_json::to_value(why).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The field that has the keyboard, in the foreground window of another app.
fn focused_field(ua: &IUIAutomation2) -> Option<Field> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let pid = window_pid(window);
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let element = unsafe { ua.GetFocusedElement() }.ok()?;
    // The focus can sit in a popup of another process (a system menu, say):
    // only a field of the foreground app is a target.
    let element_pid = unsafe { element.CurrentProcessId() }.ok()? as u32;
    (element_pid == pid).then(|| Field { window, pid, element })
}

fn facts(field: &Field) -> (Facts, Option<IUIAutomationValuePattern>) {
    let el = &field.element;
    let element_alive = unsafe { el.CurrentProcessId() }.is_ok_and(|p| p as u32 == field.pid);
    let enabled = unsafe { el.CurrentIsEnabled() }.is_ok_and(|b| b.as_bool());
    let pattern = unsafe { el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }.ok();
    let value_read_only = pattern.as_ref().map(|p| unsafe { p.CurrentIsReadOnly() }.map_or(true, |b| b.as_bool()));
    let password = unsafe { el.CurrentIsPassword() }.map_or(true, |b| b.as_bool());
    // Query window/focus after the blocking provider reads, so their latency
    // cannot leave us relying on a foreground snapshot from before them.
    let (target_pid, foreground) = window_state(field);
    let window_alive = target_pid == Some(field.pid);
    (Facts { window_alive, foreground, element_alive, password, enabled, value_read_only }, pattern)
}

/// Native checks only: focus restoration must not wait on the UIA provider.
/// None means the target window is gone; a different PID means it was reused.
fn window_state(field: &Field) -> (Option<u32>, Foreground) {
    let target_pid = unsafe { IsWindow(Some(field.window)) }.as_bool().then(|| window_pid(field.window));
    let foreground = unsafe { GetForegroundWindow() };
    let foreground = if foreground == field.window {
        Foreground::Target
    } else if !foreground.is_invalid() && window_pid(foreground) == std::process::id() {
        Foreground::Coucou
    } else {
        Foreground::Other
    };
    (target_pid, foreground)
}

fn check(field: &Field) -> Result<IUIAutomationValuePattern, Rejection> {
    let (facts, pattern) = facts(field);
    match (refusal(&facts), pattern) {
        (Some(why), _) => Err(Rejection::checked(why, &facts)),
        (None, Some(pattern)) => Ok(pattern),
        (None, None) => Err(Rejection::checked(Refusal::Unsupported, &facts)),
    }
}

fn write(field: &Field, text: &str, permit: &WritePermit, reply: &Reply<Result<Written, Rejection>>) -> Result<Written, Rejection> {
    let permitted = || {
        permit.check()?;
        if reply.is_closed() { return Err(Refusal::Gone); }
        Ok(())
    };
    permitted()?;
    let pattern = check(field)?;
    let current = unsafe { pattern.CurrentValue() }.map_err(|_| Refusal::Gone)?.to_string();
    let selection = caret(&field.element);
    let expected_caret = selection.as_ref().map(|(before, selected)| (before.as_str(), selected.as_str()));
    let next = spliced(&current, expected_caret, text);
    let next_value = BSTR::from(next.as_str());
    write_revalidated(
        &current,
        expected_caret,
        || check(field),
        |pattern| unsafe { pattern.CurrentValue() }.map(|value| value.to_string()).map_err(|_| Refusal::Gone.into()),
        || caret(&field.element),
        &permitted,
        |pattern| {
            if unsafe { pattern.SetValue(&next_value) }.is_err() {
                return Written::Failed;
            }
            finish_write(
                field.pid,
                || unsafe { pattern.CurrentValue() }.is_ok_and(|value| verified(&value.to_string(), &next)),
                &permitted,
                || window_state(field),
                || unsafe { SetForegroundWindow(field.window) }.as_bool(),
            )
        },
    )
}

/// (text before the caret, selected text), from the field's TextPattern.
fn caret(element: &IUIAutomationElement) -> Option<(String, String)> {
    unsafe {
        let text: IUIAutomationTextPattern = element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
        let selection = text.GetSelection().ok()?;
        if selection.Length().ok()? < 1 {
            return None;
        }
        let range = selection.GetElement(0).ok()?;
        let before = text.DocumentRange().ok()?;
        before.MoveEndpointByRange(TextPatternRangeEndpoint_End, &range, TextPatternRangeEndpoint_Start).ok()?;
        Some((before.GetText(-1).ok()?.to_string(), range.GetText(-1).ok()?.to_string()))
    }
}

// ── The app's name ───────────────────────────────────────────────────────────

/// The app as Windows describes it (its FileDescription), or its file name.
fn app_label(pid: u32) -> String {
    let Some(path) = image_path(pid) else { return "this app".into() };
    let label = description(&path).unwrap_or_else(|| {
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string()
    });
    label.chars().take(60).collect()
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn description(path: &str) -> Option<String> {
    let wide = windows::core::HSTRING::from(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(&wide, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&wide, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr().cast(), windows::core::w!("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool()
            || len < 4
        {
            return None;
        }
        let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
        let key = windows::core::HSTRING::from(format!("\\StringFileInfo\\{:04x}{:04x}\\FileDescription", pair[0], pair[1]));
        if !VerQueryValueW(data.as_ptr().cast(), &key, &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let text = String::from_utf16_lossy(text).trim_end_matches('\0').trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

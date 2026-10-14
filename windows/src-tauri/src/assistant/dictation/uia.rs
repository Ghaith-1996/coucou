// The dictation thread on Windows: UI Automation in a COM MTA of its own.
// Holds at most one remembered field, for at most MAX_AGE. Logs say what
// happened, never the words or the app.

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
    capture: Option<String>,
    window: HWND,
    pid: u32,
    element: IUIAutomationElement,
    since: Instant,
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
            Job::Probe(reply) => drop(reply.send(())),
            Job::Bind(_) => {}
            Job::Offer(_, reply) => drop(reply.send(Err(Refusal::Unsupported))),
            Job::Write(_, _, _, reply) => drop(reply.send(Err(Refusal::Gone))),
        }
    }
}

fn serve(ua: &IUIAutomation2, jobs: mpsc::Receiver<Job>, log: fn(String)) {
    // The latest probe, until a capture claims it; then the capture's field.
    let mut pending: Option<Field> = None;
    let mut bound: Option<Field> = None;
    for job in jobs {
        // A field past its age is dropped whatever comes next.
        if bound.as_ref().is_some_and(|f| f.since.elapsed() > MAX_AGE) {
            bound = None;
        }
        match job {
            Job::Probe(reply) => {
                pending = focused_field(ua);
                let _ = reply.send(());
            }
            Job::Bind(capture) => {
                let probed = pending.take();
                match capture {
                    // A stop press: that capture keeps the field it started with.
                    Some(id) if bound.as_ref().is_some_and(|f| f.capture.as_deref() == Some(&id)) => {}
                    Some(id) => {
                        bound = probed.map(|f| Field { capture: Some(id), ..f });
                        if bound.is_some() {
                            log("dictation: field remembered for the capture".into());
                        }
                    }
                    None => {}
                }
            }
            Job::Offer(capture, reply) => {
                let answer = match bound.as_ref().filter(|f| f.capture.as_deref() == Some(&capture)) {
                    None => Err(Refusal::Gone),
                    Some(field) => check(field).map(|_| Offer {
                        label: app_label(field.pid),
                        target_window: field.window.0 as usize as u64,
                        target_pid: field.pid,
                    }),
                };
                if let Err(why) = answer {
                    log(format!("dictation: no insertion offered ({})", name(why)));
                }
                let _ = reply.send(answer);
            }
            Job::Write(capture, args, permit, reply) => {
                let answer = write_bound(&mut bound, |f| {
                    f.capture.as_deref() == Some(&capture)
                        && f.window.0 as usize as u64 == args.target_window
                        && f.pid == args.target_pid
                }, |field| write(field, &args.text, &permit, &reply));
                log(match answer {
                    Ok(Written::Verified) => "dictation: text inserted and read back".into(),
                    Ok(Written::Unconfirmed) => "dictation: text set but not read back".into(),
                    Ok(Written::Failed) => "dictation: the field refused the text".into(),
                    Err(why) => format!("dictation: nothing inserted ({})", name(why)),
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
    (element_pid == pid).then(|| Field { capture: None, window, pid, element, since: Instant::now() })
}

fn facts(field: &Field) -> (Facts, Option<IUIAutomationValuePattern>) {
    let window_alive = unsafe { IsWindow(Some(field.window)) }.as_bool() && window_pid(field.window) == field.pid;
    let foreground = unsafe { GetForegroundWindow() };
    let foreground = if foreground == field.window {
        Foreground::Target
    } else if !foreground.is_invalid() && window_pid(foreground) == std::process::id() {
        Foreground::Coucou
    } else {
        Foreground::Other
    };
    let el = &field.element;
    let element_alive = unsafe { el.CurrentProcessId() }.is_ok_and(|p| p as u32 == field.pid);
    let password = unsafe { el.CurrentIsPassword() }.map_or(true, |b| b.as_bool());
    let enabled = unsafe { el.CurrentIsEnabled() }.is_ok_and(|b| b.as_bool());
    let pattern = unsafe { el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }.ok();
    let value_read_only = pattern.as_ref().map(|p| unsafe { p.CurrentIsReadOnly() }.map_or(true, |b| b.as_bool()));
    (Facts { window_alive, foreground, element_alive, password, enabled, value_read_only }, pattern)
}

fn check(field: &Field) -> Result<IUIAutomationValuePattern, Refusal> {
    let (facts, pattern) = facts(field);
    match (refusal(&facts), pattern) {
        (Some(why), _) => Err(why),
        (None, Some(pattern)) => Ok(pattern),
        (None, None) => Err(Refusal::Unsupported),
    }
}

fn write(field: &Field, text: &str, permit: &WritePermit, reply: &Reply<Result<Written, Refusal>>) -> Result<Written, Refusal> {
    permit.check()?;
    if reply.is_closed() { return Err(Refusal::Gone); }
    let pattern = check(field)?;
    let current = unsafe { pattern.CurrentValue() }.map_err(|_| Refusal::Gone)?.to_string();
    let caret = caret(&field.element);
    let next = spliced(&current, caret.as_ref().map(|(b, s)| (b.as_str(), s.as_str())), text);
    // UIA reads can block. Check again immediately before the only effect,
    // so a cancelled/replaced capture or a timed-out queued job cannot write.
    let next = BSTR::from(next);
    permit.check()?;
    if reply.is_closed() { return Err(Refusal::Gone); }
    if unsafe { pattern.SetValue(&next) }.is_err() {
        return Ok(Written::Failed);
    }
    let after = unsafe { pattern.CurrentValue() }.map(|v| v.to_string()).unwrap_or_default();
    // Give the keyboard back to the app the words went to. Coucou has it
    // (the island's card), so Windows lets it pass it on.
    let _ = unsafe { SetForegroundWindow(field.window) };
    Ok(if verified(&after, text) { Written::Verified } else { Written::Unconfirmed })
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

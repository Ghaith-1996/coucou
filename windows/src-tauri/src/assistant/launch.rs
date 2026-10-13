// Starting a confirmed app (`launch-app`) and seeing that it opened.
//
// The app is opened the way Start opens it: ShellExecuteEx on
// `shell:AppsFolder\<AppID>`, with no arguments and no error dialog. Success
// needs evidence: a visible window of the started process, or another app's
// window coming to the front (a Store app, or an app already running, opens
// through another process). With neither within the wait, the outcome is
// `unknown`, never retried: opening an app twice is not the same as once.

use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER, SW_SHOWNORMAL,
};
use windows::core::BOOL;

use super::contracts::{ActionReceipt, ErrorCode};

/// How long a window has to appear.
pub const WAIT: Duration = Duration::from_secs(10);

/// Opens the app with this AppsFolder AppID (apps.rs) and reports what came
/// of it, for `action_id`.
pub fn open(action_id: &str, app_id: &str) -> ActionReceipt {
    let before = unsafe { GetForegroundWindow() };
    let file = HSTRING::from(format!(r"shell:AppsFolder\{app_id}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) }.is_err() {
        return ActionReceipt::failed(action_id, ErrorCode::Failed);
    }
    // No process when the shell handed it to one already running.
    let process = info.hProcess;
    let pid = if process.is_invalid() { 0 } else { unsafe { GetProcessId(process) } };
    if !process.is_invalid() {
        let _ = unsafe { CloseHandle(process) };
    }

    let ours = std::process::id();
    let started = Instant::now();
    while started.elapsed() < WAIT {
        if pid != 0 && has_window(pid) {
            if let Some(done) = ActionReceipt::succeeded(action_id, &format!("window:pid:{pid}")) {
                return done;
            }
        }
        let front = unsafe { GetForegroundWindow() };
        if !front.is_invalid() && front != before {
            let owner = window_pid(front);
            if owner != 0 && owner != ours {
                if let Some(done) = ActionReceipt::succeeded(action_id, &format!("foreground:pid:{owner}")) {
                    return done;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ActionReceipt::unknown(action_id)
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// A visible top-level window (not owned by another) of this process.
fn has_window(pid: u32) -> bool {
    struct Search {
        pid: u32,
        found: bool,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let top = unsafe { GetWindow(hwnd, GW_OWNER) }.map_or(true, |o| o.is_invalid());
        if top && unsafe { IsWindowVisible(hwnd) }.as_bool() && window_pid(hwnd) == search.pid {
            search.found = true;
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { pid, found: false };
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::apps;
    use crate::assistant::contracts::Outcome;

    /// Opens the real Calculator from the real list, then the caller closes
    /// it (opt-in: it shows a window on this computer).
    #[test]
    #[ignore]
    fn the_real_calculator_opens_with_evidence() {
        let list = apps::catalog();
        let app = apps::find(&list, "calculatrice").expect("Calculator in the Start list");
        let started = Instant::now();
        let receipt = open("act-0000000000000001", &app.target);
        println!("{} → {:?} in {} ms", app.label, receipt, started.elapsed().as_millis());
        assert_eq!(receipt.outcome(), Outcome::Succeeded);
    }

    #[test]
    #[ignore]
    fn an_app_id_that_is_not_in_the_list_fails_without_a_dialog() {
        let receipt = open("act-0000000000000002", "Not.A.Real.App_0000000000000!App");
        println!("{receipt:?}");
        assert_ne!(receipt.outcome(), Outcome::Succeeded);
    }
}

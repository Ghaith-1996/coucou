// Starting a confirmed app (`launch-app`) and seeing that it opened.
//
// The app is opened the way Start opens it: ShellExecuteEx on
// `shell:AppsFolder\<AppID>`, with no arguments and no error dialog. Success
// needs evidence: a visible window of the started process. A foreground change
// alone cannot identify the requested app: the user may simply have Alt-Tabbed.
// Store apps and already-running apps can open through another process; without
// a window tied to the returned process, the outcome is `unknown`, never retried:
// opening an app twice is not the same as once.

use std::marker::PhantomData;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER, SW_SHOWNORMAL,
};
use windows::core::BOOL;

use super::contracts::{ActionReceipt, ErrorCode};

/// How long a window has to appear.
pub const WAIT: Duration = Duration::from_secs(10);

/// Opens the app with this AppsFolder AppID (apps.rs) and reports what came
/// of it, for `action_id`.
pub async fn open(action_id: String, app_id: String) -> Result<ActionReceipt, ErrorCode> {
    on_launch_thread(move || open_in_sta(&action_id, &app_id)).await
}

/// Hands a checked web address (policy::url_ok) to the default browser.
/// Nothing tells which window or tab shows it, so a launch Windows accepted
/// is `unknown`, never `succeeded`.
pub async fn open_url(action_id: String, url: String) -> Result<ActionReceipt, ErrorCode> {
    on_launch_thread(move || {
        let file = HSTRING::from(url.as_str());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if unsafe { ShellExecuteExW(&mut info) }.is_err() {
            return ActionReceipt::failed(&action_id, ErrorCode::Failed);
        }
        ActionReceipt::unknown(&action_id)
    })
    .await
}

async fn on_launch_thread(launch: impl FnOnce() -> ActionReceipt + Send + 'static) -> Result<ActionReceipt, ErrorCode> {
    let (send, recv) = tokio::sync::oneshot::channel();
    // Shell extensions may require STA. A reused async/blocking worker could
    // already be MTA; never change its apartment or run the shell without COM.
    std::thread::Builder::new()
        .name("coucou-app-launch".into())
        .spawn(move || {
            let _ = send.send(with_sta(launch));
        })
        .map_err(|_| ErrorCode::Failed)?;
    recv.await.map_err(|_| ErrorCode::Failed)?
}

fn with_sta(launch: impl FnOnce() -> ActionReceipt) -> Result<ActionReceipt, ErrorCode> {
    let _com = Sta::initialize()?;
    Ok(launch())
}

// COM initialization and teardown must happen on the same thread. Rc makes
// the guard neither Send nor Sync, without allocating an Rc at runtime.
struct Sta(PhantomData<Rc<()>>);

impl Sta {
    fn initialize() -> Result<Self, ErrorCode> {
        // Both S_OK and S_FALSE own one CoUninitialize. A failed init owns
        // none and must prevent the launch, including RPC_E_CHANGED_MODE.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok().map_err(|_| ErrorCode::Failed)?;
        Ok(Self(PhantomData))
    }
}

impl Drop for Sta {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn open_in_sta(action_id: &str, app_id: &str) -> ActionReceipt {
    // A Start menu shortcut AppsFolder doesn't list (apps.rs) opens as the
    // shortcut itself, the way a click on it in Start does.
    let file = if super::apps::is_start_shortcut(app_id) {
        HSTRING::from(app_id)
    } else {
        HSTRING::from(format!(r"shell:AppsFolder\{app_id}"))
    };
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
    // Keep the process object alive while observing windows so its PID cannot
    // be recycled for an unrelated process. This closes the handle on any return.
    // SAFETY: ShellExecuteExW returned this owned handle with NOCLOSEPROCESS.
    let _process = (!process.is_invalid()).then(|| unsafe { OwnedHandle::from_raw_handle(process.0) });
    let pid = if process.is_invalid() { 0 } else { unsafe { GetProcessId(process) } };

    let started = Instant::now();
    while started.elapsed() < WAIT {
        if pid != 0 && has_window(pid) {
            if let Some(done) = ActionReceipt::succeeded(action_id, &format!("window:pid:{pid}")) {
                return done;
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

fn is_launch_window(launch_pid: u32, owner_pid: u32, visible: bool, top_level: bool) -> bool {
    launch_pid != 0 && owner_pid == launch_pid && visible && top_level
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
        if is_launch_window(search.pid, window_pid(hwnd), unsafe { IsWindowVisible(hwnd) }.as_bool(), top) {
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
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_OK};
    use windows::Win32::System::Com::COINIT_MULTITHREADED;

    // Probe the real COM apartment without opening any app. Balance the probe
    // if it unexpectedly succeeds, so even a failing test leaves COM clean.
    fn assert_sta() {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            unsafe { CoUninitialize() };
        }
        assert_eq!(result, RPC_E_CHANGED_MODE);
    }

    fn assert_com_was_uninitialized() {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            unsafe { CoUninitialize() };
        }
        // S_OK (not S_FALSE) proves the previous initialization was balanced.
        assert_eq!(result, S_OK);
    }

    struct TestMta;

    impl TestMta {
        fn initialize() -> Self {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().unwrap();
            Self
        }
    }

    impl Drop for TestMta {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_launch_operation_gets_its_own_sta_even_when_the_caller_is_mta() {
        let _caller_com = TestMta::initialize();
        let caller = std::thread::current().id();
        let receipt = on_launch_thread(move || {
            assert_ne!(std::thread::current().id(), caller);
            assert_sta();
            ActionReceipt::unknown("act-sta")
        }).await.unwrap();
        assert_eq!(receipt, ActionReceipt::unknown("act-sta"));
        assert!(Sta::initialize().is_err(), "the caller's MTA must remain untouched");
    }

    #[test]
    fn sta_is_balanced_after_success_failure_and_unknown_receipts() {
        std::thread::spawn(|| {
            for receipt in [
                ActionReceipt::succeeded("act-sta", "test:window").unwrap(),
                ActionReceipt::failed("act-sta", ErrorCode::Failed),
                ActionReceipt::unknown("act-sta"),
            ] {
                let result = with_sta(|| {
                    assert_sta();
                    receipt.clone()
                });
                assert_eq!(result, Ok(receipt));
                assert_com_was_uninitialized();
            }
        }).join().unwrap();
    }

    #[test]
    fn an_existing_sta_is_balanced_once_for_s_false() {
        std::thread::spawn(|| {
            let caller_com = Sta::initialize().unwrap();
            assert!(with_sta(|| ActionReceipt::unknown("act-sta")).is_ok());
            assert_sta(); // The inner S_FALSE must not release the caller's STA.
            drop(caller_com);
            assert_com_was_uninitialized(); // Nor may it leak an extra init.
        }).join().unwrap();
    }

    #[test]
    fn incompatible_com_skips_the_operation_and_does_not_uninitialize_the_caller() {
        std::thread::spawn(|| {
            let caller_com = TestMta::initialize();
            let mut called = false;
            let result = with_sta(|| {
                called = true;
                ActionReceipt::unknown("act-sta")
            });
            assert_eq!(result, Err(ErrorCode::Failed));
            assert!(!called);
            assert!(Sta::initialize().is_err(), "failed init must leave the caller's MTA intact");
            drop(caller_com);
            assert_com_was_uninitialized();
        }).join().unwrap();
    }

    #[test]
    fn sta_is_balanced_when_the_operation_panics() {
        std::thread::spawn(|| {
            let result = std::panic::catch_unwind(|| with_sta(|| panic!("mock shell failure")));
            assert!(result.is_err());
            assert_com_was_uninitialized();
        }).join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_panicking_launch_thread_returns_an_error_without_retrying() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let result = on_launch_thread(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            panic!("mock shell failure");
        }).await;
        assert_eq!(result, Err(ErrorCode::Failed));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_visible_window_of_the_started_process_is_evidence() {
        assert!(is_launch_window(42, 42, true, true));
    }

    #[test]
    fn switching_to_an_unrelated_app_is_not_evidence() {
        assert!(!is_launch_window(42, 99, true, true));
    }

    #[test]
    fn a_store_or_reused_process_without_a_launch_pid_is_unverified() {
        assert!(!is_launch_window(0, 99, true, true));
        assert!(!is_launch_window(0, 0, true, true));
    }

    #[test]
    fn hidden_or_owned_windows_are_not_evidence() {
        assert!(!is_launch_window(42, 42, false, true));
        assert!(!is_launch_window(42, 42, true, false));
    }

    /// Opens the real Calculator from the real list, then the caller closes
    /// it (opt-in: it shows a window on this computer). Store activation may
    /// return no matching process window, so unknown is an honest outcome.
    #[tokio::test]
    #[ignore]
    async fn the_real_calculator_opens_or_reports_unverified() {
        let list = apps::catalog();
        let app = apps::find(&list, "calculatrice").expect("Calculator in the Start list");
        let started = Instant::now();
        let receipt = open("act-0000000000000001".into(), app.target.clone()).await.unwrap();
        println!("{} → {:?} in {} ms", app.label, receipt, started.elapsed().as_millis());
        assert!(matches!(receipt.outcome(), Outcome::Succeeded | Outcome::Unknown));
    }

    #[tokio::test]
    #[ignore]
    async fn an_app_id_that_is_not_in_the_list_fails_without_a_dialog() {
        let receipt = open("act-0000000000000002".into(), "Not.A.Real.App_0000000000000!App".into()).await.unwrap();
        println!("{receipt:?}");
        assert_ne!(receipt.outcome(), Outcome::Succeeded);
    }
}

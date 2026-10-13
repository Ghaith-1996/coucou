// Voice stops when the user is away: the screen locks, the session is
// disconnected or logged off, or the PC goes to sleep. A hidden window (never
// shown, no taskbar entry) receives those notices from Windows on its own
// thread and interrupts the capture (`Voice::interrupt`).

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WIN32_ERROR, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::Threading::{OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowLongPtrW, KillTimer,
    PostQuitMessage, RegisterClassW, SetTimer, SetWindowLongPtrW, TranslateMessage, UnregisterClassW,
    GWLP_USERDATA, MSG, PBT_APMSUSPEND, WINDOW_EX_STYLE, WM_CLOSE, WM_POWERBROADCAST, WM_TIMER, WM_WTSSESSION_CHANGE,
    WNDCLASSW, WS_OVERLAPPED, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF,
};

use super::Voice;

// WinError.h: keep this one error code without enabling the entire Rpc API.
const RPC_S_INVALID_BINDING: WIN32_ERROR = WIN32_ERROR(1702);
const REGISTRATION_TIMER: usize = 1;

#[derive(Debug, PartialEq, Eq)]
enum Registration {
    Waiting,
    Registered,
    Unavailable,
}

impl Registration {
    fn attempt(result: windows::core::Result<()>, log: fn(String)) -> Self {
        match result {
            Ok(()) => Self::Registered,
            Err(err) if err.code() == RPC_S_INVALID_BINDING.to_hresult() => Self::Waiting,
            Err(err) => {
                log(format!("voice: session notifications unavailable ({err}); sleep watcher remains active"));
                Self::Unavailable
            }
        }
    }

    fn retry(&mut self, ready: bool, register: impl FnOnce() -> windows::core::Result<()>, log: fn(String)) {
        if *self == Self::Waiting && ready {
            *self = Self::attempt(register(), log);
        }
    }
}

/// WTS can be unavailable during logon until Remote Desktop services start:
/// https://learn.microsoft.com/windows/win32/api/wtsapi32/nf-wtsapi32-wtsregistersessionnotification
/// Never block the window thread on this event: it must still handle suspend.
fn term_srv_ready() -> bool {
    unsafe {
        match OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Global\\TermSrvReadyEvent")) {
            Ok(event) => {
                let ready = WaitForSingleObject(event, 0) == WAIT_OBJECT_0;
                let _ = CloseHandle(event);
                ready
            }
            // The event may not exist yet (or be inaccessible). A timed WTS
            // retry still allows recovery and avoids a tight retry loop.
            Err(_) => true,
        }
    }
}

/// Which notices interrupt voice, and how the log names them.
pub fn interruption(msg: u32, wparam: usize) -> Option<&'static str> {
    match (msg, wparam as u32) {
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some("screen locked"),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOGOFF) => Some("logging off"),
        (WM_WTSSESSION_CHANGE, WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) => Some("session disconnected"),
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some("going to sleep"),
        _ => None,
    }
}

/// This process's own window class, so that nothing (a test, another
/// Coucou) finds another process's window by its name.
fn class_name() -> HSTRING {
    HSTRING::from(format!("CoucouSessionWatch{}", std::process::id()))
}

/// Starts the watcher for the app's lifetime. Without it (it could not be
/// set up) voice still works; the log says so.
pub fn watch(voice: Voice, log: fn(String)) {
    let spawned = std::thread::Builder::new().name("coucou-session".into()).spawn(move || {
        if let Err(err) = run(
            voice,
            log,
            |hwnd| unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) },
            term_srv_ready,
        ) {
            log(format!("voice: lock/sleep watcher unavailable ({err})"));
        }
    });
    if spawned.is_err() {
        log("voice: lock/sleep watcher unavailable".into());
    }
}

fn run(
    voice: Voice,
    log: fn(String),
    mut register: impl FnMut(HWND) -> windows::core::Result<()>,
    ready: impl Fn() -> bool,
) -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let name = class_name();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: PCWSTR(name.as_ptr()), ..Default::default() };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        // A top-level window, never shown: a message-only one would not get
        // the power broadcast.
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            &name,
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = UnregisterClassW(&name, Some(instance));
                return Err(err);
            }
        };
        // Keep this allocation alive until the message loop and window end.
        let voice = Box::new(voice);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&*voice as *const Voice) as isize);
        let mut registration = Registration::attempt(register(hwnd), log);
        if registration == Registration::Waiting {
            if SetTimer(Some(hwnd), REGISTRATION_TIMER, 1000, None) == 0 {
                log(format!(
                    "voice: session notification retry unavailable ({}); sleep watcher remains active",
                    windows::core::Error::from_win32(),
                ));
                registration = Registration::Unavailable;
            } else {
                log("voice: waiting for session notification services; sleep watcher remains active".into());
            }
        }
        let mut msg = MSG::default();
        let result = loop {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                -1 => break Err(windows::core::Error::from_win32()),
                0 => break Ok(()),
                _ => {}
            }
            if msg.hwnd == hwnd && msg.message == WM_TIMER && msg.wParam.0 == REGISTRATION_TIMER {
                if registration == Registration::Waiting {
                    registration.retry(ready(), || register(hwnd), log);
                }
                if registration != Registration::Waiting {
                    let _ = KillTimer(Some(hwnd), REGISTRATION_TIMER);
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        };
        if registration == Registration::Registered {
            let _ = WTSUnRegisterSessionNotification(hwnd);
        }
        let _ = KillTimer(Some(hwnd), REGISTRATION_TIMER);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(&name, Some(instance));
        result
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        // Let the owning thread unregister notifications before destruction.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    if let Some(why) = interruption(msg, wparam.0) {
        let voice = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Voice;
        if !voice.is_null() {
            unsafe { &*voice }.interrupt(why);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_registration_waits_for_readiness_and_then_recovers() {
        let mut registration = Registration::attempt(
            Err(windows::core::Error::from(RPC_S_INVALID_BINDING)),
            |_| {},
        );
        assert_eq!(registration, Registration::Waiting);
        registration.retry(false, || panic!("must wait for TermSrvReadyEvent"), |_| {});
        assert_eq!(registration, Registration::Waiting);
        registration.retry(true, || Err(windows::core::Error::from(RPC_S_INVALID_BINDING)), |_| {});
        assert_eq!(registration, Registration::Waiting, "another startup failure remains retryable");
        registration.retry(true, || Ok(()), |_| {});
        assert_eq!(registration, Registration::Registered);
        registration.retry(true, || panic!("already registered"), |_| {});
    }

    #[test]
    fn permanent_registration_failure_stops_retrying() {
        use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
        let mut registration = Registration::attempt(
            Err(windows::core::Error::from(ERROR_ACCESS_DENIED)),
            |_| {},
        );
        assert_eq!(registration, Registration::Unavailable);
        registration.retry(true, || panic!("must not retry a permanent error"), |_| {});

        let mut registration = Registration::Waiting;
        registration.retry(true, || Err(windows::core::Error::from(ERROR_ACCESS_DENIED)), |_| {});
        assert_eq!(registration, Registration::Unavailable);
        registration.retry(true, || panic!("must stop after a failed retry"), |_| {});
    }

    #[test]
    fn away_notices_interrupt_and_others_do_not() {
        assert_eq!(interruption(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize), Some("screen locked"));
        assert_eq!(interruption(WM_WTSSESSION_CHANGE, WTS_SESSION_LOGOFF as usize), Some("logging off"));
        assert_eq!(interruption(WM_WTSSESSION_CHANGE, WTS_CONSOLE_DISCONNECT as usize), Some("session disconnected"));
        assert_eq!(interruption(WM_WTSSESSION_CHANGE, WTS_REMOTE_DISCONNECT as usize), Some("session disconnected"));
        assert_eq!(interruption(WM_POWERBROADCAST, PBT_APMSUSPEND as usize), Some("going to sleep"));
        // Unlocking, resuming or anything else changes nothing.
        assert_eq!(interruption(WM_WTSSESSION_CHANGE, 8), None, "unlock");
        assert_eq!(interruption(WM_POWERBROADCAST, 18), None, "resume");
        assert_eq!(interruption(0x0010, 0), None);
    }

    /// Real hidden windows and message pumps, with only service startup
    /// simulated. No actual session lock, sleep, microphone or model is used.
    #[test]
    fn the_hidden_window_handles_power_while_waiting_or_unavailable_and_lock_after_recovery() {
        use super::super::simulated::{Chunk, Microphone, Transcriber};
        use super::super::{Sink, VoiceEvent, VoiceState};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc, Mutex};
        use std::time::Duration;
        use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

        struct Events(Mutex<mpsc::Sender<VoiceEvent>>);
        impl Sink for Events {
            fn emit(&self, event: &VoiceEvent) {
                let _ = self.0.lock().unwrap().send(event.clone());
            }
        }
        for first_error in [RPC_S_INVALID_BINDING, ERROR_ACCESS_DENIED] {
            let (tx, rx) = mpsc::channel();
            let mic = Microphone::new(1000, (0..300).map(|_| Chunk::Tone(10, 0.3)).collect());
            let voice = Voice::new(Events(Mutex::new(tx)), mic, Transcriber, |_| {});
            let ready = Arc::new(AtomicBool::new(false));
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let watch_voice = voice.clone();
            let watch_ready = ready.clone();
            let watcher = std::thread::spawn(move || {
                let mut attempt = 0;
                let result = run(
                    watch_voice,
                    |_| {},
                    |hwnd| {
                        attempt += 1;
                        attempt_tx.send((hwnd.0 as usize, attempt)).unwrap();
                        if attempt == 1 { Err(windows::core::Error::from(first_error)) } else { Ok(()) }
                    },
                    || watch_ready.load(Ordering::SeqCst),
                );
                let _ = done_tx.send(result);
            });
            let (raw_hwnd, attempt) = attempt_rx.recv_timeout(Duration::from_secs(5)).expect("first registration");
            assert_eq!(attempt, 1);
            let hwnd = HWND(raw_hwnd as *mut _);
            let next = |state| loop {
                let event: VoiceEvent = rx.recv_timeout(Duration::from_secs(5)).expect("voice event");
                if event.state == state {
                    break event;
                }
            };
            voice.toggle().unwrap();
            next(VoiceState::Listening);
            unsafe { PostMessageW(Some(hwnd), WM_POWERBROADCAST, WPARAM(PBT_APMSUSPEND as usize), LPARAM(0)) }.unwrap();
            next(VoiceState::Idle);

            if first_error == RPC_S_INVALID_BINDING {
                ready.store(true, Ordering::SeqCst);
                // The real one-second timer must wake the pump and retry.
                let (_, attempt) = attempt_rx.recv_timeout(Duration::from_secs(5)).expect("registration retry");
                assert_eq!(attempt, 2);
                voice.toggle().unwrap();
                next(VoiceState::Listening);
                unsafe { PostMessageW(Some(hwnd), WM_WTSSESSION_CHANGE, WPARAM(WTS_SESSION_LOCK as usize), LPARAM(0)) }.unwrap();
                next(VoiceState::Idle);
            }
            unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).expect("watcher must stop").expect("watcher cleanup");
            watcher.join().expect("watcher thread");
            assert!(unsafe { FindWindowW(&class_name(), None) }.is_err(), "window must be destroyed");
            // The next iteration also proves the class was unregistered.
        }
    }
}

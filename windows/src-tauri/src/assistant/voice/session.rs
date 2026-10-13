// Voice stops when the user is away: the screen locks, the session is
// disconnected or logged off, or the PC goes to sleep. A hidden window (never
// shown, no taskbar entry) receives those notices from Windows on its own
// thread and interrupts the capture (`Voice::interrupt`).

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, RegisterClassW, SetWindowLongPtrW,
    TranslateMessage, GWLP_USERDATA, MSG, PBT_APMSUSPEND, WINDOW_EX_STYLE, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE,
    WNDCLASSW, WS_OVERLAPPED, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF,
};

use super::Voice;

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
        if let Err(err) = run(voice) {
            log(format!("voice: lock/sleep watcher unavailable ({err})"));
        }
    });
    if spawned.is_err() {
        log("voice: lock/sleep watcher unavailable".into());
    }
}

fn run(voice: Voice) -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let name = class_name();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: PCWSTR(name.as_ptr()), ..Default::default() };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        // A top-level window, never shown: a message-only one would not get
        // the power broadcast.
        let hwnd = CreateWindowExW(
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
        )?;
        // The window owns the Voice for the rest of the process.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(voice)) as isize);
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)?;
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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

    /// The real window on its thread, sent the notice Windows sends on lock.
    #[test]
    fn the_hidden_window_interrupts_a_capture_on_lock() {
        use super::super::simulated::{Chunk, Microphone, Transcriber};
        use super::super::{Sink, VoiceEvent, VoiceState};
        use std::sync::{mpsc, Mutex};
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW};

        struct Events(Mutex<mpsc::Sender<VoiceEvent>>);
        impl Sink for Events {
            fn emit(&self, event: &VoiceEvent) {
                let _ = self.0.lock().unwrap().send(event.clone());
            }
        }
        let (tx, rx) = mpsc::channel();
        let mic = Microphone::new(1000, (0..300).map(|_| Chunk::Tone(10, 0.3)).collect());
        let voice = Voice::new(Events(Mutex::new(tx)), mic, Transcriber, |_| {});
        watch(voice.clone(), |line| panic!("{line}"));

        let since = Instant::now();
        let hwnd = loop {
            match unsafe { FindWindowW(&class_name(), None) } {
                Ok(hwnd) => break hwnd,
                Err(_) if since.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(10)),
                Err(err) => panic!("no watcher window: {err}"),
            }
        };
        voice.toggle().unwrap();
        let next = |state| loop {
            let event: VoiceEvent = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            if event.state == state {
                break event;
            }
        };
        next(VoiceState::Listening);
        unsafe { SendMessageW(hwnd, WM_WTSSESSION_CHANGE, Some(WPARAM(WTS_SESSION_LOCK as usize)), Some(LPARAM(0))) };
        next(VoiceState::Idle);
    }
}

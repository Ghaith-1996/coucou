// What the ChatGPT or Claude desktop app's window shows, read through UI
// Automation, for the chat to summarize when the user asks
// (assistant/tools.rs: read_chat_window). Their conversations are kept by
// their services, not in files here (sessions.rs), so the open window is
// the only local source.
//
// Only those two apps, recognised by their installed executable, never
// another window and never Coucou. It is the accessible text the app has
// loaded, not a screenshot and not the whole history: a long chat shows
// what the app keeps on screen. Text fields (the unsent draft) and password
// fields are skipped. Never called unless the user turned on "Let the chat
// read my conversations" in Settings.
//
// COM runs multithreaded on a worker thread of its own, as in browser.rs.
// Both apps draw their chat with Chromium, which builds its accessibility
// tree only once something asks for it: the reader asks each page surface
// (`Chrome_RenderWidgetHostHWND`) for its accessible object first, then
// reads, and reads again shortly after while the tree is still filling in.

use serde::Serialize;

/// What the chat gets of one window, in characters (the end is kept).
pub const MAX_CHARS: usize = 20_000;
/// At most this many elements are visited.
pub const MAX_NODES: usize = 20_000;
/// And for at most this long.
pub const MAX_MILLIS: u64 = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ChatApp {
    #[serde(rename = "ChatGPT")]
    ChatGpt,
    #[serde(rename = "Claude")]
    Claude,
}

impl ChatApp {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "chatgpt" => Some(ChatApp::ChatGpt),
            "claude" => Some(ChatApp::Claude),
            _ => None,
        }
    }
}

/// The desktop chat app an executable belongs to, by the exact folders its
/// installers use: the Store packages (`Program Files\WindowsApps\
/// OpenAI.ChatGPT-Desktop_…\app\ChatGPT….exe`, `…\WindowsApps\Claude_…\app\
/// claude.exe`) or Claude's own installer (`AppData\Local\AnthropicClaude\
/// app-…\claude.exe`). Claude Code's `claude.exe` lives elsewhere and has no
/// window of its own.
pub fn chat_app_of(exe_path: &str) -> Option<ChatApp> {
    let path = exe_path.replace('/', "\\").to_lowercase();
    let mut parts = path.rsplit('\\');
    let file = parts.next()?;
    // Nearest folder first: [0] holds the file, [1] holds [0], and so on.
    let parents: Vec<&str> = parts.collect();
    let at = |i: usize| parents.get(i).copied().unwrap_or_default();
    // Store apps live in Program Files\WindowsApps, or in X:\WindowsApps when installed on another drive.
    let apps_root = at(3) == "program files" || (parents.len() == 4 && at(3).ends_with(':'));
    let store = |package: &str| at(0) == "app" && at(1).starts_with(package) && at(2) == "windowsapps" && apps_root;
    if file.starts_with("chatgpt") && file.ends_with(".exe") && store("openai.chatgpt-desktop_") {
        return Some(ChatApp::ChatGpt);
    }
    let own_installer = at(0).starts_with("app-") && at(1) == "anthropicclaude" && at(2) == "local" && at(3) == "appdata";
    if file == "claude.exe" && (store("claude_") || own_installer) {
        return Some(ChatApp::Claude);
    }
    None
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowText {
    pub app: ChatApp,
    /// The window's title.
    pub title: String,
    /// The text, one element per line, in the window's order.
    pub text: String,
    /// The start was cut to fit, or the walk stopped at its limits.
    pub shortened: bool,
}

/// Lines in reading order, consecutive repeats dropped (a group is often
/// named after the text inside it), the end kept within `max` characters:
/// the line that doesn't fit whole keeps its own end ("…" first).
pub fn join_lines(lines: &[String], max: usize, stopped_early: bool) -> (String, bool) {
    let mut kept: Vec<&str> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || kept.last() == Some(&line) {
            continue;
        }
        kept.push(line);
    }
    let mut out: Vec<String> = Vec::new();
    let mut budget = max;
    let mut cut = false;
    for line in kept.iter().rev() {
        let n = line.chars().count() + 1;
        if n <= budget {
            budget -= n;
            out.push(format!("{line}\n"));
            continue;
        }
        // "…" and "\n" take two characters of what is left.
        if budget > 2 {
            let tail: String = line.chars().skip(n - 1 - (budget - 2)).collect();
            out.push(format!("…{tail}\n"));
        }
        cut = true;
        break;
    }
    out.reverse();
    (out.concat(), cut || stopped_early)
}

#[cfg(windows)]
pub use uia::read;

#[cfg(windows)]
mod uia {
    use super::*;
    use std::time::{Duration, Instant};
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::core::Interface;
    use windows::Win32::UI::Accessibility::{
        AccessibleObjectFromWindow, CUIAutomation8, IAccessible, IUIAutomation2, IUIAutomationElement, IUIAutomationTreeWalker,
        UIA_ComboBoxControlTypeId, UIA_EditControlTypeId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetClassNameW, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER, OBJID_CLIENT,
    };

    /// Asking a Chromium page surface for its accessible object is what turns
    /// its accessibility tree on.
    fn wake_page_surfaces(hwnd: HWND) {
        unsafe extern "system" fn each(child: HWND, data: LPARAM) -> BOOL {
            let found = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
            let mut class = [0u16; 64];
            let n = unsafe { GetClassNameW(child, &mut class) };
            if String::from_utf16_lossy(&class[..n.max(0) as usize]) == "Chrome_RenderWidgetHostHWND" {
                found.push(child);
            }
            BOOL(1)
        }
        let mut surfaces: Vec<HWND> = Vec::new();
        let _ = unsafe { EnumChildWindows(Some(hwnd), Some(each), LPARAM(&mut surfaces as *mut _ as isize)) };
        for surface in surfaces {
            let mut object: *mut core::ffi::c_void = std::ptr::null_mut();
            if unsafe { AccessibleObjectFromWindow(surface, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut object) }.is_ok() && !object.is_null() {
                // Released when dropped.
                drop(unsafe { IAccessible::from_raw(object) });
            }
        }
    }

    /// Fewer words than this: the tree is still being built.
    const FILLED_WORDS: usize = 20;

    fn exe_path(pid: u32) -> Option<String> {
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

    /// The app's visible, unowned top-level windows, front to back.
    fn windows_of(app: ChatApp) -> Vec<HWND> {
        struct Search {
            app: ChatApp,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let search = unsafe { &mut *(data.0 as *mut Search) };
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.is_invalid());
            if visible && !owned {
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
                if pid != std::process::id() && exe_path(pid).as_deref().and_then(chat_app_of) == Some(search.app) {
                    search.found.push(hwnd);
                }
            }
            BOOL(1)
        }
        let mut search = Search { app, found: Vec::new() };
        let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut _ as isize)) };
        search.found
    }

    fn title_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    struct Walk {
        lines: Vec<String>,
        visited: usize,
        deadline: Instant,
        stopped: bool,
    }

    fn walk(walker: &IUIAutomationTreeWalker, el: &IUIAutomationElement, w: &mut Walk) {
        let mut child = unsafe { walker.GetFirstChildElement(el) }.ok();
        while let Some(c) = child {
            w.visited += 1;
            if w.visited > MAX_NODES || Instant::now() > w.deadline {
                w.stopped = true;
                return;
            }
            let kind = unsafe { c.CurrentControlType() }.unwrap_or_default();
            let password = unsafe { c.CurrentIsPassword() }.is_ok_and(|b| b.as_bool());
            // The draft being typed and anything secret: never read.
            if !(password || kind == UIA_EditControlTypeId || kind == UIA_ComboBoxControlTypeId) {
                let has_children = unsafe { walker.GetFirstChildElement(&c) }.is_ok();
                if !has_children {
                    if let Ok(name) = unsafe { c.CurrentName() } {
                        w.lines.push(name.to_string());
                    }
                } else {
                    walk(walker, &c, w);
                }
            }
            child = unsafe { walker.GetNextSiblingElement(&c) }.ok();
        }
    }

    /// The front window of `app`, read. None when it has no open window.
    pub fn read(app: ChatApp) -> Result<Option<WindowText>, String> {
        let Some(hwnd) = windows_of(app).first().copied() else { return Ok(None) };
        let title = title_of(hwnd);
        let hwnd_value = hwnd.0 as isize;
        let (lines, stopped) = std::thread::Builder::new()
            .name("coucou-chat-window".into())
            .spawn(move || -> Result<(Vec<String>, bool), String> {
                if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
                    return Err("COM unavailable".to_string());
                }
                let result = (|| {
                    let ua: IUIAutomation2 =
                        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.message().to_string())?;
                    unsafe {
                        let _ = ua.SetConnectionTimeout(2000);
                        let _ = ua.SetTransactionTimeout(2000);
                    }
                    let hwnd = HWND(hwnd_value as *mut _);
                    let deadline = Instant::now() + Duration::from_millis(MAX_MILLIS);
                    wake_page_surfaces(hwnd);
                    let walker = unsafe { ua.ControlViewWalker() }.map_err(|e| e.message().to_string())?;
                    loop {
                        let root = unsafe { ua.ElementFromHandle(hwnd) }.map_err(|e| e.message().to_string())?;
                        let mut w = Walk { lines: Vec::new(), visited: 0, deadline, stopped: false };
                        walk(&walker, &root, &mut w);
                        let words: usize = w.lines.iter().map(|l| l.split_whitespace().count()).sum();
                        if words >= FILLED_WORDS || w.stopped || Instant::now() + Duration::from_millis(500) > deadline {
                            return Ok((w.lines, w.stopped));
                        }
                        std::thread::sleep(Duration::from_millis(400));
                    }
                })();
                unsafe { CoUninitialize() };
                result
            })
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|_| "window reader failed".to_string())??;
        let (text, shortened) = join_lines(&lines, MAX_CHARS, stopped);
        Ok(Some(WindowText { app, title, text, shortened }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_desktop_chat_apps_are_recognised() {
        assert_eq!(chat_app_of(r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.190.0_x64__2p2nqsd0c76g0\app\ChatGPT Classic.exe"), Some(ChatApp::ChatGpt));
        assert_eq!(chat_app_of(r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2025.1.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe"), Some(ChatApp::ChatGpt));
        assert_eq!(chat_app_of(r"C:\Program Files\WindowsApps\Claude_2.37337.1.0_x64__pzs8sxrjxfjjc\app\Claude.exe"), Some(ChatApp::Claude));
        assert_eq!(chat_app_of(r"C:\Users\me\AppData\Local\AnthropicClaude\app-0.9.3\claude.exe"), Some(ChatApp::Claude));
        assert_eq!(chat_app_of(r"D:\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.190.0_x64__2p2nqsd0c76g0\app\ChatGPT Classic.exe"), Some(ChatApp::ChatGpt), "a Store app moved to another drive");
        for other in [
            r"C:\Users\me\.local\bin\claude.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\claude.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__2p2nqsd0c76g0\app\Codex.exe",
            r"C:\Temp\OpenAI.ChatGPT-Desktop_fake\ChatGPT.txt",
            r"C:\Temp\OpenAI.ChatGPT-Desktop_fake\ChatGPT.exe",
            r"C:\Temp\WindowsApps\OpenAI.ChatGPT-Desktop_fake\app\ChatGPT.exe",
            r"D:\Temp\WindowsApps\Claude_fake\app\claude.exe",
            r"C:\Temp\AnthropicClaude\claude.exe",
            r"C:\Users\me\Downloads\AnthropicClaude\app-1\claude.exe",
            r"C:\Program Files\WindowsApps\Claude_1.0_x64__x\app\tools\claude.exe",
            r"D:\cv\release\coucou.exe",
        ] {
            assert_eq!(chat_app_of(other), None, "{other}");
        }
        assert_eq!(ChatApp::parse("chatgpt"), Some(ChatApp::ChatGpt));
        assert_eq!(ChatApp::parse("notepad"), None);
    }

    #[test]
    fn lines_keep_the_end_and_drop_repeats() {
        let lines: Vec<String> = ["New chat", "", "Hello", "Hello", "  How can I help?  ", "a".repeat(30).as_str()].iter().map(|s| s.to_string()).collect();
        let (all, cut) = join_lines(&lines, 1_000, false);
        assert_eq!(all, format!("New chat\nHello\nHow can I help?\n{}\n", "a".repeat(30)));
        assert!(!cut);
        let (end, cut) = join_lines(&lines, 40, false);
        assert_eq!(end, format!("…I help?\n{}\n", "a".repeat(30)), "the most recent text is at the end");
        assert_eq!(end.chars().count(), 40);
        assert!(cut);
        // The newest element alone is longer than everything allowed: its end stays.
        let long = vec!["old".to_string(), format!("start {}", "x".repeat(100)), "Copy".to_string()];
        let (end, cut) = join_lines(&long, 20, false);
        assert_eq!(end, format!("…{}\nCopy\n", "x".repeat(13)));
        assert!(cut);
        assert!(join_lines(&lines, 1_000, true).1, "a walk that stopped early says so");
    }

    /// The real windows: `cargo test real_chat_window -- --ignored --nocapture`.
    /// Prints only sizes, never the text.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn real_chat_window_on_this_pc() {
        for app in [ChatApp::Claude, ChatApp::ChatGpt] {
            let started = std::time::Instant::now();
            match read(app) {
                Ok(Some(w)) => {
                    let words: usize = w.text.split_whitespace().count();
                    println!("{app:?}: {} lines, {words} words, shortened {} in {:?}", w.text.lines().count(), w.shortened, started.elapsed());
                }
                Ok(None) => println!("{app:?}: no window"),
                Err(e) => println!("{app:?}: {e}"),
            }
        }
    }
}

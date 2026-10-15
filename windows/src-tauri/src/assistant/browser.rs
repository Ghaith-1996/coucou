// The user's browser tabs, through UI Automation: the tab strips of Chrome,
// Edge and Brave windows (Chromium's tab strip is a Tab control of TabItems
// in the browser's own UI). Reading gives each tab's title; switching selects
// one tab (SelectionItemPattern) and brings its window forward. No keystroke,
// no extension, and nothing inside a web page is read: the walk never enters
// a Document, so a page's own tab widgets never count.

use serde::Serialize;

/// The browsers whose tab strips are read, by executable name.
pub const BROWSERS: [(&str, &str); 4] =
    [("chrome.exe", "Chrome"), ("msedge.exe", "Edge"), ("brave.exe", "Brave"), ("vivaldi.exe", "Vivaldi")];
/// At most this many tabs are reported (the model doesn't need more).
pub const MAX_TABS: usize = 60;
/// Titles are cut to this many characters.
pub const MAX_TITLE: usize = 120;
/// The walk stops after this many UI elements per window.
pub const MAX_NODES: usize = 4000;

/// One tab, as the chat's model sees it. `id` is "window.index", stable for
/// as long as the tabs don't move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: String,
    pub browser: String,
    /// 1-based, left to right within its window.
    pub position: usize,
    pub title: String,
    pub active: bool,
    #[serde(skip)]
    pub window: u64,
}

/// The browser's name for an executable path, if it is one we read.
pub fn browser_name(exe_path: &str) -> Option<&'static str> {
    let file = exe_path.rsplit(['\\', '/']).next()?.to_ascii_lowercase();
    BROWSERS.iter().find(|(exe, _)| *exe == file).map(|(_, name)| *name)
}

/// A tab title for the model: one line, trimmed, at most MAX_TITLE characters.
pub fn clean_title(title: &str) -> String {
    let one_line: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_TITLE {
        return one_line;
    }
    let mut cut: String = one_line.chars().take(MAX_TITLE - 1).collect();
    cut.push('…');
    cut
}

/// The tab the model named: by id first, else by its position in the only
/// (or frontmost) window, else by the one title that contains `query`.
pub fn resolve<'a>(tabs: &'a [Tab], id: Option<&str>, position: Option<usize>, query: Option<&str>) -> Option<&'a Tab> {
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        return tabs.iter().find(|t| t.id == id);
    }
    if let Some(position) = position {
        let first_window = tabs.first()?.window;
        return tabs.iter().find(|t| t.window == first_window && t.position == position);
    }
    let query = query?.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    let mut found = tabs.iter().filter(|t| t.title.to_lowercase().contains(&query));
    let first = found.next()?;
    // Ambiguous: let the model ask, rather than guess.
    found.next().is_none().then_some(first)
}

/// Resolve the exact snapshot the user confirmed, before selecting anything.
/// Window-order ids can be reused by a different browser window after a move.
/// A position is not identity when its title is shared inside the same window.
/// Refuse that ambiguity (including titles made equal by truncation).
#[cfg(any(windows, test))]
fn confirmed_tab<'a, T>(tabs: &'a [(Tab, T)], window: u64, id: &str, title: &str) -> Result<&'a (Tab, T), String> {
    let Some(found) = tabs.iter().find(|(tab, _)| tab.id == id) else {
        return Err("the tab is gone".into());
    };
    if found.0.window != window || found.0.title != title {
        return Err("the tabs moved".into());
    }
    if tabs.iter().filter(|(tab, _)| tab.window == window && tab.title == title).count() != 1 {
        return Err("the tab title is ambiguous".into());
    }
    Ok(found)
}

/// Selecting a tab in a background window does not complete a switch.
#[cfg(any(windows, test))]
fn switch_succeeded(selected: bool, window: u64, foreground: u64) -> bool {
    selected && window != 0 && window == foreground
}

#[cfg(windows)]
mod uia {
    use super::*;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation8, IUIAutomation2, IUIAutomationElement, IUIAutomationSelectionItemPattern, IUIAutomationTreeWalker,
        UIA_DocumentControlTypeId, UIA_SelectionItemPatternId, UIA_TabControlTypeId, UIA_TabItemControlTypeId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, GW_OWNER,
        SW_RESTORE,
    };
    use windows::core::PWSTR;

    fn with_uia<T: Send + 'static>(f: impl FnOnce(&IUIAutomation2) -> Result<T, String> + Send + 'static) -> Result<T, String> {
        std::thread::Builder::new()
            .name("coucou-browser".into())
            .spawn(move || {
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
                    f(&ua)
                })();
                unsafe { CoUninitialize() };
                result
            })
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|_| "browser thread failed".to_string())?
    }

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

    /// Visible, unowned top-level windows of a browser we read, front to back.
    fn browser_windows() -> Vec<(HWND, &'static str)> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let found = unsafe { &mut *(data.0 as *mut Vec<(HWND, &'static str)>) };
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.is_invalid());
            if visible && !owned {
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
                if let Some(name) = exe_path(pid).as_deref().and_then(browser_name) {
                    found.push((hwnd, name));
                }
            }
            BOOL(1)
        }
        let mut found: Vec<(HWND, &'static str)> = Vec::new();
        let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize)) };
        found
    }

    /// The TabItems of a window's own tab strip, never entering a Document.
    fn tab_items(ua: &IUIAutomation2, hwnd: HWND) -> Vec<IUIAutomationElement> {
        let Ok(root) = (unsafe { ua.ElementFromHandle(hwnd) }) else { return Vec::new() };
        let Ok(walker) = (unsafe { ua.ControlViewWalker() }) else { return Vec::new() };
        let mut out = Vec::new();
        let mut visited = 0;
        walk(&walker, &root, &mut out, &mut visited, false);
        out
    }

    fn walk(walker: &IUIAutomationTreeWalker, el: &IUIAutomationElement, out: &mut Vec<IUIAutomationElement>, visited: &mut usize, in_strip: bool) {
        let mut child = unsafe { walker.GetFirstChildElement(el) }.ok();
        while let Some(c) = child {
            *visited += 1;
            if *visited > MAX_NODES || out.len() >= MAX_TABS {
                return;
            }
            let kind = unsafe { c.CurrentControlType() }.unwrap_or_default();
            if kind == UIA_DocumentControlTypeId {
                // A web page: never read.
            } else if in_strip && kind == UIA_TabItemControlTypeId {
                out.push(c.clone());
            } else {
                walk(walker, &c, out, visited, in_strip || kind == UIA_TabControlTypeId);
            }
            child = unsafe { walker.GetNextSiblingElement(&c) }.ok();
        }
    }

    fn selected(el: &IUIAutomationElement) -> bool {
        unsafe { el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) }
            .and_then(|p| unsafe { p.CurrentIsSelected() })
            .is_ok_and(|b| b.as_bool())
    }

    fn read(ua: &IUIAutomation2) -> Vec<(Tab, IUIAutomationElement)> {
        let mut tabs = Vec::new();
        for (w, (hwnd, browser)) in browser_windows().into_iter().enumerate() {
            for (i, el) in tab_items(ua, hwnd).into_iter().enumerate() {
                if tabs.len() >= MAX_TABS {
                    return tabs;
                }
                let title = unsafe { el.CurrentName() }.map(|n| clean_title(&n.to_string())).unwrap_or_default();
                let tab = Tab {
                    id: format!("{}.{}", w + 1, i + 1),
                    browser: browser.to_string(),
                    position: i + 1,
                    title,
                    active: selected(&el),
                    window: hwnd.0 as usize as u64,
                };
                tabs.push((tab, el));
            }
        }
        tabs
    }

    pub fn tabs() -> Result<Vec<Tab>, String> {
        with_uia(|ua| Ok(read(ua).into_iter().map(|(t, _)| t).collect()))
    }

    /// Selects the confirmed tab only while its window, id and title match.
    /// Ok(true) requires both selection and the confirmed window in front.
    pub fn switch(window: u64, id: String, title: String) -> Result<bool, String> {
        switch_if(window, id, title, || true)
    }

    /// The chat can be cancelled while this UIA worker starts or reads tabs.
    /// Check its authority at the last boundary before the selection begins.
    pub fn switch_if(window: u64, id: String, title: String, start: impl FnOnce() -> bool + Send + 'static) -> Result<bool, String> {
        with_uia(move |ua| {
            let all = read(ua);
            let (tab, el) = confirmed_tab(&all, window, &id, &title)?;
            let pattern: IUIAutomationSelectionItemPattern = unsafe { el.GetCurrentPatternAs(UIA_SelectionItemPatternId) }
                .map_err(|_| "this tab can't be selected".to_string())?;
            if !start() {
                return Err("The chat was stopped.".into());
            }
            unsafe { pattern.Select() }.map_err(|e| e.message().to_string())?;
            let hwnd = HWND(tab.window as usize as *mut _);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
            let is_selected = selected(el);
            let foreground = unsafe { GetForegroundWindow() }.0 as usize as u64;
            Ok(switch_succeeded(is_selected, tab.window, foreground))
        })
    }
}

#[cfg(windows)]
pub use uia::{switch, switch_if, tabs};

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, window: u64, position: usize, title: &str) -> Tab {
        Tab { id: id.into(), browser: "Chrome".into(), position, title: title.into(), active: false, window }
    }

    #[test]
    fn only_known_browsers_are_read() {
        assert_eq!(browser_name(r"C:\Program Files\Google\Chrome\Application\chrome.exe"), Some("Chrome"));
        assert_eq!(browser_name(r"C:\Program Files (x86)\Microsoft\Edge\Application\MSEDGE.EXE"), Some("Edge"));
        assert_eq!(browser_name(r"C:\Windows\notepad.exe"), None);
        assert_eq!(browser_name(r"C:\evil\chrome.exe.bak"), None);
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(clean_title("  YouTube \n - Music  "), "YouTube - Music");
        let long = "a".repeat(300);
        let cut = clean_title(&long);
        assert_eq!(cut.chars().count(), MAX_TITLE);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_tab_is_found_by_id_position_or_a_unique_title() {
        let tabs = vec![
            tab("1.1", 10, 1, "Inbox - Gmail"),
            tab("1.2", 10, 2, "YouTube"),
            tab("2.1", 20, 1, "GitHub - coucou"),
            tab("2.2", 20, 2, "YouTube Music"),
        ];
        assert_eq!(resolve(&tabs, Some("2.1"), None, None).unwrap().title, "GitHub - coucou");
        assert_eq!(resolve(&tabs, None, Some(2), None).unwrap().id, "1.2", "position in the front window");
        assert_eq!(resolve(&tabs, None, None, Some("gmail")).unwrap().id, "1.1");
        assert!(resolve(&tabs, None, None, Some("youtube")).is_none(), "two matches: ambiguous");
        assert!(resolve(&tabs, Some("9.9"), None, None).is_none());
        assert!(resolve(&tabs, None, Some(7), None).is_none());
        assert!(resolve(&tabs, None, None, Some("  ")).is_none());
    }

    #[test]
    fn a_confirmed_tab_cannot_move_to_a_different_window_with_the_same_title() {
        let reordered = vec![(tab("1.1", 20, 1, "Inbox"), ()), (tab("2.1", 10, 1, "Inbox"), ())];
        assert_eq!(confirmed_tab(&reordered, 10, "1.1", "Inbox"), Err("the tabs moved".into()));
        assert_eq!(confirmed_tab(&reordered, 20, "1.1", "Inbox").unwrap().0.window, 20);
    }

    #[test]
    fn a_confirmed_tab_still_requires_its_original_id_and_title() {
        let tabs = vec![(tab("1.1", 10, 1, "Inbox"), ())];
        assert!(confirmed_tab(&tabs, 10, "1.1", "Inbox").is_ok());
        assert_eq!(confirmed_tab(&tabs, 10, "1.2", "Inbox"), Err("the tab is gone".into()));
        assert_eq!(confirmed_tab(&tabs, 10, "1.1", "New title"), Err("the tabs moved".into()));
    }

    #[test]
    fn same_window_duplicate_titles_cannot_reuse_a_confirmed_position() {
        // The old 1.2 has moved into the confirmed 1.1 position. A matching
        // HWND/id/title is insufficient to tell these two tabs apart.
        let reordered = vec![(tab("1.1", 10, 1, "Inbox"), "other"), (tab("1.2", 10, 2, "Inbox"), "confirmed")];
        assert_eq!(confirmed_tab(&reordered, 10, "1.1", "Inbox"), Err("the tab title is ambiguous".into()));
    }

    #[test]
    fn truncated_titles_are_ambiguous_even_when_original_titles_differ() {
        let prefix = "x".repeat(MAX_TITLE);
        let first = clean_title(&format!("{prefix} one"));
        let second = clean_title(&format!("{prefix} two"));
        assert_eq!(first, second);
        let tabs = vec![(tab("1.1", 10, 1, &second), ()), (tab("1.2", 10, 2, &first), ())];
        assert_eq!(confirmed_tab(&tabs, 10, "1.1", &first), Err("the tab title is ambiguous".into()));
    }

    #[test]
    fn duplicate_titles_in_other_windows_do_not_make_a_unique_target_ambiguous() {
        let tabs = vec![(tab("1.1", 10, 1, "Inbox"), ()), (tab("2.1", 20, 1, "Inbox"), ())];
        assert_eq!(confirmed_tab(&tabs, 10, "1.1", "Inbox").unwrap().0.window, 10);
    }

    #[test]
    fn switching_requires_the_selected_tab_and_its_window_in_front() {
        assert!(switch_succeeded(true, 10, 10));
        assert!(!switch_succeeded(true, 10, 20), "foreground activation was refused");
        assert!(!switch_succeeded(true, 10, 0), "there is no foreground window");
        assert!(!switch_succeeded(false, 10, 10), "foreground alone is not tab selection");
        assert!(!switch_succeeded(true, 0, 0), "a missing window is not success");
    }

    /// Opt-in: counts the tabs of the browsers open now (titles not printed).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn the_open_browser_tabs_are_read() {
        let started = std::time::Instant::now();
        let tabs = tabs().unwrap();
        let windows: std::collections::BTreeSet<u64> = tabs.iter().map(|t| t.window).collect();
        println!(
            "{} tab(s) in {} window(s), {} active, in {} ms",
            tabs.len(),
            windows.len(),
            tabs.iter().filter(|t| t.active).count(),
            started.elapsed().as_millis()
        );
    }
}

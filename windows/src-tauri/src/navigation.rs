// Where a Coucou webview may navigate: its own bundled pages, nowhere else.
//
// Every window loads index.html, settings.html or mochi.html from the app
// bundle, and every outside link goes through `open_url` to the default
// browser. Nothing in the app navigates a webview anywhere else, so a
// navigation that leaves the bundle can only be a bug or something injected
// into a page (a link in a rendered chat reply, a diff, a pill subtitle).
// It is refused here, for every webview, before WebView2 or WebKitGTK loads it.
//
// This is also why `additionalBrowserArgs` can keep wry's default
// `--disable-features=…,msSmartScreenProtection` (lib.rs, BROWSER_ARGS):
// SmartScreen in a webview only checks the pages it navigates to, and these
// webviews never navigate to a page from the network. Turning it on would send
// the address of every bundled page to Microsoft for nothing. The default
// browser, which is where links actually open, keeps its own SmartScreen.

#[cfg(dev)]
use tauri::Manager;
use tauri::{Runtime, Url};

/// True when `url` is one of the app's own pages: Tauri's asset protocol
/// (`tauri://localhost` on Linux, `http(s)://tauri.localhost` on Windows),
/// `about:blank`, or — in a dev build only — the Vite dev server's origin.
pub fn allowed(url: &Url, dev_origin: Option<&Url>) -> bool {
    if url.as_str() == "about:blank" {
        return true;
    }
    let host = url.host_str().unwrap_or_default();
    match url.scheme() {
        "tauri" => host == "localhost" && url.port().is_none(),
        "http" | "https" => {
            matches!(url.origin().ascii_serialization().as_str(),
                "http://tauri.localhost" | "https://tauri.localhost")
                || dev_origin.is_some_and(|dev| dev.origin() == url.origin())
        }
        _ => false,
    }
}

/// A plugin that holds every webview to `allowed`. Refusals are logged by
/// scheme and host only: a full address could carry a token.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            #[cfg(dev)]
            let dev = webview.app_handle().config().build.dev_url.clone();
            #[cfg(not(dev))]
            let dev: Option<Url> = {
                let _ = webview;
                None
            };
            let ok = allowed(url, dev.as_ref());
            if !ok {
                crate::log::line(format!(
                    "blocked a navigation to {}://{}",
                    url.scheme(),
                    url.host_str().unwrap_or("")
                ));
            }
            ok
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn the_bundled_pages_are_allowed() {
        for page in [
            "http://tauri.localhost/index.html",
            "http://tauri.localhost:80/index.html",
            "https://tauri.localhost:443/settings.html",
            "https://tauri.localhost/settings.html",
            "http://tauri.localhost/mochi.html#x",
            "tauri://localhost/index.html",
            "about:blank",
        ] {
            assert!(allowed(&url(page), None), "{page}");
        }
    }

    #[test]
    fn anything_else_is_refused() {
        for page in [
            "https://example.com/",
            "http://localhost:1420/",
            "http://tauri.localhost.evil.com/",
            "http://tauri.localhost:1420/",
            "https://tauri.localhost:8443/",
            "http://tauri.localhost:443/",
            "https://tauri.localhost:80/",
            "tauri://localhost:1420/",
            "http://evil.com/?tauri.localhost",
            "https://user:pass@example.com/tauri.localhost",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,hi",
            "tauri://evil/",
            "ms-settings:privacy",
        ] {
            assert!(!allowed(&url(page), None), "{page}");
        }
    }

    #[test]
    fn the_dev_server_is_allowed_only_by_its_own_origin() {
        let dev = url("http://localhost:1420");
        assert!(allowed(&url("http://localhost:1420/settings.html"), Some(&dev)));
        assert!(!allowed(&url("http://localhost:1421/"), Some(&dev)));
        assert!(!allowed(&url("https://localhost:1420/"), Some(&dev)));
        assert!(!allowed(&url("https://example.com/"), Some(&dev)));
    }
}

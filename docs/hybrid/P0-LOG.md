# Hybrid assistant — P0 engineering log

Running log for phase P0 of the hybrid assistant work (agent supervision kept as is,
desktop assistant features added on top). Every entry says what changed, what was
actually run and what came out of it. This is the agent's own review, not an
independent one.

Status: **compiled** = it builds · **unit-tested** = automated tests pass ·
**run on Windows** = checked in the running app on a real Windows machine.

## Baseline

| | |
|---|---|
| Base commit | `e1b2cfd` (Windows and Linux 0.4.0). Newest `main` commit with a green Windows CI run. `windows/` is identical to the audited commit `71b773f` (`git diff e1b2cfd 71b773f -- windows/` is empty). |
| Fork / branch | `Ghaith-1996/coucou`, PRs target `hybrid/main` |
| Machine | Windows 11 Home 10.0.26300, x86_64 |
| Toolchain | rustc/cargo 1.99.0 stable-x86_64-pc-windows-msvc · VS 2022 Community + MSVC x64 tools + Windows 11 SDK 22621 · WebView2 154.0.4258.62 · Node 22.18.0 / npm 10.9.3 (CI uses Node 24; its own comment requires ≥ 22.18) |

Commands run in `windows/` before any change, all green:

| Command | Result |
|---|---|
| `npm ci` | ok, 0 vulnerabilities |
| `npx tsc --noEmit` | ok, no errors |
| `npm test` | 397 pass, 0 fail |
| `cargo build --release -p coucou-hook` | ok |
| `cargo test --workspace --locked` | app 412 pass (2 ignored), hook 30 pass |

Side effect of the tests, already present upstream: they write to the real
`%LOCALAPPDATA%\Coucou` folder (`coucou.log`, `inbox/`).

## P0.1 — WebView2 browser arguments and navigation

**What the audit flagged.** `additionalBrowserArgs` includes
`--disable-features=…,msSmartScreenProtection`.

**What the source shows.** That string is wry's own default. wry 0.57,
`src/webview2/mod.rs`, uses `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`
when an app passes no arguments, and adds `--autoplay-policy=no-user-gesture-required`
when autoplay is on. Passing any arguments replaces that default, so Coucou repeats
it word for word to keep autoplay for Mochi's sounds. Every Tauri 2 app on Windows
runs with SmartScreen off inside its webviews.

**Analysis.** WebView2's SmartScreen checks the pages a webview navigates to.
Coucou's webviews only ever load bundled pages (`index.html`, `settings.html`,
`mochi.html`). Outside links already go to the default browser through `open_url`
(`http`/`https` only), and that browser keeps its own SmartScreen. The two
`WebviewUrl::External` cases are dev-only and point at the Vite dev server.
Turning SmartScreen on would send the address of every bundled page to Microsoft
and protect nothing. That clashes with the repo rule "no network calls except to
services the user configured".

**Decision: keep the flag, and enforce what makes it safe.**

- `src-tauri/src/navigation.rs` (new) is a plugin that refuses any navigation
  outside the app's own pages, for every webview: Tauri's asset origin, `about:blank`,
  and the dev server's exact origin in dev builds only. It refuses `file:`, `data:`,
  `javascript:`, look-alike hosts and everything else. A refusal is logged with
  scheme and host only.
- `lib.rs` tests pin `BROWSER_ARGS` to exactly *wry default + autoplay*, and check
  that both `tauri.conf.json` and `tauri.linux.conf.json` ask for the same string.
  Nothing else can be switched off quietly, and the "second window comes up blank"
  mismatch is caught in CI.

Status: unit-tested; run on Windows (dev and release builds, see below).

## P0.2 — Agent permissions: characterization tests

No logic changed in `pipe.rs`. Tests were added that pin the guarantees its header
states, using tokio's paused clock (dev-dependency feature `test-util` only, so
`Cargo.lock` is unchanged and `--locked` passes):

- no ack within 800 ms → no decision (the terminal takes over)
- ack, then nobody answers → no decision after `DECISION_TIMEOUT`
- decline, before or after the ack → never an approval
- island gone (channel closed) → never an approval
- only an explicit `allow`/`deny` decides; a click that beats the ack still counts
- a click that arrives after the timeout is ignored
- `DECISION_TIMEOUT` (108 s) < the relay's `DECISION_BUDGET` (110 s, `hook/src/main.rs`)
- the log never contains a question's answer

Status: unit-tested.

### Commands after P0.1 + P0.2

| Command | Result |
|---|---|
| `cargo test --workspace --locked` | app **425** pass (412 + 13 new), hook 30 pass |
| `npm run tauri dev` + `coucou --shortcut openChat` | island renders, chat opens; no "blocked a navigation" line in `coucou.log` |
| `npx tauri build --no-bundle`, then run the release exe + `--shortcut openChat` | builds; island renders from `http://tauri.localhost`, chat opens; settings and desktop windows (created hidden at launch) load; no "blocked a navigation" line |

## Open points

- `npm test` and the Rust tests write to the real `%LOCALAPPDATA%\Coucou` (upstream behaviour, left as is).
- New-window requests (Ctrl/middle-click on a link) aren't covered by the navigation guard. Links already open through `open_url`. To check in P0 UI work.

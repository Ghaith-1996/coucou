# Hybrid assistant — where things stand (handoff)

Last updated 2026-10-13. Read this first to pick the work up.

## Goal and rules

Turn Coucou's Windows app (`windows/`, Tauri 2 + Rust + TypeScript) into an
agent supervisor plus a desktop assistant (Hitodama-style). The plan is the
audit `Audit-Coucou-Hitodama-Windows-agent-unique.md`, phases P0–P4.

- **Windows only.** No Linux or Mac work. The Linux CI is disabled on the fork,
  and the Mac code (`NotchBuddy/`) is only a reference to port from.
- **Never** let voice, an LLM, a routine or web content approve an agent's
  permission. AgentPermission and AssistantAction stay separate (a test
  enforces it). A timeout or no answer is never approval.
- Each feature: test, then push, then open a PR on the fork
  `Ghaith-1996/coucou`, never upstream. English docs and logs.
- Speech stays local and free (whisper.cpp).

## Build and run

From any terminal:

```
windows\scripts\build-exe.cmd gpu   # → D:\cv\release\coucou.exe (standalone, GPU)
windows\scripts\dev.cmd gpu         # dev mode (keep its console open)
windows\scripts\cargo-vs.cmd test --workspace --locked   # Rust tests
cd windows && npx tsc --noEmit && npm test               # JS tests
```

- Quit Coucou first (tray → Quit). Only one instance runs at a time, and the
  exe is locked while it runs.
- GPU builds need the Vulkan SDK (`C:\VulkanSDK`) and a short target folder
  (`D:\cv`). `vs-env.cmd` sets both up.
- The owner's exe is built from a local branch, `local/all`: the open PR
  branches merged together (today `hybrid/chatgpt-plan` + `hybrid/p2-dictation`).
  Expect trivial conflicts in `src/i18n/extra.json` and `src/core/bridge.ts`
  imports: keep both sides.
- Logs: `%LOCALAPPDATA%\Coucou\coucou.log`. Settings:
  `%APPDATA%\Coucou\settings.json`.

## PRs (base `hybrid/main`)

| PR | Branch | State |
|---|---|---|
| #1–#4, #9 | security baseline, contracts, voice, whisper, receipts | merged |
| #5 | `hybrid/p1-voice-commands` | open (stack start, on `hybrid/main`) |
| #6 | `hybrid/p1-model-choice` | open, on #5 |
| #7 | `hybrid/p1-vulkan-experiment` | open, on #6 |
| #10 | `hybrid/p1-interruptions` | open, on #7 |
| #12 | `hybrid/p2-launch-app` | open, on #10 |
| #11 | `hybrid/island-move-hide` | open, on `hybrid/main` (independent) |

- Merge in order: #5 → #6 → #7 → #10 → #12. #11 can go in at any time.
- #11 and the stack both append to the end of `windows/src/i18n/extra.json`.
  Whichever merges second has a conflict there: keep both blocks.
- The owner's reviewer sometimes pushes fixes onto PR branches. Run
  `git pull --ff-only` before working on a branch, then merge `hybrid/main`
  down the stack. Merge, never rebase.

## Done

| Phase | What | Docs |
|---|---|---|
| P0 | WebView2 args review, navigation guard, permission safety tests, closed action registry + policy + grants + receipts, voice capture (WASAPI) + preview + cancel | `P0-*.md` |
| P1 | whisper.cpp local speech, model choice (base / large-v3-turbo q5_0 / large-v3 q5_0), opt-in Vulkan GPU (~0.4 s per clip on an RX 7800 XT), Mac pill commands port, cancel aborts Whisper, voice stops on lock/sleep, first baseline | `P1-*.md` |
| UI | Island draggable anywhere (docks back near the top centre), Hide button (an agent's permission request brings it back) | `ISLAND-MOVE-HIDE.md` |
| UI | Microphone button beside the chat Send arrow; the shortcut is optional, with the same voice preview and confirmation flow | `CHAT-VOICE-BUTTON.md` |
| P2 (1/3) | "open <app>" by voice: Windows' All apps list, a confirmation bound by a grant, success only with a window as evidence | `P2-LAUNCH-APP.md` |
| P2 (2/3) | Dictation into another app's field (`insert-text`, PR open): the field is remembered when the capture starts, re-checked before writing (focus, password, read-only), written with UI Automation at the caret, read back as evidence; never keystrokes, never Enter | `P2-DICTATION.md` |
| P2 (3/3) | Ask about a screen region (`ask-region`, PR open, stacked on dictation): one overlay per monitor, one GDI copy of the picked rectangle without Coucou's windows, a preview with black-out masks and the destination, attached to the chat only on the click | `P2-ASK-REGION.md` |

## Next (in order)

1. **P3:** editable memory, durable routines (time zone, DST, late policy),
   tasks, opt-in encrypted clipboard history, with migrations, export and
   delete.
2. **P4:** accessibility, packaging (bundle `vulkan-1.dll`, release workflow),
   original name, character, icons and sounds before any distribution,
   user guide.

Optional, offered to the owner: show "Graphics card (Vulkan)" or "Processor" in
Settings → Voice.

## Not verified yet

- Ask about a screen region by hand in the running app, on one and on two
  monitors with different scales.
- Dictation by voice into a real app in the running Coucou (Notepad, a
  browser field). Each step is tested, including a live UI Automation write
  into a separate process's text box.
- A spoken command through the live microphone, end to end in the running app
  ("open <app>", pill commands). Each step is tested separately, and the owner
  has used dictation to chat live.
- An actual screen lock or sleep. Dragging between monitors with different
  scales.
- Startup time, and memory before a model loads.

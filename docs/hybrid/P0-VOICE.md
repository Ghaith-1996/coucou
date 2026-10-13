# Hybrid assistant — P0 voice capture slice

Shortcut → microphone with a visible indicator → short capture → **simulated**
transcript → preview on the island → Use (fills Coucou's chat field, unsent) or
Cancel. No global desktop control. The transcription is simulated by design
(audit P0); a real local engine is P1.

## Flow

```
Ctrl+Alt+V (off by default)                    Rust: assistant/voice
   │  island checks: agent card waiting? ──yes──► opens that card instead, mic stays closed
   ▼ no
assistant_voice_toggle ──► WASAPI capture thread (mic open, island pinned)
   │  assistant-voice {listening, level, elapsedMs} ~10/s ──► card + level meter
   ▼  2nd press / Stop / 30 s limit
mic closed ──► {processing} ──► simulated transcriber ──► audio dropped
   ▼
{preview, transcript, simulated:true} ──► card: [Use in chat ↵] [Cancel Esc]
   ▼ Use
assistant_voice_use ──► AssistantAction chat-draft ──► policy.decide = allow
   ▼
island fills chat field (not sent) ──► receipt succeeded, evidence "chat-field:draft-set"
```

## Rules held, and where

| Rule | Where | Checked by |
|---|---|---|
| Mic open only between press and stop/cancel/30 s limit | `voice/mod.rs` `run_capture` | Rust tests (`open_now` counter); live: Windows' taskbar mic icon shows while listening and is gone after Stop |
| Mic closed before transcribing | `drop(capture)` before `Processing` | `press_speak_press…` asserts `open_now == 0` at processing |
| Audio only in memory, dropped after transcription or cancel | `drop(samples)` | code review; nothing writes audio anywhere |
| Transcripts never logged | log lines carry ids and durations only | Rust test scans the log for the transcript text |
| Agent's waiting card comes first | `voice.ts` `voiceShortcut` + `guard` | JS tests: shortcut opens the card, no toggle; card arriving while listening → cancel; no `approval_*` command ever sent |
| Mic only while its card is on screen | `voice.ts` `guard` | JS tests: collapse → cancel |
| Auto-close never cuts a capture short | card pins the island, gives the pin back after | JS tests; live: 20 s capture and 18 s preview survived the 15 s auto-close (this bug was **found by the live test** and fixed) |
| Using text is a policy-decided action | `use_transcript` → `policy::decide` | Rust + JS tests |
| Success needs evidence | receipt `succeeded` only if the field holds the text | JS tests (`failed/unverified` otherwise) |
| Page-sent receipt can't shape the log | id must be `act-` + 16 hex | Rust test |
| Stale events/clicks ignored | capture ids everywhere | Rust + JS tests |

## Choices

- **Shortcut `voiceCapture` = Ctrl+Alt+V, off by default.** It opens the microphone, so it is opt-in (Settings → Shortcuts lists it automatically). V types nothing with AltGr on the six layouts the repo checks, and installed layouts are checked at registration. It goes after the Mac's ten ids, which keep their order. Tests now say "Mac ids, then hybrid ids".
- **WASAPI through the existing `windows` crate**: four more feature flags, no new crate, `Cargo.lock` unchanged. Shared mode, mix format (float32 / PCM16 / PCM32), mixed down to mono. COM is set up on the capture thread and torn down after the COM objects (drop order).
- **Linux**: no microphone adapter yet, so a capture reports "Microphone unavailable".
- **Strings**: 7 new strings, translated into the 9 languages in `extra.json`. Mac strings reused where they exist ("Listening…", "Cancel", "Close", "Microphone unavailable", "Use in chat").
- **No existing view restyled.** New `voice` view and one new CSS block. `card`/`btn`/`stack` in `views.ts` are now exported, unchanged. `chat.ts` gains `applyChatDraft`, and its focus keeps the caret after a draft instead of selecting it.

## Verification (Windows 11, this machine)

| Check | Result |
|---|---|
| `cargo test --workspace --locked` | app **436 pass** (423 + 13 voice), hook 30, no warnings |
| `npx tsc --noEmit` | clean |
| `npm test` | **415 pass** (402 + 13 island voice) |
| Live, dev build, real mic (Realtek / USB) | shortcut → listening card with live level → stop → simulated preview (`3.0 s`, `20.0 s`) → Enter → chat field holds the text, unsent; log: `chat draft act-… allowed`, `assistant action act-… succeeded`, no transcript text |
| Live, Windows taskbar | microphone-in-use icon present while listening, absent 3 s after Stop |
| Not verified live | permission card arriving mid-capture (JS-tested), no-mic and denied-mic paths (Rust-tested with the simulated mic), Linux |

Windows' `CapabilityAccessManager` usage record wasn't written for the unpackaged dev
exe, so the taskbar icon was used as the OS-level evidence instead.

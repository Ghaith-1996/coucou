# Hybrid assistant — P1: interruptions and a first baseline

## Cancel stops the transcription too

Before, Cancel (or Escape, or an agent's card taking the island) during
"Transcribing…" put the island back to idle at once and dropped the result. But
whisper.cpp went on working until the end: about 20 s of CPU for a large model on
the CPU build.

Now the transcriber gets an abort flag (`Transcriber::transcribe(…, abort)`).
`Voice::cancel` sets it while processing. `whisper.rs` hands it to whisper.cpp as
its abort callback. That goes through whisper-rs's raw setter, because 0.16's
`set_abort_callback_safe` passes whisper.cpp a pointer of another type than its
trampoline reads.

**Limit:** whisper.cpp checks the flag between passes, not inside one. Its
scheduler doesn't pass the callback down to the backend. So the encoder pass
under way finishes first:

| Real model, real clip (opt-in test) | Full | Cancelled at | Returned at |
|---|---|---|---|
| base, CPU | 1.18 s | 0.24 s | 0.97 s, no text |
| large-v3 turbo q5_0, CPU | 26.6 s | 5.3 s | 22.8 s, no text (the decoding was skipped) |

On the GPU build a whole transcription takes about 0.4 s, so this doesn't matter
there. Cutting a pass short would mean patching the vendored whisper.cpp, which
isn't worth it for the CPU fallback.

## Voice stops when the user is away

`assistant/voice/session.rs` creates a hidden window that is never shown. On its
own thread it receives Windows' session and power notices:

| Notice | Effect |
|---|---|
| Screen locked (`WTS_SESSION_LOCK`) | a capture listening is cancelled and its audio dropped; a transcription stops |
| Logging off, console or remote session disconnected | same |
| Going to sleep (`PBT_APMSUSPEND`) | same |
| Unlock, resume, anything else | nothing |

A transcript already waiting on the island stays until it expires (2 min). It is
never used by itself. The log says `voice: interrupted (screen locked)`, never
what was said. If the window can't be created, voice still works and the log says
so.

The window class name carries the process id, so a test (or anything else) never
finds another Coucou's window by name.

## A first baseline (measured, nothing optimised yet)

Windows 11 Home 10.0.26300, Ryzen 5 7600 (12 threads), RX 7800 XT, release
build with Vulkan (`build-exe.cmd gpu`), large-v3 turbo q5_0 picked.

| Measure | Result | How |
|---|---|---|
| Idle CPU (app + its 9 WebView2 processes) | 0.094 CPU-s in 20 s = **0.47 % of one core**, 0.04 % of the machine | `Get-Process` CPU time, before and after 20 s |
| Memory, model loaded after a capture | coucou.exe private 716 MB, working set 157 MB; WebView2 private 379 MB in all | `Get-Process` |
| End of speech → transcript | turbo: ~0.4 s on the GPU, ~22 s on the CPU; base: ~0.1 s / ~1.1 s | opt-in real-speech test, 6 clips (P1-VULKAN.md) |
| Cancel → transcription stopped | see the table above | opt-in abort test |
| Cancel → island idle | at once (same lock, no wait) | unit test |
| Real use | the owner's own captures (11.9 s and 1.8 s) went to the chat as drafts; no errors in the log | `coucou.log` |

From now on the log has `voice: transcribed in N ms` after each transcription
(never the text), so real use gives numbers too.

**Not measured yet:** startup time, and memory before any model is loaded. Both
need Coucou closed and started fresh, and the owner's instance was in use.
Storage growth: the only thing voice stores is the downloaded models (sizes in
P1-MODELS.md). Audio and transcripts are never written.

The loaded model stays in memory until another one is picked. Freeing it after a
while unused is a possible next step, but only once the reload time is measured
against the memory it saves.

## Verification

| Check | Result |
|---|---|
| `scripts\cargo-vs.cmd test --workspace --locked` | **451 + 30 pass** (4 opt-in ignored); `npm test` 593 pass |
| New unit tests | cancel while transcribing stops the transcriber; lock while listening closes the mic; sleep while transcribing stops it; idle ignores it; notice mapping |
| Win32 wiring | real hidden window on its thread, sent `WM_WTSSESSION_CHANGE`/`WTS_SESSION_LOCK` → capture back to idle |
| Real Whisper abort | opt-in test, base and turbo (table above) |
| **Not verified** | an actual lock or sleep of this PC (it was in use). The notice Windows sends is the one the test sends, but its delivery wasn't observed. |

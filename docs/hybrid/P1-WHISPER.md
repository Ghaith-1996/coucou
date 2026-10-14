# Hybrid assistant — P1: local speech recognition (Whisper)

The voice capture from P0 now transcribes **on this computer** with whisper.cpp,
once the user downloads its model from Settings → Voice. It's free and needs no
key or account. No audio or text leaves the machine. Until the model is there, the
island keeps showing the labelled simulated transcript.

## Pieces

| File | Role |
|---|---|
| `assistant/voice/whisper.rs` | `Local` transcriber: resamples to 16 kHz mono, loads the model once (CPU, half the cores, max 8), detects the language among **French and English only** (whisper's language probabilities narrowed to `fr`/`en`, French on a tie; owner's choice, 2026-10-14), fresh whisper state per capture, drops non-speech markers (`[BLANK_AUDIO]`, `(music)`). whisper.cpp's own logging is silenced so decoded text never reaches stderr. |
| `assistant/voice/model.rs` | The model: `ggml-base.bin` (multilingual, 147,951,465 bytes, SHA-256 `60ed5bc3…2efe`, pinned) from `huggingface.co/ggerganov/whisper.cpp`. Streamed to `<file>.part`, size-capped while receiving, size + SHA-256 checked, then renamed into place. A mismatch, error or cancel deletes the partial file. One download at a time. |
| `assistant/voice/mod.rs` `Auto` | Whisper when the model is installed, simulated otherwise. A deleted model is also unloaded from memory. |
| `assistant/app.rs` | `voice_model_status / download / cancel / delete`. Download, cancel and delete are refused unless they come from the **settings window**. Progress goes out as `voice-model` events, about one per MB. |
| `src/settings/voice.ts` | Settings → Voice: explanation, model name and size, exact source URL, Download / progress + Cancel / Delete, and errors. |
| `scripts/cargo-vs.cmd` | Runs cargo inside the VS developer environment (found with `vswhere`, any edition). CI's Rust test step uses it. |

## Build requirements (new)

whisper-rs builds whisper.cpp from vendored source and generates its FFI bindings.
The bindings the crate ships are **Linux-only**: their layout asserts fail on Windows,
where `long` is 4 bytes. So on Windows:

- VS 2022 (any edition, or the Build Tools) with **Desktop development with C++**, **C++ CMake tools for Windows** and **C++ Clang Compiler for Windows** (for libclang).
- Build cargo inside the developer environment: `scripts\cargo-vs.cmd …`. For `npm run tauri dev/build`, start from a "x64 Native Tools" prompt, or set `LIBCLANG_PATH` and run `vcvars64.bat` first.

New crates in `Cargo.lock`: whisper-rs, whisper-rs-sys (Unlicense), and their build
tooling: bindgen, clang-sys, cmake, cexpr, nom, minimal-lexical, prettyplease,
shlex, itertools, either, libloading, fs_extra. Only whisper-rs/whisper-rs-sys and
whisper.cpp (MIT) end up in the binary. It's a Windows-only dependency, so Linux keeps
the simulated transcriber.

The release workflow (`windows.yml`, `tauri build`) needs the same environment.
That isn't changed here; it belongs to the P4 packaging work.

## Build flags (`windows/.cargo/config.toml`)

whisper-rs-sys passes `GGML_*`/`CMAKE_*` environment variables to CMake. Two
problems found by measuring, fixed there:

- **No optimisation.** The cmake crate left MSVC's Release flags as
  `-nologo -MD -Brepro -W0`, with no `/O`. A 5 s clip took **11.4 s**. With
  `CMAKE_{C,CXX}_FLAGS_RELEASE = /O2 /Ob2 /DNDEBUG` it takes **1.1 s**.
- **Not portable.** `GGML_NATIVE` defaulted to ON: built on this Ryzen it would use
  AVX-512 and crash on CPUs without it. Now `GGML_NATIVE=OFF` + AVX/AVX2/FMA/F16C,
  which covers every x86-64 PC since about 2013.

The build script doesn't watch these variables. After changing them, run
`scripts\cargo-vs.cmd clean -p whisper-rs-sys` (and with `--release`).

Whisper also gets the app's vocabulary as context ("Mochi, Coucou, Claude Code…,
pull request, commit, branch"). Without it, "Mochi" came out as "Machi".

## Verification

| Check | Result |
|---|---|
| `scripts\cargo-vs.cmd test --workspace --locked` | app **443 pass**, 3 ignored (1 is the opt-in real-speech test), hook 30, no warnings |
| `npx tsc --noEmit` / `npm test` | clean / 415 pass |
| Model download, live, from the Settings button (clicked via UI Automation) | `voice: model download started` → `voice: model installed` in **4 s**; file 147,951,465 bytes; `Get-FileHash` SHA-256 = the pinned `60ed5bc3…2efe` |
| Real speech, opt-in test, release build, model loaded from disk | Two clips spoken by Windows' offline TTS at 48 kHz (so through the resampler): 15.6 s (David) → **0% WER, 1.32 s** including model load; 5.1 s (Zira) → **0% WER, 1.10 s**. Before the build-flag fix: 12.1 s / 11.4 s. |
| Live, dev build | Settings → Voice before the download: red dot, URL, Download button (screenshot). After it, "installed" is confirmed by the log and the file hash only; the section wasn't screenshotted again. |
| **Not verified** | A live microphone capture transcribed by Whisper end to end. Each part is verified separately: the mic capture in P0, Whisper on recorded speech here, and `Auto` switching by unit test. I didn't record the room while the owner was using the PC. |
| Not verified | CI on the fork (Actions off) |

Opt-in real-speech test: put `name.wav` (PCM16) + `name.wav.txt` (expected text)
in a folder, then
`set COUCOU_SPEECH_DIR=<folder>` and `scripts\cargo-vs.cmd test --release -p coucou real_speech -- --ignored --nocapture`.

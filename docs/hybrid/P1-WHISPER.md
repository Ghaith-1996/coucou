# Hybrid assistant — P1: local speech recognition (Whisper)

The voice capture from P0 now transcribes **on this computer** with whisper.cpp,
once the user downloads its model from Settings → Voice. It's free and needs no
key or account. No audio or text leaves the machine. Until the model is there, the
island keeps showing the labelled simulated transcript.

## Pieces

| File | Role |
|---|---|
| `assistant/voice/whisper.rs` | `Local` transcriber: resamples to 16 kHz mono, loads the model once (CPU, half the cores, max 8), detects the language (`auto`, so the app's 10 languages work), fresh whisper state per capture, drops non-speech markers (`[BLANK_AUDIO]`, `(music)`). whisper.cpp's own logging is silenced so decoded text never reaches stderr. |
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

## Verification

| Check | Result |
|---|---|
| `scripts\cargo-vs.cmd test --workspace --locked` | app **442 pass** (436 + 6: model pin/verify, resampling, marker cleanup), hook 30, no warnings |
| `npx tsc --noEmit` / `npm test` | clean / 415 pass |
| Live, dev build | Settings → Voice renders: red status dot (no model), `ggml-base.bin · 148 MB`, source URL, "Download the model (148 MB)". The new shortcut row "Talk to Mochi … Ctrl+Alt+V" is off by default. |
| **Not verified** | **Real transcription and the actual download.** Downloading the 148 MB model needs the user's explicit OK, which hasn't been given yet. Until then, Whisper has only been compiled, not run. |
| Not verified | CI on the fork (GitHub Actions is off on the fork), so the `cargo-vs.cmd` step has only run locally. |

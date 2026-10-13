# Hybrid assistant — P1: choosing the speech model

Settings → Voice now lists three whisper.cpp models. Each is downloaded with
its own button (pinned size + SHA-256, as before), can be deleted, and one
installed model is **in use**. The choice is stored in `settings.json`
(`voiceModel`), owned by Rust: only Settings → Voice changes it, and only to an
installed model. The first model downloaded is picked automatically. Default:
`base`.

| id | File | Size | SHA-256 (pinned) |
|---|---|---|---|
| `base` | ggml-base.bin | 147,951,465 | `60ed5bc3…2efe` |
| `large-v3-turbo-q5_0` | ggml-large-v3-turbo-q5_0.bin | 574,041,195 | `39422170…a7e2` |
| `large-v3-q5_0` | ggml-large-v3-q5_0.bin | 1,081,140,203 | `d75795ec…8ad1` |

## Measured (this machine, CPU only)

Ryzen 5 7600 (6 cores), release build, 6 threads, AVX2 build. Six clips spoken
by Windows' offline TTS at 48 kHz: two dictation sentences and four pill commands.

| Model | Word errors over the 6 clips | Time per clip |
|---|---|---|
| base | "Vercel" heard as "Versal" twice (the parser's fuzzy match recovers both) | **~1.1 s** |
| large-v3-turbo q5_0 | **none** | ~22 s |
| large-v3 q5_0 | "Mochi" heard as "Machi" once | ~27 s |

The time is nearly constant per clip, because Whisper always encodes a 30 s window.
The large models' encoder is about 20x base's compute, which matches the measured
~21x. That's how fast this CPU is, not a build problem (the `/O2` and AVX2 fixes
from #4 are in).

**Rejected: `audio_ctx`.** Shrinking the encoder window to the clip's length made
turbo only ~1.6x faster (13–18 s) and caused repetition hallucinations: one
sentence came back three times (208% WER). Not used.

**Conclusion.** On a CPU, base is the usable default and the large models are for
accuracy over speed. The fix for the large models is the GPU (Vulkan), tried
next, separately. Settings says so: turbo is "much slower without a graphics card".

## Verification

| Check | Result |
|---|---|
| Live, dev build, buttons clicked via UI Automation | Settings → Voice listed base (installed, in use) and the two large models (Download). "Download (574 MB)" → `model large-v3-turbo-q5_0 installed` in 11 s; "Use this model" → `picked`, `settings.json` `voiceModel = large-v3-turbo-q5_0`; "Download (1.08 GB)" → installed in 22 s; afterwards turbo shows "In use", the others "Use this model" + Delete |
| `Get-FileHash` on both downloads | equal to the pinned SHA-256 |
| `scripts\cargo-vs.cmd test --workspace --locked` | 446 pass (model catalog pins, unknown choice → default, settings round trip with `voiceModel`) |
| `npm test` | 593 pass |
| Not verified | a live microphone capture transcribed by the large models (same reason as before) |

Open: cancelling during "Transcribing…" discards the result, but whisper keeps
computing until it finishes. That matters with ~20 s large-model runs on CPU and
should be wired to whisper's abort callback.

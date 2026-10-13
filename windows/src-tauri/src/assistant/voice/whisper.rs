// Speech to text on this computer: whisper.cpp (MIT) through whisper-rs,
// with the model the user downloaded (model.rs). No network, no key; the
// audio and the text never leave the process.
//
// The model loads on the first transcription and stays loaded. Each capture
// gets its own whisper state, so nothing of one capture carries into the next.

use std::path::PathBuf;
use std::sync::Mutex;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{Transcript, VoiceError};

/// The app's own names, given to Whisper as context so it spells them right.
const VOCABULARY: &str = "Mochi, Coucou, Claude Code, Codex, Gemini, Cursor, GitHub, pull request, commit, branch.";

/// What whisper.cpp wants: 16 kHz mono.
pub const WHISPER_RATE: u32 = 16_000;

pub struct Local {
    model: PathBuf,
    ctx: Mutex<Option<WhisperContext>>,
}

impl Local {
    pub fn new(model: PathBuf) -> Self {
        // whisper.cpp and ggml would print to stderr, model details and the
        // decoded text included. Silenced, once per process.
        whisper_rs::install_logging_hooks();
        Self { model, ctx: Mutex::new(None) }
    }

    /// Forgets the loaded model (it was deleted, or replaced).
    pub fn unload(&self) {
        *self.ctx.lock().unwrap() = None;
    }
}

impl super::Transcriber for Local {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<Transcript, VoiceError> {
        let audio = resample(samples, sample_rate, WHISPER_RATE);
        let mut guard = self.ctx.lock().unwrap();
        if guard.is_none() {
            let params = WhisperContextParameters { use_gpu: false, ..Default::default() };
            let ctx = WhisperContext::new_with_params(&self.model, params).map_err(|_| VoiceError::Provider)?;
            *guard = Some(ctx);
        }
        let ctx = guard.as_ref().ok_or(VoiceError::Provider)?;
        let mut state = ctx.create_state().map_err(|_| VoiceError::Provider)?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // The language is detected from the audio: Coucou speaks ten.
        params.set_language(Some("auto"));
        // Names Whisper would otherwise spell like common words ("Machi").
        params.set_initial_prompt(VOCABULARY);
        params.set_n_threads(threads());
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state.full(params, &audio).map_err(|_| VoiceError::Provider)?;

        let segments: Vec<String> = state
            .as_iter()
            .filter_map(|segment| segment.to_str_lossy().ok().map(|s| s.into_owned()))
            .collect();
        Ok(Transcript { text: clean(&segments), simulated: false })
    }
}

/// Half the cores, at least one and at most eight: fast, without taking the
/// whole machine away from what the user is doing.
fn threads() -> i32 {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (cores / 2).clamp(1, 8) as i32
}

/// The segments as one line, without whisper's non-speech markers
/// (`[BLANK_AUDIO]`, `(music)`, `[Applause]`…).
pub fn clean(segments: &[String]) -> String {
    let words: Vec<&str> = segments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| !(s.starts_with('[') && s.ends_with(']')) && !(s.starts_with('(') && s.ends_with(')')))
        .collect();
    words.join(" ")
}

/// Mono samples at `from` Hz → `to` Hz. An integer ratio is averaged over each
/// block (a box filter, enough against aliasing for speech); anything else is
/// interpolated linearly.
pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    if from > to && from % to == 0 {
        let factor = (from / to) as usize;
        return samples.chunks(factor).map(|block| block.iter().sum::<f32>() / block.len() as f32).collect();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let left = pos.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let frac = (pos - left as f64) as f32;
            samples[left] * (1.0 - frac) + samples[right] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_duration() {
        for from in [44_100u32, 48_000, 32_000, 22_050, 16_000, 8_000] {
            let one_second = vec![0.25f32; from as usize];
            let out = resample(&one_second, from, WHISPER_RATE);
            let diff = (out.len() as i64 - WHISPER_RATE as i64).abs();
            assert!(diff <= 1, "{from} Hz → {} samples", out.len());
            assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-6), "{from} Hz keeps the level");
        }
    }

    #[test]
    fn an_integer_ratio_averages_each_block() {
        assert_eq!(resample(&[0.0, 0.3, 0.6, 1.0, 1.0, 1.0], 48_000, 16_000), vec![0.3, 1.0]);
    }

    /// PCM16 WAV → mono f32 and its rate. Enough for the test clips.
    fn read_wav(bytes: &[u8]) -> (Vec<f32>, u32) {
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        assert_eq!(&bytes[0..4], b"RIFF");
        let (mut channels, mut rate, mut at) = (1usize, 16_000u32, 12usize);
        while at + 8 <= bytes.len() {
            let (id, len) = (&bytes[at..at + 4], u32_at(at + 4) as usize);
            if id == b"fmt " {
                assert_eq!(u16_at(at + 8), 1, "PCM only");
                channels = u16_at(at + 10) as usize;
                rate = u32_at(at + 12);
                assert_eq!(u16_at(at + 22), 16, "16-bit only");
            } else if id == b"data" {
                let data = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
                let frames: Vec<f32> = data
                    .chunks_exact(2 * channels)
                    .map(|f| {
                        f.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0).sum::<f32>()
                            / channels as f32
                    })
                    .collect();
                return (frames, rate);
            }
            at += 8 + len + (len & 1);
        }
        panic!("no data chunk");
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// Word error rate: word-level edit distance over the reference length.
    fn wer(reference: &str, heard: &str) -> f64 {
        let (r, h) = (words(reference), words(heard));
        let mut row: Vec<usize> = (0..=h.len()).collect();
        for i in 1..=r.len() {
            let mut prev = row[0];
            row[0] = i;
            for j in 1..=h.len() {
                let cur = row[j];
                row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(r[i - 1] != h[j - 1]));
                prev = cur;
            }
        }
        row[h.len()] as f64 / r.len().max(1) as f64
    }

    #[test]
    fn word_error_rate_counts_edits() {
        assert_eq!(wer("open the chat", "Open the chat."), 0.0);
        assert!((wer("open the chat now", "open a chat now") - 0.25).abs() < 1e-9);
    }

    /// The real model on real speech. Needs the downloaded model and a folder
    /// of `<name>.wav` clips with their text in `<name>.wav.txt`:
    ///   set COUCOU_SPEECH_DIR=…  then
    ///   scripts\cargo-vs.cmd test --release -p coucou real_speech -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_speech_is_transcribed_on_this_computer() {
        use super::super::Transcriber;
        let dir = std::env::var("COUCOU_SPEECH_DIR").expect("COUCOU_SPEECH_DIR");
        assert!(super::super::model::installed(), "download the model from Settings → Voice first");
        let local = Local::new(super::super::model::path());
        let mut clips: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        clips.sort();
        assert!(!clips.is_empty());
        for (n, clip) in clips.iter().enumerate() {
            let expected = std::fs::read_to_string(clip.with_extension("wav.txt")).unwrap();
            let (samples, rate) = read_wav(&std::fs::read(clip).unwrap());
            let seconds = samples.len() as f64 / rate as f64;
            let started = std::time::Instant::now();
            let transcript = local.transcribe(&samples, rate).unwrap();
            let took = started.elapsed().as_secs_f64();
            let error = wer(&expected, &transcript.text);
            println!(
                "{}: {seconds:.1} s of audio at {rate} Hz, transcribed in {took:.2} s{} — WER {:.0}%\n  heard: {}",
                clip.file_name().unwrap().to_string_lossy(),
                if n == 0 { " (includes loading the model)" } else { "" },
                error * 100.0,
                transcript.text
            );
            assert!(!transcript.simulated);
            assert!(error <= 0.15, "too many errors");
        }
    }

    #[test]
    fn non_speech_markers_are_dropped() {
        let segments = [" Hello Mochi,", "[BLANK_AUDIO]", " (music) ", " open the chat.", "  "]
            .map(str::to_string);
        assert_eq!(clean(&segments), "Hello Mochi, open the chat.");
        assert_eq!(clean(&["[BLANK_AUDIO]".to_string()]), "");
    }
}

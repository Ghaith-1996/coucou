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

    #[test]
    fn non_speech_markers_are_dropped() {
        let segments = [" Hello Mochi,", "[BLANK_AUDIO]", " (music) ", " open the chat.", "  "]
            .map(str::to_string);
        assert_eq!(clean(&segments), "Hello Mochi, open the chat.");
        assert_eq!(clean(&["[BLANK_AUDIO]".to_string()]), "");
    }
}

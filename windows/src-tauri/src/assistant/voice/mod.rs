// Voice capture (hybrid P0): a shortcut opens the microphone, the island shows
// that it is open, a second press stops it, the audio becomes a transcript and
// the transcript waits on the island to be used or cancelled.
//
// Rules this holds to, from the audit and the Mac's docs/VOICE.md:
//   * The microphone is open only between a press and the stop, cancel or the
//     30 s limit. Nothing listens in the background.
//   * Audio stays in memory and is dropped as soon as it is transcribed or the
//     capture is cancelled. It is never written to disk.
//   * Transcripts are never logged; the log says what happened, not what was said.
//   * Using a transcript is an assistant action (`chat-draft`) decided by the
//     policy: it fills Coucou's own chat field and is never sent.
//
// Everything that touches the OS sits behind `Microphone` and `Transcriber`,
// so the state machine is tested here with simulated ones; the real microphone
// is WASAPI on Windows (wasapi.rs). P0 transcribes with `simulated::Transcriber`
// only: a real local engine comes in P1.

pub mod simulated;
#[cfg(windows)]
pub mod wasapi;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::contracts::*;
use super::policy;

/// Longest capture; it stops by itself there.
pub const MAX_CAPTURE: Duration = Duration::from_secs(30);
/// Shorter than this, nothing was really said.
pub const MIN_CAPTURE: Duration = Duration::from_millis(250);
/// How often the level goes to the island while listening.
pub const LEVEL_EVERY: Duration = Duration::from_millis(100);
/// How long a transcript waits on the island to be used.
pub const PREVIEW_TTL: Duration = Duration::from_secs(120);
/// An RMS below this over every 50 ms window is silence.
pub const SILENCE_RMS: f32 = 0.004;
/// How long the chat draft action stays valid once the user pressed Use.
pub const DRAFT_TTL_MS: u64 = 30_000;

/// Where voice events go: the island in the app, a recorder in tests.
pub trait Sink: Send + Sync + 'static {
    fn emit(&self, event: &VoiceEvent);
}

/// The audio input. `open` runs on the capture thread, which is the only
/// thread that ever touches the returned capture.
pub trait Microphone: Send + Sync + 'static {
    fn open(&self) -> Result<Box<dyn Capture>, VoiceError>;
}

pub trait Capture {
    fn sample_rate(&self) -> u32;
    /// Waits a little (tens of milliseconds) and appends what arrived, as
    /// mono samples in -1…1.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), VoiceError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub simulated: bool,
}

pub trait Transcriber: Send + Sync + 'static {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<Transcript, VoiceError>;
}

const RUN: u8 = 0;
const STOP: u8 = 1;
const CANCEL: u8 = 2;

enum Phase {
    Idle,
    Listening { id: String, signal: Arc<AtomicU8> },
    Processing { id: String },
    Preview { id: String, transcript: Transcript, since: Instant },
    Error { id: String },
}

impl Phase {
    fn id(&self) -> Option<&str> {
        match self {
            Phase::Idle => None,
            Phase::Listening { id, .. } | Phase::Processing { id } | Phase::Preview { id, .. } | Phase::Error { id } => {
                Some(id)
            }
        }
    }
}

/// What "Use" hands the island: the action Rust allowed and its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDraft {
    pub action_id: String,
    pub text: String,
}

struct Inner {
    sink: Box<dyn Sink>,
    mic: Box<dyn Microphone>,
    stt: Box<dyn Transcriber>,
    phase: Mutex<Phase>,
    log: fn(String),
}

/// The voice capture state machine. Cheap to clone; one per app.
#[derive(Clone)]
pub struct Voice(Arc<Inner>);

impl Voice {
    pub fn new(sink: impl Sink, mic: impl Microphone, stt: impl Transcriber, log: fn(String)) -> Self {
        Voice(Arc::new(Inner {
            sink: Box::new(sink),
            mic: Box::new(mic),
            stt: Box::new(stt),
            phase: Mutex::new(Phase::Idle),
            log,
        }))
    }

    /// The shortcut or the mic button: starts a capture, or stops the one
    /// that is listening. Returns the capture id it started or stopped.
    pub fn toggle(&self) -> Option<String> {
        let mut phase = self.0.phase.lock().unwrap();
        match &*phase {
            Phase::Listening { id, signal } => {
                signal.store(STOP, Ordering::SeqCst);
                Some(id.clone())
            }
            // Busy turning the last one into text: a press waits for it.
            Phase::Processing { .. } => None,
            // Idle, or a transcript / error on screen: a fresh capture replaces it.
            _ => {
                let id = policy::new_id("cap");
                let signal = Arc::new(AtomicU8::new(RUN));
                *phase = Phase::Listening { id: id.clone(), signal: signal.clone() };
                drop(phase);
                (self.0.log)("voice: capture started".into());
                let inner = self.0.clone();
                let capture_id = id.clone();
                let spawned = std::thread::Builder::new()
                    .name("coucou-voice".into())
                    .spawn(move || run_capture(inner, capture_id, signal));
                if spawned.is_err() {
                    *self.0.phase.lock().unwrap() = Phase::Idle;
                    (self.0.log)("voice: could not start the capture thread".into());
                    return None;
                }
                Some(id)
            }
        }
    }

    /// The Stop button: like a second press, for this capture only.
    pub fn stop(&self, capture_id: &str) {
        if let Phase::Listening { id, signal } = &*self.0.phase.lock().unwrap() {
            if id == capture_id {
                signal.store(STOP, Ordering::SeqCst);
            }
        }
    }

    /// Cancel, Escape, the card going away, or an agent's card taking the
    /// island: whatever this capture was doing ends, and its audio or
    /// transcript is dropped. Never an action.
    pub fn cancel(&self, capture_id: &str) {
        let mut phase = self.0.phase.lock().unwrap();
        if phase.id() != Some(capture_id) {
            return;
        }
        if let Phase::Listening { signal, .. } = &*phase {
            // The capture thread closes the microphone and says idle.
            signal.store(CANCEL, Ordering::SeqCst);
            return;
        }
        *phase = Phase::Idle;
        drop(phase);
        (self.0.log)("voice: cancelled".into());
        self.emit(VoiceEvent::new(capture_id, VoiceState::Idle));
    }

    /// Use: the transcript becomes a `chat-draft` action, decided by the
    /// policy like any other. The island fills its chat field with the text
    /// and reports back (`receipt`).
    pub fn use_transcript(&self, capture_id: &str) -> Result<ChatDraft, ErrorCode> {
        let mut phase = self.0.phase.lock().unwrap();
        let Phase::Preview { id, transcript, since } = &*phase else {
            return Err(ErrorCode::Expired);
        };
        if id != capture_id {
            return Err(ErrorCode::Expired);
        }
        let stale = since.elapsed() > PREVIEW_TTL;
        let text = transcript.text.clone();
        *phase = Phase::Idle;
        drop(phase);
        self.emit(VoiceEvent::new(capture_id, VoiceState::Idle));
        if stale {
            (self.0.log)("voice: transcript expired before use".into());
            return Err(ErrorCode::Expired);
        }
        let now = policy::now_ms();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: policy::new_id("act"),
            kind: ActionKind::ChatDraft,
            args: serde_json::json!({ "text": text }),
            intent_id: capture_id.to_string(),
            expires_at_ms: now + DRAFT_TTL_MS,
        };
        let verdict = policy::decide(&action, now);
        if verdict.decision != Decision::Allow {
            let code = verdict.error.unwrap_or(ErrorCode::InvalidArgs);
            (self.0.log)(format!("voice: chat draft refused ({})", code_name(code)));
            return Err(code);
        }
        (self.0.log)(format!("voice: chat draft {} allowed", action.id));
        Ok(ChatDraft { action_id: action.id, text })
    }

    /// What the island says came of an action. Logged without any text, and
    /// only for an id of ours: what the page sends never shapes the log.
    pub fn receipt(&self, receipt: &ActionReceipt) {
        let id = &receipt.action_id;
        let ours = id.len() == 20 && id.starts_with("act-") && id[4..].bytes().all(|b| b.is_ascii_hexdigit());
        if !ours {
            return;
        }
        let outcome = serde_json::to_value(receipt.outcome()).ok();
        let outcome = outcome.as_ref().and_then(|v| v.as_str()).unwrap_or("?");
        (self.0.log)(format!("assistant action {} {outcome}", receipt.action_id));
    }

    /// The app is quitting: the microphone closes now.
    pub fn shutdown(&self) {
        if let Phase::Listening { signal, .. } = &*self.0.phase.lock().unwrap() {
            signal.store(CANCEL, Ordering::SeqCst);
        }
    }

    fn emit(&self, event: VoiceEvent) {
        self.0.sink.emit(&event);
    }
}

fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The capture thread: opens the microphone, reads until stopped, cancelled
/// or at the limit, closes it, then transcribes.
fn run_capture(inner: Arc<Inner>, id: String, signal: Arc<AtomicU8>) {
    let emit = |event: VoiceEvent| inner.sink.emit(&event);
    let still_ours = |phase: &Phase| phase.id() == Some(id.as_str());

    let fail = |error: VoiceError| {
        let mut phase = inner.phase.lock().unwrap();
        if !still_ours(&phase) {
            return;
        }
        let cancelled = signal.load(Ordering::SeqCst) == CANCEL;
        *phase = if cancelled { Phase::Idle } else { Phase::Error { id: id.clone() } };
        drop(phase);
        if cancelled {
            emit(VoiceEvent::new(&id, VoiceState::Idle));
        } else {
            (inner.log)(format!("voice: {}", serde_json::to_value(error).unwrap_or_default()));
            emit(VoiceEvent { error: Some(error), ..VoiceEvent::new(&id, VoiceState::Error) });
        }
    };

    let mut capture = match inner.mic.open() {
        Ok(c) => c,
        Err(error) => return fail(error),
    };
    let rate = capture.sample_rate().max(1);
    emit(VoiceEvent { level: Some(0.0), elapsed_ms: Some(0), ..VoiceEvent::new(&id, VoiceState::Listening) });

    let started = Instant::now();
    let mut samples: Vec<f32> = Vec::new();
    let mut last_level = Instant::now();
    let mut level_from = 0usize;
    let max_samples = (MAX_CAPTURE.as_secs_f64() * rate as f64) as usize;
    let outcome = loop {
        match signal.load(Ordering::SeqCst) {
            CANCEL => break Ok(false),
            STOP => break Ok(true),
            _ => {}
        }
        if let Err(error) = capture.read(&mut samples) {
            break Err(error);
        }
        if samples.len() >= max_samples || started.elapsed() >= MAX_CAPTURE {
            samples.truncate(max_samples);
            break Ok(true);
        }
        if last_level.elapsed() >= LEVEL_EVERY {
            let level = (rms(&samples[level_from..]) * 8.0).min(1.0) as f64;
            level_from = samples.len();
            last_level = Instant::now();
            emit(VoiceEvent {
                level: Some((level * 100.0).round() / 100.0),
                elapsed_ms: Some(started.elapsed().as_millis() as u64),
                ..VoiceEvent::new(&id, VoiceState::Listening)
            });
        }
    };
    // The microphone closes here, before anything is transcribed.
    drop(capture);
    let elapsed_ms = (samples.len() as u64 * 1000) / rate as u64;

    let keep = match outcome {
        Err(error) => return fail(error),
        Ok(keep) => keep,
    };
    {
        let mut phase = inner.phase.lock().unwrap();
        if !still_ours(&phase) {
            return;
        }
        if !keep || signal.load(Ordering::SeqCst) == CANCEL {
            *phase = Phase::Idle;
            drop(phase);
            (inner.log)("voice: cancelled".into());
            emit(VoiceEvent::new(&id, VoiceState::Idle));
            return;
        }
        *phase = Phase::Processing { id: id.clone() };
    }
    (inner.log)(format!("voice: captured {elapsed_ms} ms"));
    emit(VoiceEvent { elapsed_ms: Some(elapsed_ms), ..VoiceEvent::new(&id, VoiceState::Processing) });

    let result = if (samples.len() as u64) < (MIN_CAPTURE.as_millis() as u64 * rate as u64 / 1000)
        || is_silent(&samples, rate)
    {
        Err(VoiceError::Silence)
    } else {
        inner.stt.transcribe(&samples, rate)
    };
    // The audio goes as soon as it has been heard.
    drop(samples);

    let mut phase = inner.phase.lock().unwrap();
    if !still_ours(&phase) {
        return;
    }
    match result {
        Ok(transcript) if !transcript.text.trim().is_empty() => {
            let event = VoiceEvent {
                elapsed_ms: Some(elapsed_ms),
                transcript: Some(transcript.text.clone()),
                simulated: transcript.simulated,
                ..VoiceEvent::new(&id, VoiceState::Preview)
            };
            *phase = Phase::Preview { id: id.clone(), transcript, since: Instant::now() };
            drop(phase);
            emit(event);
        }
        Ok(_) => {
            drop(phase);
            fail(VoiceError::Silence);
        }
        Err(error) => {
            drop(phase);
            fail(error);
        }
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// True when no 50 ms window rises above the silence threshold.
pub fn is_silent(samples: &[f32], rate: u32) -> bool {
    let window = (rate as usize / 20).max(1);
    !samples.chunks(window).any(|w| rms(w) >= SILENCE_RMS)
}

#[cfg(test)]
mod tests;

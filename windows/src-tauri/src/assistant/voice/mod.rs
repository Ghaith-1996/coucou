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
// is WASAPI on Windows (wasapi.rs). Speech becomes text on this computer with
// whisper.cpp (whisper.rs) once the user downloaded its model (model.rs);
// until then the simulated transcriber stands in, labelled as such.

pub mod model;
#[cfg(windows)]
pub mod session;
pub mod simulated;
#[cfg(windows)]
pub mod wasapi;
#[cfg(windows)]
pub mod whisper;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::contracts::*;
use super::dictation::WritePermit;
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
/// How long an action stays valid once the user pressed its button.
pub const DRAFT_TTL_MS: u64 = 30_000;
/// How long an action the card offers for confirmation stays valid: as long
/// as the transcript it came from.
pub const PROPOSAL_TTL_MS: u64 = 120_000;

/// The app's transcriber: whisper.cpp with the picked model once it is
/// installed, the simulated one until then (and again if it is deleted).
pub struct Auto {
    #[cfg(windows)]
    local: whisper::Local,
}

impl Default for Auto {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            local: whisper::Local::default(),
        }
    }
}

impl Transcriber for Auto {
    fn transcribe(&self, samples: &[f32], sample_rate: u32, abort: &AtomicBool) -> Result<Transcript, VoiceError> {
        #[cfg(windows)]
        {
            if let Some(chosen) = model::active() {
                return self.local.transcribe_with(&model::path(chosen), samples, sample_rate, abort);
            }
            // Deleted while loaded: let the memory go too.
            self.local.unload();
        }
        simulated::Transcriber.transcribe(samples, sample_rate, abort)
    }
}

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
    /// Should give up soon after `abort` turns true (the capture was
    /// cancelled while it was being transcribed): its result is not wanted.
    fn transcribe(&self, samples: &[f32], sample_rate: u32, abort: &AtomicBool) -> Result<Transcript, VoiceError>;
}

const RUN: u8 = 0;
const STOP: u8 = 1;
const CANCEL: u8 = 2;

enum Phase {
    Idle,
    Listening { id: String, signal: Arc<AtomicU8> },
    Processing { id: String, abort: Arc<AtomicBool> },
    Preview { id: String, transcript: Transcript, since: Instant },
    /// Owns the preview while one confirmed dictation write is outstanding.
    Inserting { id: String, transcript: Transcript, since: Instant, action_id: String, permit: WritePermit },
    Error { id: String },
}

impl Phase {
    fn id(&self) -> Option<&str> {
        match self {
            Phase::Idle => None,
            Phase::Listening { id, .. } | Phase::Processing { id, .. } | Phase::Preview { id, .. }
            | Phase::Inserting { id, .. } | Phase::Error { id } => {
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

/// An action the card shows for the user to confirm (`launch-app`), and the
/// capture it came from. At most one; Rust keeps it, the page only holds its
/// grant.
struct Proposal {
    capture: String,
    action: AssistantAction,
}

struct Inner {
    sink: Box<dyn Sink>,
    mic: Box<dyn Microphone>,
    stt: Box<dyn Transcriber>,
    phase: Mutex<Phase>,
    proposal: Mutex<Option<Proposal>>,
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
            proposal: Mutex::new(None),
            log,
        }))
    }

    /// The shortcut or the mic button: starts a capture, or stops the one
    /// that is listening. Returns the capture id it started or stopped.
    pub fn toggle(&self) -> Option<String> {
        self.begin(false)
    }

    /// Audio mode: a capture that stops by itself once the user has spoken
    /// and paused (`Endpoint`), or ends with `silence` when nothing is said.
    /// Like `toggle`, a call while one is listening stops it.
    pub fn listen_hands_free(&self) -> Option<String> {
        self.begin(true)
    }

    fn begin(&self, hands_free: bool) -> Option<String> {
        let mut phase = self.0.phase.lock().unwrap();
        match &*phase {
            Phase::Listening { id, signal } => {
                // A pending cancellation is terminal, even before the capture
                // thread has closed the microphone and left Listening.
                let _ = signal.compare_exchange(RUN, STOP, Ordering::SeqCst, Ordering::SeqCst);
                Some(id.clone())
            }
            // Busy turning the last one into text: a press waits for it.
            Phase::Processing { .. } => None,
            // Idle, or a transcript / error on screen: a fresh capture replaces it.
            _ => {
                if let Phase::Inserting { permit, .. } = &*phase {
                    permit.cancelled.store(true, Ordering::SeqCst);
                }
                *self.0.proposal.lock().unwrap() = None;
                let id = policy::new_id("cap");
                let signal = Arc::new(AtomicU8::new(RUN));
                *phase = Phase::Listening { id: id.clone(), signal: signal.clone() };
                drop(phase);
                (self.0.log)("voice: capture started".into());
                let inner = self.0.clone();
                let capture_id = id.clone();
                let spawned = std::thread::Builder::new()
                    .name("coucou-voice".into())
                    .spawn(move || run_capture(inner, capture_id, signal, hands_free));
                if spawned.is_err() {
                    *self.0.phase.lock().unwrap() = Phase::Idle;
                    (self.0.log)("voice: could not start the capture thread".into());
                    return None;
                }
                Some(id)
            }
        }
    }

    /// A new capture replaces its preview. Retire it before awaiting the
    /// field probe, which may itself be queued behind a pending UIA write.
    pub fn cancel_preview(&self) {
        let id = match &*self.0.phase.lock().unwrap() {
            Phase::Preview { id, .. } | Phase::Inserting { id, .. } => Some(id.clone()),
            _ => None,
        };
        if let Some(id) = id { self.cancel(&id); }
    }

    /// The Stop button: like a second press, for this capture only.
    pub fn stop(&self, capture_id: &str) {
        if let Phase::Listening { id, signal } = &*self.0.phase.lock().unwrap() {
            if id == capture_id {
                let _ = signal.compare_exchange(RUN, STOP, Ordering::SeqCst, Ordering::SeqCst);
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
        *self.0.proposal.lock().unwrap() = None;
        if let Phase::Listening { signal, .. } = &*phase {
            // The capture thread closes the microphone and says idle.
            signal.store(CANCEL, Ordering::SeqCst);
            return;
        }
        if let Phase::Processing { abort, .. } = &*phase {
            // The transcription stops too, instead of finishing for nothing.
            abort.store(true, Ordering::SeqCst);
        }
        if let Phase::Inserting { permit, .. } = &*phase {
            permit.cancelled.store(true, Ordering::SeqCst);
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
        let text = self.take_preview(capture_id)?.text;
        let action = self.decided(capture_id, ActionKind::ChatDraft, serde_json::json!({ "text": text }), "chat draft")?;
        Ok(ChatDraft { action_id: action.id, text })
    }

    /// A command the transcript asked for, proposed by the island as an
    /// action for the policy to decide. Only `pill-set` comes this way: a chat
    /// draft is built here from the transcript itself (`use_transcript`).
    /// Either way the transcript is used once, and then it is gone.
    pub fn act(&self, capture_id: &str, kind: ActionKind, args: serde_json::Value) -> Result<String, ErrorCode> {
        if kind != ActionKind::PillSet {
            return Err(ErrorCode::Unavailable);
        }
        self.take_preview(capture_id)?;
        Ok(self.decided(capture_id, kind, args, "pill set")?.id)
    }

    /// An action that needs the user's confirmation (`launch-app` with the
    /// app the card names, `insert-text` with the field it was dictated
    /// for), for this capture's transcript. The policy must say `ask`; Rust
    /// keeps the action and returns its grant, which the card hands back on
    /// the user's click (`confirm`). A newer proposal replaces it. The
    /// transcript stays on the card meanwhile.
    pub fn propose(&self, capture_id: &str, kind: ActionKind, args: serde_json::Value) -> Result<policy::Grant, ErrorCode> {
        let what = match kind {
            ActionKind::LaunchApp => "launch app",
            ActionKind::InsertText => "insert text",
            _ => return Err(ErrorCode::Unavailable),
        };
        let phase = self.0.phase.lock().unwrap();
        let remaining = match &*phase {
            Phase::Preview { id, since, .. } if id == capture_id => PREVIEW_TTL.checked_sub(since.elapsed()),
            _ => None,
        }.ok_or(ErrorCode::Expired)?;
        let now = policy::now_ms();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: policy::new_id("act"),
            kind,
            args,
            intent_id: capture_id.to_string(),
            expires_at_ms: now + (remaining.as_millis() as u64).min(PROPOSAL_TTL_MS),
        };
        let Some(grant) = policy::grant_for(&action, now) else {
            let code = policy::decide(&action, now).error.unwrap_or(ErrorCode::InvalidArgs);
            drop(phase);
            (self.0.log)(format!("voice: {what} refused ({})", code_name(code)));
            return Err(code);
        };
        // Publish before releasing capture ownership, so a newer capture
        // cannot be replaced by this request after its preview check.
        *self.0.proposal.lock().unwrap() = Some(Proposal { capture: capture_id.to_string(), action });
        drop(phase);
        (self.0.log)(format!("voice: {what} {} proposed", grant.action_id));
        Ok(grant)
    }

    /// The user's click on a proposal: its grant must be the one Rust gave
    /// for the action it kept, for this capture, still in time. Everything
    /// is decided again (policy::check_grant). This capture's proposal and
    /// transcript are used once, whatever the answer; a stale capture cannot
    /// consume a newer proposal. Returns the action id and its checked
    /// arguments, for the caller to run.
    pub fn confirm(&self, capture_id: &str, grant: &policy::Grant) -> Result<(String, ValidArgs), ErrorCode> {
        let mut phase = self.0.phase.lock().unwrap();
        let mut proposal = self.0.proposal.lock().unwrap();
        // Keep the phase and proposal under the same ownership check. In
        // particular, another IPC cannot consume an in-flight insertion.
        if proposal.as_ref().is_some_and(|p| p.capture != capture_id) {
            return Err(ErrorCode::GrantMismatch);
        }
        let Phase::Preview { id, since, .. } = &*phase else { return Err(ErrorCode::Expired) };
        if id != capture_id { return Err(ErrorCode::Expired); }
        let expired = since.elapsed() > PREVIEW_TTL;
        let result = match proposal.take() {
            Some(p) if p.action.id == grant.action_id && !expired => {
                policy::check_grant(grant, &p.action, policy::now_ms()).map(|args| (p.action.id, args))
            }
            Some(_) if !expired => Err(ErrorCode::GrantMismatch),
            _ => Err(ErrorCode::Expired),
        };
        *phase = Phase::Idle;
        drop(proposal);
        drop(phase);
        self.emit(VoiceEvent::new(capture_id, VoiceState::Idle));
        match &result {
            Ok((id, _)) => (self.0.log)(format!("voice: {id} confirmed")),
            Err(code) => (self.0.log)(format!("voice: confirmation refused ({})", code_name(*code))),
        }
        result
    }

    /// Reserves exactly this proposal for one write. Unlike a launch, a
    /// definitive refusal before SetValue can return the unchanged preview.
    pub fn begin_insert(&self, capture_id: &str, grant: &policy::Grant) -> Result<(String, InsertTextArgs, WritePermit), ErrorCode> {
        let mut phase = self.0.phase.lock().unwrap();
        let Phase::Preview { id, transcript, since } = &*phase else { return Err(ErrorCode::Expired) };
        if id != capture_id || since.elapsed() >= PREVIEW_TTL { return Err(ErrorCode::Expired); }
        if transcript.simulated { return Err(ErrorCode::Unavailable); }
        let proposal = self.0.proposal.lock().unwrap();
        let p = proposal.as_ref().ok_or(ErrorCode::Expired)?;
        if p.capture != capture_id || p.action.id != grant.action_id { return Err(ErrorCode::GrantMismatch); }
        let now = policy::now_ms();
        let ValidArgs::InsertText(args) = policy::check_grant(grant, &p.action, now)? else {
            return Err(ErrorCode::InvalidArgs);
        };
        let permit = WritePermit {
            cancelled: Arc::new(AtomicBool::new(false)),
            expires: (*since + PREVIEW_TTL).min(Instant::now() + Duration::from_millis(grant.expires_at_ms.saturating_sub(now))),
        };
        let action_id = p.action.id.clone();
        *phase = Phase::Inserting {
            id: capture_id.into(), transcript: transcript.clone(), since: *since,
            action_id: action_id.clone(), permit: permit.clone(),
        };
        Ok((action_id, args, permit))
    }

    /// Only a definitive pre-write refusal keeps the original transcript,
    /// grant and expiry. Cancellation or a newer capture can never revive it.
    pub fn finish_insert(&self, capture_id: &str, action_id: &str, refused: bool) -> Result<(), ErrorCode> {
        let mut phase = self.0.phase.lock().unwrap();
        let Phase::Inserting { id, transcript, since, action_id: pending, permit } = &*phase else {
            return Err(ErrorCode::Cancelled);
        };
        if id != capture_id || pending != action_id { return Err(ErrorCode::Cancelled); }
        let expired = Instant::now() >= permit.expires;
        permit.cancelled.store(true, Ordering::SeqCst);
        if refused && !expired {
            *phase = Phase::Preview { id: id.clone(), transcript: transcript.clone(), since: *since };
            return Ok(());
        }
        *self.0.proposal.lock().unwrap() = None;
        *phase = Phase::Idle;
        drop(phase);
        self.emit(VoiceEvent::new(capture_id, VoiceState::Idle));
        if refused && expired { Err(ErrorCode::Expired) } else { Ok(()) }
    }

    /// The words and original deadline of this capture's preview, left there.
    /// Both are read under the same capture ownership check. Only real speech:
    /// a simulated transcript is never offered to another app.
    pub fn insertion_preview(&self, capture_id: &str) -> Result<(String, Instant), ErrorCode> {
        match &*self.0.phase.lock().unwrap() {
            Phase::Preview { id, transcript, since } if id == capture_id && since.elapsed() < PREVIEW_TTL => {
                if transcript.simulated {
                    Err(ErrorCode::Unavailable)
                } else {
                    Ok((transcript.text.clone(), *since + PREVIEW_TTL))
                }
            }
            _ => Err(ErrorCode::Expired),
        }
    }

    /// Text the user confirmed for another app that could not be put there:
    /// it becomes a `chat-draft` instead, decided by the policy like any
    /// other, so the words are not lost. The transcript is already used.
    pub fn fallback_draft(&self, capture_id: &str, text: String) -> Result<ChatDraft, ErrorCode> {
        let action = self.decided(capture_id, ActionKind::ChatDraft, serde_json::json!({ "text": text }), "chat draft")?;
        Ok(ChatDraft { action_id: action.id, text })
    }

    /// Audio mode: the transcript, taken off the card, to be sent to the
    /// chat as the user's message. Audio mode is the user's explicit choice
    /// to talk to the chat, so no confirmation; it is never an assistant
    /// action and never reaches anything but the chat. A simulated
    /// transcript is never sent.
    pub fn converse(&self, capture_id: &str) -> Result<String, ErrorCode> {
        {
            let phase = self.0.phase.lock().unwrap();
            if let Phase::Preview { id, transcript, .. } = &*phase {
                if id == capture_id && transcript.simulated {
                    return Err(ErrorCode::Unavailable);
                }
            }
        }
        let transcript = self.take_preview(capture_id)?;
        (self.0.log)("voice: transcript sent to the chat (audio mode)".into());
        Ok(transcript.text)
    }

    /// The transcript waiting on the island for this capture, taken off it:
    /// the card goes back to idle whatever comes next.
    fn take_preview(&self, capture_id: &str) -> Result<Transcript, ErrorCode> {
        let mut phase = self.0.phase.lock().unwrap();
        let Phase::Preview { id, transcript, since } = &*phase else {
            return Err(ErrorCode::Expired);
        };
        if id != capture_id {
            return Err(ErrorCode::Expired);
        }
        // Whatever this transcript was offering goes with it. Keep capture
        // ownership until both are consumed, in phase → proposal lock order.
        {
            let mut proposal = self.0.proposal.lock().unwrap();
            if proposal.as_ref().is_some_and(|p| p.capture == capture_id) {
                *proposal = None;
            }
        }
        let stale = since.elapsed() > PREVIEW_TTL;
        let transcript = transcript.clone();
        *phase = Phase::Idle;
        drop(phase);
        self.emit(VoiceEvent::new(capture_id, VoiceState::Idle));
        if stale {
            (self.0.log)("voice: transcript expired before use".into());
            return Err(ErrorCode::Expired);
        }
        Ok(transcript)
    }

    /// An action of this capture, decided by the policy; only an allowed one
    /// comes back. `what` names it in the log (never its content).
    fn decided(&self, capture_id: &str, kind: ActionKind, args: serde_json::Value, what: &str) -> Result<AssistantAction, ErrorCode> {
        let now = policy::now_ms();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: policy::new_id("act"),
            kind,
            args,
            intent_id: capture_id.to_string(),
            expires_at_ms: now + DRAFT_TTL_MS,
        };
        let verdict = policy::decide(&action, now);
        if verdict.decision != Decision::Allow {
            let code = verdict.error.unwrap_or(ErrorCode::InvalidArgs);
            (self.0.log)(format!("voice: {what} refused ({})", code_name(code)));
            return Err(code);
        }
        (self.0.log)(format!("voice: {what} {} allowed", action.id));
        Ok(action)
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

    /// The app is quitting: the microphone closes now, and a transcription
    /// under way stops.
    pub fn shutdown(&self) {
        self.interrupt("app quitting");
    }

    /// The screen locked, the session went away or the PC is going to sleep
    /// (session.rs): a capture listening is cancelled (its audio dropped) and
    /// a transcription under way stops, as with Cancel. A transcript already
    /// waiting stays until it expires. `why` names the event in the log.
    pub fn interrupt(&self, why: &str) {
        let mut phase = self.0.phase.lock().unwrap();
        match &*phase {
            Phase::Listening { signal, .. } => signal.store(CANCEL, Ordering::SeqCst),
            Phase::Inserting { id, permit, .. } => {
                permit.cancelled.store(true, Ordering::SeqCst);
                let id = id.clone();
                *self.0.proposal.lock().unwrap() = None;
                *phase = Phase::Idle;
                drop(phase);
                self.emit(VoiceEvent::new(&id, VoiceState::Idle));
            }
            Phase::Processing { id, abort } => {
                abort.store(true, Ordering::SeqCst);
                let id = id.clone();
                *phase = Phase::Idle;
                drop(phase);
                self.emit(VoiceEvent::new(&id, VoiceState::Idle));
            }
            _ => return,
        }
        (self.0.log)(format!("voice: interrupted ({why})"));
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
fn run_capture(inner: Arc<Inner>, id: String, signal: Arc<AtomicU8>, hands_free: bool) {
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

    let abort = Arc::new(AtomicBool::new(false));
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
    let mut endpoint = hands_free.then(|| Endpoint::new(rate));
    let mut heard_from = 0usize;
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
        if let Some(endpoint) = endpoint.as_mut() {
            let end = endpoint.feed(&samples[heard_from..]);
            heard_from = samples.len();
            match end {
                Some(Heard::Spoke) => break Ok(true),
                Some(Heard::Nothing) => break Err(VoiceError::Silence),
                None => {}
            }
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
        *phase = Phase::Processing { id: id.clone(), abort: abort.clone() };
    }
    (inner.log)(format!("voice: captured {elapsed_ms} ms"));
    emit(VoiceEvent { elapsed_ms: Some(elapsed_ms), ..VoiceEvent::new(&id, VoiceState::Processing) });

    let result = if (samples.len() as u64) < (MIN_CAPTURE.as_millis() as u64 * rate as u64 / 1000)
        || is_silent(&samples, rate)
    {
        Err(VoiceError::Silence)
    } else {
        let started = Instant::now();
        let result = inner.stt.transcribe(&samples, rate, &abort);
        // How long, never what: for the baseline (docs/hybrid/P1-BASELINE.md).
        if result.is_ok() {
            (inner.log)(format!("voice: transcribed in {} ms", started.elapsed().as_millis()));
        }
        result
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

/// What a hands-free capture heard so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Speech, then a pause long enough to be the end of it.
    Spoke,
    /// No speech at all for NOTHING_FOR.
    Nothing,
}

/// A pause this long after speech ends the turn.
pub const PAUSE: Duration = Duration::from_millis(1100);
/// Less speech than this is a cough or a click, not a turn.
pub const MIN_SPEECH: Duration = Duration::from_millis(300);
/// No speech for this long: the user isn't talking, the capture ends.
pub const NOTHING_FOR: Duration = Duration::from_secs(12);
/// Speech is this many times louder than the room, and never quieter than
/// SPEECH_RMS.
pub const SPEECH_OVER_NOISE: f32 = 3.0;
pub const SPEECH_RMS: f32 = 0.012;
/// The room is never taken for louder than this, so a bad estimate can't
/// hide speech.
pub const NOISE_CAP: f32 = 0.03;

/// End-of-speech detection for audio mode, on 50 ms windows: the room's
/// noise is learned from the quiet windows, speech is clearly above it.
pub struct Endpoint {
    window: usize,
    pending: Vec<f32>,
    noise: Option<f32>,
    speech_ms: u64,
    quiet_ms: u64,
    total_ms: u64,
}

impl Endpoint {
    pub fn new(rate: u32) -> Self {
        Self { window: (rate as usize / 20).max(1), pending: Vec::new(), noise: None, speech_ms: 0, quiet_ms: 0, total_ms: 0 }
    }

    /// Takes the samples that arrived since the last call.
    pub fn feed(&mut self, samples: &[f32]) -> Option<Heard> {
        self.pending.extend_from_slice(samples);
        let mut at = 0;
        while self.pending.len() - at >= self.window {
            let level = rms(&self.pending[at..at + self.window]);
            at += self.window;
            self.total_ms += 50;
            // Before the room is known, the window itself, capped: speech at
            // the very start still counts, a noisy room still doesn't.
            let floor = self.noise.unwrap_or(level).min(NOISE_CAP);
            if level >= (floor * SPEECH_OVER_NOISE).max(SPEECH_RMS) {
                self.speech_ms += 50;
                self.quiet_ms = 0;
            } else {
                // The room, slowly: a quiet window moves the estimate a little.
                self.noise = Some(match self.noise {
                    None => level,
                    Some(n) => n * 0.95 + level * 0.05,
                });
                self.quiet_ms += 50;
            }
            if self.speech_ms >= MIN_SPEECH.as_millis() as u64 && self.quiet_ms >= PAUSE.as_millis() as u64 {
                self.pending.clear();
                return Some(Heard::Spoke);
            }
            if self.speech_ms < MIN_SPEECH.as_millis() as u64 && self.total_ms >= NOTHING_FOR.as_millis() as u64 {
                self.pending.clear();
                return Some(Heard::Nothing);
            }
        }
        self.pending.drain(..at);
        None
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

// The voice state machine, driven by a simulated microphone.

use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use super::simulated::{Chunk, Microphone as SimMic, Transcriber as SimStt};
use super::*;

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log(line: String) {
    LOG.lock().unwrap().push(line);
}

struct Recorder(Mutex<mpsc::Sender<VoiceEvent>>);

impl Sink for Recorder {
    fn emit(&self, event: &VoiceEvent) {
        let _ = self.0.lock().unwrap().send(event.clone());
    }
}

struct Harness {
    voice: Voice,
    events: mpsc::Receiver<VoiceEvent>,
    mic: SimMic,
}

const RATE: u32 = 1000;

fn harness(mic: SimMic) -> Harness {
    let (tx, rx) = mpsc::channel();
    let voice = Voice::new(Recorder(Mutex::new(tx)), mic.clone(), SimStt, log);
    Harness { voice, events: rx, mic }
}

/// Speech for a while, then (by default) silence until stopped.
fn speaking(ms: usize) -> SimMic {
    SimMic::new(RATE, (0..ms / 10).map(|_| Chunk::Tone(RATE as usize / 100, 0.3)).collect())
}

impl Harness {
    /// The next event in `state`, skipping the others; panics after 5 s.
    fn wait(&self, state: VoiceState) -> VoiceEvent {
        loop {
            let event = self.events.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("no {state:?}"));
            assert_eq!(event.v, CONTRACT_VERSION);
            if let Some(level) = event.level {
                assert!((0.0..=1.0).contains(&level));
            }
            if event.state == state {
                return event;
            }
        }
    }

    fn mic_open(&self) -> usize {
        *self.mic.open_now.lock().unwrap()
    }
}

#[test]
fn press_speak_press_gives_a_simulated_transcript_to_preview() {
    let h = harness(speaking(400));
    let id = h.voice.toggle().unwrap();
    let listening = h.wait(VoiceState::Listening);
    assert_eq!(listening.capture_id, id);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.voice.toggle().as_deref(), Some(id.as_str()), "the second press stops this capture");
    let processing = h.wait(VoiceState::Processing);
    assert_eq!(h.mic_open(), 0, "the microphone is closed before transcribing");
    assert!(processing.elapsed_ms.unwrap() > 0);
    let preview = h.wait(VoiceState::Preview);
    assert_eq!(preview.capture_id, id);
    assert!(preview.simulated);
    assert!(preview.transcript.unwrap().starts_with("Simulated transcript:"));
}

#[test]
fn cancel_while_listening_closes_the_microphone_and_keeps_nothing() {
    let h = harness(speaking(2000));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    h.voice.cancel(&id);
    let idle = h.wait(VoiceState::Idle);
    assert_eq!(idle.capture_id, id);
    assert_eq!(h.mic_open(), 0);
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired), "nothing to use");
}

#[test]
fn a_stale_capture_id_changes_nothing() {
    let h = harness(speaking(400));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    h.voice.cancel("cap-someone-else");
    h.voice.stop("cap-someone-else");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.mic_open(), 1, "still listening");
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&id);
    h.wait(VoiceState::Preview);
}

#[test]
fn silence_is_an_error_not_a_transcript() {
    let h = harness(SimMic::new(RATE, vec![Chunk::Silence(500)]));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&id);
    let error = h.wait(VoiceState::Error);
    assert_eq!(error.error, Some(VoiceError::Silence));
    assert_eq!(error.transcript, None);
}

#[test]
fn no_microphone_is_reported_and_the_next_press_tries_again() {
    let mut mic = speaking(400);
    mic.open_error = Some(VoiceError::NoMicrophone);
    let h = harness(mic);
    let first = h.voice.toggle().unwrap();
    let error = h.wait(VoiceState::Error);
    assert_eq!((error.capture_id.as_str(), error.error), (first.as_str(), Some(VoiceError::NoMicrophone)));
    let second = h.voice.toggle().unwrap();
    assert_ne!(first, second);
    h.wait(VoiceState::Error);
}

#[test]
fn a_denied_microphone_says_so() {
    let mut mic = speaking(400);
    mic.open_error = Some(VoiceError::MicrophoneDenied);
    let h = harness(mic);
    h.voice.toggle().unwrap();
    assert_eq!(h.wait(VoiceState::Error).error, Some(VoiceError::MicrophoneDenied));
}

#[test]
fn a_microphone_unplugged_mid_capture_ends_it_closed() {
    let mic = SimMic::new(
        RATE,
        vec![Chunk::Tone(100, 0.3), Chunk::Tone(100, 0.3), Chunk::Fail(VoiceError::NoMicrophone)],
    );
    let h = harness(mic);
    h.voice.toggle().unwrap();
    assert_eq!(h.wait(VoiceState::Error).error, Some(VoiceError::NoMicrophone));
    assert_eq!(h.mic_open(), 0);
}

#[test]
fn the_capture_stops_by_itself_at_the_limit() {
    let limit = (MAX_CAPTURE.as_secs() as usize) * RATE as usize;
    let h = harness(SimMic::new(RATE, vec![Chunk::Tone(limit + 5_000, 0.3)]));
    h.voice.toggle().unwrap();
    let processing = h.wait(VoiceState::Processing);
    assert_eq!(processing.elapsed_ms, Some(MAX_CAPTURE.as_millis() as u64));
    assert_eq!(h.mic_open(), 0);
    h.wait(VoiceState::Preview);
}

#[test]
fn use_turns_the_transcript_into_an_allowed_chat_draft_once() {
    let h = harness(speaking(400));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&id);
    let preview = h.wait(VoiceState::Preview);

    assert_eq!(h.voice.use_transcript("cap-other"), Err(ErrorCode::Expired));
    let draft = h.voice.use_transcript(&id).unwrap();
    assert_eq!(Some(draft.text.clone()), preview.transcript);
    assert!(draft.action_id.starts_with("act-"));
    h.wait(VoiceState::Idle);
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired), "only once");

    h.voice.receipt(&ActionReceipt::succeeded(&draft.action_id, "chat-field:draft-set").unwrap());
    // What was said never reaches the log.
    let lines = LOG.lock().unwrap().clone();
    assert!(lines.iter().any(|l| l.contains(&draft.action_id) && l.ends_with("succeeded")));
    assert!(!lines.iter().any(|l| l.contains(&draft.text)), "{lines:?}");
}

#[test]
fn cancel_on_a_preview_drops_the_transcript() {
    let h = harness(speaking(400));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&id);
    h.wait(VoiceState::Preview);
    h.voice.cancel(&id);
    h.wait(VoiceState::Idle);
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired));
}

#[test]
fn a_press_on_a_preview_starts_a_fresh_capture() {
    let h = harness(speaking(2000));
    let first = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&first);
    h.wait(VoiceState::Preview);
    let second = h.voice.toggle().unwrap();
    assert_ne!(first, second);
    assert_eq!(h.wait(VoiceState::Listening).capture_id, second);
    assert_eq!(h.voice.use_transcript(&first), Err(ErrorCode::Expired), "the old transcript is gone");
    h.voice.shutdown();
    h.wait(VoiceState::Idle);
    assert_eq!(h.mic_open(), 0);
}

fn preview(h: &Harness) -> String {
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.stop(&id);
    h.wait(VoiceState::Preview);
    id
}

#[test]
fn a_pill_command_is_an_allowed_pill_set_once() {
    let h = harness(speaking(400));
    let id = preview(&h);
    let args = serde_json::json!({ "mainPill": "integration_claude", "activeIntegrations": ["integration_github"] });
    let action = h.voice.act(&id, ActionKind::PillSet, args.clone()).unwrap();
    assert!(action.starts_with("act-"));
    h.wait(VoiceState::Idle);
    assert_eq!(h.voice.act(&id, ActionKind::PillSet, args), Err(ErrorCode::Expired), "only once");
}

#[test]
fn the_island_cannot_propose_any_other_kind_or_bad_args() {
    let h = harness(speaking(400));
    let id = preview(&h);
    // A chat draft is built from Rust's own copy of the transcript, never from the page.
    let draft = serde_json::json!({ "text": "something else entirely" });
    assert_eq!(h.voice.act(&id, ActionKind::ChatDraft, draft), Err(ErrorCode::Unavailable));
    assert_eq!(h.voice.act(&id, ActionKind::LaunchApp, serde_json::json!({ "appId": "x" })), Err(ErrorCode::Unavailable));
    // Still waiting: a refused kind doesn't use the transcript up.
    let five = serde_json::json!({ "mainPill": "integration_claude", "activeIntegrations": ["a", "b", "c", "d", "e"] });
    assert_eq!(h.voice.act(&id, ActionKind::PillSet, five), Err(ErrorCode::InvalidArgs));
    h.wait(VoiceState::Idle);
}

#[test]
fn a_receipt_for_an_id_that_is_not_ours_is_not_logged() {
    let h = harness(speaking(10));
    h.voice.receipt(&ActionReceipt::failed("act-1
forged line", ErrorCode::Failed));
    assert!(!LOG.lock().unwrap().iter().any(|l| l.contains("forged")));
}

#[test]
fn silence_detection_needs_one_loud_window() {
    let quiet = vec![0.001f32; 1000];
    assert!(is_silent(&quiet, RATE));
    let mut speech = quiet.clone();
    for s in speech[400..450].iter_mut() {
        *s = 0.2;
    }
    assert!(!is_silent(&speech, RATE));
    assert!(is_silent(&[], RATE));
}

/// Takes up to 5 s, unless told to stop; says whether it was.
struct SlowStt(Arc<Mutex<Option<bool>>>);

impl Transcriber for SlowStt {
    fn transcribe(&self, _: &[f32], _: u32, abort: &AtomicBool) -> Result<Transcript, VoiceError> {
        let started = Instant::now();
        while !abort.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let aborted = abort.load(Ordering::SeqCst);
        *self.0.lock().unwrap() = Some(aborted);
        if aborted {
            Err(VoiceError::Provider)
        } else {
            Ok(Transcript { text: "late".into(), simulated: false })
        }
    }
}

#[test]
fn cancel_while_transcribing_stops_the_transcription() {
    let (tx, rx) = mpsc::channel();
    let heard = Arc::new(Mutex::new(None));
    let mic = speaking(400);
    let h = Harness { voice: Voice::new(Recorder(Mutex::new(tx)), mic.clone(), SlowStt(heard.clone()), log), events: rx, mic };
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.toggle();
    h.wait(VoiceState::Processing);
    let cancelled = Instant::now();
    h.voice.cancel(&id);
    assert_eq!(h.wait(VoiceState::Idle).capture_id, id, "idle at once");
    while heard.lock().unwrap().is_none() && cancelled.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*heard.lock().unwrap(), Some(true), "the transcriber was told to stop");
    assert!(cancelled.elapsed() < Duration::from_secs(1), "and stopped well before finishing");
    std::thread::sleep(Duration::from_millis(50));
    assert!(h.events.try_iter().all(|e| e.state != VoiceState::Preview && e.state != VoiceState::Error), "nothing after idle");
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired));
}

#[test]
fn locking_the_screen_while_listening_closes_the_microphone() {
    let h = harness(speaking(2000));
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    h.voice.interrupt("screen locked");
    assert_eq!(h.wait(VoiceState::Idle).capture_id, id);
    assert_eq!(h.mic_open(), 0);
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired), "nothing kept");
    assert!(LOG.lock().unwrap().iter().any(|l| l == "voice: interrupted (screen locked)"));
}

#[test]
fn sleep_while_transcribing_stops_it_and_idle_changes_nothing() {
    let (tx, rx) = mpsc::channel();
    let heard = Arc::new(Mutex::new(None));
    let mic = speaking(400);
    let h = Harness { voice: Voice::new(Recorder(Mutex::new(tx)), mic.clone(), SlowStt(heard.clone()), log), events: rx, mic };
    h.voice.interrupt("going to sleep");
    assert!(h.events.recv_timeout(Duration::from_millis(100)).is_err(), "idle: no event");
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    std::thread::sleep(Duration::from_millis(300));
    h.voice.toggle();
    h.wait(VoiceState::Processing);
    h.voice.interrupt("going to sleep");
    assert_eq!(h.wait(VoiceState::Idle).capture_id, id);
    let since = Instant::now();
    while heard.lock().unwrap().is_none() && since.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*heard.lock().unwrap(), Some(true));
}

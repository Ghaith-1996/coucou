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

/// Holds the capture thread inside a read after speech is buffered, so the
/// cancellation and button presses run while the phase is still Listening.
struct GatedMic {
    mic: SimMic,
    buffered: Mutex<mpsc::Sender<()>>,
    resume: Arc<Mutex<mpsc::Receiver<()>>>,
}

struct GatedCapture {
    capture: Box<dyn Capture>,
    buffered: mpsc::Sender<()>,
    resume: Arc<Mutex<mpsc::Receiver<()>>>,
}

impl Microphone for GatedMic {
    fn open(&self) -> Result<Box<dyn Capture>, VoiceError> {
        Ok(Box::new(GatedCapture {
            capture: self.mic.open()?,
            buffered: self.buffered.lock().unwrap().clone(),
            resume: self.resume.clone(),
        }))
    }
}

impl Capture for GatedCapture {
    fn sample_rate(&self) -> u32 {
        self.capture.sample_rate()
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), VoiceError> {
        self.capture.read(out)?;
        self.buffered.send(()).unwrap();
        self.resume.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(())
    }
}

struct RecordingStt(Arc<AtomicBool>);

impl Transcriber for RecordingStt {
    fn transcribe(&self, samples: &[f32], rate: u32, abort: &AtomicBool) -> Result<Transcript, VoiceError> {
        self.0.store(true, Ordering::SeqCst);
        SimStt.transcribe(samples, rate, abort)
    }
}

fn assert_buffered_capture_cancelled(actions: impl FnOnce(&Voice, &str)) {
    let (tx, events) = mpsc::channel();
    let (buffered_tx, buffered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let transcribed = Arc::new(AtomicBool::new(false));
    let mic = SimMic::new(RATE, vec![Chunk::Tone(400, 0.3)]);
    let gated = GatedMic { mic: mic.clone(), buffered: Mutex::new(buffered_tx), resume: Arc::new(Mutex::new(resume_rx)) };
    let h = Harness { voice: Voice::new(Recorder(Mutex::new(tx)), gated, RecordingStt(transcribed.clone()), log), events, mic };
    let id = h.voice.toggle().unwrap();
    h.wait(VoiceState::Listening);
    buffered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    actions(&h.voice, &id);
    resume_tx.send(()).unwrap();

    loop {
        let event = h.events.recv_timeout(Duration::from_secs(5)).expect("cancelled capture did not become idle");
        assert_eq!(event.capture_id, id);
        assert!(matches!(event.state, VoiceState::Listening | VoiceState::Idle), "cancelled audio produced {event:?}");
        if event.state == VoiceState::Idle {
            break;
        }
    }
    assert_eq!(h.mic_open(), 0);
    assert!(!transcribed.load(Ordering::SeqCst), "cancelled audio must never reach the transcriber");
    assert_eq!(h.voice.use_transcript(&id), Err(ErrorCode::Expired));
}

fn press_stop(voice: &Voice, id: &str, toggle: bool) {
    if toggle {
        assert_eq!(voice.toggle().as_deref(), Some(id));
    } else {
        voice.stop(id);
    }
}

#[test]
fn interruption_wins_over_stop_and_toggle_in_either_order() {
    for toggle in [false, true] {
        for stop_first in [false, true] {
            assert_buffered_capture_cancelled(|voice, id| {
                if stop_first {
                    press_stop(voice, id, toggle);
                }
                voice.interrupt("screen locked");
                if !stop_first {
                    press_stop(voice, id, toggle);
                }
            });
        }
    }
}

#[test]
fn cancel_and_shutdown_cannot_be_undone_by_stop_or_toggle() {
    for toggle in [false, true] {
        for shutdown in [false, true] {
            assert_buffered_capture_cancelled(|voice, id| {
                if shutdown {
                    voice.shutdown();
                } else {
                    voice.cancel(id);
                }
                press_stop(voice, id, toggle);
            });
        }
    }
}

#[test]
fn interruption_racing_stop_or_toggle_drops_buffered_audio() {
    for toggle in [false, true] {
        for _ in 0..16 {
            assert_buffered_capture_cancelled(|voice, id| {
                let start = std::sync::Barrier::new(2);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        start.wait();
                        voice.interrupt("going to sleep");
                    });
                    start.wait();
                    press_stop(voice, id, toggle);
                });
            });
        }
    }
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

fn app() -> serde_json::Value {
    serde_json::json!({ "appId": "figma-0a1b2c3d" })
}

/// A completed capture without a microphone thread or timing-dependent waits.
fn app_proposal(h: &Harness, capture_id: &str) -> policy::Grant {
    *h.voice.0.phase.lock().unwrap() = Phase::Preview {
        id: capture_id.into(),
        transcript: Transcript { text: "Open Figma".into(), simulated: true },
        since: Instant::now(),
    };
    h.voice.propose(capture_id, ActionKind::LaunchApp, app()).unwrap()
}

static PROPOSAL_LOG_GATE: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = Mutex::new(None);

fn pause_one_proposal_log(line: String) {
    log(line.clone());
    if line.ends_with(" proposed") {
        let gate = PROPOSAL_LOG_GATE.lock().unwrap().take();
        if let Some((arrived, resume)) = gate {
            arrived.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
}

#[test]
fn a_delayed_proposal_return_cannot_overwrite_a_newer_capture_proposal() {
    let (tx, rx) = mpsc::channel();
    let mic = speaking(0);
    let h = Harness {
        voice: Voice::new(Recorder(Mutex::new(tx)), mic.clone(), SimStt, pause_one_proposal_log),
        events: rx,
        mic,
    };
    app_proposal(&h, "cap-old");
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    *PROPOSAL_LOG_GATE.lock().unwrap() = Some((arrived_tx, resume_rx));
    let voice = h.voice.clone();
    let delayed = std::thread::spawn(move || voice.propose("cap-old", ActionKind::LaunchApp, app()));
    arrived_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // The old request is paused at its final log, after publishing its proposal.
    let current = app_proposal(&h, "cap-current");
    resume_tx.send(()).unwrap();
    let old = delayed.join().unwrap().unwrap();
    assert_eq!(h.voice.confirm("cap-old", &old), Err(ErrorCode::GrantMismatch));
    assert!(h.events.try_recv().is_err(), "the delayed request leaves the new card alone");
    assert_eq!(h.voice.confirm("cap-current", &current).unwrap().0, current.action_id);
}

#[test]
fn preview_consumption_keeps_capture_ownership_while_waiting_for_its_proposal() {
    let h = harness(speaking(0));
    app_proposal(&h, "cap-current");
    let proposal = h.voice.0.proposal.lock().unwrap();
    let voice = h.voice.clone();
    let consuming = std::thread::spawn(move || voice.use_transcript("cap-current"));
    let started = Instant::now();
    // Wait for the consumer's phase guard, not for an arbitrary sleep. It must
    // retain ownership of the preview until its proposal is consumed too.
    loop {
        match h.voice.0.phase.try_lock() {
            Err(std::sync::TryLockError::WouldBlock) => break,
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("phase lock poisoned"),
            Ok(_) => {}
        }
        assert!(started.elapsed() < Duration::from_secs(5), "preview ownership was released before proposal cleanup");
        std::thread::yield_now();
    }
    drop(proposal);
    assert_eq!(consuming.join().unwrap().unwrap().text, "Open Figma");
    assert!(h.voice.0.proposal.lock().unwrap().is_none());
    let current = app_proposal(&h, "cap-new");
    assert_eq!(h.voice.confirm("cap-new", &current).unwrap().0, current.action_id);
}

#[test]
fn a_delayed_confirmation_leaves_the_newer_capture_confirmable() {
    let h = harness(speaking(0));
    let old = app_proposal(&h, "cap-old");
    let current = app_proposal(&h, "cap-current");

    assert_eq!(h.voice.confirm("cap-old", &old), Err(ErrorCode::GrantMismatch));
    assert!(h.events.try_recv().is_err(), "the current card stays on screen");
    assert_eq!(
        h.voice.confirm("cap-current", &current),
        Ok((current.action_id.clone(), ValidArgs::LaunchApp(LaunchAppArgs { app_id: "figma-0a1b2c3d".into() }))),
        "the current card's grant still works",
    );
    let idle = h.events.try_recv().unwrap();
    assert_eq!((idle.capture_id.as_str(), idle.state), ("cap-current", VoiceState::Idle));
    assert_eq!(h.voice.confirm("cap-current", &current), Err(ErrorCode::Expired), "still only once");
}

#[test]
fn a_stale_capture_cannot_consume_even_the_current_grant() {
    let h = harness(speaking(0));
    app_proposal(&h, "cap-old");
    let current = app_proposal(&h, "cap-current");

    assert_eq!(h.voice.confirm("cap-old", &current), Err(ErrorCode::GrantMismatch));
    assert!(h.events.try_recv().is_err(), "capture ownership is checked before consumption");
    assert_eq!(h.voice.confirm("cap-current", &current).unwrap().0, current.action_id);
    let idle = h.events.try_recv().unwrap();
    assert_eq!((idle.capture_id.as_str(), idle.state), ("cap-current", VoiceState::Idle));
}

#[test]
fn using_a_stale_preview_preserves_the_new_capture_proposal() {
    let h = harness(speaking(0));
    app_proposal(&h, "cap-old");
    let current = app_proposal(&h, "cap-current");

    assert_eq!(h.voice.use_transcript("cap-old"), Err(ErrorCode::Expired));
    let pills = serde_json::json!({ "mainPill": "integration_claude", "activeIntegrations": [] });
    assert_eq!(h.voice.act("cap-old", ActionKind::PillSet, pills), Err(ErrorCode::Expired));
    assert!(h.events.try_recv().is_err(), "a stale preview cannot dismiss the current card");
    assert_eq!(h.voice.confirm("cap-current", &current).unwrap().0, current.action_id);
}

#[test]
fn an_invalid_grant_from_the_owning_capture_consumes_its_proposal_and_preview() {
    for changed_field in ["action", "digest", "expiry"] {
        let h = harness(speaking(0));
        let original = app_proposal(&h, "cap-current");
        let mut forged = original.clone();
        match changed_field {
            "action" => forged.action_id = "act-forged".into(),
            "digest" => forged.digest = "forged".into(),
            "expiry" => forged.expires_at_ms += 1,
            _ => unreachable!(),
        }

        assert_eq!(h.voice.confirm("cap-current", &forged), Err(ErrorCode::GrantMismatch), "{changed_field}");
        let idle = h.events.try_recv().unwrap();
        assert_eq!((idle.capture_id.as_str(), idle.state), ("cap-current", VoiceState::Idle));
        assert_eq!(h.voice.confirm("cap-current", &original), Err(ErrorCode::Expired), "{changed_field}: consumed grant");
        assert_eq!(h.voice.use_transcript("cap-current"), Err(ErrorCode::Expired), "{changed_field}: consumed preview");
    }
}

#[test]
fn opening_an_app_is_proposed_then_confirmed_once_with_its_grant() {
    let h = harness(speaking(400));
    let id = preview(&h);
    let grant = h.voice.propose(&id, ActionKind::LaunchApp, app()).unwrap();
    assert!(grant.action_id.starts_with("act-"));
    // Proposing leaves the transcript on the card.
    assert!(h.events.recv_timeout(Duration::from_millis(100)).is_err(), "no event yet");
    let (action, args) = h.voice.confirm(&id, &grant).unwrap();
    assert_eq!(action, grant.action_id);
    assert_eq!(args, ValidArgs::LaunchApp(LaunchAppArgs { app_id: "figma-0a1b2c3d".into() }));
    h.wait(VoiceState::Idle);
    assert_eq!(h.voice.confirm(&id, &grant), Err(ErrorCode::Expired), "only once");
}

#[test]
fn a_grant_the_page_changed_or_made_up_launches_nothing() {
    let h = harness(speaking(400));
    let id = preview(&h);
    let grant = h.voice.propose(&id, ActionKind::LaunchApp, app()).unwrap();
    let mut forged = grant.clone();
    forged.digest = "0".repeat(64);
    assert_eq!(h.voice.confirm(&id, &forged), Err(ErrorCode::GrantMismatch));
    // The proposal and the transcript are gone: the real grant is worthless now too.
    assert_eq!(h.voice.confirm(&id, &grant), Err(ErrorCode::Expired));

    // Each capture needs its own microphone script.
    let h = harness(speaking(400));
    let id = preview(&h);
    let grant = h.voice.propose(&id, ActionKind::LaunchApp, app()).unwrap();
    let longer = policy::Grant { expires_at_ms: grant.expires_at_ms + 60_000, ..grant.clone() };
    assert_eq!(h.voice.confirm(&id, &longer), Err(ErrorCode::GrantMismatch), "a later expiry");
    let h = harness(speaking(400));
    let id = preview(&h);
    h.voice.propose(&id, ActionKind::LaunchApp, app()).unwrap();
    let other = policy::Grant { action_id: "act-0000000000000000".into(), ..grant };
    assert_eq!(h.voice.confirm(&id, &other), Err(ErrorCode::GrantMismatch), "another action");
}

#[test]
fn only_an_app_id_may_be_proposed_and_only_for_a_waiting_transcript() {
    let h = harness(speaking(400));
    assert_eq!(h.voice.propose("cap-none", ActionKind::LaunchApp, app()).unwrap_err(), ErrorCode::Expired);
    let id = preview(&h);
    let chat = serde_json::json!({ "text": "hi" });
    assert_eq!(h.voice.propose(&id, ActionKind::ChatDraft, chat).unwrap_err(), ErrorCode::Unavailable);
    for bad in [serde_json::json!({ "appId": "C:/Windows/cmd.exe" }), serde_json::json!({ "appId": "x", "args": "-y" }), serde_json::json!({})] {
        assert_eq!(h.voice.propose(&id, ActionKind::LaunchApp, bad).unwrap_err(), ErrorCode::InvalidArgs);
    }
    // Cancelling the card drops the proposal with the transcript.
    let grant = h.voice.propose(&id, ActionKind::LaunchApp, app()).unwrap();
    h.voice.cancel(&id);
    h.wait(VoiceState::Idle);
    assert_eq!(h.voice.confirm(&id, &grant), Err(ErrorCode::Expired));
}

/// A real (not simulated) transcript waiting on the card, set directly.
fn spoken(h: &Harness, capture_id: &str, text: &str) {
    *h.voice.0.phase.lock().unwrap() = Phase::Preview {
        id: capture_id.into(),
        transcript: Transcript { text: text.into(), simulated: false },
        since: Instant::now(),
    };
}

fn insertion(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": text, "targetWindow": 4242, "targetPid": 77 })
}

#[test]
fn insertion_offers_and_retries_share_the_native_previews_exact_deadline() {
    let h = harness(speaking(0));
    // Keep a scheduling margin here; the target tests exercise the last
    // millisecond and exact deadline with explicit instants and no sleep.
    let since = Instant::now() - Duration::from_secs(110);
    *h.voice.0.phase.lock().unwrap() = Phase::Preview {
        id: "cap-late".into(),
        transcript: Transcript { text: "Still valid".into(), simulated: false },
        since,
    };
    let (text, expires) = h.voice.insertion_preview("cap-late").unwrap();
    assert_eq!(text, "Still valid");
    assert_eq!(expires, since + PREVIEW_TTL);
    let grant = h.voice.propose("cap-late", ActionKind::InsertText, insertion(&text)).unwrap();
    let (action, _, permit) = h.voice.begin_insert("cap-late", &grant).unwrap();
    assert!(permit.expires <= expires, "the write cannot outlive the target's preview");
    h.voice.finish_insert("cap-late", &action, true).unwrap();
    assert_eq!(h.voice.insertion_preview("cap-late").unwrap(), (text.clone(), expires));
    let reoffered = h.voice.propose("cap-late", ActionKind::InsertText, insertion(&text)).unwrap();
    let (_, _, retry_permit) = h.voice.begin_insert("cap-late", &reoffered).unwrap();
    assert!(retry_permit.expires <= expires, "even a new offer cannot renew the preview");
}

#[test]
fn an_expired_preview_cannot_supply_a_target_deadline() {
    let h = harness(speaking(0));
    *h.voice.0.phase.lock().unwrap() = Phase::Preview {
        id: "cap-expired".into(),
        transcript: Transcript { text: "Too late".into(), simulated: false },
        since: Instant::now() - PREVIEW_TTL,
    };
    assert_eq!(h.voice.insertion_preview("cap-expired"), Err(ErrorCode::Expired));
}

#[test]
fn dictated_text_is_proposed_for_its_field_then_confirmed_once() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "Hello there");
    assert_eq!(h.voice.insertion_preview("cap-dictate").unwrap().0, "Hello there");
    assert_eq!(h.voice.insertion_preview("cap-other"), Err(ErrorCode::Expired));
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("Hello there")).unwrap();
    // Peeking and proposing leave the transcript on the card.
    assert_eq!(h.voice.insertion_preview("cap-dictate").unwrap().0, "Hello there");
    let (action, args, permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
    assert_eq!(action, grant.action_id);
    assert_eq!(
        args,
        InsertTextArgs { text: "Hello there".into(), target_window: 4242, target_pid: 77 },
    );
    assert!(!permit.cancelled.load(Ordering::SeqCst));
    assert!(h.events.try_recv().is_err(), "a pending write does not dismiss the card");
    h.voice.finish_insert("cap-dictate", &action, false).unwrap();
    assert!(permit.cancelled.load(Ordering::SeqCst));
    assert_eq!(h.wait(VoiceState::Idle).capture_id, "cap-dictate");
    assert_eq!(h.voice.begin_insert("cap-dictate", &grant).unwrap_err(), ErrorCode::Expired, "only once");
    assert_eq!(h.voice.confirm("cap-dictate", &grant), Err(ErrorCode::Expired), "only once");
    assert_eq!(h.voice.insertion_preview("cap-dictate"), Err(ErrorCode::Expired), "the transcript is used");
}

#[test]
fn a_prewrite_refusal_restores_the_exact_preview_and_same_grant_without_extending_its_lifetime() {
    let h = harness(speaking(0));
    let since = Instant::now() - Duration::from_secs(60);
    let original = Transcript { text: "Keep these words".into(), simulated: false };
    *h.voice.0.phase.lock().unwrap() = Phase::Preview {
        id: "cap-dictate".into(), transcript: original.clone(), since,
    };
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion(&original.text)).unwrap();
    let original_action = h.voice.0.proposal.lock().unwrap().as_ref().unwrap().action.clone();
    assert!(grant.expires_at_ms <= policy::now_ms() + 60_000, "proposing cannot restart the preview's TTL");

    let (action, args, first_permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
    assert!(first_permit.expires <= since + PREVIEW_TTL);
    h.voice.finish_insert("cap-dictate", &action, true).unwrap();
    assert!(first_permit.cancelled.load(Ordering::SeqCst), "the refused attempt's permit is retired");
    assert!(h.events.try_recv().is_err(), "refusal leaves the existing card in place");
    {
        let phase = h.voice.0.phase.lock().unwrap();
        let Phase::Preview { id, transcript, since: restored_since } = &*phase else { panic!("preview was not restored") };
        assert_eq!(id, "cap-dictate");
        assert_eq!(transcript, &original);
        assert_eq!(*restored_since, since, "the preview keeps its original deadline");
        let proposal = h.voice.0.proposal.lock().unwrap();
        let restored = proposal.as_ref().unwrap();
        assert_eq!(restored.capture, "cap-dictate");
        assert_eq!(serde_json::to_value(&restored.action).unwrap(), serde_json::to_value(&original_action).unwrap());
    }

    let (retry_action, retry_args, retry_permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
    assert_eq!(retry_action, action, "the card retries the grant it already has");
    assert_eq!(retry_args, args);
    assert!(retry_permit.expires <= since + PREVIEW_TTL);
    assert!(!retry_permit.cancelled.load(Ordering::SeqCst));
    assert!(!Arc::ptr_eq(&first_permit.cancelled, &retry_permit.cancelled), "a retry gets its own permit");
    h.voice.finish_insert("cap-dictate", &retry_action, false).unwrap();
    assert_eq!(h.voice.begin_insert("cap-dictate", &grant).unwrap_err(), ErrorCode::Expired);
}

#[test]
fn pending_insertion_cannot_be_consumed_by_duplicate_or_other_ipc_actions() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "Keep these words");
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("Keep these words")).unwrap();
    let (action, _, permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();

    assert_eq!(h.voice.begin_insert("cap-dictate", &grant).unwrap_err(), ErrorCode::Expired);
    assert_eq!(h.voice.confirm("cap-dictate", &grant), Err(ErrorCode::Expired));
    assert_eq!(h.voice.use_transcript("cap-dictate"), Err(ErrorCode::Expired));
    let pills = serde_json::json!({ "mainPill": "integration_claude", "activeIntegrations": [] });
    assert_eq!(h.voice.act("cap-dictate", ActionKind::PillSet, pills), Err(ErrorCode::Expired));
    assert_eq!(h.voice.propose("cap-dictate", ActionKind::LaunchApp, app()).unwrap_err(), ErrorCode::Expired);
    assert_eq!(h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("different words")).unwrap_err(), ErrorCode::Expired);
    assert_eq!(h.voice.insertion_preview("cap-dictate"), Err(ErrorCode::Expired));
    h.voice.cancel("cap-stale");
    for refused in [false, true] {
        assert_eq!(h.voice.finish_insert("cap-stale", &action, refused), Err(ErrorCode::Cancelled));
        assert_eq!(h.voice.finish_insert("cap-dictate", "act-stale", refused), Err(ErrorCode::Cancelled));
    }
    assert!(!permit.cancelled.load(Ordering::SeqCst), "stale calls leave the active write alone");
    assert!(h.events.try_recv().is_err());
    assert_eq!(h.voice.0.proposal.lock().unwrap().as_ref().unwrap().action.id, action);

    h.voice.finish_insert("cap-dictate", &action, true).unwrap();
    assert_eq!(h.voice.insertion_preview("cap-dictate").unwrap().0, "Keep these words");
    h.voice.begin_insert("cap-dictate", &grant).unwrap();
    h.voice.finish_insert("cap-dictate", &action, false).unwrap();
}

#[test]
fn concurrent_insert_confirmations_reserve_exactly_one_write() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "Only once");
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("Only once")).unwrap();
    let start = std::sync::Barrier::new(2);
    let voice = &h.voice;
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            voice.begin_insert("cap-dictate", &grant)
        });
        let second = scope.spawn(|| {
            start.wait();
            voice.begin_insert("cap-dictate", &grant)
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter_map(|result| result.as_ref().err()).copied().collect::<Vec<_>>(), vec![ErrorCode::Expired]);
    assert!(h.events.try_recv().is_err());
    h.voice.finish_insert("cap-dictate", &grant.action_id, false).unwrap();
    assert!(h.voice.0.proposal.lock().unwrap().is_none());
}

#[test]
fn a_refusal_after_expiry_cannot_revive_the_preview_or_grant() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "Too late");
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("Too late")).unwrap();
    let (action, _, permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
    // Advance the stored deadline directly: std::time::Instant is not the
    // paused Tokio clock, and this regression must not sleep for two minutes.
    {
        let mut phase = h.voice.0.phase.lock().unwrap();
        let Phase::Inserting { permit, .. } = &mut *phase else { panic!("no pending insertion") };
        permit.expires = Instant::now() - Duration::from_millis(1);
    }
    assert_eq!(h.voice.finish_insert("cap-dictate", &action, true), Err(ErrorCode::Expired));
    assert!(permit.cancelled.load(Ordering::SeqCst));
    assert_eq!(h.wait(VoiceState::Idle).capture_id, "cap-dictate");
    assert!(h.voice.0.proposal.lock().unwrap().is_none());
    assert_eq!(h.voice.insertion_preview("cap-dictate"), Err(ErrorCode::Expired));
    assert_eq!(h.voice.begin_insert("cap-dictate", &grant).unwrap_err(), ErrorCode::Expired);
    assert_eq!(h.voice.finish_insert("cap-dictate", &action, true), Err(ErrorCode::Cancelled));
}

#[test]
fn cancellation_interruption_and_shutdown_retire_the_pending_write_without_restoring_it() {
    for reason in ["cancel", "cancel-before-probe", "interrupt", "shutdown"] {
        let h = harness(speaking(0));
        spoken(&h, "cap-dictate", "Discard me");
        let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("Discard me")).unwrap();
        let (action, _, permit) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
        match reason {
            "cancel" => h.voice.cancel("cap-dictate"),
            "cancel-before-probe" => h.voice.cancel_preview(),
            "interrupt" => h.voice.interrupt("screen locked"),
            "shutdown" => h.voice.shutdown(),
            _ => unreachable!(),
        }
        assert!(permit.cancelled.load(Ordering::SeqCst), "{reason}");
        assert_eq!(h.wait(VoiceState::Idle).capture_id, "cap-dictate");
        assert!(h.voice.0.proposal.lock().unwrap().is_none());
        for refused in [false, true] {
            assert_eq!(h.voice.finish_insert("cap-dictate", &action, refused), Err(ErrorCode::Cancelled), "{reason}");
        }
        assert_eq!(h.voice.begin_insert("cap-dictate", &grant).unwrap_err(), ErrorCode::Expired, "{reason}");
        assert_eq!(h.voice.use_transcript("cap-dictate"), Err(ErrorCode::Expired), "{reason}");
        assert!(h.events.try_recv().is_err(), "a late completion emits no stale event: {reason}");
    }
}

#[test]
fn cancelling_before_a_probe_retires_a_preview_but_leaves_listening_and_processing_alone() {
    let h = harness(speaking(0));
    spoken(&h, "cap-preview", "Old words");
    h.voice.propose("cap-preview", ActionKind::InsertText, insertion("Old words")).unwrap();
    h.voice.cancel_preview();
    assert_eq!(h.wait(VoiceState::Idle).capture_id, "cap-preview");
    assert!(h.voice.0.proposal.lock().unwrap().is_none());
    assert_eq!(h.voice.insertion_preview("cap-preview"), Err(ErrorCode::Expired));

    // These phases are seeded directly: no microphone or transcriber runs.
    for signal_value in [RUN, STOP] {
        let signal = Arc::new(AtomicU8::new(signal_value));
        *h.voice.0.phase.lock().unwrap() = Phase::Listening { id: "cap-listening".into(), signal: signal.clone() };
        h.voice.cancel_preview();
        assert_eq!(signal.load(Ordering::SeqCst), signal_value);
        assert!(matches!(&*h.voice.0.phase.lock().unwrap(), Phase::Listening { id, .. } if id == "cap-listening"));
    }
    let abort = Arc::new(AtomicBool::new(false));
    *h.voice.0.phase.lock().unwrap() = Phase::Processing { id: "cap-processing".into(), abort: abort.clone() };
    h.voice.cancel_preview();
    assert!(!abort.load(Ordering::SeqCst));
    assert!(matches!(&*h.voice.0.phase.lock().unwrap(), Phase::Processing { id, .. } if id == "cap-processing"));
    assert!(h.events.try_recv().is_err());
}

#[test]
fn a_new_capture_cancels_the_old_write_and_a_late_refusal_cannot_replace_it() {
    let h = harness(speaking(0));
    spoken(&h, "cap-old", "Old words");
    let grant = h.voice.propose("cap-old", ActionKind::InsertText, insertion("Old words")).unwrap();
    let (action, _, permit) = h.voice.begin_insert("cap-old", &grant).unwrap();
    let current = h.voice.toggle().unwrap();
    assert_ne!(current, "cap-old");
    assert_eq!(h.wait(VoiceState::Listening).capture_id, current);
    assert!(permit.cancelled.load(Ordering::SeqCst));
    assert!(h.voice.0.proposal.lock().unwrap().is_none());
    for refused in [false, true] {
        assert_eq!(h.voice.finish_insert("cap-old", &action, refused), Err(ErrorCode::Cancelled));
    }
    assert_eq!(h.voice.0.phase.lock().unwrap().id(), Some(current.as_str()));
    h.voice.cancel(&current);
    assert_eq!(h.wait(VoiceState::Idle).capture_id, current);

    // A late result also cannot consume a newer proposal for another field.
    spoken(&h, "cap-new", "New words");
    let next = h.voice.propose("cap-new", ActionKind::InsertText, insertion("New words")).unwrap();
    let (next_action, _, next_permit) = h.voice.begin_insert("cap-new", &next).unwrap();
    assert_eq!(h.voice.finish_insert("cap-old", &action, true), Err(ErrorCode::Cancelled));
    assert!(!next_permit.cancelled.load(Ordering::SeqCst));
    h.voice.finish_insert("cap-new", &next_action, true).unwrap();
    assert_eq!(h.voice.insertion_preview("cap-new").unwrap().0, "New words");
}

#[test]
fn a_simulated_transcript_is_never_offered_to_another_app() {
    let h = harness(speaking(400));
    let id = preview(&h);
    assert_eq!(h.voice.insertion_preview(&id), Err(ErrorCode::Unavailable));
}

#[test]
fn an_insertion_needs_a_target_and_sane_text() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "hi");
    for bad in [
        serde_json::json!({ "text": "hi", "targetWindow": 0, "targetPid": 77 }),
        serde_json::json!({ "text": "hi", "targetWindow": 4242, "targetPid": 0 }),
        serde_json::json!({ "text": "   ", "targetWindow": 4242, "targetPid": 77 }),
        serde_json::json!({ "text": "a\u{1b}[2J", "targetWindow": 4242, "targetPid": 77 }),
        serde_json::json!({ "text": "hi", "targetWindow": 4242, "targetPid": 77, "submit": true }),
    ] {
        assert_eq!(h.voice.propose("cap-dictate", ActionKind::InsertText, bad.clone()).unwrap_err(), ErrorCode::InvalidArgs, "{bad}");
    }
}

#[test]
fn a_terminal_insertion_failure_can_become_an_allowed_chat_draft() {
    let h = harness(speaking(0));
    spoken(&h, "cap-dictate", "keep me");
    let grant = h.voice.propose("cap-dictate", ActionKind::InsertText, insertion("keep me")).unwrap();
    let (action, _, _) = h.voice.begin_insert("cap-dictate", &grant).unwrap();
    h.voice.finish_insert("cap-dictate", &action, false).unwrap();
    let draft = h.voice.fallback_draft("cap-dictate", "keep me".into()).unwrap();
    assert_eq!(draft.text, "keep me");
    assert!(draft.action_id.starts_with("act-"));
    assert_eq!(h.voice.fallback_draft("cap-dictate", " ".into()).unwrap_err(), ErrorCode::InvalidArgs);
}

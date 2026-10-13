// The assistant inside the app: voice events to the island, and the commands
// the island calls. Every command names the capture it is about, so a click on
// a card that is already stale does nothing.

use tauri::{AppHandle, Emitter, State};

use super::contracts::{ActionReceipt, ErrorCode, VoiceEvent, VOICE_EVENT};
use super::voice::{self, ChatDraft, Voice};
use crate::island::WINDOW_LABEL;

struct IslandSink(AppHandle);

impl voice::Sink for IslandSink {
    fn emit(&self, event: &VoiceEvent) {
        let _ = self.0.emit_to(WINDOW_LABEL, VOICE_EVENT, event);
    }
}

fn log(line: String) {
    crate::log::line(line);
}

#[cfg(windows)]
type Mic = voice::wasapi::Microphone;

/// No microphone adapter outside Windows yet: every capture says so.
#[cfg(not(windows))]
#[derive(Default)]
struct Mic;

#[cfg(not(windows))]
impl voice::Microphone for Mic {
    fn open(&self) -> Result<Box<dyn voice::Capture>, super::contracts::VoiceError> {
        Err(super::contracts::VoiceError::NoMicrophone)
    }
}

pub fn voice(app: &AppHandle) -> Voice {
    // P0: the simulated transcriber, labelled as such on the island.
    Voice::new(IslandSink(app.clone()), Mic::default(), voice::simulated::Transcriber, log)
}

/// The voice shortcut or the island's own button. The island decides first
/// whether a capture may start (never over an agent's waiting card).
#[tauri::command]
pub fn assistant_voice_toggle(voice: State<Voice>) -> Option<String> {
    voice.toggle()
}

#[tauri::command]
pub fn assistant_voice_stop(voice: State<Voice>, capture_id: String) {
    voice.stop(&capture_id);
}

#[tauri::command]
pub fn assistant_voice_cancel(voice: State<Voice>, capture_id: String) {
    voice.cancel(&capture_id);
}

/// Use: the transcript as a `chat-draft` action the policy allowed, or why not.
#[tauri::command]
pub fn assistant_voice_use(voice: State<Voice>, capture_id: String) -> Result<ChatDraft, ErrorCode> {
    voice.use_transcript(&capture_id)
}

#[tauri::command]
pub fn assistant_action_receipt(voice: State<Voice>, receipt: ActionReceipt) {
    voice.receipt(&receipt);
}

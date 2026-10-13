// The assistant inside the app: voice events to the island, and the commands
// the island calls. Every command names the capture it is about, so a click on
// a card that is already stale does nothing.

use tauri::{AppHandle, Emitter, State, Webview};

use super::contracts::{ActionKind, ActionReceipt, ErrorCode, VoiceEvent, VOICE_EVENT};
use super::voice::{self, model, ChatDraft, Voice};
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
    // Whisper on this computer once its model is installed; the simulated
    // transcriber (labelled as such on the island) until then.
    Voice::new(IslandSink(app.clone()), Mic::default(), voice::Auto::default(), log)
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

/// A command the transcript asked for (a `pill-set` today), proposed by the
/// island for the policy to decide. The action id when it is allowed.
#[tauri::command]
pub fn assistant_voice_act(
    voice: State<Voice>,
    capture_id: String,
    kind: ActionKind,
    args: serde_json::Value,
) -> Result<String, ErrorCode> {
    voice.act(&capture_id, kind, args)
}

#[tauri::command]
pub fn assistant_action_receipt(voice: State<Voice>, receipt: ActionReceipt) {
    voice.receipt(&receipt);
}

// ── The speech model (Settings → Voice) ──────────────────────────────────────

/// Only the settings window manages the model, from a click there.
fn from_settings(webview: &Webview) -> Result<(), String> {
    model::require_supported()?;
    if webview.label() == "settings" {
        Ok(())
    } else {
        Err("only from Settings".into())
    }
}

#[tauri::command]
pub fn voice_model_status() -> model::Status {
    model::status()
}

/// Downloads the model the user asked for, with progress as `voice-model`
/// events. Resolves once it is verified and in place, or with why not.
#[tauri::command]
pub async fn voice_model_download(app: AppHandle, webview: Webview) -> Result<model::Status, String> {
    from_settings(&webview)?;
    log("voice: model download started".into());
    let mut last = 0u64;
    let result = model::download(|received| {
        // About one event per megabyte.
        if received - last >= 1 << 20 || received == model::MODEL.size {
            last = received;
            let _ = app.emit("voice-model", model::status());
        }
    })
    .await;
    match &result {
        Ok(()) => log("voice: model installed".into()),
        Err(err) => log(format!("voice: model download stopped: {err}")),
    }
    let status = model::status();
    let _ = app.emit("voice-model", status.clone());
    result.map(|()| status)
}

#[tauri::command]
pub fn voice_model_cancel(webview: Webview) -> Result<(), String> {
    from_settings(&webview)?;
    model::cancel();
    Ok(())
}

#[tauri::command]
pub fn voice_model_delete(app: AppHandle, webview: Webview) -> Result<model::Status, String> {
    from_settings(&webview)?;
    model::delete()?;
    log("voice: model deleted".into());
    let status = model::status();
    let _ = app.emit("voice-model", status.clone());
    Ok(status)
}

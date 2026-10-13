// The assistant inside the app: voice events to the island, and the commands
// the island calls. Every command names the capture it is about, so a click on
// a card that is already stale does nothing.

use tauri::{AppHandle, Emitter, State, Webview};

use serde::Serialize;

use super::contracts::{ActionKind, ActionReceipt, ErrorCode, ValidArgs, VoiceEvent, VOICE_EVENT};
use super::policy::Grant;
use super::voice::{self, model, ChatDraft, Voice};
use crate::island::WINDOW_LABEL;
use crate::Shared;

struct IslandSink(AppHandle);

impl voice::Sink for IslandSink {
    fn emit(&self, event: &VoiceEvent) {
        let _ = self.0.emit_to(WINDOW_LABEL, VOICE_EVENT, event);
    }
}

pub fn log(line: String) {
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

/// "Open <name>": the app the Start menu has under that name, offered on the
/// card for the user to confirm. None when there is no such app.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProposal {
    /// The app as Start names it: what the card shows and the user confirms.
    pub label: String,
    pub grant: Grant,
}

#[tauri::command]
pub fn assistant_app_propose(voice: State<Voice>, capture_id: String, name: String) -> Result<Option<AppProposal>, ErrorCode> {
    #[cfg(windows)]
    {
        let catalog = super::apps::catalog();
        let Some(app) = super::apps::find(&catalog, &name) else {
            log("voice: no app by that name".into());
            return Ok(None);
        };
        let grant = voice.propose(&capture_id, ActionKind::LaunchApp, serde_json::json!({ "appId": app.id }))?;
        Ok(Some(AppProposal { label: app.label.clone(), grant }))
    }
    #[cfg(not(windows))]
    {
        let _ = (voice, capture_id, name);
        Err(ErrorCode::Unavailable)
    }
}

/// The user confirmed the app on the card: Rust checks the grant against the
/// action it kept, finds the app again by its id and opens it, then reports
/// what came of it (and logs it, without the app's name).
#[tauri::command]
pub async fn assistant_app_launch(voice: State<'_, Voice>, capture_id: String, grant: Grant) -> Result<ActionReceipt, ErrorCode> {
    let (action_id, args) = voice.confirm(&capture_id, &grant)?;
    let ValidArgs::LaunchApp(args) = args else { return Err(ErrorCode::InvalidArgs) };
    #[cfg(windows)]
    let receipt = {
        let found = super::apps::catalog().into_iter().find(|e| e.id == args.app_id);
        match found {
            // Gone since it was offered (uninstalled, renamed).
            None => ActionReceipt::failed(&action_id, ErrorCode::Unavailable),
            Some(app) => tauri::async_runtime::spawn_blocking(move || super::launch::open(&action_id, &app.target))
                .await
                .map_err(|_| ErrorCode::Failed)?,
        }
    };
    #[cfg(not(windows))]
    let receipt = {
        let _ = args;
        ActionReceipt::failed(&action_id, ErrorCode::Unavailable)
    };
    voice.receipt(&receipt);
    Ok(receipt)
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

/// Downloads one model the user asked for, with progress as `voice-model`
/// events. Resolves once it is verified and in place, or with why not. When
/// no installed model was picked yet, this one is.
#[tauri::command]
pub async fn voice_model_download(
    app: AppHandle,
    webview: Webview,
    shared: State<'_, Shared>,
    id: String,
) -> Result<model::Status, String> {
    from_settings(&webview)?;
    let chosen = model::find(&id).ok_or("unknown model")?;
    log(format!("voice: model {} download started", chosen.id));
    let mut last = 0u64;
    let result = model::download(chosen.id, |received| {
        // About one event per megabyte.
        if received - last >= 1 << 20 || received == chosen.size {
            last = received;
            let _ = app.emit("voice-model", model::status());
        }
    })
    .await;
    match &result {
        Ok(()) => {
            log(format!("voice: model {} installed", chosen.id));
            let _ = update_choice(&app, &shared, || {
                if model::active().is_none() {
                    model::select(chosen.id).map(Some)
                } else {
                    Ok(None)
                }
            });
        }
        Err(err) => log(format!("voice: model {} download stopped: {err}", chosen.id)),
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
pub fn voice_model_delete(
    app: AppHandle,
    webview: Webview,
    shared: State<'_, Shared>,
    id: String,
) -> Result<model::Status, String> {
    from_settings(&webview)?;
    update_choice(&app, &shared, || {
        model::delete(&id)?;
        match model::replacement_after_delete(&id, model::selected().id, model::installed) {
            Some(next) => model::select(next.id).map(Some),
            None => Ok(None),
        }
    })?;
    log(format!("voice: model {id} deleted"));
    let status = model::status();
    let _ = app.emit("voice-model", status.clone());
    Ok(status)
}

/// Picks an installed model for the next captures.
#[tauri::command]
pub fn voice_model_select(
    app: AppHandle,
    webview: Webview,
    shared: State<'_, Shared>,
    id: String,
) -> Result<model::Status, String> {
    from_settings(&webview)?;
    pick(&app, &shared, &id)?;
    let status = model::status();
    let _ = app.emit("voice-model", status.clone());
    Ok(status)
}

fn pick(app: &AppHandle, shared: &Shared, id: &str) -> Result<(), String> {
    update_choice(app, shared, || model::select(id).map(Some))
}

/// Serialize manual selection, deletion fallback and download completion with
/// the persisted choice. No settings lock is held during a network download.
fn update_choice(
    app: &AppHandle,
    shared: &Shared,
    choose: impl FnOnce() -> Result<Option<&'static model::ModelInfo>, String>,
) -> Result<(), String> {
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        let Some(chosen) = choose()? else { return Ok(()); };
        current.voice_model = chosen.id.to_string();
        if let Err(err) = crate::settings::save(&current) {
            crate::log::line(format!("could not save settings: {err}"));
        }
        current.clone()
    };
    log(format!("voice: model {} picked", updated.voice_model));
    let _ = app.emit("settings-changed", updated);
    Ok(())
}

// The assistant inside the app: voice events to the island, and the commands
// the island calls. Every command names the capture it is about, so a click on
// a card that is already stale does nothing.

use tauri::{AppHandle, Emitter, State, Webview};

use serde::Serialize;

use super::contracts::{ActionKind, ActionReceipt, ErrorCode, ValidArgs, VoiceEvent, VOICE_EVENT};
use super::dictation::{Dictation, Refusal, Rejection, Written};
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
/// whether a capture may start (never over an agent's waiting card). The field
/// of another app that has the keyboard right now is remembered first, before
/// the island takes the keyboard for its card: dictation may go there later.
#[tauri::command]
pub async fn assistant_voice_toggle(voice: State<'_, Voice>, dictation: State<'_, Dictation>) -> Result<Option<String>, ()> {
    voice.cancel_preview();
    dictation.probe().await;
    let id = voice.toggle();
    dictation.bind(id.clone());
    Ok(id)
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
            Some(app) => super::launch::open(action_id, app.target).await?,
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

/// Dictation: the transcript offered for the field that had the keyboard
/// when the capture started, if that field can take it. Rust builds the
/// `insert-text` action from its own transcript and remembered field; the
/// page only ever holds the grant.
#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum InsertOffer {
    /// "Insert into <label>", for the user to confirm.
    Ready { label: String, grant: Grant },
    /// There was a field but it can't take the words (shown on the card).
    Refused { #[serde(flatten)] refusal: Rejection },
}

#[tauri::command]
pub async fn assistant_insert_offer(
    voice: State<'_, Voice>,
    dictation: State<'_, Dictation>,
    capture_id: String,
) -> Result<Option<InsertOffer>, ErrorCode> {
    let (text, expires) = voice.insertion_preview(&capture_id)?;
    let offer = match dictation.offer(&capture_id, expires).await {
        Ok(Some(offer)) => offer,
        // No other app's field had the keyboard: nothing to offer.
        Ok(None) => return Ok(None),
        Err(refusal) => return Ok(Some(InsertOffer::Refused { refusal })),
    };
    let args = serde_json::json!({ "text": text, "targetWindow": offer.target_window, "targetPid": offer.target_pid });
    let grant = voice.propose(&capture_id, ActionKind::InsertText, args)?;
    Ok(Some(InsertOffer::Ready { label: offer.label, grant }))
}

/// Fresh evidence for one clipboard attempt, without proposing or consuming
/// an insertion. The page already holds the transcript; no text is returned.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum InsertCopyCheck {
    Allowed,
    Refused { reason: Refusal },
}

#[tauri::command]
pub async fn assistant_insert_copy_check(
    webview: Webview,
    voice: State<'_, Voice>,
    dictation: State<'_, Dictation>,
    capture_id: String,
) -> Result<InsertCopyCheck, ErrorCode> {
    if webview.label() != WINDOW_LABEL { return Err(ErrorCode::Unavailable); }
    checked_copy_preview(&voice, &capture_id, |expires| dictation.check_copy(&capture_id, expires)).await
}

/// Check ownership on both sides of the await. A native Cancel, replacement,
/// Use, insertion or expiry can invalidate a positive UIA answer meanwhile.
pub(super) async fn checked_copy_preview<F>(
    voice: &Voice,
    capture_id: &str,
    check: impl FnOnce(std::time::Instant) -> F,
) -> Result<InsertCopyCheck, ErrorCode>
where F: std::future::Future<Output = Result<(), Refusal>> {
    let (_, expires) = voice.insertion_preview(capture_id)?;
    let result = check(expires).await;
    if voice.insertion_preview(capture_id)?.1 != expires { return Err(ErrorCode::Expired); }
    Ok(match result {
        Ok(()) => InsertCopyCheck::Allowed,
        Err(reason) => InsertCopyCheck::Refused { reason },
    })
}

/// What came of the user's "Insert into <app>".
#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum InsertResult {
    /// The words are in the field (`succeeded`), or may be (`unknown`).
    Done { receipt: ActionReceipt },
    /// Checked again and refused before anything was written. The
    /// transcript is still on the card, untouched.
    Kept { #[serde(flatten)] refusal: Rejection },
    /// Confirmed but not written: the words went to Coucou's chat field
    /// instead, unsent (`draft`), so they are not lost.
    Failed { receipt: ActionReceipt, draft: Option<ChatDraft> },
}

/// The user confirmed "Insert into <app>". Reserve its preview and grant
/// until the write is terminal, or a definitive pre-write refusal keeps them.
#[tauri::command]
pub async fn assistant_insert_text(
    voice: State<'_, Voice>,
    dictation: State<'_, Dictation>,
    capture_id: String,
    grant: Grant,
) -> Result<InsertResult, ErrorCode> {
    let (action_id, args, permit) = voice.begin_insert(&capture_id, &grant)?;
    let text = args.text.clone();
    let written = dictation.write(&capture_id, args, permit).await;
    voice.finish_insert(&capture_id, &action_id, written.is_err())?;
    let reply = match written {
        Ok(Written::Verified) => match ActionReceipt::succeeded(&action_id, "uia-value:read-back") {
            Some(receipt) => InsertResult::Done { receipt },
            None => InsertResult::Done { receipt: ActionReceipt::unknown(&action_id) },
        },
        // Unverified read-back or write timeout: it may be there already,
        // and must never be written twice.
        Ok(Written::Unconfirmed) => InsertResult::Done { receipt: ActionReceipt::unknown(&action_id) },
        Err(refusal) => InsertResult::Kept { refusal },
        Ok(Written::Failed) => InsertResult::Failed {
            receipt: ActionReceipt::failed(&action_id, ErrorCode::Failed),
            draft: voice.fallback_draft(&capture_id, text).ok(),
        },
    };
    match &reply {
        InsertResult::Done { receipt } | InsertResult::Failed { receipt, .. } => voice.receipt(receipt),
        InsertResult::Kept { .. } => {}
    }
    Ok(reply)
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

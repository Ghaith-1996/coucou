// Speech out, for audio mode: local answers use Windows' own voices
// (Windows.Media.SpeechSynthesis), entirely on this computer. Only an answer
// requested from ChatGPT may use OpenAI's voice, with Windows as its fallback.
// The island sends sentences with their native-bound answer ID, as the answer
// streams in, and plays the clip that comes back.
//
// The voice follows the reply's language (French or English, like the
// transcription): a voice of that language when Windows has one, the
// default voice otherwise. Windows ships English voices; French ones come
// with the French speech pack (Settings → Time & language → Speech).

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Longest text read in one go; the island sends sentences, far shorter.
pub const MAX_CHARS: usize = 1500;

/// Logged once: OpenAI's voice was used.
static OPENAI_USED: AtomicBool = AtomicBool::new(false);

/// One answer's speech policy, captured from the same native settings snapshot
/// that sends its chat request. Neither later settings nor an IPC provider flag
/// may turn a local answer into a remote speech request.
pub struct Turn {
    request_id: String,
    remote: bool,
    active: AtomicBool,
}

impl Turn {
    fn require_current(&self) -> Result<(), String> {
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("speech turn ended".into())
        }
    }
}

/// Only the current answer is retained, including while its completed text is
/// still being played. In-flight clips hold the same revocable policy, so a
/// replacement, stop or navigation invalidates them without retaining history.
#[derive(Default)]
pub struct Turns {
    current: Mutex<Option<Arc<Turn>>>,
}

impl Turns {
    pub fn begin(&self, request_id: Option<&str>, settings: &crate::settings::Settings) -> Option<Arc<Turn>> {
        // Legacy callers may omit the ID, but cannot authorize speech. Keep
        // retained caller-controlled identity bounded and never normalize it.
        let next = request_id.filter(|id| !id.trim().is_empty() && id.len() <= 256).map(|id| Arc::new(Turn {
            request_id: id.to_string(),
            remote: settings.chat_provider == crate::chatgpt::PROVIDER,
            active: AtomicBool::new(true),
        }));
        let mut current = self.current.lock().unwrap();
        Self::retire(&mut current);
        *current = next.clone();
        next
    }

    /// A failed old chat must never erase a replacement's speech identity.
    /// Successful completion deliberately leaves its queued clips authorized.
    pub fn failed(&self, turn: Option<&Arc<Turn>>) {
        let mut current = self.current.lock().unwrap();
        if current.as_ref().zip(turn).is_some_and(|(current, turn)| Arc::ptr_eq(current, turn)) {
            Self::retire(&mut current);
        }
    }

    pub fn clear(&self) {
        Self::retire(&mut self.current.lock().unwrap());
    }

    fn end(&self, request_id: &str) {
        let mut current = self.current.lock().unwrap();
        if current.as_ref().is_some_and(|turn| turn.request_id == request_id) {
            Self::retire(&mut current);
        }
    }

    fn acquire(&self, request_id: Option<&str>) -> Result<Arc<Turn>, String> {
        self.current.lock().unwrap().as_ref()
            .filter(|turn| Some(turn.request_id.as_str()) == request_id)
            .cloned()
            .ok_or_else(|| "speech turn missing or ended".into())
    }

    fn retire(current: &mut Option<Arc<Turn>>) {
        if let Some(turn) = current.take() {
            turn.active.store(false, Ordering::Release);
        }
    }
}

/// French or English, from common words: enough to pick a voice.
pub fn language_of(text: &str) -> &'static str {
    const FRENCH: &[&str] = &[
        "le", "la", "les", "un", "une", "des", "est", "et", "je", "tu", "il", "elle", "nous", "vous", "que", "qui",
        "pas", "pour", "dans", "sur", "avec", "ce", "cette", "c'est", "du", "au", "mais", "ou", "oui", "non", "très",
    ];
    const ENGLISH: &[&str] = &[
        "the", "a", "an", "is", "are", "and", "i", "you", "it", "he", "she", "we", "they", "that", "this", "to", "of",
        "in", "on", "with", "for", "not", "but", "or", "yes", "no", "what", "can", "do",
    ];
    let mut fr = 0;
    let mut en = 0;
    for word in text.split(|c: char| !c.is_alphabetic() && c != '\'' && c != '’') {
        let word = word.to_lowercase().replace('’', "'");
        if word.is_empty() {
            continue;
        }
        if FRENCH.contains(&word.as_str()) {
            fr += 1;
        }
        if ENGLISH.contains(&word.as_str()) {
            en += 1;
        }
        if word.chars().any(|c| matches!(c, 'é' | 'è' | 'ê' | 'à' | 'ç' | 'ù' | 'û' | 'ô' | 'î' | 'œ')) {
            fr += 1;
        }
    }
    if en > fr { "en" } else if fr > 0 { "fr" } else { "en" }
}

/// A voice Windows has, as Settings would show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    pub id: String,
    pub name: String,
    /// BCP-47, "fr-FR", "en-US".
    pub language: String,
}

/// The first voice for `language` ("fr", "en"), else None.
pub fn voice_for<'a>(voices: &'a [Voice], language: &str) -> Option<&'a Voice> {
    voices.iter().find(|v| v.language.to_lowercase().split('-').next() == Some(language))
}

/// One clip to play: a WAV, and what read it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// A `data:audio/wav;base64,…` URL.
    pub audio: String,
    /// The language the text was taken for.
    pub language: String,
    /// False when Windows has no voice of that language and another read it.
    pub matched: bool,
    /// Who read it: "chatgpt" (OpenAI's voice) or "windows".
    pub voice: String,
}

#[cfg(windows)]
mod windows_voices {
    use super::*;
    use windows::core::HSTRING;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    /// Runs `f` on a fresh thread with COM (MTA) of its own.
    fn on_com_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
        std::thread::Builder::new()
            .name("coucou-speech".into())
            .spawn(move || {
                let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                let result = if ok { f() } else { Err("COM unavailable".into()) };
                if ok {
                    unsafe { CoUninitialize() };
                }
                result
            })
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|_| "speech thread failed".to_string())?
    }

    fn installed() -> windows::core::Result<Vec<Voice>> {
        let mut out = Vec::new();
        for v in SpeechSynthesizer::AllVoices()? {
            out.push(Voice { id: v.Id()?.to_string(), name: v.DisplayName()?.to_string(), language: v.Language()?.to_string() });
        }
        Ok(out)
    }

    pub fn voices() -> Result<Vec<Voice>, String> {
        on_com_thread(|| installed().map_err(|e| e.message().to_string()))
    }

    pub fn say(text: String) -> Result<Clip, String> {
        on_com_thread(move || {
            let text: String = text.chars().take(MAX_CHARS).collect();
            let language = language_of(&text);
            let synth = SpeechSynthesizer::new().map_err(|e| e.message().to_string())?;
            let voices = installed().unwrap_or_default();
            let wanted = voice_for(&voices, language).map(|v| v.id.clone());
            let mut matched = false;
            if let Some(id) = wanted {
                for v in SpeechSynthesizer::AllVoices().map_err(|e| e.message().to_string())? {
                    if v.Id().map(|i| i.to_string() == id).unwrap_or(false) {
                        matched = synth.SetVoice(&v).is_ok();
                        break;
                    }
                }
            }
            let wav = (|| -> windows::core::Result<Vec<u8>> {
                let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text.as_str()))?.get()?;
                let size = stream.Size()? as u32;
                let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
                reader.LoadAsync(size)?.get()?;
                let mut bytes = vec![0u8; size as usize];
                reader.ReadBytes(&mut bytes)?;
                Ok(bytes)
            })()
            .map_err(|e| e.message().to_string())?;
            Ok(Clip {
                audio: format!("data:audio/wav;base64,{}", crate::claude::base64_for(&wav)),
                language: language.into(),
                matched,
                voice: "windows".into(),
            })
        })
    }
}

#[cfg(windows)]
pub use windows_voices::say;
#[cfg(windows)]
#[allow(unused_imports)]
pub use windows_voices::voices;

/// Reads `text` aloud into a clip for the island to play. Only the island asks.
#[tauri::command]
pub async fn speech_say(
    webview: tauri::Webview,
    speech: tauri::State<'_, Turns>,
    text: String,
    request_id: Option<String>,
) -> Result<Clip, String> {
    if webview.label() != crate::island::WINDOW_LABEL {
        return Err("only from the island".into());
    }
    if text.trim().is_empty() {
        return Err("nothing to say".into());
    }
    let turn = speech.acquire(request_id.as_deref())?;
    turn.require_current()?;
    // Only the provider that answered this turn may authorize OpenAI's voice.
    // Checking current settings here would leak a local answer on picker change.
    if turn.remote && crate::chatgpt::speech_available().await {
        turn.require_current()?;
        let text: String = text.chars().take(MAX_CHARS).collect();
        match crate::chatgpt::speak(&text, || turn.require_current()).await {
            Ok(mp3) => {
                turn.require_current()?;
                if !OPENAI_USED.swap(true, Ordering::SeqCst) {
                    crate::log::line("speech: ChatGPT's voice in use".to_string());
                }
                return Ok(Clip {
                    audio: format!("data:audio/mpeg;base64,{}", crate::claude::base64_for(&mp3)),
                    language: language_of(&text).into(),
                    matched: true,
                    voice: "chatgpt".into(),
                });
            }
            Err(_) => {} // Windows' voice reads it instead.
        }
    }
    // A retired remote attempt must not continue by falling back to Windows.
    turn.require_current()?;
    #[cfg(windows)]
    {
        let queued = turn.clone();
        let clip = tauri::async_runtime::spawn_blocking(move || {
            queued.require_current()?;
            say(text)
        }).await.map_err(|e| e.to_string())??;
        turn.require_current()?;
        Ok(clip)
    }
    #[cfg(not(windows))]
    {
        Err("speech is only on Windows".into())
    }
}

/// Playback can end after chat completion. Scope this to its answer so a late
/// audio cleanup cannot retire a newer typed or spoken request.
#[tauri::command]
pub fn speech_end(webview: tauri::Webview, speech: tauri::State<'_, Turns>, request_id: String) -> Result<(), String> {
    if webview.label() != crate::island::WINDOW_LABEL {
        return Err("only from the island".into());
    }
    speech.end(&request_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(provider: &str) -> crate::settings::Settings {
        crate::settings::Settings { chat_provider: provider.into(), ..Default::default() }
    }

    #[test]
    fn every_local_chunk_keeps_the_native_answer_snapshot_after_picker_changes() {
        for provider in ["ollama", "lmstudio", "openai-compatible", crate::chat::ANTHROPIC] {
            let turns = Turns::default();
            let mut current_settings = settings(provider);
            let answer = turns.begin(Some("local-answer"), &current_settings).unwrap();
            assert!(!turns.acquire(Some("local-answer")).unwrap().remote);
            current_settings.chat_provider = crate::chatgpt::PROVIDER.into();
            assert_eq!(current_settings.chat_provider, crate::chatgpt::PROVIDER);
            for _ in 0..3 {
                let chunk = turns.acquire(Some("local-answer")).unwrap();
                assert!(Arc::ptr_eq(&answer, &chunk));
                assert!(!chunk.remote, "changing the picker cannot export a local answer");
            }
        }
    }

    #[test]
    fn successful_completion_keeps_queued_clips_and_the_original_provider() {
        let turns = Turns::default();
        let mut current_settings = settings(crate::chatgpt::PROVIDER);
        let answer = turns.begin(Some("remote-answer"), &current_settings).unwrap();
        current_settings.chat_provider = "ollama".into();
        assert_eq!(current_settings.chat_provider, "ollama");
        // chat_send drops its handle on success, with queued playback remaining.
        drop(answer);
        let chunk = turns.acquire(Some("remote-answer")).unwrap();
        assert!(chunk.remote);
        assert!(chunk.require_current().is_ok());
        turns.end("remote-answer");
        assert!(turns.acquire(Some("remote-answer")).is_err());
        assert!(chunk.require_current().is_err());
    }

    #[test]
    fn speech_requires_the_matching_bounded_native_request_identity() {
        let turns = Turns::default();
        assert!(turns.acquire(None).is_err());
        turns.begin(Some("known"), &settings(crate::chatgpt::PROVIDER));
        for id in [None, Some(""), Some("unknown"), Some(" known ")] {
            assert!(turns.acquire(id).is_err());
        }
        let oversized = "x".repeat(257);
        for id in [None, Some(""), Some("  "), Some(oversized.as_str())] {
            let previous = turns.begin(Some("known"), &settings(crate::chatgpt::PROVIDER)).unwrap();
            assert!(turns.begin(id, &settings(crate::chatgpt::PROVIDER)).is_none());
            assert!(turns.acquire(Some("known")).is_err());
            assert!(previous.require_current().is_err());
        }
    }

    #[test]
    fn replacement_revokes_in_flight_clips_and_old_failure_cannot_clear_new_turn() {
        let turns = Turns::default();
        let old = turns.begin(Some("old"), &settings(crate::chatgpt::PROVIDER)).unwrap();
        let new = turns.begin(Some("new"), &settings("ollama")).unwrap();
        assert!(old.require_current().is_err());
        assert!(turns.acquire(Some("old")).is_err());
        turns.failed(Some(&old));
        assert!(Arc::ptr_eq(&new, &turns.acquire(Some("new")).unwrap()));
        assert!(new.require_current().is_ok());
        assert!(!new.remote);
        turns.failed(Some(&new));
        assert!(new.require_current().is_err());
        assert!(turns.acquire(Some("new")).is_err());
    }

    #[test]
    fn old_failure_is_guarded_by_native_identity_even_if_caller_reuses_an_id() {
        let turns = Turns::default();
        let old = turns.begin(Some("reused"), &settings(crate::chatgpt::PROVIDER)).unwrap();
        let new = turns.begin(Some("reused"), &settings("ollama")).unwrap();
        turns.failed(Some(&old));
        assert!(old.require_current().is_err());
        assert!(Arc::ptr_eq(&new, &turns.acquire(Some("reused")).unwrap()));
        assert!(new.require_current().is_ok());
        assert!(!new.remote);
    }

    #[test]
    fn late_scoped_end_cannot_retire_newer_speech_but_stop_and_reset_do() {
        let turns = Turns::default();
        turns.begin(Some("old"), &settings(crate::chatgpt::PROVIDER));
        let new = turns.begin(Some("new"), &settings(crate::chatgpt::PROVIDER)).unwrap();
        turns.end("old");
        assert!(new.require_current().is_ok());
        assert!(turns.acquire(Some("new")).is_ok());
        turns.clear();
        assert!(new.require_current().is_err());
        assert!(turns.acquire(Some("new")).is_err());
    }

    #[tokio::test]
    async fn retirement_during_async_access_fails_the_pre_send_authorization_check() {
        let turns = Turns::default();
        let turn = turns.begin(Some("remote"), &settings(crate::chatgpt::PROVIDER)).unwrap();
        let (ready, waiting) = tokio::sync::oneshot::channel::<()>();
        let pending = tokio::spawn(async move {
            turn.require_current()?;
            let _ = waiting.await;
            // The same callback checked inside chatgpt::speak after access-token
            // acquisition and before both its original request and 401 retry.
            turn.require_current()
        });
        tokio::task::yield_now().await;
        turns.end("remote");
        let _ = ready.send(());
        assert!(pending.await.unwrap().is_err());
    }

    #[test]
    fn french_and_english_are_told_apart() {
        assert_eq!(language_of("Bonjour, c'est une très bonne idée pour le projet."), "fr");
        assert_eq!(language_of("Sure, that is a good idea for the project."), "en");
        assert_eq!(language_of("Clique sur l’onglet à gauche."), "fr");
        assert_eq!(language_of("12:45"), "en", "no words: the default");
    }

    #[test]
    fn a_voice_is_picked_by_its_language() {
        let voices = vec![
            Voice { id: "a".into(), name: "Microsoft David".into(), language: "en-US".into() },
            Voice { id: "b".into(), name: "Microsoft Hortense".into(), language: "fr-FR".into() },
        ];
        assert_eq!(voice_for(&voices, "fr").unwrap().id, "b");
        assert_eq!(voice_for(&voices, "en").unwrap().id, "a");
        assert!(voice_for(&voices[..1], "fr").is_none());
    }

    /// Opt-in: reads a sentence with this computer's voices (no sound plays).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_reads_a_sentence_into_a_wav() {
        println!("voices: {:?}", voices().unwrap());
        let clip = say("Hello, this is Coucou speaking.".into()).unwrap();
        assert!(clip.audio.starts_with("data:audio/wav;base64,UklGR"), "a RIFF header");
        println!("{} chars of audio, language {}, matched {}", clip.audio.len(), clip.language, clip.matched);
    }
}

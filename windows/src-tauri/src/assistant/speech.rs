// Speech out, for audio mode: a reply read aloud with Windows' own voices
// (Windows.Media.SpeechSynthesis), on this computer, nothing downloaded and
// nothing sent anywhere. The island sends one sentence or two at a time, as
// the answer streams in, and plays the WAV that comes back.
//
// The voice follows the reply's language (French or English, like the
// transcription): a voice of that language when Windows has one, the
// default voice otherwise. Windows ships English voices; French ones come
// with the French speech pack (Settings → Time & language → Speech).

use serde::Serialize;

/// Longest text read in one go; the island sends sentences, far shorter.
pub const MAX_CHARS: usize = 1500;

/// Logged once: OpenAI's voice was used.
static OPENAI_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    shared: tauri::State<'_, crate::Shared>,
    text: String,
) -> Result<Clip, String> {
    if webview.label() != crate::island::WINDOW_LABEL {
        return Err("only from the island".into());
    }
    if text.trim().is_empty() {
        return Err("nothing to say".into());
    }
    // OpenAI's voice when the chat is on ChatGPT and the sign-in allows it.
    let on_chatgpt = shared.settings.lock().unwrap().chat_provider == crate::chatgpt::PROVIDER;
    if on_chatgpt && crate::chatgpt::speech_available() {
        let text: String = text.chars().take(MAX_CHARS).collect();
        match crate::chatgpt::speak(&text).await {
            Ok(mp3) => {
                if !OPENAI_USED.swap(true, std::sync::atomic::Ordering::SeqCst) {
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
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || say(text)).await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        Err("speech is only on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

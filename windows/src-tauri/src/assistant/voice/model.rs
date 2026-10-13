// The speech models on disk: whisper.cpp models the user downloads from
// Settings → Voice, one click each, and picks between. Nothing is downloaded
// otherwise, and nothing else is ever sent anywhere: the model runs on this
// computer (whisper.rs).
//
// Each file is fetched from its address below, written next to its final name
// and only moved into place once its size and SHA-256 match the values pinned
// here. A file that doesn't match is deleted, never used.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use ring::digest::{Context, SHA256};
use serde::Serialize;

pub struct ModelInfo {
    /// What settings.json stores; never renamed.
    pub id: &'static str,
    pub label: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// From the whisper.cpp project (MIT), multilingual. Sizes and SHA-256 as
/// Hugging Face lists them for each file.
pub const MODELS: [ModelInfo; 3] = [
    ModelInfo {
        id: "base",
        label: "Whisper base",
        file: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    ModelInfo {
        id: "large-v3-turbo-q5_0",
        label: "Whisper large-v3 turbo (q5_0)",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    ModelInfo {
        id: "large-v3-q5_0",
        label: "Whisper large-v3 (q5_0)",
        file: "ggml-large-v3-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin",
        size: 1_081_140_203,
        sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
    },
];

pub const DEFAULT_MODEL: &str = "base";

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn dir() -> PathBuf {
    crate::settings::local_dir().join("models")
}

pub fn path(model: &ModelInfo) -> PathBuf {
    dir().join(model.file)
}

/// In place, at its full size. (Its hash was checked when it was downloaded;
/// nothing else writes there.)
pub fn installed(model: &ModelInfo) -> bool {
    std::fs::metadata(path(model)).map(|m| m.is_file() && m.len() == model.size).unwrap_or(false)
}

// ── Which one is used ────────────────────────────────────────────────────────

static SELECTED: Mutex<String> = Mutex::new(String::new());

/// The choice from settings.json, at launch. An unknown id is the default.
pub fn init(selected: &str) {
    let id = find(selected).map_or(DEFAULT_MODEL, |m| m.id);
    *SELECTED.lock().unwrap() = id.to_string();
}

pub fn selected() -> &'static ModelInfo {
    let id = SELECTED.lock().unwrap().clone();
    find(&id).unwrap_or(&MODELS[0])
}

/// The model to transcribe with: the selected one, when it is installed.
pub fn active() -> Option<&'static ModelInfo> {
    let model = selected();
    installed(model).then_some(model)
}

/// Only an installed model can be picked.
pub fn select(id: &str) -> Result<&'static ModelInfo, String> {
    let model = find(id).ok_or("unknown model")?;
    if !installed(model) {
        return Err("download it first".into());
    }
    *SELECTED.lock().unwrap() = model.id.to_string();
    Ok(model)
}

// ── Status for Settings ──────────────────────────────────────────────────────

static DOWNLOADING: Mutex<Option<&'static str>> = Mutex::new(None);
static CANCEL: AtomicBool = AtomicBool::new(false);
static RECEIVED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub file: &'static str,
    pub size: u64,
    /// Where it comes from, shown before the user downloads it.
    pub source: &'static str,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub models: Vec<ModelStatus>,
    pub selected: &'static str,
    /// The model being downloaded, if any, and how far it got.
    pub downloading: Option<&'static str>,
    pub received: u64,
}

pub fn status() -> Status {
    Status {
        models: MODELS
            .iter()
            .map(|m| ModelStatus {
                id: m.id,
                label: m.label,
                file: m.file,
                size: m.size,
                source: m.url,
                installed: installed(m),
            })
            .collect(),
        selected: selected().id,
        downloading: *DOWNLOADING.lock().unwrap(),
        received: RECEIVED.load(Ordering::SeqCst),
    }
}

pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn delete(id: &str) -> Result<(), String> {
    let model = find(id).ok_or("unknown model")?;
    if *DOWNLOADING.lock().unwrap() == Some(model.id) {
        return Err("a download is running".into());
    }
    match std::fs::remove_file(path(model)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Checks what was received against the pinned size and hash.
pub fn verify(model: &ModelInfo, len: u64, sha256_hex: &str) -> Result<(), String> {
    if len != model.size {
        return Err(format!("unexpected size ({len} bytes)"));
    }
    if sha256_hex != model.sha256 {
        return Err("the file's checksum doesn't match".into());
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads one model. `progress` gets the bytes received so far. One
/// download at a time; `cancel` stops it and removes the partial file.
pub async fn download(id: &str, mut progress: impl FnMut(u64)) -> Result<(), String> {
    let model = find(id).ok_or("unknown model")?;
    {
        let mut running = DOWNLOADING.lock().unwrap();
        if running.is_some() {
            return Err("already downloading".into());
        }
        *running = Some(model.id);
    }
    CANCEL.store(false, Ordering::SeqCst);
    RECEIVED.store(0, Ordering::SeqCst);
    let part = dir().join(format!("{}.part", model.file));
    let result = fetch(model, &part, &mut progress).await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    *DOWNLOADING.lock().unwrap() = None;
    result
}

async fn fetch(model: &ModelInfo, part: &std::path::Path, progress: &mut impl FnMut(u64)) -> Result<(), String> {
    crate::platform::ensure_private_dir(&dir()).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(model.url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let mut file = std::fs::File::create(part).map_err(|e| e.to_string())?;
    let mut hash = Context::new(&SHA256);
    let mut len: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if CANCEL.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        len += chunk.len() as u64;
        // Never more than the pinned size: a bigger answer is not our file.
        if len > model.size {
            return Err("the file is bigger than expected".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        RECEIVED.store(len, Ordering::SeqCst);
        progress(len);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    verify(model, len, &hex(hash.finish().as_ref()))?;
    std::fs::rename(part, path(model)).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pinned_file_passes() {
        for model in &MODELS {
            assert!(verify(model, model.size, model.sha256).is_ok());
            assert!(verify(model, model.size - 1, model.sha256).is_err());
            assert!(verify(model, model.size, &"0".repeat(64)).is_err());
        }
        // One model's hash never passes for another.
        assert!(verify(&MODELS[1], MODELS[1].size, MODELS[0].sha256).is_err());
    }

    #[test]
    fn every_pin_is_well_formed_and_unique() {
        let mut ids = std::collections::HashSet::new();
        for model in &MODELS {
            assert!(ids.insert(model.id), "{}", model.id);
            assert_eq!(model.sha256.len(), 64);
            assert!(model.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            assert!(model.url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/"));
            assert!(model.url.ends_with(model.file));
            assert!(model.file.starts_with("ggml-") && model.file.ends_with(".bin"));
        }
        assert_eq!(MODELS[0].id, DEFAULT_MODEL);
    }

    #[test]
    fn an_unknown_choice_is_the_default_and_only_installed_models_are_picked() {
        init("no-such-model");
        assert_eq!(selected().id, DEFAULT_MODEL);
        assert!(select("no-such-model").is_err());
    }

    #[test]
    fn a_hash_is_hex_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}

// The speech model on disk: whisper.cpp's multilingual "base" model, which
// the user downloads from Settings → Voice with one click. Nothing is
// downloaded otherwise, and nothing else is ever sent anywhere: the model
// runs on this computer (whisper.rs).
//
// The file is fetched from the address below, written next to its final name
// and only moved into place once its size and SHA-256 match the values pinned
// here. A file that doesn't match is deleted, never used.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ring::digest::{Context, SHA256};
use serde::Serialize;

pub struct ModelInfo {
    pub file: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// ggml-base.bin from the whisper.cpp project (MIT). Size and SHA-256 as
/// Hugging Face lists them for that file.
pub const MODEL: ModelInfo = ModelInfo {
    file: "ggml-base.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    size: 147_951_465,
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
};

pub fn dir() -> PathBuf {
    crate::settings::local_dir().join("models")
}

pub fn path() -> PathBuf {
    dir().join(MODEL.file)
}

/// The model is in place, at its full size. (Its hash was checked when it
/// was downloaded; nothing else writes there.)
pub fn installed() -> bool {
    std::fs::metadata(path()).map(|m| m.is_file() && m.len() == MODEL.size).unwrap_or(false)
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static RECEIVED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub installed: bool,
    pub downloading: bool,
    pub received: u64,
    pub size: u64,
    /// Where it comes from, shown before the user downloads it.
    pub source: &'static str,
}

pub fn status() -> Status {
    Status {
        installed: installed(),
        downloading: DOWNLOADING.load(Ordering::SeqCst),
        received: RECEIVED.load(Ordering::SeqCst),
        size: MODEL.size,
        source: MODEL.url,
    }
}

pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn delete() -> Result<(), String> {
    if DOWNLOADING.load(Ordering::SeqCst) {
        return Err("a download is running".into());
    }
    match std::fs::remove_file(path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Checks what was received against the pinned size and hash.
pub fn verify(len: u64, sha256_hex: &str) -> Result<(), String> {
    if len != MODEL.size {
        return Err(format!("unexpected size ({len} bytes)"));
    }
    if sha256_hex != MODEL.sha256 {
        return Err("the file's checksum doesn't match".into());
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads the model. `progress` is called with the bytes received so far.
/// Only one download at a time; `cancel` stops it and removes the partial file.
pub async fn download(mut progress: impl FnMut(u64)) -> Result<(), String> {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err("already downloading".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    RECEIVED.store(0, Ordering::SeqCst);
    let part = dir().join(format!("{}.part", MODEL.file));
    let result = fetch(&part, &mut progress).await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    DOWNLOADING.store(false, Ordering::SeqCst);
    result
}

async fn fetch(part: &std::path::Path, progress: &mut impl FnMut(u64)) -> Result<(), String> {
    crate::platform::ensure_private_dir(&dir()).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(MODEL.url).send().await.map_err(|e| e.to_string())?;
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
        if len > MODEL.size {
            return Err("the file is bigger than expected".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        RECEIVED.store(len, Ordering::SeqCst);
        progress(len);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    verify(len, &hex(hash.finish().as_ref()))?;
    std::fs::rename(part, path()).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pinned_file_passes() {
        assert!(verify(MODEL.size, MODEL.sha256).is_ok());
        assert!(verify(MODEL.size - 1, MODEL.sha256).is_err());
        assert!(verify(MODEL.size, &"0".repeat(64)).is_err());
    }

    #[test]
    fn the_pin_is_well_formed() {
        assert_eq!(MODEL.sha256.len(), 64);
        assert!(MODEL.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(MODEL.url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/"));
        assert!(MODEL.url.ends_with(MODEL.file));
    }

    #[test]
    fn a_hash_is_hex_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}

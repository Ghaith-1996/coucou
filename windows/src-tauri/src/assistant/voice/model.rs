// The speech models on disk: whisper.cpp models the user downloads from
// Settings → Voice, one click each, and picks between. Nothing is downloaded
// otherwise, and nothing else is ever sent anywhere: the model runs on this
// computer (whisper.rs).
//
// Each file is fetched from its address below, written next to its final name
// and only moved into place once its size and SHA-256 match the values pinned
// here. A file that doesn't match is deleted, never used.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use ring::digest::{Context, SHA256};
use serde::Serialize;
use tokio::sync::watch;

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
    require_supported()?;
    let model = find(id).ok_or("unknown model")?;
    if !installed(model) {
        return Err("download it first".into());
    }
    *SELECTED.lock().unwrap() = model.id.to_string();
    Ok(model)
}

/// After deleting the selected model, prefer the first remaining installed
/// model in catalog order. Deleting any other model must keep the user's choice.
/// The predicate keeps this decision testable without model files or downloads.
pub fn replacement_after_delete(
    deleted: &str,
    selected: &str,
    is_installed: impl Fn(&ModelInfo) -> bool,
) -> Option<&'static ModelInfo> {
    if deleted != selected {
        return None;
    }
    MODELS.iter().find(|model| model.id != deleted && is_installed(model))
}

// ── Status for Settings ──────────────────────────────────────────────────────

static DOWNLOAD: DownloadState = DownloadState::new();
static RECEIVED: AtomicU64 = AtomicU64::new(0);

/// Publish ownership and its cancellation signal together, so a cancellation
/// cannot be lost between claiming the download and initializing its signal.
struct DownloadState {
    active: Mutex<Option<(&'static str, watch::Sender<bool>)>>,
}

impl DownloadState {
    const fn new() -> Self {
        Self { active: Mutex::new(None) }
    }

    fn downloading(&self) -> Option<&'static str> {
        self.active.lock().unwrap().as_ref().map(|(id, _)| *id)
    }

    fn cancel(&self) {
        if let Some((_, cancel)) = self.active.lock().unwrap().as_ref() {
            let _ = cancel.send(true);
        }
    }

    fn start<'a>(
        &'a self,
        id: &'static str,
        part: &'a Path,
    ) -> Result<(DownloadGuard<'a>, watch::Receiver<bool>), String> {
        let mut active = self.active.lock().unwrap();
        if active.is_some() {
            return Err("already downloading".into());
        }
        let (cancel, receiver) = watch::channel(false);
        *active = Some((id, cancel));
        Ok((DownloadGuard { state: self, part }, receiver))
    }
}

/// The fetch future (and its open file) drops before this guard. Cleanup also
/// runs if the command future is dropped, and finishes before another download
/// can acquire the same partial-file path. A successful rename leaves no part.
struct DownloadGuard<'a> {
    state: &'a DownloadState,
    part: &'a Path,
}

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.part);
        let _ = self.state.active.lock().unwrap().take();
    }
}

/// Race every network wait, including response headers and a stalled body,
/// against a wakeable signal. An already requested cancellation wins even
/// when the network result is ready in the same poll.
async fn cancellable<T>(cancel: &mut watch::Receiver<bool>, operation: impl Future<Output = T>) -> Result<T, String> {
    if *cancel.borrow() {
        return Err("cancelled".into());
    }
    tokio::select! {
        biased;
        _ = cancel.changed() => Err("cancelled".into()),
        result = operation => Ok(result),
    }
}

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
    /// This build has both the microphone adapter and local transcriber.
    pub supported: bool,
    pub models: Vec<ModelStatus>,
    pub selected: &'static str,
    /// The model being downloaded, if any, and how far it got.
    pub downloading: Option<&'static str>,
    pub received: u64,
}

pub fn status() -> Status {
    Status {
        supported: cfg!(windows),
        models: MODELS
            .iter()
            .map(|m| ModelStatus {
                id: m.id,
                label: m.label,
                file: m.file,
                size: m.size,
                source: m.url,
                installed: cfg!(windows) && installed(m),
            })
            .collect(),
        selected: selected().id,
        downloading: DOWNLOAD.downloading(),
        received: RECEIVED.load(Ordering::SeqCst),
    }
}

/// UI visibility is not an authority boundary: model commands also refuse
/// unsupported builds before they can touch files or start a download.
pub fn require_supported() -> Result<(), String> {
    if cfg!(windows) {
        Ok(())
    } else {
        Err("local speech recognition is only supported on Windows".into())
    }
}

pub fn cancel() {
    DOWNLOAD.cancel();
}

pub fn delete(id: &str) -> Result<(), String> {
    require_supported()?;
    let model = find(id).ok_or("unknown model")?;
    if DOWNLOAD.downloading() == Some(model.id) {
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
    require_supported()?;
    let model = find(id).ok_or("unknown model")?;
    let part = dir().join(format!("{}.part", model.file));
    let (_guard, mut cancel) = DOWNLOAD.start(model.id, &part)?;
    RECEIVED.store(0, Ordering::SeqCst);
    fetch(model, &part, &mut progress, &mut cancel).await
}

async fn fetch(
    model: &ModelInfo,
    part: &Path,
    progress: &mut impl FnMut(u64),
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    crate::platform::ensure_private_dir(&dir()).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        // Reset on each successful read, rather than limiting a slow but
        // progressing download of the whole model.
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = cancellable(cancel, client.get(model.url).send()).await?.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let mut file = std::fs::File::create(part).map_err(|e| e.to_string())?;
    let mut hash = Context::new(&SHA256);
    let mut len: u64 = 0;
    while let Some(chunk) = cancellable(cancel, response.chunk()).await?.map_err(|e| e.to_string())? {
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
    if *cancel.borrow() {
        return Err("cancelled".into());
    }
    // std::fs::rename replaces an existing regular file on Windows too; do
    // not delete the destination first and risk losing it if installation fails.
    std::fs::rename(part, path(model)).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "coucou-model-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The same ownership order as download → fetch, with a tiny local file
    /// and a permanently pending future instead of any network/model access.
    async fn stalled_download(
        state: &DownloadState,
        id: &'static str,
        part: &Path,
        started: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), String> {
        let (_guard, mut cancel) = state.start(id, part)?;
        let mut file = std::fs::File::create(part).unwrap();
        file.write_all(b"partial").unwrap();
        started.send(()).unwrap();
        cancellable(&mut cancel, std::future::pending::<()>()).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_interrupts_a_stalled_download_and_releases_ownership() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let destination = dir.0.join("model.bin");
        std::fs::write(&destination, b"previous file").unwrap();
        let state = std::sync::Arc::new(DownloadState::new());
        let (started, ready) = tokio::sync::oneshot::channel();
        let task_state = state.clone();
        let task_part = part.clone();
        let download = tokio::spawn(async move { stalled_download(&task_state, MODELS[0].id, &task_part, started).await });
        ready.await.unwrap();
        assert_eq!(state.downloading(), Some(MODELS[0].id));
        assert!(part.exists());
        // A rejected second caller must neither remove the first caller's
        // partial file nor release its ownership.
        assert!(state.start(MODELS[1].id, &part).is_err());
        assert!(part.exists());
        assert_eq!(state.downloading(), Some(MODELS[0].id));
        state.cancel();
        // Await only the task's JoinHandle: the cancellation signal must wake
        // the suspended task, rather than a test manually polling it again.
        let result = tokio::time::timeout(Duration::from_secs(1), download).await.unwrap().unwrap();
        assert_eq!(result, Err("cancelled".into()));
        assert!(!part.exists());
        assert_eq!(state.downloading(), None);
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous file");

        // Cancellation belongs to that attempt, never the next download.
        let (_guard, mut cancel) = state.start(MODELS[1].id, &part).unwrap();
        assert_eq!(state.downloading(), Some(MODELS[1].id));
        assert_eq!(cancellable(&mut cancel, std::future::ready(7)).await, Ok(7));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_the_download_future_removes_its_partial_file() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let state = DownloadState::new();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut download = Box::pin(stalled_download(&state, MODELS[0].id, &part, started));
        tokio::select! {
            _ = &mut download => panic!("the synthetic network wait must stall"),
            result = ready => result.unwrap(),
        }
        assert!(part.exists());
        drop(download);
        assert!(!part.exists());
        assert_eq!(state.downloading(), None);
        assert!(state.start(MODELS[1].id, &part).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_before_a_wait_wins_over_a_ready_result() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let state = DownloadState::new();
        let (_guard, mut cancel) = state.start(MODELS[0].id, &part).unwrap();
        state.cancel();
        assert_eq!(cancellable(&mut cancel, std::future::ready(7)).await, Err("cancelled".into()));
    }

    #[test]
    fn installing_replaces_an_existing_wrong_size_file() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let destination = dir.0.join("model.bin");
        let state = DownloadState::new();
        let (guard, _cancel) = state.start(MODELS[0].id, &part).unwrap();
        std::fs::write(&destination, b"bad").unwrap();
        std::fs::write(&part, b"verified replacement").unwrap();
        // Exercise the same std operation as fetch on Windows and Unix.
        std::fs::rename(&part, &destination).unwrap();
        drop(guard);
        assert_eq!(std::fs::read(&destination).unwrap(), b"verified replacement");
        assert!(!part.exists());
        assert_eq!(state.downloading(), None);
    }

    #[test]
    fn local_model_support_matches_the_compiled_backend() {
        assert_eq!(status().supported, cfg!(windows));
        assert_eq!(require_supported().is_ok(), cfg!(windows));
        #[cfg(not(windows))]
        assert!(status().models.iter().all(|model| !model.installed));
    }

    #[cfg(not(windows))]
    #[tokio::test(flavor = "current_thread")]
    async fn unsupported_model_commands_refuse_before_resolving_a_model() {
        let expected = Err("local speech recognition is only supported on Windows".into());
        assert_eq!(select("no-such-model").map(|_| ()), expected);
        assert_eq!(delete("no-such-model"), expected);
        assert_eq!(download("no-such-model", |_| panic!("unsupported downloads must never progress")).await, expected);
    }

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
    fn deleting_the_selected_model_uses_the_first_installed_replacement() {
        for (deleted, installed, expected) in [
            ("large-v3-turbo-q5_0", vec!["base", "large-v3-q5_0"], "base"),
            ("base", vec!["large-v3-turbo-q5_0", "large-v3-q5_0"], "large-v3-turbo-q5_0"),
            ("large-v3-turbo-q5_0", vec!["large-v3-q5_0"], "large-v3-q5_0"),
        ] {
            let next = replacement_after_delete(deleted, deleted, |model| installed.contains(&model.id));
            assert_eq!(next.map(|model| model.id), Some(expected));
        }
    }

    #[test]
    fn deletion_does_not_replace_another_choice_or_choose_a_missing_model() {
        assert!(replacement_after_delete("base", "large-v3-turbo-q5_0", |_| true).is_none());
        assert!(replacement_after_delete("base", "base", |_| false).is_none());
        assert!(replacement_after_delete("base", "base", |model| model.id == "base").is_none());
    }

    #[test]
    fn a_hash_is_hex_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}

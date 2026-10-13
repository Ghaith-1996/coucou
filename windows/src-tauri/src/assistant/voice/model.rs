// The speech model on disk: whisper.cpp's multilingual "base" model, which
// the user downloads from Settings → Voice with one click. Nothing is
// downloaded otherwise, and nothing else is ever sent anywhere: the model
// runs on this computer (whisper.rs).
//
// The file is fetched from the address below, written next to its final name
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

static DOWNLOAD: DownloadState = DownloadState::new();
static RECEIVED: AtomicU64 = AtomicU64::new(0);

/// Publish ownership and its cancellation signal together, so a cancellation
/// cannot be lost between claiming the download and initializing its signal.
struct DownloadState {
    active: Mutex<Option<watch::Sender<bool>>>,
}

impl DownloadState {
    const fn new() -> Self {
        Self { active: Mutex::new(None) }
    }

    fn downloading(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    fn cancel(&self) {
        if let Some(cancel) = self.active.lock().unwrap().as_ref() {
            let _ = cancel.send(true);
        }
    }

    fn start<'a>(&'a self, part: &'a Path) -> Result<(DownloadGuard<'a>, watch::Receiver<bool>), String> {
        let mut active = self.active.lock().unwrap();
        if active.is_some() {
            return Err("already downloading".into());
        }
        let (cancel, receiver) = watch::channel(false);
        *active = Some(cancel);
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
pub struct Status {
    /// This build has both the microphone adapter and local transcriber.
    pub supported: bool,
    pub installed: bool,
    pub downloading: bool,
    pub received: u64,
    pub size: u64,
    /// Where it comes from, shown before the user downloads it.
    pub source: &'static str,
}

pub fn status() -> Status {
    Status {
        supported: cfg!(windows),
        installed: cfg!(windows) && installed(),
        downloading: DOWNLOAD.downloading(),
        received: RECEIVED.load(Ordering::SeqCst),
        size: MODEL.size,
        source: MODEL.url,
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

pub fn delete() -> Result<(), String> {
    require_supported()?;
    if DOWNLOAD.downloading() {
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
    require_supported()?;
    let part = dir().join(format!("{}.part", MODEL.file));
    let (_guard, mut cancel) = DOWNLOAD.start(&part)?;
    RECEIVED.store(0, Ordering::SeqCst);
    fetch(&part, &mut progress, &mut cancel).await
}

async fn fetch(part: &Path, progress: &mut impl FnMut(u64), cancel: &mut watch::Receiver<bool>) -> Result<(), String> {
    crate::platform::ensure_private_dir(&dir()).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        // Reset on each successful read, rather than limiting a slow but
        // progressing download of the whole model.
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = cancellable(cancel, client.get(MODEL.url).send()).await?.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let mut file = std::fs::File::create(part).map_err(|e| e.to_string())?;
    let mut hash = Context::new(&SHA256);
    let mut len: u64 = 0;
    while let Some(chunk) = cancellable(cancel, response.chunk()).await?.map_err(|e| e.to_string())? {
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
    if *cancel.borrow() {
        return Err("cancelled".into());
    }
    // std::fs::rename replaces an existing regular file on Windows too; do
    // not delete the destination first and risk losing it if installation fails.
    std::fs::rename(part, path()).map_err(|e| e.to_string())?;
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
        part: &Path,
        started: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), String> {
        let (_guard, mut cancel) = state.start(part)?;
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
        let download = tokio::spawn(async move { stalled_download(&task_state, &task_part, started).await });
        ready.await.unwrap();
        assert!(state.downloading());
        assert!(part.exists());
        // A rejected second caller must neither remove the first caller's
        // partial file nor release its ownership.
        assert!(state.start(&part).is_err());
        assert!(part.exists());
        assert!(state.downloading());
        state.cancel();
        // Await only the task's JoinHandle: the cancellation signal must wake
        // the suspended task, rather than a test manually polling it again.
        let result = tokio::time::timeout(Duration::from_secs(1), download).await.unwrap().unwrap();
        assert_eq!(result, Err("cancelled".into()));
        assert!(!part.exists());
        assert!(!state.downloading());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous file");

        // Cancellation belongs to that attempt, never the next download.
        let (_guard, mut cancel) = state.start(&part).unwrap();
        assert_eq!(cancellable(&mut cancel, std::future::ready(7)).await, Ok(7));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_the_download_future_removes_its_partial_file() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let state = DownloadState::new();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut download = Box::pin(stalled_download(&state, &part, started));
        tokio::select! {
            _ = &mut download => panic!("the synthetic network wait must stall"),
            result = ready => result.unwrap(),
        }
        assert!(part.exists());
        drop(download);
        assert!(!part.exists());
        assert!(!state.downloading());
        assert!(state.start(&part).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_before_a_wait_wins_over_a_ready_result() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let state = DownloadState::new();
        let (_guard, mut cancel) = state.start(&part).unwrap();
        state.cancel();
        assert_eq!(cancellable(&mut cancel, std::future::ready(7)).await, Err("cancelled".into()));
    }

    #[test]
    fn installing_replaces_an_existing_wrong_size_file() {
        let dir = TestDir::new();
        let part = dir.0.join("model.part");
        let destination = dir.0.join("model.bin");
        let state = DownloadState::new();
        let (guard, _cancel) = state.start(&part).unwrap();
        std::fs::write(&destination, b"bad").unwrap();
        std::fs::write(&part, b"verified replacement").unwrap();
        // Exercise the same std operation as fetch on Windows and Unix.
        std::fs::rename(&part, &destination).unwrap();
        drop(guard);
        assert_eq!(std::fs::read(&destination).unwrap(), b"verified replacement");
        assert!(!part.exists());
        assert!(!state.downloading());
    }

    #[test]
    fn local_model_support_matches_the_compiled_backend() {
        assert_eq!(status().supported, cfg!(windows));
        assert_eq!(require_supported().is_ok(), cfg!(windows));
        #[cfg(not(windows))]
        assert!(!status().installed);
    }

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

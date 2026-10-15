// Memory (hybrid P3): short things the user wants every chat to know ("I
// write in French", "my sister is Lina", "I use Rust at work").
//
// The user owns it. Settings → Memory lists every item; each can be edited or
// deleted, all of them at once too, and exported as a plain JSON file. The
// chat may *propose* an item (the `remember` tool, a `memory-save` action),
// which is saved only when the user confirms the card. A switch turns its use
// in the chat off without deleting anything.
//
// On disk it is `memory.bin` next to settings.json, encrypted for this
// Windows account (DPAPI): another account or a copied file can't read it.
// Written whole then renamed, so a crash never leaves half a file. A file
// that can't be read or comes from a newer Coucou is set aside, never
// overwritten; memory then starts empty and says so in the log.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// The format on disk. A newer file is never read by an older Coucou.
pub const VERSION: u32 = 1;
pub const MAX_ITEMS: usize = 200;
pub const MAX_CHARS: usize = 500;
/// The most memory that goes into one chat's instructions.
pub const MAX_PROMPT_CHARS: usize = 6000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Typed in Settings.
    User,
    /// Proposed by the chat, confirmed by the user.
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub text: String,
    pub source: Source,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub version: u32,
    /// Used in the chat. Off keeps the items, unused.
    pub enabled: bool,
    pub items: Vec<Item>,
}

impl Default for Memory {
    fn default() -> Self {
        Self { version: VERSION, enabled: true, items: Vec::new() }
    }
}

/// One line of text worth remembering: trimmed, not empty, not too long,
/// no control characters.
pub fn clean(text: &str) -> Result<String, String> {
    let text: String = text.replace(|c: char| c.is_control(), " ").split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err(crate::i18n::t("Nothing to remember."));
    }
    if text.chars().count() > MAX_CHARS {
        return Err(crate::i18n::tf("A memory is at most {count} characters.", &[("count", &MAX_CHARS.to_string())]));
    }
    Ok(text)
}

impl Memory {
    pub fn add(&mut self, text: &str, source: Source, now: u64) -> Result<Item, String> {
        let text = clean(text)?;
        if let Some(same) = self.items.iter().find(|i| i.text.eq_ignore_ascii_case(&text)) {
            return Ok(same.clone());
        }
        if self.items.len() >= MAX_ITEMS {
            return Err(crate::i18n::t("Memory is full: delete something first."));
        }
        let item = Item { id: crate::assistant::policy::new_id("mem"), text, source, created_at: now, updated_at: now };
        self.items.push(item.clone());
        Ok(item)
    }

    pub fn edit(&mut self, id: &str, text: &str, now: u64) -> Result<(), String> {
        let text = clean(text)?;
        let index = self.items.iter().position(|i| i.id == id).ok_or_else(|| crate::i18n::t("This memory is gone."))?;
        // Keep add's cleaned-text/ASCII-case uniqueness without merging away
        // either item's identity, source, or timestamps on a rejected edit.
        if self.items.iter().enumerate().any(|(other, item)| other != index && item.text.eq_ignore_ascii_case(&text)) {
            return Err(crate::i18n::t("This memory already exists."));
        }
        let item = &mut self.items[index];
        item.text = text;
        item.updated_at = now;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() != before
    }

    /// What the chat's instructions get, or None when off or empty.
    pub fn prompt_block(&self) -> Option<String> {
        if !self.enabled || self.items.is_empty() {
            return None;
        }
        let mut block = String::from(
            "What the user asked you to remember about them (they can see and edit it in Coucou's settings). \
Use it when it helps; don't repeat it back unasked:",
        );
        for item in &self.items {
            let line = format!("\n- {}", item.text);
            if block.len() + line.len() > MAX_PROMPT_CHARS {
                break;
            }
            block.push_str(&line);
        }
        Some(block)
    }
}

// ── Reading and writing ──────────────────────────────────────────────────────

/// Why a file wasn't used.
#[derive(Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// Not ours to read (another account) or damaged.
    Damaged,
    /// Written by a newer Coucou.
    Newer(u32),
}

/// The memory in decrypted bytes, migrated to VERSION.
pub fn parse(bytes: &[u8]) -> Result<Memory, Unreadable> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Unreadable::Damaged)?;
    let version = value["version"].as_u64().unwrap_or(0) as u32;
    if version > VERSION {
        return Err(Unreadable::Newer(version));
    }
    // Version 1 is the first; later versions migrate from here, step by step.
    let mut memory: Memory = serde_json::from_value(value).map_err(|_| Unreadable::Damaged)?;
    memory.version = VERSION;
    memory.items.retain(|i| clean(&i.text).is_ok());
    memory.items.truncate(MAX_ITEMS);
    Ok(memory)
}

fn path() -> PathBuf {
    crate::settings::config_dir().join("memory.bin")
}

static STORE: Mutex<Option<Memory>> = Mutex::new(None);

fn now_ms() -> u64 {
    crate::assistant::policy::now_ms()
}

fn load_from(file: &Path) -> Result<Memory, String> {
    let sealed = match std::fs::read(file) {
        Ok(sealed) => sealed,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Memory::default()),
        // A sharing/permission/device error says nothing about the contents.
        // Never cache an empty replacement; the next operation can retry.
        Err(err) => return Err(format!("memory.bin could not be read: {err}")),
    };
    let parsed = unseal(&sealed).map_err(|_| Unreadable::Damaged).and_then(|plain| parse(&plain));
    match parsed {
        Ok(memory) => Ok(memory),
        Err(why) => {
            let aside = file.with_extension(format!("bin.unreadable-{}-{}", now_ms(), crate::assistant::policy::new_id("backup")));
            std::fs::rename(file, &aside)
                .map_err(|err| format!("memory.bin could not be read nor set aside: not overwriting it ({err})"))?;
            crate::log::line(format!("memory: {why:?}; the file is kept aside and memory starts empty"));
            Ok(Memory::default())
        }
    }
}

fn save_to(file: &Path, memory: &Memory) -> Result<(), String> {
    let plain = serde_json::to_vec(memory).map_err(|e| e.to_string())?;
    let sealed = seal(&plain)?;
    let dir = file.parent().ok_or("no folder")?;
    crate::platform::ensure_private_dir(dir).map_err(|e| e.to_string())?;
    let tmp = file.with_extension("bin.tmp");
    std::fs::write(&tmp, sealed).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file).map_err(|e| e.to_string())
}

/// Runs `f` on the memory, loading it first, and saves it when `f` changed it.
pub fn with<T>(f: impl FnOnce(&mut Memory) -> Result<T, String>) -> Result<T, String> {
    let mut store = STORE.lock().unwrap();
    with_store(&mut store, &path(), f)
}

fn with_store<T>(store: &mut Option<Memory>, file: &Path, f: impl FnOnce(&mut Memory) -> Result<T, String>) -> Result<T, String> {
    if store.is_none() {
        // Failed reads leave the cache uninitialized, so no mutation can turn
        // a transient failure into an empty-derived overwrite on a later save.
        *store = Some(load_from(file)?);
    }
    let memory = store.as_mut().expect("loaded above");
    let before = memory.clone();
    let out = match f(memory) {
        Ok(out) => out,
        Err(error) => { *memory = before; return Err(error); }
    };
    if *memory != before {
        if let Err(err) = save_to(file, memory) {
            *memory = before;
            return Err(err);
        }
    }
    Ok(out)
}

pub fn get() -> Memory {
    with(|m| Ok(m.clone())).unwrap_or_default()
}

pub fn now() -> u64 {
    now_ms()
}

/// A plain JSON copy in the user's Downloads folder, for them to keep.
pub fn export(webview: &tauri::Webview) -> Result<PathBuf, String> {
    // Unlike optional prompt memory, a backup must not silently export an
    // empty placeholder when the real store could not be read.
    let memory = with(|m| Ok(m.clone()))?;
    let t = crate::platform::local_time();
    let stem = format!("Coucou memory {:04}-{:02}-{:02} {:02}.{:02}", t.year, t.month, t.day, t.hour, t.minute);
    // Tauri resolves Windows' FOLDERID_Downloads, including redirected folders.
    // Never guess a home/Downloads fallback if the native lookup fails.
    export_using(|| webview.path().download_dir().map_err(|e| e.to_string()), &stem, &memory)
}

fn export_using(
    resolve_downloads: impl FnOnce() -> Result<PathBuf, String>,
    stem: &str,
    memory: &Memory,
) -> Result<PathBuf, String> {
    let dir = resolve_downloads()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    export_to(&dir, stem, memory)
}

fn export_to(dir: &Path, stem: &str, memory: &Memory) -> Result<PathBuf, String> {
    let text = serde_json::to_vec_pretty(memory).map_err(|e| e.to_string())?;
    // Atomic create-new also protects against a concurrent export or a file
    // placed here between checking its name and opening it.
    for suffix in 0..10_000 {
        let name = if suffix == 0 { format!("{stem}.json") } else { format!("{stem} ({suffix}).json") };
        let file = dir.join(name);
        match write_new_export(&file, &text, |output, bytes| output.write_all(bytes)) {
            Ok(()) => return Ok(file),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.to_string()),
        }
    }
    Err("No unused memory export filename is available.".into())
}

/// Only the file this call created is removed on failure, never an older backup.
fn write_new_export(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = write(&mut output, bytes).and_then(|_| output.sync_all());
    drop(output);
    if let Err(error) = result {
        return match std::fs::remove_file(path) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(std::io::Error::new(ErrorKind::Other, format!("{error}; partial export cleanup failed: {cleanup}"))),
        };
    }
    Ok(())
}

// ── DPAPI ────────────────────────────────────────────────────────────────────

const ENTROPY: &[u8] = b"coucou-memory-v1";

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        let result = if protect {
            CryptProtectData(&input, windows::core::w!("Coucou memory"), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        };
        result.map_err(|e| e.message().to_string())?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Ok(bytes)
    }
}

#[cfg(windows)]
fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    dpapi(plain, true)
}

#[cfg(windows)]
fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
    dpapi(sealed, false)
}

/// No DPAPI elsewhere: memory isn't kept on disk at all.
#[cfg(not(windows))]
fn seal(_plain: &[u8]) -> Result<Vec<u8>, String> {
    Err("memory is only kept on Windows".into())
}

#[cfg(not(windows))]
fn unseal(_sealed: &[u8]) -> Result<Vec<u8>, String> {
    Err("memory is only kept on Windows".into())
}

// ── Commands (Settings → Memory) ─────────────────────────────────────────────

fn from_settings(webview: &tauri::Webview) -> Result<(), String> {
    (webview.label() == "settings").then_some(()).ok_or_else(|| "only from Settings".to_string())
}

#[tauri::command]
pub fn memory_list(webview: tauri::Webview) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| Ok(m.clone()))
}

#[tauri::command]
pub fn memory_add(webview: tauri::Webview, text: String) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| m.add(&text, Source::User, now_ms()).map(|_| ()))?;
    crate::log::line("memory: item added in Settings".to_string());
    Ok(get())
}

#[tauri::command]
pub fn memory_edit(webview: tauri::Webview, id: String, text: String) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| m.edit(&id, &text, now_ms()))?;
    Ok(get())
}

#[tauri::command]
pub fn memory_delete(webview: tauri::Webview, id: String) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| Ok(m.delete(&id)))?;
    crate::log::line("memory: item deleted".to_string());
    Ok(get())
}

#[tauri::command]
pub fn memory_clear(webview: tauri::Webview) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| {
        m.items.clear();
        Ok(())
    })?;
    crate::log::line("memory: everything deleted".to_string());
    Ok(get())
}

#[tauri::command]
pub fn memory_set_enabled(webview: tauri::Webview, enabled: bool) -> Result<Memory, String> {
    from_settings(&webview)?;
    with(|m| {
        m.enabled = enabled;
        Ok(())
    })?;
    Ok(get())
}

/// Writes the export and shows it in Explorer.
#[tauri::command]
pub fn memory_export(webview: tauri::Webview) -> Result<String, String> {
    from_settings(&webview)?;
    let file = export(&webview)?;
    crate::log::line("memory: exported".to_string());
    Ok(file.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir_for(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coucou-memory-{test}-{}", crate::assistant::policy::new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_a_missing_file_initializes_an_empty_store() {
        let dir = temp_dir_for("read-failure");
        let file = dir.join("memory.bin");
        assert_eq!(load_from(&file).unwrap(), Memory::default());
        std::fs::create_dir(&file).unwrap();
        assert!(load_from(&file).is_err(), "an existing unreadable path is not an empty store");
        let mut store = None;
        let mut called = false;
        assert!(with_store(&mut store, &file, |_| { called = true; Ok(()) }).is_err());
        assert!(!called, "no mutation runs after a failed read");
        assert!(store.is_none(), "failed loads are retryable, never cached as empty");
        assert!(file.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_temporary_sharing_failure_cannot_replace_existing_memory() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir_for("sharing-failure");
        let file = dir.join("memory.bin");
        let mut original = Memory::default();
        original.add("Keep the existing fact", Source::User, 1).unwrap();
        save_to(&file, &original).unwrap();
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        let mut store = None;
        assert!(with_store(&mut store, &file, |m| m.add("New fact", Source::User, 2)).is_err());
        assert!(store.is_none());
        drop(lock);
        assert_eq!(load_from(&file).unwrap(), original, "failed mutation did not alter disk");
        with_store(&mut store, &file, |m| m.add("New fact", Source::User, 2)).unwrap();
        let restored = load_from(&file).unwrap();
        assert_eq!(restored.items.len(), 2);
        assert_eq!(restored.items[0], original.items[0], "retry loads before adding");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exports_use_the_resolved_downloads_folder_without_appending_or_guessing() {
        let root = temp_dir_for("redirected-downloads");
        let profile = root.join("profile");
        std::fs::create_dir(&profile).unwrap();
        let downloads = root.join("redirected volume").join("Téléchargements 私用");
        let mut memory = Memory::default();
        memory.add("Keep this fact", Source::User, 1).unwrap();
        let mut lookups = 0;
        let file = export_using(|| {
            lookups += 1;
            Ok(downloads.clone())
        }, "backup", &memory).unwrap();
        assert_eq!(lookups, 1);
        assert_eq!(file, downloads.join("backup.json"));
        assert_eq!(serde_json::from_slice::<Memory>(&std::fs::read(&file).unwrap()).unwrap(), memory);
        assert!(!profile.join("Downloads").exists(), "no obsolete profile folder is created");
        assert!(!downloads.join("Downloads").exists(), "the resolved directory is already Downloads");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_downloads_lookup_failure_does_not_export_to_a_fallback() {
        let root = temp_dir_for("downloads-lookup-failure");
        let mut lookups = 0;
        let result = export_using(|| {
            lookups += 1;
            Err("Downloads is unavailable".to_string())
        }, "backup", &Memory::default());
        assert_eq!(result, Err("Downloads is unavailable".to_string()));
        assert_eq!(lookups, 1);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unwritable_resolved_downloads_path_does_not_fall_back() {
        let root = temp_dir_for("downloads-path-failure");
        let blocked = root.join("redirected downloads");
        std::fs::write(&blocked, b"keep this file").unwrap();
        assert!(export_using(|| Ok(blocked.clone()), "backup", &Memory::default()).is_err());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"keep this file");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_exports_preserve_all_preexisting_files() {
        let dir = temp_dir_for("exports");
        let stem = "Coucou memory 2026-10-15 04.00";
        let existing = dir.join(format!("{stem}.json"));
        std::fs::write(&existing, b"previous backup").unwrap();
        let mut memory = Memory::default();
        memory.add("First fact", Source::User, 1).unwrap();
        let first = export_to(&dir, stem, &memory).unwrap();
        let first_bytes = std::fs::read(&first).unwrap();
        memory.add("Second fact", Source::User, 2).unwrap();
        let second = export_to(&dir, stem, &memory).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&existing).unwrap(), b"previous backup");
        assert_eq!(std::fs::read(&first).unwrap(), first_bytes);
        let latest: Memory = serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
        assert_eq!(latest, memory);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_exports_claim_different_names_atomically() {
        let dir = temp_dir_for("concurrent-exports");
        let jobs: Vec<_> = (0..8).map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || export_to(&dir, "same minute", &Memory::default()).unwrap())
        }).collect();
        let paths: std::collections::BTreeSet<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
        assert_eq!(paths.len(), 8);
        for file in paths {
            assert_eq!(serde_json::from_slice::<Memory>(&std::fs::read(file).unwrap()).unwrap(), Memory::default());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_io_failure_reports_an_error_without_replacing_anything() {
        let dir = temp_dir_for("export-failure");
        let blocked = dir.join("not-a-directory");
        std::fs::write(&blocked, b"keep me").unwrap();
        assert!(export_to(&blocked, "backup", &Memory::default()).is_err());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"keep me");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_export_write_removes_only_its_own_partial_file() {
        let dir = temp_dir_for("partial-export");
        let older = dir.join("older.json");
        std::fs::write(&older, b"keep this backup").unwrap();
        let partial = dir.join("partial.json");
        assert!(write_new_export(&partial, b"private memory", |output, bytes| {
            output.write_all(&bytes[..5])?;
            Err(std::io::Error::new(ErrorKind::Other, "simulated disk full"))
        }).is_err());
        assert!(!partial.exists());
        assert!(write_new_export(&older, b"replacement", |output, bytes| output.write_all(bytes)).is_err());
        assert_eq!(std::fs::read(&older).unwrap(), b"keep this backup");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_refused_mutation_restores_the_cached_memory() {
        let dir = temp_dir_for("refused-mutation");
        let file = dir.join("memory.bin");
        let original = Memory::default();
        let mut store = Some(original.clone());
        assert!(with_store(&mut store, &file, |memory| {
            memory.enabled = false;
            Err::<(), _>("refused".to_string())
        }).is_err());
        assert_eq!(store, Some(original));
        assert!(!file.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_retired_chat_cannot_load_mutate_or_save_memory() {
        let dir = temp_dir_for("cancelled-chat");
        let file = dir.join("memory.bin");
        let scope = crate::assistant::tools::ActionScope::new();
        let actions = crate::assistant::tools::ChatActions::default();
        actions.retire(&scope);
        let mut store = None;
        let result = scope.run_if_active(|| with_store(&mut store, &file, |m| m.add("Cancelled fact", Source::Chat, 1)));
        assert!(result.is_none());
        assert!(store.is_none(), "a retired request cannot initialize the cache");
        assert!(!file.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_waits_until_the_memory_transaction_is_persisted_or_rolled_back() {
        use std::sync::mpsc;
        use std::time::Duration;
        for fail_save in [false, true] {
            let dir = temp_dir_for("atomic-chat-save");
            let file = dir.join("memory.bin");
            if fail_save { std::fs::create_dir(&file).unwrap(); }
            let scope = crate::assistant::tools::ActionScope::new();
            let worker_scope = scope.clone();
            let worker_file = file.clone();
            let (mutated, mutation) = mpsc::channel();
            let (resume, resumed) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut store = Some(Memory::default());
                let result = worker_scope.run_if_active(|| with_store(&mut store, &worker_file, |m| {
                    let item = m.add("Confirmed fact", Source::Chat, 1)?;
                    mutated.send(()).unwrap();
                    // Still inside the transaction, before its disk save.
                    resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(item)
                }));
                (result, store)
            });
            mutation.recv_timeout(Duration::from_secs(5)).unwrap();
            let (attempting, attempted) = mpsc::channel();
            let (retired, retirement) = mpsc::channel();
            let cancelling_scope = scope.clone();
            let cancellation = std::thread::spawn(move || {
                attempting.send(()).unwrap();
                crate::assistant::tools::ChatActions::default().retire(&cancelling_scope);
                retired.send(()).unwrap();
            });
            attempted.recv_timeout(Duration::from_secs(5)).unwrap();
            let early = retirement.recv_timeout(Duration::from_millis(50));
            // Release before asserting so a regression does not strand workers.
            resume.send(()).unwrap();
            assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)), "cancellation cannot return between mutation and save");
            let (result, store) = worker.join().unwrap();
            retirement.recv_timeout(Duration::from_secs(5)).unwrap();
            cancellation.join().unwrap();
            let result = result.expect("the transaction won the scope lock");
            if fail_save {
                assert!(result.is_err());
                assert_eq!(store, Some(Memory::default()), "failed persistence rolls the cache back before retirement");
                assert!(file.is_dir(), "failed save did not replace the existing path");
            } else {
                let item = result.unwrap();
                assert_eq!(load_from(&file).unwrap().items, vec![item]);
            }
            assert!(scope.run_if_active(|| ()).is_none(), "retirement prevents another transaction");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn items_are_cleaned_deduplicated_and_bounded() {
        let mut m = Memory::default();
        let a = m.add("  I write   in French\n", Source::User, 1).unwrap();
        assert_eq!(a.text, "I write in French");
        assert_eq!(m.add("i write in french", Source::Chat, 2).unwrap().id, a.id, "the same thing once");
        assert!(m.add("   ", Source::User, 3).is_err());
        assert_eq!(clean("a\u{1b}[31mb\u{0}c").unwrap(), "a [31mb c", "no control characters");
        assert!(m.add(&"x".repeat(MAX_CHARS + 1), Source::User, 3).is_err());
        for n in 1..MAX_ITEMS {
            m.add(&format!("fact {n}"), Source::User, 4).unwrap();
        }
        assert_eq!(m.items.len(), MAX_ITEMS);
        assert!(m.add("one too many", Source::User, 5).is_err());
    }

    #[test]
    fn edit_and_delete_by_id() {
        let mut m = Memory::default();
        let a = m.add("My sister is Lina", Source::User, 1).unwrap();
        m.edit(&a.id, "My sister is Lina, she lives in Lyon", 9).unwrap();
        assert_eq!(m.items[0].text, "My sister is Lina, she lives in Lyon");
        assert_eq!((m.items[0].created_at, m.items[0].updated_at), (1, 9));
        assert!(m.edit("mem-nope", "x", 10).is_err());
        assert!(m.delete(&a.id));
        assert!(!m.delete(&a.id));
    }

    #[test]
    fn duplicate_edits_are_rejected_without_changing_any_item() {
        for duplicate_first in [false, true] {
            for text in ["I write in French", "i WRITE in FRENCH", "  I  write\tin French\n", "I\u{0}write\u{2003}in French"] {
                let mut m = Memory::default();
                let (target, duplicate) = if duplicate_first {
                    let duplicate = m.add("I write in French", Source::Chat, 1).unwrap();
                    (m.add("I use Rust at work", Source::User, 2).unwrap(), duplicate)
                } else {
                    let target = m.add("I use Rust at work", Source::User, 1).unwrap();
                    (target, m.add("I write in French", Source::Chat, 2).unwrap())
                };
                m.add("Prefers short answers", Source::User, 3).unwrap();
                let before = m.clone();
                let prompt = m.prompt_block();
                assert!(m.edit(&target.id, text, 99).is_err(), "duplicate edit: {text:?}");
                assert_eq!(m, before, "a refusal must preserve every id, source, timestamp, and position");
                assert_eq!(m.prompt_block(), prompt, "the prompt must not gain a duplicate fact");
                assert_eq!(parse(&serde_json::to_vec(&m).unwrap()).unwrap(), before);
                assert_eq!(m.add(text, Source::User, 100).unwrap(), duplicate, "add and edit agree about duplicates");
                assert_eq!(m, before);
            }
        }
    }

    #[test]
    fn an_item_can_edit_its_own_case_without_conflicting_with_itself() {
        let mut m = Memory::default();
        let a = m.add("I write in French", Source::Chat, 1).unwrap();
        let other = m.add("I use Rust at work", Source::User, 2).unwrap();
        for (offset, text) in ["I write in French", "i WRITE in FRENCH", "  I  write\tin French\n"].iter().enumerate() {
            let now = 10 + offset as u64;
            m.edit(&a.id, text, now).unwrap();
            assert_eq!(m.items[0], Item { text: clean(text).unwrap(), updated_at: now, ..a.clone() });
            assert_eq!(m.items[1], other, "self-edits do not touch another item");
        }
    }

    #[test]
    fn a_unique_edit_only_changes_the_selected_text_and_update_time() {
        let mut m = Memory::default();
        let first = m.add("Prefers short answers", Source::User, 1).unwrap();
        let target = m.add("I use Rust", Source::Chat, 2).unwrap();
        let last = m.add("I write in French", Source::User, 3).unwrap();
        m.edit(&target.id, "  I use\tRust at work\n", 9).unwrap();
        assert_eq!(m.items, vec![first, Item { text: "I use Rust at work".into(), updated_at: 9, ..target }, last]);
    }

    #[test]
    fn invalid_edits_and_missing_ids_leave_memory_unchanged() {
        let mut m = Memory::default();
        let a = m.add("I write in French", Source::User, 1).unwrap();
        m.add("I use Rust", Source::Chat, 2).unwrap();
        let before = m.clone();
        for text in [" \n\t".to_string(), "x".repeat(MAX_CHARS + 1)] {
            assert!(m.edit(&a.id, &text, 9).is_err());
            assert_eq!(m, before);
        }
        assert_eq!(m.edit("mem-missing", "i WRITE in FRENCH", 9), Err(crate::i18n::t("This memory is gone.")));
        assert_eq!(m, before, "a missing target must not merge with an existing item");
    }

    #[test]
    fn a_full_memory_can_be_edited_but_cannot_gain_duplicate_facts() {
        let mut m = Memory::default();
        for n in 0..MAX_ITEMS {
            m.add(&format!("fact {n}"), Source::User, n as u64).unwrap();
        }
        let target = m.items[0].clone();
        let before = m.clone();
        assert!(m.edit(&target.id, " FACT   1 ", 999).is_err());
        assert_eq!(m, before);
        assert_eq!(m.add(" FACT   1 ", Source::Chat, 999).unwrap(), before.items[1]);
        assert_eq!(m, before, "deduplicated adds still work at capacity");
        m.edit(&target.id, "a different fact", 1000).unwrap();
        assert_eq!(m.items.len(), MAX_ITEMS);
        assert_eq!(m.items[0], Item { text: "a different fact".into(), updated_at: 1000, ..target });
        assert_eq!(&m.items[1..], &before.items[1..]);
    }

    #[test]
    fn deleting_the_conflicting_fact_allows_a_later_edit_without_replacing_its_id() {
        let mut m = Memory::default();
        let target = m.add("I use Rust", Source::Chat, 1).unwrap();
        let duplicate = m.add("I write in French", Source::User, 2).unwrap();
        let other = m.add("Prefers short answers", Source::User, 3).unwrap();
        assert!(m.edit(&target.id, "I write in French", 4).is_err());
        assert!(m.delete(&duplicate.id));
        m.edit(&target.id, "I write in French", 5).unwrap();
        assert_eq!(m.items, vec![Item { text: "I write in French".into(), updated_at: 5, ..target }, other]);
    }

    #[test]
    fn the_chat_gets_memory_only_when_on_and_bounded() {
        let mut m = Memory::default();
        assert_eq!(m.prompt_block(), None);
        m.add("Prefers short answers", Source::User, 1).unwrap();
        let block = m.prompt_block().unwrap();
        assert!(block.ends_with("\n- Prefers short answers"));
        m.enabled = false;
        assert_eq!(m.prompt_block(), None, "off keeps the items, unused");
        m.enabled = true;
        for n in 0..MAX_ITEMS - 1 {
            m.add(&format!("{n} {}", "y".repeat(400)), Source::User, 2).unwrap();
        }
        assert!(m.prompt_block().unwrap().len() <= MAX_PROMPT_CHARS);
    }

    #[test]
    fn a_file_from_a_newer_coucou_or_a_damaged_one_is_not_read() {
        assert_eq!(parse(br#"{"version":2,"enabled":true,"items":[]}"#), Err(Unreadable::Newer(2)));
        assert_eq!(parse(b"not json"), Err(Unreadable::Damaged));
        assert_eq!(parse(br#"{"version":1,"enabled":"yes","items":[]}"#), Err(Unreadable::Damaged));
    }

    #[test]
    fn reading_drops_what_is_no_longer_valid() {
        let file = br#"{"version":1,"enabled":true,"items":[
            {"id":"mem-1","text":"ok","source":"user","createdAt":1,"updatedAt":1},
            {"id":"mem-2","text":"   ","source":"chat","createdAt":1,"updatedAt":1}]}"#;
        let m = parse(file).unwrap();
        assert_eq!(m.items.len(), 1);
        assert_eq!(m.items[0].source, Source::User);
    }

    #[cfg(windows)]
    #[test]
    fn the_file_is_encrypted_for_this_account_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("coucou-memory-{}", std::process::id()));
        let file = dir.join("memory.bin");
        let mut m = Memory::default();
        m.add("The secret word is pineapple", Source::User, 1).unwrap();
        save_to(&file, &m).unwrap();
        let raw = std::fs::read(&file).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("pineapple"), "never in clear on disk");
        assert_eq!(load_from(&file).unwrap(), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn an_unreadable_file_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("coucou-memory-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("memory.bin");
        std::fs::write(&file, b"garbage that DPAPI can't open").unwrap();
        assert_eq!(load_from(&file).unwrap(), Memory::default());
        assert!(!file.exists());
        let aside: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].file_name().to_string_lossy().starts_with("memory.bin.unreadable-"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

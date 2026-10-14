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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

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
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
        let item = self.items.iter_mut().find(|i| i.id == id).ok_or_else(|| crate::i18n::t("This memory is gone."))?;
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

/// Set once a file could not be read, so it is never overwritten.
static SET_ASIDE_FAILED: Mutex<bool> = Mutex::new(false);
static STORE: Mutex<Option<Memory>> = Mutex::new(None);

fn now_ms() -> u64 {
    crate::assistant::policy::now_ms()
}

fn load_from(file: &Path) -> Memory {
    let Ok(sealed) = std::fs::read(file) else { return Memory::default() };
    let parsed = unseal(&sealed).map_err(|_| Unreadable::Damaged).and_then(|plain| parse(&plain));
    match parsed {
        Ok(memory) => memory,
        Err(why) => {
            let aside = file.with_extension(format!("bin.unreadable-{}", now_ms()));
            crate::log::line(format!("memory: {why:?}; the file is kept aside and memory starts empty"));
            if std::fs::rename(file, &aside).is_err() {
                *SET_ASIDE_FAILED.lock().unwrap() = true;
            }
            Memory::default()
        }
    }
}

fn save_to(file: &Path, memory: &Memory) -> Result<(), String> {
    if *SET_ASIDE_FAILED.lock().unwrap() {
        return Err("memory.bin could not be read nor set aside: not overwriting it".into());
    }
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
    let memory = store.get_or_insert_with(|| load_from(&path()));
    let before = memory.clone();
    let out = f(memory)?;
    if *memory != before {
        if let Err(err) = save_to(&path(), memory) {
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
pub fn export() -> Result<PathBuf, String> {
    let memory = get();
    let t = crate::platform::local_time();
    let dir = crate::platform::home_dir().join("Downloads");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("Coucou memory {:04}-{:02}-{:02} {:02}.{:02}.json", t.year, t.month, t.day, t.hour, t.minute));
    let text = serde_json::to_string_pretty(&memory).map_err(|e| e.to_string())?;
    std::fs::write(&file, text).map_err(|e| e.to_string())?;
    Ok(file)
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
    Ok(get())
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
    let file = export()?;
    crate::log::line("memory: exported".to_string());
    Ok(file.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_cleaned_deduplicated_and_bounded() {
        let mut m = Memory::default();
        let a = m.add("  I write   in French\n", Source::User, 1).unwrap();
        assert_eq!(a.text, "I write in French");
        assert_eq!(m.add("i write in french", Source::Chat, 2).unwrap().id, a.id, "the same thing once");
        assert!(m.add("   ", Source::User, 3).is_err());
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
        assert_eq!(load_from(&file), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn an_unreadable_file_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("coucou-memory-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("memory.bin");
        std::fs::write(&file, b"garbage that DPAPI can't open").unwrap();
        assert_eq!(load_from(&file), Memory::default());
        assert!(!file.exists());
        let aside: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].file_name().to_string_lossy().starts_with("memory.bin.unreadable-"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

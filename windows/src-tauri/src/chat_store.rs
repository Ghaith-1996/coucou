// Saved chat conversations: one JSON file each in
// %LOCALAPPDATA%\Coucou\conversations, so a conversation can be reopened from
// the chat's history, renamed or deleted. The island builds the record (it
// knows which model and effort answered each message); Rust checks it before
// writing: the id names the file, so it must be exactly `c-` + 16 hex digits,
// and every size is capped. Small, disposable summaries under .summaries
// speed up history pages; the transcript JSON remains the source of truth.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const MAX_MESSAGES: usize = 400;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_LISTED: usize = 500;
const MAX_SUMMARY_BYTES: usize = 2048;
const SUMMARY_VERSION: u8 = 1;

// Keep reads and read-modify-writes together. Reopening during a save must
// wait for its new transcript, not restore an older one that can overwrite it.
// Private *_locked helpers are only called while the caller holds this lock.
// Deleted ids cannot be reused by delayed saves, including a first save that
// has not reached the lock yet. Pending commands cannot survive a restart, so
// these tombstones need only live for this process (and are not summary data).
static STORE: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The user pressed Stop: shown, but never sent back as context.
    #[serde(default)]
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<SavedMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub count: usize,
}

// A change detector, not a content hash: deliberately preserving all these
// filesystem attributes can defeat it. Cached metadata is disposable; loading
// a conversation always reads and validates its actual transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Fingerprint {
    len: u64,
    modified: std::time::SystemTime,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedSummary {
    version: u8,
    fingerprint: Fingerprint,
    // None means bytes were read successfully but failed validation. Never
    // persist transient I/O failures: access may recover without a file change.
    // Require the field even though its value can be null for an invalid body.
    #[serde(deserialize_with = "Option::deserialize")]
    summary: Option<Summary>,
}

fn fingerprint(metadata: &std::fs::Metadata) -> Option<Fingerprint> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_file() { return None; }
    Some(Fingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok()?,
        created: metadata.created().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

fn file_fingerprint(file: &Path) -> Option<Fingerprint> {
    fingerprint(&std::fs::metadata(file).ok()?)
}

fn summary_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(".summaries").join(format!("{id}.json"))
}

fn summary(c: &Conversation) -> Summary {
    Summary { id: c.id.clone(), title: c.title.clone(), updated_at: c.updated_at, count: c.messages.len() }
}

fn read_summary(dir: &Path, id: &str, current: &Fingerprint) -> Option<CachedSummary> {
    let file = std::fs::File::open(summary_path(dir, id)).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SUMMARY_BYTES as u64 { return None; }
    let mut bytes = Vec::new();
    file.take(MAX_SUMMARY_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_SUMMARY_BYTES { return None; }
    let cached: CachedSummary = serde_json::from_slice(&bytes).ok()?;
    if cached.version != SUMMARY_VERSION || &cached.fingerprint != current { return None; }
    if cached.summary.as_ref().is_some_and(|s| s.id != id || !title_ok(&s.title) || s.count > MAX_MESSAGES) {
        return None;
    }
    Some(cached)
}

// Best effort: failing to create/replace metadata must never fail a successful
// transcript save or make a valid conversation inaccessible.
fn write_summary(dir: &Path, id: &str, fingerprint: Fingerprint, summary: Option<Summary>) -> Option<()> {
    let bytes = serde_json::to_vec(&CachedSummary { version: SUMMARY_VERSION, fingerprint, summary }).ok()?;
    if bytes.len() > MAX_SUMMARY_BYTES { return None; }
    let target = summary_path(dir, id);
    std::fs::create_dir_all(target.parent()?).ok()?;
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, bytes).ok()?;
    let result = std::fs::rename(&tmp, &target);
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result.ok()
}

fn list_summary_locked(dir: &Path, id: &str) -> Option<Summary> {
    let file = path(dir, id)?;
    let metadata = std::fs::metadata(&file).ok()?;
    if !metadata.is_file() { return None; }
    let before = fingerprint(&metadata);
    if let Some(current) = &before {
        if current.len > MAX_FILE_BYTES as u64 { return None; }
        if let Some(cached) = read_summary(dir, id, current) {
            if file_fingerprint(&file).as_ref() == Some(current) { return cached.summary; }
        }
    }
    let bytes = read_transcript(&file).ok()?;
    let result = parse_transcript(&bytes, id).ok().map(|c| summary(&c));
    // An external writer does not hold STORE. Only certify metadata when the
    // file fingerprint is unchanged across the full read and validation.
    if let Some(current) = before {
        if file_fingerprint(&file).as_ref() == Some(&current) {
            let _ = write_summary(dir, id, current, result.clone());
        }
    }
    result
}

/// Continue after this ordering key, even if its conversation was deleted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListCursor {
    pub updated_at: u64,
    pub id: String,
}

pub fn dir() -> PathBuf {
    crate::settings::local_dir().join("conversations")
}

pub fn id_ok(id: &str) -> bool {
    id.len() == 18 && id.starts_with("c-") && id[2..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn title_ok(title: &str) -> bool {
    let n = title.chars().count();
    (1..=MAX_TITLE_CHARS).contains(&n) && !title.chars().any(char::is_control)
}

pub fn validate(c: &Conversation) -> Result<(), String> {
    if !id_ok(&c.id) {
        return Err("bad conversation id".into());
    }
    if !title_ok(&c.title) {
        return Err("bad title".into());
    }
    if c.messages.len() > MAX_MESSAGES {
        return Err("too many messages".into());
    }
    for m in &c.messages {
        if !matches!(m.role.as_str(), "user" | "assistant") || m.content.len() > MAX_MESSAGE_BYTES {
            return Err("bad message".into());
        }
        let short = |v: &Option<String>| v.as_ref().is_none_or(|s| s.len() <= 80 && !s.chars().any(char::is_control));
        if !short(&m.provider) || !short(&m.model) || !short(&m.effort) {
            return Err("bad message".into());
        }
    }
    Ok(())
}

fn path(dir: &Path, id: &str) -> Option<PathBuf> {
    id_ok(id).then(|| dir.join(format!("{id}.json")))
}

pub fn save(dir: &Path, c: &Conversation) -> Result<(), String> {
    validate(c)?;
    #[cfg(test)]
    tests::before_save_lock();
    let deleted = STORE.lock().map_err(|_| "conversation store unavailable")?;
    let mut c = c.clone();
    let target = path(dir, &c.id).ok_or("bad conversation id")?;
    if deleted.contains(&target) {
        return Err(crate::i18n::t("This conversation is gone."));
    }
    if target.try_exists().map_err(|e| e.to_string())? {
        // Read the transcript, not cached metadata: external edits are still
        // authoritative. Lock acquisition order is not snapshot capture order.
        let current = load_locked(dir, &c.id)?;
        if c.updated_at < current.updated_at { return Ok(()); }
        if c.updated_at == current.updated_at {
            // Date.now() can tie. Exact retries and older prefixes are harmless;
            // only a strict append proves progress without a distinct revision.
            // Conflicting replacements (including regeneration) need a newer
            // timestamp instead of guessing from length or lock order.
            if current.messages.starts_with(&c.messages) { return Ok(()); }
            if !c.messages.starts_with(&current.messages) {
                return Err("conversation snapshot conflicts with the saved revision".into());
            }
        }
        // Only rename changes an existing title. The view may have captured
        // its save before a rename completed, even after reopening the chat.
        c.title = current.title;
        c.created_at = current.created_at;
    }
    write_locked(dir, &c)
}

fn write_locked(dir: &Path, c: &Conversation) -> Result<(), String> {
    validate(c)?;
    let bytes = serde_json::to_vec(c).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("conversation too large".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let target = path(dir, &c.id).ok_or("bad conversation id")?;
    // Written whole, then renamed: a crash never leaves half a file.
    let tmp = dir.join(format!("{}.tmp", c.id));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    // The write handle is closed before checking timestamps (Windows updates
    // last-write time on close). Keep an optional read handle through rename.
    let written = std::fs::File::open(&tmp).ok().and_then(|file| {
        let before = fingerprint(&file.metadata().ok()?)?;
        Some((file, before))
    });
    // Invalidate known-old metadata before committing, including when its
    // best-effort refresh later fails and filesystem timestamps are coarse.
    let _ = std::fs::remove_file(summary_path(dir, &c.id));
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    #[cfg(test)]
    tests::after_transcript_write();
    // The same handle detects an external in-place edit; the path comparison
    // detects replacement. Rename may change ctime (Unix) or tunnel creation
    // time (Windows), so compare size/mtime across rename on this stable handle.
    if let Some((file, before)) = written {
        if let Some(current) = file.metadata().ok().and_then(|m| fingerprint(&m)) {
            if before.len == current.len && before.modified == current.modified
                && file_fingerprint(&target).as_ref() == Some(&current) {
                let _ = write_summary(dir, &c.id, current, Some(summary(c)));
            }
        }
    }
    Ok(())
}

pub fn load(dir: &Path, id: &str) -> Result<Conversation, String> {
    let _guard = STORE.lock().map_err(|_| "conversation store unavailable")?;
    load_locked(dir, id)
}

fn load_locked(dir: &Path, id: &str) -> Result<Conversation, String> {
    let file = path(dir, id).ok_or("bad conversation id")?;
    let bytes = read_transcript(&file).map_err(|_| crate::i18n::t("This conversation is gone."))?;
    parse_transcript(&bytes, id)
}

fn read_transcript(file: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(file)?;
    #[cfg(test)]
    tests::transcript_read();
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    #[cfg(test)]
    tests::after_transcript_read();
    Ok(bytes)
}

fn parse_transcript(bytes: &[u8], id: &str) -> Result<Conversation, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("conversation too large".into());
    }
    let c: Conversation = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    validate(&c)?;
    if c.id != id {
        return Err("bad conversation id".into());
    }
    Ok(c)
}

/// One page, newest first, then id for ties. Unreadable files are skipped;
/// directory errors are returned so callers can retry without losing rows.
pub fn list(dir: &Path, before: Option<&ListCursor>) -> Result<Vec<Summary>, String> {
    if before.is_some_and(|cursor| !id_ok(&cursor.id)) {
        return Err("bad conversation cursor".into());
    }
    let _guard = STORE.lock().map_err(|_| "conversation store unavailable")?;
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| e.to_string())?;
        let Ok(name) = entry.file_name().into_string() else { continue };
        let Some(id) = name.strip_suffix(".json") else { continue };
        let Some(row) = list_summary_locked(dir, id) else { continue };
        if before.is_some_and(|cursor| row.updated_at > cursor.updated_at
            || (row.updated_at == cursor.updated_at && row.id <= cursor.id)) {
            continue;
        }
        out.push(row);
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    out.truncate(MAX_LISTED);
    Ok(out)
}

pub fn rename(dir: &Path, id: &str, title: &str) -> Result<(), String> {
    let _guard = STORE.lock().map_err(|_| "conversation store unavailable")?;
    let mut c = load_locked(dir, id)?;
    c.title = title.trim().to_string();
    write_locked(dir, &c)
}

pub fn delete(dir: &Path, id: &str) -> Result<(), String> {
    let mut deleted = STORE.lock().map_err(|_| "conversation store unavailable")?;
    let file = path(dir, id).ok_or("bad conversation id")?;
    match std::fs::remove_file(&file) {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.to_string()),
    }
    // Also fence a pending first save when there was no transcript to remove.
    // A failed removal must not poison an existing conversation's later saves.
    deleted.insert(file);
    let cached = summary_path(dir, id);
    let _ = std::fs::remove_file(&cached);
    let _ = std::fs::remove_file(cached.with_extension("tmp"));
    Ok(())
}

/// The turns to give the model again when a conversation is reopened:
/// complete user → assistant pairs, without stopped answers.
pub fn context_turns(c: &Conversation) -> Vec<(String, String)> {
    let mut turns = Vec::new();
    let mut question: Option<&str> = None;
    for m in &c.messages {
        match m.role.as_str() {
            "user" => question = Some(&m.content),
            "assistant" if !m.stopped => {
                if let Some(q) = question.take() {
                    turns.push((q.to_string(), m.content.clone()));
                }
            }
            _ => question = None,
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static TRANSCRIPT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static BEFORE_SAVE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
        static AFTER_TRANSCRIPT_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
        static AFTER_TRANSCRIPT_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn before_save_lock() {
        let callback = BEFORE_SAVE_LOCK.with(|callback| callback.borrow_mut().take());
        if let Some(callback) = callback { callback(); }
    }

    pub(super) fn transcript_read() {
        TRANSCRIPT_READS.with(|reads| reads.set(reads.get() + 1));
    }

    pub(super) fn after_transcript_read() {
        let callback = AFTER_TRANSCRIPT_READ.with(|callback| callback.borrow_mut().take());
        if let Some(callback) = callback { callback(); }
    }

    pub(super) fn after_transcript_write() {
        let callback = AFTER_TRANSCRIPT_WRITE.with(|callback| callback.borrow_mut().take());
        if let Some(callback) = callback { callback(); }
    }

    fn reset_reads() { TRANSCRIPT_READS.with(|reads| reads.set(0)); }
    fn read_count() -> usize { TRANSCRIPT_READS.with(std::cell::Cell::get) }

    fn msg(role: &str, content: &str) -> SavedMessage {
        SavedMessage { role: role.into(), content: content.into(), provider: None, model: None, effort: None, stopped: false }
    }

    fn conv(id: &str, updated: u64) -> Conversation {
        Conversation {
            id: id.into(),
            title: "Hello".into(),
            created_at: 1,
            updated_at: updated,
            messages: vec![msg("user", "hi"), SavedMessage { model: Some("gpt-6.1-sol".into()), effort: Some("max".into()), ..msg("assistant", "hello") }],
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coucou-chats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_conversation_is_saved_listed_renamed_reopened_and_deleted() {
        let dir = scratch("life");
        save(&dir, &conv("c-0000000000000001", 10)).unwrap();
        save(&dir, &conv("c-0000000000000002", 20)).unwrap();
        let ids: Vec<String> = list(&dir, None).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["c-0000000000000002", "c-0000000000000001"], "most recent first");
        rename(&dir, "c-0000000000000001", "  Trip plans ").unwrap();
        let back = load(&dir, "c-0000000000000001").unwrap();
        assert_eq!(back.title, "Trip plans");
        assert_eq!(back.messages[1].model.as_deref(), Some("gpt-6.1-sol"), "which model answered is kept");
        delete(&dir, "c-0000000000000001").unwrap();
        delete(&dir, "c-0000000000000001").unwrap();
        assert_eq!(list(&dir, None).unwrap().len(), 1);
        assert!(!dir.join("c-0000000000000001.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pages_reach_every_saved_file_in_stable_timestamp_and_id_order() {
        let dir = scratch("pages");
        let count = MAX_LISTED * 2 + 3;
        for n in (0..count).rev() {
            save(&dir, &conv(&format!("c-{n:016x}"), if n < 3 { 20 } else { 10 })).unwrap();
        }
        reset_reads();
        let mut ids = Vec::new();
        let mut before = None;
        loop {
            let page = list(&dir, before.as_ref()).unwrap();
            assert!(page.len() <= MAX_LISTED);
            if page.is_empty() { break; }
            let last = page.last().unwrap();
            before = Some(ListCursor { updated_at: last.updated_at, id: last.id.clone() });
            ids.extend(page.into_iter().map(|s| s.id));
        }
        assert_eq!(read_count(), 0, "every page uses summaries written by save");
        assert_eq!(ids, (0..count).map(|n| format!("c-{n:016x}")).collect::<Vec<_>>());
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json")).count(),
            count, "listing never deletes transcript files");
        assert_eq!(load(&dir, ids.last().unwrap()).unwrap().messages.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cursor_survives_renaming_deletion_and_newer_saves_between_pages() {
        let dir = scratch("page-mutations");
        for n in 0..MAX_LISTED + 2 {
            save(&dir, &conv(&format!("c-{n:016x}"), 10)).unwrap();
        }
        let first = list(&dir, None).unwrap();
        let last = first.last().unwrap();
        let before = ListCursor { updated_at: last.updated_at, id: last.id.clone() };
        let older = format!("c-{:016x}", MAX_LISTED);
        rename(&dir, &older, "An older title").unwrap();
        delete(&dir, &before.id).unwrap();
        delete(&dir, &first[0].id).unwrap();
        save(&dir, &conv("c-ffffffffffffffff", 30)).unwrap();
        reset_reads();
        let page = list(&dir, Some(&before)).unwrap();
        assert_eq!(read_count(), 0, "mutations keep summary metadata current");
        assert_eq!(page.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            [older, format!("c-{:016x}", MAX_LISTED + 1)]);
        assert_eq!(page[0].title, "An older title");
        assert_eq!(list(&dir, None).unwrap()[0].id, "c-ffffffffffffffff");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_last_pages_and_failed_listing_remain_retryable() {
        let dir = scratch("page-errors");
        assert!(list(&dir, None).unwrap().is_empty(), "a missing folder is an empty history");
        // A file where the directory should be fails on Windows and Unix.
        std::fs::write(&dir, b"not a directory").unwrap();
        assert!(list(&dir, None).is_err(), "listing failures are not empty history");
        std::fs::remove_file(&dir).unwrap();
        let c = conv("c-0000000000000001", 10);
        save(&dir, &c).unwrap();
        assert_eq!(list(&dir, None).unwrap().len(), 1, "retry succeeds");
        let before = ListCursor { updated_at: c.updated_at, id: c.id.clone() };
        assert!(list(&dir, Some(&before)).unwrap().is_empty());
        let invalid = ListCursor { updated_at: 10, id: "../../other".into() };
        assert!(list(&dir, Some(&invalid)).is_err());
        assert_eq!(load(&dir, &c.id).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durable_summaries_list_without_reading_transcripts_in_a_fresh_process() {
        const CHILD_DIR: &str = "COUCOU_TEST_CHAT_SUMMARIES_DIR";
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            reset_reads();
            let rows = list(Path::new(&dir), None).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title, "Saved before restart");
            assert_eq!(read_count(), 0, "a new process needs no transcript bodies");
            return;
        }
        let dir = scratch("summary-restart");
        let mut c = conv("c-0000000000000010", 10);
        c.title = "Saved before restart".into();
        c.messages = (0..12).map(|_| msg("user", &"x".repeat(MAX_MESSAGE_BYTES))).collect();
        save(&dir, &c).unwrap();
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].title, c.title);
        assert_eq!(list(&dir, None).unwrap()[0].count, c.messages.len());
        assert_eq!(read_count(), 0, "cold and warm listings use on-disk metadata");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "chat_store::tests::durable_summaries_list_without_reading_transcripts_in_a_fresh_process", "--nocapture"])
            .env(CHILD_DIR, &dir).output().unwrap();
        assert!(child.status.success(), "child failed: {}\n{}",
            String::from_utf8_lossy(&child.stdout), String::from_utf8_lossy(&child.stderr));
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"), "the child test must run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_files_are_validated_only_once_across_all_pages() {
        let dir = scratch("summary-legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let count = MAX_LISTED + 2;
        for n in 0..count {
            let c = conv(&format!("c-{n:016x}"), 10);
            std::fs::write(path(&dir, &c.id).unwrap(), serde_json::to_vec(&c).unwrap()).unwrap();
        }
        reset_reads();
        let first = list(&dir, None).unwrap();
        assert_eq!(first.len(), MAX_LISTED);
        assert_eq!(read_count(), count, "legacy migration validates each body once");
        let last = first.last().unwrap();
        let before = ListCursor { updated_at: last.updated_at, id: last.id.clone() };
        assert_eq!(list(&dir, Some(&before)).unwrap().len(), 2);
        assert_eq!(list(&dir, None).unwrap(), first);
        assert_eq!(read_count(), count, "subsequent pages do not read bodies again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_corrupt_and_invalid_summary_metadata_falls_back_to_the_transcript() {
        let dir = scratch("summary-corrupt");
        let c = conv("c-0000000000000011", 10);
        save(&dir, &c).unwrap();
        let cache = summary_path(&dir, &c.id);
        let valid = std::fs::read(&cache).unwrap();
        std::fs::remove_file(&cache).unwrap();
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].title, c.title);
        assert_eq!(read_count(), 1);
        let mut missing_summary: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        missing_summary.as_object_mut().unwrap().remove("summary");
        let mut bad_version: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        bad_version["version"] = serde_json::json!(0);
        let mut bad_id: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        bad_id["summary"]["id"] = serde_json::json!("c-0000000000000012");
        let mut bad_title: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        bad_title["summary"]["title"] = serde_json::json!("bad\ntitle");
        let mut bad_count: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        bad_count["summary"]["count"] = serde_json::json!(MAX_MESSAGES + 1);
        for bytes in [b"{ not json".to_vec(), vec![b' '; MAX_SUMMARY_BYTES + 1],
            serde_json::to_vec(&missing_summary).unwrap(), serde_json::to_vec(&bad_version).unwrap(),
            serde_json::to_vec(&bad_id).unwrap(),
            serde_json::to_vec(&bad_title).unwrap(), serde_json::to_vec(&bad_count).unwrap()] {
            std::fs::write(&cache, bytes).unwrap();
            reset_reads();
            assert_eq!(list(&dir, None).unwrap()[0].title, c.title);
            assert_eq!(read_count(), 1, "bad metadata triggers validation");
            assert_eq!(list(&dir, None).unwrap()[0].count, c.messages.len());
            assert_eq!(read_count(), 1, "fallback repairs metadata");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_transcripts_are_skipped_and_external_repairs_are_discovered() {
        let dir = scratch("summary-invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let c = conv("c-0000000000000013", 10);
        let file = path(&dir, &c.id).unwrap();
        std::fs::write(&file, b"{ not json").unwrap();
        std::fs::write(dir.join("unrelated.json"), b"{}").unwrap();
        std::fs::create_dir(dir.join("c-0000000000000014.json")).unwrap();
        reset_reads();
        assert!(list(&dir, None).unwrap().is_empty());
        assert_eq!(read_count(), 1);
        assert!(list(&dir, None).unwrap().is_empty());
        assert_eq!(read_count(), 1, "unchanged invalid bodies are not reparsed");
        std::fs::write(&file, serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(list(&dir, None).unwrap()[0].id, c.id);
        assert_eq!(read_count(), 2, "repair invalidates negative metadata");
        let mut invalid = c.clone();
        invalid.messages[0].role = "system".into();
        std::fs::write(&file, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(list(&dir, None).unwrap().is_empty());
        assert_eq!(read_count(), 3, "valid JSON still receives full validation");
        std::fs::write(&file, vec![b' '; MAX_FILE_BYTES + 1]).unwrap();
        assert!(list(&dir, None).unwrap().is_empty());
        assert_eq!(read_count(), 3, "oversized files are skipped before reading a body");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_transient_read_failure_is_not_persisted_as_an_invalid_conversation() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("summary-read-fail");
        let c = conv("c-000000000000001a", 10);
        save(&dir, &c).unwrap();
        let file = path(&dir, &c.id).unwrap();
        let cache = summary_path(&dir, &c.id);
        std::fs::remove_file(&cache).unwrap();
        let before = file_fingerprint(&file).unwrap();
        // An exclusive reader produces a real, temporary sharing violation.
        let blocked = std::fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        assert!(list(&dir, None).unwrap().is_empty());
        assert!(!cache.exists(), "an I/O error is not a negative validation result");
        drop(blocked);
        assert_eq!(file_fingerprint(&file).unwrap(), before);
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].id, c.id);
        assert_eq!(read_count(), 1);
        assert_eq!(list(&dir, None).unwrap()[0].id, c.id);
        assert_eq!(read_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_edits_and_removals_invalidate_durable_summaries() {
        let dir = scratch("summary-external");
        let mut c = conv("c-0000000000000015", 10);
        save(&dir, &c).unwrap();
        let file = path(&dir, &c.id).unwrap();
        let old_metadata = std::fs::metadata(&file).unwrap();
        c.title = "World".into(); // Same byte length: mtime, not size, detects the edit.
        c.updated_at = 20;
        std::fs::write(&file, serde_json::to_vec(&c).unwrap()).unwrap();
        let changed_time = old_metadata.modified().unwrap() + std::time::Duration::from_secs(5);
        std::fs::OpenOptions::new().write(true).open(&file).unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(changed_time)).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().len(), old_metadata.len());
        reset_reads();
        let rows = list(&dir, None).unwrap();
        assert_eq!(rows[0].title, "World");
        assert_eq!(rows[0].updated_at, 20);
        assert_eq!(read_count(), 1);
        assert_eq!(list(&dir, None).unwrap(), rows);
        assert_eq!(read_count(), 1);
        std::fs::remove_file(&file).unwrap();
        assert!(list(&dir, None).unwrap().is_empty(), "orphan metadata never lists a deleted file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_external_replacement_during_validation_is_not_cached_with_the_old_fingerprint() {
        let dir = scratch("summary-read-race");
        std::fs::create_dir_all(&dir).unwrap();
        let c = conv("c-0000000000000016", 10);
        let file = path(&dir, &c.id).unwrap();
        std::fs::write(&file, serde_json::to_vec(&c).unwrap()).unwrap();
        let mut newer = c.clone();
        newer.title = "External replacement".into();
        let replacement = serde_json::to_vec(&newer).unwrap();
        AFTER_TRANSCRIPT_READ.with(|callback| {
            *callback.borrow_mut() = Some(Box::new(move || std::fs::write(file, replacement).unwrap()));
        });
        reset_reads();
        assert_eq!(list(&dir, None).unwrap().len(), 1);
        assert!(!summary_path(&dir, &c.id).exists(), "a changed file cannot certify the old summary");
        assert_eq!(list(&dir, None).unwrap()[0].title, newer.title);
        assert_eq!(read_count(), 2);
        assert_eq!(list(&dir, None).unwrap()[0].title, newer.title);
        assert_eq!(read_count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_changes_after_a_save_cannot_certify_the_saved_summary_for_different_bytes() {
        for atomic in [false, true] {
            let dir = scratch(if atomic { "summary-save-replace" } else { "summary-save-edit" });
            let c = conv("c-000000000000001b", 10);
            let mut external = c.clone();
            external.title = "World".into(); // Same-sized edit to exercise the timestamp stamp.
            let target = path(&dir, &c.id).unwrap();
            let replacement = dir.join("external.tmp");
            let bytes = serde_json::to_vec(&external).unwrap();
            AFTER_TRANSCRIPT_WRITE.with(|callback| {
                *callback.borrow_mut() = Some(Box::new(move || {
                    let changed_time = std::fs::metadata(&target).unwrap().modified().unwrap()
                        + std::time::Duration::from_secs(5);
                    let destination = if atomic { &replacement } else { &target };
                    std::fs::write(destination, bytes).unwrap();
                    std::fs::OpenOptions::new().write(true).open(destination).unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(changed_time)).unwrap();
                    if atomic { std::fs::rename(replacement, target).unwrap(); }
                }));
            });
            save(&dir, &c).unwrap();
            assert!(!summary_path(&dir, &c.id).exists(), "changed bytes must not receive the old summary");
            reset_reads();
            assert_eq!(list(&dir, None).unwrap()[0].title, external.title);
            assert_eq!(read_count(), 1);
            assert_eq!(list(&dir, None).unwrap()[0].title, external.title);
            assert_eq!(read_count(), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn summary_write_failures_preserve_saves_renames_and_listing() {
        let dir = scratch("summary-write-fail");
        std::fs::create_dir_all(&dir).unwrap();
        let cache_dir = dir.join(".summaries");
        std::fs::write(&cache_dir, b"cannot be a directory").unwrap();
        let mut c = conv("c-0000000000000017", 10);
        save(&dir, &c).unwrap();
        rename(&dir, &c.id, "Renamed without metadata").unwrap();
        c.title = "Renamed without metadata".into();
        c.messages.push(msg("user", "A new turn"));
        save(&dir, &c).unwrap();
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].count, 3);
        assert_eq!(read_count(), 1);
        assert_eq!(load(&dir, &c.id).unwrap(), c);
        std::fs::remove_file(&cache_dir).unwrap();
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].title, c.title);
        assert_eq!(read_count(), 1, "retry builds metadata once its folder is writable");
        assert_eq!(list(&dir, None).unwrap()[0].count, 3);
        assert_eq!(read_count(), 1);
        let cache = summary_path(&dir, &c.id);
        let cache_bytes = std::fs::read(&cache).unwrap();
        std::fs::remove_file(&cache).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("block-removal"), cache_bytes).unwrap();
        delete(&dir, &c.id).unwrap();
        assert!(!path(&dir, &c.id).unwrap().exists());
        assert!(list(&dir, None).unwrap().is_empty(), "failed metadata cleanup cannot restore rows");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_summary_refresh_removes_known_stale_metadata_after_a_save_or_rename() {
        let dir = scratch("summary-refresh-fail");
        let mut c = conv("c-0000000000000019", 10);
        save(&dir, &c).unwrap();
        let cache = summary_path(&dir, &c.id);
        let cache_tmp = cache.with_extension("tmp");
        std::fs::create_dir(&cache_tmp).unwrap();
        c.messages.push(msg("user", "A saved new message"));
        save(&dir, &c).unwrap();
        assert!(!cache.exists(), "failed refresh cannot retain the previous summary");
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].count, 3);
        assert_eq!(read_count(), 1);
        assert_eq!(load(&dir, &c.id).unwrap(), c);
        std::fs::remove_dir(&cache_tmp).unwrap();
        assert_eq!(list(&dir, None).unwrap()[0].count, 3);
        assert!(cache.exists());
        std::fs::create_dir(&cache_tmp).unwrap();
        rename(&dir, &c.id, "Fresh title").unwrap();
        assert!(!cache.exists());
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].title, "Fresh title");
        assert_eq!(read_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_transcript_mutations_leave_metadata_usable_and_delete_cleans_up() {
        let dir = scratch("summary-mutation-fail");
        let c = conv("c-0000000000000018", 10);
        save(&dir, &c).unwrap();
        let cache = summary_path(&dir, &c.id);
        let cache_bytes = std::fs::read(&cache).unwrap();
        let tmp = dir.join(format!("{}.tmp", c.id));
        std::fs::create_dir(&tmp).unwrap();
        let mut newer = c.clone();
        newer.updated_at = 20;
        newer.messages.push(msg("user", "Not saved"));
        assert!(save(&dir, &newer).is_err());
        assert!(rename(&dir, &c.id, "Not renamed").is_err());
        assert_eq!(std::fs::read(&cache).unwrap(), cache_bytes);
        reset_reads();
        assert_eq!(list(&dir, None).unwrap()[0].title, c.title);
        assert_eq!(read_count(), 0);
        std::fs::remove_dir(&tmp).unwrap();
        let file = path(&dir, &c.id).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(delete(&dir, &c.id).is_err());
        assert_eq!(std::fs::read(&cache).unwrap(), cache_bytes, "failed delete preserves metadata");
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, bytes).unwrap();
        delete(&dir, &c.id).unwrap();
        assert!(!cache.exists());
        delete(&dir, &c.id).unwrap();
        assert!(list(&dir, None).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Start a valid save, then hold it immediately before STORE acquisition.
    // This reproduces command scheduling inversion without sleeps or relying
    // on the mutex implementation to give either writer priority.
    fn delayed_save(dir: &Path, c: &Conversation) -> (
        std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Result<(), String>>,
    ) {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let dir = dir.to_path_buf();
        let c = c.clone();
        let writer = std::thread::spawn(move || {
            BEFORE_SAVE_LOCK.with(|callback| {
                *callback.borrow_mut() = Some(Box::new(move || {
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }));
            });
            save(&dir, &c)
        });
        ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        (release_tx, writer)
    }

    #[test]
    fn a_delayed_older_save_cannot_undo_newer_messages_or_a_rename() {
        let dir = scratch("snapshot-order");
        let initial = conv("c-0000000000000020", 10);
        save(&dir, &initial).unwrap();
        let mut older = initial.clone();
        older.updated_at = 20;
        older.messages.push(msg("user", "An older extra turn"));
        older.messages.push(msg("assistant", "Old answer"));
        let (release, writer) = delayed_save(&dir, &older);

        // A newer regeneration may be shorter: length cannot order revisions.
        let mut latest = initial.clone();
        latest.updated_at = 30;
        latest.messages[1].content = "Newest regenerated answer".into();
        save(&dir, &latest).unwrap();
        rename(&dir, &latest.id, "Latest explicit title").unwrap();
        latest.title = "Latest explicit title".into();
        let cache = std::fs::read(summary_path(&dir, &latest.id)).unwrap();
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();

        assert_eq!(load(&dir, &latest.id).unwrap(), latest);
        assert_eq!(std::fs::read(summary_path(&dir, &latest.id)).unwrap(), cache,
            "ignored snapshots must not rewrite current summary metadata");
        reset_reads();
        assert_eq!(list(&dir, None).unwrap(), [summary(&latest)]);
        assert_eq!(read_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn equal_timestamps_allow_only_exact_retries_and_prefix_progress() {
        let dir = scratch("snapshot-ties");
        let initial = conv("c-0000000000000021", 10);
        save(&dir, &initial).unwrap();
        rename(&dir, &initial.id, "Keep this title").unwrap();
        let mut appended = initial.clone();
        appended.messages.push(msg("user", "A same-millisecond turn"));
        save(&dir, &appended).unwrap();
        appended.title = "Keep this title".into();
        assert_eq!(load(&dir, &initial.id).unwrap(), appended);
        let cache = std::fs::read(summary_path(&dir, &initial.id)).unwrap();

        save(&dir, &initial).unwrap(); // A delayed strict prefix cannot truncate.
        save(&dir, &appended).unwrap(); // An exact retry is an idempotent no-op.
        let mut conflict = appended.clone();
        conflict.messages[1].content = "Different same-revision answer".into();
        assert!(save(&dir, &conflict).is_err(), "equal-length rewrites are ambiguous");
        conflict.messages.push(msg("assistant", "A longer conflicting transcript"));
        assert!(save(&dir, &conflict).is_err(), "length alone cannot prove progress");
        assert_eq!(load(&dir, &initial.id).unwrap(), appended);
        assert_eq!(std::fs::read(summary_path(&dir, &initial.id)).unwrap(), cache);

        conflict.updated_at += 1;
        conflict.created_at = 999;
        save(&dir, &conflict).unwrap();
        conflict.created_at = initial.created_at;
        assert_eq!(load(&dir, &initial.id).unwrap(), conflict,
            "a distinct revision permits replacement and preserves creation time");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletion_fences_pending_saves_even_before_the_first_transcript_exists() {
        for first_save in [false, true] {
            let dir = scratch(if first_save { "delete-before-first-save" } else { "delete-before-update" });
            let mut pending = conv("c-0000000000000022", 10);
            if !first_save { save(&dir, &pending).unwrap(); }
            pending.updated_at = 20;
            pending.messages.push(msg("user", "Captured before deletion"));
            let (release, writer) = delayed_save(&dir, &pending);
            delete(&dir, &pending.id).unwrap();
            delete(&dir, &pending.id).unwrap(); // Idempotent deletion keeps its fence.
            release.send(()).unwrap();
            assert!(writer.join().unwrap().is_err(), "a pending save must not recreate a deleted id");
            pending.updated_at = u64::MAX;
            assert!(save(&dir, &pending).is_err(), "a newer timestamp cannot bypass deletion");
            assert!(!path(&dir, &pending.id).unwrap().exists());
            assert!(!summary_path(&dir, &pending.id).exists());
            assert!(list(&dir, None).unwrap().is_empty());

            // The tombstone names a directory and id, not all conversations.
            let other_dir = dir.join("another-store");
            save(&other_dir, &pending).unwrap();
            assert_eq!(load(&other_dir, &pending.id).unwrap(), pending);
            pending.id = "c-0000000000000023".into();
            save(&dir, &pending).unwrap();
            assert_eq!(list(&dir, None).unwrap(), [summary(&pending)]);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_failed_delete_does_not_fence_later_saves() {
        let dir = scratch("delete-failure-fence");
        let mut c = conv("c-0000000000000024", 10);
        let file = path(&dir, &c.id).unwrap();
        std::fs::create_dir_all(&file).unwrap();
        assert!(delete(&dir, &c.id).is_err());
        std::fs::remove_dir(&file).unwrap();
        save(&dir, &c).unwrap();
        c.updated_at = 20;
        c.messages[1].content = "Saved after a failed deletion".into();
        save(&dir, &c).unwrap();
        assert_eq!(load(&dir, &c.id).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_ordering_uses_external_transcript_revisions_instead_of_cached_summaries() {
        let dir = scratch("snapshot-external");
        let initial = conv("c-0000000000000025", 10);
        save(&dir, &initial).unwrap();
        let mut external = initial.clone();
        external.title = "Externally edited".into();
        external.updated_at = 30;
        external.messages[1].content = "An external answer".into();
        std::fs::write(path(&dir, &external.id).unwrap(), serde_json::to_vec(&external).unwrap()).unwrap();
        let mut delayed = initial;
        delayed.updated_at = 20;
        save(&dir, &delayed).unwrap();
        assert_eq!(load(&dir, &external.id).unwrap(), external);
        assert_eq!(list(&dir, None).unwrap(), [summary(&external)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_message_saves_keep_the_latest_explicit_title() {
        let dir = scratch("title");
        let mut stale = conv("c-0000000000000003", 10);
        save(&dir, &stale).unwrap();
        rename(&dir, &stale.id, "Lisbon trip").unwrap();

        // A reply or Stop may already have captured the old title in the view.
        stale.updated_at = 20;
        stale.messages.push(msg("user", "What next?"));
        stale.messages.push(SavedMessage { stopped: true, ..msg("assistant", "A partial answer") });
        save(&dir, &stale).unwrap();
        let reopened = load(&dir, &stale.id).unwrap();
        assert_eq!(reopened.title, "Lisbon trip");
        assert_eq!(reopened.messages, stale.messages);
        assert_eq!(reopened.updated_at, 20);
        assert_eq!(reopened.created_at, stale.created_at);

        // The explicit rename still works; another stale save cannot undo it.
        rename(&dir, &stale.id, "Lisbon holiday").unwrap();
        save(&dir, &reopened).unwrap();
        assert_eq!(load(&dir, &stale.id).unwrap().title, "Lisbon holiday");
        assert_eq!(list(&dir, None).unwrap()[0].title, "Lisbon holiday");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_rename_and_save_keep_both_the_title_and_messages() {
        let dir = scratch("title-race");
        let mut stale = conv("c-0000000000000004", 10);
        save(&dir, &stale).unwrap();
        stale.messages.push(msg("user", "What next?"));
        stale.messages.push(msg("assistant", "Day two"));
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                save(&dir, &stale).unwrap();
            });
            scope.spawn(|| {
                barrier.wait();
                rename(&dir, &stale.id, "Lisbon trip").unwrap();
            });
        });
        let back = load(&dir, &stale.id).unwrap();
        assert_eq!(back.title, "Lisbon trip");
        assert_eq!(back.messages, stale.messages);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_and_listing_wait_for_an_in_flight_write() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;

        let dir = scratch("read-write-race");
        let mut latest = conv("c-0000000000000005", 10);
        save(&dir, &latest).unwrap();
        latest.updated_at = 20;
        latest.messages.push(msg("user", "What next?"));
        latest.messages.push(msg("assistant", "Day two"));

        // Pause a writer before replacing the old JSON. The barriers/channels
        // coordinate the readers; no sleeps are used to race the filesystem.
        let guard = STORE.lock().unwrap();
        let ready = Arc::new(Barrier::new(3));
        let (opened_tx, opened_rx) = mpsc::channel();
        let (listed_tx, listed_rx) = mpsc::channel();
        let opener = {
            let ready = Arc::clone(&ready);
            let dir = dir.clone();
            let id = latest.id.clone();
            std::thread::spawn(move || {
                ready.wait();
                opened_tx.send(load(&dir, &id)).unwrap();
            })
        };
        let lister = {
            let ready = Arc::clone(&ready);
            let dir = dir.clone();
            std::thread::spawn(move || {
                ready.wait();
                listed_tx.send(list(&dir, None).unwrap()).unwrap();
            })
        };
        ready.wait();
        let opened_early = opened_rx.recv_timeout(Duration::from_millis(100));
        let listed_early = listed_rx.recv_timeout(Duration::from_millis(100));
        write_locked(&dir, &latest).unwrap();
        drop(guard);

        // Release the writer before asserting, so a regression cannot leave
        // the readers stuck or poison the store lock for the other tests.
        let opened = opened_early.as_ref().ok().cloned().unwrap_or_else(|| {
            opened_rx.recv_timeout(Duration::from_secs(5)).expect("reopen must finish after the write")
        }).unwrap();
        let listed = listed_early.as_ref().ok().cloned().unwrap_or_else(|| {
            listed_rx.recv_timeout(Duration::from_secs(5)).expect("listing must finish after the write")
        });
        opener.join().unwrap();
        lister.join().unwrap();
        assert!(matches!(opened_early, Err(mpsc::RecvTimeoutError::Timeout)), "reopen read before the writer committed");
        assert!(matches!(listed_early, Err(mpsc::RecvTimeoutError::Timeout)), "listing read before the writer committed");
        assert_eq!(opened, latest);
        assert_eq!(context_turns(&opened).last(), Some(&("What next?".into(), "Day two".into())));
        assert_eq!(listed, [Summary { id: latest.id.clone(), title: latest.title.clone(), updated_at: 20, count: 4 }]);

        // Saving the reopened transcript must not discard the committed turn.
        save(&dir, &opened).unwrap();
        rename(&dir, &latest.id, "Trip plans").unwrap();
        assert_eq!(load(&dir, &latest.id).unwrap().messages, latest.messages);
        assert_eq!(list(&dir, None).unwrap()[0].title, "Trip plans");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_errors_release_the_lock_and_preserve_the_saved_file() {
        let dir = scratch("store-errors");
        let saved = conv("c-0000000000000006", 10);
        assert!(list(&dir, None).unwrap().is_empty());
        assert!(load(&dir, &saved.id).is_err());
        save(&dir, &saved).unwrap();

        assert!(rename(&dir, &saved.id, " \n ").is_err());
        assert_eq!(load(&dir, &saved.id).unwrap(), saved);

        // A directory at the temporary-file path makes the write fail on
        // both Windows and Unix, without relying on permission settings.
        let tmp = dir.join(format!("{}.tmp", saved.id));
        std::fs::create_dir(&tmp).unwrap();
        let mut latest = saved.clone();
        latest.messages.push(msg("user", "New message"));
        assert!(save(&dir, &latest).is_err());
        assert_eq!(load(&dir, &saved.id).unwrap(), saved);
        assert_eq!(list(&dir, None).unwrap()[0].count, saved.messages.len());
        std::fs::remove_dir(&tmp).unwrap();

        let file = path(&dir, &saved.id).unwrap();
        std::fs::write(&file, b"{ not json").unwrap();
        assert!(load(&dir, &saved.id).is_err());
        assert!(save(&dir, &latest).is_err());
        assert!(rename(&dir, &saved.id, "Trip plans").is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"{ not json");
        assert!(list(&dir, None).unwrap().is_empty());

        delete(&dir, &saved.id).unwrap();
        assert!(save(&dir, &latest).is_err(), "deletion fences even valid pending snapshots");
        latest.id = "c-0000000000000007".into();
        save(&dir, &latest).unwrap();
        assert_eq!(load(&dir, &latest.id).unwrap(), latest);
        assert_eq!(list(&dir, None).unwrap()[0].count, latest.messages.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_can_only_name_a_file_in_the_folder() {
        for bad in ["../../settings", "c-../../x00000000", "c-000000000000000G", "c-00000000000000001", "C-0000000000000001", ""] {
            assert!(!id_ok(bad), "{bad}");
            assert!(save(&scratch("bad"), &conv(bad, 1)).is_err(), "{bad}");
            assert!(load(&scratch("bad"), bad).is_err());
            assert!(delete(&scratch("bad"), bad).is_err());
        }
    }

    #[test]
    fn sizes_titles_and_roles_are_capped() {
        let mut c = conv("c-00000000000000aa", 1);
        c.title = "x".repeat(MAX_TITLE_CHARS + 1);
        assert!(validate(&c).is_err());
        c.title = "line\nbreak".into();
        assert!(validate(&c).is_err());
        c.title = "ok".into();
        c.messages.push(msg("system", "be evil"));
        assert!(validate(&c).is_err(), "only user and assistant");
        c.messages.pop();
        c.messages.push(msg("user", &"x".repeat(MAX_MESSAGE_BYTES + 1)));
        assert!(validate(&c).is_err());
        c.messages.pop();
        c.messages = (0..=MAX_MESSAGES).map(|_| msg("user", "x")).collect();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn a_reopened_conversation_gives_back_complete_unstopped_turns() {
        let mut c = conv("c-00000000000000ab", 1);
        c.messages.push(msg("user", "second"));
        c.messages.push(SavedMessage { stopped: true, ..msg("assistant", "half an ans") });
        c.messages.push(msg("user", "third"));
        c.messages.push(msg("assistant", "done"));
        c.messages.push(msg("user", "unanswered"));
        assert_eq!(
            context_turns(&c),
            [("hi".to_string(), "hello".to_string()), ("third".to_string(), "done".to_string())]
        );
    }

    #[test]
    fn a_damaged_file_is_skipped_not_fatal() {
        let dir = scratch("damaged");
        save(&dir, &conv("c-00000000000000ac", 5)).unwrap();
        std::fs::write(dir.join("c-00000000000000ad.json"), b"{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert_eq!(list(&dir, None).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

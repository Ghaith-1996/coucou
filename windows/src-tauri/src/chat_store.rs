// Saved chat conversations: one JSON file each in
// %LOCALAPPDATA%\Coucou\conversations, so a conversation can be reopened from
// the chat's history, renamed or deleted. The island builds the record (it
// knows which model and effort answered each message); Rust checks it before
// writing: the id names the file, so it must be exactly `c-` + 16 hex digits,
// and every size is capped. Nothing else is stored, and deleting removes the
// file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_MESSAGES: usize = 400;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_LISTED: usize = 500;

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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub count: usize,
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
    let bytes = serde_json::to_vec(c).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("conversation too large".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let target = path(dir, &c.id).ok_or("bad conversation id")?;
    // Written whole, then renamed: a crash never leaves half a file.
    let tmp = dir.join(format!("{}.tmp", c.id));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())
}

pub fn load(dir: &Path, id: &str) -> Result<Conversation, String> {
    let file = path(dir, id).ok_or("bad conversation id")?;
    let bytes = std::fs::read(file).map_err(|_| crate::i18n::t("This conversation is gone."))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("conversation too large".into());
    }
    let c: Conversation = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&c)?;
    if c.id != id {
        return Err("bad conversation id".into());
    }
    Ok(c)
}

/// Every saved conversation, most recent first. Unreadable files are skipped.
pub fn list(dir: &Path) -> Vec<Summary> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Summary> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?;
            let c = load(dir, id).ok()?;
            Some(Summary { id: c.id, title: c.title, updated_at: c.updated_at, count: c.messages.len() })
        })
        .collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    out.truncate(MAX_LISTED);
    out
}

pub fn rename(dir: &Path, id: &str, title: &str) -> Result<(), String> {
    let mut c = load(dir, id)?;
    c.title = title.trim().to_string();
    save(dir, &c)
}

pub fn delete(dir: &Path, id: &str) -> Result<(), String> {
    let file = path(dir, id).ok_or("bad conversation id")?;
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
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
        let ids: Vec<String> = list(&dir).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["c-0000000000000002", "c-0000000000000001"], "most recent first");
        rename(&dir, "c-0000000000000001", "  Trip plans ").unwrap();
        let back = load(&dir, "c-0000000000000001").unwrap();
        assert_eq!(back.title, "Trip plans");
        assert_eq!(back.messages[1].model.as_deref(), Some("gpt-6.1-sol"), "which model answered is kept");
        delete(&dir, "c-0000000000000001").unwrap();
        delete(&dir, "c-0000000000000001").unwrap();
        assert_eq!(list(&dir).len(), 1);
        assert!(!dir.join("c-0000000000000001.json").exists());
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
        assert_eq!(list(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// The user's Claude Code and Codex conversations, read from the files those
// agents keep, for the chat to summarize when the user asks
// (assistant/tools.rs: list_agent_sessions, read_agent_session).
//
// Where they are (checked on the owner's PC, October 2026):
//   * Claude Code: `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/<folder>/<id>.jsonl`.
//     Sessions from the Claude desktop app's Code tab are the same files
//     (its `claude-code-sessions/*.json` point to them by `cliSessionId`).
//     Lines: `type` "user" / "assistant" with `message.content` (a string or
//     blocks; only `text` blocks are kept), `isMeta`, `isSidechain`, `origin`
//     (`{"kind": "human"}` for what the user typed; task notifications are
//     `task-notification`), `cwd`, `timestamp`; `custom-title` lines name it.
//   * Codex: `<CODEX_HOME or ~/.codex>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`.
//     `session_meta` (payload `cwd`, `timestamp`), then the conversation as
//     `event_msg` `item_completed` items `UserMessage` / `AgentMessage`
//     (content blocks with `text`), or in older versions `event_msg`
//     `user_message` / `agent_message` (`message`). Sessions Codex starts
//     for itself (`source: {"subagent": …}`: reviewers, spawned agents) are
//     left out. With files attached, the request follows "## My request:".
//   * Plain chats in the Claude and ChatGPT desktop apps are kept by their
//     services, not in files here.
//
// Only what was said is kept: no tool calls or their output, no reasoning,
// no instructions the agents were given, no injected reminders. Reading is
// bounded: a fixed number of files, the start of each for the list, the end
// of one for a conversation, and lines still being written are skipped.
// Never called unless the user turned on "Let the chat read my Codex and
// Claude Code conversations" in Settings.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

/// At most this many session files are looked at, newest first.
const MAX_FILES: usize = 500;
/// At most this many directory entries are walked, all folders together.
const MAX_ENTRIES: usize = 20_000;
/// At most this many sessions are listed.
pub const MAX_LISTED: usize = 30;
/// The list reads this much from the start of each file (Codex puts its
/// instructions, tens of kilobytes, before the first message).
const HEAD_BYTES: u64 = 1 << 20;
/// A conversation is read from at most this much of the end of its file.
const TAIL_BYTES: u64 = 4 << 20;
/// What the chat gets of one conversation, in characters.
pub const MAX_TRANSCRIPT_CHARS: usize = 24_000;
/// One message, in characters, before it is shortened.
const MAX_MESSAGE_CHARS: usize = 4_000;
const TITLE_CHARS: usize = 80;

static APP_START_MS: OnceLock<u64> = OnceLock::new();

/// Called once at startup: "since Coucou started" means since then.
pub fn mark_app_start() {
    let _ = APP_START_MS.set(now_ms());
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    AppStart,
    Day,
    Week,
}

impl Since {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "since_coucou_started" => Some(Since::AppStart),
            "last_24_hours" => Some(Since::Day),
            "last_7_days" => Some(Since::Week),
            _ => None,
        }
    }

    pub fn from_ms(self, now: u64) -> u64 {
        const HOUR: u64 = 3_600_000;
        match self {
            Since::AppStart => *APP_START_MS.get().unwrap_or(&now),
            Since::Day => now.saturating_sub(24 * HOUR),
            Since::Week => now.saturating_sub(7 * 24 * HOUR),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Agent {
    #[serde(rename = "Claude Code")]
    Claude,
    #[serde(rename = "Codex")]
    Codex,
}

/// One conversation in the list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// "claude-<uuid>" or "codex-<uuid>": what read_agent_session takes.
    pub id: String,
    pub agent: Agent,
    /// The project folder's name, not its path.
    pub project: String,
    /// Its title, or the start of the first thing the user said.
    pub title: String,
    pub started: String,
    pub last_activity: String,
    #[serde(skip)]
    pub path: PathBuf,
}

/// One conversation, as text for the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    #[serde(flatten)]
    pub session: Session,
    /// The first request and the end of the conversation fit; what was cut
    /// from the middle is said so.
    pub shortened: bool,
    pub text: String,
}

/// Where the agents keep their files.
#[derive(Debug, Clone)]
pub struct Roots {
    pub claude: PathBuf,
    pub codex: PathBuf,
}

impl Roots {
    pub fn from_env() -> Self {
        let home = crate::platform::home_dir();
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        Roots { claude, codex }
    }
}

/// The conversations active since `since_ms`, newest first.
pub fn list(roots: &Roots, since_ms: u64) -> Vec<Session> {
    let mut walked = 0usize;
    let mut files: Vec<(u64, Agent, PathBuf)> = Vec::new();
    for dir in entries(&roots.claude.join("projects"), &mut walked).into_iter().filter(|p| p.is_dir()) {
        for file in entries(&dir, &mut walked) {
            push_if_recent(&mut files, Agent::Claude, file, since_ms);
        }
    }
    for day in codex_days(&roots.codex.join("sessions"), since_ms, &mut walked) {
        for file in entries(&day, &mut walked) {
            push_if_recent(&mut files, Agent::Codex, file, since_ms);
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(MAX_FILES);
    files
        .into_iter()
        .filter_map(|(modified, agent, path)| describe(agent, &path, modified))
        .take(MAX_LISTED)
        .collect()
}

/// One conversation from a fresh list of the last seven days. None for an
/// id that isn't in it: no path ever comes from the model.
pub fn read(roots: &Roots, id: &str, now: u64) -> Option<Transcript> {
    if id.is_empty() || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let session = list(roots, Since::Week.from_ms(now)).into_iter().find(|s| s.id == id)?;
    let mut messages = messages(session.agent, &tail_lines(&session.path, TAIL_BYTES)?);
    // Only the end of a long file was read: its first request comes from the start.
    let gap = std::fs::metadata(&session.path).map_or(false, |m| m.len() > TAIL_BYTES);
    if gap {
        let first = head_lines(&session.path, HEAD_BYTES)
            .and_then(|lines| lines.iter().find_map(|l| message(session.agent, l).filter(|(role, _)| *role == Role::User)));
        if let Some(first) = first {
            messages.insert(0, first);
        }
    }
    let (text, shortened) = transcript_text(&messages, MAX_TRANSCRIPT_CHARS, gap);
    Some(Transcript { session, shortened, text })
}

fn entries(dir: &Path, walked: &mut usize) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in read.flatten() {
        if *walked >= MAX_ENTRIES {
            break;
        }
        *walked += 1;
        out.push(entry.path());
    }
    out
}

fn push_if_recent(files: &mut Vec<(u64, Agent, PathBuf)>, agent: Agent, path: PathBuf, since_ms: u64) {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return;
    }
    let Ok(meta) = std::fs::metadata(&path) else { return };
    if !meta.is_file() {
        return;
    }
    let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
    if modified >= since_ms {
        files.push((modified, agent, path));
    }
}

/// Codex's `YYYY/MM/DD` folders from the day before `since_ms` on (its
/// folders are in local time; a day of slack covers any time zone).
fn codex_days(sessions: &Path, since_ms: u64, walked: &mut usize) -> Vec<PathBuf> {
    let from = civil(since_ms.saturating_sub(86_400_000));
    let mut days = Vec::new();
    for year in entries(sessions, walked) {
        let Some(y) = number(&year) else { continue };
        if y < from.0 {
            continue;
        }
        for month in entries(&year, walked) {
            let Some(m) = number(&month) else { continue };
            if (y, m) < (from.0, from.1) {
                continue;
            }
            for day in entries(&month, walked) {
                let Some(d) = number(&day) else { continue };
                if (y, m, d) >= from && day.is_dir() {
                    days.push(day);
                }
            }
        }
    }
    days
}

fn number(path: &Path) -> Option<u32> {
    path.file_name()?.to_str()?.parse().ok()
}

/// The id, title and dates of one file, from its first lines.
fn describe(agent: Agent, path: &Path, modified: u64) -> Option<Session> {
    let stem = path.file_stem()?.to_str()?;
    let uuid = match agent {
        Agent::Claude => stem,
        // rollout-2026-10-13T19-08-21-<uuid>
        Agent::Codex => stem.get(stem.len().checked_sub(36)?..)?,
    };
    if uuid.len() != 36 || !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let lines = head_lines(path, HEAD_BYTES)?;
    let mut cwd = String::new();
    let mut started = String::new();
    let mut title = String::new();
    let mut first_said = String::new();
    for line in &lines {
        if line.pointer("/payload/source/subagent").is_some() {
            // Codex's own reviewer or a spawned agent, not the user's session.
            return None;
        }
        if cwd.is_empty() {
            if let Some(c) = line.get("cwd").or_else(|| line.pointer("/payload/cwd")).and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        if started.is_empty() {
            if let Some(t) = line.get("timestamp").and_then(Value::as_str) {
                started = t.to_string();
            }
        }
        if let Some(t) = line.get("customTitle").and_then(Value::as_str) {
            title = t.to_string();
        }
        if first_said.is_empty() {
            if let Some((Role::User, text)) = message(agent, line) {
                first_said = text;
            }
        }
    }
    if first_said.is_empty() && title.is_empty() {
        // Nothing was said yet (an empty or just-started session).
        return None;
    }
    let title = if title.trim().is_empty() { first_said } else { title };
    Some(Session {
        id: format!("{}-{uuid}", match agent { Agent::Claude => "claude", Agent::Codex => "codex" }),
        agent,
        project: Path::new(&cwd).file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string(),
        title: shorten(&one_line(&title), TITLE_CHARS),
        started,
        last_activity: iso_utc(modified),
        path: path.to_path_buf(),
    })
}

/// The complete lines in the first `max` bytes. A line cut by the limit
/// (or still being written) is skipped, and so is one that isn't JSON.
fn head_lines(path: &Path, max: u64) -> Option<Vec<Value>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(max));
    let mut lines = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).ok()?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        if let Ok(value) = serde_json::from_slice(&buf) {
            lines.push(value);
        }
    }
    Some(lines)
}

/// The complete lines in the last `max` bytes: the first, maybe cut, line of
/// that window and an unfinished last line are skipped.
fn tail_lines(path: &Path, max: u64) -> Option<Vec<Value>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    let mut chunks: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    // After the last newline: unfinished (or empty).
    chunks.pop();
    let skip_first = start > 0;
    Some(
        chunks
            .into_iter()
            .skip(usize::from(skip_first))
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

fn messages(agent: Agent, lines: &[Value]) -> Vec<(Role, String)> {
    lines.iter().filter_map(|line| message(agent, line)).collect()
}

/// What one line says, if it is something the user or the agent said.
fn message(agent: Agent, line: &Value) -> Option<(Role, String)> {
    let (role, text) = match agent {
        Agent::Claude => claude_message(line)?,
        Agent::Codex => codex_message(line)?,
    };
    let text = without_reminders(&text);
    let text = text.trim();
    if text.is_empty() || (role == Role::User && injected(text)) {
        return None;
    }
    Some((role, text.to_string()))
}

fn claude_message(line: &Value) -> Option<(Role, String)> {
    let role = match line.get("type")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) || line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if role == Role::User {
        if let Some(kind) = line.pointer("/origin/kind").and_then(Value::as_str) {
            if kind != "human" {
                return None;
            }
        }
    }
    Some((role, text_of(line.pointer("/message/content")?)))
}

fn codex_message(line: &Value) -> Option<(Role, String)> {
    if line.get("type")?.as_str()? != "event_msg" {
        return None;
    }
    let payload = line.get("payload")?;
    match payload.get("type")?.as_str()? {
        "user_message" => Some((Role::User, payload.get("message")?.as_str()?.to_string())),
        "agent_message" => Some((Role::Assistant, payload.get("message")?.as_str()?.to_string())),
        "item_completed" => {
            let item = payload.get("item")?;
            let role = match item.get("type")?.as_str()? {
                "UserMessage" => Role::User,
                "AgentMessage" => Role::Assistant,
                _ => return None,
            };
            let text = text_of(item.get("content")?);
            // Attached files come first, then "## My request:" and the words.
            let text = match text.split_once("## My request:") {
                Some((_, request)) if role == Role::User => request.to_string(),
                _ => text,
            };
            Some((role, text))
        }
        _ => None,
    }
}

/// A string, or the text of the text blocks among `[{type, text}]`.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("text" | "Text" | "input_text" | "output_text")))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Text the agent's app put in the user's place, not words the user typed.
fn injected(text: &str) -> bool {
    ["<command-", "<local-command", "<task-notification", "<environment_context", "<user_instructions", "[Request interrupted"]
        .iter()
        .any(|start| text.starts_with(start))
}

fn without_reminders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// "[user] …" / "[agent] …" lines: the first request, then as much of the
/// end as fits in `max` characters. `gap`: messages are already missing
/// after the first one.
fn transcript_text(messages: &[(Role, String)], max: usize, gap: bool) -> (String, bool) {
    let render = |(role, text): &(Role, String)| {
        let who = match role {
            Role::User => "[user]",
            Role::Assistant => "[agent]",
        };
        format!("{who} {}\n", shorten(text, MAX_MESSAGE_CHARS))
    };
    let all: Vec<String> = messages.iter().map(render).collect();
    let total: usize = all.iter().map(|m| m.chars().count()).sum();
    if total <= max && !gap {
        return (all.concat(), false);
    }
    let first_user = messages.iter().position(|(role, _)| *role == Role::User);
    let head = first_user.map(|i| all[i].clone()).unwrap_or_default();
    let marker = "[… earlier messages left out …]\n";
    let mut budget = max.saturating_sub(head.chars().count() + marker.chars().count());
    let mut tail = Vec::new();
    for (i, m) in all.iter().enumerate().rev() {
        let n = m.chars().count();
        if Some(i) == first_user || n > budget {
            break;
        }
        budget -= n;
        tail.push(m.as_str());
    }
    tail.reverse();
    (format!("{head}{marker}{}", tail.concat()), true)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// (year, month, day) in UTC of a time in milliseconds.
fn civil(ms: u64) -> (u32, u32, u32) {
    // Howard Hinnant's days-to-civil.
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as u32, m as u32, d as u32)
}

fn iso_utc(ms: u64) -> String {
    let (y, m, d) = civil(ms);
    let secs = (ms / 1000) % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CLAUDE_ID: &str = "0d9cdae1-e69a-4f81-b88e-bdddc5c8183b";
    const CODEX_ID: &str = "01a13786-29df-7232-bf00-ca76f5a4bfeb";

    fn scratch(test: &str) -> Roots {
        let dir = std::env::temp_dir().join(format!("coucou-sessions-{test}-{}", crate::assistant::policy::new_id("t")));
        Roots { claude: dir.join("claude"), codex: dir.join("codex") }
    }

    fn write_lines(path: &Path, lines: &[Value], tail: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        text.push_str(tail);
        std::fs::write(path, text).unwrap();
    }

    fn claude_file(roots: &Roots) -> PathBuf {
        roots.claude.join("projects").join("D--StockLab").join(format!("{CLAUDE_ID}.jsonl"))
    }

    fn codex_file(roots: &Roots) -> PathBuf {
        let (y, m, d) = civil(now_ms());
        roots.codex.join("sessions").join(format!("{y:04}")).join(format!("{m:02}")).join(format!("{d:02}"))
            .join(format!("rollout-2026-10-13T19-08-21-{CODEX_ID}.jsonl"))
    }

    fn claude_lines() -> Vec<Value> {
        vec![
            json!({"type": "file-history-snapshot", "snapshot": {}}),
            json!({"type": "user", "cwd": "D:\\StockLab", "timestamp": "2026-10-15T01:00:00Z", "isMeta": true, "message": {"role": "user", "content": "<local-command-caveat>x</local-command-caveat>"}}),
            json!({"type": "user", "cwd": "D:\\StockLab", "timestamp": "2026-10-15T01:00:01Z", "origin": {"kind": "human"}, "message": {"role": "user", "content": "Add a <system-reminder>secret context</system-reminder>price chart to the dashboard"}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "hidden"}, {"type": "text", "text": "I'll add the chart."}, {"type": "tool_use", "name": "Bash", "input": {"command": "rm -rf /"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "content": "TOOL OUTPUT"}]}}),
            json!({"type": "user", "origin": {"kind": "task-notification"}, "message": {"role": "user", "content": "<task-notification>done</task-notification>"}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"role": "assistant", "content": [{"type": "text", "text": "subagent chatter"}]}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Done: the chart is on the dashboard."}]}}),
            json!({"type": "custom-title", "customTitle": "Stock chart"}),
        ]
    }

    fn codex_lines() -> Vec<Value> {
        vec![
            json!({"type": "session_meta", "timestamp": "2026-10-15T02:00:00Z", "payload": {"id": CODEX_ID, "cwd": "D:\\cv", "base_instructions": {"text": "You are Codex"}}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "developer rules"}]}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>cwd</environment_context>"}]}}),
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "content": [{"type": "text", "text": "Fix the build script"}]}}}),
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "CommandExecution", "command": "cargo build"}}}),
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "AgentMessage", "content": [{"type": "Text", "text": "The build works again."}], "phase": "final_answer"}}}),
            json!({"type": "event_msg", "payload": {"type": "user_message", "message": "And the old format?"}}),
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "content": [{"type": "text", "text": "# Files mentioned by the user:\n## shot.png: C:/x.png\n## My request:\nwhy is this happening"}]}}}),
            json!({"type": "event_msg", "payload": {"type": "agent_message", "message": "Read too."}}),
        ]
    }

    #[test]
    fn both_agents_files_are_listed_newest_first_with_what_was_said() {
        let roots = scratch("list");
        write_lines(&claude_file(&roots), &claude_lines(), "");
        write_lines(&codex_file(&roots), &codex_lines(), "");
        let listed = list(&roots, 0);
        assert_eq!(listed.len(), 2);
        let claude = listed.iter().find(|s| s.agent == Agent::Claude).unwrap();
        assert_eq!(claude.id, format!("claude-{CLAUDE_ID}"));
        assert_eq!(claude.project, "StockLab");
        assert_eq!(claude.title, "Stock chart", "its custom title wins");
        assert_eq!(claude.started, "2026-10-15T01:00:00Z");
        let codex = listed.iter().find(|s| s.agent == Agent::Codex).unwrap();
        assert_eq!(codex.id, format!("codex-{CODEX_ID}"));
        assert_eq!(codex.project, "cv");
        assert_eq!(codex.title, "Fix the build script", "the first thing the user said");
        let wire = serde_json::to_value(codex).unwrap();
        assert_eq!(wire["agent"], "Codex");
        assert!(wire.get("path").is_none(), "no path ever reaches the model");
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn only_the_words_are_kept_never_tools_reasoning_or_injected_text() {
        let roots = scratch("read");
        write_lines(&claude_file(&roots), &claude_lines(), "");
        write_lines(&codex_file(&roots), &codex_lines(), "");
        let claude = read(&roots, &format!("claude-{CLAUDE_ID}"), now_ms()).unwrap();
        assert_eq!(claude.text, "[user] Add a price chart to the dashboard\n[agent] I'll add the chart.\n[agent] Done: the chart is on the dashboard.\n");
        for hidden in ["secret context", "hidden", "rm -rf", "TOOL OUTPUT", "task-notification", "subagent chatter", "local-command"] {
            assert!(!claude.text.contains(hidden), "{hidden}");
        }
        let codex = read(&roots, &format!("codex-{CODEX_ID}"), now_ms()).unwrap();
        assert_eq!(codex.text, "[user] Fix the build script\n[agent] The build works again.\n[user] And the old format?\n[user] why is this happening\n[agent] Read too.\n");
        for hidden in ["You are Codex", "developer rules", "environment_context", "cargo build"] {
            assert!(!codex.text.contains(hidden), "{hidden}");
        }
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn a_line_still_being_written_or_broken_is_skipped() {
        let roots = scratch("partial");
        let mut lines = claude_lines();
        lines.insert(3, json!("not an object"));
        write_lines(&claude_file(&roots), &lines, "{\"type\":\"assistant\",\"message\":{\"content\":\"half");
        let t = read(&roots, &format!("claude-{CLAUDE_ID}"), now_ms()).unwrap();
        assert!(t.text.ends_with("[agent] Done: the chart is on the dashboard.\n"));
        assert!(!t.text.contains("half"));
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn only_listed_ids_are_read_and_no_path_is_accepted() {
        let roots = scratch("ids");
        write_lines(&claude_file(&roots), &claude_lines(), "");
        for id in ["", "../secret", "claude-..\\x", "C:\\Users\\x.jsonl", "claude-00000000-0000-0000-0000-000000000000"] {
            assert!(read(&roots, id, now_ms()).is_none(), "{id}");
        }
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn sessions_codex_starts_for_itself_are_left_out() {
        let roots = scratch("subagent");
        let mut lines = codex_lines();
        lines[0]["payload"]["source"] = json!({"subagent": {"other": "guardian"}});
        write_lines(&codex_file(&roots), &lines, "");
        assert!(list(&roots, 0).is_empty());
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn a_long_file_still_starts_with_its_first_request() {
        let roots = scratch("long");
        let mut lines = claude_lines();
        let filler = "y".repeat(10_000);
        for i in 0..(TAIL_BYTES as usize / 10_000 + 20) {
            lines.insert(4, json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "name": "Read", "input": {"n": i, "pad": filler}}]}}));
        }
        write_lines(&claude_file(&roots), &lines, "");
        let t = read(&roots, &format!("claude-{CLAUDE_ID}"), now_ms()).unwrap();
        assert!(t.shortened);
        assert!(t.text.starts_with("[user] Add a price chart to the dashboard\n[… earlier messages left out …]\n"), "{}", t.text);
        assert!(t.text.ends_with("[agent] Done: the chart is on the dashboard.\n"));
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn older_sessions_and_other_files_are_left_out() {
        let roots = scratch("since");
        write_lines(&claude_file(&roots), &claude_lines(), "");
        std::fs::write(claude_file(&roots).with_extension("txt"), "x").unwrap();
        assert_eq!(list(&roots, now_ms() + 60_000).len(), 0, "modified before `since`");
        assert_eq!(list(&roots, 0).len(), 1, "the .txt is not a session");
        // A session where nothing was said yet is not listed.
        write_lines(&roots.claude.join("projects").join("D--x").join("11111111-1111-1111-1111-111111111111.jsonl"), &[json!({"type": "mode"})], "");
        assert_eq!(list(&roots, 0).len(), 1);
        let _ = std::fs::remove_dir_all(roots.claude.parent().unwrap());
    }

    #[test]
    fn a_long_conversation_keeps_its_first_request_and_its_end() {
        let mut messages = vec![(Role::User, "first request".to_string())];
        for i in 0..200 {
            messages.push((Role::Assistant, format!("reply {i} {}", "x".repeat(300))));
        }
        let (text, shortened) = transcript_text(&messages, 5_000, false);
        assert!(shortened);
        assert!(text.chars().count() <= 5_000);
        assert!(text.starts_with("[user] first request\n[… earlier messages left out …]\n"));
        assert!(text.ends_with(&format!("reply 199 {}\n", "x".repeat(300))));
        let (short, cut) = transcript_text(&messages[..2], 5_000, false);
        assert!(!cut);
        assert!(short.starts_with("[user] first request\n[agent] reply 0"));
        let (gap, cut) = transcript_text(&messages[..2], 5_000, true);
        assert!(cut, "the end of a long file, after its first request");
        assert!(gap.starts_with("[user] first request\n[… earlier messages left out …]\n[agent] reply 0"));
    }

    /// The real files on this PC: `cargo test real_sessions -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_sessions_on_this_pc() {
        let roots = Roots::from_env();
        let started = std::time::Instant::now();
        let listed = list(&roots, Since::Week.from_ms(now_ms()));
        println!("{} sessions in {:?}", listed.len(), started.elapsed());
        for s in &listed {
            println!("{:?} {} [{}] {} | {} -> {}", s.agent, s.id, s.project, s.title, s.started, s.last_activity);
        }
        for s in listed.iter().take(3) {
            let t = read(&roots, &s.id, now_ms()).expect("listed sessions can be read");
            println!("--- {} ({} chars, shortened {})\n{}", s.id, t.text.chars().count(), t.shortened, t.text.chars().take(600).collect::<String>());
            assert!(t.text.chars().count() <= MAX_TRANSCRIPT_CHARS);
        }
    }

    #[test]
    fn since_names_and_dates() {
        assert_eq!(Since::parse("last_7_days"), Some(Since::Week));
        assert_eq!(Since::parse("since_coucou_started"), Some(Since::AppStart));
        assert_eq!(Since::parse("forever"), None);
        assert_eq!(Since::Day.from_ms(100_000_000), 100_000_000 - 86_400_000);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_791_946_106_017), (2026, 10, 14));
        assert_eq!(iso_utc(1_791_946_106_017), "2026-10-14T02:48:26Z");
    }
}

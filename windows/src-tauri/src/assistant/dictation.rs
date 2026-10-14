// Dictation into another app (`insert-text`, P2).
//
// When a capture starts, the field that had the keyboard is remembered: its
// window, its process and its UI Automation element. Coucou's own windows are
// never a target (the chat has "Use in chat" for that). Once the transcript is
// on the card, the card offers "Insert into <app>" only when that field takes
// text through UI Automation: a writable ValuePattern that is not a password.
//
// On the user's click everything is checked again before a single character
// is written: the window still exists and belongs to the same process, the
// keyboard has not gone to another window meanwhile (Coucou's island taking it
// for its card is fine), and the field is still there, enabled, writable and
// not protected. If any of that fails, nothing is written and the transcript
// stays on the card.
//
// Writing is ValuePattern.SetValue on that one element, never simulated
// keystrokes: a keystroke lands wherever the focus happens to be, SetValue
// only on the element named. The words go where the caret (or the selection)
// was when TextPattern can say where that is and the field's value agrees,
// at the end otherwise. Nothing is ever sent: no Enter, no submit. Success
// needs evidence, the field's value read back with the words in it.
//
// Every UI Automation call runs on one thread of its own (COM MTA), which also
// holds the remembered element; the rest of the app only sees plain data.

use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

use super::contracts::InsertTextArgs;

/// How long a remembered field stays usable: the longest capture plus the
/// time its transcript may wait on the card, and a little slack.
pub const MAX_AGE: Duration = Duration::from_secs(30 + 120 + 10);
/// How long the app waits on the UI Automation thread for one answer. A hung
/// target app must not hang the island.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(4);

/// Why the field cannot take the words (now). Shown on the card; nothing was
/// written whenever one of these comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// The keyboard went to another window after the capture started.
    FocusMoved,
    /// The window, its process or the field is no longer there.
    Gone,
    /// A password field: never typed into.
    Protected,
    /// The field is disabled or read-only.
    ReadOnly,
    /// The field doesn't take text through UI Automation (many rich editors).
    Unsupported,
    /// The app didn't answer in time.
    Busy,
}

/// What the card shows before the user decides: the app the field belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The app as Windows describes it ("Notepad", "Google Chrome").
    pub label: String,
    pub target_window: u64,
    pub target_pid: u32,
}

/// What came of a write the user confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// The value read back contains the words.
    Verified,
    /// SetValue said yes but the value read back doesn't show the words.
    Unconfirmed,
    /// SetValue failed: nothing was written.
    Failed,
}

/// What is known about the field and its window when it is checked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// The remembered window still exists and still belongs to its process.
    pub window_alive: bool,
    /// Which window has the keyboard now.
    pub foreground: Foreground,
    /// The element still answers and is in the same process.
    pub element_alive: bool,
    pub password: bool,
    pub enabled: bool,
    /// None when the element has no ValuePattern at all.
    pub value_read_only: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Foreground {
    /// The remembered window.
    Target,
    /// One of Coucou's own windows (the island took the keyboard for its card).
    Coucou,
    /// Anything else, including another window of the target app.
    Other,
}

/// The first reason the field can't take the words, or None when it can.
pub fn refusal(facts: &Facts) -> Option<Refusal> {
    if !facts.window_alive || !facts.element_alive {
        return Some(Refusal::Gone);
    }
    if facts.foreground == Foreground::Other {
        return Some(Refusal::FocusMoved);
    }
    if facts.password {
        return Some(Refusal::Protected);
    }
    match facts.value_read_only {
        None => Some(Refusal::Unsupported),
        Some(true) => Some(Refusal::ReadOnly),
        Some(false) if !facts.enabled => Some(Refusal::ReadOnly),
        Some(false) => None,
    }
}

/// The field's new value: `text` put where the caret was, replacing the
/// selection, or at the end. `caret` is (text before the caret, selected text)
/// as TextPattern reports them; it is used only when the value agrees with
/// it, since some controls report line breaks differently in the two.
pub fn spliced(value: &str, caret: Option<(&str, &str)>, text: &str) -> String {
    let text = text.trim();
    let (before, after) = match caret {
        Some((before, selected)) if value.starts_with(before) && value[before.len()..].starts_with(selected) => {
            (before, &value[before.len() + selected.len()..])
        }
        _ => (value, ""),
    };
    let lead = if needs_space(before, text) { " " } else { "" };
    let trail = if needs_space(text, after) { " " } else { "" };
    format!("{before}{lead}{text}{trail}{after}")
}

/// A space between two runs of words that would otherwise touch: not after
/// a space or a line break, not before punctuation that hugs the word, and
/// not in Chinese or Japanese, which are written without spaces.
fn needs_space(left: &str, right: &str) -> bool {
    let (Some(l), Some(r)) = (left.chars().next_back(), right.chars().next()) else {
        return false;
    };
    !l.is_whitespace()
        && !r.is_whitespace()
        && !matches!(r, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '…')
        && !matches!(l, '(' | '[' | '{' | '\'' | '"' | '«' | '“')
        && !unspaced(l)
        && !unspaced(r)
}

/// Kana, CJK ideographs and full-width forms.
fn unspaced(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// The words are in the value read back.
pub fn verified(value: &str, text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && value.contains(text)
}

// ── The UI Automation thread ─────────────────────────────────────────────────

type Reply<T> = tokio::sync::oneshot::Sender<T>;

enum Job {
    /// Remember the field that has the keyboard now, for the capture about
    /// to start. Replies once it is remembered (or there is none).
    Probe(Reply<()>),
    /// The capture that probe was for (None: no capture started).
    Bind(Option<String>),
    Offer(String, Reply<Result<Offer, Refusal>>),
    /// Checked again, then written.
    Write(String, InsertTextArgs, Reply<Result<Written, Refusal>>),
}

/// The app's handle on the UI Automation thread. Cheap to share.
pub struct Dictation {
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl Dictation {
    /// Starts the thread. Without one, every answer is "unsupported".
    pub fn start(log: fn(String)) -> Self {
        let (send, recv) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("coucou-dictation".into())
            .spawn(move || run(recv, log));
        if spawned.is_err() {
            log("dictation: could not start its thread".into());
        }
        Self { jobs: Mutex::new(send) }
    }

    fn send(&self, job: Job) {
        let _ = self.jobs.lock().unwrap().send(job);
    }

    async fn ask<T>(&self, job: impl FnOnce(Reply<T>) -> Job, fallback: T) -> T {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send(job(reply));
        match tokio::time::timeout(REPLY_TIMEOUT, answer).await {
            Ok(Ok(value)) => value,
            _ => fallback,
        }
    }

    /// Before a capture starts: remember the field that has the keyboard.
    pub async fn probe(&self) {
        self.ask(Job::Probe, ()).await;
    }

    /// The capture the last probe was for. A stop press returns the capture
    /// that is already remembered, which keeps its field.
    pub fn bind(&self, capture_id: Option<String>) {
        self.send(Job::Bind(capture_id));
    }

    /// The app this capture's field belongs to, if the field can take text.
    pub async fn offer(&self, capture_id: &str) -> Result<Offer, Refusal> {
        let capture = capture_id.to_string();
        self.ask(|reply| Job::Offer(capture, reply), Err(Refusal::Busy)).await
    }

    /// Checks the field again, then writes the confirmed words into it.
    pub async fn write(&self, capture_id: &str, args: InsertTextArgs) -> Result<Written, Refusal> {
        let capture = capture_id.to_string();
        self.ask(|reply| Job::Write(capture, args, reply), Err(Refusal::Busy)).await
    }
}

#[cfg(windows)]
fn run(jobs: mpsc::Receiver<Job>, log: fn(String)) {
    uia::run(jobs, log)
}

/// No UI Automation outside Windows: nothing is ever remembered.
#[cfg(not(windows))]
fn run(jobs: mpsc::Receiver<Job>, _log: fn(String)) {
    for job in jobs {
        match job {
            Job::Probe(reply) => drop(reply.send(())),
            Job::Bind(_) => {}
            Job::Offer(_, reply) => drop(reply.send(Err(Refusal::Unsupported))),
            Job::Write(_, _, reply) => drop(reply.send(Err(Refusal::Gone))),
        }
    }
}

#[cfg(windows)]
mod uia;

#[cfg(test)]
mod tests;

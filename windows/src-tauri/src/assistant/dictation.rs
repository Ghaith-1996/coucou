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
// needs evidence, the field's value read back exactly as intended.
//
// Every UI Automation call runs on one thread of its own (COM MTA), which also
// holds the remembered element; the rest of the app only sees plain data.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::contracts::InsertTextArgs;

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
    /// The field changed while UI Automation was reading it. Retry explicitly.
    ValueChanged,
}

/// A refusal is not evidence that copying is safe: timeouts, missing fields
/// and unavailable UI Automation have not classified the capture's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejection {
    pub reason: Refusal,
    pub copy_allowed: bool,
}

impl From<Refusal> for Rejection {
    fn from(reason: Refusal) -> Self {
        Self { reason, copy_allowed: false }
    }
}

impl Rejection {
    /// Only a live remembered field with a successful negative password check
    /// can allow Copy. The refusal's name alone never grants that permission.
    fn checked(reason: Refusal, facts: &Facts) -> Self {
        Self {
            reason,
            copy_allowed: facts.window_alive && facts.element_alive && !facts.password
                && !matches!(reason, Refusal::Protected | Refusal::Gone | Refusal::Busy | Refusal::ValueChanged),
        }
    }
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
    /// The value read back matches the complete intended value.
    Verified,
    /// The write may have happened (unverified read-back or a reply timeout).
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
    /// True also when the password query failed: unknown is never safe to copy.
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
    if facts.password {
        return Some(Refusal::Protected);
    }
    if facts.foreground == Foreground::Other {
        return Some(Refusal::FocusMoved);
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

/// The exact requested state is read back. A phrase may already have been in
/// the field before an ignored write; matching only that phrase is no proof.
/// Replacing a selection with identical words is a valid no-op when the whole
/// intended value agrees. Do not normalize differences between providers.
pub fn verified(value: &str, intended: &str) -> bool {
    !intended.is_empty() && value == intended
}

/// One capture's field. Transcription has no fixed duration; only an offer
/// supplies the original native preview deadline. Further offers cannot renew
/// it, and stale captures cannot change the current field's lifetime.
struct Remembered<T> {
    capture: String,
    // None without a deadline means this capture started without a target;
    // None with a deadline means expiry released its remembered UIA element.
    // Both retain the capture so a stop press cannot adopt a different field.
    field: Option<T>,
    expires: Option<Instant>,
}

impl<T> Remembered<T> {
    fn new(capture: String, field: T) -> Self {
        Self { capture, field: Some(field), expires: None }
    }

    fn expired(&self, now: Instant) -> bool {
        self.expires.is_some_and(|expires| now >= expires)
    }

    fn expire(&mut self, now: Instant) {
        if self.expired(now) { self.field = None; }
    }

    fn offer(&mut self, capture: &str, expires: Instant, now: Instant) -> Result<&T, Refusal> {
        if self.capture != capture { return Err(Refusal::Gone); }
        self.expires = Some(self.expires.map_or(expires, |original| original.min(expires)));
        self.expire(now);
        self.field.as_ref().ok_or(Refusal::Gone)
    }
}

/// A repeated stop binding cannot replace the original field or revive an
/// expired one. Returns whether a new capture remembered a field.
fn bind_capture<T>(bound: &mut Option<Remembered<T>>, capture: Option<String>, probed: Option<T>) -> bool {
    let Some(capture) = capture else { return false };
    if bound.as_ref().is_some_and(|target| target.capture == capture) { return false; }
    *bound = Some(match probed {
        Some(field) => Remembered::new(capture, field),
        None => Remembered { capture, field: None, expires: None },
    });
    bound.as_ref().is_some_and(|target| target.field.is_some())
}

/// Only no initially captured field means no offer. Ownership, expiry and UIA
/// checks retain their refusal instead of hiding a vanished field's reason.
fn offer_bound<T>(
    bound: &mut Option<Remembered<T>>,
    capture: &str,
    expires: Instant,
    now: Instant,
    check: impl FnOnce(&T) -> Result<Offer, Rejection>,
) -> Result<Option<Offer>, Rejection> {
    let Some(target) = bound.as_mut() else { return Ok(None) };
    if target.capture != capture { return Err(Refusal::Gone.into()); }
    if target.field.is_none() && target.expires.is_none() { return Ok(None); }
    check(target.offer(capture, expires, now)?).map(Some)
}

/// Valid only for the outstanding confirmation, until cancellation or its
/// original preview/grant expiry. The UIA thread checks this just before writing.
#[derive(Debug, Clone)]
pub struct WritePermit {
    pub cancelled: Arc<AtomicBool>,
    pub expires: Instant,
}

impl WritePermit {
    fn check(&self) -> Result<(), Refusal> {
        self.check_at(Instant::now())
    }

    fn check_at(&self, now: Instant) -> Result<(), Refusal> {
        if self.cancelled.load(Ordering::SeqCst) || now >= self.expires {
            Err(Refusal::Gone)
        } else {
            Ok(())
        }
    }
}

/// Revalidate after the potentially slow value/selection reads used to build
/// the insertion. The writer receives only the freshly checked pattern, and
/// every blocking final read is followed by the cancellation/deadline gate.
/// UIA has no atomic compare-and-set: another app can still change after its
/// last answer. Refuse observed changes instead of overwriting stale text.
fn write_revalidated<P>(
    expected: &str,
    mut check_target: impl FnMut() -> Result<P, Rejection>,
    read_value: impl FnOnce(&P) -> Result<String, Rejection>,
    permitted: impl Fn() -> Result<(), Refusal>,
    write: impl FnOnce(P) -> Written,
) -> Result<Written, Rejection> {
    permitted()?;
    let pattern = check_target()?;
    permitted()?;
    let current = read_value(&pattern)?;
    permitted()?;
    // Read mutable safety facts after CurrentValue: it can block while the
    // target becomes a password/read-only field, disappears or loses focus.
    let pattern = check_target()?;
    permitted()?;
    if current != expected {
        return Err(Refusal::ValueChanged.into());
    }
    // Comparing a large field can itself take time. No work that depends on
    // the transcript remains between this final gate and the single effect.
    permitted()?;
    Ok(write(pattern))
}

/// SetValue already succeeded: losing the caller/permit can only skip the
/// remaining work, never turn this attempt into a retryable refusal. Read-back
/// may block past the reply timeout, so refresh ownership and cheap native
/// window/process/foreground state before the optional focus handoff. No UIA
/// calls belong between these final checks and restoring focus.
fn finish_write(
    target_pid: u32,
    read_back: impl FnOnce() -> bool,
    permitted: impl Fn() -> Result<(), Refusal>,
    window_state: impl FnOnce() -> (Option<u32>, Foreground),
    restore_focus: impl FnOnce() -> bool,
) -> Written {
    if permitted().is_err() { return Written::Unconfirmed; }
    let written = if read_back() { Written::Verified } else { Written::Unconfirmed };
    if permitted().is_ok() {
        let (current_pid, foreground) = window_state();
        if current_pid == Some(target_pid) && foreground == Foreground::Coucou && permitted().is_ok() {
            let _ = restore_focus();
        }
    }
    written
}

/// Ownership is checked before consuming anything. Only a definitive
/// pre-write refusal retains the target; every attempted write is terminal.
fn write_bound<T>(
    bound: &mut Option<T>,
    owns: impl FnOnce(&T) -> bool,
    write: impl FnOnce(&T) -> Result<Written, Rejection>,
) -> Result<Written, Rejection> {
    let field = bound.as_ref().filter(|field| owns(field)).ok_or(Refusal::Gone)?;
    let answer = write(field);
    if answer.is_ok() { *bound = None; }
    answer
}

// ── The UI Automation thread ─────────────────────────────────────────────────

type Reply<T> = tokio::sync::oneshot::Sender<T>;

enum Job {
    /// Remember the field that has the keyboard now, for the capture about
    /// to start. Replies once it is remembered (or there is none).
    Probe(Reply<()>),
    /// The capture that probe was for (None: no capture started).
    Bind(Option<String>),
    Offer(String, Instant, Reply<Result<Option<Offer>, Rejection>>),
    /// Checked again, then written.
    Write(String, InsertTextArgs, WritePermit, Reply<Result<Written, Rejection>>),
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
    /// `expires` is the original preview deadline supplied by Voice, never by
    /// the page and never computed anew when the user retries an offer.
    pub async fn offer(&self, capture_id: &str, expires: Instant) -> Result<Option<Offer>, Rejection> {
        let capture = capture_id.to_string();
        self.ask(|reply| Job::Offer(capture, expires, reply), Err(Refusal::Busy.into())).await
    }

    /// Checks the field again, then writes the confirmed words into it.
    pub async fn write(&self, capture_id: &str, args: InsertTextArgs, permit: WritePermit) -> Result<Written, Rejection> {
        let capture = capture_id.to_string();
        // A missing reply cannot prove SetValue never ran. Retire the permit
        // and return unknown, never a retryable "nothing written" refusal.
        let answer = self.ask(|reply| Job::Write(capture, args, permit.clone(), reply), Ok(Written::Unconfirmed)).await;
        permit.cancelled.store(true, Ordering::SeqCst);
        answer
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
            Job::Offer(_, _, reply) => drop(reply.send(Err(Refusal::Unsupported.into()))),
            Job::Write(_, _, _, reply) => drop(reply.send(Err(Refusal::Gone.into()))),
        }
    }
}

#[cfg(windows)]
mod uia;

#[cfg(test)]
mod tests;

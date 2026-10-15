// What happens to a proposed assistant action: checked against its kind's
// closed schema, then allowed, shown for confirmation, or refused.
//
// The order never changes: version, expiry, availability, arguments. Anything
// that fails is `deny`, with a code saying why. `ask` means the user sees the
// exact action and confirms it; the confirmation is a Grant bound to that
// action's id, normalized arguments and expiry, and check_grant recomputes the
// binding on the Rust side instead of trusting what the page sends back.

use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};

use super::contracts::*;

/// Pills the island shows besides the main one (MAX_DECLARED in pills.ts).
pub const MAX_ACTIVE_PILLS: usize = 4;

/// Longest text a draft or an insertion may carry.
pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_QUESTION_CHARS: usize = 1000;
/// How far ahead an action may expire. Anything later is refused: an action
/// is for now, not for whenever.
pub const MAX_LIFETIME_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    /// Why it was refused, when it was.
    pub error: Option<ErrorCode>,
    /// The checked arguments, when it wasn't.
    pub args: Option<ValidArgs>,
}

impl Verdict {
    fn deny(code: ErrorCode) -> Self {
        Self { decision: Decision::Deny, error: Some(code), args: None }
    }
}

pub fn decide(action: &AssistantAction, now_ms: u64) -> Verdict {
    if action.v != CONTRACT_VERSION {
        return Verdict::deny(ErrorCode::Version);
    }
    if now_ms >= action.expires_at_ms {
        return Verdict::deny(ErrorCode::Expired);
    }
    if action.expires_at_ms - now_ms > MAX_LIFETIME_MS || action.id.trim().is_empty() {
        return Verdict::deny(ErrorCode::InvalidArgs);
    }
    if !action.kind.available() {
        return Verdict::deny(ErrorCode::Unavailable);
    }
    let Some(args) = validate(action.kind, &action.args) else {
        return Verdict::deny(ErrorCode::InvalidArgs);
    };
    let decision = match action.kind {
        // Stays inside Coucou and is never sent: the user still has to press Send.
        ActionKind::ChatDraft => Decision::Allow,
        // Coucou's own pills, exactly what Settings → Active pills does, from
        // a click on the card that shows the change. Undone the same way.
        ActionKind::PillSet => Decision::Allow,
        // Anything that reaches outside Coucou is confirmed first.
        ActionKind::InsertText | ActionKind::LaunchApp | ActionKind::AskRegion | ActionKind::SwitchTab => Decision::Ask,
        // The chat proposed it: the user sees exactly what is kept.
        ActionKind::MemorySave => Decision::Ask,
        // Reaches the web: the user sees the address first.
        ActionKind::OpenUrl => Decision::Ask,
    };
    Verdict { decision, error: None, args: Some(args) }
}

/// `args` against `kind`'s schema and limits. Unknown fields, missing fields,
/// wrong types and out-of-range values all fail.
pub fn validate(kind: ActionKind, args: &serde_json::Value) -> Option<ValidArgs> {
    let args = args.clone();
    Some(match kind {
        ActionKind::ChatDraft => {
            let a: ChatDraftArgs = serde_json::from_value(args).ok()?;
            text_ok(&a.text, MAX_TEXT_CHARS).then_some(ValidArgs::ChatDraft(a))?
        }
        ActionKind::PillSet => {
            let a: PillSetArgs = serde_json::from_value(args).ok()?;
            let mut seen = std::collections::HashSet::new();
            (pill_id_ok(&a.main_pill)
                && a.active_integrations.len() <= MAX_ACTIVE_PILLS
                && a.active_integrations.iter().all(|id| pill_id_ok(id) && *id != a.main_pill && seen.insert(id)))
            .then_some(ValidArgs::PillSet(a))?
        }
        ActionKind::InsertText => {
            let a: InsertTextArgs = serde_json::from_value(args).ok()?;
            (text_ok(&a.text, MAX_TEXT_CHARS) && a.target_window != 0 && a.target_pid != 0)
                .then_some(ValidArgs::InsertText(a))?
        }
        ActionKind::LaunchApp => {
            let a: LaunchAppArgs = serde_json::from_value(args).ok()?;
            app_id_ok(&a.app_id).then_some(ValidArgs::LaunchApp(a))?
        }
        ActionKind::MemorySave => {
            let a: MemorySaveArgs = serde_json::from_value(args).ok()?;
            (text_ok(&a.text, crate::memory::MAX_CHARS) && !a.text.contains(['\n', '\r'])).then_some(ValidArgs::MemorySave(a))?
        }
        ActionKind::OpenUrl => {
            let a: OpenUrlArgs = serde_json::from_value(args).ok()?;
            url_ok(&a.url).then_some(ValidArgs::OpenUrl(a))?
        }
        ActionKind::SwitchTab => {
            let a: SwitchTabArgs = serde_json::from_value(args).ok()?;
            let id_ok = (3..=16).contains(&a.tab_id.len())
                && a.tab_id.split('.').count() == 2
                && a.tab_id.split('.').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            (a.window != 0 && id_ok && text_ok(&a.title, 200)).then_some(ValidArgs::SwitchTab(a))?
        }
        ActionKind::AskRegion => {
            let a: AskRegionArgs = serde_json::from_value(args).ok()?;
            let size_ok = |n: u32| (1..=16_384).contains(&n);
            (text_ok(&a.question, MAX_QUESTION_CHARS)
                && !a.monitor.is_empty()
                && a.monitor.len() <= 128
                && size_ok(a.width)
                && size_ok(a.height))
            .then_some(ValidArgs::AskRegion(a))?
        }
    })
}

/// Something to say, not too long, and no control characters besides line
/// breaks and tabs (no escape sequences, no NUL).
fn text_ok(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && text.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
}

/// Longest address `open-url` takes.
pub const MAX_URL_CHARS: usize = 2048;

/// An http(s) address the browser opens as a page: printable ASCII with
/// no spaces or quotes, a host, and no user name or password (which would
/// make "https://google.com@evil.example" look like Google on the card).
pub fn url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= MAX_URL_CHARS
        && url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'))
        && !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
        && !host.starts_with(['.', '-', ':'])
}

/// A pill id as the catalog writes them: `integration_github`, `agent_claude-desktop`.
fn pill_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// An id from the user's list: lowercase letters, digits, `.`, `_`, `-`.
fn app_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

// ── Confirmation bound to one exact action ───────────────────────────────────

/// A user's confirmation of one action. It names the action and a digest of
/// what was shown; it is worthless for any other action, other arguments or
/// after the expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub action_id: String,
    pub digest: String,
    pub expires_at_ms: u64,
}

/// The digest of what the user is shown: domain, id, expiry and the
/// normalized arguments (the checked ones, re-serialized, so key order and
/// whitespace in what was proposed don't matter).
pub fn binding_digest(action: &AssistantAction, args: &ValidArgs) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"coucou-assistant-grant\0");
    bytes.extend_from_slice(&CONTRACT_VERSION.to_be_bytes());
    bytes.extend_from_slice(action.id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&action.expires_at_ms.to_be_bytes());
    bytes.extend_from_slice(&serde_json::to_vec(args).unwrap_or_default());
    hex(digest(&SHA256, &bytes).as_ref())
}

/// The grant for an action the policy says to `ask` about. None for anything
/// it allows or refuses: those need no confirmation, or can't have one.
pub fn grant_for(action: &AssistantAction, now_ms: u64) -> Option<Grant> {
    let verdict = decide(action, now_ms);
    if verdict.decision != Decision::Ask {
        return None;
    }
    let args = verdict.args?;
    Some(Grant {
        action_id: action.id.clone(),
        digest: binding_digest(action, &args),
        expires_at_ms: action.expires_at_ms,
    })
}

/// Checks a returned grant against the action Rust is about to run, now.
/// Everything is decided again: the policy may have changed its mind, and the
/// action may have expired while the user was reading.
pub fn check_grant(grant: &Grant, action: &AssistantAction, now_ms: u64) -> Result<ValidArgs, ErrorCode> {
    let verdict = decide(action, now_ms);
    let args = match (verdict.decision, verdict.args) {
        (Decision::Deny, _) | (_, None) => return Err(verdict.error.unwrap_or(ErrorCode::InvalidArgs)),
        (_, Some(args)) => args,
    };
    if grant.action_id != action.id
        || grant.expires_at_ms != action.expires_at_ms
        || grant.digest != binding_digest(action, &args)
    {
        return Err(ErrorCode::GrantMismatch);
    }
    Ok(args)
}

// ── Ids and time ─────────────────────────────────────────────────────────────

/// A fresh random id, `prefix-` + 16 hex digits.
pub fn new_id(prefix: &str) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 8];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        // Uniqueness within this run is all an id needs.
        static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        bytes = FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_be_bytes();
    }
    format!("{prefix}-{}", hex(&bytes))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

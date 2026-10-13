// The desktop assistant's contracts, version 1: what an assistant action is,
// how it is decided, what comes back once it ran, and the voice states.
//
// These are deliberately a domain of their own. An agent's permission request
// (pipe.rs, AgentPermission in the audit) is answered by a human click on its
// card and nothing else; nothing here produces, carries or converts into one.
// A heard sentence, a model's reply, a routine or a web page can at most become
// an AssistantAction, which only ever acts on the user's own desktop and goes
// through the policy in policy.rs. The `domain` field makes that explicit on
// the wire, and a test keeps this module from reaching the relay at all.
//
// The TypeScript side is src/assistant/contracts.ts; tests/fixtures/
// assistant-contracts.json is read by both test suites to keep them in step.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped on any change to a shape below that is already available.
pub const CONTRACT_VERSION: u32 = 1;

/// The event the island listens to for voice capture.
pub const VOICE_EVENT: &str = "assistant-voice";

/// The only domain an assistant action can belong to. Anything else on the
/// wire fails to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Domain {
    DesktopAssistant,
}

/// The closed registry of what the assistant may do. An unknown kind fails to
/// parse; there is no free-form command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    /// Put text into Coucou's own chat field, unsent. P0.
    ChatDraft,
    /// Set which pills the island shows: the main one and up to four more,
    /// as a spoken pill command asked (P1, the Mac's pill intents).
    PillSet,
    /// Type text into another app's focused field. Reserved for P2.
    InsertText,
    /// Start an app from a list the user controls. Reserved for P2.
    LaunchApp,
    /// Ask about a screen region the user picked. Reserved for P2.
    AskRegion,
}

pub const ACTION_KINDS: [ActionKind; 5] = [
    ActionKind::ChatDraft,
    ActionKind::PillSet,
    ActionKind::InsertText,
    ActionKind::LaunchApp,
    ActionKind::AskRegion,
];

impl ActionKind {
    /// Implemented in this version. A reserved kind is always refused, and its
    /// arguments may still change before it becomes available.
    pub fn available(self) -> bool {
        matches!(self, ActionKind::ChatDraft | ActionKind::PillSet)
    }

    /// Running it twice leaves the same result as running it once. Only
    /// these may be retried after an `unknown` outcome.
    pub fn idempotent(self) -> bool {
        matches!(self, ActionKind::ChatDraft | ActionKind::PillSet)
    }
}

/// An action as proposed, before validation. `args` is checked against the
/// closed schema of its `kind` (policy::validate) before anything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantAction {
    pub v: u32,
    pub domain: Domain,
    pub id: String,
    pub kind: ActionKind,
    pub args: Value,
    /// The voice capture or request this came from, for correlation.
    pub intent_id: String,
    /// Unix milliseconds. Past it, the action is refused, never run late.
    pub expires_at_ms: u64,
}

// ── Arguments, one closed schema per kind ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatDraftArgs {
    pub text: String,
}

/// The whole result, not a change: applying it twice is applying it once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PillSetArgs {
    pub main_pill: String,
    pub active_integrations: Vec<String>,
}

/// Reserved (P2). The target is fixed when the action is proposed and checked
/// again right before writing; a changed focus stops it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InsertTextArgs {
    pub text: String,
    pub target_window: u64,
    pub target_pid: u32,
}

/// Reserved (P2). An id from the user's own list, never a path or a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchAppArgs {
    pub app_id: String,
}

/// Reserved (P2). A region in physical pixels of one monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskRegionArgs {
    pub question: String,
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Arguments once they passed their kind's schema and checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "args", rename_all = "kebab-case")]
pub enum ValidArgs {
    ChatDraft(ChatDraftArgs),
    PillSet(PillSetArgs),
    InsertText(InsertTextArgs),
    LaunchApp(LaunchAppArgs),
    AskRegion(AskRegionArgs),
}

// ── Decisions and receipts ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    Allow,
    /// Shown to the user first; runs only after an explicit confirmation.
    Ask,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// Not the current contract version.
    Version,
    /// The arguments don't match the kind's schema or limits.
    InvalidArgs,
    /// A reserved kind, not in this version.
    Unavailable,
    Expired,
    /// The confirmation doesn't match this exact action.
    GrantMismatch,
    /// The user cancelled.
    Cancelled,
    /// The effect could not be confirmed either way.
    Unverified,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    /// It may or may not have happened (a crash after the effect, say).
    Unknown,
}

/// What came of one action. `succeeded` always carries evidence of the
/// effect; build receipts with the constructors so it can't be left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionReceipt {
    pub action_id: String,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

impl ActionReceipt {
    /// `None` when there is no evidence: then it is not a success.
    pub fn succeeded(action_id: &str, evidence: &str) -> Option<Self> {
        (!evidence.trim().is_empty()).then(|| Self {
            action_id: action_id.to_string(),
            outcome: Outcome::Succeeded,
            evidence_ref: Some(evidence.to_string()),
            error_code: None,
        })
    }

    pub fn failed(action_id: &str, code: ErrorCode) -> Self {
        Self { action_id: action_id.to_string(), outcome: Outcome::Failed, evidence_ref: None, error_code: Some(code) }
    }

    pub fn cancelled(action_id: &str) -> Self {
        Self {
            action_id: action_id.to_string(),
            outcome: Outcome::Cancelled,
            evidence_ref: None,
            error_code: Some(ErrorCode::Cancelled),
        }
    }

    pub fn unknown(action_id: &str) -> Self {
        Self {
            action_id: action_id.to_string(),
            outcome: Outcome::Unknown,
            evidence_ref: None,
            error_code: Some(ErrorCode::Unverified),
        }
    }
}

/// Whether an action may run again after `outcome`. Never automatically
/// after `unknown` unless running it twice is harmless.
pub fn may_retry(kind: ActionKind, outcome: Outcome) -> bool {
    match outcome {
        Outcome::Succeeded | Outcome::Cancelled => false,
        Outcome::Failed => true,
        Outcome::Unknown => kind.idempotent(),
    }
}

// ── Voice ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceState {
    Idle,
    /// The microphone is open. The island always shows it.
    Listening,
    /// Recording stopped; the transcript is being made.
    Processing,
    /// A transcript waits for the user to use it or cancel.
    Preview,
    Speaking,
    Error,
}

pub const VOICE_STATES: [VoiceState; 6] = [
    VoiceState::Idle,
    VoiceState::Listening,
    VoiceState::Processing,
    VoiceState::Preview,
    VoiceState::Speaking,
    VoiceState::Error,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceError {
    /// No microphone, or it went away mid-capture.
    NoMicrophone,
    /// Windows' microphone privacy setting refuses desktop apps.
    MicrophoneDenied,
    /// Nothing was heard.
    Silence,
    /// The speech provider failed.
    Provider,
}

/// One `assistant-voice` event. `capture_id` correlates every event of one
/// capture; a stale one (another id) is ignored by the island.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceEvent {
    pub v: u32,
    pub capture_id: String,
    pub state: VoiceState,
    /// Input level 0…1 while listening, for the indicator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f64>,
    /// Captured length so far, or in total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// The transcript came from the simulated provider, not real speech.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub simulated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<VoiceError>,
}

impl VoiceEvent {
    pub fn new(capture_id: &str, state: VoiceState) -> Self {
        Self {
            v: CONTRACT_VERSION,
            capture_id: capture_id.to_string(),
            state,
            level: None,
            elapsed_ms: None,
            transcript: None,
            simulated: false,
            error: None,
        }
    }
}

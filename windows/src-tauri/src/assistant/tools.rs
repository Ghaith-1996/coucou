// What the chat may do on the user's PC (ChatGPT tools, `coucou` namespace).
//
// The model can only call these, and each one goes through the assistant's
// contracts like any other action:
//   * list_browser_tabs: reads the titles of the user's browser tabs. Read
//     only; the chat shows that it looked.
//   * switch_browser_tab: a `switch-tab` action. The policy says `ask`.
//   * open_app: a `launch-app` action, from the Start menu's own list. The
//     policy says `ask`.
//   * remember: a `memory-save` action, one line for the user's memory
//     (memory.rs). The policy says `ask`.
// "Ask" means the island shows a card in the chat with exactly what will
// happen, and nothing runs until the user clicks it. Rust keeps the action;
// the page only hands the grant back, and check_grant recomputes the binding.
// Declining, two minutes without an answer, Stop or a new question is "no".
// No tool reaches an agent's permission request, ever.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::contracts::*;
use super::policy::{self, Grant};

pub const NAMESPACE: &str = "coucou";
/// How long a card waits for the user.
pub const ANSWER_TTL: Duration = Duration::from_secs(120);
/// To the island: a card to confirm (`Proposal`).
pub const PROPOSAL_EVENT: &str = "chat-action";
/// To the island: a card is no longer waiting (answered, expired, stopped).
pub const CLOSED_EVENT: &str = "chat-action-closed";
/// To the island: a read-only tool was used ("Looked at your browser tabs").
pub const USED_EVENT: &str = "chat-tool-used";

/// Frontend note ownership only; unrelated to the native authorization intent.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolUsed<'a> {
    request_id: Option<&'a str>,
    tool: &'a str,
}

/// The tools, as one namespace (Sign in with ChatGPT accepts function tools
/// only grouped in a namespace).
pub fn definitions() -> Value {
    json!([{
        "type": "namespace",
        "name": NAMESPACE,
        "description": "Act on the user's Windows PC. Every action is shown to the user, who confirms or declines it.",
        "tools": [
            {
                "type": "function",
                "name": "list_browser_tabs",
                "description": "List the tabs open in the user's Chrome, Edge, Brave or Vivaldi windows: id, browser, position (1 = leftmost in its window), title, and whether it is the window's active tab. Use it before switching when you don't know the tab's id.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false },
                "strict": true
            },
            {
                "type": "function",
                "name": "switch_browser_tab",
                "description": "Bring one of the user's browser tabs to the front. Give tab_id from list_browser_tabs (preferred), or position for the frontmost browser window, or title_contains with a part of its title that only that tab has. The user is asked to confirm.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": ["string", "null"], "description": "The tab's id, e.g. \"1.2\"." },
                        "position": { "type": ["integer", "null"], "description": "1-based position in the frontmost browser window." },
                        "title_contains": { "type": ["string", "null"], "description": "Part of the title only that tab has." }
                    },
                    "required": ["tab_id", "position", "title_contains"],
                    "additionalProperties": false
                },
                "strict": true
            },
            {
                "type": "function",
                "name": "remember",
                "description": "Propose to add one short fact about the user to their memory, which every future chat sees (\"Prefers answers in French\", \"Works with Rust\"). Only for lasting facts the user shared, never secrets or passwords. The user sees exactly the text and confirms it.",
                "parameters": {
                    "type": "object",
                    "properties": { "text": { "type": "string", "description": "One line, at most 500 characters, written about the user." } },
                    "required": ["text"],
                    "additionalProperties": false
                },
                "strict": true
            },
            {
                "type": "function",
                "name": "open_app",
                "description": "Open an app installed on the user's PC, by the name the Start menu shows (\"Calculator\", \"Spotify\", \"Visual Studio Code\"). The user is asked to confirm.",
                "parameters": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"],
                    "additionalProperties": false
                },
                "strict": true
            }
        ]
    }])
}

/// What the model is told about the tools, after the chat's own instructions.
pub const INSTRUCTIONS: &str = "You can act on the user's Windows PC only through the coucou tools: list and switch browser tabs, open apps, and propose a fact for the user's memory. Every action is shown to the user, who confirms it; if they decline, say so in a few words. Never claim something happened unless the tool's result says it did. You cannot click, type, read pages or answer an agent's permission request.";

/// One call the model made.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    /// Without the namespace: "switch_browser_tab".
    pub name: String,
    pub arguments: String,
}

/// The tool's name without its namespace, however the stream spells it.
pub fn bare_name(name: &str) -> &str {
    name.rsplit(['.', '/']).next().unwrap_or(name).trim_start_matches("coucou__")
}

#[derive(Deserialize, Default)]
struct SwitchArgs {
    tab_id: Option<String>,
    position: Option<usize>,
    title_contains: Option<String>,
}

#[derive(Deserialize, Default)]
struct OpenArgs {
    name: String,
}

#[derive(Deserialize, Default)]
struct RememberArgs {
    text: String,
}

/// A card for the island: what will happen, and the grant that confirms it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub action_id: String,
    /// Frontend correlation only; never an authorization intent or cancellation key.
    pub request_id: Option<String>,
    pub kind: ActionKind,
    /// The tab's title, or the app's name as Start shows it.
    pub label: String,
    /// The browser, or "app".
    pub detail: String,
    pub grant: Grant,
}

/// Native lifetime of one chat turn. It cannot be supplied or revived by the page.
#[derive(Clone)]
pub struct ActionScope(Arc<Mutex<bool>>);

impl ActionScope {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(true)))
    }

    pub fn same_turn(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn is_active(&self) -> bool {
        *self.0.lock().unwrap()
    }

    /// The execution boundary, checked on the OS worker just before starting.
    /// Cancellation and this check are serialized. Once this succeeds, an OS
    /// operation has started and cannot be undone by stopping the chat. No
    /// lock is held while UI Automation or the shell does potentially slow work.
    fn try_start(&self) -> bool {
        self.is_active()
    }
}

struct Waiting {
    scope: ActionScope,
    answer: tokio::sync::oneshot::Sender<Option<Grant>>,
}

/// Cards waiting for the user, by action id.
#[derive(Default)]
pub struct ChatActions(Mutex<HashMap<String, Waiting>>);

impl ChatActions {
    /// Registration and publication share the scope lock with retirement, so
    /// a late lookup cannot publish a card after its close event was emitted.
    fn propose(&self, scope: &ActionScope, id: &str, answer: tokio::sync::oneshot::Sender<Option<Grant>>, publish: impl FnOnce()) -> bool {
        let active = scope.0.lock().unwrap();
        if !*active {
            return false;
        }
        self.0.lock().unwrap().insert(id.to_string(), Waiting { scope: scope.clone(), answer });
        publish();
        true
    }

    /// Retire grants before waking the cancelled chat future. This also
    /// invalidates a grant already buffered in its confirmation receiver.
    pub fn retire(&self, scope: &ActionScope) -> Vec<String> {
        let mut active = scope.0.lock().unwrap();
        *active = false;
        let mut cards = self.0.lock().unwrap();
        let ids: Vec<String> = cards.iter()
            .filter(|(_, waiting)| waiting.scope.same_turn(scope))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            cards.remove(id);
        }
        ids
    }

    /// The user's click (`Some(grant)`) or "No" (`None`) on a card.
    pub fn answer(&self, action_id: &str, grant: Option<Grant>) -> Result<(), ErrorCode> {
        let waiting = self.0.lock().unwrap().remove(action_id).ok_or(ErrorCode::Expired)?;
        waiting.answer.send(grant).map_err(|_| ErrorCode::Expired)
    }
}

pub fn close_cards(app: &AppHandle, ids: Vec<String>) {
    for id in ids {
        let _ = app.emit_to(crate::island::WINDOW_LABEL, CLOSED_EVENT, id);
    }
}

/// Removes its card when the call ends, however it ends (answered, timed
/// out, or dropped because the chat was stopped).
struct CardGuard<'a> {
    app: &'a AppHandle,
    id: String,
}

impl Drop for CardGuard<'_> {
    fn drop(&mut self) {
        if let Some(actions) = self.app.try_state::<ChatActions>() {
            actions.0.lock().unwrap().remove(&self.id);
        }
        let _ = self.app.emit_to(crate::island::WINDOW_LABEL, CLOSED_EVENT, &self.id);
    }
}

fn log(line: String) {
    crate::log::line(line);
}

/// Shows the card and waits for the user. The checked arguments when they
/// confirmed with the right grant; None for no, timeout or a wrong grant.
async fn confirm(app: &AppHandle, kind: ActionKind, args: Value, label: String, detail: String, intent: &str, request_id: Option<&str>, scope: &ActionScope) -> Option<(String, ValidArgs)> {
    let now = policy::now_ms();
    let action = AssistantAction {
        v: CONTRACT_VERSION,
        domain: Domain::DesktopAssistant,
        id: policy::new_id("act"),
        kind,
        args,
        intent_id: intent.to_string(),
        expires_at_ms: now + ANSWER_TTL.as_millis() as u64,
    };
    let grant = policy::grant_for(&action, now)?;
    let (send, recv) = tokio::sync::oneshot::channel();
    let id = action.id.clone();
    let proposal = Proposal { action_id: id.clone(), request_id: request_id.map(str::to_owned), kind, label, detail, grant };
    let _guard = CardGuard { app, id: id.clone() };
    if !app.state::<ChatActions>().propose(scope, &id, send, || {
        let _ = app.emit_to(crate::island::WINDOW_LABEL, PROPOSAL_EVENT, &proposal);
    }) {
        return None;
    }
    log(format!("chat: {id} proposed"));
    let answer = tokio::time::timeout(ANSWER_TTL, recv).await.ok().and_then(|r| r.ok()).flatten();
    let Some(grant) = answer else {
        log(format!("chat: {id} declined"));
        return None;
    };
    if !scope.is_active() {
        return None;
    }
    match policy::check_grant(&grant, &action, policy::now_ms()) {
        Ok(args) => {
            log(format!("chat: {id} confirmed"));
            Some((id, args))
        }
        Err(code) => {
            log(format!("chat: {id} refused ({code:?})"));
            None
        }
    }
}

fn result(text: impl Into<String>) -> String {
    json!({ "result": text.into() }).to_string()
}

/// Runs one call and returns what the model is told.
pub async fn run(app: &AppHandle, call: &ToolCall, intent: &str, request_id: Option<&str>, scope: &ActionScope) -> String {
    if !scope.is_active() {
        return result("The chat was stopped.");
    }
    match bare_name(&call.name) {
        "list_browser_tabs" => list_tabs(app, request_id, scope).await,
        "switch_browser_tab" => switch_tab(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent, request_id, scope).await,
        "open_app" => open_app(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent, request_id, scope).await,
        "remember" => remember(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent, request_id, scope).await,
        _ => result("There is no such tool."),
    }
}

async fn tabs() -> Result<Vec<super::browser::Tab>, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(super::browser::tabs).await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        Err("only on Windows".into())
    }
}

async fn list_tabs(app: &AppHandle, request_id: Option<&str>, scope: &ActionScope) -> String {
    match tabs().await {
        Ok(tabs) => {
            if !scope.is_active() {
                return result("The chat was stopped.");
            }
            let _ = app.emit_to(crate::island::WINDOW_LABEL, USED_EVENT, ToolUsed { request_id, tool: "browser-tabs" });
            log(format!("chat: browser tabs read ({})", tabs.len()));
            if tabs.is_empty() {
                return result("No Chrome, Edge, Brave or Vivaldi window is open.");
            }
            json!({ "tabs": tabs }).to_string()
        }
        Err(_) => result("The browser tabs could not be read."),
    }
}

/// An ordinal id is not stable identity when two tabs in the same window
/// share the displayed title. Reject that snapshot before asking for a grant,
/// even if one duplicate might disappear before the user answers the card.
fn proposed_tab<'a>(all: &'a [super::browser::Tab], args: &SwitchArgs) -> Option<&'a super::browser::Tab> {
    let tab = super::browser::resolve(all, args.tab_id.as_deref(), args.position, args.title_contains.as_deref())?;
    (all.iter().filter(|candidate| candidate.window == tab.window && candidate.title == tab.title).count() == 1)
        .then_some(tab)
}

async fn switch_tab(app: &AppHandle, args: SwitchArgs, intent: &str, request_id: Option<&str>, scope: &ActionScope) -> String {
    let Ok(all) = tabs().await else { return result("The browser tabs could not be read.") };
    let Some(tab) = proposed_tab(&all, &args).cloned() else {
        return result("No single tab can be identified safely. Call list_browser_tabs and use a tab with a unique title in its window.");
    };
    let action_args = json!({ "window": tab.window, "tabId": tab.id, "title": tab.title });
    let Some((action_id, ValidArgs::SwitchTab(a))) =
        confirm(app, ActionKind::SwitchTab, action_args, tab.title.clone(), tab.browser.clone(), intent, request_id, scope).await
    else {
        return result("The user declined: the tab was not switched.");
    };
    #[cfg(windows)]
    let done = {
        let scope = scope.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if !scope.try_start() {
                return Err("The chat was stopped.".to_string());
            }
            super::browser::switch_if(a.window, a.tab_id, a.title, move || scope.try_start())
        }).await.map_err(|e| e.to_string())
    };
    #[cfg(not(windows))]
    let done: Result<Result<bool, String>, String> = { let _ = a; Err("only on Windows".into()) };
    let receipt = match done {
        Ok(Ok(true)) => ActionReceipt::succeeded(&action_id, "uia:tab-selected-and-foreground").unwrap_or_else(|| ActionReceipt::unknown(&action_id)),
        Ok(Ok(false)) => ActionReceipt::unknown(&action_id),
        _ => ActionReceipt::failed(&action_id, ErrorCode::Failed),
    };
    log(format!("assistant action {action_id} {:?}", receipt.outcome()));
    match receipt.outcome() {
        Outcome::Succeeded => result(format!("Switched to the tab \"{}\".", tab.title)),
        Outcome::Unknown => result("The browser switch couldn't be confirmed."),
        _ => result("The tab could not be switched (it may have moved or closed)."),
    }
}

async fn open_app(app: &AppHandle, args: OpenArgs, intent: &str, request_id: Option<&str>, scope: &ActionScope) -> String {
    #[cfg(windows)]
    {
        let catalog = tauri::async_runtime::spawn_blocking(super::apps::catalog).await.unwrap_or_default();
        let Some(entry) = super::apps::find(&catalog, &args.name).cloned() else {
            return result(format!("No app called \"{}\" in the Start menu.", args.name));
        };
        let Some((action_id, ValidArgs::LaunchApp(a))) = confirm(
            app,
            ActionKind::LaunchApp,
            json!({ "appId": entry.id }),
            entry.label.clone(),
            "app".into(),
            intent,
            request_id,
            scope,
        )
        .await
        else {
            return result("The user declined: the app was not opened.");
        };
        let fresh = tauri::async_runtime::spawn_blocking(super::apps::catalog).await.unwrap_or_default();
        let Some(target) = fresh.into_iter().find(|e| e.id == a.app_id).map(|e| e.target) else {
            return result("The app is no longer in the Start menu.");
        };
        let scope = scope.clone();
        let receipt = super::launch::open_if(action_id.clone(), target, move || scope.try_start())
            .await.unwrap_or_else(|_| ActionReceipt::failed(&action_id, ErrorCode::Failed));
        log(format!("assistant action {action_id} {:?}", receipt.outcome()));
        match receipt.outcome() {
            Outcome::Succeeded => result(format!("{} is open.", entry.label)),
            Outcome::Unknown => result(format!("{} was started; its window wasn't seen yet.", entry.label)),
            _ => result(format!("{} could not be opened.", entry.label)),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, args, intent, request_id, scope);
        result("Opening apps is only on Windows.")
    }
}

async fn remember(app: &AppHandle, args: RememberArgs, intent: &str, request_id: Option<&str>, scope: &ActionScope) -> String {
    let Ok(text) = crate::memory::clean(&args.text) else {
        return result("Nothing to remember: the text was empty or too long.");
    };
    let Some((action_id, ValidArgs::MemorySave(a))) =
        confirm(app, ActionKind::MemorySave, json!({ "text": text }), text.clone(), "memory".into(), intent, request_id, scope).await
    else {
        return result("The user declined: nothing was remembered.");
    };
    let saved = crate::memory::with(|m| {
        // Check after acquiring/loading the memory store, immediately before
        // mutation: a queued write must not survive its chat turn.
        if !scope.try_start() {
            return Err("The chat was stopped.".into());
        }
        m.add(&a.text, crate::memory::Source::Chat, crate::memory::now())
    });
    let receipt = match &saved {
        Ok(item) => ActionReceipt::succeeded(&action_id, &format!("memory:{}", item.id)).unwrap_or_else(|| ActionReceipt::unknown(&action_id)),
        Err(_) => ActionReceipt::failed(&action_id, ErrorCode::Failed),
    };
    log(format!("assistant action {action_id} {:?}", receipt.outcome()));
    match saved {
        Ok(_) => result("Saved to the user's memory."),
        Err(why) => result(format!("Not saved: {why}")),
    }
}

/// The island's answer to a card: the grant on "Do it", nothing on "No".
#[tauri::command]
pub fn chat_action_answer(
    webview: tauri::Webview,
    actions: tauri::State<ChatActions>,
    action_id: String,
    grant: Option<Grant>,
) -> Result<(), ErrorCode> {
    if webview.label() != crate::island::WINDOW_LABEL {
        return Err(ErrorCode::Unavailable);
    }
    actions.answer(&action_id, grant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_notes_carry_only_the_frontend_request_identity_and_tool() {
        let note = ToolUsed { request_id: Some("request-one"), tool: "browser-tabs" };
        assert_eq!(serde_json::to_value(note).unwrap(), json!({
            "requestId": "request-one", "tool": "browser-tabs"
        }));
        let unscoped = ToolUsed { request_id: None, tool: "browser-tabs" };
        assert_eq!(serde_json::to_value(unscoped).unwrap(), json!({
            "requestId": null, "tool": "browser-tabs"
        }));
    }

    fn action_fixture(id: &str) -> (AssistantAction, Grant) {
        let now = policy::now_ms();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: id.into(),
            kind: ActionKind::SwitchTab,
            args: json!({ "window": 1, "tabId": "1.1", "title": "x" }),
            intent_id: "native-chat-intent".into(),
            expires_at_ms: now + 60_000,
        };
        let grant = policy::grant_for(&action, now).unwrap();
        (action, grant)
    }

    #[test]
    fn an_ambiguous_original_tab_snapshot_cannot_create_a_confirmation() {
        let tab = |id: &str, window, position| super::super::browser::Tab {
            id: id.into(), window, position, title: "Inbox".into(), browser: "Chrome".into(), active: false,
        };
        let mut tabs = vec![tab("1.1", 10, 1), tab("1.2", 10, 2), tab("2.1", 20, 1)];
        let by_id = SwitchArgs { tab_id: Some("1.1".into()), ..Default::default() };
        assert!(proposed_tab(&tabs, &by_id).is_none());
        assert!(proposed_tab(&tabs, &SwitchArgs { position: Some(1), ..Default::default() }).is_none());
        tabs.remove(1);
        assert_eq!(proposed_tab(&tabs, &by_id).unwrap().id, "1.1", "the same title in another window is distinct");
    }

    #[test]
    fn proposals_carry_frontend_ownership_without_changing_the_native_grant() {
        let (action, grant) = action_fixture("act-request");
        let mut proposal = Proposal {
            action_id: action.id.clone(), request_id: Some("frontend-request".into()),
            kind: action.kind, label: "x".into(), detail: "browser".into(), grant: grant.clone(),
        };
        let value = serde_json::to_value(&proposal).unwrap();
        assert_eq!(value["requestId"], "frontend-request");
        assert_eq!(value["grant"], serde_json::to_value(&grant).unwrap());
        assert_eq!(action.intent_id, "native-chat-intent");
        assert!(policy::check_grant(&proposal.grant, &action, policy::now_ms()).is_ok());
        proposal.request_id = None;
        assert!(serde_json::to_value(proposal).unwrap()["requestId"].is_null());
    }

    #[test]
    fn reset_retires_pending_grants_before_changing_history() {
        let actions = ChatActions::default();
        let stop = crate::ChatStop::default();
        let chat = crate::chat::Chat::default();
        let (scope, mut cancelled, _) = stop.begin(&actions);
        let (action, grant) = action_fixture("act-reset");
        let (send, mut recv) = tokio::sync::oneshot::channel();
        assert!(actions.propose(&scope, &action.id, send, || {}));
        let mut reset = false;
        let closed = stop.cancel_with(&actions, || {
            assert_eq!(actions.answer(&action.id, Some(grant)), Err(ErrorCode::Expired));
            assert!(!scope.try_start());
            assert_eq!(recv.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed));
            assert_eq!(cancelled.try_recv(), Ok(()));
            chat.reset();
            reset = true;
        });
        assert!(reset);
        assert_eq!(closed, [action.id]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_lookup_that_returns_after_cancellation_cannot_publish_a_card() {
        let actions = Arc::new(ChatActions::default());
        let scope = ActionScope::new();
        let (started, lookup_started) = tokio::sync::oneshot::channel();
        let (complete, lookup_complete) = tokio::sync::oneshot::channel();
        let worker_actions = actions.clone();
        let worker_scope = scope.clone();
        let lookup = tokio::spawn(async move {
            started.send(()).unwrap();
            lookup_complete.await.unwrap();
            let (send, _recv) = tokio::sync::oneshot::channel();
            let mut published = false;
            let registered = worker_actions.propose(&worker_scope, "act-late", send, || published = true);
            (registered, published)
        });
        lookup_started.await.unwrap();
        actions.retire(&scope);
        complete.send(()).unwrap();
        assert_eq!(lookup.await.unwrap(), (false, false));
        assert!(actions.0.lock().unwrap().is_empty());
        assert_eq!(actions.answer("act-late", None), Err(ErrorCode::Expired));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_buffered_valid_grant_loses_its_authority_when_the_turn_is_cancelled() {
        let actions = ChatActions::default();
        let scope = ActionScope::new();
        let (action, grant) = action_fixture("act-buffered");
        let (send, recv) = tokio::sync::oneshot::channel();
        assert!(actions.propose(&scope, &action.id, send, || {}));
        actions.answer(&action.id, Some(grant)).unwrap();
        // The answer has left the waiting map, but has not started OS work.
        assert!(actions.retire(&scope).is_empty());
        let grant = recv.await.unwrap().unwrap();
        assert!(policy::check_grant(&grant, &action, policy::now_ms()).is_ok());
        assert!(!scope.try_start(), "a valid grant cannot revive its retired native turn");
    }

    #[test]
    fn an_os_worker_queued_before_cancellation_cannot_start_after_it() {
        let actions = ChatActions::default();
        let scope = ActionScope::new();
        let worker_scope = scope.clone();
        let (ready, queued) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready.send(()).unwrap();
            resumed.recv().unwrap();
            worker_scope.try_start()
        });
        queued.recv().unwrap();
        actions.retire(&scope);
        resume.send(()).unwrap();
        assert!(!worker.join().unwrap());
    }

    #[test]
    fn finishing_the_old_turn_cannot_retire_a_replacement_turn() {
        let actions = ChatActions::default();
        let stop = crate::ChatStop::default();
        let (old, mut old_cancelled, _) = stop.begin(&actions);
        let (old_send, _old_recv) = tokio::sync::oneshot::channel();
        assert!(actions.propose(&old, "act-old", old_send, || {}));
        let (fresh, mut fresh_cancelled, closed) = stop.begin(&actions);
        assert_eq!(closed, ["act-old"]);
        assert_eq!(old_cancelled.try_recv(), Ok(()));
        assert!(!old.try_start());
        let (fresh_send, fresh_recv) = tokio::sync::oneshot::channel();
        assert!(actions.propose(&fresh, "act-fresh", fresh_send, || {}));
        assert!(stop.finish(&actions, &old).is_empty());
        assert!(fresh.try_start());
        assert_eq!(fresh_cancelled.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty));
        assert_eq!(actions.answer("act-fresh", None), Ok(()));
        assert_eq!(fresh_recv.blocking_recv().unwrap(), None);
        stop.cancel_with(&actions, || {});
        assert!(!fresh.try_start());
        assert_eq!(fresh_cancelled.try_recv(), Ok(()));
    }

    #[test]
    fn the_tools_are_one_namespace_of_strict_functions() {
        let d = definitions();
        assert_eq!(d.as_array().unwrap().len(), 1);
        assert_eq!(d[0]["type"], "namespace");
        assert_eq!(d[0]["name"], NAMESPACE);
        let names: Vec<&str> = d[0]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_browser_tabs", "switch_browser_tab", "remember", "open_app"]);
        for tool in d[0]["tools"].as_array().unwrap() {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["strict"], true);
            assert_eq!(tool["parameters"]["additionalProperties"], false);
        }
    }

    #[test]
    fn a_namespaced_name_is_read_either_way() {
        assert_eq!(bare_name("coucou.switch_browser_tab"), "switch_browser_tab");
        assert_eq!(bare_name("coucou__open_app"), "open_app");
        assert_eq!(bare_name("list_browser_tabs"), "list_browser_tabs");
    }

    #[test]
    fn the_tools_never_mention_agent_permissions_as_something_they_can_do() {
        let text = definitions().to_string().to_lowercase();
        for word in ["approve", "permission", "allow"] {
            assert!(!text.contains(word), "{word}");
        }
    }

    #[test]
    fn an_unanswered_or_unknown_card_cannot_be_answered() {
        let actions = ChatActions::default();
        assert_eq!(actions.answer("act-0000000000000000", None), Err(ErrorCode::Expired));
        let (send, recv) = tokio::sync::oneshot::channel();
        let action = AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: "act-1".into(),
            kind: ActionKind::SwitchTab,
            args: json!({ "window": 1, "tabId": "1.1", "title": "x" }),
            intent_id: "chat".into(),
            expires_at_ms: policy::now_ms() + 60_000,
        };
        assert!(actions.propose(&ActionScope::new(), &action.id, send, || {}));
        assert_eq!(actions.answer("act-1", None), Ok(()));
        assert_eq!(recv.blocking_recv().unwrap(), None);
        assert_eq!(actions.answer("act-1", None), Err(ErrorCode::Expired), "answered once");
    }
}

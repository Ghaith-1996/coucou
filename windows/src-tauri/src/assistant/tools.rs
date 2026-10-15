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
use std::sync::Mutex;
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
    pub kind: ActionKind,
    /// The tab's title, or the app's name as Start shows it.
    pub label: String,
    /// The browser, or "app".
    pub detail: String,
    pub grant: Grant,
}

struct Waiting {
    action: AssistantAction,
    answer: tokio::sync::oneshot::Sender<Option<Grant>>,
}

/// Cards waiting for the user, by action id.
#[derive(Default)]
pub struct ChatActions(Mutex<HashMap<String, Waiting>>);

impl ChatActions {
    /// The user's click (`Some(grant)`) or "No" (`None`) on a card.
    pub fn answer(&self, action_id: &str, grant: Option<Grant>) -> Result<(), ErrorCode> {
        let waiting = self.0.lock().unwrap().remove(action_id).ok_or(ErrorCode::Expired)?;
        waiting.answer.send(grant).map_err(|_| ErrorCode::Expired)
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
async fn confirm(app: &AppHandle, kind: ActionKind, args: Value, label: String, detail: String, intent: &str) -> Option<(String, ValidArgs)> {
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
    app.state::<ChatActions>().0.lock().unwrap().insert(id.clone(), Waiting { action: action.clone(), answer: send });
    let _guard = CardGuard { app, id: id.clone() };
    let proposal = Proposal { action_id: id.clone(), kind, label, detail, grant };
    let _ = app.emit_to(crate::island::WINDOW_LABEL, PROPOSAL_EVENT, &proposal);
    log(format!("chat: {id} proposed"));
    let answer = tokio::time::timeout(ANSWER_TTL, recv).await.ok().and_then(|r| r.ok()).flatten();
    let Some(grant) = answer else {
        log(format!("chat: {id} declined"));
        return None;
    };
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
pub async fn run(app: &AppHandle, call: &ToolCall, intent: &str) -> String {
    match bare_name(&call.name) {
        "list_browser_tabs" => list_tabs(app).await,
        "switch_browser_tab" => switch_tab(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent).await,
        "open_app" => open_app(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent).await,
        "remember" => remember(app, serde_json::from_str(&call.arguments).unwrap_or_default(), intent).await,
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

async fn list_tabs(app: &AppHandle) -> String {
    match tabs().await {
        Ok(tabs) => {
            let _ = app.emit_to(crate::island::WINDOW_LABEL, USED_EVENT, "browser-tabs");
            log(format!("chat: browser tabs read ({})", tabs.len()));
            if tabs.is_empty() {
                return result("No Chrome, Edge, Brave or Vivaldi window is open.");
            }
            json!({ "tabs": tabs }).to_string()
        }
        Err(_) => result("The browser tabs could not be read."),
    }
}

async fn switch_tab(app: &AppHandle, args: SwitchArgs, intent: &str) -> String {
    let Ok(all) = tabs().await else { return result("The browser tabs could not be read.") };
    let Some(tab) = super::browser::resolve(&all, args.tab_id.as_deref(), args.position, args.title_contains.as_deref()).cloned() else {
        return result("No single tab matches. Call list_browser_tabs and pass its tab_id.");
    };
    let action_args = json!({ "window": tab.window, "tabId": tab.id, "title": tab.title });
    let Some((action_id, ValidArgs::SwitchTab(a))) =
        confirm(app, ActionKind::SwitchTab, action_args, tab.title.clone(), tab.browser.clone(), intent).await
    else {
        return result("The user declined: the tab was not switched.");
    };
    #[cfg(windows)]
    let done = tauri::async_runtime::spawn_blocking(move || super::browser::switch(a.tab_id, a.title)).await.map_err(|e| e.to_string());
    #[cfg(not(windows))]
    let done: Result<Result<bool, String>, String> = { let _ = a; Err("only on Windows".into()) };
    let receipt = match done {
        Ok(Ok(true)) => ActionReceipt::succeeded(&action_id, "uia:tab-selected").unwrap_or_else(|| ActionReceipt::unknown(&action_id)),
        Ok(Ok(false)) => ActionReceipt::unknown(&action_id),
        _ => ActionReceipt::failed(&action_id, ErrorCode::Failed),
    };
    log(format!("assistant action {action_id} {:?}", receipt.outcome()));
    match receipt.outcome() {
        Outcome::Succeeded => result(format!("Switched to the tab \"{}\".", tab.title)),
        Outcome::Unknown => result("The tab was selected but couldn't be confirmed."),
        _ => result("The tab could not be switched (it may have moved or closed)."),
    }
}

async fn open_app(app: &AppHandle, args: OpenArgs, intent: &str) -> String {
    #[cfg(windows)]
    {
        let catalog = tauri::async_runtime::spawn_blocking(super::apps::catalog).await.unwrap_or_default();
        let Some(entry) = super::apps::find_typed(&catalog, &args.name).cloned() else {
            return result(format!("No app called \"{}\" in the Start menu.", args.name));
        };
        let Some((action_id, ValidArgs::LaunchApp(a))) = confirm(
            app,
            ActionKind::LaunchApp,
            json!({ "appId": entry.id }),
            entry.label.clone(),
            "app".into(),
            intent,
        )
        .await
        else {
            return result("The user declined: the app was not opened.");
        };
        let fresh = tauri::async_runtime::spawn_blocking(super::apps::catalog).await.unwrap_or_default();
        let Some(target) = fresh.into_iter().find(|e| e.id == a.app_id).map(|e| e.target) else {
            return result("The app is no longer in the Start menu.");
        };
        let receipt = super::launch::open(action_id.clone(), target).await.unwrap_or_else(|_| ActionReceipt::failed(&action_id, ErrorCode::Failed));
        log(format!("assistant action {action_id} {:?}", receipt.outcome()));
        match receipt.outcome() {
            Outcome::Succeeded => result(format!("{} is open.", entry.label)),
            Outcome::Unknown => result(format!("{} was started; its window wasn't seen yet.", entry.label)),
            _ => result(format!("{} could not be opened.", entry.label)),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, args, intent);
        result("Opening apps is only on Windows.")
    }
}

async fn remember(app: &AppHandle, args: RememberArgs, intent: &str) -> String {
    let Ok(text) = crate::memory::clean(&args.text) else {
        return result("Nothing to remember: the text was empty or too long.");
    };
    let Some((action_id, ValidArgs::MemorySave(a))) =
        confirm(app, ActionKind::MemorySave, json!({ "text": text }), text.clone(), "memory".into(), intent).await
    else {
        return result("The user declined: nothing was remembered.");
    };
    let saved = crate::memory::with(|m| m.add(&a.text, crate::memory::Source::Chat, crate::memory::now()));
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
        actions.0.lock().unwrap().insert("act-1".into(), Waiting { action, answer: send });
        assert_eq!(actions.answer("act-1", None), Ok(()));
        assert_eq!(recv.blocking_recv().unwrap(), None);
        assert_eq!(actions.answer("act-1", None), Err(ErrorCode::Expired), "answered once");
    }
}

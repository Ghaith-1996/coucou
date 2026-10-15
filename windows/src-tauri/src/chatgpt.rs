// The chat on the user's ChatGPT plan: "Sign in with ChatGPT" with plan usage
// for open-source and locally hosted apps (developers.openai.com/siwc/
// token-sharing-open-source), then the Responses API with that sign-in.
//
// Sign-in, as the docs describe it:
//   * A loopback listener on 127.0.0.1 (`/auth/callback`), then the system
//     browser on the authorize page, with PKCE (S256), a fresh state and nonce,
//     and the stable `ext_agent_host_id` of this computer (an opaque urn:uuid).
//   * The first time, `client_id=dynamic_agent_client` registers Coucou and the
//     callback carries the issued client id; later sign-ins reuse it.
//   * The code is exchanged with the verifier; the ID token is verified
//     (RS256 against OpenAI's JWKS, issuer, audience, expiry, nonce), and the
//     granted scopes must include `chatgpt.tokens.use.direct`.
//
// Tokens never leave Rust. The refresh token (rotated on every refresh, which
// is serialised) is in the Windows Credential Manager; the access token lives
// in memory only (it lasts an hour). The account's non-secret record (host id,
// client id, email) is `%APPDATA%\Coucou\chatgpt.json`.
//
// Inference is `POST https://api.openai.com/v1/responses` with `store: false`,
// `stream: true`, the whole history in `input`, and the picked model and
// `reasoning.effort`. Nothing ever falls back to another billing path: a
// failure is reported, with the ChatGPT error's own meaning.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::assistant::tools::ToolCall;
use crate::chat::{self, Chat, ChatContext, ChatReply, ModelInfo};
use crate::openai_compat::{file_part, FilePart};
use crate::i18n::{t, tf};
use crate::secrets;

pub const PROVIDER: &str = "chatgpt";

const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const RESPONSES: &str = "https://api.openai.com/v1/responses";
const MODELS: &str = "https://api.openai.com/v1/models";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const AGENT_NAME: &str = "Coucou";
const CALLBACK_PATH: &str = "/auth/callback";
const PREFERRED_PORT: u16 = 1455;
/// Where "Manage usage" goes: ChatGPT's settings.
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/#settings";

/// How long the browser has to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// An access token this close to its expiry is refreshed first.
const REFRESH_MARGIN: Duration = Duration::from_secs(120);
/// The two Credential Manager entries (secrets.rs INTERNAL_KEYS).
pub const REFRESH_TOKEN_KEY: &str = "chatgpt-refresh-token";
pub const ID_TOKEN_KEY: &str = "chatgpt-id-token";

// ── Models and reasoning effort ──────────────────────────────────────────────

/// The two models the chat always offers, in this order.
pub const LUNA: &str = "gpt-6-luna";
pub const SOL: &str = "gpt-6.1-sol";
pub const DEFAULT_EFFORT: &str = "medium";

const ALL_EFFORTS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

/// The `reasoning.effort` values a model takes. GPT-6.1 Sol has no `none`
/// (the reasoning guide); a model of the account's catalog that is neither
/// is offered the values every reasoning model takes.
pub fn efforts(model: &str) -> &'static [&'static str] {
    match model {
        LUNA => &ALL_EFFORTS,
        SOL => &ALL_EFFORTS[1..],
        _ => &ALL_EFFORTS[1..4],
    }
}

pub fn effort_ok(model: &str, effort: &str) -> bool {
    efforts(model).contains(&effort)
}

/// The effort saved for `model`, or the default when it has none or the saved
/// one doesn't apply to it.
pub fn effort_for(saved: Option<&str>, model: &str) -> &'static str {
    let list = efforts(model);
    saved
        .and_then(|s| list.iter().find(|e| **e == s).copied())
        .unwrap_or(if list.contains(&DEFAULT_EFFORT) { DEFAULT_EFFORT } else { list[0] })
}

/// A model of the account's catalog, for the picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub label: String,
    /// False for Luna or Sol when the account's catalog doesn't list them.
    pub available: bool,
    pub efforts: Vec<String>,
}

/// Luna and Sol first, marked unavailable when the catalog lacks them, then
/// the catalog's other models. `/v1/models` lists `slug` and `display_name`;
/// only `visibility == "list"` entries are shown.
pub fn catalog(models: &Value) -> Vec<CatalogModel> {
    let listed: Vec<(String, String)> = models["data"]
        .as_array()
        .or_else(|| models["models"].as_array())
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m["slug"].as_str().or_else(|| m["id"].as_str())?.to_string();
            let label = m["display_name"].as_str().unwrap_or(&id).to_string();
            Some((id, label))
        })
        .collect();
    let entry = |id: &str, label: String, available: bool| CatalogModel {
        id: id.to_string(),
        label,
        available,
        efforts: efforts(id).iter().map(|e| e.to_string()).collect(),
    };
    let mut out = Vec::new();
    for (id, fallback) in [(LUNA, "GPT-6 Luna"), (SOL, "GPT-6.1 Sol")] {
        match listed.iter().find(|(m, _)| m == id) {
            Some((_, label)) => out.push(entry(id, label.clone(), true)),
            None => out.push(entry(id, fallback.to_string(), false)),
        }
    }
    for (id, label) in &listed {
        if id != LUNA && id != SOL {
            out.push(entry(id, label.clone(), true));
        }
    }
    out
}

// ── The account record ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    /// This computer's `ext_agent_host_id`: an opaque `urn:uuid:…`, made once.
    pub host_id: String,
    /// Issued at registration (`oaiapp_…`); retained even if sign-in then fails.
    /// Never `dynamic_agent_client`; identity is only set after verification.
    pub client_id: Option<String>,
    pub email: Option<String>,
    pub sub: Option<String>,
    /// The current token grant permits `chatgpt.tokens.use.direct`.
    pub plan_enabled: bool,
    /// "You're using your ChatGPT plan" was shown once.
    pub welcomed: bool,
    /// Switching is allowed only after explicit, fully persisted cleanup.
    pub explicitly_disconnected: bool,
    /// The session was rejected, or a refresh/credential replacement has not
    /// completed safely. Persisted so a crash or failed cleanup reconnects.
    pub session_needs_reconnect: bool,
    /// Inactive registrations have no credentials: only one session is live.
    pub saved_accounts: Vec<Registration>,
    /// Retain an issued client even if a new account's exchange/checks fail.
    pub pending_account: Option<Registration>,
}

/// Non-secret account/client mapping. Never mix registrations by email.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub client_id: String,
    pub email: Option<String>,
    pub sub: Option<String>,
    pub plan_enabled: bool,
    pub welcomed: bool,
}

impl Account {
    fn registration(&self) -> Option<Registration> {
        Some(Registration {
            client_id: self.client_id.clone()?, email: self.email.clone(), sub: self.sub.clone(),
            plan_enabled: self.plan_enabled, welcomed: self.welcomed,
        })
    }

    fn select_registration(&self, selected: &Registration) -> Self {
        let mut next = self.clone();
        next.saved_accounts.retain(|r| r.client_id != selected.client_id);
        if let Some(previous) = self.registration().filter(|r| r.client_id != selected.client_id) {
            next.saved_accounts.retain(|r| r.client_id != previous.client_id);
            next.saved_accounts.push(previous);
        }
        next.client_id = Some(selected.client_id.clone());
        next.email = selected.email.clone();
        next.sub = selected.sub.clone();
        next.plan_enabled = selected.plan_enabled;
        next.welcomed = selected.welcomed;
        next
    }
}

fn account_path() -> PathBuf {
    crate::settings::config_dir().join("chatgpt.json")
}

fn read_account_from(path: &Path) -> Result<Account, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Account::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn save_account(account: &Account) -> Result<(), String> {
    save_account_to(&account_path(), account)
}

fn save_account_to(path: &Path, account: &Account) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        crate::platform::ensure_private_dir(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_vec_pretty(account).map_err(|e| e.to_string())?;
    // All mutations share REFRESHING. A failed write must leave the previous
    // client/identity mapping intact, including when disconnect is retried.
    let temp = path.with_extension("json.tmp");
    // Replace a stale temp entry, never follow a symlink or reuse a publicly
    // readable inode that another user might still have open.
    if let Err(error) = std::fs::remove_file(&temp) {
        if error.kind() != std::io::ErrorKind::NotFound { return Err(error.to_string()); }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // Only clean up an entry this attempt created; create_new can fail if
    // another entry appeared, and that entry must remain untouched.
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    let result = file.write_all(&text).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() { let _ = std::fs::remove_file(&temp); }
    result.map_err(|e| e.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavedAccount {
    pub id: String,
    pub email: Option<String>,
}

/// What the island and Settings may know: never a token.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub account_id: Option<String>,
    pub can_switch_accounts: bool,
    pub saved_accounts: Vec<SavedAccount>,
    pub has_pending_account: bool,
    /// Fences retry/replacement against the unfinished registration shown.
    pub pending_account_id: Option<String>,
    pub connected: bool,
    /// Either saved credential remains (including an orphaned hint), or the
    /// keystore could not confirm their absence. Cleanup stays available.
    pub has_saved_credentials: bool,
    pub email: Option<String>,
    pub plan_enabled: bool,
    /// Signed in before, but the session ended: "Reconnect".
    pub needs_reconnect: bool,
    /// Show "You're using your ChatGPT plan" once.
    pub show_welcome: bool,
    pub manage_usage_url: &'static str,
}

pub fn status() -> Status {
    let active = ACTIVE_REFRESH.lock().unwrap();
    status_from_storage(read_account_from(&account_path()), credential_presence(), active.as_deref(), REJECTED_SESSION.lock().unwrap().as_deref())
}

fn status_from_storage(account: Result<Account, String>, credentials: Result<(bool, bool), String>, active_refresh: Option<&str>, rejected_client: Option<&str>) -> Status {
    let unreadable = account.is_err() || credentials.is_err();
    let mut account = account.unwrap_or_default();
    // The durable guard is provisional only while this process is refreshing
    // this same, previously usable session. Do not flicker to disconnected or
    // wait behind a browser sign-in. Failed reads always remain fail-closed.
    if !unreadable && active_refresh.is_some() && active_refresh == account.client_id.as_deref() {
        account.session_needs_reconnect = false;
    }
    apply_session_rejection(&mut account, rejected_client);
    // A locked/unreadable keystore is not proof that cleanup succeeded.
    let (refresh, id) = credentials.unwrap_or((true, true));
    let mut status = status_for(&account, refresh, id);
    if unreadable {
        status.connected = false;
        status.can_switch_accounts = false;
        status.needs_reconnect = true;
        status.show_welcome = false;
    }
    status
}

fn apply_session_rejection(account: &mut Account, rejected_client: Option<&str>) {
    if rejected_client.is_some() && rejected_client == account.client_id.as_deref() {
        account.session_needs_reconnect = true;
    }
}

fn status_for(account: &Account, has_refresh: bool, has_id_token: bool) -> Status {
    let connected = has_refresh && account.client_id.is_some()
        && !account.explicitly_disconnected && !account.session_needs_reconnect;
    Status {
        account_id: account.client_id.clone(),
        can_switch_accounts: account.explicitly_disconnected && !has_refresh && !has_id_token,
        saved_accounts: account.saved_accounts.iter().map(|r| SavedAccount { id: r.client_id.clone(), email: r.email.clone() }).collect(),
        has_pending_account: account.pending_account.is_some(),
        pending_account_id: account.pending_account.as_ref().map(|r| r.client_id.clone()),
        connected,
        has_saved_credentials: has_refresh || has_id_token,
        email: account.email.clone(),
        plan_enabled: account.plan_enabled,
        needs_reconnect: !connected && account.client_id.is_some(),
        show_welcome: connected && account.plan_enabled && !account.welcomed,
        manage_usage_url: MANAGE_USAGE_URL,
    }
}

pub async fn mark_welcomed(client_id: Option<&str>) -> Result<(), String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let account = read_account_from(&account_path())?;
    mark_welcomed_for(&account, client_id, secrets::get_internal(REFRESH_TOKEN_KEY).is_some(), save_account)
}

fn mark_welcomed_for(account: &Account, client_id: Option<&str>, connected: bool, persist: impl FnOnce(&Account) -> Result<(), String>) -> Result<(), String> {
    // An old window's acknowledgement must never mark a different account.
    if client_id.is_none() || client_id != account.client_id.as_deref() || !status_for(account, connected, false).show_welcome {
        return Ok(());
    }
    let mut next = account.clone();
    next.welcomed = true;
    persist(&next)
}

fn credential_presence() -> Result<(bool, bool), String> {
    let refresh = secrets::read_internal(REFRESH_TOKEN_KEY);
    let id = secrets::read_internal(ID_TOKEN_KEY);
    match (refresh, id) {
        (Ok(refresh), Ok(id)) => Ok((refresh.is_some(), id.is_some())),
        _ => Err(t("Couldn't check saved ChatGPT credentials. Try again.")),
    }
}

fn require_signed_out(account: &Account, credentials: Result<(bool, bool), String>) -> Result<(), String> {
    require_empty_credentials(credentials)?;
    if !account.explicitly_disconnected {
        return Err(t("Disconnect ChatGPT successfully before switching accounts."));
    }
    Ok(())
}

fn require_empty_credentials(credentials: Result<(bool, bool), String>) -> Result<(), String> {
    let (refresh, id) = credentials?;
    if refresh || id {
        return Err(t("Disconnect ChatGPT successfully before switching accounts."));
    }
    Ok(())
}

fn new_account_candidate(account: &Account) -> Account {
    let blank = Account { host_id: account.host_id.clone(), ..Default::default() };
    account.pending_account.as_ref().map_or_else(|| blank.clone(), |pending| blank.select_registration(pending))
}

fn retain_pending_account(account: &mut Account, candidate: &Account, persist: impl FnOnce(&Account) -> Result<(), String>) -> Result<(), String> {
    let registration = candidate.registration().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    if account.pending_account.is_some()
        || account.client_id.as_deref() == Some(registration.client_id.as_str())
        || account.saved_accounts.iter().any(|r| r.client_id == registration.client_id) {
        return Err(t("ChatGPT sign-in failed."));
    }
    let mut next = account.clone();
    next.pending_account = Some(registration);
    persist(&next)?;
    *account = next;
    Ok(())
}

/// Only an explicit start-another action may set the unfinished sign-in
/// aside. Keep its issued client and identity recoverable in the saved picker.
/// The displayed pending ID is also checked for retries: a stale click must
/// not continue a different registration or create a new one after removal.
fn prepare_new_account(
    account: &Account,
    pending_account_id: Option<&str>,
    start_another: bool,
    credentials: Result<(bool, bool), String>,
    persist: impl FnOnce(&Account) -> Result<(), String>,
) -> Result<Account, String> {
    require_signed_out(account, credentials)?;
    if account.pending_account.as_ref().map(|r| r.client_id.as_str()) != pending_account_id
        || (start_another && pending_account_id.is_none()) {
        return Err(t("The unfinished ChatGPT sign-in changed. Try again."));
    }
    if !start_another { return Ok(account.clone()); }
    let pending = account.pending_account.as_ref().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    if account.client_id.as_deref() == Some(pending.client_id.as_str())
        || account.saved_accounts.iter().any(|r| r.client_id == pending.client_id) {
        return Err(t("ChatGPT sign-in failed."));
    }
    let mut next = account.clone();
    next.saved_accounts.push(pending.clone());
    next.pending_account = None;
    // Do not launch a new flow or lose the retry path if this write fails.
    persist(&next)?;
    Ok(next)
}

/// Browser sign-ins must not queue behind another operation. Otherwise a
/// duplicate click could open a second browser after the first attempt ends.
fn try_sign_in_lock(serial: &tokio::sync::Mutex<()>) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
    serial.try_lock().map_err(|_| t("Another ChatGPT operation is in progress. Try again."))
}

/// Selecting saved metadata never grants access; Reconnect still validates
/// that registration's subject. The shared credential slots must be empty.
pub async fn select_account(client_id: &str) -> Result<Status, String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let account = read_account_from(&account_path())?;
    require_signed_out(&account, credential_presence())?;
    let selected = account.saved_accounts.iter().find(|r| r.client_id == client_id)
        .ok_or_else(|| t("ChatGPT sign-in failed."))?;
    let next = account.select_registration(selected);
    save_account(&next)?;
    invalidate_session();
    Ok(status_for(&next, false, false))
}

// ── Encoding helpers ─────────────────────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding (RFC 7636 / JWT).
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..chunk.len() + 1 {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        bits = bits << 6 | v as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

fn random(bytes: usize) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).expect("system randomness");
    b64url(&buf)
}

/// A version 4 UUID as `urn:uuid:…`, the host id format the docs accept.
pub fn new_host_id() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut b = [0u8; 16];
    SystemRandom::new().fill(&mut b).expect("system randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("urn:uuid:{}-{}-{}-{}-{}", h[..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..].concat())
}

pub fn pkce_challenge(verifier: &str) -> String {
    b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ── Sign-in ──────────────────────────────────────────────────────────────────

/// One attempt's secrets: kept in this process only.
pub struct Attempt {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect_uri: String,
}

impl Attempt {
    pub fn new(port: u16) -> Self {
        Attempt {
            state: random(32),
            nonce: random(32),
            verifier: random(48),
            redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
        }
    }
}

/// The authorize URL. A returning account reuses its issued client id (and
/// hints its identity); a new one registers with `dynamic_agent_client`.
pub fn authorize_url(account: &Account, attempt: &Attempt, id_token_hint: Option<&str>) -> String {
    let mut url = Url::parse(AUTHORIZE).expect("authorize URL");
    {
        let mut q = url.query_pairs_mut();
        match &account.client_id {
            Some(client) => {
                q.append_pair("client_id", client);
                // connect() is user-initiated. A previous sign-in without plan
                // access needs the consent screen again; ordinary reconnects
                // with an existing grant should keep their normal flow.
                // https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery
                if !account.plan_enabled {
                    q.append_pair("prompt", "consent");
                }
                if let Some(hint) = id_token_hint {
                    q.append_pair("id_token_hint", hint);
                }
                if let Some(email) = &account.email {
                    q.append_pair("login_hint", email);
                }
            }
            None => {
                q.append_pair("client_id", DYNAMIC_CLIENT);
                q.append_pair("agent_name_hint", AGENT_NAME);
            }
        }
        q.append_pair("ext_agent_host_id", &account.host_id);
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", &attempt.redirect_uri);
        q.append_pair("scope", SCOPE);
        q.append_pair("resource", RESOURCE);
        q.append_pair("state", &attempt.state);
        q.append_pair("nonce", &attempt.nonce);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("code_challenge", &pkce_challenge(&attempt.verifier));
    }
    // `+` for spaces in scope is what the callback example shows; %20 is safer.
    url.to_string().replace('+', "%20")
}

/// What the browser came back with.
#[derive(Debug, PartialEq)]
pub enum Callback {
    Code { code: String, client_id: Option<String> },
    Denied,
    Error(String),
}

/// The callback's query, checked: the state must be this attempt's, and an
/// issued client id must not contradict the one already known.
pub fn read_callback(target: &str, attempt: &Attempt, known_client: Option<&str>) -> Callback {
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Error("bad callback".into());
    };
    let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    if q.get("state").map(String::as_str) != Some(attempt.state.as_str()) {
        return Callback::Error("state mismatch".into());
    }
    if let Some(error) = q.get("error") {
        return if error == "access_denied" { Callback::Denied } else { Callback::Error(error.clone()) };
    }
    let Some(code) = q.get("code").filter(|c| !c.is_empty()) else {
        return Callback::Error("no code".into());
    };
    let issued = q.get("client_id").filter(|c| !c.is_empty() && c.as_str() != DYNAMIC_CLIENT).cloned();
    match (known_client, &issued) {
        (Some(known), Some(issued)) if known != issued => Callback::Error("another client".into()),
        (None, None) => Callback::Error("registration incomplete".into()),
        _ => Callback::Code { code: code.clone(), client_id: issued.or(known_client.map(str::to_string)) },
    }
}

/// Validate the callback and retain a new registration before returning its
/// code for exchange. Exchange or ID-token verification can fail afterwards;
/// the next attempt must still reuse this issued client id. No identity,
/// permission, or credential is accepted here.
fn prepare_code_exchange(
    account: &mut Account,
    attempt: &Attempt,
    target: &str,
    persist: impl FnOnce(&Account) -> Result<(), String>,
) -> Result<(String, String), String> {
    let (code, client_id) = match read_callback(target, attempt, account.client_id.as_deref()) {
        Callback::Code { code, client_id } => (code, client_id.ok_or_else(|| t("ChatGPT sign-in failed."))?),
        Callback::Denied => return Err(t("ChatGPT sign-in was cancelled.")),
        Callback::Error(why) => {
            crate::log::line(format!("chatgpt: sign-in refused ({why})"));
            return Err(t("ChatGPT sign-in failed."));
        }
    };
    if account.client_id.is_none() {
        let mut registration = account.clone();
        registration.client_id = Some(client_id.clone());
        persist(&registration)?;
        *account = registration;
    }
    Ok((code, client_id))
}

/// The token endpoint's answer.
#[derive(Debug, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IdClaims {
    pub iss: String,
    pub aud: Value,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub azp: Option<String>,
    pub exp: u64,
    pub sub: String,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

fn deserialize_optional_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    // Omission is allowed; a supplied scope or azp must be a string. Explicit
    // null must not silently acquire an omitted field's authorization meaning.
    String::deserialize(deserializer).map(Some)
}

/// The ID token's claims, decoded without checking anything.
pub fn decode_claims(token: &str) -> Option<IdClaims> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&b64url_decode(payload)?).ok()
}

/// Issuer, audience, authorized party, expiry and nonce (signature checked separately).
pub fn check_claims(claims: &IdClaims, client_id: &str, nonce: &str, now: u64) -> Result<(), String> {
    // OIDC Core 3.1.3.7 requires rejecting untrusted additional audiences.
    // Coucou trusts only this registration's client ID, never the API resource
    // or another saved registration, and every array element must be a string.
    let aud_ok = match &claims.aud {
        Value::String(a) => a == client_id,
        Value::Array(list) => !list.is_empty() && list.iter().all(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if claims.iss != ISSUER {
        return Err("issuer".into());
    }
    if !aud_ok {
        return Err("audience".into());
    }
    // Conservative client policy: any supplied authorized party must be us;
    // multi-entry arrays also need it, even when they repeat our audience.
    let multiple = claims.aud.as_array().is_some_and(|list| list.len() > 1);
    match claims.azp.as_deref() {
        Some(azp) if azp != client_id => return Err("authorized party".into()),
        None if multiple => return Err("authorized party".into()),
        _ => {}
    }
    if claims.exp <= now {
        return Err("expired".into());
    }
    if claims.nonce.as_deref() != Some(nonce) {
        return Err("nonce".into());
    }
    Ok(())
}

/// RS256 against the JWKS key named by the token's `kid`.
pub fn verify_signature(token: &str, jwks: &Value) -> Result<(), String> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("not a JWT".into());
    };
    let header: Value = serde_json::from_slice(&b64url_decode(h).ok_or("header")?).map_err(|_| "header")?;
    if header["alg"] != "RS256" {
        return Err("algorithm".into());
    }
    let kid = header["kid"].as_str();
    let key = jwks["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|k| k["kty"] == "RSA" && (kid.is_none() || k["kid"].as_str() == kid))
        .ok_or("no key")?;
    let n = b64url_decode(key["n"].as_str().ok_or("n")?).ok_or("n")?;
    let e = b64url_decode(key["e"].as_str().ok_or("e")?).ok_or("e")?;
    let sig = b64url_decode(s).ok_or("signature")?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, format!("{h}.{p}").as_bytes(), &sig)
        .map_err(|_| "signature".to_string())
}

fn http(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

async fn discovery() -> Result<Value, String> {
    let reply = http(Duration::from_secs(20))?.get(DISCOVERY).send().await.map_err(|_| t("ChatGPT can't be reached."))?;
    reply.json().await.map_err(|e| e.to_string())
}

/// Waits for the browser's request on `listener` and answers it.
async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<String, String> {
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            let n = socket.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("").to_string();
        if !target.starts_with(CALLBACK_PATH) {
            let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let page = format!(
            "<!doctype html><meta charset=utf-8><title>Coucou</title><body style=\"font:16px system-ui;margin:3em\"><p>{}</p>",
            t("You can close this tab and go back to Coucou.")
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = socket.write_all(reply.as_bytes()).await;
        return Ok(target);
    }
}

/// The whole sign-in: listener, browser, callback, code exchange, checks.
/// `open` opens the authorize URL in the system browser.
pub async fn connect(open: impl Fn(&str), new_account: bool, pending_account_id: Option<&str>, start_another: bool) -> Result<Status, String> {
    let _serial = try_sign_in_lock(REFRESHING.get_or_init(Default::default))?;
    let mut selected = read_account_from(&account_path())?;
    if new_account {
        selected = prepare_new_account(&selected, pending_account_id, start_another, credential_presence(), save_account)?;
    } else if start_another || pending_account_id.is_some() {
        return Err(t("ChatGPT sign-in failed."));
    } else if selected.client_id.is_none() {
        // A config reset may leave OS credentials behind. Even a seemingly
        // first registration cannot reuse those slots until they are empty.
        require_empty_credentials(credential_presence())?;
    }
    if selected.host_id.is_empty() {
        selected.host_id = new_host_id();
        save_account(&selected)?;
    }
    // An add attempt stays separate until its issued identity is verified.
    // Reconnect always keeps the selected registration and subject binding.
    let mut account = if new_account { new_account_candidate(&selected) } else { selected.clone() };
    invalidate_session();
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(l) => l,
        // Only the port may change between sign-ins.
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| e.to_string())?,
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let attempt = Attempt::new(port);
    let hint = (!new_account).then(|| account.client_id.as_ref().and_then(|_| secrets::get_internal(ID_TOKEN_KEY))).flatten();
    open(&authorize_url(&account, &attempt, hint.as_deref()));

    let target = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_callback(listener))
        .await
        .map_err(|_| t("The sign-in took too long. Try again."))??;
    let (code, client_id) = prepare_code_exchange(&mut account, &attempt, &target, |candidate| {
        if new_account { retain_pending_account(&mut selected, candidate, save_account) } else { save_account(candidate) }
    })?;

    let reply = http(Duration::from_secs(30))?
        .post(TOKEN)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", attempt.verifier.as_str()),
            ("redirect_uri", attempt.redirect_uri.as_str()),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|_| t("ChatGPT can't be reached."))?;
    if !reply.status().is_success() {
        crate::log::line(format!("chatgpt: code exchange HTTP {}", reply.status().as_u16()));
        return Err(t("ChatGPT sign-in failed."));
    }
    let tokens: Tokens = reply.json().await.map_err(|_| t("ChatGPT sign-in failed."))?;
    let id_token = tokens.id_token.clone().ok_or_else(|| t("ChatGPT sign-in failed."))?;

    let config = discovery().await?;
    let jwks_uri = config["jwks_uri"].as_str().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    let jwks: Value = http(Duration::from_secs(20))?
        .get(jwks_uri)
        .send()
        .await
        .map_err(|_| t("ChatGPT can't be reached."))?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let claims = decode_claims(&id_token).ok_or_else(|| t("ChatGPT sign-in failed."))?;
    if let Err(why) = verify_signature(&id_token, &jwks).and_then(|_| check_claims(&claims, &client_id, &attempt.nonce, now_secs())) {
        crate::log::line(format!("chatgpt: ID token refused ({why})"));
        return Err(t("ChatGPT sign-in failed."));
    }
    store_verified_sign_in(&selected, &account, &client_id, &claims, &tokens, new_account,
        save_account, secrets::set_internal, |tokens| {
            remember(&client_id, tokens);
            *REJECTED_SESSION.lock().unwrap() = None;
        })?;
    crate::log::line("chatgpt: connected".to_string());
    Ok(status())
}

/// Only called after signature, issuer, audience, expiry and nonce checks.
/// Persist the validated mapping before writing credentials. A failed file
/// write can never pair another registration's tokens with the old identity.
fn store_verified_sign_in(
    selected: &Account,
    candidate: &Account,
    client_id: &str,
    claims: &IdClaims,
    tokens: &Tokens,
    new_account: bool,
    mut persist: impl FnMut(&Account) -> Result<(), String>,
    mut store: impl FnMut(&str, &str) -> Result<(), String>,
    remember: impl FnOnce(&Tokens),
) -> Result<Account, String> {
    if candidate.sub.as_ref().is_some_and(|s| *s != claims.sub) && candidate.client_id.as_deref() == Some(client_id) {
        return Err(t("That's another ChatGPT account than the one connected before."));
    }
    let refresh = tokens.refresh_token.as_deref().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    let id_token = tokens.id_token.as_deref().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    // This is an authorization-code response for authorize_url's SCOPE.
    // RFC 6749 §5.1: omitted scope means the requested scopes were granted;
    // an explicit (even empty or reduced) scope must be honored instead.
    // https://www.rfc-editor.org/rfc/rfc6749#section-5.1
    let granted = tokens.scope.as_deref().unwrap_or(SCOPE).split_whitespace().any(|s| s == PLAN_SCOPE);
    let registration = Registration {
        client_id: client_id.to_string(), email: claims.email.clone(), sub: Some(claims.sub.clone()),
        plan_enabled: granted, welcomed: candidate.welcomed,
    };
    let mut next = if new_account { selected.select_registration(&registration) } else { candidate.select_registration(&registration) };
    if new_account { next.pending_account = None; }
    next.explicitly_disconnected = false;
    // A failed overwrite can leave an unusable previous refresh token. Do not
    // declare the session usable until both credential and metadata commits.
    next.session_needs_reconnect = true;
    persist(&next)?;
    store(REFRESH_TOKEN_KEY, refresh)?;
    // The ID token is only a hint for the next sign-in; too long for the
    // Credential Manager means no hint.
    let _ = store(ID_TOKEN_KEY, id_token);
    next.session_needs_reconnect = false;
    persist(&next)?;
    remember(tokens);
    Ok(next)
}

/// Remote revocation and local cleanup are independent outcomes. No secrets
/// or raw keystore errors are exposed to the webview.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisconnectResult {
    /// None means there was no refresh token to revoke, not remote success.
    pub revocation_confirmed: Option<bool>,
    pub local_cleanup_succeeded: bool,
}

/// Sign out: try revocation, then always attempt both local deletions and
/// clear the in-memory access token. Account/client mapping and host id stay
/// for reconnect. Only a fully persisted cleanup enables switching accounts.
pub async fn disconnect() -> DisconnectResult {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    invalidate_session();
    let loaded = read_account_from(&account_path());
    let account = loaded.clone().unwrap_or_default();
    let refresh = secrets::read_internal(REFRESH_TOKEN_KEY);
    let mut result = disconnect_with(
        refresh.as_ref().ok().and_then(|token| token.as_deref()),
        &account,
        revoke,
        |key| secrets::set_internal(key, ""),
        || *ACCESS.lock().unwrap() = None,
        |next| {
            // Even an unreadable account must not prevent token cleanup, but
            // never overwrite an account/client mapping we could not load.
            loaded.as_ref().map_err(Clone::clone)?;
            save_account(next)
        },
    ).await;
    if refresh.is_err() { result.revocation_confirmed = Some(false); }
    crate::log::line(format!(
        "chatgpt: disconnect (revocation {}, local cleanup {})",
        match result.revocation_confirmed {
            Some(true) => "confirmed",
            Some(false) => "not confirmed",
            None => "not attempted",
        },
        if result.local_cleanup_succeeded { "succeeded" } else { "failed" },
    ));
    result
}

/// Keep the cleanup orchestration testable without OAuth or a real keystore.
async fn disconnect_with<'a, F: std::future::Future<Output = bool>>(
    refresh: Option<&'a str>,
    account: &'a Account,
    revoke: impl FnOnce(&'a str, &'a str) -> F,
    mut forget: impl FnMut(&str) -> Result<(), String>,
    clear_access: impl FnOnce(),
    persist: impl FnOnce(&Account) -> Result<(), String>,
) -> DisconnectResult {
    let revocation_confirmed = match (refresh, account.client_id.as_deref()) {
        (Some(refresh), Some(client)) => Some(revoke(refresh, client).await),
        (Some(_), None) => Some(false),
        (None, _) => None,
    };
    // Evaluate each operation separately: a failure must not skip the next.
    let refresh_deleted = forget(REFRESH_TOKEN_KEY).is_ok();
    let id_deleted = forget(ID_TOKEN_KEY).is_ok();
    clear_access();
    let account_saved = if refresh_deleted && id_deleted {
        let mut next = account.clone();
        next.explicitly_disconnected = true;
        persist(&next).is_ok()
    } else { false };
    DisconnectResult {
        revocation_confirmed,
        local_cleanup_succeeded: refresh_deleted && id_deleted && account_saved,
    }
}

async fn revoke(refresh: &str, client_id: &str) -> bool {
    let Ok(config) = discovery().await else { return false };
    let Some(endpoint) = config["revocation_endpoint"].as_str() else { return false };
    let Ok(client) = http(Duration::from_secs(20)) else { return false };
    for wait in [0u64, 1, 3] {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let sent = client
            .post(endpoint)
            .form(&[("token", refresh), ("token_type_hint", "refresh_token"), ("client_id", client_id)])
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => return true,
            Ok(r) if !r.status().is_server_error() => return false,
            _ => {}
        }
    }
    false
}

// ── The access token ─────────────────────────────────────────────────────────

struct Access {
    client_id: String,
    generation: u64,
    token: String,
    until: Instant,
}

impl Access {
    fn matches_rejection(&self, request: &RequestAccount, token: &str) -> bool {
        self.client_id == request.client_id && self.generation == request.generation && self.token == token
    }

    fn fresh_for(&self, request: &RequestAccount) -> Option<String> {
        (self.client_id == request.client_id && self.generation == request.generation
            && self.until > Instant::now() + REFRESH_MARGIN).then(|| self.token.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestAccount {
    client_id: String,
    generation: u64,
}

static ACCESS: std::sync::Mutex<Option<Access>> = std::sync::Mutex::new(None);
static ACTIVE_REFRESH: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Keep a known rejection fail-closed even when both persistent stores fail.
/// Only a fully committed verified sign-in clears this process-local guard.
static REJECTED_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static SESSION_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// One refresh at a time: the refresh token rotates.
static REFRESHING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn session_generation() -> u64 {
    SESSION_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Called with REFRESHING held, before disconnect/reconnect can await work.
fn invalidate_session() {
    SESSION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *ACCESS.lock().unwrap() = None;
}

fn check_session_generation(request: &RequestAccount, current: u64) -> Result<(), String> {
    if request.generation != current {
        return Err(t("The selected ChatGPT account changed. Try again."));
    }
    Ok(())
}

fn remember(client_id: &str, tokens: &Tokens) {
    let ttl = Duration::from_secs(tokens.expires_in.unwrap_or(3600));
    *ACCESS.lock().unwrap() = Some(Access { client_id: client_id.to_string(), generation: session_generation(), token: tokens.access_token.clone(), until: Instant::now() + ttl });
}

fn fresh_access(request: &RequestAccount) -> Option<String> {
    ACCESS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|a| a.fresh_for(request))
}

fn require_selected_client(account: &Account, client_id: &str) -> Result<(), String> {
    if account.client_id.as_deref() != Some(client_id) {
        return Err(t("The selected ChatGPT account changed. Try again."));
    }
    if account.explicitly_disconnected || account.session_needs_reconnect {
        return Err(t("Your ChatGPT session ended. Reconnect in Settings."));
    }
    if !account.plan_enabled {
        return Err(t("Plan usage wasn't allowed at sign-in. Reconnect and allow it to chat on your ChatGPT plan."));
    }
    Ok(())
}

/// Refresh errors after which the token set is useless (errors-and-recovery).
fn terminal_refresh_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant" | "invalid_refresh_token" | "token_expired" | "refresh_token_expired" | "refresh_token_invalidated" | "refresh_token_reused"
    )
}

enum RefreshFailure {
    Rejected { status: u16, code: String },
    Unreachable,
    /// A successful exchange may already have rotated the refresh token.
    InvalidResponse,
}

struct ActiveRefresh<'a>(&'a std::sync::Mutex<Option<String>>);

impl<'a> ActiveRefresh<'a> {
    fn new(active: &'a std::sync::Mutex<Option<String>>, client: &str) -> Self {
        *active.lock().unwrap() = Some(client.to_string());
        Self(active)
    }
}

impl Drop for ActiveRefresh<'_> {
    fn drop(&mut self) { *self.0.lock().unwrap() = None; }
}

/// If Stop drops an in-flight refresh, its outcome (including rotation) is
/// unknown. Keep the persisted guard and invalidate the old session on drop.
struct PendingRefresh<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for PendingRefresh<F> {
    fn drop(&mut self) {
        if let Some(invalidate) = self.0.take() { invalidate(); }
    }
}

async fn exchange_refresh(refresh: &str, client: &str) -> Result<Tokens, RefreshFailure> {
    let reply = http(Duration::from_secs(30)).map_err(|_| RefreshFailure::Unreachable)?
        .post(TOKEN)
        .form(&[("grant_type", "refresh_token"), ("client_id", client), ("refresh_token", refresh), ("resource", RESOURCE)])
        .send()
        .await
        .map_err(|_| RefreshFailure::Unreachable)?;
    let status = reply.status();
    if !status.is_success() {
        let body: Value = reply.json().await.unwrap_or(Value::Null);
        let code = body["error"].as_str().or_else(|| body.pointer("/error/code").and_then(Value::as_str)).unwrap_or("").to_string();
        return Err(RefreshFailure::Rejected { status: status.as_u16(), code });
    }
    reply.json().await.map_err(|_| RefreshFailure::InvalidResponse)
}

/// The durable guard must precede the network request: after a terminal
/// response both the disk and keystore may fail, or the process may exit.
/// Inject the effects to test those paths without OAuth or real credentials.
async fn refresh_with<F: std::future::Future<Output = Result<Tokens, RefreshFailure>>>(
    active: &std::sync::Mutex<Option<String>>,
    account: &Account,
    exchange: impl FnOnce() -> F,
    mut persist: impl FnMut(&Account) -> Result<(), String>,
    mut store: impl FnMut(&str, &str) -> Result<(), String>,
    invalidate: impl FnOnce(),
    remember: impl FnOnce(&Tokens),
) -> Result<String, String> {
    let client = account.client_id.as_deref().ok_or_else(|| t("Connect your ChatGPT account first."))?;
    require_selected_client(account, client)?;
    let _active = ActiveRefresh::new(active, client);
    let mut guarded = account.clone();
    guarded.session_needs_reconnect = true;
    persist(&guarded)?;
    let mut pending = PendingRefresh(Some(invalidate));
    match exchange().await {
        Err(RefreshFailure::Rejected { code, .. }) if terminal_refresh_error(&code) => {
            let deleted = store(REFRESH_TOKEN_KEY, "").is_ok();
            crate::log::line(format!("chatgpt: session ended ({code}, credential cleanup {})", if deleted { "succeeded" } else { "failed" }));
            Err(t("Your ChatGPT session ended. Reconnect in Settings."))
        }
        Err(RefreshFailure::InvalidResponse) => {
            Err(t("ChatGPT didn't accept the sign-in. Reconnect in Settings."))
        }
        Err(error) => {
            // Transient/nonterminal errors do not end a session. If restoring
            // its metadata fails, the durable guard remains authoritative.
            persist(account)?;
            pending.0 = None;
            if let RefreshFailure::Rejected { status, .. } = error {
                crate::log::line(format!("chatgpt: refresh HTTP {status}"));
            }
            Err(t("ChatGPT can't be reached."))
        }
        Ok(tokens) => {
            // RFC 6749 §§5.1, 6: omitted refresh scope retains the prior grant;
            // explicit scope can reduce it, never authorize a new permission.
            let mut next = account.clone();
            next.plan_enabled &= tokens.scope.as_deref().is_none_or(|scope| scope.split_whitespace().any(|s| s == PLAN_SCOPE));
            if !next.plan_enabled {
                // Fence in-flight models/Responses and erase cached access as
                // soon as permission is lost, even if either store now fails.
                if let Some(invalidate) = pending.0.take() { invalidate(); }
            }
            let committed = (|| {
                if let Some(next) = &tokens.refresh_token {
                    store(REFRESH_TOKEN_KEY, next)?;
                }
                persist(&next)
            })();
            committed?;
            // Save rotation and the reduced grant for same-account recovery,
            // but never return/cache this token for the triggering request.
            require_selected_client(&next, client)?;
            pending.0 = None;
            remember(&tokens);
            Ok(tokens.access_token)
        }
    }
}

/// Called with REFRESHING held. A delayed 401 may only end the session and
/// exact access token that issued it, never a newer refresh or sign-in.
fn reject_session_with(
    request: &RequestAccount,
    current_generation: u64,
    owns_token: bool,
    account: Result<Account, String>,
    rejected: &std::sync::Mutex<Option<String>>,
    persist: impl FnOnce(&Account) -> Result<(), String>,
    store: impl FnOnce(&str, &str) -> Result<(), String>,
    invalidate: impl FnOnce(),
) -> Result<(), String> {
    check_session_generation(request, current_generation)?;
    if !owns_token || account.as_ref().is_ok_and(|a| a.client_id.as_deref() != Some(request.client_id.as_str())) {
        return Err(t("The selected ChatGPT account changed. Try again."));
    }
    // No await after ownership verification: Stop cannot lose the known
    // rejection while reading an error body or performing local recovery.
    *rejected.lock().unwrap() = Some(request.client_id.clone());
    invalidate();
    let saved = account.and_then(|mut account| {
        account.session_needs_reconnect = true;
        persist(&account)
    });
    // Independent cleanup makes a failed metadata write recoverable after
    // restart if the keystore remains writable. Retain the ID hint/client.
    let deleted = store(REFRESH_TOKEN_KEY, "").is_ok();
    crate::log::line(format!("chatgpt: refreshed token rejected (metadata {}, credential cleanup {})",
        if saved.is_ok() { "saved" } else { "failed" }, if deleted { "succeeded" } else { "failed" }));
    saved
}

async fn reject_session(request: &RequestAccount, token: &str) -> Result<(), String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let owns_token = ACCESS.lock().unwrap().as_ref().is_some_and(|access| access.matches_rejection(request, token));
    reject_session_with(request, session_generation(), owns_token, read_account_from(&account_path()),
        &REJECTED_SESSION, save_account, secrets::set_internal, invalidate_session)
}

/// Share the one-refresh-then-reconnect policy between models and Responses.
/// Authentication attempts are independent of Responses' transient backoff.
async fn recover_authentication_with<
    F: std::future::Future<Output = Result<String, String>>,
    R: std::future::Future<Output = Result<(), String>>,
>(
    status: u16,
    refreshed: &mut bool,
    refresh: impl FnOnce() -> F,
    reject: impl FnOnce() -> R,
) -> Result<Option<String>, String> {
    if status != 401 { return Ok(None); }
    if !*refreshed {
        *refreshed = true;
        return refresh().await.map(Some);
    }
    reject().await?;
    Err(t("Your ChatGPT session ended. Reconnect in Settings."))
}

/// A valid access token: the one in memory, or a refreshed one. `force`
/// refreshes even a token that looks valid (after a 401).
async fn access_token(force: bool, request: &RequestAccount) -> Result<String, String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    // A request, including a late 401 retry, stays with its starting account.
    let mut account = read_account_from(&account_path())?;
    check_session_generation(request, session_generation())?;
    apply_session_rejection(&mut account, REJECTED_SESSION.lock().unwrap().as_deref());
    require_selected_client(&account, &request.client_id)?;
    // Another call may have refreshed while this one waited.
    if !force {
        if let Some(token) = fresh_access(request) {
            return Ok(token);
        }
    }
    let (Some(refresh), Some(client)) = (secrets::get_internal(REFRESH_TOKEN_KEY), account.client_id.clone()) else {
        return Err(t("Connect your ChatGPT account first."));
    };
    refresh_with(&ACTIVE_REFRESH, &account, || exchange_refresh(&refresh, &client), save_account,
        secrets::set_internal, invalidate_session, |tokens| remember(&client, tokens)).await
}

// ── Models ───────────────────────────────────────────────────────────────────

pub async fn models() -> Result<Vec<CatalogModel>, String> {
    let account = plan_ready().await?;
    let mut token = access_token(false, &account).await?;
    let mut refreshed = false;
    for _ in 0..2 {
        check_session_generation(&account, session_generation())?;
        let reply = http(Duration::from_secs(30))?
            .get(MODELS)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| t("ChatGPT can't be reached."))?;
        check_session_generation(&account, session_generation())?;
        if let Some(next) = recover_authentication_with(reply.status().as_u16(), &mut refreshed,
            || access_token(true, &account), || reject_session(&account, &token),
        ).await? {
            token = next;
            continue;
        }
        if !reply.status().is_success() {
            let status = reply.status().as_u16();
            let body: Value = reply.json().await.unwrap_or(Value::Null);
            let _serial = REFRESHING.get_or_init(Default::default).lock().await;
            check_session_generation(&account, session_generation())?;
            return Err(error_message(status, &body).text);
        }
        let body: Value = reply.json().await.map_err(|e| e.to_string())?;
        let _serial = REFRESHING.get_or_init(Default::default).lock().await;
        check_session_generation(&account, session_generation())?;
        return Ok(catalog(&body));
    }
    Err(t("ChatGPT didn't accept the sign-in. Reconnect in Settings."))
}

/// The models as the generic picker lists them.
pub fn as_model_infos(models: &[CatalogModel]) -> Vec<ModelInfo> {
    models.iter().filter(|m| m.available).map(|m| ModelInfo { id: m.id.clone(), label: m.label.clone() }).collect()
}

// ── Speech (audio mode's voice) ──────────────────────────────────────────────
//
// Sign in with ChatGPT documents the Responses API and the model list; it
// rules out audio input and transcription, and says nothing of speech out.
// Audio mode asks for OpenAI's voice once in a while: when the account may
// use it, replies are read by it, on the plan like everything else here;
// when it may not, the first refusal is remembered for that session and Windows'
// own voices read instead. Network failures briefly defer remote speech too, so
// every sentence doesn't wait for an unreachable endpoint. Never another billing.

const SPEECH: &str = "https://api.openai.com/v1/audio/speech";
/// OpenAI's speech model and voice.
pub const SPEECH_MODEL: &str = "gpt-4o-mini-tts";
pub const SPEECH_VOICE: &str = "marin";
const SPEECH_STYLE: &str = "Speak naturally and warmly, like a friendly assistant in a relaxed conversation. Match the language of the text.";

/// Speech is optional: prefer the native voice over a long network stall.
const SPEECH_TIMEOUT: Duration = Duration::from_secs(5);
const SPEECH_RETRY_DELAY: Duration = Duration::from_secs(60);

/// A failure belongs to one account session. Transport failures expire;
/// endpoint refusals last until reconnect. Neither follows an account switch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SpeechFailure {
    account: RequestAccount,
    retry_after: Option<Instant>,
}

static SPEECH_REFUSED: std::sync::Mutex<Option<SpeechFailure>> = std::sync::Mutex::new(None);

fn check_speech_session(
    request: &RequestAccount,
    current_generation: u64,
    refused: Option<&SpeechFailure>,
    now: Instant,
) -> Result<(), String> {
    check_session_generation(request, current_generation)?;
    if refused.is_some_and(|failure| failure.account == *request
        && failure.retry_after.is_none_or(|until| now < until)) {
        return Err("speech temporarily unavailable or refused earlier".into());
    }
    Ok(())
}

/// Both sending the request and reading its body can fail. Keep the same
/// cooldown for either failure, without replacing a concurrent HTTP refusal.
/// Called with REFRESHING held, so a late failure cannot affect a new session.
fn speech_transport_result<T, E>(
    request: &RequestAccount,
    current_generation: u64,
    result: Result<T, E>,
    refused: &mut Option<SpeechFailure>,
    now: Instant,
) -> Result<T, String> {
    check_session_generation(request, current_generation)?;
    result.map_err(|_| {
        if !refused.as_ref().is_some_and(|failure| failure.account == *request && failure.retry_after.is_none()) {
            *refused = Some(SpeechFailure { account: request.clone(), retry_after: Some(now + SPEECH_RETRY_DELAY) });
        }
        t("ChatGPT can't be reached.")
    })
}

async fn speech_transport<T, E>(request: &RequestAccount, result: Result<T, E>) -> Result<T, String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    speech_transport_result(request, session_generation(), result, &mut SPEECH_REFUSED.lock().unwrap(), Instant::now())
}

pub fn speech_body(text: &str) -> Value {
    json!({
        "model": SPEECH_MODEL,
        "voice": SPEECH_VOICE,
        "input": text,
        "instructions": SPEECH_STYLE,
        "response_format": "mp3",
    })
}

/// True when OpenAI's voice may be tried: signed in with plan usage and not
/// refused earlier in this account session or cooling down after a failure.
pub async fn speech_available() -> bool {
    let Ok(account) = plan_ready().await else { return false };
    check_speech_session(&account, session_generation(), SPEECH_REFUSED.lock().unwrap().as_ref(), Instant::now()).is_ok()
}

/// `text` read by OpenAI's voice, as MP3 bytes. Err when it can't be
/// (refused, offline, signed out): the caller reads it another way.
pub async fn speak(text: &str, authorize: impl Fn() -> Result<(), String> + Send + Sync) -> Result<Vec<u8>, String> {
    authorize()?;
    let account = plan_ready().await?;
    authorize()?;
    check_speech_session(&account, session_generation(), SPEECH_REFUSED.lock().unwrap().as_ref(), Instant::now())?;
    let body = speech_body(text);
    let mut token = access_token(false, &account).await?;
    for attempt in 0..2 {
        // A parallel chunk may have failed while this request got its token.
        check_speech_session(&account, session_generation(), SPEECH_REFUSED.lock().unwrap().as_ref(), Instant::now())?;
        let request = http(SPEECH_TIMEOUT)?
            .post(SPEECH)
            .bearer_auth(&token)
            .json(&body);
        // Token refresh and a 401 retry can outlive the answer. Recheck its
        // native speech scope immediately before transmitting its text.
        authorize()?;
        let sent = request.send().await;
        authorize()?;
        let reply = speech_transport(&account, sent).await?;
        let status = reply.status().as_u16();
        // Speech is optional on this sign-in. An endpoint-specific refusal
        // falls back to Windows; it must not expire an otherwise valid chat.
        if status == 401 && attempt == 0 {
            token = access_token(true, &account).await?;
            continue;
        }
        if !reply.status().is_success() {
            // Even an error response can stall or lose its body. Remember
            // that transport failure before interpreting the HTTP status.
            let body: Value = speech_transport(&account, reply.json().await).await.unwrap_or(Value::Null);
            let _serial = REFRESHING.get_or_init(Default::default).lock().await;
            check_session_generation(&account, session_generation())?;
            let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
            // A refusal of this kind of request, not a passing failure.
            if matches!(status, 400 | 401 | 403 | 404) {
                *SPEECH_REFUSED.lock().unwrap() = Some(SpeechFailure { account: account.clone(), retry_after: None });
                crate::log::line(format!("chatgpt: speech not available on this sign-in (HTTP {status} {code})"));
            } else {
                crate::log::line(format!("chatgpt: speech HTTP {status} {code}"));
            }
            return Err(error_message(status, &body).text);
        }
        let bytes = speech_transport(&account, reply.bytes().await).await?;
        let _serial = REFRESHING.get_or_init(Default::default).lock().await;
        authorize()?;
        check_session_generation(&account, session_generation())?;
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err("empty or oversized audio".into());
        }
        return Ok(bytes.to_vec());
    }
    Err(t("ChatGPT didn't accept the sign-in. Reconnect in Settings."))
}

async fn plan_ready() -> Result<RequestAccount, String> {
    // Capture identity and epoch together; even same-account reconnects must
    // not adopt a request that started in the previous session.
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let s = status();
    if !s.connected {
        return Err(t(if s.needs_reconnect { "Your ChatGPT session ended. Reconnect in Settings." } else { "Connect your ChatGPT account first." }));
    }
    if !s.plan_enabled {
        return Err(t("Plan usage wasn't allowed at sign-in. Reconnect and allow it to chat on your ChatGPT plan."));
    }
    Ok(RequestAccount {
        client_id: s.account_id.ok_or_else(|| t("Connect your ChatGPT account first."))?,
        generation: session_generation(),
    })
}

// ── Inference ────────────────────────────────────────────────────────────────

/// The user's native content, retained in full for later turns. The first
/// turn carries the window line, a dropped text file's contents, or a dropped
/// picture (`input_image`); a PDF goes by name only.
pub fn user_content(first: bool, context: Option<&ChatContext>, query: &str) -> Value {
    let mut prefix = String::new();
    let mut image = None;
    match context.filter(|_| first) {
        Some(ChatContext::File { name, path }) => match file_part(path) {
            Some(FilePart::Image(part)) => {
                image = part.pointer("/image_url/url").and_then(Value::as_str).map(str::to_string);
                prefix = format!("File: {name}

");
            }
            Some(FilePart::Text(body)) => prefix = format!("File: {name}
File contents:
{body}

"),
            None => prefix = format!("File: {name}

"),
        },
        Some(ChatContext::Window { app_name, title, url }) => {
            prefix = format!("{}

", chat::window_line(app_name, title, url.as_deref()));
        }
        None => {}
    }
    let text = format!("{prefix}{query}");
    match image {
        None => json!(text),
        Some(url) => json!([{ "type": "input_image", "image_url": url }, { "type": "input_text", "text": text }]),
    }
}

/// Only the input parts this provider creates may be replayed. Rebuild their
/// supported fields rather than forwarding arbitrary provider-native blocks.
fn history_content(role: &str, content: &Value) -> Option<Value> {
    if content.is_string() {
        return Some(content.clone());
    }
    let parts = content.as_array()?;
    if role != "user" || parts.is_empty() {
        return None;
    }
    let parts: Option<Vec<Value>> = parts.iter().map(|part| {
        match part["type"].as_str()? {
            "input_text" => Some(json!({ "type": "input_text", "text": part["text"].as_str()? })),
            "input_image" => Some(json!({ "type": "input_image", "image_url": part["image_url"].as_str()? })),
            _ => None,
        }
    }).collect();
    parts.map(Value::Array)
}

/// The request body. History retains native text/image input because this
/// route uses `store: false`. System-level text goes in `instructions` (a
/// `system` message is refused on this route).
pub fn request_body(model: &str, effort: &str, instructions: &str, history: &[Value], user: Value) -> Value {
    let mut input: Vec<Value> = history
        .iter()
        .filter_map(|m| {
            let role = m["role"].as_str()?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let content = history_content(role, &m["content"])?;
            Some(json!({ "role": role, "content": content }))
        })
        .collect();
    input.push(json!({ "role": "user", "content": user }));
    json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "reasoning": { "effort": effort },
        "stream": true,
        "store": false,
    })
}

/// One server-sent event of the Responses stream.
#[derive(Debug, PartialEq)]
pub enum StreamEvent {
    Delta(String),
    /// A finished output item (a message, reasoning, a function call).
    Item(Value),
    Completed,
    Failed { status: u16, code: String, message: String },
    Incomplete(String),
    Other,
}

pub fn parse_event(data: &str) -> StreamEvent {
    let Ok(v) = serde_json::from_str::<Value>(data) else { return StreamEvent::Other };
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    match v["type"].as_str().unwrap_or("") {
        "response.output_text.delta" => StreamEvent::Delta(s("/delta")),
        "response.output_item.done" => match v.get("item") {
            Some(item) if item.is_object() => StreamEvent::Item(item.clone()),
            _ => StreamEvent::Other,
        },
        "response.completed" => StreamEvent::Completed,
        "response.failed" => StreamEvent::Failed { status: 0, code: s("/response/error/code"), message: s("/response/error/message") },
        "response.incomplete" => StreamEvent::Incomplete(s("/response/incomplete_details/reason")),
        "error" => StreamEvent::Failed {
            status: 0,
            code: if s("/code").is_empty() { s("/error/code") } else { s("/code") },
            message: if s("/message").is_empty() { s("/error/message") } else { s("/message") },
        },
        _ => StreamEvent::Other,
    }
}

/// A failure, as the user sees it, and what Coucou does about it.
#[derive(Debug, PartialEq)]
pub struct Failure {
    pub text: String,
    /// "Usage limit reached": the island shows "Manage usage".
    pub usage_limit: bool,
    /// Worth one more try after a pause (a 503 before the stream opened).
    pub transient: bool,
}

/// errors-and-recovery's table, then the plain HTTP statuses.
pub fn error_message(status: u16, body: &Value) -> Failure {
    let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
    let param = body.pointer("/error/param").and_then(Value::as_str).unwrap_or("");
    failure(status, code, param)
}

fn failure(status: u16, code: &str, param: &str) -> Failure {
    let plain = |text: String| Failure { text, usage_limit: false, transient: false };
    match code {
        "subscription_sharing_usage_limit_exceeded" => Failure {
            text: t("Usage limit reached. Review your plan or this app's limit in ChatGPT settings."),
            usage_limit: true,
            transient: false,
        },
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => Failure {
            text: t("ChatGPT plan usage is unavailable right now. Try again in a moment."),
            usage_limit: false,
            transient: true,
        },
        "subscription_sharing_user_not_eligible" => plain(t("This ChatGPT account can't use its plan in Coucou.")),
        "subscription_sharing_unsupported_capability" => {
            plain(tf("Not available on your ChatGPT plan: {what}.", &[("what", if param.is_empty() { "this request" } else { param })]))
        }
        "subscription_sharing_invalid_user" => plain(t("ChatGPT didn't accept the sign-in. Reconnect in Settings.")),
        "subscription_sharing_route_not_supported" | "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => {
            plain(t("ChatGPT refused this app's request. Check the app's configuration and permissions."))
        }
        "model_not_found" => plain(t("This model isn't available on your ChatGPT account.")),
        _ => match status {
            401 => plain(t("ChatGPT didn't accept the sign-in. Reconnect in Settings.")),
            403 => plain(t("ChatGPT refused this request (account or region policy).")),
            429 => Failure {
                text: t("Usage limit reached. Review your plan or this app's limit in ChatGPT settings."),
                usage_limit: true,
                transient: false,
            },
            503 => Failure { text: t("ChatGPT plan usage is unavailable right now. Try again in a moment."), usage_limit: false, transient: true },
            _ if status >= 400 => plain(tf("ChatGPT error {status}.", &[("status", &status.to_string())])),
            _ => plain(t("ChatGPT stopped before finishing the answer.")),
        },
    }
}

/// Record the exact native user content only after the stream has completed.
/// The plain record remains separate for switching providers.
fn finish_turn(chat: &Chat, turn: &chat::Turn, user: Value, plain: &str, answer: String) -> ChatReply {
    chat.commit(turn, json!({ "role": "user", "content": user }), json!({ "role": "assistant", "content": answer }), plain, &answer);
    ChatReply { text: answer }
}

/// One turn on the ChatGPT plan, streamed: `on_delta` gets the text so far.
/// `on_usage_limit` is told when the plan's limit is reached.
/// How many times one question may go back and forth through tools.
pub const MAX_TOOL_ROUNDS: usize = 4;

/// Runs one tool call (assistant/tools.rs) and returns what the model is told.
pub type ToolRunner<'a> =
    dyn Fn(ToolCall) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> + Send + Sync + 'a;

/// The round's request: the question's body, plus the tools and what the
/// earlier rounds produced (their output items and the tools' results).
/// The last round may not call tools again.
pub fn with_tools(base: &Value, extra: &[Value], last_round: bool) -> Value {
    let mut body = base.clone();
    body["tools"] = crate::assistant::tools::definitions();
    // Stateless (store: false): reasoning travels back encrypted.
    body["include"] = json!(["reasoning.encrypted_content"]);
    if last_round {
        body["tool_choice"] = json!("none");
    }
    if let Some(input) = body["input"].as_array_mut() {
        input.extend(extra.iter().cloned());
    }
    body
}

/// A function call among a round's output items.
pub fn tool_call(item: &Value) -> Option<ToolCall> {
    if item["type"] != "function_call" {
        return None;
    }
    let name = item["name"].as_str()?;
    let name = match item["namespace"].as_str() {
        Some(ns) if !name.contains('.') => format!("{ns}.{name}"),
        _ => name.to_string(),
    };
    Some(ToolCall {
        call_id: item["call_id"].as_str()?.to_string(),
        name: crate::assistant::tools::bare_name(&name).to_string(),
        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
    })
}

pub async fn send(
    chat: &Chat,
    model: &str,
    effort: &str,
    query: String,
    context: Option<ChatContext>,
    mut on_delta: impl FnMut(String),
    on_usage_limit: impl Fn(),
    tools: Option<&ToolRunner<'_>>,
) -> Result<ChatReply, String> {
    let account = plan_ready().await?;
    if !effort_ok(model, effort) {
        return Err(tf("{model} doesn't take this reasoning effort.", &[("model", model)]));
    }
    let turn = chat.begin(PROVIDER);
    let user = user_content(turn.first, context.as_ref(), &query);
    let instructions = match tools {
        Some(_) => format!("{}\n\n{}", chat::system_prompt(false), crate::assistant::tools::INSTRUCTIONS),
        None => chat::system_prompt(false),
    };
    let base = request_body(model, effort, &instructions, &turn.history, user.clone());

    let mut token = access_token(false, &account).await?;
    let mut answer = String::new();
    let mut extra: Vec<Value> = Vec::new();
    for round in 0..=MAX_TOOL_ROUNDS {
        check_session_generation(&account, session_generation())?;
        let body = match tools {
            Some(_) => with_tools(&base, &extra, round == MAX_TOOL_ROUNDS),
            None => base.clone(),
        };
        let reply = open_stream(&body, &mut token, &account, &on_usage_limit).await?;
        let prefix = if answer.is_empty() { String::new() } else { format!("{answer}\n\n") };
        let (text, items) = read_stream(reply, &prefix, &account, &mut on_delta, &on_usage_limit).await?;
        if !text.is_empty() {
            answer = format!("{prefix}{text}");
        }
        let calls: Vec<ToolCall> = items.iter().filter_map(tool_call).collect();
        let Some(run) = tools.filter(|_| !calls.is_empty()) else { break };
        extra.extend(items);
        for call in calls {
            check_session_generation(&account, session_generation())?;
            crate::log::line(format!("chatgpt: tool {}", call.name));
            let output = run(call.clone()).await;
            check_session_generation(&account, session_generation())?;
            extra.push(json!({ "type": "function_call_output", "call_id": call.call_id, "output": output }));
        }
    }
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    check_session_generation(&account, session_generation())?;
    on_delta(answer.clone());
    let plain = chat::plain_question(turn.first, context.as_ref(), &query);
    Ok(finish_turn(chat, &turn, user, &plain, answer))
}

/// The request, retried after a refreshed token (401) or a passing failure.
async fn open_stream(body: &Value, token: &mut String, account: &RequestAccount, on_usage_limit: &impl Fn()) -> Result<reqwest::Response, String> {
    let mut tries = 0;
    let mut refreshed = false;
    loop {
        check_session_generation(account, session_generation())?;
        tries += 1;
        let reply = http(Duration::from_secs(600))?
            .post(RESPONSES)
            .bearer_auth(&*token)
            .json(body)
            .send()
            .await
            .map_err(|_| t("ChatGPT can't be reached."))?;
        check_session_generation(account, session_generation())?;
        let status = reply.status().as_u16();
        if reply.status().is_success() {
            return Ok(reply);
        }
        if let Some(next) = recover_authentication_with(status, &mut refreshed,
            || access_token(true, account), || reject_session(account, token),
        ).await? {
            *token = next;
            continue;
        }
        let body: Value = reply.json().await.unwrap_or(Value::Null);
        check_session_generation(account, session_generation())?;
        let failure = error_message(status, &body);
        // Bounded backoff, only before anything streamed.
        if failure.transient && tries <= 2 {
            tokio::time::sleep(Duration::from_secs(tries * 2)).await;
            continue;
        }
        let _serial = REFRESHING.get_or_init(Default::default).lock().await;
        check_session_generation(account, session_generation())?;
        crate::log::line(format!("chatgpt: HTTP {status} {}", body.pointer("/error/code").and_then(Value::as_str).unwrap_or("")));
        if failure.usage_limit {
            on_usage_limit();
        }
        return Err(failure.text);
    }
}

/// One round's stream: its text (shown after `prefix`) and its output items.
async fn read_stream(
    mut reply: reqwest::Response,
    prefix: &str,
    account: &RequestAccount,
    on_delta: &mut impl FnMut(String),
    on_usage_limit: &impl Fn(),
) -> Result<(String, Vec<Value>), String> {
    let mut answer = String::new();
    let mut items = Vec::new();
    let mut pending = Vec::<u8>::new();
    let mut last = Instant::now();
    let mut done = false;
    'read: while let Some(chunk) = reply.chunk().await.map_err(|_| t("ChatGPT stopped before finishing the answer."))? {
        let _serial = REFRESHING.get_or_init(Default::default).lock().await;
        check_session_generation(account, session_generation())?;
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim_end().strip_prefix("data:") else { continue };
            match parse_event(data.trim()) {
                StreamEvent::Delta(text) => answer.push_str(&text),
                StreamEvent::Item(item) => {
                    if items.len() < 64 {
                        items.push(item);
                    }
                }
                StreamEvent::Completed => {
                    done = true;
                    break 'read;
                }
                StreamEvent::Failed { code, .. } => {
                    crate::log::line(format!("chatgpt: stream failed {code}"));
                    let failure = failure(0, &code, "");
                    if failure.usage_limit {
                        on_usage_limit();
                    }
                    return Err(failure.text);
                }
                StreamEvent::Incomplete(reason) => {
                    crate::log::line(format!("chatgpt: incomplete ({reason})"));
                    return Err(t("ChatGPT stopped before finishing the answer."));
                }
                StreamEvent::Other => {}
            }
        }
        if answer.len() > 4 * 1024 * 1024 {
            return Err(t("The server's answer is too large."));
        }
        if last.elapsed() >= Duration::from_millis(66) && !answer.is_empty() {
            last = Instant::now();
            on_delta(format!("{prefix}{answer}"));
        }
    }
    // `response.completed` is the only sign it succeeded.
    if !done {
        return Err(t("ChatGPT stopped before finishing the answer."));
    }
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    check_session_generation(account, session_generation())?;
    Ok((answer, items))
}

#[cfg(test)]
mod tests;

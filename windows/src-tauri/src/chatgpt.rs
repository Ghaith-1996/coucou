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

use std::path::PathBuf;
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
    /// The sign-in granted `chatgpt.tokens.use.direct`.
    pub plan_enabled: bool,
    /// "You're using your ChatGPT plan" was shown once.
    pub welcomed: bool,
}

fn account_path() -> PathBuf {
    crate::settings::config_dir().join("chatgpt.json")
}

pub fn load_account() -> Account {
    std::fs::read(account_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_account(account: &Account) -> Result<(), String> {
    let path = account_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_vec_pretty(account).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// What the island and Settings may know: never a token.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    /// Either saved credential remains, including an orphaned sign-in hint.
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
    let account = load_account();
    status_for(
        &account,
        secrets::get_internal(REFRESH_TOKEN_KEY).is_some(),
        secrets::get_internal(ID_TOKEN_KEY).is_some(),
    )
}

fn status_for(account: &Account, connected: bool, has_id_token: bool) -> Status {
    Status {
        connected,
        has_saved_credentials: connected || has_id_token,
        email: account.email.clone(),
        plan_enabled: account.plan_enabled,
        needs_reconnect: !connected && account.client_id.is_some() && account.email.is_some(),
        show_welcome: connected && account.plan_enabled && !account.welcomed,
        manage_usage_url: MANAGE_USAGE_URL,
    }
}

pub fn mark_welcomed() -> Result<(), String> {
    let mut account = load_account();
    account.welcomed = true;
    save_account(&account)
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
    scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IdClaims {
    pub iss: String,
    pub aud: Value,
    pub exp: u64,
    pub sub: String,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// The ID token's claims, decoded without checking anything.
pub fn decode_claims(token: &str) -> Option<IdClaims> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&b64url_decode(payload)?).ok()
}

/// Issuer, audience, expiry and nonce (the signature is `verify_signature`).
pub fn check_claims(claims: &IdClaims, client_id: &str, nonce: &str, now: u64) -> Result<(), String> {
    let aud_ok = match &claims.aud {
        Value::String(a) => a == client_id,
        Value::Array(list) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if claims.iss != ISSUER {
        return Err("issuer".into());
    }
    if !aud_ok {
        return Err("audience".into());
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
pub async fn connect(open: impl Fn(&str)) -> Result<Status, String> {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let mut account = load_account();
    if account.host_id.is_empty() {
        account.host_id = new_host_id();
        save_account(&account)?;
    }
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(l) => l,
        // Only the port may change between sign-ins.
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| e.to_string())?,
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let attempt = Attempt::new(port);
    let hint = account.client_id.as_ref().and_then(|_| secrets::get_internal(ID_TOKEN_KEY));
    open(&authorize_url(&account, &attempt, hint.as_deref()));

    let target = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_callback(listener))
        .await
        .map_err(|_| t("The sign-in took too long. Try again."))??;
    let (code, client_id) = prepare_code_exchange(&mut account, &attempt, &target, save_account)?;

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
    // A returning account must be the same person.
    if account.sub.as_ref().is_some_and(|s| *s != claims.sub) && account.client_id.as_deref() == Some(client_id.as_str()) {
        return Err(t("That's another ChatGPT account than the one connected before."));
    }
    let refresh = tokens.refresh_token.clone().ok_or_else(|| t("ChatGPT sign-in failed."))?;
    let granted = tokens.scope.as_deref().unwrap_or("").split_whitespace().any(|s| s == PLAN_SCOPE);

    secrets::set_internal(REFRESH_TOKEN_KEY, &refresh)?;
    // The ID token is only a hint for the next sign-in; too long for the
    // Credential Manager means no hint.
    let _ = secrets::set_internal(ID_TOKEN_KEY, &id_token);
    account.client_id = Some(client_id);
    account.email = claims.email.clone();
    account.sub = Some(claims.sub.clone());
    account.plan_enabled = granted;
    save_account(&account)?;
    remember(&tokens);
    crate::log::line(format!("chatgpt: connected (plan usage {})", if granted { "enabled" } else { "not granted" }));
    Ok(status())
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
/// clear the in-memory access token. Client and host ids stay for next time.
pub async fn disconnect() -> DisconnectResult {
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    let account = load_account();
    let refresh = secrets::get_internal(REFRESH_TOKEN_KEY);
    let result = disconnect_with(
        refresh.as_deref(),
        account.client_id.as_deref(),
        revoke,
        |key| secrets::set_internal(key, ""),
        || *ACCESS.lock().unwrap() = None,
    ).await;
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
    client: Option<&'a str>,
    revoke: impl FnOnce(&'a str, &'a str) -> F,
    mut forget: impl FnMut(&str) -> Result<(), String>,
    clear_access: impl FnOnce(),
) -> DisconnectResult {
    let revocation_confirmed = match (refresh, client) {
        (Some(refresh), Some(client)) => Some(revoke(refresh, client).await),
        (Some(_), None) => Some(false),
        (None, _) => None,
    };
    // Evaluate each operation separately: a failure must not skip the next.
    let refresh_deleted = forget(REFRESH_TOKEN_KEY).is_ok();
    let id_deleted = forget(ID_TOKEN_KEY).is_ok();
    clear_access();
    DisconnectResult {
        revocation_confirmed,
        local_cleanup_succeeded: refresh_deleted && id_deleted,
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
    token: String,
    until: Instant,
}

static ACCESS: std::sync::Mutex<Option<Access>> = std::sync::Mutex::new(None);
/// One refresh at a time: the refresh token rotates.
static REFRESHING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn remember(tokens: &Tokens) {
    let ttl = Duration::from_secs(tokens.expires_in.unwrap_or(3600));
    *ACCESS.lock().unwrap() = Some(Access { token: tokens.access_token.clone(), until: Instant::now() + ttl });
}

fn fresh_access() -> Option<String> {
    ACCESS
        .lock()
        .unwrap()
        .as_ref()
        .filter(|a| a.until > Instant::now() + REFRESH_MARGIN)
        .map(|a| a.token.clone())
}

/// Refresh errors after which the token set is useless (errors-and-recovery).
fn terminal_refresh_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant" | "invalid_refresh_token" | "token_expired" | "refresh_token_expired" | "refresh_token_invalidated" | "refresh_token_reused"
    )
}

/// A valid access token: the one in memory, or a refreshed one. `force`
/// refreshes even a token that looks valid (after a 401).
async fn access_token(force: bool) -> Result<String, String> {
    if !force {
        if let Some(token) = fresh_access() {
            return Ok(token);
        }
    }
    let _serial = REFRESHING.get_or_init(Default::default).lock().await;
    // Another call may have refreshed while this one waited.
    if !force {
        if let Some(token) = fresh_access() {
            return Ok(token);
        }
    }
    let account = load_account();
    let (Some(refresh), Some(client)) = (secrets::get_internal(REFRESH_TOKEN_KEY), account.client_id.clone()) else {
        return Err(t("Connect your ChatGPT account first."));
    };
    let reply = http(Duration::from_secs(30))?
        .post(TOKEN)
        .form(&[("grant_type", "refresh_token"), ("client_id", client.as_str()), ("refresh_token", refresh.as_str()), ("resource", RESOURCE)])
        .send()
        .await
        .map_err(|_| t("ChatGPT can't be reached."))?;
    let status = reply.status();
    if !status.is_success() {
        let body: Value = reply.json().await.unwrap_or(Value::Null);
        let code = body["error"].as_str().or_else(|| body.pointer("/error/code").and_then(Value::as_str)).unwrap_or("");
        if terminal_refresh_error(code) {
            // The session is over: forget it, keep the account for "Reconnect".
            let _ = secrets::set_internal(REFRESH_TOKEN_KEY, "");
            *ACCESS.lock().unwrap() = None;
            crate::log::line(format!("chatgpt: session ended ({code})"));
            return Err(t("Your ChatGPT session ended. Reconnect in Settings."));
        }
        crate::log::line(format!("chatgpt: refresh HTTP {}", status.as_u16()));
        return Err(t("ChatGPT can't be reached."));
    }
    let tokens: Tokens = reply.json().await.map_err(|e| e.to_string())?;
    if let Some(next) = &tokens.refresh_token {
        secrets::set_internal(REFRESH_TOKEN_KEY, next)?;
    }
    remember(&tokens);
    Ok(tokens.access_token)
}

// ── Models ───────────────────────────────────────────────────────────────────

pub async fn models() -> Result<Vec<CatalogModel>, String> {
    plan_ready()?;
    let mut token = access_token(false).await?;
    for attempt in 0..2 {
        let reply = http(Duration::from_secs(30))?
            .get(MODELS)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| t("ChatGPT can't be reached."))?;
        if reply.status().as_u16() == 401 && attempt == 0 {
            token = access_token(true).await?;
            continue;
        }
        if !reply.status().is_success() {
            let status = reply.status().as_u16();
            let body: Value = reply.json().await.unwrap_or(Value::Null);
            return Err(error_message(status, &body).text);
        }
        let body: Value = reply.json().await.map_err(|e| e.to_string())?;
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
// when it may not, the first refusal is remembered for this run and Windows'
// own voices read instead. Never an API key, never another billing.

const SPEECH: &str = "https://api.openai.com/v1/audio/speech";
/// OpenAI's speech model and voice.
pub const SPEECH_MODEL: &str = "gpt-4o-mini-tts";
pub const SPEECH_VOICE: &str = "marin";
const SPEECH_STYLE: &str = "Speak naturally and warmly, like a friendly assistant in a relaxed conversation. Match the language of the text.";

/// Set once the account was refused speech: not asked again until restart.
static SPEECH_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
/// refused earlier in this run.
pub fn speech_available() -> bool {
    !SPEECH_REFUSED.load(std::sync::atomic::Ordering::SeqCst) && plan_ready().is_ok()
}

/// `text` read by OpenAI's voice, as MP3 bytes. Err when it can't be
/// (refused, offline, signed out): the caller reads it another way.
pub async fn speak(text: &str) -> Result<Vec<u8>, String> {
    plan_ready()?;
    if SPEECH_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("refused earlier".into());
    }
    let body = speech_body(text);
    let mut token = access_token(false).await?;
    for attempt in 0..2 {
        let reply = http(Duration::from_secs(30))?
            .post(SPEECH)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|_| t("ChatGPT can't be reached."))?;
        let status = reply.status().as_u16();
        if status == 401 && attempt == 0 {
            token = access_token(true).await?;
            continue;
        }
        if !reply.status().is_success() {
            let body: Value = reply.json().await.unwrap_or(Value::Null);
            let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
            // A refusal of this kind of request, not a passing failure.
            if matches!(status, 400 | 401 | 403 | 404) {
                SPEECH_REFUSED.store(true, std::sync::atomic::Ordering::SeqCst);
                crate::log::line(format!("chatgpt: speech not available on this sign-in (HTTP {status} {code})"));
            } else {
                crate::log::line(format!("chatgpt: speech HTTP {status} {code}"));
            }
            return Err(error_message(status, &body).text);
        }
        let bytes = reply.bytes().await.map_err(|_| t("ChatGPT can't be reached."))?;
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err("empty or oversized audio".into());
        }
        return Ok(bytes.to_vec());
    }
    Err(t("ChatGPT didn't accept the sign-in. Reconnect in Settings."))
}

fn plan_ready() -> Result<(), String> {
    let s = status();
    if !s.connected {
        return Err(t("Connect your ChatGPT account first."));
    }
    if !s.plan_enabled {
        return Err(t("Plan usage wasn't allowed at sign-in. Reconnect and allow it to chat on your ChatGPT plan."));
    }
    Ok(())
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
            plain(t("ChatGPT refused this app's request. Reconnect in Settings."))
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
    plan_ready()?;
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

    let mut token = access_token(false).await?;
    let mut answer = String::new();
    let mut extra: Vec<Value> = Vec::new();
    for round in 0..=MAX_TOOL_ROUNDS {
        let body = match tools {
            Some(_) => with_tools(&base, &extra, round == MAX_TOOL_ROUNDS),
            None => base.clone(),
        };
        let reply = open_stream(&body, &mut token, &on_usage_limit).await?;
        let prefix = if answer.is_empty() { String::new() } else { format!("{answer}\n\n") };
        let (text, items) = read_stream(reply, &prefix, &mut on_delta, &on_usage_limit).await?;
        if !text.is_empty() {
            answer = format!("{prefix}{text}");
        }
        let calls: Vec<ToolCall> = items.iter().filter_map(tool_call).collect();
        let Some(run) = tools.filter(|_| !calls.is_empty()) else { break };
        extra.extend(items);
        for call in calls {
            crate::log::line(format!("chatgpt: tool {}", call.name));
            let output = run(call.clone()).await;
            extra.push(json!({ "type": "function_call_output", "call_id": call.call_id, "output": output }));
        }
    }
    on_delta(answer.clone());
    let plain = chat::plain_question(turn.first, context.as_ref(), &query);
    Ok(finish_turn(chat, &turn, user, &plain, answer))
}

/// The request, retried after a refreshed token (401) or a passing failure.
async fn open_stream(body: &Value, token: &mut String, on_usage_limit: &impl Fn()) -> Result<reqwest::Response, String> {
    let mut tries = 0;
    loop {
        tries += 1;
        let reply = http(Duration::from_secs(600))?
            .post(RESPONSES)
            .bearer_auth(&*token)
            .json(body)
            .send()
            .await
            .map_err(|_| t("ChatGPT can't be reached."))?;
        let status = reply.status().as_u16();
        if reply.status().is_success() {
            return Ok(reply);
        }
        let body: Value = reply.json().await.unwrap_or(Value::Null);
        let failure = error_message(status, &body);
        if status == 401 && tries == 1 {
            *token = access_token(true).await?;
            continue;
        }
        // Bounded backoff, only before anything streamed.
        if failure.transient && tries <= 2 {
            tokio::time::sleep(Duration::from_secs(tries * 2)).await;
            continue;
        }
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
    on_delta: &mut impl FnMut(String),
    on_usage_limit: &impl Fn(),
) -> Result<(String, Vec<Value>), String> {
    let mut answer = String::new();
    let mut items = Vec::new();
    let mut pending = Vec::<u8>::new();
    let mut last = Instant::now();
    let mut done = false;
    'read: while let Some(chunk) = reply.chunk().await.map_err(|_| t("ChatGPT stopped before finishing the answer."))? {
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
    Ok((answer, items))
}

#[cfg(test)]
mod tests;

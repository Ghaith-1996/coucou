// chatgpt.rs without the network: sign-in pieces, the ID token checks, the
// request body, the stream's events and the error table.

use super::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!("test-id-token.json")).unwrap()
}

#[test]
fn base64url_round_trips_and_has_no_padding() {
    for len in 0..40 {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
        let text = b64url(&bytes);
        assert!(!text.contains('=') && !text.contains('+') && !text.contains('/'), "{text}");
        assert_eq!(b64url_decode(&text).unwrap(), bytes);
    }
    assert_eq!(b64url(b"hello"), "aGVsbG8");
    assert_eq!(b64url_decode("not base64!"), None);
}

#[test]
fn the_pkce_challenge_is_rfc_7636s() {
    // RFC 7636 appendix B.
    assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

#[test]
fn the_host_id_is_an_opaque_uuid_v4_urn() {
    let a = new_host_id();
    assert!(a.starts_with("urn:uuid:"), "{a}");
    let uuid = &a[9..];
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "4", "version 4");
    assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"), "variant");
    assert_ne!(a, new_host_id());
}

fn account(client: Option<&str>) -> Account {
    Account {
        host_id: "urn:uuid:00000000-0000-4000-8000-000000000000".into(),
        client_id: client.map(str::to_string),
        email: client.map(|_| "someone@example.com".to_string()),
        ..Default::default()
    }
}

fn query(url: &str) -> std::collections::HashMap<String, String> {
    Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

#[test]
fn a_first_sign_in_registers_with_the_dynamic_client() {
    let attempt = Attempt::new(1455);
    let url = authorize_url(&account(None), &attempt, None);
    assert!(url.starts_with("https://auth.openai.com/api/accounts/authorize?"));
    assert!(!url.contains('+'), "spaces as %20");
    let q = query(&url);
    assert_eq!(q["client_id"], "dynamic_agent_client");
    assert_eq!(q["agent_name_hint"], "Coucou");
    assert_eq!(q["ext_agent_host_id"], "urn:uuid:00000000-0000-4000-8000-000000000000");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:1455/auth/callback");
    assert_eq!(q["scope"], "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct");
    assert_eq!(q["resource"], "https://api.openai.com/v1");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["code_challenge"], pkce_challenge(&attempt.verifier));
    assert_eq!(q["state"], attempt.state);
    assert_eq!(q["nonce"], attempt.nonce);
    assert!(!q.contains_key("id_token_hint") && !q.contains_key("login_hint"));
}

#[test]
fn a_returning_account_reuses_its_client_and_hints_itself() {
    let attempt = Attempt::new(54321);
    let q = query(&authorize_url(&account(Some("oaiapp_test123")), &attempt, Some("old.id.token")));
    assert_eq!(q["client_id"], "oaiapp_test123");
    assert!(!q.contains_key("agent_name_hint"), "only for a new registration");
    assert_eq!(q["id_token_hint"], "old.id.token");
    assert_eq!(q["login_hint"], "someone@example.com");
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:54321/auth/callback", "only the port changes");
}

#[test]
fn only_a_returning_account_without_plan_access_requests_consent_again() {
    for (client, plan_enabled, expected_prompt) in [
        (None, false, None),
        (None, true, None),
        (Some("oaiapp_test123"), false, Some("consent")),
        (Some("oaiapp_test123"), true, None),
    ] {
        let mut account = account(client);
        account.plan_enabled = plan_enabled;
        // Recovery also works after restart without an ID-token hint.
        for hint in [None, Some("old.id.token")] {
            let attempt = Attempt::new(1455);
            let q = query(&authorize_url(&account, &attempt, hint));
            assert_eq!(q.get("prompt").map(String::as_str), expected_prompt,
                "client={client:?}, plan_enabled={plan_enabled}, hint={hint:?}");
            assert!(!q.contains_key("force_reconsent"), "requires confirmed integration rollout");
            assert_eq!(q["client_id"], client.unwrap_or(DYNAMIC_CLIENT));
            assert_eq!(q["scope"], SCOPE, "recovery retains the complete requested scopes");
            assert_eq!(q["ext_agent_host_id"], account.host_id);
            assert_eq!(q["state"], attempt.state);
            assert_eq!(q["nonce"], attempt.nonce);
            assert_eq!(q["code_challenge"], pkce_challenge(&attempt.verifier));
        }
    }
}

#[test]
fn every_attempt_has_fresh_secrets() {
    let (a, b) = (Attempt::new(1), Attempt::new(1));
    assert_ne!(a.state, b.state);
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.verifier, b.verifier);
    assert!((43..=128).contains(&a.verifier.len()), "RFC 7636 verifier length");
}

#[test]
fn the_callback_is_checked_before_any_code_is_used() {
    let at = Attempt::new(1455);
    let cb = |q: &str, known: Option<&str>| read_callback(&format!("/auth/callback?{q}"), &at, known);
    let state = &at.state;
    assert_eq!(
        cb(&format!("code=abc&state={state}&client_id=oaiapp_new"), None),
        Callback::Code { code: "abc".into(), client_id: Some("oaiapp_new".into()) }
    );
    assert_eq!(cb(&format!("code=abc&state={state}"), Some("oaiapp_old")), Callback::Code { code: "abc".into(), client_id: Some("oaiapp_old".into()) });
    assert!(matches!(cb("code=abc&state=forged&client_id=oaiapp_new", None), Callback::Error(_)), "state");
    assert!(matches!(cb("code=abc&client_id=oaiapp_new", None), Callback::Error(_)), "no state");
    assert_eq!(cb(&format!("error=access_denied&state={state}"), None), Callback::Denied);
    assert!(matches!(cb(&format!("code=abc&state={state}"), None), Callback::Error(_)), "registration without a client id");
    assert!(matches!(cb(&format!("code=abc&state={state}&client_id=dynamic_agent_client"), None), Callback::Error(_)), "the dynamic id is not an issued one");
    assert!(matches!(cb(&format!("code=abc&state={state}&client_id=oaiapp_other"), Some("oaiapp_old")), Callback::Error(_)), "another client");
    assert!(matches!(cb(&format!("state={state}&client_id=oaiapp_new"), None), Callback::Error(_)), "no code");
}

#[test]
fn a_real_rs256_id_token_verifies_and_a_changed_one_does_not() {
    let f = fixture();
    let token = f["token"].as_str().unwrap();
    verify_signature(token, &f["jwks"]).expect("valid signature");

    // Another payload under the same signature.
    let parts: Vec<&str> = token.split('.').collect();
    let forged_claims = b64url(br#"{"iss":"https://auth.openai.com","aud":"x","exp":4102444800,"sub":"evil","nonce":"n"}"#);
    let forged = format!("{}.{}.{}", parts[0], forged_claims, parts[2]);
    assert_eq!(verify_signature(&forged, &f["jwks"]), Err("signature".into()));
    // "none" or HMAC are refused before any key is looked at.
    let none = format!("{}.{}.", b64url(br#"{"alg":"none"}"#), parts[1]);
    assert_eq!(verify_signature(&none, &f["jwks"]), Err("algorithm".into()));
    let hs = format!("{}.{}.{}", b64url(br#"{"alg":"HS256","kid":"test-key"}"#), parts[1], parts[2]);
    assert_eq!(verify_signature(&hs, &f["jwks"]), Err("algorithm".into()));
    // A key set without that key.
    assert!(verify_signature(token, &json!({ "keys": [] })).is_err());
}

#[test]
fn the_id_tokens_claims_must_be_ours() {
    let claims = decode_claims(fixture()["token"].as_str().unwrap()).unwrap();
    assert_eq!(claims.sub, "user-abc");
    assert_eq!(claims.email.as_deref(), Some("someone@example.com"));
    let now = 1_800_000_000;
    assert_eq!(check_claims(&claims, "oaiapp_test123", "nonce-xyz", now), Ok(()));
    assert_eq!(check_claims(&claims, "oaiapp_other", "nonce-xyz", now), Err("audience".into()));
    assert_eq!(check_claims(&claims, "oaiapp_test123", "other-nonce", now), Err("nonce".into()));
    assert_eq!(check_claims(&claims, "oaiapp_test123", "nonce-xyz", 4_102_444_800), Err("expired".into()));
    let mut wrong = claims.clone();
    wrong.iss = "https://evil.example".into();
    assert_eq!(check_claims(&wrong, "oaiapp_test123", "nonce-xyz", now), Err("issuer".into()));
    let mut single = claims;
    single.aud = json!("oaiapp_test123");
    assert_eq!(check_claims(&single, "oaiapp_test123", "nonce-xyz", now), Ok(()));
}

#[test]
fn each_model_offers_its_own_efforts() {
    assert_eq!(efforts(LUNA), ["none", "low", "medium", "high", "xhigh", "max"]);
    assert_eq!(efforts(SOL), ["low", "medium", "high", "xhigh", "max"]);
    assert!(!effort_ok(SOL, "none"), "Sol has no none");
    assert!(effort_ok(LUNA, "none"));
    assert!(!effort_ok(LUNA, "minimal") && !effort_ok(LUNA, "ultra"));
    assert_eq!(efforts("gpt-other"), ["low", "medium", "high"]);
    // What's saved is kept when it applies, else the default.
    assert_eq!(effort_for(Some("max"), SOL), "max");
    assert_eq!(effort_for(Some("none"), SOL), "medium");
    assert_eq!(effort_for(None, LUNA), "medium");
    assert_eq!(effort_for(Some("none"), LUNA), "none");
}

#[test]
fn the_catalog_marks_missing_models_and_keeps_listed_ones() {
    let models = json!({ "data": [
        { "slug": "gpt-6.1-sol", "display_name": "GPT-6.1 Sol", "visibility": "list" },
        { "slug": "gpt-hidden", "display_name": "Hidden", "visibility": "hide" },
        { "slug": "gpt-other", "display_name": "GPT Other", "visibility": "list" }
    ]});
    let c = catalog(&models);
    let ids: Vec<(&str, bool)> = c.iter().map(|m| (m.id.as_str(), m.available)).collect();
    assert_eq!(ids, [(LUNA, false), (SOL, true), ("gpt-other", true)], "Luna shown as unavailable, never swapped");
    assert_eq!(c[0].label, "GPT-6 Luna");
    assert_eq!(c[1].efforts, ["low", "medium", "high", "xhigh", "max"]);
    assert_eq!(as_model_infos(&c).len(), 2);
    let empty = catalog(&json!({}));
    assert!(empty.iter().all(|m| !m.available) && empty.len() == 2);
}

#[test]
fn the_request_is_exactly_what_the_picker_shows() {
    let history = vec![
        json!({ "role": "user", "content": "hi" }),
        json!({ "role": "assistant", "content": "hello" }),
        json!({ "role": "system", "content": "ignored" }),
    ];
    let body = request_body(LUNA, "low", "Be Mochi.", &history, json!("next?"));
    assert_eq!(body["model"], "gpt-6-luna");
    assert_eq!(body["reasoning"], json!({ "effort": "low" }));
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["instructions"], "Be Mochi.");
    assert_eq!(
        body["input"],
        json!([
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": "hello" },
            { "role": "user", "content": "next?" }
        ]),
        "no system message in input"
    );
    let body = request_body(SOL, "max", "x", &[], json!("q"));
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(body["reasoning"]["effort"], "max");
    for unsupported in ["temperature", "max_output_tokens", "previous_response_id", "metadata", "user", "top_p", "truncation"] {
        assert!(body.get(unsupported).is_none(), "{unsupported}");
    }
}

#[test]
fn stream_events_are_read_as_the_docs_name_them() {
    assert_eq!(parse_event(r#"{"type":"response.output_text.delta","delta":"Hi"}"#), StreamEvent::Delta("Hi".into()));
    assert_eq!(parse_event(r#"{"type":"response.completed","response":{}}"#), StreamEvent::Completed);
    assert_eq!(
        parse_event(r#"{"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}}}"#),
        StreamEvent::Failed { status: 0, code: "subscription_sharing_usage_limit_exceeded".into(), message: "m".into() }
    );
    assert_eq!(parse_event(r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#), StreamEvent::Incomplete("max_output_tokens".into()));
    assert!(matches!(parse_event(r#"{"type":"error","code":"x","message":"y"}"#), StreamEvent::Failed { .. }));
    assert_eq!(parse_event(r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#), StreamEvent::Other, "reasoning isn't the answer");
    assert_eq!(parse_event("[DONE]"), StreamEvent::Other);
}

#[test]
fn errors_follow_the_recovery_table() {
    let e = |status, code: &str| error_message(status, &json!({ "error": { "code": code, "param": "tools" } }));
    let limit = e(429, "subscription_sharing_usage_limit_exceeded");
    assert!(limit.usage_limit && !limit.transient);
    assert!(e(503, "subscription_sharing_usage_unavailable").transient);
    assert!(e(503, "subscription_sharing_user_unavailable").transient);
    assert!(!e(403, "subscription_sharing_user_not_eligible").transient, "never retried");
    assert!(!e(400, "subscription_sharing_unsupported_capability").transient, "never resent as is");
    assert!(e(400, "subscription_sharing_unsupported_capability").text.contains("tools"));
    assert!(!e(401, "subscription_sharing_invalid_user").usage_limit);
    assert!(error_message(429, &Value::Null).usage_limit, "a bare 429 is the limit too");
    assert!(error_message(503, &Value::Null).transient);
    assert!(!error_message(500, &Value::Null).transient);
}

#[test]
fn the_status_never_carries_a_token() {
    let text = serde_json::to_string(&status()).unwrap();
    for word in ["token", "refresh", "access", "secret"] {
        assert!(!text.to_lowercase().contains(word), "{text}");
    }
}

#[test]
fn the_effort_table_is_the_one_the_island_uses() {
    let f: Value = serde_json::from_str(include_str!("../../../tests/fixtures/chatgpt-efforts.json")).unwrap();
    assert_eq!(f["default"], DEFAULT_EFFORT);
    for (model, list) in f["models"].as_object().unwrap() {
        let expected: Vec<&str> = list.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(efforts(model), expected.as_slice(), "{model}");
    }
}

/// Prints a real authorize URL for a first sign-in (opt-in): opened in a
/// browser, OpenAI shows its sign-in page, or an error for a bad parameter.
#[test]
#[ignore]
fn print_a_first_sign_in_url() {
    let account = Account { host_id: new_host_id(), ..Default::default() };
    println!("URL={}", authorize_url(&account, &Attempt::new(1455), None));
}

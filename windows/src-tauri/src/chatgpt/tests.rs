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
fn a_registration_is_saved_before_its_code_is_available_for_exchange() {
    let mut account = account(None);
    let attempt = Attempt::new(1455);
    let target = format!("/auth/callback?code=abc&state={}&client_id=oaiapp_new", attempt.state);
    let mut saved = Vec::new();
    let (code, client_id) = prepare_code_exchange(&mut account, &attempt, &target, |record| {
        saved = serde_json::to_vec(record).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(code, "abc");
    assert_eq!(client_id, "oaiapp_new");

    // Exchange/verification have not run. If either fails, a restart still
    // reloads the registration without trusting an unverified identity.
    let restored: Account = serde_json::from_slice(&saved).unwrap();
    assert_eq!(restored, account);
    assert_eq!(restored.email, None);
    assert_eq!(restored.sub, None);
    assert!(!restored.plan_enabled);
    assert_eq!(restored.host_id, account.host_id);
    let retry = query(&authorize_url(&restored, &Attempt::new(54321), None));
    assert_eq!(retry["client_id"], "oaiapp_new");
    assert!(!retry.contains_key("agent_name_hint"));
    assert!(!retry.contains_key("id_token_hint") && !retry.contains_key("login_hint"));
    assert_eq!(retry["prompt"], "consent", "the missing plan grant can still be authorized");
}

#[test]
fn an_invalid_callback_never_saves_a_registration_or_returns_a_code() {
    let attempt = Attempt::new(1455);
    for (known, parameters) in [
        (None, "code=abc&state=forged&client_id=oaiapp_new".to_string()),
        (None, format!("error=access_denied&state={}&client_id=oaiapp_new", attempt.state)),
        (None, format!("code=abc&state={}", attempt.state)),
        (None, format!("code=abc&state={}&client_id=dynamic_agent_client", attempt.state)),
        (None, format!("state={}&client_id=oaiapp_new", attempt.state)),
        (Some("oaiapp_old"), format!("code=abc&state={}&client_id=oaiapp_other", attempt.state)),
    ] {
        let mut account = account(known);
        let original = account.clone();
        let result = prepare_code_exchange(&mut account, &attempt, &format!("/auth/callback?{parameters}"), |_| {
            panic!("an invalid callback must not be persisted")
        });
        assert!(result.is_err(), "{parameters}");
        assert_eq!(account, original);
    }
}

#[test]
fn a_registration_save_failure_stops_before_code_exchange() {
    let mut account = account(None);
    let original = account.clone();
    let attempt = Attempt::new(1455);
    let target = format!("/auth/callback?code=abc&state={}&client_id=oaiapp_new", attempt.state);
    assert_eq!(prepare_code_exchange(&mut account, &attempt, &target, |_| Err("disk full".into())), Err("disk full".into()));
    assert_eq!(account, original);
}

#[test]
fn reauthorization_keeps_the_existing_account_until_identity_is_verified() {
    let mut account = account(Some("oaiapp_old"));
    account.sub = Some("original-user".into());
    account.plan_enabled = true;
    let original = account.clone();
    let attempt = Attempt::new(1455);
    for suffix in ["", "&client_id=oaiapp_old"] {
        let target = format!("/auth/callback?code=abc&state={}{suffix}", attempt.state);
        let result = prepare_code_exchange(&mut account, &attempt, &target, |_| {
            panic!("the known registration need not be saved again")
        });
        assert_eq!(result, Ok(("abc".into(), "oaiapp_old".into())));
        assert_eq!(account, original);
    }
}

#[test]
fn a_real_rs256_id_token_verifies_and_a_changed_one_does_not() {
    let f = fixture();
    let token = f["token"].as_str().unwrap();
    verify_signature(token, &f["jwks"]).expect("valid signature");
    let claims = decode_claims(token).unwrap();
    check_claims(&claims, "oaiapp_test123", "nonce-xyz", 1_800_000_000).expect("valid signed claims");

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
fn id_token_audiences_must_all_be_trusted_by_this_client() {
    let original = decode_claims(fixture()["token"].as_str().unwrap()).unwrap();
    for (aud, accepted) in [
        (json!("oaiapp_test123"), true),
        (json!(["oaiapp_test123"]), true),
        // Repeating the same trusted audience does not add another party.
        (json!(["oaiapp_test123", "oaiapp_test123"]), true),
        (json!("oaiapp_other"), false),
        (json!("OAIAPP_test123"), false),
        (json!(["oaiapp_other"]), false),
        (json!(["oaiapp_test123", "oaiapp_other"]), false),
        (json!(["oaiapp_other", "oaiapp_test123"]), false),
        (json!(["oaiapp_test123", RESOURCE]), false),
        (json!(""), false),
        (json!([]), false),
        (json!([""]), false),
        (json!(["oaiapp_test123", ""]), false),
        (Value::Null, false),
        (json!(123), false),
        (json!(true), false),
        (json!({ "client_id": "oaiapp_test123" }), false),
        (json!(["oaiapp_test123", null]), false),
        (json!(["oaiapp_test123", 123]), false),
        (json!(["oaiapp_test123", true]), false),
        (json!(["oaiapp_test123", ["oaiapp_test123"]]), false),
        (json!(["oaiapp_test123", { "client_id": "oaiapp_test123" }]), false),
    ] {
        let mut claims = original.clone();
        claims.aud = aud.clone();
        claims.azp = Some("oaiapp_test123".into());
        let result = check_claims(&claims, "oaiapp_test123", "nonce-xyz", 1_800_000_000);
        assert_eq!(result, if accepted { Ok(()) } else { Err("audience".into()) }, "aud={aud}");
    }
}

#[test]
fn an_id_tokens_optional_authorized_party_must_be_this_client() {
    for (aud, trusted) in [
        (json!("oaiapp_test123"), true),
        (json!(["oaiapp_test123"]), true),
        (json!(["oaiapp_test123", "oaiapp_test123"]), true),
        (json!(["oaiapp_test123", "oaiapp_other"]), false),
        (json!(["oaiapp_other", "oaiapp_test123"]), false),
    ] {
        for (azp, well_formed, matches_client) in [
            (None, true, true),
            (Some(json!("oaiapp_test123")), true, true),
            (Some(json!("oaiapp_other")), true, false),
            (Some(json!("OAIAPP_test123")), true, false),
            (Some(json!("")), true, false),
            (Some(Value::Null), false, false),
            (Some(json!(123)), false, false),
            (Some(json!(true)), false, false),
            (Some(json!(["oaiapp_test123"])), false, false),
            (Some(json!({ "client_id": "oaiapp_test123" })), false, false),
        ] {
            let mut payload = json!({ "iss": ISSUER, "aud": aud, "sub": "user-abc",
                "exp": 4_102_444_800_u64, "nonce": "nonce-xyz" });
            if let Some(azp) = &azp { payload["azp"] = azp.clone(); }
            // Exercise decoding without pretending these synthetic claims
            // have a valid signature; the real RS256 fixture is tested above.
            let token = format!("e30.{}.", b64url(&serde_json::to_vec(&payload).unwrap()));
            let decoded = decode_claims(&token);
            assert_eq!(decoded.is_some(), well_formed, "aud={aud}, azp={azp:?}");
            if let Some(claims) = decoded {
                let result = check_claims(&claims, "oaiapp_test123", "nonce-xyz", 1_800_000_000);
                let multiple = aud.as_array().is_some_and(|list| list.len() > 1);
                let accepted = trusted && matches_client && (!multiple || azp.is_some());
                assert_eq!(result.is_ok(), accepted, "aud={aud}, azp={azp:?}");
            }
        }
    }
}

#[test]
fn an_id_token_without_an_audience_is_rejected() {
    let payload = json!({ "iss": ISSUER, "sub": "user-abc", "exp": 4_102_444_800_u64,
        "nonce": "nonce-xyz", "azp": "oaiapp_test123" });
    let token = format!("e30.{}.", b64url(&serde_json::to_vec(&payload).unwrap()));
    assert!(decode_claims(&token).is_none());
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
fn a_dropped_image_survives_the_next_native_turn_without_rereading_the_file() {
    let path = std::env::temp_dir().join(format!("coucou-chatgpt-image-{}.png", random(12)));
    std::fs::write(&path, b"image fixture").unwrap();
    let context = ChatContext::File { name: "picture.png".into(), path: path.to_string_lossy().into_owned() };
    let chat = Chat::default();
    let first = chat.begin(PROVIDER);
    let user = user_content(first.first, Some(&context), "Describe this image");
    std::fs::remove_file(&path).unwrap();
    let initial = request_body(LUNA, "low", "x", &first.history, user.clone());
    assert_eq!(initial["input"][0]["content"][0]["type"], "input_image");
    assert_eq!(initial["input"][0]["content"][0]["image_url"], "data:image/png;base64,aW1hZ2UgZml4dHVyZQ==");
    let plain = chat::plain_question(first.first, Some(&context), "Describe this image");
    let reply = finish_turn(&chat, &first, user, &plain, "A picture".into());
    assert_eq!(reply.text, "A picture");

    let next = chat.begin(PROVIDER);
    assert!(!next.first);
    let follow_up = user_content(next.first, Some(&context), "What color is it?");
    assert_eq!(follow_up, json!("What color is it?"), "the original file is not read again");
    let body = request_body(SOL, "max", "x", &next.history, follow_up);
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][0], initial["input"][0], "the full visual input is replayed");
    assert_eq!(body["input"][1], json!({ "role": "assistant", "content": "A picture" }));
    assert_eq!(body["input"][2], json!({ "role": "user", "content": "What color is it?" }));
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
    assert!(chat.begin(chat::ANTHROPIC).history[0]["content"].is_string(), "other providers still get plain history");
}

#[test]
fn an_uncommitted_provider_switch_discards_native_image_and_text_attachments() {
    // begin() converts history immediately, before any network request. A
    // failed or stopped switch can therefore lose attachment content without
    // ever committing an answer; switching back alone cannot recover it.
    for extension in ["png", "txt"] {
        let path = std::env::temp_dir().join(format!("coucou-chatgpt-switch-{}.{extension}", random(12)));
        std::fs::write(&path, b"attachment fixture").unwrap();
        let context = ChatContext::File { name: format!("file.{extension}"), path: path.to_string_lossy().into_owned() };
        let chat = Chat::default();
        let first = chat.begin(PROVIDER);
        let user = user_content(first.first, Some(&context), "Describe it");
        std::fs::remove_file(&path).unwrap();
        let plain = chat::plain_question(first.first, Some(&context), "Describe it");
        assert_ne!(user, json!(plain), "the native input includes actual file content");
        finish_turn(&chat, &first, user.clone(), &plain, "An answer".into());

        let same = chat.begin(PROVIDER);
        assert_eq!(same.history[0]["content"], user, "same-provider retries retain attachment content without rereading the file");
        let switched = chat.begin("openai");
        assert_eq!(switched.history[0]["content"], json!(plain));
        assert!(!switched.first);
        // Deliberately do not commit: this models failure or Stop after begin.
        let back = chat.begin(PROVIDER);
        assert_eq!(back.history[0]["content"], json!(plain));
        assert_eq!(back.history.len(), 2, "the abandoned switch added no answer");
        assert!(!back.first, "the original attachment is not reattached on a later turn");
    }
}

#[test]
fn an_uncommitted_attempt_keeps_image_and_text_context_for_the_first_completed_turn() {
    for extension in ["png", "txt"] {
        let path = std::env::temp_dir().join(format!("coucou-chatgpt-retry-{}.{}", random(12), extension));
        std::fs::write(&path, b"attachment fixture").unwrap();
        let context = ChatContext::File { name: format!("file.{extension}"), path: path.to_string_lossy().into_owned() };
        let chat = Chat::default();

        // Neither Stop (with or without a visible delta) nor failure commits.
        for _ in 0..3 {
            let abandoned = chat.begin(PROVIDER);
            assert!(abandoned.first);
            assert_ne!(user_content(abandoned.first, Some(&context), "Describe it"), json!("Describe it"));
        }
        let retry = chat.begin(PROVIDER);
        let user = user_content(retry.first, Some(&context), "Try again");
        let first = request_body(SOL, "max", "x", &retry.history, user.clone());
        if extension == "png" {
            assert_eq!(first["input"][0]["content"][0]["type"], "input_image");
        } else {
            assert!(first["input"][0]["content"].as_str().unwrap().contains("attachment fixture"));
        }
        finish_turn(&chat, &retry, user, &chat::plain_question(retry.first, Some(&context), "Try again"), "An answer".into());
        std::fs::remove_file(path).unwrap();

        let next = chat.begin(PROVIDER);
        assert!(!next.first);
        let user = user_content(next.first, Some(&context), "What else?");
        assert_eq!(user, json!("What else?"));
        let body = request_body(SOL, "max", "x", &next.history, user);
        assert_eq!(body["input"][0], first["input"][0], "the completed turn keeps its attachment without rereading it");
        assert_eq!(body["input"].as_array().unwrap().len(), 3, "abandoned attempts are not replayed");
    }
}

// The provider is polled directly by chat_send's select, not spawned. Every
// provider commits synchronously immediately before returning Ready. Cover
// both possible polling orders without relying on select's random choice.
#[tokio::test]
async fn a_completed_turn_wins_even_if_stop_arrives_between_commit_and_ready() {
    for stop_first in [false, true] {
        let chat = Chat::default();
        let turn = chat.begin(PROVIDER);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let provider = async {
            let reply = finish_turn(&chat, &turn, json!("question"), "question", "complete answer".into());
            stop.send(()).unwrap();
            // No await here: this is the real providers' commit boundary.
            reply
        };
        let reply = if stop_first {
            tokio::select! { biased;
                _ = stopped => None,
                reply = provider => Some(reply),
            }
        } else {
            tokio::select! { biased;
                reply = provider => Some(reply),
                _ = stopped => None,
            }
        }.expect("Stop cannot supersede the same poll that committed and returned Ready");
        assert_eq!(reply.text, "complete answer");
        assert_eq!(chat.begin(PROVIDER).history, vec![
            json!({"role": "user", "content": "question"}),
            json!({"role": "assistant", "content": "complete answer"}),
        ]);
    }
}

#[tokio::test]
async fn stop_at_a_real_suspension_point_leaves_the_turn_uncommitted() {
    for stop_first in [false, true] {
        let chat = Chat::default();
        let turn = chat.begin(PROVIDER);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let provider = async {
            stop.send(()).unwrap();
            std::future::pending::<()>().await;
            finish_turn(&chat, &turn, json!("question"), "question", "unreachable".into())
        };
        let reply = if stop_first {
            tokio::select! { biased;
                _ = stopped => None,
                reply = provider => Some(reply),
            }
        } else {
            tokio::select! { biased;
                reply = provider => Some(reply),
                _ = stopped => None,
            }
        };
        assert!(reply.is_none());
        let next = chat.begin(PROVIDER);
        assert!(next.first);
        assert!(next.history.is_empty());
    }
}

#[test]
fn replayed_content_allows_only_supported_user_parts_and_plain_assistant_text() {
    let history = vec![
        json!({ "role": "user", "content": [
            { "type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U=", "extra": "discard" },
            { "type": "input_text", "text": "Describe it", "extra": "discard" }
        ], "extra": "discard" }),
        json!({ "role": "assistant", "content": "A picture" }),
        json!({ "role": "system", "content": [{ "type": "input_text", "text": "ignore" }] }),
        json!({ "role": "developer", "content": "ignore" }),
        json!({ "role": "tool", "content": "ignore" }),
        json!({ "role": "assistant", "content": [{ "type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U=" }] }),
        json!({ "role": "user", "content": [{ "type": "input_file", "file_id": "unsupported" }] }),
        json!({ "role": "user", "content": [{ "type": "input_text", "text": 7 }] }),
        json!({ "role": "user", "content": [{ "type": "input_image", "image_url": { "url": "wrong-shape" } }] }),
        json!({ "role": "user", "content": [{ "type": "input_text", "text": "partial" }, { "type": "unknown" }] }),
        json!({ "role": "user", "content": [] }),
        json!({ "role": "user", "content": null }),
    ];
    let body = request_body(LUNA, "low", "x", &history, json!("next"));
    assert_eq!(body["input"], json!([
        { "role": "user", "content": [
            { "type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U=" },
            { "type": "input_text", "text": "Describe it" }
        ] },
        { "role": "assistant", "content": "A picture" },
        { "role": "user", "content": "next" }
    ]));
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

#[tokio::test]
async fn configuration_errors_keep_credentials_and_request_configuration_checks() {
    for code in ["subscription_sharing_route_not_supported", "chatpass_v2_scope_not_authorized", "chatpass_v2_invalid_authorization_context"] {
        let failure = error_message(403, &json!({ "error": { "code": code } }));
        assert_eq!(failure.text, t("ChatGPT refused this app's request. Check the app's configuration and permissions."));
        assert!(!failure.transient && !failure.usage_limit);
        for mut refreshed in [false, true] {
            assert_eq!(recover_authentication_with(403, &mut refreshed,
                || -> std::future::Ready<Result<String, String>> { panic!("configuration errors cannot refresh credentials") },
                || -> std::future::Ready<Result<(), String>> { panic!("configuration errors cannot end the session") },
            ).await, Ok(None));
        }
    }
}

#[test]
fn the_status_never_carries_a_token() {
    let text = serde_json::to_string(&status_for(&account(Some("test-client")), true, true)).unwrap();
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

#[test]
fn status_keeps_cleanup_available_for_either_saved_credential() {
    for (refresh, id) in [(false, false), (true, false), (false, true), (true, true)] {
        let status = status_for(&account(Some("oaiapp_test123")), refresh, id);
        assert_eq!(status.connected, refresh);
        assert_eq!(status.has_saved_credentials, refresh || id);
        let serialized = serde_json::to_value(status).unwrap();
        assert_eq!(serialized["hasSavedCredentials"], refresh || id);
    }
}

#[test]
fn ended_sessions_keep_reconnect_and_cleanup_visible_without_authorizing_switching() {
    for explicit in [false, true] {
        for (refresh, id) in [(false, false), (true, false), (false, true), (true, true)] {
            let account = Account { session_needs_reconnect: true, explicitly_disconnected: explicit, ..registered_account() };
            let status = status_for(&account, refresh, id);
            assert!(!status.connected && status.needs_reconnect && !status.show_welcome);
            assert_eq!(status.has_saved_credentials, refresh || id);
            assert_eq!(status.can_switch_accounts, explicit && !refresh && !id);
            assert_eq!(require_signed_out(&account, Ok((refresh, id))).is_ok(), explicit && !refresh && !id);
            assert!(require_selected_client(&account, "oaiapp_original").is_err());
        }
    }
}

#[test]
fn status_read_failures_never_hide_recovery_or_claim_a_usable_session() {
    for (refresh, id) in [(false, false), (true, false), (false, true), (true, true)] {
        let status = status_from_storage(Err("unreadable metadata".into()), Ok((refresh, id)), Some("oaiapp_original"), None);
        assert!(!status.connected && !status.can_switch_accounts && !status.show_welcome);
        assert!(status.needs_reconnect);
        assert_eq!(status.has_saved_credentials, refresh || id);
    }
    for account in [Ok(registered_account()), Ok(Account { explicitly_disconnected: true, ..registered_account() }), Err("corrupt metadata".into())] {
        let status = status_from_storage(account, Err("keystore locked".into()), Some("oaiapp_original"), None);
        assert!(!status.connected && !status.can_switch_accounts && !status.show_welcome);
        assert!(status.needs_reconnect && status.has_saved_credentials);
    }
    let orphaned = status_from_storage(Ok(Account::default()), Ok((true, false)), None, None);
    assert!(!orphaned.connected && orphaned.has_saved_credentials && !orphaned.can_switch_accounts);
}

#[tokio::test]
async fn authenticated_requests_refresh_once_even_after_transient_responses() {
    for (statuses, should_reconnect) in [
        (vec![200], false), (vec![401, 200], false), (vec![503, 401, 200], false),
        (vec![401, 503, 200], false), (vec![503, 503, 503], false),
        (vec![401, 401], true), (vec![503, 401, 401], true), (vec![401, 503, 401], true),
    ] {
        let mut refreshed = false;
        let refreshes = std::cell::Cell::new(0);
        let rejections = std::cell::Cell::new(0);
        let mut token = "original-access".to_string();
        let mut terminal = false;
        for status in statuses.iter().copied() {
            let result = recover_authentication_with(status, &mut refreshed,
                || {
                    refreshes.set(refreshes.get() + 1);
                    std::future::ready(Ok("new-access".into()))
                },
                || {
                    assert_eq!(token, "new-access", "only a rejected refreshed token ends the session");
                    rejections.set(rejections.get() + 1);
                    std::future::ready(Ok(()))
                },
            ).await;
            match result {
                Ok(Some(next)) => token = next,
                Ok(None) => {},
                Err(error) => {
                    assert_eq!(error, t("Your ChatGPT session ended. Reconnect in Settings."));
                    terminal = true;
                    break;
                },
            }
        }
        assert_eq!(refreshes.get(), usize::from(statuses.contains(&401)));
        assert_eq!(rejections.get(), usize::from(should_reconnect));
        assert_eq!(terminal, should_reconnect);
    }
}

#[tokio::test]
async fn a_failed_forced_refresh_keeps_its_original_recovery_result() {
    for error in ["temporary refresh outage", "refresh token expired", "credential write failed"] {
        let mut refreshed = false;
        let result = recover_authentication_with(401, &mut refreshed,
            || std::future::ready(Err(error.to_string())),
            || -> std::future::Ready<Result<(), String>> { panic!("the refresh path already owns recovery") },
        ).await;
        assert_eq!(result, Err(error.to_string()));
    }
}

#[test]
fn a_second_rejection_is_recorded_before_an_error_body_can_wait_or_be_cancelled() {
    let rejected = std::sync::Mutex::new(None);
    let original = registered_account();
    let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 4 };
    let persisted = std::cell::RefCell::new(original.clone());
    let invalidated = std::cell::Cell::new(false);
    let mut operation = Box::pin(async {
        let mut refreshed = true;
        recover_authentication_with(401, &mut refreshed,
            || -> std::future::Ready<Result<String, String>> { panic!("never refresh twice") },
            || std::future::ready(reject_session_with(&request, 4, true, Ok(original.clone()), &rejected,
                |next| { persisted.replace(next.clone()); Ok(()) },
                |key, value| { assert_eq!((key, value), (REFRESH_TOKEN_KEY, "")); Ok(()) },
                || invalidated.set(true),
            )),
        ).await?;
        std::future::pending::<Result<(), String>>().await
    });
    let mut context = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(matches!(std::future::Future::poll(operation.as_mut(), &mut context), std::task::Poll::Ready(Err(_))));
    drop(operation);
    assert!(invalidated.get());
    assert!(persisted.borrow().session_needs_reconnect);
    assert_eq!(rejected.lock().unwrap().as_deref(), Some("oaiapp_original"));
}

#[test]
fn rejected_sessions_fail_closed_when_metadata_or_credential_cleanup_fails() {
    for readable in [false, true] {
        for saved in [false, true] {
            for deleted in [false, true] {
                let rejected = std::sync::Mutex::new(None);
                let original = registered_account();
                let persisted = std::cell::RefCell::new(original.clone());
                let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 4 };
                let generation = std::cell::Cell::new(4);
                let access_present = std::cell::Cell::new(true);
                let refresh_present = std::cell::Cell::new(true);
                let result = reject_session_with(&request, generation.get(), true,
                    if readable { Ok(original.clone()) } else { Err("unreadable metadata".into()) }, &rejected,
                    |next| {
                        assert!(!access_present.get(), "invalidate before touching fallible storage");
                        assert!(next.session_needs_reconnect);
                        if !saved { return Err("disk full".into()); }
                        persisted.replace(next.clone()); Ok(())
                    },
                    |key, value| {
                        assert_eq!((key, value), (REFRESH_TOKEN_KEY, ""), "retain the ID hint and registration");
                        if !deleted { return Err("keystore locked".into()); }
                        refresh_present.set(false); Ok(())
                    },
                    || { access_present.set(false); generation.set(generation.get() + 1); },
                );
                assert_eq!(result.is_ok(), readable && saved);
                assert!(!access_present.get());
                assert_eq!(generation.get(), 5);
                assert!(check_session_generation(&request, generation.get()).is_err());
                assert_eq!(persisted.borrow().registration(), original.registration());
                assert_eq!(persisted.borrow().host_id, original.host_id);
                assert!(!persisted.borrow().explicitly_disconnected);
                let status = status_from_storage(Ok(persisted.borrow().clone()), Ok((refresh_present.get(), true)),
                    Some("oaiapp_original"), rejected.lock().unwrap().as_deref());
                assert!(!status.connected && status.needs_reconnect && !status.show_welcome && !status.can_switch_accounts,
                    "even a failed save and an active-refresh hint cannot hide the known rejection");
                let mut next_request_account = persisted.borrow().clone();
                apply_session_rejection(&mut next_request_account, rejected.lock().unwrap().as_deref());
                assert!(require_selected_client(&next_request_account, &request.client_id).is_err());
                if (readable && saved) || deleted {
                    let restarted = status_for(&persisted.borrow(), refresh_present.get(), true);
                    assert!(!restarted.connected && restarted.needs_reconnect);
                }
            }
        }
    }
}

#[test]
fn stale_rejections_cannot_end_a_new_account_or_a_newer_access_token() {
    let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 4 };
    let original = registered_account();
    let mut other = original.clone(); other.client_id = Some("oaiapp_new".into());
    let cached = Access { client_id: request.client_id.clone(), generation: 4,
        token: "newer-access".into(), until: Instant::now() + Duration::from_secs(3600) };
    assert!(!cached.matches_rejection(&request, "old-access"));
    assert!(cached.matches_rejection(&request, "newer-access"));
    assert!(!cached.matches_rejection(&RequestAccount { client_id: "oaiapp_new".into(), generation: 4 }, "newer-access"));
    assert!(!cached.matches_rejection(&RequestAccount { client_id: request.client_id.clone(), generation: 5 }, "newer-access"));
    for (generation, owns_token, account) in [(5, true, original.clone()), (4, true, other), (4, false, original)] {
        let rejected = std::sync::Mutex::new(None);
        assert!(reject_session_with(&request, generation, owns_token, Ok(account), &rejected,
            |_| panic!("stale requests cannot persist metadata"),
            |_, _| panic!("stale requests cannot erase current credentials"),
            || panic!("stale requests cannot invalidate current access"),
        ).is_err());
        assert!(rejected.lock().unwrap().is_none());
    }
}

#[test]
fn rejected_client_latches_never_hide_another_selected_accounts_connection() {
    let status = status_from_storage(Ok(registered_account()), Ok((true, true)), None, Some("oaiapp_other"));
    assert!(status.connected && !status.needs_reconnect);
}

#[tokio::test]
async fn terminal_refresh_recovery_survives_failed_cleanup_and_restart_without_a_post_error_write() {
    for code in ["invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalidated", "refresh_token_reused"] {
        for deletion_ok in [false, true] {
            let active = std::sync::Mutex::new(None);
            let original = registered_account();
            let persisted = std::cell::RefCell::new(serde_json::to_vec(&original).unwrap());
            let writes = std::cell::Cell::new(0);
            let access = std::cell::Cell::new(true);
            let generation = std::cell::Cell::new(4);
            let refresh_present = std::cell::Cell::new(true);
            let result = refresh_with(&active, &original,
                || {
                    let guarded: Account = serde_json::from_slice(&persisted.borrow()).unwrap();
                    assert!(guarded.session_needs_reconnect,
                        "the guard must reach disk before the token can be rejected");
                    assert!(status_from_storage(Ok(guarded.clone()), Ok((true, true)), active.lock().unwrap().as_deref(), None).connected,
                        "in-flight status still describes the starting session");
                    assert!(!status_from_storage(Ok(guarded), Ok((true, true)), Some("oaiapp_other"), None).connected,
                        "another client's refresh must never mask this marker");
                    std::future::ready(Err(RefreshFailure::Rejected { status: 400, code: code.into() }))
                },
                |next| {
                    writes.set(writes.get() + 1);
                    if writes.get() != 1 { return Err("disk failed after the exchange".into()); }
                    persisted.replace(serde_json::to_vec(next).unwrap()); Ok(())
                },
                |key, value| {
                    assert_eq!((key, value), (REFRESH_TOKEN_KEY, ""));
                    if deletion_ok { refresh_present.set(false); Ok(()) } else { Err("keystore locked".into()) }
                },
                || { access.set(false); generation.set(generation.get() + 1); },
                |_| panic!("a terminal error cannot cache access"),
            ).await;
            assert_eq!(result, Err(t("Your ChatGPT session ended. Reconnect in Settings.")));
            assert!(active.lock().unwrap().is_none());
            assert_eq!(writes.get(), 1, "terminal recovery never depends on a post-error metadata write");
            assert!(!access.get());
            assert_eq!(generation.get(), 5);
            assert!(check_session_generation(&RequestAccount { client_id: "oaiapp_original".into(), generation: 4 }, generation.get()).is_err());
            let restarted: Account = serde_json::from_slice(&persisted.borrow()).unwrap();
            assert_eq!(restarted.registration(), original.registration());
            assert_eq!(restarted.host_id, original.host_id);
            assert!(!restarted.explicitly_disconnected);
            let status = status_for(&restarted, refresh_present.get(), true);
            assert!(!status.connected && status.needs_reconnect && status.has_saved_credentials);
            assert!(!status.can_switch_accounts && !status.show_welcome);
            assert!(require_selected_client(&restarted, "oaiapp_original").is_err(), "the unusable token cannot be retried");
            assert!(require_signed_out(&restarted, Ok((false, false))).is_err(), "expiry is not explicit Disconnect");
        }
    }
}

#[tokio::test]
async fn failed_refresh_preflight_cannot_call_the_endpoint_or_change_credentials() {
    let active = std::sync::Mutex::new(None);
    let original = registered_account();
    let result = refresh_with(&active, &original,
        || -> std::future::Ready<Result<Tokens, RefreshFailure>> { panic!("no exchange without durable recovery") },
        |_| Err("disk full".into()),
        |_, _| panic!("no credential writes before the guard is saved"),
        || panic!("a failed preflight never changed the session"),
        |_| panic!("no access token was issued"),
    ).await;
    assert_eq!(result, Err("disk full".into()));
    assert!(active.lock().unwrap().is_none());
    assert!(!original.session_needs_reconnect);
}

#[tokio::test]
async fn nonterminal_refresh_failures_restore_the_session_only_if_metadata_commit_succeeds() {
    for code in [None, Some("temporarily_unavailable"), Some("server_error"), Some("invalid_client"), Some("")] {
        for restore_ok in [true, false] {
            let active = std::sync::Mutex::new(None);
            let original = registered_account();
            let persisted = std::cell::RefCell::new(original.clone());
            let invalidated = std::cell::Cell::new(false);
            let mut writes = 0;
            let result = refresh_with(&active, &original,
                || std::future::ready(Err(match code {
                    None => RefreshFailure::Unreachable,
                    Some(code) => RefreshFailure::Rejected { status: 503, code: code.into() },
                })),
                |next| {
                    writes += 1;
                    if writes == 2 && !restore_ok { return Err("disk full".into()); }
                    persisted.replace(next.clone()); Ok(())
                },
                |_, _| panic!("nonterminal errors must not delete or replace credentials"),
                || invalidated.set(true),
                |_| panic!("no token was issued"),
            ).await;
            assert!(result.is_err());
            assert!(active.lock().unwrap().is_none());
            assert_eq!(writes, 2);
            assert_eq!(invalidated.get(), !restore_ok);
            let restarted: Account = serde_json::from_slice(&serde_json::to_vec(&*persisted.borrow()).unwrap()).unwrap();
            assert_eq!(restarted.session_needs_reconnect, !restore_ok);
            assert_eq!(restarted.registration(), original.registration());
            assert_eq!(status_for(&restarted, true, true).connected, restore_ok);
            assert_eq!(require_selected_client(&restarted, "oaiapp_original").is_ok(), restore_ok);
        }
    }
}

#[tokio::test]
async fn successful_refresh_unblocks_only_after_both_credential_and_metadata_commits() {
    for (scope, granted) in sign_in_scope_cases().into_iter().chain([
        (Some("   "), false),
        (Some(PLAN_SCOPE), true),
        (Some("  email  chatgpt.tokens.use.direct  openid  "), true),
    ]) {
      for rotated in [false, true] {
        for store_ok in [false, true] {
            for finish_ok in [false, true] {
                let active = std::sync::Mutex::new(None);
                let mut original = registered_account();
                original.saved_accounts.push(account(Some("oaiapp_saved")).registration().unwrap());
                original.pending_account = account(Some("oaiapp_pending")).registration();
                let persisted = std::cell::RefCell::new(original.clone());
                let generation = std::cell::Cell::new(4);
                let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 4 };
                let access = std::cell::RefCell::new(Some(Access {
                    client_id: request.client_id.clone(), generation: 4,
                    token: "old-access".into(), until: Instant::now() + Duration::from_secs(3600),
                }));
                let remembered = std::cell::Cell::new(false);
                let credential_saved = std::cell::Cell::new(false);
                let mut writes = 0;
                let mut stores = 0;
                let result = refresh_with(&active, &original,
                    || {
                        assert!(status_from_storage(Ok(persisted.borrow().clone()), Ok((true, true)), active.lock().unwrap().as_deref(), None).connected);
                        let mut response = json!({ "access_token": "new-access", "expires_in": 3600 });
                        if rotated { response["refresh_token"] = json!("new-refresh"); }
                        if let Some(scope) = scope { response["scope"] = json!(scope); }
                        let tokens: Tokens = serde_json::from_value(response).unwrap();
                        std::future::ready(Ok(tokens))
                    },
                    |next| {
                        writes += 1;
                        if writes == 2 {
                            assert!(!rotated || credential_saved.get(), "rotation must be saved before clearing recovery");
                            assert_eq!(next.plan_enabled, granted, "scope={scope:?}");
                            assert!(!next.session_needs_reconnect);
                            assert_eq!(generation.get(), if granted { 4 } else { 5 }, "fence old requests before fallible storage");
                            if !finish_ok { return Err("disk full".into()); }
                        }
                        persisted.replace(next.clone()); Ok(())
                    },
                    |key, value| {
                        stores += 1;
                        assert_eq!((key, value), (REFRESH_TOKEN_KEY, "new-refresh"));
                        assert!(persisted.borrow().session_needs_reconnect);
                        assert_eq!(generation.get(), if granted { 4 } else { 5 }, "lost scope invalidates before credential writes");
                        if store_ok { credential_saved.set(true); Ok(()) } else { Err("keystore locked".into()) }
                    },
                    || { access.replace(None); generation.set(generation.get() + 1); },
                    |tokens| {
                        assert!(!persisted.borrow().session_needs_reconnect);
                        assert!(persisted.borrow().plan_enabled, "never cache a token without the plan grant");
                        assert_eq!(tokens.access_token, "new-access");
                        remembered.set(true);
                    },
                ).await;
                let committed = (!rotated || store_ok) && finish_ok;
                assert!(active.lock().unwrap().is_none());
                if committed && granted { assert_eq!(result, Ok("new-access".into())); }
                else if committed {
                    assert_eq!(result, Err(t("Plan usage wasn't allowed at sign-in. Reconnect and allow it to chat on your ChatGPT plan.")));
                } else { assert!(result.is_err()); }
                assert_eq!(remembered.get(), committed && granted);
                assert_eq!(generation.get(), if committed && granted { 4 } else { 5 });
                assert_eq!(check_session_generation(&request, generation.get()).is_ok(), committed && granted);
                assert_eq!(access.borrow().is_some(), committed && granted);
                assert_eq!(stores, usize::from(rotated));
                let mut expected = original.clone();
                expected.session_needs_reconnect = !committed;
                if committed { expected.plan_enabled = granted; }
                assert_eq!(*persisted.borrow(), expected, "refresh preserves identity, host, welcome and other registrations");
                let restarted: Account = serde_json::from_slice(&serde_json::to_vec(&*persisted.borrow()).unwrap()).unwrap();
                let status = status_for(&restarted, true, true);
                assert_eq!(status.connected, committed);
                assert_eq!(status.plan_enabled, !committed || granted);
                assert!(!status.show_welcome && !status.can_switch_accounts);
                assert_eq!(require_selected_client(&restarted, &request.client_id).is_ok(), committed && granted);
                assert!(require_selected_client(&restarted, "oaiapp_saved").is_err());
                if committed && !granted {
                    let consent = query(&authorize_url(&restarted, &Attempt::new(1455), None));
                    assert_eq!(consent["client_id"], request.client_id);
                    assert_eq!(consent["prompt"], "consent", "recover permission on the same registration");
                }
            }
        }
      }
    }
}

#[tokio::test]
async fn refresh_cannot_add_plan_permission_to_an_account_without_a_prior_grant() {
    for (scope, _) in sign_in_scope_cases() {
        let active = std::sync::Mutex::new(None);
        let original = Account { plan_enabled: false, ..registered_account() };
        let mut tokens = new_account_tokens(); tokens.scope = scope.map(str::to_string);
        let result = refresh_with(&active, &original,
            || std::future::ready(Ok(tokens)),
            |_| panic!("no metadata changes without a prior grant"),
            |_, _| panic!("no credential changes without a prior grant"),
            || panic!("no session change"),
            |_| panic!("no new access"),
        ).await;
        assert_eq!(result, Err(t("Plan usage wasn't allowed at sign-in. Reconnect and allow it to chat on your ChatGPT plan.")));
        assert!(active.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn malformed_supplied_scope_is_not_omission_and_keeps_refresh_recovery_guarded() {
    for scope in [Value::Null, json!(false), json!(7), json!([PLAN_SCOPE]), json!({ "scope": PLAN_SCOPE })] {
        let active = std::sync::Mutex::new(None);
        let original = registered_account();
        let mut saved = None;
        let invalidated = std::cell::Cell::new(false);
        let result = refresh_with(&active, &original,
            || {
                let parsed = serde_json::from_value::<Tokens>(json!({ "access_token": "new-access",
                    "refresh_token": "new-refresh", "scope": scope }));
                assert!(parsed.is_err(), "a supplied scope must be a string");
                std::future::ready(parsed.map_err(|_| RefreshFailure::InvalidResponse))
            },
            |next| { saved = Some(next.clone()); Ok(()) },
            |_, _| panic!("a malformed response cannot commit credentials"),
            || invalidated.set(true),
            |_| panic!("a malformed response cannot cache access"),
        ).await;
        assert_eq!(result, Err(t("ChatGPT didn't accept the sign-in. Reconnect in Settings.")));
        assert!(active.lock().unwrap().is_none());
        assert!(invalidated.get());
        let saved = saved.unwrap();
        assert!(saved.session_needs_reconnect);
        assert_eq!(saved.registration(), original.registration());
        assert!(!status_for(&saved, true, true).connected);
    }
}

#[tokio::test]
async fn malformed_success_keeps_the_session_blocked_because_rotation_may_have_happened() {
    let active = std::sync::Mutex::new(None);
    let original = registered_account();
    let mut saved = None;
    let invalidated = std::cell::Cell::new(false);
    assert!(refresh_with(&active, &original, || std::future::ready(Err(RefreshFailure::InvalidResponse)),
        |next| { saved = Some(next.clone()); Ok(()) },
        |_, _| panic!("a malformed reply provides no credential to store"),
        || invalidated.set(true), |_| panic!("no verified token response"),
    ).await.is_err());
    assert!(active.lock().unwrap().is_none());
    assert!(invalidated.get() && saved.unwrap().session_needs_reconnect);
}

#[test]
fn cancelled_refresh_leaves_durable_recovery_and_invalidates_the_previous_access() {
  for response_ready_before_stop in [false, true] {
    let active = std::sync::Mutex::new(None);
    let original = registered_account();
    let persisted = std::cell::RefCell::new(original.clone());
    let invalidated = std::cell::Cell::new(false);
    let (response, receive) = tokio::sync::oneshot::channel::<Tokens>();
    let mut refreshed = false;
    let mut refresh = Box::pin(recover_authentication_with(401, &mut refreshed,
        || refresh_with(&active, &original,
            || async move { receive.await.map_err(|_| RefreshFailure::Unreachable) },
            |next| { persisted.replace(next.clone()); Ok(()) },
            |_, _| panic!("a cancelled exchange cannot commit credentials"),
            || invalidated.set(true), |_| panic!("a cancelled exchange cannot cache access"),
        ),
        || -> std::future::Ready<Result<(), String>> { panic!("no second rejection arrived") },
    ));
    let mut context = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(std::future::Future::poll(refresh.as_mut(), &mut context).is_pending());
    assert!(persisted.borrow().session_needs_reconnect);
    assert!(status_from_storage(Ok(persisted.borrow().clone()), Ok((true, true)), active.lock().unwrap().as_deref(), None).connected);
    assert!(!invalidated.get());
    if response_ready_before_stop {
        let mut tokens = new_account_tokens(); tokens.scope = Some(String::new());
        response.send(tokens).unwrap();
    }
    drop(refresh);
    assert!(active.lock().unwrap().is_none());
    assert!(invalidated.get());
    assert!(!status_for(&persisted.borrow(), true, true).connected);
    assert_eq!(persisted.borrow().registration(), original.registration(), "unobserved responses never replace account metadata");
  }
}

#[tokio::test]
async fn an_already_ended_session_cannot_start_a_refresh_or_mask_its_persisted_marker() {
    for explicit in [false, true] {
        let active = std::sync::Mutex::new(None);
        let account = Account { session_needs_reconnect: true, explicitly_disconnected: explicit, ..registered_account() };
        assert!(refresh_with(&active, &account,
            || -> std::future::Ready<Result<Tokens, RefreshFailure>> { panic!("never retry the unusable token") },
            |_| panic!("do not overwrite the terminal marker"),
            |_, _| panic!("no credential mutation"),
            || panic!("the previous session was already invalidated"),
            |_| panic!("no new access"),
        ).await.is_err());
        assert!(active.lock().unwrap().is_none());
        assert!(!status_from_storage(Ok(account), Ok((true, true)), active.lock().unwrap().as_deref(), None).connected);
    }
}


#[tokio::test]
async fn disconnect_attempts_both_deletions_and_clears_access_for_every_failure_combination() {
    for remote_ok in [true, false] {
        for refresh_ok in [true, false] {
            for id_ok in [true, false] {
                for file_ok in [true, false] {
                    let account = registered_account();
                    let mut deleted = Vec::new();
                    let access_present = std::cell::Cell::new(true);
                    let mut saved = None;
                    let result = disconnect_with(
                        Some("test-refresh"), &account,
                        |refresh, client| {
                            assert_eq!((refresh, client), ("test-refresh", "oaiapp_original"));
                            std::future::ready(remote_ok)
                        },
                        |key| {
                            deleted.push(key.to_string());
                            if if key == REFRESH_TOKEN_KEY { refresh_ok } else { id_ok } {
                                Ok(())
                            } else { Err("test keystore deletion failed".into()) }
                        },
                        || access_present.set(false),
                        |next| {
                            assert!(refresh_ok && id_ok, "switching cannot be enabled while credentials remain");
                            if file_ok { saved = Some(next.clone()); Ok(()) } else { Err("disk full".into()) }
                        },
                    ).await;
                    assert_eq!(deleted, [REFRESH_TOKEN_KEY, ID_TOKEN_KEY]);
                    assert!(!access_present.get(), "access is cleared even when either deletion or account save fails");
                    assert_eq!(result.revocation_confirmed, Some(remote_ok));
                    assert_eq!(result.local_cleanup_succeeded, refresh_ok && id_ok && file_ok);
                    if let Some(saved) = saved {
                        assert!(saved.explicitly_disconnected);
                        assert_eq!(saved.registration(), account.registration(), "sign-out retains the subject/client binding");
                        assert_eq!(saved.host_id, account.host_id);
                        assert!(status_for(&saved, false, false).can_switch_accounts);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn disconnect_distinguishes_no_refresh_from_a_missing_client() {
    for (refresh, client, expected) in [
        (None, None, None),
        (None, Some("test-client"), None),
        (Some("test-refresh"), None, Some(false)),
    ] {
        let account = account(client);
        let mut deleted = Vec::new();
        let access_present = std::cell::Cell::new(true);
        let result = disconnect_with(
            refresh, &account,
            |_, _| -> std::future::Ready<bool> { panic!("revocation needs both a refresh token and a client") },
            |key| { deleted.push(key.to_string()); Ok(()) },
            || access_present.set(false),
            |_| Ok(()),
        ).await;
        assert_eq!(result.revocation_confirmed, expected);
        assert!(result.local_cleanup_succeeded);
        assert_eq!(deleted, [REFRESH_TOKEN_KEY, ID_TOKEN_KEY]);
        assert!(!access_present.get());
    }
}

#[tokio::test]
async fn account_save_failure_can_be_retried_after_restart_without_any_credentials() {
    let original = registered_account();
    let mut stored = serde_json::to_vec(&original).unwrap();
    let failed = disconnect_with(Some("refresh"), &original, |_, _| std::future::ready(true),
        |_| Ok(()), || {}, |_| Err("disk full".into())).await;
    assert!(!failed.local_cleanup_succeeded);
    let reloaded: Account = serde_json::from_slice(&stored).unwrap();
    let status = status_for(&reloaded, false, false);
    assert!(status.needs_reconnect && !status.has_saved_credentials && !status.can_switch_accounts);
    let result = disconnect_with(None, &reloaded,
        |_, _| -> std::future::Ready<bool> { panic!("there is no refresh token to revoke") },
        |_| Ok(()), || {}, |next| { stored = serde_json::to_vec(next).unwrap(); Ok(()) }).await;
    assert_eq!(result.revocation_confirmed, None);
    assert!(result.local_cleanup_succeeded);
    let reloaded: Account = serde_json::from_slice(&stored).unwrap();
    assert_eq!(reloaded.registration(), original.registration());
    assert!(status_for(&reloaded, false, false).can_switch_accounts);
}

#[tokio::test]
async fn disconnecting_an_expired_session_preserves_recovery_until_cleanup_is_fully_persisted() {
    for credentials_ok in [false, true] {
        for metadata_ok in [false, true] {
            let original = Account { session_needs_reconnect: true, ..registered_account() };
            let mut saved = original.clone();
            let result = disconnect_with(Some("unusable-refresh"), &original,
                |_, _| std::future::ready(false),
                |_| if credentials_ok { Ok(()) } else { Err("keystore locked".into()) },
                || {},
                |next| {
                    if metadata_ok { saved = next.clone(); Ok(()) } else { Err("disk full".into()) }
                },
            ).await;
            let restarted: Account = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
            let status = status_for(&restarted, !credentials_ok, !credentials_ok);
            assert!(!status.connected && status.needs_reconnect);
            assert!(restarted.session_needs_reconnect);
            assert_eq!(restarted.registration(), original.registration());
            assert_eq!(result.local_cleanup_succeeded, credentials_ok && metadata_ok);
            assert_eq!(status.can_switch_accounts, credentials_ok && metadata_ok);
            assert_eq!(require_signed_out(&restarted, Ok((!credentials_ok, !credentials_ok))).is_ok(), credentials_ok && metadata_ok);
        }
    }
}

#[test]
fn disconnect_result_serializes_only_the_two_outcomes() {
    let result = DisconnectResult { revocation_confirmed: None, local_cleanup_succeeded: false };
    assert_eq!(serde_json::to_value(result).unwrap(), json!({
        "revocationConfirmed": null,
        "localCleanupSucceeded": false,
    }));
}

#[test]
fn speech_asks_for_openais_voice_and_nothing_else() {
    let body = speech_body("Bonjour, ça va ?");
    assert_eq!(body["model"], SPEECH_MODEL);
    assert_eq!(body["voice"], SPEECH_VOICE);
    assert_eq!(body["input"], "Bonjour, ça va ?");
    assert_eq!(body["response_format"], "mp3");
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["model", "voice", "input", "instructions", "response_format"]);
}

#[test]
fn finished_output_items_are_kept_for_the_tool_loop() {
    let event = r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"switch_browser_tab","namespace":"coucou","arguments":"{\"tab_id\":\"1.2\",\"position\":null,\"title_contains\":null}"}}"#;
    let StreamEvent::Item(item) = parse_event(event) else { panic!("an item") };
    let call = tool_call(&item).unwrap();
    assert_eq!(call.call_id, "call_1");
    assert_eq!(call.name, "switch_browser_tab");
    assert!(call.arguments.contains("1.2"));
    assert_eq!(parse_event(r#"{"type":"response.output_item.done"}"#), StreamEvent::Other);
}

#[test]
fn only_function_calls_become_tool_calls() {
    assert!(tool_call(&json!({ "type": "message", "content": [] })).is_none());
    assert!(tool_call(&json!({ "type": "reasoning", "encrypted_content": "x" })).is_none());
    assert!(tool_call(&json!({ "type": "function_call", "name": "open_app" })).is_none(), "no call id");
    let dotted = tool_call(&json!({ "type": "function_call", "call_id": "c", "name": "coucou.open_app", "arguments": "{}" })).unwrap();
    assert_eq!(dotted.name, "open_app");
}

#[test]
fn each_round_carries_the_tools_and_what_came_before() {
    let base = request_body(LUNA, "low", "x", &[], json!("open my second tab"));
    let call = json!({ "type": "function_call", "call_id": "c1", "name": "list_browser_tabs", "arguments": "{}" });
    let output = json!({ "type": "function_call_output", "call_id": "c1", "output": "{\"tabs\":[]}" });
    let body = with_tools(&base, &[call.clone(), output.clone()], false);
    assert_eq!(body["tools"][0]["type"], "namespace");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(body.get("tool_choice").is_none());
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[input.len() - 2], call);
    assert_eq!(input[input.len() - 1], output);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    // The last round answers in words.
    assert_eq!(with_tools(&base, &[], true)["tool_choice"], "none");
    // The question's own body is untouched.
    assert!(base.get("tools").is_none());
}

fn registered_account() -> Account {
    Account {
        sub: Some("original-user".into()), plan_enabled: true, welcomed: true,
        ..account(Some("oaiapp_original"))
    }
}

fn new_account_tokens() -> Tokens {
    Tokens {
        access_token: "new-access".into(), refresh_token: Some("new-refresh".into()),
        id_token: Some("new-id-hint".into()), expires_in: Some(3600), scope: Some(SCOPE.into()),
    }
}

fn new_account_claims() -> IdClaims {
    IdClaims {
        iss: ISSUER.into(), aud: json!("oaiapp_new"), azp: None, exp: 4_102_444_800,
        sub: "new-user".into(), email: Some("new@example.com".into()), nonce: Some("nonce".into()),
    }
}

#[test]
fn legacy_accounts_keep_their_exact_registration_and_need_explicit_cleanup_before_switching() {
    let legacy = json!({ "hostId": "old-host", "clientId": "oaiapp_old", "sub": "old-user",
        "email": "same@example.com", "planEnabled": true, "welcomed": true });
    let account: Account = serde_json::from_value(legacy).unwrap();
    assert_eq!(account.host_id, "old-host");
    assert_eq!(account.client_id.as_deref(), Some("oaiapp_old"));
    assert_eq!(account.sub.as_deref(), Some("old-user"));
    assert!(account.saved_accounts.is_empty() && account.pending_account.is_none());
    assert!(!account.session_needs_reconnect, "legacy usable sessions retain their behavior until refreshed");
    assert!(!status_for(&account, false, false).can_switch_accounts);
    assert!(require_signed_out(&account, Ok((false, false))).is_err());
}

#[test]
fn switching_requires_explicit_disconnect_and_error_free_absence_of_both_credentials() {
    for explicit in [false, true] {
        for refresh in [false, true] {
            for id in [false, true] {
                let mut account = registered_account();
                account.explicitly_disconnected = explicit;
                assert_eq!(require_signed_out(&account, Ok((refresh, id))).is_ok(), explicit && !refresh && !id);
                assert_eq!(status_for(&account, refresh, id).can_switch_accounts, explicit && !refresh && !id);
            }
        }
    }
    let account = Account { explicitly_disconnected: true, ..registered_account() };
    assert!(require_signed_out(&account, Err("keystore locked".into())).is_err());
}

#[test]
fn a_pending_new_registration_is_separate_and_retries_its_issued_client_after_restart() {
    let mut original = Account { explicitly_disconnected: true, ..registered_account() };
    let original_identity = original.registration();
    let mut candidate = new_account_candidate(&original);
    let request = query(&authorize_url(&candidate, &Attempt::new(1455), None));
    assert_eq!(request["client_id"], DYNAMIC_CLIENT);
    assert_eq!(request["ext_agent_host_id"], original.host_id);
    assert!(!request.contains_key("login_hint") && !request.contains_key("id_token_hint"));
    let attempt = Attempt::new(1455);
    let callback = format!("/auth/callback?state={}&code=new-code&client_id=oaiapp_new", attempt.state);
    let mut saved = Vec::new();
    prepare_code_exchange(&mut candidate, &attempt, &callback, |record| {
        retain_pending_account(&mut original, record, |next| {
            saved = serde_json::to_vec(next).unwrap();
            Ok(())
        })
    }).unwrap();
    assert_eq!(original.registration(), original_identity, "exchange failure cannot replace the selected identity");
    let restarted: Account = serde_json::from_slice(&saved).unwrap();
    assert!(status_for(&restarted, false, false).has_pending_account);
    let retry = new_account_candidate(&restarted);
    assert_eq!(retry.client_id.as_deref(), Some("oaiapp_new"));
    assert!(retry.sub.is_none() && retry.email.is_none() && !retry.plan_enabled && !retry.welcomed);
    let q = query(&authorize_url(&retry, &Attempt::new(54321), None));
    assert_eq!(q["client_id"], "oaiapp_new");
    assert!(!q.contains_key("agent_name_hint") && !q.contains_key("login_hint"));
    let serialized = String::from_utf8(saved).unwrap();
    assert!(!serialized.contains("new-code"), "codes and credentials never enter the registration file");
}

#[test]
fn cancelled_or_unpersisted_new_registrations_leave_the_old_account_unchanged() {
    let mut original = Account { explicitly_disconnected: true, ..registered_account() };
    let before = original.clone();
    let mut candidate = new_account_candidate(&original);
    let attempt = Attempt::new(1455);
    for query in [format!("state={}&error=access_denied", attempt.state),
                  format!("state={}&code=abc&client_id=oaiapp_new", attempt.state)] {
        assert!(prepare_code_exchange(&mut candidate, &attempt, &format!("/auth/callback?{query}"), |record| {
            retain_pending_account(&mut original, record, |_| Err("disk full".into()))
        }).is_err());
        assert_eq!(original, before);
        assert!(candidate.client_id.is_none());
    }
}

#[test]
fn ordinary_reconnect_rejects_a_different_subject_before_any_persistence_or_credential_write() {
    for expired in [false, true] {
        let account = Account { session_needs_reconnect: expired, ..registered_account() };
        let mut claims = new_account_claims();
        claims.aud = json!("oaiapp_original");
        let result = store_verified_sign_in(&account, &account, "oaiapp_original", &claims, &new_account_tokens(), false,
            |_| panic!("mismatched identity must not be persisted"),
            |_, _| panic!("mismatched identity must not write credentials"),
            |_| panic!("mismatched identity must not become active"));
        assert!(result.is_err());
    }
}

#[test]
fn reconnect_clears_expiry_only_after_refresh_storage_and_the_final_metadata_write() {
    for preflight_ok in [false, true] {
        for refresh_ok in [false, true] {
            for final_write_ok in [false, true] {
                let original = Account { session_needs_reconnect: true, ..registered_account() };
                let mut claims = new_account_claims(); claims.sub = "original-user".into();
                let persisted = std::cell::RefCell::new(original.clone());
                let remembered = std::cell::Cell::new(false);
                let rejected = std::sync::Mutex::new(Some("oaiapp_original".to_string()));
                let refresh_present = std::cell::Cell::new(true);
                let mut writes = 0;
                let result = store_verified_sign_in(&original, &original, "oaiapp_original", &claims, &new_account_tokens(), false,
                    |next| {
                        writes += 1;
                        if (writes == 1 && !preflight_ok) || (writes == 2 && !final_write_ok) {
                            return Err("disk full".into());
                        }
                        persisted.replace(next.clone()); Ok(())
                    },
                    |key, _| {
                        assert!(persisted.borrow().session_needs_reconnect, "surviving expired credentials must stay blocked until replaced");
                        if key == ID_TOKEN_KEY { return Err("optional hint too long".into()); }
                        assert_eq!(key, REFRESH_TOKEN_KEY);
                        if refresh_ok { refresh_present.set(true); Ok(()) } else { Err("keystore locked".into()) }
                    },
                    |_| {
                        assert!(!persisted.borrow().session_needs_reconnect);
                        remembered.set(true);
                        *rejected.lock().unwrap() = None;
                    },
                );
                let success = preflight_ok && refresh_ok && final_write_ok;
                assert_eq!(result.is_ok(), success);
                assert_eq!(remembered.get(), success);
                assert_eq!(rejected.lock().unwrap().is_none(), success);
                let restarted: Account = serde_json::from_slice(&serde_json::to_vec(&*persisted.borrow()).unwrap()).unwrap();
                assert_eq!(restarted.client_id, original.client_id);
                assert_eq!(restarted.sub, original.sub);
                assert_eq!(restarted.session_needs_reconnect, !success);
                let status = status_from_storage(Ok(restarted), Ok((refresh_present.get(), true)), None, rejected.lock().unwrap().as_deref());
                assert_eq!(status.connected, success);
                assert_eq!(status.needs_reconnect, !success);
                assert!(!status.can_switch_accounts);
            }
        }
    }
}

#[test]
fn a_validated_new_account_gets_fresh_flags_and_never_reuses_another_registration() {
    let original = Account { explicitly_disconnected: true, ..registered_account() };
    let candidate = Account { host_id: original.host_id.clone(), client_id: Some("oaiapp_new".into()), ..Default::default() };
    let persisted = std::cell::RefCell::new(None);
    let mut credentials = std::collections::HashMap::new();
    let access = std::cell::RefCell::new(None);
    let next = store_verified_sign_in(&original, &candidate, "oaiapp_new", &new_account_claims(), &new_account_tokens(), true,
        |record| { persisted.replace(Some(record.clone())); Ok(()) },
        |key, value| {
            assert_eq!(persisted.borrow().as_ref().unwrap().sub.as_deref(), Some("new-user"), "identity is stored before credentials");
            credentials.insert(key.to_string(), value.to_string()); Ok(())
        },
        |tokens| { access.replace(Some(tokens.access_token.clone())); }).unwrap();
    assert_eq!(next.host_id, original.host_id);
    assert_eq!(next.client_id.as_deref(), Some("oaiapp_new"));
    assert_eq!(next.email.as_deref(), Some("new@example.com"));
    assert_eq!(next.sub.as_deref(), Some("new-user"));
    assert!(!next.welcomed && next.plan_enabled && !next.explicitly_disconnected);
    assert!(next.pending_account.is_none());
    assert_eq!(next.saved_accounts, vec![original.registration().unwrap()]);
    assert!(status_for(&next, true, true).show_welcome);
    assert_eq!(credentials[REFRESH_TOKEN_KEY], "new-refresh");
    assert_eq!(credentials[ID_TOKEN_KEY], "new-id-hint");
    assert_eq!(access.borrow().as_deref(), Some("new-access"));
    let text = serde_json::to_string(&next).unwrap();
    for secret in ["new-refresh", "new-id-hint", "new-access"] { assert!(!text.contains(secret)); }
}

#[test]
fn a_new_account_persistence_failure_writes_no_credentials_or_access() {
    let original = Account { explicitly_disconnected: true, ..registered_account() };
    let candidate = Account { host_id: original.host_id.clone(), client_id: Some("oaiapp_new".into()), ..Default::default() };
    let result = store_verified_sign_in(&original, &candidate, "oaiapp_new", &new_account_claims(), &new_account_tokens(), true,
        |_| Err("disk full".into()), |_, _| panic!("do not pair new credentials with old metadata"),
        |_| panic!("a failed switch must not cache access"));
    assert_eq!(result, Err("disk full".into()));
}

#[test]
fn saved_registrations_are_distinct_even_when_their_emails_match_and_can_be_selected_again() {
    let original = Account { explicitly_disconnected: true, ..registered_account() };
    let mut other = original.registration().unwrap();
    other.client_id = "oaiapp_other".into(); other.sub = Some("other-user".into());
    other.welcomed = false; other.plan_enabled = false;
    let next = original.select_registration(&other);
    assert_eq!(next.saved_accounts, vec![original.registration().unwrap()]);
    assert_eq!(next.email, original.email);
    assert_eq!(next.sub, other.sub);
    let restored = next.select_registration(&next.saved_accounts[0]);
    assert_eq!(restored.registration(), original.registration());
    assert_eq!(restored.saved_accounts, vec![other]);
}

#[test]
fn welcome_acknowledgement_cannot_modify_a_signed_out_or_different_registration() {
    let account = registered_account();
    for (client, connected) in [(None, true), (Some("oaiapp_other"), true), (Some("oaiapp_original"), false)] {
        mark_welcomed_for(&account, client, connected, |_| panic!("stale welcome must not write an account record")).unwrap();
    }
    let mut unseen = account.clone(); unseen.welcomed = false;
    mark_welcomed_for(&unseen, Some("oaiapp_original"), true, |saved| {
        assert!(saved.welcomed); assert_eq!(saved.sub, unseen.sub); Ok(())
    }).unwrap();
}

#[test]
fn failed_atomic_account_writes_preserve_the_previous_registration_file() {
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-account-{}", random(12)));
    let path = dir.join("chatgpt.json");
    let original = registered_account();
    save_account_to(&path, &original).unwrap();
    let blocked = path.with_extension("json.tmp");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("keep"), b"blocked").unwrap();
    let mut changed = original.clone(); changed.explicitly_disconnected = true;
    assert!(save_account_to(&path, &changed).is_err());
    let retained: Account = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(retained, original);
    std::fs::remove_dir_all(&blocked).unwrap();
    save_account_to(&path, &changed).unwrap();
    assert_eq!(serde_json::from_slice::<Account>(&std::fs::read(path).unwrap()).unwrap(), changed);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_account_rename_preserves_the_target_and_removes_its_temporary_file() {
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-rename-{}", random(12)));
    let path = dir.join("chatgpt.json");
    std::fs::create_dir_all(&path).unwrap();
    let sentinel = path.join("keep");
    std::fs::write(&sentinel, b"existing target contents").unwrap();

    assert!(save_account_to(&path, &registered_account()).is_err());

    assert!(path.is_dir());
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"existing target contents");
    assert!(!path.with_extension("json.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fresh_account_record_and_its_directory_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-private-{}", random(12)));
    let path = dir.join("chatgpt.json");
    let account = registered_account();

    save_account_to(&path, &account).unwrap();

    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(read_account_from(&path).unwrap(), account);
    assert!(!path.with_extension("json.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn saving_an_account_closes_lax_directory_and_temporary_file_permissions() {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-lax-{}", random(12)));
    let path = dir.join("chatgpt.json");
    let temp = path.with_extension("json.tmp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&path, b"previous account record").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&temp, b"a temporary record left by an older version").unwrap();
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o666)).unwrap();
    let mut stale_handle = std::fs::File::open(&temp).unwrap();
    let mut account = registered_account();
    account.saved_accounts.push(Registration {
        client_id: "oaiapp_saved".into(), email: Some("saved@example.com".into()),
        sub: Some("saved-user".into()), plan_enabled: true, welcomed: false,
    });
    account.pending_account = account.registration();
    account.session_needs_reconnect = true;

    save_account_to(&path, &account).unwrap();

    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(read_account_from(&path).unwrap(), account);
    assert!(!temp.exists());
    let mut stale_content = String::new();
    stale_handle.read_to_string(&mut stale_content).unwrap();
    assert_eq!(stale_content, "a temporary record left by an older version",
        "an already-open stale temp must not receive the new account metadata");
    drop(stale_handle);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn saving_an_account_does_not_follow_a_stale_temporary_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-temp-link-{}", random(12)));
    let path = dir.join("chatgpt.json");
    let temp = path.with_extension("json.tmp");
    let original = registered_account();
    save_account_to(&path, &original).unwrap();
    let target = dir.join("unrelated.json");
    std::fs::write(&target, b"keep this unrelated file").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&target, &temp).unwrap();
    let mut changed = original;
    changed.explicitly_disconnected = true;

    save_account_to(&path, &changed).unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"keep this unrelated file");
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o644);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(read_account_from(&path).unwrap(), changed);
    assert!(!temp.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_refresh_credential_write_failure_keeps_the_new_identity_recoverable_without_caching_access() {
    let original = Account { explicitly_disconnected: true, ..registered_account() };
    let candidate = Account { host_id: original.host_id.clone(), client_id: Some("oaiapp_new".into()), ..Default::default() };
    let mut saved = None;
    let mut written = Vec::new();
    let result = store_verified_sign_in(&original, &candidate, "oaiapp_new", &new_account_claims(), &new_account_tokens(), true,
        |account| { saved = Some(account.clone()); Ok(()) },
        |key, _| { written.push(key.to_string()); Err("keystore locked".into()) },
        |_| panic!("failed credential storage cannot cache access"));
    assert_eq!(result, Err("keystore locked".into()));
    assert_eq!(written, [REFRESH_TOKEN_KEY]);
    let saved = saved.unwrap();
    assert_eq!(saved.sub.as_deref(), Some("new-user"));
    assert_eq!(saved.saved_accounts, vec![original.registration().unwrap()]);
    let status = status_for(&saved, false, false);
    assert!(!status.connected && status.needs_reconnect && !status.can_switch_accounts);
    let request = query(&authorize_url(&saved, &Attempt::new(1455), None));
    assert_eq!(request["client_id"], "oaiapp_new", "retry reuses the now-validated registration");
}

fn sign_in_scope_cases() -> [(Option<&'static str>, bool); 6] {
    [
        (None, true),
        (Some(""), false),
        (Some("openid profile email offline_access resource.invoke"), false),
        (Some(SCOPE), true),
        (Some("openid chatgpt.tokens.use.direct.extra"), false),
        (Some("openid CHATGPT.TOKENS.USE.DIRECT"), false),
    ]
}

#[test]
fn a_new_sign_in_uses_omitted_scope_without_inheriting_another_accounts_flags() {
    for (scope, granted) in sign_in_scope_cases() {
        for previous_plan in [false, true] {
            let original = Account { plan_enabled: previous_plan, explicitly_disconnected: true, ..registered_account() };
            let candidate = Account { host_id: original.host_id.clone(), client_id: Some("oaiapp_new".into()), ..Default::default() };
            // Deserialize the wire shape as well: an absent field differs from
            // an explicitly empty or reduced grant.
            let mut response = json!({ "access_token": "new-access", "refresh_token": "new-refresh",
                "id_token": "new-id-hint", "expires_in": 3600 });
            if let Some(scope) = scope { response["scope"] = json!(scope); }
            let tokens: Tokens = serde_json::from_value(response).unwrap();
            let mut claims = new_account_claims(); claims.email = None;
            let mut saved = None;
            let next = store_verified_sign_in(&original, &candidate, "oaiapp_new", &claims, &tokens, true,
                |record| { saved = Some(record.clone()); Ok(()) }, |_, _| Ok(()), |_| {}).unwrap();
            assert_eq!(saved.as_ref(), Some(&next));
            assert_eq!(next.client_id, candidate.client_id);
            assert_eq!(next.sub.as_deref(), Some("new-user"));
            assert_eq!(next.email, None, "no stale email from the previous registration");
            assert_eq!(next.plan_enabled, granted, "scope={scope:?}, previous_plan={previous_plan}");
            assert!(!next.welcomed, "another account's welcome cannot carry over");
            assert_eq!(status_for(&next, true, true).show_welcome, granted);
            assert_eq!(require_selected_client(&next, "oaiapp_new").is_ok(), granted);
            assert_eq!(next.saved_accounts, vec![original.registration().unwrap()],
                "the old flags and identity stay with their own registration");
        }
    }
}

#[test]
fn reconnect_uses_the_new_scope_result_and_preserves_only_its_own_welcome() {
    for (scope, granted) in sign_in_scope_cases() {
        for previous_plan in [false, true] {
            for welcomed in [false, true] {
                let mut original = Account { plan_enabled: previous_plan, welcomed, ..registered_account() };
                let other = Registration { client_id: "oaiapp_other".into(), sub: Some("other-user".into()),
                    email: None, plan_enabled: !previous_plan, welcomed: !welcomed };
                original.saved_accounts.push(other.clone());
                let request = query(&authorize_url(&original, &Attempt::new(1455), None));
                assert_eq!(request["scope"], SCOPE);
                assert_eq!(request.get("prompt").map(String::as_str), (!previous_plan).then_some("consent"));
                let mut tokens = new_account_tokens(); tokens.scope = scope.map(str::to_string);
                let mut claims = new_account_claims();
                claims.aud = json!("oaiapp_original"); claims.sub = "original-user".into();
                let next = store_verified_sign_in(&original, &original, "oaiapp_original", &claims, &tokens, false,
                    |_| Ok(()), |_, _| Ok(()), |_| {}).unwrap();
                assert_eq!(next.client_id, original.client_id);
                assert_eq!(next.sub, original.sub);
                assert_eq!(next.plan_enabled, granted, "scope={scope:?}, previous_plan={previous_plan}");
                assert_eq!(next.welcomed, welcomed);
                assert_eq!(status_for(&next, true, true).show_welcome, granted && !welcomed);
                assert_eq!(require_selected_client(&next, "oaiapp_original").is_ok(), granted);
                assert_eq!(next.saved_accounts, vec![other]);
            }
        }
    }
}

#[test]
fn reconnecting_the_same_subject_keeps_its_welcome_and_registration() {
    let original = registered_account();
    let mut claims = new_account_claims(); claims.sub = "original-user".into();
    let next = store_verified_sign_in(&original, &original, "oaiapp_original", &claims, &new_account_tokens(), false,
        |_| Ok(()), |_, _| Ok(()), |_| {}).unwrap();
    assert_eq!(next.client_id, original.client_id);
    assert_eq!(next.sub, original.sub);
    assert!(next.welcomed && next.saved_accounts.is_empty());
    assert!(!status_for(&next, true, true).show_welcome);
}

#[test]
fn missing_required_tokens_cannot_change_selection_or_write_any_credentials() {
    let original = registered_account();
    let mut claims = new_account_claims(); claims.sub = "original-user".into();
    for missing_refresh in [true, false] {
        let mut tokens = new_account_tokens();
        if missing_refresh { tokens.refresh_token = None; } else { tokens.id_token = None; }
        assert!(store_verified_sign_in(&original, &original, "oaiapp_original", &claims, &tokens, false,
            |_| panic!("incomplete tokens must not change the account"),
            |_, _| panic!("incomplete tokens must not change stored credentials"),
            |_| panic!("incomplete tokens must not cache access")).is_err());
    }
}

#[test]
fn a_new_registration_cannot_replace_an_existing_registration_with_the_same_client_id() {
    let mut original = registered_account();
    let duplicate = original.clone();
    assert!(retain_pending_account(&mut original, &duplicate, |_| panic!("do not replace an existing registration")).is_err());
    assert_eq!(original, duplicate);
}

#[test]
fn only_missing_account_files_default_to_a_new_installation() {
    let dir = std::env::temp_dir().join(format!("coucou-chatgpt-read-{}", random(12)));
    let path = dir.join("chatgpt.json");
    assert_eq!(read_account_from(&path).unwrap(), Account::default());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, b"{corrupted existing registration}").unwrap();
    assert!(read_account_from(&path).is_err(), "corruption cannot erase an existing registration");
    assert!(read_account_from(&dir).is_err(), "an I/O error cannot become an empty account");
    assert_eq!(std::fs::read(path).unwrap(), b"{corrupted existing registration}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn in_flight_requests_cannot_take_credentials_from_a_different_selected_registration() {
    let original = registered_account();
    assert!(require_selected_client(&original, "oaiapp_original").is_ok());
    let mut changed = original.clone(); changed.client_id = Some("oaiapp_new".into());
    assert!(require_selected_client(&changed, "oaiapp_original").is_err(), "a late 401 from A cannot refresh B");
    let mut signed_out = original.clone(); signed_out.explicitly_disconnected = true;
    assert!(require_selected_client(&signed_out, "oaiapp_original").is_err());
    let mut plan_off = original; plan_off.plan_enabled = false;
    assert!(require_selected_client(&plan_off, "oaiapp_original").is_err());
    let cached = Access { client_id: "oaiapp_new".into(), generation: 1, token: "new-access".into(), until: Instant::now() + Duration::from_secs(3600) };
    assert_eq!(cached.fresh_for(&RequestAccount { client_id: "oaiapp_original".into(), generation: 1 }), None, "a request cannot reuse another account's access cache");
    assert_eq!(cached.fresh_for(&RequestAccount { client_id: "oaiapp_new".into(), generation: 1 }).as_deref(), Some("new-access"));
}

#[test]
fn a_metadata_reset_cannot_register_over_surviving_credentials() {
    for (refresh, id) in [(true, false), (false, true), (true, true)] {
        assert!(require_empty_credentials(Ok((refresh, id))).is_err());
    }
    assert!(require_empty_credentials(Err("keystore locked".into())).is_err());
    assert!(require_empty_credentials(Ok((false, false))).is_ok(), "a genuinely new installation can register");
}

#[test]
fn same_registration_reauthorization_also_invalidates_older_requests_and_cached_tokens() {
    let old = RequestAccount { client_id: "oaiapp_original".into(), generation: 4 };
    assert!(check_session_generation(&old, 4).is_ok());
    assert!(check_session_generation(&old, 5).is_err(), "same client after disconnect/reconnect is a different session");
    let cached = Access { client_id: old.client_id.clone(), generation: 5, token: "new-access".into(), until: Instant::now() + Duration::from_secs(3600) };
    assert_eq!(cached.fresh_for(&old), None);
    assert_eq!(cached.fresh_for(&RequestAccount { generation: 5, ..old }).as_deref(), Some("new-access"));
}

fn account_with_pending_registration() -> Account {
    let mut original = Account { explicitly_disconnected: true, ..registered_account() };
    original.pending_account = account(Some("oaiapp_pending")).registration();
    original.saved_accounts.push(Registration {
        client_id: "oaiapp_saved".into(), email: original.email.clone(), sub: Some("saved-user".into()),
        plan_enabled: true, welcomed: true,
    });
    original
}

#[test]
fn starting_another_registration_preserves_the_abandoned_mapping_and_selected_account() {
    let original = account_with_pending_registration();
    let mut persisted = None;
    let next = prepare_new_account(&original, Some("oaiapp_pending"), true, Ok((false, false)), |record| {
        persisted = Some(serde_json::to_vec(record).unwrap()); Ok(())
    }).unwrap();
    let restarted: Account = serde_json::from_slice(&persisted.unwrap()).unwrap();
    assert_eq!(next, restarted);
    assert_eq!(next.registration(), original.registration());
    assert_eq!(next.host_id, original.host_id);
    assert_eq!(next.explicitly_disconnected, original.explicitly_disconnected);
    assert_eq!(next.session_needs_reconnect, original.session_needs_reconnect);
    assert!(next.pending_account.is_none());
    assert_eq!(next.saved_accounts, [original.saved_accounts.clone(), vec![original.pending_account.clone().unwrap()]].concat());
    let fresh = new_account_candidate(&restarted);
    assert!(fresh.client_id.is_none() && fresh.sub.is_none());
    assert_eq!(query(&authorize_url(&fresh, &Attempt::new(1455), None))["client_id"], DYNAMIC_CLIENT);
    let restored = restarted.select_registration(&restarted.saved_accounts[1]);
    assert_eq!(restored.registration(), original.pending_account);
    assert_eq!(query(&authorize_url(&restored, &Attempt::new(1455), None))["client_id"], "oaiapp_pending");
    assert!(!status_for(&restored, false, false).connected, "recoverable metadata never grants access");
}

#[test]
fn retrying_pending_sign_in_reuses_its_identity_without_archiving_or_writing() {
    let original = account_with_pending_registration();
    let next = prepare_new_account(&original, Some("oaiapp_pending"), false, Ok((false, false)),
        |_| panic!("retry must not change the registration record")).unwrap();
    assert_eq!(next, original);
    assert_eq!(new_account_candidate(&next).registration(), original.pending_account);
    assert_eq!(status_for(&next, false, false).pending_account_id.as_deref(), Some("oaiapp_pending"));
}

#[test]
fn pending_recovery_requires_explicit_cleanup_and_confirmed_empty_credentials() {
    for explicit in [false, true] {
        for refresh in [false, true] {
            for id in [false, true] {
                for start_another in [false, true] {
                    let original = Account { explicitly_disconnected: explicit, ..account_with_pending_registration() };
                    let may_start = explicit && !refresh && !id;
                    let result = prepare_new_account(&original, Some("oaiapp_pending"), start_another, Ok((refresh, id)), |_| {
                        assert!(may_start, "native gate must precede every write"); Ok(())
                    });
                    assert_eq!(result.is_ok(), may_start);
                }
            }
        }
    }
    for start_another in [false, true] {
        assert!(prepare_new_account(&account_with_pending_registration(), Some("oaiapp_pending"), start_another,
            Err("keystore locked".into()), |_| panic!("unknown credential state must not write")).is_err());
    }
}

#[test]
fn stale_or_missing_pending_identity_cannot_retry_or_archive_another_registration() {
    let original = account_with_pending_registration();
    for expected in [None, Some("oaiapp_other")] {
        for start_another in [false, true] {
            assert!(prepare_new_account(&original, expected, start_another, Ok((false, false)),
                |_| panic!("stale pending ownership must not write")).is_err());
        }
    }
    let fresh = Account { pending_account: None, ..original.clone() };
    for start_another in [false, true] {
        assert!(prepare_new_account(&fresh, Some("oaiapp_pending"), start_another, Ok((false, false)),
            |_| panic!("a consumed action must not run again")).is_err());
    }
    assert!(prepare_new_account(&fresh, None, true, Ok((false, false)),
        |_| panic!("start-another requires a specific pending registration")).is_err());
    assert_eq!(prepare_new_account(&fresh, None, false, Ok((false, false)),
        |_| panic!("first add needs no archive")).unwrap(), fresh);
}

#[test]
fn failed_pending_archive_persistence_leaves_the_retry_mapping_and_selection_intact() {
    let original = account_with_pending_registration();
    let before = original.clone();
    let result = prepare_new_account(&original, Some("oaiapp_pending"), true, Ok((false, false)), |_| Err("disk full".into()));
    assert_eq!(result, Err("disk full".into()));
    assert_eq!(original, before);
    assert_eq!(new_account_candidate(&original).client_id.as_deref(), Some("oaiapp_pending"));
}

#[test]
fn an_archived_pending_mapping_survives_failure_of_the_next_registration() {
    let original = account_with_pending_registration();
    let mut next = prepare_new_account(&original, Some("oaiapp_pending"), true, Ok((false, false)), |_| Ok(())).unwrap();
    let mut candidate = new_account_candidate(&next);
    let attempt = Attempt::new(1455);
    let callback = format!("/auth/callback?state={}&code=new-code&client_id=oaiapp_next", attempt.state);
    prepare_code_exchange(&mut candidate, &attempt, &callback, |record| retain_pending_account(&mut next, record, |_| Ok(()))).unwrap();
    assert_eq!(next.registration(), original.registration());
    assert_eq!(next.saved_accounts.last(), original.pending_account.as_ref());
    assert_eq!(next.pending_account.as_ref().unwrap().client_id, "oaiapp_next");
    assert!(prepare_new_account(&next, Some("oaiapp_pending"), true, Ok((false, false)),
        |_| panic!("old recovery click must not archive the newer pending account")).is_err());
    assert!(retain_pending_account(&mut next, &candidate, |_| panic!("an issued pending mapping must not be overwritten")).is_err());
}

#[test]
fn pending_archive_cannot_overwrite_a_selected_or_saved_registration() {
    for selected in [false, true] {
        let mut original = account_with_pending_registration();
        original.pending_account = Some(if selected { original.registration().unwrap() } else { original.saved_accounts[0].clone() });
        let id = original.pending_account.as_ref().unwrap().client_id.as_str();
        assert!(prepare_new_account(&original, Some(id), true, Ok((false, false)),
            |_| panic!("conflicting metadata must not overwrite either mapping")).is_err());
    }
}

#[test]
fn a_busy_native_account_operation_rejects_sign_in_without_queuing() {
    let serial = tokio::sync::Mutex::new(());
    let operation = try_sign_in_lock(&serial).unwrap();
    assert!(try_sign_in_lock(&serial).is_err(), "double clicks and other busy operations fail immediately");
    drop(operation);
    assert!(try_sign_in_lock(&serial).is_ok(), "a later explicit retry can proceed");
}

#[test]
fn speech_refusals_belong_to_one_account_session() {
    let now = Instant::now();
    let original = RequestAccount { client_id: "oaiapp_original".into(), generation: 7 };
    let other = RequestAccount { client_id: "oaiapp_other".into(), generation: 7 };
    let reconnected = RequestAccount { client_id: original.client_id.clone(), generation: 8 };
    let refusal = SpeechFailure { account: original.clone(), retry_after: None };
    let new_refusal = SpeechFailure { account: reconnected.clone(), retry_after: None };
    assert!(check_speech_session(&original, 7, None, now).is_ok());
    assert!(check_speech_session(&original, 7, Some(&refusal), now).is_err());
    assert!(check_speech_session(&original, 7, Some(&refusal), now + SPEECH_RETRY_DELAY).is_err(), "HTTP refusals last for the session");
    assert!(check_speech_session(&other, 7, Some(&refusal), now).is_ok());
    assert!(check_speech_session(&reconnected, 8, Some(&refusal), now).is_ok());
    assert!(check_speech_session(&original, 8, None, now).is_err(), "a late speech request must not follow a reconnect");
    assert!(check_speech_session(&original, 8, Some(&new_refusal), now).is_err(), "stale requests cannot publish into the new session");
}

#[test]
fn speech_transport_failures_skip_later_chunks_until_the_cooldown_expires() {
    let now = Instant::now();
    let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 7 };
    let mut refused = None;
    let mut sends = 0;
    for seconds in 0..=60 {
        let at = now + Duration::from_secs(seconds);
        // The same gate used by both speech_available() and speak(): later
        // chunks use Windows immediately, without starting another request.
        if check_speech_session(&request, 7, refused.as_ref(), at).is_ok() {
            sends += 1;
            assert!(speech_transport_result(&request, 7, Err::<(), _>("offline"), &mut refused, at).is_err());
        }
        assert_eq!(sends, if seconds < 60 { 1 } else { 2 }, "chunk at {seconds}s");
    }
    assert_eq!(refused.as_ref().unwrap().retry_after, Some(now + SPEECH_RETRY_DELAY * 2));
    assert_eq!(SPEECH_TIMEOUT, Duration::from_secs(5), "the first probe must also fall back quickly");
}

#[test]
fn speech_transport_cooldowns_do_not_follow_reconnects_or_account_switches() {
    let now = Instant::now();
    let original = RequestAccount { client_id: "oaiapp_original".into(), generation: 7 };
    let other = RequestAccount { client_id: "oaiapp_other".into(), generation: 7 };
    let reconnected = RequestAccount { client_id: original.client_id.clone(), generation: 8 };
    let mut refused = None;
    assert!(speech_transport_result(&original, 7, Err::<(), _>("timeout"), &mut refused, now).is_err());
    assert!(check_speech_session(&original, 7, refused.as_ref(), now).is_err());
    assert!(check_speech_session(&other, 7, refused.as_ref(), now).is_ok());
    assert!(check_speech_session(&reconnected, 8, refused.as_ref(), now).is_ok());
    assert!(check_speech_session(&original, 8, refused.as_ref(), now + SPEECH_RETRY_DELAY).is_err(), "expiry cannot revive a stale request");
}

#[test]
fn stale_speech_transport_completions_cannot_affect_a_new_session() {
    let now = Instant::now();
    let original = RequestAccount { client_id: "oaiapp_original".into(), generation: 7 };
    let reconnected = RequestAccount { client_id: original.client_id.clone(), generation: 8 };
    let new_failure = SpeechFailure { account: reconnected, retry_after: Some(now + SPEECH_RETRY_DELAY) };
    for initial in [None, Some(new_failure)] {
        let mut refused = initial.clone();
        assert!(speech_transport_result(&original, 8, Err::<(), _>("late timeout"), &mut refused, now).is_err());
        assert_eq!(refused, initial, "a stale failure neither creates nor replaces a cooldown");
        assert!(speech_transport_result(&original, 8, Ok::<_, ()>(vec![1, 2, 3]), &mut refused, now).is_err());
        assert_eq!(refused, initial, "a stale success cannot publish audio or change the cache");
    }
}

#[test]
fn speech_body_failures_share_the_cooldown_without_shortening_http_refusals() {
    let now = Instant::now();
    let request = RequestAccount { client_id: "oaiapp_original".into(), generation: 7 };
    let mut refused = None;
    assert_eq!(speech_transport_result(&request, 7, Ok::<_, ()>(vec![1, 2, 3]), &mut refused, now).unwrap(), vec![1, 2, 3]);
    assert_eq!(refused, None, "a successful response does not disable speech");
    assert!(speech_transport_result(&request, 7, Err::<Vec<u8>, _>("body interrupted"), &mut refused, now).is_err());
    assert!(check_speech_session(&request, 7, refused.as_ref(), now).is_err());
    refused = Some(SpeechFailure { account: request.clone(), retry_after: None });
    let permanent = refused.clone();
    assert!(speech_transport_result(&request, 7, Err::<(), _>("late transport failure"), &mut refused, now).is_err());
    assert_eq!(refused, permanent, "a concurrent transport failure must not shorten a session refusal");
    assert!(speech_transport_result(&request, 7, Ok::<_, ()>(()), &mut refused, now).is_ok());
    assert_eq!(refused, permanent, "a concurrent success must not clear a session refusal");
}

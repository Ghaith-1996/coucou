// The desktop assistant (hybrid P0+): voice capture, assistant actions and,
// later, desktop adapters, memory and routines. See contracts.rs for why it
// is a domain apart from the agents' permission requests.

pub mod app;
pub mod apps;
pub mod contracts;
pub mod dictation;
#[cfg(windows)]
pub mod launch;
pub mod policy;
pub mod speech;
pub mod voice;

#[cfg(test)]
mod tests {
    use super::contracts::*;
    use super::policy::*;
    use serde_json::{json, Value};

    fn fixture() -> Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/assistant-contracts.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn kebab<T: serde::Serialize>(value: T) -> String {
        serde_json::to_value(value).unwrap().as_str().unwrap().to_string()
    }

    #[test]
    fn the_registry_and_states_match_the_shared_fixture() {
        let f = fixture();
        assert_eq!(f["version"], CONTRACT_VERSION);
        assert_eq!(f["voiceEvent"], VOICE_EVENT);
        let kinds: Vec<Value> = ACTION_KINDS
            .iter()
            .map(|k| json!({ "kind": kebab(k), "available": k.available(), "idempotent": k.idempotent() }))
            .collect();
        assert_eq!(f["actionKinds"], Value::Array(kinds));
        let states: Vec<String> = VOICE_STATES.iter().map(kebab).collect();
        assert_eq!(f["voiceStates"], json!(states));
    }

    #[test]
    fn every_fixture_action_gets_its_expected_decision() {
        let f = fixture();
        let now = f["now"].as_u64().unwrap();
        for case in f["actions"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let action: AssistantAction = serde_json::from_value(case["action"].clone()).unwrap();
            let verdict = decide(&action, now);
            assert_eq!(kebab(verdict.decision), case["decision"], "{name}");
            let error = verdict.error.map(kebab);
            assert_eq!(error.as_deref(), case["error"].as_str(), "{name}");
            assert_eq!(verdict.args.is_some(), verdict.decision != Decision::Deny, "{name}");
        }
    }

    #[test]
    fn what_is_not_an_assistant_action_does_not_even_parse() {
        for case in fixture()["unparseable"].as_array().unwrap() {
            let parsed = serde_json::from_value::<AssistantAction>(case["action"].clone());
            assert!(parsed.is_err(), "{}", case["name"]);
        }
    }

    #[test]
    fn voice_events_are_accepted_or_rejected_as_the_fixture_says() {
        for case in fixture()["voiceEvents"].as_array().unwrap() {
            let parsed = serde_json::from_value::<VoiceEvent>(case["event"].clone());
            if case["valid"] != true {
                assert!(parsed.is_err(), "{}", case["name"]);
                continue;
            }
            let event = parsed.unwrap();
            assert_eq!(serde_json::to_value(&event).unwrap(), case["event"], "{}", case["name"]);
        }
    }

    // ── Grants ───────────────────────────────────────────────────────────────

    const NOW: u64 = 1_000_000;

    fn insert(text: &str) -> AssistantAction {
        AssistantAction {
            v: CONTRACT_VERSION,
            domain: Domain::DesktopAssistant,
            id: "act-1".into(),
            kind: ActionKind::InsertText,
            args: json!({ "text": text, "targetWindow": 42, "targetPid": 7 }),
            intent_id: "cap-1".into(),
            expires_at_ms: NOW + 60_000,
        }
    }

    /// A reserved kind is refused, so its `ask` and grant are exercised
    /// through `decide`'s logic with the availability check bypassed: the
    /// binding itself must already hold before any kind uses it.
    fn grant_ignoring_availability(action: &AssistantAction) -> Grant {
        let args = validate(action.kind, &action.args).unwrap();
        Grant { action_id: action.id.clone(), digest: binding_digest(action, &args), expires_at_ms: action.expires_at_ms }
    }

    fn reserved() -> AssistantAction {
        AssistantAction {
            kind: ActionKind::AskRegion,
            args: json!({ "question": "what is this?", "monitor": "DISPLAY1", "x": 0, "y": 0, "width": 10, "height": 10 }),
            ..insert("hi")
        }
    }

    #[test]
    fn only_an_available_kind_that_asks_gets_a_grant() {
        assert!(grant_for(&reserved(), NOW).is_none(), "reserved kind");
        assert!(grant_for(&insert("hi"), NOW).is_some(), "insert-text asks");
        let mut draft = insert("hi");
        draft.kind = ActionKind::ChatDraft;
        draft.args = json!({ "text": "hi" });
        assert!(grant_for(&draft, NOW).is_none(), "allowed outright: nothing to confirm");
    }

    #[test]
    fn a_grant_is_bound_to_the_exact_arguments_id_and_expiry() {
        let action = insert("hello");
        let grant = grant_ignoring_availability(&action);

        let same_but_reordered = AssistantAction {
            args: json!({ "targetPid": 7, "targetWindow": 42, "text": "hello" }),
            ..action.clone()
        };
        assert_eq!(binding_digest(&same_but_reordered, &validate(action.kind, &same_but_reordered.args).unwrap()), grant.digest);

        for changed in [
            AssistantAction { args: json!({ "text": "rm -rf", "targetWindow": 42, "targetPid": 7 }), ..action.clone() },
            AssistantAction { args: json!({ "text": "hello", "targetWindow": 43, "targetPid": 7 }), ..action.clone() },
            AssistantAction { id: "act-2".into(), ..action.clone() },
            AssistantAction { expires_at_ms: action.expires_at_ms + 1, ..action.clone() },
        ] {
            let args = validate(changed.kind, &changed.args).unwrap();
            assert_ne!(binding_digest(&changed, &args), grant.digest);
        }
    }

    #[test]
    fn check_grant_decides_again_and_refuses_a_mismatch() {
        // A chat draft is allowed, so check_grant can be exercised end to end.
        let draft = AssistantAction {
            kind: ActionKind::ChatDraft,
            args: json!({ "text": "hi" }),
            ..insert("hi")
        };
        let args = validate(draft.kind, &draft.args).unwrap();
        let grant = Grant { action_id: draft.id.clone(), digest: binding_digest(&draft, &args), expires_at_ms: draft.expires_at_ms };
        assert_eq!(check_grant(&grant, &draft, NOW), Ok(args));

        let tampered = AssistantAction { args: json!({ "text": "bye" }), ..draft.clone() };
        assert_eq!(check_grant(&grant, &tampered, NOW), Err(ErrorCode::GrantMismatch));

        // Expired while the user was reading.
        assert_eq!(check_grant(&grant, &draft, draft.expires_at_ms), Err(ErrorCode::Expired));

        // A refused action never gets through, grant or not.
        let reserved = reserved();
        let grant = grant_ignoring_availability(&reserved);
        assert_eq!(check_grant(&grant, &reserved, NOW), Err(ErrorCode::Unavailable));

        // An insertion the user confirmed passes; the same grant for other
        // words or another window does not.
        let action = insert("hello");
        let grant = grant_for(&action, NOW).unwrap();
        assert!(check_grant(&grant, &action, NOW).is_ok());
        let other = AssistantAction { args: json!({ "text": "hello", "targetWindow": 43, "targetPid": 7 }), ..action };
        assert_eq!(check_grant(&grant, &other, NOW), Err(ErrorCode::GrantMismatch));
    }

    // ── Receipts and retries ─────────────────────────────────────────────────

    #[test]
    fn success_needs_evidence() {
        assert!(ActionReceipt::succeeded("act-1", "").is_none());
        assert!(ActionReceipt::succeeded("act-1", "  ").is_none());
        let ok = ActionReceipt::succeeded("act-1", "chat-field:draft-set").unwrap();
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({ "actionId": "act-1", "outcome": "succeeded", "evidenceRef": "chat-field:draft-set" })
        );
        assert_eq!(
            serde_json::to_value(ActionReceipt::unknown("act-1")).unwrap(),
            json!({ "actionId": "act-1", "outcome": "unknown", "errorCode": "unverified" })
        );
    }

    #[test]
    fn deserialized_success_needs_nonblank_evidence() {
        for evidence in [None, Some(Value::Null), Some(json!("")), Some(json!("  \n  "))] {
            let mut receipt = json!({ "actionId": "act-1", "outcome": "succeeded" });
            if let Some(value) = evidence {
                receipt["evidenceRef"] = value;
            }
            assert!(serde_json::from_value::<ActionReceipt>(receipt.clone()).is_err(), "{receipt}");
        }
    }

    #[test]
    fn valid_receipts_round_trip() {
        for receipt in [
            ActionReceipt::succeeded("act-1", "chat-field:draft-set").unwrap(),
            ActionReceipt::failed("act-1", ErrorCode::Failed),
            ActionReceipt::cancelled("act-1"),
            ActionReceipt::unknown("act-1"),
        ] {
            let json = serde_json::to_value(&receipt).unwrap();
            assert_eq!(serde_json::from_value::<ActionReceipt>(json).unwrap(), receipt);
        }
    }

    #[test]
    fn an_unknown_outcome_is_retried_only_when_harmless() {
        assert!(may_retry(ActionKind::ChatDraft, Outcome::Unknown));
        assert!(!may_retry(ActionKind::InsertText, Outcome::Unknown));
        assert!(!may_retry(ActionKind::LaunchApp, Outcome::Unknown));
        assert!(!may_retry(ActionKind::ChatDraft, Outcome::Succeeded));
        assert!(!may_retry(ActionKind::ChatDraft, Outcome::Cancelled));
        assert!(may_retry(ActionKind::InsertText, Outcome::Failed));
    }

    #[test]
    fn ids_are_fresh() {
        let a = new_id("act");
        assert!(a.starts_with("act-") && a.len() == 4 + 16);
        assert_ne!(a, new_id("act"));
    }

    // ── The wall between the two domains ─────────────────────────────────────

    /// No file of the assistant may reach the agents' permission relay or the
    /// approval commands. A voice command, a model's reply or a routine can't
    /// answer an agent if the code that runs them can't name the way to.
    #[test]
    fn the_assistant_never_reaches_agent_permissions() {
        const FORBIDDEN: [&str; 7] = [
            "pipe::", "crate::pipe", "Pending", "Reply::", "approval_", "answer_question", "PermissionRequest",
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assistant");
        let mut stack = vec![root];
        let mut checked = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                // This test names them; it is the one place allowed to.
                let code = text.split("fn the_assistant_never_reaches_agent_permissions").next().unwrap();
                for word in FORBIDDEN {
                    assert!(!code.contains(word), "{} mentions {word}", path.display());
                }
                checked += 1;
            }
        }
        assert!(checked >= 3);
    }
}

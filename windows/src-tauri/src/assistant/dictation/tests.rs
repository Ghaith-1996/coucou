use super::*;

fn permit() -> WritePermit {
    WritePermit { cancelled: Arc::new(AtomicBool::new(false)), expires: Instant::now() + Duration::from_secs(120) }
}

fn writable() -> Facts {
    Facts {
        window_alive: true,
        foreground: Foreground::Coucou,
        element_alive: true,
        password: false,
        enabled: true,
        value_read_only: Some(false),
    }
}

#[test]
fn a_writable_field_of_the_same_window_takes_the_words() {
    assert_eq!(refusal(&writable()), None);
    assert_eq!(refusal(&Facts { foreground: Foreground::Target, ..writable() }), None);
}

#[test]
fn the_keyboard_going_to_another_window_stops_it() {
    assert_eq!(refusal(&Facts { foreground: Foreground::Other, ..writable() }), Some(Refusal::FocusMoved));
}

#[test]
fn a_closed_window_or_a_vanished_field_stops_it() {
    assert_eq!(refusal(&Facts { window_alive: false, ..writable() }), Some(Refusal::Gone));
    assert_eq!(refusal(&Facts { element_alive: false, ..writable() }), Some(Refusal::Gone));
    // Gone wins over everything else: there is nothing to look at.
    assert_eq!(
        refusal(&Facts { window_alive: false, foreground: Foreground::Other, password: true, ..writable() }),
        Some(Refusal::Gone),
    );
}

#[test]
fn a_password_field_is_never_written() {
    assert_eq!(refusal(&Facts { password: true, ..writable() }), Some(Refusal::Protected));
    assert_eq!(refusal(&Facts { password: true, foreground: Foreground::Target, ..writable() }), Some(Refusal::Protected));
    assert_eq!(refusal(&Facts { password: true, foreground: Foreground::Other, ..writable() }), Some(Refusal::Protected));
}

#[test]
fn read_only_disabled_or_patternless_fields_are_refused() {
    assert_eq!(refusal(&Facts { value_read_only: Some(true), ..writable() }), Some(Refusal::ReadOnly));
    assert_eq!(refusal(&Facts { enabled: false, ..writable() }), Some(Refusal::ReadOnly));
    assert_eq!(refusal(&Facts { value_read_only: None, ..writable() }), Some(Refusal::Unsupported));
}

#[test]
fn protected_or_unclassified_targets_never_allow_copy_despite_other_refusals() {
    for reason in [Refusal::FocusMoved, Refusal::Gone, Refusal::Protected, Refusal::ReadOnly, Refusal::Unsupported, Refusal::Busy, Refusal::ValueChanged] {
        assert!(!Rejection::from(reason).copy_allowed, "a refusal name alone is not evidence: {reason:?}");
        for foreground in [Foreground::Target, Foreground::Coucou, Foreground::Other] {
            // UIA also maps a failed CurrentIsPassword read to true.
            let facts = Facts { password: true, foreground, ..writable() };
            assert!(!Rejection::checked(reason, &facts).copy_allowed);
        }
        assert!(!Rejection::checked(reason, &Facts { window_alive: false, ..writable() }).copy_allowed);
        assert!(!Rejection::checked(reason, &Facts { element_alive: false, ..writable() }).copy_allowed);
    }
}

#[test]
fn only_a_live_positively_checked_non_password_target_can_allow_copy() {
    for facts in [
        Facts { foreground: Foreground::Other, ..writable() },
        Facts { value_read_only: Some(true), ..writable() },
        Facts { enabled: false, ..writable() },
        Facts { value_read_only: None, ..writable() },
    ] {
        let reason = refusal(&facts).expect("a fallback field");
        let rejection = Rejection::checked(reason, &facts);
        assert!(rejection.copy_allowed, "a checked non-password field can offer Copy: {reason:?}");
    }
    for reason in [Refusal::Gone, Refusal::Protected, Refusal::Busy, Refusal::ValueChanged] {
        assert!(!Rejection::checked(reason, &writable()).copy_allowed);
    }
}

#[test]
fn refusal_wire_replies_explicitly_carry_native_copy_eligibility() {
    use super::super::app::{InsertOffer, InsertResult};

    let refusal = Rejection::checked(Refusal::FocusMoved, &Facts { foreground: Foreground::Other, ..writable() });
    assert_eq!(serde_json::to_value(InsertOffer::Refused { refusal }).unwrap(), serde_json::json!({
        "status": "refused", "reason": "focus-moved", "copyAllowed": true,
    }));
    assert_eq!(serde_json::to_value(InsertResult::Kept { refusal: Refusal::Protected.into() }).unwrap(), serde_json::json!({
        "status": "kept", "reason": "protected", "copyAllowed": false,
    }));
    assert_eq!(serde_json::to_value(InsertOffer::Refused { refusal: Refusal::Unsupported.into() }).unwrap(), serde_json::json!({
        "status": "refused", "reason": "unsupported", "copyAllowed": false,
    }));
}

#[test]
fn words_go_where_the_caret_was() {
    assert_eq!(spliced("Hello world", Some(("Hello ", "")), "big"), "Hello big world");
    assert_eq!(spliced("Hello world", Some(("Hello", "")), "big"), "Hello big world");
    assert_eq!(spliced("", Some(("", "")), " Hi there. "), "Hi there.");
}

#[test]
fn the_selection_is_replaced() {
    assert_eq!(spliced("Hello cruel world", Some(("Hello ", "cruel")), "kind"), "Hello kind world");
    assert_eq!(spliced("abc", Some(("", "abc")), "xyz"), "xyz");
}

#[test]
fn without_a_caret_or_when_the_value_disagrees_the_words_go_at_the_end() {
    assert_eq!(spliced("Dear Sam,", None, "thanks"), "Dear Sam, thanks");
    // RichEdit may report \r where the value has \r\n: never guess an offset.
    assert_eq!(spliced("one\r\ntwo", Some(("one\rt", "")), "x"), "one\r\ntwo x");
    assert_eq!(spliced("Hello world", Some(("Hello ", "planet")), "big"), "Hello world big");
    assert_eq!(spliced("line\n", None, "next"), "line\nnext");
}

#[test]
fn spacing_respects_punctuation_and_brackets() {
    assert_eq!(spliced("Hi", None, ", friend"), "Hi, friend");
    assert_eq!(spliced("(", None, "aside"), "(aside");
    assert_eq!(spliced("Say ", Some(("Say ", "")), "hello"), "Say hello");
    assert_eq!(spliced("end.", Some(("end", "")), "the"), "end the.");
}

#[test]
fn straight_quotes_keep_words_inside_and_outside_the_delimiters() {
    for quote in ['\'', '"'] {
        let value = format!("He said {quote}hello{quote}");
        let opening = format!("He said {quote}");
        let closing = format!("He said {quote}hello");
        assert_eq!(spliced(&value, None, "world"), format!("{value} world"));
        assert_eq!(spliced(&value, Some((&opening, "")), "dear"), format!("He said {quote}dear hello{quote}"));
        assert_eq!(spliced(&value, Some((&closing, "")), "world"), format!("He said {quote}hello world{quote}"));
        assert_eq!(spliced(&value, Some(("He said ", "")), "plainly"), format!("He said plainly {quote}hello{quote}"));
        assert_eq!(spliced(&format!("{quote}{quote}"), Some((&quote.to_string(), "")), "hello"), format!("{quote}hello{quote}"));
    }
}

#[test]
fn inserted_quotes_get_context_at_both_splice_boundaries() {
    for quote in ['\'', '"'] {
        let quoted = format!("{quote}middle{quote}");
        assert_eq!(spliced("before after", Some(("before ", "")), &quoted), format!("before {quoted} after"));
        assert_eq!(spliced("beforeafter", Some(("before", "")), &quoted), format!("before {quoted} after"));
        let before = format!("He said {quote}hello");
        assert_eq!(spliced(&before, None, &format!("{quote}, friend")), format!("{before}{quote}, friend"));
        assert_eq!(spliced("before world", Some(("before ", "")), &format!("say {quote}")), format!("before say {quote}world"));
    }
}

#[test]
fn directional_quote_pairs_keep_inside_and_outside_spacing() {
    for (opening, closing) in [('“', '”'), ('‘', '’'), ('«', '»')] {
        let value = format!("He said {opening}hello{closing}");
        let inside_start = format!("He said {opening}");
        let inside_end = format!("He said {opening}hello");
        assert_eq!(spliced(&value, None, "world"), format!("{value} world"));
        assert_eq!(spliced(&value, Some((&inside_start, "")), "dear"), format!("He said {opening}dear hello{closing}"));
        assert_eq!(spliced(&value, Some((&inside_end, "")), "world"), format!("He said {opening}hello world{closing}"));
        assert_eq!(spliced(&value, Some(("He said ", "")), "plainly"), format!("He said plainly {opening}hello{closing}"));
        assert_eq!(spliced("beforeafter", Some(("before", "")), &format!("{opening}middle{closing}")),
            format!("before {opening}middle{closing} after"));
    }
}

#[test]
fn directional_quotes_preserve_existing_whitespace() {
    for (opening, closing) in [('“', '”'), ('‘', '’'), ('«', '»')] {
        let before = format!("{opening} hello");
        let value = format!("{before} {closing}");
        assert_eq!(spliced(&value, Some((&before, "")), "world"), format!("{opening} hello world {closing}"));
        assert_eq!(spliced(&value, Some((&opening.to_string(), "")), "dear"), format!("{opening}dear hello {closing}"));
        assert_eq!(spliced(&format!("{value}\n"), None, "world"), format!("{value}\nworld"));
    }
}

#[test]
fn apostrophes_inside_words_do_not_change_quote_context() {
    for (value, before, expected) in [
        ("can't say 'hello'", "can't say 'hello", "can't say 'hello world'"),
        ("l'homme dit 'bonjour'", "l'homme dit 'bonjour", "l'homme dit 'bonjour world'"),
        ("l'été dit 'bonjour'", "l'été dit 'bonjour", "l'été dit 'bonjour world'"),
        ("He said 'can't'", "He said 'can't", "He said 'can't world'"),
        ("He said 'l'homme'", "He said 'l'homme", "He said 'l'homme world'"),
    ] {
        assert_eq!(spliced(value, Some((before, "")), "world"), expected);
        assert_eq!(spliced(value, None, "world"), format!("{value} world"));
    }
    assert_eq!(spliced("before after", Some(("before ", "")), "'can't'"), "before 'can't' after");
    assert_eq!(spliced("He said 'hello'", Some(("He said 'hello", "")), "don't stop"), "He said 'hello don't stop'");
}

#[test]
fn unmatched_possessive_apostrophes_are_not_opening_quotes() {
    assert_eq!(spliced("dogs'", None, "tails"), "dogs' tails");
    assert_eq!(spliced("dog's", None, "tail"), "dog's tail");
    assert_eq!(spliced("dogs' 'tails'", None, "wag"), "dogs' 'tails' wag");
    assert_eq!(spliced("dogs' 'tails'", Some(("dogs' ", "")), "long"), "dogs' long 'tails'");
    assert_eq!(spliced("before after", Some(("before ", "")), "dogs'"), "before dogs' after");
}

#[test]
fn opposite_quote_marks_keep_independent_context() {
    for (outer, inner) in [('"', '\''), ('\'', '"')] {
        let value = format!("He said {outer}{inner}hello{inner}{outer}");
        let inside = format!("He said {outer}{inner}hello");
        let outside = format!("{inside}{inner}");
        assert_eq!(spliced(&value, None, "world"), format!("{value} world"));
        assert_eq!(spliced(&value, Some((&inside, "")), "world"), format!("He said {outer}{inner}hello world{inner}{outer}"));
        assert_eq!(spliced(&value, Some((&outside, "")), "world"), format!("{outside} world{outer}"));
        assert_eq!(spliced("before after", Some(("before ", "")), &format!("{outer}{inner}hello{inner}{outer}")),
            format!("before {outer}{inner}hello{inner}{outer} after"));
    }
}

#[test]
fn quoted_splices_preserve_whitespace_unicode_and_selection_fallbacks() {
    assert_eq!(spliced("\"hello\"\n", None, "world"), "\"hello\"\nworld");
    assert_eq!(spliced("\"hello \"", Some(("\"hello ", "")), "world"), "\"hello world\"");
    assert_eq!(spliced("\"hello\"", Some(("\"", "hello")), "café"), "\"café\"");
    assert_eq!(spliced("\"hello\"", Some(("", "\"hello")), "café \"bonjour"), "café \"bonjour\"");
    assert_eq!(spliced("\"hello\"", Some(("different", "")), "world"), "\"hello\" world");
    assert_eq!(spliced("\"hello\"", Some(("\"", "different")), "world"), "\"hello\" world");
    assert_eq!(spliced("\"café\"", Some(("\"café", "")), "crème"), "\"café crème\"");
    assert_eq!(spliced("\"日本\"", Some(("\"日", "")), "の"), "\"日の本\"");
    assert_eq!(spliced("(\"hello\")", Some(("(\"hello", "")), "world"), "(\"hello world\")");
}

#[test]
fn non_ascii_text_splits_on_character_boundaries() {
    assert_eq!(spliced("Café crème", Some(("Café ", "")), "très"), "Café très crème");
    assert_eq!(spliced("日本", Some(("日", "")), "の"), "日の本");
}

#[test]
fn evidence_is_the_exact_intended_value_read_back() {
    assert!(verified("Hello big world", "Hello big world"));
    assert!(!verified("Hello world", "Hello big world"));
    assert!(!verified("Hello big world!", "Hello big world"));
    assert!(!verified(" Hello big world ", "Hello big world"));
    assert!(!verified("one\r\ntwo", "one\ntwo"), "different ValuePattern results are not normalized");
    assert!(!verified("", ""), "an empty fallback is not evidence");
}

#[test]
fn preexisting_words_do_not_verify_an_ignored_write() {
    let original = "Hello big world";
    for caret in [None, Some(("Hello ", "")), Some(("Hello big ", "world"))] {
        let intended = spliced(original, caret, "big");
        assert_ne!(intended, original);
        assert!(!verified(original, &intended), "SetValue ignored the requested insertion");
        assert!(verified(&intended, &intended));
    }
}

#[test]
fn replacing_the_selection_with_identical_words_verifies_the_requested_noop() {
    let original = "Hello big world";
    let intended = spliced(original, Some(("Hello ", "big")), " big ");
    assert_eq!(intended, original);
    assert!(verified(original, &intended), "the exact requested state is already present");
}

#[test]
fn slow_transcription_keeps_the_target_until_the_native_preview_deadline() {
    let captured = Instant::now();
    let mut target = Remembered::new("cap-slow".into(), "original field");
    // A full capture plus more than the old ten-second transcription slack.
    // Seed instants instead of sleeping or invoking a microphone/model.
    let preview = captured + Duration::from_secs(30 + 90);
    let expires = preview + super::super::voice::PREVIEW_TTL;
    assert!(!target.expired(preview), "an unoffered target has no guessed transcription timeout");
    assert_eq!(target.offer("cap-slow", expires, preview), Ok(&"original field"));
    let near_end = expires - Duration::from_millis(1);
    assert!(near_end > captured + Duration::from_secs(160));
    assert!(!target.expired(near_end));
    assert_eq!(target.offer("cap-slow", expires, near_end), Ok(&"original field"));
    assert!(target.expired(expires));
    assert_eq!(target.offer("cap-slow", expires, expires), Err(Refusal::Gone));
}

#[test]
fn a_first_offer_after_a_slow_transcription_can_arrive_near_preview_expiry() {
    let captured = Instant::now();
    let mut target = Remembered::new("cap-slow".into(), "original field");
    let expires = captured + Duration::from_secs(30 + 90 + 120);
    assert_eq!(target.offer("cap-slow", expires, expires - Duration::from_millis(1)), Ok(&"original field"));
    assert!(target.expired(expires));
}

#[test]
fn repeated_offers_never_renew_the_original_target_deadline() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut target = Remembered::new("cap-current".into(), "original field");
    assert_eq!(target.offer("cap-current", expires, now), Ok(&"original field"));
    assert_eq!(target.offer("cap-current", expires + Duration::from_secs(60), expires - Duration::from_millis(1)), Ok(&"original field"));
    assert_eq!(target.expires, Some(expires));
    assert_eq!(target.offer("cap-current", expires + Duration::from_secs(120), expires), Err(Refusal::Gone));
    assert_eq!(target.expires, Some(expires), "an expired target cannot be renewed either");
}

#[test]
fn stale_capture_offers_cannot_set_or_replace_a_new_targets_deadline() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut target = Remembered::new("cap-new".into(), "new field");
    assert_eq!(target.offer("cap-old", now, now), Err(Refusal::Gone));
    assert_eq!(target.expires, None);
    assert_eq!(target.offer("cap-new", expires, now), Ok(&"new field"));
    for stale_deadline in [now, expires + Duration::from_secs(120)] {
        assert_eq!(target.offer("cap-old", stale_deadline, now), Err(Refusal::Gone));
        assert_eq!(target.expires, Some(expires));
    }
    assert_eq!(target.offer("cap-new", expires, expires - Duration::from_millis(1)), Ok(&"new field"));
}

#[derive(Debug, PartialEq, Eq)]
struct FakeField {
    capture: &'static str,
    window: u64,
    pid: u32,
    value: std::cell::RefCell<String>,
}

fn field() -> FakeField {
    FakeField { capture: "cap-current", window: 4242, pid: 77, value: std::cell::RefCell::new("Hello".into()) }
}

#[test]
fn a_stale_capture_or_changed_target_cannot_write_or_consume_the_current_field() {
    for (capture, window, pid) in [("cap-old", 4242, 77), ("cap-current", 4243, 77), ("cap-current", 4242, 78)] {
        let mut bound = Some(field());
        let answer = write_bound(
            &mut bound,
            |field| field.capture == capture && field.window == window && field.pid == pid,
            |_| panic!("a stale request must not reach the writer"),
        );
        assert_eq!(answer, Err(Refusal::Gone.into()));
        assert_eq!(bound, Some(field()), "the owning capture must keep its field");
        assert_eq!(write_bound(&mut bound, |_| true, |_| Ok(Written::Verified)), Ok(Written::Verified));
        assert!(bound.is_none());
    }
}

#[test]
fn a_missing_field_never_calls_the_writer() {
    let mut bound: Option<FakeField> = None;
    assert_eq!(
        write_bound(&mut bound, |_| panic!("nothing to own"), |_| panic!("nothing to write")),
        Err(Refusal::Gone.into()),
    );
}

#[test]
fn prewrite_refusals_keep_the_exact_field_for_a_later_success() {
    for why in [Refusal::FocusMoved, Refusal::Gone, Refusal::Protected, Refusal::ReadOnly, Refusal::Unsupported, Refusal::Busy, Refusal::ValueChanged] {
        let mut bound = Some(field());
        assert_eq!(write_bound(&mut bound, |_| true, |_| Err(why.into())), Err(why.into()));
        assert_eq!(bound, Some(field()), "nothing written or consumed after {why:?}");
        let writes = std::cell::Cell::new(0);
        assert_eq!(
            write_bound(&mut bound, |field| field.capture == "cap-current", |field| {
                writes.set(writes.get() + 1);
                let next = spliced(&field.value.borrow(), None, "world");
                *field.value.borrow_mut() = next;
                assert_eq!(*field.value.borrow(), "Hello world");
                Ok(Written::Verified)
            }),
            Ok(Written::Verified),
        );
        assert!(bound.is_none());
        assert_eq!(write_bound(&mut bound, |_| true, |_| panic!("a completed insertion is never retried")), Err(Refusal::Gone.into()));
        assert_eq!(writes.get(), 1);
    }
}

#[test]
fn every_attempted_write_outcome_consumes_the_field() {
    for outcome in [Written::Verified, Written::Unconfirmed, Written::Failed] {
        let mut bound = Some(field());
        assert_eq!(write_bound(&mut bound, |_| true, |_| Ok(outcome)), Ok(outcome));
        assert!(bound.is_none(), "{outcome:?} is terminal, even without verified success");
        assert_eq!(write_bound(&mut bound, |_| true, |_| panic!("a terminal outcome cannot write twice")), Err(Refusal::Gone.into()));
    }
}

#[test]
fn ignored_set_value_with_preexisting_words_is_unconfirmed_and_terminal() {
    let mut bound = Some(FakeField {
        value: std::cell::RefCell::new("Hello big world".into()),
        ..field()
    });
    let answer = write_bound(&mut bound, |_| true, |field| {
        let intended = spliced(&field.value.borrow(), None, "big");
        // The provider reports SetValue success but leaves its value unchanged.
        Ok(if verified(&field.value.borrow(), &intended) { Written::Verified } else { Written::Unconfirmed })
    });
    assert_eq!(answer, Ok(Written::Unconfirmed));
    assert!(bound.is_none());
    assert_eq!(write_bound(&mut bound, |_| true, |_| panic!("an uncertain write cannot be retried")), Err(Refusal::Gone.into()));
}

#[test]
fn a_write_permit_expires_and_cancellation_is_shared_with_the_worker() {
    let live = permit();
    let worker = live.clone();
    assert_eq!(worker.check(), Ok(()));
    live.cancelled.store(true, Ordering::SeqCst);
    assert_eq!(worker.check(), Err(Refusal::Gone));

    let expired = WritePermit { expires: Instant::now() - Duration::from_millis(1), ..permit() };
    assert!(!expired.cancelled.load(Ordering::SeqCst), "expiry alone must prevent a write");
    assert_eq!(expired.check(), Err(Refusal::Gone));
    let boundary = WritePermit { expires: Instant::now(), ..permit() };
    assert_eq!(boundary.check(), Err(Refusal::Gone), "the expiry instant is already too late");
}

#[test]
fn a_target_that_changes_during_the_final_value_read_is_never_written() {
    use std::cell::Cell;

    for changed in [
        Facts { password: true, ..writable() },
        Facts { value_read_only: Some(true), ..writable() },
        Facts { enabled: false, ..writable() },
        Facts { foreground: Foreground::Other, ..writable() },
        Facts { window_alive: false, ..writable() },
        Facts { element_alive: false, ..writable() },
        Facts { value_read_only: None, ..writable() },
    ] {
        let current = Cell::new(writable());
        let checks = Cell::new(0);
        let writes = Cell::new(0);
        let mut bound = Some(field());
        let answer = write_bound(&mut bound, |_| true, |_| write_revalidated(
            "Hello",
            || {
                checks.set(checks.get() + 1);
                let facts = current.get();
                match refusal(&facts) {
                    Some(reason) => Err(Rejection::checked(reason, &facts)),
                    None => Ok(checks.get()),
                }
            },
            |pattern| {
                assert_eq!(*pattern, 1, "reread uses a freshly obtained pattern");
                current.set(changed);
                Ok("Hello".into())
            },
            || Ok(()),
            |_| { writes.set(writes.get() + 1); Written::Verified },
        ));
        let reason = refusal(&changed).unwrap();
        assert_eq!(answer, Err(Rejection::checked(reason, &changed)));
        assert_eq!(checks.get(), 2, "safety facts must be refreshed after the blocking reread");
        assert_eq!(writes.get(), 0);
        assert_eq!(bound, Some(field()), "a definitive pre-write refusal keeps the target");
    }
}

#[test]
fn a_changed_value_is_preserved_until_an_explicit_fresh_insertion() {
    use std::cell::Cell;

    let mut bound = Some(field());
    *bound.as_ref().unwrap().value.borrow_mut() = "Hello from the app".into();
    let writes = Cell::new(0);
    let answer = write_bound(&mut bound, |_| true, |field| write_revalidated(
        "Hello", // The earlier CurrentValue/TextPattern snapshot.
        || Ok(()),
        |_| Ok(field.value.borrow().clone()),
        || Ok(()),
        |_| { writes.set(writes.get() + 1); Written::Verified },
    ));
    assert_eq!(answer, Err(Refusal::ValueChanged.into()));
    assert_eq!(writes.get(), 0);
    assert_eq!(*bound.as_ref().unwrap().value.borrow(), "Hello from the app");

    assert_eq!(write_bound(&mut bound, |_| true, |field| {
        let current = field.value.borrow().clone();
        let next = spliced(&current, None, "world");
        write_revalidated(
            &current,
            || Ok(()),
            |_| Ok(field.value.borrow().clone()),
            || Ok(()),
            |_| {
                writes.set(writes.get() + 1);
                *field.value.borrow_mut() = next;
                assert_eq!(*field.value.borrow(), "Hello from the app world");
                Written::Verified
            },
        )
    }), Ok(Written::Verified));
    assert_eq!(writes.get(), 1);
    assert!(bound.is_none());
}

#[test]
fn a_revalidation_refusal_keeps_the_same_capture_and_original_deadline() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    assert_eq!(bound.as_mut().unwrap().offer("cap-current", expires, now), Ok(&"original field"));
    let answer = write_bound(&mut bound, |target| target.capture == "cap-current", |_| write_revalidated(
        "Hello",
        || Ok(()),
        |_| Ok("Changed by the app".into()),
        || Ok(()),
        |_| panic!("a changed field must be kept, never overwritten"),
    ));
    assert_eq!(answer, Err(Refusal::ValueChanged.into()));
    let target = bound.as_mut().unwrap();
    assert_eq!(target.capture, "cap-current");
    assert_eq!(target.expires, Some(expires));
    assert_eq!(target.offer("cap-current", expires + Duration::from_secs(120), expires), Err(Refusal::Gone));
    assert_eq!(target.expires, Some(expires), "a refused insertion cannot renew its target");
}

#[test]
fn unchanged_revalidation_writes_once_using_the_last_checked_pattern() {
    use std::cell::Cell;

    for outcome in [Written::Verified, Written::Unconfirmed, Written::Failed] {
        let mut bound = Some(field());
        let checks = Cell::new(0);
        let writes = Cell::new(0);
        let answer = write_bound(&mut bound, |_| true, |_| write_revalidated(
            "Hello",
            || { checks.set(checks.get() + 1); Ok(checks.get()) },
            |pattern| { assert_eq!(*pattern, 1); Ok("Hello".into()) },
            || Ok(()),
            |pattern| {
                assert_eq!(pattern, 2, "SetValue must not use the earlier pattern");
                writes.set(writes.get() + 1);
                outcome
            },
        ));
        assert_eq!(answer, Ok(outcome));
        assert_eq!(writes.get(), 1);
        assert!(bound.is_none(), "every attempted effect is terminal, including unknown");
    }
}

#[test]
fn cancellation_deadline_or_closed_reply_during_final_reads_prevents_the_effect() {
    use std::cell::{Cell, RefCell};

    // Each injection happens inside a read, with no sleep, live UIA, or wall
    // clock race. The same gate used in production follows every final read.
    for stop in ["cancel", "expiry", "reply-timeout"] {
        for stage in ["initial-check", "value", "last-check"] {
            let permit = permit();
            let now = Cell::new(permit.expires - Duration::from_secs(1));
            let (reply, answer) = tokio::sync::oneshot::channel::<()>();
            let answer = RefCell::new(answer);
            let interrupt = || match stop {
                "cancel" => permit.cancelled.store(true, Ordering::SeqCst),
                "expiry" => now.set(permit.expires),
                "reply-timeout" => answer.borrow_mut().close(),
                _ => unreachable!(),
            };
            let checks = Cell::new(0);
            let writes = Cell::new(0);
            let result = write_revalidated(
                "Hello",
                || {
                    checks.set(checks.get() + 1);
                    if (stage == "initial-check" && checks.get() == 1) || (stage == "last-check" && checks.get() == 2) {
                        interrupt();
                    }
                    Ok(())
                },
                |_| {
                    if stage == "value" { interrupt(); }
                    Ok("Hello".into())
                },
                || {
                    permit.check_at(now.get())?;
                    if reply.is_closed() { return Err(Refusal::Gone); }
                    Ok(())
                },
                |_| { writes.set(writes.get() + 1); Written::Verified },
            );
            assert_eq!(result, Err(Refusal::Gone.into()), "{stop} during {stage}");
            assert_eq!(writes.get(), 0, "{stop} during {stage}");
            assert_eq!(checks.get(), if stage == "last-check" { 2 } else { 1 });
        }
    }
}

#[test]
fn an_unreadable_final_value_is_a_definitive_no_write_refusal() {
    assert_eq!(write_revalidated(
        "Hello",
        || Ok(()),
        |_| Err(Refusal::Gone.into()),
        || Ok(()),
        |_| panic!("a failed reread must never call SetValue"),
    ), Err(Refusal::Gone.into()));
}

#[test]
fn an_abandoned_write_skips_readback_and_focus_but_remains_terminal() {
    for stop in ["cancel", "expiry", "closed-caller"] {
        let permit = permit();
        let now = if stop == "expiry" { permit.expires } else { Instant::now() };
        let (reply, mut answer) = tokio::sync::oneshot::channel::<()>();
        if stop == "cancel" { permit.cancelled.store(true, Ordering::SeqCst); }
        if stop == "closed-caller" { answer.close(); }
        let mut bound = Some(field());
        let result = write_bound(&mut bound, |_| true, |_| Ok(finish_write(
            77,
            || panic!("an abandoned SetValue needs no more blocking UIA reads"),
            || {
                permit.check_at(now)?;
                if reply.is_closed() { return Err(Refusal::Gone); }
                Ok(())
            },
            || panic!("an abandoned write cannot restore focus"),
            || panic!("an abandoned write cannot restore focus"),
        )));
        assert_eq!(result, Ok(Written::Unconfirmed), "{stop} cannot become a pre-write refusal");
        assert!(bound.is_none());
    }
}

#[test]
fn abandonment_during_readback_or_window_checks_prevents_focus_without_changing_the_write() {
    use std::cell::{Cell, RefCell};

    for stop in ["cancel", "expiry", "closed-caller"] {
        for stage in ["readback", "window-check"] {
            for verified in [false, true] {
                let permit = permit();
                let now = Cell::new(permit.expires - Duration::from_secs(1));
                let (reply, answer) = tokio::sync::oneshot::channel::<()>();
                let answer = RefCell::new(answer);
                let interrupt = || match stop {
                    "cancel" => permit.cancelled.store(true, Ordering::SeqCst),
                    "expiry" => now.set(permit.expires),
                    "closed-caller" => answer.borrow_mut().close(),
                    _ => unreachable!(),
                };
                let mut bound = Some(field());
                let result = write_bound(&mut bound, |_| true, |_| Ok(finish_write(
                    77,
                    || { if stage == "readback" { interrupt(); } verified },
                    || {
                        permit.check_at(now.get())?;
                        if reply.is_closed() { return Err(Refusal::Gone); }
                        Ok(())
                    },
                    || {
                        assert_eq!(stage, "window-check", "no window work after abandoned readback");
                        interrupt();
                        (Some(77), Foreground::Coucou)
                    },
                    || panic!("{stop} during {stage} must prevent the focus effect"),
                )));
                assert_eq!(result, Ok(if verified { Written::Verified } else { Written::Unconfirmed }));
                assert!(bound.is_none(), "a completed SetValue must never be retryable");
            }
        }
    }
}

#[test]
fn focus_returns_only_from_coucou_to_the_still_owned_window_after_readback() {
    use std::cell::Cell;

    for (pid, foreground, should_restore) in [
        (None, Foreground::Coucou, false), // Target window was destroyed.
        (Some(78), Foreground::Coucou, false), // Handle reused by another process.
        (Some(77), Foreground::Other, false), // User switched apps/windows.
        (Some(77), Foreground::Target, false), // No handoff is needed.
        (Some(77), Foreground::Coucou, true),
    ] {
        for verified in [false, true] {
            for focus_succeeded in [false, true] {
                let window = Cell::new((Some(77), Foreground::Coucou));
                let restores = Cell::new(0);
                let mut bound = Some(field());
                let result = write_bound(&mut bound, |_| true, |_| Ok(finish_write(
                    77,
                    || { window.set((pid, foreground)); verified },
                    || Ok(()),
                    || window.get(),
                    || { restores.set(restores.get() + 1); focus_succeeded },
                )));
                assert_eq!(restores.get(), usize::from(should_restore));
                assert_eq!(result, Ok(if verified { Written::Verified } else { Written::Unconfirmed }),
                    "an optional focus handoff cannot change the attempted write outcome");
                assert!(bound.is_none());
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_caller_timeout_during_readback_never_restores_focus_when_the_worker_resumes() {
    use std::future::Future;
    use std::task::Poll;

    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let args = InsertTextArgs { text: "world".into(), target_window: 4242, target_pid: 77 };
    let mut writing = Box::pin(dictation.write("cap-current", args, permit()));
    std::future::poll_fn(|cx| {
        assert!(writing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    let Job::Write(_, _, permit, reply) = jobs.try_recv().unwrap() else { panic!("expected a write job") };
    let (started, readback_started) = tokio::sync::oneshot::channel();
    let (resume, resumed) = mpsc::channel();
    let (finished, worker_finished) = tokio::sync::oneshot::channel();
    let worker = std::thread::spawn(move || {
        let mut bound = Some(field());
        let outcome = write_bound(&mut bound, |_| true, |field| {
            *field.value.borrow_mut() = "Hello world".into(); // SetValue already applied.
            Ok(finish_write(
                77,
                || {
                    started.send(()).unwrap();
                    resumed.recv().unwrap(); // A blocked CurrentValue, released after the caller times out.
                    verified(&field.value.borrow(), "Hello world")
                },
                || {
                    permit.check()?;
                    if reply.is_closed() { return Err(Refusal::Gone); }
                    Ok(())
                },
                || panic!("a timed-out caller no longer owns any focus handoff"),
                || panic!("the late worker must not steal focus"),
            ))
        });
        assert_eq!(outcome, Ok(Written::Verified));
        assert!(bound.is_none());
        assert!(reply.send(outcome).is_err(), "late evidence cannot replace the caller's unknown result");
        finished.send(()).unwrap();
    });
    readback_started.await.unwrap();
    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(writing.await, Ok(Written::Unconfirmed));
    resume.send(()).unwrap();
    worker_finished.await.unwrap();
    worker.join().unwrap();
    assert!(jobs.try_recv().is_err(), "a timed-out write cannot be retried automatically");
}

#[tokio::test]
async fn an_offer_queues_the_exact_native_deadline_without_restarting_it() {
    use std::future::Future;
    use std::task::Poll;

    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let expires = Instant::now() + Duration::from_secs(10);
    let mut offering = Box::pin(dictation.offer("cap-current", expires));
    std::future::poll_fn(|cx| {
        assert!(offering.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    let Job::Offer(capture, queued_deadline, reply) = jobs.try_recv().unwrap() else {
        panic!("expected an offer job");
    };
    assert_eq!(capture, "cap-current");
    assert_eq!(queued_deadline, expires);
    let refusal = Rejection::checked(Refusal::FocusMoved, &Facts { foreground: Foreground::Other, ..writable() });
    reply.send(Err(refusal)).unwrap();
    assert_eq!(offering.await, Err(refusal));
}

#[tokio::test(start_paused = true)]
async fn an_offer_timeout_cannot_allow_copy_even_if_a_late_check_would() {
    use std::future::Future;
    use std::task::Poll;

    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut offering = Box::pin(dictation.offer("cap-current", permit().expires));
    std::future::poll_fn(|cx| {
        assert!(offering.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    let Job::Offer(_, _, reply) = jobs.try_recv().unwrap() else {
        panic!("expected an offer job");
    };
    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(offering.await, Err(Refusal::Busy.into()));
    assert!(reply.is_closed());
    let late = Rejection::checked(Refusal::FocusMoved, &writable());
    assert!(late.copy_allowed);
    assert!(reply.send(Err(late)).is_err(), "a late answer cannot replace the failed-closed timeout");
}

#[tokio::test]
async fn an_unavailable_worker_cannot_classify_the_field_as_safe_to_copy() {
    let (send, jobs) = mpsc::channel();
    drop(jobs);
    let dictation = Dictation::new(send);
    assert_eq!(dictation.offer("cap-current", permit().expires).await, Err(Refusal::Busy.into()));
}

#[tokio::test(start_paused = true)]
async fn a_write_reply_timeout_is_terminal_and_closes_the_late_workers_permission() {
    use std::future::Future;
    use std::task::Poll;

    // Inject only a job channel: no desktop, COM thread, or real field.
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let permit = permit();
    let cancelled = permit.cancelled.clone();
    let args = InsertTextArgs { text: "Only once".into(), target_window: 4242, target_pid: 77 };
    let mut writing = Box::pin(dictation.write("cap-current", args.clone(), permit));
    // Poll once to enqueue the job and register its timeout without waiting
    // on a blocking receiver or relying on which spawned task runs first.
    std::future::poll_fn(|cx| {
        assert!(writing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    let Job::Write(capture, queued_args, worker_permit, reply) = jobs.try_recv().unwrap() else {
        panic!("expected a write job");
    };
    assert_eq!(capture, "cap-current");
    assert_eq!(queued_args, args);
    assert!(Arc::ptr_eq(&worker_permit.cancelled, &cancelled));
    assert_eq!(worker_permit.check(), Ok(()));
    assert!(!reply.is_closed());

    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(writing.await, Ok(Written::Unconfirmed), "a timeout cannot prove that nothing was written");
    assert!(reply.is_closed(), "a queued job sees that its caller already timed out");
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(worker_permit.check(), Err(Refusal::Gone), "a late worker has no permission to write");
    assert_eq!(reply.send(Ok(Written::Verified)), Err(Ok(Written::Verified)), "a late success cannot resurrect this attempt");
    assert!(jobs.try_recv().is_err(), "the timeout never queues an automatic retry");
}

#[tokio::test]
async fn nothing_remembered_means_no_offer_and_no_write() {
    let dictation = Dictation::start(|_| {});
    // No probe or target binding is needed to refuse an unknown capture.
    let offer = dictation.offer("cap-none", permit().expires).await;
    assert!(matches!(offer, Ok(None) | Err(Rejection { reason: Refusal::Unsupported, copy_allowed: false })));
    let args = InsertTextArgs { text: "hi".into(), target_window: 1, target_pid: 1 };
    assert!(dictation.write("cap-none", args, permit()).await.is_err());
}

/// A WinForms text box in a separate PowerShell process: the field a user
/// would dictate into. `password` makes it a password box.
#[cfg(windows)]
fn text_box(password: bool) -> std::process::Child {
    let script = format!(
        r#"Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form
$f.Text = 'Coucou dictation test'
$f.TopMost = $true
$t = New-Object Windows.Forms.TextBox
$t.Width = 300
$t.Text = 'Hello world'
$t.UseSystemPasswordChar = {password}
$f.Controls.Add($t)
$f.Add_Shown({{ $f.Activate(); $t.Focus(); $t.SelectionStart = 6; $t.SelectionLength = 0 }})
[Windows.Forms.Application]::Run($f)"#,
        password = if password { "$true" } else { "$false" },
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .spawn()
        .expect("powershell")
}

/// Waits for the test box to have the keyboard; false if Windows kept it
/// elsewhere (focus rules), in which case the test can't run here.
#[cfg(windows)]
fn has_keyboard(pid: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner)) };
        if owner == pid {
            std::thread::sleep(Duration::from_millis(500));
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Opt-in: shows a small window on this computer for a few seconds.
#[cfg(windows)]
#[tokio::test]
#[ignore]
async fn words_go_into_a_real_text_box_at_its_caret_and_are_read_back() {
    let mut child = text_box(false);
    if !has_keyboard(child.id()) {
        let _ = child.kill();
        println!("skipped: the test window never got the keyboard");
        return;
    }
    let dictation = Dictation::start(|line| println!("{line}"));
    dictation.toggle_capture(|| Some("cap-live".into())).await;
    let offer = dictation.offer("cap-live", permit().expires).await;
    let result = match offer {
        // Never write anywhere but the test's own box.
        Ok(Some(offer)) if offer.target_pid == child.id() => {
            println!("offered: {}", offer.label);
            let args = InsertTextArgs { text: "big".into(), target_window: offer.target_window, target_pid: offer.target_pid };
            dictation.write("cap-live", args, permit()).await
        }
        other => {
            let _ = child.kill();
            panic!("unexpected offer: {other:?}");
        }
    };
    let _ = child.kill();
    assert_eq!(result, Ok(Written::Verified));
}

#[cfg(windows)]
#[tokio::test]
#[ignore]
async fn a_real_password_box_is_never_offered() {
    let mut child = text_box(true);
    if !has_keyboard(child.id()) {
        let _ = child.kill();
        println!("skipped: the test window never got the keyboard");
        return;
    }
    let dictation = Dictation::start(|line| println!("{line}"));
    dictation.toggle_capture(|| Some("cap-live".into())).await;
    let offer = dictation.offer("cap-live", permit().expires).await;
    let _ = child.kill();
    assert_eq!(offer, Err(Refusal::Protected.into()));
}

#[test]
fn only_an_absent_binding_has_no_offer() {
    let now = Instant::now();
    let mut bound: Option<Remembered<()>> = None;
    assert_eq!(offer_bound(&mut bound, "cap-none", now, now, |_| {
        panic!("there is no remembered field to check");
    }), Ok(None));
}

#[test]
fn another_captures_offer_is_refused_without_checking_or_expiring_the_owned_field() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    assert_eq!(offer_bound(&mut bound, "cap-old", now, now, |_| {
        panic!("another capture must not inspect this field");
    }), Err(Refusal::Gone.into()));
    let target = bound.as_mut().unwrap();
    assert_eq!(target.expires, None);
    assert_eq!(target.offer("cap-current", expires, now), Ok(&"original field"));
    assert_eq!(offer_bound(&mut bound, "cap-old", now, now, |_| {
        panic!("another capture must not inspect this field");
    }), Err(Refusal::Gone.into()));
    assert_eq!(bound.as_ref().unwrap().expires, Some(expires));
    assert_eq!(bound.as_ref().unwrap().field, Some("original field"));
}

#[test]
fn a_checked_vanished_window_or_element_keeps_its_offer_refusal_without_copy_permission() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    for facts in [Facts { window_alive: false, ..writable() }, Facts { element_alive: false, ..writable() }] {
        let mut bound = Some(Remembered::new("cap-current".into(), facts));
        for _ in 0..2 {
            let answer = offer_bound(&mut bound, "cap-current", expires, now, |facts| {
                Err(Rejection::checked(refusal(facts).unwrap(), facts))
            });
            assert_eq!(answer, Err(Rejection { reason: Refusal::Gone, copy_allowed: false }));
            assert!(bound.is_some(), "a vanished field is not an absent initial target");
        }
    }
}

#[test]
fn expiry_releases_the_field_but_keeps_a_refused_offer_for_its_capture() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    let expected = Offer { label: "Test app".into(), target_window: 4242, target_pid: 77 };
    assert_eq!(offer_bound(&mut bound, "cap-current", expires, now, |_| Ok(expected.clone())), Ok(Some(expected)));
    // The worker releases the UIA element at expiry without losing its owner.
    bound.as_mut().unwrap().expire(expires);
    assert_eq!(bound.as_ref().unwrap().field, None);
    for checked_at in [expires, expires + Duration::from_secs(60)] {
        assert_eq!(offer_bound(&mut bound, "cap-current", checked_at + Duration::from_secs(120), checked_at, |_| {
            panic!("an expired field must not be checked or revived");
        }), Err(Rejection { reason: Refusal::Gone, copy_allowed: false }));
        assert_eq!(bound.as_ref().unwrap().expires, Some(expires));
    }
}

#[test]
fn a_first_offer_that_arrives_expired_is_gone_rather_than_absent() {
    let now = Instant::now();
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    assert_eq!(offer_bound(&mut bound, "cap-current", now, now, |_| {
        panic!("an already expired field must not be checked");
    }), Err(Refusal::Gone.into()));
    assert_eq!(bound.as_ref().unwrap().field, None);
    assert_eq!(bound.as_ref().unwrap().capture, "cap-current");
}

#[tokio::test]
async fn queued_offers_preserve_the_difference_between_absent_and_gone() {
    use std::future::Future;
    use std::task::Poll;

    for answer in [Ok(None), Err(Refusal::Gone.into())] {
        let (send, jobs) = mpsc::channel();
        let dictation = Dictation::new(send);
        let mut offering = Box::pin(dictation.offer("cap-current", permit().expires));
        std::future::poll_fn(|cx| {
            assert!(offering.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let Job::Offer(_, _, reply) = jobs.try_recv().unwrap() else {
            panic!("expected an offer job");
        };
        reply.send(answer.clone()).unwrap();
        assert_eq!(offering.await, answer);
    }
}

#[test]
fn a_stop_binding_cannot_revive_an_expired_field_but_a_new_capture_can_replace_it() {
    let now = Instant::now();
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    assert_eq!(bound.as_mut().unwrap().offer("cap-current", now, now), Err(Refusal::Gone));
    assert!(!bind_capture(&mut bound, Some("cap-current".into()), Some("replacement field")));
    assert_eq!(bound.as_ref().unwrap().field, None);
    assert_eq!(offer_bound(&mut bound, "cap-current", now + Duration::from_secs(120), now, |_| {
        panic!("a stop press cannot revive an expired remembered target");
    }), Err(Refusal::Gone.into()));
    assert!(bind_capture(&mut bound, Some("cap-new".into()), Some("new field")));
    assert_eq!(bound.as_mut().unwrap().offer("cap-new", now + Duration::from_secs(120), now), Ok(&"new field"));
}

#[test]
fn an_absent_initial_target_keeps_its_capture_across_offers_and_stop_bindings() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound: Option<Remembered<&str>> = None;
    assert!(!bind_capture(&mut bound, Some("cap-empty".into()), None));
    assert_eq!(bound.as_ref().unwrap().capture, "cap-empty");
    for checked_at in [now, expires, expires + Duration::from_secs(120)] {
        assert_eq!(offer_bound(&mut bound, "cap-empty", expires, checked_at, |_| {
            panic!("an initially absent field cannot be checked");
        }), Ok(None));
        bound.as_mut().unwrap().expire(checked_at);
        assert_eq!(bound.as_ref().unwrap().expires, None, "absence cannot become an expired target");
    }
    assert!(!bind_capture(&mut bound, Some("cap-empty".into()), Some("different field")));
    assert_eq!(bound.as_ref().unwrap().field, None, "a stop press cannot adopt a field that was not captured");
    assert_eq!(offer_bound(&mut bound, "cap-empty", expires, now, |_| {
        panic!("the stop press must leave the capture without a field");
    }), Ok(None));
    assert_eq!(offer_bound(&mut bound, "cap-old", expires, now, |_| {
        panic!("a different capture must not inspect this binding");
    }), Err(Refusal::Gone.into()));
    assert_eq!(bound.as_ref().unwrap().capture, "cap-empty");
    assert_eq!(bound.as_ref().unwrap().expires, None);
    assert!(bind_capture(&mut bound, Some("cap-new".into()), Some("new field")));
    assert_eq!(bound.as_mut().unwrap().offer("cap-new", expires, now), Ok(&"new field"));
}

#[test]
fn a_new_capture_without_a_field_replaces_an_expired_capture_with_initial_absence() {
    let now = Instant::now();
    let mut bound = Some(Remembered::new("cap-expired".into(), "old field"));
    assert_eq!(bound.as_mut().unwrap().offer("cap-expired", now, now), Err(Refusal::Gone));
    assert!(!bind_capture(&mut bound, Some("cap-empty".into()), None));
    assert_eq!(offer_bound(&mut bound, "cap-empty", now + Duration::from_secs(120), now, |_| {
        panic!("the new capture never had a field");
    }), Ok(None));
    assert_eq!(offer_bound(&mut bound, "cap-expired", now, now, |_| {
        panic!("the old capture cannot inspect its replacement");
    }), Err(Refusal::Gone.into()));
}

#[test]
fn copy_reads_fresh_password_evidence_after_element_identity() {
    use std::cell::{Cell, RefCell};
    let password = Cell::new(false);
    let reads = RefCell::new(Vec::new());
    // A cached read-only/unsupported offer said non-password. The field
    // switches while the new check is reading its identity.
    assert_eq!(copy_revalidated(
        || { reads.borrow_mut().push("identity"); password.set(true); Ok(true) },
        || { reads.borrow_mut().push("password"); Ok(password.get()) },
        || { reads.borrow_mut().push("window"); true },
    ), Err(Refusal::Protected));
    assert_eq!(*reads.borrow(), vec!["identity", "password", "window"]);
}

#[test]
fn copy_requires_live_identity_and_a_successful_negative_password_read() {
    for (identity, password, window, expected) in [
        (Ok(true), Ok(false), true, Ok(())),
        (Ok(true), Ok(true), true, Err(Refusal::Protected)),
        (Ok(false), Ok(false), true, Err(Refusal::Gone)),
        (Ok(true), Ok(false), false, Err(Refusal::Gone)),
        (Err(Refusal::Busy), Ok(false), true, Err(Refusal::Busy)),
        (Ok(true), Err(Refusal::Busy), true, Err(Refusal::Busy)),
    ] {
        assert_eq!(copy_revalidated(|| identity, || password, || window), expected);
    }
}

#[test]
fn copy_keeps_the_same_target_and_original_deadline_without_writing() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    for answer in [Ok(()), Err(Refusal::Protected), Err(Refusal::Busy), Ok(())] {
        assert_eq!(copy_bound(&mut bound, "cap-current", expires, || now, |field| {
            assert_eq!(*field, "original field");
            answer
        }), answer);
        assert_eq!(bound.as_ref().unwrap().field, Some("original field"));
        assert_eq!(bound.as_ref().unwrap().expires, Some(expires));
    }
    assert_eq!(copy_bound(&mut bound, "cap-old", now, || now, |_| panic!("stale capture")), Err(Refusal::Gone));
    assert_eq!(bound.as_ref().unwrap().expires, Some(expires));
    assert_eq!(copy_bound(&mut bound, "cap-current", expires + Duration::from_secs(120), || expires,
        |_| panic!("an expired field is not checked")), Err(Refusal::Gone));
    assert_eq!(bound.as_ref().unwrap().expires, Some(expires));
    assert!(bound.as_ref().unwrap().field.is_none());
}

#[test]
fn a_copy_check_cannot_outlive_its_deadline_while_reading_the_field() {
    let now = std::cell::Cell::new(Instant::now());
    let expires = now.get() + Duration::from_secs(120);
    let mut bound = Some(Remembered::new("cap-current".into(), "original field"));
    assert_eq!(copy_bound(&mut bound, "cap-current", expires, || now.get(), |_| {
        now.set(expires);
        Ok(())
    }), Err(Refusal::Gone));
    assert!(bound.as_ref().unwrap().field.is_none());
}

#[test]
fn copy_never_adopts_an_absent_or_another_captures_target() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    for mut bound in [
        None,
        Some(Remembered { capture: "cap-current".into(), field: None, expires: None }),
        Some(Remembered::new("cap-new".into(), "new field")),
    ] {
        assert_eq!(copy_bound(&mut bound, "cap-current", expires, || now,
            |_| panic!("there is no owned target")), Err(Refusal::Gone));
        if let Some(target) = bound.filter(|target| target.capture == "cap-new") {
            assert_eq!(target.field, Some("new field"));
            assert_eq!(target.expires, None);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_copy_check_timeout_discards_late_positive_evidence() {
    use std::future::Future;
    use std::task::Poll;
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let expires = permit().expires;
    let mut checking = Box::pin(dictation.check_copy("cap-current", expires));
    std::future::poll_fn(|cx| {
        assert!(checking.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    let Job::CheckCopy(capture, deadline, reply) = jobs.try_recv().unwrap() else {
        panic!("only a read-only check may be queued");
    };
    assert_eq!(capture, "cap-current");
    assert_eq!(deadline, expires);
    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(checking.await, Err(Refusal::Busy));
    assert!(reply.is_closed());
    assert!(reply.send(Ok(())).is_err());
    assert!(jobs.try_recv().is_err(), "no write, grant, or automatic retry");
}

#[tokio::test]
async fn an_unavailable_copy_worker_fails_closed() {
    let (send, jobs) = mpsc::channel();
    drop(jobs);
    let dictation = Dictation::new(send);
    assert_eq!(dictation.check_copy("cap-current", permit().expires).await, Err(Refusal::Busy));
}

#[test]
fn checking_copy_preserves_an_owned_absent_target_across_offers_and_stop() {
    let now = Instant::now();
    let expires = now + Duration::from_secs(120);
    let mut bound: Option<Remembered<&str>> = None;
    assert!(!bind_capture(&mut bound, Some("cap-absent".into()), None));
    for capture in ["cap-stale", "cap-absent", "cap-absent"] {
        assert_eq!(copy_bound(&mut bound, capture, expires, || now,
            |_| panic!("an absent target cannot be checked")), Err(Refusal::Gone));
        let target = bound.as_ref().unwrap();
        assert_eq!(target.capture, "cap-absent");
        assert!(target.field.is_none());
        assert!(target.expires.is_none(), "a Copy check cannot invent an expiry marker");
        assert_eq!(offer_bound(&mut bound, "cap-absent", expires, now,
            |_| panic!("an absent target cannot be offered")), Ok(None));
    }
    assert!(!bind_capture(&mut bound, Some("cap-absent".into()), Some("field focused on stop")));
    assert_eq!(offer_bound(&mut bound, "cap-absent", expires, now,
        |_| panic!("stop cannot replace the absent target")), Ok(None));
}

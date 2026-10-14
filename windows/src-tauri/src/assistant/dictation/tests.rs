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
    for reason in [Refusal::FocusMoved, Refusal::Gone, Refusal::Protected, Refusal::ReadOnly, Refusal::Unsupported, Refusal::Busy] {
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
    for reason in [Refusal::Gone, Refusal::Protected, Refusal::Busy] {
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
    for why in [Refusal::FocusMoved, Refusal::Gone, Refusal::Protected, Refusal::ReadOnly, Refusal::Unsupported, Refusal::Busy] {
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

#[tokio::test]
async fn an_offer_queues_the_exact_native_deadline_without_restarting_it() {
    use std::future::Future;
    use std::task::Poll;

    let (send, jobs) = mpsc::channel();
    let dictation = Dictation { jobs: Mutex::new(send) };
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
    let dictation = Dictation { jobs: Mutex::new(send) };
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
    let dictation = Dictation { jobs: Mutex::new(send) };
    assert_eq!(dictation.offer("cap-current", permit().expires).await, Err(Refusal::Busy.into()));
}

#[tokio::test(start_paused = true)]
async fn a_write_reply_timeout_is_terminal_and_closes_the_late_workers_permission() {
    use std::future::Future;
    use std::task::Poll;

    // Inject only a job channel: no desktop, COM thread, or real field.
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation { jobs: Mutex::new(send) };
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
    dictation.bind(Some("cap-none".into()));
    assert!(dictation.offer("cap-none", permit().expires).await.is_err());
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
    dictation.probe().await;
    dictation.bind(Some("cap-live".into()));
    let offer = dictation.offer("cap-live", permit().expires).await;
    let result = match offer {
        // Never write anywhere but the test's own box.
        Ok(offer) if offer.target_pid == child.id() => {
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
    dictation.probe().await;
    dictation.bind(Some("cap-live".into()));
    let offer = dictation.offer("cap-live", permit().expires).await;
    let _ = child.kill();
    assert_eq!(offer, Err(Refusal::Protected.into()));
}

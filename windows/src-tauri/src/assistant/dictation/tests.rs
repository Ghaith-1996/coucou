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
}

#[test]
fn read_only_disabled_or_patternless_fields_are_refused() {
    assert_eq!(refusal(&Facts { value_read_only: Some(true), ..writable() }), Some(Refusal::ReadOnly));
    assert_eq!(refusal(&Facts { enabled: false, ..writable() }), Some(Refusal::ReadOnly));
    assert_eq!(refusal(&Facts { value_read_only: None, ..writable() }), Some(Refusal::Unsupported));
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
fn evidence_is_the_words_read_back() {
    assert!(verified("Hello big world", " big "));
    assert!(!verified("Hello world", "big"));
    assert!(!verified("anything", "   "));
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
        assert_eq!(answer, Err(Refusal::Gone));
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
        Err(Refusal::Gone),
    );
}

#[test]
fn prewrite_refusals_keep_the_exact_field_for_a_later_success() {
    for why in [Refusal::FocusMoved, Refusal::Gone, Refusal::Protected, Refusal::ReadOnly, Refusal::Unsupported, Refusal::Busy] {
        let mut bound = Some(field());
        assert_eq!(write_bound(&mut bound, |_| true, |_| Err(why)), Err(why));
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
        assert_eq!(write_bound(&mut bound, |_| true, |_| panic!("a completed insertion is never retried")), Err(Refusal::Gone));
        assert_eq!(writes.get(), 1);
    }
}

#[test]
fn every_attempted_write_outcome_consumes_the_field() {
    for outcome in [Written::Verified, Written::Unconfirmed, Written::Failed] {
        let mut bound = Some(field());
        assert_eq!(write_bound(&mut bound, |_| true, |_| Ok(outcome)), Ok(outcome));
        assert!(bound.is_none(), "{outcome:?} is terminal, even without verified success");
        assert_eq!(write_bound(&mut bound, |_| true, |_| panic!("a terminal outcome cannot write twice")), Err(Refusal::Gone));
    }
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
    assert!(dictation.offer("cap-none").await.is_err());
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
    let offer = dictation.offer("cap-live").await;
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
    let offer = dictation.offer("cap-live").await;
    let _ = child.kill();
    assert_eq!(offer, Err(Refusal::Protected));
}

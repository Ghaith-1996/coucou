use super::*;
use std::cell::Cell;
use std::future::Future;
use std::task::Poll;

async fn pending(mut future: std::pin::Pin<&mut impl Future>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
}

fn finish_probe<T>(jobs: &mpsc::Receiver<Job>, targets: &mut CaptureTargets<T>, field: Option<T>) {
    let Job::Probe(id, reply) = jobs.try_recv().unwrap() else { panic!("expected a probe") };
    targets.probe(id, reply, || field);
}

fn finish_bind<T>(jobs: &mpsc::Receiver<Job>, targets: &mut CaptureTargets<T>) -> bool {
    let Job::Bind { probe, answered, capture } = jobs.try_recv().unwrap() else { panic!("expected a binding") };
    targets.bind(probe, answered, capture)
}

fn assert_absent(targets: &mut CaptureTargets<&str>, capture: &str) {
    let now = Instant::now();
    assert_eq!(offer_bound(&mut targets.bound, capture, now + Duration::from_secs(120), now,
        |_| panic!("an absent initial target must not be offered")), Ok(None));
    assert_eq!(copy_bound(&mut targets.bound, capture, now + Duration::from_secs(120), || now,
        |_| panic!("an absent initial target must not authorize Copy")), Err(Refusal::Gone));
}

#[tokio::test(start_paused = true)]
async fn a_queued_probe_timeout_starts_capture_without_sampling_or_adopting_an_old_field() {
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut targets = CaptureTargets::new();
    // A previous caller received its probe, but was cancelled before it bound.
    let mut cancelled = Box::pin(dictation.toggle_capture(|| panic!("cancelled start")));
    pending(cancelled.as_mut()).await;
    finish_probe(&jobs, &mut targets, Some("previous snapshot"));
    drop(cancelled);

    let started = Cell::new(false);
    let mut starting = Box::pin(dictation.toggle_capture(|| {
        started.set(true);
        Some("cap-timeout".into())
    }));
    pending(starting.as_mut()).await;
    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(starting.await.as_deref(), Some("cap-timeout"));
    assert!(started.get(), "target discovery must not prevent voice capture");
    let Job::Probe(id, reply) = jobs.try_recv().unwrap() else { panic!("expected a queued probe") };
    assert!(reply.is_closed());
    targets.probe(id, reply, || panic!("the queued timeout must not sample the newly focused field"));
    assert!(targets.pending.is_none(), "skipping a probe also discards prior pending data");
    assert!(!finish_bind(&jobs, &mut targets));
    assert_absent(&mut targets, "cap-timeout");

    // A stop press in a different app cannot fill that deliberately absent target.
    let mut stopping = Box::pin(dictation.toggle_capture(|| Some("cap-timeout".into())));
    pending(stopping.as_mut()).await;
    finish_probe(&jobs, &mut targets, Some("different field"));
    assert_eq!(stopping.await.as_deref(), Some("cap-timeout"));
    assert!(!finish_bind(&jobs, &mut targets));
    assert_absent(&mut targets, "cap-timeout");
}

#[tokio::test(start_paused = true)]
async fn a_probe_that_blocks_past_its_reply_timeout_discards_the_late_field() {
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut starting = Box::pin(dictation.toggle_capture(|| Some("cap-timeout".into())));
    pending(starting.as_mut()).await;
    let Job::Probe(id, reply) = jobs.try_recv().unwrap() else { panic!("expected a probe") };
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (resume, resumed) = mpsc::channel();
    let (finished, finishing) = tokio::sync::oneshot::channel();
    let worker = std::thread::spawn(move || {
        let mut targets = CaptureTargets::new();
        targets.probe(id, reply, || {
            entered.send(()).unwrap();
            resumed.recv().unwrap();
            Some("field reached after capture began")
        });
        assert!(targets.pending.is_none(), "the blocking provider's late result is discarded");
        assert!(!finish_bind(&jobs, &mut targets));
        assert_absent(&mut targets, "cap-timeout");
        finished.send(()).unwrap();
    });
    entering.await.unwrap();
    tokio::time::advance(REPLY_TIMEOUT).await;
    assert_eq!(starting.await.as_deref(), Some("cap-timeout"));
    resume.send(()).unwrap();
    finishing.await.unwrap();
    worker.join().unwrap();
}

#[tokio::test]
async fn concurrent_toggles_cannot_queue_a_second_probe_before_the_first_capture_is_bound() {
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut targets = CaptureTargets::new();
    let mut first = Box::pin(dictation.toggle_capture(|| Some("cap-original".into())));
    let mut stop = Box::pin(dictation.toggle_capture(|| Some("cap-original".into())));
    pending(first.as_mut()).await;
    pending(stop.as_mut()).await;
    finish_probe(&jobs, &mut targets, Some("original field"));
    assert!(jobs.try_recv().is_err(), "the second toggle is still waiting for ownership");
    assert_eq!(first.await.as_deref(), Some("cap-original"));
    pending(stop.as_mut()).await;
    assert!(finish_bind(&jobs, &mut targets), "the first bind precedes every later probe");
    finish_probe(&jobs, &mut targets, Some("field focused on stop"));
    assert_eq!(stop.await.as_deref(), Some("cap-original"));
    assert!(!finish_bind(&jobs, &mut targets));
    assert_eq!(targets.bound.as_ref().unwrap().field, Some("original field"));
    assert!(targets.pending.is_none());
}

#[tokio::test]
async fn cancelling_a_queued_start_releases_coordination_and_cannot_start_or_bind() {
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut targets = CaptureTargets::new();
    let mut cancelled = Box::pin(dictation.toggle_capture(|| panic!("cancelled start")));
    pending(cancelled.as_mut()).await;
    drop(cancelled);
    let mut next = Box::pin(dictation.toggle_capture(|| Some("cap-next".into())));
    pending(next.as_mut()).await;
    let Job::Probe(id, reply) = jobs.try_recv().unwrap() else { panic!("expected the cancelled probe") };
    targets.probe(id, reply, || panic!("a cancelled caller cannot probe"));
    finish_probe(&jobs, &mut targets, Some("next field"));
    assert_eq!(next.await.as_deref(), Some("cap-next"));
    assert!(finish_bind(&jobs, &mut targets));
    assert_eq!(targets.bound.as_ref().unwrap().field, Some("next field"));
    assert!(jobs.try_recv().is_err(), "the cancelled start queued no bind");
}

#[tokio::test]
async fn a_failed_capture_start_discards_its_successful_probe_without_touching_the_current_capture() {
    let (send, jobs) = mpsc::channel();
    let dictation = Dictation::new(send);
    let mut targets = CaptureTargets::new();
    targets.bound = Some(Remembered::new("cap-current".into(), "original field"));
    let mut failed = Box::pin(dictation.toggle_capture(|| None));
    pending(failed.as_mut()).await;
    finish_probe(&jobs, &mut targets, Some("unused field"));
    assert_eq!(failed.await, None);
    assert!(!finish_bind(&jobs, &mut targets));
    assert!(targets.pending.is_none());
    assert_eq!(targets.bound.as_ref().unwrap().field, Some("original field"));

    let mut next = Box::pin(dictation.toggle_capture(|| Some("cap-next".into())));
    pending(next.as_mut()).await;
    finish_probe(&jobs, &mut targets, None);
    assert_eq!(next.await.as_deref(), Some("cap-next"));
    assert!(!finish_bind(&jobs, &mut targets));
    assert_absent(&mut targets, "cap-next");
}

#[tokio::test]
async fn an_unavailable_probe_worker_does_not_prevent_capture() {
    let (send, jobs) = mpsc::channel();
    drop(jobs);
    let dictation = Dictation::new(send);
    assert_eq!(dictation.toggle_capture(|| Some("cap-available".into())).await.as_deref(), Some("cap-available"));
}

#[test]
fn a_stale_bind_never_consumes_a_newer_probes_snapshot_or_replaces_a_newer_capture() {
    let mut targets = CaptureTargets::new();
    let (reply_a, _answer_a) = tokio::sync::oneshot::channel();
    targets.probe(1, reply_a, || Some("field A"));
    let (reply_b, _answer_b) = tokio::sync::oneshot::channel();
    targets.probe(2, reply_b, || Some("field B"));
    assert!(!targets.bind(1, true, Some("cap-A".into())));
    assert_absent(&mut targets, "cap-A");
    assert!(targets.bind(2, true, Some("cap-B".into())), "A did not consume B's pending field");
    for capture in [None, Some("cap-A".into()), Some("cap-B".into())] {
        assert!(!targets.bind(1, true, capture));
        assert_eq!(targets.bound.as_ref().unwrap().capture, "cap-B");
        assert_eq!(targets.bound.as_ref().unwrap().field, Some("field B"));
    }
    assert!(!targets.bind(2, false, Some("cap-stale".into())), "a settled attempt binds at most once");
    assert_eq!(targets.bound.as_ref().unwrap().capture, "cap-B");
}

#[test]
fn a_failed_stale_start_does_not_discard_another_probes_snapshot() {
    let mut targets = CaptureTargets::new();
    let (reply_a, _answer_a) = tokio::sync::oneshot::channel();
    targets.probe(1, reply_a, || Some("field A"));
    let (reply_b, _answer_b) = tokio::sync::oneshot::channel();
    targets.probe(2, reply_b, || Some("field B"));
    assert!(!targets.bind(1, true, None));
    assert!(targets.bind(2, true, Some("cap-B".into())));
    assert_eq!(targets.bound.as_ref().unwrap().field, Some("field B"));
}

#[test]
fn an_unanswered_binding_discards_even_a_matching_pending_snapshot() {
    let mut targets = CaptureTargets::new();
    let (reply, _answer) = tokio::sync::oneshot::channel();
    targets.probe(1, reply, || Some("unaccepted snapshot"));
    assert!(!targets.bind(1, false, Some("cap-timeout".into())));
    assert!(targets.pending.is_none());
    assert_absent(&mut targets, "cap-timeout");
}

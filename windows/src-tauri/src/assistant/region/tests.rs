use super::*;

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Image {
    Image { width, height, rgba: rgba.repeat((width * height) as usize) }
}

fn monitor() -> Monitor {
    Monitor { name: r"\\.\DISPLAY2".into(), bounds: rect(-1920, 0, 1920, 1080) }
}

#[test]
fn a_drag_is_clamped_to_its_monitor() {
    assert_eq!(clamp(rect(100, 50, 300, 200), (1920, 1080)), Some(rect(100, 50, 300, 200)));
    assert_eq!(clamp(rect(-40, -10, 100, 100), (1920, 1080)), Some(rect(0, 0, 60, 90)));
    assert_eq!(clamp(rect(1900, 1000, 500, 500), (1920, 1080)), Some(rect(1900, 1000, 20, 80)));
}

#[test]
fn a_click_or_a_drag_off_the_monitor_is_not_a_region() {
    assert_eq!(clamp(rect(10, 10, 3, 400), (1920, 1080)), None);
    assert_eq!(clamp(rect(10, 10, 400, MIN_SIDE - 1), (1920, 1080)), None);
    assert_eq!(clamp(rect(5000, 10, 400, 400), (1920, 1080)), None);
    assert_eq!(clamp(rect(i32::MAX, i32::MAX, u32::MAX, u32::MAX), (1920, 1080)), None);
}

#[test]
fn masks_black_out_only_what_they_cover() {
    let mut image = solid(4, 3, [200, 100, 50, 255]);
    apply_masks(&mut image, &[rect(1, 1, 2, 1), rect(3, 2, 10, 10), rect(-5, -5, 2, 2)]);
    let px = |x: u32, y: u32| &image.rgba[((y * 4 + x) * 4) as usize..((y * 4 + x) * 4 + 4) as usize];
    assert_eq!(px(0, 0), [200, 100, 50, 255]);
    assert_eq!(px(1, 1), [0, 0, 0, 255]);
    assert_eq!(px(2, 1), [0, 0, 0, 255]);
    assert_eq!(px(3, 1), [200, 100, 50, 255]);
    assert_eq!(px(3, 2), [0, 0, 0, 255], "clipped to the picture");
    assert_eq!(px(0, 2), [200, 100, 50, 255]);
}

#[test]
fn the_preview_is_shrunk_keeping_its_shape() {
    let big = solid(1600, 400, [10, 20, 30, 255]);
    let small = shrink(&big, 720);
    assert_eq!((small.width, small.height), (720, 180));
    assert_eq!(&small.rgba[..4], &[10, 20, 30, 255]);
    let tiny = solid(10, 10, [1, 2, 3, 255]);
    assert_eq!(shrink(&tiny, 720), tiny);
}

#[test]
fn shrinking_averages_what_it_drops() {
    // Two columns, black and white, become one grey pixel.
    let image = Image { width: 2, height: 1, rgba: vec![0, 0, 0, 255, 255, 255, 255, 255] };
    let one = shrink(&image, 1);
    assert_eq!((one.width, one.height), (1, 1));
    assert_eq!(one.rgba, vec![127, 127, 127, 255]);
}

#[test]
fn the_png_is_a_real_png_of_the_right_size() {
    let bytes = png(&solid(3, 2, [1, 2, 3, 255])).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let decoder = ::png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (3, 2));
    assert!(data_url(b"x").starts_with("data:image/png;base64,"));
}

#[test]
fn the_file_name_says_what_and_when() {
    let t = crate::platform::LocalTime { year: 2026, month: 10, day: 4, hour: 9, minute: 5, second: 7 };
    assert_eq!(file_name(&t), "Screen region 2026-10-04 09.05.07.png");
}

#[test]
fn a_pick_is_proposed_for_confirmation_then_used_once() {
    let regions = Regions::default();
    let id = regions.start(vec![monitor()]);
    assert_eq!(regions.waiting().as_deref(), Some(id.as_str()));
    let m = regions.monitor(&id, 0).unwrap();
    assert!(regions.monitor(&id, 1).is_none(), "no such monitor");
    assert!(regions.monitor("reg-other", 0).is_none());

    let preview = regions.picked(&id, &m, rect(10, 20, 30, 40), solid(30, 40, [9, 9, 9, 255])).unwrap();
    assert_eq!((preview.width, preview.height), (30, 40));
    assert!(preview.image.starts_with("data:image/png;base64,"));
    assert!(regions.waiting().is_none(), "picked: no more overlays");
    assert!(regions.monitor(&id, 0).is_none());
    assert_eq!(
        regions.picked(&id, &m, rect(0, 0, 30, 40), solid(30, 40, [0, 0, 0, 255])).unwrap_err(),
        ErrorCode::Expired,
        "one picture per pick",
    );

    let (action_id, image) = regions.confirm(&id, &preview.grant).unwrap();
    assert_eq!(action_id, preview.grant.action_id);
    assert_eq!((image.width, image.height), (30, 40));
    assert_eq!(regions.confirm(&id, &preview.grant).unwrap_err(), ErrorCode::Expired, "only once");
}

#[test]
fn a_changed_or_made_up_grant_attaches_nothing() {
    let regions = Regions::default();
    let id = regions.start(vec![monitor()]);
    let m = regions.monitor(&id, 0).unwrap();
    let preview = regions.picked(&id, &m, rect(0, 0, 30, 40), solid(30, 40, [9, 9, 9, 255])).unwrap();
    let forged = Grant { digest: "0".repeat(64), ..preview.grant.clone() };
    assert_eq!(regions.confirm(&id, &forged).unwrap_err(), ErrorCode::GrantMismatch);
    // Used once whatever the answer: the real grant is worthless now too.
    assert_eq!(regions.confirm(&id, &preview.grant).unwrap_err(), ErrorCode::Expired);

    let id = regions.start(vec![monitor()]);
    let m = regions.monitor(&id, 0).unwrap();
    let preview = regions.picked(&id, &m, rect(0, 0, 30, 40), solid(30, 40, [9, 9, 9, 255])).unwrap();
    let other = Grant { action_id: "act-0000000000000000".into(), ..preview.grant.clone() };
    assert_eq!(regions.confirm(&id, &other).unwrap_err(), ErrorCode::GrantMismatch);
}

#[test]
fn a_stale_session_cannot_take_a_newer_picture() {
    let regions = Regions::default();
    let old = regions.start(vec![monitor()]);
    let new = regions.start(vec![monitor()]);
    assert!(regions.monitor(&old, 0).is_none());
    let m = regions.monitor(&new, 0).unwrap();
    assert_eq!(regions.picked(&old, &m, rect(0, 0, 30, 40), solid(30, 40, [0; 4])).unwrap_err(), ErrorCode::Expired);
    let preview = regions.picked(&new, &m, rect(0, 0, 30, 40), solid(30, 40, [0; 4])).unwrap();
    assert_eq!(regions.confirm(&old, &preview.grant).unwrap_err(), ErrorCode::GrantMismatch);
    assert!(!regions.cancel(&old), "a stale cancel leaves the newer pick alone");
    assert!(regions.confirm(&new, &preview.grant).is_ok());
}

#[test]
fn cancel_drops_the_picture() {
    let regions = Regions::default();
    let id = regions.start(vec![monitor()]);
    let m = regions.monitor(&id, 0).unwrap();
    let preview = regions.picked(&id, &m, rect(0, 0, 30, 40), solid(30, 40, [0; 4])).unwrap();
    assert!(regions.cancel(&id));
    assert_eq!(regions.confirm(&id, &preview.grant).unwrap_err(), ErrorCode::Expired);
    assert!(!regions.cancel(&id));
}

#[test]
fn the_action_names_the_monitor_and_the_rectangle_on_it() {
    let regions = Regions::default();
    let id = regions.start(vec![monitor()]);
    let m = regions.monitor(&id, 0).unwrap();
    let preview = regions.picked(&id, &m, rect(10, 20, 30, 40), solid(30, 40, [0; 4])).unwrap();
    let guard = regions.0.lock().unwrap();
    let (action, _) = guard.as_ref().unwrap().picked.as_ref().unwrap();
    assert_eq!(action.kind, ActionKind::AskRegion);
    assert_eq!(action.intent_id, id);
    assert_eq!(
        action.args,
        serde_json::json!({ "monitor": r"\\.\DISPLAY2", "x": 10, "y": 20, "width": 30, "height": 40 }),
    );
    assert_eq!(action.id, preview.grant.action_id);
}

/// Opt-in: copies a corner of the real screen, without Coucou's windows.
#[cfg(windows)]
#[test]
#[ignore]
fn a_real_rectangle_of_the_screen_is_copied() {
    let image = capture::grab(rect(0, 0, 64, 48)).expect("a picture");
    assert_eq!((image.width, image.height), (64, 48));
    assert_eq!(image.rgba.len(), 64 * 48 * 4);
    assert!(image.rgba.chunks_exact(4).all(|p| p[3] == 255));
    println!("first pixel {:?}", &image.rgba[..4]);
}

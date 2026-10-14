use super::*;

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

#[tokio::test]
async fn nothing_remembered_means_no_offer_and_no_write() {
    let dictation = Dictation::start(|_| {});
    dictation.bind(Some("cap-none".into()));
    assert!(dictation.offer("cap-none").await.is_err());
    let args = InsertTextArgs { text: "hi".into(), target_window: 1, target_pid: 1 };
    assert!(dictation.write("cap-none", args).await.is_err());
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
            dictation.write("cap-live", args).await
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

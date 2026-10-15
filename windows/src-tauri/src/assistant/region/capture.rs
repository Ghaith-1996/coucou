// One rectangle of the screen, copied once with GDI. Coucou's own windows
// (the island, Settings, the desktop Mochi) are excluded from it for the moment of the
// copy (SetWindowDisplayAffinity), then returned to Coucou's normal capture policy.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowDisplayAffinity, WINDOW_DISPLAY_AFFINITY, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
};

use super::{Image, Rect};

/// Copies `rect` of the virtual screen (physical pixels). None when Windows
/// refuses (a secure desktop, say).
pub fn grab(rect: Rect) -> Option<Image> {
    let (w, h) = (rect.width as i32, rect.height as i32);
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let previous = SelectObject(memory, bitmap.into());
        // CAPTUREBLT includes layered windows (menus, tooltips, overlays).
        let copied = BitBlt(memory, 0, 0, w, h, Some(screen), rect.x, rect.y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative: rows from the top.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; rect.width as usize * rect.height as usize * 4];
        SelectObject(memory, previous);
        let rows = if copied {
            GetDIBits(memory, bitmap, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        if rows != h {
            return None;
        }
        // BGRA → RGBA; the screen has no alpha to keep.
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Some(Image { width: rect.width, height: rect.height, rgba: bgra })
    }
}

// Cancellation can retire a session while its blocking worker is still alive.
// Its replacement must not observe or restore that worker's temporary affinity.
static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// All required windows must be excluded before the supplied settle/copy step.
/// Serialize restoration too, including when setup fails or copying panics.
pub fn excluding(windows: impl IntoIterator<Item = HWND>, copy: impl FnOnce() -> Option<Image>) -> Option<Image> {
    // Coucou owns these windows and this is its only affinity writer. Keep the
    // existing WDA_NONE restoration policy: GetWindowDisplayAffinity requires
    // layered windows and would newly reject ordinary Settings windows, even
    // while hidden. Hidden windows are still excluded in case they become visible.
    with_excluded(&CAPTURE_LOCK, windows, |window, affinity| unsafe {
        SetWindowDisplayAffinity(window, WINDOW_DISPLAY_AFFINITY(affinity)).is_ok()
    }, copy)
}

/// The platform calls are injected so partial failures and overlapping workers
/// can be exercised without capturing a real desktop.
fn with_excluded<W: Copy, T>(
    lock: &Mutex<()>,
    windows: impl IntoIterator<Item = W>,
    set: impl Fn(W, u32) -> bool,
    copy: impl FnOnce() -> Option<T>,
) -> Option<T> {
    let _serial = lock.lock().ok()?;
    let mut excluded = Excluded { windows: Vec::new(), set: &set };
    for window in windows {
        // Record first: even a failed platform call is rolled back defensively.
        excluded.windows.push(window);
        if !set(window, WDA_EXCLUDEFROMCAPTURE.0) {
            return None;
        }
    }
    copy()
    // Local variables drop in reverse order: restore before releasing _serial.
}

struct Excluded<'a, W: Copy, S: Fn(W, u32) -> bool> {
    windows: Vec<W>,
    set: &'a S,
}

impl<W: Copy, S: Fn(W, u32) -> bool> Drop for Excluded<'_, W, S> {
    fn drop(&mut self) {
        for &window in self.windows.iter().rev() {
            let _ = (self.set)(window, WDA_NONE.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    const EXCLUDED: u32 = WDA_EXCLUDEFROMCAPTURE.0;

    #[test]
    fn copies_only_after_all_windows_are_excluded_and_restores_the_normal_policy() {
        let original = BTreeMap::from([(1, 0), (2, 0), (3, 0)]);
        let affinities = RefCell::new(original.clone());
        let writes = RefCell::new(Vec::new());
        let result = with_excluded(&Mutex::new(()), [1, 2, 3], |w, a| {
            writes.borrow_mut().push((w, a));
            affinities.borrow_mut().insert(w, a);
            true
        }, || {
            assert!(affinities.borrow().values().all(|a| *a == EXCLUDED));
            Some("copied")
        });
        assert_eq!(result, Some("copied"));
        assert_eq!(*affinities.borrow(), original);
        assert_eq!(*writes.borrow(), [(1, EXCLUDED), (2, EXCLUDED), (3, EXCLUDED), (3, 0), (2, 0), (1, 0)]);
    }

    #[test]
    fn hidden_windows_are_excluded_too_without_an_affinity_read_precondition() {
        let writes = RefCell::new(Vec::new());
        // Visibility is deliberately irrelevant: hidden Settings may appear
        // while the worker settles, so every supplied window must be protected.
        let required = [(1, true), (2, false)];
        let result = with_excluded(&Mutex::new(()), required, |window, a| {
            writes.borrow_mut().push((window, a));
            true
        }, || Some(()));
        assert_eq!(result, Some(()));
        assert_eq!(*writes.borrow(), [((1, true), EXCLUDED), ((2, false), EXCLUDED), ((2, false), 0), ((1, true), 0)]);
    }

    #[test]
    fn failed_exclusion_aborts_and_rolls_back_even_a_partially_applied_failure() {
        let original = BTreeMap::from([(1, 0), (2, 0)]);
        let affinities = RefCell::new(original.clone());
        let copied = Cell::new(false);
        let result = with_excluded(&Mutex::new(()), [1, 2], |w, a| {
            affinities.borrow_mut().insert(w, a);
            !(w == 2 && a == EXCLUDED)
        }, || { copied.set(true); Some(()) });
        assert_eq!(result, None);
        assert!(!copied.get());
        assert_eq!(*affinities.borrow(), original);
    }

    #[test]
    fn failed_copy_restores_affinity_before_another_capture() {
        let affinity = Cell::new(0);
        let lock = Mutex::new(());
        let result: Option<()> = with_excluded(&lock, [1], |_, a| { affinity.set(a); true }, || None);
        assert_eq!(result, None);
        assert_eq!(affinity.get(), 0);
        assert!(lock.try_lock().is_ok());
    }

    #[test]
    fn unwinding_restores_affinity_and_a_poisoned_capture_lock_fails_closed() {
        let affinity = Cell::new(0);
        let lock = Mutex::new(());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_excluded(&lock, [1], |_, a| { affinity.set(a); true }, || -> Option<()> { panic!("copy failed") })
        }));
        assert!(result.is_err());
        assert_eq!(affinity.get(), 0);
        let copied = Cell::new(false);
        let result = with_excluded(&lock, [1], |_, a| { affinity.set(a); true }, || { copied.set(true); Some(()) });
        assert_eq!(result, None);
        assert!(!copied.get());
    }

    #[test]
    fn a_replacement_worker_waits_until_the_cancelled_workers_affinity_is_restored() {
        let lock = Arc::new(Mutex::new(()));
        let affinity = Arc::new(Mutex::new(0));
        let (copying, copying_rx) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let first_lock = lock.clone();
        let first_affinity = affinity.clone();
        let first = std::thread::spawn(move || {
            with_excluded(&first_lock, [1], |_, a| { *first_affinity.lock().unwrap() = a; true }, || {
                copying.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(*first_affinity.lock().unwrap(), EXCLUDED);
                Some(())
            })
        });
        copying_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (started, started_rx) = mpsc::channel();
        let (entered, entered_rx) = mpsc::channel();
        let second_lock = lock.clone();
        let second_affinity = affinity.clone();
        let second = std::thread::spawn(move || {
            // Session cancellation cannot drop the first blocking worker. Its
            // replacement is free to queue, but must not share its exclusions.
            started.send(()).unwrap();
            with_excluded(&second_lock, [1], |_, a| {
                let mut current = second_affinity.lock().unwrap();
                if a == EXCLUDED { entered.send(*current).unwrap(); }
                *current = a;
                true
            }, || {
                assert_eq!(*second_affinity.lock().unwrap(), EXCLUDED);
                Some(())
            })
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let early_entry = entered_rx.recv_timeout(Duration::from_millis(50));
        resume.send(()).unwrap();
        assert!(matches!(early_entry, Err(mpsc::RecvTimeoutError::Timeout)), "replacement cannot change affinity before the first worker restores it");
        assert_eq!(first.join().unwrap(), Some(()));
        assert_eq!(entered_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 0, "the first worker restores before unlocking");
        assert_eq!(second.join().unwrap(), Some(()));
        assert_eq!(*affinity.lock().unwrap(), 0);
    }
}

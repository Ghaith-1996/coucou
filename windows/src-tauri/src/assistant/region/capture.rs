// One rectangle of the screen, copied once with GDI. Coucou's own windows
// (the island, the desktop Mochi) are excluded from it for the moment of the
// copy (SetWindowDisplayAffinity), then shown to captures again.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};

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

/// Keeps these windows out of screen captures until dropped.
pub struct Excluded(Vec<HWND>);

impl Excluded {
    pub fn new(windows: impl IntoIterator<Item = HWND>) -> Self {
        let hidden = windows
            .into_iter()
            .filter(|&hwnd| unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }.is_ok())
            .collect();
        Self(hidden)
    }
}

impl Drop for Excluded {
    fn drop(&mut self) {
        for &hwnd in &self.0 {
            let _ = unsafe { SetWindowDisplayAffinity(hwnd, WDA_NONE) };
        }
    }
}

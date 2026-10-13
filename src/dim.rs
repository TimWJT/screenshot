//! A see-through black window that dims the screen the moment a capture is woken, before the
//! screenshot is taken and the editor is up. It's kept out of screen captures, so the grab
//! underneath still sees the screen as it was.

use std::sync::atomic::{AtomicPtr, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetStockObject, UpdateWindow, BLACK_BRUSH, HBRUSH};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::hinstance;

const CLASS: PCWSTR = w!("TimWJT.Screenshot.Dim");
static DIM: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Make the window, hidden. Does nothing on a Windows too old to keep it out of captures
/// (before 10 version 2004), as it would then end up darkening the screenshot itself.
pub fn prepare() {
    if !DIM.load(Ordering::Relaxed).is_null() {
        return;
    }
    unsafe {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance(),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        let Ok(hwnd) = CreateWindowExW(ex, CLASS, None, WS_POPUP, 0, 0, 1, 1, None, None, Some(hinstance()), None) else { return };
        // The same 45% black the editor lays over everything outside the selection.
        if SetLayeredWindowAttributes(hwnd, COLORREF(0), 115, LWA_ALPHA).is_err() || SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(hwnd);
            return;
        }
        DIM.store(hwnd.0, Ordering::Relaxed);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Dim every monitor, now.
pub fn show() {
    prepare();
    let hwnd = HWND(DIM.load(Ordering::Relaxed));
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = UpdateWindow(hwnd);
    }
}

pub fn close() {
    let p = DIM.swap(std::ptr::null_mut(), Ordering::Relaxed);
    if !p.is_null() {
        unsafe {
            let _ = DestroyWindow(HWND(p));
        }
    }
}

#[cfg(test)]
mod tests {
    /// The dim window must not show up in the screenshot it's covering for.
    #[test]
    fn stays_out_of_the_screenshot() {
        // Build machines may have no desktop to grab.
        let Some(_) = crate::capture::grab_screen() else { return };
        super::show();
        let a = crate::capture::grab_screen().unwrap();
        super::close();
        let b = crate::capture::grab_screen().unwrap();
        // Something on screen may change between the two grabs (a clock, a cursor blink), so
        // compare overall brightness: a 45% black layer would cut it nearly in half.
        let sum = |s: &crate::capture::Shot| s.img.px.iter().map(|&p| p as u64).sum::<u64>();
        let (a, b) = (sum(&a), sum(&b));
        assert!(a * 100 > b * 95, "screen was darkened in the grab: {a} vs {b}");
    }
}

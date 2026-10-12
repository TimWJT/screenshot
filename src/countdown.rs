//! The "3, 2, 1" bubble shown before a delayed capture. Click it to cancel.

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::{hinstance, wide};

const CLASS: PCWSTR = w!("TimWJT.Screenshot.Countdown");
const TICK: usize = 1;

thread_local! {
    static LEFT: Cell<u32> = const { Cell::new(0) };
    static FULL: Cell<bool> = const { Cell::new(false) };
    static HWND_: Cell<isize> = const { Cell::new(0) };
}

pub fn start(secs: u32, full: bool) {
    if HWND_.with(|h| h.get()) != 0 {
        return;
    }
    unsafe {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance(),
                hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        LEFT.with(|l| l.set(secs));
        FULL.with(|f| f.set(full));

        // Centre it on the monitor the mouse is on.
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            CLASS,
            w!("Screenshot countdown"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinstance()),
            None,
        ) else {
            return;
        };
        let size = 120 * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let r = mi.rcWork;
        let _ = SetWindowPos(
            hwnd,
            None,
            r.left + (r.right - r.left - size) / 2,
            r.top + (r.bottom - r.top - size) / 2,
            size,
            size,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let rgn = CreateRoundRectRgn(0, 0, size + 1, size + 1, size / 4, size / 4);
        SetWindowRgn(hwnd, Some(rgn), false);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 225, LWA_ALPHA);
        HWND_.with(|h| h.set(hwnd.0 as isize));
        SetTimer(Some(hwnd), TICK, 1000, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

unsafe fn finish(hwnd: HWND, capture: bool) {
    let _ = KillTimer(Some(hwnd), TICK);
    let _ = DestroyWindow(hwnd);
    HWND_.with(|h| h.set(0));
    if capture {
        // Give the compositor a moment to take the bubble off screen.
        crate::capture_after(FULL.with(|f| f.get()), 0, 120);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(COLORREF(0x00242120));
            FillRect(dc, &rc, bg);
            let _ = DeleteObject(bg.into());
            let h = rc.bottom * 11 / 20;
            let font = CreateFontW(
                h,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let old = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00ffffff));
            let mut text = wide(&LEFT.with(|l| l.get()).to_string());
            text.pop();
            DrawTextW(dc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            let left = LEFT.with(|l| {
                l.set(l.get().saturating_sub(1));
                l.get()
            });
            if left == 0 {
                finish(hwnd, true);
            } else {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            finish(hwnd, false);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

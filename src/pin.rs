//! Screenshots pinned to the screen as small always-on-top windows.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::Image;
use crate::util::{hinstance, timestamp_name, wide, x_lparam, y_lparam};
use crate::{dialogs, gfx, settings};

struct Pin {
    img: Image,
    zoom: f32,
    alpha: u8,
    /// Cursor offset inside the window while dragging.
    grab: Option<POINT>,
}

thread_local! {
    static PINS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

pub const CLASS: PCWSTR = w!("TimWJT.Screenshot.Pin");
const BORDER: i32 = 1;

pub fn count() -> usize {
    PINS.with(|p| p.borrow().len())
}

/// Is any pin open, in any capture process?
pub fn any_open() -> bool {
    unsafe { FindWindowW(CLASS, None).is_ok() }
}

/// Close every pin, including those owned by other capture processes.
pub fn close_all() {
    unsafe {
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, CLASS, None) {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            after = Some(h);
        }
    }
}

pub fn open(img: Image, x: i32, y: i32) {
    unsafe {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS | CS_DROPSHADOW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance(),
                hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let (w, h) = (img.w as i32 + BORDER * 2, img.h as i32 + BORDER * 2);
        let pin = Box::new(Pin { img, zoom: 1.0, alpha: 255, grab: None });
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            CLASS,
            w!("Pinned screenshot"),
            WS_POPUP,
            x - BORDER,
            y - BORDER,
            w,
            h,
            None,
            None,
            Some(hinstance()),
            None,
        ) else {
            return;
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(pin) as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        PINS.with(|p| p.borrow_mut().push(hwnd));
        let _ = ShowWindow(hwnd, SW_SHOW);
        crate::util::force_foreground(hwnd);
    }
}

unsafe fn pin<'a>(hwnd: HWND) -> Option<&'a mut Pin> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin).as_mut()
}

unsafe fn resize(hwnd: HWND, p: &Pin, anchor: Option<POINT>) {
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    let w = (p.img.w as f32 * p.zoom).round() as i32 + BORDER * 2;
    let h = (p.img.h as f32 * p.zoom).round() as i32 + BORDER * 2;
    // Keep the point under the cursor still while zooming.
    let (x, y) = match anchor {
        Some(a) => {
            let fx = (a.x - r.left) as f32 / (r.right - r.left).max(1) as f32;
            let fy = (a.y - r.top) as f32 / (r.bottom - r.top).max(1) as f32;
            (a.x - (fx * w as f32) as i32, a.y - (fy * h as f32) as i32)
        }
        None => (r.left, r.top),
    };
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    let _ = InvalidateRect(Some(hwnd), None, false);
}

const CMD_COPY: usize = 1;
const CMD_SAVE: usize = 2;
const CMD_ZOOM: usize = 3;
const CMD_CLOSE: usize = 4;
const CMD_CLOSE_ALL: usize = 5;
const CMD_SAVE_AS: usize = 6;

unsafe fn command(hwnd: HWND, cmd: usize) {
    let Some(p) = pin(hwnd) else { return };
    match cmd {
        CMD_COPY => {
            gfx::copy_image(hwnd, &p.img);
        }
        CMD_SAVE => match crate::save_to_folder(&p.img) {
            Ok(path) => crate::notify_saved(&path),
            Err(msg) => crate::error_box(hwnd, &msg),
        },
        CMD_SAVE_AS => {
            let img = p.img.clone();
            if let Some(path) = dialogs::save_as(hwnd, &settings::get().save_path(), &timestamp_name()) {
                if let Err(e) = gfx::save_image(&img, &path) {
                    crate::error_box(hwnd, &format!("Could not save {}\n\n{e}", path.display()));
                }
            }
        }
        CMD_ZOOM => {
            p.zoom = 1.0;
            resize(hwnd, p, None);
        }
        CMD_CLOSE => {
            let _ = DestroyWindow(hwnd);
        }
        CMD_CLOSE_ALL => close_all(),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(p) = pin(hwnd) {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let border = CreateSolidBrush(COLORREF(0x00ed802f)); // the overlay's accent, as BGR
                FillRect(dc, &rc, border);
                let _ = DeleteObject(border.into());
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: p.img.w as i32,
                        biHeight: -(p.img.h as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                SetStretchBltMode(dc, if p.zoom < 1.0 { HALFTONE } else { COLORONCOLOR });
                StretchDIBits(
                    dc,
                    BORDER,
                    BORDER,
                    rc.right - BORDER * 2,
                    rc.bottom - BORDER * 2,
                    0,
                    0,
                    p.img.w as i32,
                    p.img.h as i32,
                    Some(p.img.px.as_ptr() as *const _),
                    &bi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(p) = pin(hwnd) {
                p.grab = Some(POINT { x: x_lparam(lp), y: y_lparam(lp) });
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(g) = pin(hwnd).and_then(|p| p.grab) {
                let mut c = POINT::default();
                let _ = GetCursorPos(&mut c);
                let _ = SetWindowPos(hwnd, None, c.x - g.x, c.y - g.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(p) = pin(hwnd) {
                p.grab = None;
            }
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            if let Some(p) = pin(hwnd) {
                if ctrl {
                    let a = p.alpha as i32 + if delta > 0 { 25 } else { -25 };
                    p.alpha = a.clamp(40, 255) as u8;
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), p.alpha, LWA_ALPHA);
                } else {
                    let f = if delta > 0 { 1.1 } else { 1.0 / 1.1 };
                    p.zoom = (p.zoom * f).clamp(0.1, 8.0);
                    if (p.zoom - 1.0).abs() < 0.04 {
                        p.zoom = 1.0;
                    }
                    resize(hwnd, p, Some(POINT { x: x_lparam(lp), y: y_lparam(lp) }));
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => command(hwnd, CMD_CLOSE),
                VK_C if ctrl => command(hwnd, CMD_COPY),
                VK_S if ctrl && GetKeyState(VK_SHIFT.0 as i32) < 0 => command(hwnd, CMD_SAVE_AS),
                VK_S if ctrl => command(hwnd, CMD_SAVE),
                VK_0 if ctrl => command(hwnd, CMD_ZOOM),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let menu = CreatePopupMenu().unwrap_or_default();
            let item = |id: usize, text: &str| {
                let t = wide(text);
                let _ = AppendMenuW(menu, MF_STRING, id, crate::util::pcwstr(&t));
            };
            item(CMD_COPY, "Copy\tCtrl+C");
            item(CMD_SAVE, "Save\tCtrl+S");
            item(CMD_SAVE_AS, "Save as…\tCtrl+Shift+S");
            item(CMD_ZOOM, "Actual size\tCtrl+0");
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            item(CMD_CLOSE, "Close\tEsc");
            if count() > 1 {
                item(CMD_CLOSE_ALL, "Close all pins");
            }
            let mut c = POINT::default();
            let _ = GetCursorPos(&mut c);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, c.x, c.y, None, hwnd, None);
            let _ = DestroyMenu(menu);
            command(hwnd, cmd.0 as usize);
            LRESULT(0)
        }
        // A pinned image keeps its pixel size when dragged across monitors.
        WM_DPICHANGED => LRESULT(0),
        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin;
            if !p.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(p));
            }
            PINS.with(|ps| ps.borrow_mut().retain(|h| *h != hwnd));
            crate::check_exit();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

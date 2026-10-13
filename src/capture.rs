use std::ffi::c_void;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 32-bit BGRA pixels, top-down, alpha always 255.
#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return 0;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        (self.px[i + 2] as u32) << 16 | (self.px[i + 1] as u32) << 8 | self.px[i] as u32
    }

    /// The `w`×`h` block at (x, y), which must lie inside the image.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * self.w + x) * 4) as usize;
            px.extend_from_slice(&self.px[start..start + (w * 4) as usize]);
        }
        Image { w, h, px }
    }
}

/// Everything on every monitor, plus where its top-left sits on the virtual desktop.
pub struct Shot {
    pub x: i32,
    pub y: i32,
    pub img: Image,
}

pub fn grab_screen() -> Option<Shot> {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            return None;
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        };
        let old = SelectObject(mem, dib.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut px = Vec::new();
        if ok && !bits.is_null() {
            let len = w as usize * h as usize * 4;
            px = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
            for a in px.iter_mut().skip(3).step_by(4) {
                *a = 255;
            }
        }
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if px.is_empty() {
            return None;
        }
        Some(Shot { x, y, img: Image { w: w as u32, h: h as u32, px } })
    }
}

/// Visible top-level windows, front to back, as their on-screen frame (no invisible resize border).
pub fn window_rects() -> Vec<RECT> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<RECT>);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true.into();
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
        if cloaked != 0 {
            return true.into();
        }
        let mut class = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if class == "Progman" || class == "WorkerW" {
            return true.into();
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut c_void, size_of::<RECT>() as u32).is_err() {
            let _ = GetWindowRect(hwnd, &mut r);
        }
        if r.right - r.left > 1 && r.bottom - r.top > 1 {
            out.push(r);
        }
        true.into()
    }
    let mut out: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Every monitor's rectangle with its UI scale (1.0 = 96 DPI).
pub fn monitors() -> Vec<(RECT, f32)> {
    unsafe extern "system" fn each(mon: HMONITOR, _: HDC, r: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<(RECT, f32)>);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        out.push((*r, dx as f32 / 96.0));
        true.into()
    }
    let mut out: Vec<(RECT, f32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The part of the monitor at `r` not covered by the taskbar, in screen coordinates.
pub fn work_area(r: RECT) -> Option<RECT> {
    unsafe {
        let mon = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(mon, &mut mi).as_bool().then_some(mi.rcWork)
    }
}

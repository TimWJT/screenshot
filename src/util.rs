use std::path::PathBuf;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, LPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Null-terminated UTF-16 copy of `s`. Keep the Vec alive while the PCWSTR is in use.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub unsafe fn take_pwstr(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

pub fn hinstance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| m.into()).unwrap_or_default() }
}

pub fn x_lparam(lp: LPARAM) -> i32 {
    (lp.0 & 0xffff) as u16 as i16 as i32
}

pub fn y_lparam(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xffff) as u16 as i16 as i32
}

/// "Screenshot 2026-10-12 113045", the same shape Windows uses.
pub fn timestamp_name() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "Screenshot {:04}-{:02}-{:02} {:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn default_save_dir() -> PathBuf {
    let pictures = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None)
            .map(|p| take_pwstr(p))
            .unwrap_or_default()
    };
    if pictures.is_empty() {
        return PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Pictures").join("Screenshots");
    }
    PathBuf::from(pictures).join("Screenshots")
}

pub fn config_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("Screenshot")
}

/// A file name in `dir` that does not exist yet.
pub fn unique_path(dir: &std::path::Path, stem: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    p
}

/// Bring `hwnd` to the front with keyboard focus, even when another app is in the foreground.
/// SetForegroundWindow alone is refused unless this process got the last input, which is not
/// the case after a keyboard-hook shortcut, a tray menu timer or a second launch.
pub fn force_foreground(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let me = GetCurrentThreadId();
        let other = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attach = other != 0 && other != me;
        if attach {
            let _ = AttachThreadInput(me, other, true);
        }
        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attach {
            let _ = AttachThreadInput(me, other, false);
        }
    }
}

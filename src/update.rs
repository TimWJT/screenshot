//! Checking GitHub for a newer release, and installing it when asked.
//!
//! This uses the plain releases page rather than GitHub's API, which only allows 60 requests an
//! hour per IP address and so often fails for everyone behind the same network.

use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::util::{from_wide, pcwstr, wide};

/// Redirects to the newest published release, `…/releases/tag/v1.2.3`.
pub const PAGE: &str = "https://github.com/TimWJT/screenshot/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "1.2.3"
    pub version: String,
}

impl Release {
    /// The installer the release workflow attaches to every release.
    fn setup_url(&self) -> String {
        let v = &self.version;
        format!("https://github.com/TimWJT/screenshot/releases/download/v{v}/Screenshot_{v}_x64-setup.exe")
    }
}

pub enum Done {
    /// A check finished: the latest release, or why it couldn't be found.
    Checked { manual: bool, result: Result<Release, String> },
    /// An installer finished downloading to this path, or failed to.
    Downloaded(Result<std::path::PathBuf, String>),
}

static DONE: Mutex<Vec<Done>> = Mutex::new(Vec::new());

/// Results come back to `hwnd` as `msg`; collect them with `take`.
fn finish(hwnd: isize, msg: u32, done: Done) {
    DONE.lock().unwrap().push(done);
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

pub fn take() -> Vec<Done> {
    std::mem::take(&mut *DONE.lock().unwrap())
}

pub fn check(hwnd: HWND, msg: u32, manual: bool) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = get(PAGE, false).and_then(|r| {
            let to = r.location.unwrap_or_default();
            release_from_url(&to).ok_or_else(|| "There are no releases yet.".to_string())
        });
        finish(h, msg, Done::Checked { manual, result });
    });
}

pub fn download(hwnd: HWND, msg: u32, rel: Release) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = (|| {
            let r = get(&rel.setup_url(), true)?;
            if r.status != 200 {
                return Err(format!("GitHub has no installer for {} (HTTP {}).", rel.version, r.status));
            }
            let path = std::env::temp_dir().join(format!("Screenshot_{}_x64-setup.exe", rel.version));
            std::fs::write(&path, r.body).map_err(|e| format!("Could not save the installer: {e}"))?;
            Ok(path)
        })();
        finish(h, msg, Done::Downloaded(result));
    });
}

/// Is `latest` ("0.2.0") newer than `current`?
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(latest) > parse(current)
}

/// Installed with the installer (rather than run as the portable exe)?
pub fn installed() -> bool {
    std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("unins000.exe"))).is_some_and(|p| p.exists())
}

/// The release a `…/releases/tag/v1.2.3` URL points at. With no releases, GitHub sends
/// `/releases/latest` to the releases list instead.
fn release_from_url(url: &str) -> Option<Release> {
    let (_, tag) = url.rsplit_once("/releases/tag/")?;
    let version = tag.strip_prefix('v')?;
    let ok = !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    ok.then(|| Release { version: version.to_string() })
}

struct Response {
    status: u32,
    location: Option<String>,
    body: Vec<u8>,
}

struct Handle(*mut std::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// HTTPS GET. The error is a message ready to show.
fn get(url: &str, follow_redirects: bool) -> Result<Response, String> {
    let fail = |what: &str| format!("Could not reach GitHub ({what}). Check your internet connection.");
    let rest = url.strip_prefix("https://").ok_or_else(|| fail("bad address"))?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let agent = wide(&format!("Screenshot/{}", env!("CARGO_PKG_VERSION")));
    let (host, path) = (wide(host), wide(path));
    unsafe {
        let session = Handle(WinHttpOpen(pcwstr(&agent), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(fail("no session"));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let conn = Handle(WinHttpConnect(session.0, pcwstr(&host), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(fail("no connection"));
        }
        let req = Handle(WinHttpOpenRequest(conn.0, w!("GET"), pcwstr(&path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err(fail("no request"));
        }
        if !follow_redirects {
            let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
            let _ = WinHttpSetOption(Some(req.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&never));
        }
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(|_| fail("send failed"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|_| fail("no response"))?;
        let mut status = 0u32;
        let mut len = 4u32;
        let flags = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
        WinHttpQueryHeaders(req.0, flags, PCWSTR::null(), Some(&mut status as *mut _ as *mut _), &mut len, std::ptr::null_mut())
            .map_err(|_| fail("no status"))?;
        let mut loc = [0u16; 1024];
        let mut len = (loc.len() * 2) as u32;
        let location = WinHttpQueryHeaders(req.0, WINHTTP_QUERY_LOCATION, PCWSTR::null(), Some(loc.as_mut_ptr() as *mut _), &mut len, std::ptr::null_mut())
            .ok()
            .map(|_| from_wide(&loc[..len as usize / 2]));
        // A 404 is an answer (no release, or no installer attached yet), not a network problem;
        // the callers say what's missing.
        if status >= 400 && status != 404 {
            return Err(fail(&format!("HTTP {status}")));
        }
        let mut body = Vec::new();
        if follow_redirects {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut n = 0u32;
                WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n).map_err(|_| fail("download cut off"))?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n as usize]);
            }
        }
        Ok(Response { status, location, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn reads_release_redirect() {
        let r = release_from_url("https://github.com/TimWJT/screenshot/releases/tag/v0.2.0").unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.setup_url(), "https://github.com/TimWJT/screenshot/releases/download/v0.2.0/Screenshot_0.2.0_x64-setup.exe");
        assert_eq!(release_from_url("https://github.com/TimWJT/screenshot/releases"), None);
        assert_eq!(release_from_url("https://github.com/TimWJT/screenshot/releases/tag/v1.0/../x"), None);
        assert_eq!(release_from_url(""), None);
    }

    /// Talks to GitHub: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn finds_latest_release_and_installer() {
        let r = get(PAGE, false).expect("request");
        let rel = release_from_url(&r.location.expect("redirect")).expect("tag");
        println!("latest: {rel:?}");
        let setup = get(&rel.setup_url(), true).expect("installer");
        assert_eq!(setup.status, 200);
        assert!(setup.body.starts_with(b"MZ"), "not an exe");
        println!("installer: {} bytes", setup.body.len());
    }
}

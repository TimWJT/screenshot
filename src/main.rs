//! Screenshot: a tray app that captures a region, lets you draw on it, then copies, saves or pins it.

#![windows_subsystem = "windows"]

mod annot;
mod capture;
mod countdown;
mod dialogs;
mod gfx;
mod overlay;
mod pin;
mod settings;
mod util;

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::Image;
use crate::util::{hinstance, pcwstr, wide};

const MAIN_CLASS: PCWSTR = w!("TimWJT.Screenshot.Main");
const WM_TRAY: u32 = WM_APP + 1;
const WM_HOOKED: u32 = WM_APP + 2;
const TIMER_CAPTURE: usize = 1;
const TIMER_TRAY: usize = 2;
const HK_MAIN: i32 = 1;
const COPYDATA_ARGS: usize = 0x5353;
const COPYDATA_SAVED: usize = 0x5354;

const CMD_REGION: usize = 1;
const CMD_FULL: usize = 2;
const CMD_DELAY3: usize = 3;
const CMD_DELAY5: usize = 4;
const CMD_DELAY10: usize = 5;
const CMD_FOLDER: usize = 6;
const CMD_UNPIN: usize = 7;
const CMD_SETTINGS: usize = 8;
const CMD_EXIT: usize = 9;

static MAIN: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static PENDING_FULL: Cell<bool> = const { Cell::new(false) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static LAST_LAUNCH: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static TRAY_TRIES: Cell<u32> = const { Cell::new(0) };
    /// The file the last "saved" notification was about, opened when it's clicked.
    static LAST_SAVED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::Relaxed) as *mut _)
}

pub fn error_box(owner: HWND, text: &str) {
    let t = wide(text);
    // With no owner, keep it above anything topmost so it can't end up hidden.
    let top = if owner.is_invalid() { MB_TOPMOST } else { MESSAGEBOX_STYLE(0) };
    unsafe {
        MessageBoxW(Some(owner), pcwstr(&t), w!("Screenshot"), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | top);
    }
}

/// Save `img` as a new PNG in the save folder (Pictures\Screenshots unless Settings says
/// otherwise). The error is a message ready to show.
pub fn save_to_folder(img: &Image) -> Result<std::path::PathBuf, String> {
    let dir = settings::get().save_path();
    let _ = std::fs::create_dir_all(&dir);
    let path = util::unique_path(&dir, &util::timestamp_name(), "png");
    match gfx::save_image(img, &path) {
        Ok(()) => Ok(path),
        Err(e) => Err(format!("Could not save {}\n\n{e}", path.display())),
    }
}

/// When "also save every copied screenshot" is on, drop a PNG in the save folder.
pub fn autosave(img: &Image) {
    if !settings::get().autosave {
        return;
    }
    if let Err(msg) = save_to_folder(img) {
        error_box(HWND::default(), &msg);
    }
}

/// Have the tray app say where a screenshot was saved; clicking the notification opens it.
pub fn notify_saved(path: &std::path::Path) {
    send_to_tray(COPYDATA_SAVED, &path.to_string_lossy());
}

/// Pass `text` to the running tray app. False when it isn't running.
fn send_to_tray(kind: usize, text: &str) -> bool {
    unsafe {
        let Ok(tray) = FindWindowW(MAIN_CLASS, None) else { return false };
        // Let it come to the front if what it's asked to do opens a window (Settings).
        let mut pid = 0u32;
        GetWindowThreadProcessId(tray, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let text: Vec<u16> = text.encode_utf16().collect();
        let cds = COPYDATASTRUCT { dwData: kind, cbData: (text.len() * 2) as u32, lpData: text.as_ptr() as *mut _ };
        // A timeout, so a busy or hung tray app can't freeze the caller.
        let _ = SendMessageTimeoutW(tray, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 2000, None);
        true
    }
}

#[derive(Default)]
struct Args {
    /// Internal: this process is a one-off capture started by the tray process.
    capture: bool,
    /// Close the tray app, its pins and any open capture (the uninstaller uses this).
    quit: bool,
    tray_only: bool,
    settings: bool,
    full: bool,
    delay: u32,
}

fn parse_args<I: IntoIterator<Item = String>>(it: I) -> Args {
    let mut a = Args::default();
    let mut it = it.into_iter();
    while let Some(s) = it.next() {
        match s.as_str() {
            "--tray" => a.tray_only = true,
            "--capture" => a.capture = true,
            "--quit" => a.quit = true,
            "--full" => a.full = true,
            "--settings" => a.settings = true,
            "--delay" => a.delay = it.next().and_then(|n| n.parse().ok()).unwrap_or(3),
            _ => {}
        }
    }
    a
}

fn run_args(a: &Args) {
    if a.settings {
        settings::show();
        return;
    }
    if a.tray_only {
        return;
    }
    capture_after(a.full, a.delay, 0);
}

/// Capture after a countdown of `secs`, or after `ms` for menus to finish fading out.
pub fn capture_after(full: bool, secs: u32, ms: u32) {
    if capture_busy() {
        return;
    }
    if secs > 0 {
        countdown::start(secs, full);
        return;
    }
    if ms == 0 {
        capture_now(full);
    } else {
        PENDING_FULL.with(|p| p.set(full));
        unsafe {
            SetTimer(Some(main_hwnd()), TIMER_CAPTURE, ms, None);
        }
    }
}

/// True while a capture window is up, or one was launched a moment ago and is still starting.
fn capture_busy() -> bool {
    let recent = LAST_LAUNCH.with(|t| t.get().is_some_and(|t| t.elapsed().as_millis() < 700));
    recent || unsafe { FindWindowW(overlay::CLASS, None).is_ok() }
}

// Each capture runs in a short-lived child process. Direct2D and the graphics driver keep
// 100+ MB of memory around after the editor closes, and only exiting gives it back, so the
// tray process never touches Direct2D and stays at a couple of megabytes.
fn capture_now(full: bool) {
    if capture_busy() {
        return;
    }
    unsafe {
        let _ = DwmFlush();
        // Let the child take the foreground; this process got the hotkey, so it may pass that on.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--capture");
    if full {
        cmd.arg("--full");
    }
    if cmd.spawn().is_ok() {
        LAST_LAUNCH.with(|t| t.set(Some(std::time::Instant::now())));
    }
}

/// Wake the capture process's message loop so it sees whether anything is left on screen.
pub fn check_exit() {
    unsafe {
        let _ = PostThreadMessageW(GetCurrentThreadId(), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

fn capture_main(full: bool) {
    unsafe {
        // One editor at a time, even if two captures were started in quick succession.
        let Ok(busy) = CreateMutexW(None, false, w!("TimWJT.Screenshot.Overlay")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(busy);
            return;
        }
        let mut busy = Some(busy);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        settings::load();
        if let Some(shot) = capture::grab_screen() {
            overlay::open(shot, full);
        }
        // Checked before every wait rather than on a posted message: a dialog's own message
        // loop would swallow that message and leave the process running with nothing on screen.
        let mut msg = MSG::default();
        while overlay::is_open() || pin::count() > 0 {
            if !overlay::is_open() {
                if let Some(h) = busy.take() {
                    let _ = CloseHandle(h);
                }
            }
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Close every window of every running copy, then wait (briefly) for the processes to go.
fn quit_running() {
    let classes = [MAIN_CLASS, overlay::CLASS, pin::CLASS];
    unsafe {
        for class in classes {
            let mut after: Option<HWND> = None;
            while let Ok(h) = FindWindowExW(None, after, class, None) {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                after = Some(h);
            }
        }
        for _ in 0..50 {
            if classes.iter().all(|&c| FindWindowW(c, None).is_err()) {
                // The windows are gone; give the processes a moment to finish exiting.
                std::thread::sleep(std::time::Duration::from_millis(300));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

// ---- hotkeys -----------------------------------------------------------------------------
//
// The custom shortcut goes through RegisterHotKey. Print Screen, and any shortcut another app
// already owns (Alt+F1 is also the NVIDIA overlay's), go through a low-level keyboard hook on
// its own thread instead, which sees the key before Windows' Snipping Tool binding does.

/// Packed (mods << 32 | vk) pairs the hook should catch, up to two.
static HOOK_KEYS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static HOOK_THREAD: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && (wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN) {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if k.flags.0 & LLKHF_INJECTED.0 == 0 {
            let down = |vk: VIRTUAL_KEY| GetAsyncKeyState(vk.0 as i32) < 0;
            let mut mods = 0;
            if down(VK_MENU) {
                mods |= MOD_ALT.0;
            }
            if down(VK_CONTROL) {
                mods |= MOD_CONTROL.0;
            }
            if down(VK_SHIFT) {
                mods |= MOD_SHIFT.0;
            }
            if down(VK_LWIN) || down(VK_RWIN) {
                mods |= MOD_WIN.0;
            }
            for slot in &HOOK_KEYS {
                let v = slot.load(Ordering::Relaxed);
                if v != 0 && (v & 0xffff_ffff) as u32 == k.vkCode && (v >> 32) as u32 == mods {
                    if mods & (MOD_ALT.0 | MOD_WIN.0) != 0 {
                        // Swallowing the key leaves Alt or Win pressed on its own, and releasing
                        // it would open the app's menu bar or the Start menu. A dummy key
                        // (0xE8 is unassigned) in between stops that.
                        let key = |flags| INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xe8), dwFlags: flags, ..Default::default() } },
                        };
                        SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
                    }
                    let _ = PostMessageW(Some(main_hwnd()), WM_HOOKED, WPARAM(0), LPARAM(0));
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn ensure_hook_thread() {
    if HOOK_THREAD.load(Ordering::Relaxed) != 0 {
        return;
    }
    HOOK_THREAD.store(1, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        let _hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(hinstance()), 0);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

/// Stop listening for the shortcuts while Settings is in front, so pressing them in the
/// shortcut box records them instead of starting a capture. `apply_hotkeys` turns them back on.
pub fn suspend_hotkeys() {
    unsafe {
        let _ = UnregisterHotKey(Some(main_hwnd()), HK_MAIN);
    }
    for slot in &HOOK_KEYS {
        slot.store(0, Ordering::Relaxed);
    }
}

pub fn apply_hotkeys() {
    let cfg = settings::get();
    let hwnd = main_hwnd();
    let mut hooked = Vec::new();
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HK_MAIN);
        if cfg.hotkey_vk != 0 {
            let mods = HOT_KEY_MODIFIERS(cfg.hotkey_mods) | MOD_NOREPEAT;
            if RegisterHotKey(Some(hwnd), HK_MAIN, mods, cfg.hotkey_vk).is_err() {
                hooked.push(((cfg.hotkey_mods as u64) << 32) | cfg.hotkey_vk as u64);
            }
        }
    }
    if cfg.printscreen {
        hooked.push(VK_SNAPSHOT.0 as u64);
    }
    for (i, slot) in HOOK_KEYS.iter().enumerate() {
        slot.store(hooked.get(i).copied().unwrap_or(0), Ordering::Relaxed);
    }
    if !hooked.is_empty() {
        ensure_hook_thread();
    }
    update_tray_tip();
}

fn shortcut_text() -> String {
    let cfg = settings::get();
    let mut parts = Vec::new();
    if cfg.hotkey_vk != 0 {
        parts.push(settings::hotkey_name(cfg.hotkey_mods, cfg.hotkey_vk));
    }
    if cfg.printscreen {
        parts.push("Print Screen".to_string());
    }
    parts.join(" or ")
}

// ---- tray --------------------------------------------------------------------------------

fn tray_data() -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: main_hwnd(),
        uID: 1,
        ..Default::default()
    };
    nid.uFlags = NIF_TIP | NIF_SHOWTIP;
    let tip = match shortcut_text() {
        s if s.is_empty() => "Screenshot".to_string(),
        s => format!("Screenshot ({s})"),
    };
    for (d, s) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *d = s;
    }
    nid
}

/// Add the tray icon. False when the taskbar isn't ready yet (early in sign-in).
fn add_tray() -> bool {
    let mut nid = tray_data();
    nid.uFlags |= NIF_ICON | NIF_MESSAGE;
    nid.uCallbackMessage = WM_TRAY;
    unsafe {
        nid.hIcon = LoadIconMetric(Some(hinstance()), PCWSTR(1 as _), LIM_SMALL).unwrap_or_default();
        // An add that timed out may still have gone through, so a working modify counts too.
        Shell_NotifyIconW(NIM_ADD, &nid).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool()
    }
}

/// Add the tray icon, retrying every two seconds for a minute if the taskbar isn't up yet.
fn add_tray_retrying() {
    unsafe {
        let _ = KillTimer(Some(main_hwnd()), TIMER_TRAY);
        if !add_tray() {
            TRAY_TRIES.with(|t| t.set(0));
            SetTimer(Some(main_hwnd()), TIMER_TRAY, 2000, None);
        }
    }
}

fn update_tray_tip() {
    let nid = tray_data();
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn tray_balloon(title: &str, text: &str) {
    // Clicking this one shouldn't open a file an earlier notification was about.
    LAST_SAVED.with(|s| s.borrow_mut().take());
    let mut nid = tray_data();
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
    for (d, s) in nid.szInfoTitle.iter_mut().zip(title.encode_utf16().take(63)) {
        *d = s;
    }
    for (d, s) in nid.szInfo.iter_mut().zip(text.encode_utf16().take(255)) {
        *d = s;
    }
    unsafe {
        nid.hBalloonIcon = LoadIconMetric(Some(hinstance()), PCWSTR(1 as _), LIM_LARGE).unwrap_or_default();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_tray() {
    let nid = tray_data();
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn tray_menu() {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let delay = CreatePopupMenu().unwrap_or_default();
        let add = |m: HMENU, id: usize, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(m, MF_STRING, id, pcwstr(&t));
        };
        let keys = shortcut_text();
        add(menu, CMD_REGION, &if keys.is_empty() { "Capture".to_string() } else { format!("Capture\t{keys}") });
        add(menu, CMD_FULL, "Capture whole screen");
        add(delay, CMD_DELAY3, "3 seconds");
        add(delay, CMD_DELAY5, "5 seconds");
        add(delay, CMD_DELAY10, "10 seconds");
        let t = wide("Capture after a delay");
        let _ = AppendMenuW(menu, MF_POPUP, delay.0 as usize, pcwstr(&t));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(menu, CMD_FOLDER, "Open screenshots folder");
        if pin::any_open() {
            add(menu, CMD_UNPIN, "Close all pinned screenshots");
        }
        add(menu, CMD_SETTINGS, "Settings…");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(menu, CMD_EXIT, "Exit");
        let _ = SetMenuDefaultItem(menu, CMD_REGION as u32, 0);

        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let hwnd = main_hwnd();
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            CMD_REGION => capture_after(false, 0, 250),
            CMD_FULL => capture_after(true, 0, 250),
            CMD_DELAY3 => capture_after(false, 3, 0),
            CMD_DELAY5 => capture_after(false, 5, 0),
            CMD_DELAY10 => capture_after(false, 10, 0),
            CMD_FOLDER => {
                let dir = settings::get().save_path();
                let _ = std::fs::create_dir_all(&dir);
                let d = wide(&dir.to_string_lossy());
                ShellExecuteW(None, w!("open"), pcwstr(&d), None, None, SW_SHOWNORMAL);
            }
            CMD_UNPIN => pin::close_all(),
            CMD_SETTINGS => settings::show(),
            CMD_EXIT => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY | WM_HOOKED => {
            capture_after(false, 0, 0);
            LRESULT(0)
        }
        WM_TRAY => {
            match (lp.0 & 0xffff) as u32 {
                // Wait for the tray overflow flyout, if the icon lives there, to close.
                WM_LBUTTONUP => capture_after(false, 0, 250),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(),
                NIN_BALLOONUSERCLICK => {
                    // A "Screenshot saved" notification: show the file in its folder.
                    if let Some(path) = LAST_SAVED.with(|s| s.borrow_mut().take()) {
                        let params = wide(&format!("/select,\"{path}\""));
                        ShellExecuteW(None, w!("open"), w!("explorer.exe"), pcwstr(&params), None, SW_SHOWNORMAL);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CAPTURE => {
            let _ = KillTimer(Some(hwnd), TIMER_CAPTURE);
            capture_now(PENDING_FULL.with(|p| p.get()));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TRAY => {
            let tries = TRAY_TRIES.with(|t| {
                t.set(t.get() + 1);
                t.get()
            });
            if add_tray() || tries >= 30 {
                let _ = KillTimer(Some(hwnd), TIMER_TRAY);
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            if cds.lpData.is_null() {
                return LRESULT(1);
            }
            let words = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
            let text = String::from_utf16_lossy(words);
            match cds.dwData {
                COPYDATA_ARGS => {
                    let args = parse_args(text.split('\n').filter(|s| !s.is_empty()).map(String::from));
                    if args.tray_only && !args.settings {
                        tray_balloon("Screenshot is already running", "It lives in the tray, by the clock.");
                    } else {
                        run_args(&args);
                    }
                }
                COPYDATA_SAVED => {
                    let path = std::path::Path::new(&text);
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    tray_balloon("Screenshot saved", &format!("{name}\nin {folder}\nClick to show it."));
                    LAST_SAVED.with(|s| *s.borrow_mut() = Some(text));
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            remove_tray();
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m == TASKBAR_CREATED.with(|t| t.get()) && m != 0 => {
            // Explorer restarted; put the icon back.
            add_tray_retrying();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// If another copy is already running, hand it our arguments and return true.
fn forward_to_running(raw_args: &[String]) -> bool {
    unsafe {
        let mutex = CreateMutexW(None, false, w!("TimWJT.Screenshot.Instance"));
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // Keep the handle open for the life of the process.
            std::mem::forget(mutex);
            return false;
        }
    }
    send_to_tray(COPYDATA_ARGS, &raw_args.join("\n"));
    true
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(raw_args.clone());
    if args.capture {
        capture_main(args.full);
        return;
    }
    if args.quit {
        quit_running();
        return;
    }
    if forward_to_running(&raw_args) {
        return;
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_HOTKEY_CLASS | ICC_LINK_CLASS | ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    let first_run = !settings::load();

    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hinstance(),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A real (hidden) top-level window, not message-only, so it hears TaskbarCreated.
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            MAIN_CLASS,
            w!("Screenshot"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance()),
            None,
        ) else {
            return;
        };
        MAIN.store(hwnd.0 as isize, Ordering::Relaxed);
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
    }

    add_tray_retrying();
    apply_hotkeys();

    if first_run {
        settings::update(|_| {});
        let keys = shortcut_text();
        tray_balloon(
            "Screenshot is in your tray",
            &format!("Press {keys} to capture. Right-click the tray icon for settings."),
        );
    }
    run_args(&args);

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if let Some(dlg) = settings::window() {
                if IsDialogMessageW(dlg, &msg).as_bool() {
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Args {
        parse_args(s.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_arguments() {
        let a = args("--full --delay 5");
        assert!(a.full && !a.capture && !a.quit && a.delay == 5);
        assert_eq!(args("--delay").delay, 3, "a bare --delay falls back to 3 seconds");
        assert_eq!(args("--delay soon").delay, 3);
        assert!(args("--tray --settings").settings);
        assert!(args("--quit").quit);
        assert!(args("--capture --full").capture);
        let none = args("--unknown");
        assert!(!none.full && !none.tray_only && none.delay == 0);
    }
}

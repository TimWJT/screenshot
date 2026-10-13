//! Saved preferences and the Settings window.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::{config_dir, default_save_dir, hinstance, pcwstr, wide};

#[derive(Clone, Debug)]
pub struct Config {
    /// MOD_ALT / MOD_CONTROL / MOD_SHIFT / MOD_WIN bits.
    pub hotkey_mods: u32,
    /// Virtual-key code; 0 means no custom shortcut.
    pub hotkey_vk: u32,
    pub printscreen: bool,
    pub save_dir: String,
    pub autosave: bool,
    pub color: u32,
    pub size: f32,
    pub fill: bool,
    pub check_updates: bool,
    /// Copied screenshots are resized to this percent of their real size.
    pub copy_scale: u32,
    /// Show the magnifier while selecting without holding Shift.
    pub magnifier: bool,
}

/// The choices for `copy_scale`.
pub const COPY_SCALES: [u32; 6] = [50, 75, 100, 125, 150, 200];

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey_mods: MOD_ALT.0,
            hotkey_vk: VK_F1.0 as u32,
            printscreen: true,
            save_dir: String::new(),
            autosave: false,
            color: 0xe53935,
            size: 3.0,
            fill: false,
            check_updates: true,
            copy_scale: 100,
            magnifier: false,
        }
    }
}

impl Config {
    pub fn save_path(&self) -> PathBuf {
        if self.save_dir.trim().is_empty() {
            default_save_dir()
        } else {
            PathBuf::from(&self.save_dir)
        }
    }
}

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
    static WINDOW: RefCell<Option<SettingsWindow>> = const { RefCell::new(None) };
}

fn file() -> PathBuf {
    config_dir().join("settings.ini")
}

/// Load settings. Returns false when there was no settings file yet (first run).
pub fn load() -> bool {
    let Ok(text) = std::fs::read_to_string(file()) else { return false };
    let c = parse(&text);
    CONFIG.with(|cfg| *cfg.borrow_mut() = c);
    true
}

fn parse(text: &str) -> Config {
    let mut c = Config::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let num = || v.parse::<u32>().ok();
        let flag = v == "1" || v.eq_ignore_ascii_case("true");
        match k.trim() {
            "hotkey_mods" => c.hotkey_mods = num().unwrap_or(c.hotkey_mods),
            "hotkey_vk" => c.hotkey_vk = num().unwrap_or(c.hotkey_vk),
            "printscreen" => c.printscreen = flag,
            "save_dir" => c.save_dir = v.to_string(),
            "autosave" => c.autosave = flag,
            "color" => c.color = u32::from_str_radix(v.trim_start_matches('#'), 16).unwrap_or(c.color),
            "size" => c.size = v.parse::<f32>().unwrap_or(c.size).clamp(1.0, 40.0),
            "fill" => c.fill = flag,
            "check_updates" => c.check_updates = flag,
            "copy_scale" => c.copy_scale = num().filter(|n| COPY_SCALES.contains(n)).unwrap_or(c.copy_scale),
            "magnifier" => c.magnifier = flag,
            _ => {}
        }
    }
    c
}

fn format(c: &Config) -> String {
    format!(
        "hotkey_mods={}\nhotkey_vk={}\nprintscreen={}\nsave_dir={}\nautosave={}\ncolor=#{:06X}\nsize={}\nfill={}\ncheck_updates={}\ncopy_scale={}\nmagnifier={}\n",
        c.hotkey_mods,
        c.hotkey_vk,
        c.printscreen as u8,
        c.save_dir,
        c.autosave as u8,
        c.color,
        c.size,
        c.fill as u8,
        c.check_updates as u8,
        c.copy_scale,
        c.magnifier as u8
    )
}

fn save() {
    let _ = std::fs::create_dir_all(config_dir());
    // Write a temp file and swap it in, so the other process never reads a half-written file.
    let tmp = config_dir().join(format!("settings.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, format(&get())).is_ok() && std::fs::rename(&tmp, file()).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Is this usable as a global shortcut? A key with no Ctrl, Alt or Win would fire on normal
/// typing, so on its own only a function key, Pause, Scroll Lock or Print Screen will do.
pub fn valid_shortcut(mods: u32, vk: u32) -> bool {
    mods & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_WIN.0) != 0 || matches!(vk, 0x70..=0x87 | 0x13 | 0x91 | 0x2c)
}

pub fn get() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Change settings and write them out. Re-reads the file first, because the capture process
/// and the tray process both write to it.
pub fn update(f: impl FnOnce(&mut Config)) {
    load();
    CONFIG.with(|c| f(&mut c.borrow_mut()));
    save();
}

/// "Ctrl+Shift+S", "Alt+F1", "Print Screen".
pub fn hotkey_name(mods: u32, vk: u32) -> String {
    let mut s = String::new();
    for (bit, name) in [(MOD_CONTROL.0, "Ctrl+"), (MOD_ALT.0, "Alt+"), (MOD_SHIFT.0, "Shift+"), (MOD_WIN.0, "Win+")] {
        if mods & bit != 0 {
            s.push_str(name);
        }
    }
    let key = match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6f),
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(vk).unwrap().to_string(),
        0x2c => "Print Screen".into(),
        _ => unsafe {
            let ext = matches!(vk, 0x21..=0x2e) as i32; // navigation keys live on the extended scan codes
            let sc = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as i32;
            let mut buf = [0u16; 64];
            let n = GetKeyNameTextW((sc << 16) | (ext << 24), &mut buf);
            if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                format!("Key {vk}")
            }
        },
    };
    s + &key
}

// ---- start with Windows ------------------------------------------------------------------

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Screenshot");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let cmd = wide(&format!("\"{}\" --tray", exe.display()));
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(cmd.as_ptr() as *const _),
                (cmd.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

// ---- window ------------------------------------------------------------------------------

const ID_HOTKEY: i32 = 101;
const ID_PRTSC: i32 = 102;
const ID_STARTUP: i32 = 103;
const ID_DIR: i32 = 104;
const ID_BROWSE: i32 = 105;
const ID_AUTOSAVE: i32 = 106;
const ID_LINK: i32 = 107;
const ID_UPDATES: i32 = 108;
const ID_SCALE: i32 = 109;
const ID_MAGNIFIER: i32 = 110;
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;

const HKM_SETHOTKEY: u32 = WM_USER + 1;
const HKM_GETHOTKEY: u32 = WM_USER + 2;
const HOTKEYF_SHIFT: u32 = 0x1;
const HOTKEYF_CONTROL: u32 = 0x2;
const HOTKEYF_ALT: u32 = 0x4;

struct SettingsWindow {
    hwnd: HWND,
    font: HFONT,
}

pub fn window() -> Option<HWND> {
    WINDOW.with(|w| w.try_borrow().ok().and_then(|w| w.as_ref().map(|s| s.hwnd)))
}

pub fn show() {
    if let Some(h) = window() {
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNORMAL);
            crate::util::force_foreground(h);
        }
        return;
    }
    unsafe {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        let class = w!("TimWJT.Screenshot.Settings");
        REGISTERED.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance(),
                hIcon: LoadIconW(Some(hinstance()), PCWSTR(1 as _)).unwrap_or_default(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Screenshot Settings"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            300,
            None,
            None,
            Some(hinstance()),
            None,
        ) else {
            return;
        };
        build(hwnd, style);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        crate::util::force_foreground(hwnd);
    }
}

unsafe fn build(hwnd: HWND, style: WINDOW_STYLE) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let px = |v: i32| v * dpi as i32 / 96;

    let mut ncm = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, ncm.cbSize, Some(&mut ncm as *mut _ as *mut _), 0, dpi);
    let font = CreateFontIndirectW(&ncm.lfMessageFont);

    let child = |class: PCWSTR, text: &str, style: u32, id: i32, x: i32, y: i32, w: i32, h: i32| -> HWND {
        let t = wide(text);
        let h = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            pcwstr(&t),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
            px(x),
            px(y),
            px(w),
            px(h),
            Some(hwnd),
            Some(HMENU(id as isize as *mut _)),
            Some(hinstance()),
            None,
        )
        .unwrap_or_default();
        SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        h
    };

    let cfg = get();
    let tab = WS_TABSTOP.0;
    let check = BS_AUTOCHECKBOX as u32 | tab;
    child(w!("STATIC"), "Capture shortcut", 0, -1, 16, 19, 130, 20);
    let hk = child(w!("msctls_hotkey32"), "", tab | WS_BORDER.0, ID_HOTKEY, 150, 16, 274, 24);
    child(w!("STATIC"), "Click the box, then press the keys you want.", 0, -1, 150, 44, 290, 18);
    let prtsc = child(w!("BUTTON"), "Print Screen key also captures", check, ID_PRTSC, 16, 70, 400, 22);
    let startup = child(w!("BUTTON"), "Start with Windows (stays in the tray)", check, ID_STARTUP, 16, 96, 400, 22);
    child(w!("STATIC"), "Save folder", 0, -1, 16, 134, 130, 20);
    let dir = child(w!("EDIT"), &cfg.save_path().to_string_lossy(), ES_AUTOHSCROLL as u32 | tab | WS_BORDER.0, ID_DIR, 16, 156, 316, 24);
    child(w!("BUTTON"), "Browse…", BS_PUSHBUTTON as u32 | tab, ID_BROWSE, 340, 155, 84, 26);
    let auto = child(w!("BUTTON"), "Also save every copied screenshot to this folder", check, ID_AUTOSAVE, 16, 188, 400, 22);
    let updates = child(w!("BUTTON"), "Tell me when there's a new version", check, ID_UPDATES, 16, 214, 400, 22);
    child(w!("STATIC"), "Copied image size", 0, -1, 16, 251, 130, 20);
    let scale = child(w!("COMBOBOX"), "", CBS_DROPDOWNLIST as u32 | tab | WS_VSCROLL.0, ID_SCALE, 150, 248, 120, 200);
    let magnifier = child(w!("BUTTON"), "Always show the magnifier (otherwise hold Shift)", check, ID_MAGNIFIER, 16, 280, 400, 22);
    child(
        WC_LINK,
        &format!("Screenshot {} by Tim  ·  <a href=\"https://github.com/TimWJT/screenshot\">GitHub</a>", env!("CARGO_PKG_VERSION")),
        tab,
        ID_LINK,
        16,
        326,
        230,
        20,
    );
    child(w!("BUTTON"), "Save", BS_DEFPUSHBUTTON as u32 | tab, ID_OK, 252, 320, 84, 28);
    child(w!("BUTTON"), "Cancel", BS_PUSHBUTTON as u32 | tab, ID_CANCEL, 340, 320, 84, 28);

    let mut flags = 0;
    if cfg.hotkey_mods & MOD_SHIFT.0 != 0 {
        flags |= HOTKEYF_SHIFT;
    }
    if cfg.hotkey_mods & MOD_CONTROL.0 != 0 {
        flags |= HOTKEYF_CONTROL;
    }
    if cfg.hotkey_mods & MOD_ALT.0 != 0 {
        flags |= HOTKEYF_ALT;
    }
    SendMessageW(hk, HKM_SETHOTKEY, Some(WPARAM(((flags << 8) | cfg.hotkey_vk) as usize)), None);
    let set = |h: HWND, on: bool| {
        SendMessageW(h, BM_SETCHECK, Some(WPARAM(if on { 1 } else { 0 })), None);
    };
    set(prtsc, cfg.printscreen);
    set(startup, autostart_enabled());
    set(auto, cfg.autosave);
    set(updates, cfg.check_updates);
    set(magnifier, cfg.magnifier);
    for (i, pct) in COPY_SCALES.iter().enumerate() {
        let label = if *pct == 100 { "100% (actual size)".to_string() } else { format!("{pct}%") };
        let t = wide(&label);
        SendMessageW(scale, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        if *pct == cfg.copy_scale {
            SendMessageW(scale, CB_SETCURSEL, Some(WPARAM(i)), None);
        }
    }
    let _ = dir;

    // Size the window around a 440×362 client area and centre it on the primary monitor.
    let mut r = RECT { left: 0, top: 0, right: px(440), bottom: px(362) };
    let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE::default(), dpi);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let x = work.left + (work.right - work.left - w) / 2;
    let y = work.top + (work.bottom - work.top - h) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER);

    WINDOW.with(|wnd| *wnd.borrow_mut() = Some(SettingsWindow { hwnd, font }));
    let _ = SetFocus(Some(hk));
}

unsafe fn item(hwnd: HWND, id: i32) -> HWND {
    GetDlgItem(Some(hwnd), id).unwrap_or_default()
}

unsafe fn checked(hwnd: HWND, id: i32) -> bool {
    SendMessageW(item(hwnd, id), BM_GETCHECK, None, None).0 == 1
}

/// Save what's in the window. False, after telling the user why, when something needs fixing.
unsafe fn apply(hwnd: HWND) -> bool {
    let hk = SendMessageW(item(hwnd, ID_HOTKEY), HKM_GETHOTKEY, None, None).0 as u32;
    let vk = hk & 0xff;
    let flags = (hk >> 8) & 0xff;
    let mut mods = 0;
    if flags & HOTKEYF_SHIFT != 0 {
        mods |= MOD_SHIFT.0;
    }
    if flags & HOTKEYF_CONTROL != 0 {
        mods |= MOD_CONTROL.0;
    }
    if flags & HOTKEYF_ALT != 0 {
        mods |= MOD_ALT.0;
    }
    let mut buf = [0u16; 1024];
    let n = GetWindowTextW(item(hwnd, ID_DIR), &mut buf);
    let mut dir = String::from_utf16_lossy(&buf[..n as usize]).trim().to_string();
    if Path::new(&dir) == default_save_dir() {
        dir.clear();
    }
    if vk != 0 && !valid_shortcut(mods, vk) {
        crate::error_box(hwnd, "Pick a shortcut with Ctrl or Alt in it, or a function key such as F9.\n\nA key on its own would start a capture every time you typed it.");
        let _ = SetFocus(Some(item(hwnd, ID_HOTKEY)));
        return false;
    }
    // A relative folder would depend on where the app was started from (System32, at sign-in).
    if !dir.is_empty() && !Path::new(&dir).is_absolute() {
        crate::error_box(hwnd, "The save folder needs to be a full path, such as C:\\Users\\you\\Pictures\\Screenshots.");
        let _ = SetFocus(Some(item(hwnd, ID_DIR)));
        return false;
    }
    let prtsc = checked(hwnd, ID_PRTSC);
    let auto = checked(hwnd, ID_AUTOSAVE);
    let updates = checked(hwnd, ID_UPDATES);
    let magnifier = checked(hwnd, ID_MAGNIFIER);
    let pick = SendMessageW(item(hwnd, ID_SCALE), CB_GETCURSEL, None, None).0;
    let scale = usize::try_from(pick).ok().and_then(|i| COPY_SCALES.get(i).copied());
    update(|c| {
        c.hotkey_mods = mods;
        c.hotkey_vk = vk;
        c.printscreen = prtsc;
        c.save_dir = dir;
        c.autosave = auto;
        c.check_updates = updates;
        c.magnifier = magnifier;
        if let Some(s) = scale {
            c.copy_scale = s;
        }
    });
    set_autostart(checked(hwnd, ID_STARTUP));
    crate::apply_hotkeys();
    true
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            match id {
                ID_OK => {
                    if apply(hwnd) {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_BROWSE => {
                    let mut buf = [0u16; 1024];
                    let n = GetWindowTextW(item(hwnd, ID_DIR), &mut buf);
                    let cur = PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
                    if let Some(p) = crate::dialogs::pick_folder(hwnd, &cur) {
                        let t = wide(&p);
                        let _ = SetWindowTextW(item(hwnd, ID_DIR), pcwstr(&t));
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lp.0 as *const NMHDR);
            if hdr.idFrom == ID_LINK as usize && (hdr.code == NM_CLICK || hdr.code == NM_RETURN) {
                let link = &*(lp.0 as *const NMLINK);
                let url = crate::util::from_wide(&link.item.szUrl);
                let u = wide(&url);
                ShellExecuteW(None, w!("open"), pcwstr(&u), None, None, SW_SHOWNORMAL);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let dc = HDC(wp.0 as *mut _);
            SetBkMode(dc, TRANSPARENT);
            let id = GetDlgCtrlID(HWND(lp.0 as *mut _));
            if id == -1 || id == 0xffff {
                SetTextColor(dc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
            }
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        // While Settings is in front, pressing the current shortcut should record it in the
        // shortcut box, not start a capture.
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                crate::apply_hotkeys();
            } else {
                crate::suspend_hotkeys();
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(w) = WINDOW.with(|w| w.borrow_mut().take()) {
                let _ = DeleteObject(w.font.into());
            }
            crate::apply_hotkeys();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let c = Config { hotkey_mods: MOD_CONTROL.0 | MOD_SHIFT.0, hotkey_vk: 0x53, printscreen: false, save_dir: r"D:\Shots = mine".into(), autosave: true, color: 0x00ff7f, size: 7.0, fill: true, check_updates: false, copy_scale: 150, magnifier: true };
        let back = parse(&format(&c));
        assert_eq!(format(&back), format(&c));
    }

    #[test]
    fn bad_values_fall_back() {
        let c = parse("hotkey_vk=abc\ncolor=#zzzzzz\nsize=999\nnonsense\nunknown=1\n");
        let d = Config::default();
        assert_eq!((c.hotkey_vk, c.color, c.size), (d.hotkey_vk, d.color, 40.0));
        assert_eq!(parse("").hotkey_mods, d.hotkey_mods);
    }

    #[test]
    fn shortcuts_need_a_modifier_unless_special() {
        assert!(valid_shortcut(MOD_ALT.0, VK_F1.0 as u32));
        assert!(valid_shortcut(MOD_CONTROL.0, 0x41));
        assert!(valid_shortcut(0, VK_F9.0 as u32));
        assert!(valid_shortcut(0, VK_SNAPSHOT.0 as u32));
        assert!(!valid_shortcut(0, 0x41), "a bare letter would fire while typing");
        assert!(!valid_shortcut(MOD_SHIFT.0, 0x41), "Shift+letter is just a capital");
        assert!(!valid_shortcut(0, VK_SPACE.0 as u32));
    }

    #[test]
    fn names_shortcuts() {
        assert_eq!(hotkey_name(MOD_ALT.0, VK_F1.0 as u32), "Alt+F1");
        assert_eq!(hotkey_name(MOD_CONTROL.0 | MOD_SHIFT.0, 0x53), "Ctrl+Shift+S");
        assert_eq!(hotkey_name(0, 0x2c), "Print Screen");
    }
}

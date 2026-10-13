//! The full-screen capture window: pick a region, draw on it, then copy, save or pin.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Matrix3x2;

use crate::annot::{contrast, Annot, Tool};
use crate::capture::{self, Image, Shot};
use crate::gfx::{self, affine, brush, rf, v2};
use crate::util::{hinstance, x_lparam, y_lparam};
use crate::{dialogs, pin, settings};

pub const ACCENT: u32 = 0x2f80ed;
const PALETTE: [(u32, &str); 8] = [
    (0xe53935, "Red"),
    (0xfb8c00, "Orange"),
    (0xfdd835, "Yellow"),
    (0x43a047, "Green"),
    (0x1e88e5, "Blue"),
    (0x8e24aa, "Purple"),
    (0x111111, "Black"),
    (0xffffff, "White"),
];
const SIZES: [(f32, &str); 3] = [(3.0, "Thin (1)"), (6.0, "Medium (2)"), (12.0, "Thick (3)")];
const BAR_BG: u32 = 0x202124;
const BAR_HOVER: u32 = 0x3c4043;
const ICON: u32 = 0xe8eaed;

const L: u8 = 1;
const T: u8 = 2;
const R_: u8 = 4;
const B: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    Save,
    Copy,
    Close,
    Size(usize),
    Fill,
    Color(usize),
    Custom,
}

impl Btn {
    fn label(self) -> String {
        match self {
            Btn::Tool(t) => t.label().into(),
            Btn::Undo => "Undo (Ctrl+Z)".into(),
            Btn::Redo => "Redo (Ctrl+Y)".into(),
            Btn::Pin => "Pin to screen (Ctrl+P)".into(),
            Btn::Save => "Save (Ctrl+S)".into(),
            Btn::Copy => "Copy and close (Enter)".into(),
            Btn::Close => "Cancel (Esc)".into(),
            Btn::Size(i) => SIZES[i].1.into(),
            Btn::Fill => "Filled shape (F)".into(),
            Btn::Color(i) => PALETTE[i].1.into(),
            Btn::Custom => "More colours…".into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Rc {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Rc {
    fn span(ax: i32, ay: i32, bx: i32, by: i32) -> Self {
        Rc { l: ax.min(bx), t: ay.min(by), r: ax.max(bx), b: ay.max(by) }
    }
    fn w(&self) -> i32 {
        self.r - self.l
    }
    fn h(&self) -> i32 {
        self.b - self.t
    }
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    fn clip(&self, o: &Rc) -> Option<Rc> {
        let c = Rc { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) };
        (c.w() > 0 && c.h() > 0).then_some(c)
    }
    fn f(&self) -> D2D_RECT_F {
        rf(self.l as f32, self.t as f32, self.r as f32, self.b as f32)
    }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

enum Drag {
    None,
    NewRegion { ax: i32, ay: i32 },
    MoveRegion { ax: i32, ay: i32, orig: Rc },
    Resize { edges: u8, ax: i32, ay: i32, orig: Rc },
    Draw,
    MoveAnnot { idx: usize, lx: f32, ly: f32, moved: bool },
    Button(Btn),
}

#[derive(Default)]
enum Act {
    #[default]
    None,
    Copy,
    /// Straight into the save folder.
    Save,
    /// Through the Save As dialog.
    SaveAs,
    Pin,
    Close,
    PickColor,
    CopyColor(u32),
}

enum Item {
    Btn(Btn),
    Sep,
}

struct Bars {
    rects: Vec<D2D_RECT_F>,
    items: Vec<(Btn, D2D_RECT_F)>,
    seps: Vec<D2D_RECT_F>,
    below: bool,
}

/// The screenshot as GDI bitmaps, plain and dimmed, for the frames drawn before Direct2D is up.
struct GdiShot {
    plain: (HDC, HBITMAP, HGDIOBJ),
    dim: (HDC, HBITMAP, HGDIOBJ),
}

impl GdiShot {
    fn new(img: &Image) -> Option<Self> {
        let make = |dim: bool| unsafe {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: img.w as i32,
                    biHeight: -(img.h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let out = std::slice::from_raw_parts_mut(bits as *mut u8, img.px.len());
            if dim {
                // The same 45% black the Direct2D frame lays over everything outside the selection.
                for (o, &p) in out.iter_mut().zip(&img.px) {
                    *o = ((p as u32 * 141) >> 8) as u8;
                }
            } else {
                out.copy_from_slice(&img.px);
            }
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bmp.into());
            Some((dc, bmp, old))
        };
        Some(GdiShot { plain: make(false)?, dim: make(true)? })
    }
}

impl Drop for GdiShot {
    fn drop(&mut self) {
        for (dc, bmp, old) in [self.plain, self.dim] {
            unsafe {
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(dc);
            }
        }
    }
}

/// Starting the graphics driver is most of the time it takes to show the first Direct2D frame
/// (one to two seconds on some laptops), so a standby process does it before it's woken. A
/// capture started without one starts it on another thread, alongside grabbing the screen,
/// while GDI draws meanwhile. The device is only kept until the window's own render target exists.
static WARM_DONE: AtomicBool = AtomicBool::new(false);
static WARM_DEVICE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static OVERLAY_HWND: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Start the graphics driver now, on this thread.
pub fn warm_gpu_now() {
    warm();
}

/// Start the graphics driver on another thread, unless it already is.
pub fn warm_gpu() {
    if !WARM_DONE.load(Ordering::Acquire) {
        std::thread::spawn(warm);
    }
}

fn warm() {
    unsafe {
        let mut dev: Option<ID3D11Device> = None;
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let _ = D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut dev), None, None);
        if let Some(d) = dev {
            WARM_DEVICE.store(d.into_raw(), Ordering::Release);
        }
        WARM_DONE.store(true, Ordering::Release);
        let hwnd = HWND(OVERLAY_HWND.load(Ordering::Acquire));
        if !hwnd.is_invalid() {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn release_warm_device() {
    let p = WARM_DEVICE.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !p.is_null() {
        drop(unsafe { ID3D11Device::from_raw(p) });
    }
}

struct Overlay {
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    gdi: Option<GdiShot>,
    /// The screenshot, uploaded to `rt`.
    tiles: gfx::Tiles,
    shot: Shot,
    wins: Vec<Rc>,
    mons: Vec<(Rc, f32)>,
    region: Option<Rc>,
    hover: Option<Rc>,
    mx: i32,
    my: i32,
    drag: Drag,
    tool: Tool,
    color: u32,
    size: f32,
    fill: bool,
    annots: Vec<Annot>,
    undo: Vec<Vec<Annot>>,
    redo: Vec<Vec<Annot>>,
    current: Option<Annot>,
    typing: bool,
    typing_saved: bool,
    selected: Option<usize>,
    hot: Option<Btn>,
    /// Show the magnifier without Shift held.
    magnifier: bool,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    static HIGH_SURROGATE: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

pub const CLASS: PCWSTR = w!("TimWJT.Screenshot.Overlay");

pub fn is_open() -> bool {
    OVERLAY.with(|o| o.try_borrow().map(|o| o.is_some()).unwrap_or(true))
}

/// Run `f` on the open overlay. Re-entrant calls (from a nested message loop) are skipped.
fn with<R: Default>(f: impl FnOnce(&mut Overlay) -> R) -> R {
    OVERLAY.with(|o| match o.try_borrow_mut() {
        Ok(mut o) => o.as_mut().map(f).unwrap_or_default(),
        Err(_) => R::default(),
    })
}

pub fn open(shot: Shot, full: bool) {
    if is_open() {
        return;
    }
    unsafe {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });

        let (ox, oy) = (shot.x, shot.y);
        let to_client = |r: RECT| Rc { l: r.left - ox, t: r.top - oy, r: r.right - ox, b: r.bottom - oy };
        let screen = Rc { l: 0, t: 0, r: shot.img.w as i32, b: shot.img.h as i32 };
        let wins = capture::window_rects().into_iter().filter_map(|r| to_client(r).clip(&screen)).collect();
        let mons: Vec<(Rc, f32)> = capture::monitors().into_iter().map(|(r, s)| (to_client(r), s)).collect();

        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let (mx, my) = (cur.x - ox, cur.y - oy);
        let cfg = settings::get();

        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Screenshot"),
            WS_POPUP,
            ox,
            oy,
            shot.img.w as i32,
            shot.img.h as i32,
            None,
            None,
            Some(hinstance()),
            None,
        ) else {
            return;
        };

        let mut o = Overlay {
            hwnd,
            rt: None,
            gdi: None,
            tiles: Vec::new(),
            shot,
            wins,
            mons,
            region: None,
            hover: None,
            mx,
            my,
            drag: Drag::None,
            tool: Tool::Select,
            color: cfg.color,
            size: cfg.size,
            fill: cfg.fill,
            annots: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            current: None,
            typing: false,
            typing_saved: false,
            selected: None,
            hot: None,
            magnifier: cfg.magnifier,
        };
        if full {
            o.region = Some(o.monitor_at(mx, my).0);
        } else {
            o.update_hover();
        }
        if !WARM_DONE.load(Ordering::Acquire) {
            o.gdi = GdiShot::new(&o.shot.img);
        }
        OVERLAY.with(|cell| *cell.borrow_mut() = Some(o));
        OVERLAY_HWND.store(hwnd.0, Ordering::Release);

        let _ = ShowWindow(hwnd, SW_SHOW);
        crate::util::force_foreground(hwnd);
        let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
    }
}

fn close() {
    let taken = OVERLAY.with(|o| o.try_borrow_mut().ok().and_then(|mut o| o.take()));
    if let Some(o) = taken {
        settings::update(|c| {
            c.color = o.color;
            c.size = o.size;
            c.fill = o.fill;
        });
        let hwnd = o.hwnd;
        OVERLAY_HWND.store(std::ptr::null_mut(), Ordering::Release);
        drop(o);
        release_warm_device();
        // It's behind the editor, so going first can't flash anything.
        crate::dim::close();
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        crate::check_exit();
    }
}

fn run(act: Act) {
    match act {
        Act::None => {}
        Act::Close => close(),
        Act::Copy => {
            let Some((hwnd, img)) = with(|o| o.export().map(|i| (o.hwnd, i))) else { return };
            let pct = settings::get().copy_scale;
            let scaled = if pct == 100 { None } else { gfx::scale_image(&img, pct) };
            if !gfx::copy_image(hwnd, scaled.as_ref().unwrap_or(&img)) {
                // Keep the editor open so the capture isn't lost.
                crate::error_box(hwnd, "Could not copy to the clipboard. Another app may be using it; try again.");
                return;
            }
            close();
            // After closing, so a "could not save" message isn't hidden behind the editor.
            crate::autosave(&img);
        }
        Act::Save => {
            let Some((hwnd, img)) = with(|o| o.export().map(|i| (o.hwnd, i))) else { return };
            match crate::save_to_folder(&img) {
                Ok(path) => {
                    close();
                    crate::notify_saved(&path);
                }
                // Keep the editor open so the capture isn't lost.
                Err(msg) => crate::error_box(hwnd, &msg),
            }
        }
        Act::SaveAs => {
            let Some((hwnd, img)) = with(|o| o.export().map(|i| (o.hwnd, i))) else { return };
            let cfg = settings::get();
            if let Some(path) = dialogs::save_as(hwnd, &cfg.save_path(), &crate::util::timestamp_name()) {
                if let Err(e) = gfx::save_image(&img, &path) {
                    crate::error_box(hwnd, &format!("Could not save {}\n\n{e}", path.display()));
                    return;
                }
                close();
            }
        }
        Act::Pin => {
            let Some((img, x, y)) = with(|o| {
                let r = o.region?;
                o.export().map(|i| (i, r.l + o.shot.x, r.t + o.shot.y))
            }) else {
                return;
            };
            close();
            pin::open(img, x, y);
        }
        Act::PickColor => {
            let Some((hwnd, color)) = with(|o| Some((o.hwnd, o.color))) else { return };
            if let Some(c) = dialogs::choose_color(hwnd, color) {
                with(|o| {
                    o.color = c;
                    o.recolor_selected();
                    o.redraw();
                });
            }
        }
        Act::CopyColor(rgb) => {
            if let Some(hwnd) = with(|o| Some(o.hwnd)) {
                gfx::copy_text(hwnd, &format!("#{rgb:06X}"));
            }
            close();
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = (x_lparam(lp), y_lparam(lp));
    let act = match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            with(|o| o.paint());
            let _ = ValidateRect(Some(hwnd), None);
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let id = with(|o| Some(o.cursor())).unwrap_or(IDC_ARROW);
            if let Ok(c) = LoadCursorW(None, id) {
                SetCursor(Some(c));
            }
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => with(|o| o.on_down(x, y, false)),
        WM_LBUTTONDBLCLK => with(|o| o.on_down(x, y, true)),
        WM_MOUSEMOVE => {
            with(|o| o.on_move(x, y));
            Act::None
        }
        WM_LBUTTONUP => with(|o| o.on_up(x, y)),
        WM_RBUTTONUP => with(|o| o.on_right()),
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16;
            with(|o| o.on_wheel(delta));
            Act::None
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            if msg == WM_SYSKEYDOWN && vk == VK_F4 {
                Act::Close
            } else {
                let a = with(|o| o.on_key(vk));
                if msg == WM_SYSKEYDOWN {
                    run(a);
                    return LRESULT(0);
                }
                a
            }
        }
        WM_KEYUP if wp.0 as u16 == VK_SHIFT.0 => {
            with(|o| o.redraw());
            Act::None
        }
        // Swallow Alt on its own so it does not enter menu mode on a window with no menu.
        WM_SYSKEYUP | WM_SYSCHAR => return LRESULT(0),
        WM_CHAR => {
            // Characters beyond U+FFFF (emoji) arrive as two WM_CHARs, one per surrogate.
            let unit = wp.0 as u16;
            let c = HIGH_SURROGATE.with(|high| match unit {
                0xd800..=0xdbff => {
                    high.set(unit);
                    None
                }
                0xdc00..=0xdfff => char::decode_utf16([high.replace(0), unit]).next()?.ok(),
                _ => char::from_u32(unit as u32),
            });
            if let Some(c) = c {
                with(|o| o.on_char(c));
            }
            Act::None
        }
        // Lost the mouse mid-drag (Alt+Tab, a dialog, Ctrl+Alt+Del): drop the drag.
        WM_CAPTURECHANGED => {
            with(|o| {
                if HWND(lp.0 as *mut _) != o.hwnd {
                    o.cancel_drag();
                }
            });
            Act::None
        }
        // The window spans every monitor on purpose; ignore the size Windows suggests.
        WM_DPICHANGED => return LRESULT(0),
        WM_CLOSE => Act::Close,
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    };
    run(act);
    LRESULT(0)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

impl Overlay {
    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn screen(&self) -> Rc {
        Rc { l: 0, t: 0, r: self.shot.img.w as i32, b: self.shot.img.h as i32 }
    }

    fn monitor_at(&self, x: i32, y: i32) -> (Rc, f32) {
        if let Some(m) = self.mons.iter().find(|m| m.0.contains(x, y)) {
            return *m;
        }
        let dist = |r: &Rc| {
            let dx = (r.l - x).max(0).max(x - r.r);
            let dy = (r.t - y).max(0).max(y - r.b);
            dx as i64 * dx as i64 + dy as i64 * dy as i64
        };
        self.mons.iter().min_by_key(|m| dist(&m.0)).copied().unwrap_or((self.screen(), 1.0))
    }

    fn update_hover(&mut self) {
        let (x, y) = (self.mx, self.my);
        self.hover = self
            .wins
            .iter()
            .find(|r| r.contains(x, y))
            .copied()
            .or_else(|| self.monitor_at(x, y).0.clip(&self.screen()));
    }

    /// Region as shown right now, including one being dragged out.
    fn live_region(&self) -> Option<Rc> {
        if let Drag::NewRegion { ax, ay } = self.drag {
            return Some(Rc::span(ax, ay, self.mx, self.my).clip(&self.screen()).unwrap_or(Rc { l: ax, t: ay, r: ax, b: ay }));
        }
        self.region
    }

    fn editing(&self) -> bool {
        self.region.is_some() && !matches!(self.drag, Drag::NewRegion { .. })
    }

    fn scale(&self) -> f32 {
        match self.live_region() {
            Some(r) => self.monitor_at(r.r - 1, r.b - 1).1,
            None => self.monitor_at(self.mx, self.my).1,
        }
    }

    // ---- toolbar -------------------------------------------------------------------------

    fn bars(&self) -> Option<Bars> {
        let r = self.region?;
        if !self.editing() {
            return None;
        }
        let s = self.scale();
        let (btn, pad, sep, gap) = (30.0 * s, 4.0 * s, 9.0 * s, 6.0 * s);

        let mut main: Vec<Item> = Tool::ALL.iter().map(|&t| Item::Btn(Btn::Tool(t))).collect();
        main.extend([Item::Sep, Item::Btn(Btn::Undo), Item::Btn(Btn::Redo), Item::Sep]);
        main.extend([Item::Btn(Btn::Pin), Item::Btn(Btn::Save), Item::Btn(Btn::Copy), Item::Btn(Btn::Close)]);

        let mut rows = vec![main];
        if self.tool != Tool::Select {
            let mut props: Vec<Item> = (0..SIZES.len()).map(|i| Item::Btn(Btn::Size(i))).collect();
            if self.tool.fillable() {
                props.extend([Item::Sep, Item::Btn(Btn::Fill)]);
            }
            if !self.tool.colorless() {
                props.push(Item::Sep);
                props.extend((0..PALETTE.len()).map(|i| Item::Btn(Btn::Color(i))));
                props.push(Item::Btn(Btn::Custom));
            }
            rows.push(props);
        }

        let width = |row: &[Item]| row.iter().map(|i| if matches!(i, Item::Sep) { sep } else { btn }).sum::<f32>() + pad * 2.0;
        let row_h = btn + pad * 2.0;
        let total_h = rows.len() as f32 * row_h + (rows.len() - 1) as f32 * gap;
        let (mon, _) = self.monitor_at(r.r - 1, r.b - 1);
        let (mt, mb) = (mon.t as f32 + 4.0 * s, mon.b as f32 - 4.0 * s);

        let (mut y, below) = if r.b as f32 + 8.0 * s + total_h <= mb {
            (r.b as f32 + 8.0 * s, true)
        } else if r.t as f32 - 8.0 * s - total_h >= mt {
            (r.t as f32 - 8.0 * s - total_h, false)
        } else {
            ((r.b as f32 - 8.0 * s - total_h).max(mt), true)
        };

        let mut bars = Bars { rects: vec![], items: vec![], seps: vec![], below };
        for row in &rows {
            let w = width(row);
            let x0 = (r.r as f32 - w).min(mon.r as f32 - 4.0 * s - w).max(mon.l as f32 + 4.0 * s);
            bars.rects.push(rf(x0, y, x0 + w, y + row_h));
            let mut x = x0 + pad;
            for item in row {
                match item {
                    Item::Sep => {
                        bars.seps.push(rf(x + sep / 2.0, y + pad + 6.0 * s, x + sep / 2.0, y + row_h - pad - 6.0 * s));
                        x += sep;
                    }
                    Item::Btn(b) => {
                        bars.items.push((*b, rf(x, y + pad, x + btn, y + pad + btn)));
                        x += btn;
                    }
                }
            }
            y += row_h + gap;
        }
        Some(bars)
    }

    fn bar_hit(&self, x: i32, y: i32) -> (bool, Option<Btn>) {
        let Some(b) = self.bars() else { return (false, None) };
        let (fx, fy) = (x as f32, y as f32);
        let on_bar = b.rects.iter().any(|r| inside(r, fx, fy));
        (on_bar, b.items.iter().find(|(_, r)| inside(r, fx, fy)).map(|(b, _)| *b))
    }

    fn press(&mut self, b: Btn) -> Act {
        match b {
            Btn::Tool(t) => self.set_tool(t),
            Btn::Undo => self.undo(),
            Btn::Redo => self.redo(),
            Btn::Pin => return Act::Pin,
            Btn::Save => return Act::Save,
            Btn::Copy => return Act::Copy,
            Btn::Close => return Act::Close,
            Btn::Size(i) => {
                self.size = SIZES[i].0;
                self.resize_selected();
            }
            Btn::Fill => {
                self.fill = !self.fill;
                if let Some(a) = self.selected.and_then(|i| self.annots.get(i)) {
                    if a.tool.fillable() {
                        let i = self.selected.unwrap();
                        self.save_undo();
                        self.annots[i].fill = self.fill;
                    }
                }
            }
            Btn::Color(i) => {
                self.color = PALETTE[i].0;
                self.recolor_selected();
            }
            Btn::Custom => return Act::PickColor,
        }
        self.redraw();
        Act::None
    }

    fn set_tool(&mut self, t: Tool) {
        self.commit_text();
        self.tool = t;
        self.selected = None;
    }

    fn recolor_selected(&mut self) {
        if let Some(i) = self.selected {
            if !self.annots[i].tool.colorless() {
                self.save_undo();
                self.annots[i].color = self.color;
            }
        }
        if let Some(c) = self.current.as_mut() {
            c.color = self.color;
        }
    }

    fn resize_selected(&mut self) {
        if let Some(i) = self.selected {
            self.save_undo();
            self.annots[i].size = self.size;
        }
        if let Some(c) = self.current.as_mut() {
            c.size = self.size;
        }
    }

    // ---- history -------------------------------------------------------------------------

    fn save_undo(&mut self) {
        self.undo.push(self.annots.clone());
        self.redo.clear();
    }

    fn undo(&mut self) {
        self.commit_text();
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.annots, prev));
            self.selected = None;
        }
    }

    fn redo(&mut self) {
        self.commit_text();
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.annots, next));
            self.selected = None;
        }
    }

    // ---- text ----------------------------------------------------------------------------

    fn start_text(&mut self, x: i32, y: i32) {
        self.commit_text();
        let hit = self.annots.iter().rposition(|a| a.tool == Tool::Text && a.hit(x as f32, y as f32));
        if let Some(i) = hit {
            self.save_undo();
            self.current = Some(self.annots.remove(i));
            self.typing_saved = true;
        } else {
            // Put the click at the middle of the first line rather than its top.
            let px = crate::annot::text_px(self.size) * self.scale();
            self.current = Some(Annot::new(Tool::Text, self.color, self.size, false, self.scale(), [x as f32, y as f32 - px * 0.66]));
            self.typing_saved = false;
        }
        self.typing = true;
        self.selected = None;
    }

    fn commit_text(&mut self) {
        if !self.typing {
            return;
        }
        self.typing = false;
        if let Some(a) = self.current.take() {
            if a.meaningful() {
                if !self.typing_saved {
                    self.save_undo();
                }
                self.annots.push(a);
            }
        }
        self.redraw();
    }

    fn on_char(&mut self, c: char) {
        if !self.typing {
            return;
        }
        let Some(a) = self.current.as_mut() else { return };
        match c {
            '\u{8}' => {
                a.text.pop();
            }
            '\r' => a.text.push('\n'),
            c if !c.is_control() => a.text.push(c),
            _ => {}
        }
        self.redraw();
    }

    // ---- mouse ---------------------------------------------------------------------------

    fn edges_at(&self, x: i32, y: i32) -> u8 {
        let Some(r) = self.region else { return 0 };
        let tol = (6.0 * self.scale()).round() as i32;
        let near = |a: i32, b: i32| (a - b).abs() <= tol;
        let in_y = y >= r.t - tol && y <= r.b + tol;
        let in_x = x >= r.l - tol && x <= r.r + tol;
        let mut e = 0;
        if in_y && near(x, r.l) {
            e |= L;
        }
        if in_y && near(x, r.r) {
            e |= R_;
        }
        if in_x && near(y, r.t) {
            e |= T;
        }
        if in_x && near(y, r.b) {
            e |= B;
        }
        if e & L != 0 && e & R_ != 0 {
            e &= !L;
        }
        if e & T != 0 && e & B != 0 {
            e &= !T;
        }
        if self.tool != Tool::Select {
            // Drawing tools only resize from the handles, so strokes can start near the edge.
            let (cx, cy) = ((r.l + r.r) / 2, (r.t + r.b) / 2);
            let on_handle = (e & (L | R_) != 0 && (near(y, r.t) || near(y, r.b) || near(y, cy)))
                || (e & (T | B) != 0 && (near(x, r.l) || near(x, r.r) || near(x, cx)));
            if !on_handle {
                e = 0;
            }
        }
        e
    }

    fn annot_at(&self, x: i32, y: i32) -> Option<usize> {
        self.annots.iter().rposition(|a| a.hit(x as f32, y as f32))
    }

    fn on_down(&mut self, x: i32, y: i32, dbl: bool) -> Act {
        self.mx = x;
        self.my = y;
        unsafe {
            SetCapture(self.hwnd);
        }
        let (on_bar, btn) = self.bar_hit(x, y);
        if on_bar {
            self.drag = btn.map(Drag::Button).unwrap_or(Drag::None);
            self.redraw();
            return Act::None;
        }

        if self.typing {
            if let Some(a) = &self.current {
                if a.hit(x as f32, y as f32) {
                    return Act::None;
                }
            }
            self.commit_text();
            if self.tool == Tool::Text {
                // A click away from the text just finishes it; the next click starts another.
                return Act::None;
            }
        }

        let Some(r) = self.region else {
            self.drag = Drag::NewRegion { ax: x, ay: y };
            self.redraw();
            return Act::None;
        };

        let edges = self.edges_at(x, y);
        if edges != 0 {
            self.drag = Drag::Resize { edges, ax: x, ay: y, orig: r };
            return Act::None;
        }

        match self.tool {
            Tool::Select => {
                if let Some(i) = self.annot_at(x, y) {
                    self.selected = Some(i);
                    self.drag = Drag::MoveAnnot { idx: i, lx: x as f32, ly: y as f32, moved: false };
                } else if r.contains(x, y) {
                    if dbl {
                        return Act::Copy;
                    }
                    self.selected = None;
                    self.drag = Drag::MoveRegion { ax: x, ay: y, orig: r };
                } else if self.annots.is_empty() {
                    self.selected = None;
                    self.region = None;
                    self.drag = Drag::NewRegion { ax: x, ay: y };
                } else {
                    self.selected = None;
                }
            }
            _ if !r.contains(x, y) => {}
            Tool::Text => self.start_text(x, y),
            Tool::Step => {
                let n = self.annots.iter().filter(|a| a.tool == Tool::Step).filter_map(|a| a.text.parse::<u32>().ok()).max().unwrap_or(0) + 1;
                let mut a = Annot::new(Tool::Step, self.color, self.size, false, self.scale(), [x as f32, y as f32]);
                a.text = n.to_string();
                self.save_undo();
                self.annots.push(a);
            }
            t => {
                self.selected = None;
                self.current = Some(Annot::new(t, self.color, self.size, self.fill, self.scale(), [x as f32, y as f32]));
                self.drag = Drag::Draw;
            }
        }
        self.redraw();
        Act::None
    }

    fn on_move(&mut self, x: i32, y: i32) {
        self.mx = x;
        self.my = y;
        let shift = key_down(VK_SHIFT);
        let screen = self.screen();
        match &mut self.drag {
            Drag::None => {
                if self.region.is_none() {
                    self.update_hover();
                }
                self.hot = self.bar_hit(x, y).1;
            }
            Drag::NewRegion { .. } | Drag::Button(_) => {}
            Drag::MoveRegion { ax, ay, orig } => {
                let dx = (x - *ax).clamp(-orig.l, screen.r - orig.r);
                let dy = (y - *ay).clamp(-orig.t, screen.b - orig.b);
                self.region = Some(Rc { l: orig.l + dx, t: orig.t + dy, r: orig.r + dx, b: orig.b + dy });
            }
            Drag::Resize { edges, ax, ay, orig } => {
                let (dx, dy) = (x - *ax, y - *ay);
                let mut n = *orig;
                if *edges & L != 0 {
                    n.l += dx;
                }
                if *edges & R_ != 0 {
                    n.r += dx;
                }
                if *edges & T != 0 {
                    n.t += dy;
                }
                if *edges & B != 0 {
                    n.b += dy;
                }
                let n = Rc::span(n.l, n.t, n.r, n.b);
                if let Some(c) = n.clip(&screen) {
                    self.region = Some(c);
                }
            }
            Drag::Draw => {
                if let Some(a) = self.current.as_mut() {
                    let p = [x as f32, y as f32];
                    match a.tool {
                        Tool::Pen | Tool::Marker => {
                            let last = *a.pts.last().unwrap();
                            if shift {
                                // Shift draws a straight stroke from where it began.
                                a.pts.truncate(1);
                                a.pts.push(p);
                            } else if (last[0] - p[0]).abs() + (last[1] - p[1]).abs() >= 1.5 {
                                a.pts.push(p);
                            }
                        }
                        Tool::Line | Tool::Arrow => {
                            a.pts[1] = if shift { snap_angle(a.pts[0], p) } else { p };
                        }
                        _ => {
                            a.pts[1] = if shift { square(a.pts[0], p) } else { p };
                        }
                    }
                }
            }
            Drag::MoveAnnot { idx, lx, ly, moved } => {
                let (dx, dy) = (x as f32 - *lx, y as f32 - *ly);
                if dx != 0.0 || dy != 0.0 {
                    let i = *idx;
                    if !*moved {
                        *moved = true;
                        self.undo.push(self.annots.clone());
                        self.redo.clear();
                    }
                    *lx = x as f32;
                    *ly = y as f32;
                    if let Some(a) = self.annots.get_mut(i) {
                        a.translate(dx, dy);
                    }
                }
            }
        }
        self.redraw();
    }

    fn on_up(&mut self, x: i32, y: i32) -> Act {
        unsafe {
            let _ = ReleaseCapture();
        }
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        let mut act = Act::None;
        match drag {
            Drag::Button(b) => {
                if self.bar_hit(x, y).1 == Some(b) {
                    act = self.press(b);
                }
            }
            Drag::NewRegion { ax, ay } => {
                let tiny = (x - ax).abs() < 4 && (y - ay).abs() < 4;
                // The window under the click, not whatever was hovered before a region was picked.
                self.region = if tiny {
                    self.update_hover();
                    self.hover
                } else {
                    Rc::span(ax, ay, x, y).clip(&self.screen())
                };
                if self.region.is_some_and(|r| r.w() < 2 || r.h() < 2) {
                    self.region = None;
                }
                if self.region.is_none() {
                    self.update_hover();
                }
            }
            Drag::Draw => {
                if let Some(a) = self.current.take() {
                    if a.meaningful() {
                        self.save_undo();
                        self.annots.push(a);
                    }
                }
            }
            _ => {}
        }
        self.redraw();
        act
    }

    fn on_right(&mut self) -> Act {
        if self.typing {
            self.commit_text();
            return Act::None;
        }
        if !matches!(self.drag, Drag::None) {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.cancel_drag();
            return Act::None;
        }
        match self.region {
            None => Act::Close,
            Some(_) if self.annots.is_empty() => {
                // Back to picking a region.
                self.region = None;
                self.tool = Tool::Select;
                self.update_hover();
                self.redraw();
                Act::None
            }
            Some(_) => Act::None,
        }
    }

    fn cancel_drag(&mut self) {
        if matches!(self.drag, Drag::None) {
            return;
        }
        // Put back whatever the drag had moved.
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Draw => self.current = None,
            Drag::MoveRegion { orig, .. } | Drag::Resize { orig, .. } => self.region = Some(orig),
            Drag::MoveAnnot { moved: true, .. } => {
                if let Some(before) = self.undo.pop() {
                    self.annots = before;
                }
            }
            _ => {}
        }
        self.redraw();
    }

    fn on_wheel(&mut self, delta: i16) {
        if self.region.is_none() || self.tool == Tool::Select {
            return;
        }
        let step = if delta > 0 { 1.0 } else { -1.0 };
        self.size = (self.size + step).clamp(1.0, 40.0);
        self.resize_selected();
        self.redraw();
    }

    fn on_key(&mut self, vk: VIRTUAL_KEY) -> Act {
        let ctrl = key_down(VK_CONTROL);
        let shift = key_down(VK_SHIFT);

        // Holding Shift shows the magnifier.
        if vk == VK_SHIFT {
            self.redraw();
            return Act::None;
        }

        if self.typing {
            if ctrl && matches!(vk, VK_C | VK_S | VK_P) {
                // Copy, save and pin work mid-text too: finish the text, then go on below.
                self.commit_text();
            } else {
                match vk {
                    VK_ESCAPE => self.commit_text(),
                    VK_RETURN if ctrl => self.commit_text(),
                    VK_V if ctrl => {
                        if let Some(t) = gfx::paste_text(self.hwnd) {
                            if let Some(a) = self.current.as_mut() {
                                a.text.push_str(&t.replace("\r\n", "\n"));
                            }
                        }
                    }
                    _ => return Act::None,
                }
                self.redraw();
                return Act::None;
            }
        }

        if vk == VK_ESCAPE {
            if !matches!(self.drag, Drag::None) {
                return self.on_right();
            }
            return Act::Close;
        }

        // Undo or delete mid-drag would leave the drag holding the wrong drawing, or none.
        if !matches!(self.drag, Drag::None) && matches!(vk, VK_Z | VK_Y | VK_DELETE | VK_BACK) {
            return Act::None;
        }

        let Some(r) = self.region else {
            // Still picking.
            match vk {
                VK_RETURN | VK_SPACE => {
                    self.region = self.hover;
                }
                VK_F => self.region = Some(self.monitor_at(self.mx, self.my).0),
                VK_A if ctrl => self.region = Some(self.screen()),
                VK_C => return Act::CopyColor(self.shot.img.pixel(self.mx, self.my)),
                VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => nudge_cursor(vk, if shift { 10 } else { 1 }),
                _ => {}
            }
            self.redraw();
            return Act::None;
        };

        if ctrl {
            match vk {
                VK_C => return Act::Copy,
                VK_S if shift => return Act::SaveAs,
                VK_S => return Act::Save,
                VK_P => return Act::Pin,
                VK_Z if shift => self.redo(),
                VK_Z => self.undo(),
                VK_Y => self.redo(),
                VK_A => self.region = Some(self.screen()),
                _ => return Act::None,
            }
            self.redraw();
            return Act::None;
        }

        match vk {
            VK_RETURN => return Act::Copy,
            VK_DELETE | VK_BACK => {
                if let Some(i) = self.selected.take() {
                    self.save_undo();
                    self.annots.remove(i);
                }
            }
            VK_F => {
                return self.press(Btn::Fill);
            }
            VK_1 | VK_2 | VK_3 => {
                return self.press(Btn::Size((vk.0 - VK_1.0) as usize));
            }
            VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                let n = if shift { 10 } else { 1 };
                let (dx, dy) = match vk {
                    VK_LEFT => (-n, 0),
                    VK_RIGHT => (n, 0),
                    VK_UP => (0, -n),
                    _ => (0, n),
                };
                if let Some(i) = self.selected {
                    self.save_undo();
                    self.annots[i].translate(dx as f32, dy as f32);
                } else {
                    let s = self.screen();
                    let dx = dx.clamp(-r.l, s.r - r.r);
                    let dy = dy.clamp(-r.t, s.b - r.b);
                    self.region = Some(Rc { l: r.l + dx, t: r.t + dy, r: r.r + dx, b: r.b + dy });
                }
            }
            _ => match Tool::from_key(vk.0) {
                Some(t) => self.set_tool(t),
                None => return Act::None,
            },
        }
        self.redraw();
        Act::None
    }

    fn cursor(&self) -> PCWSTR {
        if self.bar_hit(self.mx, self.my).0 {
            return IDC_ARROW;
        }
        let edge_cursor = |e: u8| match e {
            e if e == L | T || e == R_ | B => IDC_SIZENWSE,
            e if e == R_ | T || e == L | B => IDC_SIZENESW,
            L | R_ => IDC_SIZEWE,
            _ => IDC_SIZENS,
        };
        match &self.drag {
            Drag::NewRegion { .. } | Drag::Draw => return IDC_CROSS,
            Drag::MoveRegion { .. } | Drag::MoveAnnot { .. } => return IDC_SIZEALL,
            Drag::Resize { edges, .. } => return edge_cursor(*edges),
            Drag::Button(_) => return IDC_ARROW,
            Drag::None => {}
        }
        let Some(r) = self.region else { return IDC_CROSS };
        let e = self.edges_at(self.mx, self.my);
        if e != 0 {
            return edge_cursor(e);
        }
        let within = r.contains(self.mx, self.my);
        match self.tool {
            Tool::Select if self.annot_at(self.mx, self.my).is_some() || within => IDC_SIZEALL,
            Tool::Select => IDC_ARROW,
            _ if !within => IDC_ARROW,
            Tool::Text => IDC_IBEAM,
            _ => IDC_CROSS,
        }
    }

    // ---- output --------------------------------------------------------------------------

    fn export(&mut self) -> Option<Image> {
        self.commit_text();
        let r = self.region?;
        let img = &self.shot.img;
        if self.annots.is_empty() {
            // Nothing drawn on it: just the pixels, without a round trip through Direct2D.
            return Some(img.crop(r.l as u32, r.t as u32, r.w() as u32, r.h() as u32));
        }
        let annots = &self.annots;
        gfx::render_image(r.w() as u32, r.h() as u32, |rt| unsafe {
            gfx::draw_tiles(rt, &gfx::tiles_of(rt, img, r.l as u32, r.t as u32, r.w() as u32, r.h() as u32));
            rt.SetTransform(&Matrix3x2::translation(-r.l as f32, -r.t as f32));
            for a in annots {
                a.draw(rt, img);
            }
        })
    }

    // ---- painting ------------------------------------------------------------------------

    fn ensure_target(&mut self) -> Option<()> {
        if self.rt.is_some() {
            return Some(());
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES { dpiX: 96.0, dpiY: 96.0, ..Default::default() };
        let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U { width: self.shot.img.w, height: self.shot.img.h },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let side = self.shot.img.w.max(self.shot.img.h);
        let rt = gfx::with(|g| unsafe {
            // A desktop wider than the GPU allows can't have a hardware target; use software then.
            g.d2d
                .CreateHwndRenderTarget(&props, &hp)
                .ok()
                .filter(|rt| rt.GetMaximumBitmapSize() >= side)
                .or_else(|| {
                    let soft = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, ..props };
                    g.d2d.CreateHwndRenderTarget(&soft, &hp).ok()
                })
        })?;
        let img = &self.shot.img;
        self.tiles = gfx::tiles_of(&rt, img, 0, 0, img.w, img.h);
        self.rt = Some(rt);
        self.gdi = None;
        release_warm_device();
        Some(())
    }

    fn paint(&mut self) {
        if self.rt.is_none() && self.gdi.is_some() && !WARM_DONE.load(Ordering::Acquire) {
            self.paint_gdi();
            return;
        }
        if self.ensure_target().is_none() {
            return;
        }
        let rt: ID2D1RenderTarget = (*self.rt.clone().unwrap()).clone();
        unsafe {
            rt.BeginDraw();
            rt.SetTransform(&Matrix3x2::identity());
            gfx::draw_tiles(&rt, &self.tiles);
            self.paint_scene(&rt);
            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.tiles.clear();
                    self.redraw();
                }
            }
        }
    }

    /// Just the screenshot, dimmed outside the selection, with its border.
    fn paint_gdi(&self) {
        let Some(g) = &self.gdi else { return };
        let (w, h) = (self.shot.img.w as i32, self.shot.img.h as i32);
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            let blt = |src: HDC, l: i32, t: i32, r: i32, b: i32| {
                if r > l && b > t {
                    let _ = BitBlt(hdc, l, t, r - l, b - t, Some(src), l, t, SRCCOPY);
                }
            };
            match self.live_region() {
                Some(r) => {
                    blt(g.dim.0, 0, 0, w, r.t);
                    blt(g.dim.0, 0, r.b, w, h);
                    blt(g.dim.0, 0, r.t, r.l, r.b);
                    blt(g.dim.0, r.r, r.t, w, r.b);
                    blt(g.plain.0, r.l, r.t, r.r, r.b);
                    let bw = (1.5 * self.scale()).round().max(1.0) as i32;
                    let accent = CreateSolidBrush(COLORREF(gfx_bgr(ACCENT)));
                    for (l, t, rr, b) in [
                        (r.l - bw, r.t - bw, r.r + bw, r.t),
                        (r.l - bw, r.b, r.r + bw, r.b + bw),
                        (r.l - bw, r.t, r.l, r.b),
                        (r.r, r.t, r.r + bw, r.b),
                    ] {
                        FillRect(hdc, &RECT { left: l, top: t, right: rr, bottom: b }, accent);
                    }
                    let _ = DeleteObject(accent.into());
                }
                None => blt(g.dim.0, 0, 0, w, h),
            }
            ReleaseDC(Some(self.hwnd), hdc);
        }
    }

    unsafe fn paint_scene(&self, rt: &ID2D1RenderTarget) {
        let s = self.scale();
        let (w, h) = (self.shot.img.w as f32, self.shot.img.h as f32);
        let dim = brush(rt, 0x000000, 0.45);
        let accent = brush(rt, ACCENT, 1.0);
        let white = brush(rt, 0xffffff, 1.0);

        // Until something is selected the whole screen dims evenly. Lighting up the window
        // under the mouse made the taskbar and the rest of the screen look highlighted apart.
        let lit = self.live_region();
        match lit {
            Some(r) => {
                let (l, t, rr, b) = (r.l as f32, r.t as f32, r.r as f32, r.b as f32);
                rt.FillRectangle(&rf(0.0, 0.0, w, t), &dim);
                rt.FillRectangle(&rf(0.0, b, w, h), &dim);
                rt.FillRectangle(&rf(0.0, t, l, b), &dim);
                rt.FillRectangle(&rf(rr, t, w, b), &dim);
            }
            None => rt.FillRectangle(&rf(0.0, 0.0, w, h), &dim),
        }

        if let Some(r) = lit {
            rt.PushAxisAlignedClip(&r.f(), D2D1_ANTIALIAS_MODE_ALIASED);
            for a in &self.annots {
                a.draw(rt, &self.shot.img);
            }
            if let Some(a) = &self.current {
                a.draw(rt, &self.shot.img);
            }
            rt.PopAxisAlignedClip();

            let bw = (1.5 * s).max(1.0);
            let f = r.f();
            rt.DrawRectangle(&rf(f.left - bw / 2.0, f.top - bw / 2.0, f.right + bw / 2.0, f.bottom + bw / 2.0), &accent, bw, None);

            if self.region.is_some() || matches!(self.drag, Drag::NewRegion { .. }) {
                self.size_label(rt, r, s);
            }
            if self.editing() {
                let hs = (7.0 * s).round();
                let (cx, cy) = ((f.left + f.right) / 2.0, (f.top + f.bottom) / 2.0);
                for (x, y) in [
                    (f.left, f.top),
                    (cx, f.top),
                    (f.right, f.top),
                    (f.left, cy),
                    (f.right, cy),
                    (f.left, f.bottom),
                    (cx, f.bottom),
                    (f.right, f.bottom),
                ] {
                    let hr = rf(x - hs / 2.0, y - hs / 2.0, x + hs / 2.0, y + hs / 2.0);
                    rt.FillRectangle(&hr, &white);
                    rt.DrawRectangle(&hr, &accent, 1.0 * s, None);
                }
            }
        }

        if let Some(a) = self.selected.and_then(|i| self.annots.get(i)) {
            let (l, t, r, b) = a.bounds();
            gfx::with(|g| rt.DrawRectangle(&rf(l - 4.0, t - 4.0, r + 4.0, b + 4.0), &white, 1.0 * s, &g.dash));
        }

        if self.typing {
            if let Some(a) = &self.current {
                let ([x, y], ch) = a.caret();
                let c = brush(rt, a.color, 1.0);
                rt.DrawLine(v2(x + 1.0, y), v2(x + 1.0, y + ch), &c, (1.5 * s).max(1.0), None);
            }
        }

        self.paint_bars(rt, s);

        let picking = self.region.is_none() || matches!(self.drag, Drag::NewRegion { .. } | Drag::Resize { .. });
        if picking && (self.magnifier || key_down(VK_SHIFT)) {
            self.magnifier(rt, s);
        }
    }

    unsafe fn label(&self, rt: &ID2D1RenderTarget, text: &str, x: f32, y: f32, s: f32) -> (f32, f32) {
        let layout = gfx::with(|g| g.layout(text, 12.0 * s, false));
        let (tw, th) = gfx::text_size(&layout);
        let (px, py) = (7.0 * s, 3.0 * s);
        let bg = brush(rt, BAR_BG, 0.92);
        let r = rf(x, y, x + tw + px * 2.0, y + th + py * 2.0);
        rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0 * s, radiusY: 4.0 * s }, &bg);
        rt.DrawTextLayout(v2(x + px, y + py), &layout, &brush(rt, 0xffffff, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE);
        (r.right - r.left, r.bottom - r.top)
    }

    unsafe fn size_label(&self, rt: &ID2D1RenderTarget, r: Rc, s: f32) {
        let text = format!("{} × {}", r.w(), r.h());
        let lh = 22.0 * s;
        // Above the selection when it fits on that monitor, otherwise just inside its corner.
        let above = r.t as f32 - lh - 6.0 * s >= self.monitor_at(r.l, r.t).0.t as f32;
        let (x, y) = if above { (r.l as f32, r.t as f32 - lh - 6.0 * s) } else { (r.l as f32 + 6.0 * s, r.t as f32 + 6.0 * s) };
        self.label(rt, &text, x, y, s);
    }

    unsafe fn paint_bars(&self, rt: &ID2D1RenderTarget, s: f32) {
        let Some(bars) = self.bars() else { return };
        let bg = brush(rt, BAR_BG, 0.96);
        let border = brush(rt, 0x5f6368, 0.6);
        let hover = brush(rt, BAR_HOVER, 1.0);
        let accent = brush(rt, ACCENT, 1.0);
        let icon = brush(rt, ICON, 1.0);
        for r in &bars.rects {
            let rr = D2D1_ROUNDED_RECT { rect: *r, radiusX: 7.0 * s, radiusY: 7.0 * s };
            rt.FillRoundedRectangle(&rr, &bg);
            rt.DrawRoundedRectangle(&rr, &border, 1.0, None);
        }
        for r in &bars.seps {
            rt.DrawLine(v2(r.left, r.top), v2(r.left, r.bottom), &border, 1.0, None);
        }
        let pressed = match self.drag {
            Drag::Button(b) => Some(b),
            _ => None,
        };
        for &(b, r) in &bars.items {
            let active = match b {
                Btn::Tool(t) => t == self.tool,
                Btn::Size(i) => (SIZES[i].0 - self.size).abs() < 0.01,
                Btn::Fill => self.fill,
                _ => false,
            };
            let inner = rf(r.left + 1.0 * s, r.top + 1.0 * s, r.right - 1.0 * s, r.bottom - 1.0 * s);
            let rr = D2D1_ROUNDED_RECT { rect: inner, radiusX: 5.0 * s, radiusY: 5.0 * s };
            if active {
                rt.FillRoundedRectangle(&rr, &accent);
            } else if self.hot == Some(b) || pressed == Some(b) {
                rt.FillRoundedRectangle(&rr, &hover);
            }
            let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            let k = 0.9 * s;
            rt.SetTransform(&affine(k, cx - 10.0 * k, cy - 10.0 * k));
            self.icon(rt, b, &icon);
            rt.SetTransform(&Matrix3x2::identity());
        }

        if let (Some(b), None) = (self.hot, pressed) {
            if let Some((_, r)) = bars.items.iter().find(|(x, _)| *x == b) {
                let text = b.label();
                let lh = 22.0 * s;
                let top = bars.rects.iter().map(|r| r.top).fold(f32::MAX, f32::min);
                let bottom = bars.rects.iter().map(|r| r.bottom).fold(f32::MIN, f32::max);
                let y = if bars.below { bottom + 6.0 * s } else { top - 6.0 * s - lh };
                let (mon, _) = self.monitor_at(r.left as i32, r.top as i32);
                let y = if y + lh > mon.b as f32 { top - 6.0 * s - lh } else { y };
                let x = r.left.min(mon.r as f32 - 220.0 * s);
                self.label(rt, &text, x, y, s);
            }
        }
    }

    /// Toolbar glyphs, drawn in a 20×20 box.
    unsafe fn icon(&self, rt: &ID2D1RenderTarget, b: Btn, fg: &ID2D1SolidColorBrush) {
        gfx::with(|g| {
            let sw = 1.6;
            let st = &g.round;
            let line = |x0: f32, y0: f32, x1: f32, y1: f32| rt.DrawLine(v2(x0, y0), v2(x1, y1), fg, sw, st);
            let poly = |pts: &[(f32, f32)], close: bool, fill: bool| {
                if let Some(p) = g.path(|s| {
                    s.BeginFigure(v2(pts[0].0, pts[0].1), if fill { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
                    for &(x, y) in &pts[1..] {
                        s.AddLine(v2(x, y));
                    }
                    s.EndFigure(if close { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
                }) {
                    if fill {
                        rt.FillGeometry(&p, fg, None);
                    }
                    rt.DrawGeometry(&p, fg, sw, st);
                }
            };
            match b {
                Btn::Tool(Tool::Select) => poly(&[(6.0, 3.0), (6.0, 16.0), (9.3, 12.8), (11.6, 17.6), (13.6, 16.7), (11.4, 12.0), (15.8, 12.0)], true, false),
                Btn::Tool(Tool::Rect) => rt.DrawRectangle(&rf(3.0, 5.0, 17.0, 15.0), fg, sw, st),
                Btn::Tool(Tool::Ellipse) => rt.DrawEllipse(&D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 7.5, radiusY: 5.8 }, fg, sw, st),
                Btn::Tool(Tool::Arrow) => {
                    line(4.0, 16.0, 15.0, 5.0);
                    poly(&[(9.0, 5.0), (15.0, 5.0), (15.0, 11.0)], false, false);
                }
                Btn::Tool(Tool::Line) => line(4.0, 16.0, 16.0, 4.0),
                Btn::Tool(Tool::Pen) => {
                    if let Some(p) = g.path(|s| {
                        s.BeginFigure(v2(3.0, 14.0), D2D1_FIGURE_BEGIN_HOLLOW);
                        s.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v2(6.0, 3.0), point2: v2(9.0, 19.0), point3: v2(12.0, 9.0) });
                        s.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v2(13.5, 5.0), point2: v2(16.0, 6.0), point3: v2(17.0, 8.0) });
                        s.EndFigure(D2D1_FIGURE_END_OPEN);
                    }) {
                        rt.DrawGeometry(&p, fg, sw, st);
                    }
                }
                Btn::Tool(Tool::Marker) => {
                    let soft = brush(rt, 0xfdd835, 0.75);
                    rt.DrawLine(v2(4.0, 13.0), v2(16.0, 7.0), &soft, 5.0, st);
                    line(4.0, 17.0, 16.0, 17.0);
                }
                Btn::Tool(Tool::Text) => {
                    line(5.0, 4.5, 15.0, 4.5);
                    line(10.0, 4.5, 10.0, 16.0);
                }
                Btn::Tool(Tool::Step) => {
                    rt.DrawEllipse(&D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 7.5, radiusY: 7.5 }, fg, sw, st);
                    poly(&[(8.3, 7.6), (10.4, 6.2), (10.4, 14.0)], false, false);
                }
                Btn::Tool(Tool::Pixelate) => {
                    for (i, j) in [(0, 0), (2, 0), (1, 1), (0, 2), (2, 2)] {
                        let (x, y) = (3.5 + i as f32 * 4.5, 3.5 + j as f32 * 4.5);
                        rt.FillRectangle(&rf(x, y, x + 4.5, y + 4.5), fg);
                    }
                    rt.DrawRectangle(&rf(3.5, 3.5, 17.0, 17.0), fg, 1.0, st);
                }
                Btn::Undo | Btn::Redo => {
                    let m = |x: f32| if b == Btn::Redo { 20.0 - x } else { x };
                    if let Some(p) = g.path(|s| {
                        s.BeginFigure(v2(m(5.0), 8.0), D2D1_FIGURE_BEGIN_HOLLOW);
                        s.AddLine(v2(m(12.0), 8.0));
                        s.AddArc(&D2D1_ARC_SEGMENT {
                            point: v2(m(12.0), 16.0),
                            size: D2D_SIZE_F { width: 4.0, height: 4.0 },
                            rotationAngle: 0.0,
                            sweepDirection: if b == Btn::Redo { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_CLOCKWISE },
                            arcSize: D2D1_ARC_SIZE_SMALL,
                        });
                        s.AddLine(v2(m(8.0), 16.0));
                        s.EndFigure(D2D1_FIGURE_END_OPEN);
                    }) {
                        rt.DrawGeometry(&p, fg, sw, st);
                    }
                    poly(&[(m(8.5), 4.5), (m(5.0), 8.0), (m(8.5), 11.5)], false, false);
                }
                Btn::Pin => {
                    poly(&[(7.0, 3.0), (13.0, 3.0), (12.0, 8.5), (15.0, 11.5), (5.0, 11.5), (8.0, 8.5)], true, false);
                    line(10.0, 11.5, 10.0, 17.5);
                }
                Btn::Save => {
                    line(10.0, 3.0, 10.0, 12.5);
                    poly(&[(6.0, 8.5), (10.0, 12.5), (14.0, 8.5)], false, false);
                    poly(&[(4.0, 12.5), (4.0, 16.5), (16.0, 16.5), (16.0, 12.5)], false, false);
                }
                Btn::Copy => {
                    let rr = D2D1_ROUNDED_RECT { rect: rf(7.5, 7.0, 16.0, 17.0), radiusX: 1.5, radiusY: 1.5 };
                    rt.DrawRoundedRectangle(&rr, fg, sw, st);
                    poly(&[(4.5, 13.5), (4.5, 4.5), (12.5, 4.5)], false, false);
                }
                Btn::Close => {
                    line(5.0, 5.0, 15.0, 15.0);
                    line(15.0, 5.0, 5.0, 15.0);
                }
                Btn::Size(i) => {
                    let r = [2.0, 3.5, 5.5][i];
                    rt.FillEllipse(&D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: r, radiusY: r }, fg);
                }
                Btn::Fill => {
                    rt.FillRectangle(&rf(4.0, 5.0, 16.0, 15.0), fg);
                }
                Btn::Color(i) => {
                    let c = PALETTE[i].0;
                    let e = D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 6.5, radiusY: 6.5 };
                    rt.FillEllipse(&e, &brush(rt, c, 1.0));
                    if c == 0x111111 {
                        rt.DrawEllipse(&e, &brush(rt, 0x80868b, 1.0), 1.0, None);
                    }
                    if c == self.color {
                        let ring = D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 9.0, radiusY: 9.0 };
                        rt.DrawEllipse(&ring, fg, 1.6, None);
                    }
                }
                Btn::Custom => {
                    let custom = !PALETTE.iter().any(|p| p.0 == self.color);
                    let e = D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 6.5, radiusY: 6.5 };
                    if custom {
                        rt.FillEllipse(&e, &brush(rt, self.color, 1.0));
                        let ring = D2D1_ELLIPSE { point: v2(10.0, 10.0), radiusX: 9.0, radiusY: 9.0 };
                        rt.DrawEllipse(&ring, fg, 1.6, None);
                    } else {
                        rt.DrawEllipse(&e, fg, 1.2, None);
                        let plus = brush(rt, ICON, 1.0);
                        rt.DrawLine(v2(10.0, 6.5), v2(10.0, 13.5), &plus, 1.4, None);
                        rt.DrawLine(v2(6.5, 10.0), v2(13.5, 10.0), &plus, 1.4, None);
                    }
                }
            }
        });
    }

    unsafe fn magnifier(&self, rt: &ID2D1RenderTarget, s: f32) {
        let (mx, my) = (self.mx, self.my);
        let cells = 15;
        let cell = (8.0 * s).round();
        let size = cells as f32 * cell;
        let (mon, _) = self.monitor_at(mx, my);
        let label_h = 24.0 * s;
        let mut x = mx as f32 + 22.0 * s;
        let mut y = my as f32 + 22.0 * s;
        if x + size > mon.r as f32 {
            x = mx as f32 - 22.0 * s - size;
        }
        if y + size + label_h > mon.b as f32 {
            y = my as f32 - 22.0 * s - size - label_h;
        }
        let dest = rf(x, y, x + size, y + size);
        rt.FillRectangle(&dest, &brush(rt, 0x000000, 1.0));

        let half = cells / 2;
        let src = Rc { l: mx - half, t: my - half, r: mx - half + cells, b: my - half + cells };
        if let Some(vis) = src.clip(&self.screen()) {
            let d = rf(
                x + (vis.l - src.l) as f32 * cell,
                y + (vis.t - src.t) as f32 * cell,
                x + (vis.r - src.l) as f32 * cell,
                y + (vis.b - src.t) as f32 * cell,
            );
            // Just the few pixels around the cursor, so it doesn't depend on how the screenshot is tiled.
            if let Some(bmp) = gfx::bitmap_of(rt, &self.shot.img, vis.l as u32, vis.t as u32, vis.w() as u32, vis.h() as u32) {
                rt.DrawBitmap(&bmp, Some(&d), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            }
        }
        let cross = brush(rt, ACCENT, 0.35);
        let c0 = half as f32 * cell;
        rt.FillRectangle(&rf(x, y + c0, x + size, y + c0 + cell), &cross);
        rt.FillRectangle(&rf(x + c0, y, x + c0 + cell, y + size), &cross);
        rt.DrawRectangle(&rf(x + c0, y + c0, x + c0 + cell, y + c0 + cell), &brush(rt, 0xffffff, 1.0), 1.0, None);
        rt.DrawRectangle(&dest, &brush(rt, 0xffffff, 0.9), 1.0, None);

        let rgb = self.shot.img.pixel(mx, my);
        let text = format!("{}, {}   #{:06X}", mx + self.shot.x, my + self.shot.y, rgb);
        let ly = y + size + 4.0 * s;
        let (lw, lh) = self.label(rt, &text, x, ly, s);
        let sw = lh - 6.0 * s;
        let sx = x + lw + 4.0 * s;
        let sr = rf(sx, ly + 3.0 * s, sx + sw, ly + 3.0 * s + sw);
        rt.FillRectangle(&sr, &brush(rt, rgb, 1.0));
        rt.DrawRectangle(&sr, &brush(rt, contrast(rgb), 0.6), 1.0, None);
    }
}

/// 0xRRGGBB as a GDI COLORREF.
fn gfx_bgr(rgb: u32) -> u32 {
    (rgb & 0xff) << 16 | (rgb & 0xff00) | (rgb >> 16) & 0xff
}

fn snap_angle(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let step = std::f32::consts::FRAC_PI_4;
    let ang = (dy.atan2(dx) / step).round() * step;
    [a[0] + len * ang.cos(), a[1] + len * ang.sin()]
}

fn square(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let m = dx.abs().max(dy.abs());
    [a[0] + m * dx.signum(), a[1] + m * dy.signum()]
}

fn nudge_cursor(vk: VIRTUAL_KEY, n: i32) {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        match vk {
            VK_LEFT => p.x -= n,
            VK_RIGHT => p.x += n,
            VK_UP => p.y -= n,
            _ => p.y += n,
        }
        let _ = SetCursorPos(p.x, p.y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the editor off-screen to target/*.png so the UI can be looked at without
    /// opening a window or touching the mouse.
    #[test]
    fn render_scenes() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        // A made-up desktop: a gradient with a few light "windows", so no real screen is involved.
        let (w, h) = (2400u32, 1500u32);
        let mut px = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let win = (x / 300 + y / 250) % 3 == 0;
                px[i] = if win { 240 } else { (x * 255 / w) as u8 };
                px[i + 1] = if win { 236 } else { (y * 255 / h) as u8 };
                px[i + 2] = if win { 230 } else { 90 };
            }
        }
        let shot = Shot { x: 0, y: 0, img: Image { w, h, px } };
        let mons = vec![(Rc { l: 0, t: 0, r: w as i32, b: h as i32 }, 1.5)];
        let mut o = Overlay {
            hwnd: HWND::default(),
            rt: None,
            gdi: None,
            tiles: vec![],
            shot,
            wins: vec![],
            mons,
            region: Some(Rc { l: 400, t: 250, r: 1900, b: 1150 }),
            hover: None,
            mx: 1000,
            my: 700,
            drag: Drag::None,
            tool: Tool::Rect,
            color: 0xe53935,
            size: 6.0,
            fill: false,
            annots: vec![],
            undo: vec![],
            redo: vec![],
            current: None,
            typing: false,
            typing_saved: false,
            selected: None,
            hot: Some(Btn::Tool(Tool::Arrow)),
            magnifier: true,
        };
        let mk = |t: Tool, c: u32, pts: Vec<[f32; 2]>, text: &str| Annot { tool: t, color: c, size: 6.0, fill: false, scale: 1.5, pts, text: text.into(), pixels: Default::default() };
        o.annots = vec![
            mk(Tool::Rect, 0xe53935, vec![[450.0, 300.0], [800.0, 500.0]], ""),
            mk(Tool::Ellipse, 0x43a047, vec![[850.0, 300.0], [1150.0, 500.0]], ""),
            mk(Tool::Arrow, 0x1e88e5, vec![[1200.0, 520.0], [1500.0, 320.0]], ""),
            mk(Tool::Line, 0x8e24aa, vec![[1550.0, 320.0], [1850.0, 500.0]], ""),
            mk(Tool::Pen, 0xfb8c00, (0..60).map(|i| [450.0 + i as f32 * 6.0, 650.0 + (i as f32 / 5.0).sin() * 40.0]).collect(), ""),
            mk(Tool::Marker, 0xfdd835, vec![[850.0, 650.0], [1150.0, 650.0]], ""),
            mk(Tool::Step, 0xe53935, vec![[1250.0, 650.0]], "1"),
            mk(Tool::Step, 0xfdd835, vec![[1330.0, 650.0]], "2"),
            mk(Tool::Pixelate, 0, vec![[1450.0, 580.0], [1850.0, 760.0]], ""),
            mk(Tool::Text, 0xffffff, vec![[450.0, 800.0]], "Hello there\nsecond line"),
        ];
        o.selected = Some(2);
        o.current = Some(mk(Tool::Text, 0xe53935, vec![[1000.0, 850.0]], "typing"));
        o.typing = true;

        let render = |o: &mut Overlay, name: &str| {
            let img = gfx::render_image(w, h, |rt| unsafe {
                o.tiles = gfx::tiles_of(rt, &o.shot.img, 0, 0, w, h);
                gfx::draw_tiles(rt, &o.tiles);
                o.paint_scene(rt);
            })
            .unwrap();
            gfx::save_image(&img, std::path::Path::new(&format!("target/{name}.png"))).unwrap();
        };
        render(&mut o, "scene-edit");

        o.typing = false;
        o.current = None;
        let exported = o.export().unwrap();
        gfx::save_image(&exported, std::path::Path::new("target/scene-export.png")).unwrap();

        o.region = None;
        o.hover = Some(Rc { l: 300, t: 200, r: 1700, b: 1000 });
        o.hot = None;
        render(&mut o, "scene-pick");
    }

    /// Cropping gives the same pixels as drawing through Direct2D, and copies resize.
    #[test]
    fn crop_and_scale() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let (w, h) = (300u32, 200u32);
        let px = (0..w * h * 4).map(|i| if i % 4 == 3 { 255 } else { (i * 7 % 251) as u8 }).collect();
        let img = Image { w, h, px };
        let crop = img.crop(40, 30, 120, 90);
        let drawn = gfx::render_image(120, 90, |rt| gfx::draw_tiles(rt, &gfx::tiles_of(rt, &img, 40, 30, 120, 90))).unwrap();
        assert_eq!(crop.px, drawn.px);
        let big = gfx::scale_image(&crop, 150).unwrap();
        assert_eq!((big.w, big.h, big.px.len()), (180, 135, 180 * 135 * 4));
    }

    /// An image wider than any GPU texture still comes out whole, via tiles.
    #[test]
    fn tiles_cover_wide_images() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let (w, h) = (20_000u32, 40u32);
        let mut px = vec![255u8; (w * h * 4) as usize];
        for x in 0..w {
            for y in 0..h {
                px[((y * w + x) * 4) as usize] = (x % 251) as u8;
            }
        }
        let img = Image { w, h, px };
        let out = gfx::render_image(w, h, |rt| gfx::draw_tiles(rt, &gfx::tiles_of(rt, &img, 0, 0, w, h))).unwrap();
        for x in [0, 16_383, 16_384, 19_999] {
            assert_eq!(out.pixel(x, 20), img.pixel(x, 20), "pixel at x = {x}");
        }
    }
}


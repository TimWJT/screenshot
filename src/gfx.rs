//! Direct2D, DirectWrite and WIC plumbing: shared factories, image export, clipboard.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use windows::core::{Result, GUID};
use windows::Win32::Foundation::{GlobalFree, GENERIC_WRITE, HANDLE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};
use windows_numerics::{Matrix3x2, Vector2};

use crate::capture::Image;
use crate::util::wide;

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub wic: IWICImagingFactory,
    pub round: ID2D1StrokeStyle,
    pub dash: ID2D1StrokeStyle,
    formats: RefCell<HashMap<(u32, bool), IDWriteTextFormat>>,
}

thread_local! {
    static GFX: Gfx = Gfx::new().expect("Direct2D is unavailable");
}

impl Gfx {
    fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_FLAT,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            };
            let round = d2d.CreateStrokeStyle(&props, None)?;
            props.startCap = D2D1_CAP_STYLE_FLAT;
            props.endCap = D2D1_CAP_STYLE_FLAT;
            props.dashStyle = D2D1_DASH_STYLE_CUSTOM;
            let dash = d2d.CreateStrokeStyle(&props, Some(&[4.0, 3.0]))?;
            Ok(Gfx { d2d, dw, wic, round, dash, formats: RefCell::new(HashMap::new()) })
        }
    }

    pub fn format(&self, size: f32, bold: bool) -> IDWriteTextFormat {
        let key = ((size * 10.0) as u32, bold);
        if let Some(f) = self.formats.borrow().get(&key) {
            return f.clone();
        }
        let family = wide("Segoe UI");
        let locale = wide("en-us");
        let f = unsafe {
            self.dw
                .CreateTextFormat(
                    crate::util::pcwstr(&family),
                    None,
                    if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    crate::util::pcwstr(&locale),
                )
                .expect("CreateTextFormat")
        };
        self.formats.borrow_mut().insert(key, f.clone());
        f
    }

    pub fn layout(&self, text: &str, size: f32, bold: bool) -> IDWriteTextLayout {
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dw.CreateTextLayout(&t, &self.format(size, bold), 100_000.0, 100_000.0).expect("CreateTextLayout") }
    }

    pub fn path(&self, build: impl FnOnce(&ID2D1GeometrySink)) -> Option<ID2D1PathGeometry> {
        unsafe {
            let g = self.d2d.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            build(&sink);
            sink.Close().ok()?;
            Some(g)
        }
    }
}

pub fn with<R>(f: impl FnOnce(&Gfx) -> R) -> R {
    GFX.with(f)
}

pub fn text_size(layout: &IDWriteTextLayout) -> (f32, f32) {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut m);
    }
    (m.widthIncludingTrailingWhitespace, m.height)
}

pub fn col(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a,
    }
}

pub fn rf(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

pub fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

pub fn affine(scale: f32, dx: f32, dy: f32) -> Matrix3x2 {
    Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: dx, M32: dy }
}

pub fn brush(rt: &ID2D1RenderTarget, rgb: u32, a: f32) -> ID2D1SolidColorBrush {
    unsafe { rt.CreateSolidColorBrush(&col(rgb, a), None).expect("brush") }
}

fn bitmap_props() -> D2D1_BITMAP_PROPERTIES {
    D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    }
}

/// A Direct2D bitmap of the sub-rectangle (x, y, w, h) of `img`.
pub fn bitmap_of(rt: &ID2D1RenderTarget, img: &Image, x: u32, y: u32, w: u32, h: u32) -> Option<ID2D1Bitmap> {
    let stride = img.w * 4;
    let start = (y * stride + x * 4) as usize;
    unsafe {
        rt.CreateBitmap(
            D2D_SIZE_U { width: w, height: h },
            Some(img.px[start..].as_ptr() as *const _),
            stride,
            &bitmap_props(),
        )
        .ok()
    }
}

pub type Tiles = Vec<(D2D_RECT_F, ID2D1Bitmap)>;

/// The sub-rectangle (x, y, w, h) of `img` as bitmaps no bigger than `rt` allows, each with
/// where it goes relative to (x, y). GPUs stop at 16384 px, which a row of monitors can pass.
pub fn tiles_of(rt: &ID2D1RenderTarget, img: &Image, x: u32, y: u32, w: u32, h: u32) -> Tiles {
    let max = unsafe { rt.GetMaximumBitmapSize() }.max(256);
    let mut out = Vec::new();
    for ty in (0..h).step_by(max as usize) {
        for tx in (0..w).step_by(max as usize) {
            let (tw, th) = (max.min(w - tx), max.min(h - ty));
            if let Some(b) = bitmap_of(rt, img, x + tx, y + ty, tw, th) {
                out.push((rf(tx as f32, ty as f32, (tx + tw) as f32, (ty + th) as f32), b));
            }
        }
    }
    out
}

pub fn draw_tiles(rt: &ID2D1RenderTarget, tiles: &Tiles) {
    for (dest, b) in tiles {
        unsafe {
            rt.DrawBitmap(b, Some(dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        }
    }
}

/// Render something off-screen into a new `w`×`h` image.
pub fn render_image(w: u32, h: u32, draw: impl FnOnce(&ID2D1RenderTarget)) -> Option<Image> {
    with(|g| unsafe {
        let bmp = g.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = g.d2d.CreateWicBitmapRenderTarget(&bmp, &props).ok()?;
        rt.BeginDraw();
        rt.Clear(Some(&col(0xffffff, 1.0)));
        draw(&rt);
        rt.EndDraw(None, None).ok()?;
        drop(rt);
        let mut px = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
        for a in px.iter_mut().skip(3).step_by(4) {
            *a = 255;
        }
        Some(Image { w, h, px })
    })
}

/// Write `img` as PNG, or JPEG when the path ends in .jpg/.jpeg.
pub fn save_image(img: &Image, path: &Path) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let (container, mut format): (GUID, GUID) = if ext == "jpg" || ext == "jpeg" {
        (GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR)
    } else {
        (GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR)
    };
    let name = wide(&path.to_string_lossy());
    with(|g| unsafe {
        let src = g.wic.CreateBitmapFromMemory(img.w, img.h, &GUID_WICPixelFormat32bppBGR, img.w * 4, &img.px)?;
        let stream = g.wic.CreateStream()?;
        stream.InitializeFromFilename(crate::util::pcwstr(&name), GENERIC_WRITE.0)?;
        let enc = g.wic.CreateEncoder(&container, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut bag = None;
        enc.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.unwrap();
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(img.w, img.h)?;
        frame.SetPixelFormat(&mut format)?;
        frame.WriteSource(&src, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
        Ok(())
    })
}

pub fn copy_image(owner: HWND, img: &Image) -> bool {
    let header = size_of::<BITMAPINFOHEADER>();
    let row = img.w as usize * 4;
    let size = header + row * img.h as usize;
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, size) else { return false };
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        let bih = BITMAPINFOHEADER {
            biSize: header as u32,
            biWidth: img.w as i32,
            biHeight: img.h as i32, // bottom-up: the layout every app understands
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (row * img.h as usize) as u32,
            ..Default::default()
        };
        std::ptr::copy_nonoverlapping(&bih as *const _ as *const u8, p, header);
        for y in 0..img.h as usize {
            let src = &img.px[y * row..(y + 1) * row];
            let dst = p.add(header + (img.h as usize - 1 - y) * row);
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst, row);
        }
        let _ = GlobalUnlock(mem);
        set_clipboard(owner, CF_DIB.0 as u32, mem)
    }
}

pub fn copy_text(owner: HWND, text: &str) -> bool {
    let w = wide(text);
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) else { return false };
        let p = GlobalLock(mem) as *mut u16;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        let _ = GlobalUnlock(mem);
        set_clipboard(owner, CF_UNICODETEXT.0 as u32, mem)
    }
}

unsafe fn set_clipboard(owner: HWND, format: u32, mem: windows::Win32::Foundation::HGLOBAL) -> bool {
    // Another app can hold the clipboard for a moment; retry briefly.
    let mut opened = false;
    for _ in 0..10 {
        if OpenClipboard(Some(owner)).is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        let _ = GlobalFree(Some(mem));
        return false;
    }
    let _ = EmptyClipboard();
    let ok = SetClipboardData(format, Some(HANDLE(mem.0))).is_ok();
    if !ok {
        let _ = GlobalFree(Some(mem));
    }
    let _ = CloseClipboard();
    ok
}

pub fn paste_text(owner: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let out = (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let mem = windows::Win32::Foundation::HGLOBAL(h.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            // Stop at the end of the block even if the app that put it there left off the NUL.
            let max = GlobalSize(mem) / 2;
            let mut n = 0;
            while n < max && *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(mem);
            Some(s)
        })();
        let _ = CloseClipboard();
        out
    }
}

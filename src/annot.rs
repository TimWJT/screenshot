//! Things drawn on top of a capture, and how each one renders and hit-tests.

use std::cell::RefCell;

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

use crate::capture::Image;
use crate::gfx::{self, brush, rf, v2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Marker,
    Text,
    Step,
    Pixelate,
}

impl Tool {
    pub const ALL: [Tool; 10] = [
        Tool::Select,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Arrow,
        Tool::Line,
        Tool::Pen,
        Tool::Marker,
        Tool::Text,
        Tool::Step,
        Tool::Pixelate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select and move (V)",
            Tool::Rect => "Rectangle (R)",
            Tool::Ellipse => "Ellipse (E)",
            Tool::Arrow => "Arrow (A)",
            Tool::Line => "Line (L)",
            Tool::Pen => "Pen (P)",
            Tool::Marker => "Highlighter (H)",
            Tool::Text => "Text (T)",
            Tool::Step => "Numbered step (N)",
            Tool::Pixelate => "Pixelate (B)",
        }
    }

    pub fn from_key(vk: u16) -> Option<Tool> {
        Some(match vk as u8 {
            b'V' => Tool::Select,
            b'R' => Tool::Rect,
            b'E' => Tool::Ellipse,
            b'A' => Tool::Arrow,
            b'L' => Tool::Line,
            b'P' => Tool::Pen,
            b'H' => Tool::Marker,
            b'T' => Tool::Text,
            b'N' => Tool::Step,
            b'B' => Tool::Pixelate,
            _ => return None,
        })
    }

    pub fn fillable(self) -> bool {
        matches!(self, Tool::Rect | Tool::Ellipse)
    }

    /// Tools that ignore colour.
    pub fn colorless(self) -> bool {
        matches!(self, Tool::Select | Tool::Pixelate)
    }
}

#[derive(Clone)]
pub struct Annot {
    pub tool: Tool,
    pub color: u32,
    pub size: f32,
    pub fill: bool,
    /// Display scale where it was drawn (1.0 = 96 DPI). Sizes are multiplied by it.
    pub scale: f32,
    /// Freehand: every point. Shapes: two corners / two ends. Text and steps: the anchor.
    pub pts: Vec<[f32; 2]>,
    pub text: String,
    /// Pixelate: the averaged blocks from the last draw, keyed by area and block size, so
    /// redrawing on every mouse move doesn't average the whole area again.
    pub pixels: RefCell<Option<([i32; 5], Image)>>,
}

pub fn text_px(size: f32) -> f32 {
    12.0 + size * 2.5
}

fn step_radius(size: f32) -> f32 {
    9.0 + size * 1.4
}

/// Big enough at every size that text underneath can't be read back out of the averages.
fn pixel_block(size: f32) -> f32 {
    (10.0 + size * 2.0).round()
}

fn marker_width(size: f32) -> f32 {
    size * 2.5 + 8.0
}

fn norm(a: [f32; 2], b: [f32; 2]) -> (f32, f32, f32, f32) {
    (a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1]))
}

fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
    ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
}

/// Readable text colour on top of `rgb`.
pub fn contrast(rgb: u32) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255);
    if r * 299 + g * 587 + b * 114 > 160_000 {
        0x111111
    } else {
        0xffffff
    }
}

impl Annot {
    pub fn new(tool: Tool, color: u32, size: f32, fill: bool, scale: f32, at: [f32; 2]) -> Self {
        let pts = match tool {
            Tool::Pen | Tool::Marker | Tool::Text | Tool::Step => vec![at],
            _ => vec![at, at],
        };
        Annot { tool, color, size, fill, scale, pts, text: String::new(), pixels: RefCell::new(None) }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        for p in &mut self.pts {
            p[0] += dx;
            p[1] += dy;
        }
    }

    /// Worth keeping once the mouse is released?
    pub fn meaningful(&self) -> bool {
        match self.tool {
            Tool::Pen | Tool::Marker | Tool::Step => true,
            Tool::Text => !self.text.trim().is_empty(),
            _ => {
                let (a, b) = (self.pts[0], self.pts[1]);
                (a[0] - b[0]).abs() > 2.0 || (a[1] - b[1]).abs() > 2.0
            }
        }
    }

    fn stroke(&self) -> f32 {
        match self.tool {
            Tool::Marker => marker_width(self.size) * self.scale,
            _ => self.size * self.scale,
        }
    }

    fn text_layout(&self) -> IDWriteTextLayout {
        gfx::with(|g| g.layout(&self.text, text_px(self.size) * self.scale, true))
    }

    /// Where the caret sits after the last character, and its height.
    pub fn caret(&self) -> ([f32; 2], f32) {
        let layout = self.text_layout();
        let n = self.text.encode_utf16().count() as u32;
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = layout.HitTestTextPosition(n, false, &mut x, &mut y, &mut m);
        }
        let h = if m.height > 0.0 { m.height } else { text_px(self.size) * self.scale * 1.33 };
        ([self.pts[0][0] + x, self.pts[0][1] + y], h)
    }

    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let pad = self.stroke() / 2.0;
        match self.tool {
            Tool::Text => {
                let (w, h) = gfx::text_size(&self.text_layout());
                let [x, y] = self.pts[0];
                (x, y, x + w.max(4.0), y + h)
            }
            Tool::Step => {
                let r = step_radius(self.size) * self.scale;
                let [x, y] = self.pts[0];
                (x - r, y - r, x + r, y + r)
            }
            Tool::Arrow => {
                let (l, t, r, b) = self.pts_bounds();
                let head = (self.size * 3.0 + 8.0) * self.scale;
                (l - head, t - head, r + head, b + head)
            }
            _ => {
                let (l, t, r, b) = self.pts_bounds();
                (l - pad, t - pad, r + pad, b + pad)
            }
        }
    }

    fn pts_bounds(&self) -> (f32, f32, f32, f32) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in &self.pts {
            b = (b.0.min(p[0]), b.1.min(p[1]), b.2.max(p[0]), b.3.max(p[1]));
        }
        b
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        let p = [x, y];
        let tol = self.stroke() / 2.0 + 5.0;
        match self.tool {
            Tool::Pen | Tool::Marker => {
                if self.pts.len() == 1 {
                    return seg_dist(p, self.pts[0], self.pts[0]) <= tol;
                }
                self.pts.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= tol)
            }
            Tool::Line | Tool::Arrow => seg_dist(p, self.pts[0], self.pts[1]) <= tol,
            Tool::Rect => {
                let (l, t, r, b) = norm(self.pts[0], self.pts[1]);
                let outer = x >= l - tol && x <= r + tol && y >= t - tol && y <= b + tol;
                let inner = x > l + tol && x < r - tol && y > t + tol && y < b - tol;
                outer && (self.fill || !inner)
            }
            Tool::Ellipse => {
                let (l, t, r, b) = norm(self.pts[0], self.pts[1]);
                let (rx, ry) = (((r - l) / 2.0).max(1.0), ((b - t) / 2.0).max(1.0));
                let (nx, ny) = ((x - (l + r) / 2.0) / rx, (y - (t + b) / 2.0) / ry);
                let d = (nx * nx + ny * ny).sqrt();
                let slack = tol / rx.min(ry);
                if self.fill {
                    d <= 1.0 + slack
                } else {
                    (d - 1.0).abs() <= slack
                }
            }
            Tool::Pixelate => {
                let (l, t, r, b) = norm(self.pts[0], self.pts[1]);
                x >= l && x <= r && y >= t && y <= b
            }
            Tool::Text | Tool::Step => {
                let (l, t, r, b) = self.bounds();
                x >= l - 4.0 && x <= r + 4.0 && y >= t - 4.0 && y <= b + 4.0
            }
            Tool::Select => false,
        }
    }

    /// Draw onto any render target. `src` is the untouched capture, used by pixelate.
    pub fn draw(&self, rt: &ID2D1RenderTarget, src: &Image) {
        gfx::with(|g| unsafe {
            let solid = brush(rt, self.color, 1.0);
            match self.tool {
                Tool::Pen | Tool::Marker => {
                    let (b, w) = if self.tool == Tool::Marker {
                        (brush(rt, self.color, 0.42), marker_width(self.size) * self.scale)
                    } else {
                        (solid.clone(), self.size * self.scale)
                    };
                    if self.pts.len() == 1 {
                        let [x, y] = self.pts[0];
                        rt.FillEllipse(&D2D1_ELLIPSE { point: v2(x, y), radiusX: w / 2.0, radiusY: w / 2.0 }, &b);
                    } else if let Some(path) = g.path(|s| {
                        s.BeginFigure(v2(self.pts[0][0], self.pts[0][1]), D2D1_FIGURE_BEGIN_HOLLOW);
                        for p in &self.pts[1..] {
                            s.AddLine(v2(p[0], p[1]));
                        }
                        s.EndFigure(D2D1_FIGURE_END_OPEN);
                    }) {
                        rt.DrawGeometry(&path, &b, w, &g.round);
                    }
                }
                Tool::Line => {
                    let (a, b) = (self.pts[0], self.pts[1]);
                    rt.DrawLine(v2(a[0], a[1]), v2(b[0], b[1]), &solid, self.size * self.scale, &g.round);
                }
                Tool::Arrow => {
                    let (a, b) = (self.pts[0], self.pts[1]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let len = (dx * dx + dy * dy).sqrt().max(0.001);
                    let (ux, uy) = (dx / len, dy / len);
                    let head = ((self.size * 3.0 + 8.0) * self.scale).min(len);
                    let half = head * 0.55;
                    let base = [b[0] - ux * head, b[1] - uy * head];
                    // Stop the shaft inside the head so its round cap does not poke out of the tip.
                    let shaft_end = [b[0] - ux * head * 0.6, b[1] - uy * head * 0.6];
                    rt.DrawLine(v2(a[0], a[1]), v2(shaft_end[0], shaft_end[1]), &solid, self.size * self.scale, &g.round);
                    if let Some(tri) = g.path(|s| {
                        s.BeginFigure(v2(b[0], b[1]), D2D1_FIGURE_BEGIN_FILLED);
                        s.AddLine(v2(base[0] - uy * half, base[1] + ux * half));
                        s.AddLine(v2(base[0] + uy * half, base[1] - ux * half));
                        s.EndFigure(D2D1_FIGURE_END_CLOSED);
                    }) {
                        rt.FillGeometry(&tri, &solid, None);
                        rt.DrawGeometry(&tri, &solid, 1.0, &g.round);
                    }
                }
                Tool::Rect => {
                    let (l, t, r, b) = norm(self.pts[0], self.pts[1]);
                    if self.fill {
                        rt.FillRectangle(&rf(l, t, r, b), &solid);
                    } else {
                        rt.DrawRectangle(&rf(l, t, r, b), &solid, self.size * self.scale, &g.round);
                    }
                }
                Tool::Ellipse => {
                    let (l, t, r, b) = norm(self.pts[0], self.pts[1]);
                    let e = D2D1_ELLIPSE { point: v2((l + r) / 2.0, (t + b) / 2.0), radiusX: (r - l) / 2.0, radiusY: (b - t) / 2.0 };
                    if self.fill {
                        rt.FillEllipse(&e, &solid);
                    } else {
                        rt.DrawEllipse(&e, &solid, self.size * self.scale, &g.round);
                    }
                }
                Tool::Text => {
                    let layout = self.text_layout();
                    rt.DrawTextLayout(v2(self.pts[0][0], self.pts[0][1]), &layout, &solid, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                Tool::Step => {
                    let [x, y] = self.pts[0];
                    let r = step_radius(self.size) * self.scale;
                    rt.FillEllipse(&D2D1_ELLIPSE { point: v2(x, y), radiusX: r, radiusY: r }, &solid);
                    let layout = g.layout(&self.text, r * 1.15, true);
                    let _ = layout.SetMaxWidth(r * 2.0);
                    let _ = layout.SetMaxHeight(r * 2.0);
                    let _ = layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let fg = brush(rt, contrast(self.color), 1.0);
                    rt.DrawTextLayout(v2(x - r, y - r - r * 0.06), &layout, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                Tool::Pixelate => draw_pixelate(rt, src, self.pts[0], self.pts[1], pixel_block(self.size) * self.scale, &self.pixels),
                Tool::Select => {}
            }
        });
    }
}

fn draw_pixelate(rt: &ID2D1RenderTarget, src: &Image, a: [f32; 2], b: [f32; 2], block: f32, cache: &RefCell<Option<([i32; 5], Image)>>) {
    let (l, t, r, b) = norm(a, b);
    let (l, t) = (l.max(0.0).floor() as i32, t.max(0.0).floor() as i32);
    let (r, b) = ((r.ceil() as i32).min(src.w as i32), (b.ceil() as i32).min(src.h as i32));
    if r - l < 1 || b - t < 1 {
        return;
    }
    let bs = block.max(2.0) as i32;
    let cols = ((r - l) + bs - 1) / bs;
    let rows = ((b - t) + bs - 1) / bs;
    let key = [l, t, r, b, bs];
    let mut cache = cache.borrow_mut();
    if !cache.as_ref().is_some_and(|(k, _)| *k == key) {
        *cache = Some((key, average_blocks(src, l, t, r, b, bs)));
    }
    let small = &cache.as_ref().unwrap().1;
    if let Some(bmp) = gfx::bitmap_of(rt, small, 0, 0, small.w, small.h) {
        unsafe {
            rt.PushAxisAlignedClip(&rf(l as f32, t as f32, r as f32, b as f32), D2D1_ANTIALIAS_MODE_ALIASED);
            let dest = rf(l as f32, t as f32, (l + cols * bs) as f32, (t + rows * bs) as f32);
            rt.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            rt.PopAxisAlignedClip();
        }
    }
}

/// One averaged pixel per `bs`×`bs` block of the area (l, t)–(r, b) of `src`.
fn average_blocks(src: &Image, l: i32, t: i32, r: i32, b: i32, bs: i32) -> Image {
    let cols = ((r - l) + bs - 1) / bs;
    let rows = ((b - t) + bs - 1) / bs;
    let mut px = vec![255u8; (cols * rows * 4) as usize];
    for cy in 0..rows {
        for cx in 0..cols {
            let (x0, y0) = (l + cx * bs, t + cy * bs);
            let (x1, y1) = ((x0 + bs).min(r), (y0 + bs).min(b));
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for y in y0..y1 {
                let row = (y as usize * src.w as usize) * 4;
                for x in x0..x1 {
                    let i = row + x as usize * 4;
                    sum[0] += src.px[i] as u32;
                    sum[1] += src.px[i + 1] as u32;
                    sum[2] += src.px[i + 2] as u32;
                    n += 1;
                }
            }
            let o = ((cy * cols + cx) * 4) as usize;
            for k in 0..3 {
                px[o + k] = (sum[k] / n.max(1)) as u8;
            }
        }
    }
    Image { w: cols as u32, h: rows as u32, px }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(tool: Tool, fill: bool, a: [f32; 2], b: [f32; 2]) -> Annot {
        let mut s = Annot::new(tool, 0, 3.0, fill, 1.0, a);
        s.pts = vec![a, b];
        s
    }

    #[test]
    fn outlines_hit_on_the_edge_only() {
        let r = shape(Tool::Rect, false, [100.0, 100.0], [200.0, 200.0]);
        assert!(r.hit(100.0, 150.0));
        assert!(!r.hit(150.0, 150.0), "an outline is not hit in its middle");
        assert!(!r.hit(50.0, 50.0));
        let f = shape(Tool::Rect, true, [100.0, 100.0], [200.0, 200.0]);
        assert!(f.hit(150.0, 150.0), "a filled shape is");

        let e = shape(Tool::Ellipse, false, [0.0, 0.0], [200.0, 100.0]);
        assert!(e.hit(200.0, 50.0));
        assert!(!e.hit(100.0, 50.0));
    }

    #[test]
    fn lines_hit_near_the_segment() {
        let l = shape(Tool::Line, false, [0.0, 0.0], [100.0, 0.0]);
        assert!(l.hit(50.0, 4.0));
        assert!(!l.hit(50.0, 20.0));
        assert!(!l.hit(130.0, 0.0), "not beyond the end");
    }

    #[test]
    fn tiny_shapes_are_dropped() {
        assert!(!shape(Tool::Rect, false, [10.0, 10.0], [11.0, 11.0]).meaningful());
        assert!(shape(Tool::Rect, false, [10.0, 10.0], [20.0, 11.0]).meaningful());
        let mut t = Annot::new(Tool::Text, 0, 3.0, false, 1.0, [0.0, 0.0]);
        assert!(!t.meaningful());
        t.text = "  ".into();
        assert!(!t.meaningful());
        t.text = "hi".into();
        assert!(t.meaningful());
    }

    #[test]
    fn pixelate_blocks_average_and_cover_the_area() {
        let (w, h) = (40u32, 20u32);
        let mut px = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = if (x + y) % 2 == 0 { 0 } else { 200 };
            }
        }
        let src = Image { w, h, px };
        let small = average_blocks(&src, 0, 0, 40, 20, 16);
        assert_eq!((small.w, small.h), (3, 2));
        assert_eq!(small.px[0], 100, "a checkerboard averages to the middle");
        assert!(pixel_block(1.0) >= 12.0, "blocks stay coarse even at the smallest size");
    }
}

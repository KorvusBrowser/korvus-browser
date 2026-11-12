//! A 2D canvas as its read-backs hand it to the page once the loaded plugins have perturbed it (`read_back`), with the
//! two rules a perturbation must keep that a plugin cannot keep alone (the persona spec's §6, invariants 2 and 3):
//!
//! * **What no device draws differently reads back exactly.** A pixel `putImageData` wrote, one an opaque solid
//!   `fillRect` covers whole (axis-aligned, on pixel boundaries), one an opaque image pixel `drawImage` copied one to
//!   one, and the canvas's untouched background: every device stores these the same, and a probe that paints exact
//!   colours and reads them back (CreepJS's `getPixelMods`, a device SDK's `hasNoise`, a put→get round trip) catches
//!   any change to them. The plugin sees only pixels, so the engine, which has the display list, puts those pixels
//!   back after it ([`read_back_2d`]). What is left to perturb is what devices really raster differently: anti-aliased
//!   edges, glyphs, curves, gradients, blends, resampled images.
//! * **Every read of one drawing agrees.** A `getImageData` in another colour space or pixel format is read from the
//!   backing through Skia, not from the 8-bit raster the plugin perturbs; it takes the same perturbation, carried
//!   over as the same one step on the same channel of the same pixel ([`carry_noise`]), so a float or display-p3
//!   read is never the raw canvas beside a noised `toDataURL`.
//!
//! Which pixels are exact is answered conservatively ([`exact_pixels`]): any other draw takes back every pixel it could
//! have touched -- its bounds in device space, widened for anti-aliasing and strokes, or the whole canvas
//! when a shadow, a filter, a layer or a composite mode that reaches outside the shape is in force. A pixel wrongly
//! kept would be a raster pixel left unperturbed, which no page can tell from a device that rasterised it the same;
//! an exact pixel wrongly given up is the tell this file exists to remove, so every doubt gives up the pixel only
//! where a draw may really have changed it.

use spec::{Canvas2DOp, CompositeOp, PathOp, ReadFormat, Rgba8, Transform2D};

/// `raw`, the canvas's raster, as the page reads it back: perturbed by the loaded plugins, with the pixels every device
/// stores alike ([`exact_pixels`]) exactly as rastered.
pub(crate) fn read_back_2d(raw: Rgba8, ops: &[Canvas2DOp]) -> Rgba8 {
    if !plugin_host::has_readback(plugin_host::READBACK_CANVAS) {
        return raw;
    }
    let mut img = super::html_canvas_element::read_back(plugin_host::READBACK_CANVAS, raw.clone());
    restore_exact_pixels(&raw, &mut img, ops);
    img
}

fn restore_exact_pixels(raw: &Rgba8, img: &mut Rgba8, ops: &[Canvas2DOp]) {
    if img.data.len() != raw.data.len() {
        return;
    }
    if let Some(mask) = exact_pixels(ops, raw.width, raw.height) {
        for (i, keep) in mask.iter().enumerate() {
            if *keep {
                img.data[i * 4..i * 4 + 4].copy_from_slice(&raw.data[i * 4..i * 4 + 4]);
            }
        }
    }
}

/// The perturbation `noised` made of `raw` (whole canvases), carried onto `bytes`: the `w` x `h` region at `(sx, sy)`
/// of the same canvas read in another space or `format`. Each changed channel moves one step of that format the same
/// way (`1/255` for a float read), so the read rounds to what the 8-bit read reads. Only opaque pixels move, as the
/// plugin moves only those.
#[allow(clippy::too_many_arguments)]
pub(crate) fn carry_noise(raw: &Rgba8, noised: &Rgba8, sx: i64, sy: i64, w: u32, h: u32, format: ReadFormat, bytes: &mut [u8]) {
    let (iw, ih) = (raw.width as i64, raw.height as i64);
    if raw.data.len() != noised.data.len() || raw.data.len() != (iw * ih * 4) as usize {
        return;
    }
    let width = match format {
        ReadFormat::Unorm8 => 1,
        ReadFormat::Float16 => 2,
        ReadFormat::Float32 => 4,
    };
    if bytes.len() < (w as usize) * (h as usize) * 4 * width {
        return;
    }
    for row in 0..h as i64 {
        for col in 0..w as i64 {
            let (px, py) = (sx + col, sy + row);
            if px < 0 || py < 0 || px >= iw || py >= ih {
                continue;
            }
            let si = ((py * iw + px) * 4) as usize;
            if raw.data[si..si + 4] == noised.data[si..si + 4] {
                continue;
            }
            let di = ((row * w as i64 + col) * 4) as usize;
            for c in 0..3 {
                let d = noised.data[si + c] as i32 - raw.data[si + c] as i32;
                if d != 0 {
                    step(bytes, format, di + c, d.signum(), di + 3);
                }
            }
        }
    }
}

/// Move channel `at` of a `format` read one step towards `dir`, back the other way at the end of the range -- the
/// rule the plugin's 8-bit step keeps -- when channel `alpha` reads opaque.
fn step(bytes: &mut [u8], format: ReadFormat, at: usize, dir: i32, alpha: usize) {
    match format {
        ReadFormat::Unorm8 => {
            if bytes[alpha] != 255 {
                return;
            }
            let v = bytes[at];
            bytes[at] = if (dir > 0 && v < 255) || v == 0 { v + 1 } else { v - 1 };
        }
        ReadFormat::Float32 => {
            let get = |i: usize| f32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
            if get(alpha) != 1.0 {
                return;
            }
            let v = get(at);
            let out = float_step(v, dir).to_le_bytes();
            bytes[at * 4..at * 4 + 4].copy_from_slice(&out);
        }
        ReadFormat::Float16 => {
            let get = |i: usize| super::pixel_format::f16_to_f32(u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]));
            if get(alpha) != 1.0 {
                return;
            }
            let out = super::pixel_format::f32_to_f16(float_step(get(at), dir)).to_le_bytes();
            bytes[at * 2..at * 2 + 2].copy_from_slice(&out);
        }
    }
}

/// One 8-bit step of `v` towards `dir`, the other way at the end of `[0, 1]` -- the end judged with a float's slack:
/// `254/255 + 1/255` is a hair over 1 in f32, and is the top step, not past it.
fn float_step(v: f32, dir: i32) -> f32 {
    let s = 1.0 / 255.0;
    let up = (v + s).min(1.0);
    let down = (v - s).max(0.0);
    if (dir > 0 && v + s <= 1.0 + 1e-5) || v - s < -1e-5 {
        up
    } else {
        down
    }
}

/// For a `w` x `h` canvas drawn by `ops`: whether each pixel (row-major) holds a value every device stores alike -- the
/// background, a `putImageData`'s, an opaque pixel-aligned solid `fillRect`'s, an opaque image pixel copied one to one.
/// `None` when no pixel is (a zero-sized canvas).
pub(crate) fn exact_pixels(ops: &[Canvas2DOp], w: u32, h: u32) -> Option<Vec<bool>> {
    if w == 0 || h == 0 {
        return None;
    }
    let mut m = Mask { w: w as i64, h: h as i64, keep: vec![true; (w as usize) * (h as usize)], kept: (w as usize) * (h as usize) };
    let mut st = State::default();
    let mut stack: Vec<State> = Vec::new();
    // the current path's bounds in device space, as its points were added under the transform of the time
    let mut path = Bounds::EMPTY;
    let mut pen: Option<(f64, f64)> = None;
    for op in ops {
        match op {
            Canvas2DOp::PutImageData { image, dx, dy, dirty_x, dirty_y, dirty_w, dirty_h } => {
                // as `put_image_data` in korvus-render places it
                let (iw, ih) = (image.width as i64, image.height as i64);
                let x0 = dirty_x.floor().max(0.0) as i64;
                let y0 = dirty_y.floor().max(0.0) as i64;
                let x1 = ((dirty_x + dirty_w).ceil() as i64).min(iw);
                let y1 = ((dirty_y + dirty_h).ceil() as i64).min(ih);
                if x1 > x0 && y1 > y0 {
                    let (ax, ay) = (dx.round() as i64 + x0, dy.round() as i64 + y0);
                    m.set(ax, ay, ax + (x1 - x0), ay + (y1 - y0), true);
                }
            }
            Canvas2DOp::SetFillStyle { color } => st.fill_alpha = color[3] as f64,
            Canvas2DOp::SetFillGradient { .. } | Canvas2DOp::SetFillPattern { .. } => st.fill_alpha = f64::NAN,
            Canvas2DOp::SetGlobalAlpha { alpha } => st.global_alpha = *alpha,
            Canvas2DOp::Clip { .. } | Canvas2DOp::ClipPath { .. } => st.clipped = true,
            Canvas2DOp::Save => stack.push(st.clone()),
            Canvas2DOp::Restore => {
                if let Some(s) = stack.pop() {
                    st = s;
                }
            }
            // the document paint's layers: what they composite is not followed here
            Canvas2DOp::SaveLayer { .. } | Canvas2DOp::SaveLayerFilter { .. } | Canvas2DOp::SaveLayerBackdrop { .. } | Canvas2DOp::SaveLayerBlend { .. } => {
                stack.push(st.clone());
                m.clear_all();
            }
            Canvas2DOp::Transform { t } => st.ctm = st.ctm.multiply(t),
            Canvas2DOp::SetTransform { t } => st.ctm = *t,
            Canvas2DOp::SetLineWidth { width } => st.line_width = *width,
            Canvas2DOp::SetMiterLimit { limit } => st.miter = *limit,
            Canvas2DOp::SetShadowColor { color } => st.shadow_alpha = color[3] as f64,
            Canvas2DOp::SetShadowBlur { blur } => st.shadow_blur = *blur,
            Canvas2DOp::SetShadowOffset { x, y } => st.shadow_offset = (*x, *y),
            Canvas2DOp::SetFilter { filters } => st.filtered = !filters.is_empty(),
            Canvas2DOp::SetGlobalCompositeOperation { op } => st.composite = *op,
            Canvas2DOp::SetFont { size_px, .. } => st.font_px = *size_px,
            Canvas2DOp::SetLetterSpacing { px } => st.letter = *px,
            Canvas2DOp::SetWordSpacing { px } => st.word = *px,
            Canvas2DOp::BeginPath => {
                path = Bounds::EMPTY;
                pen = None;
            }
            Canvas2DOp::MoveTo { x, y } | Canvas2DOp::LineTo { x, y } => {
                path.add(&st.ctm, *x, *y);
                pen = Some((*x, *y));
            }
            Canvas2DOp::Arc { x, y, radius, .. } => {
                path.add_box(&st.ctm, *x - radius, *y - radius, *x + radius, *y + radius);
                pen.get_or_insert((*x, *y));
            }
            Canvas2DOp::Ellipse { x, y, rx, ry, .. } => {
                let r = rx.abs().max(ry.abs());
                path.add_box(&st.ctm, *x - r, *y - r, *x + r, *y + r);
                pen.get_or_insert((*x, *y));
            }
            Canvas2DOp::Rect { x, y, w, h } | Canvas2DOp::RoundRect { x, y, w, h, .. } => {
                path.add_box(&st.ctm, *x, *y, *x + w, *y + h);
                pen = Some((*x, *y));
            }
            Canvas2DOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => {
                path.add(&st.ctm, *cp1x, *cp1y);
                path.add(&st.ctm, *cp2x, *cp2y);
                path.add(&st.ctm, *x, *y);
                pen = Some((*x, *y));
            }
            Canvas2DOp::QuadraticCurveTo { cpx, cpy, x, y } => {
                path.add(&st.ctm, *cpx, *cpy);
                path.add(&st.ctm, *x, *y);
                pen = Some((*x, *y));
            }
            Canvas2DOp::ArcTo { x1, y1, x2, y2, radius } => {
                // the tangent arc lies in the corner the three points make, the radius about it
                for (x, y) in [(*x1, *y1), (*x2, *y2)] {
                    path.add_box(&st.ctm, x - radius, y - radius, x + radius, y + radius);
                }
                if let Some((px, py)) = pen {
                    path.add(&st.ctm, px, py);
                }
                pen = Some((*x2, *y2));
            }
            Canvas2DOp::ClosePath => {}
            Canvas2DOp::Fill { .. } => m.draw(&st, path, 0.0),
            Canvas2DOp::Stroke => m.draw(&st, path, st.stroke_reach()),
            Canvas2DOp::FillPath { path: p, .. } => m.draw(&st, path_bounds(&st.ctm, p), 0.0),
            Canvas2DOp::StrokePath { path: p } => m.draw(&st, path_bounds(&st.ctm, p), st.stroke_reach()),
            Canvas2DOp::FillRect { x, y, w, h } | Canvas2DOp::ClearRect { x, y, w, h } => {
                let mut b = Bounds::EMPTY;
                b.add_box(&st.ctm, *x, *y, x + w, y + h);
                let clear = matches!(op, Canvas2DOp::ClearRect { .. });
                // a rectangle on pixel boundaries leaves no partial pixel: an opaque solid fill, or a clear (which
                // ignores shadows, filters, alpha and the composite mode), sets each pixel it covers to one value
                let opaque_solid = st.fill_alpha == 1.0 && st.global_alpha == 1.0 && st.composite == CompositeOp::SourceOver && !st.reaches_everywhere();
                match b.pixel_aligned().filter(|_| st.axis_aligned() && !st.clipped && (clear || opaque_solid)) {
                    Some((x0, y0, x1, y1)) => m.set(x0, y0, x1, y1, true),
                    None if clear => m.unmark(b, 1.0),
                    None => m.draw(&st, b, 0.0),
                }
            }
            Canvas2DOp::StrokeRect { x, y, w, h } => {
                let mut b = Bounds::EMPTY;
                b.add_box(&st.ctm, *x, *y, x + w, y + h);
                m.draw(&st, b, st.stroke_reach());
            }
            Canvas2DOp::FillText { text, x, y, .. } | Canvas2DOp::StrokeText { text, x, y, .. } => {
                // no shaping here: a run no wider than two ems a character plus its spacing, on either side of its
                // origin (any `textAlign`), three ems above or below it (any `textBaseline`)
                let em = st.font_px.abs().max(1.0);
                let n = text.chars().count() as f64;
                let half = n * (2.0 * em + st.letter.abs() + st.word.abs()) + 2.0 * em;
                let mut b = Bounds::EMPTY;
                b.add_box(&st.ctm, x - half, y - 3.0 * em, x + half, y + 3.0 * em);
                let reach = if matches!(op, Canvas2DOp::StrokeText { .. }) { st.stroke_reach() } else { 0.0 };
                m.draw(&st, b, reach);
            }
            Canvas2DOp::DrawImage { image, sx, sy, sw, sh, dx, dy, dw, dh } => {
                let mut b = Bounds::EMPTY;
                b.add_box(&st.ctm, *dx, *dy, dx + dw, dy + dh);
                // one to one: no scale, no resampling, on pixel boundaries -- an opaque source pixel is copied as it is
                let t = &st.ctm;
                let copy = t.a == 1.0 && t.d == 1.0 && t.b == 0.0 && t.c == 0.0 && sw == dw && sh == dh && *sw > 0.0 && *sh > 0.0
                    && whole(*sx) && whole(*sy) && st.global_alpha == 1.0 && st.composite == CompositeOp::SourceOver
                    && !st.reaches_everywhere() && !st.clipped;
                match b.pixel_aligned().filter(|_| copy) {
                    Some((x0, y0, x1, y1)) => {
                        let (sx, sy) = (*sx as i64, *sy as i64);
                        for y in y0.max(0)..y1.min(m.h) {
                            for x in x0.max(0)..x1.min(m.w) {
                                let (ix, iy) = (sx + x - x0, sy + y - y0);
                                let opaque = ix >= 0 && iy >= 0 && ix < image.width as i64 && iy < image.height as i64
                                    && image.data.get(((iy * image.width as i64 + ix) * 4 + 3) as usize) == Some(&255);
                                m.set(x, y, x + 1, y + 1, opaque);
                            }
                        }
                    }
                    None => m.draw(&st, b, 1.0),
                }
            }
            _ => {}
        }
    }
    (m.kept > 0).then_some(m.keep)
}

fn path_bounds(ctm: &Transform2D, p: &[PathOp]) -> Bounds {
    let mut b = Bounds::EMPTY;
    let mut pen: Option<(f64, f64)> = None;
    for op in p {
        match op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => {
                b.add(ctm, *x, *y);
                pen = Some((*x, *y));
            }
            PathOp::Arc { x, y, radius, .. } => b.add_box(ctm, x - radius, y - radius, x + radius, y + radius),
            PathOp::Ellipse { x, y, rx, ry, .. } => {
                let r = rx.abs().max(ry.abs());
                b.add_box(ctm, x - r, y - r, x + r, y + r);
            }
            PathOp::Rect { x, y, w, h } | PathOp::RoundRect { x, y, w, h, .. } => {
                b.add_box(ctm, *x, *y, x + w, y + h);
                pen = Some((*x, *y));
            }
            PathOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => {
                b.add(ctm, *cp1x, *cp1y);
                b.add(ctm, *cp2x, *cp2y);
                b.add(ctm, *x, *y);
                pen = Some((*x, *y));
            }
            PathOp::QuadraticCurveTo { cpx, cpy, x, y } => {
                b.add(ctm, *cpx, *cpy);
                b.add(ctm, *x, *y);
                pen = Some((*x, *y));
            }
            PathOp::ArcTo { x1, y1, x2, y2, radius } => {
                for (x, y) in [(*x1, *y1), (*x2, *y2)] {
                    b.add_box(ctm, x - radius, y - radius, x + radius, y + radius);
                }
                if let Some((px, py)) = pen {
                    b.add(ctm, px, py);
                }
                pen = Some((*x2, *y2));
            }
            PathOp::ClosePath => {}
        }
    }
    b
}

/// The drawing state that decides how far a draw reaches.
#[derive(Clone)]
struct State {
    ctm: Transform2D,
    line_width: f64,
    miter: f64,
    shadow_alpha: f64,
    shadow_blur: f64,
    shadow_offset: (f64, f64),
    filtered: bool,
    composite: CompositeOp,
    font_px: f64,
    letter: f64,
    word: f64,
    /// The solid fill colour's alpha; NaN for a gradient or pattern fill.
    fill_alpha: f64,
    global_alpha: f64,
    /// A clip is in force: its edge may cut any draw anti-aliased.
    clipped: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            ctm: Transform2D::IDENTITY,
            line_width: 1.0,
            miter: 10.0,
            shadow_alpha: 0.0,
            shadow_blur: 0.0,
            shadow_offset: (0.0, 0.0),
            filtered: false,
            composite: CompositeOp::SourceOver,
            font_px: 10.0,
            letter: 0.0,
            word: 0.0,
            fill_alpha: 1.0,
            global_alpha: 1.0,
            clipped: false,
        }
    }
}

impl State {
    /// A draw that may change pixels anywhere on the canvas, whatever its shape's bounds.
    fn reaches_everywhere(&self) -> bool {
        let shadow = self.shadow_alpha > 0.0 && (self.shadow_blur > 0.0 || self.shadow_offset != (0.0, 0.0));
        let outside = matches!(
            self.composite,
            CompositeOp::Copy | CompositeOp::SourceIn | CompositeOp::SourceOut | CompositeOp::DestinationIn | CompositeOp::DestinationAtop
        );
        shadow || self.filtered || outside || !self.ctm.is_finite()
    }

    /// The transform keeps rectangles axis-aligned (no rotation or skew).
    fn axis_aligned(&self) -> bool {
        self.ctm.b == 0.0 && self.ctm.c == 0.0
    }

    /// How far past its path a stroke paints, in device px: half the line, its miter at most, through the transform.
    fn stroke_reach(&self) -> f64 {
        let t = &self.ctm;
        let scale = (t.a * t.a + t.b * t.b).sqrt().max((t.c * t.c + t.d * t.d).sqrt());
        self.line_width.abs() * scale * self.miter.max(1.5) / 2.0
    }
}

fn whole(v: f64) -> bool {
    v.is_finite() && (v - v.round()).abs() < 1e-9
}

/// An axis-aligned box in device space.
#[derive(Clone, Copy)]
struct Bounds {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bounds {
    const EMPTY: Bounds = Bounds { x0: f64::INFINITY, y0: f64::INFINITY, x1: f64::NEG_INFINITY, y1: f64::NEG_INFINITY };

    fn add(&mut self, t: &Transform2D, x: f64, y: f64) {
        let (dx, dy) = (t.a * x + t.c * y + t.e, t.b * x + t.d * y + t.f);
        if !dx.is_finite() || !dy.is_finite() {
            // a point no box holds: take everything
            *self = Bounds { x0: f64::NEG_INFINITY, y0: f64::NEG_INFINITY, x1: f64::INFINITY, y1: f64::INFINITY };
            return;
        }
        self.x0 = self.x0.min(dx);
        self.y0 = self.y0.min(dy);
        self.x1 = self.x1.max(dx);
        self.y1 = self.y1.max(dy);
    }

    fn add_box(&mut self, t: &Transform2D, x0: f64, y0: f64, x1: f64, y1: f64) {
        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            self.add(t, x, y);
        }
    }

    fn is_empty(&self) -> bool {
        !(self.x0 <= self.x1 && self.y0 <= self.y1)
    }

    /// The box as whole device pixels `(x0, y0, x1, y1)`, when every edge lies on a pixel boundary.
    fn pixel_aligned(&self) -> Option<(i64, i64, i64, i64)> {
        let v = [self.x0, self.y0, self.x1, self.y1];
        (!self.is_empty() && v.iter().all(|e| whole(*e) && e.abs() < 1e9)).then(|| (self.x0 as i64, self.y0 as i64, self.x1 as i64, self.y1 as i64))
    }
}

/// Which pixels are still the page's.
struct Mask {
    w: i64,
    h: i64,
    keep: Vec<bool>,
    kept: usize,
}

impl Mask {
    fn set(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, v: bool) {
        let (x0, y0, x1, y1) = (x0.max(0), y0.max(0), x1.min(self.w), y1.min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * self.w + x) as usize;
                if self.keep[i] != v {
                    self.keep[i] = v;
                    if v {
                        self.kept += 1;
                    } else {
                        self.kept -= 1;
                    }
                }
            }
        }
    }

    fn clear_all(&mut self) {
        if self.kept > 0 {
            self.keep.iter_mut().for_each(|k| *k = false);
            self.kept = 0;
        }
    }

    /// Give up every pixel within `margin` px (plus one for anti-aliasing) of `b`.
    fn unmark(&mut self, b: Bounds, margin: f64) {
        if self.kept == 0 || b.is_empty() {
            return;
        }
        let m = margin.max(0.0) + 1.0;
        let clamp = |v: f64, hi: i64| if v.is_nan() { 0 } else { v.clamp(-1.0, hi as f64 + 1.0) as i64 };
        let (x0, y0) = (clamp((b.x0 - m).floor(), self.w), clamp((b.y0 - m).floor(), self.h));
        let (x1, y1) = (clamp((b.x1 + m).ceil() + 1.0, self.w), clamp((b.y1 + m).ceil() + 1.0, self.h));
        self.set(x0, y0, x1, y1, false);
    }

    /// A draw of a shape within `b`, painted `reach` px past it.
    fn draw(&mut self, st: &State, b: Bounds, reach: f64) {
        if self.kept == 0 {
            return;
        }
        if st.reaches_everywhere() {
            self.clear_all();
        } else {
            self.unmark(b, reach);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dx: f64, dy: f64, w: u32, h: u32) -> Canvas2DOp {
        Canvas2DOp::PutImageData {
            image: Rgba8 { width: w, height: h, data: vec![9; (w * h * 4) as usize] },
            dx,
            dy,
            dirty_x: 0.0,
            dirty_y: 0.0,
            dirty_w: w as f64,
            dirty_h: h as f64,
        }
    }

    fn kept(m: &[bool], w: u32) -> Vec<(u32, u32)> {
        m.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| (i as u32 % w, i as u32 / w)).collect()
    }

    /// A canvas whose every pixel a curve may have touched: nothing exact left.
    fn inked() -> Vec<Canvas2DOp> {
        vec![Canvas2DOp::BeginPath, Canvas2DOp::Arc { x: 16.0, y: 16.0, radius: 40.0, start: 0.0, end: 6.3, ccw: false }, Canvas2DOp::Fill { even_odd: false }]
    }

    fn with(mut a: Vec<Canvas2DOp>, b: Vec<Canvas2DOp>) -> Vec<Canvas2DOp> {
        a.extend(b);
        a
    }

    #[test]
    fn an_untouched_canvas_and_a_curve_s_ink() {
        assert_eq!(kept(&exact_pixels(&[], 8, 8).unwrap(), 8).len(), 64, "the background is every device's");
        assert!(exact_pixels(&inked(), 32, 32).is_none());
    }

    #[test]
    fn a_put_is_kept_where_it_landed() {
        let m = exact_pixels(&with(inked(), vec![put(2.0, 3.0, 2, 2)]), 8, 8).unwrap();
        assert_eq!(kept(&m, 8), vec![(2, 3), (3, 3), (2, 4), (3, 4)]);
    }

    #[test]
    fn an_opaque_solid_rect_on_pixel_boundaries_is_exact_and_any_other_is_not() {
        let rect = |x: f64| Canvas2DOp::FillRect { x, y: 0.0, w: 2.0, h: 2.0 };
        assert_eq!(kept(&exact_pixels(&with(inked(), vec![rect(4.0)]), 32, 32).unwrap(), 32), vec![(4, 0), (5, 0), (4, 1), (5, 1)]);
        assert!(exact_pixels(&with(inked(), vec![rect(4.5)]), 32, 32).is_none(), "a half-pixel edge is anti-aliased");
        let faint = vec![Canvas2DOp::SetFillStyle { color: [1.0, 0.0, 0.0, 0.5] }, rect(4.0)];
        assert!(exact_pixels(&with(inked(), faint), 32, 32).is_none(), "a translucent fill blends");
        let faded = vec![Canvas2DOp::SetGlobalAlpha { alpha: 0.5 }, rect(4.0)];
        assert!(exact_pixels(&with(inked(), faded), 32, 32).is_none());
        let rotated = vec![Canvas2DOp::Transform { t: Transform2D::rotate(0.3) }, rect(4.0)];
        assert!(exact_pixels(&with(inked(), rotated), 32, 32).is_none());
        let clipped = vec![Canvas2DOp::Clip { even_odd: false }, rect(4.0)];
        assert!(exact_pixels(&with(inked(), clipped), 32, 32).is_none(), "a clip's edge may cut it");
    }

    #[test]
    fn a_draw_away_from_exact_pixels_leaves_them_and_one_over_them_takes_them() {
        let blob = |x: f64, y: f64| vec![Canvas2DOp::BeginPath, Canvas2DOp::Arc { x, y, radius: 2.0, start: 0.0, end: 6.3, ccw: false }, Canvas2DOp::Fill { even_odd: false }];
        let ops = with(with(inked(), vec![put(0.0, 0.0, 4, 4)]), blob(20.0, 20.0));
        assert_eq!(kept(&exact_pixels(&ops, 32, 32).unwrap(), 32).len(), 16);
        let ops = with(with(inked(), vec![put(0.0, 0.0, 4, 4)]), blob(4.0, 4.0));
        // the blob's box (2..6) widened by one for anti-aliasing reaches (1, 1): the top row and left column stay
        assert_eq!(kept(&exact_pixels(&ops, 32, 32).unwrap(), 32), vec![(0, 0), (1, 0), (2, 0), (3, 0), (0, 1), (0, 2), (0, 3)]);
    }

    #[test]
    fn a_shadow_takes_everything_until_a_restore() {
        let shadow = vec![Canvas2DOp::SetShadowColor { color: [0.0, 0.0, 0.0, 1.0] }, Canvas2DOp::SetShadowBlur { blur: 4.0 }];
        let ops = with(with(vec![put(0.0, 0.0, 4, 4)], shadow.clone()), vec![Canvas2DOp::FillRect { x: 20.0, y: 20.0, w: 2.0, h: 2.0 }]);
        assert!(exact_pixels(&ops, 32, 32).is_none());
        let mut ops = vec![Canvas2DOp::Save];
        ops.extend(shadow);
        ops.extend([Canvas2DOp::Restore, Canvas2DOp::FillRect { x: 20.0, y: 20.0, w: 2.0, h: 2.0 }]);
        assert_eq!(kept(&exact_pixels(&ops, 32, 32).unwrap(), 32).len(), 32 * 32);
    }

    #[test]
    fn an_opaque_image_copied_one_to_one_is_exact() {
        let mut img = Rgba8 { width: 2, height: 1, data: vec![9, 9, 9, 255, 9, 9, 9, 128] };
        let draw = |img: &Rgba8, dw: f64| Canvas2DOp::DrawImage { image: img.clone(), sx: 0.0, sy: 0.0, sw: 2.0, sh: 1.0, dx: 3.0, dy: 2.0, dw, dh: 1.0 };
        // the opaque pixel is copied as it is; the translucent one blends
        assert_eq!(kept(&exact_pixels(&with(inked(), vec![draw(&img, 2.0)]), 8, 8).unwrap(), 8), vec![(3, 2)]);
        assert!(exact_pixels(&with(inked(), vec![draw(&img, 4.0)]), 8, 8).is_none(), "a scaled image is resampled");
        img.data[7] = 255;
        assert_eq!(kept(&exact_pixels(&with(inked(), vec![draw(&img, 2.0)]), 8, 8).unwrap(), 8), vec![(3, 2), (4, 2)]);
    }

    #[test]
    fn text_and_paths_reach_their_bounds() {
        let font = Canvas2DOp::SetFont { family: "x".into(), size_px: 4.0, bold: false, italic: false, weight: 400, families: String::new(), lang: String::new(), default_family: String::new() };
        let ops = vec![put(0.0, 0.0, 4, 4), font, Canvas2DOp::FillText { text: "ab".into(), x: 30.0, y: 10.0, max_width: None }];
        // two chars at 4px: 8 + 2*8 = 24 px either side of x = 30 -> from 6 (5 with anti-aliasing)
        let k = kept(&exact_pixels(&ops, 64, 64).unwrap(), 64);
        assert!(k.contains(&(4, 4)) && !k.contains(&(5, 4)), "{:?}", &k[..8]);
        let ops = vec![Canvas2DOp::BeginPath, Canvas2DOp::MoveTo { x: 10.0, y: 10.0 }, Canvas2DOp::LineTo { x: 20.0, y: 10.0 }, Canvas2DOp::Stroke];
        // a 1px line with a miter of 10 reaches 5 px (+1)
        let k = kept(&exact_pixels(&ops, 32, 32).unwrap(), 32);
        assert!(k.contains(&(3, 3)) && !k.contains(&(4, 4)) && k.contains(&(27, 10)));
    }

    #[test]
    fn a_float_step_rounds_to_the_8_bit_step() {
        let r = |v: f32| (v * 255.0).round() as i32;
        assert_eq!(r(float_step(254.0 / 255.0, 1)), 255, "the top step is reached, not mistaken for past it");
        assert_eq!(float_step(254.0 / 255.0, 1), 1.0);
        assert_eq!(r(float_step(1.0, 1)), 254, "at the top an up step goes down");
        assert_eq!(r(float_step(0.0, -1)), 1, "at the bottom a down step goes up");
        assert_eq!(r(float_step(100.0 / 255.0, -1)), 99);
    }

    #[test]
    fn noise_is_carried_one_step_onto_another_format() {
        let raw = Rgba8 { width: 2, height: 1, data: vec![10, 20, 30, 255, 40, 50, 60, 255] };
        let mut noised = raw.clone();
        noised.data[4] = 41;
        let mut u8s = raw.data.clone();
        carry_noise(&raw, &noised, 0, 0, 2, 1, ReadFormat::Unorm8, &mut u8s);
        assert_eq!(u8s, noised.data);
        let mut f32s: Vec<u8> = raw.data.iter().flat_map(|v| (*v as f32 / 255.0).to_le_bytes()).collect();
        carry_noise(&raw, &noised, 0, 0, 2, 1, ReadFormat::Float32, &mut f32s);
        let back: Vec<u8> = f32s.chunks(4).map(|b| (f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * 255.0).round() as u8).collect();
        assert_eq!(back, noised.data);
        // a region read from (1, 0): the second pixel only
        let mut one = raw.data[4..8].to_vec();
        carry_noise(&raw, &noised, 1, 0, 1, 1, ReadFormat::Unorm8, &mut one);
        assert_eq!(one, noised.data[4..8]);
    }
}

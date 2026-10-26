//! The Canvas 2D interpreter: one recorded display list, executed into a Skia canvas.
//!
//! This is the other half of `spec::Canvas2DOp`. The vocabulary is defined once in the contract
//! crate and every variant of it is executed HERE — a recorded op with no arm in this file would let
//! `getImageData` and `toDataURL` claim a fidelity the pixels do not have, which is the one failure
//! a display list must not have. (`SolidBackend` in `lib.rs` is deliberately different: it is the
//! pure-Rust rect-only reference, and says so.)
//!
//! ## Two coordinate spaces, and why the split is not arbitrary
//!
//! HTML bakes the transform into the PATH as it is built: `moveTo` under one matrix and `lineTo`
//! under another gives a segment whose ends were mapped by different matrices. So the current path
//! accumulates in DEVICE space, each point mapped by the CTM at the moment it was added.
//!
//! Stroking, though, happens in USER space — `lineWidth` is a user-space length, so `scale(2,2)`
//! doubles it and a non-uniform scale makes an elliptical pen. So `stroke` maps the device path back
//! through the inverse CTM and strokes it under `concat(ctm)`, which is the geometry the browser
//! strokes. When the CTM is the identity the inverse is the identity and the path is used unchanged:
//! the untransformed case — every canvas fingerprint probe in the wild — goes through exactly the
//! code it did before this file existed, which is what keeps the pixel ratchet honest.
//!
//! Rectangles, text and images are given in user space at the point of the call, so they draw under
//! `concat(ctm)` directly.

use skia_safe::font::Edging;
#[allow(deprecated)]
use skia_safe::gradient_shader;
use skia_safe::{
    color_filters, dash_path_effect, image_filters, images, path, path_utils,
    surfaces, AlphaType, BlendMode, Canvas, ClipOp, Color, Color4f, ColorType, CubicResampler, FilterMode, Font,
    FontHinting, FontMgr, FontStyle, IPoint, Image, ImageFilter, ImageInfo, Matrix, MipmapMode,
    Paint, PaintCap, PaintJoin, PaintStyle, Path, PathBuilder, PathDirection, PathFillType, Point,
    RRect, Rect, SamplingOptions, Shader, TileMode, Typeface, Vector,
};
use spec::{
    Canvas2DOp, CanvasFilter, CompositeOp, HitTest, LineCap, LineJoin, PathOp, PatternRepeat, Rgba8,
    SmoothingQuality, TextAlign, TextBaseline, TextDirection, Transform2D,
};
use std::collections::HashMap;

/// Map a canvas `CompositeOp` to Skia's `BlendMode` (the mode names align 1:1 with Chrome's canvas).
pub(crate) fn composite_to_blend(op: CompositeOp) -> BlendMode {
    match op {
        CompositeOp::SourceOver => BlendMode::SrcOver,
        CompositeOp::SourceIn => BlendMode::SrcIn,
        CompositeOp::SourceOut => BlendMode::SrcOut,
        CompositeOp::SourceAtop => BlendMode::SrcATop,
        CompositeOp::DestinationOver => BlendMode::DstOver,
        CompositeOp::DestinationIn => BlendMode::DstIn,
        CompositeOp::DestinationOut => BlendMode::DstOut,
        CompositeOp::DestinationAtop => BlendMode::DstATop,
        CompositeOp::Lighter => BlendMode::Plus,
        CompositeOp::Copy => BlendMode::Src,
        CompositeOp::Xor => BlendMode::Xor,
        CompositeOp::Multiply => BlendMode::Multiply,
        CompositeOp::Screen => BlendMode::Screen,
        CompositeOp::Overlay => BlendMode::Overlay,
        CompositeOp::Darken => BlendMode::Darken,
        CompositeOp::Lighten => BlendMode::Lighten,
        CompositeOp::ColorDodge => BlendMode::ColorDodge,
        CompositeOp::ColorBurn => BlendMode::ColorBurn,
        CompositeOp::HardLight => BlendMode::HardLight,
        CompositeOp::SoftLight => BlendMode::SoftLight,
        CompositeOp::Difference => BlendMode::Difference,
        CompositeOp::Exclusion => BlendMode::Exclusion,
        CompositeOp::Hue => BlendMode::Hue,
        CompositeOp::Saturation => BlendMode::Saturation,
        CompositeOp::Color => BlendMode::Color,
        CompositeOp::Luminosity => BlendMode::Luminosity,
    }
}

thread_local! {
    /// The typeface each `(family, style)` resolves to, and each `(family, style, character)`'s fallback.
    /// A canvas read REPLAYS every recorded op, and every text op asked fontconfig to
    /// match its family again -- ~10 ms per `fillText` per read (300x300 after 200 fillText, read three
    /// times: 5.9 s against Chrome's 9 ms; a device-check script times its canvas probe). The answers do not change
    /// while the process runs.
    static TYPEFACES: std::cell::RefCell<HashMap<(String, i32, i32, i32), Option<Typeface>>> = std::cell::RefCell::new(HashMap::new());
    static FALLBACKS: std::cell::RefCell<HashMap<(String, i32, i32, i32, u32), Option<Typeface>>> = std::cell::RefCell::new(HashMap::new());
}

fn style_key(style: FontStyle) -> (i32, i32, i32) {
    (*style.weight(), *style.width(), style.slant() as i32)
}

/// `font_mgr.legacy_make_typeface(Some(family), style)`, once per family and style.
pub(crate) fn cached_typeface(font_mgr: &FontMgr, family: &str, style: FontStyle) -> Option<Typeface> {
    let (w, wd, sl) = style_key(style);
    let key = (family.to_string(), w, wd, sl);
    if let Some(hit) = TYPEFACES.with(|t| t.borrow().get(&key).cloned()) {
        return hit;
    }
    let tf = font_mgr.legacy_make_typeface(Some(family), style);
    TYPEFACES.with(|t| t.borrow_mut().insert(key, tf.clone()));
    tf
}

/// `font_mgr.match_family_style_character(family, style, &[], ch)`, once per family, style and character.
fn cached_fallback(font_mgr: &FontMgr, family: &str, style: FontStyle, ch: char) -> Option<Typeface> {
    let (w, wd, sl) = style_key(style);
    let key = (family.to_string(), w, wd, sl, ch as u32);
    if let Some(hit) = FALLBACKS.with(|t| t.borrow().get(&key).cloned()) {
        return hit;
    }
    let tf = font_mgr.match_family_style_character(family, style, &[], ch as i32);
    FALLBACKS.with(|t| t.borrow_mut().insert(key, tf.clone()));
    tf
}

/// Build a Blink-canvas-style `Font` from a typeface: grayscale AA, subpixel positioning, no hinting.
pub(crate) fn mk_font(tf: Option<Typeface>, size: f32) -> Option<Font> {
    let mut f = Font::new(tf?, Some(size));
    f.set_edging(Edging::AntiAlias);
    f.set_subpixel(true);
    f.set_hinting(FontHinting::None);
    f.set_force_auto_hinting(false);
    Some(f)
}

/// `spec::Transform2D` -> Skia's row-major 3x3.
fn mat(t: &Transform2D) -> Matrix {
    Matrix::new_all(t.a as f32, t.c as f32, t.e as f32, t.b as f32, t.d as f32, t.f as f32, 0.0, 0.0, 1.0)
}

fn cap_of(c: LineCap) -> PaintCap {
    match c {
        LineCap::Butt => PaintCap::Butt,
        LineCap::Round => PaintCap::Round,
        LineCap::Square => PaintCap::Square,
    }
}

fn join_of(j: LineJoin) -> PaintJoin {
    match j {
        LineJoin::Miter => PaintJoin::Miter,
        LineJoin::Round => PaintJoin::Round,
        LineJoin::Bevel => PaintJoin::Bevel,
    }
}

fn tiles_of(r: PatternRepeat) -> (TileMode, TileMode) {
    match r {
        PatternRepeat::Repeat => (TileMode::Repeat, TileMode::Repeat),
        PatternRepeat::RepeatX => (TileMode::Repeat, TileMode::Decal),
        PatternRepeat::RepeatY => (TileMode::Decal, TileMode::Repeat),
        PatternRepeat::NoRepeat => (TileMode::Decal, TileMode::Decal),
    }
}

/// Wrap an unpremultiplied RGBA8 buffer as a Skia image (a copy — the caller's buffer is shared).
///
/// PREMULTIPLIED FIRST. A Skia image tagged `Unpremul` is SAMPLED unpremultiplied, so a
/// smoothed `drawImage` blended colour and alpha separately and a red pixel next to a half-transparent
/// yellow one read back 159,64,47,247 where Chrome 150 reads 156,57,49,247 (a 20x20 four-colour PNG
/// drawn at 2x, pixel 19,19). Chrome's decoded images are premultiplied, which makes the bilinear
/// weights apply to premultiplied colour -- the arithmetic reproduces Chrome's value exactly. The
/// rounding is Skia's own `SkMulDiv255Round`.
///
/// N32, as Chrome's decoded images and canvas snapshots are: Chrome's Skia samples an
/// N32 image drawn source-over through its legacy shader context (`SK_ENABLE_LEGACY_SHADERCONTEXT`, 4-bit
/// bilinear weights), any other colour type through the raster pipeline -- an 8x1 strip drawn 3 px wide read
/// 207/147/98 in Chrome and 213/148/102 from an RGBA source.
fn image_of(buf: &Rgba8) -> Option<Image> {
    let need = (buf.width as usize).checked_mul(buf.height as usize)?.checked_mul(4)?;
    if buf.width == 0 || buf.height == 0 || buf.data.len() < need {
        return None;
    }
    let info = ImageInfo::new(
        (buf.width as i32, buf.height as i32),
        ColorType::N32,
        AlphaType::Premul,
        None,
    );
    let bgra = ColorType::N32 == ColorType::BGRA8888;
    let mul = |c: u8, a: u8| -> u8 {
        let p = c as u32 * a as u32 + 128;
        ((p + (p >> 8)) >> 8) as u8
    };
    let mut px = buf.data[..need].to_vec();
    for p in px.chunks_exact_mut(4) {
        let a = p[3];
        if a != 255 {
            p[0] = mul(p[0], a);
            p[1] = mul(p[1], a);
            p[2] = mul(p[2], a);
        }
        if bgra {
            p.swap(0, 2);
        }
    }
    let data = skia_safe::Data::new_copy(&px);
    images::raster_from_data(&info, data, (buf.width as usize) * 4)
}

/// Where a fill or a stroke gets its colour: a flat colour, or a live gradient/pattern object.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// A paint colour as Blink hands Skia one ([`spec::Color4f`]: unpremultiplied extended sRGB).
    Color(spec::Color4f),
    Gradient(u32),
    Pattern(u32),
}

#[derive(Debug, Clone, Copy)]
enum GradKind {
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    Radial { x0: f32, y0: f32, r0: f32, x1: f32, y1: f32, r1: f32 },
    Conic { start: f32, cx: f32, cy: f32 },
}

/// A `CanvasGradient` as the display list sees it: its geometry, plus the stops added SO FAR. The
/// stops arrive as their own op, so a stop added after a draw affects only later draws — which is
/// what a live gradient object does in a browser.
#[derive(Debug, Clone)]
struct Gradient {
    kind: GradKind,
    stops: Vec<Stop>,
    /// Interpolated with premultiplied alpha (`Canvas2DOp::PremultiplyGradient`).
    premul: bool,
}

/// One `addColorStop`: its offset, its sRGB paint colour, the colour as an OKLab-interpolated gradient takes it,
/// and whether it interpolates as a legacy colour (see `Canvas2DOp::AddColorStop`).
#[derive(Debug, Clone, Copy)]
struct Stop {
    pos: f32,
    color: Color4f,
    oklab: Color4f,
    legacy: bool,
}

struct Pattern {
    image: Option<Image>,
    repeat: PatternRepeat,
    transform: Transform2D,
}

/// The canvas drawing state — everything `save()` pushes and `restore()` pops. The current path is
/// deliberately NOT here: HTML keeps it outside the state stack.
#[derive(Clone)]
struct State {
    ctm: Transform2D,
    fill: Source,
    stroke: Source,
    alpha: f32,
    composite: BlendMode,
    line_width: f32,
    cap: PaintCap,
    join: PaintJoin,
    miter: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    shadow_color: spec::Color4f,
    shadow_blur: f32,
    shadow_dx: f32,
    shadow_dy: f32,
    filters: Vec<CanvasFilter>,
    smoothing: bool,
    smoothing_quality: SmoothingQuality,
    font_family: String,
    font_size: f32,
    font_bold: bool,
    font_italic: bool,
    /// The whole `font-family` list, the shaping language and the default face (`SetFont`); empty
    /// when the op carried none (the page painter), which draws as before.
    font_families: String,
    font_lang: String,
    font_default: String,
    align: TextAlign,
    baseline: TextBaseline,
    direction: TextDirection,
    letter_spacing: f32,
    word_spacing: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Transform2D::IDENTITY,
            fill: Source::Color([0.0, 0.0, 0.0, 1.0]), // 2D default: opaque black
            stroke: Source::Color([0.0, 0.0, 0.0, 1.0]),
            alpha: 1.0,
            composite: BlendMode::SrcOver,
            line_width: 1.0,
            cap: PaintCap::Butt,
            join: PaintJoin::Miter,
            miter: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            shadow_color: [0.0, 0.0, 0.0, 0.0],
            shadow_blur: 0.0,
            shadow_dx: 0.0,
            shadow_dy: 0.0,
            filters: Vec::new(),
            smoothing: true,
            smoothing_quality: SmoothingQuality::Low,
            font_family: String::from("sans-serif"), // canvas default is "10px sans-serif"
            font_size: 10.0,
            font_bold: false,
            font_italic: false,
            font_families: String::new(),
            font_lang: String::new(),
            font_default: String::new(),
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: TextDirection::Inherit,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
    }
}

impl State {
    fn font_style(&self) -> FontStyle {
        match (self.font_bold, self.font_italic) {
            (true, true) => FontStyle::bold_italic(),
            (true, false) => FontStyle::bold(),
            (false, true) => FontStyle::italic(),
            (false, false) => FontStyle::normal(),
        }
    }
    /// A shadow is drawn only when its colour is not fully transparent AND something offsets or
    /// blurs it — exactly HTML's condition.
    fn has_shadow(&self) -> bool {
        self.shadow_color[3] > 0.0
            && (self.shadow_blur > 0.0 || self.shadow_dx != 0.0 || self.shadow_dy != 0.0)
    }
    /// The shadow of a fill, a stroke or text goes through cc's draw looper, as Blink draws it (see
    /// [`shadowed`]); with a `filter` the shadow stays in the image-filter chain the filter is in.
    fn looper_shadow(&self) -> bool {
        self.has_shadow() && self.filters.is_empty()
    }
    /// The sampling an image is drawn with, as cc's raster plays a canvas recording back
    /// (`PaintFlags::FilterQualityToSkSamplingOptions`): Blink records a quality, and the
    /// raster maps it with the scaling the DEVICE matrix does -- `upscale` only when it scales up on both axes
    /// (`MatrixToScalingOperation`). Medium is trilinear either way, high is Mitchell's cubic only when
    /// upscaling; an image shader (a pattern) is always mapped as an unknown scaling, so never cubic.
    fn sampling(&self, upscale: bool) -> SamplingOptions {
        if !self.smoothing {
            return SamplingOptions::new(FilterMode::Nearest, MipmapMode::None);
        }
        match self.smoothing_quality {
            SmoothingQuality::Low => SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
            SmoothingQuality::High if upscale => SamplingOptions::from(CubicResampler::mitchell()),
            SmoothingQuality::Medium | SmoothingQuality::High => SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear),
        }
    }
}

/// The interpreter's whole mutable world: the state stack, the current path, and the live
/// gradient/pattern objects the display list has defined.
struct Exec {
    st: State,
    stack: Vec<State>,
    path: PathBuilder,
    /// The current point, in the space the path is being built in (device space) — `arcTo` and the
    /// "line to the start of the arc" rule need one, and `PathBuilder` will not report it.
    current: Option<(f64, f64)>,
    /// The current path as Blink's builders see it (see [`Shape`]).
    shape: Shape,
    gradients: HashMap<u32, Gradient>,
    patterns: HashMap<u32, Pattern>,
    font_mgr: FontMgr,
}

thread_local! {
    /// The font manager every canvas raster on this thread draws text with, made once -- as `text_ink`'s is.
    /// `FontMgr::new()` builds a fontconfig manager from nothing (every config file parsed, the font set built), and
    /// [`Exec::new`] made one per rasterisation: a `toDataURL` of a canvas with text in it cost 5.5-17.6 ms against
    /// Chrome 152's 0.1-0.6 ms (2026-10-22), 186 ms of www.costco.com's main thread up to a bot-detection endpoint's `/tl` -- and a
    /// script that times its canvas read could tell. The typefaces it answers are cached per thread by family and
    /// style already ([`cached_typeface`], [`cached_fallback`]), so one manager changes no glyph.
    static FONT_MGR: FontMgr = FontMgr::new();
}

impl Exec {
    fn new() -> Self {
        Self {
            st: State::default(),
            stack: Vec::new(),
            path: PathBuilder::new(),
            current: None,
            shape: Shape::Empty,
            gradients: HashMap::new(),
            patterns: HashMap::new(),
            font_mgr: FONT_MGR.with(FontMgr::clone),
        }
    }

    // ── paint construction ────────────────────────────────────────────────────────────────────

    /// The shader for a gradient/pattern source. `local` is the matrix taking the source's own
    /// (user-space) coordinates into the space the geometry will be drawn in.
    // skia-safe 0.99 deprecates `gradient_shader` in favour of a `gradient` module that takes the
    // same three gradients through a builder. Same Skia calls underneath; switching is a mechanical
    // change and not this unit's.
    #[allow(deprecated)]
    fn shader(&self, src: &Source, local: &Matrix) -> Option<Shader> {
        match src {
            Source::Color(_) => None,
            Source::Gradient(id) => {
                let g = self.gradients.get(id)?;
                // HTML: a gradient with no stops paints nothing; with one stop it is that colour.
                if g.stops.is_empty() {
                    return None;
                }
                // `Gradient::CreateShaderInternal`: interpolated in OKLab once any stop is not
                // a legacy colour, else in sRGB -- never in the canvas's own space -- and `FillSkiaStops` pads a stop
                // at 0 and at 1 with the end colours ("in practice there are rendering diffs" against Skia's own
                // padding, gradient.cc).
                let oklab = g.stops.iter().any(|s| !s.legacy);
                let col = |s: &Stop| if oklab { s.oklab } else { s.color };
                let (mut pos, mut cols) = (Vec::new(), Vec::new());
                if g.stops[0].pos > 0.0 {
                    pos.push(0.0);
                    cols.push(col(&g.stops[0]));
                }
                for st in &g.stops {
                    pos.push(st.pos);
                    cols.push(col(st));
                }
                if pos.last().is_some_and(|p| *p < 1.0) {
                    pos.push(1.0);
                    cols.push(*cols.last()?);
                }
                use gradient_shader::interpolation::{ColorSpace as Space, HueMethod, InPremul};
                let interp = gradient_shader::Interpolation {
                    in_premul: if g.premul { InPremul::Yes } else { InPremul::No },
                    color_space: if oklab { Space::OKLab } else { Space::SRGB },
                    hue_method: HueMethod::Shorter,
                };
                match g.kind {
                    GradKind::Linear { x0, y0, x1, y1 } => gradient_shader::linear_with_interpolation(
                        (Point::new(x0, y0), Point::new(x1, y1)),
                        (&cols[..], None),
                        &pos[..],
                        TileMode::Clamp,
                        interp,
                        Some(local),
                    ),
                    GradKind::Radial { x0, y0, r0, x1, y1, r1 } => gradient_shader::two_point_conical_with_interpolation(
                        (Point::new(x0, y0), r0),
                        (Point::new(x1, y1), r1),
                        (&cols[..], None),
                        &pos[..],
                        TileMode::Clamp,
                        interp,
                        Some(local),
                    ),
                    GradKind::Conic { start, cx, cy } => {
                        // A conic gradient runs a full turn from `startAngle`. Skia's sweep gradient
                        // starts at 0deg, so the start angle rides in the local matrix.
                        let mut m = *local;
                        m.pre_translate((cx, cy));
                        m.pre_rotate(start.to_degrees(), None);
                        m.pre_translate((-cx, -cy));
                        gradient_shader::sweep_with_interpolation(
                            Point::new(cx, cy),
                            (&cols[..], None),
                            &pos[..],
                            TileMode::Clamp,
                            None,
                            interp,
                            Some(&m),
                        )
                    }
                }
            }
            Source::Pattern(id) => {
                let p = self.patterns.get(id)?;
                let img = p.image.as_ref()?;
                let m = Matrix::concat(local, &mat(&p.transform));
                let (tx, ty) = tiles_of(p.repeat);
                img.to_shader(Some((tx, ty)), self.st.sampling(false), Some(&m))
            }
        }
    }

    /// The image filter for the state's `filter` chain and its shadow, composed the way HTML orders
    /// them: the filter runs on the drawing, the shadow is cast from the filtered result. `shadow` =
    /// the shadow belongs in the chain (an image's; a fill's, a stroke's and text's is [`shadowed`]'s).
    fn effects(&self, shadow: bool) -> Option<ImageFilter> {
        let mut chain: Option<ImageFilter> = None;
        for f in &self.st.filters {
            chain = apply_filter(*f, chain);
        }
        if shadow && self.st.has_shadow() {
            let sigma = (self.st.shadow_blur / 2.0).max(0.0);
            chain = image_filters::drop_shadow(
                Vector::new(self.st.shadow_dx, self.st.shadow_dy),
                (sigma, sigma),
                Color4f::new(self.st.shadow_color[0], self.st.shadow_color[1], self.st.shadow_color[2], self.st.shadow_color[3]),
                None,
                chain,
                None,
            );
        }
        chain
    }

    /// A filled paint for `src`, with the state's alpha, blend mode, filter and shadow applied.
    fn fill_paint(&self, src: &Source, local: &Matrix) -> Paint {
        let mut p = Paint::default();
        p.set_anti_alias(true);
        p.set_style(PaintStyle::Fill);
        p.set_blend_mode(self.st.composite);
        match src {
            // `CanvasStyle::ApplyColorToFlags`: the colour's alpha times `globalAlpha`, then `toSkColor4f` -- an
            // sRGB `SkColor4f` (no colour space) Skia takes into the canvas's space.
            Source::Color(c) => {
                p.set_color4f(Color4f::new(c[0], c[1], c[2], c[3] * self.st.alpha), None);
            }
            _ => match self.shader(src, local) {
                Some(s) => {
                    p.set_shader(s);
                    p.set_alpha_f(self.st.alpha);
                    // Blink dithers a gradient: Chrome 150's 517x389 linear gradient
                    // differs from an undithered one by +-1..2 in a quarter of its channels.
                    if matches!(src, Source::Gradient(_)) {
                        p.set_dither(true);
                    }
                }
                // A gradient with no stops, or a pattern with no pixels: HTML paints nothing. A
                // fully transparent paint is exactly that — and `globalAlpha` must NOT be applied
                // over it, or "nothing" becomes opaque black.
                None => {
                    p.set_color(Color::from_argb(0, 0, 0, 0));
                }
            },
        }
        if let Some(f) = self.effects(!self.st.looper_shadow()) {
            p.set_image_filter(f);
        }
        p
    }

    /// The stroking counterpart: the same paint plus the line state (width, cap, join, dash).
    fn stroke_paint(&self, local: &Matrix) -> Paint {
        let mut p = self.fill_paint(&self.st.stroke, local);
        apply_line_state(&mut p, &self.st);
        p
    }

    /// Append one user-space segment to the current path, mapped through the CTM.
    fn add_seg(&mut self, seg: PathOp) {
        self.shape = self.shape.then(&seg);
        // Under the identity CTM the segment goes straight into the path — the same call the R2
        // recorder made, so the untransformed fingerprint scene is bit-for-bit unchanged.
        if self.st.ctm.is_identity() {
            self.current = append_seg(&mut self.path, seg, self.current);
            return;
        }
        // Otherwise it is built in user space (a rotated ellipse is not an oval once mapped, so it
        // cannot be built point by point) and the whole sub-path is transformed onto the end of the
        // current contour. The current point is DEVICE-space, so it comes back through the inverse
        // first — feeding the device point into a sub-path that is about to be transformed would map
        // it twice, which is a line to somewhere the page never asked for.
        let Some(inv) = self.st.ctm.invert() else { return };
        let user_current = self.current.map(|(x, y)| inv.apply(x, y));
        let mut sub = PathBuilder::new();
        let had_current = user_current.is_some();
        if let Some((cx, cy)) = user_current {
            sub.move_to((cx as f32, cy as f32));
        }
        let end = append_seg(&mut sub, seg, user_current);
        // `Path::with_transform`, not `snapshot_and_transform`: the builder's keeps a circle's "is an oval" through a
        // rotation with its conics' control points for the oval's bounds, and a clip to it is a larger circle (minea's
        // spinning `border-radius: 500px` glow, clipped to its rotated square)
        let built = sub.snapshot().with_transform(&mat(&self.st.ctm));
        // Extend welds the sub-path onto the current contour (Skia turns its leading moveTo into a
        // lineTo). The leading point is the current point, so that line is zero-length and invisible.
        let mode = if had_current { Some(path::AddPathMode::Extend) } else { None };
        self.path.add_path(&built, mode);
        self.current = end.map(|(x, y)| self.st.ctm.apply(x, y));
    }

    /// The current path, in device space.
    fn device_path(&self) -> Path {
        self.path.snapshot()
    }
}

fn apply_line_state(p: &mut Paint, st: &State) {
    p.set_style(PaintStyle::Stroke);
    p.set_stroke_width(st.line_width);
    p.set_stroke_cap(st.cap);
    p.set_stroke_join(st.join);
    p.set_stroke_miter(st.miter);
    if !st.dash.is_empty() {
        if let Some(e) = dash_path_effect::new(&st.dash, st.dash_offset) {
            p.set_path_effect(e);
        }
    }
}

/// Append one segment to `b`, in whatever space `b` is being built in. Returns the new current
/// point, in that same space. One implementation, used by the context's current path, by the
/// transformed sub-path route, and by `Path2D` — so the three cannot drift apart.
fn append_seg(b: &mut PathBuilder, seg: PathOp, current: Option<(f64, f64)>) -> Option<(f64, f64)> {
    match seg {
        PathOp::MoveTo { x, y } => {
            b.move_to((x as f32, y as f32));
            Some((x, y))
        }
        PathOp::LineTo { x, y } => {
            // HTML: `lineTo` with no subpath behaves as `moveTo`.
            if current.is_none() {
                b.move_to((x as f32, y as f32));
            } else {
                b.line_to((x as f32, y as f32));
            }
            Some((x, y))
        }
        PathOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => {
            if current.is_none() {
                b.move_to((cp1x as f32, cp1y as f32));
            }
            b.cubic_to((cp1x as f32, cp1y as f32), (cp2x as f32, cp2y as f32), (x as f32, y as f32));
            Some((x, y))
        }
        PathOp::QuadraticCurveTo { cpx, cpy, x, y } => {
            if current.is_none() {
                b.move_to((cpx as f32, cpy as f32));
            }
            b.quad_to((cpx as f32, cpy as f32), (x as f32, y as f32));
            Some((x, y))
        }
        PathOp::ClosePath => {
            b.close();
            current
        }
        PathOp::Rect { x, y, w, h } => {
            b.add_rect(Rect::from_xywh(x as f32, y as f32, w as f32, h as f32), None, None);
            // HTML leaves the current point at the rectangle's origin.
            Some((x, y))
        }
        // `CanvasPath::arc`: float arguments; an empty arc is the line to its start; the
        // angles canonicalized and adjusted (`CanonicalizeAngle`, `AdjustEndAngle`); `PathBuilder::AddEllipse`.
        PathOp::Arc { x, y, radius, start, end, ccw } => {
            let (x, y, r) = (x as f32, y as f32, radius as f32);
            let (mut s, mut e) = (start as f32, end as f32);
            if r == 0.0 || s == e {
                return append_seg(b, PathOp::LineTo { x: (x + r * s.cos()) as f64, y: (y + r * s.sin()) as f64 }, current);
            }
            canonicalize_angles(&mut s, &mut e);
            let e = adjust_end_angle(s, e, ccw);
            add_ellipse_arcs(b, x, y, r, r, s, e);
            Some(((x + r * e.cos()) as f64, (y + r * e.sin()) as f64))
        }
        PathOp::Ellipse { x, y, rx, ry, rotation, start, end, ccw } => {
            // `CanvasPath::ellipse`: the angles as `arc`'s; a degenerate one is lines (`DegenerateEllipse`).
            let (mut s, mut e) = (start as f32, end as f32);
            canonicalize_angles(&mut s, &mut e);
            let e = adjust_end_angle(s, e, ccw);
            let (frx, fry, rot) = (rx as f32, ry as f32, rotation as f32);
            if frx == 0.0 || fry == 0.0 || s == e {
                let (sr, cr) = (rot as f64).sin_cos();
                let at = |a: f32| -> PathOp {
                    let (px, py) = ((frx * a.cos()) as f64, (fry * a.sin()) as f64);
                    PathOp::LineTo { x: x + px * cr - py * sr, y: y + px * sr + py * cr }
                };
                let mut cur = append_seg(b, at(s), current);
                if (frx == 0.0 && fry == 0.0) || s == e {
                    return cur;
                }
                const QUARTER: f32 = std::f32::consts::FRAC_PI_2;
                if !ccw {
                    let mut a = s - s % QUARTER + QUARTER;
                    while a < e {
                        cur = append_seg(b, at(a), cur);
                        a += QUARTER;
                    }
                } else {
                    let mut a = s - s % QUARTER;
                    while a > e {
                        cur = append_seg(b, at(a), cur);
                        a -= QUARTER;
                    }
                }
                return append_seg(b, at(e), cur);
            }
            // Built as an axis-aligned arc about the origin, then rotated and translated into place:
            // Skia's arc primitives take an oval, and a rotated ellipse is not one.
            let mut arc = PathBuilder::new();
            add_ellipse_arcs(&mut arc, 0.0, 0.0, frx, fry, s, e);
            let mut m = Matrix::default();
            m.set_translate((x as f32, y as f32));
            m.pre_rotate(rotation.to_degrees() as f32, None);
            let placed = arc.snapshot().with_transform(&m);
            let mode = if current.is_some() { Some(path::AddPathMode::Extend) } else { None };
            b.add_path(&placed, mode);
            let (lx, ly) = ((frx * e.cos()) as f64, (fry * e.sin()) as f64);
            let (sr, cr) = rotation.sin_cos();
            Some((x + lx * cr - ly * sr, y + lx * sr + ly * cr))
        }
        PathOp::ArcTo { x1, y1, x2, y2, radius } => {
            // HTML: with no subpath, `arcTo` starts one at (x1, y1).
            if current.is_none() {
                b.move_to((x1 as f32, y1 as f32));
            }
            b.arc_to_tangent((x1 as f32, y1 as f32), (x2 as f32, y2 as f32), radius as f32);
            // Skia computed the tangent point; what the next segment needs is only that there IS a
            // current point, and the builder now has the right one.
            Some((x2, y2))
        }
        PathOp::RoundRect { x, y, w, h, radii } => {
            let rect = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32);
            let pts: [Vector; 4] = [
                Vector::new(radii[0].0 as f32, radii[0].1 as f32),
                Vector::new(radii[1].0 as f32, radii[1].1 as f32),
                Vector::new(radii[2].0 as f32, radii[2].1 as f32),
                Vector::new(radii[3].0 as f32, radii[3].1 as f32),
            ];
            let rr = RRect::new_rect_radii(rect, &pts);
            // HTML: a negative width or height reverses the winding direction.
            let dir = if (w < 0.0) != (h < 0.0) { PathDirection::CCW } else { PathDirection::CW };
            b.add_rrect(rr, Some(dir), None);
            Some((x, y))
        }
    }
}

/// `CanonicalizeAngle`: the start angle into [0, 2pi), the end moved with it.
fn canonicalize_angles(start: &mut f32, end: &mut f32) {
    const TWO_PI: f32 = std::f32::consts::PI * 2.0;
    let mut s = *start % TWO_PI;
    if s < 0.0 {
        s += TWO_PI;
        if s >= TWO_PI {
            s -= TWO_PI;
        }
    }
    let delta = s - *start;
    *start = s;
    *end += delta;
}

/// `AdjustEndAngle`: a sweep of a full turn or more is exactly one; a sweep the wrong way round goes the other.
fn adjust_end_angle(start: f32, end: f32, ccw: bool) -> f32 {
    const TWO_PI: f32 = std::f32::consts::PI * 2.0;
    if !ccw && end - start >= TWO_PI {
        start + TWO_PI
    } else if ccw && start - end >= TWO_PI {
        start - TWO_PI
    } else if !ccw && start > end {
        start + (TWO_PI - (start - end) % TWO_PI)
    } else if ccw && start < end {
        start - (TWO_PI - (end - start) % TWO_PI)
    } else {
        end
    }
}

/// `PathBuilder::AddEllipse` (no rotation): `arcTo` in degrees from the angles in float, a full turn as two half
/// turns (`arcTo` of 360 degrees draws nothing), never `addOval`, which would start a contour of its own.
fn add_ellipse_arcs(b: &mut PathBuilder, cx: f32, cy: f32, rx: f32, ry: f32, start: f32, end: f32) {
    let oval = Rect::from_ltrb(cx - rx, cy - ry, cx + rx, cy + ry);
    let rad2deg = |r: f32| r * (180.0f32 / std::f32::consts::PI);
    let start_deg = rad2deg(start);
    let sweep_deg = rad2deg(end - start);
    if (sweep_deg.abs() - 360.0).abs() <= 1.0 / 4096.0 {
        let half = 180.0f32.copysign(sweep_deg);
        b.arc_to(oval, start_deg, half, false);
        b.arc_to(oval, start_deg + half, half, false);
    } else {
        b.arc_to(oval, start_deg, sweep_deg, false);
    }
}

/// What Blink's `CanvasPath` keeps beside its general path (`canvas_path.h`): a path that is
/// only `moveTo` + `lineTo` is a LINE and is stroked with `drawLine`; one whose first op was an `arc()` of radius 1 or
/// more is an ARC (`CanvasUsesArcPaintOp`, on in Chrome 150) and is drawn with `drawArc` -- or `drawOval` once
/// closed round a full turn (`DrawArcImpl`) -- which splits its curve otherwise than the path `arcTo` builds. Any
/// other op, or a change of transform, turns the path GENERAL. Coordinates are user space, in floats.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    Empty,
    Start(f32, f32),
    Line(f32, f32, f32, f32),
    Arc { x: f32, y: f32, r: f32, start: f32, sweep: f32, closed: bool },
    General,
}

impl Shape {
    /// The shape after one more path op.
    fn then(self, op: &PathOp) -> Shape {
        match *op {
            PathOp::MoveTo { x, y } => match self {
                Shape::Empty => Shape::Start(x as f32, y as f32),
                _ => Shape::General,
            },
            PathOp::LineTo { x, y } => match self {
                Shape::Empty => Shape::Line(x as f32, y as f32, x as f32, y as f32),
                Shape::Start(sx, sy) => Shape::Line(sx, sy, x as f32, y as f32),
                _ => Shape::General,
            },
            PathOp::Arc { x, y, radius, start, end, ccw } => {
                let (x, y, r) = (x as f32, y as f32, radius as f32);
                let (mut s, mut e) = (start as f32, end as f32);
                if r == 0.0 || s == e {
                    return self.then(&PathOp::LineTo { x: (x + r * s.cos()) as f64, y: (y + r * s.sin()) as f64 });
                }
                canonicalize_angles(&mut s, &mut e);
                let e = adjust_end_angle(s, e, ccw);
                if self == Shape::Empty && r >= 1.0 {
                    Shape::Arc { x, y, r, start: s, sweep: e - s, closed: false }
                } else {
                    Shape::General
                }
            }
            PathOp::ClosePath => match self {
                Shape::Empty => Shape::Empty,
                Shape::Arc { x, y, r, start, sweep, .. } => Shape::Arc { x, y, r, start, sweep, closed: true },
                // A zero-length line closed is cleared (`CanvasPath::closePath`).
                Shape::Line(ax, ay, bx, by) if ax == bx && ay == by => Shape::Empty,
                _ => Shape::General,
            },
            _ => Shape::General,
        }
    }

    /// The shape of a whole `Path2D`'s segment list.
    fn of(segs: &[PathOp]) -> Shape {
        segs.iter().fold(Shape::Empty, |s, op| s.then(op))
    }
}

/// Fill or stroke a LINE or ARC shape as Blink draws it (`DrawPathInternal`); `false` = it is neither, and the caller
/// draws the general path. Filling a line draws nothing.
fn draw_shape(canvas: &Canvas, e: &Exec, shape: Shape, paint: &Paint, stroking: bool) -> bool {
    match shape {
        Shape::Line(ax, ay, bx, by) => {
            if stroking {
                in_user_space(canvas, e, |c| {
                    shadowed(c, e, paint, |c, p| {
                        c.draw_line((ax, ay), (bx, by), p);
                    });
                });
            }
            true
        }
        Shape::Arc { x, y, r, start, sweep, closed } => {
            let oval = Rect::from_ltrb(x - r, y - r, x + r, y + r);
            let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
            let start_deg = finite(start * 180.0 / std::f32::consts::PI);
            let sweep_deg = finite(sweep * 180.0 / std::f32::consts::PI);
            in_user_space(canvas, e, |c| {
                shadowed(c, e, paint, |c, p| {
                    if !closed {
                        c.draw_arc(oval, start_deg, sweep_deg, false, p);
                    } else if (sweep_deg.abs() - 360.0).abs() <= 1.0 / 4096.0 {
                        c.draw_oval(oval, p);
                    } else {
                        let mut pb = PathBuilder::new();
                        pb.arc_to(oval, start_deg, sweep_deg, false);
                        pb.close();
                        c.draw_path(&pb.snapshot(), p);
                    }
                });
            });
            true
        }
        _ => false,
    }
}

/// A free-standing `Path2D`, built in user space and mapped by `ctm`.
fn build_path(segs: &[PathOp], ctm: &Transform2D) -> Path {
    let mut b = PathBuilder::new();
    let mut current: Option<(f64, f64)> = None;
    for seg in segs {
        current = append_seg(&mut b, *seg, current);
    }
    if ctm.is_identity() {
        b.snapshot()
    } else {
        b.snapshot().with_transform(&mat(ctm))
    }
}

/// One CSS filter function, as a Skia image filter over `input`: Blink's `FilterEffectBuilder::BuildFilterEffect`.
/// `grayscale` and `sepia` are colour matrices of the Filter Effects coefficients worked in
/// double and stored as floats; `saturate` and `hue-rotate` are `FEColorMatrix`'s own float formulas; `invert`,
/// `opacity`, `brightness` and `contrast` are COMPONENT TRANSFERS -- 256-entry tables per channel, each entry
/// truncated to a byte (`FEComponentTransfer`, Skia's `TableARGB`), which a matrix only approximates: stock
/// Chrome 150's `grayscale(1)` of an LCD-fringed glyph read 231 where the saturate-based matrix read 232.
///
/// Skia's float matrices work on unpremultiplied 0..1 colour, row-major RGBA with the translation in the fifth
/// column; its tables on unpremultiplied bytes.
fn apply_filter(f: CanvasFilter, input: Option<ImageFilter>) -> Option<ImageFilter> {
    let m = |rows: [f32; 20]| Some(color_filters::matrix_row_major(&rows, None));
    // `FEColorMatrix`'s `SaturateMatrix(float s)`, term for term
    let saturate = |s: f32| {
        m([
            0.213f32 + 0.787f32 * s, 0.715f32 - 0.715f32 * s, 0.072f32 - 0.072f32 * s, 0.0, 0.0,
            0.213f32 - 0.213f32 * s, 0.715f32 + 0.285f32 * s, 0.072f32 - 0.072f32 * s, 0.0, 0.0,
            0.213f32 - 0.213f32 * s, 0.715f32 - 0.715f32 * s, 0.072f32 + 0.928f32 * s, 0.0, 0.0,
            0.0, 0.0, 0.0, 1.0, 0.0,
        ])
    };
    // `FEComponentTransfer`'s functions, as the tables `GetValues` fills (identity where none is given)
    let identity: [u8; 256] = std::array::from_fn(|i| i as u8);
    let to_u8 = |v: f64| v.clamp(0.0, 255.0) as u8;
    let table = |values: [f32; 2]| -> [u8; 256] {
        let n = values.len() as u32;
        std::array::from_fn(|i| {
            let c = i as f64 / 255.0;
            let k = (c * (n - 1) as f64) as u32;
            let v1 = values[k as usize] as f64;
            let v2 = values[(k + 1).min(n - 1) as usize] as f64;
            to_u8(255.0 * (v1 + (c * (n - 1) as f64 - k as f64) * (v2 - v1)))
        })
    };
    // `slope * i + 255 * intercept`, in float as Blink's float members make it
    let linear = |slope: f32, intercept: f32| -> [u8; 256] { std::array::from_fn(|i| to_u8((slope * i as f32 + 255.0f32 * intercept) as f64)) };
    let transfer = |r: [u8; 256], g: [u8; 256], b: [u8; 256], a: [u8; 256]| color_filters::table_argb(&a, &r, &g, &b);
    let cf = match f {
        CanvasFilter::Blur { sigma } => {
            return image_filters::blur((sigma as f32, sigma as f32), None, input, None);
        }
        CanvasFilter::DropShadow { dx, dy, sigma, r, g, b, a } => {
            return image_filters::drop_shadow(
                Vector::new(dx as f32, dy as f32),
                (sigma as f32, sigma as f32),
                Color4f::from(Color::from_argb(a, r, g, b)),
                None,
                input,
                None,
            );
        }
        CanvasFilter::Opacity { amount } => transfer(identity, identity, identity, table([0.0, amount as f32])),
        CanvasFilter::Brightness { amount } => {
            let t = linear(amount as f32, 0.0);
            transfer(t, t, t, identity)
        }
        CanvasFilter::Contrast { amount } => {
            let k = amount as f32;
            let t = linear(k, (-0.5 * k as f64 + 0.5) as f32);
            transfer(t, t, t, identity)
        }
        CanvasFilter::Grayscale { amount } => {
            let o = (1.0 - amount).clamp(0.0, 1.0);
            let f = |v: f64| v as f32;
            m([
                f(0.2126 + 0.7874 * o), f(0.7152 - 0.7152 * o), f(0.0722 - 0.0722 * o), 0.0, 0.0,
                f(0.2126 - 0.2126 * o), f(0.7152 + 0.2848 * o), f(0.0722 - 0.0722 * o), 0.0, 0.0,
                f(0.2126 - 0.2126 * o), f(0.7152 - 0.7152 * o), f(0.0722 + 0.9278 * o), 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ])
        }
        CanvasFilter::Saturate { amount } => saturate(amount as f32),
        CanvasFilter::Sepia { amount } => {
            let o = (1.0 - amount).clamp(0.0, 1.0);
            let f = |v: f64| v as f32;
            m([
                f(0.393 + 0.607 * o), f(0.769 - 0.769 * o), f(0.189 - 0.189 * o), 0.0, 0.0,
                f(0.349 - 0.349 * o), f(0.686 + 0.314 * o), f(0.168 - 0.168 * o), 0.0, 0.0,
                f(0.272 - 0.272 * o), f(0.534 - 0.534 * o), f(0.131 + 0.869 * o), 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ])
        }
        CanvasFilter::Invert { amount } => {
            let t = table([amount as f32, (1.0 - amount) as f32]);
            transfer(t, t, t, identity)
        }
        CanvasFilter::HueRotate { degrees } => {
            // `HueRotateMatrix(float hue)`: `Deg2rad` is `d * (pi / 180)` in float, then `cosf` / `sinf`
            let (s, c) = (degrees as f32).to_radians().sin_cos();
            m([
                0.213f32 + c * 0.787f32 - s * 0.213f32, 0.715f32 - c * 0.715f32 - s * 0.715f32, 0.072f32 - c * 0.072f32 + s * 0.928f32, 0.0, 0.0,
                0.213f32 - c * 0.213f32 + s * 0.143f32, 0.715f32 + c * 0.285f32 + s * 0.140f32, 0.072f32 - c * 0.072f32 - s * 0.283f32, 0.0, 0.0,
                0.213f32 - c * 0.213f32 - s * 0.787f32, 0.715f32 - c * 0.715f32 + s * 0.715f32, 0.072f32 + c * 0.928f32 + s * 0.072f32, 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ])
        }
    };
    image_filters::color_filter(cf?, input, None)
}

/// Draw `text` at (x, baseline) with per-character font fallback — Skia's `draw_str` does NO
/// fallback, so a codepoint the primary family lacks (e.g. an emoji) would render as `.notdef`
/// (tofu). Blink falls back (Arial → Apple Color Emoji); we mirror it: consecutive chars the primary
/// covers are drawn as one run (kerning preserved), each uncovered char is drawn with a fallback font
/// found for that codepoint. Each run advances the cursor by its measured width, so following text
/// lands where Blink puts it.
///
/// A `canvas` of `None` MEASURES instead of drawing; either way the return value is the advance,
/// which is what `textAlign` and `maxWidth` need — and taking both from one routine is what keeps
/// the alignment from disagreeing with the glyphs it aligned.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_fallback(
    canvas: Option<&Canvas>,
    font_mgr: &FontMgr,
    family: &str,
    style: FontStyle,
    size: f32,
    text: &str,
    x: f32,
    y: f32,
    paint: Option<&Paint>,
    letter_spacing: f32,
    word_spacing: f32,
) -> f32 {
    let primary = match mk_font(cached_typeface(font_mgr, family, style), size) {
        Some(f) => f,
        None => return 0.0,
    };
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = x;
    let mut i = 0;
    // With no letter/word spacing this stays the original run-at-a-time path, character for
    // character; spacing forces per-character runs because the extra advance goes between them.
    let spaced = letter_spacing != 0.0 || word_spacing != 0.0;
    while i < chars.len() {
        let (run, font) = if primary.unichar_to_glyph(chars[i] as i32) != 0 {
            let start = i;
            if spaced {
                i += 1;
            } else {
                while i < chars.len() && primary.unichar_to_glyph(chars[i] as i32) != 0 {
                    i += 1;
                }
            }
            (chars[start..i].iter().collect::<String>(), primary.clone())
        } else {
            let ch = chars[i];
            i += 1;
            let f = mk_font(cached_fallback(font_mgr, family, style, ch), size)
                .unwrap_or_else(|| primary.clone());
            (ch.to_string(), f)
        };
        if let (Some(c), Some(p)) = (canvas, paint) {
            c.draw_str(&run, (cursor, y), &font, p);
        }
        cursor += font.measure_str(&run, paint).0;
        if spaced {
            cursor += letter_spacing;
            if run == " " {
                cursor += word_spacing;
            }
        }
    }
    cursor - x
}

/// Execute a recorded display list into `canvas`.
pub(crate) fn execute(canvas: &Canvas, ops: &[Canvas2DOp]) {
    let mut e = Exec::new();
    for op in ops {
        step(canvas, &mut e, op);
    }
    // Every `save()` the page never `restore()`d still holds a Skia save level; drop them so the
    // canvas is handed back balanced.
    while e.stack.pop().is_some() {
        canvas.restore();
    }
}

/// One op. Split out so `hit_test` replays the same list without a second interpreter — a
/// path-only re-implementation is exactly the divergence `isPointInPath` must not have.
fn step(canvas: &Canvas, e: &mut Exec, op: &Canvas2DOp) {
    let ident = Matrix::default();
    match op {
        // ── paint + text state ────────────────────────────────────────────────────────────────
        Canvas2DOp::SetFillStyle { color } => e.st.fill = Source::Color(*color),
        Canvas2DOp::SetStrokeStyle { color } => e.st.stroke = Source::Color(*color),
        Canvas2DOp::SetFillGradient { id } => e.st.fill = Source::Gradient(*id),
        Canvas2DOp::SetStrokeGradient { id } => e.st.stroke = Source::Gradient(*id),
        Canvas2DOp::SetFillPattern { id } => e.st.fill = Source::Pattern(*id),
        Canvas2DOp::SetStrokePattern { id } => e.st.stroke = Source::Pattern(*id),
        Canvas2DOp::SetGlobalAlpha { alpha } => e.st.alpha = *alpha as f32,
        Canvas2DOp::SetGlobalCompositeOperation { op } => e.st.composite = composite_to_blend(*op),
        Canvas2DOp::SetLineWidth { width } => e.st.line_width = *width as f32,
        Canvas2DOp::SetLineCap { cap } => e.st.cap = cap_of(*cap),
        Canvas2DOp::SetLineJoin { join } => e.st.join = join_of(*join),
        Canvas2DOp::SetMiterLimit { limit } => e.st.miter = *limit as f32,
        Canvas2DOp::SetLineDash { segments } => {
            // HTML doubles an odd-length dash list so the pattern alternates.
            let mut d: Vec<f32> = segments.iter().map(|v| *v as f32).collect();
            if d.len() % 2 == 1 {
                d.extend_from_within(..);
            }
            // An all-zero list is a solid line, and Skia rejects it outright.
            e.st.dash = if d.iter().all(|v| *v <= 0.0) { Vec::new() } else { d };
        }
        Canvas2DOp::SetLineDashOffset { offset } => e.st.dash_offset = *offset as f32,
        Canvas2DOp::SetShadowColor { color } => e.st.shadow_color = *color,
        Canvas2DOp::SetShadowBlur { blur } => e.st.shadow_blur = *blur as f32,
        Canvas2DOp::SetShadowOffset { x, y } => {
            e.st.shadow_dx = *x as f32;
            e.st.shadow_dy = *y as f32;
        }
        Canvas2DOp::SetFilter { filters } => e.st.filters = filters.clone(),
        Canvas2DOp::SetImageSmoothing { enabled } => e.st.smoothing = *enabled,
        Canvas2DOp::SetImageSmoothingQuality { quality } => e.st.smoothing_quality = *quality,
        Canvas2DOp::SetFont { family, size_px, bold, italic, families, lang, default_family, .. } => {
            e.st.font_family = family.clone();
            e.st.font_size = *size_px as f32;
            e.st.font_bold = *bold;
            e.st.font_italic = *italic;
            e.st.font_families = families.clone();
            e.st.font_lang = lang.clone();
            e.st.font_default = default_family.clone();
        }
        Canvas2DOp::SetTextAlign { align } => e.st.align = *align,
        Canvas2DOp::SetTextBaseline { baseline } => e.st.baseline = *baseline,
        Canvas2DOp::SetTextDirection { dir } => e.st.direction = *dir,
        Canvas2DOp::SetLetterSpacing { px } => e.st.letter_spacing = *px as f32,
        Canvas2DOp::SetWordSpacing { px } => e.st.word_spacing = *px as f32,

        // ── state stack. Skia's own save level carries the clip; ours carries everything else. ──
        Canvas2DOp::Save => {
            e.stack.push(e.st.clone());
            canvas.save();
        }
        Canvas2DOp::SaveLayer { alpha } => {
            e.stack.push(e.st.clone());
            canvas.save_layer_alpha_f(None, alpha.clamp(0.0, 1.0) as f32);
        }
        Canvas2DOp::SaveLayerFilter { filters } => {
            e.stack.push(e.st.clone());
            let mut paint = Paint::default();
            paint.set_image_filter(filters.iter().fold(None, |chain, f| apply_filter(*f, chain)));
            canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().paint(&paint));
        }
        Canvas2DOp::SaveLayerBlend { op } => {
            e.stack.push(e.st.clone());
            let mut paint = Paint::default();
            paint.set_blend_mode(composite_to_blend(*op));
            canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().paint(&paint));
        }
        Canvas2DOp::SaveLayerBackdrop { filters, rect } => {
            e.stack.push(e.st.clone());
            // a blur reads the backdrop past the rect's edge mirrored, as Chrome's backdrop-filter does
            let backdrop = filters.iter().fold(None, |chain, f| match f {
                CanvasFilter::Blur { sigma } => image_filters::blur((*sigma as f32, *sigma as f32), TileMode::Mirror, chain, None),
                f => apply_filter(*f, chain),
            });
            let [x, y, w, h] = *rect;
            let bounds = mat(&e.st.ctm).map_rect(Rect::from_xywh(x as f32, y as f32, w as f32, h as f32)).0;
            match backdrop.as_ref() {
                Some(b) => canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().bounds(&bounds).backdrop(b)),
                None => canvas.save(),
            };
        }
        Canvas2DOp::Restore => {
            if let Some(prev) = e.stack.pop() {
                // `PopStateStack` re-maps the path through either transform that is not the identity.
                if (!e.st.ctm.is_identity() || !prev.ctm.is_identity()) && e.shape != Shape::Empty {
                    e.shape = Shape::General;
                }
                e.st = prev;
                canvas.restore();
            }
        }

        // ── transforms ────────────────────────────────────────────────────────────────────────
        // A transform change re-maps the path, which makes it general (`GetModifiablePath().Transform`).
        Canvas2DOp::Transform { t } => {
            e.st.ctm = e.st.ctm.multiply(t);
            if e.shape != Shape::Empty {
                e.shape = Shape::General;
            }
        }
        Canvas2DOp::SetTransform { t } => {
            e.st.ctm = *t;
            if e.shape != Shape::Empty {
                e.shape = Shape::General;
            }
        }

        // ── path building ─────────────────────────────────────────────────────────────────────
        Canvas2DOp::BeginPath => {
            e.path = PathBuilder::new();
            e.current = None;
            e.shape = Shape::Empty;
        }
        Canvas2DOp::MoveTo { x, y } => e.add_seg(PathOp::MoveTo { x: *x, y: *y }),
        Canvas2DOp::LineTo { x, y } => e.add_seg(PathOp::LineTo { x: *x, y: *y }),
        Canvas2DOp::Rect { x, y, w, h } => e.add_seg(PathOp::Rect { x: *x, y: *y, w: *w, h: *h }),
        Canvas2DOp::ClosePath => e.add_seg(PathOp::ClosePath),
        Canvas2DOp::Arc { x, y, radius, start, end, ccw } => {
            e.add_seg(PathOp::Arc { x: *x, y: *y, radius: *radius, start: *start, end: *end, ccw: *ccw })
        }
        Canvas2DOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => e.add_seg(PathOp::BezierCurveTo {
            cp1x: *cp1x,
            cp1y: *cp1y,
            cp2x: *cp2x,
            cp2y: *cp2y,
            x: *x,
            y: *y,
        }),
        Canvas2DOp::QuadraticCurveTo { cpx, cpy, x, y } => {
            e.add_seg(PathOp::QuadraticCurveTo { cpx: *cpx, cpy: *cpy, x: *x, y: *y })
        }
        Canvas2DOp::ArcTo { x1, y1, x2, y2, radius } => {
            e.add_seg(PathOp::ArcTo { x1: *x1, y1: *y1, x2: *x2, y2: *y2, radius: *radius })
        }
        Canvas2DOp::Ellipse { x, y, rx, ry, rotation, start, end, ccw } => e.add_seg(PathOp::Ellipse {
            x: *x,
            y: *y,
            rx: *rx,
            ry: *ry,
            rotation: *rotation,
            start: *start,
            end: *end,
            ccw: *ccw,
        }),
        Canvas2DOp::RoundRect { x, y, w, h, radii } => {
            e.add_seg(PathOp::RoundRect { x: *x, y: *y, w: *w, h: *h, radii: *radii })
        }

        // ── painting ──────────────────────────────────────────────────────────────────────────
        Canvas2DOp::Fill { even_odd } => {
            if draw_shape(canvas, e, e.shape, &e.fill_paint(&e.st.fill, &ident), false) {
                return;
            }
            let mut p = e.device_path();
            p.set_fill_type(if *even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
            let paint = e.fill_paint(&e.st.fill, &mat(&e.st.ctm));
            shadowed(canvas, e, &paint, |c, pt| {
                c.draw_path(&p, pt);
            });
        }
        Canvas2DOp::FillPath { path, even_odd } => {
            if draw_shape(canvas, e, Shape::of(path), &e.fill_paint(&e.st.fill, &ident), false) {
                return;
            }
            let mut p = build_path(path, &e.st.ctm);
            p.set_fill_type(if *even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
            let paint = e.fill_paint(&e.st.fill, &mat(&e.st.ctm));
            shadowed(canvas, e, &paint, |c, pt| {
                c.draw_path(&p, pt);
            });
        }
        Canvas2DOp::Stroke => {
            if draw_shape(canvas, e, e.shape, &e.stroke_paint(&ident), true) {
                return;
            }
            let device = e.device_path();
            stroke_device_path(canvas, e, &device);
        }
        Canvas2DOp::StrokePath { path } => {
            if draw_shape(canvas, e, Shape::of(path), &e.stroke_paint(&ident), true) {
                return;
            }
            let device = build_path(path, &e.st.ctm);
            stroke_device_path(canvas, e, &device);
        }
        Canvas2DOp::Clip { even_odd } => {
            let mut p = e.device_path();
            p.set_fill_type(if *even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
            canvas.clip_path(&p, ClipOp::Intersect, true);
        }
        Canvas2DOp::ClipPath { path, even_odd } => {
            let mut p = build_path(path, &e.st.ctm);
            p.set_fill_type(if *even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
            canvas.clip_path(&p, ClipOp::Intersect, true);
        }
        Canvas2DOp::FillRect { x, y, w, h } => {
            let paint = e.fill_paint(&e.st.fill, &ident);
            let r = Rect::from_xywh(*x as f32, *y as f32, *w as f32, *h as f32);
            in_user_space(canvas, e, |c| {
                shadowed(c, e, &paint, |c, pt| {
                    c.draw_rect(r, pt);
                });
            });
        }
        Canvas2DOp::StrokeRect { x, y, w, h } => {
            let paint = e.stroke_paint(&ident);
            let r = Rect::from_xywh(*x as f32, *y as f32, *w as f32, *h as f32);
            in_user_space(canvas, e, |c| {
                shadowed(c, e, &paint, |c, pt| {
                    c.draw_rect(r, pt);
                });
            });
        }
        Canvas2DOp::ClearRect { x, y, w, h } => {
            // `clearRect` takes no paint state: no alpha, no shadow, no filter, no blend mode. It
            // does respect the transform and the clip.
            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_blend_mode(BlendMode::Clear);
            let r = Rect::from_xywh(*x as f32, *y as f32, *w as f32, *h as f32);
            in_user_space(canvas, e, |c| {
                c.draw_rect(r, &paint);
            });
        }
        Canvas2DOp::FillText { text, x, y, max_width } => {
            let paint = e.fill_paint(&e.st.fill, &ident);
            shadowed(canvas, e, &paint, |c, pt| draw_text(c, e, text, *x, *y, *max_width, pt));
        }
        Canvas2DOp::StrokeText { text, x, y, max_width } => {
            let paint = e.stroke_paint(&ident);
            shadowed(canvas, e, &paint, |c, pt| draw_text(c, e, text, *x, *y, *max_width, pt));
        }

        // ── gradients + patterns ──────────────────────────────────────────────────────────────
        Canvas2DOp::DefineLinearGradient { id, x0, y0, x1, y1 } => {
            e.gradients.insert(
                *id,
                Gradient {
                    kind: GradKind::Linear { x0: *x0 as f32, y0: *y0 as f32, x1: *x1 as f32, y1: *y1 as f32 },
                    stops: Vec::new(),
                    premul: false,
                },
            );
        }
        Canvas2DOp::DefineRadialGradient { id, x0, y0, r0, x1, y1, r1 } => {
            e.gradients.insert(
                *id,
                Gradient {
                    kind: GradKind::Radial {
                        x0: *x0 as f32,
                        y0: *y0 as f32,
                        r0: *r0 as f32,
                        x1: *x1 as f32,
                        y1: *y1 as f32,
                        r1: *r1 as f32,
                    },
                    stops: Vec::new(),
                    premul: false,
                },
            );
        }
        Canvas2DOp::DefineConicGradient { id, start_angle, x, y } => {
            e.gradients.insert(
                *id,
                Gradient {
                    kind: GradKind::Conic { start: *start_angle as f32, cx: *x as f32, cy: *y as f32 },
                    stops: Vec::new(),
                    premul: false,
                },
            );
        }
        Canvas2DOp::AddColorStop { id, offset, color, oklab, legacy } => {
            if let Some(grad) = e.gradients.get_mut(id) {
                // `Color::ToGradientStopSkColor4f`: the stop's sRGB floats, unmapped.
                let c4 = |c: &spec::Color4f| Color4f::new(c[0], c[1], c[2], c[3]);
                let pos = *offset as f32;
                // Kept sorted by offset; equal offsets keep insertion order, which is what makes a
                // hard colour break work.
                let at = grad.stops.iter().position(|s| s.pos > pos).unwrap_or(grad.stops.len());
                grad.stops.insert(at, Stop { pos, color: c4(color), oklab: c4(oklab), legacy: *legacy });
            }
        }
        Canvas2DOp::PremultiplyGradient { id } => {
            if let Some(grad) = e.gradients.get_mut(id) {
                grad.premul = true;
            }
        }
        Canvas2DOp::DefinePattern { id, image, repeat } => {
            e.patterns.insert(
                *id,
                Pattern { image: image_of(image), repeat: *repeat, transform: Transform2D::IDENTITY },
            );
        }
        Canvas2DOp::DefineTilePattern { id, w, h, ops, repeat } => {
            // drawn once at whole pixels, `w` x `h` CSS px scaled onto them
            let (pw, ph) = (w.ceil().clamp(1.0, 4096.0) as i32, h.ceil().clamp(1.0, 4096.0) as i32);
            let image = surfaces::raster_n32_premul((pw, ph)).map(|mut s| {
                let c = s.canvas();
                c.scale((pw as f32 / *w as f32, ph as f32 / *h as f32));
                execute(c, ops);
                s.image_snapshot()
            });
            e.patterns.insert(*id, Pattern { image, repeat: *repeat, transform: Transform2D::IDENTITY });
        }
        Canvas2DOp::SetPatternTransform { id, t } => {
            if let Some(p) = e.patterns.get_mut(id) {
                p.transform = *t;
            }
        }

        // ── images ────────────────────────────────────────────────────────────────────────────
        Canvas2DOp::DrawImage { image, sx, sy, sw, sh, dx, dy, dw, dh } => {
            if let Some(img) = image_of(image) {
                let src = Rect::from_xywh(*sx as f32, *sy as f32, *sw as f32, *sh as f32);
                let dst = Rect::from_xywh(*dx as f32, *dy as f32, *dw as f32, *dh as f32);
                let mut paint = Paint::default();
                paint.set_anti_alias(image_antialiased(&e.st.ctm, *dw, *dh));
                paint.set_blend_mode(e.st.composite);
                paint.set_alpha_f(e.st.alpha);
                if let Some(f) = e.effects(true) {
                    paint.set_image_filter(f);
                }
                in_user_space(canvas, e, |c| {
                    // cc's `MatrixToScalingOperation`: the device matrix the raster draws through, source to
                    // destination, scales up only when both of its axes grow
                    let upscale = Matrix::rect_2_rect(src, dst, None)
                        .map(|r| Matrix::concat(&c.local_to_device_as_3x3(), &r))
                        .and_then(|m| m.decompose_scale(None))
                        .is_some_and(|s| s.width > 1.0 && s.height > 1.0);
                    c.draw_image_rect_with_sampling_options(
                        &img,
                        Some((&src, skia_safe::canvas::SrcRectConstraint::Fast)),
                        dst,
                        e.st.sampling(upscale),
                        &paint,
                    );
                });
            }
        }
        Canvas2DOp::PutImageData { image, dx, dy, dirty_x, dirty_y, dirty_w, dirty_h } => {
            put_image_data(canvas, image, *dx, *dy, *dirty_x, *dirty_y, *dirty_w, *dirty_h);
        }
    }
}

/// Blink's `ShouldDrawImageAntialiased`: an image is drawn anti-aliased only under a rotation
/// or a skew, or when its destination is less than one device pixel wide or high. Skia samples an anti-aliased
/// image draw through another path: stock Chrome 150's 80x24 -> 4x4 `drawImage` differed in 3 of 16 pixels.
fn image_antialiased(ctm: &Transform2D, dw: f64, dh: f64) -> bool {
    let m = mat(ctm);
    if !m.rect_stays_rect() {
        return true;
    }
    let (wx, hx) = if m.skew_x() != 0.0 || m.skew_y() != 0.0 { (m.skew_y(), m.skew_x()) } else { (m.scale_x(), m.scale_y()) };
    (dw as f32).abs() * wx.abs() < 1.0 || (dh as f32).abs() * hx.abs() < 1.0
}

/// `putImageData` writes PIXELS: no transform, no clip, no globalAlpha, no composite mode, no
/// shadow — the source rectangle replaces the destination outright. `write_pixels` is the Skia call
/// that means exactly that (it goes to the base device, ignoring both matrix and clip); drawing an
/// image would have inherited all five.
#[allow(clippy::too_many_arguments)]
fn put_image_data(canvas: &Canvas, image: &Rgba8, dx: f64, dy: f64, dirty_x: f64, dirty_y: f64, dirty_w: f64, dirty_h: f64) {
    let (iw, ih) = (image.width as i64, image.height as i64);
    // The dirty rectangle, clamped to the source. HTML normalises negative extents before this.
    let x0 = dirty_x.floor().max(0.0) as i64;
    let y0 = dirty_y.floor().max(0.0) as i64;
    let x1 = ((dirty_x + dirty_w).ceil() as i64).min(iw);
    let y1 = ((dirty_y + dirty_h).ceil() as i64).min(ih);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut rows = vec![0u8; w * h * 4];
    for row in 0..h {
        let src = (((y0 as usize + row) * iw as usize) + x0 as usize) * 4;
        let dst = row * w * 4;
        if src + w * 4 <= image.data.len() {
            rows[dst..dst + w * 4].copy_from_slice(&image.data[src..src + w * 4]);
        }
    }
    let info = ImageInfo::new((w as i32, h as i32), ColorType::RGBA8888, AlphaType::Unpremul, None);
    let at = IPoint::new((dx.round() as i64 + x0) as i32, (dy.round() as i64 + y0) as i32);
    let _ = canvas.write_pixels(&info, &rows, w * 4, at);
}

/// Run `f` with the canvas matrix set to the current CTM, then put it back.
fn in_user_space(canvas: &Canvas, e: &Exec, f: impl FnOnce(&Canvas)) {
    if e.st.ctm.is_identity() {
        f(canvas);
        return;
    }
    let saved = canvas.save();
    canvas.concat(&mat(&e.st.ctm));
    f(canvas);
    canvas.restore_to_count(saved);
}

/// Stroke a DEVICE-space path with the current line state. See the module comment for why the path
/// goes back to user space first.
fn stroke_device_path(canvas: &Canvas, e: &Exec, device: &Path) {
    let ident = Matrix::default();
    if e.st.ctm.is_identity() {
        let paint = e.stroke_paint(&ident);
        shadowed(canvas, e, &paint, |c, pt| {
            c.draw_path(device, pt);
        });
        return;
    }
    let Some(inv) = e.st.ctm.invert() else { return };
    let user = device.with_transform(&mat(&inv));
    let paint = e.stroke_paint(&ident);
    let saved = canvas.save();
    canvas.concat(&mat(&e.st.ctm));
    shadowed(canvas, e, &paint, |c, pt| {
        c.draw_path(&user, pt);
    });
    canvas.restore_to_count(saved);
}

/// Blink's shadow for a fill, a stroke or text: cc's draw looper
/// (`ShadowAndForegroundDrawLooper`) draws the same thing first under a copy of the paint recoloured to the
/// shadow colour (`SkColorFilters::Blend(color, sRGB, kSrcIn)`) and blurred by a mask filter that ignores the
/// transform (`kShadowIgnoresTransforms`: sigma -- half of `shadowBlur` -- and the offset are DEVICE pixels,
/// whatever `scale()` says), then draws it as it is. Measured, a bot challenge's own scene in software
/// Chrome 150: under `scale(0.384)` a `shadowBlur` of 11 spreads 5.5 device pixels; the drop-shadow image filter
/// this used, which Skia scales with the transform, spread 2.1.
fn shadowed(canvas: &Canvas, e: &Exec, paint: &Paint, draw: impl Fn(&Canvas, &Paint)) {
    if e.st.looper_shadow() {
        let mut sp = paint.clone();
        let sigma = e.st.shadow_blur * 0.5;
        if sigma > 0.0 {
            sp.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, sigma, false));
        }
        let c = e.st.shadow_color;
        sp.set_color_filter(color_filters::blend_with_color_space(
            Color4f::new(c[0], c[1], c[2], c[3]),
            skia_safe::ColorSpace::new_srgb(),
            BlendMode::SrcIn,
        ));
        let saved = canvas.save();
        let mut m = canvas.local_to_device();
        m.post_translate(e.st.shadow_dx, e.st.shadow_dy, None);
        canvas.set_matrix(&m);
        draw(canvas, &sp);
        canvas.restore_to_count(saved);
    }
    draw(canvas, paint);
}

/// [`draw_text`] through [`crate::text::shape`]: Blink's `textAlign` shift by the shaped width, its
/// `textBaseline` offset (`GetFontBaseline`), `maxWidth`'s horizontal squeeze, then each glyph run at
/// its own positions in its own font. `false` = no font resolved; the caller draws the old way.
fn draw_shaped(canvas: &Canvas, e: &Exec, text: &str, x: f64, y: f64, max_width: Option<f64>, paint: &Paint) -> bool {
    let families = if e.st.font_families.is_empty() { e.st.font_family.clone() } else { e.st.font_families.clone() };
    let req = spec::TextFontRequest {
        families,
        default_family: if e.st.font_default.is_empty() { "sans-serif".into() } else { e.st.font_default.clone() },
        size: e.st.font_size,
        bold: e.st.font_bold,
        italic: e.st.font_italic,
        lang: e.st.font_lang.clone(),
        words: true,
    };
    let Some(shaped) = crate::text::shape(&e.font_mgr, &req, text) else { return false };
    let width = shaped.width;
    let rtl = matches!(e.st.direction, TextDirection::Rtl);
    let dx = match e.st.align {
        TextAlign::Right => -width,
        TextAlign::Center => -width / 2.0,
        TextAlign::Start if rtl => -width,
        TextAlign::End if !rtl => -width,
        _ => 0.0,
    };
    let dy = crate::text::baseline_offset(&shaped, e.st.baseline);
    let squeeze = match max_width {
        Some(m) if !m.is_finite() => 1.0,
        Some(m) if m <= 0.0 => return true,
        Some(m) if width > m as f32 && width > 0.0 => m as f32 / width,
        _ => 1.0,
    };
    let anchor_x = x as f32 + dx;
    let baseline_y = y as f32 + dy;
    in_user_space(canvas, e, |c| {
        let saved = c.save();
        if squeeze != 1.0 {
            c.translate((anchor_x, 0.0));
            c.scale((squeeze, 1.0));
            c.translate((-anchor_x, 0.0));
        }
        for run in &shaped.runs {
            // a filled run's glyphs as Chrome 150 on Linux makes them (skrifa outlines, Skia's own masks)
            if crate::fontations::fill_run(c, run, shaped.size, (anchor_x, baseline_y), paint) {
                continue;
            }
            let font = crate::fontations::surface_font(c, run, shaped.size, paint);
            c.draw_glyphs_at(&run.glyphs, run.points.as_slice(), (anchor_x, baseline_y), &font, paint);
        }
        c.restore_to_count(saved);
    });
    true
}

/// Lay out and draw one text run: `textAlign` shifts it by its own advance, `textBaseline` shifts it
/// by the font's metrics, and `maxWidth` condenses it horizontally when it does not fit.
fn draw_text(canvas: &Canvas, e: &Exec, text: &str, x: f64, y: f64, max_width: Option<f64>, paint: &Paint) {
    // A context that told us its family list and language draws text shaped as Chrome shapes it --
    // the glyphs, fonts and positions `measureText` measures (`text::shape`) -- unless letter or word
    // spacing asks for per-character placement, which the older path below does.
    if !e.st.font_lang.is_empty() && e.st.letter_spacing == 0.0 && e.st.word_spacing == 0.0 {
        if draw_shaped(canvas, e, text, x, y, max_width, paint) {
            return;
        }
    }
    let style = e.st.font_style();
    let dx = match e.st.align {
        TextAlign::Left => 0.0,
        // Only the shifted alignments need the advance, so the default path never measures.
        _ => {
            let width = draw_text_fallback(
                None,
                &e.font_mgr,
                &e.st.font_family,
                style,
                e.st.font_size,
                text,
                0.0,
                0.0,
                None,
                e.st.letter_spacing,
                e.st.word_spacing,
            );
            let rtl = matches!(e.st.direction, TextDirection::Rtl);
            match e.st.align {
                TextAlign::Right => -width,
                TextAlign::Center => -width / 2.0,
                TextAlign::Start if rtl => -width,
                TextAlign::End if !rtl => -width,
                _ => 0.0,
            }
        }
    };
    let dy = if matches!(e.st.baseline, TextBaseline::Alphabetic) {
        0.0
    } else {
        // Skia's ascent is negative (above the baseline) and its descent positive.
        let metrics = mk_font(
            cached_typeface(&e.font_mgr, e.st.font_family.as_str(), style),
            e.st.font_size,
        )
        .map(|f| f.metrics().1);
        let (ascent, descent) = metrics.map(|m| (m.ascent, m.descent)).unwrap_or((0.0, 0.0));
        match e.st.baseline {
            TextBaseline::Top => -ascent,
            // The hanging baseline sits 80% of the ascent above the alphabetic one.
            TextBaseline::Hanging => -ascent * 0.8,
            TextBaseline::Middle => -(ascent + descent) / 2.0,
            TextBaseline::Ideographic | TextBaseline::Bottom => -descent,
            TextBaseline::Alphabetic => 0.0,
        }
    };
    let squeeze = match max_width {
        Some(m) if !m.is_finite() => 1.0,
        Some(m) if m <= 0.0 => return, // maxWidth 0 (or negative): nothing is drawn
        Some(m) => {
            let width = draw_text_fallback(
                None,
                &e.font_mgr,
                &e.st.font_family,
                style,
                e.st.font_size,
                text,
                0.0,
                0.0,
                None,
                e.st.letter_spacing,
                e.st.word_spacing,
            );
            if width > m as f32 && width > 0.0 {
                (m as f32) / width
            } else {
                1.0
            }
        }
        None => 1.0,
    };
    let anchor_x = x as f32 + dx;
    let baseline_y = y as f32 + dy;
    in_user_space(canvas, e, |c| {
        let saved = c.save();
        if squeeze != 1.0 {
            // Condense about the anchor, so the alignment survives the squeeze.
            c.translate((anchor_x, 0.0));
            c.scale((squeeze, 1.0));
            c.translate((-anchor_x, 0.0));
        }
        draw_text_fallback(
            Some(c),
            &e.font_mgr,
            &e.st.font_family,
            style,
            e.st.font_size,
            text,
            anchor_x,
            baseline_y,
            Some(paint),
            e.st.letter_spacing,
            e.st.word_spacing,
        );
        c.restore_to_count(saved);
    });
}

/// `isPointInPath` / `isPointInStroke`: replay the list, then ask the geometry the rasterizer would
/// have drawn. The point is in canvas space, untouched by the current transform (HTML says so
/// explicitly), and the path is already in canvas space, so this is a straight containment test.
pub(crate) fn hit_test(ops: &[Canvas2DOp], x: f64, y: f64, what: &HitTest) -> bool {
    // A throwaway 1x1 surface: the ops go through the same `step` the rasterizer uses, and `step`
    // draws. Nothing reads these pixels — the point is that there is no second interpreter.
    let Some(mut surface) = surfaces::raster_n32_premul((1, 1)) else { return false };
    let mut e = Exec::new();
    {
        let canvas = surface.canvas();
        for op in ops {
            step(canvas, &mut e, op);
        }
    }
    let (path, even_odd, stroke) = match what {
        HitTest::InPath { even_odd } => (e.device_path(), *even_odd, false),
        HitTest::InStroke => (e.device_path(), false, true),
        HitTest::InExplicitPath { path, even_odd } => (build_path(path, &e.st.ctm), *even_odd, false),
        HitTest::InExplicitStroke { path } => (build_path(path, &e.st.ctm), false, true),
    };
    if !stroke {
        let mut p = path;
        p.set_fill_type(if even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
        return p.contains((x as f32, y as f32));
    }
    // The stroked outline, taken in user space (where `lineWidth` lives) and mapped back.
    let ctm = e.st.ctm;
    let user = if ctm.is_identity() {
        path
    } else {
        match ctm.invert() {
            Some(inv) => path.with_transform(&mat(&inv)),
            None => return false,
        }
    };
    let mut paint = Paint::default();
    apply_line_state(&mut paint, &e.st);
    let mut outline = PathBuilder::new();
    if !path_utils::fill_path_with_paint(&user, &paint, &mut outline, None, None) {
        return false;
    }
    let outline = outline.snapshot();
    let outline = if ctm.is_identity() { outline } else { outline.with_transform(&mat(&ctm)) };
    outline.contains((x as f32, y as f32))
}

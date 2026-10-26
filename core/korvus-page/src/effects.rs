//! What a box's style does to how it is painted and hit beyond where layout put it: CSS `transform` (with
//! `translate`, `rotate`, `scale` and `transform-origin`) and `opacity` (D77).
//!
//! Neither moves a box -- layout ignores both, and a style write that changes only them relays nothing out
//! (`Dom::inert_style_writes`) -- so they are read here, from the computed style, once per style change
//! ([`Page::box_effects`], keyed by the layout fingerprint), and kept beside the laid-out tree
//! (`BoxTree::set_effects`). The paint walk, hit testing and the client rects (`getBoundingClientRect`,
//! `getClientRects`, DevTools' quads) all read the same answer, so what a page shows, what a click lands on and what
//! script measures agree -- as they do in Chrome.
//!
//! A transform is kept in the box's own coordinates (the origin at its border box's top-left corner), so the same
//! matrix holds wherever scrolling takes the box; `BoxTree` places it.

use crate::style::media::MediaContext;
use crate::style::property::PropertyId;
use crate::{LaidOut, Page};
use korvus_dom::{Dom, NodeId};

/// A 2D affine map: `x' = a·x + c·y + e`, `y' = b·x + d·y + f` -- the canvas's and CSS's `matrix(a, b, c, d, e, f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine { e: x, f: y, ..Affine::IDENTITY }
    }

    pub fn scale(x: f64, y: f64) -> Affine {
        Affine { a: x, d: y, ..Affine::IDENTITY }
    }

    pub fn rotate(rad: f64) -> Affine {
        let (s, c) = rad.sin_cos();
        Affine { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    pub fn skew(ax: f64, ay: f64) -> Affine {
        Affine { a: 1.0, b: ay.tan(), c: ax.tan(), d: 1.0, e: 0.0, f: 0.0 }
    }

    /// `self · other`: `other` applied first, then `self` (CSS's left-to-right list multiplies this way).
    pub fn then(&self, other: &Affine) -> Affine {
        Affine {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// The inverse, or `None` for a singular map (`scale(0)`, which makes the box unhittable).
    pub fn invert(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine { a, b, c, d, e: -(a * self.e + c * self.f), f: -(b * self.e + d * self.f) })
    }

    pub fn is_identity(&self) -> bool {
        *self == Affine::IDENTITY
    }

    /// The same map in a frame whose origin sits at `(x, y)` of this one: `T(x,y) · self · T(-x,-y)`.
    pub fn about(&self, x: f64, y: f64) -> Affine {
        Affine::translate(x, y).then(self).then(&Affine::translate(-x, -y))
    }
}

/// Every box's transform and opacity, by box index.
#[derive(Clone, Debug, Default)]
pub struct BoxEffects {
    /// The box's own transform in its own coordinates (origin at its border box's top-left), `None` for none.
    pub local: Vec<Option<Affine>>,
    /// The box's own `opacity`, 1 when it has none.
    pub opacity: Vec<f32>,
    /// Does any box transform?
    pub any_transform: bool,
    /// Does any box fade?
    pub any_opacity: bool,
    /// The rectangle a box and what it holds are clipped to, in its own coordinates (x, y, width, height): an
    /// absolutely positioned box's `clip: rect()`. `None` for none.
    pub clip: Vec<Option<[f32; 4]>>,
    /// The shape a box and what it holds are clipped to by its `clip-path`, in its own coordinates; `None` for none
    /// (and for a `url()`, which names an SVG `<clipPath>`).
    pub clip_path: Vec<Option<ClipShape>>,
    /// The box's `filter` -- what it and everything it paints are drawn through -- and its `backdrop-filter`, each a
    /// chain of filter functions; empty for `none`. (Default-empty vectors are no box's: see [`BoxEffects::filter`].)
    pub filter: Vec<Vec<spec::CanvasFilter>>,
    pub backdrop: Vec<Vec<spec::CanvasFilter>>,
    /// The box's `mix-blend-mode` -- how it and everything it paints composite onto what is beneath -- `None` for
    /// `normal`.
    pub blend: Vec<Option<spec::CompositeOp>>,
    /// Does the box have a `mask-image` (it and everything it paints drawn through it)?
    pub mask: Vec<bool>,
}

impl BoxEffects {
    /// Box `index`'s `filter` chain, `None` for none.
    pub fn filter(&self, index: u32) -> Option<&[spec::CanvasFilter]> {
        self.filter.get(index as usize).filter(|f| !f.is_empty()).map(|f| f.as_slice())
    }

    /// Box `index`'s `backdrop-filter` chain, `None` for none.
    pub fn backdrop(&self, index: u32) -> Option<&[spec::CanvasFilter]> {
        self.backdrop.get(index as usize).filter(|f| !f.is_empty()).map(|f| f.as_slice())
    }

    /// Is box `index` masked?
    pub fn masked(&self, index: u32) -> bool {
        self.mask.get(index as usize).copied().unwrap_or(false)
    }

    /// Box `index`'s `mix-blend-mode`, `None` for `normal`.
    pub fn blend(&self, index: u32) -> Option<spec::CompositeOp> {
        self.blend.get(index as usize).copied().flatten()
    }

    pub fn local(&self, index: u32) -> Option<Affine> {
        self.local.get(index as usize).copied().flatten()
    }

    pub fn opacity(&self, index: u32) -> f32 {
        self.opacity.get(index as usize).copied().unwrap_or(1.0)
    }

    pub fn clip(&self, index: u32) -> Option<[f32; 4]> {
        self.clip.get(index as usize).copied().flatten()
    }

    pub fn clip_path(&self, index: u32) -> Option<&ClipShape> {
        self.clip_path.get(index as usize).and_then(Option::as_ref)
    }
}

impl Page {
    /// Every box's [`BoxEffects`]: its element's computed `transform`, `translate`, `rotate`, `scale`,
    /// `transform-origin` and `opacity`, resolved against its border box -- or its `::before`'s or `::after`'s, for
    /// the box one generated. A box that is neither (a text run, an anonymous box) has none of its own: it moves and
    /// fades with its ancestors. A pseudo-element's own `opacity` was lost, so the `opacity: 0` state layer under
    /// Google's sign-in button (a white `::before` over it) painted the button white.
    pub fn box_effects(&self, dom: &Dom, media: &MediaContext, laid: &LaidOut) -> BoxEffects {
        let tree = &laid.tree;
        let n = tree.len();
        let mut fx = BoxEffects {
            local: vec![None; n],
            opacity: vec![1.0; n],
            any_transform: false,
            any_opacity: false,
            clip: vec![None; n],
            clip_path: vec![None; n],
            filter: vec![Vec::new(); n],
            backdrop: vec![Vec::new(); n],
            blend: vec![None; n],
            mask: vec![false; n],
        };
        let pseudo: std::collections::HashMap<u32, &'static str> =
            tree.pseudo_box_list().into_iter().map(|(_, after, index)| (index, if after { "after" } else { "before" })).collect();
        for index in 0..n as u32 {
            let node = tree.node_of(index);
            let generated = pseudo.get(&index).copied();
            if dom.node_type(node) != 1 || (tree.box_of(node) != Some(index) && generated.is_none()) {
                continue;
            }
            // an element's own box whose style has none of what follows ([`Page::styles_box_effects`]); a pseudo-element's
            // box reads its own style below
            if generated.is_none() && !self.styles_box_effects(dom, node, media) {
                continue;
            }
            let cv = |p| match generated {
                Some(name) => self.computed_pseudo_value(dom, node, name, media, p, Some(laid)),
                None => self.computed_value(dom, node, media, p, Some(laid)),
            };
            if let Some(o) = cv(PropertyId::Opacity).and_then(|s| parse_number_or_percent(&s)) {
                let o = o.clamp(0.0, 1.0) as f32;
                if o < 1.0 {
                    fx.opacity[index as usize] = o;
                    fx.any_opacity = true;
                }
            }
            // `clip: rect()` on an absolutely positioned box (the "visually hidden" of skip links: Google's
            // `clip: rect(1px,1px,1px,1px)`, which painted in full over the page's logo), and `clip-path`'s shape
            {
                let b = tree.border_box(index);
                let (w, h) = (b.width as f64, b.height as f64);
                let ctx = Lengths { w, h, font: 16.0, vw: media.viewport_width as f64, vh: media.viewport_height as f64 };
                let abs = matches!(tree.css_position(index), crate::layout::style::CssPosition::Absolute | crate::layout::style::CssPosition::Fixed);
                fx.clip[index as usize] = cv(PropertyId::Clip).filter(|_| abs).and_then(|c| clip_rect(&c, &ctx));
                if let Some(path) = cv(PropertyId::ClipPath).filter(|c| !matches!(c.trim(), "" | "none")) {
                    // the reference box a keyword names, in the border box's coordinates
                    let px = |p| cv(p).and_then(|v| length(&v, 0.0, &ctx)).unwrap_or(0.0);
                    let sides = |t, r, b, l| [px(t), px(r), px(b), px(l)];
                    let border = sides(PropertyId::BorderTopWidth, PropertyId::BorderRightWidth, PropertyId::BorderBottomWidth, PropertyId::BorderLeftWidth);
                    let reference = |keyword: &str| -> (f64, f64, f64, f64) {
                        let inset = |e: [f64; 4]| (e[3], e[0], (w - e[1] - e[3]).max(0.0), (h - e[0] - e[2]).max(0.0));
                        match keyword {
                            "padding-box" => inset(border),
                            "content-box" => {
                                let pad = sides(PropertyId::PaddingTop, PropertyId::PaddingRight, PropertyId::PaddingBottom, PropertyId::PaddingLeft);
                                inset([border[0] + pad[0], border[1] + pad[1], border[2] + pad[2], border[3] + pad[3]])
                            }
                            "margin-box" => {
                                let m = sides(PropertyId::MarginTop, PropertyId::MarginRight, PropertyId::MarginBottom, PropertyId::MarginLeft);
                                inset([-m[0], -m[1], -m[2], -m[3]])
                            }
                            _ => (0.0, 0.0, w, h),
                        }
                    };
                    fx.clip_path[index as usize] = clip_shape(&path, &ctx, &reference);
                }
            }
            // `filter` and `backdrop-filter` -- a non-replaced inline box takes neither (CSS Filter Effects 1 §1)
            if tree.box_level(index) != crate::layout::style::BoxLevel::Inline {
                fx.filter[index as usize] = cv(PropertyId::Filter).map(|f| parse_filters(&f)).unwrap_or_default();
                fx.backdrop[index as usize] = cv(PropertyId::BackdropFilter).map(|f| parse_filters(&f)).unwrap_or_default();
                fx.mask[index as usize] = cv(PropertyId::MaskImage).is_some_and(|m| !matches!(m.trim(), "" | "none"));
            }
            fx.blend[index as usize] = cv(PropertyId::MixBlendMode).and_then(|m| blend_mode(&m));
            let source = TransformSource::read(|p| cv(p));
            if source.is_none() {
                continue;
            }
            // A non-replaced inline box is not transformable (CSS Transforms 1 §2).
            if tree.box_level(index) == crate::layout::style::BoxLevel::Inline {
                continue;
            }
            let b = tree.border_box(index);
            let Some(m) = source.local((b.width as f64, b.height as f64), (media.viewport_width as f64, media.viewport_height as f64)) else { continue };
            fx.local[index as usize] = Some(m);
            fx.any_transform = true;
        }
        fx
    }
}

/// A computed `mix-blend-mode` as the backend's composite mode; `None` for `normal` (and what is not a mode).
pub(crate) fn blend_mode(value: &str) -> Option<spec::CompositeOp> {
    use spec::CompositeOp as C;
    Some(match value.trim() {
        "multiply" => C::Multiply,
        "screen" => C::Screen,
        "overlay" => C::Overlay,
        "darken" => C::Darken,
        "lighten" => C::Lighten,
        "color-dodge" => C::ColorDodge,
        "color-burn" => C::ColorBurn,
        "hard-light" => C::HardLight,
        "soft-light" => C::SoftLight,
        "difference" => C::Difference,
        "exclusion" => C::Exclusion,
        "hue" => C::Hue,
        "saturation" => C::Saturation,
        "color" => C::Color,
        "luminosity" => C::Luminosity,
        "plus-lighter" => C::Lighter,
        _ => return None,
    })
}

/// A computed `filter` (or `backdrop-filter`) as the chain the backend draws through, its functions in order: the
/// lengths in px, a `blur()`'s radius the Gaussian's standard deviation (Blink's `FEGaussianBlur`), a
/// `drop-shadow()` as Chrome serializes it (colour first). Empty for `none`, a `url()` reference, or anything it
/// cannot read -- which then draws unfiltered, as before.
pub(crate) fn parse_filters(value: &str) -> Vec<spec::CanvasFilter> {
    use spec::CanvasFilter as F;
    let v = value.trim();
    if v.is_empty() || v == "none" {
        return Vec::new();
    }
    let px = |s: &str| -> Option<f64> { s.trim().strip_suffix("px").unwrap_or(s.trim()).trim().parse::<f64>().ok() };
    let amount = |s: &str, default: f64| -> Option<f64> {
        let s = s.trim();
        if s.is_empty() {
            return Some(default);
        }
        match s.strip_suffix('%') {
            Some(p) => p.trim().parse::<f64>().ok().map(|n| n / 100.0),
            None => s.parse::<f64>().ok(),
        }
    };
    let mut out = Vec::new();
    let mut rest = v;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        // the argument runs to the matching parenthesis: a drop-shadow's colour has its own
        let mut depth = 0;
        let mut close = None;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { return Vec::new() };
        let arg = &rest[open + 1..close];
        rest = &rest[close + 1..];
        let f = match name.as_str() {
            "blur" => F::Blur { sigma: px(arg).unwrap_or(0.0) },
            "brightness" => F::Brightness { amount: amount(arg, 1.0).unwrap_or(1.0) },
            "contrast" => F::Contrast { amount: amount(arg, 1.0).unwrap_or(1.0) },
            "grayscale" => F::Grayscale { amount: amount(arg, 1.0).unwrap_or(1.0).min(1.0) },
            "invert" => F::Invert { amount: amount(arg, 1.0).unwrap_or(1.0).min(1.0) },
            "opacity" => F::Opacity { amount: amount(arg, 1.0).unwrap_or(1.0).min(1.0) },
            "saturate" => F::Saturate { amount: amount(arg, 1.0).unwrap_or(1.0) },
            "sepia" => F::Sepia { amount: amount(arg, 1.0).unwrap_or(1.0).min(1.0) },
            "hue-rotate" => {
                let a = arg.trim();
                let deg = if let Some(n) = a.strip_suffix("deg") {
                    n.trim().parse::<f64>().ok()
                } else if let Some(n) = a.strip_suffix("rad") {
                    n.trim().parse::<f64>().ok().map(f64::to_degrees)
                } else if let Some(n) = a.strip_suffix("turn") {
                    n.trim().parse::<f64>().ok().map(|t| t * 360.0)
                } else if let Some(n) = a.strip_suffix("grad") {
                    n.trim().parse::<f64>().ok().map(|g| g * 0.9)
                } else {
                    a.parse::<f64>().ok().or(Some(0.0))
                };
                F::HueRotate { degrees: deg.unwrap_or(0.0) }
            }
            "drop-shadow" => {
                // `<color>? <x> <y> <blur>?`, the colour first or last, its own parentheses kept whole
                let (color, lengths) = match arg.find(')') {
                    Some(end) if arg.trim_start().contains('(') => {
                        let start = arg.find(|c: char| !c.is_whitespace()).unwrap_or(0);
                        (&arg[start..=end], &arg[end + 1..])
                    }
                    _ => ("", arg),
                };
                let mut words: Vec<&str> = lengths.split_whitespace().collect();
                let mut color = color.to_string();
                if color.is_empty() {
                    if let Some(i) = words.iter().position(|w| px(w).is_none()) {
                        color = words.remove(i).to_string();
                    }
                }
                let n = |i: usize| words.get(i).and_then(|w| px(w)).unwrap_or(0.0);
                let c = crate::paint::parse_color_rgba(&color).unwrap_or((0, 0, 0, 255));
                F::DropShadow { dx: n(0), dy: n(1), sigma: n(2), r: c.0, g: c.1, b: c.2, a: c.3 }
            }
            _ => return Vec::new(),
        };
        out.push(f);
    }
    out
}

/// A style's computed `transform`, `translate`, `rotate`, `scale` and `transform-origin`: what makes a box's own
/// transform once its size is known -- here for the paint ([`Page::box_effects`]) and in layout, whose scrollable
/// overflow takes a transformed box's transformed rect (`BoxTree::overflow_of`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransformSource {
    transform: Box<str>,
    translate: Box<str>,
    rotate: Box<str>,
    scale: Box<str>,
    origin: Box<str>,
}

impl TransformSource {
    /// Read from a style's computed values.
    pub(crate) fn read(cv: impl Fn(PropertyId) -> Option<String>) -> TransformSource {
        let v = |p| cv(p).unwrap_or_default().into_boxed_str();
        TransformSource {
            transform: v(PropertyId::Transform),
            translate: v(PropertyId::Translate),
            rotate: v(PropertyId::Rotate),
            scale: v(PropertyId::Scale),
            origin: cv(PropertyId::TransformOrigin).unwrap_or_else(|| "50% 50%".into()).into_boxed_str(),
        }
    }

    /// Does the style transform nothing?
    pub(crate) fn is_none(&self) -> bool {
        let none = |s: &str| s.trim().is_empty() || s.trim() == "none";
        none(&self.transform) && none(&self.translate) && none(&self.rotate) && none(&self.scale)
    }

    /// The transform of a box of `size` (its border box) under a viewport of `viewport`, in the box's own coordinates
    /// (origin at its border box's top-left); `None` when it is the identity. CSS Transforms 2 §6: translate, then
    /// rotate, then scale, then transform -- all about the origin.
    pub(crate) fn local(&self, (w, h): (f64, f64), (vw, vh): (f64, f64)) -> Option<Affine> {
        let none = |s: &str| s.trim().is_empty() || s.trim() == "none";
        let ctx = Lengths { w, h, font: 16.0, vw, vh };
        let mut m = Affine::IDENTITY;
        if !none(&self.translate) {
            m = m.then(&translate_property(&self.translate, &ctx));
        }
        if !none(&self.rotate) {
            m = m.then(&rotate_property(&self.rotate));
        }
        if !none(&self.scale) {
            m = m.then(&scale_property(&self.scale));
        }
        if !none(&self.transform) {
            m = m.then(&parse_transform_list(&self.transform, &ctx));
        }
        if m.is_identity() {
            return None;
        }
        let (ox, oy) = transform_origin(&self.origin, &ctx);
        Some(m.about(ox, oy))
    }
}

impl Page {
    /// Does `node`'s style give its box anything [`Page::box_effects`] reads -- a `transform`, `translate`, `rotate` or
    /// `scale`, an `opacity` other than 1, a `clip`, a `clip-path`, a `filter`, a `backdrop-filter`, a
    /// `mix-blend-mode` or a `mask-image`? EVERY property `box_effects` reads for an element's own box belongs in this list
    /// (`no_effect_property_alone_is_skipped` holds each one up against it). One style lookup and eleven reads of the
    /// computed values, under one lock, with no strings made. The values are the ones `computed_value` reads before layout
    /// resolves them, and layout resolves none of these to or from their initial value, so an element this says no of
    /// is one `box_effects` would have skipped after its `computed_value` calls -- each a memo check under its own lock
    /// and a `String`, the transform resolved to a `matrix()` against the box and parsed back. That was every box, at
    /// every geometry read after a DOM write moved the effects key: about 3 % of www.costco.com's main thread
    /// (2026-10-23), where hardly a box has any of them.
    fn styles_box_effects(&self, dom: &Dom, node: NodeId, media: &MediaContext) -> bool {
        let Some(key) = self.computed_style(dom, node, media) else { return false };
        let state = self.state.lock().unwrap();
        let v = |p| state.arena.value(key, p).trim();
        let none = |p| matches!(v(p), "" | "none");
        !(none(PropertyId::Transform)
            && none(PropertyId::Translate)
            && none(PropertyId::Rotate)
            && none(PropertyId::Scale)
            && matches!(v(PropertyId::Opacity), "1" | "")
            && matches!(v(PropertyId::Clip), "auto" | "")
            && none(PropertyId::ClipPath)
            && none(PropertyId::Filter)
            && none(PropertyId::BackdropFilter)
            && matches!(v(PropertyId::MixBlendMode), "normal" | "")
            && none(PropertyId::MaskImage))
    }
}

/// What a length or percentage resolves against.
struct Lengths {
    /// The reference box: percentages along x and y.
    w: f64,
    h: f64,
    /// `em`.
    font: f64,
    vw: f64,
    vh: f64,
}

fn parse_number_or_percent(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.strip_suffix('%') {
        Some(p) => p.trim().parse::<f64>().ok().map(|v| v / 100.0),
        None => s.parse::<f64>().ok(),
    }
}

/// A length or a percentage of `basis`, in px. `None` when it is not one.
fn length(s: &str, basis: f64, ctx: &Lengths) -> Option<f64> {
    let s = s.trim();
    // a sum, as a computed value writes one (`calc(100% - 60px)`)
    if let Some(inner) = s.strip_prefix("calc(").and_then(|i| i.strip_suffix(')')) {
        let (mut total, mut sign) = (0.0, 1.0);
        for word in top_words(inner) {
            match word {
                "+" => sign = 1.0,
                "-" => sign = -1.0,
                w => {
                    total += sign * length(w, basis, ctx)?;
                    sign = 1.0;
                }
            }
        }
        return Some(total);
    }
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f64>().ok().map(|v| v / 100.0 * basis);
    }
    for (unit, k) in [("px", 1.0), ("rem", 16.0), ("em", ctx.font), ("vw", ctx.vw / 100.0), ("vh", ctx.vh / 100.0), ("pt", 4.0 / 3.0), ("cm", 96.0 / 2.54), ("mm", 96.0 / 25.4), ("in", 96.0)] {
        if let Some(v) = s.strip_suffix(unit) {
            if let Ok(v) = v.trim().parse::<f64>() {
                return Some(v * k);
            }
        }
    }
    // a unitless number: only 0 is a length
    s.parse::<f64>().ok().filter(|v| *v == 0.0)
}

/// An angle, in radians: `deg`, `rad`, `grad`, `turn` (a unitless 0 is one too).
fn angle(s: &str) -> Option<f64> {
    let s = s.trim();
    for (unit, k) in [("deg", std::f64::consts::PI / 180.0), ("grad", std::f64::consts::PI / 200.0), ("rad", 1.0), ("turn", std::f64::consts::TAU)] {
        if let Some(v) = s.strip_suffix(unit) {
            if let Ok(v) = v.trim().parse::<f64>() {
                return Some(v * k);
            }
        }
    }
    s.parse::<f64>().ok().filter(|v| *v == 0.0)
}

/// `clip: rect(top, right, bottom, left)` as a rectangle in the box's coordinates: each edge an offset from the border
/// box's top-left, `auto` the border box's own edge (CSS 2.1 §11.1.2).
fn clip_rect(value: &str, ctx: &Lengths) -> Option<[f32; 4]> {
    let inner = value.trim().strip_prefix("rect(")?.strip_suffix(')')?;
    let a = args(inner);
    if a.len() != 4 {
        return None;
    }
    let edge = |s: &str, auto: f64| if s.eq_ignore_ascii_case("auto") { Some(auto) } else { length(s, 0.0, ctx) };
    let (t, r, b, l) = (edge(&a[0], 0.0)?, edge(&a[1], ctx.w)?, edge(&a[2], ctx.h)?, edge(&a[3], 0.0)?);
    Some([l as f32, t as f32, (r - l).max(0.0) as f32, (b - t).max(0.0) as f32])
}

/// A box's `clip-path` basic shape, in its own coordinates (origin at its border box's top-left).
#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    /// `inset()`, `xywh()` or `rect()`: (x, y, width, height), its corners rounded when it has radii.
    Rect([f32; 4], Option<[(f32, f32); 4]>),
    /// `circle()` or `ellipse()`: (centre x, centre y, radius x, radius y).
    Ellipse([f32; 4]),
    /// `polygon()` or `path()`: the outline, and whether it fills even-odd.
    Path(Vec<spec::PathOp>, bool),
}

/// A computed `clip-path` as the shape it clips to (CSS Shapes 1 §3), in the box's coordinates: the shape laid out in
/// its reference box -- the border box unless a keyword names another (`reference`: that box, in the border box's
/// coordinates). `None` for `none`, a `url()`, a reference box alone, and what this does not read.
fn clip_shape(value: &str, ctx: &Lengths, reference: &dyn Fn(&str) -> (f64, f64, f64, f64)) -> Option<ClipShape> {
    let v = value.trim();
    let open = v.find('(')?;
    let close = v.rfind(')')?;
    let name = v[..open].trim();
    let keyword = v[close + 1..].trim();
    let keyword = if keyword.is_empty() { v[..open].split_whitespace().rev().nth(1).unwrap_or("border-box") } else { keyword };
    let name = name.rsplit(char::is_whitespace).next().unwrap_or(name);
    let inner = &v[open + 1..close];
    let (rx, ry, rw, rh) = reference(keyword);
    let ctx = Lengths { w: rw, h: rh, ..*ctx };
    let (w, h) = (rw, rh);
    let shape = match name {
        "inset" | "xywh" | "rect" => {
            let (edges, round) = match inner.split_once(" round ") {
                Some((e, r)) => (e, Some(r)),
                None => (inner, None),
            };
            let a: Vec<&str> = top_words(edges);
            let len = |i: usize, basis: f64| a.get(i).and_then(|s| length(s, basis, &ctx));
            let [x, y, rect_w, rect_h] = match name {
                "inset" => {
                    // one to four insets, each WORD repeated as a margin's are and resolved on its own axis
                    let word = |i: usize| -> Option<usize> {
                        Some(match (a.len(), i) {
                            (1, _) => 0,
                            (2, 2) => 0,
                            (2, 3) | (3, 3) => 1,
                            (n, i) if i < n => i,
                            _ => return None,
                        })
                    };
                    let (t, r, b, l) = (len(word(0)?, h)?, len(word(1)?, w)?, len(word(2)?, h)?, len(word(3)?, w)?);
                    [l, t, w - l - r, h - t - b]
                }
                "xywh" => [len(0, w)?, len(1, h)?, len(2, w)?, len(3, h)?],
                _ => {
                    let edge = |i: usize, basis: f64, auto: f64| a.get(i).and_then(|s| if *s == "auto" { Some(auto) } else { length(s, basis, &ctx) });
                    let (t, r, b, l) = (edge(0, h, 0.0)?, edge(1, w, w)?, edge(2, h, h)?, edge(3, w, 0.0)?);
                    [l, t, r - l, b - t]
                }
            };
            let (rect_w, rect_h) = (rect_w.max(0.0), rect_h.max(0.0));
            let radii = round.and_then(|r| corner_radii(r, rect_w, rect_h, &ctx));
            ClipShape::Rect([(rx + x) as f32, (ry + y) as f32, rect_w as f32, rect_h as f32], radii)
        }
        "circle" | "ellipse" => {
            let (size, at) = match inner.split_once("at ") {
                Some((s, p)) if s.is_empty() || s.ends_with(char::is_whitespace) => (s.trim(), Some(p.trim())),
                _ => (inner.trim(), None),
            };
            let (cx, cy) = match at {
                Some(p) => position(p, w, h, &ctx)?,
                None => (w / 2.0, h / 2.0),
            };
            let (near_x, far_x, near_y, far_y) = (cx.min(w - cx).abs(), cx.max(w - cx).abs(), cy.min(h - cy).abs(), cy.max(h - cy).abs());
            let radius = |word: Option<&str>, near: f64, far: f64, basis: f64| -> Option<f64> {
                match word {
                    None | Some("closest-side") => Some(near),
                    Some("farthest-side") => Some(far),
                    Some(l) => length(l, basis, &ctx),
                }
            };
            let words: Vec<&str> = size.split_whitespace().collect();
            let (rad_x, rad_y) = if name == "circle" {
                let diagonal = (w * w + h * h).sqrt() / std::f64::consts::SQRT_2;
                let r = radius(words.first().copied(), near_x.min(near_y), far_x.max(far_y), diagonal)?;
                (r, r)
            } else {
                (radius(words.first().copied(), near_x, far_x, w)?, radius(words.get(1).copied(), near_y, far_y, h)?)
            };
            ClipShape::Ellipse([(rx + cx) as f32, (ry + cy) as f32, rad_x.max(0.0) as f32, rad_y.max(0.0) as f32])
        }
        "polygon" => {
            let mut parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            let even_odd = fill_rule(&mut parts);
            let mut ops = Vec::new();
            for p in parts {
                let xy: Vec<&str> = top_words(p);
                let (x, y) = (length(xy.first()?, w, &ctx)?, length(xy.get(1)?, h, &ctx)?);
                ops.push(if ops.is_empty() { spec::PathOp::MoveTo { x: rx + x, y: ry + y } } else { spec::PathOp::LineTo { x: rx + x, y: ry + y } });
            }
            ops.push(spec::PathOp::ClosePath);
            ClipShape::Path(ops, even_odd)
        }
        "path" => {
            // a fill rule, then the data -- whose own commas are not the arguments'
            let inner = inner.trim();
            let (even_odd, data) = match inner.split_once(',') {
                Some((rule, rest)) if matches!(rule.trim(), "evenodd" | "nonzero") => (rule.trim() == "evenodd", rest),
                _ => (false, inner),
            };
            let data = data.trim().trim_matches(|c| c == '"' || c == '\'');
            let mut ops = Vec::new();
            for seg in svgtypes::SimplifyingPathParser::from(data) {
                use svgtypes::SimplePathSegment as S;
                ops.push(match seg.ok()? {
                    S::MoveTo { x, y } => spec::PathOp::MoveTo { x: rx + x, y: ry + y },
                    S::LineTo { x, y } => spec::PathOp::LineTo { x: rx + x, y: ry + y },
                    S::CurveTo { x1, y1, x2, y2, x, y } => spec::PathOp::BezierCurveTo { cp1x: rx + x1, cp1y: ry + y1, cp2x: rx + x2, cp2y: ry + y2, x: rx + x, y: ry + y },
                    S::Quadratic { x1, y1, x, y } => spec::PathOp::QuadraticCurveTo { cpx: rx + x1, cpy: ry + y1, x: rx + x, y: ry + y },
                    S::ClosePath => spec::PathOp::ClosePath,
                });
            }
            ClipShape::Path(ops, even_odd)
        }
        _ => return None,
    };
    Some(shape)
}

/// A shape's leading `evenodd` / `nonzero`, taken off `parts`: is it even-odd?
fn fill_rule(parts: &mut Vec<&str>) -> bool {
    match parts.first().copied() {
        Some("evenodd") => {
            parts.remove(0);
            true
        }
        Some("nonzero") => {
            parts.remove(0);
            false
        }
        _ => false,
    }
}

/// The words of `s`, a `calc(...)` kept whole.
fn top_words(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, None);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(st) = start.take() {
                    out.push(&s[st..i]);
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// A shape's `round <border-radius>` (CSS Backgrounds 3 §5.1: one to four horizontal radii, then `/` and one to four
/// vertical ones) on a `w` x `h` rectangle, scaled down where they overlap; `None` when every corner is square.
fn corner_radii(round: &str, w: f64, h: f64, ctx: &Lengths) -> Option<[(f32, f32); 4]> {
    let (hs, vs) = match round.split_once('/') {
        Some((a, b)) => (a, b),
        None => (round, round),
    };
    let four = |s: &str, basis: f64| -> Option<[f64; 4]> {
        let v: Vec<f64> = top_words(s).iter().map(|w| length(w, basis, ctx)).collect::<Option<_>>()?;
        Some(match v.as_slice() {
            [a] => [*a; 4],
            [a, b] => [*a, *b, *a, *b],
            [a, b, c] => [*a, *b, *c, *b],
            [a, b, c, d, ..] => [*a, *b, *c, *d],
            [] => return None,
        })
    };
    let (rh, rv) = (four(hs, w)?, four(vs, h)?);
    let f = [w / (rh[0] + rh[1]), w / (rh[3] + rh[2]), h / (rv[0] + rv[3]), h / (rv[1] + rv[2])].into_iter().filter(|v| v.is_finite()).fold(1.0_f64, f64::min);
    let r: [(f32, f32); 4] = std::array::from_fn(|i| ((rh[i] * f) as f32, (rv[i] * f) as f32));
    r.iter().any(|(a, b)| *a > 0.0 && *b > 0.0).then_some(r)
}

/// A `<position>` in a `w` x `h` box: one to four values of keywords and lengths (CSS Values 4 §9.1).
fn position(s: &str, w: f64, h: f64, ctx: &Lengths) -> Option<(f64, f64)> {
    let words = top_words(s);
    let horizontal = |k: &str| matches!(k, "left" | "right");
    let vertical = |k: &str| matches!(k, "top" | "bottom");
    let along = |word: &str, size: f64| -> Option<f64> {
        match word {
            "left" | "top" => Some(0.0),
            "right" | "bottom" => Some(size),
            "center" => Some(size / 2.0),
            l => length(l, size, ctx),
        }
    };
    let from = |edge: &str, offset: &str, size: f64| -> Option<f64> {
        let d = length(offset, size, ctx)?;
        Some(if matches!(edge, "right" | "bottom") { size - d } else { d })
    };
    match words.as_slice() {
        [a] if vertical(a) => Some((w / 2.0, along(a, h)?)),
        [a] => Some((along(a, w)?, h / 2.0)),
        [a, b] if vertical(a) || horizontal(b) => Some((along(b, w)?, along(a, h)?)),
        [a, b] => Some((along(a, w)?, along(b, h)?)),
        [a, ao, b, bo] if vertical(a) => Some((from(b, bo, w)?, from(a, ao, h)?)),
        [a, ao, b, bo] => Some((from(a, ao, w)?, from(b, bo, h)?)),
        _ => None,
    }
}

/// The arguments of a function, split on commas or, where there are none, on whitespace.
fn args(inner: &str) -> Vec<String> {
    let parts: Vec<String> = if inner.contains(',') {
        inner.split(',').map(|p| p.trim().to_string()).collect()
    } else {
        inner.split_whitespace().map(str::to_string).collect()
    };
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// A `<transform-list>` as [a, b, c, d, e, f], percentages of a `w` x `h` reference box -- an SVG element's, which
/// has no CSS box to read them off (`paint`'s inline `<svg>`).
pub(crate) fn transform_list_matrix(list: &str, w: f64, h: f64, font: f64, viewport: (f64, f64)) -> [f64; 6] {
    let m = parse_transform_list(list, &Lengths { w, h, font, vw: viewport.0, vh: viewport.1 });
    [m.a, m.b, m.c, m.d, m.e, m.f]
}

/// A `<transform-list>`: each function's matrix, multiplied left to right. A function it cannot read leaves the
/// list as far as it got; a 3D function keeps what it does to the plane (an orthographic view: `rotateX(θ)` squashes
/// y by cos θ).
fn parse_transform_list(list: &str, ctx: &Lengths) -> Affine {
    let mut m = Affine::IDENTITY;
    let mut rest = list.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else { break };
        let inner = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let a = args(inner);
        let num = |i: usize| a.get(i).and_then(|v| v.trim().parse::<f64>().ok());
        let len_x = |i: usize| a.get(i).and_then(|v| length(v, ctx.w, ctx));
        let len_y = |i: usize| a.get(i).and_then(|v| length(v, ctx.h, ctx));
        let ang = |i: usize| a.get(i).and_then(|v| angle(v));
        let f = match name.as_str() {
            "matrix" if a.len() == 6 => {
                let v: Vec<f64> = a.iter().filter_map(|x| x.parse::<f64>().ok()).collect();
                (v.len() == 6).then(|| Affine { a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5] })
            }
            "matrix3d" if a.len() == 16 => {
                let v: Vec<f64> = a.iter().filter_map(|x| x.parse::<f64>().ok()).collect();
                (v.len() == 16).then(|| Affine { a: v[0], b: v[1], c: v[4], d: v[5], e: v[12], f: v[13] })
            }
            "translate" => len_x(0).map(|x| Affine::translate(x, len_y(1).unwrap_or(0.0))),
            "translate3d" => len_x(0).map(|x| Affine::translate(x, len_y(1).unwrap_or(0.0))),
            "translatex" => len_x(0).map(|x| Affine::translate(x, 0.0)),
            "translatey" => len_y(0).map(|y| Affine::translate(0.0, y)),
            "translatez" => Some(Affine::IDENTITY),
            "scale" => num(0).map(|x| Affine::scale(x, num(1).unwrap_or(x))),
            "scale3d" => num(0).map(|x| Affine::scale(x, num(1).unwrap_or(1.0))),
            "scalex" => num(0).map(|x| Affine::scale(x, 1.0)),
            "scaley" => num(0).map(|y| Affine::scale(1.0, y)),
            "scalez" => Some(Affine::IDENTITY),
            "rotate" | "rotatez" => ang(0).map(Affine::rotate),
            "rotatex" => ang(0).map(|t| Affine::scale(1.0, t.cos())),
            "rotatey" => ang(0).map(|t| Affine::scale(t.cos(), 1.0)),
            "rotate3d" if a.len() == 4 => {
                // about the z axis only, the part that stays in the plane
                let (x, y, z) = (num(0).unwrap_or(0.0), num(1).unwrap_or(0.0), num(2).unwrap_or(0.0));
                ang(3).map(|t| if x == 0.0 && y == 0.0 && z != 0.0 { Affine::rotate(t * z.signum()) } else { Affine::IDENTITY })
            }
            "skew" => ang(0).map(|x| Affine::skew(x, ang(1).unwrap_or(0.0))),
            "skewx" => ang(0).map(|x| Affine::skew(x, 0.0)),
            "skewy" => ang(0).map(|y| Affine::skew(0.0, y)),
            "perspective" => Some(Affine::IDENTITY),
            _ => None,
        };
        match f {
            Some(f) => m = m.then(&f),
            None => break,
        }
    }
    m
}

/// The `translate` property: `<x> [<y> [<z>]]`.
fn translate_property(s: &str, ctx: &Lengths) -> Affine {
    let a: Vec<&str> = s.split_whitespace().collect();
    let x = a.first().and_then(|v| length(v, ctx.w, ctx)).unwrap_or(0.0);
    let y = a.get(1).and_then(|v| length(v, ctx.h, ctx)).unwrap_or(0.0);
    Affine::translate(x, y)
}

/// The `rotate` property: an angle about z, or `[x|y|z|<axis>] <angle>` (only z stays in the plane).
fn rotate_property(s: &str) -> Affine {
    let a: Vec<&str> = s.split_whitespace().collect();
    match a.as_slice() {
        [t] => angle(t).map(Affine::rotate).unwrap_or(Affine::IDENTITY),
        ["z", t] => angle(t).map(Affine::rotate).unwrap_or(Affine::IDENTITY),
        ["x", t] => angle(t).map(|t| Affine::scale(1.0, t.cos())).unwrap_or(Affine::IDENTITY),
        ["y", t] => angle(t).map(|t| Affine::scale(t.cos(), 1.0)).unwrap_or(Affine::IDENTITY),
        [x, y, z, t] if x.parse::<f64>() == Ok(0.0) && y.parse::<f64>() == Ok(0.0) => {
            let z: f64 = z.parse().unwrap_or(1.0);
            angle(t).map(|t| Affine::rotate(t * z.signum())).unwrap_or(Affine::IDENTITY)
        }
        _ => Affine::IDENTITY,
    }
}

/// The `scale` property: `<sx> [<sy> [<sz>]]`, numbers or percentages.
fn scale_property(s: &str) -> Affine {
    let a: Vec<f64> = s.split_whitespace().filter_map(parse_number_or_percent).collect();
    match a.as_slice() {
        [x] => Affine::scale(*x, *x),
        [x, y, ..] => Affine::scale(*x, *y),
        _ => Affine::IDENTITY,
    }
}

/// `transform-origin`, in the box's own coordinates: keywords, lengths or percentages of its border box.
fn transform_origin(s: &str, ctx: &Lengths) -> (f64, f64) {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let kw_x = |k: &str| match k {
        "left" => Some(0.0),
        "center" => Some(0.5),
        "right" => Some(1.0),
        _ => None,
    };
    let kw_y = |k: &str| match k {
        "top" => Some(0.0),
        "center" => Some(0.5),
        "bottom" => Some(1.0),
        _ => None,
    };
    let (mut x, mut y) = (ctx.w / 2.0, ctx.h / 2.0);
    match parts.as_slice() {
        [] => {}
        [one] => {
            if let Some(f) = kw_x(one) {
                x = f * ctx.w;
            } else if let Some(f) = kw_y(one) {
                y = f * ctx.h;
            } else if let Some(v) = length(one, ctx.w, ctx) {
                x = v;
            }
        }
        [first, second, ..] => {
            // a vertical keyword first (`top left`) swaps the pair
            let (hx, vy) = if kw_y(first).is_some() && kw_x(first).is_none() || kw_x(second).is_some() && kw_y(second).is_none() {
                (*second, *first)
            } else {
                (*first, *second)
            };
            x = kw_x(hx).map(|f| f * ctx.w).or_else(|| length(hx, ctx.w, ctx)).unwrap_or(x);
            y = kw_y(vy).map(|f| f * ctx.h).or_else(|| length(vy, ctx.h, ctx)).unwrap_or(y);
        }
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(w: f64, h: f64) -> Lengths {
        Lengths { w, h, font: 16.0, vw: 1000.0, vh: 800.0 }
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn a_transform_list_multiplies_left_to_right() {
        let c = ctx(100.0, 50.0);
        // translate then rotate: the rotation happens in the translated frame
        let m = parse_transform_list("translateX(10px) rotate(90deg)", &c);
        assert!(close(m.apply(1.0, 0.0), (10.0, 1.0)));
        // percentages of the reference box
        let m = parse_transform_list("translate(50%, 10%)", &c);
        assert!(close(m.apply(0.0, 0.0), (50.0, 5.0)));
        let m = parse_transform_list("matrix(2, 0, 0, 3, 5, 7)", &c);
        assert!(close(m.apply(1.0, 1.0), (7.0, 10.0)));
        let m = parse_transform_list("scale(2) translate(10px)", &c);
        assert!(close(m.apply(0.0, 0.0), (20.0, 0.0)));
    }

    #[test]
    fn the_origin_defaults_to_the_box_centre_and_reads_keywords() {
        let c = ctx(100.0, 50.0);
        assert!(close(transform_origin("50% 50%", &c), (50.0, 25.0)));
        assert!(close(transform_origin("left top", &c), (0.0, 0.0)));
        assert!(close(transform_origin("top right", &c), (100.0, 0.0)));
        assert!(close(transform_origin("10px 20px 0px", &c), (10.0, 20.0)));
        // rotating a 100x50 box by 180deg about its centre maps its corner to the opposite one
        let m = Affine::rotate(std::f64::consts::PI).about(50.0, 25.0);
        assert!(close(m.apply(0.0, 0.0), (100.0, 50.0)));
    }

    #[test]
    fn an_inverse_undoes_the_map() {
        let m = parse_transform_list("rotate(30deg) scale(2, 3) translate(4px, 5px) skewX(10deg)", &ctx(10.0, 10.0));
        let inv = m.invert().unwrap();
        let p = m.apply(3.0, 7.0);
        assert!(close(inv.apply(p.0, p.1), (3.0, 7.0)));
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn the_individual_properties_read_their_own_shapes() {
        let c = ctx(100.0, 50.0);
        assert!(close(translate_property("10px 50%", &c).apply(0.0, 0.0), (10.0, 25.0)));
        assert!(close(scale_property("2").apply(1.0, 1.0), (2.0, 2.0)));
        assert!(close(scale_property("50% 2").apply(2.0, 1.0), (1.0, 2.0)));
        assert!(close(rotate_property("90deg").apply(1.0, 0.0), (0.0, 1.0)));
        assert!(close(rotate_property("z 0.25turn").apply(1.0, 0.0), (0.0, 1.0)));
    }

    /// A `::before`'s own `opacity` fades its box: Google's sign-in button lies under an `opacity: 0` white state
    /// layer, and the element's own box keeps its own opacity.
    #[test]
    fn a_pseudo_elements_opacity_is_its_boxs() {
        let html = "<body><style>#a{position:relative;display:block;width:80px;height:30px;background:#0b57d0}\
                    #a::before{content:' ';position:absolute;inset:0;background:#fff;opacity:0}\
                    #b{opacity:.5;width:10px;height:10px}</style><a id=a></a><div id=b></div></body>";
        let page = Page::default();
        let dom = korvus_dom::Dom::parse(html);
        let media = MediaContext::default();
        let laid = page.layout(&dom, &media);
        let fx = page.box_effects(&dom, &media, &laid);
        let el = |id: &str| (0..dom.len() as u32).find(|&n| dom.get_attribute(n, "id") == Some(id)).unwrap();
        let before = laid.tree.pseudo_box(el("a"), false).expect("a ::before box");
        assert_eq!(fx.opacity(before), 0.0);
        assert_eq!(fx.opacity(laid.tree.box_of(el("a")).unwrap()), 1.0);
        assert_eq!(fx.opacity(laid.tree.box_of(el("b")).unwrap()), 0.5);
    }

    /// A transformed box adds its TRANSFORMED rect to the scrollable overflow: a 1205px box centred by `left: 50%;
    /// transform: translateX(-50%)` in a 1024px viewport widens nothing (Chrome: minea's page stays 1265 wide), and
    /// one pushed right by `translateX(300px)` widens the page by what it pushes past.
    #[test]
    fn a_transformed_box_overflows_where_it_shows() {
        let measure = |style: &str| {
            let html = format!("<body style='margin:0'><div style='position:relative;width:1000px;height:100px'>\
                                <div style='position:absolute;top:0;width:600px;height:50px;{style}'></div></div></body>");
            let page = Page::default();
            let dom = korvus_dom::Dom::parse(&html);
            let media = MediaContext::default();
            let mut laid = page.layout(&dom, &media);
            let fx = page.box_effects(&dom, &media, &laid);
            laid.set_effects(1, fx);
            let root = (0..dom.len() as u32).find(|&n| dom.tag_name(n) == Some("html")).unwrap();
            laid.scroll_size(root).0
        };
        // untransformed, left 700 + 600 overflows the 1024px viewport to 1300
        assert_eq!(measure("left:700px"), 1300);
        // centred on 700: 400..1000, inside
        assert_eq!(measure("left:700px;transform:translateX(-50%)"), 1024);
        // pushed right: 700 + 300 + 600
        assert_eq!(measure("left:700px;transform:translateX(300px)"), 1600);
    }

    /// Each property `box_effects` reads, set alone on a box, reaches its effects: the pre-check
    /// ([`Page::styles_box_effects`]) that skips a box with none of them must not skip one that has only it.
    #[test]
    fn no_effect_property_alone_is_skipped() {
        for (style, check) in [
            ("transform:rotate(30deg)", "transform"),
            ("translate:10px 0", "transform"),
            ("rotate:30deg", "transform"),
            ("scale:2", "transform"),
            ("opacity:.5", "opacity"),
            ("position:absolute;clip:rect(0px,10px,10px,0px)", "clip"),
            ("clip-path:inset(2px)", "clip"),
            ("filter:blur(2px)", "filter"),
            ("backdrop-filter:blur(2px)", "backdrop"),
            ("mix-blend-mode:multiply", "blend"),
            ("mask-image:linear-gradient(#000, transparent)", "mask"),
        ] {
            let html = format!("<body><div id=x style='display:block;width:40px;height:40px;{style}'></div></body>");
            let page = Page::default();
            let dom = korvus_dom::Dom::parse(&html);
            let media = MediaContext::default();
            let laid = page.layout(&dom, &media);
            let fx = page.box_effects(&dom, &media, &laid);
            let x = (0..dom.len() as u32).find(|&n| dom.get_attribute(n, "id") == Some("x")).unwrap();
            let b = laid.tree.box_of(x).unwrap();
            let has = match check {
                "transform" => fx.local(b).is_some(),
                "opacity" => fx.opacity(b) < 1.0,
                "clip" => fx.clip(b).is_some() || fx.clip_path(b).is_some(),
                "filter" => fx.filter(b).is_some(),
                "blend" => fx.blend(b).is_some(),
                "mask" => fx.masked(b),
                _ => fx.backdrop(b).is_some(),
            };
            assert!(has, "{style}: box_effects read nothing for a box that has only it");
        }
    }

    /// A computed `clip-path` reads as the shape it clips to, in the box's coordinates: Chrome's serializations (an
    /// `xywh()` as an `inset()` of `calc()`s), a shape's defaults (a circle's closest side, its centre), keyword
    /// positions, a `path()` whose data has commas, and a reference box other than the border box.
    #[test]
    fn a_computed_clip_path_is_its_shape() {
        let ctx = Lengths { w: 100.0, h: 80.0, font: 16.0, vw: 1000.0, vh: 800.0 };
        let border = |k: &str| if k == "padding-box" { (10.0, 10.0, 80.0, 60.0) } else { (0.0, 0.0, 100.0, 80.0) };
        let shape = |v: &str| clip_shape(v, &ctx, &border);
        assert_eq!(shape("circle()"), Some(ClipShape::Ellipse([50.0, 40.0, 40.0, 40.0])));
        assert_eq!(shape("circle(30px at right 10px top 20px)"), Some(ClipShape::Ellipse([90.0, 20.0, 30.0, 30.0])));
        assert_eq!(shape("ellipse(50% 25% at left bottom)"), Some(ClipShape::Ellipse([0.0, 80.0, 50.0, 20.0])));
        assert_eq!(
            shape("inset(10px calc(100% - 60px) calc(100% - 30px) 10px round 4px)"),
            Some(ClipShape::Rect([10.0, 10.0, 50.0, 20.0], Some([(4.0, 4.0); 4])))
        );
        assert_eq!(shape("inset(10%)"), Some(ClipShape::Rect([10.0, 8.0, 80.0, 64.0], None)));
        // in the 80 x 60 padding box: half of its diagonal over the square root of 2
        let r = (0.5 * (80f64 * 80.0 + 60.0 * 60.0).sqrt() / std::f64::consts::SQRT_2) as f32;
        assert_eq!(shape("circle(50% at 50% 50%) padding-box"), Some(ClipShape::Ellipse([50.0, 40.0, r, r])));
        use spec::PathOp as P;
        assert_eq!(
            shape("path(\"M 10 70 C 30 0, 70 0, 90 70 Z\")"),
            Some(ClipShape::Path(vec![P::MoveTo { x: 10.0, y: 70.0 }, P::BezierCurveTo { cp1x: 30.0, cp1y: 0.0, cp2x: 70.0, cp2y: 0.0, x: 90.0, y: 70.0 }, P::ClosePath], false))
        );
        assert_eq!(
            shape("polygon(evenodd, 0px 0px, 100% 0px, 50% 100%)"),
            Some(ClipShape::Path(vec![P::MoveTo { x: 0.0, y: 0.0 }, P::LineTo { x: 100.0, y: 0.0 }, P::LineTo { x: 50.0, y: 80.0 }, P::ClosePath], true))
        );
        assert_eq!(shape("url(\"#c\")"), None);
    }

    /// A computed `filter` reads as the chain the backend draws through: Chrome's serialization (a drop-shadow's
    /// colour first, with its own parentheses), percentages as fractions, angles in degrees; `none` and `url()` none.
    #[test]
    fn a_computed_filter_is_its_chain() {
        use spec::CanvasFilter as F;
        assert_eq!(parse_filters("blur(6px) grayscale(0.5)"), vec![F::Blur { sigma: 6.0 }, F::Grayscale { amount: 0.5 }]);
        assert_eq!(parse_filters("brightness(150%) hue-rotate(0.25turn)"), vec![F::Brightness { amount: 1.5 }, F::HueRotate { degrees: 90.0 }]);
        assert_eq!(
            parse_filters("drop-shadow(rgba(0, 0, 0, 0.6) 6px 8px 4px)"),
            vec![F::DropShadow { dx: 6.0, dy: 8.0, sigma: 4.0, r: 0, g: 0, b: 0, a: 153 }]
        );
        assert!(parse_filters("none").is_empty());
        assert!(parse_filters("url(#f)").is_empty());
    }
}

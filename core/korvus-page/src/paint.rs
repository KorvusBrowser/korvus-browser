//! The document paint walk — box tree → a 2D display list (`Canvas2DOp`). The pixels come
//! from korvus-render's Skia via `RenderHandle::raster_2d`; this module
//! only *builds the op list*. New module (R6): it reads the laid-out tree through public accessors and
//! never edits `layout/{tree,ifc,inline}.rs`.
//!
//! RC1.0 paints box BACKGROUNDS only, in tree order (= paint-order baseline per `layout/hit.rs`;
//! z-index / stacking-context reordering, borders, text, gradients and images follow, each step driven
//! by the two-sided harness diff vs the real browser).

use crate::style::media::MediaContext;
use crate::style::property::PropertyId;
use crate::value::{Color, Rgba};
use crate::{LaidOut, Page};
use cssparser::{Parser, ParserInput};
use korvus_dom::{Dom, NodeId};
use spec::Canvas2DOp;

/// A replaced element whose pixels the style paint can't produce: an `<img>`'s decoded bytes or a
/// `<canvas>`'s own ops. `paint_display_list` reports these (node + border-box in viewport coords) and
/// the webapi layer — which has op_state and a V8 scope — sources the pixels and appends a `DrawImage`.
pub struct ReplacedBox {
    pub node: NodeId,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub kind: ReplacedKind,
    /// Where in the op list its pixels go: the webapi layer inserts its `DrawImage` at this index, so the image keeps
    /// its place in paint order and stays inside the transforms, fades and clips around it (D77).
    pub at: usize,
    /// How an image, a canvas or a video fills the box (the default fills it).
    pub fit: ObjectFit,
}

/// How replaced content fills its content box: its `object-fit` and `object-position` (CSS Images 3 §4.5, §4.6).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectFit {
    pub fit: String,
    pub position_x: String,
    pub position_y: String,
}

impl Default for ObjectFit {
    fn default() -> Self {
        ObjectFit { fit: "fill".into(), position_x: "50%".into(), position_y: "50%".into() }
    }
}

/// A computed `<position>`'s two halves, each as [`background_offset`] reads one: `right 5px bottom 5px` is
/// `calc(100% - 5px)` and `calc(100% - 5px)`; one value is centred on the other axis.
fn position_halves(position: &str) -> (String, String) {
    let words: Vec<&str> = position.split_whitespace().collect();
    let vertical = |w: &str| matches!(w, "top" | "bottom");
    let edge = |kw: &str, offset: &str| if matches!(kw, "right" | "bottom") { format!("calc(100% - {offset})") } else { offset.to_string() };
    match words.as_slice() {
        [a, ao, b, bo] if vertical(a) => (edge(b, bo), edge(a, ao)),
        [a, ao, b, bo] => (edge(a, ao), edge(b, bo)),
        [a, b] if vertical(a) || matches!(*b, "left" | "right") => (b.to_string(), a.to_string()),
        [a, b] => (a.to_string(), b.to_string()),
        [a] if vertical(a) => ("50%".into(), a.to_string()),
        [a] => (a.to_string(), "50%".into()),
        _ => ("50%".into(), "50%".into()),
    }
}

impl ObjectFit {
    /// Where content of `natural` size (CSS px) is drawn in a `w` x `h` box, in the box's coordinates: stretched to
    /// it (`fill`), scaled to fit inside it (`contain`) or to cover it (`cover`), as it is (`none`), or the smaller
    /// of `none` and `contain` (`scale-down`) -- then placed by the position. It may overflow the box (`cover`).
    pub fn place(&self, natural: (f64, f64), (w, h): (f64, f64)) -> (f64, f64, f64, f64) {
        let (iw, ih) = natural;
        if !(iw > 0.0 && ih > 0.0) {
            return (0.0, 0.0, w, h);
        }
        let contain = || {
            let s = (w / iw).min(h / ih);
            (iw * s, ih * s)
        };
        let (dw, dh) = match self.fit.trim() {
            "contain" => contain(),
            "cover" => {
                let s = (w / iw).max(h / ih);
                (iw * s, ih * s)
            }
            "none" => (iw, ih),
            "scale-down" if iw <= w && ih <= h => (iw, ih),
            "scale-down" => contain(),
            _ => (w, h),
        };
        (background_offset(&self.position_x, w - dw), background_offset(&self.position_y, h - dh), dw, dh)
    }
}

pub enum ReplacedKind {
    Img,
    Canvas,
    /// A `<video>`: its pixels are a plugin-fed synthetic frame, sourced by the webapi layer (ABI 3, fake-media).
    Video,
    /// A `background-image: url(...)` layer: the webapi layer fetches and decodes it and tiles it over the box's
    /// painting area (the `ReplacedBox` rect) from its positioning area, as the layer's size, position and repeat say.
    Background(BackgroundLayer),
    /// An inline `<svg>`: its markup as a standalone SVG image, each element with the paint the cascade computed for
    /// it, for the webapi layer to rasterize at the box's size.
    Svg(String),
    /// An `<iframe>`'s (or `<frame>`'s) document, laid out and painted into the element's content box; the box here
    /// IS that content box.
    Frame,
    /// A form control's own text, which the webapi layer reads (the live value is not in the DOM): drawn in the control's
    /// content box, which the box here is.
    Control(ControlText),
}

/// How a form control's text is drawn ([`ReplacedKind::Control`]): the control's font and colours as its style computes
/// them, and where its line's baseline falls.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlText {
    pub role: ControlRole,
    pub family: String,
    pub size_px: f64,
    pub bold: bool,
    pub italic: bool,
    pub color: [f32; 4],
    /// `::placeholder`'s colour: a page's own rule's, else Chrome's `#757575`.
    pub placeholder_color: [f32; 4],
    /// The baseline of the control's one line (a `<textarea>`'s first), in the coordinates the box is in.
    pub baseline: f64,
    /// The placeholder's own weight: `::placeholder`'s `font-weight`, which a page may set apart from the field's
    /// (Naver's is 400 in a bold field).
    pub placeholder_bold: bool,
    /// The caret's colour (`caret-color`) while the control has the focus and takes text, else `None`.
    pub caret: Option<[f32; 4]>,
    /// The line's ascent and descent, which the caret spans.
    pub ascent: f64,
    pub descent: f64,
    /// The control's `accent-color` (Chrome's `#0075ff` for `auto`): a range's filled track and its thumb.
    pub accent: [f32; 4],
    /// A range's track and thumb as their styles have them ([`ControlRole::Range`]).
    pub slider: Option<SliderLook>,
}

/// A range input's parts (Chrome 150, measured): the theme's track -- 8px, pill-shaped, `#efefef` edged `#b2b2b2`, filled
/// with the accent up to the thumb -- while the input's `appearance` is not `none`; the theme's thumb -- a 16px accent
/// circle -- while `::-webkit-slider-thumb`'s is not; else what those pseudo-elements' own styles draw.
#[derive(Clone, Debug, PartialEq)]
pub struct SliderLook {
    pub native_track: bool,
    pub native_thumb: bool,
    /// The thumb's width and height, its background, its corner radius and its `margin-top` (a styled thumb sits at
    /// its track's top, moved by that margin).
    pub thumb: (f64, f64),
    pub thumb_color: Option<[f32; 4]>,
    pub thumb_radius: f64,
    pub thumb_margin_top: f64,
    /// `::-webkit-slider-runnable-track`'s height (the thumb's when it has none), background and radius.
    pub track_height: Option<f64>,
    pub track_color: Option<[f32; 4]>,
    pub track_radius: f64,
}

/// Where a form control's text comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlRole {
    /// A single-line text field: its value, else its placeholder.
    Field,
    /// A password field: its value as bullets, else its placeholder.
    Password,
    /// A `<textarea>`: its placeholder while it is empty (its value is laid out as text).
    Area,
    /// `<input type=submit|button|reset>`: its label, centred.
    Button,
    /// A drop-down `<select>`: its selected option's label.
    Select,
    /// A checkbox: its check mark, while it is checked.
    Check,
    /// A radio button: its dot, while it is checked.
    Radio,
    /// `<input type=range>`: its track and thumb at its live value.
    Range,
    /// `<input type=color>`: its swatch, in its live value.
    Color,
    /// `<input type=file>`: its button and the text beside it.
    File,
    /// `<input type=date|month|week|time|datetime-local>`: its value in the locale's fields, and its picker icon.
    DateTime,
}

/// A form control the platform theme draws ([`Painter::theme_part`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemePart {
    Field,
    Area,
    Button,
    MenuList,
    Checkbox,
    Radio,
}

/// The theme's control edge, `#767676` (Chrome's light controls, measured).
const THEME_EDGE: [f32; 4] = [118.0 / 255.0, 118.0 / 255.0, 118.0 / 255.0, 1.0];

/// Chrome's accent for `accent-color: auto`, `#0075ff`.
pub const CHROME_ACCENT: [f32; 4] = [0.0, 117.0 / 255.0, 1.0, 1.0];

/// One `url()` background layer, as its box's computed style places it.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundLayer {
    /// Absolute (the cascade resolved it against its stylesheet).
    pub url: String,
    /// The positioning area (`background-origin`'s box): x, y, width, height.
    pub origin: (f64, f64, f64, f64),
    /// The border box's rounded corners, when the layer is clipped to it.
    pub radii: Option<[(f64, f64); 4]>,
    /// The computed `background-size`, `background-position-x` / `-y` and `background-repeat` of this layer.
    pub size: String,
    pub position_x: String,
    pub position_y: String,
    pub repeat: String,
    /// A `mask-image` layer: its image is the CORS request the element's style made (Blink's `LoadMaskSource`), not a
    /// background's no-cors one.
    pub cors: bool,
}

/// `background-size` in a positioning area of `area`, for an image of natural size `natural` -- or of none, a
/// gradient's (CSS Backgrounds 3 §3.9: its `auto`, `cover` and `contain` are the area's): `cover`, `contain`, or one
/// or two of a length, a percentage of the area, and `auto` (the image's own proportion, else its natural size).
pub fn background_tile_size(size: &str, natural: Option<(f64, f64)>, area: (f64, f64)) -> (f64, f64) {
    let (aw, ah) = area;
    let mut parts = size.split_whitespace();
    let (w, h) = (parts.next().and_then(|p| background_length(p, aw)), parts.next().and_then(|p| background_length(p, ah)));
    let Some((iw, ih)) = natural else {
        return if matches!(size.trim(), "cover" | "contain") { area } else { (w.unwrap_or(aw), h.unwrap_or(ah)) };
    };
    match size.trim() {
        "cover" => {
            let s = (aw / iw).max(ah / ih);
            (iw * s, ih * s)
        }
        "contain" => {
            let s = (aw / iw).min(ah / ih);
            (iw * s, ih * s)
        }
        _ => match (w, h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ih * w / iw),
            (None, Some(h)) => (iw * h / ih, h),
            (None, None) => (iw, ih),
        },
    }
}

/// A `background-position` component: a percentage places that point of the image on the same point of the area
/// (`free` is the area less the image), a length is an offset, and `calc(P% ± L)` is both.
pub fn background_offset(v: &str, free: f64) -> f64 {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix("calc(").and_then(|i| i.strip_suffix(')')) {
        let mut total = 0.0;
        let mut sign = 1.0;
        for tok in inner.split_whitespace() {
            match tok {
                "+" => sign = 1.0,
                "-" => sign = -1.0,
                t => {
                    total += sign * background_offset(t, free);
                    sign = 1.0;
                }
            }
        }
        return total;
    }
    match v {
        "left" | "top" => 0.0,
        "center" => free / 2.0,
        "right" | "bottom" => free,
        _ => background_length(v, free).unwrap_or(0.0),
    }
}

/// A length in px, or a percentage of `basis`; `None` for `auto`.
fn background_length(v: &str, basis: f64) -> Option<f64> {
    let v = v.trim();
    if v == "auto" {
        return None;
    }
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f64>().ok().map(|p| p / 100.0 * basis);
    }
    v.strip_suffix("px").unwrap_or(v).trim().parse::<f64>().ok()
}

/// Which directions a `background-repeat` value repeats in. `space` and `round` repeat (their spacing and rounding
/// are a later refinement).
pub fn background_repeats(v: &str) -> (bool, bool) {
    let one = |w: &str| w != "no-repeat";
    let words: Vec<&str> = v.split_whitespace().collect();
    match words.as_slice() {
        ["repeat-x"] => (true, false),
        ["repeat-y"] => (false, true),
        [w] => (one(w), one(w)),
        [x, y, ..] => (one(x), one(y)),
        [] => (true, true),
    }
}

/// The largest TEXT box this paint produced — a `largest-contentful-paint` candidate.
/// `area` is its viewport-INTERSECTED area in px². Images/canvases are NOT tracked here — they are
/// returned as [`ReplacedBox`], and the webapi selects the LCP across this text box and the LOADED
/// replaced boxes (clamping an image to its intrinsic area), in `render_document_rgba`'s replaced loop
/// where img_state is already consulted. Plain backgrounds are excluded (a browser excludes the root bg).
pub struct LcpCandidate {
    pub node: NodeId,
    pub area: f64,
}

/// Parse a computed CSS color string (e.g. `"rgb(215, 56, 42)"`, `"rgba(0, 0, 0, 0)"`) to [`Rgba`].
/// A CSS colour as 8-bit RGBA (`currentcolor` and the like excepted), for the effects' `drop-shadow()`.
pub(crate) fn parse_color_rgba(s: &str) -> Option<(u8, u8, u8, u8)> {
    parse_rgba(s).map(|c| (c.r, c.g, c.b, (c.a.clamp(0.0, 1.0) * 255.0).round() as u8))
}

fn parse_rgba(s: &str) -> Option<Rgba> {
    let mut input = ParserInput::new(s);
    let mut p = Parser::new(&mut input);
    match Color::parse(&mut p).ok()? {
        Color::Rgba(c) => Some(c),
        _ => None,
    }
}

/// A box's computed `background-color`, or `None` when it is absent or fully transparent.
fn bg_color(page: &Page, dom: &Dom, media: &MediaContext, laid_out: &LaidOut, node: korvus_dom::NodeId) -> Option<Rgba> {
    let computed = page.computed_value(dom, node, media, PropertyId::BackgroundColor, Some(laid_out))?;
    parse_rgba(&computed).filter(|c| c.a > 0.0)
}

/// The colour the viewport canvas is painted with (CSS Backgrounds §3.11.1): the root element's
/// background, propagated from the body when the root is transparent. Box 0 is `<html>`, box 1 `<body>`.
fn canvas_background(page: &Page, dom: &Dom, media: &MediaContext, laid_out: &LaidOut) -> Option<Rgba> {
    let tree = &laid_out.tree;
    let n = tree.len() as u32;
    if n == 0 {
        return None;
    }
    bg_color(page, dom, media, laid_out, tree.node_of(0))
        .or_else(|| (n > 1).then(|| bg_color(page, dom, media, laid_out, tree.node_of(1))).flatten())
}

/// Build the 2D display list for a laid-out document: the propagated canvas background over the whole viewport, then
/// every box in CSS's paint order (CSS 2.1 Appendix E, [`paint_order`]) -- each stacking context's own background,
/// its negative `z-index` children, its in-flow blocks' backgrounds and borders, its floats, its inline content, its
/// positioned descendants and `z-index: 0` contexts in tree order, then its positive `z-index` children. Boxes used
/// to paint in tree order, whole: a dropdown painted under the content after it.
pub fn paint_display_list(
    page: &Page,
    dom: &Dom,
    media: &MediaContext,
    laid_out: &LaidOut,
    viewport_w: f64,
    viewport_h: f64,
) -> (Vec<Canvas2DOp>, Vec<ReplacedBox>, Option<LcpCandidate>) {
    let tree = &laid_out.tree;
    let pseudo = pseudo_map(laid_out);
    let mut p = Painter {
        page,
        dom,
        media,
        laid_out,
        pseudo: pseudo.clone(),
        viewport: (viewport_w, viewport_h),
        ops: Vec::new(),
        replaced: Vec::new(),
        lcp: None,
        contexts: Contexts::default(),
        cuts: std::collections::HashMap::new(),
        starts: std::collections::HashMap::new(),
        isolated: Vec::new(),
        grad_id: 0,
        // Where each box shows: layout's place less what has been scrolled (the document, its scroll containers)
        // plus a sticky offset -- the same translation hit testing and `getBoundingClientRect` apply (D77).
        translations: tree.box_translations(&laid_out.scroll),
    };

    // The root/body background fills the ENTIRE viewport canvas, not just the element's box — otherwise
    // everything below the body's content height reads as the transparent (black) default.
    if let Some(c) = canvas_background(page, dom, media, laid_out) {
        p.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
        p.ops.push(Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: viewport_w, h: viewport_h });
    }
    let (order, isolated) = paint_order(page, dom, media, laid_out, &pseudo);
    p.isolated = isolated;
    for (index, phase) in order {
        p.paint(index, phase);
    }
    p.contexts.close_all(&mut p.ops, &mut p.replaced);
    // the viewport's scrollbars, over everything the document paints, in the look of the `<body>`'s custom
    // scrollbar, else the root's (Blink's `ScrollbarStyleSource`)
    let (bar, vertical, horizontal) = tree.viewport_scrollbars();
    if bar > 0.0 && (vertical || horizontal) {
        let (sw, sh) = tree.scroll_size(0);
        let (cw, ch) = tree.client_size(0);
        let root = tree.node_of(0);
        let source = tree.body_node().filter(|&b| p.custom_scrollbar(b)).unwrap_or(root);
        let at = Scrollbars {
            rect: (0.0, 0.0, viewport_w, viewport_h),
            bars: (if vertical { bar as f64 } else { 0.0 }, if horizontal { bar as f64 } else { 0.0 }),
            scroll: laid_out.scroll.viewport,
            client: (cw as f64, ch as f64),
            size: (sw as f64, sh as f64),
            look: p.bar_look(source),
        };
        p.scrollbars(&at);
    }
    (p.ops, p.replaced, p.lcp)
}

/// Where a scroller's scrollbars go and what they show: its padding box's outer rect (inside its borders), each bar's
/// thickness (0: none), how far it is scrolled, its client and scroll sizes, and the bars' look.
struct Scrollbars {
    rect: (f64, f64, f64, f64),
    bars: (f64, f64),
    scroll: (f32, f32),
    client: (f64, f64),
    size: (f64, f64),
    look: BarLook,
}

/// How a scrollbar is drawn.
enum BarLook {
    /// The platform's: Chrome 152's Linux (Fluent) scrollbar, in its own colours or `scrollbar-color`'s (thumb and
    /// arrows, then track and corner).
    Theme { ink: Rgba, track: Rgba },
    /// A `::-webkit-scrollbar` one: the scrollbar's own background, its track's over it, its thumb (with its
    /// `border-radius`) and its corner; no buttons, as an unstyled `::-webkit-scrollbar-button` has none.
    Custom { base: Option<Rgba>, track: Option<Rgba>, thumb: Option<Rgba>, radius: f64, corner: Rgba },
}

/// Chrome 152's Linux scrollbar (the Fluent theme), measured: a `#fcfcfc` track; a button at each end holding a
/// `#8b8b8b` triangle; a `#8b8b8b` pill of a thumb inset from the track's sides, as long as the track times the
/// visible fraction (17 at least). See [`FluentGeometry`].
const BAR_TRACK: Rgba = Rgba { r: 0xfc, g: 0xfc, b: 0xfc, a: 1.0, wide: None };
const BAR_INK: Rgba = Rgba { r: 0x8b, g: 0x8b, b: 0x8b, a: 1.0, wide: None };
const BAR_MIN_THUMB: f64 = 17.0;

/// The Fluent scrollbar's parts for one thickness: the buttons' length along the bar, the arrow's width and depth,
/// where the arrow starts in its button (up, down, left, right) and the thumb's inset from the bar's sides. Measured
/// on Chrome 150 for the two thicknesses the personas wear -- 15px and `scrollbar-width: thin`'s 10px; any other is
/// scaled from the 15px one, its arrow centred.
struct FluentGeometry {
    button: f64,
    arrow: (u32, u32),
    offsets: [f64; 4],
    inset: f64,
}

fn fluent_geometry(thickness: f64) -> FluentGeometry {
    match thickness.round() as i64 {
        15 => FluentGeometry { button: 18.0, arrow: (9, 5), offsets: [6.0, 6.0, 5.0, 7.0], inset: 3.0 },
        10 => FluentGeometry { button: 12.0, arrow: (6, 3), offsets: [4.0, 5.0, 3.0, 6.0], inset: 2.0 },
        _ => {
            let button = (thickness * 1.2).round();
            let width = (thickness * 0.6).round().max(1.0) as u32;
            let depth = width.div_ceil(2);
            let centre = ((button - depth as f64) / 2.0).floor();
            FluentGeometry { button, arrow: (width, depth), offsets: [centre; 4], inset: (thickness * 0.2).round() }
        }
    }
}

/// Which half of a box a step of the paint order paints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Its shadow, background and borders.
    Own,
    /// Its text and its replaced content (an image, a canvas, a video).
    Content,
    /// Its outline, over its layer's in-flow content and under the positioned boxes above it, where Blink's
    /// `PaintLayerPainter` paints descendant and self outlines.
    Outline,
}

/// What a box is to the paint order: in flow, a float or a positioned box with `z-index: auto` (each painted as if
/// it were a stacking context, its positioned descendants left to the real one), or a stacking context of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    Flow,
    Float,
    Positioned,
    Context(i32),
}

/// The paint order of every box: `(box, phase)` steps, back to front (CSS 2.1 Appendix E, E.2) -- and which boxes are
/// stacking contexts a `mix-blend-mode` below blends inside, each painted into a group of its own (CSS Compositing 1
/// §3.2: an element blends with its stacking context's content alone; the root's is the canvas).
fn paint_order(page: &Page, dom: &Dom, media: &MediaContext, laid_out: &LaidOut, pseudo: &[Pseudo]) -> (Vec<(u32, Phase)>, Vec<bool>) {
    let tree = &laid_out.tree;
    let n = tree.len();
    let layers: Vec<Layer> = (0..n as u32).map(|b| layer_of(page, dom, media, laid_out, b, pseudo)).collect();
    let mut isolated = vec![false; n];
    if let Some(fx) = tree.effects() {
        for b in (0..n as u32).filter(|&b| fx.blend(b).is_some()) {
            let mut up = tree.parent_of(b);
            while let Some(a) = up {
                if matches!(layers[a as usize], Layer::Context(_)) {
                    isolated[a as usize] = true;
                    break;
                }
                up = tree.parent_of(a);
            }
        }
    }
    // each box's subtree is the boxes from it to `end[box]` (boxes are numbered in tree order)
    let mut end: Vec<u32> = (1..=n as u32).collect();
    for i in (0..n).rev() {
        if let Some(parent) = tree.parent_of(i as u32) {
            let e = end[i];
            let pe = &mut end[parent as usize];
            *pe = (*pe).max(e);
        }
    }
    let mut out = Vec::with_capacity(n * 2);
    let roots: Vec<u32> = (0..n as u32).filter(|&b| tree.parent_of(b).is_none()).collect();
    paint_context(None, &roots, &layers, &end, &mut out);
    (out, isolated)
}

/// One stacking context, `root` (`None`: the canvas, whose children are `tops`), into `out`.
fn paint_context(root: Option<u32>, tops: &[u32], layers: &[Layer], end: &[u32], out: &mut Vec<(u32, Phase)>) {
    // What the context holds, read in one walk over its subtree, nested contexts skipped whole: its child contexts
    // by `z-index`, its positioned boxes and `z-index: 0` contexts in tree order, and who owns each in-flow box -- the
    // context, or the float or positioned box nearest above it, which paints it.
    let mut negative: Vec<(i32, u32)> = Vec::new();
    let mut positive: Vec<(i32, u32)> = Vec::new();
    let mut zero: Vec<u32> = Vec::new();
    let mut flow: std::collections::HashMap<Option<u32>, Vec<u32>> = std::collections::HashMap::new();
    let mut floats: std::collections::HashMap<Option<u32>, Vec<u32>> = std::collections::HashMap::new();
    let mut open: Vec<u32> = Vec::new(); // the floats and positioned boxes the walk is inside
    let ranges: Vec<(u32, u32)> = match root {
        Some(r) => vec![(r + 1, end[r as usize])],
        None => tops.iter().map(|&t| (t, end[t as usize])).collect(),
    };
    for (start, stop) in ranges {
        let mut i = start;
        while i < stop {
            while open.last().is_some_and(|&o| end[o as usize] <= i) {
                open.pop();
            }
            let owner = open.last().copied().or(root);
            match layers[i as usize] {
                Layer::Context(z) => {
                    if z < 0 {
                        negative.push((z, i));
                    } else if z == 0 {
                        zero.push(i);
                    } else {
                        positive.push((z, i));
                    }
                    i = end[i as usize];
                    continue;
                }
                Layer::Positioned => {
                    zero.push(i);
                    open.push(i);
                }
                Layer::Float => {
                    floats.entry(owner).or_default().push(i);
                    open.push(i);
                }
                Layer::Flow => flow.entry(owner).or_default().push(i),
            }
            i += 1;
        }
    }
    negative.sort_by_key(|&(z, _)| z);
    positive.sort_by_key(|&(z, _)| z);

    if let Some(r) = root {
        out.push((r, Phase::Own));
    }
    for &(_, c) in &negative {
        paint_context(Some(c), &[], layers, end, out);
    }
    paint_owned(root, &flow, &floats, out);
    // the root's own content: an image that is a stacking context (`opacity`, a `z-index`) drew nothing
    if let Some(r) = root {
        out.push((r, Phase::Content));
        out.push((r, Phase::Outline));
    }
    for &b in &zero {
        match layers[b as usize] {
            Layer::Context(_) => paint_context(Some(b), &[], layers, end, out),
            _ => {
                out.push((b, Phase::Own));
                paint_owned(Some(b), &flow, &floats, out);
                out.push((b, Phase::Content));
                out.push((b, Phase::Outline));
            }
        }
    }
    for &(_, c) in &positive {
        paint_context(Some(c), &[], layers, end, out);
    }
}

/// What `owner` (a context's root, a float, or a positioned box) paints of its own: its in-flow boxes' backgrounds
/// and borders, its floats -- each painted as one -- then its own content and its in-flow boxes' content.
fn paint_owned(
    owner: Option<u32>,
    flow: &std::collections::HashMap<Option<u32>, Vec<u32>>,
    floats: &std::collections::HashMap<Option<u32>, Vec<u32>>,
    out: &mut Vec<(u32, Phase)>,
) {
    let mine = flow.get(&owner).map(Vec::as_slice).unwrap_or(&[]);
    for &b in mine {
        out.push((b, Phase::Own));
    }
    for &f in floats.get(&owner).map(Vec::as_slice).unwrap_or(&[]) {
        out.push((f, Phase::Own));
        paint_owned(Some(f), flow, floats, out);
        out.push((f, Phase::Content));
        out.push((f, Phase::Outline));
    }
    // (a float's or a positioned box's own content is painted by its caller, after this)
    for &b in mine {
        out.push((b, Phase::Content));
    }
    for &b in mine {
        out.push((b, Phase::Outline));
    }
}

/// Box `b`'s place in the paint order. Only an element's principal box, or a `::before`/`::after`'s, is ever more
/// than in flow.
fn layer_of(page: &Page, dom: &Dom, media: &MediaContext, laid_out: &LaidOut, b: u32, pseudo: &[Pseudo]) -> Layer {
    let tree = &laid_out.tree;
    let node = tree.node_of(b);
    let own = pseudo.get(b as usize).copied().flatten();
    let pseudo_root = own.is_some_and(|p| p.root);
    if !pseudo_root && (dom.node_type(node) != 1 || tree.box_of(node) != Some(b) || own.is_some()) {
        return Layer::Flow;
    }
    let cv = |p| box_value(page, dom, media, laid_out, node, own, p).unwrap_or_default();
    let position = cv(PropertyId::Position);
    let positioned = !position.is_empty() && position != "static";
    let z = cv(PropertyId::ZIndex).trim().parse::<i32>().ok();
    // a flex or grid item with a `z-index` is a stacking context though it is not positioned
    let item = z.is_some()
        && dom.parent(node).and_then(|p| page.computed_value(dom, p, media, PropertyId::Display, Some(laid_out))).is_some_and(|d| d.contains("flex") || d.contains("grid"));
    let fx = tree.effects();
    let effects = fx.is_some_and(|fx| fx.local(b).is_some() || fx.opacity(b) < 1.0);
    let isolating = cv(PropertyId::Isolation) == "isolate"
        || !matches!(cv(PropertyId::Filter).as_str(), "" | "none")
        || !matches!(cv(PropertyId::MixBlendMode).as_str(), "" | "normal")
        || !matches!(cv(PropertyId::MaskImage).as_str(), "" | "none");
    if (positioned && z.is_some()) || item || matches!(position.as_str(), "fixed" | "sticky") || effects || isolating {
        return Layer::Context(z.unwrap_or(0));
    }
    if positioned {
        return Layer::Positioned;
    }
    if !matches!(cv(PropertyId::Float).as_str(), "" | "none") {
        return Layer::Float;
    }
    Layer::Flow
}

/// The display list as it is built.
struct Painter<'a> {
    page: &'a Page,
    dom: &'a Dom,
    media: &'a MediaContext,
    laid_out: &'a LaidOut,
    /// Which boxes are a `::before`/`::after`'s (its own box, and the text in it), whose style they read.
    pseudo: Vec<Pseudo>,
    viewport: (f64, f64),
    ops: Vec<Canvas2DOp>,
    replaced: Vec<ReplacedBox>,
    lcp: Option<LcpCandidate>,
    contexts: Contexts,
    /// Each block's lines cut for an ellipsis ([`Painter::line_cuts`]), made the first time one of its texts paints.
    cuts: std::collections::HashMap<u32, std::rc::Rc<Vec<CutLine>>>,
    /// Where each block's line content starts, fragment by fragment ([`BoxTree::ifc_fragment_starts`]), made the
    /// first time one of its texts paints: the room a text has before what follows it on its line.
    starts: std::collections::HashMap<u32, std::rc::Rc<Vec<(f32, f32, f32)>>>,
    /// The stacking contexts painted into a group of their own, for a blend inside ([`paint_order`]).
    isolated: Vec<bool>,
    grad_id: u32,
    translations: Vec<(f32, f32)>,
}

impl Painter<'_> {
    fn cv(&self, node: NodeId, p: PropertyId) -> Option<String> {
        self.page.computed_value(self.dom, node, self.media, p, Some(self.laid_out))
    }

    /// A property of box `index`'s style: its element's, or its `::before`/`::after`'s.
    fn cvb(&self, index: u32, p: PropertyId) -> Option<String> {
        let node = self.laid_out.tree.node_of(index);
        box_value(self.page, self.dom, self.media, self.laid_out, node, self.pseudo.get(index as usize).copied().flatten(), p)
    }

    /// Box `index`'s `background-color`, when it paints one.
    fn bg_of(&self, index: u32) -> Option<Rgba> {
        self.cvb(index, PropertyId::BackgroundColor).as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0)
    }

    /// One step of the paint order: `phase` of box `index`.
    fn paint(&mut self, index: u32, phase: Phase) {
        let tree = &self.laid_out.tree;
        let node = tree.node_of(index);
        // `visibility: hidden` (and `collapse`) paints nothing of the box's own; a `visible` descendant still paints
        if self.cvb(index, PropertyId::Visibility).is_some_and(|v| v != "visible") {
            return;
        }
        // The transforms, fades and clips of the boxes above this one, and its own transform and fade (D77).
        let chain = context_chain(tree, index, &self.isolated);
        for c in &chain {
            if let Context::Mask(b) = *c {
                if !self.contexts.masks.contains_key(&b) {
                    let layers = self.mask_layers(b);
                    self.contexts.masks.insert(b, layers);
                }
            }
        }
        self.contexts.enter(&mut self.ops, &mut self.replaced, chain, tree, &self.translations, &self.laid_out.scroll);
        let rect = tree.border_box(index);
        let (tx, ty) = self.translations[index as usize];
        let bx = ((rect.x + tx) as f64, (rect.y + ty) as f64, rect.width as f64, rect.height as f64);
        // Only an element's own box paints its background, borders and replaced content. A text run's box and an
        // anonymous box carry their element's node too, and painted its background and border a second time over
        // their own rect -- a border drawn around every line of a bordered box's text (seen beside Chrome, D77).
        let pseudo = self.pseudo.get(index as usize).copied().flatten();
        let element_box = self.dom.node_type(node) == 1 && tree.box_of(node) == Some(index) && pseudo.is_none();
        let principal = element_box || pseudo.is_some_and(|p| p.root);
        match phase {
            Phase::Own if principal => self.paint_own(index, node, bx),
            Phase::Own => {}
            Phase::Content => self.paint_content(index, node, bx, element_box),
            Phase::Outline if principal => {
                self.outline(index, bx);
                self.own_scrollbars(index, node, bx);
            }
            Phase::Outline => {}
        }
    }

    /// A scroll container's own scrollbars, at the inside of its borders, over what it holds.
    fn own_scrollbars(&mut self, index: u32, node: NodeId, (x, y, bw, bh): (f64, f64, f64, f64)) {
        let tree = &self.laid_out.tree;
        if tree.parent_of(index).is_none() {
            return; // the root's are the viewport's
        }
        let gutter = tree.scrollbar_gutter(index);
        if gutter.width <= 0.0 && gutter.height <= 0.0 {
            return;
        }
        let border = tree.layout(index).border;
        let (sw, sh) = tree.scroll_size(index);
        let (cw, ch) = tree.client_size(index);
        let at = Scrollbars {
            rect: (
                x + border.left as f64,
                y + border.top as f64,
                (bw - (border.left + border.right) as f64).max(0.0),
                (bh - (border.top + border.bottom) as f64).max(0.0),
            ),
            bars: (gutter.width as f64, gutter.height as f64),
            scroll: self.laid_out.scroll.elements.get(&node).copied().unwrap_or((0.0, 0.0)),
            client: (cw as f64, ch as f64),
            size: (sw as f64, sh as f64),
            look: self.bar_look(node),
        };
        self.scrollbars(&at);
    }

    /// Does `node` draw a `::-webkit-scrollbar` -- one matched, and no standard `scrollbar-width` or `scrollbar-color`
    /// set, which win (as layout sizes it: `Variant::CustomScrollbar`)?
    fn custom_scrollbar(&self, node: NodeId) -> bool {
        let standard = |p: PropertyId| self.cv(node, p).is_some_and(|v| v.trim() != "auto");
        self.laid_out.tree.custom_scrollbar(node) && !standard(PropertyId::ScrollbarWidth) && !standard(PropertyId::ScrollbarColor)
    }

    /// The look of `node`'s scrollbars ([`BarLook`]).
    fn bar_look(&self, node: NodeId) -> BarLook {
        if self.custom_scrollbar(node) {
            let part = |name: &str, p: PropertyId| self.page.computed_pseudo_value(self.dom, node, name, self.media, p, Some(self.laid_out));
            let color = |name: &str| part(name, PropertyId::BackgroundColor).as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0);
            let radius = part("-webkit-scrollbar-thumb", PropertyId::BorderTopLeftRadius).map_or(0.0, |r| px(r.split_whitespace().next().unwrap_or("0")));
            return BarLook::Custom {
                base: color("-webkit-scrollbar"),
                track: color("-webkit-scrollbar-track"),
                thumb: color("-webkit-scrollbar-thumb"),
                radius,
                corner: color("-webkit-scrollbar-corner").unwrap_or(Rgba { r: 255, g: 255, b: 255, a: 1.0, wide: None }),
            };
        }
        // `scrollbar-color: <thumb> <track>`
        let colors = self.cv(node, PropertyId::ScrollbarColor).filter(|v| v.trim() != "auto");
        let pair: Vec<Rgba> = colors.as_deref().map(|v| split_top_spaces(v).iter().filter_map(|c| parse_rgba(c)).collect()).unwrap_or_default();
        match pair.as_slice() {
            [ink, track] => BarLook::Theme { ink: *ink, track: *track },
            _ => BarLook::Theme { ink: BAR_INK, track: BAR_TRACK },
        }
    }

    /// Paint scrollbars ([`Scrollbars`]).
    fn scrollbars(&mut self, s: &Scrollbars) {
        let (x, y, w, h) = s.rect;
        let (vbar, hbar) = s.bars;
        let fill = |ops: &mut Vec<Canvas2DOp>, x: f64, y: f64, w: f64, h: f64| {
            if w > 0.0 && h > 0.0 {
                ops.push(Canvas2DOp::FillRect { x, y, w, h });
            }
        };
        let paint = |ops: &mut Vec<Canvas2DOp>, c: Rgba| ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
        let vrect = (x + w - vbar, y, vbar, h - hbar);
        let hrect = (x, y + h - hbar, w - vbar, hbar);
        // the track (for a custom scrollbar, its background then its track's) and the corner
        let (track_fills, corner, ink, thumb_color, radius, buttons): (Vec<Rgba>, Rgba, Rgba, Option<Rgba>, Option<f64>, bool) = match &s.look {
            BarLook::Theme { ink, track } => (vec![*track], *track, *ink, Some(*ink), None, true),
            BarLook::Custom { base, track, thumb, radius, corner } => {
                (base.iter().chain(track.iter()).copied().collect(), *corner, Rgba::BLACK, *thumb, Some(*radius), false)
            }
        };
        for c in &track_fills {
            paint(&mut self.ops, *c);
            if vbar > 0.0 {
                fill(&mut self.ops, vrect.0, vrect.1, vrect.2, vrect.3);
            }
            if hbar > 0.0 {
                fill(&mut self.ops, hrect.0, hrect.1, hrect.2, hrect.3);
            }
        }
        if vbar > 0.0 && hbar > 0.0 {
            paint(&mut self.ops, corner);
            fill(&mut self.ops, x + w - vbar, y + h - hbar, vbar, hbar);
        }
        // one bar: `start` its first pixel along its axis, `edge` its side across it, `length` how long it runs
        let mut bar = |vertical: bool, start: f64, edge: f64, length: f64, thick: f64, scroll: f32, client: f64, size: f64| {
            let g = fluent_geometry(thick);
            let button = if buttons { g.button } else { 0.0 };
            if length < 2.0 * button || length <= 0.0 {
                return;
            }
            if buttons {
                paint(&mut self.ops, ink);
                // a triangle row by row from its point, crisp as Chrome's are
                let (width, depth) = g.arrow;
                let side = ((thick - width as f64) / 2.0).floor();
                let arrow = |ops: &mut Vec<Canvas2DOp>, at: f64, toward_start: bool| {
                    for i in 0..depth {
                        let row = if toward_start { i } else { depth - 1 - i };
                        let span = width as f64 - 2.0 * (depth - 1 - row) as f64;
                        let from = edge + side + (width as f64 - span) / 2.0;
                        if vertical {
                            fill(ops, from, at + i as f64, span, 1.0);
                        } else {
                            fill(ops, at + i as f64, from, 1.0, span);
                        }
                    }
                };
                let (first, last) = if vertical { (g.offsets[0], g.offsets[1]) } else { (g.offsets[2], g.offsets[3]) };
                arrow(&mut self.ops, start + first, true);
                arrow(&mut self.ops, start + length - button + last, false);
            }
            // Blink's `ScrollbarTheme::ThumbLength`/`ThumbPosition`: the length rounded, none past the track; the
            // position TRUNCATED, a fraction of the first pixel taken as one (Chrome 150: 139.5 is 139)
            let Some(color) = thumb_color else { return };
            let track = length - 2.0 * button;
            let min = if buttons { BAR_MIN_THUMB } else { 0.0 };
            let thumb = if size > 0.0 { (track * client / size).round().max(min) } else { track };
            if thumb > track || size <= client || thumb <= 0.0 {
                return;
            }
            let max = (size - client).max(1.0);
            let pos = (track - thumb) * (scroll as f64).clamp(0.0, max) / max;
            let offset = if pos > 0.0 && pos < 1.0 { 1.0 } else { pos.trunc() };
            let along = start + button + offset;
            let (inset, r) = match radius {
                None => (g.inset, ((thick - 2.0 * g.inset) / 2.0).max(0.0)),
                Some(r) => (0.0, r),
            };
            let across = thick - 2.0 * inset;
            let (tx, ty, tw, th) = if vertical { (edge + inset, along, across, thumb) } else { (along, edge + inset, thumb, across) };
            paint(&mut self.ops, color);
            self.ops.push(Canvas2DOp::BeginPath);
            self.ops.push(Canvas2DOp::RoundRect { x: tx, y: ty, w: tw, h: th, radii: [(r, r); 4] });
            self.ops.push(Canvas2DOp::Fill { even_odd: false });
        };
        if vbar > 0.0 {
            bar(true, y, x + w - vbar, h - hbar, vbar, s.scroll.1, s.client.1, s.size.1);
        }
        if hbar > 0.0 {
            bar(false, x, y + h - hbar, w - vbar, hbar, s.scroll.0, s.client.0, s.size.0);
        }
    }

    /// A box's `outline`: a ring `outline-offset` outside its border box, following its corners. Every style but
    /// `auto` is drawn solid; `auto` is the focus ring.
    fn outline(&mut self, index: u32, (x, y, bw, bh): (f64, f64, f64, f64)) {
        let style = self.cvb(index, PropertyId::OutlineStyle).unwrap_or_default();
        if matches!(style.as_str(), "" | "none" | "hidden") {
            return;
        }
        if style == "auto" {
            self.focus_ring(index, (x, y, bw, bh));
            return;
        }
        let w = self.cvb(index, PropertyId::OutlineWidth).map(|s| px(&s)).unwrap_or(0.0);
        let Some(c) = self.cvb(index, PropertyId::OutlineColor).as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0) else { return };
        if w <= 0.0 {
            return;
        }
        let o = self.cvb(index, PropertyId::OutlineOffset).map(|s| px(&s)).unwrap_or(0.0);
        let (ix, iy, iw, ih) = (x - o, y - o, (bw + 2.0 * o).max(0.0), (bh + 2.0 * o).max(0.0));
        let grow = |r: Option<[(f64, f64); 4]>, by: f64| r.map(|r| r.map(|(a, b)| if a > 0.0 && b > 0.0 { ((a + by).max(0.0), (b + by).max(0.0)) } else { (0.0, 0.0) }));
        let radii = self.radii(index, bw, bh);
        self.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
        self.ops.push(Canvas2DOp::BeginPath);
        match radii {
            Some(r) => {
                self.ops.push(Canvas2DOp::RoundRect { x: ix - w, y: iy - w, w: iw + 2.0 * w, h: ih + 2.0 * w, radii: grow(Some(r), o + w).unwrap() });
                self.ops.push(Canvas2DOp::RoundRect { x: ix, y: iy, w: iw, h: ih, radii: grow(Some(r), o).unwrap() });
            }
            None => {
                self.ops.push(Canvas2DOp::Rect { x: ix - w, y: iy - w, w: iw + 2.0 * w, h: ih + 2.0 * w });
                self.ops.push(Canvas2DOp::Rect { x: ix, y: iy, w: iw, h: ih });
            }
        }
        self.ops.push(Canvas2DOp::Fill { even_odd: true });
    }

    /// A box's shadows, background and borders.
    fn paint_own(&mut self, index: u32, node: NodeId, (x, y, bw, bh): (f64, f64, f64, f64)) {
        // a `<progress>` or `<meter>` in its native appearance: the theme's bar (it drew nothing)
        if self.pseudo.get(index as usize).copied().flatten().is_none() && self.paint_bar(index, node, (x, y, bw, bh)) {
            return;
        }
        // a form control in its native appearance is the theme's to draw, not its CSS border's
        if let Some(part) = self.theme_part(index, node) {
            self.paint_theme_part(part, (x, y, bw, bh));
            return;
        }
        let radii = self.radii(index, bw, bh);
        // `backdrop-filter`: what is already drawn behind the border box, filtered, inside its corners -- under the
        // box's own shadow and background (a translucent header over a blurred page)
        if let Some(filters) = self.laid_out.tree.effects().and_then(|fx| fx.backdrop(index)).map(|f| f.to_vec()) {
            self.ops.push(Canvas2DOp::Save);
            self.clip_box(x, y, bw, bh, radii);
            self.ops.push(Canvas2DOp::SaveLayerBackdrop { filters, rect: [x, y, bw, bh] });
            self.ops.push(Canvas2DOp::Restore);
            self.ops.push(Canvas2DOp::Restore);
        }
        // `box-shadow`, outside the border box, under the background
        if let Some(shadows) = self.cvb(index, PropertyId::BoxShadow).filter(|s| s != "none") {
            let current = self.cvb(index, PropertyId::Color).as_deref().and_then(parse_rgba).unwrap_or(Rgba::BLACK);
            self.box_shadows(&shadows, (x, y, bw, bh), radii, current);
        }
        // A background clipped to the text is painted with the text (`text_backgrounds`), not over the box.
        if self.cvb(index, PropertyId::BackgroundClip).is_some_and(|c| c.split(',').any(|l| l.trim() == "text")) {
            self.borders(index, (x, y, bw, bh), radii);
            return;
        }
        // The colour layer, then the image layers on top, the last listed lowest. `background-origin` and
        // `background-clip` are lists, one box per layer (repeated as needed); the colour is clipped as the lowest
        // layer is -- the whole list read as one keyword put a two-layer box's backgrounds inside its border.
        let nth = |v: &Vec<String>, i: usize, d: &str| if v.is_empty() { d.to_string() } else { v[i % v.len()].clone() };
        let layers = self.cvb(index, PropertyId::BackgroundImage).filter(|s| s != "none").map(|s| split_top_commas(&s)).unwrap_or_default();
        let clips = self.cvb(index, PropertyId::BackgroundClip).map(|s| split_top_commas(&s)).unwrap_or_default();
        if let Some(c) = self.bg_of(index) {
            let (cx, cy, cw, ch) = self.background_box(index, &nth(&clips, layers.len().max(1) - 1, "border-box"), (x, y, bw, bh));
            self.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
            self.fill_box(cx, cy, cw, ch, radii.filter(|_| (cx, cy, cw, ch) == (x, y, bw, bh)));
        }
        if !layers.is_empty() {
            let list = |p| self.cvb(index, p).map(|s| split_top_commas(&s)).unwrap_or_default();
            let (origins, sizes, xs, ys, repeats) = (
                list(PropertyId::BackgroundOrigin),
                list(PropertyId::BackgroundSize),
                list(PropertyId::BackgroundPositionX),
                list(PropertyId::BackgroundPositionY),
                list(PropertyId::BackgroundRepeat),
            );
            for (i, layer) in layers.iter().enumerate().rev() {
                let layer = layer.trim();
                let origin = self.background_box(index, &nth(&origins, i, "padding-box"), (x, y, bw, bh));
                let clip = self.background_box(index, &nth(&clips, i, "border-box"), (x, y, bw, bh));
                let placed = ImageLayer {
                    image: layer,
                    mask: false,
                    origin,
                    clip,
                    radii: radii.filter(|_| clip == (x, y, bw, bh)),
                    size: nth(&sizes, i, "auto"),
                    x: nth(&xs, i, "0%"),
                    y: nth(&ys, i, "0%"),
                    repeat: nth(&repeats, i, "repeat"),
                };
                match layer_paint(placed, &mut self.grad_id) {
                    Some(LayerPaint::Ops(o)) => self.ops.extend(o),
                    Some(LayerPaint::Image(bg)) => {
                        let at = self.ops.len();
                        self.replaced.push(ReplacedBox { node, x: clip.0, y: clip.1, w: clip.2, h: clip.3, kind: ReplacedKind::Background(bg), at, fit: ObjectFit::default() });
                    }
                    None => {}
                }
            }
        }
        self.borders(index, (x, y, bw, bh), radii);
    }

    /// A box's text, its decorations, and its replaced content.
    fn paint_content(&mut self, index: u32, node: NodeId, (x, y, bw, bh): (f64, f64, f64, f64), principal: bool) {
        let tree = &self.laid_out.tree;
        // Text: one FillText per laid-out line fragment (RC1.5).
        // a `<textarea>`'s text is its live value, which the control draws (`ReplacedKind::Control`): what is laid out is
        // the element's text, the default value, which typing and script never change
        let in_textarea = self.dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("textarea"));
        let frags = if in_textarea { Vec::new() } else { tree.text_fragments(index, &self.laid_out.scroll) };
        let has_text = !frags.is_empty();
        if has_text {
            let family = self.cvb(index, PropertyId::FontFamily).unwrap_or_else(|| "sans-serif".into());
            let size_px = self.cvb(index, PropertyId::FontSize).map(|s| px(&s)).filter(|&n| n > 0.0).unwrap_or(16.0);
            let weight = self.cvb(index, PropertyId::FontWeight).map_or(400, |w| match w.trim() {
                "bold" => 700,
                w => w.parse::<f32>().map_or(400, |n| n.round().clamp(1.0, 1000.0) as u16),
            });
            let bold = weight >= 600;
            let italic = self.cvb(index, PropertyId::FontStyle).map(|s| s.trim() != "normal").unwrap_or(false);
            // the glyphs are filled with `-webkit-text-fill-color`, which is `color` unless set
            let color = self
                .cvb(index, PropertyId::WebkitTextFillColor)
                .or_else(|| self.cvb(index, PropertyId::Color))
                .as_deref()
                .and_then(parse_rgba)
                .unwrap_or(Rgba::BLACK);
            let clip_fills = self.text_backgrounds(node);
            self.ops.push(Canvas2DOp::SetFont {
                family,
                size_px,
                bold,
                italic,
                weight,
                families: String::new(),
                lang: String::new(),
                default_family: String::new(),
            });
            // drawn on the line's baseline, where the layout put it: a canvas `top` is the em box's top, which in a font
            // whose ascent is not its em box's (Noto Sans CJK: 1.16 em against 0.88) drew the glyphs 5px above the line
            self.ops.push(Canvas2DOp::SetTextBaseline { baseline: spec::TextBaseline::Alphabetic });
            self.ops.push(Canvas2DOp::SetFillStyle { color: color.paint_color() });
            let lines = self.decorations(node);
            // where an underline's top is when the style moves it (Blink's `TextDecorationOffset`): `under` puts it
            // below the descent -- GitHub's banner link drew its line through the descenders -- and a length
            // `text-underline-offset` measures from the baseline; both `auto`, the font's own position below
            let underline_offset = self.cvb(index, PropertyId::TextUnderlineOffset).filter(|o| o.trim() != "auto").map(|o| px(&o));
            let under = self.cvb(index, PropertyId::TextUnderlinePosition).is_some_and(|p| p.split_whitespace().any(|w| w == "under"));
            let metrics = tree.font_metrics_at(&family_of(&self.cvb(index, PropertyId::FontFamily).unwrap_or_default()), weight, italic, size_px as f32);
            // the baseline sits the font's ascent below the top of the text's content area, rounded as Blink rounds it
            let ascent = (metrics.ascent as f64 * size_px).round();
            let descent = (metrics.descent as f64 * size_px).round();
            let spacing = |p: PropertyId| self.cvb(index, p).and_then(|v| crate::style::resolve::px_of(&v)).unwrap_or(0.0);
            let (letter, word) = (spacing(PropertyId::LetterSpacing), spacing(PropertyId::WordSpacing));
            let cuts = tree.ifc_of(index).map(|root| self.line_cuts(root)).filter(|c| !c.is_empty());
            let starts = tree.ifc_of(index).map(|root| {
                let laid = self.laid_out;
                self.starts.entry(root).or_insert_with(|| std::rc::Rc::new(laid.tree.ifc_fragment_starts(root, &laid.scroll))).clone()
            });
            let shadows: Vec<Shadow> = self
                .cvb(index, PropertyId::TextShadow)
                .filter(|s| s.trim() != "none")
                .map(|list| split_top_commas(&list).iter().rev().filter_map(|s| parse_shadow(s, color)).filter(|s| s.color.a > 0.0).collect())
                .unwrap_or_default();
            for (s, fr) in frags {
                let y = fr.y as f64 + ascent;
                // a line cut for an ellipsis: the characters that end past the cut are not drawn, and the fragment
                // that carries the "…" draws it after what it kept (where its line ends, when nothing is cut)
                let mut ellipsis_at = None;
                // the room before what follows it on its line: none past the line's last fragment
                let centre = fr.y + fr.height / 2.0;
                let mut room: Option<f64> = starts.as_ref().and_then(|st| {
                    st.iter()
                        .filter(|(top, bottom, x)| centre > *top && centre < *bottom && *x >= fr.x + fr.width - 0.5)
                        .map(|(_, _, x)| (*x - fr.x) as f64)
                        .reduce(f64::min)
                });
                let s = match cuts.as_ref().and_then(|c| c.iter().find(|l| fr.y + fr.height / 2.0 > l.top && fr.y + fr.height / 2.0 < l.bottom)) {
                    Some(line) => {
                        let (kept, width) = match line.cut {
                            Some(cut) => {
                                let pieces = spaced_pieces(&s, metrics, size_px, letter, word);
                                let mut kept = String::new();
                                let mut width = 0.0;
                                for (i, (piece, start)) in pieces.iter().enumerate() {
                                    let end = pieces.get(i + 1).map_or(fr.width as f64, |n| n.1);
                                    if fr.x as f64 + end > cut + 0.01 {
                                        break;
                                    }
                                    kept.push_str(piece);
                                    width = end;
                                    let _ = start;
                                }
                                (kept, width)
                            }
                            None => (s.clone(), fr.width as f64),
                        };
                        if line.host == index {
                            ellipsis_at = Some(fr.x as f64 + width);
                        }
                        room = Some(width);
                        kept
                    }
                    None => s,
                };
                // the text (and its "…") drawn moved by `(ox, oy)`: in place, or as a shadow
                // Within the room before what follows it on its line: the layout measures with the PERSONA's fonts, the
                // glyphs are this machine's, and where those are wider a word ran into the next one ("applicationfor"
                // all down a Wikipedia article). Squeezed into it, as a canvas `maxWidth` squeezes -- only where it
                // would run on: the last text of a line draws as it is (a trailing space draws nothing).
                let text_at = |out: &mut Vec<Canvas2DOp>, ox: f64, oy: f64| {
                    if letter == 0.0 && word == 0.0 {
                        // the trailing space is not drawn, and its width stays out of the room: squeezed over it, the
                        // text ran up to the link after it ("browser'saddress bar" on Wikipedia)
                        let text = s.trim_end_matches(' ');
                        let trail = &s[text.len()..];
                        let space = if trail.is_empty() { 0.0 } else { metrics.width_px_kerned(trail, size_px as f32, true) as f64 };
                        let text = text.to_string();
                        out.push(Canvas2DOp::FillText { text, x: fr.x as f64 + ox, y: y + oy, max_width: room.map(|r| (r - space).max(0.0)) });
                    } else {
                        for (piece, dx) in spaced_pieces(&s, metrics, size_px, letter, word) {
                            out.push(Canvas2DOp::FillText { text: piece, x: fr.x as f64 + dx + ox, y: y + oy, max_width: None });
                        }
                    }
                    if let Some(x) = ellipsis_at {
                        out.push(Canvas2DOp::FillText { text: "\u{2026}".into(), x: x + ox, y: y + oy, max_width: None });
                    }
                };
                let text_ops = |out: &mut Vec<Canvas2DOp>| text_at(out, 0.0, 0.0);
                // `text-shadow`: under the text, the last listed lowest, each the glyphs moved, in its colour, blurred
                // by a Gaussian of half its radius
                for sh in &shadows {
                    self.ops.push(Canvas2DOp::Save);
                    if sh.blur > 0.0 {
                        self.ops.push(Canvas2DOp::SetFilter { filters: vec![spec::CanvasFilter::Blur { sigma: sh.blur / 2.0 }] });
                    }
                    self.ops.push(Canvas2DOp::SetFillStyle { color: sh.color.paint_color() });
                    text_at(&mut self.ops, sh.dx, sh.dy);
                    self.ops.push(Canvas2DOp::Restore);
                }
                // `background-clip: text`: the background shows through the glyphs, under the text's own fill
                for fill in &clip_fills {
                    self.ops.extend(fill.iter().cloned());
                    text_ops(&mut self.ops);
                }
                if !clip_fills.is_empty() {
                    self.ops.push(Canvas2DOp::SetFillStyle { color: color.paint_color() });
                }
                if color.a > 0.0 {
                    text_ops(&mut self.ops);
                }
                let baseline = fr.y as f64 + ascent;
                let underline_at = match (under, underline_offset) {
                    (true, offset) => Some(baseline + descent + offset.unwrap_or(0.0)),
                    (false, Some(offset)) => Some(baseline + offset),
                    (false, None) => None,
                };
                for &(kind, c, thickness) in &lines {
                    // Chrome 150, measured on DejaVu Sans and Noto Sans at 16, 20 and 40px: an underline centred 0.09em
                    // below the baseline, a line-through 0.34 of the ascent above it, each about 1/12 em thick
                    let t = thickness.unwrap_or((size_px / 12.0).round()).max(1.0);
                    let centre = match kind {
                        Decoration::Underline => match underline_at {
                            Some(top) => top + t / 2.0,
                            None => baseline + 0.09 * size_px,
                        },
                        Decoration::LineThrough => baseline - 0.34 * ascent,
                        Decoration::Overline => fr.y as f64 + t / 2.0,
                    };
                    self.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
                    self.ops.push(Canvas2DOp::FillRect { x: fr.x as f64, y: (centre - t / 2.0).round(), w: fr.width as f64, h: t });
                }
                if !lines.is_empty() {
                    self.ops.push(Canvas2DOp::SetFillStyle { color: color.paint_color() });
                }
            }
        }

        // Replaced content: report the box for the webapi layer to fill with pixels (RC2), inside its rounded
        // corners when it has them (an avatar's `border-radius: 50%`).
        let svg_root = principal && self.dom.namespace_uri(node) == Some(SVG_NS) && self.dom.tag_name(node) == Some("svg");
        // Content that cannot reach the frame is not drawn: minea's two Lottie `<svg>`s below the fold (1.4 MB of
        // markup, ten blurs each, rewritten every frame) were serialized and rasterized at every frame, seconds each,
        // and the page stopped answering. A transform around the box could bring it in, so then it is kept.
        let shows = {
            let (vw, vh) = self.viewport;
            self.contexts.open.iter().any(|c| matches!(c, Context::Transform(_))) || (x < vw && y < vh && x + bw > 0.0 && y + bh > 0.0)
        };
        let kind = match self.dom.tag_name(node).filter(|_| principal && shows) {
            Some("svg") if svg_root => Some(ReplacedKind::Svg(self.svg_markup(node))),
            Some("img") => Some(ReplacedKind::Img),
            Some("canvas") => Some(ReplacedKind::Canvas),
            // The engine owns only "this box is a video" (J1); the webapi layer sources its pixels and sources
            // NOTHING when no frame is fed, so a no-plugin <video> paints exactly as before (I9/J2).
            Some("video") => Some(ReplacedKind::Video),
            Some("iframe" | "frame") => Some(ReplacedKind::Frame),
            // `appearance: none` takes a checkbox's and a radio button's native look away, the checked mark with it:
            // what CSS draws is all there is (Chrome 150; Google's Material checkbox, a 48px `appearance: none` input
            // under its own drawn box, showed korvus's native mark stretched over its label)
            Some(tag @ ("input" | "textarea" | "select")) => self
                .control_role(node, tag)
                .filter(|role| {
                    !matches!(role, ControlRole::Check | ControlRole::Radio) || self.cv(node, PropertyId::Appearance).is_none_or(|a| a.trim() != "none")
                })
                .map(|role| ReplacedKind::Control(self.control_text(index, role, bh))),
            _ => None,
        };
        if let Some(kind) = kind {
            // replaced content is cut to its rounded corners -- not a range's parts, which overflow its box as Chrome
            // draws them (a 18px thumb on a 6px rounded track)
            let range = matches!(&kind, ReplacedKind::Control(t) if t.role == ControlRole::Range);
            let radii = self.radii(index, bw, bh).filter(|_| !range);
            if radii.is_some() {
                self.ops.push(Canvas2DOp::Save);
                self.clip_box(x, y, bw, bh, radii);
            }
            let at = self.ops.len();
            // replaced content shows inside the element's border and padding: its content box (an `<img>`'s too --
            // it was stretched over its border box)
            let (x, y, bw, bh) = {
                let l = tree.layout(index);
                let (left, top) = ((l.border.left + l.padding.left) as f64, (l.border.top + l.padding.top) as f64);
                let (right, bottom) = ((l.border.right + l.padding.right) as f64, (l.border.bottom + l.padding.bottom) as f64);
                (x + left, y + top, (bw - left - right).max(0.0), (bh - top - bottom).max(0.0))
            };
            let fit = if matches!(kind, ReplacedKind::Img | ReplacedKind::Canvas | ReplacedKind::Video) {
                let position = self.cvb(index, PropertyId::ObjectPosition).unwrap_or_default();
                let (position_x, position_y) = position_halves(&position);
                ObjectFit { fit: self.cvb(index, PropertyId::ObjectFit).unwrap_or_else(|| "fill".into()), position_x, position_y }
            } else {
                ObjectFit::default()
            };
            let svg_text = matches!(kind, ReplacedKind::Svg(_));
            self.replaced.push(ReplacedBox { node, x, y, w: bw, h: bh, kind, at, fit });
            if svg_text {
                self.svg_text_ops(node, x, y, bw, bh);
            }
            if radii.is_some() {
                self.ops.push(Canvas2DOp::Restore);
            }
        }

        // Track the largest TEXT box by viewport-intersected area — the text
        // largest-contentful-paint candidate (images/canvases are weighed via ReplacedBox in the webapi).
        if has_text {
            let (vw, vh) = self.viewport;
            let ix = x.max(0.0).min(vw);
            let ixr = (x + bw).max(0.0).min(vw);
            let iy = y.max(0.0).min(vh);
            let iyb = (y + bh).max(0.0).min(vh);
            let area = (ixr - ix).max(0.0) * (iyb - iy).max(0.0);
            if area > 0.0 && self.lcp.as_ref().map_or(true, |c| area > c.area) {
                self.lcp = Some(LcpCandidate { node, area });
            }
        }
    }

    /// `outline: auto`, Chrome's focus ring (Linux, light, measured on Chrome 152): a 2px ring of the outline's colour
    /// (`-webkit-focus-ring-color`, `rgb(16,16,16)`) just inside the border box pushed out by `outline-offset`, its
    /// corners rounded 3px (or the box's own radius beyond that).
    fn focus_ring(&mut self, index: u32, (x, y, bw, bh): (f64, f64, f64, f64)) {
        let Some(c) = self.cvb(index, PropertyId::OutlineColor).as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0) else { return };
        let o = self.cvb(index, PropertyId::OutlineOffset).map(|s| px(&s)).unwrap_or(0.0);
        let (rx, ry, rw, rh) = (x - o, y - o, bw + 2.0 * o, bh + 2.0 * o);
        if rw <= 4.0 || rh <= 4.0 {
            return;
        }
        let r = self.radii(index, bw, bh).map_or(0.0, |r| r.iter().map(|&(a, b)| a.max(b)).fold(0.0, f64::max) + o).max(3.0);
        self.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
        self.ops.push(Canvas2DOp::BeginPath);
        self.ops.push(Canvas2DOp::RoundRect { x: rx, y: ry, w: rw, h: rh, radii: [(r, r); 4] });
        let ri = (r - 2.0).max(0.0);
        self.ops.push(Canvas2DOp::RoundRect { x: rx + 2.0, y: ry + 2.0, w: rw - 4.0, h: rh - 4.0, radii: [(ri, ri); 4] });
        self.ops.push(Canvas2DOp::Fill { even_odd: true });
    }

    /// A range input's parts as their styles have them ([`SliderLook`]).
    fn slider_look(&self, node: NodeId) -> SliderLook {
        let native_track = self.cv(node, PropertyId::Appearance).is_none_or(|a| a.trim() != "none");
        let part = |pseudo: &str, p: PropertyId| self.page.computed_pseudo_value(self.dom, node, pseudo, self.media, p, Some(self.laid_out));
        let thumb = |p: PropertyId| part("-webkit-slider-thumb", p);
        let track = |p: PropertyId| part("-webkit-slider-runnable-track", p);
        let length = |v: Option<String>| v.and_then(|v| crate::style::resolve::px_of(&v)).map(f64::from);
        let colour = |v: Option<String>| v.as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0).map(|c| c.paint_color());
        let native_thumb = thumb(PropertyId::Appearance).is_none_or(|a| a.trim() != "none");
        let (tw, th) = if native_thumb { (16.0, 16.0) } else { (length(thumb(PropertyId::Width)).unwrap_or(0.0), length(thumb(PropertyId::Height)).unwrap_or(0.0)) };
        let radius = |v: Option<String>, size: f64| {
            v.and_then(|r| {
                let r = r.split_ascii_whitespace().next()?.to_string();
                match r.strip_suffix('%') {
                    Some(p) => p.parse::<f64>().ok().map(|p| p / 100.0 * size),
                    None => crate::style::resolve::px_of(&r).map(f64::from),
                }
            })
            .unwrap_or(0.0)
        };
        SliderLook {
            native_track,
            native_thumb,
            thumb: (tw, th),
            thumb_color: colour(thumb(PropertyId::BackgroundColor)),
            thumb_radius: radius(thumb(PropertyId::BorderTopLeftRadius), tw.min(th)),
            thumb_margin_top: length(thumb(PropertyId::MarginTop)).unwrap_or(0.0),
            track_height: length(track(PropertyId::Height)),
            track_color: colour(track(PropertyId::BackgroundColor)),
            track_radius: radius(track(PropertyId::BorderTopLeftRadius), length(track(PropertyId::Height)).unwrap_or(0.0)),
        }
    }

    /// A `<progress>` or `<meter>` whose `appearance` is not `none`, drawn as Chrome 150's theme draws it (measured): a
    /// bar half its box's height, centred, pill-shaped, `#efefef` -- edged `#b2b2b2` for a progress, `#cbcbcb` for a
    /// meter -- filled from its start to its value: a progress in its accent colour, a meter by where its value lies
    /// against `low`, `high` and `optimum` (`#107c10`, `#ffb900`, `#d83b01`). An indeterminate progress draws its track.
    /// `false` when the box is neither.
    fn paint_bar(&mut self, index: u32, node: NodeId, (x, y, w, h): (f64, f64, f64, f64)) -> bool {
        let Some(tag) = self.dom.tag_name(node).filter(|t| t.eq_ignore_ascii_case("progress") || t.eq_ignore_ascii_case("meter")) else { return false };
        if self.cv(node, PropertyId::Appearance).is_some_and(|a| a.trim() == "none") || w <= 0.0 || h <= 0.0 {
            return false;
        }
        let num = |a: &str| self.dom.get_attribute(node, a).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let progress = tag.eq_ignore_ascii_case("progress");
        let (fill, colour) = if progress {
            let max = num("max").filter(|&m| m > 0.0).unwrap_or(1.0);
            let fill = num("value").map(|v| (v.max(0.0) / max).min(1.0));
            let accent = self
                .cvb(index, PropertyId::AccentColor)
                .filter(|a| a.trim() != "auto")
                .as_deref()
                .and_then(parse_rgba)
                .map_or(CHROME_ACCENT, |c| c.paint_color());
            (fill, accent)
        } else {
            // HTML's meter: the value clamped to [min, max], `low` and `high` to that range, `optimum` its middle by default
            let min = num("min").unwrap_or(0.0);
            let max = num("max").filter(|&m| m >= min).unwrap_or(min.max(1.0));
            let value = num("value").unwrap_or(0.0).clamp(min, max);
            let low = num("low").unwrap_or(min).clamp(min, max);
            let high = num("high").unwrap_or(max).clamp(low, max);
            let optimum = num("optimum").unwrap_or((min + max) / 2.0).clamp(min, max);
            const GOOD: [f32; 4] = [16.0 / 255.0, 124.0 / 255.0, 16.0 / 255.0, 1.0];
            const FAIR: [f32; 4] = [1.0, 185.0 / 255.0, 0.0, 1.0];
            const POOR: [f32; 4] = [216.0 / 255.0, 59.0 / 255.0, 1.0 / 255.0, 1.0];
            let colour = if optimum < low {
                if value < low { GOOD } else if value <= high { FAIR } else { POOR }
            } else if optimum > high {
                if value > high { GOOD } else if value >= low { FAIR } else { POOR }
            } else if value >= low && value <= high {
                GOOD
            } else {
                FAIR
            };
            (Some(if max > min { (value - min) / (max - min) } else { 0.0 }), colour)
        };
        let bh = (h / 2.0).round().max(1.0);
        let by = y + ((h - bh) / 2.0).round();
        let r = bh / 2.0;
        let edge = if progress { [178.0 / 255.0, 178.0 / 255.0, 178.0 / 255.0, 1.0] } else { [203.0 / 255.0, 203.0 / 255.0, 203.0 / 255.0, 1.0] };
        self.ops.push(Canvas2DOp::SetFillStyle { color: edge });
        self.fill_box(x, by, w, bh, Some([(r, r); 4]));
        let ri = (r - 1.0).max(0.0);
        self.ops.push(Canvas2DOp::SetFillStyle { color: [239.0 / 255.0, 239.0 / 255.0, 239.0 / 255.0, 1.0] });
        self.fill_box(x + 1.0, by + 1.0, (w - 2.0).max(0.0), (bh - 2.0).max(0.0), Some([(ri, ri); 4]));
        if let Some(f) = fill.filter(|&f| f > 0.0) {
            self.ops.push(Canvas2DOp::Save);
            self.ops.push(Canvas2DOp::BeginPath);
            self.ops.push(Canvas2DOp::Rect { x, y: by, w: w * f, h: bh });
            self.ops.push(Canvas2DOp::Clip { even_odd: false });
            self.ops.push(Canvas2DOp::SetFillStyle { color: colour });
            if progress {
                self.fill_box(x, by, w, bh, Some([(r, r); 4]));
            } else {
                self.fill_box(x + 1.0, by + 1.0, (w - 2.0).max(0.0), (bh - 2.0).max(0.0), Some([(ri, ri); 4]));
            }
            self.ops.push(Canvas2DOp::Restore);
        }
        true
    }

    /// The theme part element box `index` is, when the theme draws it: a form control whose `appearance` is not `none`
    /// and whose background and border are still the UA sheet's (Blink's `LayoutTheme::IsControlStyled`: a page that
    /// gives a control a border or a background of its own gets the control drawn by CSS). Chrome draws a text field
    /// with a 1px `#767676` edge round its 2px `inset` border, a button with that edge and its face, a drop-down with a
    /// chevron; this engine drew the CSS borders -- 2px dark grey insets on every field.
    fn theme_part(&self, index: u32, node: NodeId) -> Option<ThemePart> {
        if self.pseudo.get(index as usize).copied().flatten().is_some() {
            return None;
        }
        let tag = self.dom.tag_name(node)?.to_ascii_lowercase();
        if !matches!(tag.as_str(), "input" | "button" | "select" | "textarea") || self.cv(node, PropertyId::Appearance).is_some_and(|a| a.trim() == "none") {
            return None;
        }
        let v = |p| self.cv(node, p).unwrap_or_default();
        let (bg, width, style, color) = (v(PropertyId::BackgroundColor), v(PropertyId::BorderTopWidth), v(PropertyId::BorderTopStyle), v(PropertyId::BorderTopColor));
        let ua = |w: &str, s: &str, c: &str, backgrounds: &[&str]| width == w && style == s && color == c && backgrounds.contains(&bg.as_str());
        let field = || ua("2px", "inset", "rgb(118, 118, 118)", &["rgb(255, 255, 255)"]);
        let button = || ua("2px", "outset", "rgb(0, 0, 0)", &["rgb(239, 239, 239)"]);
        match tag.as_str() {
            "button" => button().then_some(ThemePart::Button),
            "textarea" => ua("1px", "solid", "rgb(118, 118, 118)", &["rgb(255, 255, 255)"]).then_some(ThemePart::Area),
            "select" => match self.control_role(node, "select") {
                Some(ControlRole::Select) => ua("1px", "solid", "rgb(118, 118, 118)", &["rgb(255, 255, 255)", "rgb(239, 239, 239)"]).then_some(ThemePart::MenuList),
                _ => None,
            },
            _ => match self.control_role(node, "input")? {
                ControlRole::Field | ControlRole::Password => field().then_some(ThemePart::Field),
                ControlRole::Button => button().then_some(ThemePart::Button),
                // a colour input is drawn as a button round its swatch (its UA 1px border and button face)
                ControlRole::Color => (width == "1px" && style == "solid" && bg == "rgb(239, 239, 239)").then_some(ThemePart::Button),
                ControlRole::Check => (bg == "rgba(0, 0, 0, 0)" && style == "none").then_some(ThemePart::Checkbox),
                ControlRole::Radio => (bg == "rgba(0, 0, 0, 0)" && style == "none").then_some(ThemePart::Radio),
                _ => None,
            },
        }
    }

    /// A theme part, over its border box: Chrome's light Linux controls (measured on Chrome 152 at 1x).
    fn paint_theme_part(&mut self, part: ThemePart, (x, y, w, h): (f64, f64, f64, f64)) {
        let round = |r: f64| Some([(r, r); 4]);
        let face = match part {
            ThemePart::Button => [239.0 / 255.0, 239.0 / 255.0, 239.0 / 255.0, 1.0],
            _ => [1.0, 1.0, 1.0, 1.0],
        };
        let r = if part == ThemePart::Radio { w.min(h) / 2.0 } else { 2.0 };
        self.ops.push(Canvas2DOp::SetFillStyle { color: face });
        self.fill_box(x, y, w, h, round(r));
        // the 1px edge, a ring inside the border box
        self.ops.push(Canvas2DOp::SetFillStyle { color: THEME_EDGE });
        self.ops.push(Canvas2DOp::BeginPath);
        self.ops.push(Canvas2DOp::RoundRect { x, y, w, h, radii: [(r, r); 4] });
        let ri = (r - 1.0).max(0.0);
        self.ops.push(Canvas2DOp::RoundRect { x: x + 1.0, y: y + 1.0, w: (w - 2.0).max(0.0), h: (h - 2.0).max(0.0), radii: [(ri, ri); 4] });
        self.ops.push(Canvas2DOp::Fill { even_odd: true });
        match part {
            // the drop-down's chevron, 5px in from the right, centred
            ThemePart::MenuList => {
                let (cx, cy) = (x + w - 9.0, y + h / 2.0);
                self.ops.push(Canvas2DOp::SetStrokeStyle { color: [0.0, 0.0, 0.0, 1.0] });
                self.ops.push(Canvas2DOp::SetLineWidth { width: 1.5 });
                self.ops.push(Canvas2DOp::BeginPath);
                self.ops.push(Canvas2DOp::MoveTo { x: cx - 4.0, y: cy - 2.0 });
                self.ops.push(Canvas2DOp::LineTo { x: cx, y: cy + 2.0 });
                self.ops.push(Canvas2DOp::LineTo { x: cx + 4.0, y: cy - 2.0 });
                self.ops.push(Canvas2DOp::Stroke);
            }
            // the resize grip: two dotted diagonals in the bottom-right corner
            ThemePart::Area => {
                self.ops.push(Canvas2DOp::SetFillStyle { color: THEME_EDGE });
                let (rx, by) = ((x + w).floor(), (y + h).floor());
                for k in 0..4 {
                    self.ops.push(Canvas2DOp::FillRect { x: rx - 10.0 + 2.0 * k as f64, y: by - 3.0 - 2.0 * k as f64, w: 1.0, h: 1.0 });
                }
                for k in 0..2 {
                    self.ops.push(Canvas2DOp::FillRect { x: rx - 6.0 + 2.0 * k as f64, y: by - 3.0 - 2.0 * k as f64, w: 1.0, h: 1.0 });
                }
            }
            _ => {}
        }
    }

    /// What a form control's text is, or `None` for a control that draws none (a checkbox, a range, a list box).
    fn control_role(&self, node: NodeId, tag: &str) -> Option<ControlRole> {
        match tag {
            "textarea" => Some(ControlRole::Area),
            "select" => {
                let list = self.dom.get_attribute(node, "multiple").is_some()
                    || self.dom.get_attribute(node, "size").and_then(|s| s.trim().parse::<u32>().ok()).is_some_and(|n| n > 1);
                (!list).then_some(ControlRole::Select)
            }
            _ => match self.dom.get_attribute(node, "type").map(|t| t.trim().to_ascii_lowercase()).as_deref() {
                None | Some("" | "text" | "search" | "email" | "url" | "tel" | "number") => Some(ControlRole::Field),
                Some("password") => Some(ControlRole::Password),
                Some("submit" | "button" | "reset") => Some(ControlRole::Button),
                Some("checkbox") => Some(ControlRole::Check),
                Some("radio") => Some(ControlRole::Radio),
                Some("range") => Some(ControlRole::Range),
                Some("color") => Some(ControlRole::Color),
                Some("file") => Some(ControlRole::File),
                Some("date" | "month" | "week" | "time" | "datetime-local") => Some(ControlRole::DateTime),
                // an unknown type is a text field (HTML's invalid-value default)
                Some(t) if !matches!(t, "hidden" | "image") => Some(ControlRole::Field),
                _ => None,
            },
        }
    }

    /// Control box `index`'s font, colours and baseline. A one-line control centres its line in its content box, as
    /// Blink's inner editor does; a `<textarea>`'s first line starts at the top.
    fn control_text(&self, index: u32, role: ControlRole, bh: f64) -> ControlText {
        let node = self.laid_out.tree.node_of(index);
        let family = self.cvb(index, PropertyId::FontFamily).unwrap_or_else(|| "sans-serif".into());
        let size_px = self.cvb(index, PropertyId::FontSize).map(|s| px(&s)).filter(|&n| n > 0.0).unwrap_or(13.333);
        let bold = self.cvb(index, PropertyId::FontWeight).is_some_and(|w| w == "bold" || w.trim().parse::<u32>().is_ok_and(|n| n >= 600));
        let italic = self.cvb(index, PropertyId::FontStyle).is_some_and(|s| s.trim() != "normal");
        let color = self.cvb(index, PropertyId::Color).as_deref().and_then(parse_rgba).unwrap_or(Rgba::BLACK);
        // a page's `::placeholder` rule, where one gives the placeholder a colour of its own
        let mut placeholder_color = self
            .page
            .computed_pseudo_value(self.dom, node, "placeholder", self.media, PropertyId::Color, Some(self.laid_out))
            .as_deref()
            .and_then(parse_rgba)
            .filter(|c| *c != color)
            .map_or([117.0 / 255.0, 117.0 / 255.0, 117.0 / 255.0, 1.0], |c| c.paint_color());
        // and its `opacity`: GitHub's sign-up field hides its placeholder with `::placeholder { opacity: 0 }` under a
        // label of its own, and the two drew over each other
        if let Some(o) = self
            .page
            .computed_pseudo_value(self.dom, node, "placeholder", self.media, PropertyId::Opacity, Some(self.laid_out))
            .and_then(|o| o.trim().parse::<f32>().ok())
        {
            placeholder_color[3] *= o.clamp(0.0, 1.0);
        }
        let placeholder_bold = self
            .page
            .computed_pseudo_value(self.dom, node, "placeholder", self.media, PropertyId::FontWeight, Some(self.laid_out))
            .map_or(bold, |w| w == "bold" || w.trim().parse::<u32>().is_ok_and(|n| n >= 600));
        // the caret, in a focused control that takes text
        let editable = matches!(role, ControlRole::Field | ControlRole::Password | ControlRole::Area)
            && self.dom.get_attribute(node, "readonly").is_none()
            && self.dom.get_attribute(node, "disabled").is_none();
        let caret = (editable && self.dom.is_focused(node))
            .then(|| self.cvb(index, PropertyId::CaretColor).as_deref().and_then(parse_rgba).unwrap_or(color.clone()).paint_color());
        let metrics = self.laid_out.tree.font_metrics(&family_of(&family), bold, italic);
        let (ascent, descent) = ((metrics.ascent as f64 * size_px).round(), (metrics.descent as f64 * size_px).round());
        let l = self.laid_out.tree.layout(index);
        let top = (l.border.top + l.padding.top) as f64;
        let inner = (bh - top - (l.border.bottom + l.padding.bottom) as f64).max(0.0);
        let rect = self.laid_out.tree.border_box(index);
        let (_, ty) = self.translations[index as usize];
        let content_top = (rect.y + ty) as f64 + top;
        let baseline = match role {
            ControlRole::Area => content_top + ascent,
            _ => content_top + ((inner - ascent - descent) / 2.0).floor() + ascent,
        };
        let accent = self
            .cvb(index, PropertyId::AccentColor)
            .filter(|a| a.trim() != "auto")
            .as_deref()
            .and_then(parse_rgba)
            .map_or(CHROME_ACCENT, |c| c.paint_color());
        let slider = (role == ControlRole::Range).then(|| self.slider_look(node));
        ControlText { role, family, size_px, bold, italic, color: color.paint_color(), placeholder_color, baseline, placeholder_bold, caret, ascent, descent, accent, slider }
    }

    /// An inline `<svg>`'s subtree as standalone SVG markup: every element with the paint the cascade computed for it
    /// (`fill`, `stroke` and the rest, `currentColor` resolved -- korvus computes them as Chrome does, from attributes
    /// and CSS alike), what is `display: none` left out, and the elements its `<use>`s name from elsewhere in the
    /// document (an icon sprite) copied in beside it.
    fn svg_markup(&self, root: NodeId) -> String {
        let mut out = String::new();
        let mut uses = Vec::new();
        let view = self.svg_view_box(root);
        let svg = SvgCtx { view };
        self.svg_element(root, true, &mut out, &mut uses, &svg);
        let mut defs = String::new();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = uses.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(target) = self.dom.get_element_by_id(&id) else { continue };
            if self.dom.get_element_by_id_in_subtree(root, &id).is_some() {
                continue;
            }
            self.svg_element(target, false, &mut defs, &mut uses, &svg);
        }
        if !defs.is_empty() {
            if let Some(i) = out.find('>') {
                out.insert_str(i + 1, &format!("<defs>{defs}</defs>"));
            }
        }
        out
    }

    fn svg_element(&self, node: NodeId, root: bool, out: &mut String, uses: &mut Vec<String>, svg: &SvgCtx) {
        let Some(name) = self.dom.tag_name(node) else { return };
        if matches!(name, "script" | "foreignObject") || self.cv(node, PropertyId::Display).as_deref() == Some("none") {
            return;
        }
        out.push('<');
        out.push_str(name);
        if root {
            out.push_str(" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
        }
        // What the cascade computed in place of the attributes it maps: the geometry (a CSS `r: 30px`, `d: path(...)`,
        // an animated `cx`) and the transform (a CSS `rotate()` about its `transform-origin`, the attribute's own
        // list). The root's transform is its box's, painted as any box's.
        let geometry = self.svg_geometry(node, name);
        let transform = if root { None } else { crate::style::hints::transform_attribute(name) };
        for (k, v) in self.dom.attribute_pairs(node) {
            if k == "style" || k == "xmlns" || k == "xmlns:xlink" || k.starts_with("on") {
                continue;
            }
            if transform == Some(k) || geometry.iter().any(|(g, _)| *g == k) {
                continue;
            }
            if name == "use" && (k == "href" || k == "xlink:href") {
                if let Some(id) = v.strip_prefix('#') {
                    uses.push(id.to_string());
                }
            }
            out.push_str(&format!(" {k}=\"{}\"", xml_escape(v)));
        }
        for (k, v) in &geometry {
            out.push_str(&format!(" {k}=\"{}\"", xml_escape(v)));
        }
        if let Some(attribute) = transform {
            if let Some(m) = self.svg_transform(node, name, svg) {
                let n = crate::style::serialize::format_number;
                out.push_str(&format!(" {attribute}=\"matrix({} {} {} {} {} {})\"", n(m[0]), n(m[1]), n(m[2]), n(m[3]), n(m[4]), n(m[5])));
            }
        }
        let mut style = String::new();
        for (p, css) in [
            (PropertyId::Fill, "fill"),
            (PropertyId::FillOpacity, "fill-opacity"),
            (PropertyId::FillRule, "fill-rule"),
            (PropertyId::Stroke, "stroke"),
            (PropertyId::StrokeWidth, "stroke-width"),
            (PropertyId::StrokeOpacity, "stroke-opacity"),
            (PropertyId::StrokeLinecap, "stroke-linecap"),
            (PropertyId::StrokeLinejoin, "stroke-linejoin"),
            (PropertyId::StrokeDasharray, "stroke-dasharray"),
            (PropertyId::Opacity, "opacity"),
            (PropertyId::Visibility, "visibility"),
        ] {
            if let Some(v) = self.cv(node, p).filter(|v| !v.is_empty()) {
                style.push_str(&format!("{css}:{v};"));
            }
        }
        // The rest where they say something: a value that is not the initial one, or one an inheriting property
        // takes back to it under a parent that set another. A CSS `stroke-dashoffset` (every spinner's animation),
        // a gradient's CSS `stop-color` (black here), `stroke-miterlimit` were never written; the text properties
        // are a text element's.
        let text = matches!(name, "text" | "tspan" | "textPath");
        let parent = self.dom.parent(node).filter(|&p| !root && self.dom.namespace_uri(p) == Some(SVG_NS));
        for (p, css, initial, inherits) in [
            (PropertyId::StrokeDashoffset, "stroke-dashoffset", "0px", true),
            (PropertyId::StrokeMiterlimit, "stroke-miterlimit", "4", true),
            (PropertyId::ClipRule, "clip-rule", "nonzero", true),
            (PropertyId::PaintOrder, "paint-order", "normal", true),
            (PropertyId::StopColor, "stop-color", "rgb(0, 0, 0)", false),
            (PropertyId::StopOpacity, "stop-opacity", "1", false),
            (PropertyId::FloodColor, "flood-color", "rgb(0, 0, 0)", false),
            (PropertyId::FloodOpacity, "flood-opacity", "1", false),
            (PropertyId::LightingColor, "lighting-color", "rgb(255, 255, 255)", false),
            (PropertyId::MixBlendMode, "mix-blend-mode", "normal", false),
            (PropertyId::Isolation, "isolation", "auto", false),
        ] {
            let Some(v) = self.cv(node, p).filter(|v| !v.is_empty()) else { continue };
            let under = inherits && parent.and_then(|q| self.cv(q, p)).is_some_and(|q| q != initial);
            if v != initial || under {
                style.push_str(&format!("{css}:{v};"));
            }
        }
        if text {
            for (p, css) in [
                (PropertyId::FontFamily, "font-family"),
                (PropertyId::FontSize, "font-size"),
                (PropertyId::FontWeight, "font-weight"),
                (PropertyId::FontStyle, "font-style"),
                (PropertyId::TextAnchor, "text-anchor"),
                (PropertyId::DominantBaseline, "dominant-baseline"),
                (PropertyId::LetterSpacing, "letter-spacing"),
                (PropertyId::WordSpacing, "word-spacing"),
            ] {
                if let Some(v) = self.cv(node, p).filter(|v| !v.is_empty()) {
                    style.push_str(&format!("{css}:{v};"));
                }
            }
        }
        if !style.is_empty() {
            out.push_str(&format!(" style=\"{}\"", xml_escape(&style)));
        }
        out.push('>');
        for child in self.dom.children(node) {
            match self.dom.node_type(child) {
                1 if self.dom.namespace_uri(child) == Some(SVG_NS) => self.svg_element(child, false, out, uses, svg),
                3 => out.push_str(&xml_escape(&self.dom.text_content(child))),
                _ => {}
            }
        }
        out.push_str(&format!("</{name}>"));
    }

    /// An inline `<svg>`'s `<text>`, drawn with the text ops HTML text is drawn with, over the SVG's pixels: resvg
    /// is built without text, and every `<text>` -- a chart's labels, a logo's word -- drew nothing (Chrome draws
    /// it). Each text element's runs (its own text, a `<tspan>` with an `x` or `y` of its own starting another) are set
    /// in its font at its position, through its transforms and the root's viewBox mapping, anchored by
    /// `text-anchor`, in its fill. Drawn after the shapes: a shape painted over text in the document's order is
    /// under it here.
    fn svg_text_ops(&mut self, root: NodeId, x: f64, y: f64, w: f64, h: f64) {
        // a tag-only scan first: an SVG with no text (an icon, a Lottie frame of thousands of groups) reads no style
        let mut any = false;
        self.dom.for_each_descendant(root, |n| any |= self.dom.tag_name(n) == Some("text"));
        if w <= 0.0 || h <= 0.0 || !any {
            return;
        }
        let view = self.svg_view_box(root);
        let svg = SvgCtx { view };
        let base = svg_root_mapping(self.dom, root, view, (x, y, w, h));
        let mut texts: Vec<(NodeId, [f64; 6], f64)> = Vec::new();
        self.svg_collect_texts(root, base, 1.0, &svg, &mut texts, 0);
        if texts.is_empty() {
            return;
        }
        self.ops.push(Canvas2DOp::Save);
        self.ops.push(Canvas2DOp::BeginPath);
        self.ops.push(Canvas2DOp::Rect { x, y, w, h });
        self.ops.push(Canvas2DOp::Clip { even_odd: false });
        for (text, m, opacity) in texts {
            self.svg_text_runs(text, m, opacity);
        }
        self.ops.push(Canvas2DOp::Restore);
    }

    /// The `<text>` elements under `node` that draw, each with the matrix from its user space to the page and the
    /// opacity of the groups it is in.
    fn svg_collect_texts(&self, node: NodeId, m: [f64; 6], opacity: f64, svg: &SvgCtx, out: &mut Vec<(NodeId, [f64; 6], f64)>, depth: u32) {
        if depth > 64 {
            return;
        }
        for child in self.dom.children(node) {
            if self.dom.node_type(child) != 1 || self.dom.namespace_uri(child) != Some(SVG_NS) {
                continue;
            }
            let Some(tag) = self.dom.tag_name(child) else { continue };
            if matches!(
                tag,
                "defs" | "symbol" | "clipPath" | "mask" | "pattern" | "marker" | "linearGradient" | "radialGradient" | "filter"
                    | "foreignObject" | "script" | "style" | "title" | "desc" | "metadata" | "textPath"
            ) || self.cv(child, PropertyId::Display).as_deref() == Some("none")
            {
                continue;
            }
            let own = self.svg_transform(child, tag, svg).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            let mut cm = mul_matrix(m, own);
            if tag == "svg" {
                let at = |p: PropertyId| self.cv(child, p).and_then(|v| svg_length_px(&v, 0.0)).unwrap_or(0.0);
                cm = mul_matrix(cm, [1.0, 0.0, 0.0, 1.0, at(PropertyId::X), at(PropertyId::Y)]);
            }
            let alpha = opacity * self.cv(child, PropertyId::Opacity).and_then(|o| o.parse::<f64>().ok()).unwrap_or(1.0);
            if tag == "text" {
                out.push((child, cm, alpha));
            } else if matches!(tag, "g" | "a" | "switch" | "svg") {
                self.svg_collect_texts(child, cm, alpha, svg, out, depth + 1);
            }
        }
    }

    /// One `<text>` element's runs, drawn in its user space `m`.
    fn svg_text_runs(&mut self, text: NodeId, m: [f64; 6], opacity: f64) {
        let first = |node: NodeId, a: &str| -> Option<f64> {
            self.dom.get_attribute(node, a)?.split(|c: char| c == ',' || c.is_ascii_whitespace()).find(|t| !t.is_empty())?.parse().ok()
        };
        let (mut cx, mut cy) = (first(text, "x").unwrap_or(0.0), first(text, "y").unwrap_or(0.0));
        cx += first(text, "dx").unwrap_or(0.0);
        cy += first(text, "dy").unwrap_or(0.0);
        // (x, y, the element whose style it is set in, its characters)
        let mut runs: Vec<(f64, f64, NodeId, String)> = vec![(cx, cy, text, String::new())];
        for child in self.dom.children(text) {
            match self.dom.node_type(child) {
                3 => runs.last_mut().unwrap().3.push_str(&self.dom.text_content(child)),
                1 if self.dom.tag_name(child) == Some("tspan") && self.cv(child, PropertyId::Display).as_deref() != Some("none") => {
                    let (nx, ny) = (first(child, "x"), first(child, "y"));
                    if nx.is_some() || ny.is_some() {
                        cx = nx.unwrap_or(cx) + first(child, "dx").unwrap_or(0.0);
                        cy = ny.unwrap_or(cy) + first(child, "dy").unwrap_or(0.0);
                        runs.push((cx, cy, child, self.dom.text_content(child)));
                    } else {
                        runs.last_mut().unwrap().3.push_str(&self.dom.text_content(child));
                    }
                }
                _ => {}
            }
        }
        for (rx, ry, styled, chars) in runs {
            let chars = chars.split_whitespace().collect::<Vec<_>>().join(" ");
            if chars.is_empty() || self.cv(styled, PropertyId::Visibility).is_some_and(|v| v != "visible") {
                continue;
            }
            let Some(mut fill) = self.cv(styled, PropertyId::Fill).as_deref().and_then(parse_rgba) else { continue };
            let fill_opacity = self.cv(styled, PropertyId::FillOpacity).and_then(|o| o.parse::<f64>().ok()).unwrap_or(1.0);
            fill.a *= (opacity * fill_opacity) as f32;
            if fill.a <= 0.0 {
                continue;
            }
            let size_px = self.cv(styled, PropertyId::FontSize).and_then(|s| svg_length_px(&s, 0.0)).filter(|&n| n > 0.0).unwrap_or(16.0);
            let weight = self.cv(styled, PropertyId::FontWeight).map_or(400, |w| match w.trim() {
                "bold" => 700,
                w => w.parse::<f32>().map_or(400, |n| n.round().clamp(1.0, 1000.0) as u16),
            });
            let italic = self.cv(styled, PropertyId::FontStyle).is_some_and(|s| s.trim() != "normal");
            let align = match self.cv(styled, PropertyId::TextAnchor).as_deref() {
                Some("middle") => spec::TextAlign::Center,
                Some("end") => spec::TextAlign::Right,
                _ => spec::TextAlign::Left,
            };
            let baseline = match self.cv(text, PropertyId::DominantBaseline).as_deref() {
                Some("middle" | "central") => spec::TextBaseline::Middle,
                Some("hanging") => spec::TextBaseline::Hanging,
                Some("text-before-edge" | "text-top") => spec::TextBaseline::Top,
                Some("text-after-edge" | "text-bottom") => spec::TextBaseline::Bottom,
                Some("ideographic") => spec::TextBaseline::Ideographic,
                _ => spec::TextBaseline::Alphabetic,
            };
            self.ops.push(Canvas2DOp::Save);
            self.ops.push(Canvas2DOp::Transform { t: spec::Transform2D { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] } });
            self.ops.push(Canvas2DOp::SetFont {
                family: self.cv(styled, PropertyId::FontFamily).unwrap_or_else(|| "sans-serif".into()),
                size_px,
                bold: weight >= 600,
                italic,
                weight,
                families: String::new(),
                lang: String::new(),
                default_family: String::new(),
            });
            self.ops.push(Canvas2DOp::SetTextAlign { align });
            self.ops.push(Canvas2DOp::SetTextBaseline { baseline });
            self.ops.push(Canvas2DOp::SetFillStyle { color: fill.paint_color() });
            self.ops.push(Canvas2DOp::FillText { text: chars, x: rx, y: ry, max_width: None });
            self.ops.push(Canvas2DOp::Restore);
        }
    }

    /// The root `<svg>`'s user space, as (x, y, width, height): its `viewBox`, else its own width and height -- what a
    /// `view-box` reference box is and what a percentage in it is of.
    fn svg_view_box(&self, root: NodeId) -> [f64; 4] {
        if let Some(vb) = self.dom.get_attribute(root, "viewBox") {
            let n: Vec<f64> = vb.split(|c: char| c == ',' || c.is_ascii_whitespace()).filter_map(|t| t.parse().ok()).collect();
            if let [x, y, w, h] = n[..] {
                if w > 0.0 && h > 0.0 {
                    return [x, y, w, h];
                }
            }
        }
        let size = self.laid_out.tree.box_of(root).map(|b| self.laid_out.tree.content_box(b));
        let (w, h) = size.map_or((300.0, 150.0), |r| (r.width as f64, r.height as f64));
        [0.0, 0.0, w, h]
    }

    /// The geometry attributes to write for `node` from its computed values ([`crate::style::hints`] maps the
    /// attributes to them): each whose attribute is there or whose value CSS moved from the attribute-less one, as an
    /// SVG attribute takes it. One computed as a `calc()` is left to its attribute.
    fn svg_geometry(&self, node: NodeId, tag: &str) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for (name, p, none) in [
            ("x", PropertyId::X, "0px"),
            ("y", PropertyId::Y, "0px"),
            ("width", PropertyId::Width, "0px"),
            ("height", PropertyId::Height, "0px"),
            ("cx", PropertyId::Cx, "0px"),
            ("cy", PropertyId::Cy, "0px"),
            ("r", PropertyId::R, "0px"),
            ("rx", PropertyId::Rx, "auto"),
            ("ry", PropertyId::Ry, "auto"),
            ("d", PropertyId::D, "none"),
        ] {
            if !crate::style::hints::is_svg_geometry_attribute(tag, name) {
                continue;
            }
            let Some(v) = self.cv(node, p) else { continue };
            if v == none && self.dom.get_attribute(node, name).is_none() {
                continue;
            }
            let value = if name == "d" {
                match v.strip_prefix("path(").and_then(|r| r.strip_suffix(')')) {
                    Some(inner) => inner.trim().trim_matches(|c| c == '"' || c == '\'').to_string(),
                    None => continue,
                }
            } else if v.contains('(') || v == "auto" {
                continue;
            } else {
                v
            };
            out.push((name, value));
        }
        out
    }

    /// `node`'s computed `transform` as the SVG matrix it draws with: about its `transform-origin`, resolved in its
    /// `transform-box` -- the root's user space (`view-box`, the initial value) or the element's own bounding box
    /// (`fill-box`; `stroke-box`, `content-box` and `border-box` taken as it). `None` for none.
    fn svg_transform(&self, node: NodeId, tag: &str, svg: &SvgCtx) -> Option<[f64; 6]> {
        let list = self.cv(node, PropertyId::Transform).filter(|t| t != "none" && !t.is_empty())?;
        let origin = self.cv(node, PropertyId::TransformOrigin).unwrap_or_default();
        let boxed = self.cv(node, PropertyId::TransformBox).is_some_and(|b| b != "view-box");
        // a `view-box` reference box is the viewBox's size at the user space's origin (CSS Transforms 1 §7)
        let reference = if boxed { self.svg_bbox(node, tag, svg, 0).unwrap_or([0.0; 4]) } else { [0.0, 0.0, svg.view[2], svg.view[3]] };
        // an element with no CSS box computes its functions as written: they are read here, a `translate()`'s
        // percentages of the reference box (Chrome 150: `translate(50%, 10%)` in a 200 x 100 viewBox is 100, 10)
        let m = match parse_matrix(&list) {
            Some(m) => m,
            None => {
                let font = self.cv(node, PropertyId::FontSize).and_then(|f| svg_length_px(&f, 0.0)).unwrap_or(16.0);
                crate::effects::transform_list_matrix(&list, reference[2], reference[3], font, (self.viewport.0 as f64, self.viewport.1 as f64))
            }
        };
        let (ox, oy) = resolve_origin(&origin, reference);
        // translate(origin) . M . translate(-origin)
        let [a, b, c, d, e, f] = m;
        Some([a, b, c, d, ox + e - (a * ox + c * oy), oy + f - (b * ox + d * oy)])
    }

    /// An SVG element's bounding box in its own user space, as (x, y, width, height): its geometry's, its children's
    /// (each through its own transform) for a container. `None` for an element with no geometry this reads (text).
    fn svg_bbox(&self, node: NodeId, tag: &str, svg: &SvgCtx, depth: u32) -> Option<[f64; 4]> {
        let px = |p: PropertyId, of: f64| self.cv(node, p).and_then(|v| svg_length_px(&v, of));
        let attr = |n: &str| self.dom.get_attribute(node, n).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
        let [_, _, vw, vh] = svg.view;
        let mut bounds = Bounds::default();
        match tag {
            "rect" | "image" | "use" | "foreignObject" => {
                let (x, y) = (px(PropertyId::X, vw).unwrap_or(0.0), px(PropertyId::Y, vh).unwrap_or(0.0));
                let (w, h) = (px(PropertyId::Width, vw).unwrap_or(0.0), px(PropertyId::Height, vh).unwrap_or(0.0));
                return Some([x, y, w, h]);
            }
            "circle" => {
                let (cx, cy, r) = (px(PropertyId::Cx, vw)?, px(PropertyId::Cy, vh)?, px(PropertyId::R, vw.hypot(vh) / 2f64.sqrt())?);
                return Some([cx - r, cy - r, 2.0 * r, 2.0 * r]);
            }
            "ellipse" => {
                let (cx, cy) = (px(PropertyId::Cx, vw)?, px(PropertyId::Cy, vh)?);
                let (rx, ry) = (px(PropertyId::Rx, vw), px(PropertyId::Ry, vh));
                let (rx, ry) = (rx.or(ry).unwrap_or(0.0), ry.or(rx).unwrap_or(0.0));
                return Some([cx - rx, cy - ry, 2.0 * rx, 2.0 * ry]);
            }
            "line" => {
                bounds.add(attr("x1"), attr("y1"));
                bounds.add(attr("x2"), attr("y2"));
            }
            "polyline" | "polygon" => {
                for (x, y) in svgtypes::PointsParser::from(self.dom.get_attribute(node, "points").unwrap_or("")) {
                    bounds.add(x, y);
                }
            }
            "path" => {
                let d = self.cv(node, PropertyId::D).filter(|d| d != "none");
                let data = match &d {
                    Some(d) => d.strip_prefix("path(").and_then(|r| r.strip_suffix(')')).unwrap_or("").trim().trim_matches('"').to_string(),
                    None => self.dom.get_attribute(node, "d").unwrap_or("").to_string(),
                };
                use svgtypes::SimplePathSegment as S;
                for seg in svgtypes::SimplifyingPathParser::from(data.as_str()) {
                    match seg {
                        Ok(S::MoveTo { x, y } | S::LineTo { x, y }) => bounds.add(x, y),
                        Ok(S::CurveTo { x1, y1, x2, y2, x, y }) => {
                            bounds.add(x1, y1);
                            bounds.add(x2, y2);
                            bounds.add(x, y);
                        }
                        Ok(S::Quadratic { x1, y1, x, y }) => {
                            bounds.add(x1, y1);
                            bounds.add(x, y);
                        }
                        Ok(S::ClosePath) => {}
                        Err(_) => break,
                    }
                }
            }
            "g" | "a" | "switch" | "svg" if depth < 32 => {
                for child in self.dom.children(node) {
                    if self.dom.node_type(child) != 1 || self.dom.namespace_uri(child) != Some(SVG_NS) {
                        continue;
                    }
                    let Some(child_tag) = self.dom.tag_name(child) else { continue };
                    let Some([x, y, w, h]) = self.svg_bbox(child, child_tag, svg, depth + 1) else { continue };
                    let m = self.svg_transform(child, child_tag, svg).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                    for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                        bounds.add(m[0] * px + m[2] * py + m[4], m[1] * px + m[3] * py + m[5]);
                    }
                }
            }
            _ => return None,
        }
        bounds.rect()
    }

    /// The fills a box's text takes from the nearest element at or above it whose background is clipped to its text
    /// (`background-clip: text`, the gradient headings of every landing page): its background colour, then each
    /// gradient layer laid over that element's border box, bottom first. Empty when there is none.
    fn text_backgrounds(&mut self, node: NodeId) -> Vec<Vec<Canvas2DOp>> {
        let mut at = Some(node);
        let mut owner = None;
        for _ in 0..16 {
            let Some(el) = at.filter(|&n| self.dom.node_type(n) == 1) else { break };
            if self.cv(el, PropertyId::BackgroundClip).is_some_and(|c| c.split(',').any(|l| l.trim() == "text")) {
                owner = Some(el);
                break;
            }
            at = self.dom.parent(el);
        }
        let Some(el) = owner else { return Vec::new() };
        let Some(b) = self.laid_out.tree.box_of(el) else { return Vec::new() };
        let r = self.laid_out.tree.border_box(b);
        let (tx, ty) = self.translations[b as usize];
        let (x, y, w, h) = ((r.x + tx) as f64, (r.y + ty) as f64, r.width as f64, r.height as f64);
        let mut fills = Vec::new();
        if let Some(c) = bg_color(self.page, self.dom, self.media, self.laid_out, el) {
            fills.push(vec![Canvas2DOp::SetFillStyle { color: c.paint_color() }]);
        }
        if let Some(images) = self.cv(el, PropertyId::BackgroundImage).filter(|s| s != "none") {
            for layer in split_top_commas(&images).iter().rev() {
                if let Some(mut g) = linear_gradient_ops(layer.trim(), x, y, w, h, self.grad_id) {
                    self.grad_id += 1;
                    g.pop(); // its `FillRect`: the text is what it fills (a linear gradient's; the others draw under a transform)
                    fills.push(g);
                }
            }
        }
        fills
    }

    /// The box `background-origin` / `background-clip` names: the border box, the padding box (the default origin),
    /// or the content box.
    fn background_box(&self, index: u32, which: &str, border: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let b = |p| self.cvb(index, p).map(|s| px(&s)).unwrap_or(0.0);
        let (x, y, w, h) = border;
        let inset = |l: f64, t: f64, r: f64, bo: f64| (x + l, y + t, (w - l - r).max(0.0), (h - t - bo).max(0.0));
        let (bl, bt, br, bb) = (b(PropertyId::BorderLeftWidth), b(PropertyId::BorderTopWidth), b(PropertyId::BorderRightWidth), b(PropertyId::BorderBottomWidth));
        match which.trim() {
            "border-box" => border,
            "content-box" => {
                let (pl, pt, pr, pb) = (b(PropertyId::PaddingLeft), b(PropertyId::PaddingTop), b(PropertyId::PaddingRight), b(PropertyId::PaddingBottom));
                inset(bl + pl, bt + pt, br + pr, bb + pb)
            }
            _ => inset(bl, bt, br, bb),
        }
    }

    /// Block `root`'s lines that end in an ellipsis: with `text-overflow: ellipsis` and an `overflow` other than
    /// `visible`, each line whose content passes the content box's right edge is cut where the "…" still fits before
    /// it; with a line clamp, the clamp's last line ends in one when more lines follow -- after its text when there is
    /// room, at the same cut when there is not (Blink's `NGLineTruncator`). Empty for a block that cuts nothing.
    fn line_cuts(&mut self, root: u32) -> std::rc::Rc<Vec<CutLine>> {
        if let Some(hit) = self.cuts.get(&root) {
            return hit.clone();
        }
        let made = std::rc::Rc::new(self.make_line_cuts(root));
        self.cuts.insert(root, made.clone());
        made
    }

    fn make_line_cuts(&self, root: u32) -> Vec<CutLine> {
        let tree = &self.laid_out.tree;
        let edge = self.cvb(root, PropertyId::TextOverflow).is_some_and(|v| v.trim() == "ellipsis")
            && self.cvb(root, PropertyId::OverflowX).is_some_and(|v| !matches!(v.trim(), "" | "visible"));
        let clamp = tree.line_clamp_of(root) as usize;
        if !edge && clamp == 0 {
            return Vec::new();
        }
        // the fragments of the texts the block lays out, grouped into lines by their vertical overlap
        let mut frags: Vec<(u32, crate::Rect)> = Vec::new();
        let mut b = root + 1;
        while (b as usize) < tree.len() && self.descends(b, root) {
            if tree.ifc_of(b) == Some(root) {
                frags.extend(tree.text_fragments(b, &self.laid_out.scroll).into_iter().map(|(_, r)| (b, r)));
            }
            b += 1;
        }
        let mut lines: Vec<Vec<(u32, crate::Rect)>> = Vec::new();
        frags.sort_by(|a, b| a.1.y.partial_cmp(&b.1.y).unwrap_or(std::cmp::Ordering::Equal));
        for f in frags {
            let centre = f.1.y + f.1.height / 2.0;
            match lines.iter_mut().find(|l| l.iter().any(|(_, r)| (r.y + r.height / 2.0 - centre).abs() < r.height.min(f.1.height) / 2.0)) {
                Some(line) => line.push(f),
                None => lines.push(vec![f]),
            }
        }
        for l in &mut lines {
            l.sort_by(|a, b| a.1.x.partial_cmp(&b.1.x).unwrap_or(std::cmp::Ordering::Equal));
        }
        // the content box's right edge, and the "…" in the block's font
        let r = tree.border_box(root);
        let l = tree.layout(root);
        let (tx, _) = self.translations[root as usize];
        let right = (r.x + tx + r.width - l.border.right - l.padding.right) as f64;
        let size = self.cvb(root, PropertyId::FontSize).map(|s| px(&s)).filter(|&n| n > 0.0).unwrap_or(16.0);
        let weight = self.cvb(root, PropertyId::FontWeight).map_or(400, |w| w.trim().parse::<f32>().map_or(if w.trim() == "bold" { 700 } else { 400 }, |n| n.round() as u16));
        let italic = self.cvb(root, PropertyId::FontStyle).is_some_and(|s| s.trim() != "normal");
        let metrics = tree.font_metrics_at(&family_of(&self.cvb(root, PropertyId::FontFamily).unwrap_or_default()), weight, italic, size as f32);
        let ellipsis = metrics.width_px_kerned("\u{2026}", size as f32, true) as f64;
        let cut_at = |line: &Vec<(u32, crate::Rect)>, cut: f64| -> CutLine {
            // the fragment the cut falls in, else the last one starting before it, carries the "…"
            let host = line.iter().rev().find(|(_, r)| (r.x as f64) < cut).or(line.first()).map(|f| f.0).unwrap_or(root);
            let (top, bottom) = line_extent(line);
            CutLine { top, bottom, cut: Some(cut), host }
        };
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let end = line.iter().map(|(_, r)| (r.x + r.width) as f64).fold(f64::MIN, f64::max);
            if clamp > 0 && i + 1 == clamp && lines.len() > clamp {
                out.push(if end + ellipsis <= right + 0.01 {
                    let (top, bottom) = line_extent(line);
                    CutLine { top, bottom, cut: None, host: line.last().map(|f| f.0).unwrap_or(root) }
                } else {
                    cut_at(line, right - ellipsis)
                });
            } else if edge && end > right + 0.01 {
                out.push(cut_at(line, right - ellipsis));
            }
        }
        out
    }

    /// Is box `b` inside box `root`'s subtree?
    fn descends(&self, b: u32, root: u32) -> bool {
        let mut up = self.laid_out.tree.parent_of(b);
        while let Some(p) = up {
            if p == root {
                return true;
            }
            if p < root {
                return false;
            }
            up = self.laid_out.tree.parent_of(p);
        }
        false
    }

    /// Box `b`'s `mask-image` layers, the last listed first (drawn lowest), each with its painting area: placed as a
    /// background's are, from `mask-origin`, `mask-clip`, `mask-size`, `mask-position` (Blink's
    /// `-webkit-mask-position-x` and `-y`, which it is the shorthand of) and `mask-repeat` -- whose boxes default to
    /// the border box.
    fn mask_layers(&mut self, b: u32) -> (NodeId, Vec<(LayerPaint, (f64, f64, f64, f64))>) {
        let tree = &self.laid_out.tree;
        let node = tree.node_of(b);
        let rect = tree.border_box(b);
        let (tx, ty) = self.translations[b as usize];
        let border = ((rect.x + tx) as f64, (rect.y + ty) as f64, rect.width as f64, rect.height as f64);
        let list = |p| self.cvb(b, p).map(|s| split_top_commas(&s)).unwrap_or_default();
        let (images, origins, clips, sizes, xs, ys, repeats) = (
            list(PropertyId::MaskImage),
            list(PropertyId::MaskOrigin),
            list(PropertyId::MaskClip),
            list(PropertyId::MaskSize),
            list(PropertyId::WebkitMaskPositionX),
            list(PropertyId::WebkitMaskPositionY),
            list(PropertyId::MaskRepeat),
        );
        let nth = |v: &Vec<String>, i: usize, d: &str| if v.is_empty() { d.to_string() } else { v[i % v.len()].trim().to_string() };
        // `mask-origin` and `mask-clip` name a box; any but the padding and content boxes is the border box here
        let area = |this: &Self, which: String| match which.as_str() {
            "padding-box" | "content-box" => this.background_box(b, &which, border),
            _ => border,
        };
        let radii = self.radii(b, border.2, border.3);
        let mut layers = Vec::new();
        for (i, image) in images.iter().enumerate().rev() {
            let (origin, clip) = (area(self, nth(&origins, i, "border-box")), area(self, nth(&clips, i, "border-box")));
            let (x, y) = (nth(&xs, i, "0%"), nth(&ys, i, "0%"));
            let placed = ImageLayer {
                image,
                mask: true,
                origin,
                clip,
                radii: radii.filter(|_| clip == border),
                size: nth(&sizes, i, "auto"),
                x,
                y,
                repeat: nth(&repeats, i, "repeat"),
            };
            if let Some(paint) = layer_paint(placed, &mut self.grad_id) {
                layers.push((paint, clip));
            }
        }
        (node, layers)
    }

    /// The box's corner radii (top-left, top-right, bottom-right, bottom-left), each `(horizontal, vertical)`, in px,
    /// scaled down where they overlap as CSS does; `None` when every corner is square.
    fn radii(&self, index: u32, w: f64, h: f64) -> Option<[(f64, f64); 4]> {
        let corner = |p| {
            let s = self.cvb(index, p).unwrap_or_default();
            let mut parts = s.split_whitespace();
            let first = parts.next();
            let rx = first.map(|v| length_of(v, w)).unwrap_or(0.0);
            // one value is both radii, each resolved on its own axis: `50%` of a wide box is an ellipse
            let ry = parts.next().or(first).map(|v| length_of(v, h)).unwrap_or(0.0);
            (rx.max(0.0), ry.max(0.0))
        };
        let mut r = [
            corner(PropertyId::BorderTopLeftRadius),
            corner(PropertyId::BorderTopRightRadius),
            corner(PropertyId::BorderBottomRightRadius),
            corner(PropertyId::BorderBottomLeftRadius),
        ];
        if r.iter().all(|&(a, b)| a <= 0.0 || b <= 0.0) {
            return None;
        }
        // CSS Backgrounds §5.5: radii that would overlap are all scaled by the same factor
        let f = [w / (r[0].0 + r[1].0), w / (r[3].0 + r[2].0), h / (r[0].1 + r[3].1), h / (r[1].1 + r[2].1)]
            .into_iter()
            .filter(|v| v.is_finite())
            .fold(1.0_f64, f64::min);
        if f < 1.0 {
            for c in &mut r {
                c.0 *= f;
                c.1 *= f;
            }
        }
        Some(r)
    }

    /// Fill the box with the current fill style, inside its rounded corners.
    fn fill_box(&mut self, x: f64, y: f64, w: f64, h: f64, radii: Option<[(f64, f64); 4]>) {
        match radii {
            None => self.ops.push(Canvas2DOp::FillRect { x, y, w, h }),
            Some(r) => {
                self.ops.push(Canvas2DOp::BeginPath);
                self.ops.push(Canvas2DOp::RoundRect { x, y, w, h, radii: r });
                self.ops.push(Canvas2DOp::Fill { even_odd: false });
            }
        }
    }

    /// Clip to the box, inside its rounded corners (the caller saves and restores).
    fn clip_box(&mut self, x: f64, y: f64, w: f64, h: f64, radii: Option<[(f64, f64); 4]>) {
        self.ops.extend(clip_ops(x, y, w, h, radii));
    }

    /// The four borders. Square: one rect per side (solid; mitred joins for differing colours are a later
    /// refinement). Rounded: the ring between the border box's corners and the padding box's, each side's colour
    /// filling its part of it.
    fn borders(&mut self, index: u32, (x, y, bw, bh): (f64, f64, f64, f64), radii: Option<[(f64, f64); 4]>) {
        let mut sides = Vec::with_capacity(4);
        let mut styles = [BorderStyle::Solid; 4];
        for (i, (wp, cp, sp, side)) in [
            (PropertyId::BorderTopWidth, PropertyId::BorderTopColor, PropertyId::BorderTopStyle, Side::Top),
            (PropertyId::BorderRightWidth, PropertyId::BorderRightColor, PropertyId::BorderRightStyle, Side::Right),
            (PropertyId::BorderBottomWidth, PropertyId::BorderBottomColor, PropertyId::BorderBottomStyle, Side::Bottom),
            (PropertyId::BorderLeftWidth, PropertyId::BorderLeftColor, PropertyId::BorderLeftStyle, Side::Left),
        ]
        .into_iter()
        .enumerate()
        {
            // The used border-width is 0 when the style is none/hidden, so a positive computed width means the side
            // paints.
            let w = self.cvb(index, wp).map(|s| px(&s)).unwrap_or(0.0);
            let style = BorderStyle::of(&self.cvb(index, sp).unwrap_or_default());
            // `inset` and `outset` darken one pair of sides (Blink's `CalculateBorderStyleColor`)
            let c = self.cvb(index, cp).as_deref().and_then(parse_rgba).filter(|c| c.a > 0.0).map(|c| style.side_color(side, hr_rule_color(self.dom, self.laid_out.tree.node_of(index), style, w, c)));
            styles[i] = style;
            sides.push((side, w, c));
        }
        if sides.iter().all(|(_, w, c)| *w <= 0.0 || c.is_none()) {
            return;
        }
        let width = |s: usize| sides[s].1.max(0.0);
        let (wt, wr, wb, wl) = (width(0), width(1), width(2), width(3));
        if let Some(r) = radii {
            let inner = [
                ((r[0].0 - wl).max(0.0), (r[0].1 - wt).max(0.0)),
                ((r[1].0 - wr).max(0.0), (r[1].1 - wt).max(0.0)),
                ((r[2].0 - wr).max(0.0), (r[2].1 - wb).max(0.0)),
                ((r[3].0 - wl).max(0.0), (r[3].1 - wb).max(0.0)),
            ];
            let (ix, iy, iw, ih) = (x + wl, y + wt, (bw - wl - wr).max(0.0), (bh - wt - wb).max(0.0));
            let uniform = sides.iter().all(|(_, w, c)| *w > 0.0 && c.is_some() && *c == sides[0].2);
            self.ops.push(Canvas2DOp::Save);
            self.ops.push(Canvas2DOp::BeginPath);
            self.ops.push(Canvas2DOp::RoundRect { x, y, w: bw, h: bh, radii: r });
            self.ops.push(Canvas2DOp::RoundRect { x: ix, y: iy, w: iw, h: ih, radii: inner });
            if uniform {
                self.ops.push(Canvas2DOp::SetFillStyle { color: sides[0].2.unwrap().paint_color() });
                self.ops.push(Canvas2DOp::Fill { even_odd: true });
            } else {
                // each side's colour over its half of the ring: the ring clips, the sides' rects (out to the corners'
                // diagonals) fill
                self.ops.push(Canvas2DOp::Clip { even_odd: true });
                for (side, w, c) in &sides {
                    let Some(c) = c.filter(|_| *w > 0.0) else { continue };
                    let (cx, cy) = (x + bw / 2.0, y + bh / 2.0);
                    let quad = match side {
                        Side::Top => [(x, y), (x + bw, y), (cx, cy)],
                        Side::Right => [(x + bw, y), (x + bw, y + bh), (cx, cy)],
                        Side::Bottom => [(x + bw, y + bh), (x, y + bh), (cx, cy)],
                        Side::Left => [(x, y + bh), (x, y), (cx, cy)],
                    };
                    self.ops.push(Canvas2DOp::SetFillStyle { color: c.paint_color() });
                    self.ops.push(Canvas2DOp::BeginPath);
                    self.ops.push(Canvas2DOp::MoveTo { x: quad[0].0, y: quad[0].1 });
                    self.ops.push(Canvas2DOp::LineTo { x: quad[1].0, y: quad[1].1 });
                    self.ops.push(Canvas2DOp::LineTo { x: quad[2].0, y: quad[2].1 });
                    self.ops.push(Canvas2DOp::ClosePath);
                    self.ops.push(Canvas2DOp::Fill { even_odd: false });
                }
            }
            self.ops.push(Canvas2DOp::Restore);
            return;
        }
        // One colour all round in one style: one ring, with no seams at the corners.
        let solid = styles.iter().all(|s| *s == BorderStyle::Solid);
        if solid && sides.iter().all(|(_, w, c)| *w > 0.0 && c.is_some() && *c == sides[0].2) {
            self.ops.push(Canvas2DOp::SetFillStyle { color: sides[0].2.unwrap().paint_color() });
            self.ops.push(Canvas2DOp::BeginPath);
            self.ops.push(Canvas2DOp::Rect { x, y, w: bw, h: bh });
            self.ops.push(Canvas2DOp::Rect { x: x + wl, y: y + wt, w: (bw - wl - wr).max(0.0), h: (bh - wt - wb).max(0.0) });
            self.ops.push(Canvas2DOp::Fill { even_odd: true });
            return;
        }
        // Each side its own trapezoid, mitred at the corners as Chrome splits them; a style draws bands of it.
        let insets = [wt, wr, wb, wl];
        for (i, (side, w, c)) in sides.into_iter().enumerate() {
            let Some(col) = c.filter(|_| w > 0.0) else { continue };
            // the quad between fractions f0 and f1 of the way in from the outer edge
            let band = |f0: f64, f1: f64| -> [(f64, f64); 4] {
                let [t, r, b, l] = insets;
                match side {
                    Side::Top => [(x + l * f0, y + t * f0), (x + bw - r * f0, y + t * f0), (x + bw - r * f1, y + t * f1), (x + l * f1, y + t * f1)],
                    Side::Bottom => [(x + l * f0, y + bh - b * f0), (x + bw - r * f0, y + bh - b * f0), (x + bw - r * f1, y + bh - b * f1), (x + l * f1, y + bh - b * f1)],
                    Side::Left => [(x + l * f0, y + t * f0), (x + l * f0, y + bh - b * f0), (x + l * f1, y + bh - b * f1), (x + l * f1, y + t * f1)],
                    Side::Right => [(x + bw - r * f0, y + t * f0), (x + bw - r * f0, y + bh - b * f0), (x + bw - r * f1, y + bh - b * f1), (x + bw - r * f1, y + t * f1)],
                }
            };
            let style = styles[i];
            match style {
                BorderStyle::Groove | BorderStyle::Ridge => {
                    // the outer half as an inset (a groove's) or an outset, the inner half the other way
                    let (outer, inner) = if style == BorderStyle::Groove { (BorderStyle::Inset, BorderStyle::Outset) } else { (BorderStyle::Outset, BorderStyle::Inset) };
                    let base = c.unwrap();
                    self.fill_quad(band(0.0, 0.5), outer.side_color(side, base.clone()));
                    self.fill_quad(band(0.5, 1.0), inner.side_color(side, base));
                }
                BorderStyle::Double if w >= 3.0 => {
                    let third = ((w + 1.0) / 3.0).floor() / w;
                    self.fill_quad(band(0.0, third), col.clone());
                    self.fill_quad(band(1.0 - third, 1.0), col);
                }
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    // dashes twice the width (dots one width) apart by one width, spread so each end has one
                    let (len, along_x) = match side {
                        Side::Top | Side::Bottom => (bw, true),
                        Side::Left | Side::Right => (bh, false),
                    };
                    let dash = if style == BorderStyle::Dashed { 2.0 * w } else { w };
                    let n = (((len + w) / (dash + w)).floor()).max(1.0);
                    let gap = if n > 1.0 { (len - n * dash) / (n - 1.0) } else { 0.0 };
                    let (sx, sy) = match side {
                        Side::Top | Side::Left => (x, y),
                        Side::Bottom => (x, y + bh - w),
                        Side::Right => (x + bw - w, y),
                    };
                    self.ops.push(Canvas2DOp::SetFillStyle { color: col.paint_color() });
                    for k in 0..n as usize {
                        let at = k as f64 * (dash + gap);
                        let (dx, dy, dw, dh) = if along_x { (sx + at, sy, dash.min(len), w) } else { (sx, sy + at, w, dash.min(len)) };
                        if style == BorderStyle::Dotted && w >= 3.0 {
                            self.ops.push(Canvas2DOp::BeginPath);
                            self.ops.push(Canvas2DOp::RoundRect { x: dx, y: dy, w: dw, h: dh, radii: [(w / 2.0, w / 2.0); 4] });
                            self.ops.push(Canvas2DOp::Fill { even_odd: false });
                        } else {
                            self.ops.push(Canvas2DOp::FillRect { x: dx, y: dy, w: dw, h: dh });
                        }
                    }
                }
                _ => self.fill_quad(band(0.0, 1.0), col),
            }
        }
    }

    /// Fill a quadrilateral with one colour.
    fn fill_quad(&mut self, q: [(f64, f64); 4], color: Rgba) {
        self.ops.push(Canvas2DOp::SetFillStyle { color: color.paint_color() });
        self.ops.push(Canvas2DOp::BeginPath);
        self.ops.push(Canvas2DOp::MoveTo { x: q[0].0, y: q[0].1 });
        for &(px_, py_) in &q[1..] {
            self.ops.push(Canvas2DOp::LineTo { x: px_, y: py_ });
        }
        self.ops.push(Canvas2DOp::ClosePath);
        self.ops.push(Canvas2DOp::Fill { even_odd: false });
    }

    /// `box-shadow`'s outer shadows, the first listed on top: each the box's shape moved, grown by its spread and
    /// blurred, never showing under the box itself. (An `inset` shadow is a later step.)
    fn box_shadows(&mut self, list: &str, (x, y, w, h): (f64, f64, f64, f64), radii: Option<[(f64, f64); 4]>, current: Rgba) {
        for shadow in split_top_commas(list).iter().rev() {
            let Some(s) = parse_shadow(shadow, current) else { continue };
            if s.inset || s.color.a <= 0.0 {
                continue;
            }
            let grow = |r: [(f64, f64); 4]| r.map(|(a, b)| ((a + s.spread).max(0.0), (b + s.spread).max(0.0)));
            let (sx, sy, sw, sh) = (x + s.dx - s.spread, y + s.dy - s.spread, w + 2.0 * s.spread, h + 2.0 * s.spread);
            if sw <= 0.0 || sh <= 0.0 {
                continue;
            }
            let reach = s.blur * 2.0 + s.spread.abs() + s.dx.abs() + s.dy.abs() + 8.0;
            self.ops.push(Canvas2DOp::Save);
            // everything around the box, never under it
            self.ops.push(Canvas2DOp::BeginPath);
            self.ops.push(Canvas2DOp::Rect { x: x - reach, y: y - reach, w: w + 2.0 * reach, h: h + 2.0 * reach });
            match radii {
                Some(r) => self.ops.push(Canvas2DOp::RoundRect { x, y, w, h, radii: r }),
                None => self.ops.push(Canvas2DOp::Rect { x, y, w, h }),
            }
            self.ops.push(Canvas2DOp::Clip { even_odd: true });
            if s.blur > 0.0 {
                // a blur radius of B is a Gaussian of standard deviation B / 2 (CSS Backgrounds §7.1)
                self.ops.push(Canvas2DOp::SetFilter { filters: vec![spec::CanvasFilter::Blur { sigma: s.blur / 2.0 }] });
            }
            self.ops.push(Canvas2DOp::SetFillStyle { color: s.color.paint_color() });
            self.fill_box(sx, sy, sw, sh, radii.map(grow));
            self.ops.push(Canvas2DOp::Restore);
        }
    }

    /// The text decorations a box's text carries: its own `text-decoration-line` and those its ancestors propagate to
    /// it, up to the first that is a float, positioned, or a stacking context (they do not propagate into those).
    fn decorations(&self, node: NodeId) -> Vec<(Decoration, Rgba, Option<f64>)> {
        let mut out = Vec::new();
        let mut at = Some(node);
        while let Some(el) = at.filter(|&n| self.dom.node_type(n) == 1) {
            if let Some(line) = self.cv(el, PropertyId::TextDecorationLine).filter(|l| l != "none") {
                let color = self
                    .cv(el, PropertyId::TextDecorationColor)
                    .as_deref()
                    .and_then(parse_rgba)
                    .or_else(|| self.cv(el, PropertyId::Color).as_deref().and_then(parse_rgba))
                    .unwrap_or(Rgba::BLACK);
                let thickness = self.cv(el, PropertyId::TextDecorationThickness).map(|t| px(&t)).filter(|&t| t > 0.0);
                for word in line.split_whitespace() {
                    let kind = match word {
                        "underline" => Decoration::Underline,
                        "line-through" => Decoration::LineThrough,
                        "overline" => Decoration::Overline,
                        _ => continue,
                    };
                    if !out.iter().any(|(k, _, _)| *k == kind) {
                        out.push((kind, color, thickness));
                    }
                }
            }
            let boundary = self.laid_out.tree.box_of(el).is_some_and(|b| layer_of(self.page, self.dom, self.media, self.laid_out, b, &self.pseudo) != Layer::Flow);
            if boundary {
                break;
            }
            at = self.dom.parent(el);
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decoration {
    Underline,
    LineThrough,
    Overline,
}

/// One `box-shadow`.
struct Shadow {
    inset: bool,
    dx: f64,
    dy: f64,
    blur: f64,
    spread: f64,
    color: Rgba,
}

/// Parse one `box-shadow` value: `[inset] <dx> <dy> [<blur> [<spread>]] [<color>]` in any order of the colour and
/// `inset`; the colour defaults to black (Chrome's `currentcolor` for a computed shadow without one).
fn parse_shadow(s: &str, current: Rgba) -> Option<Shadow> {
    let mut lengths = Vec::new();
    let mut color = None;
    let mut inset = false;
    for tok in split_top_spaces(s) {
        if tok == "inset" {
            inset = true;
        } else if tok.eq_ignore_ascii_case("currentcolor") {
            // the computed value keeps the keyword: the element's own colour
            color = Some(current);
        } else if let Some(c) = parse_rgba(&tok) {
            color = Some(c);
        } else if tok == "0" || tok.ends_with("px") {
            lengths.push(px(&tok));
        } else {
            return None;
        }
    }
    if lengths.len() < 2 {
        return None;
    }
    Some(Shadow {
        inset,
        dx: lengths[0],
        dy: lengths[1],
        blur: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
        spread: lengths.get(3).copied().unwrap_or(0.0),
        color: color.unwrap_or(Rgba::BLACK),
    })
}

/// Split on whitespace outside parentheses: `rgba(0, 0, 0, .3) 0 4px` -> `rgba(0, 0, 0, .3)`, `0`, `4px`.
fn split_top_spaces(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A computed length (`12px`, `50%` of `basis`, `0`).
fn length_of(v: &str, basis: f64) -> f64 {
    let v = v.trim();
    match v.strip_suffix('%') {
        Some(p) => p.trim().parse::<f64>().map(|p| p / 100.0 * basis).unwrap_or(0.0),
        None => px(v),
    }
}

/// A box that is a `::before`/`::after`'s: which, and whether it is the pseudo-element's own box (`root`) or text in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PseudoTag {
    name: &'static str,
    root: bool,
}
type Pseudo = Option<PseudoTag>;

/// For every box, the `::before`/`::after` whose style it takes: the generated box itself and the text runs and
/// anonymous boxes inside it (they carry the element's node, and read its style -- the pseudo-element's colour,
/// font and size were lost; Naver's sprite icons are `::before`s and painted nothing).
fn pseudo_map(laid_out: &LaidOut) -> Vec<Pseudo> {
    let tree = &laid_out.tree;
    let mut out: Vec<Pseudo> = vec![None; tree.len()];
    for (_, after, index) in tree.pseudo_box_list() {
        if let Some(slot) = out.get_mut(index as usize) {
            *slot = Some(PseudoTag { name: if after { "after" } else { "before" }, root: true });
        }
    }
    for i in 0..tree.len() as u32 {
        if out[i as usize].is_some() {
            continue;
        }
        if let Some(parent) = tree.parent_of(i) {
            if let Some(tag) = out[parent as usize] {
                if tree.node_of(parent) == tree.node_of(i) {
                    out[i as usize] = Some(PseudoTag { root: false, ..tag });
                }
            }
        }
    }
    out
}

/// A property of a box's style: `node`'s, or the pseudo-element's the box belongs to.
fn box_value(page: &Page, dom: &Dom, media: &MediaContext, laid_out: &LaidOut, node: NodeId, pseudo: Pseudo, p: PropertyId) -> Option<String> {
    match pseudo {
        Some(tag) => page.computed_pseudo_value(dom, node, tag.name, media, p, Some(laid_out)),
        None => page.computed_value(dom, node, media, p, Some(laid_out)),
    }
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Text under `letter-spacing` or `word-spacing`, as the pieces it is drawn in and where along the run each starts:
/// a character with the ones that join it (a combining mark, a ZWJ sequence, a flag's second half), at its pen
/// position in the layout's font -- the advances and kerning before it, plus the spacing after each character before
/// it ([`crate::layout::inline::FontSpec::spacing`]). Drawn whole, the glyphs would sit at their natural advances
/// and the text overrun its box (`letter-spacing: -1.28px` makes minea's 64px heading 18px narrower).
fn spaced_pieces(text: &str, metrics: &crate::font::FontMetrics, size_px: f64, letter: f32, word: f32) -> Vec<(String, f64)> {
    let size = crate::font::used_size(size_px as f32);
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut run = crate::font::Run::default();
    let mut spaced = 0.0f32;
    let (mut after_zwj, mut open_flag) = (false, false);
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let flag = (0x1F1E6..=0x1F1FF).contains(&(c as u32));
        let joins = crate::font::zero_width(c) || after_zwj || (flag && open_flag);
        let next = metrics.extend(run, c.encode_utf8(&mut buf));
        match out.last_mut() {
            Some(last) if joins => last.0.push(c),
            // where the character starts: the run through it, less its own advance (a kerning pair moves it)
            _ => out.push((c.to_string(), ((next.total - metrics.advance(c)) * size + spaced) as f64)),
        }
        run = next;
        if !crate::font::zero_width(c) {
            spaced += letter + if matches!(c, ' ' | '\u{a0}' | '\t') { word } else { 0.0 };
        }
        open_flag = flag && !(joins && open_flag);
        after_zwj = c == '\u{200D}';
    }
    out
}

/// A computed `font-family` list as the font provider takes it (it is already one).
fn family_of(list: &str) -> String {
    list.to_string()
}

/// Text for XML markup: `&`, `<`, `>` and `"` escaped.
/// What every element of one inline `<svg>` is drawn in: the root's user space ([`Painter::svg_view_box`]).
struct SvgCtx {
    view: [f64; 4],
}

/// A running bounding box over points.
#[derive(Default)]
struct Bounds(Option<[f64; 4]>);

impl Bounds {
    fn add(&mut self, x: f64, y: f64) {
        self.0 = Some(match self.0 {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }

    fn rect(&self) -> Option<[f64; 4]> {
        self.0.map(|[x0, y0, x1, y1]| [x0, y0, x1 - x0, y1 - y0])
    }
}

/// `m . n` for two 2D matrices [a, b, c, d, e, f]: `n` applied first.
fn mul_matrix(m: [f64; 6], n: [f64; 6]) -> [f64; 6] {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

/// The matrix from an inline `<svg>`'s user space to the page, as resvg draws it into the content box `(x, y, w, h)`:
/// the `viewBox` fitted to the SVG's own size by `preserveAspectRatio` (`xMidYMid meet` unless it says otherwise),
/// then that size stretched to the box.
fn svg_root_mapping(dom: &Dom, root: NodeId, [vx, vy, vw, vh]: [f64; 4], (x, y, w, h): (f64, f64, f64, f64)) -> [f64; 6] {
    let has_view_box = dom.get_attribute(root, "viewBox").is_some();
    let attr = |a: &str| dom.get_attribute(root, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f64>().ok()).filter(|&n| n > 0.0);
    let (nw, nh) = (attr("width").unwrap_or(if has_view_box { vw } else { w }), attr("height").unwrap_or(if has_view_box { vh } else { h }));
    let mut m = [w / nw, 0.0, 0.0, h / nh, x, y];
    if has_view_box {
        let par = dom.get_attribute(root, "preserveAspectRatio").unwrap_or("xMidYMid meet").trim().to_string();
        let fit = if par.starts_with("none") {
            [nw / vw, 0.0, 0.0, nh / vh, -vx * nw / vw, -vy * nh / vh]
        } else {
            let s = if par.ends_with("slice") { (nw / vw).max(nh / vh) } else { (nw / vw).min(nh / vh) };
            let along = |key: &str, room: f64| if par.contains(&format!("{key}Min")) { 0.0 } else if par.contains(&format!("{key}Max")) { room } else { room / 2.0 };
            [s, 0.0, 0.0, s, along("x", nw - vw * s) - vx * s, along("Y", nh - vh * s) - vy * s]
        };
        m = mul_matrix(m, fit);
    }
    m
}

/// A computed `transform` (`matrix(...)`, or a `matrix3d(...)`'s 2D part) as [a, b, c, d, e, f]; `None` for `none`.
fn parse_matrix(v: &str) -> Option<[f64; 6]> {
    let (name, rest) = v.trim().split_once('(')?;
    let n: Vec<f64> = rest.trim_end_matches(')').split(',').filter_map(|t| t.trim().parse().ok()).collect();
    match (name, n.len()) {
        ("matrix", 6) => Some([n[0], n[1], n[2], n[3], n[4], n[5]]),
        ("matrix3d", 16) => Some([n[0], n[1], n[4], n[5], n[12], n[13]]),
        _ => None,
    }
}

/// A computed `transform-origin` in the reference box (x, y, width, height): keywords, percentages of the box, px
/// from its corner.
fn resolve_origin(origin: &str, [bx, by, bw, bh]: [f64; 4]) -> (f64, f64) {
    let words: Vec<&str> = origin.split_ascii_whitespace().collect();
    let (mut x, mut y) = (words.first().copied().unwrap_or("50%"), words.get(1).copied().unwrap_or("50%"));
    if matches!(x, "top" | "bottom") || matches!(y, "left" | "right") {
        std::mem::swap(&mut x, &mut y);
    }
    let at = |w: &str, of: f64| match w {
        "left" | "top" => 0.0,
        "center" => of / 2.0,
        "right" | "bottom" => of,
        w => svg_length_px(w, of).unwrap_or(0.0),
    };
    (bx + at(x, bw), by + at(y, bh))
}

/// A computed length in user units: px, or a percentage of `of`.
fn svg_length_px(v: &str, of: f64) -> Option<f64> {
    let v = v.trim();
    if let Some(p) = v.strip_suffix('%') {
        return p.parse::<f64>().ok().map(|p| p / 100.0 * of);
    }
    v.strip_suffix("px").unwrap_or(v).parse().ok()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The URL of a `url(...)` image layer, unquoted.
fn css_url(layer: &str) -> Option<String> {
    let layer = layer.trim();
    if !layer.as_bytes().get(..4).is_some_and(|p| p.eq_ignore_ascii_case(b"url(")) {
        return None;
    }
    // quoted, it may hold a `)` (an SVG's `translate(1 1)`) and escapes
    let (url, end) = crate::style::urls::url_at(layer, 4)?;
    (end == layer.len() && !url.is_empty()).then_some(url)
}

// ── transforms, fades and clips (D77) ─────────────────────────────────────────────────────────────

/// One drawing context a box opens for what it paints: its transform, its opacity group, or (for what it CONTAINS,
/// not itself) its overflow clip -- in that order, as Chrome applies them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Context {
    Transform(u32),
    Fade(u32),
    Clip(u32),
    /// The box's own `clip` / `clip-path: inset()` rectangle, over it and what it holds.
    ClipRect(u32),
    /// The box's `filter`, which it and what it holds are drawn through -- inside its opacity, which applies after it.
    Filter(u32),
    /// The box's `mix-blend-mode`, with which it and what it holds, filtered and faded, composite onto what is beneath.
    Blend(u32),
    /// A stacking context with a blend inside, painted into a transparent group the blend composites within.
    Isolate(u32),
    /// The box's `mask-image`: it and what it holds, filtered, drawn into a group its mask layers then cut by their
    /// alpha (`DestinationIn`) -- under its opacity, which applies after it.
    Mask(u32),
}

/// The contexts box `index` paints inside: those of every box above it, outermost first, then its own transform and
/// fade. Empty on a page where nothing transforms, fades or clips -- the paint as it was.
fn context_chain(tree: &crate::layout::tree::BoxTree, index: u32, isolated: &[bool]) -> Vec<Context> {
    let fx = tree.effects();
    let mut ancestors = Vec::new();
    let mut current = tree.parent_of(index);
    while let Some(b) = current {
        ancestors.push(b);
        current = tree.parent_of(b);
    }
    let mut chain = Vec::new();
    let own = |b: u32, chain: &mut Vec<Context>| {
        if let Some(fx) = fx {
            if fx.local(b).is_some() {
                chain.push(Context::Transform(b));
            }
            if fx.clip(b).is_some() || fx.clip_path(b).is_some() {
                chain.push(Context::ClipRect(b));
            }
            if fx.blend(b).is_some() {
                chain.push(Context::Blend(b));
            } else if isolated.get(b as usize) == Some(&true) && fx.opacity(b) >= 1.0 && fx.filter(b).is_none() {
                // a group of its own for a blend inside to blend with, unless a layer of its own already is one
                chain.push(Context::Isolate(b));
            }
            if fx.opacity(b) < 1.0 {
                chain.push(Context::Fade(b));
            }
            if fx.masked(b) {
                chain.push(Context::Mask(b));
            }
            if fx.filter(b).is_some() {
                chain.push(Context::Filter(b));
            }
        }
    };
    for &a in ancestors.iter().rev() {
        own(a, &mut chain);
        if tree.clips_contents(a) {
            chain.push(Context::Clip(a));
        }
    }
    own(index, &mut chain);
    chain
}

/// The contexts open in the op list, outermost first.
#[derive(Default)]
struct Contexts {
    open: Vec<Context>,
    /// Each masked box's element and mask layers with their painting areas, made when its mask opens.
    masks: std::collections::HashMap<u32, (NodeId, Vec<(LayerPaint, (f64, f64, f64, f64))>)>,
}

impl Contexts {
    /// Make `want` the open contexts: close those it does not share with what is open, innermost first, then open
    /// the rest. A box after its subtree closes what the subtree opened; a box inside opens only what is new.
    fn enter(&mut self, ops: &mut Vec<Canvas2DOp>, replaced: &mut Vec<ReplacedBox>, want: Vec<Context>, tree: &crate::layout::tree::BoxTree, translations: &[(f32, f32)], scroll: &crate::layout::geometry::ScrollSnapshot) {
        let keep = self.open.iter().zip(want.iter()).take_while(|(a, b)| a == b).count();
        for i in (keep..self.open.len()).rev() {
            self.close(self.open[i], ops, replaced);
        }
        self.open.truncate(keep);
        for &c in &want[keep..] {
            match c {
                Context::Transform(b) => {
                    // about the box's border box as scrolled, in the coordinates the boxes are painted in; nested
                    // transforms multiply into the current matrix, outermost first
                    let local = tree.effects().and_then(|fx| fx.local(b)).unwrap_or(crate::effects::Affine::IDENTITY);
                    let r = tree.border_box(b);
                    let (tx, ty) = translations[b as usize];
                    let m = local.about((r.x + tx) as f64, (r.y + ty) as f64);
                    ops.push(Canvas2DOp::Save);
                    ops.push(Canvas2DOp::Transform { t: spec::Transform2D { a: m.a, b: m.b, c: m.c, d: m.d, e: m.e, f: m.f } });
                }
                Context::Fade(b) => {
                    let alpha = tree.effects().map(|fx| fx.opacity(b)).unwrap_or(1.0);
                    ops.push(Canvas2DOp::SaveLayer { alpha: alpha as f64 });
                }
                Context::Filter(b) => {
                    let filters = tree.effects().and_then(|fx| fx.filter(b)).map(|f| f.to_vec()).unwrap_or_default();
                    ops.push(Canvas2DOp::SaveLayerFilter { filters });
                }
                Context::Isolate(_) | Context::Mask(_) => ops.push(Canvas2DOp::SaveLayer { alpha: 1.0 }),
                Context::Blend(b) => {
                    let op = tree.effects().and_then(|fx| fx.blend(b)).unwrap_or(spec::CompositeOp::SourceOver);
                    ops.push(Canvas2DOp::SaveLayerBlend { op });
                }
                Context::ClipRect(b) => {
                    // its `clip` rectangle, then its `clip-path` shape: what it shows is inside both
                    let fx = tree.effects();
                    let r = tree.border_box(b);
                    let (tx, ty) = translations[b as usize];
                    let (ox, oy) = ((r.x + tx) as f64, (r.y + ty) as f64);
                    ops.push(Canvas2DOp::Save);
                    if let Some([cx, cy, cw, ch]) = fx.and_then(|fx| fx.clip(b)) {
                        ops.extend(clip_ops(ox + cx as f64, oy + cy as f64, cw as f64, ch as f64, None));
                    }
                    if let Some(shape) = fx.and_then(|fx| fx.clip_path(b)) {
                        use crate::effects::ClipShape;
                        ops.push(Canvas2DOp::BeginPath);
                        let even_odd = match shape {
                            ClipShape::Rect([x, y, w, h], radii) => {
                                let (x, y, w, h) = (ox + *x as f64, oy + *y as f64, *w as f64, *h as f64);
                                ops.push(match radii {
                                    Some(r) => Canvas2DOp::RoundRect { x, y, w, h, radii: r.map(|(a, b)| (a as f64, b as f64)) },
                                    None => Canvas2DOp::Rect { x, y, w, h },
                                });
                                false
                            }
                            ClipShape::Ellipse([cx, cy, rx, ry]) => {
                                ops.push(Canvas2DOp::Ellipse {
                                    x: ox + *cx as f64,
                                    y: oy + *cy as f64,
                                    rx: *rx as f64,
                                    ry: *ry as f64,
                                    rotation: 0.0,
                                    start: 0.0,
                                    end: std::f64::consts::TAU,
                                    ccw: false,
                                });
                                false
                            }
                            ClipShape::Path(path, even_odd) => {
                                ops.extend(path.iter().map(|op| path_op(*op, ox, oy)));
                                *even_odd
                            }
                        };
                        ops.push(Canvas2DOp::Clip { even_odd });
                    }
                }
                Context::Clip(b) => {
                    let r = tree.viewport_padding_box(b, scroll);
                    ops.push(Canvas2DOp::Save);
                    ops.push(Canvas2DOp::BeginPath);
                    ops.push(Canvas2DOp::Rect { x: r.x as f64, y: r.y as f64, w: r.width as f64, h: r.height as f64 });
                    ops.push(Canvas2DOp::Clip { even_odd: false });
                }
            }
        }
        self.open.extend_from_slice(&want[keep..]);
    }

    fn close_all(&mut self, ops: &mut Vec<Canvas2DOp>, replaced: &mut Vec<ReplacedBox>) {
        for i in (0..self.open.len()).rev() {
            self.close(self.open[i], ops, replaced);
        }
        self.open.clear();
    }

    /// End context `c`. A mask's layers are drawn first, into a group composited onto what the box painted with
    /// `DestinationIn`: where they are transparent -- past their own tiles too -- the box is cut away.
    fn close(&self, c: Context, ops: &mut Vec<Canvas2DOp>, replaced: &mut Vec<ReplacedBox>) {
        if let Context::Mask(b) = c {
            if let Some((node, layers)) = self.masks.get(&b) {
                ops.push(Canvas2DOp::SaveLayerBlend { op: spec::CompositeOp::DestinationIn });
                for (paint, clip) in layers {
                    match paint {
                        LayerPaint::Ops(o) => ops.extend(o.iter().cloned()),
                        LayerPaint::Image(bg) => replaced.push(ReplacedBox {
                            node: *node,
                            x: clip.0,
                            y: clip.1,
                            w: clip.2,
                            h: clip.3,
                            kind: ReplacedKind::Background(bg.clone()),
                            at: ops.len(),
                            fit: ObjectFit::default(),
                        }),
                    }
                }
                ops.push(Canvas2DOp::Restore);
            }
        }
        ops.push(Canvas2DOp::Restore);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// The colour an `<hr>` with the UA sheet's rule (1px `inset`, `gray`) paints its border in: Chrome 152 draws it
/// `rgb(154,154,154)` over `rgb(238,238,238)` -- an inset of `rgb(238,238,238)` -- where the same border on a `<div>`
/// is `rgb(44,44,44)` over `gray`, though both compute `gray` (measured). Any other border is its own colour.
fn hr_rule_color(dom: &Dom, node: NodeId, style: BorderStyle, width: f64, c: Rgba) -> Rgba {
    let rule = style == BorderStyle::Inset && width == 1.0 && (c.r, c.g, c.b) == (128, 128, 128) && c.a == 1.0;
    if rule && dom.is_html_ns(node) && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("hr")) {
        Rgba::opaque(238, 238, 238)
    } else {
        c
    }
}

/// A border side's style, as it paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BorderStyle {
    Solid,
    Inset,
    Outset,
    Groove,
    Ridge,
    Double,
    Dashed,
    Dotted,
}

impl BorderStyle {
    fn of(s: &str) -> BorderStyle {
        match s.trim() {
            "inset" => BorderStyle::Inset,
            "outset" => BorderStyle::Outset,
            "groove" => BorderStyle::Groove,
            "ridge" => BorderStyle::Ridge,
            "double" => BorderStyle::Double,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            _ => BorderStyle::Solid,
        }
    }

    /// The colour side `side` of a border in this style paints, measured on Chrome 152: an `inset` darkens its top and
    /// left (`rgb(118,118,118)` -> `rgb(33,33,33)`), an `outset` its bottom and right; the other pair keeps the colour --
    /// unless it is black, whose light pair is `rgb(84,84,84)`. A colour as dark as `#202020` is not darkened.
    fn side_color(self, side: Side, c: Rgba) -> Rgba {
        if !matches!(self, BorderStyle::Inset | BorderStyle::Outset) {
            return c;
        }
        let near_black = (c.r as u32).pow(2) + (c.g as u32).pow(2) + (c.b as u32).pow(2) <= 3 * 32 * 32;
        let top_left = matches!(side, Side::Top | Side::Left);
        if top_left == (self == BorderStyle::Inset) {
            if near_black {
                return c;
            }
            // Blink's `Color::Dark()`
            if (c.r, c.g, c.b) == (255, 255, 255) {
                return Rgba { r: 171, g: 171, b: 171, a: c.a, wide: None };
            }
            let v = c.r.max(c.g).max(c.b) as f32 / 255.0;
            let m = if v == 0.0 { 0.0 } else { ((v - 0.33) / v).max(0.0) };
            let ch = |x: u8| ((x as f32 / 255.0) * m * 255.99).floor() as u8;
            Rgba { r: ch(c.r), g: ch(c.g), b: ch(c.b), a: c.a, wide: None }
        } else if near_black {
            Rgba { r: 84, g: 84, b: 84, a: c.a, wide: None }
        } else {
            c
        }
    }
}

/// Parse a computed length like `"4px"` to pixels (`0.0` if absent or not px — used border widths are
/// always `px` in a computed style).
fn px(s: &str) -> f64 {
    s.trim().strip_suffix("px").and_then(|n| n.trim().parse().ok()).unwrap_or(0.0)
}

/// Split a gradient's argument list on TOP-LEVEL commas, keeping `rgb(…)`/`rgba(…)` groups intact.
fn split_top_commas(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for ch in s.chars() {
        match ch {
            '(' => { depth += 1; cur.push(ch); }
            ')' => { depth -= 1; cur.push(ch); }
            ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// CSS linear-gradient angle in degrees (0 = to top, clockwise). `"90deg"` or `"to right"` etc.
fn gradient_angle(dir: &str) -> Option<f64> {
    let dir = dir.trim();
    if let Some(n) = dir.strip_suffix("deg") {
        return n.trim().parse::<f64>().ok();
    }
    match dir {
        "to top" => Some(0.0),
        "to right" => Some(90.0),
        "to bottom" => Some(180.0),
        "to left" => Some(270.0),
        "to top right" | "to right top" => Some(45.0),
        "to bottom right" | "to right bottom" => Some(135.0),
        "to bottom left" | "to left bottom" => Some(225.0),
        "to top left" | "to left top" => Some(315.0),
        _ => None,
    }
}

/// Emit ops filling `(x,y,w,h)` with a computed `linear-`, `radial-` or `conic-gradient(...)`, or its `repeating-`
/// form; `None` for anything else (a `url()`, `none`). `id` must be unique across the display list.
fn gradient_ops(image: &str, x: f64, y: f64, w: f64, h: f64, id: u32) -> Option<Vec<Canvas2DOp>> {
    let image = image.trim();
    let plain = image.strip_prefix("repeating-").unwrap_or(image);
    if plain.starts_with("linear-gradient(") {
        linear_gradient_ops(image, x, y, w, h, id)
    } else if plain.starts_with("radial-gradient(") {
        radial_gradient_ops(image, x, y, w, h, id)
    } else if plain.starts_with("conic-gradient(") {
        conic_gradient_ops(image, x, y, w, h, id)
    } else {
        None
    }
}

/// The vertical extent of a line's fragments.
fn line_extent(line: &[(u32, crate::Rect)]) -> (f32, f32) {
    let top = line.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min);
    let bottom = line.iter().map(|(_, r)| r.y + r.height).fold(f32::MIN, f32::max);
    (top, bottom)
}

/// A line a block cuts for an ellipsis (CSS Overflow 3 §4.1 `text-overflow: ellipsis`, and a line clamp's last
/// line): its vertical extent, the x its text is cut at -- `None` when it fits and the ellipsis just follows it -- and
/// the text box whose fragment on it carries the "…".
#[derive(Clone, Debug)]
struct CutLine {
    top: f32,
    bottom: f32,
    cut: Option<f64>,
    host: u32,
}

/// One `background-image` or `mask-image` layer of a box, placed: its image, its positioning area (`origin`), its
/// painting area (`clip`) -- inside the border box's corners when it is clipped to the border box -- and its computed
/// size, position and repeat.
struct ImageLayer<'a> {
    image: &'a str,
    /// A mask's layer (see [`BackgroundLayer::cors`]).
    mask: bool,
    origin: (f64, f64, f64, f64),
    clip: (f64, f64, f64, f64),
    radii: Option<[(f64, f64); 4]>,
    size: String,
    x: String,
    y: String,
    repeat: String,
}

/// What paints an image layer: a gradient's ops, or an image the webapi layer fetches and tiles over the clip.
#[derive(Clone)]
enum LayerPaint {
    Ops(Vec<Canvas2DOp>),
    Image(BackgroundLayer),
}

/// An image layer's paint ([`LayerPaint`]); `None` for `none`, an empty tile, or an image this does not draw. A
/// gradient has no size of its own: its tile is the layer's size of the positioning area, placed by its position
/// and repeated across the clip -- one tile drawn once into a pattern where it repeats.
fn layer_paint(l: ImageLayer, grad_id: &mut u32) -> Option<LayerPaint> {
    let layer = l.image.trim();
    if let Some(url) = css_url(layer) {
        return Some(LayerPaint::Image(BackgroundLayer { url, origin: l.origin, radii: l.radii, size: l.size, position_x: l.x, position_y: l.y, repeat: l.repeat, cors: l.mask }));
    }
    if !is_gradient(layer) {
        return None;
    }
    let (ox, oy, ow, oh) = l.origin;
    let (tw, th) = background_tile_size(&l.size, None, (ow, oh));
    if tw < 0.5 || th < 0.5 {
        return None;
    }
    let x0 = ox + background_offset(&l.x, ow - tw);
    let y0 = oy + background_offset(&l.y, oh - th);
    let (rx, ry) = background_repeats(&l.repeat);
    let (cx, cy, cw, ch) = l.clip;
    let covers = x0 <= cx + 0.01 && y0 <= cy + 0.01 && x0 + tw >= cx + cw - 0.01 && y0 + th >= cy + ch - 0.01;
    let id = *grad_id;
    *grad_id += 1;
    let g = if covers || (!rx && !ry) {
        gradient_ops(layer, x0, y0, tw, th, id)?
    } else {
        let tile = gradient_ops(layer, 0.0, 0.0, tw, th, id)?;
        let pid = GRADIENT_PATTERN_BASE + id;
        let repeat = match (rx, ry) {
            (true, true) => spec::PatternRepeat::Repeat,
            (true, false) => spec::PatternRepeat::RepeatX,
            _ => spec::PatternRepeat::RepeatY,
        };
        let (pw, ph) = (tw.ceil().clamp(1.0, 4096.0), th.ceil().clamp(1.0, 4096.0));
        let (fx, fw) = if rx { (cx, cw) } else { (x0, tw) };
        let (fy, fh) = if ry { (cy, ch) } else { (y0, th) };
        vec![
            Canvas2DOp::DefineTilePattern { id: pid, w: tw, h: th, ops: tile, repeat },
            Canvas2DOp::SetPatternTransform { id: pid, t: spec::Transform2D { a: tw / pw, b: 0.0, c: 0.0, d: th / ph, e: x0, f: y0 } },
            Canvas2DOp::SetFillPattern { id: pid },
            Canvas2DOp::FillRect { x: fx, y: fy, w: fw, h: fh },
        ]
    };
    let mut ops = vec![Canvas2DOp::Save];
    ops.extend(clip_ops(cx, cy, cw, ch, l.radii));
    ops.extend(g);
    ops.push(Canvas2DOp::Restore);
    Some(LayerPaint::Ops(ops))
}

/// A path segment as the op that adds it, moved by `(dx, dy)`.
fn path_op(op: spec::PathOp, dx: f64, dy: f64) -> Canvas2DOp {
    use spec::PathOp as P;
    match op {
        P::MoveTo { x, y } => Canvas2DOp::MoveTo { x: x + dx, y: y + dy },
        P::LineTo { x, y } => Canvas2DOp::LineTo { x: x + dx, y: y + dy },
        P::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => Canvas2DOp::BezierCurveTo { cp1x: cp1x + dx, cp1y: cp1y + dy, cp2x: cp2x + dx, cp2y: cp2y + dy, x: x + dx, y: y + dy },
        P::QuadraticCurveTo { cpx, cpy, x, y } => Canvas2DOp::QuadraticCurveTo { cpx: cpx + dx, cpy: cpy + dy, x: x + dx, y: y + dy },
        P::Arc { x, y, radius, start, end, ccw } => Canvas2DOp::Arc { x: x + dx, y: y + dy, radius, start, end, ccw },
        P::ArcTo { x1, y1, x2, y2, radius } => Canvas2DOp::ArcTo { x1: x1 + dx, y1: y1 + dy, x2: x2 + dx, y2: y2 + dy, radius },
        P::Ellipse { x, y, rx, ry, rotation, start, end, ccw } => Canvas2DOp::Ellipse { x: x + dx, y: y + dy, rx, ry, rotation, start, end, ccw },
        P::Rect { x, y, w, h } => Canvas2DOp::Rect { x: x + dx, y: y + dy, w, h },
        P::RoundRect { x, y, w, h, radii } => Canvas2DOp::RoundRect { x: x + dx, y: y + dy, w, h, radii },
        P::ClosePath => Canvas2DOp::ClosePath,
    }
}

/// The ops that clip to `(x, y, w, h)`, inside `radii` when it is rounded.
fn clip_ops(x: f64, y: f64, w: f64, h: f64, radii: Option<[(f64, f64); 4]>) -> [Canvas2DOp; 3] {
    [
        Canvas2DOp::BeginPath,
        match radii {
            None => Canvas2DOp::Rect { x, y, w, h },
            Some(r) => Canvas2DOp::RoundRect { x, y, w, h, radii: r },
        },
        Canvas2DOp::Clip { even_odd: false },
    ]
}

/// Pattern ids for the gradient tiles of the page's display list, apart from a canvas's and the image layers'.
const GRADIENT_PATTERN_BASE: u32 = 0x2000_0000;

/// Is a `background-image` layer a gradient [`gradient_ops`] paints?
fn is_gradient(layer: &str) -> bool {
    let plain = layer.trim().strip_prefix("repeating-").unwrap_or(layer.trim());
    ["linear-gradient(", "radial-gradient(", "conic-gradient("].iter().any(|g| plain.starts_with(g))
}

/// A gradient's arguments: what is inside `<kind>-gradient(...)` or `repeating-<kind>-gradient(...)`, and whether it
/// repeats.
fn gradient_args<'a>(image: &'a str, kind: &str) -> Option<(&'a str, bool)> {
    let image = image.trim();
    let (image, repeating) = match image.strip_prefix("repeating-") {
        Some(rest) => (rest, true),
        None => (image, false),
    };
    Some((image.strip_prefix(kind)?.strip_prefix("-gradient(")?.strip_suffix(')')?, repeating))
}

/// The colour a gradient's stops give position `t`, approached from the right (`after`: past a hard stop at `t`) or
/// from the left: the stop before it mixed toward the next with premultiplied alpha, as a CSS gradient interpolates.
fn stop_color_at(stops: &[(f64, Rgba)], t: f64, after: bool) -> Rgba {
    let Some(i) = stops.iter().rposition(|(p, _)| if after { *p <= t } else { *p < t }) else { return stops[0].1 };
    let (p0, a) = stops[i];
    let Some(&(p1, b)) = stops.get(i + 1).filter(|(p1, _)| *p1 > p0) else { return a };
    let f = ((t - p0) / (p1 - p0)) as f32;
    let alpha = a.a + (b.a - a.a) * f;
    let ch = |ca: u8, cb: u8| {
        let v = if alpha > 0.0 { (ca as f32 * a.a + (cb as f32 * b.a - ca as f32 * a.a) * f) / alpha } else { 0.0 };
        v.round().clamp(0.0, 255.0) as u8
    };
    Rgba { r: ch(a.r, b.r), g: ch(a.g, b.g), b: ch(a.b, b.b), a: alpha, wide: None }
}

/// A repeating gradient's stops laid end to end over `[lo, hi]` of its line, then mapped onto `[0, 1]` -- what a
/// gradient that pads its ends draws the same as (CSS Images 3 §3.5). A period of zero is the stops' average colour
/// (§3.5.1); `None` for a period so short the stops would run into thousands.
fn repeat_stops(stops: &[(f64, Rgba)], lo: f64, hi: f64) -> Option<Vec<(f64, Rgba)>> {
    let (first, last) = (stops.first()?.0, stops.last()?.0);
    let period = last - first;
    if period <= 1e-9 || hi <= lo {
        let n = stops.len() as f32;
        let a = stops.iter().map(|(_, c)| c.a).sum::<f32>() / n;
        let avg = |ch: fn(&Rgba) -> u8| {
            let v = stops.iter().map(|(_, c)| ch(c) as f32 * c.a).sum::<f32>() / n;
            if a > 0.0 { (v / a).round().clamp(0.0, 255.0) as u8 } else { 0 }
        };
        let c = Rgba { r: avg(|c| c.r), g: avg(|c| c.g), b: avg(|c| c.b), a, wide: None };
        return Some(vec![(0.0, c), (1.0, c)]);
    }
    let (k0, k1) = (((lo - first) / period).floor() as i64, ((hi - first) / period).ceil() as i64);
    if (k1 - k0 + 1) as usize * stops.len() > 4096 {
        return None;
    }
    let laid: Vec<(f64, Rgba)> = (k0..=k1).flat_map(|k| stops.iter().map(move |&(p, c)| (p + k as f64 * period, c))).collect();
    let mut out = vec![(0.0, stop_color_at(&laid, lo, true))];
    out.extend(laid.iter().filter(|(p, _)| *p > lo && *p < hi).map(|&(p, c)| ((p - lo) / (hi - lo), c)));
    out.push((1.0, stop_color_at(&laid, hi, false)));
    Some(out)
}

/// A gradient's color stops as `(color, position)` with each position a fraction of the gradient line (`unit` turns
/// a length, percentage or angle into one; `None` for what is not a position), CSS Images 3 §3.4.3's fix-up applied:
/// the first stop defaults to 0 and the last to 1, a position below an earlier one is raised to it, and stops with
/// none are spread evenly between their positioned neighbours. A stop with two positions is two stops.
fn color_stops(tokens: &[String], unit: &dyn Fn(&str) -> Option<f64>) -> Option<Vec<(f64, Rgba)>> {
    let mut raw: Vec<(Rgba, Option<f64>)> = Vec::new();
    for tok in tokens {
        let mut rest = tok.trim();
        let mut positions = Vec::new();
        // trailing positions, at most two, each a word after the colour (which may itself hold spaces)
        for _ in 0..2 {
            match rest.rsplit_once(char::is_whitespace) {
                Some((head, last)) if unit(last).is_some() => {
                    positions.push(unit(last)?);
                    rest = head.trim();
                }
                _ => break,
            }
        }
        // a bare interpolation hint (a position alone) is not drawn: skipped
        let Some(color) = parse_rgba(rest) else {
            if unit(rest).is_some() {
                continue;
            }
            return None;
        };
        positions.reverse();
        if positions.is_empty() {
            raw.push((color, None));
        }
        for p in positions {
            raw.push((color, Some(p)));
        }
    }
    if raw.len() < 2 {
        return None;
    }
    let n = raw.len();
    let mut pos: Vec<Option<f64>> = raw.iter().map(|(_, p)| *p).collect();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    let mut max = f64::NEG_INFINITY;
    for p in pos.iter_mut().flatten() {
        if *p < max {
            *p = max;
        }
        max = max.max(*p);
    }
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let start = i - 1;
            let mut end = i;
            while pos[end].is_none() {
                end += 1;
            }
            let (a, b) = (pos[start].unwrap_or(0.0), pos[end].unwrap_or(1.0));
            for k in i..end {
                pos[k] = Some(a + (b - a) * (k - start) as f64 / (end - start) as f64);
            }
            i = end;
        }
        i += 1;
    }
    Some(raw.iter().zip(pos).map(|((c, _), p)| (p.unwrap_or(0.0), *c)).collect())
}

/// A `<length-percentage>` along `basis`, or `None`.
fn length_along(s: &str, basis: f64) -> Option<f64> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f64>().ok().map(|v| v / 100.0 * basis);
    }
    s.strip_suffix("px").unwrap_or(s).trim().parse::<f64>().ok()
}

/// A gradient's `at <position>`: its point inside `(x, y, w, h)`; the centre when there is none.
fn gradient_center(position: Option<&str>, (x, y, w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    let Some(p) = position else { return (x + w / 2.0, y + h / 2.0) };
    let words: Vec<&str> = p.split_whitespace().collect();
    let along = |word: &str, basis: f64, near: &str, far: &str| -> f64 {
        match word {
            w if w == near => 0.0,
            w if w == far => basis,
            "center" => basis / 2.0,
            w => length_along(w, basis).unwrap_or(basis / 2.0),
        }
    };
    // `left 40px top 30px`: each keyword offset by its length (from the far edge for `right`/`bottom`)
    let edge = |kw: &str, off: &str, basis: f64| -> f64 {
        let d = length_along(off, basis).unwrap_or(0.0);
        if kw == "right" || kw == "bottom" { basis - d } else { d }
    };
    match words.as_slice() {
        [a, ao, b, bo] if *a == "top" || *a == "bottom" => (x + edge(b, bo, w), y + edge(a, ao, h)),
        [a, ao, b, bo] => (x + edge(a, ao, w), y + edge(b, bo, h)),
        [a] if *a == "top" || *a == "bottom" => (x + w / 2.0, y + along(a, h, "top", "bottom")),
        [a] => (x + along(a, w, "left", "right"), y + h / 2.0),
        [a, b] if *a == "top" || *a == "bottom" || *b == "left" || *b == "right" => (x + along(b, w, "left", "right"), y + along(a, h, "top", "bottom")),
        [a, b, ..] => (x + along(a, w, "left", "right"), y + along(b, h, "top", "bottom")),
        [] => (x + w / 2.0, y + h / 2.0),
    }
}

/// `radial-gradient([<shape> || <size>] [at <position>], <stops>)` over `(x, y, w, h)`: an ellipse is the circle of
/// its horizontal radius drawn under a vertical scale.
fn radial_gradient_ops(image: &str, x: f64, y: f64, w: f64, h: f64, id: u32) -> Option<Vec<Canvas2DOp>> {
    let (inner, repeating) = gradient_args(image, "radial")?;
    let parts = split_top_commas(inner);
    let (head, stops_at) = if parse_rgba(parts.first()?.split_whitespace().next().unwrap_or("")).is_some() || parts.first()?.trim().starts_with("rgb") || parts.first()?.trim().starts_with('#') {
        (String::new(), 0)
    } else {
        (parts[0].trim().to_string(), 1)
    };
    let (shape_size, position) = match head.split_once(" at ").map(|(a, b)| (a.to_string(), Some(b.to_string()))) {
        Some(v) => v,
        None if head.starts_with("at ") => (String::new(), Some(head[3..].to_string())),
        None => (head.clone(), None),
    };
    let (cx, cy) = gradient_center(position.as_deref(), (x, y, w, h));
    let words: Vec<&str> = shape_size.split_whitespace().collect();
    let circle = words.contains(&"circle");
    let sizes: Vec<&str> = words.iter().copied().filter(|w| *w != "circle" && *w != "ellipse").collect();
    let (l, r, t, b) = ((cx - x).abs(), (x + w - cx).abs(), (cy - y).abs(), (y + h - cy).abs());
    let (rx, ry) = match sizes.as_slice() {
        [a, b2] if length_along(a, w).is_some() => (length_along(a, w)?, length_along(b2, h)?),
        [a] if length_along(a, w).is_some() => (length_along(a, w)?, length_along(a, w)?),
        [kw] | [kw, ..] if kw.starts_with("closest-side") => {
            if circle { let m = l.min(r).min(t).min(b); (m, m) } else { (l.min(r), t.min(b)) }
        }
        [kw] | [kw, ..] if kw.starts_with("farthest-side") => {
            if circle { let m = l.max(r).max(t).max(b); (m, m) } else { (l.max(r), t.max(b)) }
        }
        [kw] | [kw, ..] if kw.starts_with("closest-corner") => {
            let (dx, dy) = (l.min(r), t.min(b));
            if circle { let d = dx.hypot(dy); (d, d) } else { (dx * std::f64::consts::SQRT_2, dy * std::f64::consts::SQRT_2) }
        }
        _ => {
            // farthest-corner, the default
            let (dx, dy) = (l.max(r), t.max(b));
            if circle { let d = dx.hypot(dy); (d, d) } else { (dx * std::f64::consts::SQRT_2, dy * std::f64::consts::SQRT_2) }
        }
    };
    if rx <= 0.0 || ry <= 0.0 {
        return None;
    }
    let mut stops = color_stops(&parts[stops_at..], &|s| length_along(s, rx).map(|v| v / rx))?;
    // a repeating one is laid out to the farthest corner, in units of the ending shape
    let mut reach = 1.0;
    if repeating {
        reach = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].iter().map(|&(px, py)| ((px - cx) / rx).hypot((py - cy) / ry)).fold(1.0_f64, f64::max);
        stops = repeat_stops(&stops, 0.0, reach)?;
    }
    let k = ry / rx;
    let mut ops = vec![
        Canvas2DOp::Save,
        Canvas2DOp::Transform { t: spec::Transform2D { a: 1.0, b: 0.0, c: 0.0, d: k, e: 0.0, f: cy - k * cy } },
        Canvas2DOp::DefineRadialGradient { id, x0: cx, y0: cy, r0: 0.0, x1: cx, y1: cy, r1: rx * reach },
    ];
    for (off, c) in stops {
        let color = c.paint_color();
        ops.push(Canvas2DOp::AddColorStop { id, offset: off.clamp(0.0, 1.0), color, oklab: color, legacy: true });
    }
    ops.push(Canvas2DOp::PremultiplyGradient { id });
    ops.push(Canvas2DOp::SetFillGradient { id });
    ops.push(Canvas2DOp::FillRect { x, y: cy + (y - cy) / k, w, h: h / k });
    ops.push(Canvas2DOp::Restore);
    Some(ops)
}

/// `conic-gradient([from <angle>] [at <position>], <angular stops>)` over `(x, y, w, h)`. CSS turns from 12 o'clock,
/// canvas from 3 o'clock, both clockwise.
fn conic_gradient_ops(image: &str, x: f64, y: f64, w: f64, h: f64, id: u32) -> Option<Vec<Canvas2DOp>> {
    let (inner, repeating) = gradient_args(image, "conic")?;
    let parts = split_top_commas(inner);
    let first = parts.first()?.trim().to_string();
    let header = first.starts_with("from ") || first.starts_with("at ");
    let angle = |s: &str| -> Option<f64> {
        let s = s.trim();
        if let Some(v) = s.strip_suffix("deg") { return v.trim().parse::<f64>().ok(); }
        if let Some(v) = s.strip_suffix("turn") { return v.trim().parse::<f64>().ok().map(|t| t * 360.0); }
        if let Some(v) = s.strip_suffix("grad") { return v.trim().parse::<f64>().ok().map(|g| g * 0.9); }
        if let Some(v) = s.strip_suffix("rad") { return v.trim().parse::<f64>().ok().map(f64::to_degrees); }
        if let Some(v) = s.strip_suffix('%') { return v.trim().parse::<f64>().ok().map(|p| p * 3.6); }
        if s == "0" { return Some(0.0); }
        None
    };
    let (from, position) = if header {
        let (f, at) = match first.split_once("at ") {
            Some((f, at)) => (f.trim().to_string(), Some(at.trim().to_string())),
            None => (first.clone(), None),
        };
        (f.strip_prefix("from").map(str::trim).and_then(angle).unwrap_or(0.0), at)
    } else {
        (0.0, None)
    };
    let (cx, cy) = gradient_center(position.as_deref(), (x, y, w, h));
    let mut stops = color_stops(&parts[header as usize..], &|s| angle(s).map(|d| d / 360.0))?;
    if repeating {
        stops = repeat_stops(&stops, 0.0, 1.0)?;
    }
    let mut ops = vec![Canvas2DOp::DefineConicGradient { id, start_angle: (from - 90.0).to_radians(), x: cx, y: cy }];
    for (off, c) in stops {
        let color = c.paint_color();
        ops.push(Canvas2DOp::AddColorStop { id, offset: off.clamp(0.0, 1.0), color, oklab: color, legacy: true });
    }
    ops.push(Canvas2DOp::PremultiplyGradient { id });
    ops.push(Canvas2DOp::SetFillGradient { id });
    ops.push(Canvas2DOp::FillRect { x, y, w, h });
    Some(ops)
}

/// Emit ops filling `(x,y,w,h)` with a computed `linear-gradient(...)` or `repeating-linear-gradient(...)`.
fn linear_gradient_ops(image: &str, x: f64, y: f64, w: f64, h: f64, id: u32) -> Option<Vec<Canvas2DOp>> {
    let (inner, repeating) = gradient_args(image, "linear")?;
    let parts = split_top_commas(inner);
    if parts.len() < 2 {
        return None;
    }
    // Optional direction as the first argument; default is `to bottom` (180deg).
    let mut angle = 180.0_f64;
    let mut first = 0;
    if let Some(a) = gradient_angle(&parts[0]) {
        angle = a;
        first = 1;
    }
    // Gradient line through the box centre at `angle` (0deg = up); length is the projection of the box
    // onto the line (CSS Images §3.1). Endpoints are the gradient's 0% and 100% positions.
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let th = angle.to_radians();
    let (dx, dy) = (th.sin(), -th.cos());
    let line = w * th.sin().abs() + h * th.cos().abs();
    let mut stops = color_stops(&parts[first..], &|s| length_along(s, line).map(|v| if line > 0.0 { v / line } else { 0.0 }))?;
    if repeating {
        stops = repeat_stops(&stops, 0.0, 1.0)?;
    }
    let mut ops = vec![Canvas2DOp::DefineLinearGradient {
        id,
        x0: cx - dx * line / 2.0,
        y0: cy - dy * line / 2.0,
        x1: cx + dx * line / 2.0,
        y1: cy + dy * line / 2.0,
    }];
    for (off, c) in stops {
        let color = c.paint_color();
        ops.push(Canvas2DOp::AddColorStop { id, offset: off.clamp(0.0, 1.0), color, oklab: color, legacy: true });
    }
    ops.push(Canvas2DOp::PremultiplyGradient { id });
    ops.push(Canvas2DOp::SetFillGradient { id });
    ops.push(Canvas2DOp::FillRect { x, y, w, h });
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offsets(ops: &[Canvas2DOp]) -> Vec<f64> {
        ops.iter().filter_map(|o| if let Canvas2DOp::AddColorStop { offset, .. } = o { Some((offset * 1000.0).round() / 1000.0) } else { None }).collect()
    }

    /// Stops take CSS's defaults (CSS Images 3 §3.4.3): the ends at 0 and 1, the unpositioned spread evenly between
    /// their neighbours, a position below an earlier one raised to it, a two-position stop as two stops -- in a
    /// linear gradient's lengths along its line, a radial's along its radius, a conic's in angles.
    #[test]
    fn gradient_stops_take_their_default_positions() {
        let l = linear_gradient_ops("linear-gradient(rgb(255, 0, 0), rgb(0, 255, 0), rgb(0, 0, 255) 80%, rgb(0, 0, 0) 50%)", 0.0, 0.0, 100.0, 200.0, 0).unwrap();
        assert_eq!(offsets(&l), vec![0.0, 0.4, 0.8, 0.8]);
        let l = linear_gradient_ops("linear-gradient(90deg, rgb(255, 0, 0) 0px 50px, rgb(0, 0, 255) 50px)", 0.0, 0.0, 100.0, 20.0, 0).unwrap();
        assert_eq!(offsets(&l), vec![0.0, 0.5, 0.5]);
        let c = conic_gradient_ops("conic-gradient(from 90deg at 25% 50%, rgb(0, 0, 0) 0deg 90deg, rgb(255, 255, 255) 0.5turn)", 0.0, 0.0, 100.0, 100.0, 0).unwrap();
        assert_eq!(offsets(&c), vec![0.0, 0.25, 0.5]);
        let Canvas2DOp::DefineConicGradient { start_angle, x, y, .. } = c[0] else { panic!("{:?}", c[0]) };
        // CSS's 90deg (3 o'clock) is canvas's 0
        assert_eq!((start_angle, x, y), (0.0, 25.0, 50.0));
    }

    /// A text that something follows on its line draws within the room before it (the layout measures with the
    /// persona's fonts, the glyphs are this machine's); the last text of a line draws as it is.
    #[test]
    fn a_text_draws_within_the_room_before_what_follows_it() {
        let html = "<body style='margin:0;font:16px sans-serif'><p style='width:600px'>an <a href=#>application</a> for accessing</p></body>";
        let page = Page::default();
        let dom = korvus_dom::Dom::parse(html);
        let media = MediaContext::default();
        let laid = page.layout(&dom, &media);
        let (ops, _, _) = paint_display_list(&page, &dom, &media, &laid, 800.0, 600.0);
        let texts: Vec<(String, f64, Option<f64>)> = ops
            .iter()
            .filter_map(|o| if let Canvas2DOp::FillText { text, x, max_width, .. } = o { Some((text.clone(), *x, *max_width)) } else { None })
            .collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        // each of the first two is held to where the next starts -- less the trailing space it does not draw, which
        // stays a gap (a 16px space, about 4.5px); the last is not held at all
        let space = (texts[1].1 - texts[0].1) - texts[0].2.expect("held");
        assert!((3.0..6.0).contains(&space), "the room keeps the space out: {texts:?}");
        assert_eq!(texts[1].2, Some(texts[2].1 - texts[1].1), "{texts:?}");
        assert_eq!(texts[2].2, None, "{texts:?}");
        assert_eq!(texts[0].0, "an", "a trailing space draws nothing: {texts:?}");
    }

    /// Replaced content is placed by its `object-fit` and `object-position`: a 64 x 16 image in a 90 x 60 box.
    #[test]
    fn replaced_content_is_fitted_and_placed() {
        let fit = |f: &str, p: &str| {
            let (position_x, position_y) = position_halves(p);
            ObjectFit { fit: f.into(), position_x, position_y }.place((64.0, 16.0), (90.0, 60.0))
        };
        assert_eq!(fit("fill", "50% 50%"), (0.0, 0.0, 90.0, 60.0));
        assert_eq!(fit("contain", "50% 50%"), (0.0, 18.75, 90.0, 22.5));
        assert_eq!(fit("cover", "left top"), (0.0, 0.0, 240.0, 60.0));
        assert_eq!(fit("none", "right 5px bottom 5px"), (21.0, 39.0, 64.0, 16.0));
        assert_eq!(fit("scale-down", "50% 50%"), (13.0, 22.0, 64.0, 16.0));
    }

    #[test]
    fn a_layer_is_sized_and_placed_as_css_says() {
        let n = Some((40.0, 20.0));
        assert_eq!(background_tile_size("auto", n, (100.0, 100.0)), (40.0, 20.0));
        assert_eq!(background_tile_size("cover", n, (100.0, 100.0)), (200.0, 100.0));
        assert_eq!(background_tile_size("contain", n, (100.0, 100.0)), (100.0, 50.0));
        assert_eq!(background_tile_size("50% auto", n, (100.0, 100.0)), (50.0, 25.0));
        assert_eq!(background_tile_size("100px 50px", n, (100.0, 100.0)), (100.0, 50.0));
        // a gradient has no size of its own: `auto` is the area's
        assert_eq!(background_tile_size("auto", None, (100.0, 80.0)), (100.0, 80.0));
        assert_eq!(background_tile_size("cover", None, (100.0, 80.0)), (100.0, 80.0));
        assert_eq!(background_tile_size("20px auto", None, (100.0, 80.0)), (20.0, 80.0));
        assert_eq!(background_tile_size("30px 30px", None, (100.0, 80.0)), (30.0, 30.0));
        assert_eq!(background_offset("50%", 60.0), 30.0);
        assert_eq!(background_offset("-16px", 60.0), -16.0);
        assert_eq!(background_offset("calc(100% - 10px)", 60.0), 50.0);
        assert_eq!(background_repeats("repeat-x"), (true, false));
        assert_eq!(background_repeats("no-repeat"), (false, false));
        assert_eq!(background_repeats("repeat no-repeat"), (true, false));
    }

    /// A radial gradient's size: `farthest-corner` by default, a circle's keyword one radius, explicit lengths an
    /// ellipse; an ellipse is a circle of its horizontal radius under a vertical scale.
    #[test]
    fn a_radial_gradient_is_sized_and_placed() {
        let radius = |image: &str| {
            let ops = radial_gradient_ops(image, 0.0, 0.0, 200.0, 100.0, 0).unwrap();
            let k = ops.iter().find_map(|o| if let Canvas2DOp::Transform { t } = o { Some(t.d) } else { None }).unwrap();
            let (cx, cy, r) = ops.iter().find_map(|o| if let Canvas2DOp::DefineRadialGradient { x1, y1, r1, .. } = o { Some((*x1, *y1, *r1)) } else { None }).unwrap();
            ((cx * 10.0).round() / 10.0, (cy * 10.0).round() / 10.0, (r * 10.0).round() / 10.0, (r * k * 10.0).round() / 10.0)
        };
        // farthest-corner ellipse: each radius the centre's distance to the far side times sqrt(2)
        assert_eq!(radius("radial-gradient(rgb(0, 0, 0), rgb(255, 255, 255))"), (100.0, 50.0, 141.4, 70.7));
        assert_eq!(radius("radial-gradient(circle closest-side at 30% 40%, rgb(0, 0, 0), rgb(255, 255, 255))"), (60.0, 40.0, 40.0, 40.0));
        assert_eq!(radius("radial-gradient(60px 30px at left 40px top 30px, rgb(0, 0, 0), rgb(255, 255, 255))"), (40.0, 30.0, 60.0, 30.0));
        assert_eq!(radius("radial-gradient(farthest-side at right 20px bottom 10px, rgb(0, 0, 0), rgb(255, 255, 255))"), (180.0, 90.0, 180.0, 90.0));
    }

    /// An inline SVG's `<text>` is drawn with the page's text ops, through its transforms and the viewBox mapping.
    #[test]
    fn svg_text_is_drawn_with_text_ops() {
        let html = "<body style='margin:0'><svg width=120 height=100 viewBox='0 0 60 50'><g transform='translate(10 0)'>\
                    <text x=20 y=30 text-anchor=middle font-size=12 fill='#c00'>Scaled<tspan x=20 dy=10>two</tspan></text></g></svg></body>";
        let page = Page::default();
        let dom = korvus_dom::Dom::parse(html);
        let media = MediaContext::default();
        let laid = page.layout(&dom, &media);
        let (ops, _, _) = paint_display_list(&page, &dom, &media, &laid, 800.0, 600.0);
        let texts: Vec<(String, f64, f64)> =
            ops.iter().filter_map(|o| if let Canvas2DOp::FillText { text, x, y, .. } = o { Some((text.clone(), *x, *y)) } else { None }).collect();
        assert_eq!(texts, vec![("Scaled".to_string(), 20.0, 30.0), ("two".to_string(), 20.0, 40.0)]);
        let scale = ops.iter().find_map(|o| if let Canvas2DOp::Transform { t } = o { Some((t.a, t.e)) } else { None });
        assert_eq!(scale, Some((2.0, 20.0)), "the viewBox doubles it, the group moves it 10 user units");
    }

}

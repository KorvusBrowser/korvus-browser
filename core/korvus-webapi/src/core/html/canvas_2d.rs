//! `CanvasRenderingContext2D` — all 73 members, native, recording into a `spec::Canvas2DOp` display
//! list that `korvus-render` executes at readback.
//!
//! ## Why the state lives here AND in the backend
//!
//! `ctx.lineWidth` is a synchronous read, and `stroke()` is a pixel. The recorder therefore keeps
//! every value a getter can be asked for, and the backend keeps the same values because it needs
//! them at raster time. That is not duplication: it is the two ends of a display list, and the ops
//! in between are the only channel between them. The invariant is that a setter which changes what a
//! getter reports ALSO emits the op that changes what the pixels do — a setter that only did the
//! first would be the exact "reads back correctly, paints nothing" failure this file was written to
//! remove.
//!
//! ## Gradients and patterns
//!
//! A `CanvasGradient` is a live object: `addColorStop` after a draw affects only later draws. So its
//! definition is not a side table but a run of ops — `DefineLinearGradient` then one `AddColorStop`
//! per stop — recorded into the display list at the moment each call happens, and the backend
//! rebuilds it as it replays. The store keeps the same definition a second time, for the one case
//! the ordering cannot cover: a gradient made by one context and used by another, where the second
//! list has never seen the definition and gets it replayed on assignment.

use deno_core::v8;
use spec::{
    Canvas2DOp, CanvasFilter, CompositeOp, HitTest, LineCap, LineJoin, PathOp, PatternRepeat,
    RenderParams, Rgba8, SmoothingQuality, TextAlign, TextBaseline, TextDirection, Transform2D,
};
use std::collections::HashMap;

use super::html_canvas_element::priv_get;
use super::html_input_element::acc1;
use crate::binding::protos::{
    build_fn_data, get_prop, illegal_invocation, int_val, obj_set_num, obj_set_str,
    require_args, str_val, throw_type_error,
};
use crate::install::kits::define_accessor;
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

pub(crate) const CANVAS2D_ID_PRIV: &str = "korvus_canvas2d_id";
/// The context's `canvas` back-reference. A PRIVATE slot, so
/// `Object.getOwnPropertyNames(ctx)` reads empty the way Chrome's does; before the table cutover the
/// back-reference was an own data property whose only job was to SHADOW the shell's `canvas` stub,
/// and the interface's own row now answers it.
pub(crate) const CANVAS_BACKREF_PRIV: &str = "korvus_canvas2d_backref";
/// The id of a `CanvasGradient` / `CanvasPattern`, on the object itself. A v8 Private is invisible to
/// every JS reflection path, so the object keeps a real browser's shape (one method, nothing else).
const PAINT_ID_PRIV: &str = "korvus_canvas_paint_id";
/// The id of a `Path2D`'s segment list.
const PATH2D_ID_PRIV: &str = "korvus_path2d_id";

// ── state ─────────────────────────────────────────────────────────────────────────────────────

/// `fillStyle` / `strokeStyle` is either a CSS colour or one of the two paint objects. The colour is
/// kept in its SERIALIZED form, because that is what the getter must return (`ctx.fillStyle = "red"`
/// reads back as `"#ff0000"` in every browser).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StyleRef {
    Css(String),
    Paint(u32),
}

/// Everything `save()` pushes and `restore()` pops, as the recorder sees it.
#[derive(Clone, Debug)]
pub(crate) struct Canvas2DStyle {
    fill: StyleRef,
    stroke: StyleRef,
    font: String,
    composite: String,
    filter: String,
    global_alpha: f64,
    line_width: f64,
    line_cap: String,
    line_join: String,
    miter_limit: f64,
    line_dash: Vec<f64>,
    line_dash_offset: f64,
    shadow_color: String,
    shadow_blur: f64,
    shadow_offset_x: f64,
    shadow_offset_y: f64,
    text_align: String,
    text_baseline: String,
    direction: String,
    letter_spacing: String,
    word_spacing: String,
    font_kerning: String,
    font_stretch: String,
    font_variant_caps: String,
    text_rendering: String,
    lang: String,
    image_smoothing: bool,
    image_smoothing_quality: String,
    ctm: Transform2D,
}

impl Default for Canvas2DStyle {
    fn default() -> Self {
        Self {
            fill: StyleRef::Css("#000000".into()),
            stroke: StyleRef::Css("#000000".into()),
            font: "10px sans-serif".into(),
            composite: "source-over".into(),
            filter: "none".into(),
            global_alpha: 1.0,
            line_width: 1.0,
            line_cap: "butt".into(),
            line_join: "miter".into(),
            miter_limit: 10.0,
            line_dash: Vec::new(),
            line_dash_offset: 0.0,
            shadow_color: "rgba(0, 0, 0, 0)".into(),
            shadow_blur: 0.0,
            shadow_offset_x: 0.0,
            shadow_offset_y: 0.0,
            text_align: "start".into(),
            text_baseline: "alphabetic".into(),
            // Stored as the IDL's initial value; the getter resolves it (see `inherited_direction`).
            direction: "inherit".into(),
            letter_spacing: "0px".into(),
            word_spacing: "0px".into(),
            font_kerning: "auto".into(),
            font_stretch: "normal".into(),
            font_variant_caps: "normal".into(),
            text_rendering: "auto".into(),
            lang: "inherit".into(),
            image_smoothing: true,
            image_smoothing_quality: "low".into(),
            ctm: Transform2D::IDENTITY,
        }
    }
}

/// Recorded 2D drawing state for one canvas context, keyed by a per-object id.
#[derive(Default)]
pub(crate) struct Canvas2DState {
    pub(crate) ops: Vec<Canvas2DOp>,
    /// The bitmap is NOT origin-clean: something cross-origin was drawn into it. Only
    /// resetting the bitmap -- a `width`/`height` write -- clears it; `reset()` does not (measured).
    pub(crate) tainted: bool,
    /// Blink made this context's resource provider before the ops were last cleared (PHASE 3 F7): it is never
    /// unmade, so a `reset()` or a `width` write after a draw still exports the provider's sRGB-tagged
    /// snapshot (Chrome 150, measured). See [`has_paint_canvas`].
    pub(crate) provider: bool,
    st: Canvas2DStyle,
    stack: Vec<Canvas2DStyle>,
}

/// Whether Blink would have made this 2D context's resource provider: a call reached
/// `Canvas2DRecorderContext::GetOrCreatePaintCanvas()` (or `WritePixels`, for `putImageData`). From then on its
/// snapshot carries the canvas's colour space, and a JPEG or WebP export embeds the sRGB profile; before, the
/// canvas exports Blink's transparent image, which has none. Chrome 150, measured over 21 ways of touching an
/// 8x8 canvas: `save`/`restore` and the transforms, `fillRect`/`strokeRect`/`clearRect` of a non-empty-or-
/// one-sided rect (`ValidateRectForCanvas`: finite, `width || height`), a fill or stroke of a non-empty path,
/// `clip`, `fillText` (even of `""`), `drawImage`, `putImageData`, `isPointInPath`/`isPointInStroke` make it;
/// setting a style, building a path, `measureText` and `getImageData` do not.
pub(crate) fn has_paint_canvas(s: &Canvas2DState) -> bool {
    s.provider || ops_make_paint_canvas(&s.ops)
}

fn ops_make_paint_canvas(ops: &[Canvas2DOp]) -> bool {
    // `CanvasPath::IsEmpty`: no verb since the last `beginPath()`.
    let mut path = false;
    for op in ops {
        match op {
            Canvas2DOp::BeginPath => path = false,
            Canvas2DOp::MoveTo { .. }
            | Canvas2DOp::LineTo { .. }
            | Canvas2DOp::Arc { .. }
            | Canvas2DOp::Rect { .. }
            | Canvas2DOp::BezierCurveTo { .. }
            | Canvas2DOp::QuadraticCurveTo { .. }
            | Canvas2DOp::ArcTo { .. }
            | Canvas2DOp::Ellipse { .. }
            | Canvas2DOp::RoundRect { .. } => path = true,
            Canvas2DOp::Save
            | Canvas2DOp::Restore
            | Canvas2DOp::Transform { .. }
            | Canvas2DOp::SetTransform { .. }
            | Canvas2DOp::Clip { .. }
            | Canvas2DOp::ClipPath { .. }
            | Canvas2DOp::FillText { .. }
            | Canvas2DOp::StrokeText { .. }
            | Canvas2DOp::DrawImage { .. }
            | Canvas2DOp::PutImageData { .. } => return true,
            Canvas2DOp::FillRect { w, h, .. } | Canvas2DOp::StrokeRect { w, h, .. } | Canvas2DOp::ClearRect { w, h, .. }
                if *w != 0.0 || *h != 0.0 =>
            {
                return true
            }
            Canvas2DOp::Fill { .. } | Canvas2DOp::Stroke if path => return true,
            Canvas2DOp::FillPath { path: p, .. } | Canvas2DOp::StrokePath { path: p } if !p.is_empty() => return true,
            _ => {}
        }
    }
    false
}

/// A `CanvasGradient` or `CanvasPattern`, at the store level rather than inside one display list:
/// the object outlives any single list, and a page may hand it to a second context.
struct PaintObj {
    /// The context whose display list already carries this definition.
    owner: u32,
    /// `Define*` followed by its `AddColorStop`s — replayable into a list that has not seen it.
    def: Vec<Canvas2DOp>,
    pattern: bool,
    /// A pattern made from a source that was not origin-clean. It taints a context the moment it is
    /// ASSIGNED to `fillStyle`/`strokeStyle`, before anything is filled (measured).
    tainted: bool,
}

/// The last raster of each 2D context: a read of a canvas whose ops and size have not
/// changed since the last read is answered from it instead of replaying every op once more (a probe that
/// reads `toDataURL` three times paid the whole replay three times). A few contexts only: fingerprinting
/// pages make many throwaway canvases.
#[derive(Default)]
pub(crate) struct Raster2DMemo(HashMap<u32, (u32, u32, Vec<Canvas2DOp>, Rgba8)>);

pub(crate) fn raster_memo(state: &mut deno_core::OpState, id: u32, ops: &[Canvas2DOp], w: u32, h: u32, render: impl FnOnce() -> Rgba8) -> Rgba8 {
    if let Some((mw, mh, mops, img)) = state.try_borrow::<Raster2DMemo>().and_then(|m| m.0.get(&id)) {
        if *mw == w && *mh == h && mops.as_slice() == ops {
            return img.clone();
        }
    }
    let img = render();
    if state.try_borrow::<Raster2DMemo>().is_none() {
        state.put(Raster2DMemo::default());
    }
    let memo = state.borrow_mut::<Raster2DMemo>();
    if memo.0.len() >= 16 && !memo.0.contains_key(&id) {
        memo.0.clear();
    }
    memo.0.insert(id, (w, h, ops.to_vec(), img.clone()));
    img
}

/// All live 2D contexts' display lists, in OpState. Keyed by an id stashed on a v8::Private slot of
/// the context object (mirrors the EventTarget per-object id pattern).
#[derive(Default)]
pub(crate) struct Canvas2DStore {
    pub(crate) ctx: HashMap<u32, Canvas2DState>,
    next_id: u32,
    paints: HashMap<u32, PaintObj>,
    /// The paint objects themselves, so `ctx.fillStyle` can hand back the gradient a page assigned.
    paint_objs: HashMap<u32, v8::Global<v8::Value>>,
    /// `Path2D` segment lists, by the id on the object.
    paths: HashMap<u32, Vec<PathOp>>,
    next_obj: u32,
}

/// This file's store, created on first touch when a harness built a bare `OpState` (`webapi::lib.rs` puts
/// one at extension setup). Every WRITE goes through here, and every read follows the write that allocated
/// what it reads (an id on the object), so no body finds the store missing however the realm was assembled
/// -- a realm built from templates mounts these bodies without running a row installer (the
/// registry's `OffscreenCanvasRenderingContext2D` rows name them, and `installer_parity` builds that
/// registry in a bare runtime). The animation store is made the same way, for the same reason.
fn store_mut(st: &mut deno_core::OpState) -> &mut Canvas2DStore {
    if !st.has::<Canvas2DStore>() {
        st.put(Canvas2DStore::default());
    }
    st.borrow_mut::<Canvas2DStore>()
}

/// Read — or allocate — the display-list id stashed on a 2D-context object.
pub(crate) fn canvas2d_id(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
) -> u32 {
    let pname = v8::String::new(scope, CANVAS2D_ID_PRIV).unwrap();
    let key = v8::Private::for_api(scope, Some(pname));
    if let Some(id) = obj.get_private(scope, key).and_then(|v| v.uint32_value(scope)).filter(|&id| id != 0) {
        return id;
    }
    let id = {
        let mut st = op_state.borrow_mut();
        let s = store_mut(&mut st);
        s.next_id += 1;
        let id = s.next_id;
        s.ctx.entry(id).or_default();
        id
    };
    let idv = int_val(scope, id as i64);
    obj.set_private(scope, key, idv);
    id
}

/// Read a private numeric id off an object, without allocating one.
fn read_priv_id(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) -> Option<u32> {
    let pname = v8::String::new(scope, name)?;
    let key = v8::Private::for_api(scope, Some(pname));
    obj.get_private(scope, key).and_then(|v| v.uint32_value(scope)).filter(|&id| id != 0)
}

fn set_priv_id(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, id: u32) {
    if let Some(pname) = v8::String::new(scope, name) {
        let key = v8::Private::for_api(scope, Some(pname));
        let v = int_val(scope, id as i64);
        obj.set_private(scope, key, v);
    }
}

/// Append recorded ops to a context's display list.
fn push_ops(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, ops: Vec<Canvas2DOp>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = canvas2d_id(scope, this, &op_state);
    let mut st = op_state.borrow_mut();
    store_mut(&mut st).ctx.entry(id).or_default().ops.extend(ops);
}

pub(crate) fn canvas2d_push(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, op: Canvas2DOp) {
    push_ops(scope, this, vec![op]);
}

/// Read something out of a context's recorded state.
fn read<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, f: impl FnOnce(&Canvas2DState) -> R) -> R
where
    R: Default,
{
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = canvas2d_id(scope, this, &op_state);
    let st = op_state.borrow();
    match st.borrow::<Canvas2DStore>().ctx.get(&id) {
        Some(s) => f(s),
        None => R::default(),
    }
}

/// Mutate a context's recorded state (and usually record an op in the same breath).
fn edit<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, f: impl FnOnce(&mut Canvas2DState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = canvas2d_id(scope, this, &op_state);
    let mut st = op_state.borrow_mut();
    f(store_mut(&mut st).ctx.entry(id).or_default())
}

// ── parsing + serialization ───────────────────────────────────────────────────────────────────

/// Map a canvas `globalCompositeOperation` string to a [`CompositeOp`]. An unknown value returns
/// `None` (the assignment is ignored, per spec).
pub(crate) fn parse_composite(s: &str) -> Option<CompositeOp> {
    Some(match s.trim() {
        "source-over" => CompositeOp::SourceOver,
        "source-in" => CompositeOp::SourceIn,
        "source-out" => CompositeOp::SourceOut,
        "source-atop" => CompositeOp::SourceAtop,
        "destination-over" => CompositeOp::DestinationOver,
        "destination-in" => CompositeOp::DestinationIn,
        "destination-out" => CompositeOp::DestinationOut,
        "destination-atop" => CompositeOp::DestinationAtop,
        "lighter" => CompositeOp::Lighter,
        "copy" => CompositeOp::Copy,
        "xor" => CompositeOp::Xor,
        "multiply" => CompositeOp::Multiply,
        "screen" => CompositeOp::Screen,
        "overlay" => CompositeOp::Overlay,
        "darken" => CompositeOp::Darken,
        "lighten" => CompositeOp::Lighten,
        "color-dodge" => CompositeOp::ColorDodge,
        "color-burn" => CompositeOp::ColorBurn,
        "hard-light" => CompositeOp::HardLight,
        "soft-light" => CompositeOp::SoftLight,
        "difference" => CompositeOp::Difference,
        "exclusion" => CompositeOp::Exclusion,
        "hue" => CompositeOp::Hue,
        "saturation" => CompositeOp::Saturation,
        "color" => CompositeOp::Color,
        "luminosity" => CompositeOp::Luminosity,
        _ => return None,
    })
}

/// A CSS colour, through the page pipeline's parser (`spec::CssHandle`).
///
/// This replaces a 57-line parser that knew seven colour names and only comma-separated `rgb()`, so
/// `rebeccapurple`, `#f008`, `hsl()` and `rgb(0 0 0 / 50%)` were all silently "not a colour" on the
/// canvas while the cascade understood every one of them. Two parsers that disagree are not
/// duplication, they are an *inconsistency*: `ctx.fillStyle = "rebeccapurple"` and
/// `getComputedStyle(el).color` are the same question, and a page can ask both.
///
/// `None` when there is no page pipeline: a canvas without one cannot know what a colour is, and
/// per spec an unparseable `fillStyle` assignment is ignored rather than guessed at.
pub(crate) fn parse_css_color(scope: &mut v8::PinScope, s: &str) -> Option<[u8; 4]> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>()?.get::<spec::CssHandle>()?.0.parse_color(s)
}

/// A colour string as this context takes it -- `Canvas2DRecorderContext::ParseColorOrCurrentColor`:
/// the page's colour parser in Blink's colour model (`spec::CssValues::parse_canvas_color`: CSS Color 4/5,
/// kept as written, painted with `toSkColor4f`, read back with `SerializeAsCanvasColor`), `currentcolor` -- alone or
/// inside `color-mix()` or a relative colour -- being [`current_color_text`]. `None` = not a colour, and the
/// assignment is ignored.
fn canvas_color(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, raw: &str) -> Option<spec::CanvasColor> {
    let element = iface_of(scope, this) == "CanvasRenderingContext2D";
    let current = if raw.to_ascii_lowercase().contains("currentcolor") { current_color_text(scope, this) } else { None };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>()?.get::<spec::CssHandle>()?.0.parse_canvas_color(raw, element, current.as_deref())
}

/// `CanvasRenderingContext2D::GetCurrentColor`'s source: a CONNECTED `<canvas>`'s INLINE `color`, which the parser
/// reads as `CSSParser::ParseColor` reads it, black when it is not a colour -- not the element's computed colour
/// (stock Chrome 150: a canvas coloured by a class or by inheritance, or removed from its document, reads
/// `currentcolor` as `#000000`). An `OffscreenCanvas` context has none.
fn current_color_text(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    if iface_of(scope, this) != "CanvasRenderingContext2D" {
        return None;
    }
    let canvas = priv_get(scope, this, CANVAS_BACKREF_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    if !crate::page::is_connected(scope, canvas.into()) {
        return None;
    }
    let id = crate::core::dom::element::el_id_of(scope, canvas)?;
    crate::page::with_host_dom(scope, |d| crate::core::css::css_style_declaration::inline_color(d, id)).flatten()
}

/// Parse the CSS `font` shorthand into (family, size in px, bold, italic). Minimal but covers the
/// probe forms ("11pt Arial", "16px serif", "bold 18px Arial"); pt→px is 96/72. The first family in
/// a list is taken (Chrome tries each; single-family probe forms resolve identically).
/// Line-height/variant/stretch are ignored for now — refined at calibration if a metric depends on
/// them.
///
/// `None` when the string is not a font shorthand at all. The `font` shorthand REQUIRES a size and a
/// family, and HTML says an unparseable assignment leaves the attribute alone — so
/// `ctx.font = "not a font"` must read back as `"10px sans-serif"`, not as itself.
pub(crate) fn parse_font(s: &str) -> Option<(String, f64, bool, bool)> {
    let (family, size_px, bold, italic) = parse_font_parts(s)?;
    Some((family, size_px, bold, italic))
}

fn parse_font_parts(s: &str) -> Option<(String, f64, bool, bool)> {
    let (mut bold, mut italic, mut size_px) = (false, false, 10.0);
    let toks: Vec<&str> = s.split_whitespace().collect();
    let mut size_idx = None;
    for (i, t) in toks.iter().enumerate() {
        let tl = t.to_ascii_lowercase();
        if tl == "italic" || tl == "oblique" {
            italic = true;
        } else if tl == "bold" || tl == "bolder" {
            bold = true;
        } else if (tl.ends_with("px") || tl.ends_with("pt")) && size_idx.is_none() {
            let n = tl.trim_end_matches("px").trim_end_matches("pt");
            let n = n.split('/').next().unwrap_or(n);
            if let Ok(v) = n.parse::<f64>() {
                size_px = if tl.ends_with("pt") { v * 96.0 / 72.0 } else { v };
                size_idx = Some(i);
            }
        } else if let Ok(w) = tl.parse::<f64>() {
            // Font matching takes the bold face from weight 600 up when a face has only 400 and 700.
            if w >= 600.0 {
                bold = true;
            }
        }
    }
    // No size token, or nothing after it: not a `font` shorthand.
    let i = size_idx?;
    if i + 1 >= toks.len() {
        return None;
    }
    let fam = toks[i + 1..].join(" ");
    let family = fam.split(',').next().unwrap_or(&fam).trim().trim_matches(['"', '\'']).to_string();
    if family.is_empty() {
        return None;
    }
    Some((family, size_px, bold, italic))
}

/// The whole family LIST of a `font` shorthand -- everything after the size -- which measurement walks
/// the way the browser does: `16px Georgia, serif` is serif on a persona without Georgia.
/// [`parse_font`] keeps the first family, for the renderer.
/// The language canvas text is shaped in (the UI locale: the persona's first language) and the face a
/// family list naming nothing installed falls back to (the persona's default).
fn text_locale(scope: &mut v8::PinScope) -> (String, String) {
    let lang = crate::core::navigator::navigator::persona(scope)
        .and_then(|p| p.navigator().languages.first().cloned())
        .unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let font_map = crate::core::dom::geometry_queries::media_context(&st).theme.font_map;
    let default_family = st
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<spec::CssHandle>())
        .map(|h| h.0.default_family(font_map))
        .unwrap_or_else(|| "sans-serif".into());
    (lang, default_family)
}

fn font_family_list(s: &str) -> Option<String> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let i = toks.iter().position(|t| {
        let tl = t.to_ascii_lowercase();
        (tl.ends_with("px") || tl.ends_with("pt"))
            && tl.trim_end_matches("px").trim_end_matches("pt").split('/').next().is_some_and(|n| n.parse::<f64>().is_ok())
    })?;
    let list = toks.get(i + 1..)?.join(" ");
    (!list.trim().is_empty()).then_some(list)
}

/// A CSS `<length>` in px, for `letterSpacing` / `wordSpacing`. Only absolute units, because the
/// canvas has no element to resolve `em` against without a font size that is itself being set here.
fn parse_px(s: &str) -> Option<f64> {
    let t = s.trim();
    let (num, scale) = if let Some(n) = t.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = t.strip_suffix("pt") {
        (n, 96.0 / 72.0)
    } else if let Some(n) = t.strip_suffix("pc") {
        (n, 16.0)
    } else if let Some(n) = t.strip_suffix("in") {
        (n, 96.0)
    } else if let Some(n) = t.strip_suffix("cm") {
        (n, 96.0 / 2.54)
    } else if let Some(n) = t.strip_suffix("mm") {
        (n, 96.0 / 25.4)
    } else if let Some(n) = t.strip_suffix("q") {
        (n, 96.0 / 101.6)
    } else {
        return None;
    };
    let v: f64 = num.trim().parse().ok()?;
    v.is_finite().then_some(v * scale)
}

/// Parse the `filter` shorthand into the chain the backend executes. `None` means "not a filter" and
/// the assignment is ignored; an empty chain is `none`.
fn parse_filter(scope: &mut v8::PinScope, s: &str) -> Option<Vec<CanvasFilter>> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut rest = t;
    while !rest.trim().is_empty() {
        rest = rest.trim_start();
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        if close < open {
            return None;
        }
        let name = rest[..open].trim().to_ascii_lowercase();
        let arg = rest[open + 1..close].trim().to_string();
        rest = &rest[close + 1..];
        // `<number>` or `<percentage>`; a missing argument means the function's own default.
        let ratio = |d: f64| -> Option<f64> {
            if arg.is_empty() {
                return Some(d);
            }
            match arg.strip_suffix('%') {
                Some(n) => n.trim().parse::<f64>().ok().map(|v| v / 100.0),
                None => arg.parse::<f64>().ok(),
            }
        };
        let f = match name.as_str() {
            // CSS blur() takes a radius; the Gaussian sigma is half of it, as in Blink.
            "blur" => CanvasFilter::Blur { sigma: parse_px(&arg).or_else(|| arg.parse().ok())? / 2.0 },
            "brightness" => CanvasFilter::Brightness { amount: ratio(1.0)? },
            "contrast" => CanvasFilter::Contrast { amount: ratio(1.0)? },
            "grayscale" => CanvasFilter::Grayscale { amount: ratio(1.0)? },
            "invert" => CanvasFilter::Invert { amount: ratio(1.0)? },
            "opacity" => CanvasFilter::Opacity { amount: ratio(1.0)? },
            "saturate" => CanvasFilter::Saturate { amount: ratio(1.0)? },
            "sepia" => CanvasFilter::Sepia { amount: ratio(1.0)? },
            "hue-rotate" => {
                let a = arg.trim();
                let deg = if let Some(n) = a.strip_suffix("deg") {
                    n.trim().parse::<f64>().ok()?
                } else if let Some(n) = a.strip_suffix("rad") {
                    n.trim().parse::<f64>().ok()?.to_degrees()
                } else if let Some(n) = a.strip_suffix("turn") {
                    n.trim().parse::<f64>().ok()? * 360.0
                } else if a.is_empty() {
                    0.0
                } else {
                    a.parse::<f64>().ok()?
                };
                CanvasFilter::HueRotate { degrees: deg }
            }
            "drop-shadow" => {
                // `drop-shadow(<x> <y> <blur>? <color>?)`.
                let parts: Vec<&str> = arg.split_whitespace().collect();
                if parts.len() < 2 {
                    return None;
                }
                let dx = parse_px(parts[0]).or_else(|| parts[0].parse().ok())?;
                let dy = parse_px(parts[1]).or_else(|| parts[1].parse().ok())?;
                let blur = parts.get(2).and_then(|p| parse_px(p)).unwrap_or(0.0);
                let color = parts
                    .get(3)
                    .and_then(|p| parse_css_color(scope, p))
                    .unwrap_or([0, 0, 0, 255]);
                CanvasFilter::DropShadow {
                    dx,
                    dy,
                    sigma: blur / 2.0,
                    r: color[0],
                    g: color[1],
                    b: color[2],
                    a: color[3],
                }
            }
            _ => return None,
        };
        out.push(f);
    }
    (!out.is_empty()).then_some(out)
}

// ── small v8 helpers ──────────────────────────────────────────────────────────────────────────

/// Enforce a method's required-argument count the way Blink does: a `TypeError` naming the member,
/// with Chrome's own message. An implementation that skips this is detectable in one line —
/// `ctx.moveTo()` returns `undefined` where every browser throws.
/// A number as a page would print it (`-1`, `-2.5`): JavaScript's own ToString, which Chrome's canvas
/// sentences quote the offending value with.
fn js_num(scope: &mut v8::PinScope, x: f64) -> String {
    let n: v8::Local<v8::Value> = v8::Number::new(scope, x).into();
    n.to_rust_string_lossy(scope)
}

fn need(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, n: i32, member: &str) -> bool {
    // Every call passes through here: the interface's name is looked up only for the sentence.
    if args.length() >= n {
        return true;
    }
    let iface = iface_of(scope, args.this());
    require_args(scope, args, n as usize, iface, member)
}

fn num(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).number_value(scope).unwrap_or(f64::NAN)
}

pub(crate) fn rect_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> (f64, f64, f64, f64) {
    (num(scope, args, 0), num(scope, args, 1), num(scope, args, 2), num(scope, args, 3))
}

fn all_finite(vs: &[f64]) -> bool {
    vs.iter().all(|v| v.is_finite())
}

fn set_str(scope: &mut v8::PinScope, mut rv: v8::ReturnValue, s: &str) {
    let v = v8::String::new(scope, s).unwrap_or_else(|| v8::String::empty(scope));
    rv.set(v.into());
}

// ── the state accessors ───────────────────────────────────────────────────────────────────────
//
// One getter and one setter callback for the whole family, dispatching on the member name carried as
// the function's `data`. Each is still a distinct native function object with its own `name` and
// `length` (`get lineWidth` / 0, `set lineWidth` / 1), so the surface is exactly what a real
// prototype shows; only the Rust body is shared.

pub(crate) fn cb_2d_attr_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    // The two paint-object-valued members answer with the object itself, not a string.
    if key == "fillStyle" || key == "strokeStyle" {
        let which = read(scope, this, |s| {
            Some(if key == "fillStyle" { s.st.fill.clone() } else { s.st.stroke.clone() })
        });
        match which {
            Some(StyleRef::Css(c)) => set_str(scope, rv, &c),
            Some(StyleRef::Paint(id)) => {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let g = { op_state.borrow().borrow::<Canvas2DStore>().paint_objs.get(&id).cloned() };
                match g {
                    Some(g) => {
                        let v = v8::Local::new(scope, &g);
                        rv.set(v);
                    }
                    None => set_str(scope, rv, "#000000"),
                }
            }
            None => set_str(scope, rv, "#000000"),
        }
        return;
    }
    let st = read(scope, this, |s| Some(s.st.clone()));
    let st = st.unwrap_or_default();
    match key.as_str() {
        "globalAlpha" => rv.set(v8::Number::new(scope, st.global_alpha).into()),
        "lineWidth" => rv.set(v8::Number::new(scope, st.line_width).into()),
        "miterLimit" => rv.set(v8::Number::new(scope, st.miter_limit).into()),
        "lineDashOffset" => rv.set(v8::Number::new(scope, st.line_dash_offset).into()),
        "shadowBlur" => rv.set(v8::Number::new(scope, st.shadow_blur).into()),
        "shadowOffsetX" => rv.set(v8::Number::new(scope, st.shadow_offset_x).into()),
        "shadowOffsetY" => rv.set(v8::Number::new(scope, st.shadow_offset_y).into()),
        "imageSmoothingEnabled" => rv.set(v8::Boolean::new(scope, st.image_smoothing).into()),
        "font" => set_str(scope, rv, &st.font),
        "globalCompositeOperation" => set_str(scope, rv, &st.composite),
        "filter" => set_str(scope, rv, &st.filter),
        "lineCap" => set_str(scope, rv, &st.line_cap),
        "lineJoin" => set_str(scope, rv, &st.line_join),
        "shadowColor" => set_str(scope, rv, &st.shadow_color),
        "textAlign" => set_str(scope, rv, &st.text_align),
        "textBaseline" => set_str(scope, rv, &st.text_baseline),
        // `direction` never READS BACK as "inherit": the getter resolves it against the canvas
        // element's computed direction, which is why a fresh context answers "ltr" and why setting
        // it to "inherit" answers "ltr" too rather than echoing the word.
        "direction" => {
            let d = if st.direction == "inherit" { inherited_direction(scope, this) } else { st.direction.clone() };
            set_str(scope, rv, &d)
        }
        "letterSpacing" => set_str(scope, rv, &st.letter_spacing),
        "wordSpacing" => set_str(scope, rv, &st.word_spacing),
        "fontKerning" => set_str(scope, rv, &st.font_kerning),
        "fontStretch" => set_str(scope, rv, &st.font_stretch),
        "fontVariantCaps" => set_str(scope, rv, &st.font_variant_caps),
        "textRendering" => set_str(scope, rv, &st.text_rendering),
        "lang" => set_str(scope, rv, &st.lang),
        "imageSmoothingQuality" => set_str(scope, rv, &st.image_smoothing_quality),
        _ => rv.set_undefined(),
    }
}

/// The direction `direction = "inherit"` resolves to: the canvas's, as Blink's host answers it
/// ([`super::html_canvas_element::host_text_direction`]). "ltr" when there is no canvas to ask —
/// which is the initial value of the CSS property, so it is the same answer, not a guess.
fn inherited_direction(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    match priv_get(scope, this, CANVAS_BACKREF_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(canvas) => super::html_canvas_element::host_text_direction(scope, canvas).to_string(),
        None => "ltr".into(),
    }
}

/// One of a fixed set of keywords, or the assignment is ignored (HTML's rule for every canvas enum).
fn keyword(v: &str, allowed: &[&str]) -> Option<String> {
    let l = v.to_ascii_lowercase();
    allowed.iter().find(|a| **a == l).map(|a| (*a).to_string())
}

pub(crate) fn cb_2d_attr_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    let val = args.get(0);
    match key.as_str() {
        "fillStyle" | "strokeStyle" => style_set(scope, this, val, key == "fillStyle"),
        // Numbers. HTML: a non-finite or out-of-range value leaves the attribute unchanged.
        "globalAlpha" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() && (0.0..=1.0).contains(&v) {
                edit(scope, this, |s| {
                    s.st.global_alpha = v;
                    s.ops.push(Canvas2DOp::SetGlobalAlpha { alpha: v });
                });
            }
        }
        "lineWidth" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() && v > 0.0 {
                edit(scope, this, |s| {
                    s.st.line_width = v;
                    s.ops.push(Canvas2DOp::SetLineWidth { width: v });
                });
            }
        }
        "miterLimit" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() && v > 0.0 {
                edit(scope, this, |s| {
                    s.st.miter_limit = v;
                    s.ops.push(Canvas2DOp::SetMiterLimit { limit: v });
                });
            }
        }
        "lineDashOffset" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() {
                edit(scope, this, |s| {
                    s.st.line_dash_offset = v;
                    s.ops.push(Canvas2DOp::SetLineDashOffset { offset: v });
                });
            }
        }
        "shadowBlur" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() && v >= 0.0 {
                edit(scope, this, |s| {
                    s.st.shadow_blur = v;
                    s.ops.push(Canvas2DOp::SetShadowBlur { blur: v });
                });
            }
        }
        "shadowOffsetX" | "shadowOffsetY" => {
            let v = val.number_value(scope).unwrap_or(f64::NAN);
            if v.is_finite() {
                let x_axis = key == "shadowOffsetX";
                edit(scope, this, |s| {
                    if x_axis {
                        s.st.shadow_offset_x = v;
                    } else {
                        s.st.shadow_offset_y = v;
                    }
                    s.ops.push(Canvas2DOp::SetShadowOffset { x: s.st.shadow_offset_x, y: s.st.shadow_offset_y });
                });
            }
        }
        "imageSmoothingEnabled" => {
            let v = val.boolean_value(scope);
            edit(scope, this, |s| {
                s.st.image_smoothing = v;
                s.ops.push(Canvas2DOp::SetImageSmoothing { enabled: v });
            });
        }
        "shadowColor" => {
            // A `DOMString`: any value converted to a string, which may throw.
            let Some(text) = val.to_string(scope) else { return };
            let raw = text.to_rust_string_lossy(scope);
            if let Some(c) = canvas_color(scope, this, &raw) {
                edit(scope, this, |s| {
                    s.st.shadow_color = c.text;
                    s.ops.push(Canvas2DOp::SetShadowColor { color: c.paint });
                });
            }
        }
        "font" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some((family, size_px, bold, italic)) = parse_font(&raw) {
                let families = font_family_list(&raw).unwrap_or_else(|| family.clone());
                let (lang, default_family) = text_locale(scope);
                edit(scope, this, |s| {
                    s.st.font = raw;
                    s.ops.push(Canvas2DOp::SetFont { family, size_px, bold, italic, weight: if bold { 700 } else { 400 }, families, lang, default_family });
                });
            }
        }
        "globalCompositeOperation" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(op) = parse_composite(&raw) {
                edit(scope, this, |s| {
                    s.st.composite = raw;
                    s.ops.push(Canvas2DOp::SetGlobalCompositeOperation { op });
                });
            }
        }
        "filter" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(filters) = parse_filter(scope, &raw) {
                edit(scope, this, |s| {
                    s.st.filter = if filters.is_empty() { "none".into() } else { raw };
                    s.ops.push(Canvas2DOp::SetFilter { filters });
                });
            }
        }
        "lineCap" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["butt", "round", "square"]) {
                let cap = match k.as_str() {
                    "round" => LineCap::Round,
                    "square" => LineCap::Square,
                    _ => LineCap::Butt,
                };
                edit(scope, this, |s| {
                    s.st.line_cap = k;
                    s.ops.push(Canvas2DOp::SetLineCap { cap });
                });
            }
        }
        "lineJoin" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["round", "bevel", "miter"]) {
                let join = match k.as_str() {
                    "round" => LineJoin::Round,
                    "bevel" => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                };
                edit(scope, this, |s| {
                    s.st.line_join = k;
                    s.ops.push(Canvas2DOp::SetLineJoin { join });
                });
            }
        }
        "textAlign" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["start", "end", "left", "right", "center"]) {
                let align = match k.as_str() {
                    "end" => TextAlign::End,
                    "left" => TextAlign::Left,
                    "right" => TextAlign::Right,
                    "center" => TextAlign::Center,
                    _ => TextAlign::Start,
                };
                edit(scope, this, |s| {
                    s.st.text_align = k;
                    s.ops.push(Canvas2DOp::SetTextAlign { align });
                });
            }
        }
        "textBaseline" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["top", "hanging", "middle", "alphabetic", "ideographic", "bottom"]) {
                let baseline = match k.as_str() {
                    "top" => TextBaseline::Top,
                    "hanging" => TextBaseline::Hanging,
                    "middle" => TextBaseline::Middle,
                    "ideographic" => TextBaseline::Ideographic,
                    "bottom" => TextBaseline::Bottom,
                    _ => TextBaseline::Alphabetic,
                };
                edit(scope, this, |s| {
                    s.st.text_baseline = k;
                    s.ops.push(Canvas2DOp::SetTextBaseline { baseline });
                });
            }
        }
        "direction" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["ltr", "rtl", "inherit"]) {
                let dir = match k.as_str() {
                    "ltr" => TextDirection::Ltr,
                    "rtl" => TextDirection::Rtl,
                    _ => TextDirection::Inherit,
                };
                edit(scope, this, |s| {
                    s.st.direction = k;
                    s.ops.push(Canvas2DOp::SetTextDirection { dir });
                });
            }
        }
        "imageSmoothingQuality" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["low", "medium", "high"]) {
                let quality = match k.as_str() {
                    "medium" => SmoothingQuality::Medium,
                    "high" => SmoothingQuality::High,
                    _ => SmoothingQuality::Low,
                };
                edit(scope, this, |s| {
                    s.st.image_smoothing_quality = k;
                    s.ops.push(Canvas2DOp::SetImageSmoothingQuality { quality });
                });
            }
        }
        "letterSpacing" | "wordSpacing" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(px) = parse_px(&raw) {
                let letter = key == "letterSpacing";
                let norm = raw.trim().to_string();
                edit(scope, this, |s| {
                    if letter {
                        s.st.letter_spacing = norm;
                        s.ops.push(Canvas2DOp::SetLetterSpacing { px });
                    } else {
                        s.st.word_spacing = norm;
                        s.ops.push(Canvas2DOp::SetWordSpacing { px });
                    }
                });
            }
        }
        // The four typographic keywords + `lang`. These are validated, stored and reported, and
        // they do NOT record an op — the text path here draws runs through `SkFont` advances, which
        // has no shaper to hand a kerning or small-caps switch to. An op the backend could not
        // execute would be the worse answer: it would make the display list claim a typography the
        // pixels do not have. What is here is a real attribute with a real value and a named limit.
        "fontKerning" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["auto", "normal", "none"]) {
                edit(scope, this, |s| s.st.font_kerning = k);
            }
        }
        "fontStretch" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(
                &raw,
                &[
                    "ultra-condensed", "extra-condensed", "condensed", "semi-condensed", "normal",
                    "semi-expanded", "expanded", "extra-expanded", "ultra-expanded",
                ],
            ) {
                edit(scope, this, |s| s.st.font_stretch = k);
            }
        }
        "fontVariantCaps" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(
                &raw,
                &["normal", "small-caps", "all-small-caps", "petite-caps", "all-petite-caps", "unicase", "titling-caps"],
            ) {
                edit(scope, this, |s| s.st.font_variant_caps = k);
            }
        }
        "textRendering" => {
            let raw = val.to_rust_string_lossy(scope);
            if let Some(k) = keyword(&raw, &["auto", "optimizespeed", "optimizelegibility", "geometricprecision"]) {
                // The IDL spells these in camelCase; the comparison above is case-folded.
                let k = match k.as_str() {
                    "optimizespeed" => "optimizeSpeed",
                    "optimizelegibility" => "optimizeLegibility",
                    "geometricprecision" => "geometricPrecision",
                    _ => "auto",
                };
                edit(scope, this, |s| s.st.text_rendering = k.to_string());
            }
        }
        "lang" => {
            let raw = val.to_rust_string_lossy(scope);
            edit(scope, this, |s| s.st.lang = raw);
        }
        _ => {}
    }
}

/// `fillStyle` / `strokeStyle` assignment: a CSS colour string, or a gradient/pattern object.
fn style_set(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, val: v8::Local<v8::Value>, is_fill: bool) {
    // `ExtractV8CanvasStyle`: a string; a `CanvasGradient` or `CanvasPattern`; else any value converted to a string,
    // which may throw (stock Chrome 150: `{toString() { return 'red' }}` and `['blue']` are colours, `123` is not).
    let paint = v8::Local::<v8::Object>::try_from(val).ok().and_then(|o| read_priv_id(scope, o, PAINT_ID_PRIV));
    let Some(pid) = paint else {
        let Some(text) = val.to_string(scope) else { return };
        let raw = text.to_rust_string_lossy(scope);
        let Some(c) = canvas_color(scope, this, &raw) else { return };
        edit(scope, this, |s| {
            let op = if is_fill {
                s.st.fill = StyleRef::Css(c.text);
                Canvas2DOp::SetFillStyle { color: c.paint }
            } else {
                s.st.stroke = StyleRef::Css(c.text);
                Canvas2DOp::SetStrokeStyle { color: c.paint }
            };
            s.ops.push(op);
        });
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ctx_id = canvas2d_id(scope, this, &op_state);
    let mut st = op_state.borrow_mut();
    let store = store_mut(&mut st);
    let Some(paint) = store.paints.get(&pid) else { return };
    let (pattern, replay) = (paint.pattern, (paint.owner != ctx_id).then(|| paint.def.clone()));
    let tainted = paint.tainted;
    let state = store.ctx.entry(ctx_id).or_default();
    // A pattern of cross-origin pixels taints on ASSIGNMENT (measured).
    if tainted {
        state.tainted = true;
    }
    // Made by another context: that list has never seen the definition, so it gets one now.
    if let Some(def) = replay {
        state.ops.extend(def);
    }
    let op = match (is_fill, pattern) {
        (true, false) => Canvas2DOp::SetFillGradient { id: pid },
        (true, true) => Canvas2DOp::SetFillPattern { id: pid },
        (false, false) => Canvas2DOp::SetStrokeGradient { id: pid },
        (false, true) => Canvas2DOp::SetStrokePattern { id: pid },
    };
    state.ops.push(op);
    if is_fill {
        state.st.fill = StyleRef::Paint(pid);
    } else {
        state.st.stroke = StyleRef::Paint(pid);
    }
}

// ── the state stack + transforms ──────────────────────────────────────────────────────────────

pub(crate) fn cb_2d_save(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    edit(scope, args.this(), |s| {
        s.stack.push(s.st.clone());
        s.ops.push(Canvas2DOp::Save);
    });
}

pub(crate) fn cb_2d_restore(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    edit(scope, args.this(), |s| {
        // HTML: a `restore()` with an empty stack does nothing at all — including not recording,
        // because the backend's stack has to stay in step with this one.
        if let Some(prev) = s.stack.pop() {
            s.st = prev;
            s.ops.push(Canvas2DOp::Restore);
        }
    });
}

/// `reset()` — back to a fresh context: no ops (so a blank bitmap), default state, empty path.
pub(crate) fn cb_2d_reset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    edit(scope, args.this(), |s| {
        s.provider = has_paint_canvas(s);
        s.ops.clear();
        s.stack.clear();
        s.st = Canvas2DStyle::default();
    });
}

/// Multiply `t` into the current transform, in the recorder and in the list.
fn concat(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, t: Transform2D) {
    edit(scope, this, |s| {
        s.st.ctm = s.st.ctm.multiply(&t);
        s.ops.push(Canvas2DOp::Transform { t });
    });
}

pub(crate) fn cb_2d_translate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 2, "translate") {
        return;
    }
    let (x, y) = (num(scope, &args, 0), num(scope, &args, 1));
    if all_finite(&[x, y]) {
        concat(scope, args.this(), Transform2D::translate(x, y));
    }
}

pub(crate) fn cb_2d_scale(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 2, "scale") {
        return;
    }
    let (x, y) = (num(scope, &args, 0), num(scope, &args, 1));
    if all_finite(&[x, y]) {
        concat(scope, args.this(), Transform2D::scale(x, y));
    }
}

pub(crate) fn cb_2d_rotate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 1, "rotate") {
        return;
    }
    let a = num(scope, &args, 0);
    if a.is_finite() {
        concat(scope, args.this(), Transform2D::rotate(a));
    }
}

pub(crate) fn cb_2d_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 6, "transform") {
        return;
    }
    let v: Vec<f64> = (0..6).map(|i| num(scope, &args, i)).collect();
    if all_finite(&v) {
        concat(scope, args.this(), Transform2D { a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5] });
    }
}

/// A `DOMMatrix2DInit`, converted as WebIDL converts it: `a`..`f`, then `m11`, `m12`, `m21`,
/// `m22`, `m41`, `m42`, each read once as an `unrestricted double` -- then Blink's `ValidateAndFixup2D`: a member and
/// its alias that both are present must agree (NaN agreeing with NaN), else "Property mismatch on matrix
/// initialization."; each takes the other, else its identity value. The three readers this replaces read each pair
/// as `a` then `m11`, in their own order, and never refused a mismatch. `Err` after throwing.
pub(crate) fn matrix_2d_init(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, prefix: &str) -> Result<Transform2D, ()> {
    let d = crate::install::ctor_dicts::convert_dict(scope, v, "DOMMatrix2DInit", prefix)?;
    let mut n = |k: &str| get_prop(scope, d, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope));
    let pairs = [(n("a"), n("m11"), 1.0), (n("b"), n("m12"), 0.0), (n("c"), n("m21"), 0.0), (n("d"), n("m22"), 1.0), (n("e"), n("m41"), 0.0), (n("f"), n("m42"), 0.0)];
    let mut out = [0.0; 6];
    for (i, (short, long, dflt)) in pairs.into_iter().enumerate() {
        if let (Some(a), Some(b)) = (short, long) {
            if a != b && !(a.is_nan() && b.is_nan()) {
                throw_type_error(scope, &format!("{prefix}Property mismatch on matrix initialization."));
                return Err(());
            }
        }
        out[i] = long.or(short).unwrap_or(dflt);
    }
    Ok(Transform2D { a: out[0], b: out[1], c: out[2], d: out[3], e: out[4], f: out[5] })
}

/// `setTransform(a,b,c,d,e,f)` or `setTransform(matrix)`; no arguments resets to the identity, which
/// is what the IDL's all-optional `DOMMatrix2DInit` overload means.
pub(crate) fn cb_2d_set_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let t = if args.length() >= 6 {
        let v: Vec<f64> = (0..6).map(|i| num(scope, &args, i)).collect();
        if !all_finite(&v) {
            return;
        }
        Transform2D { a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5] }
    } else {
        // `setTransform(optional DOMMatrix2DInit transform = {})`
        let iface = iface_of(scope, args.this());
        let Ok(t) = matrix_2d_init(scope, args.get(0), &format!("Failed to execute 'setTransform' on '{iface}': ")) else { return };
        if !t.is_finite() {
            return;
        }
        t
    };
    edit(scope, args.this(), |s| {
        s.st.ctm = t;
        s.ops.push(Canvas2DOp::SetTransform { t });
    });
}

pub(crate) fn cb_2d_reset_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    edit(scope, args.this(), |s| {
        s.st.ctm = Transform2D::IDENTITY;
        s.ops.push(Canvas2DOp::SetTransform { t: Transform2D::IDENTITY });
    });
}

/// `getTransform()` — a `DOMMatrix` carrying the current matrix.
///
/// The engine's `DOMMatrix` is a shell whose members are stubs, so the values are written as OWN
/// data properties on an object with its prototype: that answers `m.a`, `m.m11` and
/// `Object.prototype.toString.call(m)` correctly, which is what a page reads. A `DOMMatrix` whose
/// accessors are backed by real state belongs with the geometry interfaces, not here.
pub(crate) fn cb_2d_get_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = read(scope, args.this(), |s| Some(s.st.ctm)).unwrap_or(Transform2D::IDENTITY);
    let o = crate::install::live::mint_table(scope, "DOMMatrix");
    for (k, v) in [
        ("a", t.a), ("b", t.b), ("c", t.c), ("d", t.d), ("e", t.e), ("f", t.f),
        ("m11", t.a), ("m12", t.b), ("m13", 0.0), ("m14", 0.0),
        ("m21", t.c), ("m22", t.d), ("m23", 0.0), ("m24", 0.0),
        ("m31", 0.0), ("m32", 0.0), ("m33", 1.0), ("m34", 0.0),
        ("m41", t.e), ("m42", t.f), ("m43", 0.0), ("m44", 1.0),
    ] {
        obj_set_num(scope, o, k, v);
    }
    if let Some(k) = v8::String::new(scope, "is2D") {
        let b = v8::Boolean::new(scope, true);
        o.create_data_property(scope, k.into(), b.into());
    }
    if let Some(k) = v8::String::new(scope, "isIdentity") {
        let b = v8::Boolean::new(scope, t.is_identity());
        o.create_data_property(scope, k.into(), b.into());
    }
    rv.set(o.into());
}

// ── path building ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_2d_begin_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    canvas2d_push(scope, args.this(), Canvas2DOp::BeginPath);
}

pub(crate) fn cb_2d_close_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    canvas2d_push(scope, args.this(), Canvas2DOp::ClosePath);
}

pub(crate) fn cb_2d_move_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 2, "moveTo") {
        return;
    }
    let (x, y) = (num(scope, &args, 0), num(scope, &args, 1));
    if all_finite(&[x, y]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::MoveTo { x, y });
    }
}

pub(crate) fn cb_2d_line_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 2, "lineTo") {
        return;
    }
    let (x, y) = (num(scope, &args, 0), num(scope, &args, 1));
    if all_finite(&[x, y]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::LineTo { x, y });
    }
}

pub(crate) fn cb_2d_bezier_curve_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 6, "bezierCurveTo") {
        return;
    }
    let v: Vec<f64> = (0..6).map(|i| num(scope, &args, i)).collect();
    if all_finite(&v) {
        canvas2d_push(
            scope,
            args.this(),
            Canvas2DOp::BezierCurveTo { cp1x: v[0], cp1y: v[1], cp2x: v[2], cp2y: v[3], x: v[4], y: v[5] },
        );
    }
}

pub(crate) fn cb_2d_quadratic_curve_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "quadraticCurveTo") {
        return;
    }
    let v: Vec<f64> = (0..4).map(|i| num(scope, &args, i)).collect();
    if all_finite(&v) {
        canvas2d_push(scope, args.this(), Canvas2DOp::QuadraticCurveTo { cpx: v[0], cpy: v[1], x: v[2], y: v[3] });
    }
}

pub(crate) fn cb_2d_arc(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 5, "arc") {
        return;
    }
    let v: Vec<f64> = (0..5).map(|i| num(scope, &args, i)).collect();
    let ccw = args.get(5).boolean_value(scope);
    if !all_finite(&v) {
        return;
    }
    if v[2] < 0.0 {
        let r = js_num(scope, v[2]);
        throw_index_size(scope, args.this(), "arc", &format!("The radius provided ({r}) is negative."));
        return;
    }
    canvas2d_push(scope, args.this(), Canvas2DOp::Arc { x: v[0], y: v[1], radius: v[2], start: v[3], end: v[4], ccw });
}

pub(crate) fn cb_2d_arc_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 5, "arcTo") {
        return;
    }
    let v: Vec<f64> = (0..5).map(|i| num(scope, &args, i)).collect();
    if !all_finite(&v) {
        return;
    }
    if v[4] < 0.0 {
        let r = js_num(scope, v[4]);
        throw_index_size(scope, args.this(), "arcTo", &format!("The radius provided ({r}) is negative."));
        return;
    }
    canvas2d_push(scope, args.this(), Canvas2DOp::ArcTo { x1: v[0], y1: v[1], x2: v[2], y2: v[3], radius: v[4] });
}

pub(crate) fn cb_2d_ellipse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 7, "ellipse") {
        return;
    }
    let v: Vec<f64> = (0..7).map(|i| num(scope, &args, i)).collect();
    let ccw = args.get(7).boolean_value(scope);
    if !all_finite(&v) {
        return;
    }
    if v[2] < 0.0 || v[3] < 0.0 {
        let why = ellipse_negative(scope, v[2], v[3]);
        throw_index_size(scope, args.this(), "ellipse", &why);
        return;
    }
    canvas2d_push(
        scope,
        args.this(),
        Canvas2DOp::Ellipse { x: v[0], y: v[1], rx: v[2], ry: v[3], rotation: v[4], start: v[5], end: v[6], ccw },
    );
}

pub(crate) fn cb_2d_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "rect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if all_finite(&[x, y, w, h]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::Rect { x, y, w, h });
    }
}

pub(crate) fn cb_2d_round_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "roundRect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if !all_finite(&[x, y, w, h]) {
        return;
    }
    let iface = iface_of(scope, args.this());
    let Some(radii) = round_rect_radii(scope, iface, args.get(4), w.abs(), h.abs()) else { return };
    canvas2d_push(scope, args.this(), Canvas2DOp::RoundRect { x, y, w, h, radii });
}

/// `roundRect`'s radii argument: one value, or a list of 1-4, each a number or a `{x, y}`. Returns
/// the four (rx, ry) corners already scaled down to fit the box, as HTML requires.
fn round_rect_radii(
    scope: &mut v8::PinScope,
    iface: &str,
    v: v8::Local<v8::Value>,
    w: f64,
    h: f64,
) -> Option<[(f64, f64); 4]> {
    // HTML: a negative radius is a RangeError; a non-finite one makes the call a silent no-op.
    let one = |scope: &mut v8::PinScope, v: v8::Local<v8::Value>| -> Option<(f64, f64)> {
        // Chrome 150.0.7871.46, measured: "Radius value -0.5 is negative." for a number, "X-radius value
        // -1 is negative." (or "Y-radius ...") for a `{x, y}`.
        let neg = |scope: &mut v8::PinScope, what: &str, x: f64| {
            let n = js_num(scope, x);
            crate::binding::protos::throw_range_error(scope, &format!("Failed to execute 'roundRect' on '{iface}': {what} value {n} is negative."));
        };
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            if !v.is_number() {
                let x = get_prop(scope, o, "x").and_then(|x| x.number_value(scope)).unwrap_or(0.0);
                let y = get_prop(scope, o, "y").and_then(|y| y.number_value(scope)).unwrap_or(0.0);
                if x < 0.0 {
                    neg(scope, "X-radius", x);
                    return None;
                }
                if y < 0.0 {
                    neg(scope, "Y-radius", y);
                    return None;
                }
                return (x.is_finite() && y.is_finite()).then_some((x, y));
            }
        }
        let n = v.number_value(scope).unwrap_or(f64::NAN);
        if n < 0.0 {
            neg(scope, "Radius", n);
            return None;
        }
        n.is_finite().then_some((n, n))
    };
    let list: Vec<(f64, f64)> = if v.is_undefined() {
        vec![(0.0, 0.0)]
    } else if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        let mut out = Vec::new();
        for i in 0..arr.length().min(4) {
            let e = arr.get_index(scope, i)?;
            out.push(one(scope, e)?);
        }
        if out.is_empty() {
            return None;
        }
        out
    } else {
        vec![one(scope, v)?]
    };
    let mut r = match list.len() {
        1 => [list[0], list[0], list[0], list[0]],
        2 => [list[0], list[1], list[0], list[1]],
        3 => [list[0], list[1], list[2], list[1]],
        _ => [list[0], list[1], list[2], list[3]],
    };
    // HTML: if the radii overflow the box, scale them all down by the worst ratio.
    let mut scale: f64 = 1.0;
    for (a, b, extent) in [
        (r[0].0, r[1].0, w),
        (r[2].0, r[3].0, w),
        (r[1].1, r[2].1, h),
        (r[0].1, r[3].1, h),
    ] {
        if a + b > extent && a + b > 0.0 {
            scale = scale.min(extent / (a + b));
        }
    }
    if scale < 1.0 {
        for c in r.iter_mut() {
            c.0 *= scale;
            c.1 *= scale;
        }
    }
    Some(r)
}

fn throw_index_size(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, member: &str, why: &str) {
    let iface = iface_of(scope, this);
    let msg = format!("Failed to execute '{member}' on '{iface}': {why}");
    crate::binding::protos::throw_dom(scope, &msg, "IndexSizeError");
}

// ── painting ──────────────────────────────────────────────────────────────────────────────────

/// `fill` / `stroke` / `clip` share one shape: an optional `Path2D` first, then an optional rule.
/// `None` means the rule was not one of the two the IDL enum allows and a `TypeError` is pending.
fn path_and_rule(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    member: &str,
) -> Option<(Option<Vec<PathOp>>, bool)> {
    let mut rule_idx = 0;
    let mut path = None;
    if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(pid) = read_priv_id(scope, o, PATH2D_ID_PRIV) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let segs = op_state.borrow().borrow::<Canvas2DStore>().paths.get(&pid).cloned();
            path = segs;
            rule_idx = 1;
        }
    }
    Some((path, fill_rule(scope, args, rule_idx, member)?))
}

/// A `CanvasFillRule` argument. WebIDL enums are not "anything else means the default": a value
/// outside the enum is a `TypeError`, which is what `ctx.fill("bogus")` does in a browser.
fn fill_rule(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, idx: i32, member: &str) -> Option<bool> {
    let v = args.get(idx);
    if v.is_undefined() {
        return Some(false);
    }
    match v.to_rust_string_lossy(scope).as_str() {
        "nonzero" => Some(false),
        "evenodd" => Some(true),
        other => {
            let iface = iface_of(scope, args.this());
            let msg = format!(
                "Failed to execute '{member}' on '{iface}': The provided value '{other}' \
                 is not a valid enum value of type CanvasFillRule."
            );
            throw_type_error(scope, &msg);
            None
        }
    }
}

pub(crate) fn cb_2d_fill(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((path, even_odd)) = path_and_rule(scope, &args, "fill") else { return };
    let op = match path {
        Some(p) => Canvas2DOp::FillPath { path: p, even_odd },
        None => Canvas2DOp::Fill { even_odd },
    };
    canvas2d_push(scope, args.this(), op);
}

pub(crate) fn cb_2d_stroke(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `stroke()` / `stroke(Path2D path)`: with an argument, the overload is the Path2D one, so anything
    // but a Path2D is a TypeError (Chrome 150: `stroke("evenodd")`, `stroke(null)`).
    let mut path = None;
    if args.length() > 0 {
        let pid = v8::Local::<v8::Object>::try_from(args.get(0)).ok().and_then(|o| read_priv_id(scope, o, PATH2D_ID_PRIV));
        let Some(pid) = pid else {
            let iface = iface_of(scope, args.this());
            throw_type_error(scope, &format!("Failed to execute 'stroke' on '{iface}': parameter 1 is not of type 'Path2D'."));
            return;
        };
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        path = op_state.borrow().borrow::<Canvas2DStore>().paths.get(&pid).cloned();
    }
    let op = match path {
        Some(p) => Canvas2DOp::StrokePath { path: p },
        None => Canvas2DOp::Stroke,
    };
    canvas2d_push(scope, args.this(), op);
}

pub(crate) fn cb_2d_clip(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((path, even_odd)) = path_and_rule(scope, &args, "clip") else { return };
    let op = match path {
        Some(p) => Canvas2DOp::ClipPath { path: p, even_odd },
        None => Canvas2DOp::Clip { even_odd },
    };
    canvas2d_push(scope, args.this(), op);
}

pub(crate) fn cb_2d_fill_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "fillRect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if all_finite(&[x, y, w, h]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::FillRect { x, y, w, h });
    }
}

pub(crate) fn cb_2d_stroke_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "strokeRect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if all_finite(&[x, y, w, h]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::StrokeRect { x, y, w, h });
    }
}

pub(crate) fn cb_2d_clear_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "clearRect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if all_finite(&[x, y, w, h]) {
        canvas2d_push(scope, args.this(), Canvas2DOp::ClearRect { x, y, w, h });
    }
}

fn text_call(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<(String, f64, f64, Option<f64>)> {
    if !need(scope, args, 3, member) {
        return None;
    }
    let text = args.get(0).to_rust_string_lossy(scope);
    let (x, y) = (num(scope, args, 1), num(scope, args, 2));
    if !all_finite(&[x, y]) {
        return None;
    }
    let max_width = if args.length() > 3 && !args.get(3).is_undefined() {
        let m = num(scope, args, 3);
        m.is_finite().then_some(m)
    } else {
        None
    };
    Some((text, x, y, max_width))
}

pub(crate) fn cb_2d_fill_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some((text, x, y, max_width)) = text_call(scope, &args, "fillText") {
        push_text_font(scope, args.this());
        canvas2d_push(scope, args.this(), Canvas2DOp::FillText { text, x, y, max_width });
    }
}

pub(crate) fn cb_2d_stroke_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some((text, x, y, max_width)) = text_call(scope, &args, "strokeText") {
        push_text_font(scope, args.this());
        canvas2d_push(scope, args.this(), Canvas2DOp::StrokeText { text, x, y, max_width });
    }
}

/// The current font, with the family list and the shaping language, as the op before a text draw --
/// a context that never set `font` draws in `10px sans-serif` shaped in the UI language too, and a
/// `save`/`restore` or a reset between the setter and the draw cannot leave the backend without it.
fn push_text_font(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) {
    let font = read(scope, this, |s| Some(s.st.font.clone())).unwrap_or_else(|| "10px sans-serif".into());
    let Some((family, size_px, bold, italic)) = parse_font(&font) else { return };
    let families = font_family_list(&font).unwrap_or_else(|| family.clone());
    let (lang, default_family) = text_locale(scope);
    canvas2d_push(scope, this, Canvas2DOp::SetFont { family, size_px, bold, italic, weight: if bold { 700 } else { 400 }, families, lang, default_family });
}

// ── line dash ─────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_2d_set_line_dash(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 1, "setLineDash") {
        return;
    }
    let Ok(arr) = v8::Local::<v8::Array>::try_from(args.get(0)) else { return };
    let mut segs = Vec::new();
    for i in 0..arr.length() {
        let Some(e) = arr.get_index(scope, i) else { return };
        let n = e.number_value(scope).unwrap_or(f64::NAN);
        // HTML: a negative or non-finite entry makes the whole call a no-op.
        if !n.is_finite() || n < 0.0 {
            return;
        }
        segs.push(n);
    }
    edit(scope, args.this(), |s| {
        s.st.line_dash = segs.clone();
        s.ops.push(Canvas2DOp::SetLineDash { segments: segs });
    });
}

pub(crate) fn cb_2d_get_line_dash(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let dash: Vec<f64> = read(scope, args.this(), |s| Some(s.st.line_dash.clone())).unwrap_or_default();
    // HTML: an odd-length list reads back doubled.
    let dash = if dash.len() % 2 == 1 { [dash.clone(), dash].concat() } else { dash };
    let out = v8::Array::new(scope, dash.len() as i32);
    for (i, v) in dash.iter().enumerate() {
        let n = v8::Number::new(scope, *v);
        out.set_index(scope, i as u32, n.into());
    }
    rv.set(out.into());
}

// ── hit testing ───────────────────────────────────────────────────────────────────────────────

fn hit_test(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, stroke: bool) -> bool {
    let member = if stroke { "isPointInStroke" } else { "isPointInPath" };
    // `(path, x, y[, rule])` or `(x, y[, rule])`.
    let mut base = 0;
    let mut path: Option<Vec<PathOp>> = None;
    if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(pid) = read_priv_id(scope, o, PATH2D_ID_PRIV) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            path = op_state.borrow().borrow::<Canvas2DStore>().paths.get(&pid).cloned();
            base = 1;
        }
    }
    if !need(scope, args, base + 2, member) {
        return false;
    }
    let (x, y) = (num(scope, args, base), num(scope, args, base + 1));
    let Some(even_odd) = fill_rule(scope, args, base + 2, member) else { return false };
    // `IsPointInPathInternal` / `IsPointInStrokeInternal` begin with `GetOrCreatePaintCanvas()` (see
    // [`has_paint_canvas`]), before they look at the point.
    edit(scope, args.this(), |s| s.provider = true);
    if !all_finite(&[x, y]) {
        return false;
    }
    let what = match (path, stroke) {
        (Some(p), true) => HitTest::InExplicitStroke { path: p },
        (Some(p), false) => HitTest::InExplicitPath { path: p, even_odd },
        (None, true) => HitTest::InStroke,
        (None, false) => HitTest::InPath { even_odd },
    };
    let ops = read(scope, args.this(), |s| Some(s.ops.clone())).unwrap_or_default();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    match backend {
        Some(b) => b.hit_test_2d(&ops, x, y, &what, &RenderParams::default()),
        None => false,
    }
}

pub(crate) fn cb_2d_is_point_in_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let hit = hit_test(scope, &args, false);
    rv.set(v8::Boolean::new(scope, hit).into());
}

pub(crate) fn cb_2d_is_point_in_stroke(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let hit = hit_test(scope, &args, true);
    rv.set(v8::Boolean::new(scope, hit).into());
}

// ── gradients + patterns ──────────────────────────────────────────────────────────────────────

/// Make the JS object for a new gradient/pattern, record its definition in the creating context's
/// display list, and keep a replayable copy at the store level.
fn new_paint<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    proto_name: &str,
    pattern: bool,
    make: impl FnOnce(u32) -> Canvas2DOp,
) -> v8::Local<'s, v8::Object> {
    let obj = crate::install::live::mint_table(scope, proto_name);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ctx_id = canvas2d_id(scope, this, &op_state);
    let id = {
        let mut st = op_state.borrow_mut();
        let store = store_mut(&mut st);
        store.next_obj += 1;
        let id = store.next_obj;
        let def = make(id);
        store.paints.insert(id, PaintObj { owner: ctx_id, def: vec![def.clone()], pattern, tainted: false });
        store.ctx.entry(ctx_id).or_default().ops.push(def);
        id
    };
    set_priv_id(scope, obj, PAINT_ID_PRIV, id);
    let v: v8::Local<v8::Value> = obj.into();
    let g = v8::Global::new(scope, v);
    store_mut(&mut op_state.borrow_mut()).paint_objs.insert(id, g);
    obj
}

pub(crate) fn cb_2d_create_linear_gradient(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "createLinearGradient") {
        return;
    }
    let v: Vec<f64> = (0..4).map(|i| num(scope, &args, i)).collect();
    if !all_finite(&v) {
        let iface = iface_of(scope, args.this());
        throw_type_error(
            scope,
            &format!("Failed to execute 'createLinearGradient' on '{iface}': The provided double value is non-finite."),
        );
        return;
    }
    let o = new_paint(scope, args.this(), "CanvasGradient", false, |id| Canvas2DOp::DefineLinearGradient {
        id,
        x0: v[0],
        y0: v[1],
        x1: v[2],
        y1: v[3],
    });
    rv.set(o.into());
}

pub(crate) fn cb_2d_create_radial_gradient(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 6, "createRadialGradient") {
        return;
    }
    let v: Vec<f64> = (0..6).map(|i| num(scope, &args, i)).collect();
    if !all_finite(&v) {
        let iface = iface_of(scope, args.this());
        throw_type_error(
            scope,
            &format!("Failed to execute 'createRadialGradient' on '{iface}': The provided double value is non-finite."),
        );
        return;
    }
    if v[2] < 0.0 {
        throw_index_size(scope, args.this(), "createRadialGradient", "The r0 provided is less than 0.");
        return;
    }
    if v[5] < 0.0 {
        throw_index_size(scope, args.this(), "createRadialGradient", "The r1 provided is less than 0.");
        return;
    }
    let o = new_paint(scope, args.this(), "CanvasGradient", false, |id| Canvas2DOp::DefineRadialGradient {
        id,
        x0: v[0],
        y0: v[1],
        r0: v[2],
        x1: v[3],
        y1: v[4],
        r1: v[5],
    });
    rv.set(o.into());
}

pub(crate) fn cb_2d_create_conic_gradient(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 3, "createConicGradient") {
        return;
    }
    let v: Vec<f64> = (0..3).map(|i| num(scope, &args, i)).collect();
    if !all_finite(&v) {
        let iface = iface_of(scope, args.this());
        throw_type_error(
            scope,
            &format!("Failed to execute 'createConicGradient' on '{iface}': The provided double value is non-finite."),
        );
        return;
    }
    let o = new_paint(scope, args.this(), "CanvasGradient", false, |id| Canvas2DOp::DefineConicGradient {
        id,
        start_angle: v[0],
        x: v[1],
        y: v[2],
    });
    rv.set(o.into());
}

/// `gradient.addColorStop(offset, color)` — appended to the creating context's list, so a stop added
/// after a draw affects only the draws that follow it.
pub(crate) fn cb_gradient_add_color_stop(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "CanvasGradient", "addColorStop") {
        return;
    }
    let Some(pid) = read_priv_id(scope, args.this(), PAINT_ID_PRIV) else { return };
    // The binding converts `double offset` (a non-finite one is its TypeError), then `DOMString color`.
    let Some(offset) = args.get(0).number_value(scope) else { return };
    if !offset.is_finite() {
        throw_type_error(scope, "Failed to execute 'addColorStop' on 'CanvasGradient': The provided double value is non-finite.");
        return;
    }
    let Some(text) = args.get(1).to_string(scope) else { return };
    let raw = text.to_rust_string_lossy(scope);
    if !(0.0..=1.0).contains(&offset) {
        let why = format!("The provided value ({}) is outside the range (0.0, 1.0).", page::css_color::to_precision(offset, 6));
        throw_index_size_named(scope, "addColorStop", "CanvasGradient", &why);
        return;
    }
    // `ParseCanvasColorString(color_string, color)`: no colour provider (an `OffscreenCanvas`'s system colours, for
    // either kind of canvas) and `currentcolor` black.
    let parsed = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::CssHandle>()).and_then(|h| h.0.parse_canvas_color(&raw, false, None))
    };
    let c = match parsed {
        Some(c) => c,
        None => {
            let msg = format!(
                "Failed to execute 'addColorStop' on 'CanvasGradient': The value provided ('{raw}') could not be parsed as a color."
            );
            crate::binding::protos::throw_dom(scope, &msg, "SyntaxError");
            return;
        }
    };
    let op = Canvas2DOp::AddColorStop { id: pid, offset, color: c.paint, oklab: c.oklab_stop, legacy: c.legacy };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let store = store_mut(&mut st);
    let Some(paint) = store.paints.get_mut(&pid) else { return };
    paint.def.push(op.clone());
    let owner = paint.owner;
    store.ctx.entry(owner).or_default().ops.push(op);
}

fn throw_index_size_named(scope: &mut v8::PinScope, member: &str, iface: &str, why: &str) {
    let msg = format!("Failed to execute '{member}' on '{iface}': {why}");
    crate::binding::protos::throw_dom(scope, &msg, "IndexSizeError");
}

/// `createPattern(image, repetition)`.
///
/// The pixels are snapshotted here, which is what a browser does too: a `CanvasPattern` made from a
/// canvas does not track that canvas's later drawing. A source with nothing to draw yet (an `<img>`
/// not yet loaded, a `<video>` with no frame) is "not usable", and HTML says return null rather than
/// throw; a BROKEN one throws (measured, `canvas_image`). A pattern made from a source that is not
/// origin-clean remembers it, and taints whichever context it is assigned to.
pub(crate) fn cb_2d_create_pattern(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 2, "createPattern") {
        return;
    }
    let iface = iface_of(scope, args.this());
    let source = match super::canvas_image::draw_source(scope, args.get(0), iface, super::canvas_image::Caller::CreatePattern) {
        Some(s) => s,
        None => return,
    };
    let raw = args.get(1).to_rust_string_lossy(scope);
    let repeat = match raw.trim() {
        "" | "repeat" => PatternRepeat::Repeat,
        "repeat-x" => PatternRepeat::RepeatX,
        "repeat-y" => PatternRepeat::RepeatY,
        "no-repeat" => PatternRepeat::NoRepeat,
        _ => {
            let iface = iface_of(scope, args.this());
            crate::binding::protos::throw_dom(
                scope,
                &format!(
                    "Failed to execute 'createPattern' on '{iface}': The provided type ('{raw}') is not one of \
                     'repeat', 'no-repeat', 'repeat-x', or 'repeat-y'."
                ),
                "SyntaxError",
            );
            return;
        }
    };
    let (image, clean) = match source {
        super::canvas_image::Source::Pixels(img, clean) => ((*img).clone(), clean),
        super::canvas_image::Source::Nothing => {
            rv.set_null();
            return;
        }
    };
    let o = new_paint(scope, args.this(), "CanvasPattern", true, |id| Canvas2DOp::DefinePattern {
        id,
        image,
        repeat,
    });
    if !clean {
        if let Some(pid) = read_priv_id(scope, o, PAINT_ID_PRIV) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            if let Some(p) = store_mut(&mut st).paints.get_mut(&pid) {
                p.tainted = true;
            }
        }
    }
    rv.set(o.into());
}

/// The interface a 2D context's sentences name: an `OffscreenCanvasRenderingContext2D` borrows these
/// bodies (`borrow_offscreen_2d`) and must not be called a `CanvasRenderingContext2D` in its errors.
pub(crate) fn iface_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> &'static str {
    if priv_get(scope, this, OFFSCREEN_CTX_PRIV).is_some() {
        "OffscreenCanvasRenderingContext2D"
    } else {
        "CanvasRenderingContext2D"
    }
}

/// Marks a 2D context an `OffscreenCanvas` made (set by its `getContext`).
pub(crate) const OFFSCREEN_CTX_PRIV: &str = "korvus_offscreen_2d";

/// A context's display list and whether its bitmap is tainted -- what a source canvas contributes to
/// a draw.
pub(crate) fn ops_and_taint(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> (Vec<Canvas2DOp>, bool) {
    read(scope, ctx, |s| (s.ops.clone(), s.tainted))
}

/// Is this context's bitmap tainted?
pub(crate) fn is_tainted(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> bool {
    read(scope, ctx, |s| s.tainted)
}

/// Reset the bitmap AND the drawing state, and clear the taint -- what writing a canvas's `width` or
/// `height` does, even to the value it already has (measured: `c.width = c.width` makes a tainted
/// canvas readable again, and `fillStyle` reads `#000000` afterwards).
pub(crate) fn reset_bitmap(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) {
    edit(scope, ctx, |s| {
        s.provider = has_paint_canvas(s);
        s.ops.clear();
        s.stack.clear();
        s.st = Canvas2DStyle::default();
        s.tainted = false;
    });
}

fn taint(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) {
    edit(scope, ctx, |s| s.tainted = true);
}

pub(crate) fn cb_pattern_set_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(pid) = read_priv_id(scope, args.this(), PAINT_ID_PRIV) else { return };
    // `CanvasPattern.setTransform(optional DOMMatrix2DInit transform = {})`
    let Ok(t) = matrix_2d_init(scope, args.get(0), "Failed to execute 'setTransform' on 'CanvasPattern': ") else { return };
    if !t.is_finite() {
        return;
    }
    let op = Canvas2DOp::SetPatternTransform { id: pid, t };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let store = store_mut(&mut st);
    let Some(paint) = store.paints.get_mut(&pid) else { return };
    paint.def.push(op.clone());
    let owner = paint.owner;
    store.ctx.entry(owner).or_default().ops.push(op);
}

// ── image sources ─────────────────────────────────────────────────────────────────────────────


/// `drawImage(image, dx, dy)`, `(image, dx, dy, dw, dh)`, `(image, sx, sy, sw, sh, dx, dy, dw, dh)`.
///
/// Chrome's order, measured: the argument COUNT (3 required), then the overload (4, 6, 7 or 8
/// arguments resolve to none of the three forms: "Overload resolution failed."), then the image union
/// and the source's state (`canvas_image::draw_source`), then the numbers. A non-finite number draws
/// nothing; a zero source OR destination extent draws nothing and does not taint; everything else
/// taints a clean canvas when its source is not origin-clean -- even a draw that lands wholly outside
/// the bitmap. Negative extents are normalised and the source rectangle is clipped to the image with
/// the destination scaled to match (HTML's steps, and what Chrome's pixels show).
pub(crate) fn cb_2d_draw_image(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let iface = iface_of(scope, this);
    if !require_args(scope, &args, 3, iface, "drawImage") {
        return;
    }
    let n = args.length();
    if matches!(n, 4 | 6 | 7 | 8) {
        throw_type_error(scope, &format!("Failed to execute 'drawImage' on '{iface}': Overload resolution failed."));
        return;
    }
    let source = match super::canvas_image::draw_source(scope, args.get(0), iface, super::canvas_image::Caller::DrawImage) {
        Some(s) => s,
        None => return,
    };
    let (image, clean) = match source {
        super::canvas_image::Source::Pixels(img, clean) => (img, clean),
        super::canvas_image::Source::Nothing => return,
    };
    let (iw, ih) = (image.width as f64, image.height as f64);
    let (mut sx, mut sy, mut sw, mut sh, mut dx, mut dy, mut dw, mut dh) = if n >= 9 {
        let v: Vec<f64> = (1..9).map(|i| num(scope, &args, i)).collect();
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7])
    } else if n >= 5 {
        let v: Vec<f64> = (1..5).map(|i| num(scope, &args, i)).collect();
        (0.0, 0.0, iw, ih, v[0], v[1], v[2], v[3])
    } else {
        let v: Vec<f64> = (1..3).map(|i| num(scope, &args, i)).collect();
        (0.0, 0.0, iw, ih, v[0], v[1], iw, ih)
    };
    if !all_finite(&[sx, sy, sw, sh, dx, dy, dw, dh]) {
        return;
    }
    // HTML: an empty source or destination rectangle draws nothing -- and taints nothing.
    if sw == 0.0 || sh == 0.0 || dw == 0.0 || dh == 0.0 {
        return;
    }
    if sw < 0.0 {
        sx += sw;
        sw = -sw;
    }
    if sh < 0.0 {
        sy += sh;
        sh = -sh;
    }
    if dw < 0.0 {
        dx += dw;
        dw = -dw;
    }
    if dh < 0.0 {
        dy += dh;
        dh = -dh;
    }
    if !clean {
        taint(scope, this);
    }
    let Some((csx, csy, csw, csh, ox, oy)) = super::canvas_image::clip_to_source(sx, sy, sw, sh, iw, ih) else { return };
    let (kx, ky) = (dw / sw, dh / sh);
    canvas2d_push(
        scope,
        this,
        Canvas2DOp::DrawImage {
            image: (*image).clone(),
            sx: csx,
            sy: csy,
            sw: csw,
            sh: csh,
            dx: dx + ox * kx,
            dy: dy + oy * ky,
            dw: csw * kx,
            dh: csh * ky,
        },
    );
}

pub(crate) fn cb_2d_put_image_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need(scope, &args, 3, "putImageData") {
        return;
    }
    let Ok(obj) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        let iface = iface_of(scope, args.this());
        throw_type_error(
            scope,
            &format!("Failed to execute 'putImageData' on '{iface}': parameter 1 is not of type 'ImageData'."),
        );
        return;
    };
    // The ImageData's own `data` slot (any pixel format -- a float one has no own `data` property, and a page's
    // getter must not see the engine read it), not its page-visible member.
    let Some(d) = crate::core::events::event::evt_slot_read(scope, obj, "data").filter(|d| super::pixel_format::PixelFormat::of_array(*d).is_some()) else {
        let iface = iface_of(scope, args.this());
        throw_type_error(
            scope,
            &format!("Failed to execute 'putImageData' on '{iface}': parameter 1 is not of type 'ImageData'."),
        );
        return;
    };
    let w = super::image_data::image_data_extent(scope, obj, "width").unwrap_or(0.0) as u32;
    let h = super::image_data::image_data_extent(scope, obj, "height").unwrap_or(0.0) as u32;
    let need = (w as usize) * (h as usize) * 4;
    if w == 0 || h == 0 {
        return;
    }
    let Some(bytes) = super::pixel_format::data_array_rgba8(d, need) else { return };
    let image = Rgba8 { width: w, height: h, data: bytes };
    let (dx, dy) = (num(scope, &args, 1), num(scope, &args, 2));
    if !all_finite(&[dx, dy]) {
        return;
    }
    let (mut dirty_x, mut dirty_y, mut dirty_w, mut dirty_h) = (0.0, 0.0, w as f64, h as f64);
    if args.length() >= 7 {
        let v: Vec<f64> = (3..7).map(|i| num(scope, &args, i)).collect();
        if !all_finite(&v) {
            return;
        }
        // HTML normalises a negative extent into a positive one starting further back.
        let (mut x, mut y, mut dw, mut dh) = (v[0], v[1], v[2], v[3]);
        if dw < 0.0 {
            x += dw;
            dw = -dw;
        }
        if dh < 0.0 {
            y += dh;
            dh = -dh;
        }
        dirty_x = x;
        dirty_y = y;
        dirty_w = dw;
        dirty_h = dh;
    }
    canvas2d_push(
        scope,
        args.this(),
        Canvas2DOp::PutImageData { image, dx, dy, dirty_x, dirty_y, dirty_w, dirty_h },
    );
}

/// `createImageData(sw, sh)` / `createImageData(imagedata)` — a transparent-black buffer of that
/// size, as a real `ImageData` instance.
pub(crate) fn cb_2d_create_image_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 1, "createImageData") {
        return;
    }
    let (w, h) = if args.length() >= 2 {
        (num(scope, &args, 0).abs().floor(), num(scope, &args, 1).abs().floor())
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        (
            super::image_data::image_data_extent(scope, o, "width").unwrap_or(0.0),
            super::image_data::image_data_extent(scope, o, "height").unwrap_or(0.0),
        )
    } else {
        (0.0, 0.0)
    };
    if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
        let iface = iface_of(scope, args.this());
        crate::binding::protos::throw_dom(
            scope,
            &format!("Failed to execute 'createImageData' on '{iface}': The source width is zero or not a number."),
            "IndexSizeError",
        );
        return;
    }
    let (w, h) = (w as u32, h as u32);
    // `ImageDataSettings.colorSpace`, else the source ImageData's (the one-argument form), else the
    // context's own (Chrome 150: `createImageData(2, 2, {colorSpace: "display-p3"}).colorSpace`); the pixel
    // format likewise (`createImageData(source)` keeps a float16 source's).
    let (cs, fmt) = if args.length() >= 2 {
        let Some(s) = settings_color_space(scope, args.get(2), args.this(), "createImageData") else { return };
        s
    } else {
        // The source ImageData's own slots, not its page-visible getters.
        let src = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
        let slot = |scope: &mut v8::PinScope, k: &str| {
            src.and_then(|o| crate::core::events::event::evt_slot_read(scope, o, k)).map(|v| v.to_rust_string_lossy(scope))
        };
        let cs = slot(scope, "colorSpace").unwrap_or_else(|| context_color_space(scope, args.this()));
        let fmt = slot(scope, "pixelFormat").and_then(|f| super::pixel_format::PixelFormat::parse(&f)).unwrap_or(super::pixel_format::PixelFormat::Unorm8);
        (cs, fmt)
    };
    let prefix = format!("Failed to execute 'createImageData' on '{}': ", iface_of(scope, args.this()));
    if !super::pixel_format::check_color_space_enabled(scope, &cs, &prefix) {
        return;
    }
    match new_image_data(scope, &Rgba8::transparent(w, h), w, h, &cs, fmt) {
        Some(o) => rv.set(o),
        None => rv.set_undefined(),
    }
}

/// Build a real `ImageData` — right prototype, right slots — through the same constructor a page's
/// own `new ImageData(...)` runs, in pixel format `fmt` (a float one read back from the premultiplied backing as
/// Chrome reads it: `pixel_format::rgba8_to_float`).
fn new_image_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    buf: &Rgba8,
    w: u32,
    h: u32,
    color_space: &str,
    fmt: super::pixel_format::PixelFormat,
) -> Option<v8::Local<'s, v8::Value>> {
    let len = (w as usize) * (h as usize) * 4;
    // The pixels become the buffer's backing store in one copy. They were written one V8
    // `Set` per byte: 360,000 of them for a 300x300 read, 11 ms against Chrome's under 1.
    let n = buf.data.len().min(len);
    let data = super::pixel_format::new_data_array(scope, fmt, Some(&buf.data[..n]), len)?;
    let ctor = crate::binding::protos::iface_object(scope, "ImageData").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let wv = int_val(scope, w as i64);
    let hv = int_val(scope, h as i64);
    let settings = crate::binding::protos::init_dict(scope);
    obj_set_str(scope, settings, "colorSpace", color_space);
    obj_set_str(scope, settings, "pixelFormat", fmt.as_str());
    ctor.new_instance(scope, &[data, wv, hv, settings.into()]).map(|o| o.into())
}

// ── readback + measurement ────────────────────────────────────────────────────────────────────

pub(crate) fn cb_2d_get_image_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !need(scope, &args, 4, "getImageData") {
        return;
    }
    let this = args.this();
    let iface = iface_of(scope, this);
    // `BaseRenderingContext2D::getImageDataInternal`. The binding converts four
    // `[EnforceRange] long`s, then the `ImageDataSettings`.
    let mut v = [0i64; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        let Some(n) = args.get(i as i32).number_value(scope) else { return };
        let t = n.trunc();
        if !t.is_finite() || !(-2147483648.0..=2147483647.0).contains(&t) {
            throw_type_error(scope, &format!("Failed to execute 'getImageData' on '{iface}': Value is outside the 'long' value range."));
            return;
        }
        *slot = t as i64;
    }
    let [mut sx, mut sy, mut sw, mut sh] = v;
    let Some((cs, fmt)) = settings_color_space(scope, args.get(4), this, "getImageData") else { return };
    let out_of_memory = |scope: &mut v8::PinScope| {
        crate::binding::protos::throw_range_error(scope, &format!("Failed to execute 'getImageData' on '{iface}': Out of memory at ImageData creation"));
    };
    let fits = |n: i64| (i32::MIN as i64..=i32::MAX as i64).contains(&n);
    if !fits(sw * sh) {
        out_of_memory(scope);
        return;
    }
    // A TAINTED bitmap cannot be read back (measured sentence); else a zero-width or zero-height
    // region is an IndexSizeError, not an empty ImageData.
    if is_tainted(scope, this) {
        crate::binding::protos::throw_dom(
            scope,
            &format!("Failed to execute 'getImageData' on '{iface}': The canvas has been tainted by cross-origin data."),
            "SecurityError",
        );
        return;
    }
    if sw == 0 || sh == 0 {
        let which = if sw != 0 { "height" } else { "width" };
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute 'getImageData' on '{iface}': The source {which} is 0."), "IndexSizeError");
        return;
    }
    // A negative extent reads the rectangle the other side of the origin.
    if sw < 0 {
        if !fits(sx + sw) {
            out_of_memory(scope);
            return;
        }
        sx += sw;
        sw = -sw;
    }
    if sh < 0 {
        if !fits(sy + sh) {
            out_of_memory(scope);
            return;
        }
        sy += sh;
        sh = -sh;
    }
    if !fits(sx + sw) || !fits(sy + sh) {
        out_of_memory(scope);
        return;
    }
    let prefix = format!("Failed to execute 'getImageData' on '{iface}': ");
    if !super::pixel_format::check_color_space_enabled(scope, &cs, &prefix) {
        return;
    }
    // Canvas backing size from this.canvas.width/height (default 300x150, like HTML).
    let canvas = priv_get(scope, this, CANVAS_BACKREF_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let cw = canvas.and_then(|c| super::html_canvas_element::canvas_extent(scope, c, "width")).filter(|n| *n > 0.0).unwrap_or(300.0) as u32;
    let ch = canvas.and_then(|c| super::html_canvas_element::canvas_extent(scope, c, "height")).filter(|n| *n > 0.0).unwrap_or(150.0) as u32;
    let params = ctx_render_params(scope, this);
    let space = color_space_of(&cs);
    let read = match fmt {
        super::pixel_format::PixelFormat::Unorm8 => spec::ReadFormat::Unorm8,
        super::pixel_format::PixelFormat::Float16 => spec::ReadFormat::Float16,
        super::pixel_format::PixelFormat::Float32 => spec::ReadFormat::Float32,
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = canvas2d_id(scope, this, &op_state);
    // A3.2: read the render service off the kernel Context (a render plugin provided it), instead of a hardcoded
    // RenderState. Transparent if none (a worker isolate has no render Context).
    let (ops, backend) = {
        let st = op_state.borrow();
        let ops = st.borrow::<Canvas2DStore>().ctx.get(&id).map(|c| c.ops.clone()).unwrap_or_default();
        (ops, st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone()))
    };
    let (w, h) = (sw as u32, sh as u32);
    // Skia converts the backing into the ImageData's space and format, as Blink's `readPixels` into
    // `ImageData::GetSkPixmap()` does -- unless nothing is to convert, where the context's memoised raster is
    // those very bytes.
    let same = read == spec::ReadFormat::Unorm8 && space == params.color_space && !params.float16;
    let converted = match (&backend, same) {
        (Some(b), false) => b.read_2d(&ops, cw, ch, sx as i32, sy as i32, w, h, &params, space, read),
        _ => None,
    };
    let data = match converted {
        Some(bytes) => super::pixel_format::new_raw_array(scope, fmt, bytes, (w as usize) * (h as usize) * 4),
        None => {
            let img = match backend {
                Some(b) => {
                    let mut st = op_state.borrow_mut();
                    raster_memo(&mut st, id, &ops, cw, ch, || b.raster_2d(&ops, cw, ch, &params))
                }
                None => Rgba8::transparent(cw, ch),
            };
            // the whole canvas as a read-back sees it, then the region: the region `toDataURL` encodes, pixel for pixel
            let img = super::html_canvas_element::read_back(plugin_host::READBACK_CANVAS, img);
            // Copy the requested region out (row-major RGBA); what lies outside the canvas is transparent black.
            let mut region = vec![0u8; (w as usize) * (h as usize) * 4];
            let (iw, ih) = (img.width as i64, img.height as i64);
            for row in 0..sh {
                for col in 0..sw {
                    let (px, py) = (sx + col, sy + row);
                    if px < 0 || py < 0 || px >= iw || py >= ih {
                        continue;
                    }
                    let si = ((py * iw + px) * 4) as usize;
                    let di = ((row * sw + col) * 4) as usize;
                    region[di..di + 4].copy_from_slice(&img.data[si..si + 4]);
                }
            }
            super::pixel_format::new_data_array(scope, fmt, Some(&region), region.len())
        }
    };
    // A real ImageData instance (right prototype, right slots), not a plain object -- getImageData() must satisfy
    // `instanceof ImageData` and `Object.prototype.toString.call(...)` like every other real browser.
    let made = data.and_then(|data| {
        let ctor = crate::binding::protos::iface_object(scope, "ImageData").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
        let wv = int_val(scope, w as i64);
        let hv = int_val(scope, h as i64);
        let settings = crate::binding::protos::init_dict(scope);
        obj_set_str(scope, settings, "colorSpace", &cs);
        obj_set_str(scope, settings, "pixelFormat", fmt.as_str());
        ctor.new_instance(scope, &[data, wv, hv, settings.into()])
    });
    match made {
        Some(o) => rv.set(o.into()),
        None => rv.set_undefined(),
    }
}

pub(crate) fn cb_2d_measure_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // Real advance widths, through the page pipeline's font tables (`spec::CssHandle`).
    //
    // This used to be `text.chars().count() * 7.0` -- the same number for "iiiii" and "WWWWW".
    // `measureText(s).width` is a float a page reads to four decimal places, and an engine that
    // answers with a character count disagrees with every real font, which is a fingerprint rather
    // than an approximation. It is also half of a *cross-check*: the same text in a `<span>` has an
    // `offsetWidth`, and the two must agree, which they do because both go through this service.
    if !need(scope, &args, 1, "measureText") {
        return;
    }
    let text = args.get(0).to_rust_string_lossy(scope);
    let this = args.this();
    let font = read(scope, this, |s| Some(s.st.font.clone())).unwrap_or_else(|| "10px sans-serif".into());
    let (family, size_px, bold, italic) = parse_font(&font).unwrap_or_else(|| ("sans-serif".into(), 10.0, false, false));
    let families = font_family_list(&font).unwrap_or(family);

    let lang = crate::core::navigator::navigator::persona(scope)
        .and_then(|p| p.navigator().languages.first().cloned())
        .unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (measured, ink) = {
        let st = op_state.borrow();
        // In the worn persona's fonts: its theme names the bundle.
        let font_map = crate::core::dom::geometry_queries::media_context(&st).theme.font_map;
        let ctx = st.try_borrow::<spec::kernel::Context>();
        let css = ctx.and_then(|c| c.get::<spec::CssHandle>());
        let measured = css.as_ref().map(|h| h.0.measure_text_in(font_map, &families, size_px as f32, bold, italic, &text));
        let default_family = css.as_ref().map(|h| h.0.default_family(font_map)).unwrap_or_else(|| "sans-serif".into());
        let request = spec::TextFontRequest { families: families.clone(), default_family, size: size_px as f32, bold, italic, lang, words: true };
        let ink = ctx.and_then(|c| c.get::<spec::RenderHandle>()).and_then(|h| h.0.text_ink(&request, &text));
        (measured, ink)
    };
    let (width, ascent, descent) = match measured {
        Some([w, a, d]) => (w, a, d),
        None => (text.chars().count() as f32 * 7.0, 0.0, 0.0),
    };
    // The shaped run's own advance when the fonts are here: it is in the fonts each glyph is actually
    // in (a CJK or emoji fallback rounds in ITS units per em), which the page's tables cannot know.
    let width = ink.as_ref().filter(|i| i.bounds.is_some() || i.width > 0.0).map(|i| i.width).unwrap_or(width);
    let align = read(scope, this, |s| Some(s.st.text_align.clone())).unwrap_or_else(|| "start".into());
    let baseline = read(scope, this, |s| Some(s.st.text_baseline.clone())).unwrap_or_else(|| "alphabetic".into());
    let rtl = align != "left" && align != "right" && align != "center" && inherited_direction(scope, this) == "rtl";
    // The em box is the PLATFORM font's size: Blink's font cache keeps sizes in hundredths of a px.
    let platform_size = (size_px as f32 * 100.0) as u32 as f32 / 100.0;
    // Fingerprint tap (KORVUS_READBACK_TEXT, ABI 3): a plugin may perturb the measured text metrics PER CALL,
    // keyed by host->seed(domain), beyond the static persona. It is handed [width, ascent, descent] BEFORE
    // text_metrics derives the ten TextMetrics rows, so a shift propagates coherently (into width, the
    // align-dependent actualBoundingBox left/right, and the font-box ascent/descent). It cannot reach the sibling
    // <span> offsetWidth, which comes from the layout path, not here -- that desync is the giant's named hard
    // problem, not this slice. With no plugin tapping TEXT, has_readback is false and the three values are
    // byte-for-byte what the engine measured (I9/J2); what is shifted, and by how much, is the plugin's (J6).
    let (width, ascent, descent) = if plugin_host::has_readback(plugin_host::READBACK_TEXT) {
        let mut metrics = [width as f64, ascent as f64, descent as f64];
        plugin_host::readback_text(&text, &mut metrics);
        (metrics[0] as f32, metrics[1] as f32, metrics[2] as f32)
    } else {
        (width, ascent, descent)
    };
    let m = text_metrics(width, ascent, descent, platform_size, ink.as_ref(), &align, rtl, &baseline);
    mint_text_metrics(scope, &m, &mut rv);
}

/// Blink's `TextMetrics::Update` (core/html/canvas/text_metrics.cc, Chromium 150), in its float
/// arithmetic: `width` and the font's (rounded) ascent and descent from the page's font tables; the
/// ink box and the `OS/2`/`BASE` values from the render backend ([`spec::TextInk`]).
///
/// * `textAlign` shifts the ink box by the run's width (`text_align_dx_`).
/// * `baseline_y` is where `textBaseline` puts the origin (`GetFontBaseline`): `top` / `bottom` /
///   `middle` from the em box -- `OS/2` sTypoAscender:sTypoDescender normalized to the font size in
///   1/64 px (`NormalizedTypoAscentAndDescent`) -- `hanging` / `ideographic` / `alphabetic` from the
///   `BASE` table, else 80% of the ascent / the descent / 0.
/// * every vertical member is measured from that origin.
#[allow(clippy::too_many_arguments)]
fn text_metrics(
    width: f32,
    ascent: f32,
    descent: f32,
    size: f32,
    ink: Option<&spec::TextInk>,
    align: &str,
    rtl: bool,
    baseline: &str,
) -> Vec<(&'static str, f64)> {
    let dx: f32 = match align {
        "center" => width / 2.0,
        "right" => width,
        "start" if rtl => width,
        "end" if !rtl => width,
        _ => 0.0,
    };
    let [gx, gy, gr, gb] = ink.and_then(|i| i.bounds).unwrap_or([0.0; 4]);
    let base = ink.map(|i| i.base).unwrap_or([None; 3]);
    let layout_unit = |v: f32| (v * 64.0).round() / 64.0;
    let normalized = |a: f32, d: f32| -> Option<(f32, f32)> {
        let h = a + d;
        if h <= 0.0 || a < 0.0 || a > h {
            return None;
        }
        let nta = layout_unit(a * size / h);
        Some((nta, layout_unit(size) - nta))
    };
    let (nta, ntd) = ink
        .and_then(|i| i.typo)
        .filter(|(a, _)| *a > 0)
        .and_then(|(a, d)| normalized(a as f32, d as f32))
        .or_else(|| normalized(ascent, descent))
        .unwrap_or((0.0, 0.0));
    let hanging = base[1].unwrap_or(ascent * 80.0 / 100.0);
    let ideographic = base[2].unwrap_or(-descent);
    let alphabetic = base[0].unwrap_or(0.0);
    let baseline_y: f32 = match baseline {
        "top" => nta,
        "hanging" => hanging,
        "ideographic" => ideographic,
        "bottom" => -ntd,
        "middle" => (nta - ntd) / 2.0,
        _ => alphabetic,
    };
    vec![
        ("width", width as f64),
        ("actualBoundingBoxLeft", (-gx + dx) as f64),
        ("actualBoundingBoxRight", (gr - dx) as f64),
        ("fontBoundingBoxAscent", (ascent - baseline_y) as f64),
        ("fontBoundingBoxDescent", (descent + baseline_y) as f64),
        ("actualBoundingBoxAscent", (-gy - baseline_y) as f64),
        ("actualBoundingBoxDescent", (gb + baseline_y) as f64),
        ("alphabeticBaseline", (alphabetic - baseline_y) as f64),
        ("hangingBaseline", (hanging - baseline_y) as f64),
        ("ideographicBaseline", (ideographic - baseline_y) as f64),
    ]
}

// ── TextMetrics ───────────────────────────────────────────────────────────────────────────────
//
// WHAT WAS WRONG (measured 2026-09-18 against real Brave 151.1.93.138 over a loopback
// page). `measureText()` built a PLAIN OBJECT, put `TextMetrics.prototype` on it, and wrote the six
// numbers this engine can compute as OWN DATA PROPERTIES. Two things followed, and a fingerprinter
// sees both:
//
//   1. `m.actualBoundingBoxAscent` threw `TypeError: Illegal invocation` while `m.width` on the SAME
//      object answered. Nothing was wrong with that getter in particular: the four `actualBoundingBox*`
//      rows had no own property to shadow them, so the read fell through to the registry's D2 stub on
//      the prototype, whose receiver check demands the brand a shell constructor stamps -- and a plain
//      `v8::Object::new` carries none. The six that "worked" worked by never reaching the prototype.
//      a behavioural sensor read `actualBoundingBoxAscent` 24 times in ONE a production site page load, and the
//      probe it belongs to dies on the throw.
//
//   2. `Object.getOwnPropertyNames(ctx.measureText("mm"))` answered six names. Brave answers `[]` --
//      every member is a prototype accessor, as WebIDL says. One line, no timing, no sampling.
//
// WHAT IT IS NOW. `TextMetrics` is an `interface!` table (below) whose ten rows are prototype
// accessors; the instance carries its numbers in PRIVATE slots, which no JS reflection path can see,
// and each getter refuses a receiver that has none -- so `TextMetrics.prototype.width` and
// `Object.create(TextMetrics.prototype).width` both throw "Illegal invocation" exactly as Brave's do.
//
// THE FOUR INK ROWS STAY D2 STUBS, AND THAT IS A MEASUREMENT, NOT A SHRUG. `actualBoundingBox*` is
// the glyph INK box, and this engine has no glyph outlines and no rasterizer -- the text service
// measures advances. Measured in Brave at 8/10/16/20/32/64/100/400/1000 px, the family is linear in
// the em ONLY above ~100 px (Arial `H` ascent is 0.703125 em at 400 px and at 1000 px) and is the
// HINTED bitmap extent below it (`H` at 16 px is 12, not 11.25; at 100 px it is 69, not 70.3125) --
// i.e. the small-size values are the rasterizer's, not the font's, and they are not derivable from
// any table this engine could carry. A run composes from its characters by rules this lane did
// measure and recorded here for whoever captures the data:
//
//     ascent  = max over the run's glyphs;  descent = max over the run's glyphs
//     left    = the FIRST glyph's ink left
//     right   = width - (last glyph's advance) + (last glyph's ink right)      [exact, n=2]
//     both left/right shift with `textAlign`, the baselines with `textBaseline`
//     a run of blanks has an EMPTY ink box: ascent=descent=0, and left/right straddle the advance
//
// Until that capture exists the honest answer is the D2 one: 0, recorded in the coverage map as an
// unimplemented member, rather than a plausible number invented here. What this lane removes is the
// THROW, which killed the whole probe, and the own-property shape, which was wrong on its own.

/// The private slot one `TextMetrics` number lives in. Per member, so the ten rows share one body
/// and a receiver with no slots is not one of ours.
fn tm_slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Private>> {
    let n = v8::String::new(scope, &format!("korvus_text_metrics.{name}"))?;
    Some(v8::Private::for_api(scope, Some(n)))
}

/// Mint one `TextMetrics` on the table's prototype, carrying `values` in private slots. There is no
/// other way to get one, which is what makes the receiver check below total.
fn mint_text_metrics(scope: &mut v8::PinScope, values: &[(&str, f64)], rv: &mut v8::ReturnValue) {
    let o = crate::install::live::mint_table(scope, "TextMetrics");
    for (name, value) in values {
        let Some(key) = tm_slot(scope, name) else { continue };
        let n = v8::Number::new(scope, *value);
        o.set_private(scope, key, n.into());
    }
    rv.set(o.into());
}

/// One `TextMetrics` row: the number in this member's private slot, or "Illegal invocation" for a
/// receiver that has no such slot -- the prototype itself, `Object.create(TextMetrics.prototype)`,
/// a plain object, a primitive. `data` is the member name, so all six rows share this body.
fn cb_text_metrics(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    let Some(key) = tm_slot(scope, &name) else {
        illegal_invocation(scope);
        return;
    };
    match this.get_private(scope, key).filter(|v| v.is_number()) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// The ten `TextMetrics` rows (the four `actualBoundingBox*` ones were `stub`, answering 0, until the
/// render backend measured ink boxes).
pub fn install_text_metrics(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "width" | "fontBoundingBoxAscent" | "fontBoundingBoxDescent" | "alphabeticBaseline"
        | "hangingBaseline" | "ideographicBaseline" | "actualBoundingBoxLeft" | "actualBoundingBoxRight"
        | "actualBoundingBoxAscent" | "actualBoundingBoxDescent" => {
            acc1(scope, proto, member, cb_text_metrics, Some(member.name))
        }
        other => debug_assert!(false, "install_text_metrics: unexpected member {other}"),
    }
}

webapi::interface! {
    // Rows VERBATIM from the `install/generated_registry.rs` block they replace: names, order,
    // kinds, types and descriptor flags. `both` because an `OffscreenCanvas` 2D context in a worker
    // hands one back, which is what `worker.surface.json` records.
    "TextMetrics" [interface, both] {
    ctor "TextMetrics" len 0                                 = stub,
    attr "width" : Double readonly                           = install_text_metrics,
    attr "actualBoundingBoxLeft" : Double readonly           = install_text_metrics,
    attr "actualBoundingBoxRight" : Double readonly          = install_text_metrics,
    attr "fontBoundingBoxAscent" : Double readonly           = install_text_metrics,
    attr "fontBoundingBoxDescent" : Double readonly          = install_text_metrics,
    attr "actualBoundingBoxAscent" : Double readonly         = install_text_metrics,
    attr "actualBoundingBoxDescent" : Double readonly        = install_text_metrics,
    attr "hangingBaseline" : Double readonly                 = install_text_metrics,
    attr "alphabeticBaseline" : Double readonly              = install_text_metrics,
    attr "ideographicBaseline" : Double readonly             = install_text_metrics
    }
}

// ── the small remainder ───────────────────────────────────────────────────────────────────────

/// The `CanvasRenderingContext2DSettings` a 2D context was created with:
/// `getContext("2d", {alpha: false, willReadFrequently: true, colorSpace: "display-p3"})` reads them
/// back from `getContextAttributes()` in Chrome 150, and `colorSpace` is the default of the context's
/// `ImageData`. Kept on the context, as `(alpha, desynchronized, willReadFrequently, colorSpace, colorType)`.
pub(crate) const CTX2D_SETTINGS_PRIV: &str = "korvus_2d_settings";

/// Keep the settings on a new 2D context.
pub(crate) fn store_2d_settings(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, st: &(bool, bool, bool, String, String)) {
    let text = format!("{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}", st.0, st.1, st.2, st.3, st.4);
    let v = crate::binding::protos::str_val(scope, &text);
    super::html_canvas_element::priv_set(scope, ctx, CTX2D_SETTINGS_PRIV, v);
}
fn load_2d_settings(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> (bool, bool, bool, String, String) {
    let text = priv_get(scope, ctx, CTX2D_SETTINGS_PRIV).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let p: Vec<&str> = text.split('\u{0}').collect();
    if p.len() != 5 {
        return (true, false, false, "srgb".into(), "unorm8".into());
    }
    (p[0] == "true", p[1] == "true", p[2] == "true", p[3].to_string(), p[4].to_string())
}
fn context_color_space(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> String {
    load_2d_settings(scope, ctx).3
}

/// The backing a 2D context was created with -- `getContext('2d', {alpha, colorSpace, colorType})` -- which its raster
/// is tagged with and stored in (`alpha` 2026-10-10).
pub(crate) fn ctx_render_params(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> RenderParams {
    let (alpha, _, _, cs, ct) = load_2d_settings(scope, ctx);
    RenderParams { color_space: color_space_of(&cs), float16: ct == "float16", opaque: !alpha, ..RenderParams::default() }
}

fn color_space_of(cs: &str) -> spec::CanvasColorSpace {
    if cs == "display-p3" { spec::CanvasColorSpace::DisplayP3 } else { spec::CanvasColorSpace::Srgb }
}
/// An `ImageDataSettings` argument's `colorSpace`, else the context's; `None` = a TypeError is pending.
/// An `ImageDataSettings` argument of `getImageData` / `createImageData`: its colour
/// space -- given, else the context's own -- and its pixel format (`rgba-unorm8` unless given), read and
/// enum-checked as the binding reads them (`pixel_format::read_settings`); `None` once a TypeError is thrown.
fn settings_color_space(
    scope: &mut v8::PinScope,
    v: v8::Local<v8::Value>,
    ctx: v8::Local<v8::Object>,
    method: &str,
) -> Option<(String, super::pixel_format::PixelFormat)> {
    let iface = iface_of(scope, ctx);
    let prefix = format!("Failed to execute '{method}' on '{iface}': ");
    let settings = v8::Local::<v8::Object>::try_from(v).ok();
    let (cs, fmt) = super::pixel_format::read_settings(scope, settings, &prefix)?;
    let cs = cs.unwrap_or_else(|| context_color_space(scope, ctx));
    Some((cs, fmt.unwrap_or(super::pixel_format::PixelFormat::Unorm8)))
}

/// `getContextAttributes()` — the settings this context was created with, a `CanvasRenderingContext2DSettings`
/// dictionary converted to a fresh object: its members in WebIDL's (lexicographic) order -- `alpha, colorSpace,
/// colorType, desynchronized, toneMapping, willReadFrequently` (Chrome 150, measured: `JSON.stringify` of it is
/// the order a page sees; the booleans came first here).
pub(crate) fn cb_2d_get_context_attributes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (alpha, desync, wrf, cs, ct) = load_2d_settings(scope, args.this());
    let o = v8::Object::new(scope);
    let set_bool = |scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str, v: bool| {
        if let Some(key) = v8::String::new(scope, k) {
            let b = v8::Boolean::new(scope, v);
            o.create_data_property(scope, key.into(), b.into());
        }
    };
    set_bool(scope, o, "alpha", alpha);
    obj_set_str(scope, o, "colorSpace", &cs);
    obj_set_str(scope, o, "colorType", &ct);
    set_bool(scope, o, "desynchronized", desync);
    let tone = v8::Object::new(scope);
    obj_set_str(scope, tone, "mode", "standard");
    crate::binding::protos::obj_set_obj(scope, o, "toneMapping", tone);
    set_bool(scope, o, "willReadFrequently", wrf);
    rv.set(o.into());
}

pub(crate) fn cb_2d_is_context_lost(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A CPU raster surface has no device to lose.
    rv.set(v8::Boolean::new(scope, false).into());
}

/// `drawFocusIfNeeded(element)` — draws a focus ring only when the element is focused AND is a
/// descendant of the canvas acting as its fallback content. Nothing here is focused by a script that
/// just called this, so the ring is not drawn; the argument check is the observable part.
pub(crate) fn cb_2d_draw_focus_if_needed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _ = need(scope, &args, 1, "drawFocusIfNeeded");
}

// ── Path2D ────────────────────────────────────────────────────────────────────────────────────

/// The segment recorder behind `Path2D`. Its methods are the same vocabulary the context's own path
/// methods use (`spec::PathOp`), so a path built here and a path built on the context go through one
/// implementation in the backend.
fn path2d_id(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> u32 {
    if let Some(id) = read_priv_id(scope, obj, PATH2D_ID_PRIV) {
        return id;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        let store = store_mut(&mut st);
        store.next_obj += 1;
        let id = store.next_obj;
        store.paths.insert(id, Vec::new());
        id
    };
    set_priv_id(scope, obj, PATH2D_ID_PRIV, id);
    id
}

fn path2d_push(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, segs: Vec<PathOp>) {
    let id = path2d_id(scope, this);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    store_mut(&mut st).paths.entry(id).or_default().extend(segs);
}

fn need_p(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, n: i32, member: &str) -> bool {
    require_args(scope, args, n as usize, "Path2D", member)
}

macro_rules! path2d_method {
    ($name:ident, $member:literal, $argc:literal, $build:expr) => {
        pub(crate) fn $name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
            if !need_p(scope, &args, $argc, $member) {
                return;
            }
            let v: Vec<f64> = (0..$argc).map(|i| num(scope, &args, i)).collect();
            if !all_finite(&v) {
                return;
            }
            #[allow(clippy::redundant_closure_call)]
            if let Some(seg) = ($build)(&v, args.get($argc).boolean_value(scope)) {
                path2d_push(scope, args.this(), vec![seg]);
            }
        }
    };
}

path2d_method!(cb_p2d_move_to, "moveTo", 2, |v: &[f64], _| Some(PathOp::MoveTo { x: v[0], y: v[1] }));
path2d_method!(cb_p2d_line_to, "lineTo", 2, |v: &[f64], _| Some(PathOp::LineTo { x: v[0], y: v[1] }));
path2d_method!(cb_p2d_bezier, "bezierCurveTo", 6, |v: &[f64], _| Some(PathOp::BezierCurveTo {
    cp1x: v[0],
    cp1y: v[1],
    cp2x: v[2],
    cp2y: v[3],
    x: v[4],
    y: v[5]
}));
path2d_method!(cb_p2d_quad, "quadraticCurveTo", 4, |v: &[f64], _| Some(PathOp::QuadraticCurveTo {
    cpx: v[0],
    cpy: v[1],
    x: v[2],
    y: v[3]
}));
path2d_method!(cb_p2d_rect, "rect", 4, |v: &[f64], _| Some(PathOp::Rect { x: v[0], y: v[1], w: v[2], h: v[3] }));
/// A negative radius on a path method is an `IndexSizeError`, not a silent skip — same rule on
/// `Path2D` as on the context.
fn p2d_radius(scope: &mut v8::PinScope, vals: &[f64], idx: &[usize], member: &str) -> bool {
    if idx.iter().all(|i| vals[*i] >= 0.0) {
        return true;
    }
    let why = if let [a, b] = idx {
        ellipse_negative(scope, vals[*a], vals[*b])
    } else {
        let r = js_num(scope, vals[idx[0]]);
        format!("The radius provided ({r}) is negative.")
    };
    throw_index_size_named(scope, member, "Path2D", &why);
    false
}

/// `ellipse`'s sentence for a negative radius, the major axis first -- Chrome 150.0.7871.46, measured.
fn ellipse_negative(scope: &mut v8::PinScope, rx: f64, ry: f64) -> String {
    if rx < 0.0 {
        let r = js_num(scope, rx);
        format!("The major-axis radius provided ({r}) is negative.")
    } else {
        let r = js_num(scope, ry);
        format!("The minor-axis radius provided ({r}) is negative.")
    }
}

pub(crate) fn cb_p2d_arc(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need_p(scope, &args, 5, "arc") {
        return;
    }
    let v: Vec<f64> = (0..5).map(|i| num(scope, &args, i)).collect();
    let ccw = args.get(5).boolean_value(scope);
    // HTML: a non-finite argument returns before the radius is looked at (Chrome 150: `new Path2D().arc(NaN, 0,
    // -1, 0, 1)` does not throw).
    if !all_finite(&v) || !p2d_radius(scope, &v, &[2], "arc") {
        return;
    }
    path2d_push(scope, args.this(), vec![PathOp::Arc { x: v[0], y: v[1], radius: v[2], start: v[3], end: v[4], ccw }]);
}

pub(crate) fn cb_p2d_arc_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need_p(scope, &args, 5, "arcTo") {
        return;
    }
    let v: Vec<f64> = (0..5).map(|i| num(scope, &args, i)).collect();
    // HTML: a non-finite argument returns before the radius is looked at (Chrome 150: `new Path2D().arc(NaN, 0,
    // -1, 0, 1)` does not throw).
    if !all_finite(&v) || !p2d_radius(scope, &v, &[4], "arcTo") {
        return;
    }
    path2d_push(scope, args.this(), vec![PathOp::ArcTo { x1: v[0], y1: v[1], x2: v[2], y2: v[3], radius: v[4] }]);
}

pub(crate) fn cb_p2d_ellipse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need_p(scope, &args, 7, "ellipse") {
        return;
    }
    let v: Vec<f64> = (0..7).map(|i| num(scope, &args, i)).collect();
    let ccw = args.get(7).boolean_value(scope);
    // HTML: a non-finite argument returns before the radius is looked at (Chrome 150: `new Path2D().arc(NaN, 0,
    // -1, 0, 1)` does not throw).
    if !all_finite(&v) || !p2d_radius(scope, &v, &[2, 3], "ellipse") {
        return;
    }
    path2d_push(
        scope,
        args.this(),
        vec![PathOp::Ellipse { x: v[0], y: v[1], rx: v[2], ry: v[3], rotation: v[4], start: v[5], end: v[6], ccw }],
    );
}

pub(crate) fn cb_p2d_close_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    path2d_push(scope, args.this(), vec![PathOp::ClosePath]);
}

pub(crate) fn cb_p2d_round_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need_p(scope, &args, 4, "roundRect") {
        return;
    }
    let (x, y, w, h) = rect_args(scope, &args);
    if !all_finite(&[x, y, w, h]) {
        return;
    }
    let Some(radii) = round_rect_radii(scope, "Path2D", args.get(4), w.abs(), h.abs()) else { return };
    path2d_push(scope, args.this(), vec![PathOp::RoundRect { x, y, w, h, radii }]);
}

/// `addPath(path [, transform])` — the other path's segments, mapped through the transform.
pub(crate) fn cb_p2d_add_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !need_p(scope, &args, 1, "addPath") {
        return;
    }
    let Ok(other) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_type_error(scope, "Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.");
        return;
    };
    let Some(oid) = read_priv_id(scope, other, PATH2D_ID_PRIV) else {
        throw_type_error(scope, "Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.");
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let segs = op_state.borrow().borrow::<Canvas2DStore>().paths.get(&oid).cloned().unwrap_or_default();
    // `addPath(Path2D path, optional DOMMatrix2DInit transform = {})`
    let Ok(t) = matrix_2d_init(scope, args.get(1), "Failed to execute 'addPath' on 'Path2D': ") else { return };
    let segs = if t.is_identity() { segs } else { segs.iter().map(|s| map_seg(s, &t)).collect() };
    path2d_push(scope, args.this(), segs);
}

/// Map one path segment through a transform. Only the point-valued segments can be mapped exactly;
/// an arc under a skew is not an arc, so those keep their own geometry and take the transform on the
/// centre — the same approximation `addPath` with a rotation makes in practice, and it is exact for
/// the translate/scale transforms pages actually pass.
fn map_seg(s: &PathOp, t: &Transform2D) -> PathOp {
    let p = |x: f64, y: f64| t.apply(x, y);
    match *s {
        PathOp::MoveTo { x, y } => {
            let (x, y) = p(x, y);
            PathOp::MoveTo { x, y }
        }
        PathOp::LineTo { x, y } => {
            let (x, y) = p(x, y);
            PathOp::LineTo { x, y }
        }
        PathOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y } => {
            let (cp1x, cp1y) = p(cp1x, cp1y);
            let (cp2x, cp2y) = p(cp2x, cp2y);
            let (x, y) = p(x, y);
            PathOp::BezierCurveTo { cp1x, cp1y, cp2x, cp2y, x, y }
        }
        PathOp::QuadraticCurveTo { cpx, cpy, x, y } => {
            let (cpx, cpy) = p(cpx, cpy);
            let (x, y) = p(x, y);
            PathOp::QuadraticCurveTo { cpx, cpy, x, y }
        }
        PathOp::Rect { x, y, w, h } => {
            let (nx, ny) = p(x, y);
            PathOp::Rect { x: nx, y: ny, w: w * t.a, h: h * t.d }
        }
        PathOp::Arc { x, y, radius, start, end, ccw } => {
            let (x, y) = p(x, y);
            PathOp::Arc { x, y, radius: radius * t.a.abs(), start, end, ccw }
        }
        PathOp::Ellipse { x, y, rx, ry, rotation, start, end, ccw } => {
            let (x, y) = p(x, y);
            PathOp::Ellipse { x, y, rx: rx * t.a.abs(), ry: ry * t.d.abs(), rotation, start, end, ccw }
        }
        PathOp::ArcTo { x1, y1, x2, y2, radius } => {
            let (x1, y1) = p(x1, y1);
            let (x2, y2) = p(x2, y2);
            PathOp::ArcTo { x1, y1, x2, y2, radius: radius * t.a.abs() }
        }
        PathOp::RoundRect { x, y, w, h, radii } => {
            let (nx, ny) = p(x, y);
            PathOp::RoundRect { x: nx, y: ny, w: w * t.a, h: h * t.d, radii }
        }
        PathOp::ClosePath => PathOp::ClosePath,
    }
}

/// `new Path2D()` / `new Path2D(otherPath)`. The SVG-path-string overload is not here: it needs an
/// SVG path parser, and a wrong one would build a wrong path silently.
pub(crate) fn cb_path2d_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST. Without it this body ran on a BARE `Path2D()` too,
    // with `this` bound to the global -- so it minted a path id and stamped it as a private slot on
    // the Window, then returned `undefined` where Chrome throws.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "Path2D") {
        return;
    }
    let this = args.this();
    let id = path2d_id(scope, this);
    if let Ok(other) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(oid) = read_priv_id(scope, other, PATH2D_ID_PRIV) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            let store = store_mut(&mut st);
            let segs = store.paths.get(&oid).cloned().unwrap_or_default();
            store.paths.insert(id, segs);
        }
    }
}

// ── install: the four tables ──────────────────────────────────────────────────────────────────
//
// Until this lane the file reached into the generated shell registry
// (`shell_proto(scope, "CanvasRenderingContext2D")`) and defined 73 members onto a prototype
// another layer had built -- a SECOND install mechanism beside `install/installer.rs`, which is what
// invariant I2 forbids, and members the install ledger never saw. The four interfaces are tables
// now: the installer builds each prototype from its own rows, in the capture's order, and the rows
// below carry the same names, arities and descriptor flags the generated registry carried, because
// they were derived from it row for row.

/// One `CanvasRenderingContext2D` state accessor from its row: `get x` / `set x`, lengths 0 and 1,
/// over the one generic pair of callbacks keyed on the member NAME (its `data`).
fn attr_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_2d_attr_get, &format!("get {}", member.name), 0, data);
    let data = str_val(scope, member.name);
    let s = build_fn_data(scope, cb_2d_attr_set, &format!("set {}", member.name), 1, data);
    define_accessor(scope, proto, member, g, Some(s));
}

/// `ctx.canvas` -- the element (or `OffscreenCanvas`) this context was made from, off the private
/// slot `getContext` wrote. A receiver that carries no slot is not one of ours: `Illegal
/// invocation`, which is what the shell stub this row replaces answered and what Chrome answers.
fn cb_2d_canvas(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match priv_get(scope, args.this(), CANVAS_BACKREF_PRIV) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// Every `CanvasRenderingContext2D` row.
pub fn install_canvas_2d(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "canvas" => getter_row(scope, proto, member, cb_2d_canvas),
        "fillStyle" | "strokeStyle" | "globalAlpha" | "globalCompositeOperation" | "filter" |
        "lineWidth" | "lineCap" | "lineJoin" | "miterLimit" | "lineDashOffset" | "shadowColor" |
        "shadowBlur" | "shadowOffsetX" | "shadowOffsetY" | "font" | "textAlign" | "textBaseline" |
        "direction" | "letterSpacing" | "wordSpacing" | "fontKerning" | "fontStretch" |
        "fontVariantCaps" | "textRendering" | "lang" | "imageSmoothingEnabled" | "imageSmoothingQuality"
        => attr_row(scope, proto, member),
        "save" => method_row(scope, proto, member, cb_2d_save),
        "restore" => method_row(scope, proto, member, cb_2d_restore),
        "reset" => method_row(scope, proto, member, cb_2d_reset),
        "scale" => method_row(scope, proto, member, cb_2d_scale),
        "rotate" => method_row(scope, proto, member, cb_2d_rotate),
        "translate" => method_row(scope, proto, member, cb_2d_translate),
        "transform" => method_row(scope, proto, member, cb_2d_transform),
        "setTransform" => method_row(scope, proto, member, cb_2d_set_transform),
        "resetTransform" => method_row(scope, proto, member, cb_2d_reset_transform),
        "getTransform" => method_row(scope, proto, member, cb_2d_get_transform),
        "beginPath" => method_row(scope, proto, member, cb_2d_begin_path),
        "closePath" => method_row(scope, proto, member, cb_2d_close_path),
        "moveTo" => method_row(scope, proto, member, cb_2d_move_to),
        "lineTo" => method_row(scope, proto, member, cb_2d_line_to),
        "bezierCurveTo" => method_row(scope, proto, member, cb_2d_bezier_curve_to),
        "quadraticCurveTo" => method_row(scope, proto, member, cb_2d_quadratic_curve_to),
        "arc" => method_row(scope, proto, member, cb_2d_arc),
        "arcTo" => method_row(scope, proto, member, cb_2d_arc_to),
        "ellipse" => method_row(scope, proto, member, cb_2d_ellipse),
        "rect" => method_row(scope, proto, member, cb_2d_rect),
        "roundRect" => method_row(scope, proto, member, cb_2d_round_rect),
        "fill" => method_row(scope, proto, member, cb_2d_fill),
        "stroke" => method_row(scope, proto, member, cb_2d_stroke),
        "clip" => method_row(scope, proto, member, cb_2d_clip),
        "fillRect" => method_row(scope, proto, member, cb_2d_fill_rect),
        "strokeRect" => method_row(scope, proto, member, cb_2d_stroke_rect),
        "clearRect" => method_row(scope, proto, member, cb_2d_clear_rect),
        "fillText" => method_row(scope, proto, member, cb_2d_fill_text),
        "strokeText" => method_row(scope, proto, member, cb_2d_stroke_text),
        "measureText" => method_row(scope, proto, member, cb_2d_measure_text),
        "setLineDash" => method_row(scope, proto, member, cb_2d_set_line_dash),
        "getLineDash" => method_row(scope, proto, member, cb_2d_get_line_dash),
        "isPointInPath" => method_row(scope, proto, member, cb_2d_is_point_in_path),
        "isPointInStroke" => method_row(scope, proto, member, cb_2d_is_point_in_stroke),
        "createLinearGradient" => method_row(scope, proto, member, cb_2d_create_linear_gradient),
        "createRadialGradient" => method_row(scope, proto, member, cb_2d_create_radial_gradient),
        "createConicGradient" => method_row(scope, proto, member, cb_2d_create_conic_gradient),
        "createPattern" => method_row(scope, proto, member, cb_2d_create_pattern),
        "createImageData" => method_row(scope, proto, member, cb_2d_create_image_data),
        "getImageData" => method_row(scope, proto, member, cb_2d_get_image_data),
        "putImageData" => method_row(scope, proto, member, cb_2d_put_image_data),
        "drawImage" => method_row(scope, proto, member, cb_2d_draw_image),
        "drawFocusIfNeeded" => method_row(scope, proto, member, cb_2d_draw_focus_if_needed),
        "getContextAttributes" => method_row(scope, proto, member, cb_2d_get_context_attributes),
        "isContextLost" => method_row(scope, proto, member, cb_2d_is_context_lost),
        other => debug_assert!(false, "install_canvas_2d: unexpected member {other}"),
    }
}

/// `CanvasGradient.addColorStop`.
pub fn install_canvas_gradient(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "addColorStop" => method_row(scope, proto, member, cb_gradient_add_color_stop),
        other => debug_assert!(false, "install_canvas_gradient: unexpected member {other}"),
    }
}

/// `CanvasPattern.setTransform`.
pub fn install_canvas_pattern(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "setTransform" => method_row(scope, proto, member, cb_pattern_set_transform),
        other => debug_assert!(false, "install_canvas_pattern: unexpected member {other}"),
    }
}

/// Every `Path2D` row. `new Path2D()` really constructs -- `cb_path2d_ctor` allocates the
/// instance's segment list, and `new Path2D(other)` copies one -- so the table is built through
/// `build_newable` rather than the placeholder a `Model::Value` table gets.
pub fn install_path2d(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "addPath" => method_row(scope, proto, member, cb_p2d_add_path),
        "closePath" => method_row(scope, proto, member, cb_p2d_close_path),
        "moveTo" => method_row(scope, proto, member, cb_p2d_move_to),
        "lineTo" => method_row(scope, proto, member, cb_p2d_line_to),
        "bezierCurveTo" => method_row(scope, proto, member, cb_p2d_bezier),
        "quadraticCurveTo" => method_row(scope, proto, member, cb_p2d_quad),
        "arc" => method_row(scope, proto, member, cb_p2d_arc),
        "arcTo" => method_row(scope, proto, member, cb_p2d_arc_to),
        "ellipse" => method_row(scope, proto, member, cb_p2d_ellipse),
        "rect" => method_row(scope, proto, member, cb_p2d_rect),
        "roundRect" => method_row(scope, proto, member, cb_p2d_round_rect),
        other => debug_assert!(false, "install_path2d: unexpected member {other}"),
    }
}

webapi::interface! {
    "CanvasRenderingContext2D" [interface, window] {
        ctor "CanvasRenderingContext2D" len 0 = stub,
        attr "canvas" : Any readonly = install_canvas_2d,
        attr "lang" : Any = install_canvas_2d,
        attr "font" : Any = install_canvas_2d,
        attr "textAlign" : Any = install_canvas_2d,
        attr "textBaseline" : Any = install_canvas_2d,
        attr "direction" : Any = install_canvas_2d,
        attr "fontKerning" : Any = install_canvas_2d,
        attr "fontStretch" : Any = install_canvas_2d,
        attr "fontVariantCaps" : Any = install_canvas_2d,
        attr "letterSpacing" : Any = install_canvas_2d,
        attr "textRendering" : Any = install_canvas_2d,
        attr "wordSpacing" : Any = install_canvas_2d,
        attr "globalCompositeOperation" : Any = install_canvas_2d,
        attr "filter" : Any = install_canvas_2d,
        attr "imageSmoothingQuality" : Any = install_canvas_2d,
        attr "strokeStyle" : Any = install_canvas_2d,
        attr "fillStyle" : Any = install_canvas_2d,
        attr "shadowColor" : Any = install_canvas_2d,
        attr "lineCap" : Any = install_canvas_2d,
        attr "lineJoin" : Any = install_canvas_2d,
        attr "globalAlpha" : Any = install_canvas_2d,
        attr "imageSmoothingEnabled" : Any = install_canvas_2d,
        attr "shadowOffsetX" : Any = install_canvas_2d,
        attr "shadowOffsetY" : Any = install_canvas_2d,
        attr "shadowBlur" : Any = install_canvas_2d,
        attr "lineWidth" : Any = install_canvas_2d,
        attr "miterLimit" : Any = install_canvas_2d,
        attr "lineDashOffset" : Any = install_canvas_2d,
        method "clip" : Any = install_canvas_2d,
        method "createConicGradient" : Any len 3 required 3 = install_canvas_2d,
        method "createImageData" : Any len 1 required 1 = install_canvas_2d,
        method "createLinearGradient" : Any len 4 required 4 = install_canvas_2d,
        method "createPattern" : Any len 2 required 2 nullable = install_canvas_2d,
        method "createRadialGradient" : Any len 6 required 6 = install_canvas_2d,
        method "drawFocusIfNeeded" : Any len 1 required 1 = install_canvas_2d,
        method "drawImage" : Any len 3 required 3 = install_canvas_2d,
        method "fill" : Any = install_canvas_2d,
        method "fillText" : Any len 3 required 3 = install_canvas_2d,
        method "getContextAttributes" : Any = install_canvas_2d,
        method "getImageData" : Any len 4 required 4 = install_canvas_2d,
        method "getLineDash" : Any = install_canvas_2d,
        method "getTransform" : Any = install_canvas_2d,
        method "isContextLost" : Any = install_canvas_2d,
        method "isPointInPath" : Any len 2 required 2 = install_canvas_2d,
        method "isPointInStroke" : Any len 2 required 2 = install_canvas_2d,
        method "measureText" : Any len 1 required 1 = install_canvas_2d,
        method "reset" : Any = install_canvas_2d,
        method "roundRect" : Any len 4 required 4 = install_canvas_2d,
        method "setLineDash" : Any len 1 required 1 = install_canvas_2d,
        method "strokeText" : Any len 3 required 3 = install_canvas_2d,
        method "arc" : Any len 5 required 5 = install_canvas_2d,
        method "arcTo" : Any len 5 required 5 = install_canvas_2d,
        method "beginPath" : Any = install_canvas_2d,
        method "bezierCurveTo" : Any len 6 required 6 = install_canvas_2d,
        method "clearRect" : Any len 4 required 4 = install_canvas_2d,
        method "closePath" : Any = install_canvas_2d,
        method "ellipse" : Any len 7 required 7 = install_canvas_2d,
        method "fillRect" : Any len 4 required 4 = install_canvas_2d,
        method "lineTo" : Any len 2 required 2 = install_canvas_2d,
        method "moveTo" : Any len 2 required 2 = install_canvas_2d,
        method "putImageData" : Any len 3 required 3 = install_canvas_2d,
        method "quadraticCurveTo" : Any len 4 required 4 = install_canvas_2d,
        method "rect" : Any len 4 required 4 = install_canvas_2d,
        method "resetTransform" : Any = install_canvas_2d,
        method "restore" : Any = install_canvas_2d,
        method "rotate" : Any len 1 required 1 = install_canvas_2d,
        method "save" : Any = install_canvas_2d,
        method "scale" : Any len 2 required 2 = install_canvas_2d,
        method "setTransform" : Any = install_canvas_2d,
        method "stroke" : Any = install_canvas_2d,
        method "strokeRect" : Any len 4 required 4 = install_canvas_2d,
        method "transform" : Any len 6 required 6 = install_canvas_2d,
        method "translate" : Any len 2 required 2 = install_canvas_2d
    }
}

webapi::interface! {
    "CanvasGradient" [interface, both] {
        ctor "CanvasGradient" len 0 = stub,
        method "addColorStop" : Any len 2 required 2 = install_canvas_gradient
    }
}

webapi::interface! {
    "CanvasPattern" [interface, both] {
        ctor "CanvasPattern" len 0 = stub,
        method "setTransform" : Any = install_canvas_pattern
    }
}

webapi::interface! {
    "Path2D" [value, both] {
        ctor "Path2D" len 0 = stub,
        method "addPath" : Any len 1 required 1 = install_path2d,
        method "roundRect" : Any len 4 required 4 = install_path2d,
        method "arc" : Any len 5 required 5 = install_path2d,
        method "arcTo" : Any len 5 required 5 = install_path2d,
        method "bezierCurveTo" : Any len 6 required 6 = install_path2d,
        method "closePath" : Any = install_path2d,
        method "ellipse" : Any len 7 required 7 = install_path2d,
        method "lineTo" : Any len 2 required 2 = install_path2d,
        method "moveTo" : Any len 2 required 2 = install_path2d,
        method "quadraticCurveTo" : Any len 4 required 4 = install_path2d,
        method "rect" : Any len 4 required 4 = install_path2d
    }
}


//! SVG text queries -- `SVGTextContentElement`'s character metrics and a text's `getBBox()` -- as Blink's
//! `SvgTextQuery` answers them (core/layout/svg/svg_text_query.cc, Chromium 150).
//!
//! `getNumberOfChars`, `getComputedTextLength` and `getExtentOfChar` were stubs answering 0, 0 and nothing, and a
//! text's `getBBox()` was 0x0: a font probe that sets each candidate family on an SVG `<text>` and compares
//! `getComputedTextLength()` found no font at all -- a bot-challenge frame hashed an EMPTY font list where
//! stock Chrome 150 hashed `DejaVu Sans|Liberation Mono|Liberation Sans|Noto Color Emoji|...`.
//!
//! Blink lays a rendered `<text>` out in SCALED user space (`SvgTextLayoutAlgorithm`): each item (a text node's
//! addressable characters, white space collapsed) is shaped at the font size times the text's screen scaling
//! factor -- `sqrt((a²+b²+c²+d²)/2)` of its transform up to the SVG root (each element's transform, a viewport's
//! viewBox) and the CSS transforms of the layers above (`CalculateScreenFontSizeScalingFactor`; the challenge sets
//! `scale(1.000998)`) -- that size floored to hundredths of a px (`FontDescription::EffectiveFontSize`). An item's
//! inline size is its advance rounded UP to a LayoutUnit (`SnappedWidth`, 1/64 px); a range inside it is its caret
//! positions rounded out (`LineLeftAndRightForOffsets`), a whole item its size truncated back to a LayoutUnit; its
//! box is the font's whole-pixel ascent above its baseline and descent below; positions start at `x`/`y` (times the
//! factor), items follow one another, `dx`/`dy` shift them, and `text-anchor` moves each chunk by its advance
//! extent. Every query divides back by the factor in Blink's `float` arithmetic. A text's bounding box is the
//! union of each item's box and its glyphs' INK box (`FragmentItem::ObjectBoundingBox`: an italic run's last glyph
//! overhangs its advance).
//!
//! An element without a layout object -- detached, in a document no frame shows, under `display: none`, not in an
//! SVG `<text>` -- has no characters: counts and lengths are 0 and the per-character queries throw
//! `IndexSizeError` in Blink's words.
//!
//! Not modelled, each answering as the item model above does: per-character `x`/`y`/`dx`/`dy`/`rotate` lists past
//! their first value (Blink splits such text into one item per positioned character), `textLength` /
//! `lengthAdjust`, `<textPath>` geometry, `letter-spacing` / `word-spacing`, vertical writing modes and bidi.

use deno_core::v8;
use deno_core::OpState;

use crate::binding::protos::{throw_dom, throw_type_error, unwrap_id};
use crate::binding::HostDom;
use crate::core::css::cssom::{computed_text, css_computed_one};
use page::style::property::PropertyId as P;

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn local_tag(dom: &korvus_dom::Dom, id: u32) -> String {
    dom.tag_name(id).map(|t| t.rsplit(':').next().unwrap_or(t).to_string()).unwrap_or_default()
}

fn is_svg(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.node_type(id) == 1 && dom.namespace_uri(id) == Some(SVG_NS)
}

/// The elements a `<text>` lays out the content of (`LayoutSVGText::IsChildAllowed`, `LayoutSVGInline`).
fn is_inline_text_element(tag: &str) -> bool {
    matches!(tag, "tspan" | "textPath" | "a")
}

/// The SVG containers whose children have layout objects.
fn is_container(tag: &str) -> bool {
    matches!(tag, "svg" | "g" | "a" | "switch" | "defs" | "symbol" | "marker" | "mask" | "pattern" | "clipPath")
}

/// One item: a text node's addressable characters in one font.
struct Run {
    /// the text node
    node: u32,
    /// the element whose style the run is in (the text node's parent)
    owner: u32,
    /// the run's addressable characters, white space processed, in UTF-16 code units
    units: Vec<u16>,
    /// for each code unit, its offset in the text node's own data (for `selectSubString`)
    source: Vec<u32>,
    families: String,
    /// the shaped size: font size times the scaling factor, floored to hundredths of a px
    size: f32,
    bold: bool,
    italic: bool,
    /// scaled pen position of the run's start, and its baseline
    x: f32,
    y: f32,
    /// the run's advance as shaped, and as a LayoutUnit (`SnappedWidth`, rounded up)
    raw_width: f32,
    width: f32,
    /// Blink's `FixedAscent` / `FixedDescent` at the shaped size
    ascent: f32,
    descent: f32,
    /// the glyphs' ink box from the run's start on its baseline, y down: `[left, top, right, bottom]`
    ink: Option<[f32; 4]>,
    /// the caret before each code unit and after the last, in the whole run's shaping (empty: shape prefixes)
    carets: Vec<f32>,
    /// the run's first addressable index in its `<text>`
    first: usize,
}

/// A rendered `<text>`'s layout.
struct Layout {
    runs: Vec<Run>,
    scale: f32,
}

fn lu_ceil(v: f32) -> f32 {
    (v * 64.0).ceil() / 64.0
}

fn lu_floor(v: f32) -> f32 {
    (v * 64.0).floor() / 64.0
}

fn lu_round(v: f32) -> f32 {
    (v * 64.0).round() / 64.0
}

/// `LayoutUnit(float)`: truncated to a multiple of 1/64.
fn lu_trunc(v: f32) -> f32 {
    (v * 64.0).trunc() / 64.0
}

/// The `<text>` whose layout holds `el`'s characters, when `el` has a layout object: in a document a frame shows,
/// a text-content element in a `<text>` whose ancestors up to its outermost `<svg>` are containers, nothing above
/// it `display: none`.
fn text_root(st: &mut OpState, el: u32) -> Option<u32> {
    if !document_rendered(st, el, 0) {
        return None;
    }
    let chain: Vec<u32> = {
        let dom = &st.borrow::<HostDom>().0;
        let mut chain = Vec::new();
        let mut cur = Some(el);
        while let Some(n) = cur {
            if dom.node_type(n) == 1 {
                chain.push(n);
            }
            cur = dom.parent(n);
        }
        chain
    };
    let text = {
        let dom = &st.borrow::<HostDom>().0;
        let mut i = 0;
        let text = loop {
            let n = *chain.get(i)?;
            if !is_svg(dom, n) {
                return None;
            }
            let tag = local_tag(dom, n);
            if tag == "text" {
                break n;
            }
            if !is_inline_text_element(&tag) {
                return None;
            }
            i += 1;
        };
        // the containers up to the outermost <svg>, whose parent is not an SVG element
        let mut outermost = false;
        for &n in &chain[i + 1..] {
            if !is_svg(dom, n) {
                break;
            }
            if !is_container(&local_tag(dom, n)) {
                return None;
            }
            outermost = local_tag(dom, n) == "svg";
        }
        if !outermost {
            return None;
        }
        text
    };
    for &n in &chain {
        if css_computed_one(st, n, "display") == "none" {
            return None;
        }
    }
    Some(text)
}

/// Whether `node` is in a document something renders: the page's, or a frame's whose frame element is itself in one
/// (a frame removed from its document leaves its document unrendered -- Chrome 150 answers a text's length 0 once
/// the challenge removes its measuring frame).
pub(crate) fn document_rendered(st: &OpState, node: u32, depth: u32) -> bool {
    let dom = &st.borrow::<HostDom>().0;
    let root = dom.shadow_including_root(node);
    if root == dom.root() {
        return true;
    }
    if depth > 16 || dom.node_type(root) != 9 {
        return false;
    }
    match crate::core::html::browsing_context::frame_element_showing(st, root) {
        Some(frame) => document_rendered(st, frame, depth + 1),
        None => false,
    }
}

/// The first length of an SVG `x`/`y`/`dx`/`dy` list, in user units (a number, or `px`).
fn first_length(dom: &korvus_dom::Dom, id: u32, attr: &str) -> Option<f32> {
    let v = dom.get_attribute(id, attr)?;
    let first = v.split(|c: char| c.is_ascii_whitespace() || c == ',').find(|s| !s.is_empty())?;
    first.strip_suffix("px").unwrap_or(first).parse::<f32>().ok().filter(|n| n.is_finite())
}

/// The values of a `rotate` list.
fn rotate_list(dom: &korvus_dom::Dom, id: u32) -> Vec<f32> {
    dom.get_attribute(id, "rotate")
        .map(|v| v.split(|c: char| c.is_ascii_whitespace() || c == ',').filter(|s| !s.is_empty()).map_while(|s| s.parse::<f32>().ok()).collect())
        .unwrap_or_default()
}

/// `a * b` for the linear parts `[a, b, c, d]` of two transforms (`b` applied first).
fn mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0] * b[0] + a[2] * b[1], a[1] * b[0] + a[3] * b[1], a[0] * b[2] + a[2] * b[3], a[1] * b[2] + a[3] * b[3]]
}

/// Blink's `SVGLayoutSupport::CalculateScreenFontSizeScalingFactor`: the transform from the text up to its SVG
/// root -- each element's own transform, a viewport's viewBox -- then the CSS transforms of the boxes above
/// (`DeprecatedCalculateTransformToLayer`), reduced to `sqrt((a²+b²+c²+d²)/2)`; 1 under
/// `text-rendering: geometricPrecision` (`ComputeNewScaledFontForStyle`).
fn scaling_factor(st: &mut OpState, text: u32) -> f32 {
    if computed_text(st, text, P::TextRendering).is_some_and(|v| v.eq_ignore_ascii_case("geometricPrecision")) {
        return 1.0;
    }
    let mut m = [1.0f64, 0.0, 0.0, 1.0];
    let mut in_svg = true;
    let mut cur = Some(text);
    while let Some(n) = cur {
        let (parent, element, svg, tag) = {
            let dom = &st.borrow::<HostDom>().0;
            (dom.parent(n), dom.node_type(n) == 1, is_svg(dom, n), local_tag(dom, n))
        };
        if element {
            if in_svg && svg {
                if tag == "svg" {
                    m = mul(crate::core::dom::svg_geometry::viewbox_linear(st, n), m);
                    let outermost = parent.is_none_or(|p| !is_svg(&st.borrow::<HostDom>().0, p));
                    if outermost {
                        // the root's own CSS transform is its layer's, above its viewBox
                        in_svg = false;
                        if let Some(t) = crate::core::css::cssom::transform_linear(st, n) {
                            m = mul(t, m);
                        }
                    }
                } else if let Some(t) = svg_local_transform(st, n) {
                    m = mul(t, m);
                }
            } else if let Some(t) = crate::core::css::cssom::transform_linear(st, n) {
                in_svg = false;
                m = mul(t, m);
            }
        }
        cur = parent;
    }
    let s = ((m[0] * m[0] + m[1] * m[1] + m[2] * m[2] + m[3] * m[3]) / 2.0).sqrt() as f32;
    if s == 0.0 || !s.is_finite() {
        1.0
    } else {
        s
    }
}

/// An SVG element's own transform: its CSS `transform`, else its `transform` attribute.
fn svg_local_transform(st: &mut OpState, n: u32) -> Option<[f64; 4]> {
    if let Some(t) = crate::core::css::cssom::transform_linear(st, n) {
        return Some(t);
    }
    let dom = &st.borrow::<HostDom>().0;
    dom.get_attribute(n, "transform").map(crate::core::dom::svg_geometry::linear_part_of)
}

/// The shaper: a run's advance and ink at a size, and its font's ascent and descent -- the font tables and the
/// render backend canvas `measureText` reads (`core/html/canvas_2d.rs`).
struct Shaper {
    font_map: u64,
    css: Option<std::sync::Arc<dyn spec::CssValues>>,
    render: Option<std::sync::Arc<dyn spec::RenderBackend>>,
    default_family: String,
    lang: String,
}

impl Shaper {
    fn new(scope: &mut v8::PinScope) -> Shaper {
        let lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let font_map = crate::core::dom::geometry_queries::media_context(&st).theme.font_map;
        let ctx = st.try_borrow::<spec::kernel::Context>();
        let css = ctx.and_then(|c| c.get::<spec::CssHandle>()).map(|h| h.0.clone());
        let render = ctx.and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone());
        let default_family = css.as_ref().map(|c| c.default_family(font_map)).unwrap_or_else(|| "sans-serif".into());
        Shaper { font_map, css, render, default_family, lang }
    }

    /// `text` shaped as one item: its advance, ink box, carets, and its primary font's ascent and descent.
    fn shape_run(&self, families: &str, size: f32, bold: bool, italic: bool, text: &str) -> Option<spec::TextInk> {
        let request = spec::TextFontRequest { families: families.to_string(), default_family: self.default_family.clone(), size, bold, italic, lang: self.lang.clone(), words: false };
        self.render.as_ref().and_then(|r| r.text_ink(&request, text)).filter(|i| i.bounds.is_some() || i.width > 0.0)
    }

    /// The advance of `text`, from the backend's shaping, else the page's font tables.
    fn shape(&self, families: &str, size: f32, bold: bool, italic: bool, text: &str) -> (f32, Option<[f32; 4]>, Vec<f32>) {
        if let Some(ink) = self.shape_run(families, size, bold, italic, text) {
            return (ink.width, ink.bounds, ink.carets);
        }
        let w = self.css.as_ref().map(|c| c.measure_text_in(self.font_map, families, size, bold, italic, text)[0]).unwrap_or(0.0);
        (w, None, Vec::new())
    }

    /// Blink's `FixedAscent` / `FixedDescent` from the page's font tables: the font's ascent and descent, whole px
    /// on Linux (when the backend did not shape the run).
    fn metrics(&self, families: &str, size: f32, bold: bool, italic: bool) -> (f32, f32) {
        let m = self.css.as_ref().map(|c| c.measure_text_in(self.font_map, families, size, bold, italic, "x")).unwrap_or([0.0; 3]);
        (m[1].round(), m[2].round())
    }
}

/// White-space processing of a `<text>`'s characters (`white-space-collapse: collapse`, the default): segment
/// breaks and tabs are spaces, a space after a space is gone, and so are the spaces at the text's start and end.
fn collapse(pieces: &mut [(Vec<u16>, Vec<u32>)]) {
    let mut prev_space = true; // at the text's start
    for (units, source) in pieces.iter_mut() {
        let mut out_u = Vec::with_capacity(units.len());
        let mut out_s = Vec::with_capacity(units.len());
        for (i, &u) in units.iter().enumerate() {
            if matches!(u, 0x20 | 0x09 | 0x0A | 0x0D) {
                if prev_space {
                    continue;
                }
                prev_space = true;
                out_u.push(0x20);
            } else {
                prev_space = false;
                out_u.push(u);
            }
            out_s.push(source[i]);
        }
        *units = out_u;
        *source = out_s;
    }
    // the space at the end
    for (units, source) in pieces.iter_mut().rev() {
        if units.is_empty() {
            continue;
        }
        if units.last() == Some(&0x20) {
            units.pop();
            source.pop();
        }
        break;
    }
}

/// Lay a rendered `<text>` out: its runs, positioned and measured.
fn layout(scope: &mut v8::PinScope, root: u32) -> Layout {
    let shaper = Shaper::new(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    // the text nodes, in order, with the element each is in
    let mut nodes: Vec<(u32, u32)> = Vec::new();
    {
        let dom = &st.borrow::<HostDom>().0;
        fn walk(dom: &korvus_dom::Dom, el: u32, out: &mut Vec<(u32, u32)>) {
            for c in dom.children(el) {
                match dom.node_type(c) {
                    3 | 4 => out.push((c, el)),
                    1 if is_svg(dom, c) && is_inline_text_element(&local_tag(dom, c)) => walk(dom, c, out),
                    _ => {}
                }
            }
        }
        walk(dom, root, &mut nodes);
    }
    // an element under `display: none` lays nothing out
    let mut kept = Vec::with_capacity(nodes.len());
    let mut hidden: Vec<(u32, bool)> = Vec::new();
    'nodes: for (node, owner) in nodes {
        let mut cur = owner;
        while cur != root {
            let none = match hidden.iter().find(|(e, _)| *e == cur) {
                Some(&(_, none)) => none,
                None => {
                    let none = css_computed_one(&mut st, cur, "display") == "none";
                    hidden.push((cur, none));
                    none
                }
            };
            if none {
                continue 'nodes;
            }
            match st.borrow::<HostDom>().0.parent(cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        kept.push((node, owner));
    }
    let mut pieces: Vec<(Vec<u16>, Vec<u32>)> = {
        let dom = &st.borrow::<HostDom>().0;
        kept.iter()
            .map(|&(node, _)| {
                let units: Vec<u16> = dom.node_value(node).unwrap_or_default().encode_utf16().collect();
                let source = (0..units.len() as u32).collect();
                (units, source)
            })
            .collect()
    };
    if computed_text(&mut st, root, P::WhiteSpaceCollapse).is_none_or(|v| v == "collapse") {
        collapse(&mut pieces);
    }
    let scale = scaling_factor(&mut st, root);
    let mut runs = Vec::new();
    let mut first = 0usize;
    for ((node, owner), (units, source)) in kept.into_iter().zip(pieces) {
        if units.is_empty() {
            continue;
        }
        let mut get = |p| computed_text(&mut st, owner, p).unwrap_or_default();
        let families = get(P::FontFamily);
        let font_size = get(P::FontSize).trim_end_matches("px").parse::<f32>().unwrap_or(16.0);
        let bold = get(P::FontWeight).parse::<f32>().unwrap_or(400.0) >= 600.0;
        let italic = get(P::FontStyle) != "normal";
        // `GetComputedSizeFromSpecifiedSize(scaling_factor)`, then `EffectiveFontSize`: the font cache's hundredths
        let size = ((font_size * scale) * 100.0).floor() / 100.0;
        let text = String::from_utf16_lossy(&units);
        // the primary font's box from the face the run was shaped in, as Blink's `ScaledFont().PrimaryFont()`
        let (raw_width, ink, carets, (ascent, descent)) = match shaper.shape_run(&families, size, bold, italic, &text) {
            Some(i) => {
                let metrics = i.metrics.unwrap_or_else(|| shaper.metrics(&families, size, bold, italic));
                (i.width, i.bounds, i.carets, metrics)
            }
            None => {
                let (w, b, c) = shaper.shape(&families, size, bold, italic, &text);
                (w, b, c, shaper.metrics(&families, size, bold, italic))
            }
        };
        let len = units.len();
        let mut carets = if carets.len() == len + 1 { carets } else { Vec::new() };
        // letter- and word-spacing, scaled as the font is (`ComputeNewScaledFontForStyle`), after each character
        // (`ShapeResultSpacing::ComputeSpacing`): Chrome 150 lays `letter-spacing="1"` over 13 characters 13px wider
        let mut spacing = |p| computed_text(&mut st, owner, p).and_then(|v| v.trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0) * scale;
        let (ls, ws) = (spacing(P::LetterSpacing), spacing(P::WordSpacing));
        let mut raw_width = raw_width;
        let mut ink = ink;
        if ls != 0.0 || ws != 0.0 {
            let mut acc = 0.0f32;
            let mut last = 0.0f32;
            for k in 0..len {
                if is_trail(units[k]) && k > 0 && is_lead(units[k - 1]) {
                    if let Some(c) = carets.get_mut(k + 1) {
                        *c += acc;
                    }
                    continue;
                }
                let c = if is_lead(units[k]) && units.get(k + 1).is_some_and(|&u| is_trail(u)) {
                    0x10000 + (((units[k] as u32) - 0xD800) << 10) + ((units[k + 1] as u32) - 0xDC00)
                } else {
                    units[k] as u32
                };
                let space = matches!(c, 0x20 | 0x09 | 0x0A | 0xA0);
                let mut sp = 0.0;
                if !zero_width(c) {
                    sp += ls;
                }
                if space && (k > 0 || c == 0xA0) {
                    sp += ws;
                }
                acc += sp;
                last = sp;
                if let Some(cr) = carets.get_mut(k + 1) {
                    *cr += acc;
                }
            }
            raw_width += acc;
            if let Some(b) = ink.as_mut() {
                b[2] += acc - last;
            }
        }
        runs.push(Run { node, owner, first, units, source, families, size, bold, italic, x: 0.0, y: 0.0, raw_width, width: lu_ceil(raw_width), ascent, descent, ink, carets });
        first += len;
    }
    if runs.is_empty() {
        return Layout { runs, scale };
    }
    // positions: the text's x/y start the first chunk; an element's x/y starts a new chunk at its first run, its
    // dx/dy move it; runs otherwise follow one another
    let mut chunks: Vec<(usize, usize)> = vec![(0, 0)];
    {
        let dom = &st.borrow::<HostDom>().0;
        let len = |id, a| first_length(dom, id, a);
        let mut pen_x = len(root, "x").unwrap_or(0.0) * scale + len(root, "dx").unwrap_or(0.0) * scale;
        let mut pen_y = len(root, "y").unwrap_or(0.0) * scale + len(root, "dy").unwrap_or(0.0) * scale;
        let mut started: Vec<u32> = Vec::new();
        for i in 0..runs.len() {
            // the elements this run is the first run of, outermost first
            let mut opened = Vec::new();
            let mut cur = runs[i].owner;
            while cur != root && !started.contains(&cur) {
                opened.push(cur);
                match dom.parent(cur) {
                    Some(p) => cur = p,
                    None => break,
                }
            }
            for &e in opened.iter().rev() {
                started.push(e);
                let (ax, ay) = (len(e, "x"), len(e, "y"));
                if ax.is_some() || ay.is_some() {
                    if let Some(x) = ax {
                        pen_x = x * scale;
                    }
                    if let Some(y) = ay {
                        pen_y = y * scale;
                    }
                    if i > 0 && chunks.last().is_some_and(|c| c.0 != i) {
                        chunks.push((i, i));
                    }
                }
                pen_x += len(e, "dx").unwrap_or(0.0) * scale;
                pen_y += len(e, "dy").unwrap_or(0.0) * scale;
            }
            runs[i].x = pen_x;
            runs[i].y = pen_y;
            pen_x += runs[i].width;
            if let Some(c) = chunks.last_mut() {
                c.1 = i;
            }
        }
    }
    // `text-anchor`, per chunk (`ApplyAnchoring`): the anchor of the chunk's first character's style
    for &(i, j) in &chunks {
        let anchor = computed_text(&mut st, runs[i].owner, P::TextAnchor).unwrap_or_default();
        let rtl = computed_text(&mut st, runs[i].owner, P::Direction).as_deref() == Some("rtl");
        let a = runs[i..=j].iter().map(|r| r.x).fold(f32::INFINITY, f32::min);
        let b = runs[i..=j].iter().map(|r| r.x + r.width).fold(f32::NEG_INFINITY, f32::max);
        let start = runs[i].x;
        let shift = match (anchor.as_str(), rtl) {
            ("end", false) | ("start", true) => start - b,
            ("middle", _) => start - (a + b) / 2.0,
            _ => start - a,
        };
        for r in &mut runs[i..=j] {
            r.x += shift;
        }
    }
    Layout { runs, scale }
}

/// What an element's queries read: its `<text>`'s layout and the indices of the element's own runs.
struct Query {
    layout: Layout,
    runs: Vec<usize>,
    shaper: Shaper,
}

/// A run's caret position after `k` of its code units, in the run's shaping (scaled px): Blink's
/// `CaretPositionForOffset` over the item's shape result, where a kerning pair's adjustment is its first
/// glyph's. Without the backend's carets, the advance of the prefix shaped alone.
fn caret(shaper: &Shaper, r: &Run, k: usize) -> f32 {
    if k == 0 {
        return 0.0;
    }
    if k >= r.units.len() {
        return r.raw_width;
    }
    if let Some(&c) = r.carets.get(k) {
        return c;
    }
    let prefix = String::from_utf16_lossy(&r.units[..k]);
    shaper.shape(&r.families, r.size, r.bold, r.italic, &prefix).0
}

/// Blink's `Character::TreatAsZeroWidthSpace`: no letter-spacing after it -- form feed, carriage return, the object
/// replacement character, ZWJ/ZWNJ, and Unicode's default-ignorable code points.
fn zero_width(c: u32) -> bool {
    matches!(
        c,
        0x0C | 0x0D | 0xFFFC | 0x00AD | 0x034F | 0x061C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0
            | 0xFFF0..=0xFFF8 | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF
    )
}

fn is_lead(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}

fn is_trail(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

/// `FragmentItem::LocalRect` for code units `[a, b)` of a run: `(start, width)` in scaled LayoutUnits. A whole
/// item is its size (`LayoutUnit(rect.width)`); a part is `LineLeftAndRightForOffsets`, the caret positions
/// rounded out.
fn local_span(shaper: &Shaper, r: &Run, a: usize, b: usize) -> (f32, f32) {
    if a == 0 && b >= r.units.len() {
        return (0.0, lu_trunc(r.width));
    }
    let (s, e) = (caret(shaper, r, a), caret(shaper, r, b));
    let (s, e) = (lu_floor(s.min(e)), lu_ceil(s.max(e)));
    (s, e - s)
}

impl Query {
    fn count(&self) -> usize {
        self.runs.iter().map(|&i| self.layout.runs[i].units.len()).sum()
    }

    /// `FindFragmentItemForAddressableCodeUnitIndex`: the run holding the element's `index`th code unit, and the
    /// character's code units in it (a surrogate pair is one character).
    fn at(&self, index: usize) -> Option<(&Run, usize, usize)> {
        let mut n = 0;
        for &i in &self.runs {
            let r = &self.layout.runs[i];
            if index < n + r.units.len() {
                let mut k = index - n;
                if k > 0 && is_trail(r.units[k]) && is_lead(r.units[k - 1]) {
                    k -= 1;
                }
                let end = if is_lead(r.units[k]) && r.units.get(k + 1).is_some_and(|&u| is_trail(u)) { k + 2 } else { k + 1 };
                return Some((r, k, end));
            }
            n += r.units.len();
        }
        None
    }

    /// `SvgTextQuery::SubStringLength`: the inline sizes of the ranges each item holds, each divided by the
    /// factor.
    fn sub_string_length(&self, start: usize, length: usize) -> f32 {
        if length == 0 {
            return 0.0;
        }
        let mut total = 0.0f32;
        let mut ci = 0usize;
        let end = start + length;
        for &i in &self.runs {
            let r = &self.layout.runs[i];
            if end <= ci {
                break;
            }
            let next = ci + r.units.len();
            if (ci <= start && start < next) || (ci < end && end <= next) || (start < ci && next < end) {
                let mut a = start.saturating_sub(ci);
                let mut b = end.min(next) - ci;
                // `AdjustCodeUnitStartOffset` / `AdjustCodeUnitEndOffset`
                if a > 0 && a < r.units.len() && is_trail(r.units[a]) && is_lead(r.units[a - 1]) {
                    a -= 1;
                }
                if b > 0 && b < r.units.len() && is_trail(r.units[b]) && is_lead(r.units[b - 1]) {
                    b += 1;
                }
                let (_, w) = local_span(&self.shaper, r, a, b);
                total += w / self.layout.scale;
            }
            ci = next;
        }
        total
    }

    /// `ScaledCharacterRectInContainer`: the character's rect in scaled user space.
    fn char_rect(&self, index: usize) -> Option<(&Run, [f32; 4])> {
        let (r, a, b) = self.at(index)?;
        let (start, width) = local_span(&self.shaper, r, a, b);
        let height = lu_trunc(r.ascent + r.descent);
        Some((r, [start + r.x, 0.0 + (r.y - r.ascent), width, height]))
    }

    /// `StartOrEndPosition`, horizontal and left-to-right.
    fn position(&self, index: usize, end: bool) -> Option<(f32, f32)> {
        let (r, rect) = self.char_rect(index)?;
        let x = if end { rect[0] + rect[2] } else { rect[0] };
        let y = rect[1] + r.ascent;
        let inv = 1.0 / self.layout.scale;
        Some((x * inv, y * inv))
    }

    fn extent(&self, index: usize) -> Option<[f32; 4]> {
        let (_, rect) = self.char_rect(index)?;
        let inv = 1.0 / self.layout.scale;
        Some([rect[0] * inv, rect[1] * inv, rect[2] * inv, rect[3] * inv])
    }

    /// `FragmentItem::ObjectBoundingBox`, united over the element's runs: each run's box and its glyphs' ink.
    fn bbox(&self) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        let inv = 1.0 / self.layout.scale;
        for &i in &self.runs {
            let r = &self.layout.runs[i];
            let rect = [r.x, r.y - r.ascent, r.width, r.ascent + r.descent];
            let mut b = rect;
            if let Some([l, t, rr, bb]) = r.ink {
                // the ink below the box's top (`FloatAscent`), placed at the box
                let ink = [l + rect[0], t + r.ascent + rect[1], rr - l, bb - t];
                b = union(ink, rect);
            }
            out = union(out, [b[0] * inv, b[1] * inv, b[2] * inv, b[3] * inv]);
        }
        out
    }
}

/// `gfx::RectF::Union`: an empty rect contributes nothing.
fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let empty = |r: [f32; 4]| r[2] <= 0.0 || r[3] <= 0.0;
    if empty(a) {
        return b;
    }
    if empty(b) {
        return a;
    }
    let x = a[0].min(b[0]);
    let y = a[1].min(b[1]);
    let r = (a[0] + a[2]).max(b[0] + b[2]);
    let bt = (a[1] + a[3]).max(b[1] + b[3]);
    [x, y, r - x, bt - y]
}

/// The query for `el`, when it has a layout object.
fn query(scope: &mut v8::PinScope, el: u32) -> Option<Query> {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        text_root(&mut st, el)?
    };
    let layout = layout(scope, root);
    let runs = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (0..layout.runs.len()).filter(|&i| el == root || dom.contains(el, layout.runs[i].node)).collect()
    };
    let shaper = Shaper::new(scope);
    Some(Query { layout, runs, shaper })
}

/// Whether `el` is an SVG text-content element: its `getBBox()` is its text's.
pub(crate) fn is_text_element(dom: &korvus_dom::Dom, el: u32) -> bool {
    is_svg(dom, el) && matches!(local_tag(dom, el).as_str(), "text" | "tspan" | "textPath")
}

/// A text-content element's bounding box in its user space (`LayoutSVGText::ObjectBoundingBox`), or `None` without a
/// laid-out character (`IsObjectBoundingBoxValid`: no layout object, or nothing to lay out).
pub(crate) fn text_bbox(scope: &mut v8::PinScope, el: u32) -> Option<[f32; 4]> {
    query(scope, el).filter(|q| !q.runs.is_empty()).map(|q| q.bbox())
}

// ── the members ──────────────────────────────────────────────────────────────────────────────────

fn receiver(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let id = unwrap_id(scope, args.this().into());
    if id.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    id
}

/// Blink's `ExceptionMessages::IndexExceedsMaximumBound`.
fn throw_bound(scope: &mut v8::PinScope, method: &str, given: u32, bound: usize) {
    let eq = given as usize == bound;
    throw_dom(
        scope,
        &format!(
            "Failed to execute '{method}' on 'SVGTextContentElement': The charnum provided ({given}) is greater than {}the maximum bound ({bound}).",
            if eq { "or equal to " } else { "" }
        ),
        "IndexSizeError",
    );
}

fn charnum(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> Option<u32> {
    args.get(i).uint32_value(scope)
}

pub(crate) fn cb_get_number_of_chars(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let n = query(scope, el).map(|q| q.count()).unwrap_or(0);
    rv.set_uint32(n as u32);
}

pub(crate) fn cb_get_computed_text_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let len = query(scope, el).map(|q| q.sub_string_length(0, q.count())).unwrap_or(0.0);
    rv.set_double(len as f64);
}

pub(crate) fn cb_get_sub_string_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let Some(start) = charnum(scope, &args, 0) else { return };
    let Some(nchars) = charnum(scope, &args, 1) else { return };
    let q = query(scope, el);
    let n = q.as_ref().map(|q| q.count()).unwrap_or(0);
    if start as usize >= n {
        throw_bound(scope, "getSubStringLength", start, n);
        return;
    }
    let nchars = (nchars as usize).min(n - start as usize);
    let len = q.map(|q| q.sub_string_length(start as usize, nchars)).unwrap_or(0.0);
    rv.set_double(len as f64);
}

fn position_member(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, method: &str, end: bool) {
    let Some(el) = receiver(scope, args) else { return };
    let Some(index) = charnum(scope, args, 0) else { return };
    let q = query(scope, el);
    let n = q.as_ref().map(|q| q.count()).unwrap_or(0);
    if index as usize >= n {
        throw_bound(scope, method, index, n);
        return;
    }
    let (x, y) = q.and_then(|q| q.position(index as usize, end)).unwrap_or((0.0, 0.0));
    if let Some(o) = crate::core::dom::svg_values::detached_value(scope, "SVGPoint", &[("x", x as f64), ("y", y as f64)]) {
        rv.set(o.into());
    }
}

pub(crate) fn cb_get_start_position_of_char(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    position_member(scope, &args, &mut rv, "getStartPositionOfChar", false);
}

pub(crate) fn cb_get_end_position_of_char(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    position_member(scope, &args, &mut rv, "getEndPositionOfChar", true);
}

pub(crate) fn cb_get_extent_of_char(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let Some(index) = charnum(scope, &args, 0) else { return };
    let q = query(scope, el);
    let n = q.as_ref().map(|q| q.count()).unwrap_or(0);
    if index as usize >= n {
        throw_bound(scope, "getExtentOfChar", index, n);
        return;
    }
    let r = q.and_then(|q| q.extent(index as usize)).unwrap_or([0.0; 4]);
    let fields = [("x", r[0] as f64), ("y", r[1] as f64), ("width", r[2] as f64), ("height", r[3] as f64)];
    if let Some(o) = crate::core::dom::svg_values::detached_value(scope, "SVGRect", &fields) {
        rv.set(o.into());
    }
}

/// `getRotationOfChar`: the `rotate` value the character resolves -- the nearest list among the element and its
/// text-content ancestors, indexed from that element's first character, its last value repeating.
pub(crate) fn cb_get_rotation_of_char(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let Some(index) = charnum(scope, &args, 0) else { return };
    let q = query(scope, el);
    let n = q.as_ref().map(|q| q.count()).unwrap_or(0);
    if index as usize >= n {
        throw_bound(scope, "getRotationOfChar", index, n);
        return;
    }
    let Some(q) = q else { return };
    let Some((r, k, _)) = q.at(index as usize) else { return };
    let global = r.first + k;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut angle = 0.0f32;
    let mut cur = Some(r.owner);
    while let Some(e) = cur {
        let list = rotate_list(dom, e);
        if !list.is_empty() {
            let base = q.layout.runs.iter().filter(|run| dom.contains(e, run.node)).map(|run| run.first).min().unwrap_or(0);
            angle = list.get(global.saturating_sub(base)).or(list.last()).copied().unwrap_or(0.0);
            break;
        }
        if local_tag(dom, e) == "text" {
            break;
        }
        cur = dom.parent(e);
    }
    rv.set_double(angle as f64);
}

/// `getCharNumAtPosition(point)` (`SvgTextQuery::CharacterNumberAtPosition`): the first item whose scaled box
/// holds the point, inclusively, then the character the point's inline offset falls in
/// (`ShapeResult::OffsetForPosition`); -1 when no item holds it.
pub(crate) fn cb_get_char_num_at_position(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    // the `DOMPointInit` dictionary, its members read in WebIDL's (lexicographic) order
    let p = args.get(0);
    let mut point = [0.0f64; 2];
    if !p.is_null_or_undefined() {
        let Ok(o) = v8::Local::<v8::Object>::try_from(p) else {
            throw_type_error(scope, "Failed to execute 'getCharNumAtPosition' on 'SVGTextContentElement': The provided value is not of type 'DOMPointInit'.");
            return;
        };
        for (key, slot) in [("w", None), ("x", Some(0)), ("y", Some(1)), ("z", None)] {
            let k = v8::String::new(scope, key).unwrap();
            let Some(v) = o.get(scope, k.into()) else { return };
            if v.is_undefined() {
                continue;
            }
            let Some(n) = v.number_value(scope) else { return };
            if let Some(i) = slot {
                point[i] = n;
            }
        }
    }
    let (px, py) = (point[0] as f32, point[1] as f32);
    let Some(q) = query(scope, el) else {
        rv.set_int32(-1);
        return;
    };
    let s = q.layout.scale;
    let (sx, sy) = (px * s, py * s);
    let mut count = 0usize;
    for &i in &q.runs {
        let r = &q.layout.runs[i];
        let rect = [r.x, r.y - r.ascent, r.width, r.ascent + r.descent];
        if sx >= rect[0] && sx <= rect[0] + rect[2] && sy >= rect[1] && sy <= rect[1] + rect[3] {
            // the point (rounded to LayoutUnits) from the item's unscaled enclosing rect, scaled back
            let left = lu_floor(rect[0] * (1.0 / s));
            let offset = (lu_round(px) - left) * s;
            let k = if offset <= 0.0 {
                0
            } else if offset >= r.raw_width {
                r.units.len()
            } else {
                let mut k = 0;
                while k + 1 < r.units.len() && caret(&q.shaper, r, k + 1) <= offset {
                    k += 1;
                }
                k
            };
            rv.set_int32((count + k) as i32);
            return;
        }
        count += r.units.len();
    }
    rv.set_int32(-1);
}

/// `selectSubString(charnum, nchars)`: the document's selection over those characters (Blink's
/// `FrameSelection::SelectSubString`), set through the realm's own selection, never the page's.
pub(crate) fn cb_select_sub_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(el) = receiver(scope, &args) else { return };
    let Some(start) = charnum(scope, &args, 0) else { return };
    let Some(nchars) = charnum(scope, &args, 1) else { return };
    let q = query(scope, el);
    let n = q.as_ref().map(|q| q.count()).unwrap_or(0);
    if start as usize >= n {
        throw_bound(scope, "selectSubString", start, n);
        return;
    }
    let Some(q) = q else { return };
    let end = start as usize + (nchars as usize).min(n - start as usize);
    let Some((ra, ka, _)) = q.at(start as usize) else { return };
    let (anchor, anchor_offset) = (ra.node, ra.source[ka]);
    let (focus, focus_offset) = match q.at(end) {
        Some((rb, kb, _)) => (rb.node, rb.source[kb]),
        None => match q.at(end - 1) {
            Some((rb, _, kb_end)) => (rb.node, rb.source[kb_end - 1] + 1),
            None => (anchor, anchor_offset),
        },
    };
    crate::core::editing::selection::select_text_range(scope, (anchor, anchor_offset), (focus, focus_offset));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(s: &str) -> (Vec<u16>, Vec<u32>) {
        let u: Vec<u16> = s.encode_utf16().collect();
        let n = u.len() as u32;
        (u, (0..n).collect())
    }

    #[test]
    fn white_space_collapses_across_runs_and_trims_the_ends() {
        let mut p = vec![piece("  a   b  "), piece("\n c "), piece("  ")];
        collapse(&mut p);
        let s: Vec<String> = p.iter().map(|(u, _)| String::from_utf16_lossy(u)).collect();
        assert_eq!(s, vec!["a b ", "c", ""]);
        assert_eq!(p[0].1, vec![2, 3, 6, 7], "each kept unit keeps its offset in its node");
    }

    #[test]
    fn layout_units_round_as_blink_rounds_them() {
        assert_eq!(lu_ceil(438.3), 438.3125);
        assert_eq!(lu_floor(146.1), 146.09375);
        assert_eq!(lu_trunc(1611.015625), 1611.015625);
        assert_eq!(union([0.0, 0.0, 0.0, 0.0], [1.0, 2.0, 3.0, 4.0]), [1.0, 2.0, 3.0, 4.0]);
    }
}

//! `core/css/cssom.rs` -- the bridge from `CSSStyleSheet` to the page pipeline's parsed rules.
//!
//! MOVED HERE FROM `core/korvus-runtime/src/cssom.rs` unchanged except for the paths
//! above: the kernel defines no Web API (invariant I9), and the CSSOM tables that call these are in
//! this crate.
//!
//! THIS FILE DECLARES NO OPS. It had sixteen, and every one of them was a one-line `#[op2]` wrapper
//! around the plain Rust function beneath it, put there when the CSSOM lived in `host.js` and could
//! only reach Rust through the op table. The last of that JS is gone; a later pass
//! re-measured and found that no op in this file had a caller of ANY kind -- not a Rust call, not a
//! JS string literal, not a `deno_op(scope, "…")` name lookup -- and deleted all sixteen. The
//! `interface!` table bodies in `style_sheet.rs` / `css_rule.rs` call the plain functions directly,
//! which is one hop shorter and was already the only live path.
//!
//! A stylesheet in Korvus used to be a list of `{cssText, type}` objects, and `replaceSync` split the
//! text on `}`. Nothing could be asked of that — not `selectorText`, not which declarations a rule
//! carries, not whether a selector is even valid — so every CSSOM member that reads *into* a rule
//! answered with the raw text or with nothing. These functions put `korvus-page`'s parser behind the
//! same JS surface.
//!
//! # The `#[reentrant]` annotations went with the ops, and nothing is owed
//!
//! Every op that took a `#[string]` was marked `#[reentrant]` because a non-string argument converts
//! by calling its `toString()` — page JS, running inside an op — and deno_core's guard turns that
//! into an abort rather than an exception. A plain Rust function is not an op frame and has no such
//! guard to trip; it is handed a `&str` that the table body has already coerced in a normal V8 call.
//!
//! # Degrading
//!
//! Every function here answers as an unstyled document would when the page plugin is not loaded: no
//! sheets, no rules, and edits that quietly do nothing. Loading Korvus without the pipeline has to
//! keep working, and the CSSOM is exactly where a missing service would otherwise throw.

use deno_core::{v8, OpState};
use korvus_dom::Dom;
use page::style::cssom::CssomError;
use page::style::media::MediaContext;
use page::style::property::PropertyId;
use super::computed_shorthand;
use page::layout::resolved;
use page::{LaidOut, PageHandle};
use crate::binding::host_dom::HostDom;

/// The result codes `insertRule` reports back to JS, which turns them into DOM exceptions.
/// Negative because a success is the index the rule landed at.
const ERR_INDEX_SIZE: i32 = -1;
const ERR_SYNTAX: i32 = -2;
/// The page plugin is not loaded, so there was nothing to insert into.
const ERR_NO_PIPELINE: i32 = -3;
/// The rule may not go where it was asked to (an `@import` after a style rule) -- `HierarchyRequestError`.
pub(crate) const ERR_HIERARCHY: i32 = -4;

fn page(state: &OpState) -> Option<std::sync::Arc<page::Page>> {
    let ctx = state.try_borrow::<spec::kernel::Context>()?;
    ctx.get::<PageHandle>().map(|h| h.0.clone())
}

/// What the media queries and layout are asked about -- the ONE definition
/// ([`crate::core::dom::geometry_queries::media_context`], W5.0d), shared with the geometry rows so a
/// page that measures the viewport through `@media` and through `offsetWidth` gets one answer.
pub(crate) fn media_context(state: &OpState) -> MediaContext {
    crate::core::dom::geometry_queries::media_context(state)
}

/// Parse `text` as an author stylesheet and return the handle the CSSOM refers to it by.
/// `u32::MAX` means there is no pipeline to parse it with.
///
/// Called by the `CSSStyleSheet` table body in `style_sheet.rs`. It used to have an
/// `op_css_sheet_new` wrapper above it, and so did every other function in this file; those were
/// the JS-facing doors and are deleted.
pub(crate) fn sheet_new(state: &mut OpState, text: &str) -> u32 {
    match page(state) {
        Some(p) => p.add_constructed_sheet(text),
        None => u32::MAX,
    }
}

/// The tree rooted at `tree` (a document or a shadow root) adopts these constructed sheets, in this
/// order: its `adoptedStyleSheets` was set. A constructed sheet styles only the
/// trees that adopt it.
/// `tree` is a shadow root's id or a document's root slot (the page document's slot names whichever
/// arena root is current).
pub(crate) fn adopt_sheets(state: &mut OpState, tree: u32, sheets: &[u32]) {
    let Some(p) = page(state) else { return };
    let Some(dom) = state.try_borrow::<HostDom>() else { return };
    p.set_adopted_sheets(crate::core::dom::document::resolve_doc_root(&dom.0, tree), sheets);
}

/// The handle for an element's sheet -- the one the cascade is already using, not a second empty
/// one. `u32::MAX` when there is no pipeline or no DOM to find the element in.
///
/// A LIVE PATH AGAIN, AND WHY. `sheet_for_element` stood here as the orphan of a deleted
/// op and was removed, correctly: nothing asked. Nothing asked because `<link>.sheet`
/// minted its own empty sheet with [`sheet_new`], which is exactly the defect P3a measured -- the
/// bytes of a linked stylesheet arrived, were dropped on the floor, and script read an empty sheet
/// while the cascade read nothing at all. A `<link>`'s rules have only one home, the entry
/// `Page::sync_document_sheets` gave it in document order, and this is the door to it.
pub(crate) fn sheet_for_element(state: &mut OpState, node: u32) -> u32 {
    let Some(p) = page(state) else { return u32::MAX };
    let Some(dom) = state.try_borrow::<HostDom>() else { return u32::MAX };
    p.sheet_for_owner(&dom.0, node)
}

/// A `<link rel=stylesheet>`'s bytes have arrived: parse them into that element's sheet and record
/// whether script may read it back ([`page::style::rules::Stylesheet::origin_clean`]).
pub(crate) fn linked_sheet_loaded(state: &mut OpState, node: u32, text: &str, origin_clean: bool) -> u32 {
    let Some(p) = page(state) else { return u32::MAX };
    let Some(dom) = state.try_borrow::<HostDom>() else { return u32::MAX };
    p.set_linked_sheet(&dom.0, node, text, origin_clean)
}

/// The sheet's own media text (CSSOM's `StyleSheet.media`), or `None` when there is no pipeline. The
/// document's sheets are synced first, so a `media` attribute written a moment ago is already the
/// sheet's.
pub(crate) fn sheet_media_text(state: &mut OpState, sheet: u32) -> Option<String> {
    let p = page(state)?;
    if let Some(dom) = state.try_borrow::<HostDom>() {
        p.sync_document_sheets(&dom.0);
    }
    p.with_sheet(sheet, |s| s.media_text().to_string())
}

/// Set the sheet's own media list. The owner element's `media` attribute is NOT written: Chrome
/// leaves it alone (see `Page`'s `media_attrs`). `false` when there is no pipeline.
pub(crate) fn set_sheet_media_text(state: &mut OpState, sheet: u32, text: &str) -> bool {
    let Some(p) = page(state) else { return false };
    if let Some(dom) = state.try_borrow::<HostDom>() {
        p.sync_document_sheets(&dom.0);
    }
    p.with_sheet_mut(sheet, |s| s.set_media_text(text)).is_some()
}

/// May script READ this sheet's rules? False only for a cross-origin sheet fetched without CORS.
/// A handle with no sheet behind it answers `true`: the reads that follow find no rules anyway, and
/// a missing sheet is not a security fact.
pub(crate) fn sheet_origin_clean(state: &mut OpState, sheet: u32) -> bool {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.origin_clean)).unwrap_or(true)
}

// ── THE RESOLVED STYLE OF ONE ELEMENT, ONE PROPERTY AT A TIME ──────────────────────
//
// These four replace `css_computed_all`, which packed EVERY property of `node` into one
// `\n`-delimited string that [`cb_get_computed_style`] then split apart into 475 V8 strings, 475
// array sets and 475 object sets -- about 1,900 V8 operations for a caller that reads one property.
// Measured on a synthetic 2,006-element page: `getComputedStyle` x2006 cost 355 ms here and 0.2 ms
// in Chrome 150, and reading NOTHING off the result cost the same as reading everything.
//
// The fix is not a faster pack, it is no pack: `getComputedStyle` hands back a declaration carrying
// the element's node id, and each member below answers the one question it was asked. Chrome's own
// cost does scale with what you read -- reading all 735 of its named properties off the same 2,006
// elements costs it 314 ms, the same order as our old eager pack -- so the 474x gap was never engine
// speed, it was work nobody asked for.
//
// `Page::computed_style` memoises the cascade per element under `layout_fingerprint`, so reading
// five properties off one declaration (what `innerText` does per element) still cascades once.

/// The layout the geometry ops already keep, refreshed if anything it depends on has moved.
///
/// Reusing it rather than building a private one per call is not an optimisation, it is the
/// difference between `getComputedStyle` costing ~11 ms and costing nothing: `Page::layout` rebuilds
/// the whole box tree unconditionally (design D9), and only the caller's cache makes that once per
/// document instead of once per element.
fn with_resolved<T>(state: &mut OpState, node: u32, needs_layout: bool, absent: T, f: impl FnOnce(&page::Page, &Dom, &MediaContext, Option<&LaidOut>) -> T) -> T {
    // ONLY when the question is one layout answers. Two dozen of the 475 properties resolve from the
    // box tree; `display`, `color` and the rest do not, and neither do `length`, `item` or the
    // enumeration. Refreshing for those would put a fingerprint hash and two mutex round trips on
    // every property read a page makes.
    if needs_layout {
        crate::core::dom::geometry_queries::refresh_layout_cache(state);
    }
    let Some(p) = page(state) else { return absent };
    // A node of a FRAME document -- not in the page's tree, but in a document a frame shows --
    // resolves against that frame: its viewport for media queries, its layout for a used value. It was
    // `absent` (an empty declaration: Chrome answers the full computed style), so `innerText` there read
    // every script and hidden node, and html2canvas drew a frame clone with no style at all (NGENIX).
    let frame_root = match state.try_borrow::<HostDom>() {
        Some(dom) if !dom.0.is_connected(node) => Some(dom.0.shadow_including_root(node)),
        Some(_) => None,
        None => return absent,
    };
    if let Some(root) = frame_root {
        // a document whose browsing context is gone -- its iframe removed -- has no style: Chrome 150 answers an
        // empty declaration (length 0, every value "") for an element of it, a declaration taken before the
        // removal included
        if !crate::core::dom::svg_text::document_rendered(state, node, 0) {
            return absent;
        }
        let Some(media) = crate::core::dom::geometry_queries::frame_media(state, root) else { return absent };
        let laid = needs_layout && crate::core::dom::geometry_queries::refresh_frame(state, root);
        let Some(dom) = state.try_borrow::<HostDom>() else { return absent };
        let laid_out = if laid { crate::core::dom::geometry_queries::frame_laid_out(state, root) } else { None };
        return f(&p, &dom.0, &media, laid_out);
    }
    let media = media_context(state);
    let Some(dom) = state.try_borrow::<HostDom>() else { return absent };
    // A declaration keeps a node id, not a node: an id whose element has been removed from the
    // document answers for NOTHING, which is what Chrome's live object does (measured -- a
    // `getComputedStyle` taken before `el.remove()` reports `length` 0 and "" afterwards).
    if !dom.0.is_connected(node) {
        return absent;
    }
    let laid_out = state
        .try_borrow::<crate::core::dom::geometry_queries::LayoutCache>()
        .and_then(|c| c.0.as_ref())
        .map(|(_, out)| out);
    f(&p, &dom.0, &media, laid_out)
}

/// One property of an element's COMPUTED style, without a layout -- `None` for an element with no
/// style (not connected). For a body that needs a font rather than a declaration: SVG text measurement
/// (`core/dom/svg_geometry.rs`).
pub(crate) fn computed_text(state: &mut OpState, node: u32, property: page::style::property::PropertyId) -> Option<String> {
    with_resolved(state, node, false, None, |p, dom, media, _| p.computed_value(dom, node, media, property, None))
}

/// [`computed_text`] for many elements of ONE document (its shadow trees included) at once: the page, the media
/// context and the DOM are taken once, not once per read. Row `i` is `nodes[i]`'s value of each of `properties`, `None`
/// for an element with no style. The accessibility tree reads five properties off every element of a document.
pub(crate) fn computed_texts(state: &mut OpState, nodes: &[u32], properties: &[page::style::property::PropertyId]) -> Vec<Option<Vec<String>>> {
    let Some(&first) = nodes.first() else { return Vec::new() };
    with_resolved(state, first, false, vec![None; nodes.len()], |p, dom, media, _| {
        nodes.iter().map(|&n| properties.iter().map(|&prop| p.computed_value(dom, n, media, prop, None)).collect()).collect()
    })
}

/// RENDER: paint the current document to an offscreen RGBA8 frame through the compositor paint walk.
/// Mirrors [`with_resolved`]'s gather — refresh the shared layout, take
/// Page + Dom + MediaContext + the cached `LaidOut` — build the 2D display list in korvus-page's `paint`
/// module, composite replaced content (`<img>`/`<canvas>`), and raster with the active `RenderHandle`
/// (Skia, R3). `None` until there is a page, a laid-out tree, and a render backend. Viewport at DPR 1 for
/// now (R4 fills the persona DPR later). Shared by `render_document_png` (`--screenshot`) and
/// `render_document_frame` (the per-frame rendering opportunity, RC2.2).
/// The last rastered frame — viewport size, the display list that produced it, and its
/// pixels. Used to SKIP a re-raster when the next frame's display list is identical and carries no
/// replaced content. One buffer; the `ops == ops` guard (cheap scalar/string compare, never image bytes)
/// makes a stale entry a miss rather than a wrong frame. Pairs with RC2.b.0 (which makes a rAF animation
/// paint every frame): a visually-static rAF loop then reuses the cached frame instead of re-rastering.
struct FrameCache {
    w: u32,
    h: u32,
    ops: Vec<spec::Canvas2DOp>,
    buf: spec::Rgba8,
    /// Which raster this is: one more than the frame it replaced. A reused frame keeps its number, so the
    /// frame tap ([`deliver_frame`]) hands a plugin a frame only when its pixels were drawn anew.
    gen: u64,
}

/// The [`FrameCache`] generation the frame tap last handed the plugins.
struct FrameDelivered(u64);

/// When the frame tap last painted, on the page clock (ms): it paints at most once per display refresh.
struct FramePaintedAt(f64);

// ── Per-tile damage ──────────────────────────────────────────────────────────────
// The page display list uses only ABSOLUTE-coordinate draws (FillRect/ClearRect/FillText/DrawImage)
// plus style state (fill colour, gradients, font) — paint_display_list emits no transform, clip,
// shadow, filter or composite op, and the replaced loop adds only DrawImage. So a tile's pixels depend
// only on the draws whose device box intersects it, with no cross-tile bleed; we can re-raster exactly
// the tiles whose ordered draw-sequence changed between frames and reuse the rest. ANY op the resolver
// below does not model makes it return None, and the caller falls back to a full raster — so a display
// list that grows past this subset stays correct, just not incrementally cheap.
const TILE: u32 = 256;

#[derive(Clone, PartialEq)]
enum DamageFill {
    /// A paint colour's bits.
    Solid([u32; 4]),
    Grad(u64),
}

#[derive(Clone, PartialEq)]
enum DamageKind {
    Rect { fill: DamageFill, clear: bool },
    Text { fill: DamageFill, style: u64, text: String },
    Image { pixels: u64 },
}

/// A draw resolved to its device bounding box and visual identity. `PartialEq` compares both, so a
/// moved rect, a recoloured fill, a changed image or a changed string are all distinct.
#[derive(Clone, PartialEq)]
struct DamageDraw {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    kind: DamageKind,
}

fn damage_hash<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

fn norm_rect(x: f64, y: f64, w: f64, h: f64) -> (f64, f64, f64, f64) {
    let (x, w) = if w < 0.0 { (x + w, -w) } else { (x, w) };
    let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
    (x, y, w, h)
}

/// Resolve a page display list to its draws (device box + resolved style), or `None` if it uses an op
/// this incremental path does not model (→ caller does a full raster).
fn resolve_draws(ops: &[spec::Canvas2DOp]) -> Option<Vec<DamageDraw>> {
    use spec::Canvas2DOp as Op;
    let mut draws = Vec::new();
    let mut fill = DamageFill::Solid([0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits));
    let mut grad: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut family = String::new();
    let (mut size, mut bold, mut italic) = (10.0_f64, false, false);
    let (mut baseline, mut align, mut dir) = (String::new(), String::new(), String::new());
    let (mut letter, mut word) = (0.0_f64, 0.0_f64);
    let push_f = |v: &mut Vec<u8>, f: f64| v.extend_from_slice(&f.to_bits().to_le_bytes());
    for op in ops {
        match op {
            Op::SetFillStyle { color } => fill = DamageFill::Solid(color.map(f32::to_bits)),
            Op::SetFillGradient { id } => fill = DamageFill::Grad(damage_hash(grad.get(id).unwrap_or(&Vec::new()))),
            Op::DefineLinearGradient { id, x0, y0, x1, y1 } => {
                let e = grad.entry(*id).or_default();
                e.clear();
                e.push(1);
                for f in [*x0, *y0, *x1, *y1] {
                    push_f(e, f);
                }
            }
            Op::DefineRadialGradient { id, x0, y0, r0, x1, y1, r1 } => {
                let e = grad.entry(*id).or_default();
                e.clear();
                e.push(2);
                for f in [*x0, *y0, *r0, *x1, *y1, *r1] {
                    push_f(e, f);
                }
            }
            Op::DefineConicGradient { id, start_angle, x, y } => {
                let e = grad.entry(*id).or_default();
                e.clear();
                e.push(3);
                for f in [*start_angle, *x, *y] {
                    push_f(e, f);
                }
            }
            Op::AddColorStop { id, offset, color, oklab, legacy } => {
                let e = grad.entry(*id).or_default();
                push_f(e, *offset);
                for c in color.iter().chain(oklab) {
                    e.extend_from_slice(&c.to_bits().to_le_bytes());
                }
                e.push(*legacy as u8);
            }
            Op::PremultiplyGradient { id } => grad.entry(*id).or_default().push(4),
            Op::SetFont { family: f, size_px, bold: bo, italic: it, .. } => {
                family = f.clone();
                size = *size_px;
                bold = *bo;
                italic = *it;
            }
            Op::SetTextBaseline { baseline: b } => baseline = format!("{b:?}"),
            Op::SetTextAlign { align: a } => align = format!("{a:?}"),
            Op::SetTextDirection { dir: d } => dir = format!("{d:?}"),
            Op::SetLetterSpacing { px } => letter = *px,
            Op::SetWordSpacing { px } => word = *px,
            Op::FillRect { x, y, w, h } => {
                let (x, y, w, h) = norm_rect(*x, *y, *w, *h);
                draws.push(DamageDraw { x, y, w, h, kind: DamageKind::Rect { fill: fill.clone(), clear: false } });
            }
            Op::ClearRect { x, y, w, h } => {
                let (x, y, w, h) = norm_rect(*x, *y, *w, *h);
                draws.push(DamageDraw { x, y, w, h, kind: DamageKind::Rect { fill: DamageFill::Solid([0; 4]), clear: true } });
            }
            Op::FillText { text, x, y, .. } => {
                // Text cannot bleed here (no shadow/filter); over-cover its box generously — a glyph
                // advance is at most ~the font size, and align shifts it at most its own width, so a box
                // `2·len·size` wide centred on `x` and `2.5·size` tall around `y` covers every baseline
                // and alignment. Over-covering only costs a few extra tiles on a text that CHANGED.
                let len = (text.chars().count().max(1)) as f64;
                let fs = size.max(1.0);
                let style = damage_hash(&(&family, size.to_bits(), bold, italic, &baseline, &align, &dir, letter.to_bits(), word.to_bits()));
                draws.push(DamageDraw {
                    x: x - len * fs,
                    y: y - fs * 1.5,
                    w: 2.0 * len * fs,
                    h: fs * 2.5,
                    kind: DamageKind::Text { fill: fill.clone(), style, text: text.clone() },
                });
            }
            Op::DrawImage { image, dx, dy, dw, dh, .. } => {
                let (x, y, w, h) = norm_rect(*dx, *dy, *dw, *dh);
                draws.push(DamageDraw { x, y, w, h, kind: DamageKind::Image { pixels: damage_hash(&image.data) } });
            }
            _ => return None,
        }
    }
    Some(draws)
}

/// The tiles (device rects) whose ordered draw-sequence differs between the previous and current frame,
/// or `None` if either list is not modelable (→ full raster). Empty = no visible change.
fn tile_damage(prev: &[spec::Canvas2DOp], cur: &[spec::Canvas2DOp], w: u32, h: u32) -> Option<Vec<(u32, u32, u32, u32)>> {
    let pd = resolve_draws(prev)?;
    let cd = resolve_draws(cur)?;
    let hit = |d: &DamageDraw, rx: f64, ry: f64, rw: f64, rh: f64| d.x < rx + rw && d.x + d.w > rx && d.y < ry + rh && d.y + d.h > ry;
    let cols = w.div_ceil(TILE);
    let rows = h.div_ceil(TILE);
    let mut out = Vec::new();
    for ty in 0..rows {
        for tx in 0..cols {
            let rx = tx * TILE;
            let ry = ty * TILE;
            let rw = TILE.min(w - rx);
            let rh = TILE.min(h - ry);
            let (fx, fy, fw, fh) = (rx as f64, ry as f64, rw as f64, rh as f64);
            let ps: Vec<&DamageDraw> = pd.iter().filter(|d| hit(d, fx, fy, fw, fh)).collect();
            let cs: Vec<&DamageDraw> = cd.iter().filter(|d| hit(d, fx, fy, fw, fh)).collect();
            if ps != cs {
                out.push((rx, ry, rw, rh));
            }
        }
    }
    Some(out)
}

/// Copy a freshly-rastered tile into the full frame buffer at `(tx, ty)` (both unpremultiplied RGBA8).
fn blit_tile(buf: &mut spec::Rgba8, tile: &spec::Rgba8, tx: u32, ty: u32, full_w: u32) {
    let fw = full_w as usize;
    let tw = tile.width as usize;
    let th = tile.height as usize;
    let row = tw * 4;
    for r in 0..th {
        let dst = ((ty as usize + r) * fw + tx as usize) * 4;
        let src = r * row;
        if dst + row <= buf.data.len() && src + row <= tile.data.len() {
            buf.data[dst..dst + row].copy_from_slice(&tile.data[src..src + row]);
        }
    }
}

pub fn render_document_rgba(scope: &mut v8::PinScope) -> Option<spec::Rgba8> {
    // `KORVUS_RENDER_DBG`: each phase's time on stderr
    let dbg = std::env::var_os("KORVUS_RENDER_DBG").is_some();
    let dbg_t0 = std::time::Instant::now();
    // the UI language, the language text is shaped and falls back in where the document names none
    let ui_lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Gather Page+Dom+Media+LaidOut, build the style display list, and collect the <img> boxes — all
    // under one borrow, which drops before the <img> pass (`img_state` re-borrows op_state itself).
    let (mut ops, w, h, replaced, lcp) = {
        let mut st = op_state.borrow_mut();
        crate::core::dom::geometry_queries::refresh_layout_cache(&mut st);
        let p = page(&st)?;
        let media = media_context(&st);
        let dom = st.try_borrow::<HostDom>()?;
        let laid_out = st
            .try_borrow::<crate::core::dom::geometry_queries::LayoutCache>()
            .and_then(|c| c.0.as_ref())
            .map(|(_, out)| out)?;
        // C2 (plugin ABI 3 wave 1): a plugin may force the painted surface size with `host->set_viewport`
        // (`plugin_host::viewport_override`); otherwise it is the media viewport (the persona screen).
        let (w, h) = match plugin_host::viewport_override() {
            Some((vw, vh, _dpr)) => (vw.max(1), vh.max(1)),
            None => (media.viewport_width.max(1.0) as u32, media.viewport_height.max(1.0) as u32),
        };
        let t_layout = dbg_t0.elapsed();
        let (ops, replaced, lcp) = page::paint::paint_display_list(&p, &dom.0, &media, laid_out, w as f64, h as f64);
        if dbg {
            eprintln!("frame: layout {:?}, display list {:?} ({} ops, {} replaced)", t_layout, dbg_t0.elapsed() - t_layout, ops.len(), replaced.len());
        }
        // The page's text as Chrome draws it: its family list matched as Chrome's font service matches it, shaped in
        // the document's language (the UI's where `<html lang>` says none) -- the path canvas `fillText` takes when it
        // knows them. Drawn from the bare family name, every page's Arial was fontconfig's default face (Chrome draws
        // Liberation Sans, Arial's metric twin, which is what the layout measured).
        (ops, w, h, replaced, lcp)
    };
    let dbg_t1 = std::time::Instant::now();
    // The largest-contentful-paint winner — start from the largest TEXT box (paint) and let
    // each LOADED replaced box (img/canvas) below challenge it with its INTRINSIC-clamped area (Brave sizes
    // an image by its intrinsic area, not the CSS box, and ignores an image that has not loaded). The
    // winner is stashed for render_document_frame to emit. (node id, effective area px².)
    let mut race = LcpRace { win: lcp.map(|c| (c.node, c.area)), url: None };
    compose_replaced(scope, &mut ops, replaced, 0, Some(&mut race));
    // after the replaced content, whose form-control text has fonts too
    {
        let st = op_state.borrow();
        if let Some(dom) = st.try_borrow::<HostDom>() {
            complete_fonts(&st, &dom.0, dom.0.root(), &media_context(&st), &ui_lang, &mut ops);
        }
    }
    if let Some((node, area)) = race.win {
        op_state.borrow_mut().put(LcpPending { node, area, url: race.url.unwrap_or_default() });
    }
    if dbg {
        eprintln!("frame: replaced content {:?}", dbg_t1.elapsed());
    }
    let dbg_t2 = std::time::Instant::now();
    // Reuse the last frame. An identical display list reuses it as-is (b.1).
    // Otherwise, if the list is one the tile-damage resolver models, re-raster only the tiles whose
    // draws changed and composite them over the cached frame (b.2); a list it does not model, or one
    // that changed over most of the frame, falls back to a full raster. The result is byte-identical to
    // a full raster of the same ops (same ops, same order, same unpremultiplied readback), so the
    // screenshot path, fixtures and the surface/layer locks are unaffected.
    // A single damage RECT (the bounding box of the damaged tiles) is re-rastered, not each tile: one
    // surface and one readback, and every op runs once — tiling it would re-execute an expensive op (a
    // canvas `DrawImage`) for every tile it touches, which measured SLOWER than a full raster. The
    // bounding box may re-raster a little undamaged area between two damaged regions; that is still
    // byte-correct (identical pixels) and the 60 % threshold falls back to a full raster when it is not
    // a win.
    enum Plan {
        Full,
        Partial(u32, u32, u32, u32, spec::Rgba8),
    }
    let plan = {
        let st = op_state.borrow();
        if let Some(c) = st.try_borrow::<FrameCache>().filter(|c| c.w == w && c.h == h) {
            if c.ops == ops {
                return Some(c.buf.clone());
            }
            match tile_damage(&c.ops, &ops, w, h) {
                Some(tiles) if tiles.is_empty() => return Some(c.buf.clone()),
                Some(tiles) => {
                    let x0 = tiles.iter().map(|t| t.0).min().unwrap_or(0);
                    let y0 = tiles.iter().map(|t| t.1).min().unwrap_or(0);
                    let x1 = tiles.iter().map(|t| t.0 + t.2).max().unwrap_or(0);
                    let y1 = tiles.iter().map(|t| t.1 + t.3).max().unwrap_or(0);
                    let (bx, by, bw, bh) = (x0, y0, x1 - x0, y1 - y0);
                    if (bw as u64) * (bh as u64) * 100 < (w as u64) * (h as u64) * 60 {
                        Plan::Partial(bx, by, bw, bh, c.buf.clone())
                    } else {
                        Plan::Full
                    }
                }
                None => Plan::Full,
            }
        } else {
            Plan::Full
        }
    };
    let render = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>()
            .and_then(|c| c.get::<spec::RenderHandle>())
            .map(|h| h.0.clone())?
    };
    let params = spec::RenderParams { device_pixel_ratio: 1.0, document: true, ..Default::default() };
    let buf = match plan {
        Plan::Partial(bx, by, bw, bh, mut buf) => {
            if dbg {
                eprintln!("RC2.b.2: re-rastered {bw}x{bh} at ({bx},{by}) = {}% of frame", (bw as u64) * (bh as u64) * 100 / ((w as u64) * (h as u64)).max(1));
            }
            let region = render.raster_2d_region(&ops, w, h, bx, by, bw, bh, &params);
            blit_tile(&mut buf, &region, bx, by, w);
            buf
        }
        Plan::Full => render.raster_2d(&ops, w, h, &params),
    };
    if dbg {
        eprintln!("frame: raster {:?}", dbg_t2.elapsed());
    }
    let gen = op_state.borrow().try_borrow::<FrameCache>().map_or(1, |c| c.gen + 1);
    op_state.borrow_mut().put(FrameCache { w, h, ops: ops.clone(), buf: buf.clone(), gen });
    Some(buf)
}

/// THE FRAME TAP (plugin ABI 3, C1, the D29 streaming core): paint the document and hand the frame to every
/// frame-wanting plugin's `on_frame` -- only when its pixels were drawn anew since the frame they were last handed.
///
/// It used to hand every rendering opportunity's frame on, changed or not, and `korvus-cdp` kept a frame-wanted page
/// from ever settling so there would be opportunities to hand on: a static page was painted ~130 times a second,
/// the same frame each time, and the busy loop never yielded to the task that reads the DevTools socket, so CDP
/// stopped answering (measured 2026-10-22, D72). A frame is now a change, as Chrome's screencast sends one per
/// repaint: the page settles when nothing moves, and the frames come from the two places a picture changes -- the
/// page's own frame, right after its animation-frame callbacks (`rendering::cb_frame`), and the runtime's
/// rendering opportunity, which the pump offers at each vsync while the page runs (`Engine::pump_slice`).
///
/// `true` when it painted nothing because the last frame was painted less than a display refresh ago: a frame is then
/// owed at the next vsync, and the caller sees that one comes (the runtime keeps the page running until it offers
/// one; the page's own frame asks for the next).
pub fn deliver_frame(scope: &mut v8::PinScope) -> bool {
    // At most one frame per display refresh: the runtime offers a rendering opportunity between vsyncs too (after a
    // task, at a slice's start), and a picture changing at each of them -- an animation -- went out at 80 frames/s on
    // a 60 Hz persona.
    let now = crate::core::timing::performance::clock_now_ms(scope);
    let interval = crate::core::intersection_observer::rendering::frame_interval_ms(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let last = op_state.borrow().try_borrow::<FramePaintedAt>().map(|t| t.0);
    if last.is_some_and(|t| now - t < interval * 0.9) {
        return true;
    }
    op_state.borrow_mut().put(FramePaintedAt(now));
    // A frame is an observation, as a browser's is: the animations are sampled at its moment, so one that moves only by
    // CSS -- an `@keyframes`, a `transition` -- reaches the frame (D77). And while one runs, the next frame is asked for
    // as an animation-frame callback asks for it: the page keeps painting at the display's rate, as Chrome's compositor
    // does, rather than settling with the animation still under way.
    crate::core::animation::css::tick(scope);
    if crate::core::animation::css::running(scope) || crate::core::html::media_load::video_playing(scope) {
        crate::core::intersection_observer::rendering::request_frame(scope);
    }
    render_document_frame(scope, true);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let gen = op_state.borrow().try_borrow::<FrameCache>().map(|c| c.gen);
    let Some(gen) = gen else { return false };
    if op_state.borrow().try_borrow::<FrameDelivered>().is_some_and(|d| d.0 == gen) {
        return false;
    }
    op_state.borrow_mut().put(FrameDelivered(gen));
    // `t_us`: the frame's timestamp in microseconds from the page's start (the page clock, performance.now()).
    let t_us = crate::core::timing::performance::clock_now_ms(scope) * 1000.0;
    let st = op_state.borrow();
    if let Some(c) = st.try_borrow::<FrameCache>() {
        if plugin_host::plugins_want_frames() {
            plugin_host::on_frame(c.w, c.h, &c.buf.data, t_us);
        }
        // the engine's own sink (a CDP screencast) also reads where the document was scrolled to
        let scroll = crate::core::html::window_geometry::scroll_position(&st);
        plugin_host::sink_frame(&plugin_host::SinkFrame { width: c.w, height: c.h, rgba: &c.buf.data, t_us, scroll });
    }
    false
}

/// Forget which frame the tap last handed on, so the next rendering opportunity hands on the picture already on
/// screen: a screencast that starts on a still page starts with its first frame, as Chrome's does.
pub fn offer_frame_again(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let _ = op_state.borrow_mut().try_take::<FrameDelivered>();
}

/// Fill in each `SetFont` of a document's display list as canvas `fillText` gives it when it knows them: the family
/// list (matched as Chrome's font service matches it), the document's language (`<html lang>`, else the UI's) and the
/// default family. Drawn from the bare family name, every page's Arial was fontconfig's default face (Chrome draws
/// Liberation Sans, Arial's metric twin, which is what the layout measured).
///
/// A family the document's `@font-face` rules declare is the fonts they downloaded ([`with_web_fonts`]).
fn complete_fonts(st: &OpState, dom: &Dom, root: u32, media: &MediaContext, ui_lang: &str, ops: &mut [spec::Canvas2DOp]) {
    let lang = crate::core::dom::document::doc_element_child(dom, root)
        .and_then(|e| dom.get_attribute(e, "lang"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or(ui_lang)
        .to_string();
    let default_family = st
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<spec::CssHandle>())
        .map(|c| c.0.default_family(media.theme.font_map))
        .unwrap_or_else(|| "sans-serif".into());
    let web = crate::core::css::sheet_loads::WebFaces::of(st, media);
    let ctx = st.try_borrow::<spec::kernel::Context>();
    let bodies = ctx.and_then(|c| c.get::<crate::core::fonts::font_probe::FontProbes>());
    let render = ctx.and_then(|c| c.get::<spec::RenderHandle>());
    let mut lists: std::collections::HashMap<(String, u16, bool, u32), String> = std::collections::HashMap::new();
    for op in ops {
        if let spec::Canvas2DOp::SetFont { family, families, lang: l, default_family: d, weight, italic, size_px, .. } = op {
            if families.is_empty() {
                families.clone_from(family);
            }
            if let (Some(web), Some(bodies), Some(render)) = (&web, &bodies, &render) {
                let size = page::font::optical_size(*size_px as f32);
                let key = (std::mem::take(families), *weight, *italic, size.to_bits());
                *families = lists.entry(key).or_insert_with_key(|(list, w, i, _)| with_web_fonts(list, *w, *i, size, web, bodies, &*render.0)).clone();
            }
            if l.is_empty() {
                l.clone_from(&lang);
            }
            if d.is_empty() {
                d.clone_from(&default_family);
            }
        }
    }
}

/// A `font-family` list with each family an `@font-face` rule declares replaced by the fonts that face's text draws in
/// -- the best match for `weight` and style in each `unicode-range` subset ([`WebFaces::urls`]) -- under the family
/// names the render backend registered their downloaded files by. A declared family is its faces alone: no installed
/// font answers to its name, and a face whose font has not arrived draws nothing yet (its text falls to the next family).
fn with_web_fonts(
    families: &str,
    weight: u16,
    italic: bool,
    size_px: f32,
    web: &crate::core::css::sheet_loads::WebFaces,
    bodies: &crate::core::fonts::font_probe::FontProbes,
    render: &dyn spec::RenderBackend,
) -> String {
    let mut out: Vec<String> = Vec::new();
    for entry in families.split(',') {
        let name = entry.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_ascii_lowercase();
        if !web.declares(&name) {
            out.push(entry.trim().to_string());
            continue;
        }
        // each under its instance at the weight and size the face draws this text at (a variable font's `wght` and
        // `opsz` there; a font without the axes is itself along them)
        for (url, range) in web.urls(&name, weight as f32, italic) {
            let at = page::font::instance_weight(range, weight as f32);
            out.extend(bodies.body(&url).and_then(|body| render.web_font(&url, &body)).map(|n| page::font::instance_name(&n, at, size_px)));
        }
    }
    out.join(", ")
}

/// The page's largest-contentful-paint race among its replaced boxes ([`compose_replaced`]): the winner so far
/// (node, effective area) and, for an image, its URL.
struct LcpRace {
    win: Option<(u32, f64)>,
    url: Option<String>,
}

/// Replaced content into a document's display list: source each box's pixels ([`replaced_pixels`]) and composite
/// them with `DrawImage` over the box; a background image layer is its own ops. Anything that can't produce pixels
/// draws nothing, as in a real browser. `depth`: how many frame documents deep this document is. `lcp`: the page's
/// own race -- a frame document's images are not the page's candidates.
///
/// Each image goes in at its place in the paint order (`ReplacedBox::at`), inside the transforms, fades and clips
/// around it (D77): inserted from the last, so the earlier places stay where they are. Boxes at one place go in
/// last-painted first, so each earlier one lands before it: a `::before` and `::after` that are only background
/// images share a place, and Naver's service icon (the `::after`) went under its frame (the `::before`).
fn compose_replaced(scope: &mut v8::PinScope, ops: &mut Vec<spec::Canvas2DOp>, replaced: Vec<page::paint::ReplacedBox>, depth: u32, mut lcp: Option<&mut LcpRace>) {
    let mut replaced: Vec<(usize, page::paint::ReplacedBox)> = replaced.into_iter().enumerate().collect();
    replaced.sort_by_key(|(i, b)| std::cmp::Reverse((b.at, *i)));
    let mut patterns = 0u32;
    for (_, b) in replaced {
        // a background image layer: its own ops, in its place
        if let page::paint::ReplacedKind::Background(layer) = &b.kind {
            let layer_ops = crate::core::css::background::layer_ops(scope, b.node, layer, (b.x, b.y, b.w, b.h), &mut patterns);
            let at = b.at.min(ops.len());
            ops.splice(at..at, layer_ops);
            continue;
        }
        // a form control's text, likewise
        if let page::paint::ReplacedKind::Control(text) = &b.kind {
            let text_ops = control_ops(scope, b.node, text, (b.x, b.y, b.w, b.h));
            let at = b.at.min(ops.len());
            ops.splice(at..at, text_ops);
            continue;
        }
        let Some((image, sw, sh)) = replaced_pixels(scope, &b, depth) else { continue };
        if let Some(race) = lcp.as_deref_mut() {
            // A LOADED replaced box is an LCP candidate — its effective size is the displayed
            // box area clamped to the intrinsic (source) area, as Brave reports. A frame is not one.
            let eff = (b.w * b.h).min((sw as f64) * (sh as f64));
            if !matches!(b.kind, page::paint::ReplacedKind::Frame) && eff > 0.0 && race.win.map_or(true, |(_, a)| eff > a) {
                race.win = Some((b.node, eff));
                // RENDER #6: the winner's URL — None (-> "") for a text winner, the resolved image src for an
                // image winner, as Brave reports in the LCP entry's `url`.
                race.url = match b.kind {
                    page::paint::ReplacedKind::Img => crate::core::html::canvas_image::img_url(scope, b.node),
                    _ => None,
                };
            }
        }
        // placed by its `object-fit` and `object-position`, at its natural size (an image's over its `srcset`
        // density), and cut to the box: the part of the image that shows, drawn where it shows
        let natural = match b.kind {
            page::paint::ReplacedKind::Img => crate::core::html::canvas_image::img_layout_size(scope, b.node).map(|(w, h)| (w as f64, h as f64)),
            page::paint::ReplacedKind::Canvas | page::paint::ReplacedKind::Video => Some((sw as f64, sh as f64)),
            _ => None,
        };
        let (fx, fy, fw, fh) = natural.map(|n| b.fit.place(n, (b.w, b.h))).unwrap_or((0.0, 0.0, b.w, b.h));
        // on whole pixels, as Blink snaps an image's destination rect (`ToPixelSnappedRect`) -- unless it fills the
        // box, which is drawn where layout put it
        let snap = |v: f64| if natural.is_some() && b.fit.fit != "fill" { v.round() } else { v };
        let (px0, py0, px1, py1) = (snap(b.x + fx), snap(b.y + fy), snap(b.x + fx + fw), snap(b.y + fy + fh));
        let (x0, y0, x1, y1) = (px0.max(b.x), py0.max(b.y), px1.min(b.x + b.w), py1.min(b.y + b.h));
        if !(x1 > x0 && y1 > y0 && px1 > px0 && py1 > py0) {
            continue;
        }
        let (kx, ky) = (sw as f64 / (px1 - px0), sh as f64 / (py1 - py0));
        ops.insert(b.at.min(ops.len()), spec::Canvas2DOp::DrawImage {
            image,
            sx: (x0 - px0) * kx,
            sy: (y0 - py0) * ky,
            sw: (x1 - x0) * kx,
            sh: (y1 - y0) * ky,
            dx: x0,
            dy: y0,
            dw: x1 - x0,
            dh: y1 - y0,
        });
    }
}

/// A replaced box's pixels and their size: an `<img>`'s decoded bytes via `img_state` (src + CORS/taint + decode), a
/// `<canvas>`'s rastered contents via its JS wrapper (`node_object` -> `canvas_rgba`), an inline `<svg>` rasterized,
/// a fed `<video>` frame, a frame's document painted.
fn replaced_pixels(scope: &mut v8::PinScope, b: &page::paint::ReplacedBox, depth: u32) -> Option<(spec::Rgba8, u32, u32)> {
    match b.kind {
        // their own ops ([`compose_replaced`])
        page::paint::ReplacedKind::Background(_) | page::paint::ReplacedKind::Control(_) => None,
        // an inline `<svg>`, rasterized at its box's size (cached while its markup and size stay)
        page::paint::ReplacedKind::Svg(ref markup) => crate::core::css::background::inline_svg(scope, markup, b.w, b.h).map(|r| {
            let (w, h) = (r.width, r.height);
            ((*r).clone(), w, h)
        }),
        page::paint::ReplacedKind::Img => match crate::core::html::canvas_image::img_state(scope, b.node) {
            crate::core::html::canvas_image::ImgState::Ready(img, _clean, _complete) => Some(((*img).clone(), img.width, img.height)),
            _ => None,
        },
        page::paint::ReplacedKind::Canvas => v8::Local::<v8::Object>::try_from(crate::page::node_object(scope, b.node))
            .ok()
            .and_then(|obj| crate::core::html::html_canvas_element::canvas_rgba(scope, obj)),
        page::paint::ReplacedKind::Frame => frame_pixels(scope, b.node, depth),
        page::paint::ReplacedKind::Video => {
            // A decoded picture (`video_pipe.rs`) -- the poster instead while the element has not played or sought and
            // has one, as HTML's show poster flag has it.
            if let Some(frame) = crate::core::html::media_load::video_frame(scope, b.node) {
                if crate::core::html::media_load::shows_poster(scope, b.node) {
                    if let Some(poster) = video_poster(scope, b.node) {
                        return Some(poster);
                    }
                }
                let (w, h) = (frame.width, frame.height);
                return Some(((*frame).clone(), w, h));
            }
            // A plugin-fed synthetic track (ABI 3, fake-media). Cheap gate first: nothing is fed on a
            // no-plugin / stealth-only run, so this returns None before any v8 traversal and the <video>
            // paints nothing new -- `ops` stays byte-identical (I9/J2). With a live fed frame, draw it like a
            // canvas/img bitmap through the same DrawImage path ([`compose_replaced`]).
            // No frame to show -- a format with no decoder here, or none decoded yet -- and the element shows its
            // `poster`, as Chrome shows it until a frame is ready: Wikipedia's video thumbnails were blank boxes.
            if !plugin_host::has_fed_frames() {
                video_poster(scope, b.node)
            } else {
                // Fetch the fed frame once (it owns its RGBA; a second fed_video_frame_for call would re-clone).
                let frame = crate::core::media::media_stream::fed_video_frame_for(scope, b.node);
                if let Some(f) = &frame {
                    // fake-media SESSION 2b-events: the FIRST paint that sees a fed frame for this <video> fires
                    // its getUserMedia media events (loadedmetadata/resize/loadeddata/canplay/canplaythrough via
                    // the existing set_ready table) and advances readyState to HAVE_ENOUGH_DATA -- ONCE per
                    // element (a srcObject <video> has no Media entry, so read() is None -> HAVE_NOTHING -> this
                    // fires; set_ready advances it synchronously, so every later paint/tick skips). The events
                    // are QUEUED as tasks, never dispatched during paint, so no page JS runs here (no
                    // re-entrancy). Gated on a PER-ELEMENT fed frame, so a no-plugin / no-fed / resource <video>
                    // fires nothing and paints identically (I9/J2); fed_metadata never touches src/DrawImage.
                    let ready = crate::core::html::media_load::read(scope, b.node, |m| m.ready)
                        .unwrap_or(crate::core::html::media_load::HAVE_NOTHING);
                    if ready < crate::core::html::media_load::HAVE_METADATA {
                        crate::core::html::media_load::fed_metadata(scope, b.node, f.width, f.height);
                    }
                }
                frame.map(|f| (spec::Rgba8 { width: f.width, height: f.height, data: f.rgba }, f.width, f.height)).or_else(|| video_poster(scope, b.node))
            }
        }
    }
}

/// `<video>` node `node`'s `poster` image, once the element's loader has fetched it -- as an image, `cors` under the
/// element's `crossorigin`, as the loader asks for it.
fn video_poster(scope: &mut v8::PinScope, node: u32) -> Option<(spec::Rgba8, u32, u32)> {
    let (poster, cors) = crate::page::with_host_dom(scope, |d| (d.get_attribute(node, "poster").map(str::to_string), d.get_attribute(node, "crossorigin").is_some()))?;
    let poster = poster?;
    if poster.trim().is_empty() {
        return None;
    }
    let base = crate::core::html::subresource::node_base_url(scope, node);
    let url = url::Url::parse(&base).ok()?.join(poster.trim()).ok()?.to_string();
    let image = crate::core::css::background::already_fetched(scope, &url, if cors { "cors" } else { "no-cors" })?;
    let (w, h) = (image.width, image.height);
    Some(((*image).clone(), w, h))
}

/// A form control's own text, drawn in its content box `(x, y, w, h)` and clipped to it: an `<input>`'s live value (as
/// bullets in a password field) or, while it is empty, its placeholder; a `<textarea>`'s placeholder while it is empty;
/// a button input's label, centred; a drop-down `<select>`'s selected option. Typed text never showed: an `<input>`'s
/// value is not laid out as text, and nothing drew it.
fn control_ops(scope: &mut v8::PinScope, node: u32, t: &page::paint::ControlText, (x, y, w, h): (f64, f64, f64, f64)) -> Vec<spec::Canvas2DOp> {
    use page::paint::ControlRole;
    let attr = |scope: &mut v8::PinScope, name: &str| -> Option<String> {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|d| d.0.get_attribute(node, name).map(str::to_string))
    };
    let live = |scope: &mut v8::PinScope, member: &str| -> String {
        let obj = crate::page::node_object(scope, node);
        crate::binding::protos::pristine_get(scope, obj, member).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
    };
    let one_line = |s: &str| s.replace(['\n', '\r'], "");
    if t.role == ControlRole::Range {
        let num = |v: Option<String>| v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let min = num(attr(scope, "min")).unwrap_or(0.0);
        let max = num(attr(scope, "max")).filter(|&m| m >= min).unwrap_or(min.max(100.0));
        let value = num(Some(live(scope, "HTMLInputElement.value"))).unwrap_or((min + max) / 2.0).clamp(min, max);
        let fraction = if max > min { (value - min) / (max - min) } else { 0.0 };
        let disabled = live(scope, "HTMLInputElement.disabled") == "true";
        return range_ops(t, (x, y, w, h), fraction, disabled);
    }
    if t.role == ControlRole::File {
        // the button (Chrome's light theme button: a `#efefef` face in a `#767676` edge, its label centred), then the
        // no-file text 4px after it, both on the line's baseline, cut at the box
        let theme = media_context(&deno_core::JsRuntime::op_state_from(scope).borrow()).theme;
        let (button, none) = page::layout::replaced::file_labels(&theme, attr(scope, "multiple").is_some());
        let bw = control_text_width(scope, t, button) + 16.0;
        if w <= 0.0 || h <= 0.0 {
            return Vec::new();
        }
        // the text's `text-overflow: ellipsis`: cut where a "…" still fits in what the button leaves
        let room = w - bw - 4.0;
        let mut none = none.to_string();
        if control_text_width(scope, t, &none) > room {
            let mut chars: Vec<char> = none.chars().collect();
            while !chars.is_empty() && control_text_width(scope, t, &format!("{}\u{2026}", chars.iter().collect::<String>())) > room {
                chars.pop();
            }
            none = format!("{}\u{2026}", chars.iter().collect::<String>());
        }
        let grey = |v: f32| [v / 255.0, v / 255.0, v / 255.0, 1.0];
        let font = spec::Canvas2DOp::SetFont {
            family: t.family.clone(),
            size_px: t.size_px,
            bold: t.bold,
            italic: t.italic,
            weight: if t.bold { 700 } else { 400 },
            families: String::new(),
            lang: String::new(),
            default_family: String::new(),
        };
        return vec![
            spec::Canvas2DOp::Save,
            spec::Canvas2DOp::BeginPath,
            spec::Canvas2DOp::Rect { x, y, w, h },
            spec::Canvas2DOp::Clip { even_odd: false },
            spec::Canvas2DOp::SetFillStyle { color: grey(118.0) },
            spec::Canvas2DOp::BeginPath,
            spec::Canvas2DOp::RoundRect { x, y, w: bw, h, radii: [(2.0, 2.0); 4] },
            spec::Canvas2DOp::Fill { even_odd: false },
            spec::Canvas2DOp::SetFillStyle { color: grey(239.0) },
            spec::Canvas2DOp::BeginPath,
            spec::Canvas2DOp::RoundRect { x: x + 1.0, y: y + 1.0, w: (bw - 2.0).max(0.0), h: (h - 2.0).max(0.0), radii: [(1.0, 1.0); 4] },
            spec::Canvas2DOp::Fill { even_odd: false },
            font,
            spec::Canvas2DOp::SetTextBaseline { baseline: spec::TextBaseline::Alphabetic },
            spec::Canvas2DOp::SetFillStyle { color: t.color },
            spec::Canvas2DOp::SetTextAlign { align: spec::TextAlign::Center },
            spec::Canvas2DOp::FillText { text: button.to_string(), x: x + bw / 2.0, y: t.baseline, max_width: None },
            spec::Canvas2DOp::SetTextAlign { align: spec::TextAlign::Start },
            spec::Canvas2DOp::FillText { text: none, x: x + bw + 4.0, y: t.baseline, max_width: None },
            spec::Canvas2DOp::Restore,
        ];
    }
    if t.role == ControlRole::DateTime {
        let theme = media_context(&deno_core::JsRuntime::op_state_from(scope).borrow()).theme;
        let kind = attr(scope, "type").unwrap_or_default().trim().to_ascii_lowercase();
        let Some((_, _, placeholder)) = page::layout::replaced::datetime_pattern(&theme, &kind) else { return Vec::new() };
        let value = live(scope, "HTMLInputElement.value");
        let text = datetime_text(&kind, &value, theme.submit_label == "제출").unwrap_or_else(|| placeholder.to_string());
        if w <= 0.0 || h <= 0.0 {
            return Vec::new();
        }
        let time = kind == "time";
        let s = if time { 1.05 * t.size_px } else { t.size_px };
        let (ix, iy) = (x + w - s - if time { 3.0 } else { 2.0 }, y + (h - s) / 2.0);
        let mut ops = vec![
            spec::Canvas2DOp::Save,
            spec::Canvas2DOp::BeginPath,
            spec::Canvas2DOp::Rect { x, y, w, h },
            spec::Canvas2DOp::Clip { even_odd: false },
            spec::Canvas2DOp::SetFont {
                family: t.family.clone(),
                size_px: t.size_px,
                bold: t.bold,
                italic: t.italic,
                weight: if t.bold { 700 } else { 400 },
                families: String::new(),
                lang: String::new(),
                default_family: String::new(),
            },
            spec::Canvas2DOp::SetTextBaseline { baseline: spec::TextBaseline::Alphabetic },
            spec::Canvas2DOp::SetTextAlign { align: spec::TextAlign::Start },
            spec::Canvas2DOp::SetFillStyle { color: t.color },
            spec::Canvas2DOp::FillText { text, x: x + 1.0, y: t.baseline, max_width: None },
            spec::Canvas2DOp::SetStrokeStyle { color: t.color },
            spec::Canvas2DOp::SetLineWidth { width: (s / 12.0).max(1.0) },
            spec::Canvas2DOp::BeginPath,
        ];
        // the picker icon (Blink's `calendar_icon.svg`, `time_icon.svg`): a calendar's frame and binding, a clock's face
        // and hands
        if time {
            let (cx, cy, r) = (ix + s / 2.0, iy + s / 2.0, s * 0.42);
            ops.extend([
                spec::Canvas2DOp::Arc { x: cx, y: cy, radius: r, start: 0.0, end: std::f64::consts::TAU, ccw: false },
                spec::Canvas2DOp::MoveTo { x: cx, y: cy - r * 0.6 },
                spec::Canvas2DOp::LineTo { x: cx, y: cy },
                spec::Canvas2DOp::LineTo { x: cx + r * 0.45, y: cy + r * 0.3 },
            ]);
        } else {
            let (fx, fy, fw, fh) = (ix + s * 0.1, iy + s * 0.15, s * 0.8, s * 0.75);
            ops.extend([
                spec::Canvas2DOp::Rect { x: fx, y: fy, w: fw, h: fh },
                spec::Canvas2DOp::MoveTo { x: fx, y: fy + fh * 0.3 },
                spec::Canvas2DOp::LineTo { x: fx + fw, y: fy + fh * 0.3 },
                spec::Canvas2DOp::MoveTo { x: fx + fw * 0.28, y: iy },
                spec::Canvas2DOp::LineTo { x: fx + fw * 0.28, y: fy + fh * 0.18 },
                spec::Canvas2DOp::MoveTo { x: fx + fw * 0.72, y: iy },
                spec::Canvas2DOp::LineTo { x: fx + fw * 0.72, y: fy + fh * 0.18 },
            ]);
        }
        ops.extend([spec::Canvas2DOp::Stroke, spec::Canvas2DOp::Restore]);
        return ops;
    }
    if t.role == ControlRole::Color {
        // the swatch: `::-webkit-color-swatch-wrapper`'s 4px 2px padding in, edged `#777`, in the live value
        let value = live(scope, "HTMLInputElement.value");
        // a colour input's value is always a valid simple colour, `#rrggbb`
        let byte = |i: usize| value.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
        let colour = spec::legacy_color4(byte(1), byte(3), byte(5), 1.0);
        let (sx, sy, sw, sh) = (x + 2.0, y + 4.0, (w - 4.0).max(0.0), (h - 8.0).max(0.0));
        if sw <= 0.0 || sh <= 0.0 {
            return Vec::new();
        }
        return vec![
            spec::Canvas2DOp::SetFillStyle { color: [119.0 / 255.0, 119.0 / 255.0, 119.0 / 255.0, 1.0] },
            spec::Canvas2DOp::FillRect { x: sx, y: sy, w: sw, h: sh },
            spec::Canvas2DOp::SetFillStyle { color: colour },
            spec::Canvas2DOp::FillRect { x: sx + 1.0, y: sy + 1.0, w: (sw - 2.0).max(0.0), h: (sh - 2.0).max(0.0) },
        ];
    }
    // a checked checkbox or radio button: Chrome's accent fill and its mark
    if matches!(t.role, ControlRole::Check | ControlRole::Radio) {
        if live(scope, "HTMLInputElement.checked") != "true" || w <= 0.0 || h <= 0.0 {
            return Vec::new();
        }
        const ACCENT: [f32; 4] = [0.0, 117.0 / 255.0, 1.0, 1.0];
        const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        let mut ops = vec![spec::Canvas2DOp::SetFillStyle { color: ACCENT }, spec::Canvas2DOp::BeginPath];
        if t.role == ControlRole::Check {
            ops.push(spec::Canvas2DOp::RoundRect { x, y, w, h, radii: [(2.0, 2.0); 4] });
            ops.push(spec::Canvas2DOp::Fill { even_odd: false });
            let (s, ox, oy) = (w.min(h) / 13.0, x, y);
            ops.extend([
                spec::Canvas2DOp::SetStrokeStyle { color: WHITE },
                spec::Canvas2DOp::SetLineWidth { width: 2.0 * s },
                spec::Canvas2DOp::BeginPath,
                spec::Canvas2DOp::MoveTo { x: ox + 3.0 * s, y: oy + 6.5 * s },
                spec::Canvas2DOp::LineTo { x: ox + 5.5 * s, y: oy + 9.0 * s },
                spec::Canvas2DOp::LineTo { x: ox + 10.0 * s, y: oy + 4.0 * s },
                spec::Canvas2DOp::Stroke,
            ]);
        } else {
            let (cx, cy, r) = (x + w / 2.0, y + h / 2.0, w.min(h) / 2.0);
            ops.push(spec::Canvas2DOp::RoundRect { x: cx - r, y: cy - r, w: 2.0 * r, h: 2.0 * r, radii: [(r, r); 4] });
            ops.push(spec::Canvas2DOp::Fill { even_odd: false });
            let ri = r - 1.5;
            ops.extend([
                spec::Canvas2DOp::SetFillStyle { color: WHITE },
                spec::Canvas2DOp::BeginPath,
                spec::Canvas2DOp::RoundRect { x: cx - ri, y: cy - ri, w: 2.0 * ri, h: 2.0 * ri, radii: [(ri, ri); 4] },
                spec::Canvas2DOp::Fill { even_odd: false },
                spec::Canvas2DOp::SetFillStyle { color: ACCENT },
                spec::Canvas2DOp::BeginPath,
                spec::Canvas2DOp::RoundRect { x: cx - r + 3.0, y: cy - r + 3.0, w: 2.0 * (r - 3.0), h: 2.0 * (r - 3.0), radii: [(r - 3.0, r - 3.0); 4] },
                spec::Canvas2DOp::Fill { even_odd: false },
            ]);
        }
        return ops;
    }
    let (text, placeholder) = match t.role {
        ControlRole::Field | ControlRole::Password => {
            let value = live(scope, "HTMLInputElement.value");
            if value.is_empty() {
                (one_line(&attr(scope, "placeholder").unwrap_or_default()), true)
            } else if t.role == ControlRole::Password {
                ("\u{2022}".repeat(value.chars().count()), false)
            } else {
                (value, false)
            }
        }
        ControlRole::Area => {
            let value = live(scope, "HTMLTextAreaElement.value");
            if !value.is_empty() {
                return area_ops(scope, node, t, (x, y, w, h), &value);
            }
            (attr(scope, "placeholder").unwrap_or_default(), true)
        }
        ControlRole::Button => {
            // with no `value`, the browser's localised default label, as the layout sized it
            let theme = media_context(&deno_core::JsRuntime::op_state_from(scope).borrow()).theme;
            let label = attr(scope, "value").unwrap_or_else(|| match attr(scope, "type").map(|t| t.trim().to_ascii_lowercase()).as_deref() {
                Some("submit") => if theme.submit_label.is_empty() { "Submit" } else { theme.submit_label }.into(),
                Some("reset") => if theme.reset_label.is_empty() { "Reset" } else { theme.reset_label }.into(),
                _ => String::new(),
            });
            (label, false)
        }
        ControlRole::Check | ControlRole::Radio | ControlRole::Range | ControlRole::Color | ControlRole::File | ControlRole::DateTime => {
            (String::new(), false)
        }
        ControlRole::Select => {
            let index = live(scope, "HTMLSelectElement.selectedIndex").parse::<i64>().unwrap_or(-1);
            let label = (index >= 0)
                .then(|| {
                    let op_state = deno_core::JsRuntime::op_state_from(scope);
                    let st = op_state.borrow();
                    let dom = &st.try_borrow::<HostDom>()?.0;
                    let option = dom.descendants_matching(node, "option").ok()?.into_iter().nth(index as usize)?;
                    let label = dom.get_attribute(option, "label").filter(|l| !l.is_empty()).map(str::to_string).unwrap_or_else(|| dom.text_content(option));
                    Some(label.split_whitespace().collect::<Vec<_>>().join(" "))
                })
                .flatten()
                .unwrap_or_default();
            (label, false)
        }
    };
    if (text.is_empty() && t.caret.is_none()) || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let centred = t.role == ControlRole::Button;
    let bold = if placeholder { t.placeholder_bold } else { t.bold };
    let mut ops = vec![spec::Canvas2DOp::Save, spec::Canvas2DOp::BeginPath, spec::Canvas2DOp::Rect { x, y, w, h }, spec::Canvas2DOp::Clip { even_odd: false }];
    if !text.is_empty() {
        ops.extend([
            spec::Canvas2DOp::SetFont {
                family: t.family.clone(),
                size_px: t.size_px,
                bold,
                italic: t.italic,
                weight: if bold { 700 } else { 400 },
                families: String::new(),
                lang: String::new(),
                default_family: String::new(),
            },
            spec::Canvas2DOp::SetTextBaseline { baseline: spec::TextBaseline::Alphabetic },
            spec::Canvas2DOp::SetTextAlign { align: if centred { spec::TextAlign::Center } else { spec::TextAlign::Start } },
            spec::Canvas2DOp::SetFillStyle { color: if placeholder { t.placeholder_color } else { t.color } },
            spec::Canvas2DOp::FillText { text: text.clone(), x: if centred { x + w / 2.0 } else { x }, y: t.baseline, max_width: None },
        ]);
    }
    // the caret, 1px wide over the line's ascent and descent, after the text before the selection's start
    if let Some(caret) = t.caret {
        let before = if placeholder || t.role == ControlRole::Area {
            String::new()
        } else {
            let obj = crate::page::node_object(scope, node);
            let start = v8::Local::<v8::Object>::try_from(obj).map_or(0, |o| crate::core::html::html_input_element::text_control_selection(scope, o).0) as usize;
            text.chars().take(start).collect()
        };
        let advance = if before.is_empty() { 0.0 } else { control_text_width(scope, t, &before) };
        ops.extend([
            spec::Canvas2DOp::SetFillStyle { color: caret },
            spec::Canvas2DOp::FillRect { x: (x + advance).round(), y: t.baseline - t.ascent, w: 1.0, h: t.ascent + t.descent },
        ]);
    }
    ops.push(spec::Canvas2DOp::Restore);
    ops
}

/// A date or time input's value as its fields show it in the worn browser's language (Korean: `2026. 10. 23.`,
/// `오후 02:05`, `2026년 10월`, `2026년 43주`; English: `10/23/2026`, `02:05 PM`, `October 2026`, `Week 43, 2026`);
/// `None` for an empty or malformed one, which shows the placeholder.
fn datetime_text(kind: &str, value: &str, korean: bool) -> Option<String> {
    let num = |s: &str| s.parse::<u32>().ok();
    let date = |v: &str| -> Option<String> {
        let mut p = v.split('-');
        let (y, m, d) = (p.next()?, num(p.next()?)?, num(p.next()?)?);
        Some(if korean { format!("{y}. {m:02}. {d:02}.") } else { format!("{m:02}/{d:02}/{y}") })
    };
    let time = |v: &str| -> Option<String> {
        let mut p = v.split(':');
        let (hh, mm) = (num(p.next()?)?, num(p.next()?.get(..2)?)?);
        let (h12, pm) = (if hh % 12 == 0 { 12 } else { hh % 12 }, hh >= 12);
        Some(if korean { format!("{} {h12:02}:{mm:02}", if pm { "오후" } else { "오전" }) } else { format!("{h12:02}:{mm:02} {}", if pm { "PM" } else { "AM" }) })
    };
    if value.is_empty() {
        return None;
    }
    match kind {
        "date" => date(value),
        "time" => time(value),
        "datetime-local" => {
            let (d, t) = value.split_once('T')?;
            Some(if korean { format!("{} {}", date(d)?, time(t)?) } else { format!("{}, {}", date(d)?, time(t)?) })
        }
        "month" => {
            let (y, m) = value.split_once('-')?;
            let m = num(m)?;
            const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
            Some(if korean { format!("{y}년 {m:02}월") } else { format!("{} {y}", MONTHS.get(m.checked_sub(1)? as usize)?) })
        }
        "week" => {
            let (y, w) = value.split_once("-W")?;
            let w = num(w)?;
            Some(if korean { format!("{y}년 {w:02}주") } else { format!("Week {w:02}, {y}") })
        }
        _ => None,
    }
}

/// A range input at `fraction` of its range, in its content box ([`page::paint::SliderLook`]): the theme's track (8px,
/// pill-shaped, `#efefef` edged `#b2b2b2`, the accent up to the thumb's centre) or the track pseudo-element's own
/// background, and the theme's thumb (a 16px accent circle) or the thumb pseudo-element's own box. The thumb's left is
/// the fraction of the room the track leaves it, rounded (Chrome 150: 75.0 for 50 in a 129px track, 184.0 for 20).
fn range_ops(t: &page::paint::ControlText, (x, y, w, h): (f64, f64, f64, f64), fraction: f64, disabled: bool) -> Vec<spec::Canvas2DOp> {
    use spec::Canvas2DOp as Op;
    let Some(look) = &t.slider else { return Vec::new() };
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let grey = |v: f32| [v / 255.0, v / 255.0, v / 255.0, 1.0];
    let accent = if disabled { grey(197.0) } else { t.accent };
    let (tw, th) = look.thumb;
    let left = x + ((w - tw).max(0.0) * fraction).round();
    let mut ops = Vec::new();
    let pill = |ops: &mut Vec<Op>, colour: [f32; 4], (rx, ry, rw, rh): (f64, f64, f64, f64), r: f64| {
        if rw <= 0.0 || rh <= 0.0 {
            return;
        }
        let r = r.min(rw / 2.0).min(rh / 2.0);
        ops.extend([Op::SetFillStyle { color: colour }, Op::BeginPath, Op::RoundRect { x: rx, y: ry, w: rw, h: rh, radii: [(r, r); 4] }, Op::Fill { even_odd: false }]);
    };
    // the track: the theme's, or the pseudo-element's (as tall as the thumb when it sets no height)
    let track_h = if look.native_track { 8.0 } else { look.track_height.unwrap_or(th) };
    let track_y = y + (h - track_h) / 2.0;
    if look.native_track {
        let (tx, tww) = (x + 1.0, (w - 2.0).max(0.0));
        pill(&mut ops, grey(178.0), (tx, track_y, tww, 8.0), 4.0);
        pill(&mut ops, grey(239.0), (tx + 1.0, track_y + 1.0, (tww - 2.0).max(0.0), 6.0), 3.0);
        let centre = left + tw / 2.0;
        ops.extend([Op::Save, Op::BeginPath, Op::Rect { x: tx, y: track_y, w: (centre - tx).max(0.0), h: 8.0 }, Op::Clip { even_odd: false }]);
        pill(&mut ops, accent, (tx, track_y, tww, 8.0), 4.0);
        ops.push(Op::Restore);
    } else if let Some(c) = look.track_color {
        pill(&mut ops, c, (x, track_y, w, track_h), look.track_radius);
    }
    // the thumb: the theme's circle, centred; a styled one at the track's top, moved by its margin
    if look.native_thumb {
        let r = tw / 2.0;
        ops.extend([
            Op::SetFillStyle { color: accent },
            Op::BeginPath,
            Op::RoundRect { x: left, y: y + h / 2.0 - r, w: tw, h: tw, radii: [(r, r); 4] },
            Op::Fill { even_odd: false },
        ]);
    } else if let Some(c) = look.thumb_color {
        pill(&mut ops, c, (left, track_y + look.thumb_margin_top, tw, th), look.thumb_radius);
    }
    ops
}

/// A `<textarea>`'s live value in its content box: its lines, each wrapped at the box's width at its spaces, one line
/// height apart; and the caret where the selection starts. The layout holds the element's text, the default value --
/// typed text never showed.
fn area_ops(scope: &mut v8::PinScope, node: u32, t: &page::paint::ControlText, (x, y, w, h): (f64, f64, f64, f64), value: &str) -> Vec<spec::Canvas2DOp> {
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    // lines as drawn: each `(text, index of its first char in the value)`
    let mut lines: Vec<(String, usize)> = Vec::new();
    let mut at = 0usize;
    for hard in value.split('\n') {
        let mut line = String::new();
        let mut line_start = at;
        for (i, word) in hard.split_inclusive(' ').enumerate() {
            let candidate = format!("{line}{word}");
            if i > 0 && !line.is_empty() && control_text_width(scope, t, candidate.trim_end()) > w {
                let len = line.chars().count();
                lines.push((std::mem::take(&mut line), line_start));
                line_start += len;
                line.push_str(word);
            } else {
                line = candidate;
            }
        }
        let len = line.chars().count();
        lines.push((line, line_start));
        at = line_start + len + 1;
    }
    let step = (t.ascent + t.descent).max(1.0);
    let mut ops = vec![
        spec::Canvas2DOp::Save,
        spec::Canvas2DOp::BeginPath,
        spec::Canvas2DOp::Rect { x, y, w, h },
        spec::Canvas2DOp::Clip { even_odd: false },
        spec::Canvas2DOp::SetFont { family: t.family.clone(), size_px: t.size_px, bold: t.bold, italic: t.italic, weight: if t.bold { 700 } else { 400 }, families: String::new(), lang: String::new(), default_family: String::new() },
        spec::Canvas2DOp::SetTextBaseline { baseline: spec::TextBaseline::Alphabetic },
        spec::Canvas2DOp::SetTextAlign { align: spec::TextAlign::Start },
        spec::Canvas2DOp::SetFillStyle { color: t.color },
    ];
    for (i, (line, _)) in lines.iter().enumerate() {
        if !line.is_empty() {
            ops.push(spec::Canvas2DOp::FillText { text: line.clone(), x, y: t.baseline + i as f64 * step, max_width: None });
        }
    }
    if let Some(caret) = t.caret {
        let obj = crate::page::node_object(scope, node);
        let start = v8::Local::<v8::Object>::try_from(obj).map_or(0, |o| crate::core::html::html_input_element::text_control_selection(scope, o).0) as usize;
        let row = lines.iter().rposition(|(_, s)| *s <= start).unwrap_or(0);
        let (line, line_start) = &lines[row];
        let before: String = line.chars().take(start.saturating_sub(*line_start)).collect();
        let advance = if before.is_empty() { 0.0 } else { control_text_width(scope, t, &before) };
        let baseline = t.baseline + row as f64 * step;
        ops.push(spec::Canvas2DOp::SetFillStyle { color: caret });
        ops.push(spec::Canvas2DOp::FillRect { x: (x + advance).round(), y: baseline - t.ascent, w: 1.0, h: t.ascent + t.descent });
    }
    ops.push(spec::Canvas2DOp::Restore);
    ops
}

/// How wide `text` is drawn in a control's font: shaped as its text is drawn (the backend's `text_ink`).
fn control_text_width(scope: &mut v8::PinScope, t: &page::paint::ControlText, text: &str) -> f64 {
    let lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let font_map = media_context(&st).theme.font_map;
    let ctx = st.try_borrow::<spec::kernel::Context>();
    let default_family = ctx.and_then(|c| c.get::<spec::CssHandle>()).map(|c| c.0.default_family(font_map)).unwrap_or_else(|| "sans-serif".into());
    let request = spec::TextFontRequest { families: t.family.clone(), default_family, size: t.size_px as f32, bold: t.bold, italic: t.italic, lang, words: false };
    ctx.and_then(|c| c.get::<spec::RenderHandle>()).and_then(|r| r.0.text_ink(&request, text)).map_or(0.0, |i| i.width as f64)
}

/// The last raster of each frame document ([`frame_pixels`]), by its arena root: its size, the display list it was
/// drawn from, and its pixels -- a frame that did not change is not drawn again.
#[derive(Default)]
struct FrameRasters(std::collections::HashMap<u32, (u32, u32, Vec<spec::Canvas2DOp>, spec::Rgba8)>);

/// How many frame documents deep a frame's document is painted: Chrome paints every level, this the first four.
const FRAME_DEPTH: u32 = 4;

/// The document frame element `el` shows, painted at its viewport (the element's content box): laid out
/// ([`refresh_frame`](crate::core::dom::geometry_queries::refresh_frame)), its display list built and its own replaced
/// content composed, then rastered. `None` for a frame with no document here (an out-of-process one, `KORVUS_OOPIF`)
/// or no size. Frames were never painted: Naver's banner, minea's dashboard and every challenge widget were white.
fn frame_pixels(scope: &mut v8::PinScope, el: u32, depth: u32) -> Option<(spec::Rgba8, u32, u32)> {
    if depth >= FRAME_DEPTH {
        return None;
    }
    let ui_lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (mut ops, replaced, w, h, root) = {
        let mut st = op_state.borrow_mut();
        let root = crate::core::html::browsing_context::frame_content_root(&st, el)?;
        if !crate::core::dom::geometry_queries::refresh_frame(&mut st, root) {
            return None;
        }
        let media = crate::core::dom::geometry_queries::frame_media(&mut st, root)?;
        let (w, h) = (media.viewport_width.round() as u32, media.viewport_height.round() as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let p = page(&st)?;
        let dom = st.try_borrow::<HostDom>()?;
        let laid_out = crate::core::dom::geometry_queries::frame_laid_out(&st, root)?;
        let (ops, replaced, _) = page::paint::paint_display_list(&p, &dom.0, &media, laid_out, w as f64, h as f64);
        (ops, replaced, w, h, root)
    };
    compose_replaced(scope, &mut ops, replaced, depth + 1, None);
    {
        let st = op_state.borrow();
        if let Some(dom) = st.try_borrow::<HostDom>() {
            complete_fonts(&st, &dom.0, root, &media_context(&st), &ui_lang, &mut ops);
        }
    }
    if let Some((cw, ch, cops, buf)) = op_state.borrow().try_borrow::<FrameRasters>().and_then(|r| r.0.get(&root)) {
        if (*cw, *ch) == (w, h) && *cops == ops {
            return Some((buf.clone(), w, h));
        }
    }
    let render = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())?
    };
    let params = spec::RenderParams { device_pixel_ratio: 1.0, ..Default::default() };
    let buf = render.raster_2d(&ops, w, h, &params);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<FrameRasters>().is_none() {
        st.put(FrameRasters::default());
    }
    st.borrow_mut::<FrameRasters>().0.insert(root, (w, h, ops, buf.clone()));
    Some((buf, w, h))
}

/// A popup this process shows ([`crate::core::html::browsing_context::devtools_popups`]), painted as a page -- its
/// document at its window's size, on white -- and encoded as `format`: `Page.captureScreenshot` on its target.
pub fn render_popup_shot(scope: &mut v8::PinScope, root: u32, format: spec::ImageFormat, quality: Option<f64>) -> Option<Vec<u8>> {
    crate::core::animation::css::tick(scope);
    let ui_lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (mut ops, replaced, w, h) = {
        let mut st = op_state.borrow_mut();
        if !crate::core::dom::geometry_queries::refresh_frame(&mut st, root) {
            return None;
        }
        let media = media_context(&st);
        let (w, h) = (media.viewport_width.max(1.0) as u32, media.viewport_height.max(1.0) as u32);
        let p = page(&st)?;
        let dom = st.try_borrow::<HostDom>()?;
        let laid_out = crate::core::dom::geometry_queries::frame_laid_out(&st, root)?;
        let (ops, replaced, _) = page::paint::paint_display_list(&p, &dom.0, &media, laid_out, w as f64, h as f64);
        (ops, replaced, w, h)
    };
    compose_replaced(scope, &mut ops, replaced, 1, None);
    {
        let st = op_state.borrow();
        if let Some(dom) = st.try_borrow::<HostDom>() {
            complete_fonts(&st, &dom.0, root, &media_context(&st), &ui_lang, &mut ops);
        }
    }
    let render = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())?
    };
    let params = spec::RenderParams { device_pixel_ratio: 1.0, ..Default::default() };
    let mut buf = render.raster_2d(&ops, w, h, &params);
    for px in buf.data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            }
            px[3] = 255;
        }
    }
    render.encode_image(&buf, format, quality)
}

/// Render the document to an encoded PNG — the `korvus fetch --screenshot` path.
pub fn render_document_png(scope: &mut v8::PinScope) -> Option<Vec<u8>> {
    // a screenshot is an observation too: the animations at its moment, not where the last frame left them
    crate::core::animation::css::tick(scope);
    let mut buf = render_document_rgba(scope)?;
    // On Chrome's base background, white: what the page leaves transparent is white in its screenshots (the CDP
    // screencast lays its frames on white too), where this answered black.
    for px in buf.data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            }
            px[3] = 255;
        }
    }
    let render = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>()
            .and_then(|c| c.get::<spec::RenderHandle>())
            .map(|h| h.0.clone())
    }?;
    render.encode_image(&buf, spec::ImageFormat::Png, None)
}

/// `Page.captureScreenshot`'s picture (the CDP layer's): the document painted, the region `clip` names (`[x, y, width,
/// height, scale]`, DOCUMENT coordinates in CSS px, as Chrome's `Viewport` is) cut out of it and scaled, encoded as
/// `format`.
///
/// `beyond` is `captureBeyondViewport`: a clip outside the viewport is painted by laying the document out in a
/// viewport that holds it, scrolled to its origin, for the one picture -- which is how a full-page screenshot is taken
/// (Playwright asks for the whole content size with it), and Chrome's emulation does the same. The viewport and the
/// scroll position are put back after.
///
/// What the page leaves transparent is laid on Chrome's white, or on a DevTools client's background colour
/// (`Emulation.setDefaultBackgroundColorOverride`); a colour with no alpha leaves it transparent.
pub fn render_document_shot(
    scope: &mut v8::PinScope,
    format: spec::ImageFormat,
    quality: Option<f64>,
    clip: Option<[f64; 5]>,
    beyond: bool,
) -> Option<Vec<u8>> {
    crate::core::animation::css::tick(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (vw, vh, sx, sy) = {
        let st = op_state.borrow();
        let m = media_context(&st);
        let (sx, sy) = crate::core::html::window_geometry::scroll_position(&st);
        (m.viewport_width as f64, m.viewport_height as f64, sx, sy)
    };
    let outside = clip.is_some_and(|c| c[0] < sx || c[1] < sy || c[0] + c[2] > sx + vw + 0.5 || c[1] + c[3] > sy + vh + 0.5);
    let grow = beyond && outside;
    let saved = spec::devtools::device_metrics();
    if let (true, Some(c)) = (grow, clip) {
        let mut m = saved.unwrap_or_default();
        m.width = vw.max((c[0] + c[2]).ceil());
        m.height = vh.max((c[1] + c[3]).ceil());
        spec::devtools::set_device_metrics(Some(m));
        op_state.borrow_mut().put(crate::core::html::window_geometry::ScrollPosition { x: 0.0, y: 0.0 });
    }
    let painted = render_document_rgba(scope);
    if grow {
        spec::devtools::set_device_metrics(saved);
        op_state.borrow_mut().put(crate::core::html::window_geometry::ScrollPosition { x: sx, y: sy });
    }
    let mut buf = painted?;
    // the picture's origin, in document coordinates
    let (ox, oy) = if grow { (0.0, 0.0) } else { (sx, sy) };
    if let Some(c) = clip {
        buf = crop_rgba(&buf, c[0] - ox, c[1] - oy, c[2], c[3]);
        if c[4] > 0.0 && (c[4] - 1.0).abs() > 1e-6 {
            buf = scale_rgba(&buf, c[4]);
        }
    }
    let under = spec::devtools::background().unwrap_or([255, 255, 255, 255]);
    if under[3] > 0 {
        for px in buf.data.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a < 255 {
                for (i, c) in px[..3].iter_mut().enumerate() {
                    *c = ((*c as u32 * a + under[i] as u32 * (255 - a) + 127) / 255) as u8;
                }
                px[3] = 255;
            }
        }
    }
    let render = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    }?;
    render.encode_image(&buf, format, quality)
}

/// What `Page.printToPDF` asks for: the paper and margins in inches, the scale, landscape, and the page ranges
/// (`"1-3, 5"`; empty is every page).
#[derive(Debug, Clone)]
pub struct PrintOptions {
    pub paper_width_in: f64,
    pub paper_height_in: f64,
    /// top, right, bottom, left
    pub margins_in: [f64; 4],
    pub scale: f64,
    pub landscape: bool,
    pub page_ranges: String,
}

/// Print the document as Chrome's `Page.printToPDF` does: laid out under `print` media at the paper's content width
/// (the paper less its margins, in CSS px at the scale asked), cut into pages of the content's height, and drawn by
/// the render backend into a PDF ([`spec::RenderBackend::print_pdf`]). `Err` carries Chrome's refusal of a page range
/// it cannot read or that names no page of the document.
pub fn render_document_pdf(scope: &mut v8::PinScope, opts: &PrintOptions) -> Result<Vec<u8>, String> {
    let (mut pw, mut ph) = (opts.paper_width_in, opts.paper_height_in);
    if opts.landscape {
        std::mem::swap(&mut pw, &mut ph);
    }
    let [mt, mr, mb, ml] = opts.margins_in;
    let scale = if opts.scale > 0.0 { opts.scale } else { 1.0 };
    // CSS px of content per page: 96 px to the inch, shrunk by the scale
    let content_w = ((pw - ml - mr) * 96.0 / scale).max(1.0);
    let content_h = ((ph - mt - mb) * 96.0 / scale).max(1.0);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let saved_metrics = spec::devtools::device_metrics();
    let saved_media = spec::devtools::emulated_media();
    let scroll = crate::core::html::window_geometry::scroll_position(&op_state.borrow());
    let print_media = spec::devtools::EmulatedMedia {
        media: "print".into(),
        features: saved_media.as_ref().map(|m| m.features.clone()).unwrap_or_default(),
    };
    spec::devtools::set_emulated_media(Some(print_media));
    let page_metrics = |height: f64| spec::devtools::DeviceMetrics {
        width: content_w.round(),
        height: height.round(),
        device_scale_factor: 1.0,
        mobile: false,
        screen_width: 0.0,
        screen_height: 0.0,
    };
    spec::devtools::set_device_metrics(Some(page_metrics(content_h)));
    op_state.borrow_mut().put(crate::core::html::window_geometry::ScrollPosition { x: 0.0, y: 0.0 });
    // the document's height at the paper's width, then one picture of all of it
    let doc_h = crate::core::dom::geometry_queries::layout_metrics(scope)[5].max(content_h);
    spec::devtools::set_device_metrics(Some(page_metrics(doc_h)));
    let listed = document_display_list(scope);
    spec::devtools::set_device_metrics(saved_metrics);
    spec::devtools::set_emulated_media(saved_media);
    op_state.borrow_mut().put(crate::core::html::window_geometry::ScrollPosition { x: scroll.0, y: scroll.1 });
    let (ops, w, h) = listed.ok_or_else(|| "Printing failed".to_string())?;
    let count = ((doc_h / content_h).ceil() as u32).max(1);
    let pages = page_ranges(&opts.page_ranges, count)?;
    let title = crate::core::dom::document::page_title(&op_state);
    let layout = spec::PdfLayout {
        paper_width: (pw * 72.0) as f32,
        paper_height: (ph * 72.0) as f32,
        margins: [(mt * 72.0) as f32, (mr * 72.0) as f32, (mb * 72.0) as f32, (ml * 72.0) as f32],
        points_per_px: (0.75 * scale) as f32,
        page_height_px: content_h as f32,
        pages,
        title,
    };
    let render = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    render.and_then(|r| r.print_pdf(&ops, w, h, &layout)).ok_or_else(|| "Printing failed".to_string())
}

/// `pageRanges` as Chrome reads it (`PageRangeParser`): comma-separated pages and `a-b` runs, 1-based, an open end
/// meaning the first or the last page; empty is every page. Chrome's two refusals: a range it cannot parse, and one
/// that starts past the last page.
fn page_ranges(spec_: &str, count: u32) -> Result<Vec<u32>, String> {
    let spec_ = spec_.trim();
    if spec_.is_empty() {
        return Ok((0..count).collect());
    }
    let mut out: Vec<u32> = Vec::new();
    for part in spec_.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let num = |t: &str, dflt: u32| -> Result<u32, String> {
            let t = t.trim();
            if t.is_empty() {
                return Ok(dflt);
            }
            t.parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| "Page range syntax error".to_string())
        };
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (num(a, 1)?, num(b, count)?),
            None => {
                let n = num(part, 1)?;
                (n, n)
            }
        };
        if a > b {
            return Err("Page range syntax error".to_string());
        }
        if a > count {
            return Err("Page range exceeds page count".to_string());
        }
        for p in a..=b.min(count) {
            if !out.contains(&(p - 1)) {
                out.push(p - 1);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The document's display list as the frame paints it -- boxes, replaced content and the text's fonts -- at the
/// current viewport, with its size: what the PDF printer draws. Nothing of a frame's caching or paint timing.
fn document_display_list(scope: &mut v8::PinScope) -> Option<(Vec<spec::Canvas2DOp>, u32, u32)> {
    let ui_lang = crate::core::navigator::navigator::persona(scope).and_then(|p| p.navigator().languages.first().cloned()).unwrap_or_else(|| "en-US".into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (mut ops, w, h, replaced) = {
        let mut st = op_state.borrow_mut();
        crate::core::dom::geometry_queries::refresh_layout_cache(&mut st);
        let p = page(&st)?;
        let media = media_context(&st);
        let dom = st.try_borrow::<HostDom>()?;
        let laid_out = st.try_borrow::<crate::core::dom::geometry_queries::LayoutCache>().and_then(|c| c.0.as_ref()).map(|(_, out)| out)?;
        let (w, h) = (media.viewport_width.max(1.0) as u32, media.viewport_height.max(1.0) as u32);
        let (ops, replaced, _) = page::paint::paint_display_list(&p, &dom.0, &media, laid_out, w as f64, h as f64);
        (ops, w, h, replaced)
    };
    compose_replaced(scope, &mut ops, replaced, 0, None);
    let st = op_state.borrow();
    if let Some(dom) = st.try_borrow::<HostDom>() {
        complete_fonts(&st, &dom.0, dom.0.root(), &media_context(&st), &ui_lang, &mut ops);
    }
    Some((ops, w, h))
}

/// The `w` x `h` region of `buf` at `(x, y)` (rounded to whole pixels); what lies outside `buf` is transparent.
fn crop_rgba(buf: &spec::Rgba8, x: f64, y: f64, w: f64, h: f64) -> spec::Rgba8 {
    let (x0, y0) = (x.round() as i64, y.round() as i64);
    let (w, h) = (w.round().max(1.0) as u32, h.round().max(1.0) as u32);
    let mut out = spec::Rgba8::transparent(w, h);
    for row in 0..h as i64 {
        let sy = y0 + row;
        if sy < 0 || sy >= buf.height as i64 {
            continue;
        }
        for col in 0..w as i64 {
            let sx = x0 + col;
            if sx < 0 || sx >= buf.width as i64 {
                continue;
            }
            let si = ((sy as usize * buf.width as usize) + sx as usize) * 4;
            let di = ((row as usize * w as usize) + col as usize) * 4;
            out.data[di..di + 4].copy_from_slice(&buf.data[si..si + 4]);
        }
    }
    out
}

/// `buf` scaled by `k`, each pixel of the result the source pixel under its centre.
fn scale_rgba(buf: &spec::Rgba8, k: f64) -> spec::Rgba8 {
    let (w, h) = (((buf.width as f64) * k).round().max(1.0) as u32, ((buf.height as f64) * k).round().max(1.0) as u32);
    let mut out = spec::Rgba8::transparent(w, h);
    for row in 0..h {
        let sy = (((row as f64 + 0.5) / k) as u32).min(buf.height.saturating_sub(1));
        for col in 0..w {
            let sx = (((col as f64 + 0.5) / k) as u32).min(buf.width.saturating_sub(1));
            let si = ((sy * buf.width + sx) * 4) as usize;
            let di = ((row * w + col) * 4) as usize;
            out.data[di..di + 4].copy_from_slice(&buf.data[si..si + 4]);
        }
    }
    out
}

/// Paint the document to an offscreen frame at a rendering opportunity (no encode). The
/// per-frame compositor path — opt-in (the runtime calls it only when a non-null compositor is active)
/// until its cost is verified, because it changes
/// normal-load behaviour. RC2.2.1+ will feed this frame into rAF cadence / paint timing / visibility.
/// Marks that this realm has recorded its first paint, so `first-paint`/`first-contentful-paint` are
/// emitted once (RC2.2.2b).
struct FirstPaintDone;

/// The largest-contentful-paint WINNER render_document_rgba selected for this frame — the
/// larger of the biggest text box and any loaded replaced box (already intrinsic-clamped). `node` is a
/// u32 arena id, `area` its effective size in px². render_document_frame emits it on a high-water mark.
struct LcpPending {
    node: u32,
    area: f64,
    /// RENDER #6: the winner's request URL for the entry's `url` field -- the image `src` for an image
    /// winner, empty for a text winner (Brave).
    url: String,
}

/// The largest LCP area emitted so far this realm. LCP re-emits only when a LARGER
/// contentful element paints (Brave's high-water-mark), so a static page emits once and a growing one
/// emits each time it grows.
struct LcpHighWater(f64);

/// The paint entries of a realm with NO compositor (the default) -- and the rendering
/// updates that come before them.
///
/// A document's FIRST RENDERING UPDATE ends its first animation frame (`timing::loaf::first_render`) and, for the
/// page, begins Chrome's paint holding (`timing::performance::begin_paint_hold`) -- whether or not it drew
/// anything. Its paint entries follow Blink's paint timing (`PaintController::ProcessNewItem`): `first-paint` at the
/// first update that DRAWS something -- a box's background or border, text, an image -- the document's default
/// background aside; `first-contentful-paint` at the first that draws text or an image. This recorded both at the
/// first update of any page with a body, and called the body's text content contentful -- its inline `<script>`'s
/// source included: a bot challenge page, whose body draws nothing until its challenge does, read two
/// paint entries where Chrome 150's timeline had none (2026-10-09). Asked at each rendering opportunity; a cheap
/// walk until both are recorded.
/// The page document's paint entries so far: (`first-paint`, `first-contentful-paint`) recorded.
struct PaintNoted(bool, bool);
/// The page document had its first rendering update.
struct PageRendered;
/// The frame documents that have painted, by (context, document root).
#[derive(Default)]
struct FramePaintNoted(std::collections::HashSet<(u32, u32)>);
/// The frame documents that had their first rendering update, by (context, document root).
#[derive(Default)]
struct FrameRendered(std::collections::HashSet<(u32, u32)>);

/// Whether the running realm's document has had its first rendering update (`core/timing/loaf.rs`: a task before it
/// is part of its first frame). The page's, by either paint path; a frame's, by its own record.
pub(crate) fn realm_has_rendered(scope: &mut v8::PinScope) -> bool {
    let frame = crate::core::html::browsing_context::current_realm_ctx(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Ok(st) = op_state.try_borrow() else { return true };
    match frame {
        Some(ctx) => st.try_borrow::<FrameRendered>().is_some_and(|n| n.0.iter().any(|(c, _)| *c == ctx)),
        None => st.has::<PageRendered>(),
    }
}

pub fn note_paint_without_compositor(scope: &mut v8::PinScope) {
    note_frame_paints(scope);
    note_page_paint(scope);
}

/// The page document's rendering update, as the paint timing sees it: its first one (the first frame's end, the
/// paint holding's start) and its first `first-paint` / `first-contentful-paint`.
///
/// The first one also works every element's style out, after its paint entries are recorded: a transition a
/// later class change starts runs from that style. An element nothing had styled since the page loaded -- a page
/// served and left idle, a script that never read a style -- had no before-change style, so its first change jumped
/// to the new value with no transition, where Chrome's first frame had styled it. Only a document that could declare
/// an animation or a transition at all is styled here: the animation rescan visits every element to find its
/// candidates, and a page of 100 000 `<option>`s that declares none spent the debug build's first frame on that walk
/// and recorded no paint entry within 4 s (paint_holding_live case F).
fn note_page_paint(scope: &mut v8::PinScope) {
    if page_paint_entries(scope) && may_declare_animations(scope) {
        crate::core::animation::css::tick(scope);
    }
}

/// [`page::Page::may_declare_animations`] for the page document.
fn may_declare_animations(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    match (page(&st), st.try_borrow::<crate::binding::HostDom>()) {
        (Some(p), Some(h)) => p.may_declare_animations(&h.0),
        _ => false,
    }
}

/// [`note_page_paint`]'s paint timing; `true` when this was the page document's first rendering update.
fn page_paint_entries(scope: &mut v8::PinScope) -> bool {
    let (rendered, fp, fcp) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let (fp, fcp) = st.try_borrow::<PaintNoted>().map(|p| (p.0, p.1)).unwrap_or((false, false));
        (st.has::<PageRendered>(), fp, fcp)
    };
    if rendered && fp && fcp {
        return false;
    }
    let drawn = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let (html, body) = {
            let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return false };
            let d = &h.0;
            let is = |e: u32, t: &str| d.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
            let html = d.children(d.root()).into_iter().find(|&e| is(e, "html"));
            let body = html.and_then(|html| d.children(html).into_iter().find(|&e| is(e, "body")));
            (html, body)
        };
        match (html, body) {
            (Some(html), Some(body)) => Some(if fp && fcp { (true, true) } else { memo_document_draws(&mut st, html, body) }),
            _ => None,
        }
    };
    // no body yet: no rendering update of the document's own
    let Some((draws, contentful)) = drawn else { return false };
    if !rendered {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(PageRendered);
        let now = crate::core::timing::performance::now_ms(scope);
        // the hold first, so the first frame that ends here is presented when it ends -- an HTML document served
        // over HTTP(S) holds, as Blink's `DocumentLoader` allows it (`SetDeferredCompositorCommitIsAllowed`), and
        // only while nothing contentful painted: contentful content at its first update stops the deferral at once
        let holds = !contentful && {
            let st = op_state.borrow();
            let http = st.try_borrow::<crate::core::dom::CookieUrl>().is_some_and(|u| u.0.starts_with("http://") || u.0.starts_with("https://"));
            http && st.try_borrow::<crate::binding::HostDom>().is_some_and(|h| !h.0.is_in_xml(h.0.root()))
        };
        if holds {
            crate::core::timing::performance::begin_paint_hold(scope, now);
        }
        crate::core::timing::loaf::first_render(scope, now);
    }
    let mut names = Vec::new();
    if !fp && (draws || contentful) {
        names.push("first-paint");
    }
    if !fcp && contentful {
        names.push("first-contentful-paint");
        // contentful content ends the paint holding: this frame is presented
        crate::core::timing::performance::release_paint_hold(scope);
    }
    if names.is_empty() {
        return !rendered;
    }
    crate::core::timing::performance::add_paint_entries(scope, &names);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(PaintNoted(fp || draws || contentful, fcp || contentful));
    !rendered
}

/// The page's last paint walk: the layout fingerprint it ran at and what it found. The walk is asked at every
/// rendering opportunity until both paint entries are recorded, and a document that draws nothing yet does not
/// change between most of them: walking it again each time took a page of 100 000 empty `<option>`s a whole walk
/// per frame, so its timers, its animation frames and its load event never ran (WPT
/// `html/select/options-length-too-large.html`, 2026-10-09).
struct DrawsMemo(u64, (bool, bool));

/// [`document_draws`], walked again only when the document's layout fingerprint -- its DOM revision, its sheets,
/// its viewport -- has moved since the last walk.
fn memo_document_draws(st: &mut OpState, html: u32, body: u32) -> (bool, bool) {
    let key = match (page(st), st.try_borrow::<crate::binding::HostDom>()) {
        (Some(p), Some(h)) => Some(p.layout_fingerprint(&h.0, &media_context(st))),
        _ => None,
    };
    if let (Some(k), Some(m)) = (key, st.try_borrow::<DrawsMemo>()) {
        if m.0 == k {
            return m.1;
        }
    }
    let out = document_draws(st, html, body);
    if let Some(k) = key {
        st.put(DrawsMemo(k, out));
    }
    out
}

/// Does `select` render as a drop-down (Blink's `HTMLSelectElement::UsesMenuList`): not `multiple`, and a
/// `size` of at most 1 (an absent or invalid `size` is 0).
fn uses_menu_list(dom: &korvus_dom::Dom, select: u32) -> bool {
    if dom.get_attribute(select, "multiple").is_some() {
        return false;
    }
    let size = dom.get_attribute(select, "size").and_then(|s| crate::core::dom::element::parse_html_integer(s, false)).unwrap_or(0);
    size <= 1
}

/// What a document's rendering update DRAWS, as Blink's paint timing asks it: (draws anything, draws text or an
/// image -- a CSS background image too: Blink's `GraphicsContext::DrawImage*` marks it). Anything: a rendered box
/// with a background or a border, or a document background that is not the default (white or none) -- Blink
/// leaves the default one out (`ViewPainter`). Not rendered: a `display:none` subtree, a `visibility:hidden`
/// element's own box and text, and what a script, a style, a template, a `noscript` (scripting is on), a title or
/// the head holds.
fn document_draws(st: &mut OpState, html: u32, body: u32) -> (bool, bool) {
    let mut draws = false;
    for e in [html, body] {
        let image = css_computed_one(st, e, "background-image");
        if image.contains("url(") {
            return (true, true);
        }
        let bg = css_computed_one(st, e, "background-color");
        draws |= !is_transparent(&bg) && !is_white(&bg) || image != "none";
    }
    let mut stack = vec![(body, true)];
    while let Some((n, visible)) = stack.pop() {
        let (kind, tag, kids, text) = {
            let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return (draws, false) };
            let d = &h.0;
            (d.node_type(n), d.tag_name(n).map(|t| t.to_ascii_lowercase()), flat_children(d, n), d.node_value(n))
        };
        match kind {
            3 => {
                if visible && text.is_some_and(|t| !t.trim().is_empty()) {
                    return (true, true);
                }
            }
            1 => {
                let tag = tag.unwrap_or_default();
                if matches!(tag.as_str(), "script" | "style" | "template" | "noscript" | "title" | "head") {
                    continue;
                }
                if n != body && css_computed_one(st, n, "display") == "none" {
                    continue;
                }
                let visible = !matches!(css_computed_one(st, n, "visibility").as_str(), "hidden" | "collapse");
                // A drop-down `<select>` lays none of its options out in the page: it draws its own box and its
                // arrow -- Chrome 150 records `first-paint` for a borderless, transparent one -- and the text it
                // shows is its selected option's label (contentful when that is not blank). A list box draws its
                // options, which the walk below reaches. Measured 2026-10-09 (`<select>` with empty options: paint
                // only; with `abc`: both; `multiple` or `size=3` with `abc`: both, with an empty option: paint only).
                if tag == "select" {
                    let menu = st.try_borrow::<crate::binding::HostDom>().is_some_and(|h| uses_menu_list(&h.0, n));
                    if menu {
                        if !visible {
                            continue;
                        }
                        let shown = {
                            let selected = st.try_borrow::<crate::binding::HostDom>().and_then(|h| h.0.selected_options(n).first().copied());
                            selected.map(|o| crate::core::html::html_option_element::option_display_label(st, o)).unwrap_or_default()
                        };
                        if !shown.trim().is_empty() {
                            return (true, true);
                        }
                        draws = true;
                        continue;
                    }
                }
                if visible && (matches!(tag.as_str(), "img" | "canvas" | "svg" | "video") || css_computed_one(st, n, "background-image").contains("url(")) {
                    return (true, true);
                }
                if visible && n != body && !draws && box_draws(st, n) {
                    draws = true;
                }
                stack.extend(kids.into_iter().rev().map(|c| (c, visible)));
            }
            _ => {}
        }
    }
    (draws, false)
}

/// Whether a rendered element's own box draws: a background (colour or image), or a border on any side.
fn box_draws(st: &mut OpState, el: u32) -> bool {
    if !is_transparent(&css_computed_one(st, el, "background-color")) || css_computed_one(st, el, "background-image") != "none" {
        return true;
    }
    ["top", "right", "bottom", "left"].iter().any(|side| {
        let style = css_computed_one(st, el, &format!("border-{side}-style"));
        !matches!(style.as_str(), "none" | "hidden") && css_computed_one(st, el, &format!("border-{side}-width")) != "0px"
    })
}

fn is_transparent(c: &str) -> bool {
    c == "transparent" || c.starts_with("rgba(") && c.trim_end_matches(')').rsplit(',').next().is_some_and(|a| a.trim().parse::<f64>().is_ok_and(|a| a == 0.0))
}

fn is_white(c: &str) -> bool {
    matches!(c, "rgb(255, 255, 255)" | "white" | "#ffffff" | "#fff")
}

/// The children `n` renders: a shadow host's shadow tree's, a `<slot>`'s assigned nodes (or its fallback content),
/// else its own -- the flat tree paint walks. Chrome 150 records a frame's `first-paint`
/// and `first-contentful-paint` for text or an SVG inside an open or closed shadow root and for a host's text a
/// slot shows, and none for a host's text no slot shows; the walk read the light tree, so a bot challenge
/// frame, whose widget is drawn inside a closed shadow root, never had a paint entry.
fn flat_children(d: &korvus_dom::Dom, n: u32) -> Vec<u32> {
    if let Some(root) = d.shadow_root_of(n) {
        return d.children(root);
    }
    d.slot_rendered_children(n).unwrap_or_else(|| d.children(n))
}

/// Whether `node`'s subtree paints CONTENTFUL content -- non-blank text, an image, a canvas, an SVG, a video --
/// leaving out what is never rendered: a script's, a style's, a template's, a `noscript`'s and a title's text
/// (an empty frame whose only child is its `<script>` paints nothing in Chrome 150, measured).
fn renders_content(d: &korvus_dom::Dom, node: u32) -> bool {
    match d.node_type(node) {
        3 => d.node_value(node).is_some_and(|t| !t.trim().is_empty()),
        1 => {
            let tag = d.tag_name(node).unwrap_or_default().to_ascii_lowercase();
            if matches!(tag.as_str(), "img" | "canvas" | "svg" | "video") {
                return true;
            }
            if matches!(tag.as_str(), "script" | "style" | "template" | "noscript" | "title" | "head") {
                return false;
            }
            flat_children(d, node).into_iter().any(|c| renders_content(d, c))
        }
        _ => false,
    }
}

/// The paint entries of the FRAMES, each on its own document's timeline: Chrome records
/// a frame document's `first-paint` and `first-contentful-paint` when the frame paints contentful content --
/// measured on stock Chrome 150: a visible frame holding text records both; an empty one, one holding only an
/// unstyled box, and a `display:none`, `visibility:hidden`, 0x0 or off-screen one record neither. No frame
/// recorded any here: a bot-challenge frame read `navigation, visibility-state, resource` where Chrome's
/// timeline goes on to `paint`. A showing frame's first rendering update ends its first animation frame, drawn or
/// not; a frame's document never holds its paint (`WillDoPaintHoldingForFCP`: the outermost main frame's only).
fn note_frame_paints(scope: &mut v8::PinScope) {
    for (ctx, realm) in crate::core::html::browsing_context::built_contexts(scope) {
        let Some(root) = crate::core::html::browsing_context::frame_document_root(scope, ctx) else { continue };
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        if op_state.borrow().try_borrow::<FramePaintNoted>().is_some_and(|n| n.0.contains(&(ctx, root))) {
            continue;
        }
        let (showing, painted) = {
            let mut st = op_state.borrow_mut();
            let Some(el) = crate::core::html::browsing_context::frame_element_showing(&st, root) else { continue };
            let (x, y, w, h) = crate::core::dom::geometry_queries::element_viewport_rect(&mut st, el);
            let media = crate::core::dom::geometry_queries::media_context(&st);
            let (vw, vh) = (media.viewport_width as f64, media.viewport_height as f64);
            let on_screen = w > 0.0 && h > 0.0 && x < vw && y < vh && x + w > 0.0 && y + h > 0.0;
            let shown = on_screen && !matches!(css_computed_one(&mut st, el, "visibility").as_str(), "hidden" | "collapse");
            let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { continue };
            let d = &h.0;
            let is = |e: u32, t: &str| d.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
            let body = d.children(root).into_iter().find(|&e| is(e, "html")).and_then(|html| d.children(html).into_iter().find(|&e| is(e, "body")));
            (shown && body.is_some(), shown && body.is_some_and(|b| renders_content(d, b)))
        };
        if !showing {
            continue;
        }
        let first_render = {
            let mut st = op_state.borrow_mut();
            if !st.has::<FrameRendered>() {
                st.put(FrameRendered::default());
            }
            st.borrow_mut::<FrameRendered>().0.insert((ctx, root))
        };
        if painted {
            let mut st = op_state.borrow_mut();
            if !st.has::<FramePaintNoted>() {
                st.put(FramePaintNoted::default());
            }
            st.borrow_mut::<FramePaintNoted>().0.insert((ctx, root));
        }
        if !first_render && !painted {
            continue;
        }
        let realm = v8::Local::new(scope, &realm);
        let scope = &mut v8::ContextScope::new(scope, realm);
        if first_render {
            let now = crate::core::timing::performance::now_ms(scope);
            crate::core::timing::loaf::first_render(scope, now);
        }
        if painted {
            crate::core::timing::performance::add_paint_entries(scope, &["first-paint", "first-contentful-paint"]);
        }
    }
}

pub fn render_document_frame(scope: &mut v8::PinScope, armed: bool) {
    note_frame_paints(scope);
    // The FIRST paint always happens (the initial render is a paint, like any browser); after that,
    // paint only when a frame is armed (rAF/animation/observer) — the RC2.2.2a cost win: an idle page
    // that has already painted once skips.
    let first = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        !st.has::<FirstPaintDone>()
    };
    if !first && !armed {
        return;
    }
    if render_document_rgba(scope).is_none() {
        return; // nothing painted yet (no page/layout/backend)
    }
    if first {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(FirstPaintDone);
    }
    // Paint timing (RC2.2.2b): the first rendering update, and the paint entries by what it drew -- as without
    // a compositor ([`note_page_paint`]); this recorded both at the first frame whatever it drew.
    note_page_paint(scope);
    // Emit `largest-contentful-paint` when a LARGER contentful element has painted than any
    // before (a per-realm high-water mark, as Brave re-fires LCP as content grows). The winner + its
    // intrinsic-clamped, loaded-only size were already chosen in render_document_rgba. getEntriesByType
    // filters LCP out (observer-only), so this reaches PerformanceObservers only, matching Brave.
    let cand = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<LcpPending>().map(|p| (p.node, p.area, p.url.clone()))
    };
    if let Some((node, area, url)) = cand {
        let hw = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<LcpHighWater>().map(|h| h.0).unwrap_or(0.0)
        };
        if area > 0.0 && area > hw {
            {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                op_state.borrow_mut().put(LcpHighWater(area));
            }
            crate::core::timing::performance::add_lcp_entry(scope, node, area, &url);
        }
    }
}

/// A computed declaration's target, when it is a PSEUDO-ELEMENT's: the declaration
/// keeps a `u32` marker, and a marker with this bit set indexes [`PseudoTargets`] instead of naming a
/// node. Node ids are arena indices and never reach it.
pub(crate) const PSEUDO_BIT: u32 = 1 << 31;

/// The `(element, pseudo-element name)` pairs `getComputedStyle(el, pseudo)` has been asked for, one
/// entry per distinct pair.
#[derive(Default)]
pub(crate) struct PseudoTargets {
    list: Vec<(u32, String)>,
    index: std::collections::HashMap<(u32, String), u32>,
}

/// The marker a computed declaration for `node`'s `pseudo` carries.
pub(crate) fn pseudo_marker(state: &mut OpState, node: u32, pseudo: &str) -> u32 {
    if state.try_borrow::<PseudoTargets>().is_none() {
        state.put(PseudoTargets::default());
    }
    let t = state.borrow_mut::<PseudoTargets>();
    let key = (node, pseudo.to_string());
    if let Some(&i) = t.index.get(&key) {
        return i | PSEUDO_BIT;
    }
    let i = t.list.len() as u32;
    t.list.push(key.clone());
    t.index.insert(key, i);
    i | PSEUDO_BIT
}

/// `(node, pseudo)` for a declaration marker.
fn target_of(state: &OpState, marker: u32) -> (u32, Option<String>) {
    if marker & PSEUDO_BIT == 0 {
        return (marker, None);
    }
    match state.try_borrow::<PseudoTargets>().and_then(|t| t.list.get((marker & !PSEUDO_BIT) as usize)) {
        Some((node, pseudo)) => (*node, Some(pseudo.clone())),
        None => (u32::MAX, None),
    }
}

/// The pseudo-element name `getComputedStyle(el, text)` asks for: `Some(name)` for one Chrome answers
/// a style for, `None` for anything else, which reads as the empty declaration.
///
/// Measured on Chrome 150 over `document.body`: `length` 474 for `::before`, `::after`, `::marker`,
/// `::placeholder`, `::first-line`, `::first-letter`, `::selection`, `::backdrop`,
/// `::file-selector-button`, `::-webkit-scrollbar`, `::-webkit-scrollbar-thumb`, `::-webkit-foo`,
/// `::target-text`, `::spelling-error`, `::grammar-error`, `::highlight(x)`, `::view-transition`,
/// `::cue`, `::details-content`, `::picker(select)`, `::scroll-marker`, `:before`, `:after` and
/// `::BEFORE`; 0 for `:marker` and `::nonsense`.
pub(crate) fn pseudo_name(text: &str) -> Option<String> {
    let lower = text.trim().to_ascii_lowercase();
    if let Some(name) = lower.strip_prefix("::") {
        let base = name.split('(').next().unwrap_or("");
        let known = matches!(
            base,
            "before" | "after" | "marker" | "placeholder" | "first-line" | "first-letter" | "selection" | "backdrop"
                | "file-selector-button" | "target-text" | "spelling-error" | "grammar-error" | "highlight" | "view-transition"
                | "view-transition-group" | "view-transition-image-pair" | "view-transition-old" | "view-transition-new"
                | "cue" | "details-content" | "picker" | "scroll-marker" | "scroll-marker-group" | "scroll-button"
                | "column" | "checkmark" | "picker-icon"
        ) || (base.starts_with("-webkit-") && base.len() > 8);
        return known.then(|| base.to_string());
    }
    let name = lower.strip_prefix(':')?;
    matches!(name, "before" | "after" | "first-line" | "first-letter").then(|| name.to_string())
}

/// One resolved property of `node`, as `getComputedStyle(el).paddingLeft` and
/// `getPropertyValue("padding-left")` answer. `name` is already dashed.
///
/// A custom property (`--x`) is looked up separately because it is not in the property table; an
/// unknown name is `""`, which is also Chrome's answer for one that is not a CSS property at all
/// (measured: `getPropertyValue("colr")` is `""`).
pub(crate) fn css_computed_one(state: &mut OpState, node: u32, name: &str) -> String {
    if name == "transform" {
        let v = css_computed_one_inner(state, node, name);
        return resolved_transform(state, node, v);
    }
    css_computed_one_inner(state, node, name)
}

/// The computed `transform` as Chrome reports it: a `matrix()`, or a `matrix3d()` when the list is 3D
/// (measured on Chrome 150 over `tools/realweb/seed/tf`). `translate3d(-120px,0,0)` reads
/// `matrix(1, 0, 0, 1, -120, 0)`; a percentage in a translation is of the border box. korvus handed the
/// list back as written, and jwplayer's Swiper parsed "nanpx" out of it.
fn resolved_transform(state: &mut OpState, node: u32, value: String) -> String {
    let t = value.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return value;
    }
    let (w, h) = if t.contains('%') { border_box(state, node) } else { (0.0, 0.0) };
    let resolved = resolve_translate_percentages(t, w, h);
    match crate::core::geometry::dom_matrix::parse_transform(&resolved) {
        Ok((m, _)) => {
            // 2D by the MATRIX, not by the functions that built it: `translate3d(-120px,0,0)` is
            // `matrix(1, 0, 0, 1, -120, 0)` in Chrome, `translateZ(10px)` a `matrix3d`.
            let is_2d = [m[2], m[3], m[6], m[7], m[8], m[9], m[11], m[14]].iter().all(|v| v.abs() < 1e-9)
                && (m[10] - 1.0).abs() < 1e-9
                && (m[15] - 1.0).abs() < 1e-9;
            let n: Vec<String> = if is_2d {
                [m[0], m[1], m[4], m[5], m[12], m[13]].iter().map(|v| css_number(*v)).collect()
            } else {
                m.iter().map(|v| css_number(*v)).collect()
            };
            if is_2d { format!("matrix({})", n.join(", ")) } else { format!("matrix3d({})", n.join(", ")) }
        }
        Err(_) => value,
    }
}

/// Element `node`'s `transform` as a 4x4 matrix (column-major, DOMMatrix order) and its
/// `transform-origin` in px from its border box's top-left corner (x, y, z) -- `size` its border box, as
/// laid out -- or `None` when it has no transform (whether one applies to the box is the layout's
/// question: `LaidOut::transforms`).
/// What `getBoundingClientRect` maps a box's corners through.
pub(crate) fn transform_about_origin(state: &mut OpState, node: u32, size: (f64, f64)) -> Option<([f64; 16], [f64; 3])> {
    let t = css_computed_one_inner(state, node, "transform");
    let t = t.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return None;
    }
    let (w, h) = size;
    let (m, _) = crate::core::geometry::dom_matrix::parse_transform(&float_arguments(&resolve_translate_percentages(t, w, h))).ok()?;
    // The origin from the COMPUTED value against the border box's exact size -- the resolved value is
    // written to six significant digits, and a corner mapped about a rounded origin moves by 1e-5.
    let (bw, bh) = size;
    let origin = css_computed_one_inner(state, node, "transform-origin");
    let mut parts = origin.split_whitespace();
    let mut axis = |basis: f64, horizontal: bool| -> f64 {
        match parts.next().map(|t| t.to_ascii_lowercase()) {
            None => basis / 2.0,
            Some(t) => match t.as_str() {
                "left" | "top" => 0.0,
                "center" => basis / 2.0,
                "right" | "bottom" => basis,
                t => match t.strip_suffix('%') {
                    Some(p) => p.parse::<f32>().map_or(basis / 2.0, |p| (p / 100.0 * basis as f32) as f64),
                    None => t.trim_end_matches("px").parse::<f64>().unwrap_or(if horizontal { basis / 2.0 } else { basis / 2.0 }),
                },
            },
        }
    };
    let ox = axis(bw, true);
    let oy = axis(bh, false);
    let oz = parts.next().and_then(|t| t.trim_end_matches("px").parse::<f64>().ok()).unwrap_or(0.0);
    Some((m, [ox, oy, oz]))
}

/// Element `node`'s computed `transform` as Blink's LAYOUT applies it to its border box of `size`
/// (`ComputedStyle::ApplyTransform` with the origin): a `gfx_transform::Transform` built as Chromium builds it.
/// `None` with no transform, or one whose functions `gfx_transform` does not model (the caller maps it its own way).
pub(crate) fn layout_transform(state: &mut OpState, node: u32, size: (f32, f32)) -> Option<crate::core::geometry::gfx_transform::Transform> {
    let t = css_computed_one_inner(state, node, "transform");
    let t = t.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return None;
    }
    let (w, h) = size;
    // the functions' numbers as the CSS parser read them (doubles): each operation makes a float of the ones
    // Blink's `Translate3d` / `Scale3d` take as floats, and keeps the rest (an angle, a matrix) double
    let ops = crate::core::geometry::gfx_transform::parse_ops(&resolve_translate_percentages(t, w as f64, h as f64))?;
    // `FloatValueForLength` against the border box, in float: a percentage is `size * percent / 100` -- from the
    // COMPUTED origin, not the px `getComputedStyle` resolves it to (six significant digits: 74.4844 for 74.484375)
    let origin = css_computed_unresolved(state, node, "transform-origin");
    let mut parts = origin.split_whitespace();
    let mut axis = |basis: f32| -> f32 {
        let pct = |p: f32| basis * p / 100.0;
        match parts.next().map(|t| t.to_ascii_lowercase()) {
            None => pct(50.0),
            Some(t) => match t.as_str() {
                "left" | "top" => 0.0,
                "center" => pct(50.0),
                "right" | "bottom" => pct(100.0),
                t => match t.strip_suffix('%') {
                    Some(p) => p.parse::<f32>().map_or(pct(50.0), pct),
                    None => t.trim_end_matches("px").parse::<f32>().unwrap_or(pct(50.0)),
                },
            },
        }
    };
    let ox = axis(w);
    let oy = axis(h);
    let oz = parts.next().and_then(|t| t.trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
    Some(crate::core::geometry::gfx_transform::layer_transform(&ops, (ox, oy, oz)))
}

/// The linear part `[a, b, c, d]` of element `node`'s computed `transform` -- each function's numbers as Blink
/// stores them -- or `None` when it has none. What an SVG text's font size is scaled by (`svg_text.rs`).
pub(crate) fn transform_linear(state: &mut OpState, node: u32) -> Option<[f64; 4]> {
    let t = css_computed_one_inner(state, node, "transform");
    let t = t.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return None;
    }
    let (m, _) = crate::core::geometry::dom_matrix::parse_transform(&float_arguments(&resolve_translate_percentages(t, 0.0, 0.0))).ok()?;
    Some([m[0], m[1], m[4], m[5]])
}

/// Element `node`'s computed `transform` as a 2D matrix `[a, b, c, d, e, f]` about the origin -- an SVG element's
/// (its user space's `0 0` is its `transform-origin`, Blink's `svg.css`) -- or `None` when it has none.
pub(crate) fn transform_matrix_2d(state: &mut OpState, node: u32) -> Option<[f64; 6]> {
    let t = css_computed_one_inner(state, node, "transform");
    let t = t.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return None;
    }
    let (m, _) = crate::core::geometry::dom_matrix::parse_transform(&float_arguments(&resolve_translate_percentages(t, 0.0, 0.0))).ok()?;
    Some([m[0], m[1], m[4], m[5], m[12], m[13]])
}

/// A transform list with every function's numbers rounded to `f32`, as Blink's style builder stores a
/// transform function's arguments (Chrome 150: `skewY(23.1753218deg)` maps a corner to -14.2607212, the
/// value for the float angle 23.17532158; `scale(2, 1.89)` to 37.4200020, for the float 1.88999999).
/// `matrix()` and `matrix3d()` keep theirs.
fn float_arguments(t: &str) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().rsplit(|c: char| c.is_whitespace() || c == ')').next().unwrap_or("").to_ascii_lowercase();
        let Some(close) = rest[open..].find(')').map(|i| open + i) else { break };
        out.push_str(&rest[..open + 1]);
        let args = &rest[open + 1..close];
        if name.starts_with("matrix") {
            out.push_str(args);
        } else {
            let rounded: Vec<String> = args
                .split(',')
                .map(|a| {
                    let a = a.trim();
                    let split = a.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))).unwrap_or(a.len());
                    // a unit that starts with `e` (`em`) is not an exponent
                    let split = if a[..split].ends_with(['e', 'E']) { split - 1 } else { split };
                    match a[..split].parse::<f32>() {
                        Ok(v) => format!("{}{}", v as f64, &a[split..]),
                        Err(_) => a.to_string(),
                    }
                })
                .collect();
            out.push_str(&rounded.join(", "));
        }
        out.push(')');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The border box a transform's percentages are of: the resolved `width`/`height`, plus padding and
/// border unless `box-sizing` already includes them.
fn border_box(state: &mut OpState, node: u32) -> (f64, f64) {
    let border_boxed = css_computed_one_inner(state, node, "box-sizing").trim() == "border-box";
    let mut px = |n: &str| css_computed_one_inner(state, node, n).trim().trim_end_matches("px").parse::<f64>().unwrap_or(0.0);
    let (mut w, mut h) = (px("width"), px("height"));
    if !border_boxed {
        w += px("padding-left") + px("padding-right") + px("border-left-width") + px("border-right-width");
        h += px("padding-top") + px("padding-bottom") + px("border-top-width") + px("border-bottom-width");
    }
    (w, h)
}

/// `translate(50%, 10%)` -> `translate(<px>, <px>)`: each percentage argument of a translation function
/// against the box's width (x) or height (y).
fn resolve_translate_percentages(t: &str, w: f64, h: f64) -> String {
    if !t.contains('%') {
        return t.to_string();
    }
    let mut out = String::new();
    let mut rest = t;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().rsplit(|c: char| c.is_whitespace() || c == ')').next().unwrap_or("").to_ascii_lowercase();
        let Some(close) = rest[open..].find(')').map(|i| open + i) else { break };
        let args: Vec<String> = rest[open + 1..close]
            .split(',')
            .enumerate()
            .map(|(i, a)| {
                let a = a.trim();
                let Some(p) = a.strip_suffix('%').and_then(|n| n.trim().parse::<f64>().ok()) else { return a.to_string() };
                let along_y = matches!(name.as_str(), "translatey") || (matches!(name.as_str(), "translate" | "translate3d") && i == 1);
                let base = if along_y { h } else { w };
                format!("{}px", p * base / 100.0)
            })
            .collect();
        out.push_str(&rest[..open + 1]);
        out.push_str(&args.join(", "));
        out.push(')');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A matrix component as Chrome serializes it: at most six significant digits, no trailing zeros, and
/// the float dust of `cos(90deg)` read as the 0 it is.
fn css_number(v: f64) -> String {
    if !v.is_finite() || v.abs() < 1e-9 {
        return "0".to_string();
    }
    page::value::serialize_number_f64(v)
}

/// Element `node`'s computed value of `name` before layout resolves it ([`page::Page::computed_value_unresolved`]).
fn css_computed_unresolved(state: &mut OpState, node: u32, name: &str) -> String {
    let Some(property) = PropertyId::from_name(name) else { return String::new() };
    let (node, _) = target_of(state, node);
    with_resolved(state, node, false, String::new(), |p, dom, media, _| p.computed_value_unresolved(dom, node, media, property).unwrap_or_default())
}

fn css_computed_one_inner(state: &mut OpState, node: u32, name: &str) -> String {
    let target = node;
    let (node, pseudo) = target_of(state, node);
    if name.starts_with("--") {
        return with_resolved(state, node, false, String::new(), |p, dom, media, _| match &pseudo {
            Some(ps) => p.computed_pseudo_custom(dom, node, ps, media, name).unwrap_or_default(),
            None => p.computed_custom(dom, node, media, name).unwrap_or_default(),
        });
    }
    // The shorthands `getComputedStyle` serializes that this table reads: `overflow` is
    // its two longhands, one value when they agree. Chrome 150: `overflow: overlay` reads `auto`.
    if name == "overflow" {
        let x = css_computed_one_at(state, node, pseudo.as_deref(), "overflow-x");
        let y = css_computed_one_at(state, node, pseudo.as_deref(), "overflow-y");
        return if x == y { x } else if x.is_empty() || y.is_empty() { String::new() } else { format!("{x} {y}") };
    }
    // The four-side shorthands serialize from their longhands the CSSOM way -- `margin` on a `<body>`
    // reads `8px` in Chrome; it read "" here -- and so do the pair and border ones.
    if let Some(sides) = box_shorthand(name) {
        let v: Vec<String> = sides.iter().map(|l| css_computed_one_at(state, node, pseudo.as_deref(), l)).collect();
        if v.iter().any(|x| x.is_empty()) {
            return String::new();
        }
        return shorten_sides(&v[0], &v[1], &v[2], &v[3]);
    }
    // `white-space` IS A SHORTHAND (CSS Text 4) of `white-space-collapse` and `text-wrap-mode`, and
    // Chrome answers it with the keyword the pair spells. It read "" -- getComputedStyle
    // and getPropertyValue alike -- while both longhands were right. Chrome 150, measured
    // (`S-white-space-shorthand`): pre, pre-wrap, nowrap, pre-line, break-spaces, normal.
    if name == "white-space" {
        let c = css_computed_one_at(state, node, pseudo.as_deref(), "white-space-collapse");
        let w = css_computed_one_at(state, node, pseudo.as_deref(), "text-wrap-mode");
        return match (c.as_str(), w.as_str()) {
            ("collapse", "wrap") => "normal".to_string(),
            ("preserve", "nowrap") => "pre".to_string(),
            ("preserve", "wrap") => "pre-wrap".to_string(),
            ("collapse", "nowrap") => "nowrap".to_string(),
            ("preserve-breaks", "wrap") => "pre-line".to_string(),
            ("break-spaces", "wrap") => "break-spaces".to_string(),
            ("", _) | (_, "") => String::new(),
            _ => format!("{c} {w}"),
        };
    }
    // The grid-placement shorthands, serialized from their longhands the way CSSOM says
    // (Chrome 150: `grid-area: center` reads `center`; it read `""`).
    if let Some(longhands) = grid_placement_longhands(name) {
        let v: Vec<String> = longhands.iter().map(|l| css_computed_one_at(state, node, pseudo.as_deref(), l)).collect();
        if v.iter().any(|x| x.is_empty()) {
            return String::new();
        }
        return grid_placement_serialize(&v);
    }
    if name == "grid-template-areas" {
        let raw = css_computed_one_at(state, node, pseudo.as_deref(), name);
        return template_areas_serialize(&raw);
    }
    if name == "gap" {
        let r = css_computed_one_at(state, node, pseudo.as_deref(), "row-gap");
        let c = css_computed_one_at(state, node, pseudo.as_deref(), "column-gap");
        return if r == c { r } else if r.is_empty() || c.is_empty() { String::new() } else { format!("{r} {c}") };
    }
    if let Some(side) = name.strip_prefix("border-").filter(|s| matches!(*s, "top" | "right" | "bottom" | "left")) {
        let part = |state: &mut OpState, k: &str| css_computed_one_at(state, node, pseudo.as_deref(), &format!("border-{side}-{k}"));
        let (w, st, c) = (part(state, "width"), part(state, "style"), part(state, "color"));
        return if w.is_empty() || st.is_empty() || c.is_empty() { String::new() } else { format!("{w} {st} {c}") };
    }
    if name == "border" {
        let sides: Vec<String> = ["top", "right", "bottom", "left"]
            .iter()
            .map(|sd| {
                let part = |state: &mut OpState, k: &str| css_computed_one_at(state, node, pseudo.as_deref(), &format!("border-{sd}-{k}"));
                format!("{} {} {}", part(state, "width"), part(state, "style"), part(state, "color"))
            })
            .collect();
        return if sides.iter().all(|x| *x == sides[0]) { sides[0].clone() } else { String::new() };
    }
    let pseudo = pseudo.as_deref();
    match PropertyId::from_name(name) {
        // A fill layer's position halves: Blink computes both and enumerates neither, each
        // layer's as `CreatePositionListForLayer` writes it -- `center` is `50%`, `right 5px` is
        // `calc(100% - 5px)` (Chrome 150 reads `0%` for an unset `backgroundPositionX`; it read "").
        Some(
            p @ (PropertyId::BackgroundPositionX
            | PropertyId::BackgroundPositionY
            | PropertyId::WebkitMaskPositionX
            | PropertyId::WebkitMaskPositionY),
        ) => {
            let raw = css_computed_property(state, node, pseudo, p);
            let layers = fill_layer_count(state, node, pseudo, p);
            fill_layer_items(&raw, layers).iter().map(|v| computed_shorthand::position_component(v)).collect::<Vec<_>>().join(", ")
        }
        // Shorthands over those halves in Blink, and here (`style::shorthand`): the cascade keeps the halves only.
        // `border-spacing` is Blink's shorthand over its two `-webkit-` longhands, which this cascade answers from
        // it: the UA's `table { border-spacing: 2px }` reads `2px` for all three, as in Chrome 150.
        Some(PropertyId::BackgroundPosition | PropertyId::MaskPosition | PropertyId::BorderSpacing) => {
            computed_shorthand::serialize(name, &mut |l| css_computed_one_inner(state, target, l)).unwrap_or_default()
        }
        Some(p @ (PropertyId::WebkitBorderHorizontalSpacing | PropertyId::WebkitBorderVerticalSpacing)) => {
            let spacing = css_computed_property(state, node, pseudo, PropertyId::BorderSpacing);
            let mut halves = spacing.split_whitespace();
            let horizontal = halves.next().unwrap_or("0px").to_string();
            match (p, halves.next()) {
                (PropertyId::WebkitBorderVerticalSpacing, Some(vertical)) => vertical.to_string(),
                _ => horizontal,
            }
        }
        // `page` is not enumerated either, and answers: `auto` unless a rule names a page.
        Some(PropertyId::Page) => css_computed_property(state, node, pseudo, PropertyId::Page),
        // Chromium 150 computes an unset `position-anchor` as `none` (its keywords are `auto` and `none`, measured on
        // the frame body the challenge reads); the table's `normal` was recorded from Brave 151, whose initial it is.
        Some(PropertyId::PositionAnchor) => {
            let v = css_computed_one_at(state, node, pseudo, name);
            if v == "normal" && computed_table(state).anchor_none { "none".to_string() } else { v }
        }
        // A fill layer's other lists: as many items as the layer's images (Blink culls the layers past the last
        // image and repeats a shorter list), each written as Blink writes the layer's value.
        Some(p) if fill_layer_image(p).is_some() => {
            let raw = css_computed_one_at(state, node, pseudo, name);
            if raw.is_empty() {
                return raw;
            }
            let layers = fill_layer_count(state, node, pseudo, p);
            fill_layer_items(&raw, layers).iter().map(|v| fill_layer_value(p, v)).collect::<Vec<_>>().join(", ")
        }
        Some(_) => css_computed_one_at(state, node, pseudo, name),
        // An alias answers as its property (`-webkit-border-before` is `border-block-start`), and a shorthand from
        // its longhands as Blink serializes it (`computed_shorthand`).
        None => {
            let canonical = page::style::property::canonical_name(name);
            if canonical != name {
                return css_computed_one_inner(state, target, &canonical);
            }
            computed_shorthand::serialize(name, &mut |l| css_computed_one_inner(state, target, l)).unwrap_or_default()
        }
    }
}

/// The image longhand whose list sets how many fill layers a background or mask longhand's list covers, or `None`
/// for a property that is not a fill layer's list.
fn fill_layer_image(p: PropertyId) -> Option<PropertyId> {
    use PropertyId::*;
    match p {
        BackgroundPositionX | BackgroundPositionY | BackgroundSize | BackgroundRepeat | BackgroundAttachment | BackgroundOrigin
        | BackgroundClip | BackgroundBlendMode => Some(BackgroundImage),
        WebkitMaskPositionX | WebkitMaskPositionY | MaskSize | MaskRepeat | MaskOrigin | MaskClip | MaskComposite | MaskMode => {
            Some(MaskImage)
        }
        _ => None,
    }
}

/// How many fill layers `p`'s list covers: one per item of its image list (`none` is one layer).
fn fill_layer_count(state: &mut OpState, node: u32, pseudo: Option<&str>, p: PropertyId) -> usize {
    let Some(image) = fill_layer_image(p) else { return 1 };
    let images = css_computed_property(state, node, pseudo, image);
    computed_shorthand::items(&images).len().max(1)
}

/// A computed list's first `layers` items, the list repeated when it is shorter (CSS Values 4 "coordinated value
/// list": `background-repeat: no-repeat` over two images is `no-repeat, no-repeat`).
fn fill_layer_items(raw: &str, layers: usize) -> Vec<String> {
    let items = computed_shorthand::items(raw);
    (0..layers).map(|i| items[i % items.len()].clone()).collect()
}

/// One layer's value as Blink writes it: a size is its pair, collapsed only when both are `auto` (Chromium 152's
/// `ValueForFillSize`: `10px` reads `10px auto`, `auto auto` `auto`; Chromium 150 wrote a width alone whenever the
/// height was `auto`), a repeat pair as its one-keyword spelling when it has one (`CSSRepeatStyleValue`).
fn fill_layer_value(p: PropertyId, v: &str) -> String {
    let parts: Vec<&str> = v.split_whitespace().collect();
    match (p, parts.as_slice()) {
        (PropertyId::BackgroundSize | PropertyId::MaskSize, ["auto", "auto"]) => "auto".to_string(),
        (PropertyId::BackgroundSize | PropertyId::MaskSize, [w]) if !matches!(*w, "auto" | "contain" | "cover") => format!("{w} auto"),
        (PropertyId::BackgroundRepeat | PropertyId::MaskRepeat, ["repeat", "no-repeat"]) => "repeat-x".to_string(),
        (PropertyId::BackgroundRepeat | PropertyId::MaskRepeat, ["no-repeat", "repeat"]) => "repeat-y".to_string(),
        (PropertyId::BackgroundRepeat | PropertyId::MaskRepeat, [x, y]) if x == y => x.to_string(),
        _ => v.to_string(),
    }
}

/// The longhands of a four-side shorthand, in top/right/bottom/left order.
/// The longhands of `grid-area` / `grid-row` / `grid-column`, in serialization order.
fn grid_placement_longhands(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "grid-area" => &["grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end"],
        "grid-row" => &["grid-row-start", "grid-row-end"],
        "grid-column" => &["grid-column-start", "grid-column-end"],
        _ => return None,
    })
}

/// CSS Grid's shortest serialization of a placement shorthand: a trailing value is left out while it is
/// what the shorter form would give it -- its start line when that is a `<custom-ident>`, else `auto`.
fn grid_placement_serialize(v: &[String]) -> String {
    let ident = |s: &str| {
        let s = s.trim();
        !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.contains(' ') && s.parse::<i32>().is_err()
    };
    let implied = |start: &str| if ident(start) { start.to_string() } else { "auto".to_string() };
    let mut keep = v.len();
    // [row-start, column-start, row-end, column-end]: column-end falls back to column-start, row-end to
    // row-start, column-start to row-start; [start, end]: end falls back to start.
    let falls_back: &[usize] = if v.len() == 4 { &[1, 0, 0] } else { &[0] };
    for (i, &from) in falls_back.iter().enumerate() {
        let at = v.len() - 1 - i;
        if keep == at + 1 && v[at] == implied(&v[from]) {
            keep = at;
        } else {
            break;
        }
    }
    v[..keep.max(1)].join(" / ")
}

/// `grid-template-areas` as Chrome serializes it: each row string's cells separated by one space, a run
/// of `.` as one `.`, the rows separated by one space (`"a""b c"` -> `"a" "b c"`).
fn template_areas_serialize(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return t.to_string();
    }
    let mut rows = Vec::new();
    let mut rest = t;
    while let Some(open) = rest.find(|c| c == '"' || c == '\'') {
        let quote = rest[open..].chars().next().unwrap_or('"');
        let Some(len) = rest[open + 1..].find(quote) else { return t.to_string() };
        let body = &rest[open + 1..open + 1 + len];
        let mut cells = Vec::new();
        for tok in body.split_whitespace() {
            let mut run = String::new();
            for ch in tok.chars() {
                if ch == '.' {
                    if !run.is_empty() && !run.ends_with('.') {
                        cells.push(std::mem::take(&mut run));
                    }
                    if !run.ends_with('.') {
                        run.push('.');
                    }
                } else {
                    if run.ends_with('.') {
                        cells.push(std::mem::take(&mut run));
                    }
                    run.push(ch);
                }
            }
            if !run.is_empty() {
                cells.push(run);
            }
        }
        rows.push(format!("\"{}\"", cells.join(" ")));
        rest = &rest[open + 1 + len + 1..];
    }
    if rows.is_empty() { t.to_string() } else { rows.join(" ") }
}

fn box_shorthand(name: &str) -> Option<[&'static str; 4]> {
    Some(match name {
        "margin" => ["margin-top", "margin-right", "margin-bottom", "margin-left"],
        "padding" => ["padding-top", "padding-right", "padding-bottom", "padding-left"],
        "inset" => ["top", "right", "bottom", "left"],
        "border-width" => ["border-top-width", "border-right-width", "border-bottom-width", "border-left-width"],
        "border-style" => ["border-top-style", "border-right-style", "border-bottom-style", "border-left-style"],
        "border-color" => ["border-top-color", "border-right-color", "border-bottom-color", "border-left-color"],
        "scroll-margin" => ["scroll-margin-top", "scroll-margin-right", "scroll-margin-bottom", "scroll-margin-left"],
        "scroll-padding" => ["scroll-padding-top", "scroll-padding-right", "scroll-padding-bottom", "scroll-padding-left"],
        _ => return None,
    })
}

/// CSSOM's shortest four-side serialization: one value when all agree, two when top/bottom and
/// right/left pair up, three when only right/left do.
fn shorten_sides(t: &str, r: &str, b: &str, l: &str) -> String {
    if t == r && r == b && b == l {
        t.to_string()
    } else if t == b && r == l {
        format!("{t} {r}")
    } else if r == l {
        format!("{t} {r} {b}")
    } else {
        format!("{t} {r} {b} {l}")
    }
}

/// [`css_computed_one`] for a standard property of a resolved target.
fn css_computed_one_at(state: &mut OpState, node: u32, pseudo: Option<&str>, name: &str) -> String {
    let Some(property) = PropertyId::from_name(name) else { return String::new() };
    // A property the cascade knows but `getComputedStyle` does not ENUMERATE is not readable either:
    // `background-position-x` is a name a stylesheet writes and Chrome folds away, and the eager pack
    // -- built from `ENUMERATED` -- never carried one, so neither does this.
    //
    // Membership is a bitmap lookup in the realm's [`ComputedTable`] -- never a scan: it was
    // `ENUMERATED.contains(&property)` for one round of this lane, a linear scan of 475 on the hottest
    // path in the file that cost more than the lookup it guarded.
    if !enumerated(state, property) {
        return String::new();
    }
    css_computed_property(state, node, pseudo, property)
}

/// The computed value of `property` on a resolved target, whether or not `getComputedStyle` enumerates it --
/// `background-position-x` and `page` answer though Chrome lists neither.
fn css_computed_property(state: &mut OpState, node: u32, pseudo: Option<&str>, property: PropertyId) -> String {
    let value = with_resolved(state, node, resolved::resolves_from_layout(property), String::new(), |p, dom, media, laid_out| {
        let read = |property| match pseudo {
            Some(ps) => p.computed_pseudo_value(dom, node, ps, media, property, laid_out).unwrap_or_default(),
            None => p.computed_value(dom, node, media, property, laid_out).unwrap_or_default(),
        };
        let value = read(property);
        // a shadow's `currentcolor` as this element's `color` (`page::style::computed::current_color_in`)
        page::style::computed::current_color_in(property, &value, || read(PropertyId::Color)).unwrap_or(value)
    });
    // A `url()` the cascade could not resolve -- one written in a sheet with no URL of its own, a
    // `<style>` or a `style` attribute -- resolves against the DOCUMENT's base, as Chrome reports it:
    // `url("http://127.0.0.1:…/pseudo-bg.png")` for `url(pseudo-bg.png)`.
    if value.contains("url(") {
        let base = crate::core::dom::base_url_of(state);
        if let Some(abs) = page::style::urls::absolutize(&value, &base) {
            return abs;
        }
    }
    value
}

/// `computedStyleMap().get(name)`'s value: the COMPUTED value of a longhand -- `50%`, not
/// the used `500px` -- or `None` for a name that is not one, which the caller answers from the
/// resolved declaration as before.
pub(crate) fn css_computed_phase_one(state: &mut OpState, node: u32, name: &str) -> Option<String> {
    let property = PropertyId::from_name(name)?;
    // The values `css_computed_one` writes rather than reads off the cascade -- the fill layers' positions and
    // lists, `border-spacing` and its halves, `page` -- are their computed values already; a raw cascade read
    // would be the stored whole `background-position` this cascade no longer keeps.
    if fill_layer_image(property).is_some()
        || matches!(
            property,
            PropertyId::BackgroundPosition
                | PropertyId::MaskPosition
                | PropertyId::BorderSpacing
                | PropertyId::WebkitBorderHorizontalSpacing
                | PropertyId::WebkitBorderVerticalSpacing
                | PropertyId::Page
        )
    {
        return None;
    }
    if !enumerated(state, property) {
        return Some(String::new());
    }
    let (node, pseudo) = target_of(state, node);
    if pseudo.is_some() {
        return None;
    }
    Some(with_resolved(state, node, false, String::new(), |p, dom, media, _| {
        // the style made first, then read as computed (`page::style::computed::as_computed`): a styleless border side's
        // width as it computed (Chrome 152, `bwt.html`: `CSSUnitValue` `10px`, where `getComputedStyle` reads `0px`)
        let _ = p.computed_style(dom, node, media);
        let read = |property| page::style::computed::as_computed(|| p.computed_phase_value(dom, node, media, property)).unwrap_or_default();
        let value = read(property);
        // and a shadow's `currentcolor` as this element's `color`, as `getComputedStyle` reads it (Chrome 152,
        // `fix1.html`: `rgb(0, 0, 255) 1px 1px 0px 0px` under `color: blue`)
        page::style::computed::current_color_in(property, &value, || read(PropertyId::Color)).unwrap_or(value)
    }))
}

/// The names `getComputedStyle` enumerates in this realm, in order, with a membership bitmap: the
/// persona's list when its bundle recorded one (Chrome 150 enumerates 474, the
/// Chromium-151 browsers 475 with `ruby-overhang`), else the engine's table, recorded from Brave 151.
/// Built once per realm state; the persona does not change under a document.
struct ComputedTable {
    list: Vec<PropertyId>,
    member: Vec<bool>,
    /// The device is a Chromium before 151, whose unset `position-anchor` computes `none` (151's is `normal`).
    anchor_none: bool,
}

fn computed_table(state: &mut OpState) -> std::rc::Rc<ComputedTable> {
    if let Some(t) = state.try_borrow::<std::rc::Rc<ComputedTable>>() {
        return t.clone();
    }
    let persona = crate::binding::protos::persona_handle(state);
    let names = persona.as_ref().and_then(|h| h.0.computed_style_properties());
    let anchor_none = persona.as_ref().and_then(|h| chromium_major(&h.0.navigator().user_agent)).is_some_and(|m| m < 151);
    let list: Vec<PropertyId> = match names {
        Some(n) => n.iter().filter_map(|name| PropertyId::from_name(name)).collect(),
        None => page::style::property::ENUMERATED.to_vec(),
    };
    let mut member = vec![false; page::style::property::COUNT];
    for p in &list {
        member[p.index()] = true;
    }
    let t = std::rc::Rc::new(ComputedTable { list, member, anchor_none });
    state.put(t.clone());
    t
}

/// The Chromium major version a user agent string names (`Chrome/150.0.7871.46` is 150).
fn chromium_major(ua: &str) -> Option<u32> {
    ua.split("Chrome/").nth(1)?.split('.').next()?.parse().ok()
}

/// Whether `getComputedStyle` enumerates `property` at all. See [`css_computed_one`].
fn enumerated(state: &mut OpState, property: PropertyId) -> bool {
    computed_table(state).member[property.index()]
}

/// `getComputedStyle(el).length` -- the enumerated table plus this element's custom properties.
pub(crate) fn css_computed_len(state: &mut OpState, node: u32) -> u32 {
    let (node, pseudo) = target_of(state, node);
    let table = computed_table(state).list.len();
    with_resolved(state, node, false, 0, |p, dom, media, _| {
        let customs = match &pseudo {
            Some(ps) => p.computed_pseudo_custom_names(dom, node, ps, media).len(),
            None => p.computed_custom_names(dom, node, media).len(),
        };
        (table + customs) as u32
    })
}

/// The name at `index`, as `item(i)` and `getComputedStyle(el)[i]` answer: the enumerated table in
/// its own order, then the custom properties. `""` past the end, which is what `item` returns.
pub(crate) fn css_computed_name_at(state: &mut OpState, node: u32, index: u32) -> String {
    let (node, pseudo) = target_of(state, node);
    let t = computed_table(state);
    let table = t.list.as_slice();
    if let Some(ps) = pseudo {
        return with_resolved(state, node, false, String::new(), |p, dom, media, _| match table.get(index as usize) {
            Some(property) => property.name().to_string(),
            None => p.computed_pseudo_custom_names(dom, node, &ps, media).get(index as usize - table.len()).cloned().unwrap_or_default(),
        });
    }
    // The first `table.len()` names are a STATIC table and need no style at all -- but they need the
    // element to be connected, because a detached element enumerates nothing.
    with_resolved(state, node, false, String::new(), |p, dom, media, _| match table.get(index as usize) {
        Some(property) => property.name().to_string(),
        None => p
            .computed_custom_names(dom, node, media)
            .get(index as usize - table.len())
            .cloned()
            .unwrap_or_default(),
    })
}

/// `replaceSync` / a `<style>` element's text changing: throw the rules away and parse afresh.
pub(crate) fn sheet_replace(state: &mut OpState, sheet: u32, text: &str) {
    if let Some(p) = page(state) {
        p.with_sheet_mut(sheet, |s| s.replace_sync(text));
    }
}

// ── RULES BY PATH ─────────────────────────────────────────────────────────────────
//
// A rule is addressed by its PATH from the sheet's top-level list -- `[i]` for `cssRules[i]`,
// `[i, j]` for `cssRules[i].cssRules[j]` -- because `@media`, `@supports`, `@layer`, `@import`,
// `@keyframes` and a nesting style rule all hold rules. The list a path's rule sits in is the path
// without its last index. Every function answers as an empty sheet would when a path does not
// address anything, because the table body has already bounds-checked.

/// How many rules are in the list at `parent` (the sheet's own list for an empty path).
pub(crate) fn list_len(state: &mut OpState, sheet: u32, parent: &[u32]) -> u32 {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.list_len(parent).unwrap_or(0) as u32)).unwrap_or(0)
}

/// `cssText` of the rule at `path`.
pub(crate) fn rule_text(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.css_text_at(path))).unwrap_or_default()
}

/// `selectorText` of the rule at `path`.
pub(crate) fn rule_selector(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.selector_text_at(path))).unwrap_or_default()
}

/// `style.cssText` of the rule at `path` — the declaration block without its braces, whether the
/// rule keeps its declarations in the sheet's pools (a style rule) or owns them (a descriptor
/// at-rule).
pub(crate) fn rule_style_text(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.rule_block_text(path))).unwrap_or_default()
}

/// Which interface the rule at `path` is (`page::style::cssom::RULE_*`).
pub(crate) fn rule_kind(state: &mut OpState, sheet: u32, path: &[u32]) -> u32 {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.rule_kind_code(path))).unwrap_or(0)
}

/// An at-rule's `name`.
pub(crate) fn at_rule_name(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.at_rule_name(path))).unwrap_or_default()
}

/// `name`'s setter. `false` means the text does not fit the rule's name grammar, which leaves the
/// rule's name alone rather than throwing.
pub(crate) fn at_rule_set_name(state: &mut OpState, sheet: u32, path: &[u32], name: &str) -> bool {
    page(state).and_then(|p| p.with_sheet_mut(sheet, |s| s.set_at_rule_name(path, name))).unwrap_or(false)
}

/// One descriptor off an at-rule's block, by its dashed CSS name.
pub(crate) fn descriptor(state: &mut OpState, sheet: u32, path: &[u32], name: &str) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.descriptor(path, name))).unwrap_or_default()
}

/// A descriptor's setter. Nothing is reported back: CSSOM drops a value it cannot parse silently,
/// and the page observes the drop by reading the descriptor again.
pub(crate) fn set_descriptor(state: &mut OpState, sheet: u32, path: &[u32], name: &str, value: &str) {
    if let Some(p) = page(state) {
        p.with_sheet_mut(sheet, |s| s.set_descriptor(path, name, value));
    }
}

/// A style rule's own declarations replaced, its selectors and nested rules kept
/// (`page::style::cssom::Stylesheet::set_style_block`). `false` when the rule at `path` is not a style rule.
pub(crate) fn set_style_block(state: &mut OpState, sheet: u32, path: &[u32], text: &str) -> bool {
    page(state).and_then(|p| p.with_sheet_mut(sheet, |s| s.set_style_block(path, text))).unwrap_or(false)
}

/// `selectorText`'s setter. `false` means the selector did not parse, which the spec says leaves
/// the rule alone — it is not an error the page sees.
pub(crate) fn rule_set_selector(state: &mut OpState, sheet: u32, path: &[u32], text: &str) -> bool {
    page(state).and_then(|p| p.with_sheet_mut(sheet, |s| s.set_selector_text(path, text))).unwrap_or(false)
}

/// `conditionText` of a `@media` / `@supports` rule.
pub(crate) fn condition_text(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.condition_text(path))).unwrap_or_default()
}

/// One property of a rule's declaration block, as its `CSSStyleDeclaration` reads it.
pub(crate) fn rule_property_value(state: &mut OpState, sheet: u32, path: &[u32], name: &str) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.rule_property_value(path, name))).unwrap_or_default()
}

/// The property names of a rule's declaration block (`rule.style.length` / `item(i)`).
pub(crate) fn rule_property_names(state: &mut OpState, sheet: u32, path: &[u32]) -> Vec<String> {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.rule_property_names(path))).unwrap_or_default()
}

/// `CSSContainerRule.containerName` / `containerQuery`.
pub(crate) fn container_parts(state: &mut OpState, sheet: u32, path: &[u32]) -> (String, String) {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.container_parts(path))).unwrap_or_default()
}

/// `CSSScopeRule.start` / `end`.
pub(crate) fn scope_parts(state: &mut OpState, sheet: u32, path: &[u32]) -> (Option<String>, Option<String>) {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.scope_parts(path))).unwrap_or_default()
}

/// `CSSPropertyRule`'s four members.
pub(crate) fn property_parts(state: &mut OpState, sheet: u32, path: &[u32]) -> (String, String, bool, Option<String>) {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.property_parts(path))).unwrap_or_default()
}

/// `CSSPageRule.selectorText`.
pub(crate) fn page_selector(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.page_selector(path))).unwrap_or_default()
}

/// `CSS.registerProperty` into the document's registry; `false` when script registered the name already.
pub(crate) fn register_property(state: &OpState, name: &str, reg: page::style::registered::Registration) -> bool {
    page(state).is_some_and(|p| p.register_property(name, reg))
}
/// Whether `CSS.registerProperty` registered `name` in this document.
pub(crate) fn is_script_registered(state: &OpState, name: &str) -> bool {
    page(state).is_some_and(|p| p.is_script_registered(name))
}

/// `CSSPageRule.selectorText`'s setter.
pub(crate) fn set_page_selector(state: &mut OpState, sheet: u32, path: &[u32], text: &str) -> bool {
    page(state).and_then(|p| p.with_sheet_mut(sheet, |s| s.set_page_selector(path, text))).unwrap_or(false)
}

/// `CSSLayerStatementRule.nameList`.
pub(crate) fn layer_name_list(state: &mut OpState, sheet: u32, path: &[u32]) -> Vec<String> {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.layer_name_list(path))).unwrap_or_default()
}

/// `CSSKeyframeRule.keyText` and its setter.
pub(crate) fn key_text(state: &mut OpState, sheet: u32, path: &[u32]) -> String {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.key_text(path))).unwrap_or_default()
}
pub(crate) fn set_key_text(state: &mut OpState, sheet: u32, path: &[u32], text: &str) -> bool {
    page(state).and_then(|p| p.with_sheet_mut(sheet, |s| s.set_key_text(path, text))).unwrap_or(false)
}

/// `CSSKeyframesRule.findRule`.
pub(crate) fn find_keyframe(state: &mut OpState, sheet: u32, path: &[u32], key: &str) -> Option<u32> {
    page(state).and_then(|p| p.with_sheet(sheet, |s| s.find_keyframe(path, key))).flatten().map(|i| i as u32)
}

/// What an `@import` rule says: `(href, media text, layer name, supports text, loaded url)`.
pub(crate) fn import_info(
    state: &mut OpState,
    sheet: u32,
    path: &[u32],
) -> Option<(String, String, Option<String>, Option<String>, Option<String>)> {
    use page::style::rules::RuleKind;
    page(state)
        .and_then(|p| {
            p.with_sheet(sheet, |s| match s.rule_at(path).map(|r| &r.kind) {
                Some(RuleKind::Import(i)) => Some((
                    i.href.clone(),
                    i.media.as_ref().map(|m| m.1.clone()).unwrap_or_default(),
                    i.layer.clone(),
                    i.supports.clone(),
                    i.loaded.then(|| i.url.clone()).flatten(),
                )),
                _ => None,
            })
        })
        .flatten()
}

/// Insert `text` into the list at `parent`. Returns the index it landed at, or one of the negative
/// codes above.
pub(crate) fn insert_rule_at(state: &mut OpState, sheet: u32, parent: &[u32], text: &str, index: u32) -> i32 {
    let Some(p) = page(state) else { return ERR_NO_PIPELINE };
    match p.with_sheet_mut(sheet, |s| s.insert_rule_at(parent, text, index as usize)) {
        Some(Ok(i)) => i as i32,
        Some(Err(CssomError::IndexSize)) | None => ERR_INDEX_SIZE,
        Some(Err(CssomError::Syntax)) => ERR_SYNTAX,
        Some(Err(CssomError::Hierarchy)) => ERR_HIERARCHY,
    }
}

/// Delete the rule at `index` of the list at `parent`. `false` means the index was out of bounds.
pub(crate) fn delete_rule_at(state: &mut OpState, sheet: u32, parent: &[u32], index: u32) -> bool {
    page(state)
        .and_then(|p| p.with_sheet_mut(sheet, |s| s.delete_rule_at(parent, index as usize).is_ok()))
        .unwrap_or(false)
}

/// `window.matchMedia(query).matches`.
///
/// The SAME evaluator the cascade uses for `@media`, over the same persona-derived
/// [`media_context`] — so a page that asks `matchMedia("(min-width: 600px)")` and a page that writes
/// `@media (min-width: 600px)` can never be told two different things. `matchMedia` used to answer
/// `false` for every query, which is a one-line detect: `matchMedia("(min-width: 0px)").matches` is
/// `true` on every browser that has ever shipped.
///
/// No pipeline is needed — parsing and matching a query list are pure functions of the text and the
/// media context — so this keeps working on an engine built without `korvus-page`'s cascade.
/// A query that does not parse becomes `not all`, i.e. `false`, which is what CSS asks for.
/// NOT an op any more. Its only caller was `host.js`'s `__wireMQL` block, which is the
/// `MediaQueryList` table (`core/css/media_query_list.rs`) now and calls this directly.
pub(crate) fn media_matches(scope: &mut v8::PinScope, query: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ctx = { let st = op_state.borrow(); media_context(&st) };
    page::style::media::MediaQueryList::parse(query, None).matches(&ctx, &[])
}

/// [`media_matches`] for the `MediaQueryList` `list`, against the viewport of the window that made it:
/// a frame's is its element's content box, the viewport its own styles resolve against, laid
/// out first as they are. It was always the page's: a 300px-wide frame's `matchMedia("(min-width: 400px)")`
/// matched, and Chrome 150 answers false until the page makes the frame 400px wide -- and true straight after
/// it does, before any script reads a layout (`tools/realweb/seed/framemedia`).
pub(crate) fn media_matches_for(scope: &mut v8::PinScope, query: &str, list: v8::Local<v8::Object>) -> bool {
    let frame = match list.get_creation_context(scope) {
        Some(ctx) => {
            let g = ctx.global(scope);
            crate::core::html::browsing_context::realm_frame_document_root(scope, g)
        }
        None => None,
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ctx = {
        let mut st = op_state.borrow_mut();
        match frame {
            Some(Some(root)) => crate::core::dom::geometry_queries::frame_media(&mut st, root).unwrap_or_else(|| media_context(&st)),
            _ => media_context(&st),
        }
    };
    page::style::media::MediaQueryList::parse(query, None).matches(&ctx, &[])
}

/// `window.matchMedia(query).media` -- the CSSOM serialization of a media query list, and an `@media` rule's
/// `conditionText` and `media.mediaText`.
///
/// Blink's own serializer over Blink's own parse (`page::style::media`): `MediaQuery::Serialize`
/// and `MediaQueryExp::Serialize`. This was a NORMALIZATION of the text -- lowercased, whitespace collapsed --
/// because the media AST kept too little to serialize from, and the normalization was readable: Chrome 150 writes
/// `(min-aspect-ratio: 16/9)` as `(min-aspect-ratio: 16 / 9)`, `(min-width: 1e3px)` as `(min-width: 1000px)`,
/// `screen,` as `screen, not all`, an unknown `(  foo  :  bar  )` exactly as written, and `(color) or (hover)`
/// (which did not parse here at all) as it is.
/// NOT an op any more, for the same reason [`media_matches`] is not.
pub(crate) fn media_serialize(scope: &mut v8::PinScope, query: &str) -> String {
    let _ = scope;
    page::style::media::serialize(query)
}

// THIS FILE OWNS NO NATIVE OPS, so there is no `webapi_ops![]` declaration here for `build.rs` to
// collect. It is the end of a sequence, not a sudden decision. Then `op_media_matches` was removed
// and `op_media_serialize` (their only caller was `host.js`'s `__wireMQL` block, which is the
// `MediaQueryList` table in `core/css/media_query_list.rs` now); W4.j removed `op_css_computed_all`
// and `op_computed_style_decl` from the same file's `getComputedStyle`; W11.5 re-measured the
// sixteen that were still declared, found that not one had a caller of any kind, and removed the
// list with them. An op whose only purpose was to be reachable from JS does not outlive the JS.

/// `Window.getComputedStyle(Element elt, optional CSSOMString? pseudoElt)`.
///
/// WHAT THIS REPLACES: the `__getComputedStyleImpl` block of `core/korvus-runtime/src/js/host.js` --
/// an `Object.defineProperty(globalThis, …)` behind an `op_nativize` mask, in the kernel. Its three
/// ops (`op_node_id_of`, `op_css_computed_all`, `op_computed_style_decl`) had NO OTHER CALLER and are
/// deleted with it; the Rust they wrapped is called directly.
///
/// EVERY DECISION IN THE BODY IS THE JS's, carried over with its reasoning, because every one of them
/// was measured when it was written and re-measured here against `google-chrome --headless=new`
/// 150.0.7871.46 over a `file:` URL:
///
///   * `elt` is REQUIRED, so a bare `getComputedStyle()` is a WebIDL arity TypeError, and `elt` is an
///     interface type, so `null`, a number, a Text node and the window itself are all
///     "parameter 1 is not of type 'Element'." -- all five messages confirmed.
///   * A PSEUDO-ELEMENT argument names a box this engine does not generate (no `::before`, no
///     `::marker`, design D12), and the answer for a pseudo that does not exist is an EMPTY
///     declaration, not the originating element's style: a page testing
///     `getComputedStyle(el, "::nonsense").length === 0` is asking whether the pseudo is there, and
///     127 properties answer yes. Chrome answers 0 for `::nonsense` (measured). Only an argument
///     STARTING WITH A COLON is a pseudo request -- CSSOM ignores a string without one, so
///     `getComputedStyle(el, "before")` is the element's own style, which Chrome confirms.
///   * A DETACHED element has no box and no computed style: Chrome answers `length` 0 and "" for
///     every property on `document.createElement("div")` (measured). This used to hand back all 475.
///     It is asked PER ACCESS and not once here, because Chrome's answer follows the
///     element in BOTH directions: a declaration taken while detached reports `length` 474 the
///     moment the element is appended, and one taken while connected reports 0 after `el.remove()`
///     (both measured on Chrome 150). Deciding it at the call froze the first of those at 0.
///
/// What the row ADDS is the receiver check: `getComputedStyle.call({}, el)` throws
/// `TypeError: Illegal invocation` in Chrome, and the JS body ignored `this`.
pub(crate) fn cb_get_computed_style(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::binding::protos::throw_type_error;
    // NO ARITY CHECK HERE: the row's `required 1` mounts one, and the message is the same sentence
    // (`binding::protos::require_args`). The JS enforced it by hand because there was no row.
    let el = args.get(0);
    // `nodeId` is hidden from the element's public surface on purpose (a real browser has no such
    // member), so the id comes from the binding layer rather than from a property.
    let node_id = crate::binding::protos::unwrap_id(scope, el);
    // The node's KIND, which never changes after the node is created.
    let is_element = node_id.is_some_and(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|d| d.0.node_type(id) == 1)
    });
    // THE BRAND CHECK FIRST, THE PROTOTYPE WALK ONLY WHEN IT MISSES. A wrapper around
    // an element node IS an Element, whatever its prototype chain now says -- which is Chrome's
    // answer too: `Object.setPrototypeOf(el, null); getComputedStyle(el).color` answers a color in
    // Chrome 150 (measured), where a walk of the chain threw. The walk -- a global lookup, a
    // `prototype` get and a climb, ~240 ns of a call Chrome makes in ~250 -- still decides
    // everything else, so `getComputedStyle(Object.create(Element.prototype))` and a non-object
    // answer exactly what they did.
    if !is_element && !crate::install::webidl::is_instance_of(scope, el, "Element") {
        throw_type_error(scope, "Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.");
        return;
    }
    let pseudo = args.get(1);
    let pseudo_text = if pseudo.is_null_or_undefined() { String::new() } else { pseudo.to_rust_string_lossy(scope) };
    let wants_pseudo = pseudo_text.starts_with(':');
    // A pseudo-element Chrome styles is a declaration of its own; any other `:`-string
    // is the always-empty kind.
    let pseudo_name = if wants_pseudo { pseudo_name(&pseudo_text) } else { None };

    // The declaration carries the ELEMENT, not its values. `None` is the always-empty
    // kind -- a pseudo-element this engine generates no box for, or a receiver with no node id.
    // Connectedness is NOT part of that choice any more: it is a question about the element NOW, and
    // every member asks it on access (`with_resolved`).
    let target = match (node_id, wants_pseudo) {
        // "Update animations and send events" -- the step a browser runs before it renders, which this engine
        // runs before anything can OBSERVE instead, having no vsync to hang it on -- is NOT run here any more
        // (a bot-detection service D7). It runs where Blink flushes style: at each computed property READ
        // (`css_style_declaration::computed_value`, `CSSComputedStyleDeclaration::GetPropertyCSSValue` ->
        // `UpdateStyleAndLayoutTreeForElement`), and at every layout read (`animation::css::flush_style_for`).
        // Chrome 150: `getComputedStyle(el)` that reads nothing starts no transition, so the before-change style a
        // later `opacity` change transitions from is not taken by the call -- here it was.
        (Some(id), false) => Some(id),
        (Some(id), true) => pseudo_name.map(|name| {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            pseudo_marker(&mut st, id, &name)
        }),
        _ => None,
    };
    // The result is a real `CSSStyleDeclaration` -- the same platform object `el.style` is, carrying
    // an element to resolve against instead of the element's own `style` attribute. It used to be an
    // object literal of five JS closures in a `Proxy`, so `getComputedStyle(el) instanceof
    // CSSStyleDeclaration` was FALSE (it is true in every browser) and
    // `String(getComputedStyle(el).getPropertyValue)` printed JS source.
    let decl = crate::core::css::css_style_declaration::computed_style_decl(scope, target);
    rv.set(decl.into());
}

#[cfg(test)]
mod print_tests {
    use super::page_ranges;

    #[test]
    fn page_ranges_read_as_chromes_parser_reads_them() {
        assert_eq!(page_ranges("", 3), Ok(vec![0, 1, 2]));
        assert_eq!(page_ranges("1-2, 3", 5), Ok(vec![0, 1, 2]));
        assert_eq!(page_ranges("2-", 4), Ok(vec![1, 2, 3]), "an open end is the last page");
        assert_eq!(page_ranges("-2", 4), Ok(vec![0, 1]), "an open start is the first page");
        assert_eq!(page_ranges("3-9", 4), Ok(vec![2, 3]), "a range runs to the last page there is");
        assert_eq!(page_ranges("5", 4), Err("Page range exceeds page count".to_string()));
        assert_eq!(page_ranges("2-1", 4), Err("Page range syntax error".to_string()));
        assert_eq!(page_ranges("x", 4), Err("Page range syntax error".to_string()));
    }
}

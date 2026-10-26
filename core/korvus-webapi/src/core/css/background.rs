//! `background-image: url(...)` layers the painter reports (`page::paint::ReplacedKind::Background`): fetched as an
//! image the document asked for, decoded -- an SVG rasterized at the size it is drawn, so a scaled icon stays sharp --
//! and laid over the box as the layer's `background-size`, `background-position` and `background-repeat` say (CSS
//! Backgrounds §3). Nothing painted them: every sprite icon and every photo set as a background was missing.
//!
//! The fetch is issued the first time a layer is painted and the image arrives as any image does (`FetchedImages`),
//! which asks for the next frame; until then the layer paints nothing, as Chrome's does until its image has loaded.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use deno_core::v8;
use page::paint::{background_offset, background_repeats, background_tile_size, BackgroundLayer};
use spec::Canvas2DOp;

/// The layers' images, decoded, by URL (and the response they were decoded from, so a new one is decoded again).
#[derive(Default)]
struct CssImages(HashMap<String, (usize, Option<Decoded>)>);

/// The URLs whose fetch this document has issued.
#[derive(Default)]
struct CssRequested(HashSet<String>);

#[derive(Clone)]
enum Decoded {
    Raster(Rc<spec::Rgba8>),
    Svg(Rc<Vec<u8>>, (f64, f64)),
}

/// Inline `<svg>`s rasterized, by their markup and size (the painter serializes one every frame; its pixels are made
/// again only when either changes).
#[derive(Default)]
struct InlineSvgs(HashMap<(u64, u32, u32), Option<Rc<spec::Rgba8>>>);

/// An inline `<svg>`'s pixels at `w` x `h` CSS px (`page::paint::ReplacedKind::Svg`).
pub(crate) fn inline_svg(scope: &mut v8::PinScope, markup: &str, w: f64, h: f64) -> Option<Rc<spec::Rgba8>> {
    let (pw, ph) = (w.round().max(0.0) as u32, h.round().max(0.0) as u32);
    if pw == 0 || ph == 0 {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    markup.hash(&mut hasher);
    let key = (hasher.finish(), pw, ph);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(hit) = op_state.borrow().try_borrow::<InlineSvgs>().and_then(|c| c.0.get(&key).cloned()) {
        return hit;
    }
    let pixels = crate::core::html::canvas_image::rasterize_svg_at(markup.as_bytes(), pw, ph).map(Rc::new);
    let mut st = op_state.borrow_mut();
    if !st.has::<InlineSvgs>() {
        st.put(InlineSvgs::default());
    }
    let cache = st.borrow_mut::<InlineSvgs>();
    if cache.0.len() > 512 {
        cache.0.clear();
    }
    cache.0.insert(key, pixels.clone());
    pixels
}

/// Pattern ids for the page's display list, apart from any a canvas uses.
const PATTERN_BASE: u32 = 0x4000_0000;

/// The ops that paint `layer` over `area` (the painting area: x, y, width, height); empty while its image is not here.
pub(crate) fn layer_ops(scope: &mut v8::PinScope, node: u32, layer: &BackgroundLayer, area: (f64, f64, f64, f64), pattern: &mut u32) -> Vec<Canvas2DOp> {
    // A sheet with a URL of its own made its `url()`s absolute in the cascade; a `style` attribute's and a `<style>`
    // element's resolve against their document's base, as `getComputedStyle` resolves them.
    let base = crate::core::html::subresource::node_base_url(scope, node);
    let Some(url) = url::Url::parse(&base).ok().and_then(|b| b.join(&layer.url).ok()).map(String::from) else { return Vec::new() };
    let Some(image) = image(scope, node, &url, if layer.cors { "cors" } else { "no-cors" }) else { return Vec::new() };
    let (iw, ih) = match &image {
        Decoded::Raster(r) => (r.width as f64, r.height as f64),
        Decoded::Svg(_, size) => *size,
    };
    if iw <= 0.0 || ih <= 0.0 {
        return Vec::new();
    }
    let (ox, oy, ow, oh) = layer.origin;
    let (tw, th) = background_tile_size(&layer.size, Some((iw, ih)), (ow, oh));
    if tw < 0.5 || th < 0.5 {
        return Vec::new();
    }
    let x0 = ox + background_offset(&layer.position_x, ow - tw);
    let y0 = oy + background_offset(&layer.position_y, oh - th);
    let (rx, ry) = background_repeats(&layer.repeat);
    let pixels = match &image {
        Decoded::Raster(r) => (**r).clone(),
        Decoded::Svg(bytes, _) => {
            let Some(r) = crate::core::html::canvas_image::rasterize_svg_at(bytes, tw.round() as u32, th.round() as u32) else { return Vec::new() };
            r
        }
    };
    let (pw, ph) = (pixels.width as f64, pixels.height as f64);
    let (ax, ay, aw, ah) = area;
    let mut ops = vec![Canvas2DOp::Save, Canvas2DOp::BeginPath];
    match layer.radii {
        Some(r) => ops.push(Canvas2DOp::RoundRect { x: ax, y: ay, w: aw, h: ah, radii: r }),
        None => ops.push(Canvas2DOp::Rect { x: ax, y: ay, w: aw, h: ah }),
    }
    ops.push(Canvas2DOp::Clip { even_odd: false });
    if !rx && !ry {
        ops.push(Canvas2DOp::DrawImage { image: pixels, sx: 0.0, sy: 0.0, sw: pw, sh: ph, dx: x0, dy: y0, dw: tw, dh: th });
    } else {
        // a tiled layer is one pattern fill: the tile at (x0, y0), `tw` x `th`, repeated across the area
        let id = PATTERN_BASE + *pattern;
        *pattern += 1;
        let repeat = match (rx, ry) {
            (true, true) => spec::PatternRepeat::Repeat,
            (true, false) => spec::PatternRepeat::RepeatX,
            _ => spec::PatternRepeat::RepeatY,
        };
        ops.push(Canvas2DOp::DefinePattern { id, image: pixels, repeat });
        ops.push(Canvas2DOp::SetPatternTransform { id, t: spec::Transform2D { a: tw / pw, b: 0.0, c: 0.0, d: th / ph, e: x0, f: y0 } });
        ops.push(Canvas2DOp::SetFillPattern { id });
        // a direction that does not repeat covers the one tile's row or column only
        let (fx, fw) = if rx { (ax, aw) } else { (x0, tw) };
        let (fy, fh) = if ry { (ay, ah) } else { (y0, th) };
        ops.push(Canvas2DOp::FillRect { x: fx, y: fy, w: fw, h: fh });
    }
    ops.push(Canvas2DOp::Restore);
    ops
}

/// An image another load of the document already asked for -- a `<video>`'s `poster`, which the element's own
/// loader fetches -- decoded once it has arrived, an SVG at its own size. Never a request of its own: asked before
/// the image is here, it is `None`.
pub(crate) fn already_fetched(scope: &mut v8::PinScope, url: &str, mode: &str) -> Option<Rc<spec::Rgba8>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let f = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()).and_then(|s| s.get(url, mode))
    }?;
    if !(200..300).contains(&f.status) {
        return None;
    }
    let ptr = std::sync::Arc::as_ptr(&f.bytes) as usize;
    let cached = op_state.borrow().try_borrow::<CssImages>().and_then(|c| c.0.get(url).cloned()).filter(|(p, _)| *p == ptr).map(|(_, d)| d);
    let decoded = match cached {
        Some(d) => d,
        None => {
            let d = decode_body(&op_state, &f.bytes, &f.headers);
            let mut st = op_state.borrow_mut();
            if !st.has::<CssImages>() {
                st.put(CssImages::default());
            }
            st.borrow_mut::<CssImages>().0.insert(url.to_string(), (ptr, d.clone()));
            d
        }
    };
    match decoded? {
        Decoded::Raster(r) => Some(r),
        Decoded::Svg(bytes, (w, h)) => crate::core::html::canvas_image::rasterize_svg_at(&bytes, w.round().max(1.0) as u32, h.round().max(1.0) as u32).map(Rc::new),
    }
}

/// The image at `url`, fetched in `mode` (a background's `no-cors`, a mask's `cors`), decoded; its fetch issued (once)
/// when it is not here yet.
fn image(scope: &mut v8::PinScope, node: u32, url: &str, mode: &str) -> Option<Decoded> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A `data:` image is its own body (Fetch's data: URL processor), decoded here: no request reaches the network
    // layer that records image responses, so it waited for one forever -- minea.com's logo mark, every inline icon.
    if url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
        if let Some((_, d)) = op_state.borrow().try_borrow::<CssImages>().and_then(|c| c.0.get(url).cloned()) {
            return d;
        }
        let decoded = crate::modules::url::url::decode_data_url(url)
            .and_then(|(mime, body)| decode_body(&op_state, &body, &[("content-type".to_string(), mime)]));
        let mut st = op_state.borrow_mut();
        if !st.has::<CssImages>() {
            st.put(CssImages::default());
        }
        st.borrow_mut::<CssImages>().0.insert(url.to_string(), (0, decoded.clone()));
        return decoded;
    }
    let fetched = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()).and_then(|s| s.get(url, mode))
    };
    let Some(f) = fetched else {
        let first = {
            let mut st = op_state.borrow_mut();
            if !st.has::<CssRequested>() {
                st.put(CssRequested::default());
            }
            st.borrow_mut::<CssRequested>().0.insert(url.to_string())
        };
        let web = url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "data" | "blob"));
        if first && web {
            // requested by the element's own document: a frame's image is the frame's request -- its client, its
            // referrer, its policies -- not the page's (reCAPTCHA's logo, a gstatic image in a google.com frame)
            let entered = crate::core::html::browsing_context::enter_element_context(scope, node);
            let _ = crate::core::html::subresource::start_shared_fetch(scope, url, "image", mode);
            crate::core::html::browsing_context::leave_element_context(scope, entered);
        }
        return None;
    };
    if !(200..300).contains(&f.status) {
        return None;
    }
    let ptr = std::sync::Arc::as_ptr(&f.bytes) as usize;
    if let Some((p, d)) = op_state.borrow().try_borrow::<CssImages>().and_then(|c| c.0.get(url).cloned()) {
        if p == ptr {
            return d;
        }
    }
    let decoded = decode_body(&op_state, &f.bytes, &f.headers);
    let mut st = op_state.borrow_mut();
    if !st.has::<CssImages>() {
        st.put(CssImages::default());
    }
    st.borrow_mut::<CssImages>().0.insert(url.to_string(), (ptr, decoded.clone()));
    decoded
}

/// An image response's body decoded: an SVG kept as its markup (rasterized at the size it is drawn), anything else
/// through the render backend.
fn decode_body(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, bytes: &[u8], headers: &[(String, String)]) -> Option<Decoded> {
    if crate::core::html::canvas_image::is_svg(bytes, headers) {
        return crate::core::html::canvas_image::svg_size(bytes).map(|size| Decoded::Svg(Rc::new(bytes.to_vec()), size));
    }
    if crate::core::html::canvas_image::chrome_decodes_but_we_cannot(bytes, headers) {
        return None;
    }
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    backend.and_then(|b| b.decode_frame(bytes)).map(|(i, _)| Decoded::Raster(Rc::new(i)))
}

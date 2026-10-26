//! `CanvasImageSource` -- what `drawImage`, `createPattern`, `createImageBitmap` and `texImage2D` are
//! handed, the pixels behind it, and whether those pixels TAINT the canvas they are drawn into.
//!
//! # What was here
//!
//! `canvas_2d::image_source`, which answered pixels for a `<canvas>` and `None` for everything else,
//! on the reasoning that "an `<img>` has no pixels here (there is no image loader)". There IS a loader
//! -- `core/html/subresource.rs` fetches every `<img src>` -- and its bytes were dropped after the
//! status decided `load`. So, measured on the release binary at `8f5aee08` against stock Chrome
//! 150.0.7871.46 over a loopback page:
//!
//! ```text
//! <img src=red.png> onload, drawImage(img,0,0), getImageData   korvus 0,0,0,0     chrome 255,0,0,255
//! cross-origin <img>, drawImage, toDataURL()                    korvus a PNG      chrome SecurityError
//! drawImage({}, 0, 0) / drawImage(null, 0, 0)                   korvus nothing    chrome TypeError
//! drawImage(img404, 0, 0)                                       korvus nothing    chrome InvalidStateError
//! typeof createImageBitmap                                      korvus undefined  chrome function
//! ```
//!
//! A canvas that stays blank where a page drew an image is itself a tell, and the one a fingerprinting
//! script reads first.
//!
//! # Where the pixels come from
//!
//! The kernel's subresource loader keeps every `image` response it fetches in
//! [`spec::FetchedImages`], keyed by `(url, mode)` -- the key of HTML's list of available images. An
//! `<img>` is looked up by its own `src` resolved against the document's base URL and its
//! `crossorigin` attribute, and the body is decoded ONCE by the render backend
//! ([`spec::RenderBackend::decode_image`], Skia), then kept decoded. Nothing here makes a pixel up:
//! a body this build cannot decode is never painted as anything.
//!
//! # The states, measured
//!
//! ```text
//! <img> with no src, or not yet fetched     drawImage draws nothing; createPattern null;
//!                                           createImageBitmap InvalidStateError "not usable"
//! <img> 404 / not an image / CORS failed    InvalidStateError "... in the 'broken' state."
//! <canvas> with a 0 dimension               InvalidStateError "... canvas element with a width or height of 0."
//! closed ImageBitmap                        InvalidStateError "The image source is detached"
//! <video> never played                      draws nothing (R7: no frame is ever decoded)
//! anything not in the union                 TypeError "The provided value is not of type '(CSSImageValue or ...)'"
//! ```
//!
//! # Tainting, measured
//!
//! ```text
//! cross-origin <img>, no crossorigin                  taints (getImageData/toDataURL/toBlob SecurityError)
//! cross-origin <img>, no crossorigin, ACAO: *         taints -- the request was no-cors
//! cross-origin <img crossorigin=anonymous>, ACAO: *   clean
//! cross-origin <img crossorigin=anonymous>, no ACAO   broken: `error` fires, drawImage InvalidStateError
//! same-origin <img crossorigin=anonymous>             clean
//! a tainted <canvas> / ImageBitmap / pattern drawn    taints (a pattern taints on ASSIGNMENT to fill/strokeStyle)
//! a zero-size draw of a cross-origin image            does NOT taint (nothing was drawn)
//! ctx.reset()                                         stays tainted
//! canvas.width = canvas.width, setAttribute("width")  clean again (the bitmap is reset)
//! createImageBitmap(crossOriginImg)                   resolves; the bitmap taints what it is drawn into
//! texImage2D(tainted source)                          SecurityError, nothing uploaded
//! ```

use deno_core::v8;
use spec::{Canvas2DOp, RenderParams, Rgba8};
use std::collections::HashMap;
use std::rc::Rc;

use super::html_canvas_element::{priv_get, priv_set, CANVAS_CTX_KIND_PRIV, CANVAS_CTX_PRIV};
use crate::binding::protos::{get_prop, int_val, throw_type_error};
use crate::binding::HostDom;

/// The union `drawImage` and `createPattern` accept, as Chrome words it.
const DRAW_UNION: &str = "(CSSImageValue or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or OffscreenCanvas or SVGImageElement or VideoFrame)";
/// The union `createImageBitmap` accepts.
const BITMAP_UNION: &str = "(Blob or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or ImageData or OffscreenCanvas or SVGImageElement or VideoFrame)";

/// An `ImageBitmap`'s id, on the object.
pub(crate) const BITMAP_ID_PRIV: &str = "korvus_image_bitmap_id";
/// An `OffscreenCanvas`'s brand (its width slot, written by the constructor).
const OFFSCREEN_BRAND_PRIV: &str = "korvus.offscreenWidth";

// ── what a value is ─────────────────────────────────────────────────────────────────────────────

/// One member of the image-source unions, identified by its INTERNAL brand (a node's tag, a private
/// slot) and never by a property a page could forge.
pub(crate) enum Kind {
    Img(u32),
    Canvas,
    Offscreen,
    Bitmap(u32),
    Video,
    SvgImage,
    VideoFrame,
    ImageData,
    Blob(Vec<u8>),
    Other,
}

pub(crate) fn classify(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Kind {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return Kind::Other };
    if let Some(id) = crate::binding::protos::unwrap_id(scope, v) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let (tag, ns) = {
            let st = op_state.borrow();
            match st.try_borrow::<HostDom>() {
                Some(h) => (
                    h.0.tag_name(id).map(|t| t.to_ascii_lowercase()).unwrap_or_default(),
                    h.0.namespace_uri(id).unwrap_or("").to_string(),
                ),
                None => (String::new(), String::new()),
            }
        };
        let svg = ns == "http://www.w3.org/2000/svg";
        return match tag.as_str() {
            "img" if !svg => Kind::Img(id),
            "canvas" if !svg => Kind::Canvas,
            "video" if !svg => Kind::Video,
            "image" if svg => Kind::SvgImage,
            _ => Kind::Other,
        };
    }
    if priv_get(scope, obj, OFFSCREEN_BRAND_PRIV).is_some() {
        return Kind::Offscreen;
    }
    if let Some(id) = priv_get(scope, obj, BITMAP_ID_PRIV).and_then(|v| v.uint32_value(scope)) {
        return Kind::Bitmap(id);
    }
    if crate::install::webidl::has_brand(scope, v, "VideoFrame") {
        return Kind::VideoFrame;
    }
    if crate::install::webidl::has_brand(scope, v, "ImageData") {
        return Kind::ImageData;
    }
    if let Some(b) = crate::core::fileapi::blob::blob_bytes(scope, v) {
        return Kind::Blob(b);
    }
    Kind::Other
}

// ── an <img>'s pixels ───────────────────────────────────────────────────────────────────────────

/// Where an `<img>` stands, as far as a canvas can see it.
pub(crate) enum ImgState {
    /// No `src`, not fetched yet, or a format this build cannot decode (see [`img_state`]).
    Unavailable,
    /// Fetched and failed: a network error, a non-2xx, a failed CORS check, or bytes that are no image.
    Broken,
    /// Its header parsed, so it has a size and is not broken: the frame as decoded (transparent black where it
    /// is not), `clean` = origin-clean, `complete` = the decode completed. Blink draws an INCOMPLETE frame with
    /// what it has and `createImageBitmap` refuses one (measured on stock Chrome 150).
    Ready(Rc<Rgba8>, bool, bool),
}

/// Decoded image bodies, by `(url, mode)`, with the `Arc` of the bytes they were decoded from (a
/// re-fetch of the same url replaces the body, and the pointer says so): the frame and whether its decode
/// completed, or `None` for bytes whose header does not parse (a broken image).
#[derive(Default)]
struct DecodedImages(HashMap<(String, String), (usize, Option<(Rc<Rgba8>, bool)>)>);

/// The origin of a URL, serialized, or `None` for an opaque one.
fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let o = u.origin();
    o.is_tuple().then(|| o.ascii_serialization())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// A body whose FORMAT Chrome decodes and this build cannot. There is none now: WebP and AVIF decode
/// (`korvus-render`'s `modern_decode`), where they used to read as not available and draw nothing.
pub(crate) fn chrome_decodes_but_we_cannot(_bytes: &[u8], _headers: &[(String, String)]) -> bool {
    false
}

pub(crate) fn is_svg(bytes: &[u8], headers: &[(String, String)]) -> bool {
    transport::mime::mime_type_and_charset(headers).mime == "image/svg+xml"
        || bytes.windows(4).take(512).any(|w| w == b"<svg")
}

/// How an SVG's `<image>` hrefs are resolved: a data URL is decoded as what its bytes are, whatever type it claims --
/// Chrome sniffs, and a Lottie animation's WebP frames come labelled `image/png` -- and any other href is refused:
/// resvg's default would read it from the local filesystem, which a page's SVG must never reach (a canvas reads its
/// pixels back).
fn svg_options() -> resvg::usvg::Options<'static> {
    use resvg::usvg::{ImageHrefResolver, ImageKind, Options};
    let mut options = Options::default();
    options.image_href_resolver = ImageHrefResolver {
        resolve_data: Box::new(|mime: &str, data: std::sync::Arc<Vec<u8>>, opts: &Options| {
            let b = data.as_slice();
            if b.starts_with(b"\x89PNG\r\n\x1a\n") {
                Some(ImageKind::PNG(data))
            } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
                Some(ImageKind::JPEG(data))
            } else if b.starts_with(b"GIF8") {
                Some(ImageKind::GIF(data))
            } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
                Some(ImageKind::WEBP(data))
            } else {
                (ImageHrefResolver::default_data_resolver())(mime, data, opts)
            }
        }),
        resolve_string: Box::new(|_: &str, _: &Options| None),
    };
    options
}

/// An SVG image, rasterized at its natural size: `createImageBitmap(svgImg)` rejected
/// "The image source is not usable" and `drawImage` painted nothing -- play2048.co draws its board from
/// an SVG data URL. Chrome 150 (`tools/realweb/seed/svi`): a 40x20 bitmap, red and blue halves.
///
/// `Err(())`: an SVG whose natural size is too large to rasterize whole ([`SVG_RASTER_MAX`]) -- read as a format this
/// build does not decode ([`ImgState::Unavailable`]: its size from its header). boots.com's carried one of 195 GB,
/// and the allocation aborted the process.
fn rasterize_svg(bytes: &[u8]) -> Result<Option<spec::Rgba8>, ()> {
    let Ok(tree) = resvg::usvg::Tree::from_data(bytes, &svg_options()) else { return Ok(None) };
    let size = tree.size().to_int_size();
    if size.width() as u64 * size.height() as u64 > SVG_RASTER_MAX {
        return Err(());
    }
    Ok(render_svg(&tree, size.width(), size.height()))
}

/// The most pixels an SVG is rasterized at (64 MB of RGBA): Blink draws an SVG image as a picture at the size it is
/// painted, never as a bitmap of its natural size, so no size refuses it there; a bitmap here is made whole.
const SVG_RASTER_MAX: u64 = 16 << 20;

/// An SVG image's natural size, in CSS px.
pub(crate) fn svg_size(bytes: &[u8]) -> Option<(f64, f64)> {
    let tree = resvg::usvg::Tree::from_data(bytes, &svg_options()).ok()?;
    let s = tree.size();
    Some((s.width() as f64, s.height() as f64))
}

/// An SVG image rasterized at `w`x`h` -- the size it is drawn at, so a scaled icon stays sharp (a CSS background).
pub(crate) fn rasterize_svg_at(bytes: &[u8], w: u32, h: u32) -> Option<spec::Rgba8> {
    let tree = resvg::usvg::Tree::from_data(bytes, &svg_options()).ok()?;
    render_svg(&tree, w.max(1), h.max(1))
}

fn render_svg(tree: &resvg::usvg::Tree, w: u32, h: u32) -> Option<spec::Rgba8> {
    // `Pixmap::new` refuses only a size whose byte count overflows: past that, the allocation aborts the process
    if w as u64 * h as u64 > SVG_RASTER_MAX {
        return None;
    }
    let natural = tree.size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    let t = resvg::tiny_skia::Transform::from_scale(w as f32 / natural.width(), h as f32 / natural.height());
    resvg::render(tree, t, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied pixels; the canvas takes them straight, as Skia's decode hands them.
    let data = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Some(spec::Rgba8 { width: w, height: h, data })
}

/// An image's intrinsic size read off its encoded header, for the formats the decoder here does not
/// decode (WebP, AVIF, SVG) -- enough for `naturalWidth`/`naturalHeight`, which need no pixels.
pub(crate) fn header_dimensions(bytes: &[u8], headers: &[(String, String)]) -> Option<(u32, u32)> {
    let le16 = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]) as u32;
    let le24 = |b: &[u8]| (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16);
    if bytes.len() >= 30 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return match &bytes[12..16] {
            b"VP8 " => Some((le16(&bytes[26..28]) & 0x3fff, le16(&bytes[28..30]) & 0x3fff)),
            b"VP8L" => {
                let b = &bytes[21..25];
                let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((le24(&bytes[24..27]) + 1, le24(&bytes[27..30]) + 1)),
            _ => None,
        };
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        // AVIF: the first `ispe` (image spatial extents) property box.
        let pos = bytes.windows(4).position(|w| w == b"ispe")?;
        let b = bytes.get(pos + 8..pos + 16)?;
        return Some((u32::from_be_bytes([b[0], b[1], b[2], b[3]]), u32::from_be_bytes([b[4], b[5], b[6], b[7]])));
    }
    let svg = transport::mime::mime_type_and_charset(headers).mime == "image/svg+xml"
        || bytes.windows(4).take(512).any(|w| w == b"<svg");
    if svg {
        let text = String::from_utf8_lossy(bytes);
        let start = text.find("<svg")?;
        let tag = &text[start..start + text[start..].find('>')?];
        let attr = |name: &str| -> Option<f64> {
            let i = tag.find(&format!(" {name}="))? + name.len() + 2;
            let q = tag[i..].chars().next()?;
            let rest = &tag[i + 1..];
            let v = &rest[..rest.find(q)?];
            let num: String = v.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            num.parse::<f64>().ok()
        };
        // No width/height: the SVG's default object size, 300x150, scaled by its viewBox ratio when
        // it has one (HTML "the default sizing algorithm").
        let (w, h) = (attr("width"), attr("height"));
        return Some(match (w, h) {
            (Some(w), Some(h)) => (w as u32, h as u32),
            (Some(w), None) => (w as u32, 150),
            (None, Some(h)) => (300, h as u32),
            (None, None) => (300, 150),
        });
    }
    None
}

/// Whether the response stored for the `<img>` node `id`'s request of `url` is an image a browser
/// decodes; `None` when nothing is stored under it. Keyed by the REQUEST, not the `src`
/// attribute, so a `srcset` or `<picture>` choice is judged by what was actually fetched. A body this
/// build cannot decode but Chrome does (WebP, AVIF) counts as decodable; the decode is cached where
/// [`img_state`] looks for it, so the size read that follows costs nothing.
///
/// Brave 151 / Chrome 150, measured 2026-10-01 (c8/repro/async/detail.html): `data:image/png;base64,AAAA`
/// and `data:text/plain,hello` fire `error` (`complete` true, `naturalWidth` 0); this fired `load` for
/// any 2xx -- and a page checking that a deliberately broken image errors read a browser that cannot
/// tell.
pub(crate) fn fetched_image_decodes(scope: &mut v8::PinScope, id: u32, url: &str) -> Option<bool> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (f, backend, mode) = {
        let st = op_state.borrow();
        let mode = if st.try_borrow::<HostDom>()?.0.get_attribute(id, "crossorigin").is_some() { "cors" } else { "no-cors" };
        let ctx = st.try_borrow::<spec::kernel::Context>()?;
        let f = ctx.get::<spec::FetchedImages>().and_then(|s| s.get(url, mode))?;
        (f, ctx.get::<spec::RenderHandle>().map(|h| h.0.clone()), mode)
    };
    if !(200..300).contains(&f.status) {
        return Some(false);
    }
    if chrome_decodes_but_we_cannot(&f.bytes, &f.headers) {
        return Some(true);
    }
    if is_svg(&f.bytes, &f.headers) {
        return Some(header_dimensions(&f.bytes, &f.headers).is_some());
    }
    let key = (url.to_string(), mode.to_string());
    let ptr = std::sync::Arc::as_ptr(&f.bytes) as usize;
    let cached = {
        let st = op_state.borrow();
        st.try_borrow::<DecodedImages>().and_then(|d| d.0.get(&key).filter(|(p, _)| *p == ptr).map(|(_, img)| img.is_some()))
    };
    if let Some(c) = cached {
        return Some(c);
    }
    // An image whose HEADER parses is not broken -- it fires `load` with its size even when its pixel data
    // then fails to decode (Blink; a bot challenge image is built that way). Its pixels are decoded when
    // something draws or reads them ([`img_state`]), not here.
    let _ = backend?;
    Some(header_size(scope, &f).is_some())
}

/// An `<img>`'s size as its header gives it, before (and without) a decode.
enum Sized {
    Size(u32, u32),
    Broken,
    Unavailable,
}

/// [`img_state`]'s answer, as far as a SIZE needs it: a decode already made answers with its own; any other image with
/// its header's, read without decoding a pixel ([`header_size`]); an SVG by rasterizing it, as before. Blink lays an
/// image out, and fires its `load`, once its header parses -- its pixels are decoded when they are painted, off the
/// main thread -- where this decoded every image when its response arrived, on the page's thread: 5-6% of lowes.com's
/// (its AVIF and WebP product shots, rav1d's).
fn img_sized(scope: &mut v8::PinScope, id: u32) -> Sized {
    let Fetched { f, key, ptr, .. } = match img_fetched(scope, id) {
        Ok(fetched) => fetched,
        Err(ImgState::Broken) => return Sized::Broken,
        Err(_) => return Sized::Unavailable,
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let cached = {
        let st = op_state.borrow();
        st.try_borrow::<DecodedImages>().and_then(|d| d.0.get(&key).filter(|(p, _)| *p == ptr).map(|(_, img)| img.as_ref().map(|(i, _)| (i.width, i.height))))
    };
    let size = match cached {
        Some(decoded) => decoded,
        None if is_svg(&f.bytes, &f.headers) || chrome_decodes_but_we_cannot(&f.bytes, &f.headers) => {
            return match img_state(scope, id) {
                ImgState::Ready(img, _, _) => Sized::Size(img.width, img.height),
                ImgState::Broken => Sized::Broken,
                ImgState::Unavailable => Sized::Unavailable,
            };
        }
        None => header_size(scope, &f),
    };
    match size {
        Some((w, h)) => Sized::Size(w, h),
        None => Sized::Broken,
    }
}

/// The size an image's header gives -- a PNG's by Chrome's decoder, a JPEG's turned by its EXIF orientation, a WebP's
/// from its VP8/VP8L/VP8X chunk, an AVIF's from its `ispe` box -- reading no pixel data; `None` when the header does not
/// parse, which is an image Blink calls broken.
fn header_size(scope: &mut v8::PinScope, f: &spec::FetchedImage) -> Option<(u32, u32)> {
    let b = f.bytes.as_slice();
    let modern = (b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP") || (b.len() >= 12 && &b[4..8] == b"ftyp");
    if modern {
        return header_dimensions(b, &f.headers).filter(|(w, h)| *w > 0 && *h > 0);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    backend?.image_size(b)
}

/// The URL `<img>` node `id`'s current request is for: the one the loader made (its selected `srcset` candidate or
/// `<picture>` source, else its `src`), else its `src` resolved against its OWN document -- a frame's, for an image in
/// a frame: against the page's, a bot-challenge frame's `/cdn/.../ci/...` named the PAGE's host, found
/// no response, and read 0x0 where Chrome reads the image. `Err(())`: a `src` that does not resolve.
fn img_request_url(scope: &mut v8::PinScope, id: u32) -> Result<Option<String>, ()> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (recorded, src) = {
        let st = op_state.borrow();
        let recorded = st.try_borrow::<crate::core::dom::geometry_queries::NaturalImages>().and_then(|n| n.sources.get(&id).map(|(u, _)| u.clone()));
        let src = st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "src").map(|s| s.trim().to_string())).unwrap_or_default();
        (recorded, src)
    };
    if recorded.is_some() {
        return Ok(recorded);
    }
    if src.is_empty() {
        return Ok(None);
    }
    let base = crate::core::html::subresource::node_base_url(scope, id);
    url::Url::parse(&base).ok().and_then(|b| b.join(&src).ok()).map(|u| Some(u.to_string())).ok_or(())
}

/// The pixel density of `<img>` node `id`'s current request (its `srcset` candidate's; 1 for a `src`).
fn img_density(scope: &mut v8::PinScope, id: u32) -> f32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::core::dom::geometry_queries::NaturalImages>()
        .and_then(|n| n.sources.get(&id).map(|(_, d)| *d))
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(1.0)
}

/// The state of the `<img>` node `id`.
pub(crate) fn img_state(scope: &mut v8::PinScope, id: u32) -> ImgState {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Fetched { f, key, ptr, clean } = match img_fetched(scope, id) {
        Ok(fetched) => fetched,
        Err(state) => return state,
    };
    let cached = {
        let st = op_state.borrow();
        st.try_borrow::<DecodedImages>().and_then(|d| d.0.get(&key).filter(|(p, _)| *p == ptr).map(|(_, img)| img.clone()))
    };
    let decoded = match cached {
        Some(d) => d,
        None => {
            if chrome_decodes_but_we_cannot(&f.bytes, &f.headers) {
                return ImgState::Unavailable;
            }
            let d = if is_svg(&f.bytes, &f.headers) {
                match rasterize_svg(&f.bytes) {
                    Ok(i) => i.map(|i| (Rc::new(i), true)),
                    Err(()) => return ImgState::Unavailable,
                }
            } else {
                let backend = {
                    let st = op_state.borrow();
                    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
                };
                backend.and_then(|b| b.decode_frame(&f.bytes)).map(|(i, complete)| (Rc::new(i), complete))
            };
            let mut st = op_state.borrow_mut();
            if !st.has::<DecodedImages>() {
                st.put(DecodedImages::default());
            }
            st.borrow_mut::<DecodedImages>().0.insert(key, (ptr, d.clone()));
            d
        }
    };
    match decoded {
        Some((img, complete)) => ImgState::Ready(img, clean, complete),
        None => ImgState::Broken,
    }
}

/// What [`img_state`] decides before anything is decoded: the response `<img>` node `id`'s request has, with its key in
/// [`DecodedImages`] and whether it is origin-clean -- or the state that ends it there (nothing fetched yet, a failed
/// fetch, a failed CORS check).
struct Fetched {
    f: spec::FetchedImage,
    key: (String, String),
    ptr: usize,
    clean: bool,
}

fn img_fetched(scope: &mut v8::PinScope, id: u32) -> Result<Fetched, ImgState> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let crossorigin = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return Err(ImgState::Unavailable) };
        h.0.get_attribute(id, "crossorigin").map(|s| s.to_string())
    };
    // ... and its origin check is against that document's origin, not the page's.
    let doc = crate::core::html::subresource::node_document_url(scope, id);
    let url = match img_request_url(scope, id) {
        Ok(Some(u)) => u,
        Ok(None) => return Err(ImgState::Unavailable),
        Err(()) => return Err(ImgState::Broken),
    };
    let mode = if crossorigin.is_some() { "cors" } else { "no-cors" };
    let fetched = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()).and_then(|s| s.get(&url, mode))
    };
    let Some(f) = fetched else { return Err(ImgState::Unavailable) };
    if !(200..300).contains(&f.status) {
        return Err(ImgState::Broken);
    }
    let doc_origin = origin_of(&doc);
    // A `data:` response is BASIC: Fetch's "main fetch" sets a `data:` request's response tainting to
    // "basic", so the image is CORS-same-origin in either mode, needs no ACAO, and never taints. Its
    // origin is opaque, and comparing that opaque origin with the document's read every `data:` image
    // as cross-origin -- which nothing reached while `data:` images did not load at all, and which
    // Reachable once `data:` images load: learn.microsoft.com's APNG test (draw a 1x1 `data:image/png` with
    // `crossOrigin = "anonymous"`, then `getImageData`) threw "The canvas has been tainted by
    // cross-origin data" in the replay where Chrome's reference throws nothing.
    let is_data = f.final_url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("data:"));
    let same_origin = is_data || (doc_origin.is_some() && origin_of(&f.final_url) == doc_origin);
    let clean = if mode == "cors" && !same_origin {
        // The CORS check: `*` for an anonymous request, the exact origin (and credentials allowed)
        // for `use-credentials`. Failing it is a NETWORK ERROR, so the image is broken, not tainted.
        let creds = crossorigin.as_deref().map(|c| c.eq_ignore_ascii_case("use-credentials")).unwrap_or(false);
        let acao = header(&f.headers, "access-control-allow-origin").map(str::trim);
        let ok = match (acao, creds) {
            (Some("*"), false) => true,
            (Some(o), _) if Some(o.to_string()) == doc_origin => {
                !creds || header(&f.headers, "access-control-allow-credentials").map(str::trim) == Some("true")
            }
            _ => false,
        };
        if !ok {
            return Err(ImgState::Broken);
        }
        true
    } else {
        same_origin
    };
    let key = (url, mode.to_string());
    let ptr = std::sync::Arc::as_ptr(&f.bytes) as usize;
    Ok(Fetched { f, key, ptr, clean })
}

/// RENDER #6: the resolved request URL of `<img>` node `id` -- its `src` joined onto the document base
/// -- for the `url` field of an image `largest-contentful-paint` entry (Brave reports it; a text LCP's
/// `url` is the empty string). `None` for a blank/absent src or a base+src that will not parse.
pub(crate) fn img_url(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    img_request_url(scope, id).ok().flatten()
}

/// `naturalWidth`/`naturalHeight` of the `<img>` node `id`: the decoded size, or the header's for a
/// format decoded elsewhere; `None` while nothing is available (both answered 0 always, so a
/// loaded 1x1 GIF read `0x0` where Chrome reads `1x1` -- a zero-size image is the classic check for a
/// client that never really loaded it).
pub(crate) fn img_natural_size(scope: &mut v8::PinScope, id: u32) -> Option<(u32, u32)> {
    // the image's own size over its candidate's density, truncated as Blink's `naturalWidth` is (`ToInt`)
    img_layout_size(scope, id).map(|(w, h)| (w as u32, h as u32))
}

/// The size `<img>` node `id` is laid out by: its image's own size over its `srcset` candidate's density, unrounded
/// (Chrome 150: a 20px image picked as `512w` for a 100px slot lays out 3.91 wide; its `naturalWidth` reads 3).
pub(crate) fn img_layout_size(scope: &mut v8::PinScope, id: u32) -> Option<(f32, f32)> {
    let density = img_density(scope, id);
    img_intrinsic_size(scope, id).map(|(w, h)| (w as f32 / density, h as f32 / density))
}

/// The image's own size, whatever candidate density it was picked at.
fn img_intrinsic_size(scope: &mut v8::PinScope, id: u32) -> Option<(u32, u32)> {
    match img_sized(scope, id) {
        Sized::Size(w, h) => return Some((w, h)),
        Sized::Broken => return None,
        Sized::Unavailable => {}
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let crossorigin = {
        let st = op_state.borrow();
        st.try_borrow::<HostDom>()?.0.get_attribute(id, "crossorigin").is_some()
    };
    let url = img_request_url(scope, id).ok().flatten()?;
    let mode = if crossorigin { "cors" } else { "no-cors" };
    let f = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()).and_then(|s| s.get(&url, mode))
    }?;
    if !(200..300).contains(&f.status) {
        return None;
    }
    header_dimensions(&f.bytes, &f.headers)
}

// ── a canvas's pixels ───────────────────────────────────────────────────────────────────────────

/// A `<canvas>`'s or `OffscreenCanvas`'s backing size. HTML's defaults for an element; an offscreen
/// canvas carries its own.
fn canvas_size(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, offscreen: bool) -> (u32, u32) {
    if offscreen {
        let d = |scope: &mut v8::PinScope, k: &str| priv_get(scope, obj, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0).max(0.0) as u32;
        return (d(scope, "korvus.offscreenWidth"), d(scope, "korvus.offscreenHeight"));
    }
    let dim = |scope: &mut v8::PinScope, k: &str, dflt: f64| -> u32 {
        super::html_canvas_element::canvas_extent(scope, obj, k).filter(|n| n.is_finite()).unwrap_or(dflt).max(0.0) as u32
    };
    (dim(scope, "width", 300.0), dim(scope, "height", 150.0))
}

/// The 2D context a canvas (element or offscreen) made, if it made one.
pub(crate) fn context_2d_of<'s>(scope: &mut v8::PinScope<'s, '_>, canvas: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    priv_get(scope, canvas, CANVAS_CTX_KIND_PRIV)
        .map(|k| k.to_rust_string_lossy(scope))
        .filter(|k| k == "2d")
        .and_then(|_| priv_get(scope, canvas, CANVAS_CTX_PRIV))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// A canvas's current bitmap and whether it is origin-clean.
fn canvas_pixels(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>, w: u32, h: u32) -> (Rgba8, bool) {
    if let Some(gl) = super::webgl::raster_canvas(scope, canvas, w, h) {
        // A WebGL canvas is never tainted: `texImage2D` refuses a tainted source instead.
        return (gl, true);
    }
    let Some(ctx) = context_2d_of(scope, canvas) else { return (Rgba8::transparent(w, h), true) };
    let (ops, tainted) = super::canvas_2d::ops_and_taint(scope, ctx);
    let params = super::canvas_2d::ctx_render_params(scope, ctx);
    (raster_ops(scope, &ops, w, h, &params), !tainted)
}

/// Raster a 2D display list through the active backend (transparent with none, e.g. in a worker), into the
/// backing `params` describes.
pub(crate) fn raster_ops(scope: &mut v8::PinScope, ops: &[Canvas2DOp], w: u32, h: u32, params: &RenderParams) -> Rgba8 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    match backend {
        Some(b) => b.raster_2d(ops, w, h, params),
        None => Rgba8::transparent(w, h),
    }
}

// ── ImageBitmap ─────────────────────────────────────────────────────────────────────────────────

/// Every live `ImageBitmap`'s pixels and origin-clean flag; `None` once `close()`d.
#[derive(Default)]
struct Bitmaps {
    map: HashMap<u32, Option<(Rc<Rgba8>, bool)>>,
    next: u32,
}

fn bitmap(scope: &mut v8::PinScope, id: u32) -> Option<(Rc<Rgba8>, bool)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<Bitmaps>().and_then(|b| b.map.get(&id).cloned()).flatten()
}

/// A new `ImageBitmap` over `img`, wearing the interface's prototype.
pub(crate) fn new_bitmap<'s>(scope: &mut v8::PinScope<'s, '_>, img: Rgba8, clean: bool) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "ImageBitmap")?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        if !st.has::<Bitmaps>() {
            st.put(Bitmaps::default());
        }
        let b = st.borrow_mut::<Bitmaps>();
        b.next += 1;
        let id = b.next;
        b.map.insert(id, Some((Rc::new(img), clean)));
        id
    };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, obj);
    let idv = int_val(scope, id as i64);
    priv_set(scope, obj, BITMAP_ID_PRIV, idv);
    Some(obj)
}

fn bitmap_id_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    priv_get(scope, this, BITMAP_ID_PRIV).and_then(|v| v.uint32_value(scope))
}

fn cb_bitmap_width(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = bitmap_id_of(scope, args.this()) else { return crate::binding::protos::illegal_invocation(scope) };
    rv.set_uint32(bitmap(scope, id).map(|(b, _)| b.width).unwrap_or(0));
}

fn cb_bitmap_height(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = bitmap_id_of(scope, args.this()) else { return crate::binding::protos::illegal_invocation(scope) };
    rv.set_uint32(bitmap(scope, id).map(|(b, _)| b.height).unwrap_or(0));
}

/// `close()`: the pixels go; `width`/`height` read 0 from then on; a second close is a no-op.
fn cb_bitmap_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = bitmap_id_of(scope, args.this()) else { return crate::binding::protos::illegal_invocation(scope) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(b) = st.try_borrow_mut::<Bitmaps>() {
        b.map.insert(id, None);
    }
}

/// Take a bitmap's pixels, detaching it (`transferFromImageBitmap`): `None` = not an ImageBitmap,
/// `Some(None)` = already detached / closed.
pub(crate) fn bitmap_take(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<Option<Rc<Rgba8>>> {
    let id = bitmap_id_of(scope, o)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let b = st.try_borrow_mut::<Bitmaps>()?;
    let taken = b.map.get(&id).cloned().flatten().map(|(img, _)| img);
    b.map.insert(id, None);
    Some(taken)
}

/// The bodies of `ImageBitmap`'s three registry rows, claimed by `tools/sync/gen_shells.py`'s kit
/// table (the interface stays a registry row: it is `[Exposed=(Window,Worker)]` and in both captures'
/// global orders, which is why `OffscreenCanvas` did the same).
pub fn install_image_bitmap(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "width" => getter_row(scope, proto, member, cb_bitmap_width),
        "height" => getter_row(scope, proto, member, cb_bitmap_height),
        "close" => method_row(scope, proto, member, cb_bitmap_close),
        other => debug_assert!(false, "install_image_bitmap: unexpected member {other}"),
    }
}

// ── drawImage / createPattern ───────────────────────────────────────────────────────────────────

/// What a draw call gets for its image argument.
pub(crate) enum Source {
    /// These pixels; `true` = origin-clean.
    Pixels(Rc<Rgba8>, bool),
    /// A usable source with nothing to draw: an image not yet available, a video with no frame.
    Nothing,
}

/// Which of the two members is asking -- their broken-state sentences differ (measured).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    DrawImage,
    CreatePattern,
}

/// `drawImage`'s / `createPattern`'s image argument, with Chrome's exceptions THROWN for the values
/// that must throw (`None`). `iface` is the receiver's interface, which the sentences name.
pub(crate) fn draw_source(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, caller: Caller) -> Option<Source> {
    let member = if caller == Caller::DrawImage { "drawImage" } else { "createPattern" };
    let fail = |scope: &mut v8::PinScope, msg: &str| {
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{member}' on '{iface}': {msg}"), "InvalidStateError");
    };
    match classify(scope, v) {
        Kind::Img(id) => match img_state(scope, id) {
            ImgState::Unavailable => Some(Source::Nothing),
            ImgState::Broken => {
                fail(
                    scope,
                    if caller == Caller::DrawImage {
                        "The HTMLImageElement provided is in the 'broken' state."
                    } else {
                        "Source image is in the 'broken' state."
                    },
                );
                None
            }
            // an INCOMPLETE frame draws what was decoded (Blink; stock Chrome 150, measured)
            ImgState::Ready(img, clean, _) => Some(Source::Pixels(img, clean)),
        },
        Kind::Canvas | Kind::Offscreen => {
            let Ok(c) = v8::Local::<v8::Object>::try_from(v) else { return None };
            let offscreen = priv_get(scope, c, OFFSCREEN_BRAND_PRIV).is_some();
            let (w, h) = canvas_size(scope, c, offscreen);
            if w == 0 || h == 0 {
                fail(
                    scope,
                    if offscreen {
                        "The image argument is an OffscreenCanvas element with a width or height of 0."
                    } else {
                        "The image argument is a canvas element with a width or height of 0."
                    },
                );
                return None;
            }
            let (img, clean) = canvas_pixels(scope, c, w, h);
            Some(Source::Pixels(Rc::new(img), clean))
        }
        Kind::Bitmap(id) => match bitmap(scope, id) {
            Some((img, clean)) => Some(Source::Pixels(img, clean)),
            None => {
                fail(scope, "The image source is detached");
                None
            }
        },
        // A video draws the picture it shows (`video_pipe.rs`); one with none decoded draws nothing and does not throw
        // (measured, a never-played <video>). An SVG <image> and a VideoFrame this engine mints carry no pixels.
        Kind::Video => {
            let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Some(Source::Nothing) };
            match crate::binding::protos::unwrap_id(scope, o.into()) {
                Some(id) => match crate::core::html::media_load::video_frame(scope, id) {
                    Some(frame) => {
                        let clean = crate::core::html::media_load::origin_clean(scope, id);
                        Some(Source::Pixels(Rc::new((*frame).clone()), clean))
                    }
                    None => Some(Source::Nothing),
                },
                None => Some(Source::Nothing),
            }
        }
        Kind::SvgImage | Kind::VideoFrame => Some(Source::Nothing),
        Kind::ImageData | Kind::Blob(_) | Kind::Other => {
            throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': The provided value is not of type '{DRAW_UNION}'."));
            None
        }
    }
}

/// `texImage2D`/`texSubImage2D`'s source: its pixels, or a SecurityError THROWN (`Err`) for a
/// tainted one, with Chrome's sentence for each kind (measured). `Ok(None)` = nothing to upload.
pub(crate) fn tex_source(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, member: &str) -> Result<Option<Rgba8>, ()> {
    let kind = classify(scope, v);
    let (img, clean, what) = match kind {
        Kind::Img(id) => match img_state(scope, id) {
            ImgState::Ready(img, clean, true) => ((*img).clone(), clean, "The image element contains cross-origin data, and may not be loaded."),
            _ => return Ok(None),
        },
        Kind::Canvas | Kind::Offscreen => {
            let Ok(c) = v8::Local::<v8::Object>::try_from(v) else { return Ok(None) };
            let offscreen = priv_get(scope, c, OFFSCREEN_BRAND_PRIV).is_some();
            let (w, h) = canvas_size(scope, c, offscreen);
            if w == 0 || h == 0 {
                return Ok(None);
            }
            let (img, clean) = canvas_pixels(scope, c, w, h);
            (img, clean, "Tainted canvases may not be loaded.")
        }
        Kind::Bitmap(id) => match bitmap(scope, id) {
            Some((img, clean)) => ((*img).clone(), clean, "The ImageBitmap contains cross-origin data, and may not be loaded."),
            None => return Ok(None),
        },
        Kind::ImageData => match v8::Local::<v8::Object>::try_from(v) {
            Ok(o) => return Ok(image_data_pixels(scope, o)),
            Err(_) => return Ok(None),
        },
        _ => return Ok(None),
    };
    if !clean {
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{member}' on '{iface}': {what}"), "SecurityError");
        return Err(());
    }
    Ok(Some(img))
}

/// An `ImageData`'s pixels (its `data` array, `width` x `height`).
fn image_data_pixels(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<Rgba8> {
    // its `data` SLOT, of any pixel format (a float ImageData has no own `data` property)
    let d = crate::core::events::event::evt_slot_read(scope, o, "data")?;
    let w = super::image_data::image_data_extent(scope, o, "width").unwrap_or(0.0) as u32;
    let h = super::image_data::image_data_extent(scope, o, "height").unwrap_or(0.0) as u32;
    let need = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if w == 0 || h == 0 {
        return None;
    }
    let data = super::pixel_format::data_array_rgba8(d, need)?;
    Some(Rgba8 { width: w, height: h, data })
}

// ── createImageBitmap ───────────────────────────────────────────────────────────────────────────

/// `createImageBitmap(image [, sx, sy, sw, sh] [, options])` on `holder` (`Window`), as a Promise.
///
/// EVERY error REJECTS rather than throws, argument counting included -- the IDL return type is a
/// Promise (measured: `createImageBitmap()` returns a promise that rejects with the arity TypeError).
pub(crate) fn cb_create_image_bitmap(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let resolver = match v8::PromiseResolver::new(scope) {
        Some(r) => r,
        None => return,
    };
    rv.set(resolver.get_promise(scope).into());
    let holder = "Window";
    // The RECEIVER first, and even that rejects rather than throws (measured:
    // `createImageBitmap.call({}, imageData)` rejects "... on 'Window': Illegal invocation").
    // A frame's window is a receiver too (`createImageBitmap(img)` inside an iframe
    // rejected with this sentence; Chrome resolves).
    let this: v8::Local<v8::Value> = args.this().into();
    if !(this.is_null_or_undefined() || crate::core::html::window_globals::is_global_receiver(scope, this)) {
        // Blink's context applied after the error is made
        let e = deno_core::error::native_error(scope, "TypeError", &format!("Failed to execute 'createImageBitmap' on '{holder}': Illegal invocation"));
        resolver.reject(scope, e);
        return;
    }
    let prefix = format!("Failed to execute 'createImageBitmap' on '{holder}': ");
    match create_image_bitmap(scope, &args, &prefix) {
        Ok(b) => {
            let b = v8::Local::new(scope, &b);
            resolver.resolve(scope, b.into());
        }
        Err((name, msg)) => {
            let e = match name {
                // Blink's context applied after the error is made
                "TypeError" | "RangeError" => Some(deno_core::error::native_error(scope, name, &msg)),
                _ => crate::binding::protos::make_dom_exc(scope, &msg, name),
            };
            if let Some(e) = e {
                resolver.reject(scope, e);
            }
        }
    }
}

type Rejection = (&'static str, String);

fn create_image_bitmap(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    prefix: &str,
) -> Result<v8::Global<v8::Object>, Rejection> {
    let n = args.length();
    if n < 1 {
        return Err(("TypeError", format!("{prefix}1 argument required, but only 0 present.")));
    }
    // Overloads: (image, options?) and (image, sx, sy, sw, sh, options?). Two or three trailing
    // numbers resolve to neither (measured "Overload resolution failed." for 3 and 4 arguments).
    let crop = n >= 5;
    if (3..5).contains(&n) {
        return Err(("TypeError", format!("{prefix}Overload resolution failed.")));
    }
    let opts_v = if crop { args.get(5) } else { args.get(1) };
    let opts = if opts_v.is_null_or_undefined() {
        None
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(opts_v) {
        Some(o)
    } else {
        return Err(("TypeError", format!("{prefix}The provided value is not of type 'ImageBitmapOptions'.")));
    };
    // The union FIRST, before the crop rectangle is looked at.
    let kind = classify(scope, args.get(0));
    if matches!(kind, Kind::Other) {
        return Err(("TypeError", format!("{prefix}The provided value is not of type '{BITMAP_UNION}'.")));
    }
    let rect = if crop {
        let v: Vec<f64> = (1..5).map(|i| args.get(i).number_value(scope).filter(|x| x.is_finite()).unwrap_or(0.0).trunc()).collect();
        if v[2] == 0.0 {
            return Err(("RangeError", format!("{prefix}The crop rect width is 0.")));
        }
        if v[3] == 0.0 {
            return Err(("RangeError", format!("{prefix}The crop rect height is 0.")));
        }
        Some((v[0], v[1], v[2], v[3]))
    } else {
        None
    };
    let o = read_options(scope, opts, prefix)?;
    let not_usable = || ("InvalidStateError", format!("{prefix}The image source is not usable."));
    let (img, clean): (Rc<Rgba8>, bool) = match kind {
        Kind::Img(id) => match img_state(scope, id) {
            ImgState::Ready(i, c, true) => (i, c),
            // a frame whose decode did not complete (stock Chrome 150, measured)
            ImgState::Ready(_, _, false) => return Err(("InvalidStateError", format!("{prefix}The ImageBitmap could not be allocated."))),
            ImgState::Broken => {
                return Err(("InvalidStateError", format!("{prefix}The HTMLImageElement provided is in the 'broken' state.")))
            }
            ImgState::Unavailable => return Err(not_usable()),
        },
        Kind::Canvas | Kind::Offscreen => {
            let c = v8::Local::<v8::Object>::try_from(args.get(0)).map_err(|_| not_usable())?;
            let offscreen = priv_get(scope, c, OFFSCREEN_BRAND_PRIV).is_some();
            let (w, h) = canvas_size(scope, c, offscreen);
            if w == 0 {
                return Err(("InvalidStateError", format!("{prefix}The image source's width is 0.")));
            }
            if h == 0 {
                return Err(("InvalidStateError", format!("{prefix}The image source's height is 0.")));
            }
            let (i, c) = canvas_pixels(scope, c, w, h);
            (Rc::new(i), c)
        }
        Kind::Bitmap(id) => bitmap(scope, id).ok_or_else(not_usable)?,
        Kind::ImageData => {
            let d = v8::Local::<v8::Object>::try_from(args.get(0)).map_err(|_| not_usable())?;
            (Rc::new(image_data_pixels(scope, d).ok_or_else(not_usable)?), true)
        }
        Kind::Blob(bytes) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let backend = {
                let st = op_state.borrow();
                st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
            };
            // A Blob is decoded by SNIFFING its bytes, whatever its `type` says (measured: a PNG blob
            // typed text/plain decodes). The failure sentence carries no "Failed to execute" prefix.
            match backend.and_then(|b| b.decode_image(&bytes)) {
                Some(i) => (Rc::new(i), true),
                None => return Err(("InvalidStateError", "The source image could not be decoded.".to_string())),
            }
        }
        // R7: a video has no decoded frame to take a bitmap of.
        Kind::Video | Kind::SvgImage | Kind::VideoFrame => return Err(not_usable()),
        Kind::Other => unreachable!("rejected above"),
    };
    let out = bitmap_pixels(scope, &img, rect, &o);
    match new_bitmap(scope, out, clean) {
        Some(b) => Ok(v8::Global::new(scope, b)),
        None => Err(("InvalidStateError", format!("{prefix}The image source is not usable."))),
    }
}

struct BitmapOptions {
    flip_y: bool,
    resize: (Option<u32>, Option<u32>),
    pixelated: bool,
}

fn read_options(scope: &mut v8::PinScope, opts: Option<v8::Local<v8::Object>>, prefix: &str) -> Result<BitmapOptions, Rejection> {
    let mut o = BitmapOptions { flip_y: false, resize: (None, None), pixelated: false };
    let Some(d) = opts else { return Ok(o) };
    // WebIDL reads dictionary members in LEXICOGRAPHIC order, which is also the order their errors
    // surface in: colorSpaceConversion, imageOrientation, premultiplyAlpha, resizeHeight,
    // resizeQuality, resizeWidth.
    let enum_member = |scope: &mut v8::PinScope, key: &str, ty: &str, allowed: &[&str]| -> Result<Option<String>, Rejection> {
        let Some(v) = get_prop(scope, d, key).filter(|v| !v.is_undefined()) else { return Ok(None) };
        let s = v.to_rust_string_lossy(scope);
        if allowed.contains(&s.as_str()) {
            Ok(Some(s))
        } else {
            Err((
                "TypeError",
                format!("{prefix}Failed to read the '{key}' property from 'ImageBitmapOptions': The provided value '{s}' is not a valid enum value of type {ty}."),
            ))
        }
    };
    enum_member(scope, "colorSpaceConversion", "ColorSpaceConversion", &["none", "default"])?;
    if let Some(v) = enum_member(scope, "imageOrientation", "ImageOrientation", &["from-image", "flipY", "none"])? {
        o.flip_y = v == "flipY";
    }
    enum_member(scope, "premultiplyAlpha", "PremultiplyAlpha", &["none", "premultiply", "default"])?;
    let dim = |scope: &mut v8::PinScope, key: &str| -> Option<u32> {
        get_prop(scope, d, key).filter(|v| !v.is_undefined()).and_then(|v| v.number_value(scope)).map(|n| if n.is_finite() && n > 0.0 { n as u32 } else { 0 })
    };
    let rh = dim(scope, "resizeHeight");
    if let Some(v) = enum_member(scope, "resizeQuality", "ResizeQuality", &["pixelated", "low", "medium", "high"])? {
        o.pixelated = v == "pixelated";
    }
    let rw = dim(scope, "resizeWidth");
    if rw == Some(0) {
        return Err(("InvalidStateError", format!("{prefix}The resize width dimension is equal to 0.")));
    }
    if rh == Some(0) {
        return Err(("InvalidStateError", format!("{prefix}The resize height dimension is equal to 0.")));
    }
    o.resize = (rw, rh);
    Ok(o)
}

/// Crop (HTML: a negative extent starts further back; outside the source is transparent), resize and
/// flip -- through the backend's own `drawImage`, so a resize samples the way a canvas draw does.
fn bitmap_pixels(scope: &mut v8::PinScope, src: &Rgba8, rect: Option<(f64, f64, f64, f64)>, o: &BitmapOptions) -> Rgba8 {
    let (mut sx, mut sy, mut sw, mut sh) = rect.unwrap_or((0.0, 0.0, src.width as f64, src.height as f64));
    if sw < 0.0 {
        sx += sw;
        sw = -sw;
    }
    if sh < 0.0 {
        sy += sh;
        sh = -sh;
    }
    let (cw, ch) = (sw as u32, sh as u32);
    let (ow, oh) = match o.resize {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ((ch as f64) * (w as f64) / (cw.max(1) as f64)).round().max(1.0) as u32),
        (None, Some(h)) => (((cw as f64) * (h as f64) / (ch.max(1) as f64)).round().max(1.0) as u32, h),
        (None, None) => (cw, ch),
    };
    let mut ops = Vec::new();
    if o.pixelated {
        ops.push(Canvas2DOp::SetImageSmoothing { enabled: false });
    }
    if o.flip_y {
        ops.push(Canvas2DOp::SetTransform { t: spec::Transform2D { a: 1.0, b: 0.0, c: 0.0, d: -1.0, e: 0.0, f: oh as f64 } });
    }
    // The crop, clipped to the source the way drawImage clips (the draw lands where the clipped
    // rectangle lands inside the output, scaled).
    let (kx, ky) = (ow as f64 / sw, oh as f64 / sh);
    if let Some((csx, csy, csw, csh, dx, dy)) = clip_to_source(sx, sy, sw, sh, src.width as f64, src.height as f64) {
        ops.push(Canvas2DOp::DrawImage {
            image: src.clone(),
            sx: csx,
            sy: csy,
            sw: csw,
            sh: csh,
            dx: dx * kx,
            dy: dy * ky,
            dw: csw * kx,
            dh: csh * ky,
        });
    }
    raster_ops(scope, &ops, ow, oh, &RenderParams::default())
}

/// Intersect the source rectangle with the image: `(sx, sy, sw, sh, offset_x, offset_y)` of what is
/// left, the offsets being where it starts inside the original rectangle. `None` = no overlap.
pub(crate) fn clip_to_source(sx: f64, sy: f64, sw: f64, sh: f64, iw: f64, ih: f64) -> Option<(f64, f64, f64, f64, f64, f64)> {
    let (x0, y0) = (sx.max(0.0), sy.max(0.0));
    let (x1, y1) = ((sx + sw).min(iw), (sy + sh).min(ih));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1 - x0, y1 - y0, x0 - sx, y0 - sy))
}

/// The window's `createImageBitmap`, created by the registry walk at the capture's position of the
/// name (`install::live::ORDERED_HAND`), which is where every bundle's `globals` list puts it -- between
/// `confirm` and `fetch`. Chrome's shape, measured: an own data property, writable, enumerable and
/// configurable, a native function of `length` 1.
///
/// THE WINDOW ONCE HAD NONE (`postshells_live` listed it among the five "real gaps"). The
/// worker's `WorkerGlobalScope` row stays the D2 stub it was: a worker realm has no render backend, so
/// there is nothing to decode or raster a bitmap with there.
pub fn create_window_create_image_bitmap<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    _ledger: &mut crate::install::installer::InstallLedger,
) {
    let Some(k) = v8::String::new(scope, "createImageBitmap") else { return };
    let f = crate::binding::protos::build_fn(scope, cb_create_image_bitmap, "createImageBitmap", 1);
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
    pd.set_enumerable(true);
    pd.set_configurable(true);
    global.define_property(scope, k.into(), &pd);
}

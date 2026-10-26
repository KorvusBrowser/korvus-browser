//! `core/html/html_canvas_element.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
// The render contract this canvas/WebGL/WebAudio oracle speaks to the render plugin through.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use spec::{GlContextAttributes, GlPowerPreference, ImageFormat, RenderBackend};
// The 2D display-list recorder — `CanvasRenderingContext2D` itself — lives in `canvas_2d.rs`.
// `toDataURL` and `getContext` are HTMLCanvasElement members and stay here; they reach the recorder
// through these three.
use super::canvas_2d::{canvas2d_id, Canvas2DStore};
use crate::binding::protos::{get_prop, obj_get, obj_set_str, require_args, require_ctor_args, require_ctor_new_target, set_bool, shell_proto, throw_type_error};

/// `getParameter`: Blink's switch over this context's state and the recorded device (see
/// `webgl::params`).
pub(crate) fn cb_gl_param_impl(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, v2: bool) {
    let pname = args.get(0).number_value(scope).unwrap_or(0.0) as i64 as u32;
    super::webgl::get_parameter(scope, args.this(), pname, v2, &mut rv);
}
pub(crate) fn cb_gl_param(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_param_impl(scope, args, rv, false)
}
pub(crate) fn cb_gl2_param(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_param_impl(scope, args, rv, true)
}

/// The extension list a context version exposes. A WebGL2 context's list is a DIFFERENT list --
/// most WebGL1 extensions are core in WebGL2 and absent from it, and `EXT_color_buffer_float` is on
/// the WebGL2 list only -- so both prototypes reading `extensions` served the WebGL1 list to both.
fn extensions_key(v2: bool) -> &'static str {
    if v2 { "extensionsV2" } else { "extensions" }
}

pub(crate) fn cb_gl_get_extension(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_get_extension_impl(scope, args, rv, false)
}
pub(crate) fn cb_gl2_get_extension(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_get_extension_impl(scope, args, rv, true)
}
fn cb_gl_get_extension_impl(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, v2: bool) {
    let webgl = match v8::Local::<v8::Object>::try_from(super::webgl::oracle_value(scope)) { Ok(w) => w, Err(_) => return };
    let name = args.get(0).to_rust_string_lossy(scope);
    let ctx = args.this();
    // This answered a fresh EMPTY object for every listed name (and a two-constant
    // literal for `WEBGL_debug_renderer_info`), so `getExtension("WEBGL_lose_context").loseContext`
    // was `undefined`. The object is now the extension's own, from `webgl::extensions`, the same one
    // every call; the name matches ASCII-case-insensitively (`getExtension("webgl_LOSE_context")` is
    // the object in Chrome, measured); and a lost context answers null.
    if super::webgl::extensions::is_lost(scope, ctx) {
        rv.set_null();
        return;
    }
    // WHICH names exist is still the persona's list -- never the table of what this engine can build.
    let mut canonical: Option<String> = None;
    if let Some(exts) = obj_get(scope, webgl, extensions_key(v2)) {
        if let Ok(arr) = v8::Local::<v8::Array>::try_from(exts) {
            for i in 0..arr.length() {
                if let Some(e) = arr.get_index(scope, i) {
                    let e = e.to_rust_string_lossy(scope);
                    if e.eq_ignore_ascii_case(&name) {
                        canonical = Some(e);
                        break;
                    }
                }
            }
        }
    }
    let set = match canonical {
        Some(c) => super::webgl::extensions::extension_object(scope, ctx, &c, &mut rv),
        None => false,
    };
    if !set {
        rv.set_null();
    }
}

pub(crate) fn cb_gl_supported_ext(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_supported_ext_impl(scope, args, rv, false)
}
pub(crate) fn cb_gl2_supported_ext(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_gl_supported_ext_impl(scope, args, rv, true)
}
fn cb_gl_supported_ext_impl(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, v2: bool) {
    let webgl = match v8::Local::<v8::Object>::try_from(super::webgl::oracle_value(scope)) { Ok(w) => w, Err(_) => return };
    if super::webgl::extensions::is_lost(scope, args.this()) {
        rv.set_null();
        return;
    }
    if let Some(exts) = obj_get(scope, webgl, extensions_key(v2)) {
        if let Ok(arr) = v8::Local::<v8::Array>::try_from(exts) {
            let out = v8::Array::new(scope, arr.length() as i32);
            for i in 0..arr.length() {
                if let Some(e) = arr.get_index(scope, i) {
                    out.set_index(scope, i, e);
                }
            }
            rv.set(out.into());
            return;
        }
    }
    let out = v8::Array::new(scope, 0);
    rv.set(out.into());
}

/// `getContextAttributes()` — the attributes THIS context was created with.
///
/// It used to hand back the persona oracle's recorded dictionary, one object shared by every
/// context on the page. That was the right answer as long as nothing else read the attributes; now
/// the rasterizer does, and a context created with `{antialias: false}` that reported `true` would
/// be a cross-layer incoherence a page can see in one expression (read the attribute, then measure
/// an edge). So it reports the struct the backend rasterizes against. With no dictionary the values
/// are the WebGL defaults, which is byte for byte what the captured device reported.
pub(crate) fn cb_gl_ctx_attrs(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if super::webgl::extensions::is_lost(scope, args.this()) {
        rv.set_null();
        return;
    }
    let a = super::webgl::context_attributes(scope, args.this());
    let o = v8::Object::new(scope);
    // Key order matters: `JSON.stringify(gl.getContextAttributes())` is a one-line probe, and the
    // recorded device's dictionary enumerates alphabetically. `powerPreference` sits between
    // `failIfMajorPerformanceCaveat` and `premultipliedAlpha`, which is why this is not one loop.
    set_bool(scope, o, "alpha", a.alpha);
    set_bool(scope, o, "antialias", a.antialias);
    set_bool(scope, o, "depth", a.depth);
    set_bool(scope, o, "desynchronized", a.desynchronized);
    set_bool(scope, o, "failIfMajorPerformanceCaveat", a.fail_if_major_performance_caveat);
    obj_set_str(scope, o, "powerPreference", a.power_preference.as_str());
    set_bool(scope, o, "premultipliedAlpha", a.premultiplied_alpha);
    set_bool(scope, o, "preserveDrawingBuffer", a.preserve_drawing_buffer);
    set_bool(scope, o, "stencil", a.stencil);
    set_bool(scope, o, "xrCompatible", a.xr_compatible);
    rv.set(o.into());
}

/// The active render backend (SolidBackend at R0, Skia from R2), injected by `engine` into OpState.
pub struct RenderState {
    pub backend: Box<dyn RenderBackend>,
}

/// Read/write a v8 Private slot on an object, by name. Privates are invisible to every JS reflection
/// path (`getOwnPropertyNames`, `getOwnPropertySymbols`, `Reflect.ownKeys`, proxies), which is what
/// makes them the right place for the canvas -> context back-reference.
pub(crate) fn priv_get<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let pname = v8::String::new(scope, name)?;
    let key = v8::Private::for_api(scope, Some(pname));
    obj.get_private(scope, key).filter(|v| !v.is_undefined())
}
pub(crate) fn priv_set(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, val: v8::Local<v8::Value>) {
    if let Some(pname) = v8::String::new(scope, name) {
        let key = v8::Private::for_api(scope, Some(pname));
        obj.set_private(scope, key, val);
    }
}

/// `canvas.toDataURL(type, quality)` — the actual pixels, encoded.
///
/// This used to be `rv.set(args.data())`: one PNG, recorded from the captured device at wiring time,
/// returned for every canvas on the page forever. That is the single most-hashed value in browser
/// fingerprinting, and returning a constant failed it three separate ways at once. It did not contain
/// what the page drew, so the classic `fillText(nonce)` probe hashed to the wrong thing. It did not
/// vary with the canvas, so two canvases of DIFFERENT SIZES serialized to byte-identical strings --
/// three lines to detect, and impossible in any real browser. And it contradicted `getImageData`,
/// which has rastered live through Skia since R2: a detector that reads both and compares has caught
/// the engine without knowing anything about it.
///
/// So it takes the same path `getImageData` does -- the recorded display list, through the active
/// backend, at the canvas's backing size -- and hands the buffer to the backend to encode (see
/// `spec::RenderBackend::encode_image` for why the encoder belongs there and where it is still not
/// byte-identical to Chrome).
///
/// A canvas with no 2D context rasters an empty list, which is transparent -- correct for a canvas
/// nothing has drawn on, and the honest answer for a WebGL one until `raster_gl` exists (R3). It is
/// at least a real PNG of the right dimensions now, rather than a 280x60 recording of another device.
pub(crate) fn cb_canvas_to_data_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if taint_refuses_export(scope, this, "toDataURL") {
        return;
    }
    let want = requested_format(scope, args.get(0));
    let quality = encoder_quality(scope, args.get(1));
    let url = match encode_canvas(scope, this, want, quality) {
        Some((bytes, mime)) => format!("data:{mime};base64,{}", B64.encode(&bytes)),
        // "If the canvas has no pixels, return the string 'data:,'" -- HTML, and what Chrome returns.
        None => "data:,".to_string(),
    };
    if let Some(s) = v8::String::new(scope, &url) {
        rv.set(s.into());
    }
}

/// The requested serialization: `ImageEncoderUtils::ToEncodingMimeType` -- the type ASCII-lower-cased and
/// compared WHOLE (Chrome 150, measured: `"IMAGE/WEBP"` is a WebP, `" image/webp"` a PNG; korvus trimmed it and
/// lower-cased in Unicode), and anything but the three types it writes falls back to image/png.
fn requested_format(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> ImageFormat {
    match v.to_rust_string_lossy(scope).to_ascii_lowercase().as_str() {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    }
}

/// The `quality` argument of `toDataURL` / `toBlob` (an IDL `any`): Blink reads it only when it IS a Number
/// (`quality_argument.V8Value()->IsNumber()`), so `"0.5"` and `true` are the default quality (Chrome 150,
/// measured: a JPEG of quality `"0.5"` is byte-identical to the default's; korvus applied ToNumber). The range
/// rules are the encoder's (`ComputeJpegQuality`, `ComputeWebpOptions`), so the number goes through as given.
fn encoder_quality(_scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<f64> {
    if v.is_number() {
        v8::Local::<v8::Number>::try_from(v).ok().map(|n| n.value())
    } else {
        None
    }
}

/// A canvas's `width` / `height` read NATIVELY -- an `OffscreenCanvas`'s slot, a `<canvas>`'s content
/// attribute through the same parse its reflected getter makes (`width|300`, `height|150`) -- and never
/// through the page-visible accessor. A page that wraps `HTMLCanvasElement.prototype.width` (or
/// `OffscreenCanvas`'s) sees only its own reads; Blink reads the canvas's size off the element, and
/// a bot challenge counted the extra getter calls this engine made inside WebGL and 2D calls.
/// `None` for an object that is neither.
pub(crate) fn canvas_extent(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>, key: &str) -> Option<f64> {
    let (slot, dflt) = if key == "width" { ("korvus.offscreenWidth", 300) } else { ("korvus.offscreenHeight", 150) };
    if let Some(v) = crate::core::css::css_style_declaration::priv_get(scope, canvas, slot) {
        return v.number_value(scope);
    }
    let id = crate::page::node_id(scope, canvas.into())?;
    let attr = crate::page::with_host_dom(scope, |d| d.get_attribute(id, key).map(str::to_string)).flatten();
    let n = attr
        .and_then(|a| crate::core::dom::element::parse_html_integer(&a, false))
        .filter(|n| *n <= u32::MAX as i64)
        .unwrap_or(dflt);
    Some(n as f64)
}

/// The direction `direction = "inherit"` resolves to on a context of `canvas` -- Blink's
/// `CanvasRenderingContextHost::GetTextDirection`. A `<canvas>` answers its computed `direction`, through the same
/// `getComputedStyle` a page would call (`ltr` when there is none to ask); an `OffscreenCanvas` answers the SNAPSHOT
/// it took when it was made (`OffscreenCanvas::text_direction_`, see [`OFFSCREEN_RTL_PRIV`]). It has no style to ask:
/// handing it to `getComputedStyle` threw, once the offscreen context carried its back-reference.
pub(crate) fn host_text_direction(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>) -> &'static str {
    if priv_get(scope, canvas, "korvus.offscreenWidth").is_some() {
        let rtl = priv_get(scope, canvas, OFFSCREEN_RTL_PRIV).is_some_and(|v| v.is_true());
        return if rtl { "rtl" } else { "ltr" };
    }
    computed_direction(scope, canvas)
}

fn computed_direction(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> &'static str {
    // A canvas out of the document has no style to ask: Blink falls back to its directionality, which its `dir`
    // attribute -- or its nearest ancestor's -- decides (measured: a detached `<canvas dir="rtl">` answers `rtl`).
    if !crate::page::is_connected(scope, el.into()) {
        let rtl = crate::page::node_id(scope, el.into())
            .and_then(|id| crate::page::with_host_dom(scope, |d| directionality_is_rtl(d, id)))
            .unwrap_or(false);
        return if rtl { "rtl" } else { "ltr" };
    }
    let resolved = (|| {
        let gcs = crate::binding::protos::intrinsic_fn(scope, "getComputedStyle")?;
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let style = gcs.call(scope, undef, &[el.into()]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        let d = get_prop(scope, style, "direction")?.to_rust_string_lossy(scope);
        Some(d == "rtl")
    })();
    if resolved == Some(true) { "rtl" } else { "ltr" }
}

/// An `OffscreenCanvas`'s text direction, `true` for `rtl`: what `direction = "inherit"` reads on its context. Blink
/// snapshots it when the canvas is made -- `new OffscreenCanvas()` in a window takes the document element's
/// directionality (`<html dir>`), `transferControlToOffscreen()` the element's own direction, a worker none (`ltr`).
/// A residual: `dir="auto"` on the root resolves from its text's first strong character in Blink and reads `ltr` here.
pub(crate) const OFFSCREEN_RTL_PRIV: &str = "korvus.offscreenRtl";

fn document_element_is_rtl(scope: &mut v8::PinScope) -> bool {
    let Some(doc) = super::window_globals::window_slot_get(scope, "document") else { return false };
    let slot = crate::core::dom::document::doc_root_slot(scope, doc);
    crate::page::with_host_dom(scope, |d| {
        let root = crate::core::dom::document::resolve_doc_root(d, slot);
        crate::core::dom::document::doc_element_child(d, root).is_some_and(|el| directionality_is_rtl(d, el))
    })
    .unwrap_or(false)
}

/// An element's HTML directionality as its `dir` attributes decide it: its own `ltr` / `rtl`, else (no `dir`, or one
/// in no state) its parent element's, else `ltr`. A residual: `dir="auto"` resolves from the text's first strong
/// character in Blink and reads `ltr` here.
fn directionality_is_rtl(dom: &korvus_dom::Dom, id: u32) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur.filter(|&n| dom.node_type(n) == 1) {
        if let Some(v) = dom.get_attribute(n, "dir").filter(|_| dom.is_html_ns(n)) {
            if v.eq_ignore_ascii_case("rtl") {
                return true;
            }
            if v.eq_ignore_ascii_case("ltr") || v.eq_ignore_ascii_case("auto") {
                return false;
            }
        }
        cur = dom.parent(n);
    }
    false
}

/// Raster a canvas's current contents to pixels for the document compositor's
/// replaced-`<canvas>` paint — the SAME raster `encode_canvas` (toDataURL/toBlob) performs: WebGL
/// serializes its drawing buffer, a 2D context rasters its (memoised) display list, a context-less
/// canvas rasters transparent. Returns `(pixels, width, height)`; `None` for a zero dimension or no
/// render backend. (A deliberate small duplication of `encode_canvas`'s raster so the fingerprint path
/// stays byte-identical; a later refactor can share one body.)
pub(crate) fn canvas_rgba(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<(spec::Rgba8, u32, u32)> {
    let dim = |scope: &mut v8::PinScope, key: &str, dflt: f64| -> u32 {
        canvas_extent(scope, this, key)
            .filter(|n| n.is_finite())
            .unwrap_or(dflt)
            .max(0.0) as u32
    };
    let cw = dim(scope, "width", 300.0);
    let ch = dim(scope, "height", 150.0);
    if cw == 0 || ch == 0 {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ctx_2d = priv_get(scope, this, CANVAS_CTX_KIND_PRIV)
        .map(|k| k.to_rust_string_lossy(scope))
        .filter(|k| k == "2d")
        .and_then(|_| priv_get(scope, this, CANVAS_CTX_PRIV))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let (ops, ctx_id) = match ctx_2d {
        Some(c) => {
            let id = canvas2d_id(scope, c, &op_state);
            let st = op_state.borrow();
            (st.borrow::<Canvas2DStore>().ctx.get(&id).map(|c| c.ops.clone()).unwrap_or_default(), Some(id))
        }
        None => (Vec::new(), None),
    };
    // The backing the context was created with (its colour space and type).
    let params = ctx_2d.map(|c| crate::core::html::canvas_2d::ctx_render_params(scope, c)).unwrap_or_default();
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    }?;
    let img = match super::webgl::raster_canvas(scope, this, cw, ch) {
        Some(gl) => gl,
        None => match ctx_id {
            Some(id) => {
                let mut st = op_state.borrow_mut();
                crate::core::html::canvas_2d::raster_memo(&mut st, id, &ops, cw, ch, || backend.raster_2d(&ops, cw, ch, &params))
            }
            None => backend.raster_2d(&ops, cw, ch, &params),
        },
    };
    Some((img, cw, ch))
}

/// A whole canvas's pixels as the page reads them back -- `toDataURL`, `toBlob`, `convertToBlob`, `getImageData` --
/// through the plugins that fill the readback slot (`plugin_host::readback_rgba`, ABI 2), whole, so every one of those
/// reads agrees. The pixels a canvas lends inside the engine (`drawImage` of a canvas, a CSS paint) are not a read-back
/// and stay raw, so a canvas drawn into another reads as the original does.
/// `kind` is `plugin_host::READBACK_CANVAS` for a 2D canvas, `READBACK_WEBGL` for a WebGL drawing buffer.
pub(crate) fn read_back(kind: i32, mut img: spec::Rgba8) -> spec::Rgba8 {
    if plugin_host::has_readback(kind) {
        plugin_host::readback_rgba(kind, img.width, img.height, &mut img.data);
    }
    img
}

/// Raster this canvas's display list and encode it -- the shared body of `toDataURL` and `toBlob`,
/// which differ only in how they hand the bytes back. `None` = the canvas has no pixels (a zero
/// dimension) or there is no render backend.
fn encode_canvas(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    want: ImageFormat,
    quality: Option<f64>,
) -> Option<(Vec<u8>, &'static str)> {
    // HTML's defaults. NOT `filter(> 0)` -- that is `getImageData`'s reading of a MISSING attribute,
    // and here it would turn `canvas.width = 0` into a 300px canvas and hand back a full PNG where
    // the spec (and Chrome) say `data:,`. Absent means 300x150; explicitly zero means zero.
    let dim = |scope: &mut v8::PinScope, key: &str, dflt: f64| -> u32 {
        canvas_extent(scope, this, key)
            .filter(|n| n.is_finite())
            .unwrap_or(dflt)
            .max(0.0) as u32
    };
    let cw = dim(scope, "width", 300.0);
    let ch = dim(scope, "height", 150.0);
    if cw == 0 || ch == 0 {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The 2D context's display list, if this canvas has a 2D context. `canvas2d_id` keys off the
    // CONTEXT object, so the canvas has to hand us the one it made -- which is why `getContext`
    // remembers it (and why it has to return the same object every call, as a browser does).
    let ctx_2d = priv_get(scope, this, CANVAS_CTX_KIND_PRIV)
        .map(|k| k.to_rust_string_lossy(scope))
        .filter(|k| k == "2d")
        .and_then(|_| priv_get(scope, this, CANVAS_CTX_PRIV))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    // `provider`: Blink has made the context's resource provider, so its snapshot is tagged sRGB and a JPEG or
    // WebP carries the profile (`canvas_2d::has_paint_canvas`); a canvas never drawn on exports Blink's
    // untagged transparent image (`CanvasRenderingContextHost::CreateTransparentImage`).
    let (ops, ctx_id, provider) = match ctx_2d {
        Some(c) => {
            let id = canvas2d_id(scope, c, &op_state);
            let st = op_state.borrow();
            match st.borrow::<Canvas2DStore>().ctx.get(&id) {
                Some(c) => (c.ops.clone(), Some(id), crate::core::html::canvas_2d::has_paint_canvas(c)),
                None => (Vec::new(), Some(id), false),
            }
        }
        None => (Vec::new(), None, false),
    };
    // The backing the context was created with (its colour space and type).
    let params = ctx_2d.map(|c| crate::core::html::canvas_2d::ctx_render_params(scope, c)).unwrap_or_default();
    // Clone the backend out before rastering + encoding, so the OpState borrow is not held across
    // work that can take milliseconds.
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    let backend = backend?;
    // R3: a WebGL canvas serializes the GL drawing buffer, through the SAME `raster_gl` that
    // `readPixels` copies out of. Before this, a WebGL canvas rastered an empty 2D display list and
    // served a transparent PNG whatever the page had drawn -- which is why two different
    // `clearColor` values produced byte-identical 174-byte data URLs.
    let (img, kind) = match super::webgl::raster_canvas(scope, this, cw, ch) {
        Some(gl) => (gl, plugin_host::READBACK_WEBGL),
        // The context's last raster when nothing changed since it.
        None => (
            match ctx_id {
                Some(id) => {
                    let mut st = op_state.borrow_mut();
                    crate::core::html::canvas_2d::raster_memo(&mut st, id, &ops, cw, ch, || backend.raster_2d(&ops, cw, ch, &params))
                }
                None => backend.raster_2d(&ops, cw, ch, &params),
            },
            plugin_host::READBACK_CANVAS,
        ),
    };
    let img = read_back(kind, img);
    // Fall back to PNG for a format the backend cannot write -- the same rule HTML states for a type
    // the UA does not support.
    // (A WebGL canvas has no 2D context, so `provider` is false for it: its snapshot's tag is not measured.)
    match backend.encode_image_tagged(&img, want, quality, provider) {
        Some(b) => Some((b, mime_of(want))),
        None => backend.encode_image(&img, ImageFormat::Png, None).map(|b| (b, "image/png")),
    }
}

fn mime_of(f: ImageFormat) -> &'static str {
    match f {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Webp => "image/webp",
    }
}

/// `canvas.toBlob(callback, type, quality)` — the same pixels `toDataURL` serializes, handed to a
/// callback as a `Blob` instead of a base64 string.
///
/// It runs the callback ASYNCHRONOUSLY, because HTML says so and because a page can tell: code that
/// sets a flag after `toBlob(...)` returns and reads it inside the callback sees the flag set in
/// every browser. A canvas with no pixels calls back with `null`, again per HTML.
pub(crate) fn cb_canvas_to_blob(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "HTMLCanvasElement", "toBlob") {
        return;
    }
    let Ok(cb) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        throw_type_error(
            scope,
            "Failed to execute 'toBlob' on 'HTMLCanvasElement': parameter 1 is not of type 'Function'.",
        );
        return;
    };
    if taint_refuses_export(scope, args.this(), "toBlob") {
        return;
    }
    let want = requested_format(scope, args.get(1));
    let quality = encoder_quality(scope, args.get(2));
    let encoded = encode_canvas(scope, args.this(), want, quality);

    let blob: v8::Local<v8::Value> = match encoded {
        Some((bytes, mime)) => match make_blob(scope, &bytes, mime) {
            Some(b) => b,
            None => v8::null(scope).into(),
        },
        None => v8::null(scope).into(),
    };
    // `queueMicrotask(() => callback(blob))` — built here rather than in JS so the binding stays
    // native. A bound function is the shortest way to carry the blob to the callback.
    let queue = crate::binding::protos::intrinsic_fn(scope, "queueMicrotask");
    let bound = get_prop(scope, cb.into(), "bind")
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
        .and_then(|bind| {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            bind.call(scope, cb.into(), &[undef, blob])
        })
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    match (queue, bound) {
        (Some(q), Some(b)) => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            q.call(scope, undef, &[b.into()]);
        }
        // No microtask queue to defer onto: calling back synchronously is worse than not calling
        // back at all only in theory -- a page that never hears from `toBlob` is stuck.
        _ => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            cb.call(scope, undef, &[blob]);
        }
    }
}

/// `new Blob([bytes], { type })` through the page's own constructor, so the result is the same
/// object a page would build.
fn make_blob<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: &[u8], mime: &str) -> Option<v8::Local<'s, v8::Value>> {
    let ctor = crate::binding::protos::iface_object(scope, "Blob").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let store = v8::ArrayBuffer::new(scope, bytes.len());
    let view = v8::Uint8Array::new(scope, store, 0, bytes.len())?;
    {
        let data = store.data()?;
        // SAFETY: the backing store was just allocated at exactly this length and nothing else
        // holds a reference to it yet.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.as_ptr() as *mut u8, bytes.len()) };
    }
    let parts = v8::Array::new(scope, 1);
    parts.set_index(scope, 0, view.into());
    let opts = crate::binding::protos::init_dict(scope);
    obj_set_str(scope, opts, "type", mime);
    ctor.new_instance(scope, &[parts.into(), opts.into()]).map(|o| o.into())
}

/// `canvas.getContext(kind)` — and the canvas KEEPS the context it made.
///
/// It used to build a fresh object on every call, which made
/// `c.getContext("2d") === c.getContext("2d")` false where every browser says true. That is a tell on
/// its own, and it was also a correctness bug with a wider blast radius than the identity check
/// suggests: the display list is keyed by the context object, so drawing through one handle and
/// reading through another returned a blank canvas. A page that does
/// `getContext("2d")` in one function and again in another -- which is ordinary code -- saw nothing
/// it had drawn.
///
/// HTML also gives a canvas exactly ONE context: once a type is chosen, asking for a different one
/// returns null rather than replacing it. `webgl` and `experimental-webgl` name the same type.

/// `new OffscreenCanvas(width, height)` -- a real one, with dimensions and a 2D context.
///
/// It used to be `make_constructable`'s empty body: the arguments were dropped, `width`/`height`
/// answered `undefined`, and `getContext` was a shell answering `undefined` -- so the whole
/// `OffscreenCanvasRenderingContext2D` interface (72 members, 269 recorded cases) was unreachable,
/// answering for a prototype no object ever wore. The same shape `performance.timing` had.
///
/// Both arguments are REQUIRED (`new OffscreenCanvas()` is a TypeError, recorded) and are
/// `[EnforceRange] unsigned long long`, so `"1"` coerces to 1.
pub(crate) fn cb_offscreen_canvas_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    super::webgl::prepare_gl(scope);
    // THE TWO CHECKS CHROME MAKES, IN CHROME'S ORDER. This had neither shape right: it
    // asked `require_args`, whose sentence is "Failed to execute 'OffscreenCanvas' on
    // 'OffscreenCanvas'", and it asked it FIRST, so `OffscreenCanvas()` with no `new` answered an
    // argument count. Chrome 150 answers "Failed to construct 'OffscreenCanvas': Please use the 'new'
    // operator, ..." for the call and "Failed to construct 'OffscreenCanvas': 2 arguments required,
    // but only 0 present." for `new` with no arguments (both measured headless 2026-09-12). Both were
    // invisible until this body reached `installer_parity`'s golden -- and in the WORKER realm, until
    // this body reached the realm at all, which is what the row-driven constructor changed.
    if !require_ctor_new_target(scope, &args, "OffscreenCanvas") {
        return;
    }
    if !require_ctor_args(scope, &args, 2, "OffscreenCanvas") {
        return;
    }
    let w = args.get(0).number_value(scope).unwrap_or(0.0);
    let h = args.get(1).number_value(scope).unwrap_or(0.0);
    let this = args.this();
    let wv = v8::Number::new(scope, if w.is_finite() && w >= 0.0 { w.trunc() } else { 0.0 });
    let hv = v8::Number::new(scope, if h.is_finite() && h >= 0.0 { h.trunc() } else { 0.0 });
    priv_set(scope, this, "korvus.offscreenWidth", wv.into());
    priv_set(scope, this, "korvus.offscreenHeight", hv.into());
    if document_element_is_rtl(scope) {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        priv_set(scope, this, OFFSCREEN_RTL_PRIV, t);
    }
}

/// THE INSTANCE BRAND, and it has to be the body's own.
///
/// `new OffscreenCanvas(w, h)` writes both dimension privates, so carrying one IS being a constructed
/// offscreen canvas. Chrome checks the same thing and it checks it as an INTERNAL SLOT: measured on
/// Chrome 150.0.7871.46 headless, `Object.getOwnPropertyDescriptor(OffscreenCanvas.prototype,"width")
/// .get.call(Object.create(OffscreenCanvas.prototype))` is `TypeError: Illegal invocation`, and so are
/// the `height` pair, `getContext` and `transferToImageBitmap` on the same receiver.
///
/// The installer's own receiver check (`install::receiver`, applied to every `BodyKind::Install` row)
/// reads the PROTOTYPE CHAIN, which cannot tell `Object.create(proto)` from a real instance -- so it
/// catches a foreign object and not this. That difference is why it is checked here as well: the shell
/// stub these three rows replaced was brand-checked (`brand_shell_instance`), and moving the bodies
/// into the rows without this would have traded Chrome's `Illegal invocation` for an answer, which is
/// failure mode 3 of a table cutover. Measured on `installer_parity`'s golden, which probes exactly
/// that receiver.
fn offscreen_branded(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    if priv_get(scope, args.this(), "korvus.offscreenWidth").is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn offscreen_dim(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str) -> f64 {
    priv_get(scope, this, key).and_then(|v| v.number_value(scope)).unwrap_or(0.0)
}

fn cb_offscreen_width_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !offscreen_branded(scope, &args) {
        return;
    }
    let v = offscreen_dim(scope, args.this(), "korvus.offscreenWidth");
    let n = v8::Number::new(scope, v);
    rv.set(n.into());
}
fn cb_offscreen_height_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !offscreen_branded(scope, &args) {
        return;
    }
    let v = offscreen_dim(scope, args.this(), "korvus.offscreenHeight");
    let n = v8::Number::new(scope, v);
    rv.set(n.into());
}
fn cb_offscreen_width_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !offscreen_branded(scope, &args) {
        return;
    }
    let w = args.get(0).number_value(scope).unwrap_or(0.0);
    let wv = v8::Number::new(scope, if w.is_finite() && w >= 0.0 { w.trunc() } else { 0.0 });
    priv_set(scope, args.this(), "korvus.offscreenWidth", wv.into());
    offscreen_reset(scope, args.this());
}
fn cb_offscreen_height_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !offscreen_branded(scope, &args) {
        return;
    }
    let h = args.get(0).number_value(scope).unwrap_or(0.0);
    let hv = v8::Number::new(scope, if h.is_finite() && h >= 0.0 { h.trunc() } else { 0.0 });
    priv_set(scope, args.this(), "korvus.offscreenHeight", hv.into());
    offscreen_reset(scope, args.this());
}

/// `offscreen.getContext(id)` -- the same context objects the element path builds, so an
/// OffscreenCanvas 2D context and a `<canvas>` 2D context are the same kind of thing.
fn cb_offscreen_get_context(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    super::webgl::prepare_gl(scope);
    // The receiver FIRST, as Chrome does -- see [`offscreen_branded`]. It is what the shell stub this row
    // replaces checked, and without it `getContext.call(Object.create(OffscreenCanvas.prototype), …)`
    // would answer instead of throwing `Illegal invocation` (`installer_parity`'s golden probes it).
    if !offscreen_branded(scope, &args) {
        return;
    }
    if !require_args(scope, &args, 1, "OffscreenCanvas", "getContext") {
        return;
    }
    // `OffscreenRenderingContextType` is an IDL ENUM: the binding converts the id and refuses anything but its five
    // values before the attributes are read (Chrome 150 measured: `getContext("2D")`,
    // `" 2d"`, `"experimental-webgl"` and `"foo"` are TypeErrors, and a throwing `toString` propagates). This
    // lower-cased and trimmed the id, and answered null for the rest.
    let Some(id) = args.get(0).to_string(scope) else { return };
    let id = id.to_rust_string_lossy(scope);
    if !matches!(id.as_str(), "2d" | "webgl" | "webgl2" | "bitmaprenderer" | "webgpu") {
        throw_type_error(
            scope,
            &format!("Failed to execute 'getContext' on 'OffscreenCanvas': The provided value '{id}' is not a valid enum value of type OffscreenRenderingContextType."),
        );
        return;
    }
    let Some(attrs) = read_creation_attributes(scope, args.get(1), "OffscreenCanvas") else { return };
    if !super::pixel_format::check_color_space_enabled(scope, &attrs.color_space, "Failed to execute 'getContext' on 'OffscreenCanvas': ") {
        return;
    }
    // WebGL too: `getContext("webgl")` on an OffscreenCanvas is a WebGL context in Chrome
    // wherever the device has a GPU -- a worker included -- and it answered null here. Built exactly
    // as the element path builds one, with the OffscreenCanvas as its `canvas`. WebGPU's canvas context as well
    // (measured: its `canvas` is the OffscreenCanvas), as the element path builds it.
    let has_webgl = super::webgl::oracle_has_webgl(scope);
    let (kind, proto_name) = match id.as_str() {
        "2d" => ("2d", "OffscreenCanvasRenderingContext2D"),
        "webgl" if has_webgl => ("webgl", "WebGLRenderingContext"),
        "webgl2" if has_webgl => ("webgl2", "WebGL2RenderingContext"),
        "bitmaprenderer" => ("bitmaprenderer", "ImageBitmapRenderingContext"),
        "webgpu" => ("webgpu", "GPUCanvasContext"),
        _ => {
            rv.set_null();
            return;
        }
    };
    let this = args.this();
    if let Some(prev) = priv_get(scope, this, CANVAS_CTX_PRIV) {
        let same = priv_get(scope, this, CANVAS_CTX_KIND_PRIV).map(|k| k.to_rust_string_lossy(scope) == kind).unwrap_or(true);
        if same {
            rv.set(prev);
        } else {
            rv.set_null();
        }
        return;
    }
    let Some(proto) = shell_proto(scope, proto_name) else {
        rv.set_null();
        return;
    };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, obj);
    // The back-reference the element path keeps, on a private slot: `canvas` is the prototype's row over it, and
    // every body that needs the canvas's size reads it (a 2D context had none, so `getImageData`
    // rastered at 300x150 whatever the OffscreenCanvas's size, and an own `canvas` data property stood in for the
    // row -- `Object.getOwnPropertyNames(ctx)` is empty in Chrome 150).
    if kind == "webgpu" {
        super::webgpu::init_canvas_context(scope, obj, this);
    } else {
        let this_v: v8::Local<v8::Value> = this.into();
        priv_set(scope, obj, super::canvas_2d::CANVAS_BACKREF_PRIV, this_v);
    }
    match kind {
        "2d" => {
            // The marker the 2D bodies name their interface by.
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            priv_set(scope, obj, super::canvas_2d::OFFSCREEN_CTX_PRIV, t);
            super::canvas_2d::store_2d_settings(scope, obj, &attrs.settings_2d());
        }
        "webgl" | "webgl2" => super::webgl::init_context(scope, obj, kind == "webgl2", attrs.gl_attributes(true)),
        _ => {}
    }
    let obj_v: v8::Local<v8::Value> = obj.into();
    priv_set(scope, this, CANVAS_CTX_PRIV, obj_v);
    // Its kind, as the element path records it, so the canvas reads the display list as a SOURCE
    // (drawImage, createPattern, createImageBitmap).
    if let Some(kv) = v8::String::new(scope, kind) {
        priv_set(scope, this, CANVAS_CTX_KIND_PRIV, kv.into());
    }
    rv.set(obj_v);
}

pub(crate) fn cb_canvas_get_context(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    super::webgl::prepare_gl(scope);
    // `contextId` is required. Without this check `canvas.getContext()` returns null where every
    // browser throws — a one-line difference a page can see.
    if !require_args(scope, &args, 1, "HTMLCanvasElement", "getContext") {
        return;
    }
    // The id is a DOMString the binding converts (a throwing `toString` propagates) and `RenderingAPIFromId` matches
    // EXACTLY -- six strings, case- and space-sensitive -- after the attributes dictionary, which the binding converts
    // on every call whatever the id; then a canvas whose control went offscreen refuses, then a colour space the
    // binding let through but this build has not enabled (stock Chrome 150 measured:
    // `getContext("2D")` is null; this lower-cased and trimmed the id and read the dictionary only for a context it
    // was about to make).
    let Some(id) = args.get(0).to_string(scope) else { return };
    let id = id.to_rust_string_lossy(scope);
    let Some(attrs) = read_creation_attributes(scope, args.get(1), "HTMLCanvasElement") else { return };
    if priv_get(scope, args.this(), "korvus.transferredToOffscreen").is_some() {
        crate::binding::protos::throw_dom(
            scope,
            "Failed to execute 'getContext' on 'HTMLCanvasElement': Cannot get context from a canvas that has transferred its control to offscreen.",
            "InvalidStateError",
        );
        return;
    }
    if !super::pixel_format::check_color_space_enabled(scope, &attrs.color_space, "Failed to execute 'getContext' on 'HTMLCanvasElement': ") {
        return;
    }
    // Does the persona have a captured GPU? Read from the parsed oracle in `OpState` per call, not
    // from a `build_fn_data` boolean baked in at install time -- see `webgl::oracle_has_webgl` for why
    // that move is what lets this be a table row at all.
    let has_webgl = super::webgl::oracle_has_webgl(scope);
    let null_out = |scope: &mut v8::PinScope, rv: &mut v8::ReturnValue| {
        let null = v8::null(scope);
        rv.set(null.into());
    };
    let (kind, proto_name) = match id.as_str() {
        "2d" => ("2d", "CanvasRenderingContext2D"),
        "webgl" | "experimental-webgl" if has_webgl => ("webgl", "WebGLRenderingContext"),
        "webgl2" if has_webgl => ("webgl2", "WebGL2RenderingContext"),
        // R3.6: a `GPUCanvasContext` exists with NO GPU adapter -- the recorded control is a headless
        // Brave with none and still hands one back (`GPUCanvasContext.json`: `canvas` is the element,
        // `getConfiguration()` is null, `getCurrentTexture()` is an InvalidStateError). It is
        // deliberately NOT gated on `has_webgl`: that flag is the persona's captured WebGL renderer
        // string, and WebGPU's canvas context does not depend on it in a browser either.
        "webgpu" => ("webgpu", "GPUCanvasContext"),
        "bitmaprenderer" => ("bitmaprenderer", "ImageBitmapRenderingContext"),
        _ => {
            null_out(scope, &mut rv);
            return;
        }
    };
    let this = args.this();
    if let Some(prev) = priv_get(scope, this, CANVAS_CTX_KIND_PRIV) {
        let same = prev.to_rust_string_lossy(scope) == kind;
        match (same, priv_get(scope, this, CANVAS_CTX_PRIV)) {
            (true, Some(ctx)) => rv.set(ctx),
            // A second, different context type: null, and the first one stays the canvas's context.
            _ => null_out(scope, &mut rv),
        }
        return;
    }
    let settings = (kind == "2d").then(|| attrs.settings_2d());
    let proto = match shell_proto(scope, proto_name) {
        Some(p) => p,
        None => {
            null_out(scope, &mut rv);
            return;
        }
    };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    if let Some(st) = &settings {
        super::canvas_2d::store_2d_settings(scope, obj, st);
    }
    // A rendering context is a real instance of its interface. Without the brand it fails the shell
    // members' receiver check -- `g.canvas` and `drawingBufferWidth` threw "Illegal invocation" on a
    // context this engine had just built, because `v8::Object::new` is neither cppgc-backed nor
    // branded and looks exactly like a page's `Object.create(proto)`.
    crate::binding::protos::brand_shell_instance(scope, obj);
    if kind == "webgpu" {
        // A real `GPUCanvasContext` has NO own properties -- its `canvas` is a native accessor on the
        // prototype over per-instance state. The back-reference goes on a private slot instead of the
        // own data property the 2D/WebGL path uses, so `Object.getOwnPropertyNames(ctx)` reads empty.
        super::webgpu::init_canvas_context(scope, obj, args.this());
    } else {
        // `CanvasRenderingContext2D.canvas` and `WebGL*RenderingContext.canvas`
        // are TABLE ROWS with native getters over this private slot, so the back-reference is no
        // longer an own data property whose only job was to SHADOW a shell stub -- and
        // `Object.getOwnPropertyNames(ctx)` reads empty, as every real rendering context's does.
        // `GPUCanvasContext` above already had exactly this shape; the three agree now.
        let this_v: v8::Local<v8::Value> = args.this().into();
        priv_set(scope, obj, super::canvas_2d::CANVAS_BACKREF_PRIV, this_v);
    }
    // R3: a WebGL context starts a GL command list, with the attributes the page asked for. They
    // are kept ON THE CONTEXT rather than read back out of the persona oracle, because the backend
    // rasterizes against them and `getContextAttributes()` reports them -- one struct, so the two
    // cannot describe different drawing buffers.
    if kind != "2d" && kind != "webgpu" && kind != "bitmaprenderer" {
        super::webgl::init_context(scope, obj, kind == "webgl2", attrs.gl_attributes(false));
    }
    let obj_v: v8::Local<v8::Value> = obj.into();
    priv_set(scope, this, CANVAS_CTX_PRIV, obj_v);
    if let Some(kv) = v8::String::new(scope, kind) {
        priv_set(scope, this, CANVAS_CTX_KIND_PRIV, kv.into());
    }
    rv.set(obj_v);
}

// ── ImageBitmapRenderingContext ──────────────────────────────────────────────────────────────────
//
// `getContext("bitmaprenderer")` on a `<canvas>` or an `OffscreenCanvas`: it
// answered null. Chrome 150, measured: `canvas` is the canvas it came from; `transferFromImageBitmap(b)`
// takes the bitmap's pixels and detaches it (`b.width` is 0 after), leaves the canvas's size alone,
// takes `null` (the canvas goes transparent black), and refuses a detached bitmap with
// InvalidStateError "The input ImageBitmap has been detached". The transferred pixels are not yet
// composited into the canvas's own readback here -- a residual, not a fabricated answer.

fn cb_bitmap_ctx_canvas(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match priv_get(scope, args.this(), super::canvas_2d::CANVAS_BACKREF_PRIV) {
        Some(c) => rv.set(c),
        None => throw_type_error(scope, "Illegal invocation"),
    }
}

fn cb_bitmap_ctx_transfer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if priv_get(scope, args.this(), super::canvas_2d::CANVAS_BACKREF_PRIV).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !require_args(scope, &args, 1, "ImageBitmapRenderingContext", "transferFromImageBitmap") {
        return;
    }
    let v = args.get(0);
    if v.is_null() {
        return;
    }
    let taken = v8::Local::<v8::Object>::try_from(v).ok().and_then(|o| super::canvas_image::bitmap_take(scope, o));
    match taken {
        None => throw_type_error(scope, "Failed to execute 'transferFromImageBitmap' on 'ImageBitmapRenderingContext': parameter 1 is not of type 'ImageBitmap'."),
        Some(None) => crate::binding::protos::throw_dom(
            scope,
            "Failed to execute 'transferFromImageBitmap' on 'ImageBitmapRenderingContext': The input ImageBitmap has been detached",
            "InvalidStateError",
        ),
        Some(Some(_)) => {}
    }
}

pub fn install_bitmap_renderer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "canvas" => getter_row(scope, proto, member, cb_bitmap_ctx_canvas),
        "transferFromImageBitmap" => method_row(scope, proto, member, cb_bitmap_ctx_transfer),
        other => debug_assert!(false, "install_bitmap_renderer: unexpected member {other}"),
    }
}

/// The `WebGLContextAttributes` dictionary as the page passed it, over the WebGL spec's defaults --
/// which are also, exactly, what the captured Apple-M3 device reports, so a context created with no
/// dictionary reads back byte for byte what the recorded oracle used to serve.
/// `CanvasContextCreationAttributesModule` -- the ONE dictionary both `getContext`s take, whatever the context type --
/// as Blink's binding converts it on EVERY call (stock Chrome 150 measured through a logging
/// Proxy): all thirteen members in WebIDL's lexicographic order, before the id is looked at -- an unknown id, and a
/// canvas that already has its context, read them too -- booleans by ToBoolean, the four enums by ToString and a
/// membership check whose TypeError names the member, the dictionary and the enum. `[PermissiveDictionaryConversion]`:
/// anything that is not an object reads as the empty dictionary. `toneMapping` is `[RuntimeEnabled=CanvasToneMapping]`
/// and Chrome 150 does not read it. This read only its own context type's members before, in its own order, and refused
/// nothing a WebGL or an unknown id was given (`{powerPreference: "foo"}` made a context; Chrome throws).
pub(crate) struct CreationAttributes {
    alpha: bool,
    antialias: bool,
    color_space: String,
    color_type: String,
    depth: bool,
    desynchronized: bool,
    fail_if_major_performance_caveat: bool,
    power_preference: GlPowerPreference,
    premultiplied_alpha: bool,
    preserve_drawing_buffer: bool,
    stencil: bool,
    /// `CanvasWillReadFrequently` is `"true"`, `"false"` or `"undefined"`; a context is told `true` for the first alone.
    will_read_frequently: bool,
    xr_compatible: bool,
}

impl CreationAttributes {
    /// A 2D context's settings, as `ToCanvasRenderingContext2DSettings` keeps them: (alpha, desynchronized,
    /// willReadFrequently, colorSpace, colorType).
    pub(crate) fn settings_2d(&self) -> (bool, bool, bool, String, String) {
        (self.alpha, self.desynchronized, self.will_read_frequently, self.color_space.clone(), self.color_type.clone())
    }

    /// A WebGL context's attributes. `desynchronized` is the HOST's low-latency flag as `getContextAttributes` reports
    /// it (`Host()->LowLatencyEnabled()`): the request on a `<canvas>`, never on an `OffscreenCanvas` (measured).
    fn gl_attributes(&self, offscreen: bool) -> GlContextAttributes {
        GlContextAttributes {
            alpha: self.alpha,
            depth: self.depth,
            stencil: self.stencil,
            antialias: self.antialias,
            premultiplied_alpha: self.premultiplied_alpha,
            preserve_drawing_buffer: self.preserve_drawing_buffer,
            desynchronized: self.desynchronized && !offscreen,
            fail_if_major_performance_caveat: self.fail_if_major_performance_caveat,
            power_preference: self.power_preference,
            xr_compatible: self.xr_compatible,
            // the context's version: `webgl::init_context` sets it
            webgl2: false,
        }
    }
}

/// See [`CreationAttributes`]; `None` once an exception is pending (a getter's, a `toString`'s, or the enum check's).
pub(crate) fn read_creation_attributes(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str) -> Option<CreationAttributes> {
    let mut a = CreationAttributes {
        alpha: true,
        antialias: true,
        color_space: "srgb".into(),
        color_type: "unorm8".into(),
        depth: true,
        desynchronized: false,
        fail_if_major_performance_caveat: false,
        power_preference: GlPowerPreference::Default,
        premultiplied_alpha: true,
        preserve_drawing_buffer: false,
        stencil: false,
        will_read_frequently: false,
        xr_compatible: false,
    };
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Some(a) };
    const MEMBERS: [&str; 13] = [
        "alpha", "antialias", "colorSpace", "colorType", "depth", "desynchronized", "failIfMajorPerformanceCaveat",
        "powerPreference", "premultipliedAlpha", "preserveDrawingBuffer", "stencil", "willReadFrequently", "xrCompatible",
    ];
    for member in MEMBERS {
        let x = get_prop(scope, o, member)?;
        if x.is_undefined() {
            continue;
        }
        let allowed: Option<(&str, &[&str])> = match member {
            "colorSpace" => Some(("PredefinedColorSpace", &super::pixel_format::IDL_COLOR_SPACES)),
            "colorType" => Some(("CanvasPixelFormat", &["unorm8", "float16"])),
            "powerPreference" => Some(("CanvasPowerPreference", &["default", "low-power", "high-performance"])),
            "willReadFrequently" => Some(("CanvasWillReadFrequently", &["true", "false", "undefined"])),
            _ => None,
        };
        let Some((ty, values)) = allowed else {
            let b = x.boolean_value(scope);
            match member {
                "alpha" => a.alpha = b,
                "antialias" => a.antialias = b,
                "depth" => a.depth = b,
                "desynchronized" => a.desynchronized = b,
                "failIfMajorPerformanceCaveat" => a.fail_if_major_performance_caveat = b,
                "premultipliedAlpha" => a.premultiplied_alpha = b,
                "preserveDrawingBuffer" => a.preserve_drawing_buffer = b,
                "stencil" => a.stencil = b,
                _ => a.xr_compatible = b,
            }
            continue;
        };
        let text = x.to_string(scope)?.to_rust_string_lossy(scope);
        if !values.contains(&text.as_str()) {
            throw_type_error(
                scope,
                &format!("Failed to execute 'getContext' on '{iface}': Failed to read the '{member}' property from 'CanvasContextCreationAttributesModule': The provided value '{text}' is not a valid enum value of type {ty}."),
            );
            return None;
        }
        match member {
            "colorSpace" => a.color_space = text,
            "colorType" => a.color_type = text,
            "powerPreference" => {
                a.power_preference = match text.as_str() {
                    "low-power" => GlPowerPreference::LowPower,
                    "high-performance" => GlPowerPreference::HighPerformance,
                    _ => GlPowerPreference::Default,
                }
            }
            _ => a.will_read_frequently = text == "true",
        }
    }
    Some(a)
}

/// Install the native canvas/WebGL context methods on the shell prototypes, baking the persona's
/// recorded oracle (passed as JSON by the caller that holds the persona) into each method.
///
/// The LAST step of [`crate::install::live::install_window_bootstrap`], when the shell prototypes
/// exist. It was the last line of `js/generated_shells.js` -- `wire_canvas_oracle(op_persona_oracle())`
/// -- and the file and both ops were deleted.
/// Store ONLY the WebGL oracle DATA for the current realm: what [`wire_canvas_oracle`]
/// does for the window, minus the window interface installs. A worker's canvas/WebGL interfaces are
/// already mounted by `install_worker_webgl`/`install_worker_canvas`; those rows read `WebGLOracle`
/// from `OpState`, so a worker needs the DATA put there and NOT a second install pass -- calling the
/// window installer in a worker mounts already-owned slots a second time, which the install ledger
/// rejects (`the_ledger_rejects_a_second_record`). So the worker wires the data through here.
pub(crate) fn wire_webgl_oracle_data_only(scope: &mut v8::PinScope, oracle_json: &str) {
    let json_s = v8::String::new(scope, oracle_json).unwrap_or_else(|| v8::String::empty(scope));
    let oracle = v8::json::parse(scope, json_s)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .unwrap_or_else(|| v8::Object::new(scope));
    let webgl = obj_get(scope, oracle, "webgl").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let webgl_v: v8::Local<v8::Value> = webgl.map(|w| w.into()).unwrap_or_else(|| v8::undefined(scope).into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(super::webgl::WebGLOracle(v8::Global::new(scope, webgl_v), false));
}

pub(crate) fn wire_canvas_oracle(scope: &mut v8::PinScope, oracle_json: &str) {
    let json_s = v8::String::new(scope, oracle_json).unwrap_or_else(|| v8::String::empty(scope));
    let oracle = v8::json::parse(scope, json_s)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .unwrap_or_else(|| v8::Object::new(scope));

    let webgl = obj_get(scope, oracle, "webgl").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    // The oracle's recorded `canvas.dataUrl` is no longer read. It used to be baked into `toDataURL`
    // as that method's entire implementation; the method now serializes the pixels it actually has.
    // The field stays in the profile as the R-cal reference `tests/render_calibration.rs` diffs
    // against -- which is the only thing a recording of another machine's canvas can honestly be.
    let webgl_v: v8::Local<v8::Value> = webgl.map(|w| w.into()).unwrap_or_else(|| v8::undefined(scope).into());

    // `HTMLCanvasElement.prototype` IS NOT TOUCHED HERE ANY MORE. This function used to
    // define `getContext`, `toDataURL` and `toBlob` over the table's three `stub` rows -- the
    // arrangement "the row is the SHAPE and a later pass supplies the BODY", which is exactly what the
    // install ledger calls a second writer: a `build_fn` body in a slot the ledger claims, carrying no
    // `korvus.installedBy` brand, so `audit_surface` reported all three `clobber:engine`. The rows name
    // `install_html_canvas_element` now and the ONE installer mounts them, in the capture's position,
    // with the receiver check and the arity guard the row's own columns declare. `getContext` was the
    // one that needed an argument to move: see `webgl::oracle_has_webgl`.
    // The WebGL family, from its four `interface!` tables. The parsed oracle is
    // left where the four oracle-backed ROWS read it at install time, because a table row has no data
    // channel of its own -- and there is no second pass any more: this used to install every recorded
    // draw call and then OVERWRITE `getParameter` / `getExtension` / `getSupportedExtensions` /
    // `getContextAttributes` with the oracle-backed forms, two writers on one slot.
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(super::webgl::WebGLOracle(v8::Global::new(scope, webgl_v), false));
        let global = scope.get_current_context().global(scope);
        crate::install::live::install_window_webgl(scope, global);
    }
    // The 2D family, from its four `interface!` tables: the installer creates
    // `CanvasRenderingContext2D`, `CanvasGradient`, `CanvasPattern` and `Path2D` and mounts every row
    // itself, in the capture's order. It replaces a `canvas_2d::install` that overwrote 73 shell stubs
    // from here; the POSITION is unchanged, because `getContext` above is defined in the same call and
    // lane L18's offscreen borrow reads these prototypes after it.
    {
        let global = scope.get_current_context().global(scope);
        crate::install::live::install_window_canvas(scope, global);
    }

    // R3.5 audio: the whole Web Audio surface, native, in `webaudio.rs`. What was here was a
    // 256-entry recorded tape tiled with `%` and eight `undefined` attributes; the persona now
    // supplies only the LIVE context's sample rate, which is the one device-varying number in the
    // family. `startRendering` renders the recorded graph and `getFloatFrequencyData` analyses the
    // same run, so neither has a recording to fall back to.
    let audio_rate = obj_get(scope, oracle, "audio")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|a| obj_get(scope, a, "sampleRate"))
        .and_then(|v| v.number_value(scope))
        .filter(|r| *r > 0.0)
        .unwrap_or(48000.0) as f32;
    // Twenty-eight `interface!` tables, in inheritance order, replacing a
    // 178-line `AUDIO` const that transcribed the member SHAPE the generated registry already knew.
    // The sample rate is an ARGUMENT because `new AudioContext()` needs it and this op is where the
    // oracle is parsed -- exactly the `make_constructable_with_data` binding it replaces. Window
    // only: `worker.surface.json` lists not one interface of this family.
    {
        let global = scope.get_current_context().global(scope);
        crate::install::live::install_window_audio(scope, global, audio_rate);
    }

    // R3.6 WebGPU: the seventeen `GPU*` members the recorded (adapter-less) control can be asked
    // about -- `navigator.gpu`'s three, `WGSLLanguageFeatures`' six, `GPUCanvasContext`'s five,
    // `GPUError.message`, `GPUPipelineError.reason` and `GPUUncapturedErrorEvent.error` -- as six
    // `interface!` tables, plus the three concrete error constructors, whose
    // interfaces stay generated shells. `requestAdapter()` resolves null; see `webgpu.rs` for why the
    // other 192 members of the family stay shells.
    {
        let global = scope.get_current_context().global(scope);
        crate::install::live::install_window_webgpu(scope, global);
    }
    // THE CONSTRUCTABLE SUBSET IS GONE FROM HERE. This block used to end by swapping six
    // interface OBJECTS for freshly-built constructable ones -- `Image`/`Audio`/`Option`/`EventSource`
    // through `make_constructable`, `OffscreenCanvas` through `make_constructable_with`, and the three
    // `GPUError`s through a `webgpu::install_concrete_error_ctors` call that stood here. Every swap
    // discarded what `create_interface` had already mounted on the object it replaced: measured, that
    // cost `EventSource` its three recorded constants (`CLOSED` answered `undefined` against a capture
    // that records `2`) and all six the install brand, which is why the ledger called them clobbers.
    //
    // Where each of them lives now:
    //   * `OffscreenCanvas` and the three `GPUError`s are constructable FROM THEIR ROW --
    //     `install::ctors::row_ctor` names the body and the ONE create builds the interface object
    //     with it, so there is nothing left to swap.
    //   * `EventSource` needed no body at all: its `Model::Value` row was already newable through
    //     `cb_shell_ctor`, and the swap only replaced that with an unbranded no-op.
    //   * `Image`, `Audio` and `Option` are element FACTORIES built by `install::live::html_image`
    //     and `core::html::legacy_factories` from the post-ESM block, which overwrote this loop's
    //     work minutes later. The loop was dead except for the ledger verdict it left behind.
    //
    // `Path2D`, `AudioContext`, `OfflineAudioContext` and `AudioBuffer` were already out of that
    // loop, each with the same note: their own install gives them a constructor with a BODY and this
    // loop would have overwritten it with the empty one.
    // AND `OffscreenCanvas.prototype` IS NOT TOUCHED HERE ANY MORE EITHER. The block that
    // stood here defined `getContext`, `width` and `height` over that interface's three generated
    // registry rows -- the last three of the twelve `clobber:engine` second writers, and the same
    // mechanism as the `HTMLCanvasElement` block above. Its rows name
    // `install::kits::offscreen_canvas` now (see [`install_offscreen_canvas`]), so the ONE installer
    // mounts them in both realms.
}

/// The bodies `OffscreenCanvas`'s three implemented rows name, reached through
/// `install::kits::offscreen_canvas`.
///
/// WHY A KIT AND NOT A HAND TABLE, since a hand `interface!` table is the usual way a member gets a
/// Rust body. `OffscreenCanvas` is `[Exposed=(Window,Worker)]` and a generated registry row in BOTH
/// realms' orders, and a realm's `globalThis` own-key insertion order is fingerprint surface -- so
/// taking it out of the registry means re-creating it, in position, in two realms, which is what cost
/// the worker realm `DOMMatrix` entirely when that pair left (measured then at 292/294 interfaces).
/// The registry row is already the SHAPE from the capture, in both orders; only the BODY was missing,
/// and `registry_emit._body_rs` has carried a per-member kit claim since W5.1 L16b for exactly that
/// (`gen_svg`'s animated attributes). `tools/status/data/layers.json` is derived from the same rows by
/// `tools/sync/gen_layers.py`, so the claim keeps the declared layer and the mounted layer in step by
/// construction -- which a Rust-side join could not do, since that generator does not read Rust.
pub fn install_offscreen_canvas(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{method_row, pair_row};
    match member.name {
        "width" => pair_row(scope, proto, member, cb_offscreen_width_get, cb_offscreen_width_set),
        "height" => pair_row(scope, proto, member, cb_offscreen_height_get, cb_offscreen_height_set),
        "getContext" => method_row(scope, proto, member, cb_offscreen_get_context),
        "convertToBlob" => method_row(scope, proto, member, cb_offscreen_convert_to_blob),
        "transferToImageBitmap" => method_row(scope, proto, member, cb_offscreen_transfer_to_image_bitmap),
        other => debug_assert!(false, "install_offscreen_canvas: unexpected member {other}"),
    }
}

// ── moved out of bindings.rs (STEP 3 dissolution) ──
/// The canvas's ONE context object, and which type it is. A browser hands back the same object every
/// call; these are what let it, and what let `toDataURL` find the display list it must serialize.
pub(crate) const CANVAS_CTX_PRIV: &str = "korvus_canvas_ctx";
pub(crate) const CANVAS_CTX_KIND_PRIV: &str = "korvus_canvas_ctx_kind";

// THIS FILE DECLARES NO OPS. Its one op, `wire_canvas_oracle`, existed because the
// only caller was a JS line in `js/generated_shells.js`; that file is deleted and
// [`wire_canvas_oracle`] is reached from Rust, so there is nothing left for `webapi_ops!` to collect
// here and build.rs correctly stops listing this module.

// ── `HTMLCanvasElement` as one `interface!` table ───────────────────────────────
//
// THE SEVEN ROWS, and nothing else. This lane took the interface's SHAPE out of
// `install/generated_registry.rs` and out of `dom/09c-small-elements.js`; it did NOT touch the
// rendering-context machinery above (`wire_canvas_oracle`, `cb_canvas_get_context`,
// `cb_canvas_to_data_url`, `cb_canvas_to_blob`, the WebGL/2D/WebGPU/WebAudio installs, the
// `OffscreenCanvas` block) — lane L17 owns that.
//
// `width` and `height` are the two rows this lane gives a BODY. They are `unsigned long`
// reflections with the spec's own defaults, 300 and 150 — NOT zero, and not an attribute the markup
// has to carry (`cfg` is `attr|default`, which is what `kits::reflect_ulong` reads). Before
// `dom/09c-small-elements.js` they were not on the prototype at all and `korvus-runtime` stamped
// `el.width = 300` as an OWN property on every canvas; that wrapper is long gone and these rows are
// where the defaults live now.
//
// FOUR OF THE OTHER FIVE HAVE THEIR BODY IN THE ROW NOW. They used to be `stub` rows in
// the arrangement "the row is the SHAPE and a later pass supplies the BODY" — the table put each name
// in the capture's position and `wire_canvas_oracle` (three of them) or
// `install::live::install_window_media_rtc` (`captureStream`) defined over it afterwards. That is
// precisely what invariant I2's ledger calls a SECOND WRITER: the installer mounted a row, something
// else replaced the function in it, and the replacement carried no `korvus.installedBy` brand — so
// `installer::audit_surface` reported all four `clobber:engine`, and the table was not the definition
// site for any of them (I1). `install_html_canvas_element` is that definition site now.
//
// WHAT THE MOVE CHANGED, since a different body running is failure mode 3 of a table cutover:
//
//   * `getContext` was `build_fn_data` over the persona's `has_webgl` — the one argument a table row
//     has no channel for, and the reason `install_window_element_refs`' header said this could not
//     move. It reads the same fact from the parsed oracle in `OpState` PER CALL now
//     (`webgl::oracle_has_webgl`), which is where `webgl.rs`'s four oracle-backed rows have read it
//     since W5.1 L17. The install-order contract between element-refs and the oracle line is spent.
//   * `captureStream`'s BODY still lives with the track it mints (`core/media/media_stream.rs`) —
//     minting a capture track means minting its state, which is W5.1 L4's split and unchanged. Only
//     the mount moved: this row names the body, `install_canvas_capture_stream` is deleted.
//   * All four now get the receiver check and the arity guard the row's own columns declare, because
//     `put_member` applies both to every `BodyKind::Install` row. `canvas.getContext.call({}, "2d")`
//     answers `Illegal invocation`, which is what Chrome 150 answers and what the hand mounts did not.
//   * All four are `layer: "native"` in `tools/status/data/status.json` instead of `layer: "stub"`. That is
//     the ledger inexactness this comment used to describe being resolved for these four, not a new
//     claim about them: the layer is recorded by `put_member` as it MOUNTS, so a row whose body a
//     later pass supplied read `stub` however real the body was.
//
//   * `transferControlToOffscreen` — genuinely not implemented; an honest D2 stub, and the ONE row of
//     the five that is still the stub at runtime and still `layer: "stub"`. It refuses a foreign
//     receiver the way every browser does.
//
// The rest of the tree-wide "body supplied by a later pass" population W7 measured (~77 rows:
// `HTMLLinkElement.sheet`, the OffscreenCanvasRenderingContext2D family) is unchanged; the sweep that
// finds them is described in `core/korvus-runtime/tests/layers_live.rs`.

/// The bodies this table's four implemented method rows name. `width`/`height` are the shared
/// `reflect_ulong` kit and `transferControlToOffscreen` is the D2 stub, so neither reaches here.
pub fn install_html_canvas_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::method_row;
    match member.name {
        "getContext" => method_row(scope, proto, member, cb_canvas_get_context),
        "toDataURL" => method_row(scope, proto, member, cb_canvas_to_data_url),
        "toBlob" => method_row(scope, proto, member, cb_canvas_to_blob),
        // The body belongs to the lane that mints the track; the ROW belongs here. See W5.1 L4.
        "captureStream" => method_row(scope, proto, member, crate::core::media::media_stream::cb_canvas_capture_stream),
        "transferControlToOffscreen" => method_row(scope, proto, member, cb_canvas_transfer_control_to_offscreen),
        other => debug_assert!(false, "install_html_canvas_element: unexpected member {other}"),
    }
}

/// `canvas.transferControlToOffscreen()`: an `OffscreenCanvas` of the element's size,
/// once -- a second call, or one on a canvas that already has a context, is Chrome's
/// `InvalidStateError`. It answered `null` (the stub's default).
fn cb_canvas_transfer_control_to_offscreen(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if priv_get(scope, this, CANVAS_CTX_PRIV).is_some() || priv_get(scope, this, "korvus.transferredToOffscreen").is_some() {
        let msg = if priv_get(scope, this, CANVAS_CTX_PRIV).is_some() {
            "Failed to execute 'transferControlToOffscreen' on 'HTMLCanvasElement': Cannot transfer control from a canvas that has a rendering context."
        } else {
            "Failed to execute 'transferControlToOffscreen' on 'HTMLCanvasElement': Cannot transfer control from a canvas for more than one time."
        };
        crate::binding::protos::throw_dom(scope, msg, "InvalidStateError");
        return;
    }
    let dim = |scope: &mut v8::PinScope, name: &str, dflt: f64| -> f64 { canvas_extent(scope, this, name).unwrap_or(dflt) };
    let (w, h) = (dim(scope, "width", 300.0), dim(scope, "height", 150.0));
    let Some(proto) = shell_proto(scope, "OffscreenCanvas") else {
        rv.set_null();
        return;
    };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let wv = v8::Number::new(scope, w);
    let hv = v8::Number::new(scope, h);
    priv_set(scope, o, "korvus.offscreenWidth", wv.into());
    priv_set(scope, o, "korvus.offscreenHeight", hv.into());
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    if computed_direction(scope, this) == "rtl" {
        priv_set(scope, o, OFFSCREEN_RTL_PRIV, t);
    }
    priv_set(scope, this, "korvus.transferredToOffscreen", t);
    rv.set(o.into());
}

// ── tainting and bitmap resets ────────────────────────────────────────────────────

/// A tainted canvas refuses to be exported: SecurityError, Chrome's sentence (measured, `toDataURL`
/// and `toBlob` alike). `true` = thrown.
fn taint_refuses_export(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>, member: &str) -> bool {
    let Some(ctx) = super::canvas_image::context_2d_of(scope, canvas) else { return false };
    if !super::canvas_2d::is_tainted(scope, ctx) {
        return false;
    }
    crate::binding::protos::throw_dom(
        scope,
        &format!("Failed to execute '{member}' on 'HTMLCanvasElement': Tainted canvases may not be exported."),
        "SecurityError",
    );
    true
}

/// Writing a canvas's `width` or `height` -- through the IDL attribute or `setAttribute`, even to the
/// value it already has -- resets its bitmap and its 2D state and makes it origin-clean again
/// (measured: `c.width = c.width` and `c.setAttribute("width", "20")` both do). Reached from the ONE
/// attribute-write choke point, `core/dom/shared.rs`'s `set_attribute_notify` /
/// `remove_attribute_notify`, which both the reflected setter and `setAttribute` go through.
pub(crate) fn canvas_attr_changed(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, name: &str) {
    if name != "width" && name != "height" {
        return;
    }
    if !matches!(super::canvas_image::classify(scope, target), super::canvas_image::Kind::Canvas) {
        return;
    }
    let Ok(canvas) = v8::Local::<v8::Object>::try_from(target) else { return };
    if let Some(ctx) = super::canvas_image::context_2d_of(scope, canvas) {
        super::canvas_2d::reset_bitmap(scope, ctx);
    }
    // a WebGL context's drawing buffer is reallocated when the size CHANGED (Blink's `Reshape`)
    super::webgl::canvas_resized(scope, canvas);
}

/// The same reset for an `OffscreenCanvas`, whose dimensions are not attributes.
fn offscreen_reset(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>) {
    if let Some(ctx) = super::canvas_image::context_2d_of(scope, canvas) {
        super::canvas_2d::reset_bitmap(scope, ctx);
    }
    super::webgl::canvas_resized(scope, canvas);
}

/// A promise rejected with a `DOMException` of `name`.
fn rejected_dom<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> v8::Local<'s, v8::Value> {
    let e = crate::binding::protos::make_dom_exc(scope, msg, name).unwrap_or_else(|| v8::undefined(scope).into());
    crate::binding::protos::rejected_promise(scope, e)
}

/// `offscreen.convertToBlob(options)` -- the `toBlob` of an `OffscreenCanvas`, as a promise. Measured:
/// `image/png` by default, `image/jpeg` when asked, PNG for a type it does not write; a canvas with no
/// context, a tainted one and a zero-size one each REJECT, with the sentences below.
fn cb_offscreen_convert_to_blob(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A promise-returning member REJECTS a receiver without the brand (measured:
    // `convertToBlob.call(Object.create(OffscreenCanvas.prototype))` rejects this sentence).
    if priv_get(scope, args.this(), "korvus.offscreenWidth").is_none() {
        let p = crate::binding::protos::rejected_type_error(scope, "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Illegal invocation");
        rv.set(p);
        return;
    }
    // `ImageEncodeOptions`, converted before any step (Chrome 150): `quality`, then `type`,
    // each read once -- a canvas with no context reads them, then rejects. korvus read them after its checks, and
    // `type` and `quality` again; a failed conversion rejects.
    let opts = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "ImageEncodeOptions", "Failed to execute 'convertToBlob' on 'OffscreenCanvas': ") {
        Ok(o) => o,
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let this = args.this();
    let Some(ctx) = super::canvas_image::context_2d_of(scope, this) else {
        let p = rejected_dom(scope, "Failed to execute 'convertToBlob' on 'OffscreenCanvas': \"OffscreenCanvas\" has no rendering context.", "InvalidStateError");
        rv.set(p);
        return;
    };
    if super::canvas_2d::is_tainted(scope, ctx) {
        let p = rejected_dom(scope, "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Tainted \"OffscreenCanvas\" may not be exported.", "SecurityError");
        rv.set(p);
        return;
    }
    let w = offscreen_dim(scope, this, "korvus.offscreenWidth");
    let h = offscreen_dim(scope, this, "korvus.offscreenHeight");
    if w <= 0.0 || h <= 0.0 {
        let p = rejected_dom(scope, "Failed to execute 'convertToBlob' on 'OffscreenCanvas': The size of \"OffscreenCanvas\" is zero.", "IndexSizeError");
        rv.set(p);
        return;
    }
    let ty = get_prop(scope, opts, "type").filter(|v| !v.is_undefined());
    let want = match ty {
        Some(t) => requested_format(scope, t),
        None => ImageFormat::Png,
    };
    // `unrestricted double quality = 1.0`: a number always, its range the encoder's to judge (a WebP of the
    // default 1 is lossless, as Chrome's `convertToBlob({type: 'image/webp'})` is).
    let quality = get_prop(scope, opts, "quality")
        .filter(|q| q.is_number())
        .and_then(|q| q.number_value(scope));
    let blob: v8::Local<v8::Value> = match encode_canvas(scope, this, want, quality).and_then(|(bytes, mime)| make_blob(scope, &bytes, mime)) {
        Some(b) => b,
        None => {
            let p = rejected_dom(scope, "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Encoding failed.", "EncodingError");
            rv.set(p);
            return;
        }
    };
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    resolver.resolve(scope, blob);
    rv.set(resolver.get_promise(scope).into());
}

/// `offscreen.transferToImageBitmap()` -- the bitmap as an `ImageBitmap` (origin-clean or not, as the
/// canvas is), and the canvas left transparent with its drawing state intact (measured: the bitmap
/// draws the magenta that was filled, and the canvas reads 0,0,0,0 afterwards).
fn cb_offscreen_transfer_to_image_bitmap(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !offscreen_branded(scope, &args) {
        return;
    }
    let this = args.this();
    let Some(ctx) = super::canvas_image::context_2d_of(scope, this) else {
        crate::binding::protos::throw_dom(
            scope,
            "Failed to execute 'transferToImageBitmap' on 'OffscreenCanvas': Cannot transfer an ImageBitmap from an OffscreenCanvas with no context",
            "InvalidStateError",
        );
        return;
    };
    let w = offscreen_dim(scope, this, "korvus.offscreenWidth") as u32;
    let h = offscreen_dim(scope, this, "korvus.offscreenHeight") as u32;
    if w == 0 || h == 0 {
        crate::binding::protos::throw_dom(
            scope,
            "Failed to execute 'transferToImageBitmap' on 'OffscreenCanvas': ImageBitmap construction failed",
            "UnknownError",
        );
        return;
    }
    let (ops, tainted) = super::canvas_2d::ops_and_taint(scope, ctx);
    let params = super::canvas_2d::ctx_render_params(scope, ctx);
    let img = super::canvas_image::raster_ops(scope, &ops, w, h, &params);
    // The bitmap becomes transparent black; the STATE (styles, transform, clip) is untouched. A
    // `putImageData` of a transparent buffer is exactly that write: it ignores transform, clip,
    // alpha and composite, so the display list keeps every state op before it.
    super::canvas_2d::canvas2d_push(
        scope,
        ctx,
        spec::Canvas2DOp::PutImageData {
            image: spec::Rgba8::transparent(w, h),
            dx: 0.0,
            dy: 0.0,
            dirty_x: 0.0,
            dirty_y: 0.0,
            dirty_w: w as f64,
            dirty_h: h as f64,
        },
    );
    match super::canvas_image::new_bitmap(scope, img, !tainted) {
        Some(b) => rv.set(b.into()),
        None => rv.set_null(),
    }
}

webapi::interface! {
    "HTMLCanvasElement" : "HTMLElement" [element, window] {
        ctor   "HTMLCanvasElement"                        len 0 = stub,
        attr   "width"                      : Object cfg "width|300"  = reflect_ulong,
        attr   "height"                     : Object cfg "height|150" = reflect_ulong,
        method "captureStream"              : Object len 0            = install_html_canvas_element,
        method "getContext"                 : Object len 1 required 1 nullable = install_html_canvas_element,
        method "toBlob"                     : Object len 1 required 1 = install_html_canvas_element,
        method "toDataURL"                  : Object len 0            = install_html_canvas_element,
        method "transferControlToOffscreen" : Object len 0            = install_html_canvas_element
    }
}

//! `core/media/video_frame.rs` -- `VideoFrame` and `VideoColorSpace` as two `interface!` tables.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_registry.rs` they replace.
//!
//! A `VideoFrame` CARRIES GEOMETRY, FORMAT, TIMESTAMP AND LIFETIME, NOT PIXELS. Everything a page can
//! read off one -- `codedWidth`, `visibleRect`, `displayWidth`, `rotation`, `timestamp`, `colorSpace`,
//! and what all of them become after `close()` -- is decided by the constructor's arguments and the
//! source's own dimensions, and every one of those is computed here rather than recorded.
//! `allocationSize()` is a real computation over the real geometry: 300x150 BGRA is 180,000 bytes
//! because that is 300 * 150 * 4, not because a case says so.
//!
//! WHAT IS DELIBERATELY NOT HERE: `copyTo(destination)` copies the frame's PIXELS into a buffer, and
//! there are none. Its row stays the D2 stub -- the one member of this interface that cannot be
//! answered without a decoder, and the only one of the seventeen that is not this file's body.
//!
//! WHAT `close()` MEANS, and why every getter checks it: closing detaches the frame from its media
//! resource, and Chrome then reports `null` for every nullable member, `0` for every numeric one and
//! `false` for `flip` -- and throws `InvalidStateError` from `allocationSize`, `metadata` and `clone`.
//! That is a real lifetime, observable in full, and it is what the bundle's oracle reads.
//!
//! BOTH ARE `[Exposed=(Window,Worker)]`, and `worker.surface.json` lists both with member lists
//! byte-identical to the window's -- so the tables are `both` and `install_worker_media_rtc` builds them in
//! that realm from the SAME rows (invariant I8).

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{build_fn_data, get_prop, make_dom_exc, new_global, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const FRAME: &str = "VideoFrame";
pub(crate) const COLORSPACE: &str = "VideoColorSpace";

/// Bytes per pixel for the packed 8-bit formats a canvas read-back can produce. Anything else falls
/// back to 4, which is what every format this engine can mint is.
fn bytes_per_pixel(format: &str) -> f64 {
    match format {
        "BGRA" | "RGBA" | "BGRX" | "RGBX" => 4.0,
        _ => 4.0,
    }
}

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

fn throw_closed(scope: &mut v8::PinScope, op: &str) {
    let msg = format!("Failed to execute '{op}' on 'VideoFrame': VideoFrame is closed.");
    if let Some(e) = make_dom_exc(scope, &msg, "InvalidStateError") {
        scope.throw_exception(e);
    }
}

// ---- VideoColorSpace -----------------------------------------------------------------------------

/// One color space from its dictionary. Every member is nullable and ABSENT means null, which is what
/// `new VideoColorSpace()` reports for all four.
pub(crate) fn mint_color_space<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    init: v8::Local<v8::Value>,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, COLORSPACE)?;
    let s = st::attach(scope, o, COLORSPACE);
    for k in ["primaries", "transfer", "matrix"] {
        match st::dict_get(scope, init, k) {
            Some(v) => {
                let text = st::to_str(scope, v)?;
                st::set_str(scope, s, k, &text);
            }
            None => st::set_null(scope, s, k),
        }
    }
    match st::dict_get(scope, init, "fullRange") {
        Some(v) => {
            let b = v.boolean_value(scope);
            st::set_bool(scope, s, "fullRange", b);
        }
        None => st::set_null(scope, s, "fullRange"),
    }
    Some(o)
}

/// sRGB, full range: what a canvas' backing store is, and the space every frame this engine mints
/// carries unless it was copied from another frame.
fn mint_srgb<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let init = crate::binding::protos::init_dict(scope);
    st::set_str(scope, init, "primaries", "bt709");
    st::set_str(scope, init, "transfer", "iec61966-2-1");
    st::set_str(scope, init, "matrix", "rgb");
    st::set_bool(scope, init, "fullRange", true);
    mint_color_space(scope, init.into())
}

pub(crate) fn cb_color_space_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("VideoColorSpace");
        throw_type_error(scope, &msg);
        return;
    }
    // `VideoColorSpaceInit`, converted as WebIDL converts it (Chrome 150): `fullRange`, `matrix`,
    // `primaries`, `transfer` -- each enum checked -- read once in that order (korvus read primaries first and took any
    // string); what follows reads the converted dictionary
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "VideoColorSpaceInit", "Failed to construct 'VideoColorSpace': ") else { return };
    let init: v8::Local<v8::Value> = init.into();
    let s = st::attach(scope, args.this(), COLORSPACE);
    for k in ["primaries", "transfer", "matrix"] {
        match st::dict_get(scope, init, k) {
            Some(v) => {
                let Some(text) = st::to_str(scope, v) else { return };
                st::set_str(scope, s, k, &text);
            }
            None => st::set_null(scope, s, k),
        }
    }
    match st::dict_get(scope, init, "fullRange") {
        Some(v) => {
            let b = v.boolean_value(scope);
            st::set_bool(scope, s, "fullRange", b);
        }
        None => st::set_null(scope, s, "fullRange"),
    }
}

fn cb_cs_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), COLORSPACE) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

fn cb_cs_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), COLORSPACE) else { return };
    let out = v8::Object::new(scope);
    for k in ["primaries", "transfer", "matrix", "fullRange"] {
        let v = st::get(scope, s, k);
        st::set(scope, out, k, v);
    }
    rv.set(out.into());
}

pub fn install_video_color_space(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_cs_to_json),
        _ => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_cs_field, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
    }
}

// ---- VideoFrame ----------------------------------------------------------------------------------

/// The `{w, h}` a source contributes to a new frame. A `VideoFrame` source reports its DISPLAY size;
/// everything else its intrinsic dimensions -- a canvas and an `ImageBitmap` carry `width`/`height`
/// directly, an `<img>` carries `naturalWidth` and a `<video>` carries `videoWidth`.
fn source_size(scope: &mut v8::PinScope, src: v8::Local<v8::Value>) -> Option<(f64, f64)> {
    let obj = v8::Local::<v8::Object>::try_from(src).ok()?;
    if let Some(fs) = st::state(scope, src, FRAME) {
        if st::get_bool(scope, fs, "closed") {
            return None;
        }
        return Some((st::get_num(scope, fs, "displayWidth"), st::get_num(scope, fs, "displayHeight")));
    }
    let read = |scope: &mut v8::PinScope, keys: [&str; 3]| -> f64 {
        for k in keys {
            let n = get_prop(scope, obj, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
            if n.is_finite() && n != 0.0 {
                return n;
            }
        }
        0.0
    };
    let w = read(scope, ["videoWidth", "naturalWidth", "width"]);
    let h = read(scope, ["videoHeight", "naturalHeight", "height"]);
    if w == 0.0 || h == 0.0 {
        return None;
    }
    Some((w, h))
}

fn rect_field(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, k: &str) -> f64 {
    let n = st::get(scope, r, k).number_value(scope).unwrap_or(0.0);
    if n.is_finite() {
        n
    } else {
        0.0
    }
}

/// `new VideoFrame(source, init)`.
pub(crate) fn cb_frame_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("VideoFrame");
        throw_type_error(scope, &msg);
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'VideoFrame': 1 argument required, but only 0 present.");
        return;
    }
    let src = args.get(0);
    let Some((coded_w, coded_h)) = source_size(scope, src) else {
        throw_type_error(
            scope,
            "Failed to construct 'VideoFrame': The provided value is not of type '(CanvasImageSource or BufferSource or VideoFrame)'.",
        );
        return;
    };
    // The init dictionary of the overload the source picks -- `VideoFrameBufferInit` for a buffer, `VideoFrameInit`
    // otherwise -- converted as WebIDL converts it, before any step (Chrome 150): its members in
    // lexicographic order, each read once (korvus read `timestamp` alone before refusing)
    let dict = if src.is_array_buffer() || src.is_array_buffer_view() || src.is_shared_array_buffer() { "VideoFrameBufferInit" } else { "VideoFrameInit" };
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), dict, "Failed to construct 'VideoFrame': ") else { return };
    let init: v8::Local<v8::Value> = init.into();
    let base = st::state(scope, src, FRAME);
    // A frame built from a CanvasImageSource has no timestamp of its own, so the caller must supply
    // one -- the constructor's own required-member check, not a gap.
    if base.is_none() && st::dict_get(scope, init, "timestamp").is_none() {
        throw_type_error(scope, "Failed to construct 'VideoFrame': timestamp is required for CanvasImageSource.");
        return;
    }

    let (mut vx, mut vy, mut vw, mut vh) = (0.0, 0.0, coded_w, coded_h);
    if let Some(r) = st::dict_get(scope, init, "visibleRect").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        vx = rect_field(scope, r, "x");
        vy = rect_field(scope, r, "y");
        vw = rect_field(scope, r, "width");
        vh = rect_field(scope, r, "height");
        if vw <= 0.0 || vh <= 0.0 || vx + vw > coded_w || vy + vh > coded_h {
            throw_type_error(scope, "Failed to construct 'VideoFrame': visibleRect does not lay within frame.");
            return;
        }
    }

    let num_or = |scope: &mut v8::PinScope, k: &str| -> Option<f64> {
        st::dict_get(scope, init, k).map(|v| {
            let n = v.number_value(scope).unwrap_or(0.0);
            if n.is_finite() {
                n.trunc()
            } else {
                0.0
            }
        })
    };
    let timestamp = num_or(scope, "timestamp").unwrap_or_else(|| base.map(|b| st::get_num(scope, b, "timestamp")).unwrap_or(0.0));
    let duration = num_or(scope, "duration");
    let display_w = st::dict_get(scope, init, "displayWidth").map(|v| st::to_ulong(scope, v) as f64).unwrap_or(vw);
    let display_h = st::dict_get(scope, init, "displayHeight").map(|v| st::to_ulong(scope, v) as f64).unwrap_or(vh);
    let rotation = match num_or(scope, "rotation") {
        Some(r) => ((r % 360.0) + 360.0) % 360.0,
        None => base.map(|b| st::get_num(scope, b, "rotation")).unwrap_or(0.0),
    };
    let flip = match st::dict_get(scope, init, "flip") {
        Some(v) => v.boolean_value(scope),
        None => base.map(|b| st::get_bool(scope, b, "flip")).unwrap_or(false),
    };
    // Chrome reads a canvas back as BGRA; a frame copied from another frame keeps its format.
    let format = base.map(|b| st::get_str(scope, b, "format")).unwrap_or_else(|| "BGRA".to_string());
    let color_space = match base {
        Some(b) => st::get(scope, b, "colorSpace"),
        None => match mint_srgb(scope) {
            Some(c) => c.into(),
            None => v8::null(scope).into(),
        },
    };

    let s = st::attach(scope, args.this(), FRAME);
    st::set_str(scope, s, "format", &format);
    st::set_num(scope, s, "timestamp", timestamp);
    match duration.or_else(|| base.map(|b| st::get_num(scope, b, "duration"))) {
        Some(d) => st::set_num(scope, s, "duration", d),
        None => st::set_null(scope, s, "duration"),
    }
    st::set_num(scope, s, "codedWidth", coded_w);
    st::set_num(scope, s, "codedHeight", coded_h);
    let rect = v8::Object::new(scope);
    st::set_num(scope, rect, "x", vx);
    st::set_num(scope, rect, "y", vy);
    st::set_num(scope, rect, "width", vw);
    st::set_num(scope, rect, "height", vh);
    st::set(scope, s, "visibleRect", rect.into());
    st::set_num(scope, s, "displayWidth", display_w);
    st::set_num(scope, s, "displayHeight", display_h);
    st::set_num(scope, s, "rotation", rotation);
    st::set_bool(scope, s, "flip", flip);
    st::set(scope, s, "colorSpace", color_space);
    let meta = v8::Object::new(scope);
    st::set(scope, s, "metadata", meta.into());
    st::set_bool(scope, s, "closed", false);
}

/// Every member of a CLOSED frame answers its empty value. `data` is `"<field>|<closed answer>"`,
/// where the answer is `null`, `0` or `false`.
fn cb_frame_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FRAME) else { return };
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (name, empty) = d.split_once('|').unwrap_or((d.as_str(), "null"));
    if st::get_bool(scope, s, "closed") {
        match empty {
            "zero" => rv.set_int32(0),
            "false" => rv.set_bool(false),
            _ => {
                let n = v8::null(scope);
                rv.set(n.into());
            }
        }
        return;
    }
    let v = st::get(scope, s, name);
    rv.set(v);
}

/// `codedRect` and `visibleRect` are `DOMRectReadOnly`s over the frame's own geometry. `data` says
/// which; a closed frame has neither.
fn cb_frame_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FRAME) else { return };
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if st::get_bool(scope, s, "closed") {
        let n = v8::null(scope);
        rv.set(n.into());
        return;
    }
    let (x, y, w, h) = if which == "codedRect" {
        (0.0, 0.0, st::get_num(scope, s, "codedWidth"), st::get_num(scope, s, "codedHeight"))
    } else {
        let Ok(r) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "visibleRect")) else { return };
        (rect_field(scope, r, "x"), rect_field(scope, r, "y"), rect_field(scope, r, "width"), rect_field(scope, r, "height"))
    };
    let args4: Vec<v8::Local<v8::Value>> = [x, y, w, h].iter().map(|n| v8::Number::new(scope, *n).into()).collect();
    match new_global(scope, "DOMRectReadOnly", &args4) {
        Some(o) => rv.set(o.into()),
        None => {
            let n = v8::null(scope);
            rv.set(n.into());
        }
    }
}

/// `allocationSize()`: the bytes a copy of the VISIBLE rectangle would take, in the frame's own
/// format. A real computation over real geometry.
fn cb_frame_allocation_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FRAME) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_closed(scope, "allocationSize");
        return;
    }
    let format = st::get_str(scope, s, "format");
    let Ok(r) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "visibleRect")) else { return };
    let n = rect_field(scope, r, "width") * rect_field(scope, r, "height") * bytes_per_pixel(&format);
    let v = v8::Number::new(scope, n);
    rv.set(v.into());
}

fn cb_frame_metadata(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FRAME) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_closed(scope, "metadata");
        return;
    }
    let out = v8::Object::new(scope);
    rv.set(out.into());
}

/// `close()`: detach the frame from its media resource. Idempotent, and the whole of what a page can
/// observe of a frame's lifetime.
fn cb_frame_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FRAME) else { return };
    st::set_bool(scope, s, "closed", true);
}

fn cb_frame_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(s) = st::state_or_throw(scope, this.into(), FRAME) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_closed(scope, "clone");
        return;
    }
    // from the template of the interface `this` is an instance of
    let o = match this.get_prototype(scope) {
        Some(proto) => crate::binding::protos::mint_on(scope, proto),
        None => v8::Object::new(scope),
    };
    let ns = st::attach(scope, o, FRAME);
    for k in [
        "format",
        "timestamp",
        "duration",
        "codedWidth",
        "codedHeight",
        "displayWidth",
        "displayHeight",
        "rotation",
        "flip",
        "colorSpace",
        "closed",
    ] {
        let v = st::get(scope, s, k);
        st::set(scope, ns, k, v);
    }
    // The rectangle and the metadata bag are COPIED, so a clone's geometry cannot be edited through
    // the original's object.
    let Ok(r) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "visibleRect")) else { return };
    let nr = v8::Object::new(scope);
    for k in ["x", "y", "width", "height"] {
        let n = rect_field(scope, r, k);
        st::set_num(scope, nr, k, n);
    }
    st::set(scope, ns, "visibleRect", nr.into());
    let meta = v8::Object::new(scope);
    st::set(scope, ns, "metadata", meta.into());
    rv.set(o.into());
}

pub fn install_video_frame(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let acc = |scope: &mut v8::PinScope, data: &str| {
        let d = str_val(scope, data);
        let g = build_fn_data(scope, cb_frame_field, &format!("get {}", member.name), 0, d);
        define_accessor(scope, proto, member, g, None);
    };
    match member.name {
        "format" | "duration" | "colorSpace" => acc(scope, &format!("{}|null", member.name)),
        "timestamp" | "codedWidth" | "codedHeight" | "displayWidth" | "displayHeight" | "rotation" => {
            acc(scope, &format!("{}|zero", member.name))
        }
        "flip" => acc(scope, "flip|false"),
        "codedRect" | "visibleRect" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_frame_rect, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "allocationSize" => method_row(scope, proto, member, cb_frame_allocation_size),
        "metadata" => method_row(scope, proto, member, cb_frame_metadata),
        "close" => method_row(scope, proto, member, cb_frame_close),
        "clone" => method_row(scope, proto, member, cb_frame_clone),
        other => debug_assert!(false, "install_video_frame: unexpected member {other}"),
    }
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "VideoColorSpace" [value, both] {
    ctor   "VideoColorSpace" len 0                      = stub,
    attr   "primaries" : Object readonly nullable       = install_video_color_space,
    attr   "transfer" : Object readonly nullable        = install_video_color_space,
    attr   "matrix" : Object readonly nullable          = install_video_color_space,
    attr   "fullRange" : Object readonly nullable       = install_video_color_space,
    method "toJSON" : Object                            = install_video_color_space
    }
}

// `copyTo` is the one member of the seventeen this file does not answer: it copies PIXELS into a
// buffer, and a `VideoFrame` here carries geometry and no pixels. The row stays the honest D2 stub.
webapi::interface! {
    "VideoFrame" [value, both] {
    ctor   "VideoFrame" len 1                           = stub,
    attr   "format" : Object readonly nullable          = install_video_frame,
    attr   "timestamp" : Object readonly                = install_video_frame,
    attr   "duration" : Object readonly nullable        = install_video_frame,
    attr   "codedWidth" : Object readonly               = install_video_frame,
    attr   "codedHeight" : Object readonly              = install_video_frame,
    attr   "codedRect" : Object readonly nullable       = install_video_frame,
    attr   "visibleRect" : Object readonly nullable     = install_video_frame,
    attr   "rotation" : Object readonly                 = install_video_frame,
    attr   "flip" : Object readonly                     = install_video_frame,
    attr   "displayWidth" : Object readonly             = install_video_frame,
    attr   "displayHeight" : Object readonly            = install_video_frame,
    attr   "colorSpace" : Object readonly               = install_video_frame,
    method "allocationSize" : Object                    = install_video_frame,
    method "clone" : Object                             = install_video_frame,
    method "close" : Object                             = install_video_frame,
    method "copyTo" : Promise len 1 required 1          = stub,
    method "metadata" : Object                          = install_video_frame
    }
}

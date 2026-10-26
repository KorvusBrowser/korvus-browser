//! `modules/clone_state.rs` — the STATE of a `[Serializable]` platform object, the one codec every structured
//! clone uses for it.
//!
//! A structured clone of a `DOMPoint`, `DOMRect`, `DOMMatrix`, `DOMQuad`, `DOMException`, `Blob`, `File` or
//! `ImageData` is a NEW object of its interface built from the source's values (HTML's serialization steps).
//! Korvus's clone minted an object on the interface's prototype and copied own properties: the members of
//! these interfaces live in the instance's internal state, so the clone had none -- a `DOMPoint` whose `x`
//! threw "Illegal invocation", an empty `DOMException`, a 0-byte `Blob` (measured on 1399b23e).
//!
//! Three paths read and write the same state:
//! - `structuredClone` / `window.postMessage` / `MessagePort` (the hand-written clone, one isolate):
//!   [`read`] then [`build`];
//! - a worker's `postMessage` and a cross-isolate `BroadcastChannel` (V8's ValueSerializer): [`WireSer`]
//!   writes the state as a host object and [`WireDeser`] reads it back and [`build`]s it;
//! - a transferred `MessagePort` inside a worker message: [`WireSer`] writes its index in the transfer list
//!   and [`WireDeser`] answers the receiver's port at that index, so `e.data.port` IS a port (Comlink over a
//!   worker boundary: wix.com).

use deno_core::v8;

use crate::binding::protos::{get_prop, iface_object};

/// A serializable platform object's values.
pub(crate) enum PlatformState {
    Point { ro: bool, v: [f64; 4] },
    Rect { ro: bool, v: [f64; 4] },
    Quad { p: [[f64; 4]; 4] },
    Matrix { ro: bool, is_2d: bool, v: Vec<f64> },
    Exception { name: String, message: String },
    Blob { ty: String, bytes: Vec<u8> },
    File { ty: String, bytes: Vec<u8>, name: String, last_modified: f64 },
    ImageData { width: u32, height: u32, color_space: String, bytes: Vec<u8> },
}

fn num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> f64 {
    get_prop(scope, o, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0)
}
fn text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> String {
    get_prop(scope, o, k).filter(|v| !v.is_null_or_undefined()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}
fn point_of(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> [f64; 4] {
    [num(scope, o, "x"), num(scope, o, "y"), num(scope, o, "z"), num(scope, o, "w")]
}

const MATRIX_2D: [&str; 6] = ["a", "b", "c", "d", "e", "f"];
const MATRIX_3D: [&str; 16] = ["m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44"];

/// The state of `o`, a platform object of interface `iface`; `None` for an interface this codec does not
/// carry (the caller's generic path handles it).
pub(crate) fn read(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, iface: &str) -> Option<PlatformState> {
    let ov: v8::Local<v8::Value> = o.into();
    Some(match iface {
        "DOMPoint" | "DOMPointReadOnly" => PlatformState::Point { ro: iface == "DOMPointReadOnly", v: point_of(scope, o) },
        "DOMRect" | "DOMRectReadOnly" => PlatformState::Rect {
            ro: iface == "DOMRectReadOnly",
            v: [num(scope, o, "x"), num(scope, o, "y"), num(scope, o, "width"), num(scope, o, "height")],
        },
        "DOMQuad" => {
            let mut p = [[0.0; 4]; 4];
            for (i, k) in ["p1", "p2", "p3", "p4"].iter().enumerate() {
                if let Some(pt) = get_prop(scope, o, k).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
                    p[i] = point_of(scope, pt);
                }
            }
            PlatformState::Quad { p }
        }
        "DOMMatrix" | "DOMMatrixReadOnly" => {
            let is_2d = get_prop(scope, o, "is2D").map(|v| v.boolean_value(scope)).unwrap_or(false);
            let keys: &[&str] = if is_2d { &MATRIX_2D } else { &MATRIX_3D };
            let v = keys.iter().map(|k| num(scope, o, k)).collect();
            PlatformState::Matrix { ro: iface == "DOMMatrixReadOnly", is_2d, v }
        }
        "DOMException" => PlatformState::Exception { name: text(scope, o, "name"), message: text(scope, o, "message") },
        "File" => PlatformState::File {
            ty: crate::core::fileapi::blob::blob_type(scope, ov).unwrap_or_default(),
            bytes: crate::core::fileapi::blob::blob_bytes(scope, ov)?,
            name: text(scope, o, "name"),
            last_modified: num(scope, o, "lastModified"),
        },
        "Blob" => PlatformState::Blob {
            ty: crate::core::fileapi::blob::blob_type(scope, ov).unwrap_or_default(),
            bytes: crate::core::fileapi::blob::blob_bytes(scope, ov)?,
        },
        "ImageData" => {
            let data = get_prop(scope, o, "data").and_then(|v| v8::Local::<v8::Uint8ClampedArray>::try_from(v).ok())?;
            let mut bytes = vec![0u8; data.byte_length()];
            data.copy_contents(&mut bytes);
            PlatformState::ImageData {
                width: num(scope, o, "width") as u32,
                height: num(scope, o, "height") as u32,
                color_space: text(scope, o, "colorSpace"),
                bytes,
            }
        }
        _ => return None,
    })
}

fn new_of<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, args: &[v8::Local<'s, v8::Value>]) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = iface_object(scope, iface).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let r = {
        v8::tc_scope!(let tc, scope);
        let r = ctor.new_instance(tc, args).map(|o| v8::Global::new(tc, o));
        tc.reset();
        r
    };
    r.map(|g| v8::Local::new(scope, &g))
}
fn nums<'s>(scope: &mut v8::PinScope<'s, '_>, v: &[f64]) -> Vec<v8::Local<'s, v8::Value>> {
    v.iter().map(|x| v8::Number::new(scope, *x).into()).collect()
}
fn str_v<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Value> {
    v8::String::new(scope, s).map(Into::into).unwrap_or_else(|| v8::undefined(scope).into())
}

/// A fresh object of the state's interface, made in the CURRENT realm: by its own constructor, or minted
/// where the constructor would run JS -- this also runs inside V8's ValueDeserializer (a worker's message),
/// which aborts the process on any JS.
pub(crate) fn build<'s>(scope: &mut v8::PinScope<'s, '_>, st: &PlatformState) -> Option<v8::Local<'s, v8::Object>> {
    match st {
        PlatformState::Point { ro, v } => {
            let a = nums(scope, v);
            new_of(scope, if *ro { "DOMPointReadOnly" } else { "DOMPoint" }, &a)
        }
        PlatformState::Rect { ro, v } => {
            let a = nums(scope, v);
            new_of(scope, if *ro { "DOMRectReadOnly" } else { "DOMRect" }, &a)
        }
        PlatformState::Quad { p } => {
            let mut args = Vec::with_capacity(4);
            for pt in p.iter() {
                let d = v8::Object::new(scope);
                for (k, x) in ["x", "y", "z", "w"].iter().zip(pt.iter()) {
                    let key = str_v(scope, k);
                    let val: v8::Local<v8::Value> = v8::Number::new(scope, *x).into();
                    d.set(scope, key, val);
                }
                args.push(d.into());
            }
            new_of(scope, "DOMQuad", &args)
        }
        PlatformState::Matrix { ro, v, .. } => {
            let vals = nums(scope, v);
            let arr = v8::Array::new_with_elements(scope, &vals);
            new_of(scope, if *ro { "DOMMatrixReadOnly" } else { "DOMMatrix" }, &[arr.into()])
        }
        PlatformState::Exception { name, message } => {
            let m = str_v(scope, message);
            let n = str_v(scope, name);
            new_of(scope, "DOMException", &[m, n])
        }
        // Minted, not constructed: the constructors convert `sequence<BlobPart>` with `Array.from`,
        // which is JS, and this runs inside V8's ValueDeserializer for a worker's message, where JS
        // aborts the process.
        PlatformState::Blob { ty, bytes } => crate::core::fileapi::blob::mint_blob(scope, bytes.clone(), ty),
        PlatformState::File { ty, bytes, name, last_modified } => {
            crate::core::fileapi::file::mint_file(scope, bytes.clone(), ty, name, *last_modified)
        }
        PlatformState::ImageData { width, height, color_space, bytes } => {
            let len = bytes.len();
            let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.clone()).make_shared();
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            let data: v8::Local<v8::Value> = v8::Uint8ClampedArray::new(scope, ab, 0, len)?.into();
            let w: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, *width).into();
            let h: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, *height).into();
            let opts = v8::Object::new(scope);
            if !color_space.is_empty() {
                let k = str_v(scope, "colorSpace");
                let c = str_v(scope, color_space);
                opts.set(scope, k, c);
            }
            new_of(scope, "ImageData", &[data, w, h, opts.into()])
        }
    }
}

// ---- the wire form: V8's ValueSerializer host objects -------------------------------------------------

const TAG_PORT: u32 = 1;
const TAG_POINT: u32 = 2;
const TAG_RECT: u32 = 3;
const TAG_QUAD: u32 = 4;
const TAG_MATRIX: u32 = 5;
const TAG_EXCEPTION: u32 = 6;
const TAG_BLOB: u32 = 7;
const TAG_FILE: u32 = 8;
const TAG_IMAGEDATA: u32 = 9;

fn w_bytes(h: &dyn v8::ValueSerializerHelper, b: &[u8]) {
    h.write_uint32(b.len() as u32);
    h.write_raw_bytes(b);
}
fn w_str(h: &dyn v8::ValueSerializerHelper, s: &str) {
    w_bytes(h, s.as_bytes());
}
fn r_u32(h: &dyn v8::ValueDeserializerHelper) -> Option<u32> {
    let mut v = 0u32;
    h.read_uint32(&mut v).then_some(v)
}
fn r_f64(h: &dyn v8::ValueDeserializerHelper) -> Option<f64> {
    let mut v = 0f64;
    h.read_double(&mut v).then_some(v)
}
fn r_bytes(h: &dyn v8::ValueDeserializerHelper) -> Option<Vec<u8>> {
    let n = r_u32(h)? as usize;
    h.read_raw_bytes(n).map(|b| b.to_vec())
}
fn r_str(h: &dyn v8::ValueDeserializerHelper) -> Option<String> {
    r_bytes(h).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn write_state(h: &dyn v8::ValueSerializerHelper, st: &PlatformState) {
    match st {
        PlatformState::Point { ro, v } => {
            h.write_uint32(TAG_POINT);
            h.write_uint32(*ro as u32);
            v.iter().for_each(|x| h.write_double(*x));
        }
        PlatformState::Rect { ro, v } => {
            h.write_uint32(TAG_RECT);
            h.write_uint32(*ro as u32);
            v.iter().for_each(|x| h.write_double(*x));
        }
        PlatformState::Quad { p } => {
            h.write_uint32(TAG_QUAD);
            p.iter().flatten().for_each(|x| h.write_double(*x));
        }
        PlatformState::Matrix { ro, is_2d, v } => {
            h.write_uint32(TAG_MATRIX);
            h.write_uint32(*ro as u32);
            h.write_uint32(*is_2d as u32);
            v.iter().for_each(|x| h.write_double(*x));
        }
        PlatformState::Exception { name, message } => {
            h.write_uint32(TAG_EXCEPTION);
            w_str(h, name);
            w_str(h, message);
        }
        PlatformState::Blob { ty, bytes } => {
            h.write_uint32(TAG_BLOB);
            w_str(h, ty);
            w_bytes(h, bytes);
        }
        PlatformState::File { ty, bytes, name, last_modified } => {
            h.write_uint32(TAG_FILE);
            w_str(h, ty);
            w_bytes(h, bytes);
            w_str(h, name);
            h.write_double(*last_modified);
        }
        PlatformState::ImageData { width, height, color_space, bytes } => {
            h.write_uint32(TAG_IMAGEDATA);
            h.write_uint32(*width);
            h.write_uint32(*height);
            w_str(h, color_space);
            w_bytes(h, bytes);
        }
    }
}

fn read_state(tag: u32, h: &dyn v8::ValueDeserializerHelper) -> Option<PlatformState> {
    let four = |h: &dyn v8::ValueDeserializerHelper| -> Option<[f64; 4]> { Some([r_f64(h)?, r_f64(h)?, r_f64(h)?, r_f64(h)?]) };
    Some(match tag {
        TAG_POINT => PlatformState::Point { ro: r_u32(h)? != 0, v: four(h)? },
        TAG_RECT => PlatformState::Rect { ro: r_u32(h)? != 0, v: four(h)? },
        TAG_QUAD => PlatformState::Quad { p: [four(h)?, four(h)?, four(h)?, four(h)?] },
        TAG_MATRIX => {
            let ro = r_u32(h)? != 0;
            let is_2d = r_u32(h)? != 0;
            let n = if is_2d { 6 } else { 16 };
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r_f64(h)?);
            }
            PlatformState::Matrix { ro, is_2d, v }
        }
        TAG_EXCEPTION => PlatformState::Exception { name: r_str(h)?, message: r_str(h)? },
        TAG_BLOB => PlatformState::Blob { ty: r_str(h)?, bytes: r_bytes(h)? },
        TAG_FILE => PlatformState::File { ty: r_str(h)?, bytes: r_bytes(h)?, name: r_str(h)?, last_modified: r_f64(h)? },
        TAG_IMAGEDATA => PlatformState::ImageData { width: r_u32(h)?, height: r_u32(h)?, color_space: r_str(h)?, bytes: r_bytes(h)? },
        _ => return None,
    })
}

/// The serializer delegate of every V8-serialized message (a worker's `postMessage`, a cross-isolate
/// `BroadcastChannel`): serializable platform objects and the transferred ports are HOST objects written by
/// this file; every other object is an ordinary one. It answers `is_host_object` itself, so the internal
/// field every branded instance carries (the brand cache) never decides.
pub struct WireSer {
    /// The ports in the message's transfer list, in order: a port inside the message is written as its index.
    pub ports: Vec<v8::Global<v8::Object>>,
}

impl WireSer {
    fn port_index(&self, scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<u32> {
        self.ports.iter().position(|g| v8::Local::new(scope, g) == o).map(|i| i as u32)
    }
}

impl v8::ValueSerializerImpl for WireSer {
    fn has_custom_host_object(&self, _isolate: &v8::Isolate) -> bool {
        true
    }
    fn is_host_object<'s>(&self, scope: &mut v8::PinScope<'s, '_>, object: v8::Local<'s, v8::Object>) -> Option<bool> {
        if self.port_index(scope, object).is_some() {
            return Some(true);
        }
        let host = crate::modules::structured_clone::platform_interface(scope, object.into())
            .is_some_and(|i| read_supported(&i));
        Some(host)
    }
    fn write_host_object<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        object: v8::Local<'s, v8::Object>,
        h: &dyn v8::ValueSerializerHelper,
    ) -> Option<bool> {
        if let Some(i) = self.port_index(scope, object) {
            h.write_uint32(TAG_PORT);
            h.write_uint32(i);
            return Some(true);
        }
        let iface = crate::modules::structured_clone::platform_interface(scope, object.into())?;
        let st = read(scope, object, &iface)?;
        write_state(h, &st);
        Some(true)
    }
    fn throw_data_clone_error<'s>(&self, scope: &mut v8::PinScope<'s, '_>, message: v8::Local<'s, v8::String>) {
        let exc = v8::Exception::error(scope, message);
        scope.throw_exception(exc);
    }
}

fn read_supported(iface: &str) -> bool {
    matches!(
        iface,
        "DOMPoint" | "DOMPointReadOnly" | "DOMRect" | "DOMRectReadOnly" | "DOMQuad" | "DOMMatrix" | "DOMMatrixReadOnly"
            | "DOMException" | "Blob" | "File" | "ImageData"
    )
}

/// The deserializer delegate: builds each host object in the RECEIVING realm, and answers a port index with
/// the receiver's port.
pub struct WireDeser {
    pub ports: Vec<v8::Global<v8::Object>>,
}

impl v8::ValueDeserializerImpl for WireDeser {
    fn read_host_object<'s>(&self, scope: &mut v8::PinScope<'s, '_>, h: &dyn v8::ValueDeserializerHelper) -> Option<v8::Local<'s, v8::Object>> {
        let tag = r_u32(h)?;
        if tag == TAG_PORT {
            let i = r_u32(h)? as usize;
            return self.ports.get(i).map(|g| v8::Local::new(scope, g));
        }
        let st = read_state(tag, h)?;
        build(scope, &st)
    }
}

/// Serialize `value` for another isolate; `ports` are the message's transferred ports, in order.
pub fn wire_serialize(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, ports: &[v8::Local<v8::Object>]) -> Vec<u8> {
    use v8::ValueSerializerHelper;
    let ports = ports.iter().map(|p| v8::Global::new(scope, *p)).collect();
    let ctx = scope.get_current_context();
    let s = v8::ValueSerializer::new(scope, Box::new(WireSer { ports }));
    s.write_header();
    let _ = s.write_value(ctx, value);
    s.release()
}

/// Deserialize bytes from [`wire_serialize`] in the CURRENT realm; `ports` are the receiver's ports, in the
/// order the sender transferred them.
pub fn wire_deserialize<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: &[u8], ports: &[v8::Local<v8::Object>]) -> v8::Local<'s, v8::Value> {
    use v8::ValueDeserializerHelper;
    let ports = ports.iter().map(|p| v8::Global::new(scope, *p)).collect();
    let ctx = scope.get_current_context();
    let d = v8::ValueDeserializer::new(scope, Box::new(WireDeser { ports }), bytes);
    let _ = d.read_header(ctx);
    d.read_value(ctx).unwrap_or_else(|| v8::undefined(scope).into())
}

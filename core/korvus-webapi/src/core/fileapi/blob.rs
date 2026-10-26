//! `core/fileapi/blob.rs` — `Blob`.
//!
//! Was a JS `class Blob` in `core/korvus-runtime/src/js/host.js` with a constructor-only body, whose
//! state lived in two OWN properties (`_bytes`, `_type`) that `Object.getOwnPropertyNames` showed to
//! any page that looked — Chrome answers `[]` there. The constructor is native now, the state is in
//! v8 privates, and every row is behind the interface's brand.
//!
//! WHAT THE JS CONSTRUCTOR GOT WRONG, measured with `tests/fixtures/fileapi-probe.js` on
//! `google-chrome --headless=new` 150.0.7871.46:
//!
//! * `Object.getOwnPropertyNames(new Blob(["a"]))` was `["_bytes","_type"]`; Chrome's is `[]`.
//! * `sequence<BlobPart>` was `for (const p of (parts || []))`, so `new Blob(null)` was an empty blob,
//!   `new Blob("abc").size` was 3 and `new Blob(1)` was an empty blob. WebIDL converts a sequence only
//!   from an OBJECT with a callable `@@iterator`, so Chrome throws "The provided value cannot be
//!   converted to a sequence." for all three — a String PRIMITIVE is iterable but is not an Object.
//! * `BlobPropertyBag` was read off whatever was passed, so `new Blob([], 5)` was accepted; Chrome
//!   throws "The provided value is not of type 'BlobPropertyBag'." And `endings` is a WebIDL enum:
//!   `{endings: "nope"}` is a TypeError naming `EndingType`, not a silent fall-back to transparent.
//! * `Blob.prototype.type` answered `""` for a foreign receiver; Chrome throws `Illegal invocation`.
//!
//! `type` is NOT a parsed MIME type, and that has not changed: <https://w3c.github.io/FileAPI/#dfn-type>
//! says only that a string containing a character outside U+0020..U+007E gives the empty string and
//! anything else is the string ASCII-lowercased. Nothing is parsed, reformatted or dropped — recorded
//! from Chrome and re-confirmed by the probe: `"text/plain; charset=UTF-8"` -> `"text/plain;
//! charset=utf-8"`, `"bogus"` -> `"bogus"`, `"  text/plain  "` -> `"  text/plain  "`.
//!
//! `textStream` is Chrome 152's (not in Chrome 150): the capture records it and the bundle is the denominator.

use deno_core::v8;

use crate::binding::protos::{
    build_fn, call_method, get_prop, int_val, new_global, resolved_promise, set_prop, str_val,
    throw_type_error,
};
use crate::install::webidl::{
    brand, branded_getter_row, branded_method_row, branded_promise_method_row, dict_enum, dict_string, init_dict,
    require_new,
};
use crate::install::MemberDef;

// ── the two internal slots ──────────────────────────────────────────────────────────────────────
//
// A `Blob`'s bytes and type are internal slots in every browser, so they are v8 Privates here: the
// instance owns nothing, `JSON.stringify` sees nothing, and page JS can neither read the slots nor
// forge them onto a plain object (which is what makes the brand check below meaningful).

pub(crate) const BYTES_SLOT: &str = "korvus_fileapi_bytes";
pub(crate) const TYPE_SLOT: &str = "korvus_fileapi_type";

pub(crate) fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

pub(crate) fn slot_set(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let key = slot_key(scope, name);
    this.set_private(scope, key, v);
}

pub(crate) fn slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = slot_key(scope, name);
    this.get_private(scope, key).filter(|v| !v.is_undefined())
}

/// The `Uint8Array` behind a blob, as a JS value (the bodies below hand it to `Uint8Array`,
/// `TextDecoder` and `ArrayBuffer.prototype.slice`).
fn bytes_value<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    slot_get(scope, this, BYTES_SLOT)
}

/// The RAW bytes behind an `ArrayBuffer` or any `ArrayBufferView` — the same read
/// `modules/crypto/shared.rs` does, kept local so `fileapi` does not depend on `modules`.
fn view_bytes(v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if v.is_array_buffer() {
        let ab = v8::Local::<v8::ArrayBuffer>::try_from(v).ok()?;
        let store = ab.get_backing_store();
        return Some((0..store.byte_length()).map(|i| store[i].get()).collect());
    }
    if v.is_array_buffer_view() {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        return Some(buf);
    }
    None
}

/// A JS `Uint8Array` owning `bytes`.
fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match v8::Uint8Array::new(scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(scope).into(),
    }
}

/// The RAW BYTES of a `Blob` (or `File`), for the engine's other readers — `FileReader`, the
/// blob-URL registry. `None` when `v` is not a blob this realm built.
///
/// THE BRAND IS THE TEST, and that is the point of having one entry point: these callers used to read
/// `obj._bytes` off whatever they were handed, so any object with a `_bytes` property that happened to
/// be a typed array passed for a Blob. `Object.create(Blob.prototype)` still does not.
pub fn blob_bytes(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if !crate::install::webidl::has_brand(scope, v, "Blob") {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    view_bytes(bytes_value(scope, o)?)
}

/// The normalized `type` of a `Blob` (or `File`), for the same readers.
pub fn blob_type(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    if !crate::install::webidl::has_brand(scope, v, "Blob") {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    Some(slot_get(scope, o, TYPE_SLOT)?.to_rust_string_lossy(scope))
}

/// Write a freshly built blob's two slots and stamp the `Blob` brand. The ONE entry point, so a File
/// (which runs it through `super`'s equivalent) and a `slice()` result carry the same state.
pub(crate) fn init_blob(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, bytes: Vec<u8>, ty: &str) {
    let b = u8_array(scope, bytes);
    slot_set(scope, this, BYTES_SLOT, b);
    let t = str_val(scope, ty);
    slot_set(scope, this, TYPE_SLOT, t);
    brand(scope, this, "Blob");
}

/// Mint a `Blob` from Rust, on THIS realm's `Blob.prototype` — what `slice()` returns. Not
/// `new_global("Blob", …)`: that would re-run the `sequence<BlobPart>` conversion on bytes this code
/// already has, and would follow a `globalThis.Blob` a page replaced.
pub(crate) fn mint_blob<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>, ty: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "Blob")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    init_blob(scope, obj, bytes, ty);
    Some(obj)
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

/// FileAPI's `type` normalisation, which is not mimesniff's: a character outside U+0020..U+007E makes
/// the whole type the empty string; otherwise the whole string is ASCII-lowercased. Nothing is parsed.
pub(crate) fn normalize_mime(input: &str) -> String {
    if input.chars().any(|c| (c as u32) < 0x20 || (c as u32) > 0x7e) {
        return String::new();
    }
    input.to_ascii_lowercase()
}

/// `endings: "native"` normalises CRLF and a lone CR to LF — in STRING parts only.
fn native_endings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// The WebIDL `sequence<BlobPart>` conversion, flattened to bytes.
///
/// `Err(())` means the exception is already thrown. WebIDL §es-sequence: the value must be an
/// **Object** with a callable `@@iterator` — which is why a bare string (a primitive), `null` and a
/// number are all "The provided value cannot be converted to a sequence.", exactly as Chrome answers.
/// The iteration itself is `Array.from`, so a generator and a `Set` work the way they do in a browser.
pub(crate) enum BlobPart {
    Bytes(Vec<u8>),
    Text(String),
}

/// `sequence<BlobPart>`, converted as WebIDL converts the argument -- before the dictionary that follows it is read:
/// each part a Blob (its bytes), a buffer source (its bytes) or a USVString (ToString, a
/// Symbol refused). The text parts stay text until [`parts_bytes`] knows the `endings`.
pub(crate) fn blob_parts(scope: &mut v8::PinScope, parts: v8::Local<v8::Value>, iface: &str) -> Result<Vec<BlobPart>, ()> {
    if parts.is_undefined() {
        return Ok(Vec::new());
    }
    let not_a_sequence = |scope: &mut v8::PinScope| {
        throw_type_error(scope, &format!("Failed to construct '{iface}': The provided value cannot be converted to a sequence."));
    };
    let Ok(obj) = v8::Local::<v8::Object>::try_from(parts) else {
        not_a_sequence(scope);
        return Err(());
    };
    let it_key = v8::Symbol::get_iterator(scope);
    let iterable = obj.get(scope, it_key.into()).map(|v| v.is_function()).unwrap_or(false);
    if !iterable {
        not_a_sequence(scope);
        return Err(());
    }
    let Some(list) = crate::binding::protos::call_intrinsic(scope, "Array.from", &[parts])
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
    else {
        return Err(());
    };
    let mut out = Vec::new();
    for i in 0..list.length() {
        let Some(p) = list.get_index(scope, i) else { continue };
        // A Blob (or File) part contributes its own bytes. The BRAND is the test, not a prototype
        // walk: `Object.create(Blob.prototype)` is not a Blob in any browser.
        if crate::install::webidl::has_brand(scope, p, "Blob") {
            if let Some(b) = v8::Local::<v8::Object>::try_from(p).ok().and_then(|o| bytes_value(scope, o)) {
                out.push(BlobPart::Bytes(view_bytes(b).unwrap_or_default()));
                continue;
            }
        }
        if let Some(b) = view_bytes(p) {
            out.push(BlobPart::Bytes(b));
            continue;
        }
        // Everything else is `USVString`: ToString (a Symbol throws), then UTF-8.
        if p.is_symbol() {
            throw_type_error(scope, &format!("Failed to construct '{iface}': Cannot convert a Symbol value to a string"));
            return Err(());
        }
        let Some(s) = p.to_string(scope) else { return Err(()) };
        out.push(BlobPart::Text(s.to_rust_string_lossy(scope)));
    }
    Ok(out)
}

/// The blob's bytes from its converted parts, the text parts' line endings made native when `endings` asks.
pub(crate) fn parts_bytes(parts: Vec<BlobPart>, native: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        match p {
            BlobPart::Bytes(b) => out.extend(b),
            BlobPart::Text(s) => out.extend(if native { native_endings(&s) } else { s }.as_bytes()),
        }
    }
    out
}

/// The `BlobPropertyBag` half of the constructor: `(type, endings == "native")`. Shared with `File`,
/// whose `FilePropertyBag` inherits both members.
pub(crate) fn blob_property_bag<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    idx: i32,
    iface: &str,
    dict_name: &str,
) -> Result<(String, bool, Option<v8::Local<'s, v8::Object>>), ()> {
    let opts = init_dict(scope, args, idx, iface, dict_name)?;
    let ty = dict_string(scope, opts, "type")?.map(|s| normalize_mime(&s)).unwrap_or_default();
    let endings = dict_enum(scope, opts, "endings", iface, dict_name, "EndingType", &["transparent", "native"])?;
    Ok((ty, endings.as_deref() == Some("native"), opts))
}

/// `new Blob(optional sequence<BlobPart> blobParts, optional BlobPropertyBag options)` — both
/// optional, so `length` is 0.
pub(crate) fn cb_blob_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "Blob") {
        return;
    }
    // The arguments in WebIDL's order (Chrome 150): the parts first -- a part's `toString` runs before any getter of
    // the dictionary -- then the dictionary, whose `endings` then decides how the text parts are encoded. korvus read
    // the dictionary first.
    let Ok(parts) = blob_parts(scope, args.get(0), "Blob") else { return };
    let Ok((ty, native, _)) = blob_property_bag(scope, &args, 1, "Blob", "BlobPropertyBag") else { return };
    let bytes = parts_bytes(parts, native);
    let this = args.this();
    init_blob(scope, this, bytes, &ty);
    rv.set(this.into());
}

// ── the rows ────────────────────────────────────────────────────────────────────────────────────

/// Blob.size: the flattened byte length (WebIDL readonly attribute -> prototype accessor).
fn cb_blob_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let len = bytes_value(scope, args.this())
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|b| get_prop(scope, b, "length"))
        .and_then(|v| v.integer_value(scope))
        .unwrap_or(0);
    rv.set(int_val(scope, len));
}

/// Blob.type: the normalized type the constructor stored ("" when absent).
fn cb_blob_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = slot_get(scope, args.this(), TYPE_SLOT).unwrap_or_else(|| str_val(scope, ""));
    rv.set(t);
}

fn cb_blob_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bytes = match bytes_value(scope, args.this()) { Some(b) => b, None => return };
    let decoder = match new_global(scope, "TextDecoder", &[]) { Some(d) => d, None => return };
    let s = call_method(scope, decoder.into(), "decode", &[bytes]).unwrap_or_else(|| str_val(scope, ""));
    let p = resolved_promise(scope, s); rv.set(p);
}

fn cb_blob_bytes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bytes = match bytes_value(scope, args.this()) { Some(b) => b, None => return };
    let u8 = match new_global(scope, "Uint8Array", &[bytes]) { Some(u) => u, None => return };
    let p = resolved_promise(scope, u8.into()); rv.set(p);
}

fn cb_blob_array_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bytes = match bytes_value(scope, args.this()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) { Some(b) => b, None => return };
    let buf = match get_prop(scope, bytes, "buffer") { Some(b) => b, None => return };
    let bo = get_prop(scope, bytes, "byteOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let bl = get_prop(scope, bytes, "byteLength").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let a = int_val(scope, bo); let b = int_val(scope, bo + bl);
    let ab = match call_method(scope, buf, "slice", &[a, b]) { Some(x) => x, None => return };
    let p = resolved_promise(scope, ab); rv.set(p);
}

/// `slice(optional long long start, optional long long end, optional DOMString contentType)` with the
/// FileAPI clamping. The result is a `Blob` even when the receiver is a `File`, which is what Chrome
/// does; it is minted on this realm's `Blob.prototype` rather than through `globalThis.Blob`.
fn cb_blob_slice(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let all = match bytes_value(scope, args.this()).and_then(view_bytes) { Some(b) => b, None => return };
    let len = all.len() as i64;
    let start_arg = args.get(0);
    let s = if start_arg.is_undefined() { 0 } else { let v = start_arg.integer_value(scope).unwrap_or(0); if v < 0 { (len + v).max(0) } else { v.min(len) } };
    let end_arg = args.get(1);
    let mut e = if end_arg.is_undefined() { len } else { let v = end_arg.integer_value(scope).unwrap_or(0); if v < 0 { (len + v).max(0) } else { v.min(len) } };
    if e < s { e = s; }
    let ct = args.get(2);
    let ty = if ct.is_undefined() {
        String::new()
    } else {
        match ct.to_string(scope) {
            Some(v) => normalize_mime(&v.to_rust_string_lossy(scope)),
            None => return,
        }
    };
    let part = all[s as usize..e as usize].to_vec();
    if let Some(blob) = mint_blob(scope, part, &ty) {
        rv.set(blob.into());
    }
}

fn cb_blob_stream_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let controller = args.get(0);
    let bytes = match get_prop(scope, this, "__bytes") { Some(b) => b, None => return };
    if let Some(u8) = new_global(scope, "Uint8Array", &[bytes]) { call_method(scope, controller, "enqueue", &[u8.into()]); }
    call_method(scope, controller, "close", &[]);
}
fn cb_blob_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bytes = match bytes_value(scope, args.this()) { Some(b) => b, None => return };
    let source = v8::Object::new(scope);
    set_prop(scope, source, "__bytes", bytes);
    let start_fn = build_fn(scope, cb_blob_stream_start, "start", 1);
    set_prop(scope, source, "start", start_fn.into());
    if let Some(rs) = new_global(scope, "ReadableStream", &[source.into()]) { rv.set(rs.into()); }
}

// Blob.textStream(): Chromium 152's `Blob::textStream` -- `stream()` piped through a UTF-8 `TextDecoderStream`
// (not fatal, BOM not ignored), its readable side the answer: the decoded text in the chunks the decoder writes
// (none for an empty blob).
fn cb_blob_text_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bytes = match bytes_value(scope, args.this()) { Some(b) => b, None => return };
    let source = v8::Object::new(scope);
    set_prop(scope, source, "__bytes", bytes);
    let start_fn = build_fn(scope, cb_blob_stream_start, "start", 1);
    set_prop(scope, source, "start", start_fn.into());
    let Some(rs) = new_global(scope, "ReadableStream", &[source.into()]) else { return };
    let Some(decoder) = new_global(scope, "TextDecoderStream", &[]) else { return };
    let (Some(writable), Some(readable)) = (get_prop(scope, decoder, "writable"), get_prop(scope, decoder, "readable")) else { return };
    if let Some(out) = crate::core::streams::readable_stream::pipe_through_pair(scope, rs, writable, readable) {
        rv.set(out);
    }
}

/// The body every row of [`BLOB`] names. Every one is mounted BEHIND THE BRAND, so a foreign receiver
/// throws `Illegal invocation` the way Chrome does instead of answering `0` / `""` / a pending
/// promise over nothing.
pub fn install_blob(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => branded_getter_row(scope, proto, member, cb_blob_size),
        "type" => branded_getter_row(scope, proto, member, cb_blob_type),
        // The three PROMISE-returning rows reject rather than throw for a foreign receiver, which is
        // what a promise-returning WebIDL operation does; `slice`, `stream` and `textStream` return a
        // value and throw. Both measured — see `branded_promise_method_row`.
        "arrayBuffer" => branded_promise_method_row(scope, proto, member, cb_blob_array_buffer),
        "slice" => branded_method_row(scope, proto, member, cb_blob_slice),
        "stream" => branded_method_row(scope, proto, member, cb_blob_stream),
        "text" => branded_promise_method_row(scope, proto, member, cb_blob_text),
        "bytes" => branded_promise_method_row(scope, proto, member, cb_blob_bytes),
        "textStream" => branded_method_row(scope, proto, member, cb_blob_text_stream),
        other => debug_assert!(false, "install_blob: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `Blob` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: the two readonly attributes then the six
// methods. `[value, both]`: both captures record `newable: true` and `length: 0`.
webapi::interface! {
    "Blob" [value, both] {
        ctor   "Blob"                     len 0 = stub,
        attr   "size"        : Object readonly  = install_blob,
        attr   "type"        : Object readonly  = install_blob,
        method "arrayBuffer" : Promise len 0    = install_blob,
        method "slice"       : Object len 0     = install_blob,
        method "stream"      : Object len 0     = install_blob,
        method "text"        : Promise len 0    = install_blob,
        method "bytes"       : Promise len 0    = install_blob,
        method "textStream"  : Object len 0     = install_blob
    }
}

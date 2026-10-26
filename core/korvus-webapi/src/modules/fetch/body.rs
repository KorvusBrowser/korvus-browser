//! `modules/fetch/body.rs` — the Fetch `Body` mixin, shared by `Response` and `Request`.
//!
//! A body is a `ReadableStream` of BYTES (or null), held in the object's `body` slot, and every
//! consumer reads it through that stream. It used to be the argument's STRING form in a private slot:
//! `new Response(new Uint8Array([1, 2, 255])).arrayBuffer()` answered the seven bytes of `"1,2,255"`,
//! `.body` was always `null`, `bodyUsed` was never `true`, a body could be read twice, and a `Response`
//! built from a `ReadableStream` answered `"[object ReadableStream]"` as its text.
//!
//! MEASURED on `google-chrome` 150.0.7871.46, every line of which this file answers:
//!
//! ```text
//! new Response("hi").body                              [object ReadableStream], the same object on every read
//! read it: chunk, done                                 [object Uint8Array] 104.105, then done; bodyUsed true
//! getReader() alone                                    bodyUsed false
//! text() then bodyUsed, body.locked                    true, true
//! new Response().text() then bodyUsed                  false   (a null body is never "used")
//! text() twice                                         TypeError: Failed to execute 'text' on 'Response': body stream already read
//! getReader() then text()                              TypeError: Failed to execute 'text' on 'Response': body stream is locked
//! clone() then both text()                             hi|hi, both bodyUsed; the original's body is a NEW stream
//! clone() after text() / while locked                  TypeError: Failed to execute 'clone' on 'Response': Response body is already used
//! new Response(rs).body === rs; chunks arriving later  true; "ok"
//! a non-Uint8Array chunk                               TypeError: Failed to fetch
//! Uint8Array / ArrayBuffer / Blob / URLSearchParams    byte-exact; Content-Type none / none / the blob's type / application/x-www-form-urlencoded;charset=UTF-8
//! a string                                             Content-Type text/plain;charset=UTF-8
//! text() of EF BB BF 41                                "A"   (UTF-8 decode, BOM removed)
//! blob().type for Content-Type "Text/HTML; Charset=UTF-8"   text/html; charset=utf-8
//! json() of "{"                                        SyntaxError: Failed to execute 'json' on 'Response': Expected property name or '}' in JSON at position 1 (line 1 column 2)
//! ```

use deno_core::v8;

use crate::binding::protos::{build_fn_data, call_method, get_prop, rejected_promise, rejected_type_error, resolved_promise, str_val, throw_type_error};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::core::streams::readable_stream::{as_stream, new_closed_stream_with, read_from};

/// What a `BodyInit` extracts to.
pub(crate) enum Extracted<'s> {
    /// No body.
    Null,
    /// A `ReadableStream` the page handed in (kept as the body itself).
    Stream(v8::Local<'s, v8::Object>),
    /// Bytes, with the Content-Type the body's type implies (`None` for a buffer).
    Bytes(Vec<u8>, Option<String>),
}

/// The bytes of an `ArrayBuffer` or view -- the one reader every body path uses.
pub(crate) fn buffer_bytes(v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    crate::core::streams::text_decoder_stream::buffer_source_bytes(v)
}

/// A `Uint8Array` owning `bytes`.
pub(crate) fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match v8::Uint8Array::new(scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(scope).into(),
    }
}

/// Fetch's "extract a body" for everything this engine implements. `Err(())` means an exception is
/// pending (a `toString` threw).
pub(crate) fn extract<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Result<Extracted<'s>, ()> {
    let v: v8::Local<'s, v8::Value> = v8::Local::new(scope, v);
    if v.is_null_or_undefined() {
        return Ok(Extracted::Null);
    }
    if as_stream(scope, v).is_some() {
        return Ok(Extracted::Stream(v8::Local::<v8::Object>::try_from(v).map_err(|_| ())?));
    }
    if let Some(bytes) = crate::core::fileapi::blob::blob_bytes(scope, v) {
        let ty = crate::core::fileapi::blob::blob_type(scope, v).filter(|t| !t.is_empty());
        return Ok(Extracted::Bytes(bytes, ty));
    }
    if let Some(bytes) = buffer_bytes(v) {
        return Ok(Extracted::Bytes(bytes, None));
    }
    // A `FormData` is a `BodyInit`, and it fell through to the USVString arm below -- so
    // `new Response(formData)` was the eleven bytes `[object FormData]` with Content-Type
    // `text/plain;charset=UTF-8`, where Chrome 150 answers a multipart body and
    // `multipart/form-data; boundary=----WebKitFormBoundary…`. The encoding is HTML's
    // "multipart/form-data encoding algorithm", which `<form enctype=multipart/form-data>` already
    // runs: `form_encoding` is that ONE implementation, called from here rather than copied.
    // Korvus's `FormData` holds `(name, value)` strings only, so no entry is a file.
    if let Some(pairs) = super::form_data::fd_pairs(scope, v) {
        let boundary = crate::core::html::form_encoding::multipart_boundary();
        let files = vec![false; pairs.len()];
        let encoded = crate::core::html::form_encoding::multipart_encode(&pairs, &files, &boundary);
        return Ok(Extracted::Bytes(encoded.into_bytes(), Some(format!("multipart/form-data; boundary={boundary}"))));
    }
    if crate::install::webidl::is_instance_of(scope, v, "URLSearchParams") {
        let s = v.to_string(scope).ok_or(())?.to_rust_string_lossy(scope);
        return Ok(Extracted::Bytes(s.into_bytes(), Some("application/x-www-form-urlencoded;charset=UTF-8".into())));
    }
    // A USVString (anything else is converted, as WebIDL's union conversion does).
    let s = v.to_string(scope).ok_or(())?.to_rust_string_lossy(scope);
    Ok(Extracted::Bytes(s.into_bytes(), Some("text/plain;charset=UTF-8".into())))
}

/// The body STREAM for an extracted body: the page's own stream, a closed stream over the bytes, or
/// null.
pub(crate) fn body_stream<'s>(scope: &mut v8::PinScope<'s, '_>, e: &Extracted<'s>) -> v8::Local<'s, v8::Value> {
    match e {
        Extracted::Null => v8::null(scope).into(),
        Extracted::Stream(s) => (*s).into(),
        Extracted::Bytes(b, _) => {
            let chunk = if b.is_empty() { None } else { Some(u8_array(scope, b.clone())) };
            new_closed_stream_with(scope, chunk).into()
        }
    }
}

/// A body stream over bytes that are already a `Uint8Array` (the network's), closed.
pub(crate) fn stream_over_u8<'s>(scope: &mut v8::PinScope<'s, '_>, u8: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Object> {
    let empty = v8::Local::<v8::ArrayBufferView>::try_from(u8).map(|v| v.byte_length() == 0).unwrap_or(true);
    new_closed_stream_with(scope, if empty { None } else { Some(u8) })
}

/// The private slot on a network body stream that names its body id while it is still arriving.
pub(crate) const BODY_ID: &str = "korvus_fetch_body_id";

/// A `fetch()` body still ARRIVING: an open stream that the network's bytes are enqueued
/// into when they land ([`network_body_arrived`]), and whose cancel stops the read. Chrome hands the
/// page its Response at the headers, so the body is a stream the page waits on, not bytes in hand.
pub(crate) fn pending_network_stream<'s>(scope: &mut v8::PinScope<'s, '_>, body_id: u32) -> v8::Local<'s, v8::Object> {
    let stream = crate::core::streams::readable_stream::new_stream(scope);
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, body_id as f64).into();
    let source = v8::Object::new(scope);
    let cancel = build_fn_data(scope, cb_network_body_cancel, "cancel", 1, idv);
    crate::binding::protos::set_prop(scope, source, "cancel", cancel.into());
    crate::core::streams::readable_stream::set_underlying_source(scope, stream, source);
    crate::core::dom::element::priv_set(scope, stream, BODY_ID, idv);
    stream
}

/// Stop the network read behind body `id`.
pub(crate) fn cancel_network_body(scope: &mut v8::PinScope, id: v8::Local<v8::Value>) {
    if let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_fetch_body_cancel") {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        f.call(scope, undef, &[id]);
    }
}

fn cb_network_body_cancel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = crate::binding::protos::body_data(scope, &args);
    cancel_network_body(scope, id);
}

/// The whole body of a pending network stream arrived: enqueue it and close. A stream the page
/// cancelled or an abort errored meanwhile takes nothing.
pub(crate) fn network_body_arrived(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, bytes: Option<v8::Local<v8::Value>>) {
    let Some(rs) = as_stream(scope, stream) else { return };
    if rs.is_errored() || rs.is_closed() {
        return;
    }
    if let Some(b) = bytes.filter(|b| v8::Local::<v8::ArrayBufferView>::try_from(*b).is_ok_and(|v| v.byte_length() > 0)) {
        let _ = crate::core::streams::readable_stream::rs_enqueue(scope, stream, b);
    }
    let _ = crate::core::streams::readable_stream::rs_close(scope, stream);
    if let Ok(o) = v8::Local::<v8::Object>::try_from(stream) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        crate::core::dom::element::priv_set(scope, o, BODY_ID, undef);
    }
}

/// One chunk of a pending network body landed: into the stream. `false` when the stream
/// takes no more -- the page cancelled it, or an abort errored it -- so the read should stop.
pub(crate) fn network_chunk_arrived(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, chunk: v8::Local<v8::Value>) -> bool {
    let Some(rs) = as_stream(scope, stream) else { return true };
    if rs.is_errored() || rs.is_closed() {
        return false;
    }
    if v8::Local::<v8::ArrayBufferView>::try_from(chunk).is_ok_and(|v| v.byte_length() > 0) {
        return crate::core::streams::readable_stream::rs_enqueue(scope, stream, chunk).is_ok();
    }
    true
}

/// Error a pending network stream with `reason` and stop its read -- a failed connection, or an abort
/// after the headers. One that already finished is left alone.
pub(crate) fn network_body_failed(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, reason: v8::Local<v8::Value>) {
    let Some(rs) = as_stream(scope, stream) else { return };
    if rs.is_errored() || rs.is_closed() {
        return;
    }
    crate::core::streams::readable_stream::rs_error(scope, stream, reason);
    if let Some(id) = v8::Local::<v8::Object>::try_from(stream).ok().and_then(|o| crate::core::css::css_style_declaration::priv_get(scope, o, BODY_ID)) {
        cancel_network_body(scope, id);
    }
}

/// `bodyUsed`: the body stream is disturbed. A null body is never used.
pub(crate) fn body_used(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    match evt_slot_read(scope, this, "body") {
        Some(b) if !b.is_null() => as_stream(scope, b).map(|rs| rs.is_disturbed()).unwrap_or(false),
        _ => false,
    }
}

/// Is the body unusable for a clone or a re-wrap (disturbed or locked)?
pub(crate) fn body_unusable(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    match evt_slot_read(scope, this, "body") {
        Some(b) if !b.is_null() => as_stream(scope, b).map(|rs| rs.is_disturbed() || rs.is_locked()).unwrap_or(false),
        _ => false,
    }
}

/// `clone()`'s body step: tee this body, keep one branch, return the other (or null).
pub(crate) fn tee_body<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> v8::Local<'s, v8::Value> {
    let Some(b) = evt_slot_read(scope, this, "body").filter(|b| !b.is_null()) else {
        return v8::null(scope).into();
    };
    let Some(rs) = as_stream(scope, b) else { return v8::null(scope).into() };
    let (a, c) = crate::core::streams::readable_stream::tee_stream(scope, rs);
    evt_slot_write(scope, this, "body", a.into());
    c.into()
}

/// The `bodyUsed` getter both tables mount.
pub(crate) fn cb_body_used(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let used = body_used(scope, args.this());
    rv.set_bool(used);
}

/// The five consumers.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    ArrayBuffer = 0,
    Blob = 1,
    Bytes = 2,
    Json = 3,
    Text = 4,
}

impl Kind {
    fn from_num(n: u32) -> Kind {
        match n {
            0 => Kind::ArrayBuffer,
            1 => Kind::Blob,
            2 => Kind::Bytes,
            3 => Kind::Json,
            _ => Kind::Text,
        }
    }
    fn method(self) -> &'static str {
        match self {
            Kind::ArrayBuffer => "arrayBuffer",
            Kind::Blob => "blob",
            Kind::Bytes => "bytes",
            Kind::Json => "json",
            Kind::Text => "text",
        }
    }
}

/// The `Content-Type` a `blob()` result is typed with: the object's own header, ASCII-lowercased.
/// A NETWORK response is typed by the network stack's MIME type instead (`response.rs`, `netMimeType`; A/B C5).
fn mime_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    if let Some(m) = evt_slot_read(scope, this, "netMimeType").filter(|v| v.is_string()) {
        return m.to_rust_string_lossy(scope);
    }
    evt_slot_read(scope, this, "headers")
        .and_then(|h| {
            let k = str_val(scope, "content-type");
            call_method(scope, h, "get", &[k])
        })
        .filter(|v| !v.is_null_or_undefined())
        .map(|v| v.to_rust_string_lossy(scope).to_ascii_lowercase())
        .unwrap_or_default()
}

/// Turn the collected bytes into the consumer's result, or `Err(reason)`.
fn package<'s>(scope: &mut v8::PinScope<'s, '_>, kind: Kind, iface: &str, mime: &str, bytes: Vec<u8>) -> Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>> {
    match kind {
        Kind::ArrayBuffer => {
            let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
            Ok(v8::ArrayBuffer::with_backing_store(scope, &store).into())
        }
        Kind::Bytes => Ok(u8_array(scope, bytes)),
        Kind::Blob => match crate::core::fileapi::blob::mint_blob(scope, bytes, mime) {
            Some(b) => Ok(b.into()),
            None => Ok(v8::undefined(scope).into()),
        },
        Kind::Text => {
            let (s, _) = encoding_rs::UTF_8.decode_with_bom_removal(&bytes);
            Ok(str_val(scope, &s))
        }
        Kind::Json => {
            let (s, _) = encoding_rs::UTF_8.decode_with_bom_removal(&bytes);
            let Some(src) = v8::String::new(scope, &s) else { return Ok(v8::undefined(scope).into()) };
            let parsed: Result<v8::Global<v8::Value>, String> = {
                v8::tc_scope!(let tc, scope);
                match v8::json::parse(tc, src) {
                    Some(v) => Ok(v8::Global::new(tc, v)),
                    None => {
                        let detail = tc
                            .exception()
                            .and_then(|e| v8::Local::<v8::Object>::try_from(e).ok())
                            .and_then(|o| get_prop(tc, o, "message"))
                            .map(|m| m.to_rust_string_lossy(tc))
                            .unwrap_or_default();
                        tc.reset();
                        Err(detail)
                    }
                }
            };
            let detail = match parsed {
                Ok(g) => return Ok(v8::Local::new(scope, &g)),
                Err(d) => d,
            };
            // Blink's context applied after the error is made: Chrome 150's `Response('{').json()` rejects
            // with `stack` "SyntaxError: Expected property name ..." and the whole text in `message`, measured.
            Err(deno_core::error::native_error(scope, "SyntaxError", &format!("Failed to execute 'json' on '{iface}': {detail}")))
        }
    }
}

/// `textStream()` on `this` (a `Response` or `Request`), Chromium 152's `Body::textStream` (`core/fetch/body.cc`): a
/// null body answers a new, closed stream; a locked or used one throws (`RejectInvalidConsumption`: locked first);
/// otherwise the body piped through a UTF-8 `TextDecoderStream` (always UTF-8, the response's headers ignored,
/// whatwg/fetch#1862), whose readable side is the answer.
pub(crate) fn text_stream<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, iface: &str) -> Option<v8::Local<'s, v8::Value>> {
    let body = evt_slot_read(scope, this, "body").filter(|b| !b.is_null());
    let Some(body) = body else {
        return Some(new_closed_stream_with(scope, None).into());
    };
    let rs = as_stream(scope, body)?;
    let locked = rs.is_locked();
    let used = rs.is_disturbed();
    if locked || used {
        let why = if locked { "body stream is locked" } else { "body stream already read" };
        throw_type_error(scope, &format!("Failed to execute 'textStream' on '{iface}': {why}"));
        return None;
    }
    let body_obj = v8::Local::<v8::Object>::try_from(body).ok()?;
    let decoder = crate::binding::protos::new_global(scope, "TextDecoderStream", &[])?;
    let writable = get_prop(scope, decoder, "writable")?;
    let readable = get_prop(scope, decoder, "readable")?;
    crate::core::streams::readable_stream::pipe_through_pair(scope, body_obj, writable, readable)
}

/// `arrayBuffer()` / `blob()` / `bytes()` / `json()` / `text()` on `this` (a `Response` or `Request`).
pub(crate) fn consume<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, iface: &str, kind: Kind) -> v8::Local<'s, v8::Value> {
    let mime = if matches!(kind, Kind::Blob) { mime_of(scope, this) } else { String::new() };
    let body = evt_slot_read(scope, this, "body").filter(|b| !b.is_null());
    let Some(body) = body else {
        // A NULL body's `json()` is not the parser's prefixed message: Chrome 150 answers
        // `SyntaxError: Unexpected end of input` for an opaque response (measured),
        // where an empty-but-present body says "Failed to execute 'json' on 'Response': Unexpected end
        // of JSON input".
        if matches!(kind, Kind::Json) {
            let msg = v8::String::new(scope, "Unexpected end of input").unwrap();
            let e = v8::Exception::syntax_error(scope, msg);
            return rejected_promise(scope, e);
        }
        return match package(scope, kind, iface, &mime, Vec::new()) {
            Ok(v) => resolved_promise(scope, v),
            Err(e) => rejected_promise(scope, e),
        };
    };
    let Some(rs) = as_stream(scope, body) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        return resolved_promise(scope, u);
    };
    let m = kind.method();
    if rs.is_disturbed() {
        return rejected_type_error(scope, &format!("Failed to execute '{m}' on '{iface}': body stream already read"));
    }
    if rs.is_locked() {
        return rejected_type_error(scope, &format!("Failed to execute '{m}' on '{iface}': body stream is locked"));
    }
    let Ok(body_obj) = v8::Local::<v8::Object>::try_from(body) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        return resolved_promise(scope, u);
    };
    // Lock it to an internal reader (it stays locked: Chrome answers `body.locked === true` after
    // `text()`), and read to the end.
    let _ = crate::core::streams::readable_stream::acquire_reader(scope, body_obj, rs);
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    // state = [stream, resolver, kind, chunks, iface, mime, owner]
    let state = v8::Array::new(scope, 7);
    state.set_index(scope, 0, body);
    state.set_index(scope, 1, resolver.into());
    let k: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, kind as u32).into();
    state.set_index(scope, 2, k);
    let chunks = v8::Array::new(scope, 0);
    state.set_index(scope, 3, chunks.into());
    let iv = str_val(scope, iface);
    state.set_index(scope, 4, iv);
    let mv = str_val(scope, &mime);
    state.set_index(scope, 5, mv);
    state.set_index(scope, 6, this.into());
    pump(scope, state);
    promise.into()
}

/// Read synchronously while the stream has chunks (or is closed/errored); park on the first read
/// that has to wait, and resume from its settlement.
fn pump<'s>(scope: &mut v8::PinScope<'s, '_>, state: v8::Local<'s, v8::Array>) {
    let Some(stream) = state.get_index(scope, 0) else { return };
    loop {
        let Some(rs) = as_stream(scope, stream) else { return };
        let p = read_from(scope, rs);
        let Ok(promise) = v8::Local::<v8::Promise>::try_from(p) else { return };
        match promise.state() {
            v8::PromiseState::Pending => {
                let on_ok = build_fn_data(scope, cb_pump_ok, "", 1, state.into());
                let on_err = build_fn_data(scope, cb_pump_err, "", 1, state.into());
                let _ = promise.then2(scope, on_ok, on_err);
                return;
            }
            v8::PromiseState::Rejected => {
                promise.mark_as_handled();
                let reason = promise.result(scope);
                settle(scope, state, Err(reason));
                return;
            }
            v8::PromiseState::Fulfilled => {
                let r = promise.result(scope);
                if !step(scope, state, r) {
                    return;
                }
            }
        }
    }
}

/// One `{value, done}`. `true` = keep reading.
fn step(scope: &mut v8::PinScope, state: v8::Local<v8::Array>, result: v8::Local<v8::Value>) -> bool {
    let Ok(ro) = v8::Local::<v8::Object>::try_from(result) else { return false };
    let done = get_prop(scope, ro, "done").map(|v| v.boolean_value(scope)).unwrap_or(true);
    if done {
        settle(scope, state, Ok(()));
        return false;
    }
    let value = get_prop(scope, ro, "value").unwrap_or_else(|| v8::undefined(scope).into());
    if !value.is_uint8_array() {
        let m = v8::String::new(scope, "Failed to fetch").unwrap();
        let e = v8::Exception::type_error(scope, m);
        settle(scope, state, Err(e));
        return false;
    }
    if let Some(chunks) = state.get_index(scope, 3).and_then(|c| v8::Local::<v8::Array>::try_from(c).ok()) {
        let n = chunks.length();
        chunks.set_index(scope, n, value);
    }
    true
}

fn cb_pump_ok<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Ok(state) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    if step(scope, state, args.get(0)) {
        pump(scope, state);
    }
}

fn cb_pump_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(state) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    settle(scope, state, Err(args.get(0)));
}

/// Resolve or reject the consumer's promise.
fn settle(scope: &mut v8::PinScope, state: v8::Local<v8::Array>, outcome: Result<(), v8::Local<v8::Value>>) {
    // A `Promise::Resolver` IS its promise object in V8 (`v8::Promise::Resolver` is a `JSPromise`), which
    // is why it can ride in a JS array; `is_promise` is the check before the cast back.
    let Some(r) = state.get_index(scope, 1).filter(|r| r.is_promise()) else { return };
    // SAFETY: checked above -- the value stored at index 1 is the resolver `consume` created.
    let resolver = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(r) };
    match outcome {
        Err(e) => {
            resolver.reject(scope, e);
        }
        Ok(()) => {
            // The body has been read to its end: a fetch's resource-timing entry is due now.
            if let Some(owner) = state.get_index(scope, 6).and_then(|o| v8::Local::<v8::Object>::try_from(o).ok()) {
                crate::core::timing::resource_timing::body_consumed(scope, owner);
            }
            let kind = state.get_index(scope, 2).and_then(|k| k.uint32_value(scope)).map(Kind::from_num).unwrap_or(Kind::Text);
            let iface = state.get_index(scope, 4).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let mime = state.get_index(scope, 5).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let mut bytes = Vec::new();
            if let Some(chunks) = state.get_index(scope, 3).and_then(|c| v8::Local::<v8::Array>::try_from(c).ok()) {
                for i in 0..chunks.length() {
                    if let Some(c) = chunks.get_index(scope, i) {
                        if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(c) {
                            let start = bytes.len();
                            bytes.resize(start + view.byte_length(), 0);
                            view.copy_contents(&mut bytes[start..]);
                        }
                    }
                }
            }
            match package(scope, kind, &iface, &mime, bytes) {
                // Fetch's "fully read body" queues a fetch task to run processBody: resolved from a task of its own
                // (`resolve_in_task`, with Chrome 152's measurement), rejected now.
                Ok(v) => {
                    crate::core::html::task_queue::resolve_in_task(scope, resolver, v);
                }
                Err(e) => {
                    resolver.reject(scope, e);
                }
            }
        }
    }
}

/// The bytes of a body that can be read WITHOUT waiting -- every chunk already queued and the stream
/// closed -- for the network request a `fetch(request)` / `xhr.send(body)` puts on the wire. `None`
/// for a null body or one still being produced.
pub(crate) fn buffered_bytes(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    let rs = as_stream(scope, stream)?;
    if rs.is_disturbed() || rs.is_locked() || rs.is_errored() {
        return None;
    }
    let mut out = Vec::new();
    loop {
        let p = read_from(scope, rs);
        let promise = v8::Local::<v8::Promise>::try_from(p).ok()?;
        if promise.state() != v8::PromiseState::Fulfilled {
            promise.mark_as_handled();
            return if out.is_empty() { None } else { Some(out) };
        }
        let r = promise.result(scope);
        let ro = v8::Local::<v8::Object>::try_from(r).ok()?;
        if get_prop(scope, ro, "done").map(|v| v.boolean_value(scope)).unwrap_or(true) {
            return Some(out);
        }
        let v = get_prop(scope, ro, "value")?;
        out.extend(buffer_bytes(v)?);
    }
}

/// `throw` helper for the constructors: a TypeError with the constructor's prefix.
pub(crate) fn throw_ctor(scope: &mut v8::PinScope, iface: &str, msg: &str) {
    throw_type_error(scope, &format!("Failed to construct '{iface}': {msg}"));
}

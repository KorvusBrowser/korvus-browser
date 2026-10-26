//! `core/streams/text_decoder_stream.rs` — `TextDecoderStream` and `TextEncoderStream`.
//!
//! Both were registry D2 stubs (`is_stub: true`): the constructor made an object whose `readable` and
//! `writable` were `undefined`, so `response.body.pipeThrough(new TextDecoderStream())` rejected with
//! Chrome's own sentence `Failed to execute 'pipeTo' on 'ReadableStream': parameter 1 is not of type
//! 'WritableStream'.` and React Router / Remix single-fetch (`turbo-stream`) then threw `No stream found
//! for single fetch decoding` -- six of L14's nine C05 rows.
//!
//! Each is a transform in the WHATWG Encoding sense: a real `WritableStream` whose native sink decodes
//! (or encodes) and enqueues into a real `ReadableStream`, built from the same two stream types
//! `TransformStream` uses. MEASURED on `google-chrome` 150.0.7871.46:
//!
//! ```text
//! new TextDecoderStream().writable instanceof WritableStream        true
//! new TextDecoderStream('utf-8', {fatal:true}): encoding,fatal,BOM  utf-8,true,false
//! new TextDecoderStream('latin1').encoding                          windows-1252
//! new TextDecoderStream('nope') / ('iso-2022-kr')                   RangeError: Failed to construct 'TextDecoderStream': The encoding label provided ('nope') is invalid.
//! [c3] [a9 41] [c3] piped through                                   ["éA","\u{FFFD}"]   (a split sequence waits; the flush replaces)
//! [ef bb] [bf 41]                                                   ["A"]               (the BOM is stripped across chunks)
//! [ff] with fatal                                                   read rejects TypeError: The encoded data is not valid.
//! writer.write("str")                                               write AND read reject TypeError: The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.
//! TextEncoderStream: "é", ""                                        [Uint8Array 195.169]  (an empty chunk enqueues nothing)
//! TextEncoderStream: "\u{D83D}", "\u{DE00}", "\u{D83D}"             ["240.159.152.128","239.191.189"]  (a pair split across chunks is joined; a trailing lone lead flushes as U+FFFD)
//! TextEncoderStream: 12                                             49.50  (the chunk is converted to a string)
//! own prototype names                                               encoding,fatal,ignoreBOM,readable,writable,constructor | encoding,readable,writable,constructor
//! a foreign receiver on the readable getter                         TypeError: Illegal invocation
//! ```

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{build_fn_data, get_prop, set_prop, str_val, throw_range_error, throw_type_error};
use crate::install::live::{getter_row, set_table_proto};
use crate::install::MemberDef;
use super::readable_stream::{rs_close, rs_enqueue, rs_error};

// ── TextDecoderStream ───────────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(CppgcBase)]
pub struct TextDecoderStream {
    readable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    writable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    encoding: std::cell::Cell<&'static encoding_rs::Encoding>,
    fatal: std::cell::Cell<bool>,
    ignore_bom: std::cell::Cell<bool>,
    decoder: std::cell::RefCell<Option<encoding_rs::Decoder>>,
}

pub(crate) trait TextDecoderStreamBehavior {
    fn construct() -> TextDecoderStream where Self: Sized { stub!("TextDecoderStream::constructor") }
}

#[webapi(base, own)]
impl TextDecoderStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<TextDecoderStream, ::deno_error::JsErrorBox> {
        dispatch!("TextDecoderStream::constructor", <TextDecoderStream as TextDecoderStreamBehavior>::construct())
    }
}

impl TextDecoderStreamBehavior for TextDecoderStream {
    fn construct() -> TextDecoderStream {
        TextDecoderStream {
            readable: std::cell::RefCell::new(None),
            writable: std::cell::RefCell::new(None),
            encoding: std::cell::Cell::new(encoding_rs::UTF_8),
            fatal: std::cell::Cell::new(false),
            ignore_bom: std::cell::Cell::new(false),
            decoder: std::cell::RefCell::new(None),
        }
    }
}

// ── TextEncoderStream ───────────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(CppgcBase)]
pub struct TextEncoderStream {
    readable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    writable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    /// A lead surrogate that ended the previous chunk, waiting for its trail.
    pending_lead: std::cell::Cell<Option<u16>>,
}

pub(crate) trait TextEncoderStreamBehavior {
    fn construct() -> TextEncoderStream where Self: Sized { stub!("TextEncoderStream::constructor") }
}

#[webapi(base, own)]
impl TextEncoderStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<TextEncoderStream, ::deno_error::JsErrorBox> {
        dispatch!("TextEncoderStream::constructor", <TextEncoderStream as TextEncoderStreamBehavior>::construct())
    }
}

impl TextEncoderStreamBehavior for TextEncoderStream {
    fn construct() -> TextEncoderStream {
        TextEncoderStream {
            readable: std::cell::RefCell::new(None),
            writable: std::cell::RefCell::new(None),
            pending_lead: std::cell::Cell::new(None),
        }
    }
}


// ── shared plumbing ─────────────────────────────────────────────────────────────────────────────

/// Re-prototype a freshly constructed object onto `new.target.prototype` (a subclass) or the table's.
fn adopt_proto(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, obj: v8::Local<v8::Object>, iface: &str) {
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => set_table_proto(scope, obj, iface),
    }
}

/// A `readable` + a `writable` whose sink's `write`/`close` are `write`/`close` with `owner` bound as
/// their data.
fn make_pair<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    owner: v8::Local<'s, v8::Object>,
    write: impl v8::MapFnTo<v8::FunctionCallback>,
    close: impl v8::MapFnTo<v8::FunctionCallback>,
) -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>) {
    let readable = super::readable_stream::new_stream(scope);
    let sink = v8::Object::new(scope);
    let ov: v8::Local<v8::Value> = owner.into();
    let w = build_fn_data(scope, write, "write", 1, ov);
    let c = build_fn_data(scope, close, "close", 0, ov);
    set_prop(scope, sink, "write", w.into());
    set_prop(scope, sink, "close", c.into());
    let writable = crate::binding::class::mint(
        scope,
        <super::writable_stream::WritableStream as super::writable_stream::WritableStreamBehavior>::construct(),
    );
    set_table_proto(scope, writable, "WritableStream");
    super::writable_stream::ws_init(scope, writable.into(), sink.into());
    (readable, writable)
}

/// Error `readable` with a new `TypeError(msg)` and throw the same object -- the write rejects and the
/// readable side's reads reject with it, which is what Chrome answers for both halves.
fn fail(scope: &mut v8::PinScope, readable: Option<v8::Global<v8::Object>>, msg: &str) {
    let m = v8::String::new(scope, msg).unwrap();
    let exc = v8::Exception::type_error(scope, m);
    if let Some(r) = readable {
        let r = v8::Local::new(scope, &r);
        rs_error(scope, r.into(), exc);
    }
    scope.throw_exception(exc);
}

/// The bytes of an `ArrayBuffer` / `SharedArrayBuffer` / `ArrayBufferView`, or `None`.
pub(crate) fn buffer_source_bytes(v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        return Some(buf);
    }
    let store = if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        ab.get_backing_store()
    } else if let Ok(sab) = v8::Local::<v8::SharedArrayBuffer>::try_from(v) {
        sab.get_backing_store()
    } else {
        return None;
    };
    let n = store.byte_length();
    let mut out = vec![0u8; n];
    if n > 0 {
        if let Some(ptr) = store.data() {
            // SAFETY: the store is alive for this borrow and holds exactly `n` bytes.
            unsafe { std::ptr::copy_nonoverlapping(ptr.as_ptr() as *const u8, out.as_mut_ptr(), n) };
        }
    }
    Some(out)
}

// ── TextDecoderStream bodies ────────────────────────────────────────────────────────────────────

fn tds_of<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a TextDecoderStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<TextDecoderStream>(scope, v).map(|p| unsafe { &*(p.as_ref() as *const TextDecoderStream) })
}

/// Run `bytes` through the decoder. `Err` is the fatal-mode failure.
fn tds_decode(t: &TextDecoderStream, bytes: &[u8], last: bool) -> Result<String, ()> {
    let mut slot = t.decoder.borrow_mut();
    let dec = slot.get_or_insert_with(|| {
        let e = t.encoding.get();
        if t.ignore_bom.get() { e.new_decoder_without_bom_handling() } else { e.new_decoder_with_bom_removal() }
    });
    let cap = dec.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len().saturating_mul(4) + 4);
    let mut out = String::with_capacity(cap);
    if t.fatal.get() {
        let (res, _) = dec.decode_to_string_without_replacement(bytes, &mut out, last);
        if let encoding_rs::DecoderResult::Malformed(_, _) = res {
            return Err(());
        }
    } else {
        let _ = dec.decode_to_string(bytes, &mut out, last);
    }
    Ok(out)
}

fn cb_tds_sink_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(t) = tds_of(scope, __body_data) else { return };
    let readable = t.readable.borrow().clone();
    let Some(bytes) = buffer_source_bytes(args.get(0)) else {
        fail(scope, readable, "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.");
        return;
    };
    match tds_decode(t, &bytes, false) {
        Ok(s) if s.is_empty() => {}
        Ok(s) => {
            if let Some(r) = readable {
                let r = v8::Local::new(scope, &r);
                let sv = str_val(scope, &s);
                let _ = rs_enqueue(scope, r.into(), sv);
            }
        }
        Err(()) => fail(scope, readable, "The encoded data is not valid."),
    }
}

fn cb_tds_sink_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(t) = tds_of(scope, __body_data) else { return };
    let readable = t.readable.borrow().clone();
    let Some(r) = readable.clone() else { return };
    match tds_decode(t, &[], true) {
        Ok(s) => {
            let r = v8::Local::new(scope, &r);
            if !s.is_empty() {
                let sv = str_val(scope, &s);
                let _ = rs_enqueue(scope, r.into(), sv);
            }
            let _ = rs_close(scope, r.into());
        }
        Err(()) => fail(scope, readable, "The encoded data is not valid."),
    }
}

/// `new TextDecoderStream(optional DOMString label = "utf-8", optional TextDecoderOptions options = {})`.
pub fn cb_tds_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("TextDecoderStream"));
        return;
    }
    let label_v = args.get(0);
    let label = if label_v.is_undefined() {
        "utf-8".to_string()
    } else {
        match label_v.to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => return,
        }
    };
    let Ok(opts) = crate::install::webidl::init_dict(scope, &args, 1, "TextDecoderStream", "TextDecoderOptions") else { return };
    let fatal = crate::install::webidl::dict_bool(scope, opts, "fatal");
    let ignore_bom = crate::install::webidl::dict_bool(scope, opts, "ignoreBOM");
    // WHATWG Encoding "get an encoding": trim ASCII whitespace, match a label; the `replacement`
    // encoding is a RangeError here exactly as an unknown label is.
    let enc = encoding_rs::Encoding::for_label(label.trim_matches(|c: char| c.is_ascii_whitespace()).as_bytes())
        .filter(|e| *e != encoding_rs::REPLACEMENT);
    let Some(enc) = enc else {
        throw_range_error(scope, &format!("Failed to construct 'TextDecoderStream': The encoding label provided ('{label}') is invalid."));
        return;
    };
    let obj = crate::binding::class::mint(scope, <TextDecoderStream as TextDecoderStreamBehavior>::construct());
    adopt_proto(scope, &args, obj, "TextDecoderStream");
    let (readable, writable) = make_pair(scope, obj, cb_tds_sink_write, cb_tds_sink_close);
    if let Some(t) = tds_of(scope, obj.into()) {
        t.encoding.set(enc);
        t.fatal.set(fatal);
        t.ignore_bom.set(ignore_bom);
        *t.readable.borrow_mut() = Some(v8::Global::new(scope, readable));
        *t.writable.borrow_mut() = Some(v8::Global::new(scope, writable));
    }
    rv.set(obj.into());
}

fn tds_this<'a>(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<&'a TextDecoderStream> {
    let t = tds_of(scope, args.this().into());
    if t.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    t
}

fn cb_tds_encoding(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tds_this(scope, &args) else { return };
    let v = str_val(scope, &t.encoding.get().name().to_ascii_lowercase());
    rv.set(v);
}
fn cb_tds_fatal(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tds_this(scope, &args) else { return };
    rv.set_bool(t.fatal.get());
}
fn cb_tds_ignore_bom(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tds_this(scope, &args) else { return };
    rv.set_bool(t.ignore_bom.get());
}
fn cb_tds_readable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tds_this(scope, &args) else { return };
    if let Some(g) = t.readable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}
fn cb_tds_writable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tds_this(scope, &args) else { return };
    if let Some(g) = t.writable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

/// The body every row of [`TEXTDECODERSTREAM`] names.
pub fn install_text_decoder_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "encoding" => getter_row(scope, proto, member, cb_tds_encoding),
        "fatal" => getter_row(scope, proto, member, cb_tds_fatal),
        "ignoreBOM" => getter_row(scope, proto, member, cb_tds_ignore_bom),
        "readable" => getter_row(scope, proto, member, cb_tds_readable),
        "writable" => getter_row(scope, proto, member, cb_tds_writable),
        other => debug_assert!(false, "install_text_decoder_stream: unexpected member {other}"),
    }
}

// ── TextEncoderStream bodies ────────────────────────────────────────────────────────────────────

fn tes_of<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a TextEncoderStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<TextEncoderStream>(scope, v).map(|p| unsafe { &*(p.as_ref() as *const TextEncoderStream) })
}

/// A `Uint8Array` owning `bytes`.
fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match v8::Uint8Array::new(scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(scope).into(),
    }
}

/// UTF-16 code units -> UTF-8, joining a lead surrogate carried from the previous chunk and carrying a
/// trailing lead into the next one (WHATWG Encoding "encode and enqueue a chunk").
fn tes_encode(t: &TextEncoderStream, units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 3);
    let push = |c: char, out: &mut Vec<u8>| {
        let mut b = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    };
    let mut lead = t.pending_lead.take();
    for &u in units {
        if let Some(l) = lead.take() {
            if (0xDC00..=0xDFFF).contains(&u) {
                let cp = 0x10000 + (((l as u32) - 0xD800) << 10) + ((u as u32) - 0xDC00);
                push(char::from_u32(cp).unwrap_or('\u{FFFD}'), &mut out);
                continue;
            }
            push('\u{FFFD}', &mut out);
        }
        if (0xD800..=0xDBFF).contains(&u) {
            lead = Some(u);
        } else if (0xDC00..=0xDFFF).contains(&u) {
            push('\u{FFFD}', &mut out);
        } else {
            push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'), &mut out);
        }
    }
    t.pending_lead.set(lead);
    out
}

fn cb_tes_sink_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(t) = tes_of(scope, __body_data) else { return };
    // The chunk is a DOMString: converted, so a number encodes its decimal spelling (Chrome: 12 -> 49.50).
    let Some(s) = args.get(0).to_string(scope) else { return };
    let mut units = vec![0u16; s.length()];
    s.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
    let bytes = tes_encode(t, &units);
    if bytes.is_empty() {
        return;
    }
    let readable = t.readable.borrow().clone();
    if let Some(r) = readable {
        let r = v8::Local::new(scope, &r);
        let chunk = u8_array(scope, bytes);
        let _ = rs_enqueue(scope, r.into(), chunk);
    }
}

fn cb_tes_sink_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(t) = tes_of(scope, __body_data) else { return };
    let readable = t.readable.borrow().clone();
    let Some(r) = readable else { return };
    let r = v8::Local::new(scope, &r);
    if t.pending_lead.take().is_some() {
        let chunk = u8_array(scope, vec![0xEF, 0xBF, 0xBD]);
        let _ = rs_enqueue(scope, r.into(), chunk);
    }
    let _ = rs_close(scope, r.into());
}

/// `new TextEncoderStream()`.
pub fn cb_tes_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("TextEncoderStream"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <TextEncoderStream as TextEncoderStreamBehavior>::construct());
    adopt_proto(scope, &args, obj, "TextEncoderStream");
    let (readable, writable) = make_pair(scope, obj, cb_tes_sink_write, cb_tes_sink_close);
    if let Some(t) = tes_of(scope, obj.into()) {
        *t.readable.borrow_mut() = Some(v8::Global::new(scope, readable));
        *t.writable.borrow_mut() = Some(v8::Global::new(scope, writable));
    }
    rv.set(obj.into());
}

fn tes_this<'a>(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<&'a TextEncoderStream> {
    let t = tes_of(scope, args.this().into());
    if t.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    t
}

fn cb_tes_encoding(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if tes_this(scope, &args).is_none() {
        return;
    }
    let v = str_val(scope, "utf-8");
    rv.set(v);
}
fn cb_tes_readable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tes_this(scope, &args) else { return };
    if let Some(g) = t.readable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}
fn cb_tes_writable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = tes_this(scope, &args) else { return };
    if let Some(g) = t.writable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

/// The body every row of [`TEXTENCODERSTREAM`] names.
pub fn install_text_encoder_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "encoding" => getter_row(scope, proto, member, cb_tes_encoding),
        "readable" => getter_row(scope, proto, member, cb_tes_readable),
        "writable" => getter_row(scope, proto, member, cb_tes_writable),
        other => debug_assert!(false, "install_text_encoder_stream: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `TextDecoderStream` / `TextEncoderStream` blocks of
// `install/generated_registry.rs` (I88 / I86), in the capture's order. `[value, both]`: every bundle
// records both as newable in both realms, constructor length 0.
webapi::interface! {
    "TextDecoderStream" [value, both] {
        ctor "TextDecoderStream"                 len 0 = stub,
        attr "encoding"  : DomString readonly          = install_text_decoder_stream,
        attr "fatal"     : Boolean readonly            = install_text_decoder_stream,
        attr "ignoreBOM" : Boolean readonly            = install_text_decoder_stream,
        attr "readable"  : Any readonly                = install_text_decoder_stream,
        attr "writable"  : Any readonly                = install_text_decoder_stream
    }
}

webapi::interface! {
    "TextEncoderStream" [value, both] {
        ctor "TextEncoderStream"                 len 0 = stub,
        attr "encoding" : DomString readonly           = install_text_encoder_stream,
        attr "readable" : Any readonly                 = install_text_encoder_stream,
        attr "writable" : Any readonly                 = install_text_encoder_stream
    }
}

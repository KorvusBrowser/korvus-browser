//! `core/misc_ifaces.rs` -- small Blink interfaces whose behaviour is a little real state
//! (the Chrome oracle ratchet). Each was a generated stub row; each answer here is Blink's,
//! measured on stock Chrome 150 (`core/korvus-runtime/tests/fixtures/misc_ifaces/`):
//!
//! - `CountQueuingStrategy.size` / `ByteLengthQueuingStrategy.size`: ONE function per realm per
//!   strategy (`new CountQueuingStrategy({highWaterMark: 1}).size === new CountQueuingStrategy({...}).size`),
//!   answering 1 / `chunk.byteLength` (Streams: "count queuing strategy size function").
//! - `TouchList`: `item(i)`, `length`, `list[i]` over the touches a TouchEvent holds.
//! - `EncodedVideoChunk` / `EncodedAudioChunk.byteLength`: the init's `data` length.
//! - `AudioData`: `duration` (frames / sampleRate in whole microseconds), `allocationSize(options)`
//!   (`planeIndex` required), `clone()`.
//! - `ImageDecoder.isTypeSupported(type)` (Blink's decodable image types), `tracks` (an
//!   `ImageTrackList`: `ready` a promise, `length` 0 and `selectedIndex` -1 until decoded).
//! - `Origin`: `new Origin()` is opaque; `Origin.from(x)` reads a URL string's origin, else TypeError.
//! - `SpeechGrammarList` / `SpeechGrammar`: `addFromString(s, w)` adds a grammar whose `src` is
//!   `data:application/xml,` + the percent-encoded string, `item(i)` / `list[i]`, `weight` a float.
//! - `ChapterInformation`: what `new MediaMetadata({chapterInfo})` holds.
//! - `Sanitizer`: Blink's DEFAULT configuration (vendored `gen/blink/sanitizer/`), `get()` in WebIDL's
//!   member order, the setters returning `undefined`.
//! - `WebTransport`: the URL must parse as https (SyntaxError "The URL 'x' is invalid."); its
//!   `datagrams` / `incoming*Streams` are real objects, and the datagram stream answers Chrome 152's defaults and
//!   setter rules (`maxDatagramSize` 1024, queue limits of at least one, a max age kept only when positive). `WebTransportError` / `WebSocketError` are
//!   DOMExceptions of their own name, made as Blink 150 makes them: `WebTransportError(init)` (one dictionary,
//!   not webref's `(message, options)`), `WebSocketError(message, init)` with its close code and reason checks.
//! - `PaymentRequest`: "At least one payment method is required" for an empty `methodData`.
//! - `URLPattern` from a dictionary: a component the init leaves out is `*` (no baseURL).

use deno_core::v8;

use crate::binding::protos::{body_data, build_fn_data, get_prop, make_dom_exc, str_val, throw_dom, throw_type_error};
use crate::install::MemberDef;

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_misc_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}
pub(crate) fn mset(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = key(scope, name);
    o.set_private(scope, k, v);
}
pub(crate) fn mget<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = key(scope, name);
    o.get_private(scope, k).filter(|v| !v.is_undefined())
}

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, iface);
    Some(o)
}

fn is(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str) -> bool {
    crate::install::webidl::has_brand(scope, v, iface) || crate::binding::protos::implements(scope, v, iface)
}

fn promise<'s>(scope: &mut v8::PinScope<'s, '_>, ok: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    crate::binding::protos::resolved_promise(scope, ok)
}

// ── constructor hooks (`binding::protos::cb_shell_ctor`) ─────────────────────────────────────────

/// Validation Blink's constructor makes BEFORE the dictionary is stored. `true` = it threw.
pub(crate) fn pre_ctor(scope: &mut v8::PinScope, iface: &str, args: &v8::FunctionCallbackArguments) -> bool {
    match iface {
        "WebTransport" => {
            let raw = args.get(0).to_rust_string_lossy(scope);
            let base = url::Url::parse(&crate::core::dom::document::base_url(scope)).ok();
            let ok = url::Url::options().base_url(base.as_ref()).parse(&raw).is_ok_and(|u| u.scheme() == "https");
            if !ok {
                throw_dom(scope, &format!("Failed to construct 'WebTransport': The URL '{raw}' is invalid."), "SyntaxError");
                return true;
            }
        }
        "MediaStreamTrackProcessor" => {
            // `MediaStreamTrackProcessorInit`, converted as WebIDL converts it (Chrome 150):
            // `maxBufferSize`, then `track` (required); korvus read `track` alone
            if crate::install::ctor_dicts::convert_dict(scope, args.get(0), "MediaStreamTrackProcessorInit", "Failed to construct 'MediaStreamTrackProcessor': ").is_err() {
                return true;
            }
        }
        _ => {}
    }
    false
}

/// What Blink's constructor refuses in its CONVERTED arguments, before it makes the object. `true` = it threw.
pub(crate) fn refused_ctor(scope: &mut v8::PinScope, iface: &str, conv: Option<&crate::install::ctor_dicts::Converted>) -> bool {
    if iface == "PaymentRequest" {
        // the constructor's steps, after the binding converted its arguments: no payment method at all, then
        // `ValidateAndConvertPaymentDetailsInit` -- a converted `PaymentDetailsInit` with neither `total` nor `id` was not
        // given at all, one without `total` has no total (Chrome 150, measured: korvus constructed both, and refused an
        // empty method list before reading the details)
        let methods = conv.and_then(|c| get_prop(scope, c.named, "methodData")).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
        if methods.is_some_and(|m| m.length() == 0) {
            throw_type_error(scope, "Failed to construct 'PaymentRequest': At least one payment method is required");
            return true;
        }
        if let Some(details) = conv.and_then(|c| c.dict).map(|(_, d)| d) {
            let has = |scope: &mut v8::PinScope, k: &str| get_prop(scope, details, k).is_some_and(|v| !v.is_undefined());
            let msg = if !has(scope, "total") && !has(scope, "id") {
                Some("required member details is undefined.")
            } else if !has(scope, "total") {
                Some("required member total is undefined.")
            } else {
                None
            };
            if let Some(m) = msg {
                throw_type_error(scope, &format!("Failed to construct 'PaymentRequest': {m}"));
                return true;
            }
        }
    }
    if iface == "WebSocketError" {
        // `WebSocketError::ValidateAndCreate` -> `WebSocketCommon::ValidateCloseCodeAndReason` (Chrome 150, measured: korvus
        // made the error with any code and any reason): a close code is 1000 or 3000-4999 (the `[Clamp]`ed value names
        // itself: -1 is "0 is neither."), and the reason at most 123 UTF-8 bytes.
        let init = conv.and_then(|c| c.dict).map(|(_, d)| d);
        let code = init.and_then(|d| get_prop(scope, d, "closeCode")).filter(|v| v.is_number()).and_then(|v| v.number_value(scope));
        if let Some(c) = code.filter(|c| !(*c == 1000.0 || (3000.0..=4999.0).contains(c))) {
            throw_dom(scope, &format!("Failed to construct 'WebSocketError': The close code must be either 1000, or between 3000 and 4999. {c} is neither."), "InvalidAccessError");
            return true;
        }
        let reason = init.and_then(|d| get_prop(scope, d, "reason")).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        if reason.len() > 123 {
            throw_dom(scope, "Failed to construct 'WebSocketError': The close reason must not be greater than 123 UTF-8 bytes.", "SyntaxError");
            return true;
        }
    }
    false
}

/// State Blink's constructor computes from its (converted) arguments: `dict` the dictionary, `named` the others by
/// their IDL names.
pub(crate) fn post_ctor(scope: &mut v8::PinScope, iface: &str, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments, dict: Option<v8::Local<v8::Object>>, named: Option<v8::Local<v8::Object>>) {
    match iface {
        "EncodedVideoChunk" | "EncodedAudioChunk" => {
            let n = dict.and_then(|d| get_prop(scope, d, "data")).map(|v| byte_length(scope, v)).unwrap_or(0.0);
            let v = v8::Number::new(scope, n).into();
            mset(scope, this, "byteLength", v);
        }
        "AudioData" => {
            if let Some(d) = dict {
                let frames = get_prop(scope, d, "numberOfFrames").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
                let rate = get_prop(scope, d, "sampleRate").and_then(|v| v.number_value(scope)).unwrap_or(1.0);
                // Blink: duration = frames * 1e6 / sampleRate, as an int64 of microseconds.
                let dur = if rate > 0.0 { (frames * 1_000_000.0 / rate).trunc() } else { 0.0 };
                let v = v8::Number::new(scope, dur).into();
                mset(scope, this, "duration", v);
                mset(scope, this, "init", d.into());
            }
        }
        "ImageDecoder" => {
            if let Some(list) = mint(scope, "ImageTrackList") {
                let p = v8::PromiseResolver::new(scope);
                if let Some(p) = p {
                    let pr: v8::Local<v8::Value> = p.get_promise(scope).into();
                    mset(scope, list, "ready", pr);
                }
                mset(scope, this, "tracks", list.into());
            }
        }
        "SpeechGrammar" => {
            let e = str_val(scope, "");
            mset(scope, this, "src", e);
            let w = v8::Number::new(scope, 1.0).into();
            mset(scope, this, "weight", w);
        }
        "SpeechGrammarList" => {
            let a = v8::Array::new(scope, 0).into();
            mset(scope, this, "items", a);
        }
        "Origin" => {
            let t = v8::Boolean::new(scope, true).into();
            mset(scope, this, "opaque", t);
        }
        "WebTransportError" => {
            // Blink 150's `WebTransportError(optional WebTransportErrorInit init = {})` (web_transport_error.cc): the message
            // is `init.message`, `streamErrorCode` is `init.streamErrorCode` (a `[Clamp] octet`) or null, the source is
            // "stream" (Chrome 150, measured). korvus took webref's `(message, options)`: the dictionary was the message.
            let msg = dict.and_then(|d| get_prop(scope, d, "message")).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            crate::core::dom::dom_exception::init_dom_exception(scope, this, &msg, iface);
            let sv = str_val(scope, "stream");
            crate::core::events::event::evt_slot_write(scope, this, "source", sv);
            match dict.and_then(|d| get_prop(scope, d, "streamErrorCode")).filter(|v| v.is_number()) {
                Some(v) => crate::core::events::event::evt_slot_write(scope, this, "streamErrorCode", v),
                None => crate::core::events::event::evt_slot_write_null(scope, this, "streamErrorCode"),
            }
        }
        "WebSocketError" => {
            // `WebSocketError(optional DOMString message = "", optional WebSocketCloseInfo init = {})`: a reason with no
            // close code closes normally -- `closeCode` 1000 (`ValidateCloseCodeAndReason`; [`refused_ctor`] checked both)
            let msg = named.and_then(|n| get_prop(scope, n, "message")).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            crate::core::dom::dom_exception::init_dom_exception(scope, this, &msg, iface);
            let reason = dict.and_then(|d| get_prop(scope, d, "reason")).is_some_and(|r| r.is_string() && r.to_rust_string_lossy(scope) != "");
            let code = dict.and_then(|d| get_prop(scope, d, "closeCode")).is_some_and(|c| c.is_number());
            if reason && !code {
                let n = v8::Number::new(scope, 1000.0).into();
                crate::core::events::event::evt_slot_write(scope, this, "closeCode", n);
            }
        }
        "WebTransport" => {
            if let Some(dg) = mint(scope, "WebTransportDatagramDuplexStream") {
                // `web_transport.cc`: the datagram readable is a byte stream and the writable a stream of its own,
                // made with the transport; both answer `[SameObject]` from the datagram stream.
                let r = crate::core::streams::readable_stream::new_stream(scope);
                mset(scope, dg, "readable", r.into());
                let w = crate::core::streams::writable_stream::mint_stream(scope);
                mset(scope, dg, "writable", w.into());
                mset(scope, this, "datagrams", dg.into());
            }
            for k in ["incomingBidirectionalStreams", "incomingUnidirectionalStreams"] {
                let s = crate::core::streams::readable_stream::new_stream(scope);
                mset(scope, this, k, s.into());
            }
        }
        "URLPattern" => {
            if args.get(0).is_object() {
                // `URLPatternInit`, converted as WebIDL converts it (Chrome 150): `baseURL`, `hash`,
                // `hostname`, `password`, `pathname`, `port`, `protocol`, `search`, `username`, each read once (korvus
                // read the eight components in component order, and never `baseURL`)
                let Ok(d) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "URLPatternInit", "Failed to construct 'URLPattern': ") else { return };
                {
                    for c in ["protocol", "username", "password", "hostname", "port", "pathname", "search", "hash"] {
                        let given = get_prop(scope, d, c).filter(|v| !v.is_undefined());
                        let v = match given {
                            Some(v) => v.to_string(scope).map(|s| s.into()).unwrap_or_else(|| str_val(scope, "*")),
                            None => str_val(scope, "*"),
                        };
                        mset(scope, this, c, v);
                    }
                }
            }
        }
        _ => {}
    }
}

fn byte_length(_scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> f64 {
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        return ab.byte_length() as f64;
    }
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        return view.byte_length() as f64;
    }
    0.0
}

// ── member bodies ─────────────────────────────────────────────────────────────────────────────────

fn cb_strategy_size_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let iface = body_data(scope, &args).to_rust_string_lossy(scope);
    if !is(scope, args.this().into(), &iface) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    // One function per realm (cached on the realm's global under a private).
    let global = scope.get_current_context().global(scope);
    let cache = format!("size_{iface}");
    if let Some(f) = mget(scope, global, &cache) {
        rv.set(f);
        return;
    }
    let f = if iface == "CountQueuingStrategy" {
        crate::binding::protos::build_fn(scope, cb_count_size, "size", 0)
    } else {
        crate::binding::protos::build_fn(scope, cb_byte_size, "size", 1)
    };
    mset(scope, global, &cache, f.into());
    rv.set(f.into());
}
fn cb_count_size(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_double(1.0);
}
fn cb_byte_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // `Get(chunk, "byteLength")`: a page's own object with a byteLength property counts too.
    let c = args.get(0);
    let Ok(o) = v8::Local::<v8::Object>::try_from(c) else {
        throw_type_error(scope, &format!("Cannot read properties of {} (reading 'byteLength')", if c.is_null() { "null" } else { "undefined" }));
        return;
    };
    if let Some(v) = get_prop(scope, o, "byteLength") {
        rv.set(v);
    }
}

fn cb_slot_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, name) = d.split_once('.').unwrap_or(("", d.as_str()));
    let this = args.this();
    if !is(scope, this.into(), iface) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    // `[SameObject] readonly attribute FrozenArray<MediaImage> artwork`, with no `[CallWith=ScriptState]`: the
    // binding calls the getter returning the `HeapVector`, so Chrome 150 hands out a NEW frozen array of new,
    // unfrozen images on every read, whatever `[SameObject]` promises (measured, C2; `install::frozen_array`).
    if (iface, name) == ("ChapterInformation", "artwork") {
        let stored = mget(scope, this, name).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
        let stored: Vec<v8::Local<v8::Value>> = match stored {
            Some(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect(),
            None => Vec::new(),
        };
        let answer = crate::install::frozen_array::answer(scope, this, iface, name, |_| 0.0, |scope| {
            Some(stored.iter().map(|e| crate::core::media::media_session::media_image(scope, *e)).collect())
        });
        if let Some(a) = answer {
            rv.set(a);
        }
        return;
    }
    match mget(scope, this, name) {
        Some(v) => rv.set(v),
        None => match (iface, name) {
            ("ImageTrackList", "length") => rv.set_uint32(0),
            ("ImageTrackList", "selectedIndex") => rv.set_int32(-1),
            ("ImageTrackList", "selectedTrack") => rv.set_null(),
            ("Origin", "opaque") => rv.set_bool(false),
            ("ChapterInformation", "title") => rv.set(str_val(scope, "")),
            ("ChapterInformation", "startTime") => rv.set_double(0.0),
            // `datagram_duplex_stream.h` (Chrome 152): a 1024-byte datagram until the session says otherwise, no
            // max age, and both queue limits at the spec minimum of one (`web_transport.cc` creates the stream
            // with an outgoing limit of 1). The deprecated `*HighWaterMark`s read the same limits as a `long`.
            ("WebTransportDatagramDuplexStream", "maxDatagramSize") => rv.set_uint32(1024),
            ("WebTransportDatagramDuplexStream", "incomingMaxAge" | "outgoingMaxAge") => rv.set_null(),
            ("WebTransportDatagramDuplexStream", "incomingMaxBufferedDatagrams" | "outgoingMaxBufferedDatagrams") => rv.set_uint32(1),
            ("WebTransportDatagramDuplexStream", "incomingHighWaterMark" | "outgoingHighWaterMark") => {
                let limit = if name.starts_with("incoming") { "incomingMaxBufferedDatagrams" } else { "outgoingMaxBufferedDatagrams" };
                let n = mget(scope, this, limit).and_then(|v| v.uint32_value(scope)).unwrap_or(1);
                rv.set_int32(n.min(i32::MAX as u32) as i32)
            }
            ("URLPattern", _) => rv.set(str_val(scope, "*")),
            ("SpeechGrammarList", "length") | ("TouchList", "length") => {
                let n = mget(scope, this, "items").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()).map(|a| a.length()).unwrap_or(0);
                rv.set_uint32(n)
            }
            _ => rv.set_undefined(),
        },
    }
}

fn cb_slot_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let d = body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, name) = d.split_once('.').unwrap_or(("", d.as_str()));
    let this = args.this();
    if !is(scope, this.into(), iface) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let v = args.get(0);
    let converted: v8::Local<v8::Value> = match (iface, name) {
        // `attribute float weight`
        ("SpeechGrammar", "weight") => {
            let Some(n) = v.number_value(scope) else { return };
            if !n.is_finite() {
                throw_type_error(scope, "Failed to set the 'weight' property on 'SpeechGrammar': The provided float value is non-finite.");
                return;
            }
            v8::Number::new(scope, n as f32 as f64).into()
        }
        // `attribute DOMString src`, kept as the document-resolved URL (`speech_grammar.cc`).
        ("SpeechGrammar", "src") => {
            let raw = v.to_rust_string_lossy(scope);
            let base = url::Url::parse(&crate::core::dom::document::base_url(scope)).ok();
            let resolved = url::Url::options().base_url(base.as_ref()).parse(&raw).map(|u| u.to_string()).unwrap_or(raw);
            str_val(scope, &resolved)
        }
        // `attribute double? incomingMaxAge` / `outgoingMaxAge`: null clears; a number is kept only when it is
        // above zero (`DatagramDuplexStream::setIncomingMaxAge`), and a non-finite one is the IDL conversion's TypeError.
        ("WebTransportDatagramDuplexStream", "incomingMaxAge" | "outgoingMaxAge") => {
            if v.is_null_or_undefined() {
                v8::null(scope).into()
            } else {
                let Some(n) = v.number_value(scope) else { return };
                if !n.is_finite() {
                    throw_type_error(scope, &format!("Failed to set the '{name}' property on 'WebTransportDatagramDuplexStream': The provided double value is non-finite."));
                    return;
                }
                if n <= 0.0 {
                    return;
                }
                v8::Number::new(scope, n).into()
            }
        }
        // `attribute unsigned long incomingMaxBufferedDatagrams` / `outgoing...`: ToUint32, then at least one.
        ("WebTransportDatagramDuplexStream", "incomingMaxBufferedDatagrams" | "outgoingMaxBufferedDatagrams") => {
            let Some(n) = v.uint32_value(scope) else { return };
            v8::Integer::new_from_unsigned(scope, n.max(1)).into()
        }
        // The deprecated `attribute long incomingHighWaterMark` / `outgoing...`: ToInt32, saturated to unsigned
        // (a negative one is 0) and stored as the limit it aliases, at least one.
        ("WebTransportDatagramDuplexStream", "incomingHighWaterMark" | "outgoingHighWaterMark") => {
            let Some(n) = v.int32_value(scope) else { return };
            let limit = if name.starts_with("incoming") { "incomingMaxBufferedDatagrams" } else { "outgoingMaxBufferedDatagrams" };
            let u = v8::Integer::new_from_unsigned(scope, (n.max(0) as u32).max(1)).into();
            mset(scope, this, limit, u);
            return;
        }
        _ => v,
    };
    mset(scope, this, name, converted);
}

fn cb_method(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, name) = d.split_once('.').unwrap_or(("", d.as_str()));
    let this = args.this();
    let is_static = matches!((iface, name), ("ImageDecoder", "isTypeSupported") | ("Origin", "from"));
    if !is_static && !is(scope, this.into(), iface) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    match (iface, name) {
        ("TouchList", "item") | ("SpeechGrammarList", "item") => {
            let i = args.get(0).uint32_value(scope).unwrap_or(0);
            let items = mget(scope, this, "items").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
            match items.filter(|a| i < a.length()).and_then(|a| a.get_index(scope, i)) {
                Some(v) => rv.set(v),
                None => rv.set_null(),
            }
        }
        ("SpeechGrammarList", "addFromString") | ("SpeechGrammarList", "addFromUri") => {
            let text = args.get(0).to_rust_string_lossy(scope);
            let weight = if args.length() > 1 && !args.get(1).is_undefined() { args.get(1).number_value(scope).unwrap_or(1.0) } else { 1.0 };
            let src = if name == "addFromString" {
                format!("data:application/xml,{}", percent_encode(&text))
            } else {
                let base = url::Url::parse(&crate::core::dom::document::base_url(scope)).ok();
                url::Url::options().base_url(base.as_ref()).parse(&text).map(|u| u.to_string()).unwrap_or(text)
            };
            let Some(g) = mint(scope, "SpeechGrammar") else { return };
            let s = str_val(scope, &src);
            mset(scope, g, "src", s);
            let w = v8::Number::new(scope, weight as f32 as f64).into();
            mset(scope, g, "weight", w);
            let items = mget(scope, this, "items").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
            let items = match items {
                Some(a) => a,
                None => {
                    let a = v8::Array::new(scope, 0);
                    mset(scope, this, "items", a.into());
                    a
                }
            };
            let n = items.length();
            items.set_index(scope, n, g.into());
            define_index(scope, this, n, g.into());
        }
        ("ImageDecoder", "isTypeSupported") => {
            let t = args.get(0).to_rust_string_lossy(scope).to_ascii_lowercase();
            const TYPES: &[&str] = &["image/jpeg", "image/jpg", "image/pjpeg", "image/png", "image/apng", "image/gif", "image/webp", "image/bmp", "image/x-xbitmap", "image/x-icon", "image/vnd.microsoft.icon", "image/avif"];
            let b = v8::Boolean::new(scope, TYPES.contains(&t.as_str())).into();
            let p = promise(scope, b);
            rv.set(p);
        }
        ("Origin", "from") => {
            let v = args.get(0);
            let parsed = if v.is_string() {
                let raw = v.to_rust_string_lossy(scope);
                url::Url::parse(&raw).ok()
            } else {
                None
            };
            let Some(u) = parsed else {
                throw_type_error(scope, "Failed to execute 'from' on 'Origin': An origin cannot be extracted from the given parameter.");
                return;
            };
            let Some(o) = mint(scope, "Origin") else { return };
            let opaque = !u.origin().is_tuple();
            let b = v8::Boolean::new(scope, opaque).into();
            mset(scope, o, "opaque", b);
            let s = str_val(scope, &u.origin().ascii_serialization());
            mset(scope, o, "serialized", s);
            rv.set(o.into());
        }
        ("Origin", "isSameOrigin") | ("Origin", "isSameSite") => {
            let other = args.get(0);
            let Ok(oo) = v8::Local::<v8::Object>::try_from(other) else {
                throw_type_error(scope, &format!("Failed to execute '{name}' on 'Origin': parameter 1 is not of type 'Origin'."));
                return;
            };
            let a = mget(scope, this, "serialized").map(|v| v.to_rust_string_lossy(scope));
            let b = mget(scope, oo, "serialized").map(|v| v.to_rust_string_lossy(scope));
            // Two opaque origins are the same only if they are the same object.
            let same = match (a, b) {
                (Some(a), Some(b)) => a == b,
                _ => this.strict_equals(oo.into()),
            };
            rv.set_bool(same);
        }
        ("AudioData", "allocationSize") => {
            let present = v8::Local::<v8::Object>::try_from(args.get(0)).ok().and_then(|o| get_prop(scope, o, "planeIndex")).is_some_and(|v| !v.is_undefined());
            if !present {
                throw_type_error(scope, "Failed to execute 'allocationSize' on 'AudioData': Failed to read the 'planeIndex' property from 'AudioDataCopyToOptions': Required member is undefined.");
                return;
            }
            let init = mget(scope, this, "init").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
            let frames = init.and_then(|d| get_prop(scope, d, "numberOfFrames")).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
            rv.set_double(frames * 4.0);
        }
        ("AudioData", "clone") => {
            let Some(o) = mint(scope, "AudioData") else { return };
            for k in ["format", "sampleRate", "numberOfFrames", "numberOfChannels", "timestamp"] {
                if let Some(v) = crate::core::events::event::evt_slot_read(scope, this, k) {
                    crate::core::events::event::evt_slot_write(scope, o, k, v);
                }
            }
            for k in ["duration", "init"] {
                if let Some(v) = mget(scope, this, k) {
                    mset(scope, o, k, v);
                }
            }
            rv.set(o.into());
        }
        // `addModule(moduleURL)`: the URL resolves against the document's base first; one that does not
        // parse rejects SyntaxError "'x' is not a valid URL." (Chrome 150, measured). The module is
        // not evaluated here -- there is no worklet global scope -- and the promise resolves.
        ("Worklet", "addModule") => {
            let raw = args.get(0).to_rust_string_lossy(scope);
            let base = url::Url::parse(&crate::core::dom::document::base_url(scope)).ok();
            let p = if url::Url::options().base_url(base.as_ref()).parse(&raw).is_ok() {
                let u = v8::undefined(scope).into();
                promise(scope, u)
            } else {
                match make_dom_exc(scope, &format!("'{raw}' is not a valid URL."), "SyntaxError") {
                    Some(e) => crate::binding::protos::rejected_promise(scope, e),
                    None => return,
                }
            };
            rv.set(p);
        }
        ("Sanitizer", "get") => {
            let cfg = sanitizer_config(scope, this);
            rv.set(cfg);
        }
        ("Sanitizer", "setComments") | ("Sanitizer", "setDataAttributes") => {
            let b = v8::Boolean::new(scope, args.get(0).boolean_value(scope)).into();
            mset(scope, this, name, b);
        }
        // `removeUnsafe()` and the allow/remove calls on the DEFAULT configuration: it is already safe.
        ("Sanitizer", _) => {}
        _ => {}
    }
}

/// `encodeURIComponent`'s set: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is %XX-encoded (UTF-8).
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

const SANITIZER_DEFAULT: &str = include_str!("../../gen/blink/sanitizer/builtins_default_config.json");

/// Blink's `Sanitizer::get()` for the default configuration: the dictionary's members in WebIDL's
/// (lexicographic) order, each element / attribute entry `{name, namespace}` (attributes per element
/// when it has any), lists sorted as Blink sorts them.
fn sanitizer_config<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> v8::Local<'s, v8::Value> {
    let cfg: serde_json::Value = serde_json::from_str(SANITIZER_DEFAULT).unwrap_or(serde_json::Value::Null);
    let out = v8::Object::new(scope);
    let name_ns = |scope: &mut v8::PinScope<'s, '_>, e: &serde_json::Value| -> v8::Local<'s, v8::Object> {
        let o = v8::Object::new(scope);
        let n = str_val(scope, e.get("name").and_then(|x| x.as_str()).unwrap_or(""));
        let k = str_val(scope, "name");
        o.set(scope, k, n);
        let ns: v8::Local<v8::Value> = match e.get("namespace").and_then(|x| x.as_str()) {
            Some(s) => str_val(scope, s),
            None => v8::null(scope).into(),
        };
        let k = str_val(scope, "namespace");
        o.set(scope, k, ns);
        o
    };
    let sort_key = |e: &serde_json::Value| (e.get("namespace").and_then(|x| x.as_str()).unwrap_or("").to_string(), e.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string());
    let mut attrs: Vec<serde_json::Value> = cfg.get("attributes").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    attrs.sort_by_key(sort_key);
    let items: Vec<v8::Local<v8::Value>> = attrs.iter().map(|e| name_ns(scope, e).into()).collect();
    let a = v8::Array::new_with_elements(scope, &items);
    let k = str_val(scope, "attributes");
    out.set(scope, k, a.into());
    for (key_name, slot) in [("comments", "setComments"), ("dataAttributes", "setDataAttributes")] {
        let v = mget(scope, this, slot).unwrap_or_else(|| v8::Boolean::new(scope, cfg.get(key_name).and_then(|x| x.as_bool()).unwrap_or(false)).into());
        let k = str_val(scope, key_name);
        out.set(scope, k, v);
    }
    let mut els: Vec<serde_json::Value> = cfg.get("elements").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    els.sort_by_key(sort_key);
    let mut items: Vec<v8::Local<v8::Value>> = Vec::new();
    for e in &els {
        let o = v8::Object::new(scope);
        let mut ats: Vec<serde_json::Value> = e.get("attributes").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        ats.sort_by_key(sort_key);
        let al: Vec<v8::Local<v8::Value>> = ats.iter().map(|x| name_ns(scope, x).into()).collect();
        let aa = v8::Array::new_with_elements(scope, &al);
        let k = str_val(scope, "attributes");
        o.set(scope, k, aa.into());
        let base = name_ns(scope, e);
        for f in ["name", "namespace"] {
            let k = str_val(scope, f);
            if let Some(v) = base.get(scope, k) {
                o.set(scope, k, v);
            }
        }
        items.push(o.into());
    }
    let a = v8::Array::new_with_elements(scope, &items);
    let k = str_val(scope, "elements");
    out.set(scope, k, a.into());
    out.into()
}

/// The indexed properties of a list Blink backs with an indexed getter (`TouchList`,
/// `SpeechGrammarList`): own, read-only, enumerable, configurable -- `getOwnPropertyDescriptor`'s answer.
fn define_index(scope: &mut v8::PinScope, list: v8::Local<v8::Object>, i: u32, v: v8::Local<v8::Value>) {
    let k: v8::Local<v8::Name> = v8::String::new(scope, &i.to_string()).unwrap().into();
    let attr = v8::PropertyAttribute::READ_ONLY;
    let _ = list.define_own_property(scope, k, v, attr);
}

/// A `TouchList` over `touches` (TouchEvent's three lists, `touch_event.rs`): a snapshot of the
/// sequence the init passed.
pub(crate) fn new_touch_list<'s>(scope: &mut v8::PinScope<'s, '_>, touches: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Value> {
    let Some(l) = mint(scope, "TouchList") else { return touches };
    let items: Vec<v8::Local<v8::Value>> = match v8::Local::<v8::Array>::try_from(touches) {
        Ok(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect(),
        Err(_) => Vec::new(),
    };
    for (i, t) in items.iter().enumerate() {
        define_index(scope, l, i as u32, *t);
    }
    let arr = v8::Array::new_with_elements(scope, &items);
    mset(scope, l, "items", arr.into());
    l.into()
}

/// `ChapterInformation` objects from a MediaMetadataInit's `chapterInfo` (`media_session.rs`).
pub(crate) fn new_chapter<'s>(scope: &mut v8::PinScope<'s, '_>, title: &str, start: f64, artwork: &[v8::Local<'s, v8::Value>]) -> Option<v8::Local<'s, v8::Object>> {
    let c = mint(scope, "ChapterInformation")?;
    let t = str_val(scope, title);
    mset(scope, c, "title", t);
    let s = v8::Number::new(scope, start).into();
    mset(scope, c, "startTime", s);
    let a = v8::Array::new_with_elements(scope, artwork);
    let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    mset(scope, c, "artwork", a.into());
    Some(c)
}

/// The kit body: `cfg` is unused; the row's `Interface.member` routes it.
pub fn install_misc(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let d = str_val(scope, &format!("{iface}.{}", member.name));
    match member.kind {
        crate::install::MemberKind::Attr { readonly } | crate::install::MemberKind::StaticAttr { readonly } => {
            if member.name == "size" && iface.ends_with("QueuingStrategy") {
                let n = str_val(scope, iface);
                crate::core::dom::shared::row_getter_data(scope, proto, member, cb_strategy_size_get, n);
                return;
            }
            let g = build_fn_data(scope, cb_slot_get, &format!("get {}", member.name), 0, d);
            let s = if readonly {
                None
            } else {
                let d = str_val(scope, &format!("{iface}.{}", member.name));
                Some(build_fn_data(scope, cb_slot_set, &format!("set {}", member.name), 1, d))
            };
            crate::install::kits::define_accessor(scope, proto, member, g, s);
        }
        _ => crate::core::dom::shared::row_method_data(scope, proto, member, cb_method, d),
    }
}


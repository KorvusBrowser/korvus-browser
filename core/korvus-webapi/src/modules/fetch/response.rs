//! `modules/fetch/response.rs` -- the `Response` table.
//!
//! The CONSTRUCTOR is ported: `shared-globals.js`'s `class Response` is gone, the nine readonly
//! attributes are filled from Rust into the same private slots, and the body lives in a slot too
//! rather than in an own `__body` property. Measured with `tests/fixtures/fetch-types-probe.js` on
//! `google-chrome --headless=new` 150.0.7871.46:
//!
//! * `Object.getOwnPropertyNames(new Response("x"))` was `["__body"]`; Chrome's is `[]`.
//! * `new Response("x").type` was `"basic"`; Chrome answers `"default"` for a CONSTRUCTED response and
//!   keeps `"basic"` for one a same-origin fetch produced. Both now.
//! * `headers` was a `Map`, so `r.headers instanceof Headers` was false. It is the native `Headers`.
//! * every accessor answered `undefined` for a foreign receiver, where Chrome throws
//!   `Illegal invocation`; every promise-returning method REJECTS instead ("Failed to execute 'text'
//!   on 'Response': Illegal invocation"), and `clone` throws. All three measured.
//!
//! `formData`, `textStream` and two of the three statics (`json`, `redirect`) still have no implementation
//! and are honest D2 `stub` rows -- what the registry gave them; `error` is implemented. The body is real: see `body.rs`.

use deno_core::v8;
use crate::binding::protos::{get_prop, new_global, str_val, throw_range_error, throw_type_error};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::webidl::{brand, branded_getter_row, branded_method_row, branded_promise_method_row, init_dict, require_new};
use crate::install::MemberDef;
use super::body::{self, Extracted, Kind};

// THE BODY IS A STREAM OF BYTES. `text`/`json`/`arrayBuffer`/`bytes`/`blob` all read the
// `body` slot's `ReadableStream` through `body::consume` (the Body mixin, shared with `Request`), and
// `bodyUsed` is that stream's `disturbed`. The `fetchBody` string slot and `body_bytes` -- whose own
// comment said "for a genuinely binary body the loss happened upstream in transport" -- are gone: the
// transport keeps bytes, `op_fetch` hands them over as a `Uint8Array`, and nothing decodes them until a
// consumer that is supposed to (`text()` is always UTF-8, per Fetch).

fn cb_response_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Response", Kind::Text);
    rv.set(p);
}
/// `Response.textStream()` (Chrome 152, the Body mixin's: [`body::text_stream`]).
fn cb_response_text_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(s) = body::text_stream(scope, args.this(), "Response") {
        rv.set(s);
    }
}
fn cb_response_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Response", Kind::Json);
    rv.set(p);
}
/// `Response.arrayBuffer()` -- measured on Chrome 150 for `fetch` of the bytes `0..=255`:
/// `byteLength` 256, `u[128] === 128`. This engine answered 512 and 239 (the UTF-8 of U+FFFD).
fn cb_response_array_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Response", Kind::ArrayBuffer);
    rv.set(p);
}
/// `Response.bytes()` -- the same bytes as a `Uint8Array` (Chrome 151 ships it; the capture has the row).
fn cb_response_bytes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Response", Kind::Bytes);
    rv.set(p);
}
/// `Response.blob()` -- typed by the response's own `Content-Type`, ASCII-lowercased (Chrome:
/// `Text/HTML; Charset=UTF-8` -> `text/html; charset=utf-8`).
fn cb_response_blob(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Response", Kind::Blob);
    rv.set(p);
}

/// The nine readonly attributes the constructor fills into private slots, in the table's order.
/// `clone()` copies these; `headers` is copied into a NEW `Headers` and `body` is teed.
/// `netMimeType` (a network response's MIME type, A/B C5) is not an attribute, and is copied too.
const SLOTS: &[&str] = &["type", "url", "redirected", "status", "ok", "statusText", "headers", "body", "bodyUsed", "netMimeType"];

/// `Response.clone()` -- a second Response over a TEE of the body.
///
/// NOT built through `new Response(body, init)`: the public constructor cannot carry `url`, `type` or
/// `redirected`, and Chrome's clone keeps all three. It mints an object on the same prototype and copies
/// the slots; the body is teed (the original keeps one branch -- Chrome: `b === r.body` is false after
/// `clone()`), and a disturbed or locked body is Chrome's TypeError.
fn cb_response_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if body::body_unusable(scope, this) {
        throw_type_error(scope, "Failed to execute 'clone' on 'Response': Response body is already used");
        return;
    }
    let Some(proto) = crate::binding::protos::iface_object(scope, "Response")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    let out = crate::binding::protos::mint_on(scope, proto.into());
    for name in SLOTS {
        match *name {
            "body" => {
                let b = body::tee_body(scope, this);
                evt_slot_write(scope, out, "body", b);
            }
            "headers" => {
                if let Some(h) = evt_slot_read(scope, this, "headers") {
                    // the list as it is, spelling kept (Blink's `Headers::Clone`), never through the page's iterator
                    let copy = super::headers::headers_clone(scope, h).unwrap_or(h);
                    // A clone keeps its original's guard: a fetched response's clone is immutable
                    // too, and a constructed response's clone is `response` (CONF GUARD1 -- this
                    // used to carry only the `immutable` bit, so a clone of `new Response("")` was
                    // guard `none` and took a `set-cookie`; measured, Chrome drops it).
                    let g = super::headers::guard_of(scope, h);
                    super::headers::set_guard(scope, copy, g);
                    evt_slot_write(scope, out, "headers", copy);
                }
            }
            _ => {
                if let Some(v) = evt_slot_read(scope, this, name) {
                    evt_slot_write(scope, out, name, v);
                }
            }
        }
    }
    brand(scope, out, "Response");
    rv.set(out.into());
}

fn cb_response_body_used(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    body::cb_body_used(scope, args, rv);
}

/// A null body status (Fetch): 101, 103, 204, 205, 304.
fn null_body_status(status: f64) -> bool {
    matches!(status as u32, 101 | 103 | 204 | 205 | 304)
}

/// Write the nine slots of a Response.
#[allow(clippy::too_many_arguments)]
fn fill<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<'s, v8::Object>,
    ty: &str,
    url: v8::Local<'s, v8::Value>,
    status: f64,
    status_text: v8::Local<'s, v8::Value>,
    headers: v8::Local<'s, v8::Value>,
    body_v: v8::Local<'s, v8::Value>,
) {
    let tyv = str_val(scope, ty);
    evt_slot_write(scope, this, "type", tyv);
    evt_slot_write(scope, this, "url", url);
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    evt_slot_write(scope, this, "redirected", f);
    let stv: v8::Local<v8::Value> = v8::Number::new(scope, status).into();
    evt_slot_write(scope, this, "status", stv);
    let ok: v8::Local<v8::Value> = v8::Boolean::new(scope, (200.0..300.0).contains(&status)).into();
    evt_slot_write(scope, this, "ok", ok);
    evt_slot_write(scope, this, "statusText", status_text);
    evt_slot_write(scope, this, "headers", headers);
    evt_slot_write(scope, this, "body", body_v);
    brand(scope, this, "Response");
}

/// The `Response` a NETWORK fetch resolves with, from `op_fetch`'s record `{status, statusText, url,
/// headers, body: Uint8Array}`: `type` "basic", the fetched url and status, a real `Headers` over the
/// response's header list, and a body stream over the exact bytes -- a stream for every status (Chrome
/// 150: `fetch` of a 204 has `body` `[object ReadableStream]`).
///
/// This used to be `new Response(record)` with the constructor sniffing "an object with `status` and
/// `body` and no `then`" to decide it was internal -- so a PAGE calling `new Response({status: 1, body:
/// 2})` got a network response's shape instead of the text "[object Object]".
pub(crate) fn response_from_network<'s>(scope: &mut v8::PinScope<'s, '_>, record: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let rec = v8::Local::<v8::Object>::try_from(record).ok()?;
    let this = new_global(scope, "Response", &[])?;
    let status = get_prop(scope, rec, "status").and_then(|v| v.number_value(scope)).unwrap_or(200.0);
    let stx = get_prop(scope, rec, "statusText").unwrap_or_else(|| str_val(scope, ""));
    // `Response.url` is the response's url SERIALIZED WITH THE FRAGMENT EXCLUDED (Fetch). The record's
    // url went in verbatim, so `fetch(u + "#ada").url` answered `…/#ada`; Chrome 150, measured:
    // `fetch("/resp.html#ada")` answers `http://127.0.0.1:P/resp.html`.
    let url = match get_prop(scope, rec, "url") {
        Some(v) => {
            let raw = v.to_rust_string_lossy(scope);
            match url::Url::parse(&raw) {
                Ok(mut u) => {
                    u.set_fragment(None);
                    str_val(scope, u.as_str())
                }
                Err(_) => v,
            }
        }
        None => str_val(scope, ""),
    };
    let h = get_prop(scope, rec, "headers").unwrap_or_else(|| v8::undefined(scope).into());
    let headers_obj = new_global(scope, "Headers", &[h]);
    // A fetched response's headers are IMMUTABLE (measured: "Failed to execute 'set' on 'Headers':
    // Headers are immutable").
    if let Some(ho) = headers_obj {
        super::headers::set_guard(scope, ho.into(), super::headers::Guard::Immutable);
    }
    let headers = headers_obj.map(|h| -> v8::Local<v8::Value> { h.into() }).unwrap_or_else(|| v8::undefined(scope).into());
    // The response's TYPE, from the transport's tainting. An opaque / opaque-redirect
    // response has a NULL body (the record's `body` is null), which is what makes it unreadable:
    // `text()` answers "" and `bodyUsed` stays false (measured).
    let ty = get_prop(scope, rec, "type").filter(|t| t.is_string()).map(|t| t.to_rust_string_lossy(scope)).unwrap_or_else(|| "basic".to_string());
    let redirected = get_prop(scope, rec, "redirected").map(|v| v.boolean_value(scope)).unwrap_or(false);
    // Chrome 150's `fetch` of a 204 answers `body` as a ReadableStream (empty), not null -- the null
    // body status rule is the CONSTRUCTOR's, measured both ways.
    let pending = get_prop(scope, rec, "bodyId").filter(|v| v.is_number()).and_then(|v| v.uint32_value(scope));
    let body_v: v8::Local<v8::Value> = if matches!(ty.as_str(), "opaque" | "opaqueredirect") {
        v8::null(scope).into()
    } else if let Some(id) = pending {
        // Only the HEAD is in: the body is a stream still arriving.
        body::pending_network_stream(scope, id).into()
    } else {
        match get_prop(scope, rec, "body").filter(|b| b.is_array_buffer_view()) {
            Some(u8) => body::stream_over_u8(scope, u8).into(),
            None => {
                // A body handed over as a string (a Service Worker's `respondWith` text): its UTF-8.
                let text = get_prop(scope, rec, "body").filter(|b| !b.is_null_or_undefined()).map(|b| b.to_rust_string_lossy(scope)).unwrap_or_default();
                let u8 = body::u8_array(scope, text.into_bytes());
                body::stream_over_u8(scope, u8).into()
            }
        }
    };
    fill(scope, this, &ty, url, status, stx, headers, body_v);
    // A NETWORK response's MIME type is the network stack's (`FetchResponseData::InitFromResourceResponse`:
    // `SetMimeType(response.MimeType())`) -- every `Content-Type` value read, the last valid one winning, no
    // parameters -- and it types `blob()` (A/B C5). A constructed `Response` has none and reads its header list.
    let net_mime = transport::mime::mime_type_and_charset(&crate::core::html::isolation_policy::headers_from_js(scope, h)).mime;
    let mv = str_val(scope, &net_mime);
    evt_slot_write(scope, this, "netMimeType", mv);
    if redirected {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        evt_slot_write(scope, this, "redirected", t);
    }
    Some(this)
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

/// `new Response(optional BodyInit? body = null, optional ResponseInit init = {})` — both optional, so
/// `length` is 0 and `new Response()` is a real 200 response (measured).
///
/// Measured on Chrome 150: `type` "default", `url` "", the body extracted to a stream of bytes with the
/// Content-Type its type implies (added only when `init.headers` has none), `status` outside
/// `[200, 599]` a RangeError, and a body on a null body status a TypeError.
pub(crate) fn cb_response_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "Response") {
        return;
    }
    let this = args.this();
    let Ok(extracted) = body::extract(scope, args.get(0)) else { return };
    let Ok(init) = init_dict(scope, &args, 1, "Response", "ResponseInit") else { return };
    let status = match init.and_then(|d| get_prop(scope, d, "status")).filter(|v| !v.is_undefined()) {
        Some(v) => match v.number_value(scope) {
            Some(n) => n,
            None => return,
        },
        None => 200.0,
    };
    // `unsigned short`: ToUint16 (truncate, wrap) before the range check.
    let status = if status.is_finite() { status.trunc().rem_euclid(65536.0) } else { 0.0 };
    if !(200.0..=599.0).contains(&status) {
        throw_range_error(scope, &format!("Failed to construct 'Response': The status provided ({status}) is outside the range [200, 599]."));
        return;
    }
    // `statusText` is a `ByteString` that must match HTTP's `reason-phrase` production. The ctor had
    // the status RANGE check beside this and NEITHER of these, so `{statusText: "\n"}` was stored --
    // which is why the two `response-error.any.js` rows read as an oversight rather than a ruling.
    // Both messages measured on Chrome 150 and both are in
    // `third_party/blink/renderer/core/fetch/response.cc` (`IsValidReasonPhrase`, :119; the throw at
    // :254).
    let stx = match init.and_then(|d| get_prop(scope, d, "statusText")).filter(|v| !v.is_undefined()) {
        Some(v) => {
            let Some(s) = v.to_string(scope) else { return };
            // The UTF-16 code units, not the lossy UTF-8: a lone surrogate or a code point above
            // U+00FF has to be SEEN here, and `to_rust_string_lossy` has already replaced it.
            let mut units = vec![0u16; s.length()];
            s.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
            // The WebIDL `ByteString` conversion first: a code point above U+00FF has no byte.
            if units.iter().any(|c| *c > 0xFF) {
                throw_type_error(
                    scope,
                    "Failed to construct 'Response': Failed to read the 'statusText' property from 'ResponseInit': String contains non ISO-8859-1 code point.",
                );
                return;
            }
            // `reason-phrase = *( HTAB / SP / VCHAR / obs-text )`.
            if !units.iter().all(|c| *c == 0x09 || (0x20..=0x7E).contains(c) || (0x80..=0xFF).contains(c)) {
                throw_type_error(scope, "Failed to construct 'Response': Invalid statusText");
                return;
            }
            s.into()
        }
        None => str_val(scope, ""),
    };
    let h = init.and_then(|d| get_prop(scope, d, "headers")).unwrap_or_else(|| v8::undefined(scope).into());
    let Some(headers) = new_global(scope, "Headers", &[h]) else { return };
    // THE GUARD (CONF GUARD1): a constructed `Response`'s header list is `response`, which drops
    // `set-cookie` and `set-cookie2` and nothing else. Stamped before the body's `Content-Type` is
    // appended below -- neither name is forbidden here, so the order is not observable for this
    // guard, but it is the same order the `Request` constructor needs and is written to match.
    super::headers::set_guard(scope, headers.into(), super::headers::Guard::Response);
    if !matches!(extracted, Extracted::Null) && null_body_status(status) {
        body::throw_ctor(scope, "Response", "Response with null body status cannot have body");
        return;
    }
    if let Extracted::Bytes(_, Some(ct)) = &extracted {
        let k = str_val(scope, "content-type");
        let has = crate::binding::protos::call_method(scope, headers.into(), "has", &[k]).map(|v| v.boolean_value(scope)).unwrap_or(false);
        if !has {
            let v = str_val(scope, ct);
            let _ = crate::binding::protos::call_method(scope, headers.into(), "append", &[k, v]);
        }
    }
    let body_v = body::body_stream(scope, &extracted);
    let empty = str_val(scope, "");
    fill(scope, this, "default", empty, status, stx, headers.into(), body_v);
    rv.set(this.into());
}

/// `Response.error()` -- a network-error response: `type "error"`, status 0, an empty immutable header
/// list, a null body (Chrome 150: `[r.type, r.status]` is `["error", 0]`; this engine threw, the row
/// was a D2 stub).
fn cb_response_error(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = new_global(scope, "Response", &[]) else { return };
    let Some(h) = new_global(scope, "Headers", &[]) else { return };
    super::headers::set_guard(scope, h.into(), super::headers::Guard::Immutable);
    let empty = str_val(scope, "");
    let empty2 = str_val(scope, "");
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    fill(scope, this, "error", empty, 0.0, empty2, h.into(), null);
    rv.set(this.into());
}

/// Construct a `Response` through the constructor itself -- the SAME `ResponseInit` validation -- and
/// rethrow its error under `method`'s prefix: Blink's `Response::json` / `redirect` run the constructor's
/// steps and report as the static ("Failed to execute 'json' on 'Response': Response with null body status
/// cannot have body"). `None` = it threw (rethrown).
fn construct_as<'s>(scope: &mut v8::PinScope<'s, '_>, method: &str, args: &[v8::Local<'s, v8::Value>]) -> Option<v8::Local<'s, v8::Object>> {
    // The constructor's error, caught -- then rethrown OUTSIDE the catch scope (a throw inside it would
    // be caught by it again).
    let caught: Result<v8::Global<v8::Object>, (String, bool)> = {
        let tc = std::pin::pin!(v8::TryCatch::new(scope));
        let tc = &mut tc.init();
        match new_global(tc, "Response", args) {
            Some(o) => Ok(v8::Global::new(tc, o)),
            None => {
                let m = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_default();
                let range = m.starts_with("RangeError");
                let msg = m.split_once(": ").map(|(_, r)| r.to_string()).unwrap_or(m);
                Err((msg.strip_prefix("Failed to construct 'Response': ").unwrap_or(&msg).to_string(), range))
            }
        }
    };
    match caught {
        Ok(g) => Some(v8::Local::new(scope, g)),
        Err((msg, range)) => {
            let full = format!("Failed to execute '{method}' on 'Response': {msg}");
            if range {
                throw_range_error(scope, &full);
            } else {
                throw_type_error(scope, &full);
            }
            None
        }
    }
}

/// `Response.json(data, init)` (the Chrome oracle ratchet; it was a stub answering null):
/// `data` serialized as `JSON.stringify` serializes it -- `undefined`, a function or a Symbol is "The
/// data is not JSON serializable" -- as the body of a response the constructor validates `init` for,
/// with `Content-Type: application/json` unless `init` named one.
fn cb_response_static_json<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let data = args.get(0);
    let text = if data.is_undefined() || data.is_symbol() || data.is_function() {
        None
    } else {
        match v8::json::stringify(scope, data) {
            Some(t) => Some(t),
            None => return, // a toJSON or a BigInt threw: propagate it
        }
    };
    let Some(text) = text else {
        throw_type_error(scope, "Failed to execute 'json' on 'Response': The data is not JSON serializable");
        return;
    };
    // The bytes as a Uint8Array, so the constructor adds no `text/plain` content type of its own.
    let bytes = text.to_rust_string_lossy(scope).into_bytes();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    let len = ab.byte_length();
    let Some(u8a) = v8::Uint8Array::new(scope, ab, 0, len) else { return };
    let init = args.get(1);
    let Some(r) = construct_as(scope, "json", &[u8a.into(), init]) else { return };
    let Some(h) = get_prop(scope, r, "headers") else { return };
    let k = str_val(scope, "content-type");
    let has = crate::binding::protos::call_method(scope, h, "has", &[k]).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if !has {
        let v = str_val(scope, "application/json");
        let _ = crate::binding::protos::call_method(scope, h, "append", &[k, v]);
    }
    rv.set(r.into());
}

/// `Response.redirect(url, status = 302)` (it was a stub answering null): `url` parsed against
/// the realm's base URL (TypeError "Failed to parse URL from x"), `status` one of 301 302 303 307 308
/// (RangeError "Invalid status code"), a null body, and an IMMUTABLE header list holding `Location`.
fn cb_response_redirect<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(raw) = args.get(0).to_string(scope) else { return };
    let raw = raw.to_rust_string_lossy(scope);
    let base = url::Url::parse(&crate::core::html::browsing_context::realm_base_url(scope)).ok();
    let parsed = match super::request::parse_with_base(base.as_ref(), &raw) {
        Ok(Some(u)) => u,
        _ => {
            throw_type_error(scope, &format!("Failed to execute 'redirect' on 'Response': Failed to parse URL from {raw}"));
            return;
        }
    };
    let status = if args.length() < 2 || args.get(1).is_undefined() {
        302.0
    } else {
        let Some(n) = args.get(1).number_value(scope) else { return };
        if n.is_finite() { n.trunc().rem_euclid(65536.0) } else { 0.0 }
    };
    if ![301.0, 302.0, 303.0, 307.0, 308.0].contains(&status) {
        throw_range_error(scope, "Failed to execute 'redirect' on 'Response': Invalid status code");
        return;
    }
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let init = crate::binding::protos::init_dict(scope);
    let sk = str_val(scope, "status");
    let sv: v8::Local<v8::Value> = v8::Number::new(scope, status).into();
    init.set(scope, sk, sv);
    let Some(r) = construct_as(scope, "redirect", &[null, init.into()]) else { return };
    let Some(h) = get_prop(scope, r, "headers") else { return };
    let k = str_val(scope, "location");
    let v = str_val(scope, parsed.as_str());
    let _ = crate::binding::protos::call_method(scope, h, "append", &[k, v]);
    super::headers::set_guard(scope, h, super::headers::Guard::Immutable);
    rv.set(r.into());
}

/// The bodies the implemented rows of [`RESPONSE`] name.
pub fn install_response(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // The five PROMISE-returning rows REJECT for a foreign receiver; `clone` returns a value and
        // throws. Both measured on Chrome 150 -- a promise-returning WebIDL operation never throws
        // synchronously, and a page's `.catch` is written for the rejection.
        "text" => branded_promise_method_row(scope, proto, member, cb_response_text),
        "json" => branded_promise_method_row(scope, proto, member, cb_response_json),
        "arrayBuffer" => branded_promise_method_row(scope, proto, member, cb_response_array_buffer),
        "bytes" => branded_promise_method_row(scope, proto, member, cb_response_bytes),
        "blob" => branded_promise_method_row(scope, proto, member, cb_response_blob),
        "clone" => branded_method_row(scope, proto, member, cb_response_clone),
        "textStream" => branded_method_row(scope, proto, member, cb_response_text_stream),
        "bodyUsed" => branded_getter_row(scope, proto, member, cb_response_body_used),
        "error" => crate::install::live::rows::method_row(scope, proto, member, cb_response_error),
        "redirect" => crate::install::live::rows::method_row(scope, proto, member, cb_response_redirect),
        other => debug_assert!(false, "install_response: unexpected member {other}"),
    }
}

/// The STATIC `json` (the prototype has a `json` too, so it needs its own installer name).
pub fn install_response_static(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::install::live::rows::method_row(scope, proto, member, cb_response_static_json);
}

// Rows VERBATIM from the `Response` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: nine readonly attributes over the
// instance's internal slots (`body` is the nullable one), eight methods, three statics. `[value,
// both]`: both captures record `newable: true` and the shared-globals class is the constructor.
//
// NOTE the `json` name appears twice, once as a prototype method and once as a static -- that is the
// capture, and the two land on different holders.
webapi::interface! {
    "Response" [value, both] {
        ctor          "Response"                          len 0 = stub,
        attr          "type"        : Object readonly           = evt_get,
        attr          "url"         : Object readonly           = evt_get,
        attr          "redirected"  : Object readonly           = evt_get,
        attr          "status"      : Object readonly           = evt_get,
        attr          "ok"          : Object readonly           = evt_get,
        attr          "statusText"  : Object readonly           = evt_get,
        attr          "headers"     : Object readonly           = evt_get,
        attr          "body"        : Object readonly nullable  = evt_get_null,
        attr          "bodyUsed"    : Object readonly           = install_response,
        method        "arrayBuffer" : Promise len 0             = install_response,
        method        "blob"        : Promise len 0             = install_response,
        method        "clone"       : Object len 0              = install_response,
        method        "formData"    : Promise len 0             = stub,
        method        "json"        : Promise len 0             = install_response,
        method        "text"        : Promise len 0             = install_response,
        method        "bytes"       : Promise len 0             = install_response,
        method        "textStream"  : Object len 0              = install_response,
        static_method "error"       : Object len 0              = install_response,
        static_method "json"        : Object len 1 required 1    = install_response_static,
        static_method "redirect"    : Object len 1 required 1    = install_response
    }
}

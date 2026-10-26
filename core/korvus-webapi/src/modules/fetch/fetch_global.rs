//! `modules/fetch/fetch_global.rs` — the `fetch` global.
//!
//! Was `globalThis.fetch = (url, opts) => …` in `core/korvus-runtime/src/js/shared-globals.js`, plus
//! `host.js`'s `setupSwFetch`, which REPLACED that global with a second arrow whenever a
//! ServiceWorker registered. Two definitions of one global, in two files, with the second silently
//! shadowing the first. It is one native function now, and the Service-Worker interception is a branch
//! inside it rather than a wrapper around it.
//!
//! MEASURED on `google-chrome --headless=new` 150.0.7871.46, which is what the native body fixes:
//!
//! * `fetch.length` was **2** (the arrow's two declared parameters); Chrome's is 1.
//! * `fetch()` THREW `TypeError: Failed to execute 'fetch' on 'Window': 1 argument required, but only
//!   0 present.` -- right message, wrong shape. `fetch` is a promise-returning operation, so WebIDL
//!   turns every one of its input failures into a REJECTION; Chrome rejects with that exact sentence
//!   and a page's `.catch` is what reads it. Same for `fetch.call({}, u)` ("Illegal invocation"),
//!   `fetch(Symbol())` ("Cannot convert a Symbol value to a string") and a reserved `mode`.
//! * `fetch.call({}, u)` was accepted outright: there was no receiver check at all.
//!
//! THE SERVICE-WORKER INTERCEPTION IS UNCHANGED IN MECHANISM and is now the one place that knows it
//! exists. Once a SW controls the page, the request is published on the `korvus-sw-fetch`
//! BroadcastChannel keyed by a serial id, the SW's `fetch` handler answers on `korvus-sw-fetch-resp`,
//! and the page's promise resolves with a `Response` over that body -- or falls back to the NETWORK
//! when the handler did not `respondWith` (a null body). `tests/worker.rs::
//! service_worker_fetch_interception` drives a real SW isolate through `respondWith` and asserts the
//! page's `fetch("/data")` resolves with the SW's body; it is the gate on this path.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use deno_core::v8;

use crate::binding::protos::{
    build_fn, build_fn_data, get_prop, global_get, new_global, rejected_type_error, set_prop, str_val,
};
use crate::core::xmlhttprequest::xml_http_request::deno_op;

/// The three `mode` values a PAGE may ask for. "navigate" and "websocket" are reserved for the
/// browser's own requests; a page that passed one would be dressing an ordinary fetch up as a
/// navigation, which is the exact claim Fetch Metadata exists to let a server disbelieve. Chrome
/// rejects rather than falling back, and names the member.
const PAGE_MODES: [&str; 3] = ["cors", "no-cors", "same-origin"];

/// The page's Service-Worker fetch bridge, per realm (`OpState`).
#[derive(Default)]
pub(crate) struct SwFetchBridge {
    /// Set by [`activate_sw_fetch`], i.e. by the first `navigator.serviceWorker.register` -- it was
    /// `op_sw_fetch_activate` until that transitional op was deleted. Before
    /// that every fetch is a straight network fetch and none of the rest of this exists.
    active: Cell<bool>,
    /// Set when a service worker CLAIMS this page: only then is the page a controlled
    /// client whose fetches go to the worker. Registering does not do it -- Chrome 150
    /// (`U-sw-control-hang`): the registering page's fetches go to the network while its worker answers
    /// every fetch with a promise that never settles, and tldraw.com's never left the engine.
    controlled: Cell<bool>,
    /// `new BroadcastChannel("korvus-sw-fetch")` -- the page -> SW half.
    req_channel: RefCell<Option<v8::Global<v8::Object>>>,
    /// id -> the resolver of the promise `fetch` handed the page.
    pending: RefCell<HashMap<u32, v8::Global<v8::PromiseResolver>>>,
    /// id -> the request record, kept so a handler that does NOT `respondWith` can fall back to the
    /// network with the same request the page asked for.
    fallback: RefCell<HashMap<u32, v8::Global<v8::Object>>>,
    next: Cell<u32>,
}

fn with_bridge<R>(scope: &mut v8::PinScope, f: impl FnOnce(&SwFetchBridge) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<SwFetchBridge>().is_none() {
        st.put(SwFetchBridge::default());
    }
    f(st.borrow::<SwFetchBridge>())
}

// ── the request record ──────────────────────────────────────────────────────────────────────────

/// Build the `{url, method, headers, body, mode}` record `op_fetch` takes — byte-for-byte the object
/// the deleted arrow built. `Err(())` means a rejection reason is already returned by the caller.
fn request_record<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    input: v8::Local<'s, v8::Value>,
    init: v8::Local<'s, v8::Value>,
    iface: &str,
) -> Result<(v8::Local<'s, v8::Object>, String), String> {
    if input.is_symbol() {
        return Err(format!("Failed to execute 'fetch' on '{iface}': Cannot convert a Symbol value to a string"));
    }
    // `RequestInfo` is `(Request or USVString)`: a Request contributes its url and method.
    let from_request = v8::Local::<v8::Object>::try_from(input)
        .ok()
        .filter(|o| crate::install::webidl::has_brand(scope, (*o).into(), "Request"));
    let url = match from_request {
        Some(o) => get_prop(scope, o, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default(),
        None => match input.to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => return Err(format!("Failed to execute 'fetch' on '{iface}': Cannot convert a Symbol value to a string")),
        },
    };
    // `RequestInit`, converted as WebIDL converts the argument -- before any step of fetch runs, so before the url
    // is parsed (Chrome 150, 2026-10-11: `fetch("ftp:", init)` reads every member of `init`, then
    // rejects). Each member is read once, in WebIDL's order, and converted as it is read; a getter's throw, a bad
    // enum or a `signal` that is no AbortSignal rejects with Chrome's message. korvus read nothing before the url,
    // then its own members in its own order, and let `{signal: {}}` through. The pending exception is the
    // rejection reason (the caller's TryCatch).
    let Ok(init_dict) = crate::install::ctor_dicts::convert_dict(scope, init, "RequestInit", &format!("Failed to execute 'fetch' on '{iface}': ")) else {
        return Err(String::new());
    };
    // `fetch(input)` constructs a Request from `input` first, so a url that cannot be parsed -- or
    // carries credentials -- rejects with the constructor's message under fetch's prefix
    // (Chrome 150's `fetch("ftp:")` is "Failed to execute 'fetch' on
    // 'Window': Failed to parse URL from ftp:"; this went to the network and failed as "Failed to
    // fetch", a probe costco.com's scripts run).
    if from_request.is_none() {
        let base = url::Url::parse(&crate::core::html::browsing_context::realm_base_url(scope)).ok();
        match super::request::parse_with_base(base.as_ref(), &url) {
            Err(()) => return Err(format!("Failed to execute 'fetch' on '{iface}': Failed to parse URL from {url}")),
            Ok(Some(u)) if !u.username().is_empty() || u.password().is_some() => {
                return Err(format!("Failed to execute 'fetch' on '{iface}': Request cannot be constructed from a URL that includes credentials: {url}"))
            }
            _ => {}
        }
    }
    let opts = Some(init_dict);
    let pick = |scope: &mut v8::PinScope<'s, '_>, key: &str| -> Option<v8::Local<'s, v8::Value>> {
        opts.and_then(|o| get_prop(scope, o, key)).filter(|v| !v.is_undefined())
    };
    // THE SAME `RequestInit` CONVERSION THE CONSTRUCTOR RUNS. `fetch()` used to read `method`, `mode`,
    // `credentials` and `redirect` by hand with a bare `to_rust_string_lossy` and one ad-hoc mode
    // check, so `fetch(u, {cache: "BAD"})`, `{method: "TRACE"}`, `{referrer: "http://:not a url"}` and
    // `{cache: "only-if-cached", mode: "cors"}` all went out as if they were valid -- while the
    // constructor, ten lines away in a different file, was supposed to be the same algorithm.
    // `fetch/api/abort/general.any.js` is the file that measures the difference. Chrome's message is
    // this one's with a different prefix, and nothing else (measured on 150).
    let base = url::Url::parse(&crate::core::dom::base_url(scope)).ok();
    let conv = match super::request::convert_init(scope, opts, base.as_ref()) {
        Ok(c) => c,
        // A `toString` threw: its exception is already pending and IS the rejection reason.
        Err(None) => return Err(String::new()),
        Err(Some(msg)) => return Err(format!("Failed to execute 'fetch' on '{iface}': {msg}")),
    };
    // A `Request` input contributes its own method, mode, credentials and redirect when init names
    // none (`fetch(new Request(u, {mode: 'no-cors'}))` went out as `cors`).
    let from_slot = |scope: &mut v8::PinScope<'s, '_>, key: &str| -> Option<String> {
        from_request.and_then(|o| crate::core::events::event::evt_slot_read(scope, o, key)).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
    };
    let member = |scope: &mut v8::PinScope<'s, '_>, key: &str, default: &str| -> String {
        conv.get(key).map(str::to_string).or_else(|| from_slot(scope, key)).unwrap_or_else(|| default.to_string())
    };
    let method = member(scope, "method", "GET");
    // A mode reserved for the browser's own requests cannot reach the wire even by inheritance -- a
    // navigation request handed to `fetch()` is still an ordinary fetch (`PAGE_MODES`).
    let mode = match conv.get("mode") {
        Some(m) => m.to_string(),
        None => from_slot(scope, "mode").filter(|m| PAGE_MODES.contains(&m.as_str())).unwrap_or_else(|| "cors".to_string()),
    };
    let credentials = member(scope, "credentials", "same-origin");
    let redirect = member(scope, "redirect", "follow");
    let cache = member(scope, "cache", "default");
    if let Err(msg) = super::request::check_init_combination(&cache, &mode, &method) {
        return Err(format!("Failed to execute 'fetch' on '{iface}': {msg}"));
    }
    let rec = v8::Object::new(scope);
    // the CONVERTED `signal`, for `fetch_signal` -- which must not read the page's `init` a second time
    let sig = get_prop(scope, init_dict, "signal").unwrap_or_else(|| v8::undefined(scope).into());
    let sig_key = crate::binding::protos::private_key(scope, FETCH_SIGNAL_PRIV);
    rec.set_private(scope, sig_key, sig);
    let u = str_val(scope, &url);
    set_prop(scope, rec, "url", u);
    // the url as the page wrote it -- or as the Request input's constructor saw it -- with Chromium's dangling-markup
    // mark, which `cb_fetch` refuses for http(s)
    let dangling = match from_request {
        Some(o) => super::request::is_dangling(scope, o),
        None => crate::modules::url::url::potentially_dangling_markup(&url),
    };
    if dangling {
        let k = crate::binding::protos::private_key(scope, FETCH_DANGLING_PRIV);
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        rec.set_private(scope, k, t);
    }
    let m = str_val(scope, &method);
    set_prop(scope, rec, "method", m);
    // `RequestInit.priority`, as Blink's `AdjustPriorityWithPriorityHintAndRenderBlocking` adjusts a
    // fetch's kHigh: `low` demotes it to kLow -- urgency 3, incremental, the field `i` and HTTP/2 weight 146 (Chrome
    // 150, `tests/fixtures/h2-priority`); `high` and `auto` leave kHigh, the transport's own `u=1, i` for a fetch. The
    // member was converted (validated) and then never read.
    if get_prop(scope, init_dict, "priority").filter(|v| !v.is_undefined()).map(|v| v.to_rust_string_lossy(scope)).as_deref() == Some("low") {
        let pr = str_val(scope, "i");
        set_prop(scope, rec, "priority", pr);
    }
    // The `[[k, v], …]` array the op takes, from `HeadersInit` -- a record, a sequence of pairs, or a
    // `Headers`. It was `Object.entries(opts.headers)`, which reads a `Headers` instance (no own
    // enumerable properties) as NO headers and a `[["a", "b"]]` sequence as the header `0: a,b`; a
    // page's `fetch(u, {headers: new Headers({authorization: …})})` went out bare. A Request input
    // contributes its own headers when init has none.
    // `init.headers` converted as `HeadersInit` -- a Headers in it is ITERATED, as Blink's union conversion does, so
    // its names arrive lowercase and combined (`fetch(u, {headers: h})` after `h.append('Accept', 'a/b')` is
    // `accept: a/b` on Chrome 150's wire); without it a Request input's own list is copied as it is.
    let inherited = match pick(scope, "headers") {
        Some(_) => None,
        None => from_request
            .and_then(|o| crate::core::events::event::evt_slot_read(scope, o, "headers"))
            .and_then(|h| super::headers::headers_clone(scope, h)),
    };
    let built = match inherited {
        Some(h) => v8::Local::<v8::Object>::try_from(h).ok(),
        None => {
            let headers_src = pick(scope, "headers").unwrap_or_else(|| v8::Object::new(scope).into());
            new_global(scope, "Headers", &[headers_src])
        }
    };
    // The list read off the Headers object itself -- each name once, as first spelled, its values combined --
    // never through ITS iterator: `Array.from` ran the page's `Headers.prototype[Symbol.iterator]` and the
    // iterator prototype's `next` on every fetch a page made with headers.
    let entries: v8::Local<v8::Value> = match built {
        Some(h) => {
            let pairs = super::headers::headers_wire_pairs(scope, h.into()).unwrap_or_default();
            let arr = v8::Array::new(scope, pairs.len() as i32);
            for (i, (k, v)) in pairs.iter().enumerate() {
                let kk: v8::Local<v8::Value> = str_val(scope, k);
                let vv: v8::Local<v8::Value> = str_val(scope, v);
                let pair = v8::Array::new_with_elements(scope, &[kk, vv]);
                arr.set_index(scope, i as u32, pair.into());
            }
            arr.into()
        }
        // `new Headers(init)` threw (an invalid name/value): the rejection Chrome gives is that error.
        None => return Err(String::new()),
    };
    // THE `request` GUARD. `new Headers(init)` is guard NONE, which keeps every name --
    // right for a bare `Headers`, wrong for the list a fetch is built from: Fetch builds a Request, and
    // a Request's header list drops the forbidden request-header names silently. Chrome 150, measured
    // on the wire (127.0.0.1:A -> :B): `fetch(B, {headers: {"Accept-Encoding": ...}})`, `User-Agent` and
    // `Cookie` all go out as a SIMPLE GET with the browser's own values; korvus sent an OPTIONS preflight
    // for each and then the page's `cookie`. bartleby.com's API layer adds `Accept-Encoding` to every
    // call, so three calls the replay never saw preflighted and failed.
    let entries: v8::Local<v8::Value> = match v8::Local::<v8::Array>::try_from(entries) {
        Ok(arr) => {
            let kept = v8::Array::new(scope, 0);
            for i in 0..arr.length() {
                let Some(pair) = arr.get_index(scope, i) else { continue };
                let pa = v8::Local::<v8::Array>::try_from(pair).ok();
                let name = pa.and_then(|pa| pa.get_index(scope, 0)).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                let value = pa.and_then(|pa| pa.get_index(scope, 1)).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                // the name, and a method override's value (`X-HTTP-Method-Override: TRACE`), as Blink's guard judges them
                if super::headers::is_forbidden_request_header(&name, &value) {
                    continue;
                }
                let n = kept.length();
                kept.set_index(scope, n, pair);
            }
            kept.into()
        }
        Err(_) => entries,
    };
    // The cache mode travels to the transport's caller, which adds the headers it implies
    // BELOW the CORS decision: they are the browser's, not the author's, and Chrome sends
    // `fetch(crossOrigin, {cache: "no-store"})` as a simple GET carrying `pragma` and `cache-control`.
    let cv = str_val(scope, &cache);
    set_prop(scope, rec, "cache", cv);
    // A no-cors request's header list keeps only the CORS-safelisted request headers (Fetch's
    // `request-no-cors` guard drops the rest silently; measured: `x-foo` never reached the server).
    if mode == "no-cors" {
        if let Ok(arr) = v8::Local::<v8::Array>::try_from(entries) {
            let kept = v8::Array::new(scope, 0);
            for i in 0..arr.length() {
                let Some(pair) = arr.get_index(scope, i) else { continue };
                let Ok(pa) = v8::Local::<v8::Array>::try_from(pair) else { continue };
                let k = pa.get_index(scope, 0).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                let v = pa.get_index(scope, 1).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                if no_cors_safelisted(&k, &v) {
                    let n = kept.length();
                    kept.set_index(scope, n, pair);
                }
            }
            set_prop(scope, rec, "headers", kept.into());
        } else {
            set_prop(scope, rec, "headers", entries);
        }
    } else {
        set_prop(scope, rec, "headers", entries);
    }
    // THE BODY IS BYTES. A string goes over as a string (the transport sends its UTF-8), a
    // `BufferSource`/`Blob`/`URLSearchParams` as the exact bytes a `Request` would hold, with the
    // Content-Type Fetch's "extract a body" implies when the page set none -- the same extraction
    // `new Request(u, {body})` runs (`body.rs`). It was `String(body)`, so `new Uint8Array([1, 2])`
    // went on the wire as the three bytes `1,2`. A `Request` input's own body is read when init has
    // none, if it is already fully buffered.
    //
    // A GET or HEAD request cannot have a body -- the SAME Fetch step the `Request` constructor runs
    // (`request.rs`, "Request with GET/HEAD method cannot have body."). `fetch()` builds its record
    // without going through that constructor, so it was the one subset of the algorithm missing the
    // rule and `fetch(".", {method: "HEAD", body: "test"})` went out and RESOLVED. Measured, Chrome
    // 150: it rejects, with this sentence, for both GET and HEAD.
    if matches!(method.as_str(), "GET" | "HEAD") && pick(scope, "body").filter(|v| !v.is_null()).is_some() {
        return Err(format!("Failed to execute 'fetch' on '{iface}': Request with GET/HEAD method cannot have body."));
    }
    let body: v8::Local<v8::Value> = match pick(scope, "body").filter(|v| !v.is_null()) {
        Some(v) => match super::body::extract(scope, v) {
            Ok(super::body::Extracted::Bytes(bytes, ct)) => {
                // Into the list the record CARRIES: a no-cors record holds a filtered copy of
                // `entries`, so the implied `text/plain;charset=UTF-8` of a `sendBeacon` string was
                // appended to the unfiltered one and never sent.
                let carried = get_prop(scope, rec, "headers").unwrap_or(entries);
                if let (Some(ct), Ok(arr)) = (ct, v8::Local::<v8::Array>::try_from(carried)) {
                    let has = (0..arr.length()).any(|i| {
                        arr.get_index(scope, i)
                            .and_then(|p| v8::Local::<v8::Array>::try_from(p).ok())
                            .and_then(|p| p.get_index(scope, 0))
                            .map(|k| k.to_rust_string_lossy(scope).eq_ignore_ascii_case("content-type"))
                            .unwrap_or(false)
                    });
                    if !has {
                        // spelled as Blink appends it (`http_names::kContentType`): Chrome 150 writes this one
                        // `Content-Type` on an HTTP/1.1 wire, and a page's own `content-type` as the page spelled it
                        let pair = v8::Array::new(scope, 2);
                        let k = str_val(scope, "Content-Type");
                        let cv = str_val(scope, &ct);
                        pair.set_index(scope, 0, k);
                        pair.set_index(scope, 1, cv);
                        let n = arr.length();
                        arr.set_index(scope, n, pair.into());
                    }
                }
                super::body::u8_array(scope, bytes)
            }
            Ok(super::body::Extracted::Stream(s)) => match super::body::buffered_bytes(scope, s.into()) {
                Some(b) => super::body::u8_array(scope, b),
                None => v8::null(scope).into(),
            },
            Ok(super::body::Extracted::Null) => v8::null(scope).into(),
            Err(()) => return Err(String::new()),
        },
        None => match from_request.and_then(|o| crate::core::events::event::evt_slot_read(scope, o, "body")).filter(|b| !b.is_null()) {
            Some(b) => match super::body::buffered_bytes(scope, b) {
                Some(bytes) => super::body::u8_array(scope, bytes),
                None => v8::null(scope).into(),
            },
            None => v8::null(scope).into(),
        },
    };
    set_prop(scope, rec, "body", body);
    let md = str_val(scope, &mode);
    set_prop(scope, rec, "mode", md);
    let cv = str_val(scope, &credentials);
    set_prop(scope, rec, "credentials", cv);
    let rv = str_val(scope, &redirect);
    set_prop(scope, rec, "redirect", rv);
    // The request's referrer policy: init's (or a `Request` input's), else the
    // requesting document's -- its `Referrer-Policy` or `<meta name=referrer>`. `referrer: ""` is
    // no-referrer whatever the policy. Measured, Chrome 150: `fetch(B, {referrerPolicy: "unsafe-url"})`
    // from a `same-origin` page sends the full URL; the same fetch with no init sends none.
    let own = member(scope, "referrerPolicy", "");
    let policy = if conv.referrer.as_deref() == Some("") {
        Some(transport::ReferrerPolicy::NoReferrer)
    } else {
        transport::ReferrerPolicy::from_token(&own).or_else(|| crate::core::html::referrer_policy::running_policy(scope))
    };
    let rp = str_val(scope, crate::core::html::referrer_policy::token(policy));
    set_prop(scope, rec, "referrerPolicy", rp);
    Ok((rec, url))
}

// Fetch's method normalization, the enum conversions, the referrer parse and the two cross-member
// refusals all live in `request.rs` now -- ONE copy, shared with the `Request` constructor, which is
// the other entry into the same algorithm. This file's own partial copies are gone.

/// A "no-CORS-safelisted request-header": `accept`, `accept-language`, `content-language`, and a
/// safelisted `content-type`, each with a value of at most 128 bytes and no CORS-unsafe byte.
///
/// `pub(crate)` for `headers::guard_allows`, which is the OTHER place the same question is asked --
/// the `request-no-cors` guard drops exactly what this rejects. Promoting it beat a second copy in
/// `headers.rs` (CONF GUARD1; the lane contract's "one spec algorithm, one implementation"). The
/// four NAMES are also in `tools/sync/data/header_guard.json` as `cors_safelisted_request_headers`,
/// which is what `delete` reads -- it has no value to judge, so it can only ask the name half.
pub(crate) fn no_cors_safelisted(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let unsafe_byte = value.bytes().any(|b| (b < 0x20 && b != 0x09) || b"\"():<>?@[\\]{}\x7f".contains(&b));
    match name.to_ascii_lowercase().as_str() {
        "accept" => !unsafe_byte,
        "accept-language" | "content-language" => value.bytes().all(|b| b.is_ascii_alphanumeric() || b" *,-.;=".contains(&b)),
        "content-type" => {
            // `net::ExtractMimeTypeFromMediaType`, no comma, as Chrome's `cors.cc` asks it (A/B C5).
            let essence = transport::mime::extract_mime_type_from_media_type(&value.to_ascii_lowercase(), false).unwrap_or_default();
            !unsafe_byte && matches!(essence.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        _ => false,
    }
}

// ── the network path ────────────────────────────────────────────────────────────────────────────

fn client_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    let n = v8::String::new(scope, "korvus.fetch.client")?;
    Some(v8::Private::for_api(scope, Some(n)))
}
/// Park the request's client URL (`""` = the realm's own) on the record, in a PRIVATE slot: the
/// record crosses into `op_fetch` by serde, which reads named properties through the prototype chain,
/// so a named `client` member would be one a page could supply from `Object.prototype`. It travels to
/// the op as its own argument instead ([`network_fetch`]), and survives the Service-Worker fallback,
/// which keeps the record.
fn set_client(scope: &mut v8::PinScope, rec: v8::Local<v8::Object>, client: v8::Local<v8::Value>) {
    if let Some(k) = client_key(scope) {
        rec.set_private(scope, k, client);
    }
}

/// `op_fetch(record, client).then(r => new Response(r), e => { resource entry; throw e })` — the
/// network fetch, as a promise.
fn network_fetch<'s>(scope: &mut v8::PinScope<'s, '_>, rec: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    network_fetch_as(scope, rec, "fetch")
}

/// [`network_fetch`] for a request whose resource-timing entry names `initiator` (`fetch`, `beacon`).
fn network_fetch_as<'s>(scope: &mut v8::PinScope<'s, '_>, rec: v8::Local<'s, v8::Object>, initiator: &str) -> Option<v8::Local<'s, v8::Value>> {
    // `connect-src`: a request the document's policies refuse never leaves -- Fetch's main fetch
    // answers a network error, `TypeError: Failed to fetch` with no resource-timing entry (Chrome 150, measured) --
    // and each refusing policy reports a violation.
    if let Some(u) = get_prop(scope, rec, "url").filter(|u| u.is_string()) {
        let u = u.to_rust_string_lossy(scope);
        if crate::core::html::csp::realm_request_blocked(scope, &u, "") {
            let msg = v8::String::new(scope, "Failed to fetch")?;
            let err = v8::Exception::type_error(scope, msg);
            let resolver = v8::PromiseResolver::new(scope)?;
            resolver.reject(scope, err);
            return Some(resolver.get_promise(scope).into());
        }
    }
    // At the HEAD, where the realm has that op; the whole response otherwise.
    let f = deno_op(scope, "op_fetch_head").or_else(|| deno_op(scope, "op_fetch"))?;
    let client = client_key(scope)
        .and_then(|k| rec.get_private(scope, k))
        .filter(|v| v.is_string())
        .unwrap_or_else(|| str_val(scope, ""));
    // When the page asked, and for what -- the realm's own reading, which is all a request
    // that fails before a response can contribute to its resource-timing entry.
    let asked_ms = crate::core::timing::performance::clock_now_ms(scope);
    let asked: v8::Local<v8::Value> = v8::Number::new(scope, asked_ms).into();
    let url = get_prop(scope, rec, "url").unwrap_or_else(|| v8::undefined(scope).into());
    // The requesting document's site for cookies.
    let sfc = crate::core::html::browsing_context::realm_site_for_cookies(scope);
    let sfc = str_val(scope, &sfc);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = f.call(scope, undef, &[rec.into(), client, sfc])?;
    let who: v8::Local<v8::Value> = str_val(scope, initiator);
    let data: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &[url, asked, who]).into();
    let wrap = build_fn_data(scope, cb_wrap_response, "", 1, data);
    let fail = build_fn_data(scope, cb_fetch_failed, "", 1, data);
    let settled = crate::binding::protos::promise_then(scope, p, wrap, Some(fail))?;
    // The op is the page realm's function, so its promise -- and `then`'s -- are the page's. A frame
    // realm's `fetch` answers ITS OWN realm's Promise, adopting the op's.
    let own = scope.get_current_context();
    if v8::Local::<v8::Object>::try_from(settled).ok().and_then(|o| o.get_creation_context(scope)) == Some(own) {
        return Some(settled);
    }
    let resolver = v8::PromiseResolver::new(scope)?;
    resolver.resolve(scope, settled);
    Some(resolver.get_promise(scope).into())
}

/// `navigator.sendBeacon`'s request (Beacon §3.2): a keepalive POST with credentials `include`, sent WITHOUT
/// going through the page's `fetch` binding -- Chrome's beacon never calls `window.fetch`, and a page that
/// wrapped it used to see every beacon. `mode` is `no-cors` for a CORS-safelisted body and `cors` otherwise;
/// the HTTP/2 priority is Chromium's beacon value `u=4, i` (measured 2026-09-25 against Chrome 150 and
/// Brave 151). Its resource-timing entry is `beacon`'s, made when the response is in (Blink's
/// `fetch_initiator_type_names::kBeacon`; this listed every beacon as a `fetch`, and one with a response body
/// never, since nothing reads it). Returns false when the request could not be built, or when its body would take
/// the keepalive bytes in flight past 64 KiB (Blink's fetcher refuses it: Chrome 150 answers `false` for a
/// 70000-byte string and `true` for 60000).
pub(crate) fn beacon(scope: &mut v8::PinScope, url: &str, data: v8::Local<v8::Value>, mode: &str) -> bool {
    let init = crate::binding::protos::init_dict(scope);
    let post = str_val(scope, "POST");
    set_prop(scope, init, "method", post);
    if !data.is_undefined() && !data.is_null() {
        set_prop(scope, init, "body", data);
    }
    let md = str_val(scope, mode);
    set_prop(scope, init, "mode", md);
    let cr = str_val(scope, "include");
    set_prop(scope, init, "credentials", cr);
    let ka: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    set_prop(scope, init, "keepalive", ka);
    let input = str_val(scope, url);
    let built = {
        v8::tc_scope!(let tc, scope);
        let r = request_record(tc, input, init.into(), "Navigator").ok().map(|(rec, _)| v8::Global::new(tc, rec));
        tc.reset();
        r
    };
    let Some(rec) = built else { return false };
    let rec = v8::Local::new(scope, &rec);
    let size = get_prop(scope, rec, "body")
        .and_then(|b| v8::Local::<v8::ArrayBufferView>::try_from(b).ok())
        .map_or(0, |b| b.byte_length() as u64);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<KeepaliveInflight>().is_none() {
            st.put(KeepaliveInflight(0));
        }
        let inflight = &mut st.borrow_mut::<KeepaliveInflight>().0;
        if *inflight + size > KEEPALIVE_QUOTA {
            return false;
        }
        *inflight += size;
    }
    let pr = str_val(scope, "u=4, i");
    set_prop(scope, rec, "priority", pr);
    if let Some(id) = crate::core::html::browsing_context::running_frame(scope) {
        let client = crate::core::html::browsing_context::request_client(scope, id);
        let c = str_val(scope, &client);
        set_client(scope, rec, c);
    }
    let sent = {
        v8::tc_scope!(let tc, scope);
        let p = network_fetch_as(tc, rec, "beacon");
        if let Some(p) = p.and_then(|p| v8::Local::<v8::Promise>::try_from(p).ok()) {
            // The native `then`, not the page's `Promise.prototype.then`; settled, its bytes leave the quota.
            let bytes: v8::Local<v8::Value> = v8::Number::new(tc, size as f64).into();
            let settled = build_fn_data(tc, cb_beacon_settled, "", 1, bytes);
            p.then2(tc, settled, settled);
        }
        tc.reset();
        true
    };
    sent
}

/// The keepalive bytes in flight (Blink's `ResourceFetcher` quota for keepalive requests).
struct KeepaliveInflight(u64);

const KEEPALIVE_QUOTA: u64 = 64 * 1024;

fn cb_beacon_settled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let size = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(0.0) as u64;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(q) = st.try_borrow_mut::<KeepaliveInflight>() {
        q.0 = q.0.saturating_sub(size);
    }
}

/// The resource-timing initiator a network fetch's handlers were given (`network_fetch_as`).
fn initiator_of(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> String {
    v8::Local::<v8::Array>::try_from(data)
        .ok()
        .and_then(|a| a.get_index(scope, 2))
        .filter(|v| v.is_string())
        .map_or_else(|| "fetch".to_string(), |v| v.to_rust_string_lossy(scope))
}

fn asked_of(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> (String, f64) {
    let Ok(a) = v8::Local::<v8::Array>::try_from(data) else { return (String::new(), 0.0) };
    let url = a.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let asked = a.get_index(scope, 1).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    (url, asked)
}

/// The `.then` of the network fetch: the op's result record becomes a `Response` whose body is a stream
/// over the record's `Uint8Array` (`response.rs::response_from_network`).
fn cb_wrap_response(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(r) = super::response::response_from_network(scope, args.get(0)) {
        if let Ok(record) = v8::Local::<v8::Object>::try_from(args.get(0)) {
            let __body_data = crate::binding::protos::body_data(scope, &args);
            let (_, asked) = asked_of(scope, __body_data);
            let initiator = initiator_of(scope, __body_data);
            match get_prop(scope, record, "bodyId").filter(|v| v.is_number()) {
                // A head: the body (and with it the resource-timing entry, which needs responseEnd)
                // is still to come.
                Some(id) => receive_body(scope, r, id, asked, &initiator),
                None if initiator == "beacon" => crate::core::timing::resource_timing::beacon_response(scope, record, asked),
                None => crate::core::timing::resource_timing::fetch_response(scope, r, record, asked),
            }
        }
        rv.set(r.into());
    }
}

/// Wait for the body behind a head-only response `r`: pump its chunks into the body stream
/// as they arrive (H47), then close it and record the resource-timing entry -- or error the stream if
/// the connection fails.
fn receive_body(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, id: v8::Local<v8::Value>, asked: f64, initiator: &str) {
    let stream = crate::core::events::event::evt_slot_read(scope, r, "body").unwrap_or_else(|| v8::null(scope).into());
    let askedv: v8::Local<v8::Value> = v8::Number::new(scope, asked).into();
    let who: v8::Local<v8::Value> = str_val(scope, initiator);
    let data: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &[stream, r.into(), askedv, id, who]).into();
    next_chunk(scope, data);
}

/// Ask for the next chunk of the body `data` (`[stream, response, asked, id]`) names.
fn next_chunk(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) {
    let data = v8::Local::new(scope, data);
    let Ok(d) = v8::Local::<v8::Array>::try_from(data) else { return };
    let Some(id) = d.get_index(scope, 3) else { return };
    let Some(f) = deno_op(scope, "op_fetch_body_chunk") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[id]) else { return };
    let ok = build_fn_data(scope, cb_body_chunk, "", 1, data);
    let _ = crate::binding::protos::promise_then(scope, p, ok, None);
}

/// One chunk landed: into the stream, and ask for the next. `null`: the body is over -- learn how.
fn cb_body_chunk(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(d) = v8::Local::<v8::Array>::try_from(data) else { return };
    let d = v8::Local::new(scope, d);
    let stream = d.get_index(scope, 0).unwrap_or_else(|| v8::null(scope).into());
    let chunk = args.get(0);
    if chunk.is_array_buffer_view() {
        if !super::body::network_chunk_arrived(scope, stream, chunk) {
            // The page cancelled the body, or an abort errored it: nothing more is read.
            return;
        }
        next_chunk(scope, d.into());
        return;
    }
    let Some(id) = d.get_index(scope, 3) else { return };
    let Some(f) = deno_op(scope, "op_fetch_body") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[id]) else { return };
    let ok = build_fn_data(scope, cb_body_arrived, "", 1, d.into());
    let err = build_fn_data(scope, cb_body_failed, "", 1, d.into());
    let _ = crate::binding::protos::promise_then(scope, p, ok, Some(err));
}

fn cb_body_arrived(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let d = v8::Local::new(scope, d);
    let Ok(record) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let record = v8::Local::new(scope, record);
    let stream = d.get_index(scope, 0).unwrap_or_else(|| v8::null(scope).into());
    // The chunks are already in the stream (H47): it only closes.
    super::body::network_body_arrived(scope, stream, None);
    let r = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let asked = d.get_index(scope, 2).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let beacon = d.get_index(scope, 4).is_some_and(|v| v.is_string() && v.to_rust_string_lossy(scope) == "beacon");
    if beacon {
        crate::core::timing::resource_timing::beacon_response(scope, record, asked);
    } else if let Some(r) = r {
        crate::core::timing::resource_timing::fetch_response(scope, r, record, asked);
    }
}

/// The body could not be read to its end: Chrome's body read rejects `TypeError: network error`.
fn cb_body_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let d = v8::Local::new(scope, d);
    let stream = d.get_index(scope, 0).unwrap_or_else(|| v8::null(scope).into());
    let Some(m) = v8::String::new(scope, "network error") else { return };
    let e = v8::Exception::type_error(scope, m);
    super::body::network_body_failed(scope, stream, e);
}

/// The rejection arm: a failed fetch still has a resource-timing entry (measured: a refused port and a
/// CORS failure each leave one, status 0), and the page's promise rejects with the same reason.
fn cb_fetch_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let (url, asked) = asked_of(scope, __body_data);
    let initiator = initiator_of(scope, __body_data);
    crate::core::timing::resource_timing::request_failed(scope, &url, &initiator, asked);
    let p = crate::binding::protos::rejected_promise(scope, args.get(0));
    rv.set(p);
}

// ── the Service-Worker path ─────────────────────────────────────────────────────────────────────

/// The page now has a controlling ServiceWorker, so route its fetches through the SW's `fetch` event.
///
/// It replaces `host.js`'s `setupSwFetch()` + `KB.__swActive = true` (one bridge slot, gone).
///
/// IT WAS `op_sw_fetch_activate`, ONE OF W4.c's TWO TRANSITIONAL OPS. That op existed
/// only because the caller — the ServiceWorker container's `register` — was JS and could reach Rust
/// no other way. `register` is a table row now
/// (`core/workers/service_worker.rs::cb_container_register`) and calls this directly, so the op is
/// deleted rather than kept as a second way in. The `reentrant` attribute went with it: a plain Rust
/// function that constructs a `BroadcastChannel` is not an op re-entering an op.
pub(crate) fn activate_sw_fetch(scope: &mut v8::PinScope) {
    let already = with_bridge(scope, |b| b.active.replace(true));
    if already {
        return;
    }
    let name = str_val(scope, "korvus-sw-fetch");
    let Some(req) = new_global(scope, "BroadcastChannel", &[name]) else {
        with_bridge(scope, |b| b.active.set(false));
        return;
    };
    let rname = str_val(scope, "korvus-sw-fetch-resp");
    let Some(resp) = new_global(scope, "BroadcastChannel", &[rname]) else {
        with_bridge(scope, |b| b.active.set(false));
        return;
    };
    let on = build_fn(scope, cb_sw_response, "", 1);
    set_prop(scope, resp, "onmessage", on.into());
    // The response channel is held by its own `onmessage` registration; the request channel has to be
    // kept alive explicitly, because nothing else refers to it after this function returns.
    let g = v8::Global::new(scope, req);
    with_bridge(scope, |b| *b.req_channel.borrow_mut() = Some(g));
    // Keep the response channel alive too: a BroadcastChannel with no strong reference is collectable,
    // and a collected one stops delivering.
    let rg = v8::Global::new(scope, resp);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(SwRespChannel(rg));
}

/// A service worker claimed this page (`clients.claim()`): from now on its fetches go to
/// the worker.
pub(crate) fn sw_claimed(scope: &mut v8::PinScope) {
    with_bridge(scope, |b| b.controlled.set(true));
}

/// The response channel, parked in `OpState` for its lifetime only.
struct SwRespChannel(#[allow(dead_code)] v8::Global<v8::Object>);

/// One `{id, body}` reply off `korvus-sw-fetch-resp`. A null body means the SW's handler did not
/// `respondWith`, so the page's promise is settled from the NETWORK instead.
fn cb_sw_response(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(ev) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let Some(data) = get_prop(scope, ev, "data").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    // A LIFECYCLE report rides the same channel: `{life, url}` from the worker's realm.
    if let Some(life) = get_prop(scope, data, "life").filter(|v| v.is_string()) {
        let life = life.to_rust_string_lossy(scope);
        let url = get_prop(scope, data, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        crate::core::workers::service_worker::sw_lifecycle(scope, &life, &url);
        return;
    }
    let Some(id) = get_prop(scope, data, "id").and_then(|v| v.number_value(scope)) else { return };
    let id = id as u32;
    let (resolver, fallback) = with_bridge(scope, |b| (b.pending.borrow_mut().remove(&id), b.fallback.borrow_mut().remove(&id)));
    let Some(resolver) = resolver else { return };
    let resolver = v8::Local::new(scope, &resolver);
    let body = get_prop(scope, data, "body").unwrap_or_else(|| v8::null(scope).into());
    if body.is_null_or_undefined() {
        let value = fallback
            .map(|rec| v8::Local::new(scope, &rec))
            .and_then(|rec| network_fetch(scope, rec))
            .unwrap_or_else(|| v8::undefined(scope).into());
        resolver.resolve(scope, value);
        return;
    }
    let value = new_global(scope, "Response", &[body])
        .map(|r| -> v8::Local<v8::Value> { r.into() })
        .unwrap_or_else(|| v8::undefined(scope).into());
    resolver.resolve(scope, value);
}

// ── the global ──────────────────────────────────────────────────────────────────────────────────

/// `fetch(RequestInfo input, optional RequestInit init)` — `length` 1, and EVERY failure is a
/// rejection rather than a throw, because it is a promise-returning operation (measured; see the
/// module docs).
pub(crate) fn cb_fetch<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    // The realm names ITSELF in every one of these messages, exactly as Chrome does: measured in both,
    // the window over `--dump-dom` and a worker over a loopback page that posts the probe into a real
    // dedicated worker -- "Failed to execute 'fetch' on 'Window': …" and "… on 'WorkerGlobalScope': …".
    //
    // `global_get` answers `Some(undefined)` for a name the global does not have (it is `Object::get`,
    // not a `has`), so the window test has to be that `window` is an OBJECT. Reading it as presence
    // made every worker message say 'Window', which `fetch_types_live` caught in the worker realm.
    let iface = if global_get(scope, "window").map(|v| v.is_object()).unwrap_or(false) { "Window" } else { "WorkerGlobalScope" };
    // The realm's global OR A FRAME'S WINDOW. This was `this != global`, and a nested
    // context's window is not the realm's global in this engine -- a frame's scripts reach their
    // window through `with`, so a frame's bare `fetch(u)` is called ON that window. Every `fetch`
    // inside an iframe rejected with this sentence; Chrome 150.0.7871.46 resolves it (measured over
    // loopback, srcdoc and src frames alike). A foreign receiver still rejects, as Chrome's does.
    let this: v8::Local<v8::Value> = args.this().into();
    if !crate::core::html::window_globals::is_global_receiver(scope, this) {
        let p = rejected_type_error(scope, &format!("Failed to execute 'fetch' on '{iface}': Illegal invocation"));
        rv.set(p);
        return;
    }
    if args.length() < 1 {
        let p = rejected_type_error(scope, &format!("Failed to execute 'fetch' on '{iface}': 1 argument required, but only 0 present."));
        rv.set(p);
        return;
    }
    let caught: Result<Result<(v8::Global<v8::Object>, String), String>, v8::Global<v8::Value>> = {
        v8::tc_scope!(let tc, scope);
        let r = request_record(tc, args.get(0), args.get(1), iface).map(|(rec, url)| (v8::Global::new(tc, rec), url));
        if tc.has_caught() {
            let e = tc.exception().map(|e| v8::Global::new(tc, e));
            tc.reset();
            match e {
                Some(e) => Err(e),
                None => Ok(r),
            }
        } else {
            Ok(r)
        }
    };
    let (rec, mut url) = match caught {
        Ok(Ok((rec, url))) => (v8::Local::new(scope, &rec), url),
        Ok(Err(msg)) => {
            let p = rejected_type_error(scope, &msg);
            rv.set(p);
            return;
        }
        // A conversion inside the request (a header value, a body's `toString`) threw: Chrome rejects
        // with exactly that exception.
        Err(e) => {
            let e = v8::Local::new(scope, &e);
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    // WHOSE request it is: a frame's `fetch` resolves against the frame document's base
    // and goes out with the frame document as its client. The frame is the receiver when the call
    // named a frame window; a call whose receiver is the realm's global (an aliased `const f = fetch;
    // f(u)` inside a frame arrives that way, because V8 hands an undefined receiver the global) is
    // the running frame's, if a frame's script is running.
    //
    // Ruling: `parent.fetch(u)` from a frame's script is therefore the FRAME's request here, where
    // Chrome makes it the page's (its receiver's realm) -- the receiver is the global either way and
    // this engine cannot tell the two calls apart. Cost if wrong: that request's Referer names the
    // frame document instead of the page.
    let frame = v8::Local::<v8::Object>::try_from(this)
        .ok()
        .and_then(|o| crate::core::html::browsing_context::ctx_of_window(scope, o))
        .or_else(|| crate::core::html::browsing_context::running_frame(scope));
    let client = frame.map(|id| {
        let (_, base) = crate::core::html::browsing_context::frame_request_client(scope, id);
        let client = crate::core::html::browsing_context::request_client(scope, id);
        if let Some(abs) = url::Url::parse(&base).ok().and_then(|b| b.join(&url).ok()) {
            url = abs.to_string();
            let u = str_val(scope, &url);
            set_prop(scope, rec, "url", u);
        }
        client
    });
    let client = str_val(scope, client.as_deref().unwrap_or(""));
    set_client(scope, rec, client);
    // THE SIGNAL. `fetch` never looked at one: an aborted signal still fetched, and an
    // abort or `AbortSignal.timeout` mid-flight changed nothing -- Chrome 150 rejects at the abort with
    // the signal's reason (AbortError, or TimeoutError for a timeout; c8/repro/abort). a bot-detection endpoint's
    // loader aborts its `mfc` request that way and moves on; korvus waited the request out, built its
    // fingerprint frame a second late, and costco's API calls missed their token.
    let signal = fetch_signal(scope, args.get(0), rec);
    if let Some(reason) = signal.and_then(|s| crate::core::dom::abort_controller::aborted_reason(scope, s)) {
        let p = crate::binding::protos::rejected_promise(scope, reason);
        rv.set(p);
        return;
    }
    // POTENTIALLY DANGLING MARKUP: an http(s) url the page wrote with a tab or newline and a `<` never
    // leaves. Blink's `CanRequest` refuses it before any ServiceWorker sees it: `TypeError: Failed to fetch`, as a
    // `connect-src` refusal is.
    if dangling_refused(scope, rec, &url) {
        let p = rejected_type_error(scope, "Failed to fetch");
        rv.set(p);
        return;
    }
    if !with_bridge(scope, |b| b.controlled.get()) {
        if let Some(p) = network_fetch(scope, rec) {
            let p = abortable(scope, p, signal);
            rv.set(p);
        }
        return;
    }
    // A controlling ServiceWorker: publish the request and wait for its handler.
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let id = with_bridge(scope, |b| {
        let id = b.next.get() + 1;
        b.next.set(id);
        id
    });
    let rg = v8::Global::new(scope, resolver);
    let recg = v8::Global::new(scope, rec);
    with_bridge(scope, |b| {
        b.pending.borrow_mut().insert(id, rg);
        b.fallback.borrow_mut().insert(id, recg);
    });
    let msg = crate::binding::protos::init_dict(scope);
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    set_prop(scope, msg, "id", idv);
    let uv = str_val(scope, &url);
    set_prop(scope, msg, "url", uv);
    let channel = with_bridge(scope, |b| b.req_channel.borrow().clone());
    let posted = match channel {
        Some(c) => {
            let c = v8::Local::new(scope, &c);
            let cv: v8::Local<v8::Value> = c.into();
            crate::binding::protos::pristine_call(scope, cv, "BroadcastChannel.postMessage", &[msg.into()]).is_some()
        }
        None => false,
    };
    if !posted {
        // The bridge is not reachable: settle from the network rather than leaving the page waiting.
        let (r, f) = with_bridge(scope, |b| (b.pending.borrow_mut().remove(&id), b.fallback.borrow_mut().remove(&id)));
        if let (Some(r), Some(rec)) = (r, f) {
            let r = v8::Local::new(scope, &r);
            let rec = v8::Local::new(scope, &rec);
            let value = network_fetch(scope, rec).unwrap_or_else(|| v8::undefined(scope).into());
            r.resolve(scope, value);
        }
    }
    let promise = v8::Local::new(scope, &v8::Global::new(scope, resolver)).get_promise(scope);
    let p = abortable(scope, promise.into(), signal);
    rv.set(p);
}

/// Where [`request_record`] keeps the converted `init.signal` on the record it builds.
const FETCH_SIGNAL_PRIV: &str = "korvus_fetch_init_signal";

/// Where [`request_record`] marks a record whose url carries the dangling-markup mark.
const FETCH_DANGLING_PRIV: &str = "korvus_fetch_dangling_markup";

/// Whether `fetch()` refuses `rec` for dangling markup: its url is marked ([`request_record`]) and resolves to http(s)
/// (`base_fetch_context.cc`: `url.PotentiallyDanglingMarkup() && url.ProtocolIsInHttpFamily()`).
fn dangling_refused(scope: &mut v8::PinScope, rec: v8::Local<v8::Object>, url: &str) -> bool {
    let k = crate::binding::protos::private_key(scope, FETCH_DANGLING_PRIV);
    if !rec.get_private(scope, k).is_some_and(|v| v.is_true()) {
        return false;
    }
    let base = url::Url::parse(&crate::core::html::browsing_context::realm_base_url(scope)).ok();
    matches!(super::request::parse_with_base(base.as_ref(), url), Ok(Some(u)) if matches!(u.scheme(), "http" | "https"))
}

/// The AbortSignal `fetch(input, init)` follows: `init.signal` when init names one (as converted, kept on the
/// record), else the Request input's own.
fn fetch_signal<'s>(scope: &mut v8::PinScope<'s, '_>, input: v8::Local<'s, v8::Value>, rec: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let sig_key = crate::binding::protos::private_key(scope, FETCH_SIGNAL_PRIV);
    if let Some(s) = rec.get_private(scope, sig_key).filter(|v| !v.is_undefined()) {
        return s.is_object().then_some(s);
    }
    let req = v8::Local::<v8::Object>::try_from(input).ok().filter(|o| crate::install::webidl::has_brand(scope, (*o).into(), "Request"))?;
    crate::core::events::event::evt_slot_read(scope, req, "signal").filter(|v| v.is_object())
}

/// The resolvers of the `fetch()` promises an AbortSignal can still settle, by id. An
/// entry leaves on whichever comes first -- the network's answer or the abort -- so the second finds
/// nothing and does nothing.
#[derive(Default)]
struct AbortableFetches {
    next: Cell<u32>,
    pending: RefCell<HashMap<u32, (v8::Global<v8::PromiseResolver>, v8::Global<v8::Value>)>>,
}

fn with_abortable<R>(scope: &mut v8::PinScope, f: impl FnOnce(&AbortableFetches) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<AbortableFetches>().is_none() {
        st.put(AbortableFetches::default());
    }
    f(st.borrow::<AbortableFetches>())
}

/// `p`, settled by whichever comes first: `p` itself, or `signal`'s abort -- which rejects with the
/// signal's reason (Fetch "abort the fetch() call"). Without a signal, `p` unchanged.
fn abortable<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<'s, v8::Value>, signal: Option<v8::Local<'s, v8::Value>>) -> v8::Local<'s, v8::Value> {
    let Some(sig) = signal else { return p };
    if !crate::install::webidl::has_brand(scope, sig, "AbortSignal") {
        return p;
    }
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return p };
    let g = v8::Global::new(scope, resolver);
    let sg = v8::Global::new(scope, sig);
    let id = with_abortable(scope, |a| {
        let id = a.next.get().wrapping_add(1);
        a.next.set(id);
        a.pending.borrow_mut().insert(id, (g, sg));
        id
    });
    let data: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    let ok = build_fn_data(scope, cb_abortable_resolve, "", 1, data);
    let err = build_fn_data(scope, cb_abortable_reject, "", 1, data);
    let _ = crate::binding::protos::promise_then(scope, p, ok, Some(err));
    let on_abort = build_fn_data(scope, cb_abortable_reject, "", 1, data);
    crate::core::dom::abort_controller::add_abort_algorithm(scope, sig, on_abort);
    resolver.get_promise(scope).into()
}

/// The resolver an [`abortable`] callback names, and its signal, unless the promise is already settled.
fn abortable_take<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<(v8::Local<'s, v8::PromiseResolver>, v8::Local<'s, v8::Value>)> {
    let id = crate::binding::protos::body_data(scope, args).uint32_value(scope)?;
    let (g, s) = with_abortable(scope, |a| a.pending.borrow_mut().remove(&id))?;
    Some((v8::Local::new(scope, &g), v8::Local::new(scope, &s)))
}

fn cb_abortable_resolve(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((r, sig)) = abortable_take(scope, &args) else { return };
    let response = args.get(0);
    r.resolve(scope, response);
    // The headers are in and the body may still be arriving: an abort now errors the BODY.
    let stream = v8::Local::<v8::Object>::try_from(response)
        .ok()
        .and_then(|o| crate::core::events::event::evt_slot_read(scope, o, "body"))
        .filter(|b| b.is_object());
    if let Some(stream) = stream {
        let pending = v8::Local::<v8::Object>::try_from(stream).ok().and_then(|o| crate::core::css::css_style_declaration::priv_get(scope, o, super::body::BODY_ID));
        if pending.is_some_and(|v| v.is_number()) {
            let f = build_fn_data(scope, cb_abort_body, "", 1, stream);
            crate::core::dom::abort_controller::add_abort_algorithm(scope, sig, f);
        }
    }
}

fn cb_abortable_reject(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some((r, _)) = abortable_take(scope, &args) {
        r.reject(scope, args.get(0));
    }
}

/// An abort after the headers: the body stream errors with Chrome's fixed reason -- `AbortError: The
/// user aborted a request.`, whatever the signal's own reason (measured 2026-10-01: an
/// `AbortSignal.timeout` too) -- and the network read stops.
fn cb_abort_body(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let stream = crate::binding::protos::body_data(scope, &args);
    let Some(e) = crate::binding::protos::make_dom_exc(scope, "The user aborted a request.", "AbortError") else { return };
    super::body::network_body_failed(scope, stream, e);
}

/// Install `globalThis.fetch` — a writable, enumerable, configurable data property holding the native
/// function, which is the descriptor Chrome has (measured).
pub fn install_fetch<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let f = build_fn(scope, cb_fetch, "fetch", 1);
    // `fetch` does its OWN argument check, and rejects rather than throwing -- Chrome answers
    // `[object Promise]` to a bare `fetch()`, measured. This mark is the record of that: it kept the
    // `generated_order::WINQ` arity pass (which said `("fetch", 1)`) from wrapping the body in a
    // generic guard that THROWS -- the right sentence in the wrong shape, and one a page's `.catch`
    // never sees. That pass is deleted, and the mark stays as the truthful statement that this
    // body counts its own arguments.
    crate::binding::protos::stamp_arity_guarded(scope, f);
    let Some(key) = v8::String::new(scope, "fetch") else { return };
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
    pd.set_enumerable(true);
    pd.set_configurable(true);
    global.define_property(scope, key.into(), &pd);
}

// NO `webapi_ops!` LINE HERE ANY MORE: `op_sw_fetch_activate` was this module's only
// registered op, and the page's SW-fetch switch is [`activate_sw_fetch`], a plain Rust call, now.
// `tests/registry.rs` diffs the sorted `ops.manifest` against the crate's declarations, so the
// removal is recorded there rather than in a count.

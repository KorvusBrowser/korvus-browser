//! `modules/fetch/request.rs` -- the `Request` table.
//!
//! The CONSTRUCTOR is ported: `host.js`'s `class Request` is gone and the nineteen readonly
//! attributes are filled from Rust into the same private slots. Measured with
//! `tests/fixtures/fetch-types-probe.js` on `google-chrome --headless=new` 150.0.7871.46:
//!
//! * `new Request()` CONSTRUCTED a request over `""`; Chrome throws "1 argument required, but only 0
//!   present." The table carries `required 1` now.
//! * `Request("x")` said "Class constructor Request cannot be invoked without 'new'".
//! * every accessor answered `undefined` for a foreign receiver where Chrome throws
//!   `Illegal invocation`; the promise-returning methods REJECT instead, and `clone` throws.
//!
//! The six body-reading methods have no implementation yet and are honest D2 `stub` rows -- what the
//! registry gave them.

use deno_core::v8;
use crate::binding::protos::{call_method, get_prop, new_global, str_val, throw_type_error};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::webidl::{arg_string, brand, branded_getter_row, branded_method_row, branded_promise_method_row, init_dict, require_args, require_new};
use crate::install::MemberDef;
use super::body::{self, Extracted, Kind};

/// Every private slot a `Request` carries -- what `clone()` copies.
const SLOTS: &[&str] = &[
    "url", "method", "credentials", "mode", "referrerPolicy", "cache", "redirect", "integrity", "headers", "body",
    "destination", "referrer", "keepalive", "signal", "duplex", "isHistoryNavigation", "isReloadNavigation",
    "danglingMarkup",
];

/// Whether `request`'s url carries Chromium's dangling-markup mark
/// (`url::url::potentially_dangling_markup`). The url it serializes has lost its tab or newline, but Blink's `KURL`
/// keeps the mark: `fetch(new Request(u))` and `fetch(request.clone())` are refused as `fetch(u)` is. A Request made
/// from a Request keeps its input's mark; the slot (`SLOTS`) carries it through `clone()`.
pub(crate) fn is_dangling(scope: &mut v8::PinScope, request: v8::Local<v8::Object>) -> bool {
    evt_slot_read(scope, request, "danglingMarkup").is_some_and(|v| v.is_true())
}

/// `Request.clone()` -- every slot copied, the headers into a new `Headers`, the body TEED.
///
/// It was `new Request(url, {method, body})`: the clone lost its headers, mode, credentials and every
/// other member, and shared (rather than teed) the body. Chrome 150: `r.clone()` of a POST "x" reads
/// "x" from both, and cloning a used body is `TypeError: Failed to execute 'clone' on 'Request':
/// Request body is already used`.
fn cb_request_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if body::body_unusable(scope, this) {
        throw_type_error(scope, "Failed to execute 'clone' on 'Request': Request body is already used");
        return;
    }
    let Some(proto) = crate::binding::protos::iface_object(scope, "Request")
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
                    // A clone keeps its original's guard -- `new Headers(other)` is always `none`,
                    // so without this a clone's headers were freely mutable. Measured (widened
                    // probe case D): `new Request(u, {headers: {}}).clone().headers.set("cookie", "x")`
                    // drops, and the no-cors clone drops `range`.
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
    brand(scope, out, "Request");
    rv.set(out.into());
}

fn cb_request_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Request", Kind::Text);
    rv.set(p);
}
/// `Request.textStream()` (Chrome 152, the Body mixin's: [`body::text_stream`]).
fn cb_request_text_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(s) = body::text_stream(scope, args.this(), "Request") {
        rv.set(s);
    }
}
fn cb_request_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Request", Kind::Json);
    rv.set(p);
}
fn cb_request_array_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Request", Kind::ArrayBuffer);
    rv.set(p);
}
fn cb_request_bytes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Request", Kind::Bytes);
    rv.set(p);
}
fn cb_request_blob(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = body::consume(scope, args.this(), "Request", Kind::Blob);
    rv.set(p);
}
fn cb_request_body_used(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    body::cb_body_used(scope, args, rv);
}

/// `Request.url` -- the request's url, RESOLVED.
///
/// The class stores whatever the page passed (`new Request("r.json")` stored the four bytes
/// `r.json`), and `= slot_get` handed them straight back, so `new Request(rel).url` answered the raw
/// string where Chrome answers an absolute URL. Fetch's "parse a URL" step is not optional: the url of
/// a Request is a URL record, and a page that builds one to inspect its `origin` or `pathname` gets
/// nonsense from a relative string.
///
/// Resolution is against the document's BASE url, which is the same base every other relative URL on
/// the page resolves against (`core/dom/document.rs::document_base_url`) -- measured in Chrome:
/// `<base href="/sub/">` makes `new Request("r.json").url` read `…/sub/r.json`.
///
/// THE RULING THIS COMMENT USED TO CARRY IS SPENT. It said resolution happens on READ rather than in
/// the constructor "because the constructor is `korvus-runtime`'s `host.js` class and the rules
/// forbid growing a shipped `.js` file". That constructor is Rust now (`cb_request_ctor`, below), and
/// it has to parse anyway -- Fetch's parse step is where the two TypeErrors of
/// `request-error.any.js` come from, and a getter cannot throw them. So the slot holds the ABSOLUTE
/// url from construction onward and this getter is idempotent over it (an absolute url resolves to
/// itself). It is kept, not deleted, for the one case the constructor cannot answer: a realm with no
/// usable base, where the constructor stores the string as written and this still resolves it if a
/// base appears later.
///
/// Side effect, and it is the RIGHT way round: a Request built with a relative url BEFORE the
/// document's `<base>` changes now keeps the base it was built with, which is what Chrome does; the
/// old read-time resolution reported the new one.
fn cb_request_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let raw = match evt_slot_read(scope, args.this(), "url") {
        Some(v) if !v.is_undefined() => v.to_rust_string_lossy(scope),
        _ => {
            let e = str_val(scope, "");
            rv.set(e);
            return;
        }
    };
    if let Ok(u) = url::Url::parse(&raw) {
        let v = str_val(scope, u.as_str());
        rv.set(v);
        return;
    }
    let base = crate::core::html::browsing_context::realm_base_url(scope);
    match url::Url::parse(&base).ok().and_then(|b| b.join(&raw).ok()) {
        Some(u) => {
            let v = str_val(scope, u.as_str());
            rv.set(v);
        }
        // Unresolvable (a worker or harness realm with no location): the string, as it was stored.
        None => {
            let v = str_val(scope, &raw);
            rv.set(v);
        }
    }
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

/// The `RequestInit` members this engine reflects: `(key, default, WebIDL enum type, its members)`.
/// Read from `init` first, then from a `Request` passed as `input`, then the default — which is what
/// the JS class did, written once here instead of nineteen times.
///
/// An empty `members` list is a plain string (`method` is a token, `integrity` is free-form); every
/// other row is a WebIDL **enumeration**, and a value outside it is a `TypeError` raised where the
/// conversion happens. The loop below used to be a bare `ToString`, so `{redirect: "BAD"}` STORED the
/// four bytes `BAD` and reported success — `request-error.any.js` scattered that one omission across
/// nine member rows.
const STRING_INIT: &[(&str, &str, &str, &[&str])] = &[
    ("method", "GET", "", &[]),
    ("credentials", "same-origin", "RequestCredentials", &["omit", "same-origin", "include"]),
    ("mode", "cors", "RequestMode", &["navigate", "same-origin", "no-cors", "cors"]),
    (
        "referrerPolicy",
        "",
        "ReferrerPolicy",
        &[
            "",
            "no-referrer",
            "no-referrer-when-downgrade",
            "same-origin",
            "origin",
            "strict-origin",
            "origin-when-cross-origin",
            "strict-origin-when-cross-origin",
            "unsafe-url",
        ],
    ),
    ("cache", "default", "RequestCache", &["default", "no-store", "reload", "no-cache", "force-cache", "only-if-cached"]),
    ("redirect", "follow", "RequestRedirect", &["follow", "error", "manual"]),
    ("integrity", "", "", &[]),
];

/// Fetch's "normalize a method": the six that are byte-case-insensitive matches are uppercased, and
/// nothing else is touched (measured, Chrome 150: `{method: "get"}.method` is `"GET"`, `"patch"` stays
/// `"patch"`).
///
/// It lived in `fetch_global.rs`, which is the OTHER caller; `pub(crate)` here so the constructor and
/// `fetch()` share ONE copy rather than the constructor growing a third (there were already two — this
/// one and an inline `find(|n| eq_ignore_ascii_case)` in the no-cors check below).
pub(crate) fn normalize_method(m: &str) -> String {
    for n in ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"] {
        if m.eq_ignore_ascii_case(n) {
            return n.to_string();
        }
    }
    m.to_string()
}

/// An HTTP `token` (RFC 9110 §5.6.2) — what Fetch means by "is a method". `"IN VALID"` is not one.
fn is_method_token(m: &str) -> bool {
    !m.is_empty()
        && m.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

/// Fetch's "forbidden method": `CONNECT`, `TRACE`, `TRACK`, byte-case-insensitively (measured, Chrome
/// 150: `{method: "track"}` throws and names the method AS WRITTEN, before normalization).
fn is_forbidden_method(m: &str) -> bool {
    ["CONNECT", "TRACE", "TRACK"].iter().any(|f| m.eq_ignore_ascii_case(f))
}

/// Fetch's "parse a URL with base", with the one distinction that decides whether a failure is the
/// PAGE's fault or this realm's: `url` reports `RelativeUrlWithoutBase` for a relative string and
/// nothing else, so a genuinely malformed input is a `TypeError` whatever the base, while a relative
/// string in a realm with no usable base (a bare worker, the quality harness — `base_url` answers
/// `about:blank` there, which cannot be a base) falls back to the raw string exactly as the `url`
/// getter has always done. Without that split, `new Request("")` in such a realm would start throwing.
///
/// An input that fails alone can still be relative to a base of ITS OWN scheme: the URL parser reads
/// `http:` or `http:x` against an http base as a path relative to it ("special relative or
/// authority"), so Chrome 150's `new Request("http:")` on an http page is the page's URL,
/// while `ftp:` on that page stays a parse failure.
pub(crate) fn parse_with_base(base: Option<&url::Url>, s: &str) -> Result<Option<url::Url>, ()> {
    match url::Url::parse(s) {
        Ok(u) => Ok(Some(u)),
        Err(url::ParseError::RelativeUrlWithoutBase) => match base.and_then(|b| b.join(s).ok()) {
            Some(u) => Ok(Some(u)),
            None => Ok(None),
        },
        Err(_) => {
            let scheme = s.trim_start().split(':').next().unwrap_or("").to_ascii_lowercase();
            match base {
                Some(b) if b.scheme() == scheme => b.join(s).map(Some).map_err(|_| ()),
                _ => Err(()),
            }
        }
    }
}

/// What [`convert_init`] converted: the `STRING_INIT` members in that table's order (`None` = the
/// member was absent from `init`), plus `referrer` already resolved and serialized.
pub(crate) struct ConvertedInit {
    pub(crate) members: [Option<String>; STRING_INIT.len()],
    pub(crate) referrer: Option<String>,
}

impl ConvertedInit {
    /// The converted value of `key`, or `None` if `init` did not carry it.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        let i = STRING_INIT.iter().position(|(k, ..)| *k == key)?;
        self.members[i].as_deref()
    }
}

/// `Err(None)` means a JS exception is already pending (a `toString` threw) and the caller must just
/// return; `Err(Some(msg))` is the TypeError's sentence WITHOUT the `Failed to construct 'Request': `
/// or `Failed to execute 'fetch' on 'Window': ` prefix, which is the caller's to add — Chrome's two
/// messages differ in exactly that prefix and in nothing else (measured).
pub(crate) type InitError = Option<String>;

/// **Fetch's `RequestInit` conversion — ONE implementation, run by BOTH callers.**
///
/// `new Request(input, init)` and `fetch(input, init)` are two entries into the same algorithm, and
/// they had two different partial copies of it: the constructor's was a bare `ToString` over seven
/// keys, and `fetch_global.rs`'s read `method`/`mode`/`credentials`/`redirect` by hand with one
/// hand-written mode check. `fetch/api/abort/general.any.js` is the file that proves they must be the
/// same code: its "TypeError from request constructor takes priority" cases CONSTRUCT a bad Request,
/// and only if that throws do they go on to require that `fetch` with the same arguments REJECTS with
/// a TypeError. While the constructor accepted everything those seven cases were skipped and scored as
/// passes; the moment it started throwing they began actually testing `fetch`, which is how a
/// half-shared algorithm shows up.
pub(crate) fn convert_init<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    init: Option<v8::Local<'s, v8::Object>>,
    base: Option<&url::Url>,
) -> Result<ConvertedInit, InitError> {
    let mut members: [Option<String>; STRING_INIT.len()] = Default::default();
    for (i, (key, _default, enum_ty, allowed)) in STRING_INIT.iter().enumerate() {
        let Some(v) = init.and_then(|d| get_prop(scope, d, key)).filter(|v| !v.is_undefined()) else { continue };
        let Some(s) = v.to_string(scope) else { return Err(None) };
        let s = s.to_rust_string_lossy(scope);
        // WebIDL enumeration: an unknown value is a TypeError, never a stored string.
        if !allowed.is_empty() && !allowed.iter().any(|m| *m == s) {
            return Err(Some(format!(
                "Failed to read the '{key}' property from 'RequestInit': The provided value '{s}' is not a valid enum value of type {enum_ty}."
            )));
        }
        // `navigate` IS a `RequestMode` member -- a navigation request has it -- so it passes the enum
        // conversion and this step refuses it separately (request.cc:547).
        if *key == "mode" && s == "navigate" {
            return Err(Some("Cannot construct a Request with a RequestInit whose mode member is set as 'navigate'.".to_string()));
        }
        let s = if *key == "method" {
            // request.cc:711,716: not a token, then forbidden, then normalize. Both messages name the
            // method AS WRITTEN -- normalization is the last step.
            if !is_method_token(&s) {
                return Err(Some(format!("'{s}' is not a valid HTTP method.")));
            }
            if is_forbidden_method(&s) {
                return Err(Some(format!("'{s}' HTTP method is unsupported.")));
            }
            normalize_method(&s)
        } else {
            s
        };
        members[i] = Some(s);
    }
    // `referrer` (request.cc:466 and the block after it), measured on Chrome 150 from a `127.0.0.1`
    // page: "" is no-referrer and reads back as ""; a same-origin url reads back absolute; a
    // cross-origin one and `about:client` both read back "about:client"; an unparseable one throws.
    let referrer = match init.and_then(|d| get_prop(scope, d, "referrer")).filter(|v| !v.is_undefined()) {
        Some(v) => {
            let Some(s) = v.to_string(scope) else { return Err(None) };
            let s = s.to_rust_string_lossy(scope);
            Some(if s.is_empty() {
                String::new()
            } else {
                match parse_with_base(base, &s) {
                    Err(()) => return Err(Some(format!("Referrer '{s}' is not a valid URL."))),
                    // Unresolvable in this realm for the same reason a relative url is: keep the client.
                    Ok(None) => "about:client".to_string(),
                    Ok(Some(u)) => {
                        let same_origin = base.map(|b| b.origin() == u.origin()).unwrap_or(false);
                        if (u.scheme() == "about" && u.path() == "client") || !same_origin {
                            "about:client".to_string()
                        } else {
                            u.as_str().to_string()
                        }
                    }
                }
            })
        }
        None => None,
    };
    Ok(ConvertedInit { members, referrer })
}

/// Fetch's two refusals that depend on the FINAL cache/mode/method rather than on one member, so they
/// run after [`convert_init`] in both callers (request.cc:638 for the first; the second was
/// measured). The `only-if-cached` message has no full stop, unlike its neighbours -- that is
/// Blink's, verbatim.
pub(crate) fn check_init_combination(cache: &str, mode: &str, method: &str) -> Result<(), String> {
    if cache == "only-if-cached" && mode != "same-origin" {
        return Err("'only-if-cached' can be set only with 'same-origin' mode".to_string());
    }
    if mode == "no-cors" && !matches!(method, "GET" | "HEAD" | "POST") {
        return Err(format!("'{method}' is unsupported in no-cors mode."));
    }
    Ok(())
}

/// `new Request(RequestInfo input, optional RequestInit init)` — `length` 1, `input` required.
pub(crate) fn cb_request_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "Request") || !require_args(scope, &args, "Request", 1) {
        return;
    }
    let this = args.this();
    let input = args.get(0);
    // `RequestInfo` is `(Request or USVString)`. A Request contributes its url, method, headers and
    // body; anything else is stringified.
    let from_request = v8::Local::<v8::Object>::try_from(input)
        .ok()
        .filter(|o| crate::install::webidl::has_brand(scope, (*o).into(), "Request"));
    let url = match from_request {
        Some(o) => evt_slot_read(scope, o, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default(),
        None => match arg_string(scope, &args, 0, "Request") {
            Some(s) => s,
            None => return,
        },
    };
    let dangling = match from_request {
        Some(o) => is_dangling(scope, o),
        None => crate::modules::url::url::potentially_dangling_markup(&url),
    };
    let Ok(init) = init_dict(scope, &args, 1, "Request", "RequestInit") else { return };
    let pick = |scope: &mut v8::PinScope<'s, '_>, key: &str| -> Option<v8::Local<'s, v8::Value>> {
        if let Some(v) = init.and_then(|d| get_prop(scope, d, key)).filter(|v| !v.is_undefined()) {
            return Some(v);
        }
        from_request.and_then(|o| evt_slot_read(scope, o, key))
    };
    // THE URL IS PARSED, not just stored. `input` went into the slot verbatim and only the `url`
    // getter ever resolved it, so the two steps that THROW never ran:
    // `new Request("http://:not a valid URL")` constructed, and so did one with credentials in it.
    // Messages measured on Chrome 150 (`Failed to parse URL from …` / `Request cannot be constructed
    // from a URL that includes credentials: …`), which is also what
    // `third_party/blink/renderer/core/fetch/request.cc:394,400` says.
    // The RUNNING realm's base: a frame's `new Request('/x')` is the frame's URL.
    let base = url::Url::parse(&crate::core::html::browsing_context::realm_base_url(scope)).ok();
    let url = match parse_with_base(base.as_ref(), &url) {
        Err(()) => {
            body::throw_ctor(scope, "Request", &format!("Failed to parse URL from {url}"));
            return;
        }
        Ok(Some(u)) => {
            if !u.username().is_empty() || u.password().is_some() {
                body::throw_ctor(scope, "Request", &format!("Request cannot be constructed from a URL that includes credentials: {url}"));
                return;
            }
            u.as_str().to_string()
        }
        // A relative url with no usable base: kept as it was, which is what the `url` getter does.
        Ok(None) => url,
    };
    let u = str_val(scope, &url);
    evt_slot_write(scope, this, "url", u);
    if dangling {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        evt_slot_write(scope, this, "danglingMarkup", t);
    }
    // ONE conversion for all seven members, and it is `convert_init` -- the same function `fetch()`
    // runs. It sees only what `init` ACTUALLY carries: a value inherited from a `Request` input was
    // converted when THAT request was constructed, and Fetch's "if init[x] exists" steps are written
    // against the dictionary, not against the inherited request.
    let conv = match convert_init(scope, init, base.as_ref()) {
        Ok(c) => c,
        Err(None) => return,
        Err(Some(msg)) => {
            body::throw_ctor(scope, "Request", &msg);
            return;
        }
    };
    for (i, (key, default, _, _)) in STRING_INIT.iter().enumerate() {
        let v: v8::Local<v8::Value> = match &conv.members[i] {
            Some(s) => str_val(scope, s),
            None => match from_request.and_then(|o| evt_slot_read(scope, o, key)) {
                Some(v) => v,
                None => str_val(scope, default),
            },
        };
        evt_slot_write(scope, this, key, v);
    }
    let read_slot = |scope: &mut v8::PinScope<'s, '_>, key: &str| -> String {
        evt_slot_read(scope, this, key).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
    };
    let cache = read_slot(scope, "cache");
    let mode = read_slot(scope, "mode");
    let method = read_slot(scope, "method");
    if let Err(msg) = check_init_combination(&cache, &mode, &method) {
        body::throw_ctor(scope, "Request", &msg);
        return;
    }
    // `init.headers` is converted as `HeadersInit` (a Headers in it is ITERATED: lowercase names, values combined);
    // without it the input Request's list is copied as it is (Blink's `Request` constructor: `init->hasHeaders()`
    // fills from the converted init, otherwise from `r->getHeaders()->Clone()`).
    let init_headers = init.and_then(|d| get_prop(scope, d, "headers")).filter(|v| !v.is_undefined());
    let inherited = match (init_headers, from_request) {
        (None, Some(o)) => evt_slot_read(scope, o, "headers").and_then(|h| super::headers::headers_clone(scope, h)),
        _ => None,
    };
    let headers = match inherited {
        Some(h) => h,
        None => {
            let headers_init = init_headers.unwrap_or_else(|| v8::undefined(scope).into());
            new_global(scope, "Headers", &[headers_init])
                .map(|h| -> v8::Local<v8::Value> { h.into() })
                .unwrap_or_else(|| v8::undefined(scope).into())
        }
    };
    // THE GUARD (CONF GUARD1). A `Request`'s header list is `request`; a `no-cors` one's is
    // `request-no-cors`. It is stamped HERE, before the body's `Content-Type` is appended below,
    // because that append is NOT exempt from the guard: measured on Chrome 150,
    // `new Request(u, {mode: "no-cors", method: "POST", body: new Blob([""], {type: "application/json"})})
    //  .headers.get("content-type")` is `null`, while the same request in `cors` mode answers
    // `application/json` (widened probe case B). Stamping it after the append would keep it.
    super::headers::set_guard(scope, headers, if mode == "no-cors" { super::headers::Guard::RequestNoCors } else { super::headers::Guard::Request });
    evt_slot_write(scope, this, "headers", headers);
    // THE BODY, measured on Chrome 150:
    //   GET/HEAD with a body        TypeError: Failed to construct 'Request': Request with GET/HEAD method cannot have body.
    //   a ReadableStream, no duplex TypeError: Failed to construct 'Request': The `duplex` member must be specified for a request with a streaming body
    //   new Request(usedRequest)    TypeError: Failed to construct 'Request': Cannot construct a Request with a Request object that has already been used.
    //   new Request(r) (r has body) r.bodyUsed becomes true; the new request reads r's bytes
    //   a string body               Content-Type text/plain;charset=UTF-8 (a Uint8Array: none)
    let init_body = init.and_then(|d| get_prop(scope, d, "body")).filter(|v| !v.is_undefined());
    let method_now = evt_slot_read(scope, this, "method").map(|m| m.to_rust_string_lossy(scope)).unwrap_or_default();
    let bodyless = method_now.eq_ignore_ascii_case("GET") || method_now.eq_ignore_ascii_case("HEAD");
    let body_v: v8::Local<v8::Value> = match init_body {
        Some(v) => {
            let Ok(extracted) = body::extract(scope, v) else { return };
            if !matches!(extracted, Extracted::Null) && bodyless {
                body::throw_ctor(scope, "Request", "Request with GET/HEAD method cannot have body.");
                return;
            }
            if let Extracted::Stream(_) = extracted {
                if init.and_then(|d| get_prop(scope, d, "duplex")).filter(|v| !v.is_undefined()).is_none() {
                    body::throw_ctor(scope, "Request", "The `duplex` member must be specified for a request with a streaming body");
                    return;
                }
            }
            if let (Extracted::Bytes(_, Some(ct)), Ok(h)) = (&extracted, v8::Local::<v8::Object>::try_from(headers)) {
                // the type the body implies, spelled as Blink's `http_names::kContentType` (a header list keeps
                // each name's spelling, and the wire shows it)
                let k = str_val(scope, "Content-Type");
                let has = call_method(scope, h.into(), "has", &[k]).map(|v| v.boolean_value(scope)).unwrap_or(false);
                if !has {
                    let cv = str_val(scope, ct);
                    let _ = call_method(scope, h.into(), "append", &[k, cv]);
                }
            }
            body::body_stream(scope, &extracted)
        }
        None => match from_request {
            Some(src) => {
                let src_body = evt_slot_read(scope, src, "body").filter(|b| !b.is_null());
                match src_body {
                    Some(b) => {
                        if body::body_unusable(scope, src) {
                            body::throw_ctor(scope, "Request", "Cannot construct a Request with a Request object that has already been used.");
                            return;
                        }
                        if bodyless {
                            body::throw_ctor(scope, "Request", "Request with GET/HEAD method cannot have body.");
                            return;
                        }
                        match crate::core::streams::readable_stream::as_stream(scope, b) {
                            Some(rs) => crate::core::streams::readable_stream::transfer_stream(scope, rs).into(),
                            None => v8::null(scope).into(),
                        }
                    }
                    None => v8::null(scope).into(),
                }
            }
            None => v8::null(scope).into(),
        },
    };
    evt_slot_write(scope, this, "body", body_v);
    // The readonly reflections a page reads off a Request but never sets, at their spec defaults.
    let empty = str_val(scope, "");
    evt_slot_write(scope, this, "destination", empty);
    // `referrer` was the constant "about:client", so `init.referrer` was never read at all: the two
    // subtests that pass "" and an invalid url both got the default back. Fetch's step (request.cc:466
    // and the block after it), measured on Chrome 150 from a `127.0.0.1` page:
    //   {referrer: ""}                    ""                              (no-referrer)
    //   {referrer: "r.html"}              "http://127.0.0.1:P/r.html"     (same origin: the url)
    //   {referrer: "http://example.test/x"} "about:client"                (cross origin: the client)
    //   {referrer: "http://:not a valid URL"}  TypeError
    let referrer = match &conv.referrer {
        Some(r) => r.clone(),
        None => from_request
            .and_then(|o| evt_slot_read(scope, o, "referrer"))
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_else(|| "about:client".to_string()),
    };
    let referrer = str_val(scope, &referrer);
    evt_slot_write(scope, this, "referrer", referrer);
    let keepalive = pick(scope, "keepalive").map(|v| v.boolean_value(scope)).unwrap_or(false);
    let kv: v8::Local<v8::Value> = v8::Boolean::new(scope, keepalive).into();
    evt_slot_write(scope, this, "keepalive", kv);
    // Every Request HAS a signal (Fetch, `new Request` step 29): a fresh AbortSignal that follows the
    // init's (or the input Request's) signal -- never `null`, never the init's own object. And `duplex`
    // is always "half", `targetAddressSpace` "unknown" unless the init named one (Chrome 150, the
    // Chrome oracle 2026-10-08: `[object AbortSignal]`, "half", "unknown" where these read null and
    // undefined).
    let signal: v8::Local<v8::Value> = match crate::core::dom::abort_controller::mint_signal(scope) {
        Some(sig) => {
            let parent = pick(scope, "signal")
                .filter(|v| !v.is_null_or_undefined())
                .or_else(|| from_request.and_then(|o| evt_slot_read(scope, o, "signal")));
            if let Some(p) = parent {
                crate::core::dom::abort_controller::follow_signal(scope, sig, p);
            }
            sig.into()
        }
        None => v8::null(scope).into(),
    };
    evt_slot_write(scope, this, "signal", signal);
    let duplex = str_val(scope, "half");
    evt_slot_write(scope, this, "duplex", duplex);
    let tas = pick(scope, "targetAddressSpace")
        .filter(|v| !v.is_undefined())
        .map(|v| v.to_rust_string_lossy(scope))
        .filter(|s| matches!(s.as_str(), "local" | "private" | "public" | "unknown" | "loopback"))
        .unwrap_or_else(|| "unknown".to_string());
    let tas = str_val(scope, &tas);
    evt_slot_write(scope, this, "targetAddressSpace", tas);
    for name in ["isHistoryNavigation", "isReloadNavigation"] {
        let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
        evt_slot_write(scope, this, name, f);
    }
    brand(scope, this, "Request");
    rv.set(this.into());
}

/// The bodies the implemented rows of [`REQUEST`] name.
pub fn install_request(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "clone" => branded_method_row(scope, proto, member, cb_request_clone),
        "textStream" => branded_method_row(scope, proto, member, cb_request_text_stream),
        "text" => branded_promise_method_row(scope, proto, member, cb_request_text),
        "json" => branded_promise_method_row(scope, proto, member, cb_request_json),
        "arrayBuffer" => branded_promise_method_row(scope, proto, member, cb_request_array_buffer),
        "bytes" => branded_promise_method_row(scope, proto, member, cb_request_bytes),
        "blob" => branded_promise_method_row(scope, proto, member, cb_request_blob),
        "bodyUsed" => branded_getter_row(scope, proto, member, cb_request_body_used),
        "url" => branded_getter_row(scope, proto, member, cb_request_url),
        other => debug_assert!(false, "install_request: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `Request` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order -- which is NOT the WebIDL order: sixteen
// slot-backed attributes, six methods, two more attributes, the nullable `body`, then two more
// methods. `[value, both]`: both captures record the host.js class as the constructor, WebIDL length 1.
webapi::interface! {
    "Request" [value, both] {
        ctor   "Request"                        len 1 required 1 = stub,
        attr   "method"              : Object readonly           = evt_get,
        attr   "url"                 : Object readonly           = install_request,
        attr   "headers"             : Object readonly           = evt_get,
        attr   "destination"         : Object readonly           = evt_get,
        attr   "referrer"            : Object readonly           = evt_get,
        attr   "referrerPolicy"      : Object readonly           = evt_get,
        attr   "mode"                : Object readonly           = evt_get,
        attr   "credentials"         : Object readonly           = evt_get,
        attr   "cache"               : Object readonly           = evt_get,
        attr   "redirect"            : Object readonly           = evt_get,
        attr   "integrity"           : Object readonly           = evt_get,
        attr   "keepalive"           : Object readonly           = evt_get,
        attr   "signal"              : Object readonly           = evt_get,
        attr   "duplex"              : Object readonly           = evt_get,
        attr   "isHistoryNavigation" : Object readonly           = evt_get,
        attr   "bodyUsed"            : Object readonly           = install_request,
        method "arrayBuffer"         : Promise len 0             = install_request,
        method "blob"                : Promise len 0             = install_request,
        method "clone"               : Object len 0              = install_request,
        method "formData"            : Promise len 0             = stub,
        method "json"                : Promise len 0             = install_request,
        method "text"                : Promise len 0             = install_request,
        attr   "targetAddressSpace"  : Object readonly           = evt_get,
        attr   "isReloadNavigation"  : Object readonly           = evt_get,
        attr   "body"                : Object readonly nullable  = evt_get_null,
        method "bytes"               : Promise len 0             = install_request,
        method "textStream"          : Object len 0              = install_request
    }
}

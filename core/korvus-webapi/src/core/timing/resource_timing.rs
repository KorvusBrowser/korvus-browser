//! `core/timing/resource_timing.rs` — `PerformanceResourceTiming`: the entries a document's fetches put
//! on its performance timeline.
//!
//! THE DEFECT. `performance.getEntriesByType('resource')` was always EMPTY -- nothing in the engine
//! ever made a resource entry, and the interface was the registry's D2 stub rows. salesforce.com's
//! RUM script reads it; so do boomerang, New Relic and Datadog's.
//!
//! MEASURED on stock `google-chrome` 150.0.7871.46, page `http://127.0.0.1:A/rt.html`, cross-origin
//! `http://127.0.0.2:B`, loopback server (HTTP/1.0):
//!
//! ```text
//! load                                    initiatorType    responseStatus  transferSize  nextHopProtocol  renderBlockingStatus
//! <link rel=stylesheet href=s.css>        link             200             27+300        http/1.0         blocking
//! <script src=sc.js> (parser, in head)    script           200             11+300        http/1.0         blocking
//! <script src=B/xsc.js>                   script           0               0             ""               blocking
//! <img src=i.png>                         img              200             99+300        http/1.0         non-blocking
//! <img src=B/xi.png>                      img              0               0             ""               non-blocking
//! <img src=B/ti.png> + Timing-Allow-Origin: *   img        0               399           http/1.0         non-blocking
//! css background url(bg.png)              css              200             399           http/1.0         non-blocking
//! fetch('t.txt'), body read               fetch            200             305           http/1.0
//! fetch(B/c.txt) cors, no TAO             fetch            200             0             ""
//! fetch(B/n.txt, no-cors)                 (no entry)
//! fetch('u.txt'), body never read         (no entry)       -- the entry is added when the body has been
//! fetch 204 / HEAD, body never read       one entry each      read to its end; a response with no body
//!                                                              has one at once
//! fetch to a refused port / CORS failure  fetch, status 0, transferSize 0, duration > 0
//! new XMLHttpRequest() (async and sync)   xmlhttprequest
//! new Image().src / appended <script>     img / script, non-blocking
//! a redirected fetch                      redirectStart = startTime > 0, fetchStart >= redirectEnd
//! entries                                 no own properties; toJSON keys in the order of TO_JSON below;
//!                                         getEntriesByType sorted by startTime
//! cross-origin without TAO                redirect/domainLookup/connect/request/response starts 0,
//!                                         sizes 0, nextHopProtocol ""; startTime/fetchStart/responseEnd kept
//! setResourceTimingBufferSize(2), 4 reads 2 entries kept, `resourcetimingbufferfull` fired twice
//! bufferfull handler that grows the buffer  every entry kept, the event fired once
//! PerformanceObserver({type:'resource', buffered:true})   delivers the entries already buffered
//! ```
//!
//! `transferSize` is `encodedBodySize + 300`, which is Resource Timing's own definition, not a number
//! this file chose. `responseStatus`/`contentType` are exposed only for a response the document may read
//! (same-origin, or CORS); the timing detail only when the Timing-Allow-Origin check passes.
//!
//! WHERE THE NUMBERS COME FROM. Every time is the transport's measurement of the real request
//! (`spec::FetchTiming`, on the process clock) converted to this realm's clock; a request that failed
//! before a response carries only what the realm itself observed -- when it asked and when it heard.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, get_prop, global_get, set_prop, str_val};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::live::method_row;
use crate::install::MemberDef;

/// Chrome's `toJSON()` key order (measured: `Object.keys(entry.toJSON())`, Chrome 152); `navigationId` only where
/// the realm has it (`performance::navigation_id_exposed`).
pub(crate) const TO_JSON: &[&str] = &[
    "name", "entryType", "startTime", "duration", "navigationId", "initiatorType", "deliveryType", "nextHopProtocol",
    "renderBlockingStatus", "contentType", "contentEncoding", "workerStart", "workerRouterEvaluationStart",
    "workerCacheLookupStart", "workerMatchedSourceType", "workerFinalSourceType", "redirectStart",
    "redirectEnd", "fetchStart", "domainLookupStart", "domainLookupEnd", "connectStart",
    "secureConnectionStart", "connectEnd", "requestStart", "responseStart", "firstInterimResponseStart",
    "finalResponseHeadersStart", "responseEnd", "transferSize", "encodedBodySize", "decodedBodySize",
    "responseStatus", "serverTiming",
];

/// The private slot holding a resource entry's uncoarsened request time (process clock), which
/// [`crate::core::timing::performance`] orders equal `startTime`s by.
pub(crate) const ORDER_SLOT: &str = "korvus.order";

/// What a finished (or failed) load contributes to its entry.
pub(crate) struct ResourceLoad {
    /// The URL the request was made for (the first of its URL list).
    pub name: String,
    pub initiator_type: String,
    pub render_blocking: bool,
    /// The transport's measurement, absent for a load that failed before a response.
    pub timing: Option<spec::FetchTiming>,
    /// This realm's clock when the load was asked for, and when its outcome was heard -- used only when
    /// there is no transport measurement.
    pub asked_ms: f64,
    pub heard_ms: f64,
    /// May the document read this response's status and content type (same-origin, or CORS)?
    pub readable: Option<bool>,
}

fn realm_clock(scope: &mut v8::PinScope, process: f64) -> f64 {
    // the current realm's clock: a frame's resources are timed from its own document's origin
    crate::core::timing::performance::realm_ms(scope, process)
}

/// This realm's origin, ASCII-serialized, read off its `location` (a document's or a worker's).
fn realm_origin(scope: &mut v8::PinScope) -> String {
    global_get(scope, "location")
        .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
        .and_then(|l| get_prop(scope, l, "href"))
        .and_then(|h| url::Url::parse(&h.to_rust_string_lossy(scope)).ok())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default()
}

fn origin_of(u: &str) -> String {
    url::Url::parse(u).map(|u| u.origin().ascii_serialization()).unwrap_or_default()
}

/// The fetches the MODULE LOADER has finished -- a module script's static imports, an `import()`'s modules -- waiting
/// for a scope to put them on the timeline. The loader's futures run inside deno_core's event
/// loop with no V8 scope, so they push the transport's measurement here, and [`drain_module_fetches`] adds the
/// entries: at every read of the timeline and on each turn of the load pipeline, so a page reading its entries in an
/// `import()`'s `then` finds the module's. Shared by `Rc` between the loader's state and `OpState`.
#[derive(Clone, Default)]
pub struct ModuleFetches(pub std::rc::Rc<std::cell::RefCell<Vec<spec::FetchTiming>>>);

/// Put every finished module fetch on this realm's timeline: `initiatorType` "script", not render-blocking. Chrome
/// 150 (measured): a module script's static import, an inline module's, and each `import()` have an entry each --
/// korvus had none. Only in the realm the loader fetches for (the page's, or a worker's own), never a frame's.
pub fn drain_module_fetches(scope: &mut v8::PinScope) {
    if !crate::core::html::browsing_context::in_page_realm(scope) {
        return;
    }
    let handle = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<ModuleFetches>().cloned()
    };
    let Some(handle) = handle else { return };
    let done: Vec<spec::FetchTiming> = std::mem::take(&mut *handle.0.borrow_mut());
    for t in done {
        let name = t.url_list.first().cloned().unwrap_or_default();
        // a module script is a CORS request: its response is the document's to read
        add(scope, ResourceLoad { name, initiator_type: "script".into(), render_blocking: false, timing: Some(t), asked_ms: 0.0, heard_ms: 0.0, readable: Some(true) });
    }
}

/// Add the entry for `load` to this realm's timeline.
pub(crate) fn add(scope: &mut v8::PinScope, load: ResourceLoad) {
    // The page's timeline is not made yet (a realm restored from the snapshot, before its first `performance` read):
    // the load waits as it is, and its entry is made when the timeline is (`performance::PendingTimeline`).
    if crate::core::timing::performance::timeline_deferred(scope) {
        crate::core::timing::performance::defer_resource(scope, load);
        return;
    }
    let origin = realm_origin(scope);
    let e = crate::install::live::mint_table(scope, "PerformanceResourceTiming");
    let put_str = |scope: &mut v8::PinScope, k: &str, v: &str| {
        let s = str_val(scope, v);
        evt_slot_write(scope, e, k, s);
    };
    put_str(scope, "name", &load.name);
    put_str(scope, "entryType", "resource");
    let id = crate::core::timing::performance::realm_navigation_id(scope);
    let id: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
    evt_slot_write(scope, e, "navigationId", id);
    put_str(scope, "initiatorType", &load.initiator_type);
    put_str(scope, "deliveryType", "");
    put_str(scope, "renderBlockingStatus", if load.render_blocking { "blocking" } else { "non-blocking" });
    put_str(scope, "contentEncoding", "");
    put_str(scope, "workerMatchedSourceType", "");
    put_str(scope, "workerFinalSourceType", "");
    let num = |scope: &mut v8::PinScope, k: &str, v: f64| {
        let n: v8::Local<v8::Value> = v8::Number::new(scope, v).into();
        evt_slot_write(scope, e, k, n);
    };
    for k in ["workerStart", "workerRouterEvaluationStart", "workerCacheLookupStart", "firstInterimResponseStart", "secureConnectionStart"] {
        num(scope, k, 0.0);
    }
    // `serverTiming`: the response's metrics, when Timing-Allow-Origin lets this realm see them (set below)
    let mut metrics: Vec<crate::core::timing::server_timing::Metric> = Vec::new();

    let start;
    match &load.timing {
        Some(t) => {
            let same_origin_chain = !t.url_list.is_empty() && t.url_list.iter().all(|u| origin_of(u) == origin);
            // Timing-Allow-Origin check: the document's own origin passes; otherwise `*` or the origin.
            let tao = same_origin_chain || t.timing_allow_origin.iter().any(|v| v == "*" || *v == origin);
            if tao {
                metrics = crate::core::timing::server_timing::parse(&t.server_timing);
            }
            let readable = load.readable.unwrap_or(same_origin_chain);
            let fetch_start = realm_clock(scope, t.fetch_start);
            let redirected = t.redirect_start > 0.0 && tao;
            start = if redirected { realm_clock(scope, t.redirect_start) } else { realm_clock(scope, t.start) };
            let response_end = realm_clock(scope, t.response_end);
            num(scope, "startTime", start);
            // The UNCOARSENED moment the request was asked for, the timeline's tie-break: two requests
            // the preload scanner issued a few microseconds apart share a coarsened `startTime`, and
            // the timeline still lists them in the order they left (`performance::sorted`).
            num(scope, ORDER_SLOT, if t.redirect_start > 0.0 && tao { t.redirect_start } else { t.start });
            num(scope, "duration", response_end - start);
            num(scope, "fetchStart", if redirected { fetch_start } else { start });
            num(scope, "responseEnd", response_end);
            let detail = |scope: &mut v8::PinScope, p: f64| if tao { realm_clock(scope, p) } else { 0.0 };
            let (rs, re) = if redirected { (realm_clock(scope, t.redirect_start), realm_clock(scope, t.redirect_end)) } else { (0.0, 0.0) };
            num(scope, "redirectStart", rs);
            num(scope, "redirectEnd", re);
            // The connection's phases (`performance::conn_phases`): the one this request opened, as the transport
            // measured it; a pooled one reports `fetchStart` for each (Resource Timing's rule for a reused
            // connection, and `secureConnectionStart` too over https); all 0 without Timing-Allow-Origin, and
            // `secureConnectionStart` 0 over a scheme that is not secure (Blink's `is_secure_transport`).
            if tao {
                let entry_fetch_start = if redirected { fetch_start } else { start };
                let secure = t.url_list.last().is_some_and(|u| u.starts_with("https:") || u.starts_with("wss:"));
                let c = crate::core::timing::performance::conn_phases(t, entry_fetch_start, secure, |p| realm_clock(scope, p));
                num(scope, "domainLookupStart", c.domain_lookup_start);
                num(scope, "domainLookupEnd", c.domain_lookup_end);
                num(scope, "connectStart", c.connect_start);
                num(scope, "connectEnd", c.connect_end);
                num(scope, "secureConnectionStart", c.secure_connection_start);
            } else {
                for k in ["domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd"] {
                    num(scope, k, 0.0);
                }
            }
            let req = detail(scope, t.request_start);
            num(scope, "requestStart", req);
            let resp = detail(scope, t.response_start);
            num(scope, "responseStart", resp);
            num(scope, "finalResponseHeadersStart", resp);
            // the wire bytes and the decoded ones: Chrome reports a compressed body's two sizes apart. An answer
            // the HTTP cache served moved nothing (`transferSize` 0), one a `304` confirmed moved only its headers
            // (300) -- Blink's `GetTransferSize` -- and either was delivered by the `cache`.
            let encoded = if tao { t.encoded_size as f64 } else { 0.0 };
            let transfer = match t.cache.as_str() {
                "local" => 0.0,
                "validated" => 300.0,
                _ => encoded + 300.0,
            };
            num(scope, "transferSize", if tao { transfer } else { 0.0 });
            if tao && !t.cache.is_empty() {
                put_str(scope, "deliveryType", "cache");
            }
            num(scope, "encodedBodySize", encoded);
            num(scope, "decodedBodySize", if tao { t.body_size as f64 } else { 0.0 });
            put_str(scope, "nextHopProtocol", if tao { &t.protocol } else { "" });
            num(scope, "responseStatus", if readable { t.status as f64 } else { 0.0 });
            let content_type = if readable { minimized_mime_type(scope, &t.content_type) } else { String::new() };
            put_str(scope, "contentType", &content_type);
        }
        None => {
            start = load.asked_ms;
            num(scope, "startTime", start);
            num(scope, "duration", (load.heard_ms - start).max(0.0));
            num(scope, "fetchStart", start);
            num(scope, "responseEnd", load.heard_ms.max(start));
            for k in ["redirectStart", "redirectEnd", "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd",
                      "requestStart", "responseStart", "finalResponseHeadersStart", "transferSize", "encodedBodySize",
                      "decodedBodySize", "responseStatus"] {
                num(scope, k, 0.0);
            }
            put_str(scope, "nextHopProtocol", "");
            put_str(scope, "contentType", "");
        }
    }
    let _ = start;
    let server_timing = crate::core::timing::server_timing::mint(scope, &metrics);
    evt_slot_write(scope, e, "serverTiming", server_timing.into());
    crate::core::timing::performance::add_resource_entry(scope, e);
}

/// Blink's `MinimizedMIMEType` (`http_parsers.cc`), what an entry's `contentType` reads (`resource_timing_utils.cc`,
/// for a navigation, a frame and every subresource alike): `mime` -- the response's type, lowercased and cut at its
/// parameters (`transport::mime::http_content_type`) -- folded to one name for the JavaScript, JSON and XML types,
/// kept when Blink supports it, "" otherwise. Chrome 150 over 64 types, three runs alike:
/// `font/woff2`, `application/pdf`, `application/wasm`, `text/csv`, `image/tiff`
/// and `video/quicktime` read ""; `application/x-javascript` reads "text/javascript", `application/ld+json`
/// "application/json", `text/xml` and `application/rss+xml` "application/xml" -- korvus read each type as sent.
pub(crate) fn minimized_mime_type(scope: &mut v8::PinScope, mime: &str) -> String {
    use crate::core::html::text_viewer::{IMAGE_TYPES, UNSUPPORTED_TEXT};
    if crate::core::html::html_script_element::is_javascript_mime_essence(mime) {
        return "text/javascript".into();
    }
    // `*+json` at kWildcardSlashAndTokens: a token, a slash, a token
    let tokens = mime.split_once('/').is_some_and(|(t, s)| transport::mime::is_http_token(t) && transport::mime::is_http_token(s));
    if mime == "application/json" || mime == "text/json" || (tokens && mime.ends_with("+json")) {
        return "application/json".into();
    }
    if mime == "image/svg+xml" {
        return mime.into();
    }
    // `application/*+xml` at kNone: the prefix and the suffix, not overlapping
    if mime == "text/xml" || mime == "application/xml" || (mime.len() >= 16 && mime.starts_with("application/") && mime.ends_with("+xml")) {
        return "application/xml".into();
    }
    // `IsSupportedMimeType`: an image Blink decodes, or a non-image type -- the MHTML ones, a `text/*` it renders, a
    // media container the worn browser plays (`canPlayType` not "")
    const NON_IMAGE: &[&str] = &["message/rfc822", "multipart/related", "multipart/x-mixed-replace"];
    let supported = (mime.starts_with("image/") && IMAGE_TYPES.contains(&mime))
        || NON_IMAGE.contains(&mime)
        || (mime.starts_with("text/") && !UNSUPPORTED_TEXT.contains(&mime))
        || (!mime.is_empty() && !crate::core::dom::media_codecs::can_play_type(scope, mime).is_empty());
    if supported { mime.into() } else { String::new() }
}

// ── reading a transport measurement back off a JS record ─────────────────────────────────────────

/// The `timing` member of an `op_fetch` / subresource record, as the transport serialized it.
pub(crate) fn timing_of(scope: &mut v8::PinScope, record: v8::Local<v8::Object>) -> Option<spec::FetchTiming> {
    let t = get_prop(scope, record, "timing").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let n = |scope: &mut v8::PinScope, k: &str| get_prop(scope, t, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let s = |scope: &mut v8::PinScope, k: &str| get_prop(scope, t, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let list = |scope: &mut v8::PinScope, k: &str| -> Vec<String> {
        get_prop(scope, t, k)
            .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
            .map(|a| (0..a.length()).filter_map(|i| a.get_index(scope, i)).map(|v| v.to_rust_string_lossy(scope)).collect())
            .unwrap_or_default()
    };
    Some(spec::FetchTiming {
        url_list: list(scope, "urlList"),
        start: n(scope, "start"),
        redirect_start: n(scope, "redirectStart"),
        redirect_end: n(scope, "redirectEnd"),
        fetch_start: n(scope, "fetchStart"),
        request_start: n(scope, "requestStart"),
        response_start: n(scope, "responseStart"),
        response_end: n(scope, "responseEnd"),
        protocol: s(scope, "protocol"),
        body_size: n(scope, "bodySize") as u64,
        encoded_size: n(scope, "encodedSize") as u64,
        status: n(scope, "status") as u16,
        content_type: s(scope, "contentType"),
        timing_allow_origin: list(scope, "timingAllowOrigin"),
        server_timing: s(scope, "serverTiming"),
        redirect_count: n(scope, "redirectCount") as u32,
        critical_ch_restart: n(scope, "criticalChRestart"),
        domain_lookup_start: n(scope, "domainLookupStart"),
        domain_lookup_end: n(scope, "domainLookupEnd"),
        connect_start: n(scope, "connectStart"),
        ssl_start: n(scope, "sslStart"),
        connect_end: n(scope, "connectEnd"),
        cache: s(scope, "cache"),
    })
}

fn now(scope: &mut v8::PinScope) -> f64 {
    crate::core::timing::performance::clock_now_ms(scope)
}

/// Resolve `url` against this realm's location, for the name of an entry whose request never produced a
/// URL list.
fn absolute(scope: &mut v8::PinScope, url: &str) -> String {
    // the realm's own base, never the page's `location.href`
    let base = crate::core::html::browsing_context::realm_base_url(scope);
    url::Url::parse(&base).ok().and_then(|b| b.join(url).ok()).map(|u| u.to_string()).unwrap_or_else(|| url.to_string())
}

// ── fetch() ──────────────────────────────────────────────────────────────────────────────────────

const PENDING_SLOT: &str = "korvusResourceTiming";

/// A `fetch()` response arrived. An opaque one never gets an entry; one with an empty body gets it now;
/// any other gets it when its body has been read to the end ([`body_consumed`]).
pub(crate) fn fetch_response(scope: &mut v8::PinScope, response: v8::Local<v8::Object>, record: v8::Local<v8::Object>, asked_ms: f64) {
    let ty = get_prop(scope, record, "type").map(|t| t.to_rust_string_lossy(scope)).unwrap_or_default();
    if ty == "opaque" || ty == "opaqueredirect" {
        return;
    }
    let Some(timing) = timing_of(scope, record) else { return };
    if timing.body_size == 0 {
        let name = timing.url_list.first().cloned().unwrap_or_default();
        let heard = now(scope);
        add(scope, ResourceLoad { name, initiator_type: "fetch".into(), render_blocking: false, timing: Some(timing), asked_ms, heard_ms: heard, readable: Some(true) });
        return;
    }
    let t = get_prop(scope, record, "timing").unwrap_or_else(|| v8::undefined(scope).into());
    evt_slot_write(scope, response, PENDING_SLOT, t);
}

/// A beacon's response is in, its body too when it has one: its entry now, `initiatorType` `beacon` -- nothing reads a
/// beacon's body, so it cannot wait for that (Chrome 150 lists every beacon, `beacon`).
pub(crate) fn beacon_response(scope: &mut v8::PinScope, record: v8::Local<v8::Object>, asked_ms: f64) {
    let opaque = get_prop(scope, record, "type").is_some_and(|t| t.to_rust_string_lossy(scope).starts_with("opaque"));
    let Some(timing) = timing_of(scope, record) else { return };
    let name = timing.url_list.first().cloned().unwrap_or_default();
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: "beacon".into(), render_blocking: false, timing: Some(timing), asked_ms, heard_ms: heard, readable: Some(!opaque) });
}

/// A `Response`'s body was read to its end: its pending `fetch` entry, if any, is added now.
pub(crate) fn body_consumed(scope: &mut v8::PinScope, response: v8::Local<v8::Object>) {
    let Some(t) = evt_slot_read(scope, response, PENDING_SLOT).filter(|v| v.is_object()) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    evt_slot_write(scope, response, PENDING_SLOT, undef);
    let holder = v8::Object::new(scope);
    set_prop(scope, holder, "timing", t);
    let Some(timing) = timing_of(scope, holder) else { return };
    let name = timing.url_list.first().cloned().unwrap_or_default();
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: "fetch".into(), render_blocking: false, timing: Some(timing), asked_ms: heard, heard_ms: heard, readable: Some(true) });
}

/// A `fetch()` (or XHR) failed: an entry with only the realm's own two readings.
pub(crate) fn request_failed(scope: &mut v8::PinScope, url: &str, initiator_type: &str, asked_ms: f64) {
    let name = absolute(scope, url);
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: initiator_type.into(), render_blocking: false, timing: None, asked_ms, heard_ms: heard, readable: Some(false) });
}

/// An XHR response arrived (XHR reads its whole body before `load`): its entry now.
pub(crate) fn xhr_response(scope: &mut v8::PinScope, record: v8::Local<v8::Object>, asked_ms: f64) {
    let Some(timing) = timing_of(scope, record) else { return };
    let name = timing.url_list.first().cloned().unwrap_or_default();
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: "xmlhttprequest".into(), render_blocking: false, timing: Some(timing), asked_ms, heard_ms: heard, readable: Some(true) });
}

/// A dedicated worker's own script arrived (on the worker's thread): its entry on THIS document's
/// timeline, `initiatorType` `other` (measured).
pub(crate) fn worker_script_loaded(scope: &mut v8::PinScope, timing: spec::FetchTiming) {
    let name = timing.url_list.first().cloned().unwrap_or_default();
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: "other".into(), render_blocking: false, timing: Some(timing), asked_ms: heard, heard_ms: heard, readable: None });
}

// ── element- and stylesheet-induced loads ────────────────────────────────────────────────────────

/// The element about to start a load, noted by `core/html/subresource.rs` right before it calls the
/// loader, so the load that follows is attributed to it.
struct PendingInitiator {
    initiator_type: String,
    render_blocking: bool,
    /// the element, whose own document's timeline the entry goes on ([`watch_load`])
    id: u32,
}

/// `core/html/subresource.rs`'s hook: element `id` (tag `tag`) is starting a load.
pub(crate) fn note_initiator(scope: &mut v8::PinScope, id: u32, tag: &str) {
    let blocking = render_blocking(scope, id, tag);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(PendingInitiator { initiator_type: tag.to_string(), render_blocking: blocking, id });
}

/// Is element `id`'s (tag `tag`) load render-blocking -- its entry's `renderBlockingStatus`?
fn render_blocking(scope: &mut v8::PinScope, id: u32, tag: &str) -> bool {
    // A classic `<script src>` without async/defer and a stylesheet `<link>` are render-blocking while
    // the parser is still running (measured: both in <head> read "blocking"; the same elements
    // appended by script after load read "non-blocking").
    let parsing = crate::core::html::browsing_context::doc_readiness(scope).as_str() == "loading";
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let blocking = parsing
        && match tag {
            "script" => st
                .try_borrow::<crate::HostDom>()
                .map(|d| d.0.get_attribute(id, "async").is_none() && d.0.get_attribute(id, "defer").is_none() && d.0.get_attribute(id, "type").map(|t| t != "module").unwrap_or(true))
                .unwrap_or(false),
            "link" => st
                .try_borrow::<crate::HostDom>()
                .map(|d| d.0.get_attribute(id, "rel").map(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet"))).unwrap_or(false))
                .unwrap_or(false),
            _ => false,
        };
    blocking
}

/// Element `id`'s (tag `tag`) load of `url` was REFUSED before it reached the network -- by CSP, or as
/// dangling markup -- and still gets its entry, now: Blink's `ResourceFetcher::RequestResource` stores the initiator
/// for the blocked request and adds its timing with `response_end` = now (resource_fetcher.cc 1433-1446), so the entry
/// starts and ends at the refusal -- duration 0, every phase, size and status 0, `nextHopProtocol` and `contentType`
/// "". Chrome 150 x3: an `<img>`, a `<script>`, a stylesheet and a preload `<link>` refused by the page's
/// CSP each have one; korvus had none. On the element's own document's timeline, as an allowed load's ([`watch_load`]).
pub(crate) fn refused_load(scope: &mut v8::PinScope, id: u32, tag: &str, url: &str) {
    let blocking = render_blocking(scope, id, tag);
    refused_load_blocking(scope, id, tag, url, blocking);
}

/// [`refused_load`] with the load's render-blocking state stated: a parser `<script>`'s is the fetch pass's (in
/// `<head>`, classic, neither `async` nor `defer`), Chrome 150's for both of a refused head script's entries.
pub(crate) fn refused_load_blocking(scope: &mut v8::PinScope, id: u32, tag: &str, url: &str, blocking: bool) {
    match crate::core::html::browsing_context::realm_of_element(scope, id) {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            refused_here(scope, tag, blocking, url);
        }
        None => refused_here(scope, tag, blocking, url),
    }
}

/// [`refused_load`] for a request no element made, on the current realm's timeline: a `FontFace`'s or a style sheet's
/// (`css`), an XHR's (`xmlhttprequest`). Chrome 150 x3: a `font-src`-refused `FontFace` source, an
/// `img-src`-refused `background-image` and a `connect-src`-refused XHR each have one; a refused `fetch()` has none
/// (Fetch's main fetch refuses it before Blink's resource fetcher is asked).
pub(crate) fn refused_request(scope: &mut v8::PinScope, initiator: &str, url: &str) {
    refused_here(scope, initiator, false, url);
}

/// Blink's frame FALLBACK timing (`HTMLFrameOwnerElement::fallback_timing_info_`): a frame navigation that produces no
/// resource timing of its own -- one CSP refused, which ends in an error page -- has its entry made when the frame
/// element's `load` is dispatched, from the navigation's start to then. Started here, at the refusal.
#[derive(Default)]
struct FrameFallbackTimings(std::collections::HashMap<u32, (String, String, f64)>);

pub(crate) fn frame_fallback_start(scope: &mut v8::PinScope, el: u32, tag: &str, url: &str) {
    let asked = match crate::core::html::browsing_context::realm_of_element(scope, el) {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            now(scope)
        }
        None => now(scope),
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.has::<FrameFallbackTimings>() {
        st.put(FrameFallbackTimings::default());
    }
    st.borrow_mut::<FrameFallbackTimings>().0.insert(el, (tag.to_string(), url.to_string(), asked));
}

/// A new document: no element id of the last one names a frame any more.
pub(crate) fn reset_frame_fallbacks(st: &mut deno_core::OpState) {
    let _ = st.try_take::<FrameFallbackTimings>();
}

/// The frame element `el`'s `load` is about to be dispatched: its fallback entry, if it has one, goes on its document's
/// timeline now (`ReportFallbackResourceTimingIfNeeded`, html_frame_owner_element.cc 586-600).
pub(crate) fn frame_fallback_report(scope: &mut v8::PinScope, el: u32) {
    let pending = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<FrameFallbackTimings>().and_then(|m| m.0.remove(&el))
    };
    let Some((tag, url, asked)) = pending else { return };
    let add_here = |scope: &mut v8::PinScope| {
        let name = absolute(scope, &url);
        let heard = now(scope);
        add(scope, ResourceLoad { name, initiator_type: tag.clone(), render_blocking: false, timing: None, asked_ms: asked, heard_ms: heard, readable: Some(false) });
    };
    match crate::core::html::browsing_context::realm_of_element(scope, el) {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            add_here(scope);
        }
        None => add_here(scope),
    }
}

/// [`refused_load`]'s entry, on the current realm's timeline.
fn refused_here(scope: &mut v8::PinScope, initiator: &str, blocking: bool, url: &str) {
    let name = absolute(scope, url);
    let at = now(scope);
    add(scope, ResourceLoad { name, initiator_type: initiator.into(), render_blocking: blocking, timing: None, asked_ms: at, heard_ms: at, readable: Some(false) });
}

/// `core/html/subresource.rs`'s other hook: the loader was called for `url` (`dest`/`mode`) and answered
/// `promise`. An element load is attributed to the element [`note_initiator`] named; a load nobody noted
/// is a stylesheet's own (`@import`, `url()`, `@font-face`), which is `css`; a document navigation that
/// no frame element started makes no entry.
pub(crate) fn watch_load(scope: &mut v8::PinScope, promise: v8::Local<v8::Promise>, url: &str, dest: &str, mode: &str) {
    let (pending, dirty) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        (st.try_take::<PendingInitiator>(), st.has::<OriginDirtyLoad>())
    };
    if dirty && pending.is_none() {
        return;
    }
    let (initiator, blocking, el) = match pending {
        Some(p) => (p.initiator_type, p.render_blocking, Some(p.id)),
        None if matches!(dest, "image" | "font" | "style") => ("css".to_string(), false, None),
        None => return,
    };
    // THE ELEMENT'S OWN DOCUMENT'S TIMELINE: a frame document's `<img>` is the frame's load, on the frame's
    // `performance` -- its clock, its origin's Timing-Allow-Origin check, its base for the name -- not the page's.
    // The reactions are made in the element's realm, so they run there and `add` puts the entry on that realm's
    // timeline. Chrome 150 (a frame holding one <img>): the page lists the frame's `iframe` entry only,
    // the frame its `img`; korvus listed both on the page and none on the frame -- and when a frame's
    // load waits for its image, a frame showing an image (its viewer document's <img>) gave the page two entries
    // before its `load` (WPT resource-timing/iframe-non-html, "image iframes": 1 expected).
    match el.and_then(|id| crate::core::html::browsing_context::realm_of_element(scope, id)) {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            watch_load_here(scope, promise, url, &initiator, blocking, mode);
        }
        None => watch_load_here(scope, promise, url, &initiator, blocking, mode),
    }
}

/// A style sheet's load is being started from an ORIGIN-DIRTY rule (`sheet_loads::dirty_bases`): it gets no entry, as
/// Blink adds none for a request `SetFromOriginDirtyStyleSheet` marks (resource_fetcher.cc 3222). Set around the
/// loader call by `sheet_loads` and `core/fonts/font_face.rs`, and cleared after it -- a load that joins a request
/// already made calls no loader.
struct OriginDirtyLoad;

pub(crate) fn from_origin_dirty_sheet(scope: &mut v8::PinScope, dirty: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if dirty {
        st.put(OriginDirtyLoad);
    } else {
        let _ = st.try_take::<OriginDirtyLoad>();
    }
}

/// [`watch_load`]'s reactions, made in the realm whose timeline the entry goes on (the current one).
fn watch_load_here(scope: &mut v8::PinScope, promise: v8::Local<v8::Promise>, url: &str, initiator: &str, blocking: bool, mode: &str) {
    let asked = now(scope);
    let items = [
        str_val(scope, url),
        str_val(scope, initiator),
        v8::Boolean::new(scope, blocking).into(),
        v8::Number::new(scope, asked).into(),
        v8::Boolean::new(scope, mode == "cors").into(),
    ];
    let data: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &items).into();
    let ok = build_fn_data(scope, cb_load_ok, "", 1, data);
    let err = build_fn_data(scope, cb_load_err, "", 1, data);
    let _ = promise.then2(scope, ok, err);
}

fn unpack(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(String, String, bool, f64, bool)> {
    let a = v8::Local::<v8::Array>::try_from(data).ok()?;
    let url = a.get_index(scope, 0)?.to_rust_string_lossy(scope);
    let initiator = a.get_index(scope, 1)?.to_rust_string_lossy(scope);
    let blocking = a.get_index(scope, 2)?.boolean_value(scope);
    let asked = a.get_index(scope, 3)?.number_value(scope)?;
    let cors = a.get_index(scope, 4)?.boolean_value(scope);
    Some((url, initiator, blocking, asked, cors))
}

fn cb_load_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((url, initiator, blocking, asked, cors)) = unpack(scope, __body_data) else { return };
    let Ok(resp) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let timing = timing_of(scope, resp);
    let name = timing.as_ref().and_then(|t| t.url_list.first().cloned()).unwrap_or_else(|| absolute(scope, &url));
    let heard = now(scope);
    // A no-cors load of a cross-origin URL is opaque to the document; a CORS-mode one (`crossorigin`) is not.
    let readable = if cors { Some(true) } else { None };
    add(scope, ResourceLoad { name, initiator_type: initiator, render_blocking: blocking, timing, asked_ms: asked, heard_ms: heard, readable });
}

fn cb_load_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((url, initiator, blocking, asked, _)) = unpack(scope, __body_data) else { return };
    let name = absolute(scope, &url);
    let heard = now(scope);
    add(scope, ResourceLoad { name, initiator_type: initiator, render_blocking: blocking, timing: None, asked_ms: asked, heard_ms: heard, readable: Some(false) });
}

// ── the interface ────────────────────────────────────────────────────────────────────────────────

/// `entry.toJSON()`: every attribute, read through the receiver, in Chrome's order.
fn cb_resource_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let o = v8::Object::new(scope);
    let nav = crate::core::timing::performance::navigation_id_exposed(scope);
    for k in TO_JSON {
        if *k == "navigationId" && !nav {
            continue;
        }
        // the metrics as plain objects, each its own `toJSON()` (Blink's `BuildJSONValue`)
        if *k == "serverTiming" {
            let l = crate::core::timing::server_timing::json_list(scope, this);
            set_prop(scope, o, k, l.into());
            continue;
        }
        if let Some(v) = get_prop(scope, this, k) {
            set_prop(scope, o, k, v);
        }
    }
    rv.set(o.into());
}

/// Create the `PerformanceResourceTiming` global from its table, at the registry walk's position for it
/// (see `install::live::ORDERED_HAND`). Its parent is chained to `PerformanceEntry` when the timeline
/// family installs ([`chain_to_performance_entry`]); `PerformanceNavigationTiming`, a registry row,
/// chains onto this one in the walk.
pub fn create_performance_resource_timing<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    ledger: &mut crate::install::installer::InstallLedger,
) {
    use crate::install::installer::{self, Flavor};
    let parent = crate::install::live::rows::parent_of(scope, "PerformanceEntry");
    let built = installer::create_interface(scope, global, &PERFORMANCERESOURCETIMING, "PerformanceResourceTiming", parent, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] resource-timing install gap: {}.{}", g.interface, g.member);
    }
}

/// Splice `PerformanceResourceTiming` (interface object and prototype) onto the realm's
/// `PerformanceEntry`, once the timeline family has built it.
pub(crate) fn chain_to_performance_entry<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let get = |scope: &mut v8::PinScope<'s, '_>, name: &str| -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
        let k = v8::String::new(scope, name)?;
        let c = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        let p = get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        Some((c, p))
    };
    let (Some((rc, rp)), Some((ec, ep))) = (get(scope, "PerformanceResourceTiming"), get(scope, "PerformanceEntry")) else { return };
    rp.set_prototype(scope, ep.into());
    rc.set_prototype(scope, ec.into());
}

/// The body the one method row of [`PERFORMANCERESOURCETIMING`] names.
pub fn install_resource_timing(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_resource_to_json),
        // a new frozen array of the same metric objects on every read (`server_timing.rs`)
        "serverTiming" => crate::install::live::getter_row(scope, proto, member, crate::core::timing::server_timing::cb_entry_server_timing),
        other => debug_assert!(false, "install_resource_timing: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `PerformanceResourceTiming` block of `install/generated_registry.rs` (the
// capture's order; `[Exposed=(Window,Worker)]`). Every attribute reads the private slot [`add`] writes,
// so an entry has no own property (Chrome: `Object.getOwnPropertyNames(entry)` is `[]`). The interface is
// abstract: only a timeline mints entries.
webapi::interface! {
    "PerformanceResourceTiming" : "PerformanceEntry" [interface, both] {
        ctor   "PerformanceResourceTiming"                     len 0 = stub,
        attr   "initiatorType"               : DomString readonly = slot_get,
        attr   "nextHopProtocol"             : DomString readonly = slot_get,
        attr   "deliveryType"                : DomString readonly = slot_get,
        attr   "workerStart"                 : Double readonly    = slot_get,
        attr   "redirectStart"               : Double readonly    = slot_get,
        attr   "redirectEnd"                 : Double readonly    = slot_get,
        attr   "fetchStart"                  : Double readonly    = slot_get,
        attr   "domainLookupStart"           : Double readonly    = slot_get,
        attr   "domainLookupEnd"             : Double readonly    = slot_get,
        attr   "connectStart"                : Double readonly    = slot_get,
        attr   "connectEnd"                  : Double readonly    = slot_get,
        attr   "secureConnectionStart"       : Double readonly    = slot_get,
        attr   "requestStart"                : Double readonly    = slot_get,
        attr   "responseStart"               : Double readonly    = slot_get,
        attr   "responseEnd"                 : Double readonly    = slot_get,
        attr   "transferSize"                : Integer readonly   = slot_get,
        attr   "encodedBodySize"             : Integer readonly   = slot_get,
        attr   "decodedBodySize"             : Integer readonly   = slot_get,
        attr   "serverTiming"                : Sequence readonly  = install_resource_timing,
        attr   "renderBlockingStatus"        : Any readonly       = slot_get,
        attr   "responseStatus"              : Integer readonly   = slot_get,
        attr   "contentType"                 : DomString readonly = slot_get,
        attr   "contentEncoding"             : DomString readonly = slot_get,
        attr   "finalResponseHeadersStart"   : Double readonly    = slot_get,
        attr   "firstInterimResponseStart"   : Double readonly    = slot_get,
        method "toJSON"                      : Any len 0          = install_resource_timing,
        attr   "workerRouterEvaluationStart" : Double readonly    = slot_get,
        attr   "workerCacheLookupStart"      : Double readonly    = slot_get,
        attr   "workerMatchedSourceType"     : Any readonly       = slot_get,
        attr   "workerFinalSourceType"       : Any readonly       = slot_get
    }
}

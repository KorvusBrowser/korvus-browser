//! `core/xmlhttprequest/xml_http_request.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::modules::crypto::shared::{crypto_num};
use crate::binding::protos::{build_fn_data, get_prop, global_get, set_prop, str_val};
use crate::install::live::{getter_row, method_row, pair_row};
use crate::install::MemberDef;
use crate::modules::indexeddb::backend::{idb_fire, idb_new_event};

#[repr(C)]
#[derive(CppgcBase)]
pub struct XMLHttpRequest {
    method: std::cell::RefCell<String>,
    url: std::cell::RefCell<String>,
    /// The realm's clock when `send()` issued the request, for its resource-timing entry.
    rt_asked: std::cell::Cell<f64>,
    req_headers: std::cell::RefCell<Vec<(String, String)>>,
    resp_headers: std::cell::RefCell<Vec<(String, String)>>,
    ready_state: std::cell::Cell<u8>,
    status: std::cell::Cell<u16>,
    status_text: std::cell::RefCell<String>,
    /// The response body, BYTE FOR BYTE. It was `response_text: String`, filled from a
    /// body the transport had already decoded, so `responseType = "arraybuffer"` had nothing to answer
    /// with (`response` was the text) and `overrideMimeType('…; charset=x-user-defined')` -- the
    /// pre-`arraybuffer` idiom for reading binary through XHR -- read U+FFFD where Chrome reads
    /// U+F780 + byte. Every getter decodes from here, the way the XHR spec's "text response" does.
    response_bytes: std::cell::RefCell<Vec<u8>>,
    /// `responseText`'s decode of `response_bytes`, kept until the bytes or the override change: a
    /// page polling `responseText` in `onprogress` must not re-decode the whole body every call.
    text_cache: std::cell::RefCell<Option<String>>,
    /// `open(method, url, false)`: the spec's "synchronous flag".
    sync: std::cell::Cell<bool>,
    response_url: std::cell::RefCell<String>,
    aborted: std::cell::Cell<bool>,
    // The eight members the capture records and this class did not have. They are read by real content
    // -- a telemetry beacon sets `withCredentials` and `onreadystatechange` before it sends anything --
    // and `typeof xhr.timeout === "undefined"` is a one-expression difference from Brave 151.
    timeout: std::cell::Cell<u32>,
    with_credentials: std::cell::Cell<bool>,
    /// "" | "arraybuffer" | "blob" | "document" | "json" | "text". "" means "respond with text".
    response_type: std::cell::RefCell<String>,
    /// `overrideMimeType()`'s argument, which decides how `responseXML` parses. `None` = use the
    /// response's own Content-Type.
    override_mime: std::cell::RefCell<Option<String>>,

    // ── the event model's state (the spec's flags, one field each) ───────────────────────────────
    /// The spec's "send() flag". `abort()` on an OPENED request whose send() flag is UNSET is a
    /// no-op that fires nothing -- which is the whole point of `xhr/abort-event-abort` -- so
    /// "opened" alone is not enough to decide.
    send_flag: std::cell::Cell<bool>,
    /// The spec's "upload complete flag": unset only while a request body is still in flight. A
    /// request with no body has it SET from the start, which is why `send(null)` fires no upload
    /// events at all.
    upload_complete: std::cell::Cell<bool>,
    /// The spec's "upload listener flag", latched at send() time. Registering an upload listener
    /// AFTER send() must not retroactively produce upload events.
    upload_listener: std::cell::Cell<bool>,
    /// Byte length of the body handed to `send()` -- the `total` of every upload progress event.
    upload_total: std::cell::Cell<f64>,
    /// Bumped by every `open()` and every `send()`. A fetch continuation or a timeout timer that
    /// belongs to a superseded request compares its captured value against this and does nothing:
    /// without it, `open(); send(); abort(); open(); send()` lets the FIRST response drive the
    /// second request's state machine.
    generation: std::cell::Cell<u32>,
    /// When `send()` started, so a `timeout` assigned mid-flight can be re-armed against the elapsed
    /// time rather than restarted (the spec resets the timer, it does not extend it).
    send_started: std::cell::Cell<Option<std::time::Instant>>,
    /// Bumped by every timer arm. A timer whose epoch is stale has been superseded by a later
    /// `xhr.timeout = ...` and must not fire.
    timer_epoch: std::cell::Cell<u32>,
    /// "this's response is a network error" -- set by the request error steps, cleared when a real
    /// response is copied in. `responseXML` returns null for a null body, and an aborted request has
    /// one: without this, `abort()` left an empty responseText that the default `text/xml` fallback
    /// happily parsed into an empty Document.
    network_error: std::cell::Cell<bool>,

    // ── the response as it streams in ─────────────────────────────────────
    /// The head `op_fetch_head` answered, held until the request enters HEADERS_RECEIVED -- when the first
    /// body bytes are DELIVERED, or the body ends with none (Blink's `DidReceiveData` / `DidFinishLoading`),
    /// not when the headers arrive: Chrome 150 reads `readyState 1`, `status 0` and no headers in between.
    pending_head: std::cell::RefCell<Option<XhrHead>>,
    /// The body behind that head while it is still being read, so a request that ends stops reading it.
    body_id: std::cell::Cell<Option<u32>>,
    /// Blink's `received_length_`: the body bytes delivered, every progress event's `loaded`.
    received: std::cell::Cell<f64>,
    /// While the network service sniffs the response's type (`mime_sniff`): the type it sniffs against, the
    /// response's URL, and the bytes it holds back until it has enough or the body ends.
    sniffing: std::cell::RefCell<Option<(String, String, Vec<u8>)>>,
    // Blink's `XMLHttpRequestProgressEventThrottle`, one per object for its whole life (an `open()` does
    // not reset it): `progress` at most every 50 ms ...
    /// ... while its one-shot timer runs ...
    throttle_active: std::cell::Cell<bool>,
    /// ... a `progress` that came meanwhile is held, dispatched when the timer fires or flushed at DONE ...
    throttle_deferred: std::cell::Cell<Option<(f64, f64, bool)>>,
    /// ... the timer's identity, bumped by every start and stop so a superseded one does nothing ...
    throttle_epoch: std::cell::Cell<u32>,
    /// ... and `has_dispatched_progress_progress_event_`: a LOADING `progress` after the first is
    /// preceded by a `readystatechange`.
    progress_dispatched: std::cell::Cell<bool>,
}

/// A response head held until HEADERS_RECEIVED ([`XMLHttpRequest::pending_head`]).
struct XhrHead {
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
}

pub(crate) trait XMLHttpRequestBehavior {
   fn construct() -> XMLHttpRequest where Self: Sized { stub!("XMLHttpRequest::constructor") }
}

#[webapi(base, own)]
impl XMLHttpRequest {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<XMLHttpRequest, ::deno_error::JsErrorBox> { dispatch!("XMLHttpRequest::constructor", <XMLHttpRequest as XMLHttpRequestBehavior>::construct()) }
}

impl XMLHttpRequestBehavior for XMLHttpRequest {
    fn construct() -> XMLHttpRequest { XMLHttpRequest {
            method: std::cell::RefCell::new("GET".to_string()),
            url: std::cell::RefCell::new(String::new()),
            rt_asked: std::cell::Cell::new(0.0),
            req_headers: std::cell::RefCell::new(Vec::new()),
            resp_headers: std::cell::RefCell::new(Vec::new()),
            ready_state: std::cell::Cell::new(0),
            status: std::cell::Cell::new(0),
            status_text: std::cell::RefCell::new(String::new()),
            response_bytes: std::cell::RefCell::new(Vec::new()),
            text_cache: std::cell::RefCell::new(None),
            sync: std::cell::Cell::new(false),
            response_url: std::cell::RefCell::new(String::new()),
            aborted: std::cell::Cell::new(false),
            timeout: std::cell::Cell::new(0),
            with_credentials: std::cell::Cell::new(false),
            response_type: std::cell::RefCell::new(String::new()),
            override_mime: std::cell::RefCell::new(None),
            send_flag: std::cell::Cell::new(false),
            upload_complete: std::cell::Cell::new(true),
            upload_listener: std::cell::Cell::new(false),
            upload_total: std::cell::Cell::new(0.0),
            generation: std::cell::Cell::new(0),
            send_started: std::cell::Cell::new(None),
            timer_epoch: std::cell::Cell::new(0),
            network_error: std::cell::Cell::new(false),
            pending_head: std::cell::RefCell::new(None),
            body_id: std::cell::Cell::new(None),
            received: std::cell::Cell::new(0.0),
            sniffing: std::cell::RefCell::new(None),
            throttle_active: std::cell::Cell::new(false),
            throttle_deferred: std::cell::Cell::new(None),
            throttle_epoch: std::cell::Cell::new(0),
            progress_dispatched: std::cell::Cell::new(false),
        } }
}
/// Resolve `Deno.core.ops.<name>` to a callable (cross-extension op access from Rust).
pub(crate) fn deno_op<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    let ops = ops_object(scope)?;
    get_prop(scope, ops, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
}
/// The `Deno.core.ops` object. Preferred source is a hidden Private slot on the global (stashed by
/// `op_stash_ops` before the `Deno` global is deleted for stealth); falls back to the live `Deno`
/// global (before deletion / if never stashed). Invisible to page JS either way.
pub(crate) fn ops_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    // One reader, beside the writer (`binding/hidden.rs`): this was a second copy of that lookup.
    crate::binding::hidden::ops_object(scope)
}

/// Fire one XHR event: its on<type> handler + dispatchEvent (an Event with target = the xhr).
/// `readystatechange` is the ONLY XHR event that is a plain `Event`; every other one is a
/// ProgressEvent and goes through `xhr_fire_progress`.
fn xhr_fire(scope: &mut v8::PinScope, xhr: v8::Local<v8::Value>, ty: &str) {
    if let Some(ev) = idb_new_event(scope, ty, xhr, false) { idb_fire(scope, xhr, ev, ty); }
}

/// Fire a real `ProgressEvent` at `target` -- the "fire a progress event named e" of the XHR spec.
///
/// Every event this file used to fire was a plain `Event`, so `e instanceof ProgressEvent` was false
/// and `e.loaded` / `e.total` / `e.lengthComputable` were all undefined. The suite reads those on
/// loadstart, progress, abort, error, timeout, load and loadend, in both realms; the constructor is
/// looked up off the global so the window class (host.js) and the worker class (worker-host.js) each
/// serve their own realm without this code knowing which one it is in.
fn xhr_fire_progress(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Value>,
    ty: &str,
    loaded: f64,
    total: f64,
    length_computable: bool,
) {
    let init = crate::binding::protos::init_dict(scope);
    let lc = v8::Boolean::new(scope, length_computable);
    set_prop(scope, init, "lengthComputable", lc.into());
    let l = crypto_num(scope, loaded);
    set_prop(scope, init, "loaded", l);
    let t = crypto_num(scope, total);
    set_prop(scope, init, "total", t);
    let ty_v = str_val(scope, ty);
    let ev: v8::Local<v8::Value> = match crate::binding::protos::iface_object(scope, "ProgressEvent")
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
        .and_then(|c| c.new_instance(scope, &[ty_v, init.into()]))
    {
        Some(o) => {
            // `target` is a prototype accessor over a private slot, not a property: assigning to it
            // would hit a getter-only descriptor and silently do nothing.
            crate::core::events::event::evt_slot_write(scope, o, "target", target);
            o.into()
        }
        // No ProgressEvent in this realm at all (should not happen; both realms define one). A plain
        // Event is a worse answer than a ProgressEvent and a much better one than no event.
        None => match idb_new_event(scope, ty, target, false) { Some(e) => e, None => return },
    };
    idb_fire(scope, target, ev, ty);
}

/// The `xhr.upload` object IF one has already been handed out -- never creates one. A page that never
/// touched `.upload` cannot have registered anything on it, so there is nothing to fire at and no
/// reason to materialise the object just to ask.
fn xhr_upload_existing<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8::String::new(scope, "korvus_xhr_upload")?;
    let slot = v8::Private::for_api(scope, Some(key));
    this.get_private(scope, slot).filter(|v| !v.is_undefined() && !v.is_null())
}

/// The spec's "upload listener flag": does the upload object have any registered event listener?
/// An inline `xhr.upload.onabort = f` counts -- it IS a registered listener in the DOM -- even though
/// this engine keeps inline handlers in a private slot rather than in the listener store, so both
/// halves are asked.
fn xhr_upload_has_listener(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let Some(up) = xhr_upload_existing(scope, this) else { return false };
    if crate::core::events::event_target::et_any_listener(scope, up) { return true; }
    let Ok(up_obj) = v8::Local::<v8::Object>::try_from(up) else { return false };
    ["loadstart", "progress", "abort", "error", "load", "timeout", "loadend"]
        .iter()
        .any(|ty| crate::core::events::event_target::et_on_handler_set(scope, up_obj, ty))
}

/// The response's expected length (Blink's `response_.ExpectedContentLength()`): its `Content-Length` -- the
/// first value, digits only -- unless the network stack decodes the body. A `Content-Encoding` that is all
/// codings Chrome decodes (`br`, `deflate`, `gzip`, `x-gzip`, `zstd`) leaves the size unknown
/// (`URLRequestJob::NotifyHeadersComplete` reads the header only for an uncoded body): Chrome 150 reports a
/// gzip'd response's progress as `total 0`, not computable, even one larger coded than decoded.
fn xhr_expected_length(x: &XMLHttpRequest) -> Option<f64> {
    let headers = x.resp_headers.borrow();
    let codings: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .flat_map(|(_, v)| v.split(',').map(|c| c.trim().to_ascii_lowercase()).collect::<Vec<_>>())
        .collect();
    if !codings.is_empty() && codings.iter().all(|c| matches!(c.as_str(), "br" | "deflate" | "gzip" | "x-gzip" | "zstd")) {
        return None;
    }
    let v = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length"))?.1.split(',').next()?.trim().to_string();
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then(|| v.parse::<f64>().ok()).flatten()
}

/// Blink's `DispatchProgressEventFromSnapshot` numbers, read when the event is made: `loaded` the bytes
/// delivered, and the expected length as `total` only when it is above zero and not exceeded -- otherwise
/// `0` and not computable (a `Content-Length: 0` response's `load` is not computable in Chrome 150).
fn xhr_snapshot(x: &XMLHttpRequest) -> (f64, f64, bool) {
    let loaded = x.received.get();
    match xhr_expected_length(x) {
        Some(total) if total > 0.0 && loaded <= total => (loaded, total, true),
        _ => (loaded, 0.0, false),
    }
}

/// The spec's "request error steps" for an event named `name` (`abort`, `timeout` or `error`) -- the
/// one sequence all three failure paths share:
///
///   state = DONE, unset send() flag, fire `readystatechange`; then, if the upload was still in
///   flight, `<name>` + `loadend` at the upload object; then `<name>` + `loadend` at the xhr.
///
/// The upload half runs only when the upload listener flag was set at send() time, which is what
/// makes `xhr.upload.onabort` fire for a POST and nothing fire for a bodyless GET.
fn xhr_request_error(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, name: &str) {
    let Ok(this_obj) = v8::Local::<v8::Object>::try_from(this) else { return };
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this) else { return };
    let gen = unsafe { x.as_ref() }.generation.get();
    // The fetch is terminated: a body still being read stops (Blink's `InternalAbort` cancels the loader).
    xhr_stop_body(scope, this);
    {
        // "Set this's response to a network error": status 0, no status text, no headers, no body.
        // `abort-after-send` reads all five inside the DONE readystatechange handler.
        let x = unsafe { x.as_ref() };
        x.ready_state.set(4);
        x.send_flag.set(false);
        x.status.set(0);
        x.status_text.borrow_mut().clear();
        x.response_bytes.borrow_mut().clear();
        *x.text_cache.borrow_mut() = None;
        x.response_url.borrow_mut().clear();
        x.resp_headers.borrow_mut().clear();
        x.network_error.set(true);
        x.received.set(0.0);
        *x.pending_head.borrow_mut() = None;
        *x.sniffing.borrow_mut() = None;
    }
    // DONE through the progress throttle: a held `progress` is dropped and its timer stopped (Blink's
    // `kClear`), then the readystatechange.
    xhr_state_changed(scope, this, Pending::Clear);
    // Every step below re-checks the generation: a readystatechange handler is page JS and is free to
    // call open()/send() again, and the events after it belong to the request that is now gone.
    if xhr_generation(scope, this) != gen { return; }
    let (upload_complete, upload_listener) = {
        let x = unsafe { x.as_ref() };
        (x.upload_complete.get(), x.upload_listener.get())
    };
    if !upload_complete {
        unsafe { x.as_ref() }.upload_complete.set(true);
        if upload_listener {
            if let Some(up) = xhr_upload_existing(scope, this_obj) {
                xhr_fire_progress(scope, up, name, 0.0, 0.0, false);
                if xhr_generation(scope, this) != gen { return; }
                xhr_fire_progress(scope, up, "loadend", 0.0, 0.0, false);
                if xhr_generation(scope, this) != gen { return; }
            }
        }
    }
    xhr_fire_progress(scope, this, name, 0.0, 0.0, false);
    if xhr_generation(scope, this) != gen { return; }
    xhr_fire_progress(scope, this, "loadend", 0.0, 0.0, false);
}

/// The current generation of `this`, or `u32::MAX` when it is not an XHR any more (unreachable, but
/// the comparison must not silently succeed).
fn xhr_generation(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> u32 {
    match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this) {
        Some(x) => unsafe { x.as_ref() }.generation.get(),
        None => u32::MAX,
    }
}

/// `[this, generation, timer_epoch]` -- the data a deferred callback needs to know whether the request
/// it was scheduled for is still the current one. `build_fn_data` carries exactly one value, so the
/// three travel as an array.
fn xhr_ticket<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, gen: u32, epoch: u32) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, 3);
    arr.set_index(scope, 0, this);
    let g = crypto_num(scope, gen as f64);
    arr.set_index(scope, 1, g);
    let e = crypto_num(scope, epoch as f64);
    arr.set_index(scope, 2, e);
    arr.into()
}

/// Unpack an `xhr_ticket`.
fn xhr_ticket_read<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>) -> Option<(v8::Local<'s, v8::Value>, u32, u32)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let this = arr.get_index(scope, 0)?;
    let gen = arr.get_index(scope, 1)?.uint32_value(scope)?;
    let epoch = arr.get_index(scope, 2)?.uint32_value(scope)?;
    Some((this, gen, epoch))
}

/// Arm (or re-arm) the `timeout` timer for an in-flight request. `timeout` is measured from `send()`,
/// so assigning a new value mid-flight schedules against the time ALREADY elapsed -- a value smaller
/// than the elapsed time fires on the next task rather than never.
fn xhr_arm_timeout(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this) else { return };
    let (ms, gen, started) = {
        let x = unsafe { x.as_ref() };
        if !x.send_flag.get() { return; }
        (x.timeout.get(), x.generation.get(), x.send_started.get())
    };
    // The epoch is bumped FIRST and unconditionally: it is what disarms whatever timer is already
    // pending, so `xhr.timeout = 0` mid-flight cancels rather than leaving the old one to fire.
    let epoch = unsafe { x.as_ref() }.timer_epoch.get().wrapping_add(1);
    unsafe { x.as_ref() }.timer_epoch.set(epoch);
    if ms == 0 { return; }
    let elapsed = started.map(|t| t.elapsed().as_millis() as f64).unwrap_or(0.0);
    let delay = (ms as f64 - elapsed).max(0.0);
    let ticket = xhr_ticket(scope, this, gen, epoch);
    let cb = build_fn_data(scope, cb_xhr_timeout_fire, "", 0, ticket);
    // The engine's timer, not the page's `setTimeout`.
    crate::core::intersection_observer::rendering::engine_task(scope, cb, delay);
}

/// The `timeout` timer firing: terminate the fetch and run the request error steps for `timeout`.
fn cb_xhr_timeout_fire(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this, gen, epoch)) = xhr_ticket_read(scope, __body_data) else { return };
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this) else { return };
    {
        let x = unsafe { x.as_ref() };
        // A superseded request, a superseded timer, or a request that already finished: do nothing.
        if x.generation.get() != gen || x.timer_epoch.get() != epoch || !x.send_flag.get() { return; }
        // Terminate the fetch: the response, if it ever arrives, belongs to a request that timed out.
        x.aborted.set(true);
    }
    xhr_request_error(scope, this, "timeout");
}
fn cb_xhr_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    // `open()` takes (method, url) as REQUIRED arguments. It used to accept a call with none and
    // quietly default to GET "" -- which read as a working method to any hand-written case, because
    // nobody writes a case that calls open with nothing. The generated corpus does, and Brave answers
    // with a TypeError.
    if !crate::binding::protos::require_args(scope, &args, 2, "XMLHttpRequest", "open") { return; }
    let this_v: v8::Local<v8::Value> = args.this().into();
    // `open(method, url, async, …)`: `async` is a `boolean` -- ToBoolean, so an explicit `undefined`
    // is FALSE (Chrome 150: `open('GET', u, undefined); send()` returns with readyState 4). Only a
    // two-argument call is asynchronous by default.
    let is_async = if args.length() >= 3 { args.get(2).boolean_value(scope) } else { true };
    if !is_async && xhr_in_window(scope) {
        if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
            if !unsafe { x.as_ref() }.response_type.borrow().is_empty() {
                crate::binding::protos::throw_dom(scope, "Failed to execute 'open' on 'XMLHttpRequest': Synchronous requests from a document must not set a response type.", "InvalidAccessError");
                return;
            }
        }
    }
    let was_opened;
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        unsafe { x.as_ref() }.sync.set(!is_async);
        let m = args.get(0);
        let method = if m.is_undefined() || m.is_null() { "GET".to_string() } else { let s = m.to_rust_string_lossy(scope); if s.is_empty() { "GET".to_string() } else { s } };
        let raw = args.get(1).to_rust_string_lossy(scope);
        // Resolved NOW, against the base of the realm that opened it: a frame's XHR is
        // the frame document's request, not the page's. One that does not resolve keeps its text.
        let base = crate::core::html::browsing_context::realm_base_url(scope);
        let url = url::Url::parse(&base).ok().and_then(|b| b.join(&raw).ok()).map(|u| u.to_string()).unwrap_or(raw);
        xhr_stop_body(scope, this_v);
        let x = unsafe { x.as_ref() };
        *x.method.borrow_mut() = method;
        *x.url.borrow_mut() = url;
        // "Set variables associated with the object": terminate the fetch, unset the send() flag,
        // and drop the previous response. Bumping the generation is what terminates -- an in-flight
        // continuation or timer compares against it and does nothing.
        x.generation.set(x.generation.get().wrapping_add(1));
        x.aborted.set(false);
        x.send_flag.set(false);
        x.upload_complete.set(true);
        x.upload_listener.set(false);
        x.upload_total.set(0.0);
        x.send_started.set(None);
        x.network_error.set(false);
        x.status.set(0);
        x.status_text.borrow_mut().clear();
        x.response_bytes.borrow_mut().clear();
        *x.text_cache.borrow_mut() = None;
        x.response_url.borrow_mut().clear();
        x.resp_headers.borrow_mut().clear();
        x.req_headers.borrow_mut().clear();
        // Blink's `ClearResponse`; its progress throttle is the object's and is left as it is.
        x.received.set(0.0);
        *x.pending_head.borrow_mut() = None;
        *x.sniffing.borrow_mut() = None;
        was_opened = x.ready_state.get() == 1;
        xhr_clear_response_objects(scope, this_v);
        x.ready_state.set(1);
    } else {
        return;
    }
    // "If this's state is not opened: set state to opened and FIRE an event named readystatechange."
    // Already-opened stays silent -- re-open()ing does not re-announce. This event is the leading `1`
    // that every ordered-event test in xhr/ records before anything else, and it was not being fired
    // at all: `prepare_xhr_for_event_order_test` registers its listener before open(), so the whole
    // expected sequence was off by one from the first entry.
    // Through the progress throttle, as every state change is in Blink (`ChangeState`).
    if !was_opened { xhr_state_changed(scope, this_v, Pending::Ignore); }
}
fn cb_xhr_set_request_header(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if !crate::binding::protos::require_args(scope, &args, 2, "XMLHttpRequest", "setRequestHeader") { return; }
    // Blink 150's `XMLHttpRequest::setRequestHeader` (`xml_http_request.cc:1377-1430`), step for step. It used to
    // push the pair as given -- no state check after send(), no normalization, no validation, no forbidden name, no
    // combining -- so jQuery's `Accept`, a header set twice and a value with spaces each went out unlike Chrome's.
    const CTX: &str = "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': ";
    // `(ByteString name, ByteString value)`, converted before any step runs.
    let Some(name) = xhr_byte_string(scope, args.get(0), CTX) else { return };
    let Some(value) = xhr_byte_string(scope, args.get(1), CTX) else { return };
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) else { return };
    let x = unsafe { x.as_ref() };
    // 1-2. Only an OPENED request whose send() flag is unset takes a header.
    if x.ready_state.get() != 1 || x.send_flag.get() {
        crate::binding::protos::throw_dom(scope, &format!("{CTX}The object's state must be OPENED."), "InvalidStateError");
        return;
    }
    // 3. Normalize the value.
    let value = crate::modules::fetch::headers::normalize_header_value(&value);
    // 4. A name that is no token, a value that is no field value: SyntaxError, Blink's sentences.
    if !crate::modules::fetch::headers::is_header_name(&name) {
        crate::binding::protos::throw_dom(scope, &format!("{CTX}'{name}' is not a valid HTTP header field name."), "SyntaxError");
        return;
    }
    if !crate::modules::fetch::headers::is_header_value(&value) {
        crate::binding::protos::throw_dom(scope, &format!("{CTX}'{value}' is not a valid HTTP header field value."), "SyntaxError");
        return;
    }
    // 5. A forbidden request header is ignored. Blink also logs "Refused to set unsafe header" to the DevTools
    //    console, which no page can read; korvus has no channel for an engine console message, so nothing is said.
    if crate::modules::fetch::headers::is_forbidden_request_header(&name, &value) {
        return;
    }
    // 6. Combine into the author request headers: a name already set takes `", "` and this value, keeping the
    //    spelling it was first set with (Blink's `HTTPHeaderMap`). Chrome 150 sends `Accept` set twice as
    //    `Accept: a/b, c/d` and `X-A` twice as `X-A: 1, 2`; korvus sent each as two lines.
    let mut h = x.req_headers.borrow_mut();
    match h.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(&name)) {
        Some((_, v)) => {
            v.push_str(", ");
            v.push_str(&value);
        }
        None => h.push((name, value)),
    }
}

/// WebIDL's `ByteString` conversion as Blink's binding runs it (`NativeValueTraits<IDLByteString>`): `ToString` (whose
/// throw propagates), then a TypeError -- "String contains non ISO-8859-1 code point." -- for any code unit above
/// U+00FF. `None` with the exception pending.
fn xhr_byte_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, ctx: &str) -> Option<String> {
    let s = v.to_string(scope)?;
    let mut units = vec![0u16; s.length()];
    s.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
    if units.iter().any(|c| *c > 0xFF) {
        crate::binding::protos::throw_type_error(scope, &format!("{ctx}String contains non ISO-8859-1 code point."));
        return None;
    }
    Some(units.iter().map(|c| char::from(*c as u8)).collect())
}
fn cb_xhr_get_all_response_headers(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let s = if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) {
        // Sorted by (lowercased) name, as XHR's "get all response headers" sorts them -- Chrome 150
        // answers `cache-control`, `content-language`, `content-length`, `content-type`, ... for a
        // response that sent them in another order (measured).
        let mut pairs: Vec<(String, String)> = unsafe { x.as_ref() }.resp_headers.borrow().iter().map(|(k, v)| (k.to_lowercase(), v.clone())).collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect::<String>()
    } else { String::new() };
    let out = str_val(scope, &s); rv.set(out);
}
/// `abort()`, in full:
///
///   1. terminate the fetch;
///   2. if the state is OPENED-with-send()-flag, HEADERS_RECEIVED or LOADING, run the request error
///      steps for `abort` (which set the state to DONE and fire readystatechange / abort / loadend,
///      plus the upload pair when the upload was still in flight);
///   3. if the state is now DONE, set it to UNSENT.
///
/// Step 3 reads "now": the request error steps in step 2 just SET it to done, so an abort that fired
/// events leaves `readyState === 0`. That is not a quirk to work around, it is what
/// `xhr/abort-progress-events` asserts immediately after `abort()`.
///
/// This used to be one line -- set a flag, fire nothing -- so no page could observe an abort at all.
fn cb_xhr_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let this_v: v8::Local<v8::Value> = args.this().into();
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let fire = {
        let x = unsafe { x.as_ref() };
        // Terminate: the in-flight fetch's continuation checks this and drops the response.
        x.aborted.set(true);
        let st = x.ready_state.get();
        (st == 1 && x.send_flag.get()) || st == 2 || st == 3
    };
    if fire { xhr_request_error(scope, this_v, "abort"); }
    // Re-unwrap: a listener could have replaced the object's state wholesale via open()/send().
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        let x = unsafe { x.as_ref() };
        if x.ready_state.get() == 4 && !x.send_flag.get() { x.ready_state.set(0); }
    }
}
fn cb_xhr_send(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let this_v: v8::Local<v8::Value> = args.this().into();
    let body_arg = args.get(0);

    // WEBIDL ARGUMENT CONVERSION RUNS FIRST -- before the algorithm's own state checks.
    //
    // `send(optional (Document or XMLHttpRequestBodyInit)? body = null)`: stringifying the argument is
    // part of the CALL, not of the method, so an object whose `toString` re-enters the same XHR sees
    // the state as it was before send() started, and send()'s checks then see whatever that reentrant
    // call left behind. That is the whole of `xhr/send-data-unexpected-tostring`: `toString` calling
    // abort() must leave readyState 1, calling open() must make the SECOND url win, and calling send()
    // must make the outer send() throw InvalidStateError against the flag the inner one just set.
    //
    // It also has to happen exactly ONCE. Stringifying here and again when the request object is
    // built ran the author's `toString` twice, and the second run happened after the send() flag was
    // set -- which turned the harmless abort() of test 1 into a real one.
    //
    // THE BODY IS BYTES: a `BufferSource`/`Blob` goes out byte for byte, a string as its
    // UTF-8, through the SAME extraction `fetch` and `new Request` use, with the Content-Type it implies
    // (unless the author set one). It was `String(body)`, so `send(new Uint8Array([1, 2, 3]))` sent the
    // five bytes `1,2,3`.
    let body_str: Option<(Vec<u8>, Option<String>)> = if body_arg.is_null() || body_arg.is_undefined() {
        None
    } else {
        match crate::modules::fetch::body::extract(scope, body_arg) {
            Ok(crate::modules::fetch::body::Extracted::Bytes(b, ct)) => Some((b, ct)),
            Ok(crate::modules::fetch::body::Extracted::Null) => None,
            // XHR's body union has no ReadableStream: it converts to a string like any other object.
            Ok(crate::modules::fetch::body::Extracted::Stream(_)) => Some((body_arg.to_rust_string_lossy(scope).into_bytes(), Some("text/plain;charset=UTF-8".into()))),
            Err(()) => return,
        }
    };

    // "If state is not opened, throw an InvalidStateError." send() on a fresh XHR used to fetch the
    // empty URL and fire events for it; a page that sends without opening now learns so, as it would
    // in a browser. "If the send() flag is set, throw an InvalidStateError" is the second half.
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        let x = unsafe { x.as_ref() };
        if x.ready_state.get() != 1 || x.send_flag.get() {
            // with Blink's operation context, as every DOMException a binding throws carries it
            crate::binding::protos::throw_dom(scope, "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.", "InvalidStateError");
            return;
        }
    }
    let (method, url, headers) = match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        Some(x) => {
            let m = unsafe { x.as_ref() }.method.borrow().clone();
            let u = unsafe { x.as_ref() }.url.borrow().clone();
            let h = unsafe { x.as_ref() }.req_headers.borrow().clone();
            unsafe { x.as_ref() }.aborted.set(false);
            (m, u, h)
        }
        None => return,
    };
    // "If this's request method is GET or HEAD, set body to null." A body handed to a GET is dropped,
    // not transmitted, so it also raises no upload events.
    let bodyless_method = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    let body_str = if bodyless_method { None } else { body_str };

    // ── the spec's send() event prologue, which this method did not have at all ──────────────────
    //
    // A GET's body is null and its "upload complete flag" is SET from the start: that is exactly why
    // `send(null)` produces no upload events and a POST with a body does. The upload LISTENER flag is
    // latched here too -- registering an upload listener after send() must not retroactively conjure
    // events for a body already on the wire.
    let this_obj = args.this();
    let has_body = body_str.is_some();
    let body_len = body_str.as_ref().map(|(b, _)| b.len() as f64).unwrap_or(0.0);
    let sync = match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        Some(x) => unsafe { x.as_ref() }.sync.get(),
        None => return,
    };
    // A synchronous request fires no upload events and no `loadstart` (the spec gates both on the
    // synchronous flag being unset; Chrome 150's sync XHR fires exactly readystatechange(4), load,
    // loadend).
    let upload_listener = !sync && has_body && xhr_upload_has_listener(scope, this_obj);
    let gen = match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        Some(x) => {
            let x = unsafe { x.as_ref() };
            x.generation.set(x.generation.get().wrapping_add(1));
            x.send_flag.set(true);
            x.rt_asked.set(crate::core::timing::performance::clock_now_ms(scope));
            x.upload_complete.set(!has_body);
            x.upload_listener.set(upload_listener);
            x.upload_total.set(body_len);
            x.send_started.set(Some(std::time::Instant::now()));
            x.generation.get()
        }
        None => return,
    };
    // "Fire a progress event named loadstart at this with 0 and 0", then the same at the upload
    // object when the body is still going out. Page JS runs in both: `abort-upload-event-abort`
    // calls abort() from inside the xhr loadstart handler, so every step below re-checks that the
    // request it belongs to is still the live one.
    if !sync {
        xhr_fire_progress(scope, this_v, "loadstart", 0.0, 0.0, false);
        if xhr_generation(scope, this_v) != gen { return; }
    }
    if upload_listener {
        if let Some(up) = xhr_upload_existing(scope, this_obj) {
            xhr_fire_progress(scope, up, "loadstart", 0.0, body_len, true);
            if xhr_generation(scope, this_v) != gen { return; }
        }
    }
    // A handler above may have aborted: the fetch must not be started for a request that is over.
    match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        Some(x) => { if !unsafe { x.as_ref() }.send_flag.get() { return; } }
        None => return,
    }

    let req = v8::Object::new(scope);
    let urlv = str_val(scope, &url); set_prop(scope, req, "url", urlv);
    let methv = str_val(scope, &method); set_prop(scope, req, "method", methv);
    let mut headers = headers;
    if let Some((_, Some(ct))) = &body_str {
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            headers.push(("Content-Type".to_string(), ct.clone()));
        }
    }
    let harr = v8::Array::new(scope, headers.len() as i32);
    for (i, (k, v)) in headers.iter().enumerate() {
        let pair = v8::Array::new(scope, 2);
        let kv = str_val(scope, k); let vv = str_val(scope, v);
        pair.set_index(scope, 0, kv); pair.set_index(scope, 1, vv);
        let pairv: v8::Local<v8::Value> = pair.into(); harr.set_index(scope, i as u32, pairv);
    }
    let harrv: v8::Local<v8::Value> = harr.into(); set_prop(scope, req, "headers", harrv);
    // The body string extracted ABOVE, reused: never re-stringified, because that would run the
    // author's `toString` a second time.
    let bodyv: v8::Local<v8::Value> = match body_str { None => { let n = v8::null(scope); n.into() } Some((b, _)) => crate::modules::fetch::body::u8_array(scope, b) };
    set_prop(scope, req, "body", bodyv);
    // `withCredentials` is the request's credentials mode, which the CORS check reads
    // (measured: `withCredentials = true` against `Access-Control-Allow-Origin: *` is an error, status 0).
    let with_credentials = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v).map(|x| unsafe { x.as_ref() }.with_credentials.get()).unwrap_or(false);
    let credv = str_val(scope, if with_credentials { "include" } else { "same-origin" });
    set_prop(scope, req, "credentials", credv);
    // The document's referrer policy: XHR has no per-request one.
    let doc_policy = crate::core::html::referrer_policy::running_policy(scope);
    let rp = str_val(scope, crate::core::html::referrer_policy::token(doc_policy));
    set_prop(scope, req, "referrerPolicy", rp);
    if sync {
        // A synchronous load is Blink's HIGHEST priority ("lest they hang the renderer", `FetchParameters::
        // MakeSynchronous`), `u=0` on the wire; an asynchronous XHR keeps Fetch's `u=1, i`. Chrome 150 sent `u=0, i`
        // and weight byte 255 for a synchronous XHR where korvus sent `u=1, i`
        let pr = str_val(scope, "u=0, i");
        set_prop(scope, req, "priority", pr);
        xhr_send_sync(scope, this_v, req, &url, gen);
        return;
    }
    // `connect-src`: refused, the request is never made and ends as a network error does -- with its
    // resource-timing entry, made at the refusal (Chrome 150 x3: an `xmlhttprequest` entry, every phase 0,
    // where a refused `fetch()` has none).
    if crate::core::html::csp::realm_request_blocked(scope, &url, "") {
        crate::core::timing::resource_timing::refused_request(scope, "xmlhttprequest", &url);
        let t = xhr_ticket(scope, this_v, gen, 0);
        let refused = build_fn_data(scope, cb_xhr_csp_refused, "", 0, t);
        crate::core::intersection_observer::rendering::queue_task(scope, refused, 0.0);
        return;
    }
    // At the response's HEAD, its body streamed after, where the realm has that op.
    let op_fetch = match deno_op(scope, "op_fetch_head").or_else(|| deno_op(scope, "op_fetch")) { Some(f) => f, None => return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    // The second argument is the requesting document when it is not this realm's own (a frame's
    // `fetch`; a frame's XHR); "" is the realm's own.
    let client = crate::core::html::browsing_context::realm_client(scope);
    let own_client = str_val(scope, &client);
    // The requesting document's site for cookies.
    let sfc = crate::core::html::browsing_context::realm_site_for_cookies(scope);
    let sfc = str_val(scope, &sfc);
    let fetch_result = match op_fetch.call(scope, undef, &[req.into(), own_client, sfc]) { Some(p) => p, None => return };
    // Promise.resolve(op_fetch(...)).then(onful, onrej), binding `this` as each callback's data.
    let promise = match crate::binding::protos::call_intrinsic(scope, "Promise.resolve", &[fetch_result]) { Some(p) => p, None => fetch_result };
    let this_g = v8::Global::new(scope, this_v);
    let this_l = v8::Local::new(scope, &this_g);
    let t1 = xhr_ticket(scope, this_l, gen, 0);
    let onful = build_fn_data(scope, cb_xhr_onhead, "", 1, t1);
    let this_l2 = v8::Local::new(scope, &this_g);
    let t2 = xhr_ticket(scope, this_l2, gen, 0);
    let onrej = build_fn_data(scope, cb_xhr_onrej, "", 1, t2);
    // each delivery a task of the request's realm, not a promise job (`then_task`)
    xhr_then(scope, promise, onful, Some(onrej));
    // `timeout` is armed HERE, not stored and forgotten. Until now the attribute round-tripped a
    // number and nothing ever consumed it, so `ontimeout` could not fire in either realm.
    xhr_arm_timeout(scope, this_v);
}
/// `p.then(ok, err)` with each step a task (`then_task`), for a promise; chained as before for anything else.
fn xhr_then<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<'s, v8::Value>, ok: v8::Local<'s, v8::Function>, err: Option<v8::Local<'s, v8::Function>>) {
    match v8::Local::<v8::Promise>::try_from(p) {
        Ok(pr) => {
            let _ = crate::then_task(scope, pr, ok, err);
        }
        Err(_) => {
            let _ = crate::binding::protos::promise_then(scope, p, ok, err);
        }
    }
}
/// A network delivery -- the head, a chunk, the end, a failure -- is a TASK of the request's realm (Blink posts each
/// on its networking task source), though here it runs in whichever microtask checkpoint the finished op landed in,
/// beside other realms' timers and reactions ([`crate::core::timing::loaf::begin_task`]).
fn xhr_delivery_task(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    if let Ok(o) = v8::Local::<v8::Object>::try_from(this) {
        crate::core::timing::loaf::begin_task(scope, o);
    }
}

/// The response's HEAD is in (`op_fetch_head`) -- or, from `op_fetch` and for a `data:`/`blob:` URL or a body
/// complete before its head was noticed, the whole response.
///
/// It used to be the whole response, always, with the three transitions back to back -- HEADERS_RECEIVED,
/// LOADING, DONE in one task after the last byte. Chrome 150 walks them as the body arrives, measured with a
/// local server trickling it: HEADERS_RECEIVED and LOADING when the first bytes are DELIVERED (not when the
/// headers arrive), a `readystatechange(3)` + `progress` per delivery at most every 50 ms, DONE at the end. A
/// page that reads `responseText` as it grows, or times the transitions -- a bot challenge's
/// `onreadystatechange` ran ten times over 370 ms in Chrome and three times in one millisecond here -- sees the
/// difference. The head is held ([`XMLHttpRequest::pending_head`]) until HEADERS_RECEIVED.
fn cb_xhr_onhead(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this_v, gen, _)) = xhr_ticket_read(scope, __body_data) else { return };
    xhr_delivery_task(scope, this_v);
    let ro = match v8::Local::<v8::Object>::try_from(args.get(0)) { Ok(o) => o, Err(_) => return };
    let body_id = get_prop(scope, ro, "bodyId").filter(|v| v.is_number()).and_then(|v| v.uint32_value(scope));
    if !xhr_still_live(scope, this_v, gen) {
        if let Some(id) = body_id {
            xhr_cancel_body(scope, id);
        }
        return;
    }
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let status = get_prop(scope, ro, "status").and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u16;
    let status_text = get_prop(scope, ro, "statusText").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let final_url = get_prop(scope, ro, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let mut hv: Vec<(String, String)> = Vec::new();
    if let Some(ha) = get_prop(scope, ro, "headers").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for i in 0..ha.length() {
            if let Some(pair) = ha.get_index(scope, i).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
                let k = pair.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                let vv = pair.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                hv.push((k, vv));
            }
        }
    }
    {
        let x = unsafe { x.as_ref() };
        // A sniffable type is held back with its head until the sniffer decides; any other response's URL
        // is readable at once (Blink sets `response_` in `DidReceiveResponse`: Chrome 150 reads `responseURL`
        // before `readyState` leaves OPENED, and '' while a `text/plain` body is held).
        match super::mime_sniff::sniff_hint(&final_url, &hv) {
            Some(hint) => *x.sniffing.borrow_mut() = Some((hint, final_url, Vec::new())),
            None => {
                *x.sniffing.borrow_mut() = None;
                *x.response_url.borrow_mut() = final_url;
            }
        }
        *x.pending_head.borrow_mut() = Some(XhrHead { status, status_text, headers: hv });
        x.received.set(0.0);
        x.body_id.set(body_id);
    }
    // The request body is on the wire: the upload half completes BEFORE any response state, which is the
    // order every `*-upload-*` ordering test in xhr/ records.
    let (upload_complete, upload_listener, upload_total) = {
        let x = unsafe { x.as_ref() };
        (x.upload_complete.get(), x.upload_listener.get(), x.upload_total.get())
    };
    if !upload_complete {
        unsafe { x.as_ref() }.upload_complete.set(true);
        if upload_listener {
            let Ok(this_obj) = v8::Local::<v8::Object>::try_from(this_v) else { return };
            if let Some(up) = xhr_upload_existing(scope, this_obj) {
                for ty in ["progress", "load", "loadend"] {
                    xhr_fire_progress(scope, up, ty, upload_total, upload_total, true);
                    if !xhr_still_live(scope, this_v, gen) {
                        return;
                    }
                }
            }
        }
    }
    if !xhr_still_live(scope, this_v, gen) {
        return;
    }
    if body_id.is_some() {
        xhr_next_chunk(scope, this_v, gen);
        return;
    }
    // The whole response: its bytes, then its end -- two deliveries, as Blink's loader makes them. An EMPTY body
    // is no delivery at all (Blink makes no `DidReceiveData` for it), so HEADERS_RECEIVED comes in the end's own
    // steps, where a handler that aborts or re-opens there still gets the end's DONE -- as the streamed path always
    // gave it. This path is taken when the body ended before its head was noticed, so Chrome 150's answer here
    // (`xhr_streaming_live`'s handler quirks, #3 and #4) depended on the timing: 4 runs in 10 failed.
    let body = xhr_record_bytes(scope, ro);
    if !body.is_empty() {
        xhr_body_bytes(scope, this_v, body);
    }
    if xhr_still_live(scope, this_v, gen) {
        xhr_body_end(scope, this_v, ro);
    }
}

/// Ask for the next chunk of the body behind the request's head.
fn xhr_next_chunk(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, gen: u32) {
    let Some(id) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v).and_then(|x| unsafe { x.as_ref() }.body_id.get()) else { return };
    let Some(f) = deno_op(scope, "op_fetch_body_chunk") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    let Some(p) = f.call(scope, undef, &[idv]) else { return };
    let ticket = xhr_ticket(scope, this_v, gen, id);
    let ok = build_fn_data(scope, cb_xhr_chunk, "", 1, ticket);
    let _ = crate::binding::protos::promise_then(scope, p, ok, None);
}

/// A chunk of the body arrived -- or `null`: the body is over, and `op_fetch_body` says how.
fn cb_xhr_chunk(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this_v, gen, id)) = xhr_ticket_read(scope, __body_data) else { return };
    xhr_delivery_task(scope, this_v);
    let current = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v).is_some_and(|x| unsafe { x.as_ref() }.body_id.get() == Some(id));
    if !current || !xhr_still_live(scope, this_v, gen) {
        xhr_cancel_body(scope, id);
        return;
    }
    let chunk = args.get(0);
    if chunk.is_array_buffer_view() {
        let bytes = crate::modules::fetch::body::buffer_bytes(chunk).unwrap_or_default();
        xhr_body_bytes(scope, this_v, bytes);
        if xhr_still_live(scope, this_v, gen) {
            xhr_next_chunk(scope, this_v, gen);
        }
        return;
    }
    let Some(f) = deno_op(scope, "op_fetch_body") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    let Some(p) = f.call(scope, undef, &[idv]) else { return };
    let ticket = xhr_ticket(scope, this_v, gen, id);
    let ok = build_fn_data(scope, cb_xhr_body_done, "", 1, ticket);
    let ticket = xhr_ticket(scope, this_v, gen, id);
    let err = build_fn_data(scope, cb_xhr_body_failed, "", 1, ticket);
    let _ = crate::binding::protos::promise_then(scope, p, ok, Some(err));
}

/// The body ended: the whole response, for its timing.
fn cb_xhr_body_done(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this_v, gen, _)) = xhr_ticket_read(scope, __body_data) else { return };
    xhr_delivery_task(scope, this_v);
    if !xhr_still_live(scope, this_v, gen) {
        return;
    }
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) {
        unsafe { x.as_ref() }.body_id.set(None);
    }
    let Ok(ro) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    xhr_body_end(scope, this_v, ro);
}

/// The body failed after the head (the connection dropped): a network error, as Blink's `HandleNetworkError`.
fn cb_xhr_body_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this_v, gen, _)) = xhr_ticket_read(scope, __body_data) else { return };
    xhr_delivery_task(scope, this_v);
    if !xhr_still_live(scope, this_v, gen) {
        return;
    }
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let (url, asked) = {
        let x = unsafe { x.as_ref() };
        x.body_id.set(None);
        x.aborted.set(true);
        (x.url.borrow().clone(), x.rt_asked.get())
    };
    crate::core::timing::resource_timing::request_failed(scope, &url, "xmlhttprequest", asked);
    xhr_request_error(scope, this_v, "error");
}

/// Stop reading the body of a request that ended (an abort, a timeout, an `open()`).
fn xhr_stop_body(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    let id = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this).and_then(|x| unsafe { x.as_ref() }.body_id.take());
    if let Some(id) = id {
        xhr_cancel_body(scope, id);
    }
}

fn xhr_cancel_body(scope: &mut v8::PinScope, id: u32) {
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    crate::modules::fetch::body::cancel_network_body(scope, idv);
}

/// Body bytes off the network. While the network service sniffs a sniffable type they are held, and handed
/// over together -- with the response's URL -- once the sniffer has enough (`mime_sniff::have_enough`).
fn xhr_body_bytes(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, bytes: Vec<u8>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let deliver = {
        let x = unsafe { x.as_ref() };
        let mut sniffing = x.sniffing.borrow_mut();
        match sniffing.as_mut() {
            Some((hint, url, held)) => {
                held.extend_from_slice(&bytes);
                if !super::mime_sniff::have_enough(held, url, hint) {
                    return;
                }
                let held = std::mem::take(held);
                *x.response_url.borrow_mut() = std::mem::take(url);
                *sniffing = None;
                held
            }
            None => bytes,
        }
    };
    xhr_deliver(scope, this_v, deliver);
}

/// HEADERS_RECEIVED: the held head becomes readable (Blink's `ChangeState(kHeadersReceived)`).
fn xhr_headers_received(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    {
        let x = unsafe { x.as_ref() };
        if let Some(h) = x.pending_head.borrow_mut().take() {
            x.network_error.set(false);
            x.status.set(h.status);
            *x.status_text.borrow_mut() = h.status_text;
            *x.resp_headers.borrow_mut() = h.headers;
        }
        x.ready_state.set(2);
    }
    xhr_state_changed(scope, this_v, Pending::Ignore);
}

/// Bytes delivered to the request: Blink's `XMLHttpRequest::DidReceiveData` and `TrackProgress`, one call per
/// delivery and nothing checked that Blink does not check -- so a page that aborts in this LOADING
/// `readystatechange` still gets its `progress`, empty and in UNSENT, and one that re-`open()`s in the
/// HEADERS_RECEIVED one gets these bytes in the new request's LOADING (both measured on Chrome 150).
fn xhr_deliver(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, bytes: Vec<u8>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    if unsafe { x.as_ref() }.ready_state.get() < 2 {
        xhr_headers_received(scope, this_v);
        // "user javascript can cause |abort()|" in that readystatechange
        if unsafe { x.as_ref() }.aborted.get() {
            return;
        }
    }
    if bytes.is_empty() {
        return;
    }
    let changed = {
        let x = unsafe { x.as_ref() };
        x.response_bytes.borrow_mut().extend_from_slice(&bytes);
        *x.text_cache.borrow_mut() = None;
        x.received.set(x.received.get() + bytes.len() as f64);
        let changed = x.ready_state.get() != 3;
        x.ready_state.set(3);
        changed
    };
    if changed {
        xhr_state_changed(scope, this_v, Pending::Ignore);
    }
    let snap = xhr_snapshot(unsafe { x.as_ref() });
    xhr_throttled_progress(scope, this_v, snap);
}

/// The body is over (Blink's `DidFinishLoading` -> `EndLoading`): bytes the sniffer still held are delivered
/// first; then the resource-timing entry, DONE through the throttle -- a held `progress` flushed, in DONE --
/// and `load` + `loadend`, unless a handler aborted or re-opened the request.
fn xhr_body_end(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, record: v8::Local<v8::Object>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let gen = unsafe { x.as_ref() }.generation.get();
    let held = {
        let x = unsafe { x.as_ref() };
        let taken = x.sniffing.borrow_mut().take();
        taken.map(|(_, url, held)| {
            *x.response_url.borrow_mut() = url;
            held
        })
    };
    if let Some(held) = held.filter(|h| !h.is_empty()) {
        xhr_deliver(scope, this_v, held);
        if !xhr_still_live(scope, this_v, gen) {
            return;
        }
    }
    if unsafe { x.as_ref() }.ready_state.get() < 2 {
        xhr_headers_received(scope, this_v);
    }
    // The entry is due when the body is in, with the timing the whole response carries.
    crate::core::timing::resource_timing::xhr_response(scope, record, unsafe { x.as_ref() }.rt_asked.get());
    let changed = {
        let x = unsafe { x.as_ref() };
        x.send_flag.set(false);
        let changed = x.ready_state.get() != 4;
        x.ready_state.set(4);
        // The text decoded while LOADING held back an unfinished sequence; DONE flushes it.
        *x.text_cache.borrow_mut() = None;
        changed
    };
    if changed {
        let pending = if unsafe { x.as_ref() }.aborted.get() { Pending::Clear } else { Pending::Flush };
        xhr_state_changed(scope, this_v, pending);
    }
    // "if (state_ == kDone && !error_)": `load` and `loadend`, each with the numbers as they are when it is
    // made -- a `load` handler that re-opens gets a `loadend` of 0 and 0 (Chrome 150).
    let done = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v).is_some_and(|x| {
        let x = unsafe { x.as_ref() };
        x.ready_state.get() == 4 && !x.aborted.get()
    });
    if !done {
        return;
    }
    let (l, t, c) = xhr_snapshot(unsafe { x.as_ref() });
    xhr_fire_progress(scope, this_v, "load", l, t, c);
    let (l, t, c) = xhr_snapshot(unsafe { x.as_ref() });
    xhr_fire_progress(scope, this_v, "loadend", l, t, c);
}

/// What a state change does with a `progress` the throttle holds (Blink's `DeferredEventAction`).
#[derive(Clone, Copy)]
enum Pending {
    /// Nothing: it stays held.
    Ignore,
    /// Dropped, and the timer stopped (DONE after an error).
    Clear,
    /// Dispatched now, before the `readystatechange`, and the timer stopped (DONE).
    Flush,
}

/// A state change's `readystatechange`, through the progress throttle (Blink's
/// `XMLHttpRequestProgressEventThrottle::DispatchReadyStateChangeEvent`): the held `progress` per `pending`, the
/// next LOADING `progress` is a first again, and the event -- unless the flushed `progress`'s handlers moved
/// the state on.
fn xhr_state_changed(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, pending: Pending) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let state = unsafe { x.as_ref() }.ready_state.get();
    match pending {
        Pending::Ignore => {}
        Pending::Clear => {
            unsafe { x.as_ref() }.throttle_deferred.set(None);
            xhr_throttle_stop(unsafe { x.as_ref() });
        }
        Pending::Flush => {
            if let Some(held) = unsafe { x.as_ref() }.throttle_deferred.take() {
                xhr_progress_now(scope, this_v, held);
            }
            xhr_throttle_stop(unsafe { x.as_ref() });
        }
    }
    unsafe { x.as_ref() }.progress_dispatched.set(false);
    if unsafe { x.as_ref() }.ready_state.get() == state {
        xhr_fire(scope, this_v, "readystatechange");
    }
}

/// A `progress` dispatched now (Blink's `DispatchProgressProgressEvent`): in LOADING, after the first, a
/// `readystatechange` goes first -- Chrome 150's `readystatechange(3)` per 50 ms tick -- and the `progress`
/// only if that left the state where it was.
fn xhr_progress_now(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, (loaded, total, computable): (f64, f64, bool)) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let state = unsafe { x.as_ref() }.ready_state.get();
    if state == 3 && unsafe { x.as_ref() }.progress_dispatched.get() {
        xhr_fire(scope, this_v, "readystatechange");
    }
    if unsafe { x.as_ref() }.ready_state.get() != state {
        return;
    }
    unsafe { x.as_ref() }.progress_dispatched.set(true);
    xhr_fire_progress(scope, this_v, "progress", loaded, total, computable);
}

/// A `progress` for the throttle (Blink's `DispatchProgressEvent` for `progress`): held while its timer runs,
/// otherwise dispatched now and the 50 ms timer started.
fn xhr_throttled_progress(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, snap: (f64, f64, bool)) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    if unsafe { x.as_ref() }.throttle_active.get() {
        unsafe { x.as_ref() }.throttle_deferred.set(Some(snap));
        return;
    }
    xhr_progress_now(scope, this_v, snap);
    xhr_throttle_start(scope, this_v);
}

/// `kMinimumProgressEventDispatchingInterval`: "50 ms per specification".
const PROGRESS_INTERVAL_MS: f64 = 50.0;

/// Start (or restart) the throttle's one-shot timer -- the engine's own timer, which takes no page timer id.
fn xhr_throttle_start(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>) {
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let epoch = {
        let x = unsafe { x.as_ref() };
        let e = x.throttle_epoch.get().wrapping_add(1);
        x.throttle_epoch.set(e);
        x.throttle_active.set(true);
        e
    };
    let ticket = xhr_ticket(scope, this_v, 0, epoch);
    let cb = build_fn_data(scope, cb_xhr_throttle_fired, "", 0, ticket);
    crate::core::intersection_observer::rendering::engine_task(scope, cb, PROGRESS_INTERVAL_MS);
}

fn xhr_throttle_stop(x: &XMLHttpRequest) {
    x.throttle_active.set(false);
    x.throttle_epoch.set(x.throttle_epoch.get().wrapping_add(1));
}

/// The throttle's timer fired (Blink's `Fired`): nothing held -- it stops; else the held `progress` goes out and
/// the timer starts again.
fn cb_xhr_throttle_fired(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((this_v, _, epoch)) = xhr_ticket_read(scope, __body_data) else { return };
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    if unsafe { x.as_ref() }.throttle_epoch.get() != epoch {
        return;
    }
    let Some(held) = unsafe { x.as_ref() }.throttle_deferred.take() else {
        unsafe { x.as_ref() }.throttle_active.set(false);
        return;
    };
    xhr_progress_now(scope, this_v, held);
    xhr_throttle_start(scope, this_v);
}

/// Is the request that `gen` names still the live one, and not terminated? Page JS runs between every
/// pair of events below, and it may abort, re-open or re-send.
fn xhr_still_live(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, gen: u32) -> bool {
    match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this) {
        Some(x) => { let x = unsafe { x.as_ref() }; x.generation.get() == gen && !x.aborted.get() }
        None => false,
    }
}

/// Fetch rejected (network error): the request error steps for `error` -- readystatechange, then the
/// upload pair when the body was still in flight, then error + loadend, all as ProgressEvents.
fn cb_xhr_onrej(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    xhr_network_error(scope, &args, true);
}

/// A request the document's CSP refused: the same error steps as a network error, from a task of
/// their own, and no resource-timing entry -- the request was never made.
fn cb_xhr_csp_refused(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    xhr_network_error(scope, &args, false);
}

fn xhr_network_error(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, made: bool) {
    let __body_data = crate::binding::protos::body_data(scope, args);
    let Some((this_v, gen, _)) = xhr_ticket_read(scope, __body_data) else { return };
    xhr_delivery_task(scope, this_v);
    let x = match deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) { Some(x) => x, None => return };
    if unsafe { x.as_ref() }.aborted.get() || unsafe { x.as_ref() }.generation.get() != gen { return; }
    if made {
        let (url, asked) = { let x = unsafe { x.as_ref() }; (x.url.borrow().clone(), x.rt_asked.get()) };
        crate::core::timing::resource_timing::request_failed(scope, &url, "xmlhttprequest", asked);
    }
    xhr_request_error(scope, this_v, "error");
}
fn cb_xhr_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) { let n = crypto_num(scope, unsafe { x.as_ref() }.ready_state.get() as f64); rv.set(n); }
}
fn cb_xhr_status(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) { let n = crypto_num(scope, unsafe { x.as_ref() }.status.get() as f64); rv.set(n); }
}
fn cb_xhr_status_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) { let s = unsafe { x.as_ref() }.status_text.borrow().clone(); let v = str_val(scope, &s); rv.set(v); }
}
/// `responseText` -- measured on Chrome 150:
///
/// ```text
/// responseType 'arraybuffer' / 'json'   InvalidStateError: Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text' (was 'arraybuffer').
/// in readyState 2, 3, 4 (a 21-byte UTF-8 JSON)   0, 20, 20
/// bytes 0..=255, no charset                     length 256, charCodeAt(128) 65533
/// with overrideMimeType('text/plain; charset=x-user-defined')   256, 63360 (U+F780 + 0x80), charCodeAt(65) 65
/// with overrideMimeType('text/plain; charset=windows-1252')     charCodeAt(128) 8364
/// a body starting FF FE (UTF-16LE BOM)          decoded as UTF-16LE: "hi"
/// ```
fn cb_xhr_response_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) else { return };
    let x = unsafe { x.as_ref() };
    let rt = x.response_type.borrow().clone();
    if !(rt.is_empty() || rt == "text") {
        crate::binding::protos::throw_dom(
            scope,
            &format!("Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text' (was '{rt}')."),
            "InvalidStateError",
        );
        return;
    }
    let s = xhr_text(x);
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `response`, by `responseType` -- measured on Chrome 150:
///
/// ```text
/// ''/'text'      the responseText ("" before LOADING)
/// 'arraybuffer'  null before DONE and on a network error; then [object ArrayBuffer] 256 bytes, u[128] 128 -- the SAME object on every read
/// 'blob'         null before DONE; then [object Blob] 256 bytes, type 'application/octet-stream' (the final MIME type's essence: an override 'Text/Plain; charset=…' gives 'text/plain')
/// 'json'         null before DONE and for a body that is not JSON; then the parsed value (UTF-8, BOM stripped) -- a NEW parse on every read (`x.response === x.response` is false)
/// 'document'     responseXML's answer (null for application/json)
/// ```
fn cb_xhr_response(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let this = args.this();
    let Some(xp) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this.into()) else { return };
    let x = unsafe { xp.as_ref() };
    let rt = x.response_type.borrow().clone();
    match rt.as_str() {
        "" | "text" => {
            let s = xhr_text(x);
            let v = str_val(scope, &s);
            rv.set(v);
        }
        "document" => cb_xhr_response_xml(scope, args, rv),
        _ => {
            if x.ready_state.get() != 4 || x.network_error.get() {
                rv.set_null();
                return;
            }
            match rt.as_str() {
                "arraybuffer" | "blob" => {
                    let key = if rt == "blob" { "korvus_xhr_blob" } else { "korvus_xhr_ab" };
                    let k = v8::String::new(scope, key).unwrap();
                    let slot = v8::Private::for_api(scope, Some(k));
                    if let Some(cached) = this.get_private(scope, slot).filter(|v| !v.is_undefined()) {
                        rv.set(cached);
                        return;
                    }
                    let bytes = x.response_bytes.borrow().clone();
                    let made: v8::Local<v8::Value> = if rt == "blob" {
                        let mime = xhr_effective_mime(x);
                        match crate::core::fileapi::blob::mint_blob(scope, bytes, &mime) {
                            Some(b) => b.into(),
                            None => { rv.set_null(); return; }
                        }
                    } else {
                        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
                        v8::ArrayBuffer::with_backing_store(scope, &store).into()
                    };
                    this.set_private(scope, slot, made);
                    rv.set(made);
                }
                "json" => {
                    let bytes = x.response_bytes.borrow().clone();
                    let (text, _) = encoding_rs::UTF_8.decode_with_bom_removal(&bytes);
                    let Some(src) = v8::String::new(scope, &text) else { rv.set_null(); return };
                    let parsed = {
                        v8::tc_scope!(let tc, scope);
                        let r = v8::json::parse(tc, src).map(|v| v8::Global::new(tc, v));
                        if tc.has_caught() { tc.reset(); }
                        r
                    };
                    match parsed {
                        Some(g) => rv.set(v8::Local::new(scope, &g)),
                        None => rv.set_null(),
                    }
                }
                _ => rv.set_null(),
            }
        }
    }
}

/// The page's own realm is a Window (the sync-XHR restrictions are "from a document").
fn xhr_in_window(scope: &mut v8::PinScope) -> bool {
    global_get(scope, "window").map(|v| v.is_object()).unwrap_or(false)
}

/// Drop the cached `ArrayBuffer`/`Blob` a previous response handed out.
fn xhr_clear_response_objects(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    let Ok(o) = v8::Local::<v8::Object>::try_from(this) else { return };
    for key in ["korvus_xhr_ab", "korvus_xhr_blob"] {
        let k = v8::String::new(scope, key).unwrap();
        let slot = v8::Private::for_api(scope, Some(k));
        o.delete_private(scope, slot);
    }
}

/// The body bytes of an `op_fetch` record (a `Uint8Array`; a string only from an older caller).
fn xhr_record_bytes(scope: &mut v8::PinScope, ro: v8::Local<v8::Object>) -> Vec<u8> {
    match get_prop(scope, ro, "body") {
        Some(v) if v.is_array_buffer_view() => crate::modules::fetch::body::buffer_bytes(v).unwrap_or_default(),
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope).into_bytes(),
        _ => Vec::new(),
    }
}

/// XHR's "text response": empty before LOADING; otherwise the bytes decoded with the override MIME
/// type's charset, else the response Content-Type's, else UTF-8 -- a BOM wins over all three.
fn xhr_text(x: &XMLHttpRequest) -> String {
    if x.ready_state.get() < 3 {
        return String::new();
    }
    if let Some(t) = x.text_cache.borrow().as_ref() {
        return t.clone();
    }
    // Blink's `FinalResponseCharset` (`xml_http_request.cc:1585`): the override's charset, else the RESPONSE's --
    // the network stack's, the charset of the `Content-Type` value that won (`TextEncodingName()`; A/B C5: it was the
    // first line's own `charset`).
    let label = x
        .override_mime
        .borrow()
        .as_deref()
        .map(transport::mime::charset_param)
        .filter(|c| !c.is_empty())
        .or_else(|| Some(transport::mime::mime_type_and_charset(&x.resp_headers.borrow()).charset).filter(|c| !c.is_empty()));
    let enc = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())).unwrap_or(encoding_rs::UTF_8);
    let text = {
        let bytes = x.response_bytes.borrow();
        // Decoded as it arrived, flushed at the end (Blink's streaming decoder): while LOADING, a sequence the
        // bytes so far end inside is not text yet -- Chrome 150 reads 1 for `한` plus the first byte of `글`,
        // then 2 -- and at DONE it is a U+FFFD.
        let mut decoder = enc.new_decoder();
        let mut out = String::with_capacity(decoder.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len() * 3 + 16));
        let _ = decoder.decode_to_string(&bytes, &mut out, x.ready_state.get() == 4);
        out
    };
    *x.text_cache.borrow_mut() = Some(text.clone());
    text
}

/// A synchronous `send()` (Chrome 150: `readyState 4, status 200, responseText.length 256` when it
/// returns; events `readystatechange(4)`, `load`, `loadend`; a network failure THROWS
/// `NetworkError: Failed to execute 'send' on 'XMLHttpRequest': Failed to load '<url>'.` with
/// readyState 4 and fires nothing).
fn xhr_send_sync(scope: &mut v8::PinScope, this_v: v8::Local<v8::Value>, req: v8::Local<v8::Object>, url: &str, gen: u32) {
    let Some(op) = deno_op(scope, "op_fetch_sync") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    // The requesting document when it is a frame's: a frame's synchronous XHR went
    // out initiated by the PAGE -- the page's origin as its Referer -- where the async path asked the frame.
    let client = crate::core::html::browsing_context::realm_client(scope);
    let own_client = str_val(scope, &client);
    // `sync-xhr` disabled by the document's permissions policy: the request is never
    // made, and `send()` ends exactly as a network failure does -- Chrome 150, measured over
    // `tools/realweb/seed/refperm`: `Permissions-Policy: sync-xhr=()` throws this NetworkError in the page
    // and in every frame it embeds.
    let allowed = crate::core::feature_policy::enabled_in_current_realm(scope, "sync-xhr")
        // a request the document's CSP refuses (`connect-src`) is not made either, and has its
        // resource-timing entry as an asynchronous one's
        && !{
            let refused = crate::core::html::csp::realm_request_blocked(scope, url, "");
            if refused {
                crate::core::timing::resource_timing::refused_request(scope, "xmlhttprequest", url);
            }
            refused
        };
    let outcome = if !allowed {
        None
    } else {
        v8::tc_scope!(let tc, scope);
        let r = op.call(tc, undef, &[req.into(), own_client]).map(|v| v8::Global::new(tc, v));
        if tc.has_caught() { tc.reset(); None } else { r }
    };
    let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, this_v) else { return };
    let x = unsafe { x.as_ref() };
    let record = outcome.map(|g| v8::Local::new(scope, &g)).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let Some(ro) = record else {
        // The request error steps for a synchronous request: DONE, a network-error response, and the
        // exception -- no events.
        x.ready_state.set(4);
        x.send_flag.set(false);
        x.status.set(0);
        x.status_text.borrow_mut().clear();
        x.response_bytes.borrow_mut().clear();
        *x.text_cache.borrow_mut() = None;
        x.resp_headers.borrow_mut().clear();
        x.network_error.set(true);
        if allowed {
            crate::core::timing::resource_timing::request_failed(scope, url, "xmlhttprequest", x.rt_asked.get());
        }
        let resolved = url::Url::parse(url)
            .ok()
            .or_else(|| url::Url::parse(&crate::core::dom::base_url(scope)).ok().and_then(|b| b.join(url).ok()))
            .map(|u| u.to_string())
            .unwrap_or_else(|| url.to_string());
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute 'send' on 'XMLHttpRequest': Failed to load '{resolved}'."), "NetworkError");
        return;
    };
    crate::core::timing::resource_timing::xhr_response(scope, ro, x.rt_asked.get());
    let status = get_prop(scope, ro, "status").and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u16;
    let status_text = get_prop(scope, ro, "statusText").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let final_url = get_prop(scope, ro, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let body = xhr_record_bytes(scope, ro);
    let mut hv: Vec<(String, String)> = Vec::new();
    if let Some(ha) = get_prop(scope, ro, "headers").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for i in 0..ha.length() {
            if let Some(pair) = ha.get_index(scope, i).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
                let k = pair.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                let vv = pair.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                hv.push((k, vv));
            }
        }
    }
    x.received.set(body.len() as f64);
    x.network_error.set(false);
    x.status.set(status);
    *x.status_text.borrow_mut() = status_text;
    *x.response_url.borrow_mut() = final_url;
    *x.resp_headers.borrow_mut() = hv;
    *x.response_bytes.borrow_mut() = body;
    *x.text_cache.borrow_mut() = None;
    x.upload_complete.set(true);
    x.ready_state.set(4);
    x.send_flag.set(false);
    // The same numbers as the asynchronous path's (Blink's `DispatchProgressEventFromSnapshot`).
    let (loaded, total, computable) = xhr_snapshot(x);
    xhr_fire(scope, this_v, "readystatechange");
    if xhr_generation(scope, this_v) != gen { return; }
    xhr_fire_progress(scope, this_v, "load", loaded, total, computable);
    if xhr_generation(scope, this_v) != gen { return; }
    xhr_fire_progress(scope, this_v, "loadend", loaded, total, computable);
}
fn cb_xhr_response_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()) { let s = unsafe { x.as_ref() }.response_url.borrow().clone(); let v = str_val(scope, &s); rv.set(v); }
}

// ── the eight members the capture has and this class did not ────────────────────────────────────
//
// Shapes come from `brave-151.surface.json`, not from WebIDL: timeout / withCredentials /
// responseType / onreadystatechange are get+set accessors, upload / responseXML are get-only, and
// getResponseHeader / overrideMimeType are methods of length 1. All enumerable + configurable.
//
// `onreadystatechange` is NOT here: it is an event-handler pair, and the generic native accessor
// (`op_et_on_getter` / `op_et_on_setter`, a per-instance v8::Private slot) already exists and is what
// the other seven XHR handlers use. Reusing it is the difference between `xhr.onreadystatechange = f`
// storing a handler the fire path can find and a noop setter that drops it -- which is worse than a
// missing member, because the page has no way to tell.

/// `this` as an `XMLHttpRequest`, or `None`.
macro_rules! xhr_this {
    ($scope:ident, $args:ident) => {
        deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>($scope, $args.this().into())
    };
}

fn cb_xhr_timeout_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) { let n = crypto_num(scope, unsafe { x.as_ref() }.timeout.get() as f64); rv.set(n); }
}

fn cb_xhr_timeout_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) {
        // WebIDL `unsigned long` with no [Clamp] and no [EnforceRange] means ToUint32: truncate toward
        // zero, then wrap modulo 2^32. Negatives WRAP, they do not clamp.
        //
        // The first version of this line clamped `< 0` to 0 because that is what "unsigned" sounds
        // like. Asked, Brave 151 answers `xhr.timeout = -1` -> 4294967295. That is the DOMRect
        // `unrestricted double` incident in miniature -- an invented coercion rule that the
        // implementation and its own test would have agreed on. Recorded, verified: -1 -> 4294967295,
        // "500" -> 500, 3.7 -> 3, 4294967296 -> 0.
        let n = args.get(0).number_value(scope).unwrap_or(0.0);
        let n = if n.is_finite() { n.trunc().rem_euclid(4_294_967_296.0) as u32 } else { 0 };
        // Chrome 150, after `open(m, u, false)` in a window: InvalidAccessError, measured verbatim.
        if unsafe { x.as_ref() }.sync.get() && xhr_in_window(scope) {
            crate::binding::protos::throw_dom(scope, "Failed to set the 'timeout' property on 'XMLHttpRequest': Timeouts cannot be set for synchronous requests made from a document.", "InvalidAccessError");
            return;
        }
        unsafe { x.as_ref() }.timeout.set(n);
        // "If the timeout attribute value is changed while a fetch is in progress, the timer is
        // RESET" -- reset, not restarted: it still counts from send(), so a value smaller than the
        // time already elapsed expires at once. Setting it to 0 disarms (the old timer's epoch is
        // superseded either way). This is the whole of xmlhttprequest-timeout-overrides*.
        xhr_arm_timeout(scope, args.this().into());
    }
}

fn cb_xhr_with_credentials_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) { let b = v8::Boolean::new(scope, unsafe { x.as_ref() }.with_credentials.get()); rv.set(b.into()); }
}

fn cb_xhr_with_credentials_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) {
        let x = unsafe { x.as_ref() };
        // The spec throws InvalidStateError once the request is past OPENED (state > 1), or if send()
        // has been called. Korvus tracks the send through `ready_state`, so the same test serves.
        if x.ready_state.get() > 1 {
            crate::binding::protos::throw_dom(scope, "The object's state must be OPENED or UNSENT.", "InvalidStateError");
            return;
        }
        x.with_credentials.set(args.get(0).boolean_value(scope));
    }
}

fn cb_xhr_response_type_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) { let s = unsafe { x.as_ref() }.response_type.borrow().clone(); let v = str_val(scope, &s); rv.set(v); }
}

fn cb_xhr_response_type_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if let Some(x) = xhr_this!(scope, args) {
        let x = unsafe { x.as_ref() };
        // XMLHttpRequestResponseType is an ENUM, and a WebIDL enum setter IGNORES a value outside it --
        // it does not throw and it does not store. `xhr.responseType = "nonsense"` leaves it "".
        let want = args.get(0).to_rust_string_lossy(scope);
        if !matches!(want.as_str(), "" | "arraybuffer" | "blob" | "document" | "json" | "text") { return; }
        // ... but setting it at all once the response is arriving IS an InvalidStateError.
        if x.ready_state.get() >= 3 {
            crate::binding::protos::throw_dom(scope, "The object's state must not be LOADING or DONE.", "InvalidStateError");
            return;
        }
        // Chrome 150, after `open(m, u, false)` in a window: InvalidAccessError, measured verbatim.
        if x.sync.get() && x.ready_state.get() == 1 && xhr_in_window(scope) {
            crate::binding::protos::throw_dom(scope, "Failed to set the 'responseType' property on 'XMLHttpRequest': The response type cannot be changed for synchronous requests made from a document.", "InvalidAccessError");
            return;
        }
        *x.response_type.borrow_mut() = want;
    }
}

fn cb_xhr_get_response_header(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if !crate::binding::protos::require_args(scope, &args, 1, "XMLHttpRequest", "getResponseHeader") { return; }
    let Some(x) = xhr_this!(scope, args) else { return };
    let x = unsafe { x.as_ref() };
    // Before the headers arrive there is nothing to report, and the spec says null rather than "".
    if x.ready_state.get() < 2 { rv.set_null(); return; }
    let want = args.get(0).to_rust_string_lossy(scope).to_ascii_lowercase();
    // Header names are case-insensitive, and a repeated header is reported as one comma-joined value
    // (`Set-Cookie` excepted, which XHR never exposes at all).
    let joined: Vec<String> = x
        .resp_headers
        .borrow()
        .iter()
        .filter(|(k, _)| k.to_ascii_lowercase() == want)
        .map(|(_, v)| v.clone())
        .collect();
    if joined.is_empty() || want == "set-cookie" || want == "set-cookie2" {
        rv.set_null();
    } else {
        let v = str_val(scope, &joined.join(", "));
        rv.set(v);
    }
}

fn cb_xhr_override_mime_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    if !crate::binding::protos::require_args(scope, &args, 1, "XMLHttpRequest", "overrideMimeType") { return; }
    let Some(x) = xhr_this!(scope, args) else { return };
    let x = unsafe { x.as_ref() };
    if x.ready_state.get() >= 3 {
        crate::binding::protos::throw_dom(scope, "MimeType cannot be overridden when the state is LOADING or DONE.", "InvalidStateError");
        return;
    }
    // Blink's `overrideMimeType` (`xml_http_request.cc:1354`): `application/octet-stream` unless the argument parses
    // (`ParsedContentType`) and names a type (`net::ExtractMimeTypeFromMediaType`, no comma), then the argument (A/B C5).
    let m = args.get(0).to_rust_string_lossy(scope);
    let keep = transport::mime::parsed_content_type_is_valid(&m) && transport::mime::extract_mime_type_from_media_type(&m, false).is_some();
    *x.override_mime.borrow_mut() = Some(if keep { m } else { "application/octet-stream".to_string() });
    *x.text_cache.borrow_mut() = None;
}

/// XHR's final MIME type (`FinalResponseMIMETypeInternal`, `xml_http_request.cc:1552`): the override's type, else
/// `net::ExtractMimeTypeFromMediaType` over the response's `Content-Type` lines joined with ", ", comma-separated
/// values accepted -- the FIRST value, both halves tokens, case kept; `None` for neither (A/B C5: it was the first
/// line cut at `;`, so `text/html,text/plain` was a type of its own).
fn xhr_final_mime(x: &XMLHttpRequest) -> Option<String> {
    if let Some(t) = x.override_mime.borrow().as_deref().and_then(|m| transport::mime::extract_mime_type_from_media_type(m, false)) {
        return Some(t);
    }
    let joined = transport::mime::joined_content_type(&x.resp_headers.borrow())?;
    transport::mime::extract_mime_type_from_media_type(&joined, true)
}

/// The response MIME type (`GetResponseMIMEType`): the final MIME type, else `text/xml`. Lowercased, as every reader
/// here compares or reports it (`ResponseBlob` lowercases it; `IsXMLMIMEType` compares case-insensitively).
fn xhr_effective_mime(x: &XMLHttpRequest) -> String {
    xhr_final_mime(x).unwrap_or_else(|| "text/xml".to_string()).to_ascii_lowercase()
}

fn cb_xhr_response_xml(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    let Some(x) = xhr_this!(scope, args) else { return };
    let x = unsafe { x.as_ref() };
    // "If responseType is not the empty string or "document", throw an InvalidStateError." A page that
    // asked for JSON and then reads responseXML has a bug, and the browser says so.
    {
        let rt = x.response_type.borrow();
        if !(rt.is_empty() || rt.as_str() == "document") {
            crate::binding::protos::throw_dom(scope, "The value is only accessible if the object's 'responseType' is '' or 'document'.", "InvalidStateError");
            return;
        }
    }
    if x.ready_state.get() != 4 { rv.set_null(); return; }
    // "If this's response's body is null, return null." An aborted / timed-out / errored request has
    // no body at all, and parsing the empty string under the `text/xml` fallback used to hand the page
    // an empty Document where every browser answers null.
    if x.network_error.get() { rv.set_null(); return; }
    let mime = xhr_effective_mime(x);
    // The spec's list: only XML and HTML produce a document. Anything else is null -- notably
    // text/plain and application/json, which is what most beacons come back as. Blink's tests
    // (`ResponseIsXML`: `IsXMLMIMEType` -- `text/xml`, `application/xml`, `application/*+xml` only, its TODO
    // crbug.com/362282752; `ResponseIsHTML`: the FINAL type, with no `text/xml` fallback).
    let is_xml = matches!(mime.as_str(), "text/xml" | "application/xml") || (mime.starts_with("application/") && mime.ends_with("+xml"));
    let is_html = xhr_final_mime(x).is_some_and(|m| m.eq_ignore_ascii_case("text/html"));
    if !(is_xml || is_html) { rv.set_null(); return; }
    // An HTML response is a document only for `responseType = "document"` (`InitResponseDocument`; Chrome 150
    // measured: `responseXML` of a `text/html` response is null under the default type -- A/B C5's fixture).
    if is_html && x.response_type.borrow().is_empty() { rv.set_null(); return; }
    let body = xhr_text(x);
    // Parsed through the same DOMParser page script would use, so responseXML and
    // `new DOMParser().parseFromString(xhr.responseText, ...)` cannot disagree.
    let Some(ctor) = crate::binding::protos::iface_object(scope, "DOMParser").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { rv.set_null(); return };
    let Some(ctor) = v8::Local::<v8::Function>::try_from(ctor).ok() else { rv.set_null(); return };
    let Some(parser) = ctor.new_instance(scope, &[]) else { rv.set_null(); return };
    let body_v = str_val(scope, &body);
    let mime_v = str_val(scope, if is_html { "text/html" } else { "application/xml" });
    // the realm's own `parseFromString`, never the page's
    match crate::binding::protos::pristine_call(scope, parser.into(), "DOMParser.parseFromString", &[body_v, mime_v]) {
        Some(doc) => rv.set(doc.into()),
        None => rv.set_null(),
    }
}

fn cb_xhr_upload(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !xhr_recv(scope, &args) { return; }
    // `xhr.upload` is the same object every time it is read -- a page that does
    // `xhr.upload.addEventListener(...)` and then reads `xhr.upload` again must get the object it
    // registered on. Cached on a v8::Private slot on the XHR wrapper, the pattern the canvas 2D
    // context store already uses for per-object state.
    let this = args.this();
    let key = v8::String::new(scope, "korvus_xhr_upload").unwrap();
    let slot = v8::Private::for_api(scope, Some(key));
    if let Some(cached) = this.get_private(scope, slot) {
        if !cached.is_undefined() { rv.set(cached); return; }
    }
    // An XMLHttpRequestUpload is an XMLHttpRequestEventTarget: the capture records it with no own
    // members and the chain [XMLHttpRequestEventTarget, EventTarget], and the seven on* handlers it
    // answers with are inherited from that base -- which this engine already installs as real native
    // accessors. So the honest object here is one whose prototype IS that prototype; `new` is not the
    // way to build it (Chrome's constructor is illegal to call), and Object.create is.
    let Some(up_ctor) = crate::binding::protos::iface_object(scope, "XMLHttpRequestUpload").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { rv.set_undefined(); return };
    let Some(proto) = get_prop(scope, up_ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { rv.set_undefined(); return };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    this.set_private(scope, slot, obj.into());
    rv.set(obj.into());
}

// ── the three tables ───────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `XMLHttpRequest` block of `dom/08f-crypto.js` -- a hand
// `Object.setPrototypeOf`, twenty-four `Object.defineProperty` sites over `op_xhr_*` builder
// round-trips onto the cppgc CLASS TEMPLATE's prototype, the five constants written twice by hand,
// and `globalThis.XMLHttpRequest =` the raw class -- AND the hand-written block at the top of
// `js/generated_shells.js` that gave `XMLHttpRequestEventTarget.prototype` its seven `on*` accessors
// and then spliced `XMLHttpRequest.prototype` onto it. All three interfaces are tables now; the
// chain `XMLHttpRequest -> XMLHttpRequestEventTarget -> EventTarget` is the installer's WebIDL
// splice, and the seven handlers are `event_handler` rows on the base where WebIDL puts them.
//
// `XMLHttpRequestUpload` has no members of its own in the capture: `cb_xhr_upload` mints a plain
// object on ITS prototype (the seven handlers are inherited), which is what Chrome's own
// `xhr.upload` is -- `[object XMLHttpRequestUpload]` with the base's accessors.
//
// WINDOW / WORKER. `XMLHttpRequest` is `[value, window]`: the worker's is `worker_js_owned`
// (`worker-native.js` puts these same bodies behind `generated_worker_shells.js`'s recorded shape)
// until W6. Its BASE and the upload object are `both`, because a worker XHR needs them.

/// The receiver as an `XMLHttpRequest`, or the WebIDL `TypeError` a foreign one gets. Every body used
/// to answer `undefined` (or silently do nothing) for any object at all.
fn xhr_recv(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<XMLHttpRequest>(scope, args.this().into()).is_some() {
        return true;
    }
    crate::binding::protos::throw_type_error(scope, "Illegal invocation");
    false
}

/// `new XMLHttpRequest()` -- the `ctor` row's body (the class template's `#[constructor]` is
/// unreachable once the table owns the interface object).
pub fn cb_xhr_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        crate::binding::protos::throw_type_error(scope, &crate::binding::protos::use_new_msg("XMLHttpRequest"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <XMLHttpRequest as XMLHttpRequestBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => crate::install::live::set_table_proto(scope, obj, "XMLHttpRequest"),
    }
    rv.set(obj.into());
}

/// The body every non-constant, non-handler row of [`XMLHTTPREQUEST`] names.
pub fn install_xhr(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "readyState" => getter_row(scope, proto, member, cb_xhr_ready_state),
        "timeout" => pair_row(scope, proto, member, cb_xhr_timeout_get, cb_xhr_timeout_set),
        "withCredentials" => pair_row(scope, proto, member, cb_xhr_with_credentials_get, cb_xhr_with_credentials_set),
        "upload" => getter_row(scope, proto, member, cb_xhr_upload),
        "responseURL" => getter_row(scope, proto, member, cb_xhr_response_url),
        "status" => getter_row(scope, proto, member, cb_xhr_status),
        "statusText" => getter_row(scope, proto, member, cb_xhr_status_text),
        "responseType" => pair_row(scope, proto, member, cb_xhr_response_type_get, cb_xhr_response_type_set),
        "response" => getter_row(scope, proto, member, cb_xhr_response),
        "responseText" => getter_row(scope, proto, member, cb_xhr_response_text),
        "responseXML" => getter_row(scope, proto, member, cb_xhr_response_xml),
        "abort" => method_row(scope, proto, member, cb_xhr_abort),
        "getAllResponseHeaders" => method_row(scope, proto, member, cb_xhr_get_all_response_headers),
        "getResponseHeader" => method_row(scope, proto, member, cb_xhr_get_response_header),
        "open" => method_row(scope, proto, member, cb_xhr_open),
        "overrideMimeType" => method_row(scope, proto, member, cb_xhr_override_mime_type),
        "send" => method_row(scope, proto, member, cb_xhr_send),
        "setRequestHeader" => method_row(scope, proto, member, cb_xhr_set_request_header),
        other => debug_assert!(false, "install_xhr: unexpected member {other}"),
    }
}

// `WORKER_XHR_MEMBERS` WAS HERE: the worker capture's twenty-three names, written down
// and handed to the worker install as a selector. The one fact it carried -- `responseXML` is
// `[Exposed=Window]`, because its body needs a `Document` a worker has none of -- is the `window`
// word on that row now, and WHICH members a realm installs comes from the ACTIVE capture.

// Rows VERBATIM from the three blocks of `install/generated_registry.rs`, in the capture's order:
// the handler, the eleven attributes, the five constants on the prototype, the seven methods,
// `responseXML`, then the same five constants on the interface object.
webapi::interface! {
    "XMLHttpRequest" : "XMLHttpRequestEventTarget" [value, both] {
        ctor            "XMLHttpRequest"                          len 0 = stub,
        handler         "onreadystatechange"                             = event_handler,
        attr            "readyState"            : Object readonly        = install_xhr,
        attr            "timeout"               : Object                 = install_xhr,
        attr            "withCredentials"       : Object                 = install_xhr,
        attr            "upload"                : Object readonly        = install_xhr,
        attr            "responseURL"           : Object readonly        = install_xhr,
        attr            "status"                : Object readonly        = install_xhr,
        attr            "statusText"            : Object readonly        = install_xhr,
        attr            "responseType"          : Object                 = install_xhr,
        attr            "response"              : Object readonly        = install_xhr,
        attr            "responseText"          : Object readonly        = install_xhr,
        constant        "UNSENT"                : Integer                = 0,
        constant        "OPENED"                : Integer                = 1,
        constant        "HEADERS_RECEIVED"      : Integer                = 2,
        constant        "LOADING"               : Integer                = 3,
        constant        "DONE"                  : Integer                = 4,
        method          "abort"                 : Object len 0           = install_xhr,
        method          "getAllResponseHeaders" : Object len 0           = install_xhr,
        method          "getResponseHeader"     : Object len 1 required 1 nullable = install_xhr,
        method          "open"                  : Object len 2 required 2 = install_xhr,
        method          "overrideMimeType"      : Object len 1 required 1 = install_xhr,
        method          "send"                  : Object len 0           = install_xhr,
        method          "setRequestHeader"      : Object len 2 required 2 = install_xhr,
        // THE ROW SAYS WHICH REALM. This fact used to live in a hand-written
        // per-realm selector beside the table -- a copy of one capture's member list. The list a
        // realm installs is the ACTIVE capture's now (`install::installer::join`), so the fact
        // belongs where WebIDL puts it, on the row: BOTH bundles agree, in both realms, and a
        // realm with no capture to read (a bare worker runtime) is right by this word alone.
        attr            "responseXML"           : Object window readonly nullable = install_xhr,
        // THE OTHER DEVICE'S ROWS: Attribution Reporting and Private State Tokens,
        // which `chrome-152-linux-20261016T1022` records on `XMLHttpRequest` in BOTH realms and
        // `brave-151-linux-20260907T1006` records in neither. Installed only when the active capture
        // lists them (`install::installer::join`). D2 stubs -- the engine implements neither. They sit
        // before the statics because a `static_constant` row must stay adjacent to its fellows for the
        // interface object's own key order; the prototype order a page reads is the capture's anyway.
        method          "setAttributionReporting" : Any len 1             = stub,
        method          "setPrivateToken"       : Undefined len 1 required 1 = stub,
        static_constant "UNSENT"                : Integer                = 0,
        static_constant "OPENED"                : Integer                = 1,
        static_constant "HEADERS_RECEIVED"      : Integer                = 2,
        static_constant "LOADING"               : Integer                = 3,
        static_constant "DONE"                  : Integer                = 4
    }
}

webapi::interface! {
    "XMLHttpRequestEventTarget" : "EventTarget" [interface, both] {
        ctor    "XMLHttpRequestEventTarget" len 0 = stub,
        handler "onloadstart"                     = event_handler,
        handler "onprogress"                      = event_handler,
        handler "onabort"                         = event_handler,
        handler "onerror"                         = event_handler,
        handler "onload"                          = event_handler,
        handler "ontimeout"                       = event_handler,
        handler "onloadend"                       = event_handler
    }
}

webapi::interface! {
    "XMLHttpRequestUpload" : "XMLHttpRequestEventTarget" [interface, both] {
        ctor "XMLHttpRequestUpload" len 0 = stub
    }
}

// THE TWENTY-ONE `op_xhr_*` FUNCTION-BUILDER OPS ARE GONE.
//
// They existed for ONE consumer, `core/korvus-runtime/src/js/worker-native.js`, which put these
// bodies behind `generated_worker_shells.js`'s recorded shape because the worker's XHR was
// `tools/sync/data/worker_js_owned.json`'s. Both files are deleted: the worker builds
// `XMLHttpRequest` from the table above, filtered by the worker capture's own member list, so the
// same `cb_xhr_*` callbacks
// this file already mounts in the window are the worker's too, through THE installer. That list is
// empty now, which is what goal invariant I8 asks for -- "no worker-only JS, no second shell set".


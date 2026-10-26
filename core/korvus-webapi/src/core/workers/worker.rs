//! `core/workers/worker.rs` — `Worker`.
//!
//! Was `class Worker extends EventTarget` in `core/korvus-runtime/src/js/host.js`: a constructor that
//! spawned a real V8-isolate-per-worker, four `async` receive pumps, and two `Object.defineProperty`
//! mounts of native functions onto the class's prototype. It is one `interface!` table with a native
//! constructor now — the SECOND of the three ESM-time `extends EventTarget` declarations W4.b named
//! as the reason `events.js` still exists.
//!
//! WHAT THE JS CLASS GOT WRONG, measured with `tests/fixtures/worker-iface-probe.js` on
//! `google-chrome --headless=new` 150.0.7871.46 (the probe reaches a real worker through a Blob URL in
//! both engines, so the two dumps are directly diffable):
//!
//! * `Worker.length` was **2** (a JS class with two declared parameters); the capture says 1.
//! * `new Worker()` CONSTRUCTED a worker over the string "undefined"; Chrome throws "Failed to
//!   construct 'Worker': 1 argument required, but only 0 present."
//! * `Worker("x")` said "Class constructor Worker cannot be invoked without 'new'" where a browser
//!   says "Please use the 'new' operator, this DOM object constructor cannot be called as a function."
//! * `constructor` was the FIRST own key of the prototype; the capture's four members came after it.
//! * `Symbol.toStringTag` was a GETTER (`get [Symbol.toStringTag]() { return "Worker"; }`); Chrome's
//!   is a non-writable data property, so `Object.getOwnPropertyDescriptor(...).value` was `undefined`.
//! * `Object.getOwnPropertyNames(new Worker(u))` was `["__id"]`; Chrome's is `[]`.
//! * all four rows answered their shape default for a foreign receiver, where Chrome throws
//!   `Illegal invocation` for all eight receiver/row pairs.
//!
//! WHERE THE BODIES ARE. `op_worker_spawn`, `op_worker_post_fn`, `op_worker_terminate_fn`,
//! `op_msg_deserialize` and the four `*_recv` ops belong to `korvus-runtime` (`worker.rs`) — spawning
//! a worker isolate is a KERNEL service (invariant I9), and this table calls it. The call goes through
//! [`deno_op`], the cross-extension op handle `XMLHttpRequest` has used for `op_fetch` since before
//! this unit; `op_stash_ops` keeps it reachable after `BOOTSTRAP` deletes the `Deno` global.
//!
//! BOTH REALMS SPAWN. `Worker` is `[Exposed=(Window,Worker)]` in both captures, and the worker isolate
//! registers the spawn and receive ops too, so a worker's own `new Worker(...)` starts a NESTED worker
//! through this same table. Until the live a challenge provider challenge (korvus-analysis) it registered none:
//! the constructor branded `this` and returned a worker that never ran -- no message, no error -- and
//! the challenge frame's first worker, which starts the workers that do the work, waited forever. A
//! realm whose isolate registers no `op_worker_spawn` still gets just that branded shell.

use deno_core::v8;

use crate::binding::protos::{get_prop, global_get, new_global, set_prop, str_val};
use crate::core::xmlhttprequest::xml_http_request::deno_op;
use crate::install::webidl::{arg_string, brand, require_args, require_new};
use crate::install::MemberDef;

/// The worker isolate's id, as a v8 Private. `host.js` kept it as a non-enumerable OWN `__id`, which
/// `Object.getOwnPropertyNames` listed.
pub const ID_SLOT: &str = "korvus_worker_id";

/// The private the realm's `Deno.core.unrefOpPromise` is parked on, captured at install time.
const UNREF_SLOT: &str = "korvus_unref_op_promise";

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

// ── UNREF ───────────────────────────────────────────────────────────────────────────────────────

/// Capture `Deno.core.unrefOpPromise` onto a hidden private of the global.
///
/// `host.js` captured the same function into a module-scoped `const UNREF` at ESM-evaluation time,
/// "while `Deno.core` still exists (before BOOTSTRAP deletes `Deno`)". This is that capture, moved to
/// the install — which also runs before `BOOTSTRAP`. Same function, same lifetime, no JS.
pub(crate) fn capture_unref(scope: &mut v8::PinScope) {
    let Some(f) = global_get(scope, "Deno")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|d| get_prop(scope, d, "core"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "unrefOpPromise"))
        .filter(|v| v.is_function())
    else {
        return;
    };
    let global = scope.get_current_context().global(scope);
    slot_set(scope, global, UNREF_SLOT, f);
}

/// UNREF a parked async-op promise, so a worker that is only WAITING does not by itself hold the page
/// event loop open. Losing this is the failure the unit brief warns about: the engine stops exiting.
pub(crate) fn unref(scope: &mut v8::PinScope, p: v8::Local<v8::Value>) {
    let global = scope.get_current_context().global(scope);
    let Some(f) = slot_get(scope, global, UNREF_SLOT).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    f.call(scope, undef, &[p]);
}

// ── the source and URL seams ────────────────────────────────────────────────────────────────────

/// `resolveWorkerSource`: the literal script text for this URL, when there is one.
///
/// Two seams: `globalThis.__workerScripts[url]` (an EXTERNAL registration the harness and the CDP
/// loader write) and a `blob:` URL's text out of the Rust registry
/// (`modules/url/url.rs::BlobUrlRegistry`, read through the synchronous `blob_url_text` below).
///
/// `__workerScripts` IS A PLAIN GLOBAL AND IS NOT ON THE BRIDGE -- the ruling, and the
/// reason it is a ruling rather than a move. This read had a SECOND seam that looked the name up on
/// the bridge holder, documented as "both are accepted"; it could never have answered. Every one of
/// the forty-odd test files that registers a source writes `globalThis.__workerScripts = {…}`,
/// nothing in this tree has ever written the name onto the holder, and the holder read's own miss
/// path fell back to a plain global lookup -- so the branch re-asked the question the line above it
/// had already answered. What the name really is, is a TEST and CDP INJECTION SEAM on the global; it
/// is not a Web API member body behind a mask, which is the thing invariant I3 forbids. Moving it to
/// a Rust-side registry behind an `Engine` method would touch forty test files to relocate a seam
/// that was already outside the bridge, so the slot is NAMED here instead and the dead branch is gone.
///
/// Read as the global's OWN DATA property only: a plain `globalThis.__workerScripts` lookup
/// on a page that never set it walked the window's prototype chain into its named-properties object -- the
/// challenge frame of a live an interactive-widget challenge saw a `__workerScripts` query on every `new Worker` -- and a getter a page
/// put there would have run. The seam's writers all assign a data property.
pub(crate) fn worker_source(scope: &mut v8::PinScope, url: &str) -> Option<String> {
    seam_source(scope, url).or_else(|| local_source(scope, url).map(|(text, _)| text))
}

/// The `__workerScripts` seam's text for `url` (see [`worker_source`]): a harness's or the CDP loader's
/// registration, keyed by the string the page passed -- which is often no URL at all, so a script found here skips
/// the constructor's URL checks.
pub(crate) fn seam_source(scope: &mut v8::PinScope, url: &str) -> Option<String> {
    let key = str_val(scope, url);
    let seam = {
        let global = scope.get_current_context().global(scope);
        let name = v8::String::new(scope, "__workerScripts")?;
        global
            .get_own_property_descriptor(scope, name.into())
            .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())
            .and_then(|d| crate::binding::protos::obj_get(scope, d, "value"))
    };
    let h = seam.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let v = h.get(scope, key).filter(|v| !v.is_null_or_undefined())?;
    Some(v.to_rust_string_lossy(scope))
}

/// The script a `data:` or `blob:` URL holds, read by the constructor itself, and the MIME type its response
/// carries -- the `data:` URL's own, or the Blob's `type` (a module script's check reads it). A `blob:` URL is
/// resolved HERE, in the constructing task, as Chrome resolves it to a token (`ResolveAsBlobURLToken`,
/// dedicated_worker.cc:301-306), so a page that revokes it right after `new Worker(url)` still starts its worker.
pub(crate) fn local_source(scope: &mut v8::PinScope, url: &str) -> Option<(String, String)> {
    if let Some((ty, bytes)) = crate::modules::url::url::decode_data_url(url) {
        return Some((String::from_utf8_lossy(&bytes).into_owned(), ty));
    }
    let text = crate::modules::url::url::blob_url_text(scope, url)?;
    let ty = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        crate::modules::url::url::blob_url_bytes(&st, url).map(|(ty, _)| ty).unwrap_or_default()
    };
    Some((text, ty))
}

/// The base a script URL given to `new Worker` / `new SharedWorker` is resolved against: the constructing realm's
/// document base, or a worker's own URL (Blink's `ExecutionContext::CompleteURL`).
fn script_base(scope: &mut v8::PinScope) -> String {
    // The constructing realm's DOCUMENT BASE from the engine's own state, not its
    // `location.href` read through script: a frame's `new Worker(blobUrl)` read the TOP realm's URL
    // getters (`URL.prototype.href`), which the page could watch. A realm with no document (a worker
    // nesting a worker) keeps its location.
    match crate::core::html::browsing_context::current_realm_ctx(scope) {
        Some(id) => crate::core::html::browsing_context::frame_request_client(scope, id).1,
        None if crate::core::html::browsing_context::has_document(scope) => crate::core::dom::base_url(scope),
        // a worker's own URL from its scope's state, not its `location.href` getter
        None => crate::core::workers::worker_global_scope::scope_href(scope),
    }
}

/// The creating document's URL when this realm is a FRAME's (its `HostLocation` is the page's): the client the
/// worker's script request names (`frame_request_client`). Empty for the page and for a worker.
pub(crate) fn script_client(scope: &mut v8::PinScope) -> String {
    match crate::core::html::browsing_context::current_realm_ctx(scope) {
        Some(id) => crate::core::html::browsing_context::frame_request_client(scope, id).0,
        None => String::new(),
    }
}

/// Blink's `AbstractWorker::ResolveURL` (abstract_worker.cc:53-89) for `new <iface>(url)`: the URL completed against
/// the constructing realm's base -- an invalid one is a `SyntaxError` -- and one the realm's origin may read: its
/// own origin's, or a `data:` URL (`SecurityOrigin::CanReadContent`, security_origin.cc:376-426); anything else is a
/// `SecurityError`. `None` when it threw.
///
/// Chrome 150, measured (`tests/fixtures/worker-script-loading`), from a page and from a worker:
///
/// ```text
/// 'http://['                                   SyntaxError    "Failed to construct 'Worker': 'http://[' is not a valid URL."
/// another origin, https on the same host, about:blank, javascript:, ftp:, ws:
///                                              SecurityError  "Failed to construct 'Worker': Script at '<url>' cannot be
///                                                             accessed from origin '<origin>'."
/// data: (any type)                             runs, its origin opaque
/// ```
///
/// korvus checked nothing: a cross-origin URL started a worker of the other origin, and every other one fired an
/// `error` Event (a SharedWorker's, nothing at all).
pub(crate) fn resolve_script_url(scope: &mut v8::PinScope, url: &str, iface: &str) -> Option<String> {
    let base = script_base(scope);
    let parsed = url::Url::parse(url).ok().or_else(|| url::Url::parse(&base).ok().and_then(|b| b.join(url).ok()));
    let Some(u) = parsed else {
        crate::binding::protos::throw_dom(scope, &format!("Failed to construct '{iface}': '{url}' is not a valid URL."), "SyntaxError");
        return None;
    };
    // the realm's own origin (its `self.origin` as sealed), never what a page assigned to the `[Replaceable]` name
    let origin = crate::binding::protos::realm_self_get(scope, "origin").map(|v| v.to_rust_string_lossy(scope)).filter(|o| !o.is_empty()).unwrap_or_else(|| "null".to_string());
    let readable = if u.scheme() == "data" {
        true
    } else if origin == "null" {
        // an opaque origin requests only a `blob:` URL it minted under that opaque origin (security_origin.cc:380-395)
        u.scheme() == "blob" && u.path().starts_with("null/") && {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            crate::modules::url::url::blob_url_bytes(&st, u.as_str()).is_some()
        }
    } else {
        let target = u.origin();
        target.is_tuple() && target.ascii_serialization() == origin
    };
    if !readable {
        let msg = format!("Failed to construct '{iface}': Script at '{}' cannot be accessed from origin '{origin}'.", elided(u.as_str()));
        crate::binding::protos::throw_dom(scope, &msg, "SecurityError");
        return None;
    }
    Some(u.to_string())
}

/// `KURL::ElidedString` (kurl.cc:223-231): a URL longer than 1024 characters as its first 511, `...`, its last 510.
fn elided(url: &str) -> String {
    let chars: Vec<char> = url.chars().collect();
    if chars.len() <= 1024 {
        return url.to_string();
    }
    let head: String = chars[..511].iter().collect();
    let tail: String = chars[chars.len() - 510..].iter().collect();
    format!("{head}...{tail}")
}

/// `resolveWorkerUrl`: resolve `scriptURL` against the PAGE's own URL, the way `<script src>` does.
///
/// W1.6 measured this against a real `wpt serve`: without it `op_worker_spawn`'s fetch bootstrap got
/// the UNRESOLVED string, `url::Url::parse` rejected it, and the worker fetched nothing — silently,
/// on its own thread. Falls back to the raw string when it truly does not parse (the
/// `__workerScripts` test seam's keys are often not URLs at all).
pub(crate) fn worker_url(scope: &mut v8::PinScope, url: &str) -> String {
    let base = script_base(scope);
    match url::Url::parse(&base).and_then(|b| b.join(url)) {
        Ok(u) => u.to_string(),
        Err(_) => match url::Url::parse(url) {
            Ok(u) => u.to_string(),
            Err(_) => url.to_string(),
        },
    }
}

// ── the receive pumps ───────────────────────────────────────────────────────────────────────────

/// What a worker tells its creator's `Worker` object about an error, on the `op_worker_err_recv` channel,
/// as JSON. An uncaught runtime error carries the `message` and site its own realm's `ErrorEvent` carried
/// (an `ErrorEvent` here); a script that never loaded carries no message (a plain `Event`).
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
pub struct WorkerErrorFrame {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub lineno: u32,
    #[serde(default)]
    pub colno: u32,
}

impl WorkerErrorFrame {
    /// The worker's script could not be fetched, parsed or linked.
    pub fn not_loaded() -> WorkerErrorFrame {
        WorkerErrorFrame::default()
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// A frame of another shape (no sender writes one) reads as a message with no site.
    pub fn decode(bytes: &[u8]) -> WorkerErrorFrame {
        serde_json::from_slice(bytes).unwrap_or_else(|_| WorkerErrorFrame { message: Some(String::from_utf8_lossy(bytes).into_owned()), ..WorkerErrorFrame::default() })
    }
}

/// Which channel a pump is draining. The four are the same four `host.js` ran, in the same order.
const PUMPS: [(&str, u8); 5] = [
    ("op_worker_recv", 0),      // worker -> page postMessage
    ("op_worker_bc_recv", 1),   // worker BroadcastChannel -> the page's channels of that name
    ("op_worker_port_recv", 2), // a transferred MessagePort's post -> the page's paired port
    ("op_worker_err_recv", 3),  // an uncaught worker error -> Worker.onerror + an `error` event
    ("op_worker_timing_recv", 4), // the worker's own script arrived -> the page's resource-timing entry
];

/// Arm ONE step of one pump: call the async op, unref its promise, and attach the native continuation
/// that handles the frame and re-arms. This is the `while (true) { await … }` loop of the JS, with the
/// same UNREF in the same place — an explicit continuation instead of an `async` closure, because a
/// Rust callback cannot `await`.
pub(crate) fn arm_pump(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, id: f64, kind: u8) {
    let Some((op, _)) = PUMPS.iter().find(|(_, k)| *k == kind) else { return };
    let Some(f) = deno_op(scope, op) else { return };
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[idv]) else { return };
    unref(scope, p);
    let d = v8::Array::new(scope, 3);
    d.set_index(scope, 0, target.into());
    d.set_index(scope, 1, idv);
    let kv: v8::Local<v8::Value> = v8::Integer::new(scope, kind as i32).into();
    d.set_index(scope, 2, kv);
    let step = crate::binding::protos::build_fn_data(scope, cb_pump_step, "", 1, d.into());
    // The promise's own `then`, never the page-visible `Promise.prototype.then`.
    // Each message is a task of its own (`then_task`), not a promise job.
    if let Ok(promise) = v8::Local::<v8::Promise>::try_from(p) {
        crate::then_task(scope, promise, step, None);
    }
}

/// The RAW bytes behind a `Uint8Array` the op resolved with.
fn frame_bytes(v: v8::Local<v8::Value>) -> Vec<u8> {
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) else { return Vec::new() };
    let mut buf = vec![0u8; view.byte_length()];
    view.copy_contents(&mut buf);
    buf
}

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: &[u8]) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.to_vec()).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match v8::Uint8Array::new(scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(scope).into(),
    }
}

/// One frame off one pump, then re-arm. An EMPTY frame means the channel closed (the worker was
/// terminated or closed itself), and the loop simply stops — the `break` of the JS.
fn cb_pump_step(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(target) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(id) = d.get_index(scope, 1).and_then(|v| v.number_value(scope)) else { return };
    let kind = d.get_index(scope, 2).and_then(|v| v.integer_value(scope)).unwrap_or(0) as u8;
    let frame = frame_bytes(args.get(0));
    match kind {
        // worker -> main postMessage: the V8 stream, or a port frame when the worker transferred ports
        // -- `op_worker_msg_in` answers `[data, ports]` either way.
        0 => {
            if frame.is_empty() {
                return;
            }
            let Some(de) = deno_op(scope, "op_worker_msg_in") else { return };
            let bytes = u8_array(scope, &frame);
            let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            // the Worker object: the op builds the data and ports in ITS realm (a frame's worker delivers into
            // the frame)
            let tv: v8::Local<v8::Value> = target.into();
            let Some(pair) = de.call(scope, undef, &[idv, bytes, tv]).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
            let Some(data) = pair.get_index(scope, 0) else { return };
            let ports = pair.get_index(scope, 1).unwrap_or_else(|| v8::Array::new(scope, 0).into());
            let init = crate::binding::protos::init_dict(scope);
            set_prop(scope, init, "data", data);
            set_prop(scope, init, "ports", ports);
            let ty = str_val(scope, "message");
            if let Some(ev) = new_global(scope, "MessageEvent", &[ty, init.into()]) {
                // Delivered by the engine, so TRUSTED (Chrome: `isTrusted` true).
                crate::core::html::html_dialog_element::mark_trusted(scope, ev);
                let tv: v8::Local<v8::Value> = target.into();
                let evv: v8::Local<v8::Value> = ev.into();
                crate::binding::protos::engine_call(scope, tv, "dispatchEvent", &[evv]);
            }
        }
        // `[u32 LE name_len][name][structured-clone bytes]` -> the page's channels of that name.
        1 => {
            if frame.len() < 4 {
                return;
            }
            let n = u32_le(&frame, 0) as usize;
            if frame.len() < 4 + n {
                return;
            }
            let name = String::from_utf8_lossy(&frame[4..4 + n]).into_owned();
            let Some(f) = deno_op(scope, "op_bc_deliver_external") else { return };
            let nv = str_val(scope, &name);
            let body = u8_array(scope, &frame[4 + n..]);
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            f.call(scope, undef, &[nv, body]);
        }
        // `[u32 LE channel_id][structured-clone bytes]` -> the page's paired port for that id.
        2 => {
            if frame.len() < 4 {
                return;
            }
            let cid = u32_le(&frame, 0);
            let Some(f) = deno_op(scope, "op_port_deliver_external") else { return };
            let cv: v8::Local<v8::Value> = v8::Number::new(scope, cid as f64).into();
            let body = u8_array(scope, &frame[4..]);
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            f.call(scope, undef, &[cv, body]);
        }
        // The worker's script timing, as JSON -> this document's resource-timing entry.
        4 => {
            if frame.is_empty() {
                return;
            }
            // ONE frame per worker: the script loads once, so the pump is not re-armed after it.
            if let Ok(timing) = serde_json::from_slice::<spec::FetchTiming>(&frame) {
                crate::core::timing::resource_timing::worker_script_loaded(scope, timing);
            }
            return;
        }
        // A worker's error ([`WorkerErrorFrame`]) -> an `error` event at the parent Worker, trusted and
        // cancelable: an `ErrorEvent` with the worker's message and site and a null `error`, or a plain
        // `Event` when the script never loaded (Chrome 150, measured: a 404, a module with a syntax error,
        // a module whose import 404s). An `ErrorEvent` nothing cancelled is reported again in THIS realm,
        // one level up ([`report_worker_error`](crate::core::events::report::report_worker_error)).
        _ => {
            if frame.is_empty() {
                return;
            }
            let report = WorkerErrorFrame::decode(&frame);
            let tv: v8::Local<v8::Value> = target.into();
            let init = crate::binding::protos::init_dict(scope);
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            set_prop(scope, init, "cancelable", t);
            let ty = str_val(scope, "error");
            match &report.message {
                None => {
                    if let Some(ev) = new_global(scope, "Event", &[ty, init.into()]) {
                        crate::core::html::html_dialog_element::mark_trusted(scope, ev);
                        crate::core::events::event_target::et_dispatch(scope, tv, ev);
                    }
                }
                Some(message) => {
                    let mv = str_val(scope, message);
                    set_prop(scope, init, "message", mv);
                    let fv = str_val(scope, &report.filename);
                    set_prop(scope, init, "filename", fv);
                    let l: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, report.lineno).into();
                    set_prop(scope, init, "lineno", l);
                    let c: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, report.colno).into();
                    set_prop(scope, init, "colno", c);
                    let nul: v8::Local<v8::Value> = v8::null(scope).into();
                    set_prop(scope, init, "error", nul);
                    if let Some(ev) = new_global(scope, "ErrorEvent", &[ty, init.into()]) {
                        crate::core::html::html_dialog_element::mark_trusted(scope, ev);
                        if crate::core::events::event_target::et_dispatch(scope, tv, ev) {
                            crate::core::events::report::report_worker_error(scope, message, &report.filename, report.lineno, report.colno);
                        }
                    }
                }
            }
        }
    }
    arm_pump(scope, target, id, kind);
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

/// `new Worker(USVString scriptURL, optional WorkerOptions options)` — `length` 1.
///
/// The constructor stays SYNCHRONOUS: for the test/blob seam the source string is resolved here, and a
/// real URL worker fetches its own top script asynchronously on ITS OWN thread (W1.6).
/// A worker whose script the document's CSP refused: `error`, a plain cancelable Event (Blink
/// `AbstractWorker::DispatchErrorEvent`).
fn cb_csp_refused(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let target = crate::binding::protos::body_data(scope, &args);
    let init = crate::binding::protos::init_dict(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    set_prop(scope, init, "cancelable", t);
    let ty = str_val(scope, "error");
    if let Some(ev) = new_global(scope, "Event", &[ty, init.into()]) {
        crate::core::html::html_dialog_element::mark_trusted(scope, ev);
        crate::core::events::event_target::et_dispatch(scope, target, ev);
    }
}

pub(crate) fn cb_worker_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "Worker") || !require_args(scope, &args, "Worker", 1) {
        return;
    }
    // (USVString or TrustedScriptURL): a TrustedScriptURL read as itself, nothing on it called -- an interactive-widget challenge
    // challenge frame's `new Worker(policy.createScriptURL(blobURL))` called its `toString` here
    // a string through the current realm's Trusted Types ("Worker constructor")
    let url = if crate::core::trusted_types::tables::is_trusted(scope, args.get(0), "TrustedScriptURL") {
        crate::core::trusted_types::tables::sink_text(scope, args.get(0), "TrustedScriptURL")
    } else {
        arg_string(scope, &args, 0, "Worker").and_then(|u| {
            let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedScriptURL", sample: "Worker constructor", context: "Failed to construct 'Worker'" };
            let at = crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context());
            crate::core::trusted_types::tables::check_text(scope, u, &sink, at)
        })
    };
    let Some(url) = url else { return };
    // `WorkerOptions`, converted as WebIDL converts it (Chrome 150): `credentials`, `name`, `type`,
    // each read once in that order and converted as it is read -- a bad `credentials` throws before `name` is read, a
    // getter's throw ends the constructor, a string or a number is no `WorkerOptions` at all. korvus read `name` and
    // `type` alone, took a Symbol `name` for "" and let a getter's throw vanish.
    let Ok(opts) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "WorkerOptions", "Failed to construct 'Worker': ") else { return };
    // `type`: a `module` worker's script is an ES module -- it was run as a classic script, and its
    // first `import` threw. `name` is the worker global's `name` (korvus answered "").
    let kind = get_prop(scope, opts, "type").filter(|t| t.is_string()).map(|t| t.to_rust_string_lossy(scope)).unwrap_or_else(|| "classic".into());
    let name = get_prop(scope, opts, "name").filter(|n| n.is_string()).map(|n| n.to_rust_string_lossy(scope)).unwrap_or_default();
    // the script's URL, completed and checked against this realm's origin (`AbstractWorker::ResolveURL`) -- unless a
    // harness registered the string itself (`__workerScripts`, which is no URL at all)
    let seam = seam_source(scope, &url);
    let resolved = match &seam {
        Some(_) => None,
        None => match resolve_script_url(scope, &url, "Worker") {
            Some(u) => Some(u),
            None => return,
        },
    };
    let this = args.this();
    brand(scope, this, "Worker");
    rv.set(this.into());
    // `worker-src`: a script the document's CSP refuses is never fetched. Blink 150 throws nothing
    // (`kNoThrowForCSPBlockedWorker`, on by default) and fires a plain cancelable `error` at the worker from a task, as
    // for a script that failed to load; each refusing policy reports a violation.
    if let Some(u) = &resolved {
        if crate::core::html::csp::realm_request_blocked(scope, u, "worker") {
            let f = crate::binding::protos::build_fn_data(scope, cb_csp_refused, "", 0, this.into());
            crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
            return;
        }
    }
    // A realm whose isolate registers no spawn op gets the branded shell and nothing else. The window
    // and every worker isolate register it (a worker's own workers are nested workers).
    let Some(spawn) = deno_op(scope, "op_worker_spawn") else { return };
    let src = match seam {
        Some(s) => Some(s),
        None => match local_source(scope, &url) {
            // A MODULE script's type must be JavaScript, a `blob:` or `data:` one's too (`ModuleScriptFetcher`; Chrome
            // 150, measured: a module worker from an untyped Blob fires `error`, from `data:text/javascript` runs).
            // Handed on as a URL, it is one no network request answers, and the worker fails to load.
            Some((_, ty)) if kind == "module" && !crate::page::is_javascript_mime_essence(transport::mime::blink_extract_mime_type(&ty.to_ascii_lowercase())) => None,
            other => other.map(|(text, _)| text),
        },
    };
    let (src_s, url_s) = match &src {
        Some(s) => (s.clone(), url.clone()),
        None => (String::new(), resolved.clone().unwrap_or_else(|| worker_url(scope, &url))),
    };
    let a = str_val(scope, &src_s);
    let b = str_val(scope, &url_s);
    let c = str_val(scope, &kind);
    // A worker made from a blob: or data: URL takes its creator's policy: a cross-origin-isolated
    // document's such worker is isolated too -- SharedArrayBuffer, crossOriginIsolated true (webr.r-wasm.org).
    // A network worker's own response headers decide in Chrome; this build keeps those non-isolated.
    let isolated = src.is_some() && crate::core::html::isolation_policy::cross_origin_isolated(scope);
    let d: v8::Local<v8::Value> = v8::Boolean::new(scope, isolated).into();
    // The creating document's site for cookies: a worker a cross-site frame starts makes
    // its requests in that frame's cross-site context.
    let sfc = crate::core::html::browsing_context::realm_site_for_cookies(scope);
    let e = str_val(scope, &sfc);
    // a worker whose script no response delivered (`blob:`, `data:`) holds its creator's policies; one fetched, its
    // response's (the worker sets those itself)
    let csp = if src.is_some() { crate::core::html::browsing_context::current_realm_csp_headers(scope) } else { Vec::new() };
    let f = str_val(scope, &serde_json::to_string(&csp).unwrap_or_else(|_| "[]".into()));
    let g = str_val(scope, &name);
    // the document that asks for the script: a frame's own, not the page's
    let client = script_client(scope);
    let h = str_val(scope, &client);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(id) = spawn.call(scope, undef, &[a, b, c, d, e, f, g, h]).and_then(|v| v.number_value(scope)) else { return };
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
    slot_set(scope, this, ID_SLOT, idv);
    for (_, kind) in PUMPS {
        arm_pump(scope, this, id, kind);
    }
    // the worker begins when this task is over -- the microtask checkpoint that ends it (the runtime's `WorkerStart`)
    let begin = crate::binding::protos::build_fn_data(scope, cb_worker_begin, "", 0, idv);
    scope.enqueue_microtask(begin);
}

/// The creating task is over: let the worker begin.
fn cb_worker_begin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = crate::binding::protos::body_data(scope, &args);
    let Some(start) = deno_op(scope, "op_worker_start") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let _ = start.call(scope, undef, &[id]);
}

// ── the rows ────────────────────────────────────────────────────────────────────────────────────

/// Mount a row whose native body is built by a `korvus-runtime` FACTORY OP (`op_worker_post_fn`,
/// `op_worker_terminate_fn`): call the factory with the row's WebIDL name and length, then wrap the
/// function it returns in this interface's brand so a foreign receiver throws `Illegal invocation`.
///
/// A realm where the factory is not registered gets the D2 stub for that row — the one
/// not-implemented mechanism (invariant I7), counted as a stub rather than silently absent.
pub(crate) fn factory_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, op: &str) {
    let Some(f) = deno_op(scope, op) else {
        crate::install::installer::put_table_stub(scope, proto, member, true);
        return;
    };
    let name = str_val(scope, member.name);
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, member.length as i32).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(native) = f.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        crate::install::installer::put_table_stub(scope, proto, member, true);
        return;
    };
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let guard = crate::install::webidl::brand_guard(scope, iface, member, native);
    crate::install::kits::define_value(scope, proto, member, guard, true);
}

/// The body the two method rows of [`WORKER`] name.
pub fn install_worker(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "postMessage" => factory_row(scope, proto, member, "op_worker_post_fn"),
        "terminate" => factory_row(scope, proto, member, "op_worker_terminate_fn"),
        other => debug_assert!(false, "install_worker: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `Worker` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: onmessage, postMessage, terminate,
// onerror. `[value, both]`: both captures record `newable: true`, `length: 1` and the prototype chain
// `Worker.prototype -> EventTarget.prototype`.
//
// `evt_handler` rather than `event_handler` for the two handlers: the branded pair, so
// `Worker.prototype.onmessage.call({})` throws `Illegal invocation` the way Chrome does instead of
// answering `null`. Only an interface whose constructor stamps a brand may name it, which this one now
// does.
webapi::interface! {
    "Worker" : "EventTarget" [value, both] {
        ctor    "Worker"                        len 1 required 1 = stub,
        handler "onmessage"                                      = evt_handler,
        method  "postMessage" : Undefined len 1                  = install_worker,
        method  "terminate"   : Undefined len 0                  = install_worker,
        handler "onerror"                                        = evt_handler
    }
}

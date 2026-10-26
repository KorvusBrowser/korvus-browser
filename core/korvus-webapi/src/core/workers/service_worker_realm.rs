//! `core/workers/service_worker_realm.rs` — what a ServiceWorker ISOLATE does when it starts:
//! the SW half of the cross-isolate fetch transport, and the install -> activate
//! lifecycle.
//!
//! # What this replaces
//!
//! The whole of `SW_PRELUDE`, the ~20-line JS string `core/korvus-runtime/src/worker.rs` prepended to
//! every ServiceWorker's own source (`spawn_on_thread(format!("{SW_PRELUDE}{source}"), …)`). Three
//! separate things lived in that string:
//!
//! 1. **A fake `FetchEvent`** — `new Event('fetch')` with `request`, `respondWith` (and, in the
//!    lifecycle half, `waitUntil`) written on as OWN properties, and an object literal
//!    `{url, method:'GET'}` standing in for a `Request`. That is invariants I1, I3 and I9 together
//!    and it is gone: the interfaces are tables (`core/events/extendable_event.rs`) and the events
//!    are minted natively below.
//! 2. **The lifecycle** — a `setTimeout(0)` that fired `install` then `activate` back to back, with
//!    `waitUntil` a no-op arrow, so an `install` handler's promise was discarded and `activate` ran
//!    while it was still pending. [`run_lifecycle`] awaits them.
//! 3. **The transport** — two `BroadcastChannel`s, `korvus-sw-fetch` (page -> SW) and
//!    `korvus-sw-fetch-resp` (SW -> page). THE TRANSPORT IS UNCHANGED: the same two channel names,
//!    the same `{id, url}` request frame and the same `{id, body}` reply frame, because the page half
//!    (`modules/fetch/fetch_global.rs`) speaks exactly that and moving event construction into Rust
//!    does not require moving the wire. What changed is that this side of it is a native
//!    `onmessage` rather than a JS arrow in a Rust string literal.
//!
//! `__swScriptLoaded` also left the string: it is a NATIVE function `korvus-runtime` mounts on the
//! Symbol-keyed bridge, which is the shape `__flushConnects` and `__reportWorkerError` already have
//! (`core/korvus-runtime/src/worker_recv.rs`) and for the same stated reason — a boot script injected
//! as a worker's top-level source runs after `Deno` is stripped and has no op table to reach. It is
//! not a new bridge slot; it is the same name, no longer defined in JavaScript.
//!
//! # The response shape, preserved exactly
//!
//! The deleted JS was
//!
//! ```js
//! rp.then((res) => (res && res.text) ? res.text() : res)
//!   .then((body) => __fr.postMessage({ id, body: String(body == null ? '' : body) }));
//! ```
//!
//! and [`cb_take_text`] / [`cb_post_body`] are those two continuations, in the same order, with the
//! same null handling. A handler that did not `respondWith` still posts `{id, body: null}`, which is
//! how the page falls back to the network — `fetch_global::cb_sw_response` reads a null body as
//! exactly that, so the fall-through is preserved rather than re-decided here.

use std::cell::RefCell;

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, new_global, set_prop, str_val};
use crate::core::events::extendable_event::{
    dispatch_on_global, finish_event, mint_fetch_event, mint_lifecycle_event, resolve_handled, take_wait_promises,
    taken_response,
};

/// The two channels, parked in `OpState` for the realm's lifetime.
///
/// A `BroadcastChannel` with no strong reference is collectable and a collected one stops delivering
/// — the same defect `fetch_global::SwRespChannel` records on the page's side. The JS held both in
/// `const`s of the prelude's own closure; this is that closure's lifetime, made explicit.
#[derive(Default)]
pub(crate) struct SwRealmChannels {
    req: RefCell<Option<v8::Global<v8::Object>>>,
    resp: RefCell<Option<v8::Global<v8::Object>>>,
}

fn resp_channel<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = {
        let st = op_state.borrow();
        let ch = st.try_borrow::<SwRealmChannels>()?;
        let held = ch.resp.borrow().clone();
        held?
    };
    Some(v8::Local::new(scope, &g))
}

/// Open the SW side of the fetch bridge and mount its native `onmessage`.
///
/// Called once, from the ServiceWorker realm's Rust block in `korvus-runtime`, and only there — a
/// dedicated or shared worker opens neither channel, exactly as before (the prelude was prepended to
/// ServiceWorker sources only).
pub fn install_sw_fetch_transport(scope: &mut v8::PinScope) {
    let name = str_val(scope, "korvus-sw-fetch");
    let Some(req) = new_global(scope, "BroadcastChannel", &[name]) else { return };
    let rname = str_val(scope, "korvus-sw-fetch-resp");
    let Some(resp) = new_global(scope, "BroadcastChannel", &[rname]) else { return };
    let on = build_fn(scope, cb_sw_fetch_request, "", 1);
    set_prop(scope, req, "onmessage", on.into());
    let rg = v8::Global::new(scope, req);
    let pg = v8::Global::new(scope, resp);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<SwRealmChannels>().is_none() {
        st.put(SwRealmChannels::default());
    }
    let ch = st.borrow::<SwRealmChannels>();
    *ch.req.borrow_mut() = Some(rg);
    *ch.resp.borrow_mut() = Some(pg);
}

/// One `{id, url}` request off `korvus-sw-fetch`: build a real `FetchEvent` over a real `Request`,
/// dispatch it, and answer on `korvus-sw-fetch-resp`.
fn cb_sw_fetch_request(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(e) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let data = get_prop(scope, e, "data").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let id = data.and_then(|d| get_prop(scope, d, "id")).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let url = data
        .and_then(|d| get_prop(scope, d, "url"))
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    // A REAL `Request`, not the `{url, method:'GET'}` object literal the JS passed off as one.
    // `Request` is `[Exposed=(Window,Worker)]` and `install::live::install_worker_abort_style` mounts
    // it in every worker realm, so `e.request instanceof Request` and `e.request.constructor.name`
    // answer what a browser answers. Its `method` defaults to "GET", which is the only method the
    // page half's frame can carry today (`fetch_global` posts `{id, url}`), so nothing is lost by
    // letting the constructor supply it rather than writing it by hand.
    let u = str_val(scope, &url);
    let Some(request) = new_global(scope, "Request", &[u]) else {
        post_body(scope, id, None);
        return;
    };
    let rv: v8::Local<v8::Value> = request.into();
    let Some(ev) = mint_fetch_event(scope, rv, "korvus-client") else {
        post_body(scope, id, None);
        return;
    };
    dispatch_on_global(scope, ev);
    // The handler has returned, so the event is closed: a later `respondWith` or `waitUntil` is the
    // "already finished" InvalidStateError, which is where Chrome closes it too (measured).
    finish_event(scope, ev);
    match taken_response(scope, ev) {
        Some(p) => {
            let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
            let evv: v8::Local<v8::Value> = ev.into();
            let d = v8::Array::new(scope, 2);
            d.set_index(scope, 0, idv);
            d.set_index(scope, 1, evv);
            let take = build_fn(scope, cb_take_text, "", 1);
            let post = build_fn_data(scope, cb_post_body, "", 1, d.into());
            if let Some(p2) = crate::binding::protos::promise_then(scope, p, take, None) {
                crate::binding::protos::promise_then(scope, p2, post, None);
            }
        }
        None => {
            post_body(scope, id, None);
            resolve_handled(scope, ev);
        }
    }
}

/// `(res) => (res && res.text) ? res.text() : res` — the first continuation, verbatim.
fn cb_take_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let res = args.get(0);
    let Ok(o) = v8::Local::<v8::Object>::try_from(res) else {
        rv.set(res);
        return;
    };
    let has_text = get_prop(scope, o, "text").map(|v| v.is_function()).unwrap_or(false);
    if !has_text {
        rv.set(res);
        return;
    }
    if let Some(t) = crate::binding::protos::pristine_call(scope, res, "Response.text", &[]) {
        rv.set(t);
    } else {
        rv.set(res);
    }
}

/// `(body) => __fr.postMessage({id, body: String(body == null ? '' : body)})`. data = `[id, event]`.
fn cb_post_body(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let id = d.get_index(scope, 0).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let body = args.get(0);
    let s = if body.is_null_or_undefined() { String::new() } else { body.to_rust_string_lossy(scope) };
    post_body(scope, id, Some(&s));
    if let Some(ev) = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        resolve_handled(scope, ev);
    }
}

/// Publish one `{id, body}` reply. `None` is the JS's `body: null` — "the handler did not respond",
/// which the page half turns into a network fetch.
fn post_body(scope: &mut v8::PinScope, id: f64, body: Option<&str>) {
    let Some(ch) = resp_channel(scope) else { return };
    let msg = crate::binding::protos::init_dict(scope);
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
    set_prop(scope, msg, "id", idv);
    let bv: v8::Local<v8::Value> = match body {
        Some(b) => str_val(scope, b),
        None => v8::null(scope).into(),
    };
    set_prop(scope, msg, "body", bv);
    let chv: v8::Local<v8::Value> = ch.into();
    crate::binding::protos::pristine_call(scope, chv, "BroadcastChannel.postMessage", &[msg.into()]);
}

// ── the install -> activate lifecycle ───────────────────────────────────────────────────────────

/// The ServiceWorker's top-level script has been evaluated: run its lifecycle.
///
/// The `setTimeout(0)` is the JS's and the reason is the JS's: the lifecycle has to fire AFTER the
/// user script's top level, which is where the `install`/`activate` listeners are registered. It is
/// the realm's own timer service — a kernel service a Web-API body calls, which is exactly the split
/// invariant I9 describes — rather than a second scheduler.
pub fn script_loaded(scope: &mut v8::PinScope) {
    let tick = build_fn(scope, cb_lifecycle_tick, "", 0);
    // The engine's task, not the worker script's `setTimeout`, which it may replace.
    crate::core::intersection_observer::rendering::engine_task(scope, tick, 0.0);
}

fn cb_lifecycle_tick(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    run_lifecycle(scope);
}

/// `install`, then — once every promise its handlers passed to `waitUntil` has settled — `activate`.
///
/// THE WAIT IS THE BEHAVIOUR CHANGE. The deleted JS bolted `ev.waitUntil = () => {}` onto both events
/// and fired them back to back, so an `install` handler that opened a cache raced the `activate` that
/// was supposed to follow it. `ExtendableEvent.waitUntil` records its promises now
/// (`core/events/extendable_event.rs`) and this reads them back: with none recorded, `activate` fires
/// on the spot exactly as before, so a fixture that does not call `waitUntil` sees no change at all.
fn run_lifecycle(scope: &mut v8::PinScope) {
    let Some(ev) = mint_lifecycle_event(scope, "install") else { return };
    dispatch_on_global(scope, ev);
    finish_event(scope, ev);
    match take_wait_promises(scope, ev) {
        Some(all) => {
            let next = build_fn(scope, cb_activate, "", 1);
            // `.then(next, next)`: a REJECTED extend-lifetime promise still ends the install step (the
            // spec marks the registration as failed; this engine has no failed state to mark, and
            // stalling the worker for ever would be strictly worse than proceeding).
            crate::binding::protos::promise_then(scope, all, next, Some(next));
        }
        None => fire_activate(scope),
    }
}

fn cb_activate(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    fire_activate(scope);
}

/// installed -> activating (the registration's active worker: `ready` settles on the page) -> the
/// `activate` event -> activated once its `waitUntil` promises settle. Each step is reported to the
/// registering page, which had no lifecycle at all: its registration stayed `installing`.
fn fire_activate(scope: &mut v8::PinScope) {
    report_lifecycle(scope, "installed");
    report_lifecycle(scope, "activating");
    let Some(ev) = mint_lifecycle_event(scope, "activate") else { return };
    dispatch_on_global(scope, ev);
    finish_event(scope, ev);
    match take_wait_promises(scope, ev) {
        Some(all) => {
            let done = build_fn(scope, cb_activated, "", 1);
            crate::binding::protos::promise_then(scope, all, done, Some(done));
        }
        None => report_lifecycle(scope, "activated"),
    }
}

fn cb_activated(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    report_lifecycle(scope, "activated");
}

/// Post `{life, url}` to the registering page on the fetch bridge's SW -> page channel,
/// keyed by this worker's own script URL (the `WorkerLocation`'s private slot, not anything the worker
/// script can write). A realm with no bridge (not a ServiceWorker) has no page to tell.
pub(crate) fn report_lifecycle(scope: &mut v8::PinScope, life: &str) {
    let Some(ch) = resp_channel(scope) else { return };
    let url = crate::core::workers::worker_global_scope::scope_href(scope);
    let msg = crate::binding::protos::init_dict(scope);
    let lv = str_val(scope, life);
    set_prop(scope, msg, "life", lv);
    let uv = str_val(scope, &url);
    set_prop(scope, msg, "url", uv);
    crate::binding::protos::pristine_call(scope, ch.into(), "BroadcastChannel.postMessage", &[msg.into()]);
}

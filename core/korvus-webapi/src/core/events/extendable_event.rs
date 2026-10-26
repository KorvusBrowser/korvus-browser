//! `core/events/extendable_event.rs` — `ExtendableEvent`, `FetchEvent` and `InstallEvent`, the
//! ServiceWorker realm's event chain.
//!
//! # What this replaces
//!
//! `SW_PRELUDE`, a ~20-line JS string in `core/korvus-runtime/src/worker.rs` that was prepended to
//! every ServiceWorker's own source. It built a `fetch` event like this:
//!
//! ```js
//! const ev = new globalThis.Event('fetch');
//! ev.request = { url: d.url, method: 'GET' };
//! ev.respondWith = (x) => { responded = true; rp = Promise.resolve(x); };
//! ```
//!
//! — three OWN properties written onto an `Event` instance, one of them an object literal standing in
//! for a `Request`, plus a second `fire()` that bolted `ev.waitUntil = () => {}` onto the `install`
//! and `activate` events. A real event instance owns exactly one property, `isTrusted` (measured
//! below), so every one of those was the own-property tell `instance-tells` exists to catch; the
//! interfaces did not exist at all, so `e.constructor.name` was `Event` and `e instanceof FetchEvent`
//! was a `ReferenceError`; and `waitUntil` was a no-op, so an `install` handler that awaited a cache
//! open raced `activate` rather than delaying it. Invariants I1, I3 and I9, together.
//!
//! # THE CAPTURE BUNDLE DOES NOT RECORD THIS REALM — what was used as the authority, and why
//!
//! Checked first, as the brief asks. The bundle
//! (`plugins/korvus-persona/bundles/brave-151-linux-20260907T1006`) records these names in exactly
//! one way and it is a NEGATIVE:
//!
//! * `surface.json` (the window realm) and `worker.surface.json` (a DEDICATED worker) contain the
//!   strings `FetchEvent` and `ExtendableEvent` **zero times** — neither interface, neither global.
//! * `oracle/FetchEvent.json` and `oracle/ExtendableEvent.json` exist and every entry in them is
//!   `"throws": "ReferenceError"` — the rig evaluated `new FetchEvent("x", {})` in the PAGE realm and
//!   recorded that the name is not there.
//!
//! That is a real and binding fact and this unit obeys it: it is the capture confirming
//! `[Exposed=ServiceWorker]`, so neither name may appear in the window or the dedicated-worker realm,
//! and the install below is gated on the ServiceWorker realm for exactly that reason. But the capture
//! rig never enters a `ServiceWorkerGlobalScope`, so the bundle says NOTHING about the member list,
//! the member ORDER, the descriptor flags, the constructor lengths or the error messages.
//!
//! So the authority for the positive shape is truth source #2, **real Chrome**, measured directly —
//! not webref and not the spec. `google-chrome --headless=new` 150.0.7871.46 driven by Playwright
//! (python 1.60.0) over a loopback HTTP server, registering a real ServiceWorker and evaluating in
//! ITS realm, 2026-09-12. This is the same footing `SharedWorkerGlobalScope` and
//! `ServiceWorkerGlobalScope` are already on in `core/workers/worker_global_scope.rs` ("Neither scope
//! is in the bundle") — except that those two fell back to WebIDL, and this file could do better
//! because the realm is reachable from a driver. Where webref and Chrome disagree, Chrome wins here:
//! webref's `FetchEvent` has no `isReload` and Chrome 150 does.
//!
//! ## Measured, and what each line decides
//!
//! ```text
//! ExtendableEvent  ctor.length 1   prototype: ["waitUntil", "constructor"]
//!                  waitUntil:  data, enumerable, writable, configurable, length 1
//! FetchEvent       ctor.length 2   prototype: ["request", "clientId", "resultingClientId",
//!                                              "isReload", "preloadResponse", "handled",
//!                                              "respondWith", "constructor"]
//!                  the six attributes: enumerable configurable ACCESSORS, getter length 0
//!                  respondWith: data, enumerable, writable, configurable, length 1
//! InstallEvent     ctor.length 1   prototype: ["addRoutes", "constructor"]
//! ```
//!
//! `constructor` is LAST on all three, which is where `installer::create_interface` puts it — so for
//! once the tree-wide convention and Chrome agree and there is no gap to record.
//!
//! Chains: `FetchEvent -> ExtendableEvent -> Event`, `InstallEvent -> ExtendableEvent -> Event`, and
//! the interface OBJECTS chain the same way (`Object.getPrototypeOf(FetchEvent).name` is
//! `ExtendableEvent`). `Symbol.toStringTag` is the interface's own name on each.
//!
//! A DISPATCHED event owns `["isTrusted"]` and nothing else — `install` (an `InstallEvent`),
//! `activate` (an `ExtendableEvent`) and `fetch` (a `FetchEvent`) all three. `fetch` is
//! `cancelable: true, bubbles: false`; `install` and `activate` are `cancelable: false,
//! bubbles: false`.
//!
//! Errors, verbatim:
//!
//! ```text
//! new ExtendableEvent()              TypeError: Failed to construct 'ExtendableEvent': 1 argument
//!                                    required, but only 0 present.
//! new FetchEvent("fetch")            TypeError: Failed to construct 'FetchEvent': 2 arguments
//!                                    required, but only 1 present.
//! new FetchEvent("fetch", {})        TypeError: Failed to construct 'FetchEvent': Failed to read the
//!                                    'request' property from 'FetchEventInit': Required member is
//!                                    undefined.
//! ExtendableEvent.prototype.waitUntil.call({})     TypeError: Illegal invocation
//! FetchEvent.prototype.respondWith.call({})        TypeError: Illegal invocation
//! (the request getter).call({})                    TypeError: Illegal invocation
//! e.waitUntil()                      TypeError: Failed to execute 'waitUntil' on 'ExtendableEvent':
//!                                    1 argument required, but only 0 present.
//! e.respondWith()                    TypeError: Failed to execute 'respondWith' on 'FetchEvent':
//!                                    1 argument required, but only 0 present.
//! ```
//!
//! and the two `InvalidStateError`s, which are the interesting half:
//!
//! ```text
//! waitUntil on a SCRIPT-CONSTRUCTED event of ANY of the three interfaces:
//!   "Failed to execute 'waitUntil' on 'ExtendableEvent': Can not call waitUntil on a script
//!    constructed ExtendableEvent."
//!   -- measured on all three; the sentence does NOT take the receiver's most-derived name.
//! respondWith twice during dispatch:
//!   "Failed to execute 'respondWith' on 'FetchEvent': respondWith() was already called."
//! respondWith after the handler returned:
//!   "Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished."
//! waitUntil after the handler returned:
//!   "Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is already finished and
//!    no extend lifetime promises are outstanding."
//! ```
//!
//! `respondWith` on a script-constructed `FetchEvent` is allowed (measured: "ok"), and so is calling
//! it TWICE on one — the "already called" guard applies to a dispatching event only. That asymmetry
//! with `waitUntil` is Chrome's, and it is reproduced rather than tidied.
//!
//! # State lives in slots, never in own properties
//!
//! Every member reads a private slot in the shared `korvus_evt_*` namespace (`event.rs`), exactly as
//! `CustomEvent.detail` and `MouseEvent.clientX` do, so an instance's own-key list stays
//! `["isTrusted"]`. The four engine-internal flags below start with `_`, the same convention
//! `Event`'s own `_stop` / `_stopImmediate` already use; a slot is invisible to enumeration, to
//! `hasOwnProperty` and to a Proxy trap.

use deno_core::v8;

use crate::binding::protos::{get_prop, global_get, set_prop, str_val, throw_dom};
use crate::core::events::ctor::{arg_string, brand, dict_any, dict_string, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_flag_get, evt_flag_set, evt_slot_read, evt_slot_write, trusted_key};
use crate::core::events::event::evt_branded_method;
use crate::install::MemberDef;

// ── the internal slots ──────────────────────────────────────────────────────────────────────────

/// Set by [`mint_dispatch_event`] only: this event was created by the engine for a real dispatch, so
/// `waitUntil` is legal on it. A script-constructed one never has it, which is the whole of Chrome's
/// "script constructed" test.
const SLOT_DISPATCHING: &str = "_swDispatching";
/// Set when the dispatch that minted the event has returned: both methods are `InvalidStateError`
/// after this ("The event handler is already finished").
const SLOT_FINISHED: &str = "_swFinished";
/// `respondWith` was called on this (dispatching) event.
const SLOT_RESPONDED: &str = "_swResponded";
/// The promise `respondWith` recorded — what the SW fetch transport awaits.
const SLOT_RESPONSE: &str = "_swResponse";
/// The `Array` of promises `waitUntil` recorded, present only on a dispatching event.
const SLOT_WAITS: &str = "_swWaits";
/// The key of this event's parked `handled` resolver in [`HandledResolvers`].
const SLOT_HANDLED_KEY: &str = "_swHandledKey";

/// The `handled` resolvers of the events currently in flight in this realm.
///
/// A `v8::PromiseResolver` CANNOT be read back out of a private slot: the v8 crate gives
/// `PromiseResolver -> Value/Object` and no conversion the other way (checked in `v8-150.4.0`'s
/// `data.rs`; V8 itself has no `PromiseResolver::Cast`, because a resolver and its promise are the
/// same `JSPromise` behind two wrappers). So the resolver is parked HERE as a `v8::Global` and the
/// event carries the key — which is the shape `fetch_global::SwFetchBridge.pending` already uses on
/// the page's side of this same transport, for the same reason. The entry is REMOVED when it settles,
/// so a long-lived ServiceWorker does not accumulate one per request.
#[derive(Default)]
struct HandledResolvers {
    map: std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::PromiseResolver>>>,
    next: std::cell::Cell<u32>,
}

// ── ExtendableEvent ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_extendable_event_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !require_new(scope, &args, "ExtendableEvent") || !require_args(scope, &args, "ExtendableEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "ExtendableEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "ExtendableEvent", "ExtendableEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    init_extendable_event(scope, this);
    rv.set(this.into());
}

/// The `ExtendableEvent` init step after `Event`'s: the brand, and only the brand (the interface has
/// no state of its own -- `waitUntil`'s list is written by the engine's dispatch mint, never by the
/// constructor, because a script-constructed one may not call `waitUntil` at all).
///
/// EVERY SUBCLASS CALLS IT. A brand is per-interface (`webidl::brand`) and `waitUntil`'s row checks
/// `ExtendableEvent`'s, so a `FetchEvent` that skipped this step threw `TypeError: Illegal
/// invocation` from its own `waitUntil` -- which is exactly what `service_worker_live`'s event-chain
/// gate caught on its first run, and the reason `PointerEvent`'s constructor runs `init_ui_event` and
/// `init_mouse_event` before its own.
pub(crate) fn init_extendable_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) {
    brand(scope, this, "ExtendableEvent");
}

/// `ExtendableEvent.waitUntil(promise)` — extends the event's lifetime.
///
/// It is NOT a no-op here, which is the behaviour change this file makes beyond the shape: the
/// promise is recorded on the event, and [`take_wait_promises`] hands the recorded set to whoever
/// dispatched the event. The ServiceWorker lifecycle uses that to hold `activate` back until every
/// promise an `install` handler passed has settled — the deleted JS `ev.waitUntil = () => {}` threw
/// them away, so `activate` fired while `install` was still running.
fn cb_wait_until(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    // Chrome's own order: the "script constructed" test comes before the "finished" one, and both
    // name `ExtendableEvent` however derived the receiver is (measured on all three interfaces).
    if !evt_flag_get(scope, this, SLOT_DISPATCHING) {
        throw_dom(
            scope,
            "Failed to execute 'waitUntil' on 'ExtendableEvent': Can not call waitUntil on a script constructed ExtendableEvent.",
            "InvalidStateError",
        );
        return;
    }
    if evt_flag_get(scope, this, SLOT_FINISHED) {
        throw_dom(
            scope,
            "Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is already finished and no extend lifetime promises are outstanding.",
            "InvalidStateError",
        );
        return;
    }
    let Some(list) = evt_slot_read(scope, this, SLOT_WAITS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let n = list.length();
    let p = args.get(0);
    list.set_index(scope, n, p);
}

/// `install_wait_until` — the one-row installer the table names (a body ident has to resolve in the
/// file the macro is in).
pub fn install_wait_until(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, target, member, cb_wait_until);
}

webapi::interface! {
    // `[Exposed=ServiceWorker]`. The macro's exposure vocabulary is window|worker|both and has no
    // `service` word; `worker` is the truthful half of the answer (it is NOT a window interface) and
    // the SERVICE-realm gate is at the install site, which is exactly how
    // `SharedWorkerGlobalScope` / `ServiceWorkerGlobalScope` are already kept apart from a dedicated
    // worker's global. See `install::live::event_family::install_family`.
    "ExtendableEvent" : "Event" [value, worker] {
        ctor   "ExtendableEvent"                                len 1 = stub,
        method "waitUntil" : Undefined len 1 required 1               = install_wait_until
    }
}

// ── FetchEvent ──────────────────────────────────────────────────────────────────────────────────

/// The `FetchEvent(type, FetchEventInit)` steps after `ExtendableEvent`'s. Shared with
/// [`mint_fetch_event`], so the engine's own `fetch` event and a page-constructed one fill the same
/// slots through the same code.
fn init_fetch_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) -> Result<(), ()> {
    let Some(req) = dict_any(scope, dict, "request") else {
        crate::binding::protos::throw_type_error(
            scope,
            "Failed to construct 'FetchEvent': Failed to read the 'request' property from 'FetchEventInit': Required member is undefined.",
        );
        return Err(());
    };
    evt_slot_write(scope, this, "request", req);
    for name in ["clientId", "resultingClientId"] {
        let s = match dict_string(scope, dict, name) {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => return Err(()),
        };
        let v = str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    // Navigation preload is not implemented in this engine, and Chrome's `preloadResponse` with
    // preload disabled resolves with `undefined` — which is what an unfilled resolver-less resolved
    // promise is. `handled` is left PENDING on a script-constructed event, as Chrome's is; the
    // dispatch mint below resolves it.
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    if let Some(p) = resolved(scope, undef) {
        evt_slot_write(scope, this, "preloadResponse", p);
    }
    // `handled` is PENDING on a script-constructed event, as Chrome's is (measured:
    // `[object Promise]`). Its resolver is deliberately dropped here — nothing can settle a synthetic
    // event's `handled`, in Chrome either — and [`mint_fetch_event`] replaces the pair with a parked
    // one for the events the engine dispatches.
    if let Some(r) = v8::PromiseResolver::new(scope) {
        let hp: v8::Local<v8::Value> = r.get_promise(scope).into();
        evt_slot_write(scope, this, "handled", hp);
    }
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    evt_slot_write(scope, this, "isReload", f);
    brand(scope, this, "FetchEvent");
    Ok(())
}

pub(crate) fn cb_fetch_event_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !require_new(scope, &args, "FetchEvent") || !require_args(scope, &args, "FetchEvent", 2) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "FetchEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "FetchEvent", "FetchEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    init_extendable_event(scope, this);
    if init_fetch_event(scope, this, dict).is_err() {
        return;
    }
    rv.set(this.into());
}

/// `FetchEvent.respondWith(response)` — records the response promise the transport will answer with.
///
/// The two `InvalidStateError`s apply to a DISPATCHING event only; on a script-constructed one Chrome
/// accepts the call and accepts it twice (measured), so the guards are behind the dispatch flag.
fn cb_respond_with<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if evt_flag_get(scope, this, SLOT_DISPATCHING) {
        if evt_flag_get(scope, this, SLOT_FINISHED) {
            throw_dom(
                scope,
                "Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished.",
                "InvalidStateError",
            );
            return;
        }
        if evt_flag_get(scope, this, SLOT_RESPONDED) {
            throw_dom(
                scope,
                "Failed to execute 'respondWith' on 'FetchEvent': respondWith() was already called.",
                "InvalidStateError",
            );
            return;
        }
    }
    evt_flag_set(scope, this, SLOT_RESPONDED, true);
    // `Promise.resolve(x)` — the deleted JS said exactly this, and it is what makes a plain
    // `Response` and a promise for one interchangeable.
    if let Some(p) = resolved(scope, args.get(0)) {
        evt_slot_write(scope, this, SLOT_RESPONSE, p);
    }
    // Chrome also prevents the default here (the network is not consulted once a handler responded);
    // this engine's transport reads the response slot instead, so the flag is set for observability
    // rather than for control flow.
    evt_flag_set(scope, this, "defaultPrevented", true);
}

pub fn install_respond_with(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, target, member, cb_respond_with);
}

webapi::interface! {
    // Member ORDER is Chrome 150's own, measured in a real ServiceWorker realm (see the module
    // header). `isReload` is in it and is NOT in webref's IDL — the engine follows what ships.
    "FetchEvent" : "ExtendableEvent" [value, worker] {
        ctor   "FetchEvent"                                     len 2 = stub,
        attr   "request"           : Object    readonly               = evt_get,
        attr   "clientId"          : DomString readonly               = evt_get,
        attr   "resultingClientId" : DomString readonly               = evt_get,
        attr   "isReload"          : Boolean   readonly               = evt_get,
        attr   "preloadResponse"   : Promise   readonly               = evt_get,
        attr   "handled"           : Promise   readonly               = evt_get,
        method "respondWith" : Undefined len 1 required 1             = install_respond_with
    }
}

// ── InstallEvent ────────────────────────────────────────────────────────────────────────────────
//
// Chrome 150 dispatches `install` as an `InstallEvent`, not as a bare `ExtendableEvent` (measured:
// `e.constructor.name` is `InstallEvent` and `Object.prototype.toString.call(e)` is
// `[object InstallEvent]`). `activate` IS a bare `ExtendableEvent`. Firing the wrong one would be a
// one-line tell, so the interface is here — its single member, the static-routing `addRoutes`, is a
// D2 `stub` row because this engine has no static routing: honest and counted (invariant I7), rather
// than a body that pretends.

pub(crate) fn cb_install_event_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !require_new(scope, &args, "InstallEvent") || !require_args(scope, &args, "InstallEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "InstallEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "InstallEvent", "ExtendableEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    init_extendable_event(scope, this);
    brand(scope, this, "InstallEvent");
    rv.set(this.into());
}

webapi::interface! {
    "InstallEvent" : "ExtendableEvent" [value, worker] {
        ctor   "InstallEvent"                          len 1 = stub,
        method "addRoutes" : Promise len 1 required 1        = stub
    }
}

// ── the engine's own mint + dispatch ────────────────────────────────────────────────────────────

/// `Promise.resolve(v)` through the realm's own `Promise`.
fn resolved<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    crate::binding::protos::call_intrinsic(scope, "Promise.resolve", &[v])
}

/// Mint a TRUSTED, dispatchable event of `iface` through the realm's own interface object.
///
/// The interface object is read off the global at CALL time — the same rule every other Rust mint
/// site in this tree follows (`worker_recv::new_message_event`, `events/shared.rs`) — so a realm that
/// does not have the interface simply produces nothing rather than half an event.
fn mint_dispatch_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    ty: &str,
    init: Option<v8::Local<'s, v8::Object>>,
    cancelable: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = global_get(scope, iface).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let tyv = str_val(scope, ty);
    let dict = init.unwrap_or_else(|| v8::Object::new(scope));
    let cv: v8::Local<v8::Value> = v8::Boolean::new(scope, cancelable).into();
    set_prop(scope, dict, "cancelable", cv);
    let ev = ctor.new_instance(scope, &[tyv, dict.into()])?;
    // `isTrusted` — the one own property a dispatched event carries, and it is already installed as
    // an own accessor by `init_event`; this writes the slot it reads.
    let key = trusted_key(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    ev.set_private(scope, key, t);
    evt_flag_set(scope, ev, SLOT_DISPATCHING, true);
    let waits = v8::Array::new(scope, 0);
    evt_slot_write(scope, ev, SLOT_WAITS, waits.into());
    Some(ev)
}

/// A trusted `fetch` `FetchEvent` over `request`. `cancelable: true, bubbles: false` (measured).
pub fn mint_fetch_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: v8::Local<'s, v8::Value>,
    client_id: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "request", request);
    let cid = str_val(scope, client_id);
    set_prop(scope, init, "clientId", cid);
    let ev = mint_dispatch_event(scope, "FetchEvent", "fetch", Some(init), true)?;
    park_handled(scope, ev);
    Some(ev)
}

/// A trusted lifecycle event: `install` is an `InstallEvent`, `activate` an `ExtendableEvent`, both
/// `cancelable: false` (measured).
pub fn mint_lifecycle_event<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str) -> Option<v8::Local<'s, v8::Object>> {
    let iface = if ty == "install" { "InstallEvent" } else { "ExtendableEvent" };
    mint_dispatch_event(scope, iface, ty, None, false)
}

/// Close the event: every later `respondWith`/`waitUntil` is the "already finished"
/// `InvalidStateError`. Called by the dispatcher the moment `dispatchEvent` returns, which is exactly
/// where Chrome closes it (measured: a `setTimeout(0)` after the handler already gets the error).
pub fn finish_event(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    evt_flag_set(scope, ev, SLOT_FINISHED, true);
}

/// The promise `respondWith` recorded, if the handler called it.
pub fn taken_response<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    if !evt_flag_get(scope, ev, SLOT_RESPONDED) {
        return None;
    }
    evt_slot_read(scope, ev, SLOT_RESPONSE)
}

/// The promises `waitUntil` recorded, as a `Promise.all` over them — `None` when there were none.
pub fn take_wait_promises<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let list = evt_slot_read(scope, ev, SLOT_WAITS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
    if list.length() == 0 {
        return None;
    }
    crate::binding::protos::call_intrinsic(scope, "Promise.all", &[list.into()])
}

/// Park a fresh `handled` resolver for a dispatch event and give the event its key.
fn park_handled(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let Some(r) = v8::PromiseResolver::new(scope) else { return };
    let hp: v8::Local<v8::Value> = r.get_promise(scope).into();
    evt_slot_write(scope, ev, "handled", hp);
    let g = v8::Global::new(scope, r);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let key = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<HandledResolvers>().is_none() {
            st.put(HandledResolvers::default());
        }
        let hr = st.borrow::<HandledResolvers>();
        let key = hr.next.get().wrapping_add(1);
        hr.next.set(key);
        hr.map.borrow_mut().insert(key, g);
        key
    };
    let kv: v8::Local<v8::Value> = v8::Number::new(scope, key as f64).into();
    evt_slot_write(scope, ev, SLOT_HANDLED_KEY, kv);
}

/// Settle the `handled` promise: the response (or the fall-through to the network) has been posted.
/// Chrome resolves it with `undefined` once the fetch has been handled.
pub fn resolve_handled(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let Some(key) = evt_slot_read(scope, ev, SLOT_HANDLED_KEY).and_then(|v| v.number_value(scope)) else { return };
    let key = key as u32;
    let g = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(hr) = st.try_borrow::<HandledResolvers>() else { return };
        let taken = hr.map.borrow_mut().remove(&key);
        match taken {
            Some(g) => g,
            None => return,
        }
    };
    let r = v8::Local::new(scope, &g);
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    r.resolve(scope, u);
}

/// `globalThis.dispatchEvent(ev)` through the realm's own `EventTarget` method — the one dispatch
/// algorithm (`core/events/event_target.rs`), never a second one.
pub fn dispatch_on_global<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<'s, v8::Object>) {
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let evv: v8::Local<v8::Value> = ev.into();
    let Some(f) = get_prop(scope, global, "dispatchEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    // The deleted JS wrapped this in `try { … } catch (z) {}`; this is that, in the idiom
    // `worker_recv::dispatch_on_global` already uses. A listener that throws must not take the
    // transport down with it, and an exception left pending in the scope would poison the NEXT call.
    v8::tc_scope!(let tc, scope);
    f.call(tc, gv, &[evv]);
    tc.reset();
}

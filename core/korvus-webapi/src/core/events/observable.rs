//! `core/events/observable.rs` -- `Observable`, `Subscriber` and `EventTarget.when()`.
//!
//! An Observable is either a page's (`new Observable(subscribeCallback)` -- the callback is REQUIRED
//! and callable, `new Observable()` is a TypeError) or a `when()` source (target + type + capture +
//! passive); `subscribe(observer, options)` mints a `Subscriber`, wires the options' `signal` to it,
//! and either runs the page's callback with the subscriber or registers the subscriber as a native
//! listener entry on the target (`event_target::et_add_observer`), which forwards every dispatched
//! event to the subscriber's `next`. All state lives in `v8::Private` slots on the instances: a real
//! Subscriber has no own properties (`Object.keys(subscriber)` is `[]`), and a foreign receiver on
//! any member is a TypeError "Illegal invocation", both as the capture records.
//!
//! `next(any value)`, `error(any error)` and `addTeardown(VoidFunction)` all declare a REQUIRED
//! argument, so a zero-argument call is a TypeError before the receiver is even looked at -- the
//! arity check is `require_args`, the same one `addEventListener` and `when()` use.
//!
//! Semantics (the recorded browser's, behavior-cases/Observable.json + Subscriber.json): `subscribe`
//! returns undefined and ALWAYS runs the subscribe callback, even when the options' signal is already
//! aborted (the subscriber simply starts inactive); a throwing `next` is reported, never propagated,
//! and so is an `error(e)` value that has no `error` handler to go to or arrives after the close;
//! `error`/`complete` run once and CLOSE THE SUBSCRIPTION FIRST (`close_subscription`: inactive, then
//! the signal aborts carrying `error`'s value as its reason, then the teardowns in LIFO order), so the
//! observer's own callback runs against a closed subscription; a teardown added to an inactive
//! subscriber runs at once; the options' signal aborting tears the subscription down and hands on its
//! own reason. `subscribe()` NEVER throws synchronously: a `start` callback that throws hands the
//! exception to the subscription (`error`, so the observer's handler or the global gets it), or to
//! the global outright if the callback had already closed the subscription, and a throwing observer
//! callback is reported like a throwing `next`.
//!
//! The operators (`map`, `filter`, `take`, ...) and the static `from` are `observable_ops.rs`, built on
//! the internal API at the end of this file.

use deno_core::v8;

use crate::binding::protos::{build_fn, call_method, get_prop, global_get, require_args, str_val, throw_type_error};
use crate::core::events::event_target::{et_add_observer, et_add_subscriber_abort, et_remove_observer};
use crate::core::events::report::report_exception;
use crate::install::kits::define_value;
use crate::install::MemberDef;
use crate::install::live::getter_row;
pub use super::observable_ops::install_observable_ops;

// ── private slots ──────────────────────────────────────────────────────────────────────────────

fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = slot(scope, name);
    o.get_private(scope, k).filter(|v| !v.is_undefined())
}
fn slot_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = slot(scope, name);
    o.set_private(scope, k, v);
}
fn slot_set_bool(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, b: bool) {
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
    slot_set(scope, o, name, v);
}
fn slot_bool(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> bool {
    slot_get(scope, o, name).map(|v| v.is_true()).unwrap_or(false)
}

const OBS_BRAND: &str = "korvus_obs_brand";
const OBS_START: &str = "korvus_obs_start";
const OBS_SRC_TARGET: &str = "korvus_obs_src_target";
const OBS_SRC_TYPE: &str = "korvus_obs_src_type";
const OBS_SRC_CAPTURE: &str = "korvus_obs_src_capture";
const OBS_SRC_PASSIVE: &str = "korvus_obs_src_passive";

const SUB_BRAND: &str = "korvus_sub_brand";
const SUB_DONE: &str = "korvus_sub_done";
const SUB_NEXT: &str = "korvus_sub_next";
const SUB_ERROR: &str = "korvus_sub_error";
const SUB_COMPLETE: &str = "korvus_sub_complete";
const SUB_TEARDOWNS: &str = "korvus_sub_teardowns";
const SUB_CTRL: &str = "korvus_sub_ctrl";
/// The `subscribe(_, {signal})` signal, kept so the close can take ITS reason (see [`subscriber_close`]).
const SUB_OUTER_SIGNAL: &str = "korvus_sub_outer_signal";
const SUB_SRC_TARGET: &str = "korvus_sub_src_target";
const SUB_SRC_TYPE: &str = "korvus_sub_src_type";
const SUB_SRC_CAPTURE: &str = "korvus_sub_src_capture";

/// The receiver must be a branded Observable / Subscriber: a foreign `this` is the WebIDL TypeError.
fn require_brand(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, brand: &str) -> bool {
    if slot_bool(scope, this, brand) { return true; }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, ctor_name: &str) -> Option<v8::Local<'s, v8::Object>> {
    let c = global_get(scope, ctor_name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

// ── Observable ─────────────────────────────────────────────────────────────────────────────────

/// `new Observable(subscribeCallback)`: newable only, the callback required and callable.
pub(crate) fn cb_observable_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !args.is_construct_call() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("Observable"));
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'Observable': 1 argument required, but only 0 present.");
        return;
    }
    let cb = args.get(0);
    if !cb.is_function() {
        throw_type_error(scope, "Failed to construct 'Observable': parameter 1 is not of type 'Function'.");
        return;
    }
    let this = args.this();
    slot_set_bool(scope, this, OBS_BRAND, true);
    slot_set(scope, this, OBS_START, cb);
}

/// `EventTarget.prototype.when(type, options)`: an Observable over this target's `type` events.
pub(crate) fn cb_et_when(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "EventTarget", "when") { return; }
    let Some(proto) = proto_of(scope, "Observable") else {
        throw_type_error(scope, "Observable is not implemented.");
        return;
    };
    // The arguments as WebIDL converts them: `type` to a string, then
    // `ObservableEventListenerOptions` -- `capture`, `passive`, each read once and converted as it is read.
    let t = args.get(0);
    if t.is_symbol() {
        throw_type_error(scope, "Failed to execute 'when' on 'EventTarget': Cannot convert a Symbol value to a string");
        return;
    }
    let Some(t) = t.to_string(scope) else { return };
    let ty = t.to_rust_string_lossy(scope);
    let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "ObservableEventListenerOptions", "Failed to execute 'when' on 'EventTarget': ") else { return };
    let capture = get_prop(scope, o, "capture").is_some_and(|v| v.is_true());
    let passive = get_prop(scope, o, "passive").is_some_and(|v| v.is_true());
    let obs = crate::binding::protos::mint_on(scope, proto.into());
    slot_set_bool(scope, obs, OBS_BRAND, true);
    let target: v8::Local<v8::Value> = args.this().into();
    slot_set(scope, obs, OBS_SRC_TARGET, target);
    let tyv = str_val(scope, &ty);
    slot_set(scope, obs, OBS_SRC_TYPE, tyv);
    slot_set_bool(scope, obs, OBS_SRC_CAPTURE, capture);
    slot_set_bool(scope, obs, OBS_SRC_PASSIVE, passive);
    rv.set(obs.into());
}

/// `subscribe(observer, options)`.
fn cb_obs_subscribe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let obs = args.this();
    if !require_brand(scope, obs, OBS_BRAND) { return; }
    let signal = v8::Local::<v8::Object>::try_from(args.get(1)).ok().and_then(|o| get_prop(scope, o, "signal")).filter(|s| s.is_object());
    subscribe_impl(scope, obs, args.get(0), signal);
}

/// Subscribe `observer` to `obs` with `signal` -- what the operators do upstream (`observable_ops`).
pub(crate) fn subscribe_with(scope: &mut v8::PinScope, obs: v8::Local<v8::Object>, observer: v8::Local<v8::Value>, signal: Option<v8::Local<v8::Object>>) {
    subscribe_impl(scope, obs, observer, signal.map(|s| s.into()));
}

fn subscribe_impl(scope: &mut v8::PinScope, obs: v8::Local<v8::Object>, observer: v8::Local<v8::Value>, signal: Option<v8::Local<v8::Value>>) {
    let Some(sproto) = proto_of(scope, "Subscriber") else { return };
    // The observer: a function is `next`; an object supplies next/error/complete.
    let sub = crate::binding::protos::mint_on(scope, sproto.into());
    slot_set_bool(scope, sub, SUB_BRAND, true);
    slot_set_bool(scope, sub, SUB_DONE, false);
    if observer.is_function() {
        slot_set(scope, sub, SUB_NEXT, observer);
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(observer) {
        for (k, s) in [("next", SUB_NEXT), ("error", SUB_ERROR), ("complete", SUB_COMPLETE)] {
            if let Some(f) = get_prop(scope, o, k).filter(|v| v.is_function()) { slot_set(scope, sub, s, f); }
        }
    }
    let teardowns = v8::Array::new(scope, 0);
    slot_set(scope, sub, SUB_TEARDOWNS, teardowns.into());
    // The subscriber's own signal: aborted when the subscription closes.
    if let Some(ac) = crate::binding::protos::iface_object(scope, "AbortController").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        if let Some(ctrl) = ac.new_instance(scope, &[]) { slot_set(scope, sub, SUB_CTRL, ctrl.into()); }
    }
    // A `when()` source is remembered on the subscriber for its teardown.
    let src_target = slot_get(scope, obs, OBS_SRC_TARGET);
    if let Some(t) = src_target {
        slot_set(scope, sub, SUB_SRC_TARGET, t);
        if let Some(ty) = slot_get(scope, obs, OBS_SRC_TYPE) { slot_set(scope, sub, SUB_SRC_TYPE, ty); }
        let cap = slot_bool(scope, obs, OBS_SRC_CAPTURE);
        slot_set_bool(scope, sub, SUB_SRC_CAPTURE, cap);
    }
    // options.signal: already aborted -> the subscription starts closed; else it closes on abort.
    if let Some(sig) = signal {
        slot_set(scope, sub, SUB_OUTER_SIGNAL, sig);
        let aborted = v8::Local::<v8::Object>::try_from(sig).ok().and_then(|o| get_prop(scope, o, "aborted")).map(|v| v.boolean_value(scope)).unwrap_or(false);
        if aborted { subscriber_close(scope, sub); } else { et_add_subscriber_abort(scope, sig, sub); }
    }
    match src_target {
        Some(target) => {
            if !slot_bool(scope, sub, SUB_DONE) {
                let ty = slot_get(scope, obs, OBS_SRC_TYPE).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                let cap = slot_bool(scope, obs, OBS_SRC_CAPTURE);
                let pas = slot_bool(scope, obs, OBS_SRC_PASSIVE);
                et_add_observer(scope, target, &ty, sub, cap, pas);
            }
        }
        None => {
            // The page's subscribe callback runs even for a subscriber that is already inactive
            // (the recorded browser counts the call). A throw does NOT escape `subscribe()`: the
            // subscription gets first refusal on it, exactly as if the callback had called
            // `subscriber.error(e)` itself -- so the observer's `error` handler sees it, or, with no
            // handler, the global does. A callback that had ALREADY closed the subscription has no
            // subscriber left to give it to, and the exception goes straight to the global.
            let Some(start) = slot_get(scope, obs, OBS_START).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            let subv: v8::Local<v8::Value> = sub.into();
            let exc = {
                v8::tc_scope!(let tc, scope);
                start.call(tc, undef, &[subv]);
                let e = if tc.has_caught() { tc.exception().map(|e| v8::Global::new(tc, e)) } else { None };
                tc.reset();
                e
            };
            if let Some(e) = exc {
                let e = v8::Local::new(scope, &e);
                if slot_bool(scope, sub, SUB_DONE) { report_exception(scope, e); }
                else { finish(scope, sub, SUB_ERROR, Some(e)); }
            }
        }
    }
}

// ── Subscriber ─────────────────────────────────────────────────────────────────────────────────

/// "Close a subscription" (<https://wicg.github.io/observable/#close-a-subscription>) -- ONE algorithm
/// with three steps in this order, and every caller runs all three:
///
/// 1. the subscription becomes inactive (`active` is false from here on);
/// 2. the subscriber's own signal is aborted -- WITH `reason` when the closer has one (`error(e)`
///    stores `e`, an aborting options' signal passes its own reason on), so the `abort` event fires
///    and upstream subscriptions unwind BEFORE any teardown of this one runs;
/// 3. the teardowns run in **LIFO** order and are cleared. A teardown added from here on finds an
///    inactive subscriber and runs at once (`cb_sub_add_teardown`), which is why the list is emptied
///    before the first one is called.
///
/// It used to run 3 then 2, forward, with no reason, and only after the observer's callback had
/// already been invoked -- so `complete()`/`error()` handlers saw `active === true`, teardowns ran
/// FIFO, and `subscriber.signal.reason` was always the argument-less `AbortError`.
fn close_subscription(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, reason: Option<v8::Local<v8::Value>>) {
    if slot_bool(scope, sub, SUB_DONE) { return; }
    slot_set_bool(scope, sub, SUB_DONE, true);
    // The upstream subscription of a `when()` source: its native listener entry on the target.
    if let Some(target) = slot_get(scope, sub, SUB_SRC_TARGET) {
        let ty = slot_get(scope, sub, SUB_SRC_TYPE).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let cap = slot_bool(scope, sub, SUB_SRC_CAPTURE);
        et_remove_observer(scope, target, &ty, sub, cap);
    }
    if let Some(ctrl) = slot_get(scope, sub, SUB_CTRL) {
        v8::tc_scope!(let tc, scope);
        match reason {
            Some(r) => call_method(tc, ctrl, "abort", &[r]),
            None => call_method(tc, ctrl, "abort", &[]),
        };
        tc.reset();
    }
    if let Some(arr) = slot_get(scope, sub, SUB_TEARDOWNS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let n = arr.length();
        let mut fns = Vec::with_capacity(n as usize);
        for i in (0..n).rev() {
            if let Some(f) = arr.get_index(scope, i).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { fns.push(v8::Global::new(scope, f)); }
        }
        let empty = v8::Array::new(scope, 0);
        slot_set(scope, sub, SUB_TEARDOWNS, empty.into());
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        for g in fns {
            let f = v8::Local::new(scope, &g);
            v8::tc_scope!(let tc, scope);
            f.call(tc, undef, &[]);
            tc.reset();
        }
    }
}
/// Close the subscription because the options' signal aborted: the subscriber's own signal takes THAT
/// signal's reason, so `subscribe(_, {signal: AbortSignal.abort('x')})` leaves `subscriber.signal.reason`
/// as `'x'` and a teardown can read it.
pub(crate) fn subscriber_close(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>) {
    let reason = slot_get(scope, sub, SUB_OUTER_SIGNAL)
        .and_then(|s| v8::Local::<v8::Object>::try_from(s).ok())
        .and_then(|s| get_prop(scope, s, "reason"))
        .filter(|r| !r.is_undefined());
    close_subscription(scope, sub, reason);
}
/// `subscriber.next(value)`: the observer's `next`, a throw reported.
pub(crate) fn subscriber_next(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, value: v8::Local<v8::Value>) {
    if slot_bool(scope, sub, SUB_DONE) { return; }
    let Some(next) = slot_get(scope, sub, SUB_NEXT).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let exc = {
        v8::tc_scope!(let tc, scope);
        next.call(tc, undef, &[value]);
        let e = if tc.has_caught() { tc.exception().map(|e| v8::Global::new(tc, e)) } else { None };
        tc.reset();
        e
    };
    if let Some(e) = exc {
        let e = v8::Local::new(scope, &e);
        report_exception(scope, e);
    }
}
/// `error(e)` / `complete()`: once; the subscription CLOSES first (`close_subscription`, so the
/// observer's callback already sees `active === false` and an aborted signal), and only then is the
/// observer's callback invoked. A throwing callback is reported at the global, never propagated.
///
/// AN ERROR VALUE IS NEVER DROPPED. `error(e)` has exactly three destinations and every one of them
/// is here: the observer's `error` handler if it has one; otherwise the global, which is the spec's
/// "default error algorithm" (<https://wicg.github.io/observable/#default-error-algorithm>); and if
/// the subscription has ALREADY closed, the global again, because there is no longer an observer to
/// hand it to. korvus had only the first: a subscription with no `error` handler swallowed the value,
/// and a push after the close returned at the `SUB_DONE` guard without a word.
fn finish(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, which: &str, arg: Option<v8::Local<v8::Value>>) {
    let is_error = which == SUB_ERROR;
    if slot_bool(scope, sub, SUB_DONE) {
        // A second `complete()` is simply nothing; a second `error(e)` still owes `e` to the global.
        if let (true, Some(e)) = (is_error, arg) { report_exception(scope, e); }
        return;
    }
    // A detached frame's subscriber neither delivers nor reports (the spec's step 2 of both, after the
    // inactive case above): "reporting the exception" needs an attached document.
    if !crate::core::html::browsing_context::object_realm_fully_active(scope, sub) { return; }
    let f = slot_get(scope, sub, which).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    // `error(e)` stores `e` as the subscriber's signal reason; `complete()` aborts without one.
    close_subscription(scope, sub, if is_error { arg } else { None });
    let exc = match f {
        Some(f) => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            let args: Vec<v8::Local<v8::Value>> = arg.into_iter().collect();
            v8::tc_scope!(let tc, scope);
            f.call(tc, undef, &args);
            let e = if tc.has_caught() { tc.exception().map(|e| v8::Global::new(tc, e)) } else { None };
            tc.reset();
            e
        }
        None => {
            // No handler: the "default error algorithm" reports `e` at the global.
            if let (true, Some(e)) = (is_error, arg) { report_exception(scope, e); }
            None
        }
    };
    if let Some(e) = exc {
        // A throwing observer callback is REPORTED, never propagated -- the same rule `next` already
        // followed, and the reason `subscribe()` cannot throw synchronously however the page nests
        // these calls (Blink: every one of the three is `InvokeAndReportException`).
        let e = v8::Local::new(scope, &e);
        report_exception(scope, e);
    }
}
fn cb_sub_next(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Subscriber", "next") { return; }
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    // "If this's relevant global object is a Window object, and its associated Document is not fully
    // active, then return" -- a detached frame's subscriber delivers nothing.
    if !crate::core::html::browsing_context::object_realm_fully_active(scope, sub) { return; }
    let v = args.get(0);
    subscriber_next(scope, sub, v);
}
fn cb_sub_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Subscriber", "error") { return; }
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    let e = args.get(0);
    finish(scope, sub, SUB_ERROR, Some(e));
}
fn cb_sub_complete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    finish(scope, sub, SUB_COMPLETE, None);
}
/// `addTeardown(fn)`: queued for the close; run at once on an inactive subscriber.
fn cb_sub_add_teardown(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Subscriber", "addTeardown") { return; }
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) else { return };
    if slot_bool(scope, sub, SUB_DONE) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        v8::tc_scope!(let tc, scope);
        f.call(tc, undef, &[]);
        tc.reset();
        return;
    }
    if let Some(arr) = slot_get(scope, sub, SUB_TEARDOWNS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let n = arr.length();
        arr.set_index(scope, n, f.into());
    }
}
fn cb_sub_active_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    let done = slot_bool(scope, sub, SUB_DONE);
    rv.set_bool(!done);
}
fn cb_sub_signal_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let sub = args.this();
    if !require_brand(scope, sub, SUB_BRAND) { return; }
    let sig = slot_get(scope, sub, SUB_CTRL)
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| get_prop(scope, c, "signal"));
    match sig { Some(s) => rv.set(s), None => rv.set_undefined() }
}

// ── the internal API the operators use (`observable_ops`) ──────────────────────────────────────

/// An Observable whose subscribe callback is `start` (a native function the operators build).
pub(crate) fn new_native_observable<'s>(scope: &mut v8::PinScope<'s, '_>, start: v8::Local<v8::Function>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = proto_of(scope, "Observable")?;
    let obs = crate::binding::protos::mint_on(scope, proto.into());
    slot_set_bool(scope, obs, OBS_BRAND, true);
    slot_set(scope, obs, OBS_START, start.into());
    Some(obs)
}
pub(crate) fn is_observable(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> bool {
    slot_bool(scope, o, OBS_BRAND)
}
pub(crate) fn sub_next(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, v: v8::Local<v8::Value>) {
    if crate::core::html::browsing_context::object_realm_fully_active(scope, sub) {
        subscriber_next(scope, sub, v);
    }
}
pub(crate) fn sub_error(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, e: v8::Local<v8::Value>) {
    finish(scope, sub, SUB_ERROR, Some(e));
}
pub(crate) fn sub_complete(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>) {
    finish(scope, sub, SUB_COMPLETE, None);
}
pub(crate) fn sub_active(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>) -> bool {
    !slot_bool(scope, sub, SUB_DONE)
}
/// The subscriber's own signal (aborted when the subscription closes).
pub(crate) fn sub_signal<'s>(scope: &mut v8::PinScope<'s, '_>, sub: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    slot_get(scope, sub, SUB_CTRL)
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| get_prop(scope, c, "signal"))
        .and_then(|s| v8::Local::<v8::Object>::try_from(s).ok())
}
pub(crate) fn add_teardown(scope: &mut v8::PinScope, sub: v8::Local<v8::Object>, f: v8::Local<v8::Function>) {
    if slot_bool(scope, sub, SUB_DONE) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        v8::tc_scope!(let tc, scope);
        f.call(tc, undef, &[]);
        tc.reset();
        return;
    }
    if let Some(arr) = slot_get(scope, sub, SUB_TEARDOWNS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let n = arr.length();
        arr.set_index(scope, n, f.into());
    }
}

// ── the tables ─────────────────────────────────────────────────────────────────────────────────

fn method_row<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let f = build_fn(scope, cb, member.name, member.length as i32);
    define_value(scope, proto, member, f.into(), true);
}
/// The table body of `Observable.prototype.subscribe`.
pub fn install_observable(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "subscribe" => method_row(scope, proto, member, cb_obs_subscribe),
        other => debug_assert!(false, "install_observable: unexpected member {other}"),
    }
}
/// The table body of every `Subscriber` row.
pub fn install_subscriber(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "active" => getter_row(scope, proto, member, cb_sub_active_get),
        "signal" => getter_row(scope, proto, member, cb_sub_signal_get),
        "addTeardown" => method_row(scope, proto, member, cb_sub_add_teardown),
        "complete" => method_row(scope, proto, member, cb_sub_complete),
        "error" => method_row(scope, proto, member, cb_sub_error),
        "next" => method_row(scope, proto, member, cb_sub_next),
        other => debug_assert!(false, "install_subscriber: unexpected member {other}"),
    }
}

// The rows are the capture's, in its order. The constructor is real (`cb_observable_ctor`, built by
// `events::install_event_model`); the `ctor` row carries its length. The operators and `from` are
// `observable_ops`'.
webapi::interface! {
    "Observable" [value, both] {
        ctor          "Observable"                     len 1 = stub,
        method        "catch"     : Object    len 1 = install_observable_ops,
        method        "drop"      : Object    len 1 = install_observable_ops,
        method        "every"     : Promise   len 1 = install_observable_ops,
        method        "filter"    : Object    len 1 = install_observable_ops,
        method        "finally"   : Object    len 1 = install_observable_ops,
        method        "find"      : Promise   len 1 = install_observable_ops,
        method        "first"     : Promise   len 0 = install_observable_ops,
        method        "flatMap"   : Object    len 1 = install_observable_ops,
        method        "forEach"   : Promise   len 1 = install_observable_ops,
        method        "inspect"   : Object    len 0 = install_observable_ops,
        method        "last"      : Promise   len 0 = install_observable_ops,
        method        "map"       : Object    len 1 = install_observable_ops,
        method        "reduce"    : Promise   len 1 = install_observable_ops,
        method        "some"      : Promise   len 1 = install_observable_ops,
        method        "subscribe" : Undefined len 0 = install_observable,
        method        "switchMap" : Object    len 1 = install_observable_ops,
        method        "take"      : Object    len 1 = install_observable_ops,
        method        "takeUntil" : Object    len 1 = install_observable_ops,
        method        "toArray"   : Promise   len 0 = install_observable_ops,
        static_method "from"      : Object    len 1 = install_observable_ops
    }
}

webapi::interface! {
    "Subscriber" [interface, both] {
        ctor   "Subscriber"                      len 0 = stub,
        attr   "active"      : Boolean readonly        = install_subscriber,
        attr   "signal"      : Object  readonly        = install_subscriber,
        method "addTeardown" : Undefined      len 1    = install_subscriber,
        method "complete"    : Undefined      len 0    = install_subscriber,
        method "error"       : Undefined      len 1    = install_subscriber,
        method "next"        : Undefined      len 1    = install_subscriber
    }
}

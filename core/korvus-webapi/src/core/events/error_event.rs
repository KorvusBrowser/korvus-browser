//! `core/events/error_event.rs` — `ErrorEvent`, `ProgressEvent` and `PromiseRejectionEvent`
//! (the third added by W4.f, which is the unit that made the engine FIRE one).
//!
//! Two `Event` subclasses whose whole surface is a handful of dictionary scalars, and the two that
//! existed TWICE before this unit: `host.js` had a class for the window realm and `worker-host.js`
//! had a hand copy of each for the worker, each with its own `Object.defineProperty` loop building
//! the same accessors. One table with `Exposed=(Window,Worker)` is both.
//!
//! `ErrorEvent.error` is `any`, and an ABSENT member is not the same as an explicit null: measured on
//! `google-chrome --headless=new`, `new ErrorEvent("e").error` is `undefined` while
//! `new ErrorEvent("e", {error: null}).error` is `null`. `host.js` wrote null for both. The slot is
//! simply left unwritten when the member is absent, and the (non-nullable) row reads it back as
//! `undefined` — which is the distinction a page sees.
//!
//! `ProgressEvent.loaded`/`total` are `double`, NOT `unsigned long long`: WPT's
//! progressevent-constructor asserts `{loaded: 1.5, total: -5.5}` round-trips unchanged, which an
//! integer coercion would destroy. `lengthComputable` is a plain boolean coercion ("hah" is true) and
//! the init members are matched case-SENSITIVELY (`lengthcomputable` is not `lengthComputable`).

use deno_core::v8;

use crate::core::events::ctor::{
    arg_string, brand, dict_any, dict_bool, dict_double, dict_int, dict_string, init_dict, init_event, require_args,
    require_new,
};
use crate::core::events::event::{evt_slot_write, evt_slot_write_num};

pub(crate) fn cb_error_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "ErrorEvent") || !require_args(scope, &args, "ErrorEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "ErrorEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "ErrorEvent", "ErrorEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    for name in ["message", "filename"] {
        let s = match dict_string(scope, dict, name) { Ok(v) => v.unwrap_or_default(), Err(_) => return };
        let v = crate::binding::protos::str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    for name in ["lineno", "colno"] {
        let n = match dict_int(scope, dict, name, 32, false) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
        evt_slot_write_num(scope, this, name, n);
    }
    // `any error`: written only when the member is PRESENT, so an absent one reads back undefined.
    if let Some(v) = dict_any(scope, dict, "error") {
        evt_slot_write(scope, this, "error", v);
    }
    brand(scope, this, "ErrorEvent");
    rv.set(this.into());
}

pub(crate) fn cb_progress_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "ProgressEvent") || !require_args(scope, &args, "ProgressEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "ProgressEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "ProgressEvent", "ProgressEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    let computable = dict_bool(scope, dict, "lengthComputable");
    let cv: v8::Local<v8::Value> = v8::Boolean::new(scope, computable).into();
    evt_slot_write(scope, this, "lengthComputable", cv);
    for name in ["loaded", "total"] {
        let n = match dict_double(scope, dict, name, "ProgressEvent") { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
        evt_slot_write_num(scope, this, name, n);
    }
    brand(scope, this, "ProgressEvent");
    rv.set(this.into());
}

webapi::interface! {
    "ErrorEvent" : "Event" [value, both] {
        ctor "ErrorEvent"                 len 1 = stub,
        attr "message"  : Any readonly          = evt_get,
        attr "filename" : Any readonly          = evt_get,
        attr "lineno"   : Any readonly          = evt_get,
        attr "colno"    : Any readonly          = evt_get,
        attr "error"    : Any readonly          = evt_get
    }
}

webapi::interface! {
    "ProgressEvent" : "Event" [value, both] {
        ctor "ProgressEvent"                      len 1 = stub,
        attr "lengthComputable" : Any readonly          = evt_get,
        attr "loaded"           : Any readonly          = evt_get,
        attr "total"            : Any readonly          = evt_get
    }
}

// ── PromiseRejectionEvent ───────────────────────────────────────────────────────────────────────
//
// `promise` AND `reason` WERE ONCE BOTH `undefined`, on every `unhandledrejection` this
// engine has ever fired. The interface was a registry interface with two `stub` rows, and the
// kernel's `BOOTSTRAP` string constructed it with an init dictionary the shell constructor ignored,
// so a page's `event.reason` -- the entire point of the event -- read `undefined` where Chrome gives
// the rejection reason. Measured on this tree before the change:
//
// ```text
// new PromiseRejectionEvent("unhandledrejection", {promise: p, reason: 1}).reason   ->  undefined
// ```
//
// Two rows and a real constructor fix it, and the constructor is also what the native reporting path
// ([`crate::core::events::report::report_rejection`]) mints through -- one definition, one shape,
// whether the event comes from a page or from the event loop.

/// `PromiseRejectionEvent(type, PromiseRejectionEventInit init)` — the init is REQUIRED (WebIDL
/// `length` 2, which the capture records) because `promise` is a required member of it.
pub(crate) fn cb_promise_rejection_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "PromiseRejectionEvent") || !require_args(scope, &args, "PromiseRejectionEvent", 2) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "PromiseRejectionEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "PromiseRejectionEvent", "PromiseRejectionEventInit") else { return };
    // `promise` is a REQUIRED member, and Chrome says so before it says anything else. Measured on
    // `google-chrome --headless=new` 141, 2026-09-12:
    //   new PromiseRejectionEvent("x", {})  ->  TypeError: Failed to construct
    //   'PromiseRejectionEvent': Failed to read the 'promise' property from
    //   'PromiseRejectionEventInit': Required member is undefined.
    if dict_any(scope, dict, "promise").is_none() {
        crate::binding::protos::throw_type_error(
            scope,
            "Failed to construct 'PromiseRejectionEvent': Failed to read the 'promise' property from 'PromiseRejectionEventInit': Required member is undefined.",
        );
        return;
    }
    let this = args.this();
    init_event(scope, this, &ty, dict);
    init_promise_rejection_event(scope, this, dict);
    rv.set(this.into());
}

/// The `PromiseRejectionEvent` steps after `Event`'s, shared with the native mint site.
///
/// `promise` is a required `Promise` member (the CONSTRUCTOR rejects an init without it; the native
/// mint site always supplies one) and `reason` is `any` with an implicit `undefined` default, so an
/// absent `reason` leaves the slot unwritten and reads back `undefined` — the same present/absent
/// distinction `ErrorEvent.error` keeps.
pub(crate) fn init_promise_rejection_event(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    dict: Option<v8::Local<v8::Object>>,
) {
    match dict_any(scope, dict, "promise") {
        Some(v) => evt_slot_write(scope, this, "promise", v),
        None => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            evt_slot_write(scope, this, "promise", u);
        }
    }
    if let Some(v) = dict_any(scope, dict, "reason") {
        evt_slot_write(scope, this, "reason", v);
    }
    brand(scope, this, "PromiseRejectionEvent");
}

webapi::interface! {
    // Rows VERBATIM from the `PromiseRejectionEvent` block of `install/generated_registry.rs`.
    // `Exposed=(Window,Worker)`: the worker capture lists it with the same two members, and the
    // worker registry's shell copy leaves with this table.
    "PromiseRejectionEvent" : "Event" [value, both] {
        ctor "PromiseRejectionEvent"       len 2 = stub,
        attr "promise" : Any readonly            = evt_get,
        attr "reason"  : Any readonly            = evt_get
    }
}

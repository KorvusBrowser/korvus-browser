//! `core/events/navigation_events.rs` — `PopStateEvent` and `HashChangeEvent`.
//!
//! The two events a same-document navigation fires. Both were generated registry SHELLS whose
//! constructor ignored its dictionary and whose members were stubs, so `new HashChangeEvent("h",
//! {oldURL: "a"}).oldURL` read `""` and `new PopStateEvent("p", {state: 5}).state` read `null` -- and
//! the engine had no way to fire either with a real `oldURL`/`newURL`/`state`. Measured on stock Chrome
//! 150.0.7871.46, Brave 151, Edge 151 and Opera 135 (`tests/fixtures/p11a/*.fragment.txt`):
//!
//! ```text
//! new PopStateEvent("p").state / .hasUAVisualTransition     null / false
//! new PopStateEvent("p", {state: undefined}).state          null
//! new HashChangeEvent("h", {oldURL: 1, newURL: "x"})        "1", "x"   (USVString conversion)
//! new HashChangeEvent("h", 5)                                TypeError ... 'HashChangeEventInit'.
//! ```
//!
//! Both are created by the registry walk at their capture positions (`install::live::ORDERED_HAND`),
//! where the generated rows used to be, so the own-global order does not move.

use deno_core::v8;

use crate::core::events::ctor::{arg_string, brand, dict_any, dict_string, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null};
use crate::install::installer::InstallLedger;

pub(crate) fn cb_pop_state_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "PopStateEvent") || !require_args(scope, &args, "PopStateEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "PopStateEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "PopStateEvent", "PopStateEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    // `any state = null`: an absent member and an explicit `undefined` both read back null (measured).
    match dict_any(scope, dict, "state") {
        Some(v) if !v.is_undefined() => evt_slot_write(scope, this, "state", v),
        _ => evt_slot_write_null(scope, this, "state"),
    }
    // The dictionary's member, default false (Chrome 150 reflects it).
    let h = crate::core::events::ctor::dict_bool(scope, dict, "hasUAVisualTransition");
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, h).into();
    evt_slot_write(scope, this, "hasUAVisualTransition", f);
    brand(scope, this, "PopStateEvent");
    rv.set(this.into());
}

pub(crate) fn cb_hash_change_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "HashChangeEvent") || !require_args(scope, &args, "HashChangeEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "HashChangeEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "HashChangeEvent", "HashChangeEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    for name in ["oldURL", "newURL"] {
        let s = match dict_string(scope, dict, name) {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => return,
        };
        let v = crate::binding::protos::str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    brand(scope, this, "HashChangeEvent");
    rv.set(this.into());
}

/// `PopStateEvent`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn create_pop_state_event<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut InstallLedger) {
    crate::install::live::rows::build_newable(scope, global, &POPSTATEEVENT, cb_pop_state_event_ctor, Some("Event"), None, ledger);
}

/// `HashChangeEvent`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn create_hash_change_event<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut InstallLedger) {
    crate::install::live::rows::build_newable(scope, global, &HASHCHANGEEVENT, cb_hash_change_event_ctor, Some("Event"), None, ledger);
}

/// Construct a TRUSTED `iface` event of type `ty` from `init` and dispatch it at the window -- how the
/// engine fires `popstate` and `hashchange` (neither bubbles nor is cancelable, the dictionary
/// defaults, measured).
pub(crate) fn fire_window_event(scope: &mut v8::PinScope, iface: &str, ty: &str, init: v8::Local<v8::Object>) {
    let t = crate::binding::protos::str_val(scope, ty);
    let Some(ev) = crate::binding::protos::new_global(scope, iface, &[t, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    crate::binding::protos::engine_call(scope, gv, "dispatchEvent", &[ev.into()]);
}

webapi::interface! {
    "PopStateEvent" : "Event" [value, window] {
        ctor "PopStateEvent"                          len 1 = stub,
        attr "state"                 : Any readonly nullable = evt_get_null,
        attr "hasUAVisualTransition" : Any readonly          = evt_get
    }
}

webapi::interface! {
    "HashChangeEvent" : "Event" [value, window] {
        ctor "HashChangeEvent"            len 1 = stub,
        attr "oldURL" : Any readonly            = evt_get,
        attr "newURL" : Any readonly            = evt_get
    }
}

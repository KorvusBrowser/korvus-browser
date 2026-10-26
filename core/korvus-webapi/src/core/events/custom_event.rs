//! `core/events/custom_event.rs` — `CustomEvent`.
//!
//! Was `host.js`'s `class CustomEvent extends Event` plus a `KB.__evtSetDetail` bridge slot, a JS
//! `Object.defineProperty` of `detail`, and `dom/08g-observers.js`'s `initCustomEvent` mount — four
//! places for one interface. It is one table with a native constructor now, exposed in both realms by
//! `Exposed`, so the worker's shell copy dies with it.
//!
//! `detail` is Blink's quirk, verbatim (`custom_event.cc`): a missing member, an explicit
//! `undefined` and an explicit `null` all leave the event's detail NULL — never `undefined`. Measured
//! on `google-chrome --headless=new`: `new CustomEvent("c").detail`, `{detail: undefined}` and
//! `{detail: null}` are all `null`, while `{detail: false}` is `false`.

use deno_core::v8;

use crate::core::events::ctor::{brand, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null};
use crate::install::MemberDef;

/// The `CustomEvent(type, customEventInitDict)` steps after `Event`'s.
pub(crate) fn init_custom_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) {
    match crate::core::events::ctor::dict_any(scope, dict, "detail") {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "detail", v),
        _ => evt_slot_write_null(scope, this, "detail"),
    }
    brand(scope, this, "CustomEvent");
}

pub(crate) fn cb_custom_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "CustomEvent") || !require_args(scope, &args, "CustomEvent", 1) {
        return;
    }
    let Some(ty) = crate::core::events::ctor::arg_string(scope, &args, 0, "CustomEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "CustomEvent", "CustomEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    init_custom_event(scope, this, dict);
    rv.set(this.into());
}

/// `initCustomEvent` is the legacy in-place initialiser; its body is `event.rs`'s, which the table
/// reaches through this one-row installer (the row's receiver check comes from `evt_branded_method`).
pub fn install_custom_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::events::event::install_custom_event_row(scope, proto, member);
}

webapi::interface! {
    "CustomEvent" : "Event" [value, both] {
        ctor   "CustomEvent"                             len 1 = stub,
        attr   "detail"          : Any readonly nullable       = evt_get_null,
        method "initCustomEvent" : Any len 1 required 1        = install_custom_event
    }
}

//! `core/events/legacy_events.rs` — `BeforeUnloadEvent` and `CookieChangeEvent`.
//!
//! The two window-only `Event` subclasses that are left once the input plane and the messaging plane
//! have their own files. They share this one because each is two members or fewer and neither has a
//! family of its own.
//!
//! **`BeforeUnloadEvent` is NOT constructible.** `new BeforeUnloadEvent()` is
//! `TypeError: Failed to construct 'BeforeUnloadEvent': Illegal constructor` in Chrome (measured);
//! the only way to get one is `document.createEvent("BeforeUnloadEvent")`. `host.js` made it newable
//! AND gave every instance an own `returnValue` data property, where Chrome puts a get/set ACCESSOR
//! on the prototype and leaves the instance owning nothing but `isTrusted`. Both are fixed here: the
//! table is `interface` (a constructor that throws) and `returnValue` is a slot accessor.
//!
//! `returnValue` on a `BeforeUnloadEvent` is a DOMString (the legacy dialog prompt), NOT `Event`'s
//! legacy boolean — the prototype row shadows `Event.prototype.returnValue` for exactly this reason,
//! and the setter coerces, so `e.returnValue = 5` reads back `"5"`.

use deno_core::v8;

use crate::core::events::ctor::{arg_string, brand, init_dict, init_event, receiver, require_args, require_new};
use crate::core::events::event::evt_slot_write;
use crate::install::MemberDef;

// ── BeforeUnloadEvent ───────────────────────────────────────────────────────────────────────────

/// The Rust mint site: `document.createEvent("BeforeUnloadEvent")`'s only way in, now that the
/// interface object throws. The event is built on the TABLE's prototype, so it is a real
/// `BeforeUnloadEvent` with the brand its rows check.
pub(crate) fn new_before_unload_event<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    // The shared legacy-factory mint: the prototype, the EMPTY type (WPT's
    // EventTarget-dispatchEvent reads it -- the factory hands back an uninitialised event whatever
    // the interface) and the brand. Only `returnValue` is this interface's own.
    let obj = crate::core::events::event::mint_legacy_event(scope, "BeforeUnloadEvent")?;
    let empty = crate::binding::protos::str_val(scope, "");
    evt_slot_write(scope, obj, "returnValue", empty);
    Some(obj)
}

/// `returnValue`: a DOMString slot accessor whose setter COERCES (Chrome: `e.returnValue = 5` reads
/// back `"5"`). The generic `evt_get_set` kit stores the raw value, so this row has its own setter.
fn cb_bue_return_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "BeforeUnloadEvent") else { return };
    match crate::core::events::event::evt_slot_read(scope, this, "returnValue") {
        Some(v) => rv.set(v),
        None => {
            let empty = crate::binding::protos::str_val(scope, "");
            rv.set(empty);
        }
    }
}
fn cb_bue_return_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "BeforeUnloadEvent") else { return };
    let Some(s) = args.get(0).to_string(scope) else { return };
    let v: v8::Local<v8::Value> = s.into();
    evt_slot_write(scope, this, "returnValue", v);
}

pub fn install_before_unload_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::binding::protos::build_fn;
    let g = build_fn(scope, cb_bue_return_value_get, "get returnValue", 0);
    let s = build_fn(scope, cb_bue_return_value_set, "set returnValue", 1);
    crate::install::kits::define_accessor(scope, proto, member, g, Some(s));
}

// ── CookieChangeEvent ───────────────────────────────────────────────────────────────────────────

/// `sequence<CookieListItem>`, held as the event's `[SaveSameObject] FrozenArray<CookieListItem>`: FROZEN, and the
/// one array every read hands out (`install::frozen_array`; it was the converted sequence, writable, C2) -- an empty
/// one when the member is absent. The native cookie-change delivery (`modules/cookie_store`) constructs these
/// through the interface object, so the same dictionary path serves both a page's `new CookieChangeEvent(...)` and
/// the engine's.
fn cookie_list<'s>(scope: &mut v8::PinScope<'s, '_>, dict: Option<v8::Local<v8::Object>>, key: &str) -> v8::Local<'s, v8::Value> {
    let items: Vec<v8::Local<v8::Value>> = dict
        .and_then(|d| crate::binding::protos::get_prop(scope, d, key))
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
        .map(|a| (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect())
        .unwrap_or_default();
    crate::install::frozen_array::frozen(scope, &items).into()
}

pub(crate) fn cb_cookie_change_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "CookieChangeEvent") || !require_args(scope, &args, "CookieChangeEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "CookieChangeEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "CookieChangeEvent", "CookieChangeEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    for name in ["changed", "deleted"] {
        let list = cookie_list(scope, dict, name);
        evt_slot_write(scope, this, name, list);
    }
    brand(scope, this, "CookieChangeEvent");
    rv.set(this.into());
}

// ── the two tables ──────────────────────────────────────────────────────────────────────────────

webapi::interface! {
    "BeforeUnloadEvent" : "Event" [interface, window] {
        ctor "BeforeUnloadEvent"     len 0 = stub,
        attr "returnValue"     : Any       = install_before_unload_event
    }
}

webapi::interface! {
    "CookieChangeEvent" : "Event" [value, window, secure] {
        ctor "CookieChangeEvent"        len 1 = stub,
        attr "changed" : Any readonly         = evt_get,
        attr "deleted" : Any readonly         = evt_get
    }
}

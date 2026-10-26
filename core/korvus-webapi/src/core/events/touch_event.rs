//! `core/events/touch_event.rs` — `Touch` and `TouchEvent`.
//!
//! One file, because `TouchEvent`'s three member lists are lists of `Touch`es and neither interface
//! means anything without the other. Both were classes in `host.js`; `Touch` is not an `Event` at all
//! (it is a plain value object), which is why it sits beside `TouchEvent` rather than in the
//! `UIEvent` chain.
//!
//! `TouchInit` has two REQUIRED members, `identifier` and `target`: `new Touch({})` is a TypeError in
//! Chrome ("Failed to read the 'identifier' property"), where `host.js` handed back a `Touch` with
//! identifier 0 and a null target. Measured; so is every default below.
//!
//! The three lists are `TouchList` objects, as Chrome's are (the oracle ratchet;
//! `core/misc_ifaces.rs`).

use deno_core::v8;

use crate::core::events::ctor::{
    arg_string, brand, dict_bool, dict_double, dict_int, dict_interface, init_dict, init_event, require_args, require_new,
};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null, evt_slot_write_num};
use crate::core::events::ui_event::init_ui_event;

// ── Touch ───────────────────────────────────────────────────────────────────────────────────────

/// A REQUIRED dictionary member: absent is the TypeError Chrome raises, naming the member.
fn required_member(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, key: &str) -> bool {
    let have = d
        .and_then(|d| crate::binding::protos::get_prop(scope, d, key))
        .map(|v| !v.is_undefined())
        .unwrap_or(false);
    if have {
        return true;
    }
    crate::binding::protos::throw_type_error(
        scope,
        &format!("Failed to construct 'Touch': Failed to read the '{key}' property from 'TouchInit': Required member is undefined."),
    );
    false
}

pub(crate) fn cb_touch_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "Touch") || !require_args(scope, &args, "Touch", 1) {
        return;
    }
    let Ok(dict) = init_dict(scope, &args, 0, "Touch", "TouchInit") else { return };
    if !required_member(scope, dict, "identifier") || !required_member(scope, dict, "target") {
        return;
    }
    let this = args.this();
    let id = match dict_int(scope, dict, "identifier", 32, true) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    evt_slot_write_num(scope, this, "identifier", id);
    match dict_interface(scope, dict, "target", "Touch", "TouchInit", &["EventTarget"]) {
        Ok(Some(v)) if !v.is_null() => evt_slot_write(scope, this, "target", v),
        Ok(_) => evt_slot_write_null(scope, this, "target"),
        Err(_) => return,
    }
    for name in ["screenX", "screenY", "clientX", "clientY", "pageX", "pageY", "radiusX", "radiusY", "rotationAngle", "force"] {
        let v = match dict_double(scope, dict, name, "Touch") { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
        evt_slot_write_num(scope, this, name, v);
    }
    brand(scope, this, "Touch");
    rv.set(this.into());
}

// ── TouchEvent ──────────────────────────────────────────────────────────────────────────────────

/// One of the three touch lists: a `TouchList` over the init's `sequence<Touch>`, as Chrome hands
/// back (`core/misc_ifaces.rs`). Absent is an EMPTY list, never undefined.
fn touch_list<'s>(scope: &mut v8::PinScope<'s, '_>, d: Option<v8::Local<v8::Object>>, key: &str) -> v8::Local<'s, v8::Value> {
    let from = d
        .and_then(|d| crate::binding::protos::get_prop(scope, d, key))
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let items: v8::Local<v8::Value> = match from {
        Some(a) => a.into(),
        None => v8::Array::new(scope, 0).into(),
    };
    crate::core::misc_ifaces::new_touch_list(scope, items)
}

pub(crate) fn cb_touch_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "TouchEvent") || !require_args(scope, &args, "TouchEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "TouchEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "TouchEvent", "TouchEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() {
        return;
    }
    for name in ["touches", "targetTouches", "changedTouches"] {
        let list = touch_list(scope, dict, name);
        evt_slot_write(scope, this, name, list);
    }
    for name in ["altKey", "metaKey", "ctrlKey", "shiftKey"] {
        let b = dict_bool(scope, dict, name);
        let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
        evt_slot_write(scope, this, name, v);
    }
    crate::core::events::event::init_modifier_states(scope, this, dict);
    brand(scope, this, "TouchEvent");
    rv.set(this.into());
}

webapi::interface! {
    "Touch" [value, window] {
        ctor "Touch"                            len 1 = stub,
        attr "identifier"    : Any readonly           = evt_get,
        attr "target"        : Any readonly           = evt_get,
        attr "screenX"       : Any readonly           = evt_get,
        attr "screenY"       : Any readonly           = evt_get,
        attr "clientX"       : Any readonly           = evt_get,
        attr "clientY"       : Any readonly           = evt_get,
        attr "pageX"         : Any readonly           = evt_get,
        attr "pageY"         : Any readonly           = evt_get,
        attr "radiusX"       : Any readonly           = evt_get,
        attr "radiusY"       : Any readonly           = evt_get,
        attr "rotationAngle" : Any readonly           = evt_get,
        attr "force"         : Any readonly           = evt_get
    }
}

webapi::interface! {
    "TouchEvent" : "UIEvent" [value, window] {
        ctor "TouchEvent"                       len 1 = stub,
        attr "touches"        : Any readonly          = evt_get,
        attr "targetTouches"  : Any readonly          = evt_get,
        attr "changedTouches" : Any readonly          = evt_get,
        attr "altKey"         : Any readonly          = evt_get,
        attr "metaKey"        : Any readonly          = evt_get,
        attr "ctrlKey"        : Any readonly          = evt_get,
        attr "shiftKey"       : Any readonly          = evt_get
    }
}

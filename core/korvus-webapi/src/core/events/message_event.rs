//! `core/events/message_event.rs` — `MessageEvent` and `MessageChannel`.
//!
//! One file because a `MessageChannel` exists to produce the ports a `MessageEvent` carries, and both
//! were the same pair of `host.js` / `worker-host.js` copies. `MessagePort` itself is already a cppgc
//! class with its own table (`core/messaging/message_port.rs`); this file mints the entangled PAIR
//! through that file's one mint site, `mp_channel_pair`. The `op_mp_channel` round trip the JS class
//! used to take IS DELETED -- once this table was the only minter, nothing called it.
//!
//! Measured on `google-chrome --headless=new`, and wrong before this unit:
//!   * `new MessageEvent("m", {origin: 5}).origin` is the STRING `"5"` — `origin` and `lastEventId`
//!     are `USVString`, and `host.js` stored the number.
//!   * `new MessageEvent("m").ports` is FROZEN. It was a live array a page could push into.
//!   * `new MessageEvent("m", {source: {}})` is a TypeError — `source` is
//!     `(WindowProxy or MessagePort or ServiceWorker)?`, and any object was accepted.
//!
//! `initMessageEvent` is a real body here rather than the D2 stub row the registry carried: the stub
//! threw `Illegal invocation` on a genuine `MessageEvent` (its receiver check wants the shell brand,
//! which a `host.js`-built event never had), so the member was unreachable in practice.

use deno_core::v8;

use crate::core::events::ctor::{
    arg_string, brand, dict_any, dict_interface, dict_string, init_dict, init_event, receiver, require_args, require_new,
};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null};
use crate::install::MemberDef;

/// The three interfaces `MessageEventInit.source` accepts. A `WindowProxy` is this realm's global, so
/// `Window` is the name to test.
const SOURCE_TYPES: &[&str] = &["Window", "MessagePort", "ServiceWorker"];

/// `sequence<MessagePort> ports`: a FROZEN array, empty when absent.
fn ports_value<'s>(scope: &mut v8::PinScope<'s, '_>, dict: Option<v8::Local<v8::Object>>) -> v8::Local<'s, v8::Value> {
    let arr = dict
        .and_then(|d| crate::binding::protos::get_prop(scope, d, "ports"))
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
        .unwrap_or_else(|| v8::Array::new(scope, 0));
    let obj: v8::Local<v8::Object> = arr.into();
    let _ = obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    arr.into()
}

/// The `MessageEvent(type, messageEventInitDict)` steps after `Event`'s. Shared with the Rust mint
/// sites (the worker receive loop, `BroadcastChannel` delivery) through [`init_message_event`].
pub(crate) fn init_message_event(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    dict: Option<v8::Local<v8::Object>>,
) -> Result<(), ()> {
    match dict_any(scope, dict, "data") {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "data", v),
        _ => evt_slot_write_null(scope, this, "data"),
    }
    for name in ["origin", "lastEventId"] {
        let s = dict_string(scope, dict, name)?.unwrap_or_default();
        let v = crate::binding::protos::str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    match dict_interface(scope, dict, "source", "MessageEvent", "MessageEventInit", SOURCE_TYPES)? {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "source", v),
        _ => evt_slot_write_null(scope, this, "source"),
    }
    let ports = ports_value(scope, dict);
    evt_slot_write(scope, this, "ports", ports);
    // `userActivation` is a `UserActivation?` the sender may attach; a script-constructed event and
    // every event this engine delivers have none.
    evt_slot_write_null(scope, this, "userActivation");
    brand(scope, this, "MessageEvent");
    Ok(())
}

pub(crate) fn cb_message_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "MessageEvent") || !require_args(scope, &args, "MessageEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "MessageEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "MessageEvent", "MessageEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_message_event(scope, this, dict).is_err() {
        return;
    }
    rv.set(this.into());
}

/// `initMessageEvent(type, bubbles, cancelable, data, origin, lastEventId, source, ports)` — the
/// legacy positional initialiser, writing the same slots the constructor writes. `length` is 1 (only
/// `type` is required), which is what the capture records.
fn cb_init_message_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "MessageEvent") else { return };
    let Some(ty) = arg_string(scope, &args, 0, "MessageEvent") else { return };
    let tyv = crate::binding::protos::str_val(scope, &ty);
    evt_slot_write(scope, this, "type", tyv);
    for (i, name) in [(1, "bubbles"), (2, "cancelable")] {
        let b = args.get(i).boolean_value(scope);
        crate::core::events::event::evt_flag_set(scope, this, name, b);
    }
    let data = args.get(3);
    if data.is_undefined() || data.is_null() {
        evt_slot_write_null(scope, this, "data");
    } else {
        evt_slot_write(scope, this, "data", data);
    }
    for (i, name) in [(4, "origin"), (5, "lastEventId")] {
        let s = if args.length() > i { arg_string(scope, &args, i, "MessageEvent") } else { Some(String::new()) };
        let Some(s) = s else { return };
        let v = crate::binding::protos::str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    let source = args.get(6);
    if source.is_undefined() || source.is_null() {
        evt_slot_write_null(scope, this, "source");
    } else {
        evt_slot_write(scope, this, "source", source);
    }
    let ports = match v8::Local::<v8::Array>::try_from(args.get(7)) {
        Ok(a) => {
            let o: v8::Local<v8::Object> = a.into();
            let _ = o.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            a
        }
        Err(_) => v8::Array::new(scope, 0),
    };
    let pv: v8::Local<v8::Value> = ports.into();
    evt_slot_write(scope, this, "ports", pv);
}

pub fn install_message_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::events::event::evt_branded_method(scope, proto, member, cb_init_message_event);
}

// ── MessageChannel ──────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_message_channel_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "MessageChannel") {
        return;
    }
    let this = args.this();
    let (a, b) = crate::core::messaging::message_port::mp_channel_pair(scope);
    evt_slot_write(scope, this, "port1", a);
    evt_slot_write(scope, this, "port2", b);
    brand(scope, this, "MessageChannel");
    rv.set(this.into());
}

// The two tables. `MessageEvent` is `Exposed=(Window,Worker)` and so is `MessageChannel`; the worker
// registry's shell copies of both leave with them.

webapi::interface! {
    "MessageEvent" : "Event" [value, both] {
        ctor   "MessageEvent"                             len 1 = stub,
        attr   "data"              : Any readonly nullable      = evt_get_null,
        attr   "origin"            : Any readonly               = evt_get,
        attr   "lastEventId"       : Any readonly               = evt_get,
        attr   "source"            : Any readonly nullable      = evt_get_null,
        attr   "ports"             : Any readonly               = evt_get,
        attr   "userActivation"    : Any readonly nullable      = evt_get_null,
        method "initMessageEvent"  : Any len 1 required 1       = install_message_event
    }
}

webapi::interface! {
    "MessageChannel" [value, both] {
        ctor "MessageChannel"             len 0 = stub,
        attr "port1" : Any readonly             = evt_get,
        attr "port2" : Any readonly             = evt_get
    }
}

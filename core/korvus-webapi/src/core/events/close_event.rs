//! `core/events/close_event.rs` — `CloseEvent`'s constructor (Blink `modules/websockets/close_event.idl`).
//!
//! It was a registry stub, so the `close` event a real `WebSocket` now fires would have carried
//! `code`, `reason` and `wasClean` as the stub's shape defaults, and `new CloseEvent('close', {code:
//! 1000})` minted an object that remembered none of it. The interface STAYS a registry row -- it is
//! `[Exposed=(Window,Worker)]` and sits in both captures' own-global orders, and `globalThis`
//! insertion order is fingerprint surface -- with its three attributes claimed by the event family's
//! branded slot kit (`evt_get`, `tools/sync/gen_shells.py`) and this body joined to the row's
//! constructor in `install::ctors::row_ctor`.
//!
//! `CloseEventInit` is read in Blink's dictionary order -- the inherited `EventInit` members first,
//! then this dictionary's own in lexicographic order: `code`, `reason`, `wasClean`. `code` is an
//! `unsigned short` (modular, no `[Clamp]`), `reason` a `USVString`, `wasClean` a plain boolean.

use deno_core::v8;

use crate::core::events::ctor::{arg_string, brand, dict_bool, dict_int, dict_string, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_slot_write, evt_slot_write_num};

pub(crate) fn cb_close_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "CloseEvent") || !require_args(scope, &args, "CloseEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "CloseEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "CloseEvent", "CloseEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    let code = match dict_int(scope, dict, "code", 16, false) {
        Ok(v) => v.unwrap_or(0.0),
        Err(_) => return,
    };
    let reason = match dict_string(scope, dict, "reason") {
        Ok(v) => v.unwrap_or_default(),
        Err(_) => return,
    };
    let was_clean = dict_bool(scope, dict, "wasClean");
    let wc: v8::Local<v8::Value> = v8::Boolean::new(scope, was_clean).into();
    evt_slot_write(scope, this, "wasClean", wc);
    evt_slot_write_num(scope, this, "code", code);
    let r = crate::binding::protos::str_val(scope, &reason);
    evt_slot_write(scope, this, "reason", r);
    brand(scope, this, "CloseEvent");
    rv.set(this.into());
}

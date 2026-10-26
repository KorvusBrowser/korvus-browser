//! `core/events/ui_event.rs` — `UIEvent`, the base of every input event.
//!
//! Was `host.js`'s `class UIEvent extends Event` for the constructor, a row set in the generated
//! `EVENT_PROTOS` table for the prototype, and a `= stub` block in `generated_registry.rs` for the
//! shape — three descriptions of one interface. One table now, with the constructor in Rust.
//!
//! THE TWO DEFAULTS `host.js` HAD WRONG, both measured on `google-chrome --headless=new`:
//!   * `view` defaults to **null**, not to `globalThis`. `new UIEvent("t").view` is `null` in Chrome;
//!     `host.js` wrote the window there, so every synthesised UI event carried a live `Window`
//!     reference a page could read off an event it was handed.
//!   * `which` ignores `UIEventInit.which` entirely: `new UIEvent("t", {which: 7}).which` is `0`.
//!     It is a per-subclass legacy alias — `button + 1` on a mouse event, `keyCode` on a key event —
//!     and the base value is always 0.

use deno_core::v8;

use crate::core::events::ctor::{brand, dict_any, dict_int, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_slot_write, evt_slot_write_num, evt_slot_write_null};

/// The `UIEvent(type, uiEventInitDict)` steps after `Event`'s. Every subclass calls this, so the
/// `UIEvent` brand and the four base members are on every `MouseEvent`, `KeyboardEvent`,
/// `InputEvent`, `FocusEvent` and `TouchEvent` by construction.
pub(crate) fn init_ui_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) -> Result<(), ()> {
    let detail = dict_int(scope, dict, "detail", 32, true)?.unwrap_or(0.0);
    evt_slot_write_num(scope, this, "detail", detail);
    // `view` is `Window?` — null when absent, and whatever object the dictionary names otherwise
    // (WebIDL would reject a non-Window; Chrome accepts any object here, so this does too rather
    // than inventing a stricter rule than the browser's).
    match dict_any(scope, dict, "view") {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "view", v),
        _ => evt_slot_write_null(scope, this, "view"),
    }
    evt_slot_write_null(scope, this, "sourceCapabilities");
    // The legacy alias, 0 on a plain UIEvent. The subclasses overwrite this slot after calling here.
    evt_slot_write_num(scope, this, "which", 0.0);
    // `pseudoTarget` is Chromium's legacy alias of the element a pseudo-element belongs to; a
    // script-constructed event has no target, so it stays unwritten and its nullable getter reads null.
    brand(scope, this, "UIEvent");
    Ok(())
}

pub(crate) fn cb_ui_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "UIEvent") || !require_args(scope, &args, "UIEvent", 1) {
        return;
    }
    let Some(ty) = crate::core::events::ctor::arg_string(scope, &args, 0, "UIEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "UIEvent", "UIEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() {
        return;
    }
    rv.set(this.into());
}

/// The one bespoke row: the legacy positional initialiser, whose body is `event.rs`'s.
pub fn install_ui_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    crate::core::events::event::install_ui_event_row(scope, proto, member);
}

webapi::interface! {
    "UIEvent" : "Event" [value, window] {
        ctor   "UIEvent"                                     len 1 = stub,
        attr   "view"               : Any readonly nullable        = evt_get_null,
        attr   "detail"             : Any readonly                 = evt_get,
        attr   "sourceCapabilities" : Any readonly nullable        = evt_get_null,
        attr   "which"              : Any readonly                 = evt_get,
        method "initUIEvent"        : Any len 1 required 1         = install_ui_event,
        attr   "pseudoTarget"       : Any readonly nullable        = evt_get_null
    }
}

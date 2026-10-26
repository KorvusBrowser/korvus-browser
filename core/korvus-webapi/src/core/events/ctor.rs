//! `core/events/ctor.rs` — the event family's own constructor steps.
//!
//! Every interface in `core/events/` that is constructible builds its instance HERE: the
//! `Event(type, eventInitDict)` init steps, and this realm's event clock.
//!
//! THE BRAND, the `new.target` guard, the required-argument guard and the WebIDL dictionary readers
//! moved to [`crate::install::webidl`], unchanged, because that unit needs the same
//! machinery for nine interfaces that are not events. They are re-exported below, so every call site
//! in this family still spells them `core::events::ctor::<name>` and no file in it changed.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::core::events::event::{evt_slot_write, evt_slot_write_num, install_own_is_trusted};

pub(crate) use crate::install::webidl::{
    arg_string, brand, brand_accessor, brand_guard, dict_any, has_brand, dict_bool, dict_double, dict_enum, dict_int,
    dict_interface, dict_string, init_dict, receiver, require_args, require_new,
};

// ── the Event init steps ────────────────────────────────────────────────────────────────────────

/// This realm's monotonic clock, read at CALL time out of `OpState` — the SAME `spec::PerfClock`
/// `performance.now()` reads. WPT asserts `event.timeStamp > 0`, and the clock's 100 µs
/// quantization can legitimately floor to exactly 0 inside the first tick of a realm, so a zero reads
/// as the smallest positive value instead. `worker-host.js` used to write a hard `0` here, which is
/// the defect this closes in the worker realm.
pub(crate) fn now_ms(scope: &mut v8::PinScope) -> f64 {
    let n = crate::core::timing::performance::clock_now_ms(scope);
    if n.is_finite() && n > 0.0 {
        n
    } else {
        0.001
    }
}

/// The `Event(type, eventInitDict)` constructor steps, written onto `this`'s private slots.
///
/// Every one of the nine base members is a SLOT, never an own property: `Event.prototype`'s native
/// accessors read them back, and a real event instance owns nothing but `isTrusted`. Only `type`, `timeStamp` and
/// the flags a dictionary sets are written: `target`, `currentTarget` and `eventPhase` read null, null and NONE until
/// a dispatch writes them, and a flag never set is clear (`event::install_event_base_getter`).
pub(crate) fn init_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, type_str: &str, dict: Option<v8::Local<v8::Object>>) {
    let mut bits = 0;
    if dict.is_some() {
        for (name, bit) in EVENT_INIT_FLAGS {
            if dict_bool(scope, dict, name) {
                bits |= bit;
            }
        }
    }
    init_event_flags(scope, this, type_str, bits);
}

/// `EventInit`'s members, by the flag each sets.
const EVENT_INIT_FLAGS: [(&str, i32); 3] = [
    ("bubbles", crate::core::events::event::FLAG_BUBBLES),
    ("cancelable", crate::core::events::event::FLAG_CANCELABLE),
    ("composed", crate::core::events::event::FLAG_COMPOSED),
];

/// The flags a converted `EventInit` sets ([`crate::install::ctor_dicts::convert_dict_values`]'s pairs).
pub(crate) fn event_init_bits(values: &[(&'static str, v8::Local<v8::Value>)]) -> i32 {
    values.iter().filter(|(_, v)| v.is_true()).filter_map(|(n, _)| EVENT_INIT_FLAGS.iter().find(|(m, _)| m == n).map(|(_, b)| *b)).fold(0, |a, b| a | b)
}

/// [`init_event`] with the init dictionary's flags already read.
pub(crate) fn init_event_flags(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, type_str: &str, bits: i32) {
    use crate::core::events::event::{evt_flags_put, flags_key};
    let type_v = str_val(scope, type_str);
    evt_slot_write(scope, this, "type", type_v);
    // every caller hands a fresh object (a construct call's `this`, a minted one), whose flags are all clear
    if bits != 0 {
        let key = flags_key(scope);
        evt_flags_put(scope, this, key, bits);
    }
    let ts = now_ms(scope);
    evt_slot_write_num(scope, this, "timeStamp", ts);
    // [LegacyUnforgeable]: an own, enumerable, NON-configurable native accessor over the Rust trust
    // slot -- on the instance and on no prototype, which is where a browser puts it.
    install_own_is_trusted(scope, this);
    brand(scope, this, "Event");
}

// (A Rust mint site with no dictionary -- `document.createEvent`, the trusted input path,
// `reportError` -- calls `init_event(scope, this, type, None)` directly; there is no separate
// spelling for it, because a second entry point is a second place for the steps to drift.)

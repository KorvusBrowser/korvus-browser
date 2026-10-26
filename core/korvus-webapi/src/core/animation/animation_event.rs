//! `core/animation/animation_event.rs` — `AnimationEvent`, `TransitionEvent` and
//! `AnimationPlaybackEvent`.
//!
//! Both are `Event` subclasses built ENTIRELY from their constructor's init dictionary, so they get
//! the same treatment `SecurityPolicyViolationEvent` does: a raw v8 constructor built with
//! `ConstructorBehavior::Allow` (so `.toString()` reports `[native code]`, which the capture's
//! `ctorNative: true` demands and a JS `class … extends Event` could not give), the shared `Event`
//! init steps written straight onto `this`'s private slots, then each interface's own members on the
//! same slot namespace that the slot getter (`make_evt_slot_get`) reads back.
//!
//! `TransitionEvent` is here rather than in a CSS-transitions file because it is `AnimationEvent`'s
//! twin in every way that matters to this tier: the same five-member shape, the same init-dictionary
//! constructor, and an `animation` member that is an `Animation` from this very model. It carried
//! the identical defect (a shell stub answering `undefined` where the recorded browser answers `""`,
//! `0` and `null`), and fixing one without the other would have been arbitrary.
//!
//! `AnimationPlaybackEvent` is also constructed from Rust: `Animation`'s `finish` and `cancel`
//! events are real dispatched events carrying real `currentTime` / `timelineTime` values, not a
//! promise resolution with the event half left out.

//! The three `interface!` tables at the bottom of this file are the three interfaces' ONE definition
//! site: names, order, kinds, nullability and descriptor flags VERBATIM from the
//! three `install/generated_registry.rs` blocks they replace. Every member row is the `slot_get` kit
//! -- the SAME slot accessor the deleted `dom/09j-animation.js` installed, over the same
//! private-slot namespace these constructors write -- so an event's members read back exactly what
//! its native constructor put there.

use deno_core::v8;

use crate::binding::protos::{get_prop, str_val, throw_type_error};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null, evt_slot_write_num};

use super::{new_on_proto, PROTO_PLAYBACK_EVENT};

// The `Event(type, eventInitDict)` steps, the optional-dictionary read and the required-`type` guard
// used to be COPIED HERE. They are `core/events/ctor.rs`'s now: one `init_event` writes
// the nine base slots, installs the `[LegacyUnforgeable]` `isTrusted` and stamps the `Event` BRAND
// that `Event.prototype`'s rows check, so an `AnimationEvent` is an `Event` to the same rows a
// `new Event("x")` is. The copy here predated that brand and would have made every one of these three
// answer `TypeError: Illegal invocation` for `animationEvent.type`.
use crate::core::events::ctor::{init_dict, init_event, require_args as require_type_args};

/// `type` is a REQUIRED DOMString on all three: `new AnimationEvent()` is a TypeError, which is
/// exactly what Brave 151 records.
fn require_type(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> bool {
    require_type_args(scope, args, iface, 1)
}

// ── AnimationEvent ──────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_animation_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("AnimationEvent"));
        return;
    }
    if !require_type(scope, &args, "AnimationEvent") {
        return;
    }
    let Ok(dict) = init_dict(scope, &args, 1, "AnimationEvent", "AnimationEventInit") else { return };
    let this = args.this();
    let type_str = args.get(0).to_rust_string_lossy(scope);
    init_event(scope, this, &type_str, dict);

    // `animationName` and `pseudoElement` default to "", `elapsedTime` to 0 — the three recorded
    // answers for a bare `new AnimationEvent("x")`.
    for name in ["animationName", "pseudoElement"] {
        let s = dict
            .and_then(|d| get_prop(scope, d, name))
            .filter(|v| !v.is_undefined())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default();
        let v = str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    let elapsed = dict
        .and_then(|d| get_prop(scope, d, "elapsedTime"))
        .filter(|v| !v.is_undefined())
        .and_then(|v| v.number_value(scope))
        .filter(|n| n.is_finite())
        .unwrap_or(0.0);
    evt_slot_write_num(scope, this, "elapsedTime", elapsed);
    // `animation` is the Animation the event was fired for. A script-constructed event has none;
    // `AnimationEventInit` has no member for it, so it is always null here — the recorded answer.
    evt_slot_write_null(scope, this, "animation");
    // `pseudoTarget` is Chromium's legacy alias of the element the pseudo-element belongs to. A
    // script-constructed event has no target, so it is null.
    evt_slot_write_null(scope, this, "pseudoTarget");
    rv.set(this.into());
}

// ── TransitionEvent ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_transition_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("TransitionEvent"));
        return;
    }
    if !require_type(scope, &args, "TransitionEvent") {
        return;
    }
    let Ok(dict) = init_dict(scope, &args, 1, "TransitionEvent", "TransitionEventInit") else { return };
    let this = args.this();
    let type_str = args.get(0).to_rust_string_lossy(scope);
    init_event(scope, this, &type_str, dict);

    for name in ["propertyName", "pseudoElement"] {
        let s = dict
            .and_then(|d| get_prop(scope, d, name))
            .filter(|v| !v.is_undefined())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default();
        let v = str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    let elapsed = dict
        .and_then(|d| get_prop(scope, d, "elapsedTime"))
        .filter(|v| !v.is_undefined())
        .and_then(|v| v.number_value(scope))
        .filter(|n| n.is_finite())
        .unwrap_or(0.0);
    evt_slot_write_num(scope, this, "elapsedTime", elapsed);
    // `animation` is the CSSTransition the event was fired for -- an `Animation` from this model.
    // `TransitionEventInit` has no member for it, so a script-constructed event's is null.
    evt_slot_write_null(scope, this, "animation");
    evt_slot_write_null(scope, this, "pseudoTarget");
    rv.set(this.into());
}

// ── AnimationPlaybackEvent ──────────────────────────────────────────────────────────────────────

/// Write the two `AnimationPlaybackEvent` members. Both are `double?`, so an omitted member is
/// `null` — NOT `undefined`, which is what a stub getter over an unwritten slot would give and what
/// Brave 151's recorded `new AnimationPlaybackEvent("x").currentTime === null` rules out.
fn write_playback_members(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, current: Option<f64>, timeline: Option<f64>) {
    match current {
        Some(n) => evt_slot_write_num(scope, this, "currentTime", n),
        None => evt_slot_write_null(scope, this, "currentTime"),
    }
    match timeline {
        Some(n) => evt_slot_write_num(scope, this, "timelineTime", n),
        None => evt_slot_write_null(scope, this, "timelineTime"),
    }
}

pub(crate) fn cb_playback_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("AnimationPlaybackEvent"));
        return;
    }
    if !require_type(scope, &args, "AnimationPlaybackEvent") {
        return;
    }
    let Ok(dict) = init_dict(scope, &args, 1, "AnimationPlaybackEvent", "AnimationPlaybackEventInit") else { return };
    let this = args.this();
    let type_str = args.get(0).to_rust_string_lossy(scope);
    init_event(scope, this, &type_str, dict);
    let read = |scope: &mut v8::PinScope, key: &str| -> Option<f64> {
        dict.and_then(|d| get_prop(scope, d, key))
            .filter(|v| !v.is_null_or_undefined())
            .and_then(|v| v.number_value(scope))
    };
    let current = read(scope, "currentTime");
    let timeline = read(scope, "timelineTime");
    write_playback_members(scope, this, current, timeline);
    rv.set(this.into());
}

/// Build an `AnimationPlaybackEvent` from Rust — the `finish` and `cancel` events an `Animation`
/// fires. It goes through the REGISTERED prototype rather than `globalThis.AnimationPlaybackEvent`,
/// so a page that overwrites the global still receives a real event.
pub(crate) fn new_playback_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    kind: &str,
    current: Option<f64>,
    timeline: Option<f64>,
) -> Option<v8::Local<'s, v8::Object>> {
    let obj = new_on_proto(scope, PROTO_PLAYBACK_EVENT)?;
    init_event(scope, obj, kind, None);
    write_playback_members(scope, obj, current, timeline);
    Some(obj)
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────
//
// No `install_*` function: every member of all three is the shared `slot_get` kit, which builds the
// very accessor `dom/09j-animation.js` installed (`op_evt_slot_get("get <name>", "<name>")`). The
// rows keep the registry's `nullable` flag verbatim -- it is the row's recorded shape -- but the body
// word is `slot_get` rather than the nullable `slot_get_null`, because these constructors WRITE every
// member (a null where the IDL says null), so the slot is never unread and the two would differ only
// for a receiver that is not an event of this interface.

webapi::interface! {
    "AnimationEvent" : "Event" [value, window] {
    ctor "AnimationEvent" len 1                             = stub,
    attr "animationName" : Object readonly                  = slot_get,
    attr "elapsedTime" : Object readonly                    = slot_get,
    attr "pseudoElement" : Object readonly                  = slot_get,
    attr "animation" : Object readonly nullable             = slot_get,
    attr "pseudoTarget" : Object readonly                   = slot_get
    }
}

// `AnimationEvent`'s twin: the same five-member shape over the same slot namespace, differing only
// in `propertyName` where the other has `animationName`.
webapi::interface! {
    "TransitionEvent" : "Event" [value, window] {
    ctor "TransitionEvent" len 1                            = stub,
    attr "propertyName" : Object readonly                   = slot_get,
    attr "elapsedTime" : Object readonly                    = slot_get,
    attr "pseudoElement" : Object readonly                  = slot_get,
    attr "animation" : Object readonly nullable             = slot_get,
    attr "pseudoTarget" : Object readonly                   = slot_get
    }
}

webapi::interface! {
    "AnimationPlaybackEvent" : "Event" [value, window] {
    ctor "AnimationPlaybackEvent" len 1                     = stub,
    attr "currentTime" : Object readonly nullable           = slot_get,
    attr "timelineTime" : Object readonly nullable          = slot_get
    }
}


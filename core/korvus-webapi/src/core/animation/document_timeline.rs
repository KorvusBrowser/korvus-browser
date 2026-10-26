//! `core/animation/document_timeline.rs` — `AnimationTimeline` and its two concrete subclasses,
//! `DocumentTimeline` and `ScrollTimeline`.
//!
//! `AnimationTimeline` is abstract (Brave 151 records `new AnimationTimeline()` as a TypeError) and
//! carries the two members every timeline has: `currentTime` and `duration`. `DocumentTimeline` adds
//! only a constructor — the capture gives its prototype ZERO own members — and is the timeline every
//! animation created by `element.animate()` or a bare `new Animation()` is attached to.
//!
//! `ScrollTimeline` is the other kind of timeline the capture exposes, and it is here because the
//! `currentTime`/`duration` pair it inherits has to tell the two kinds apart: a document timeline is
//! MONOTONIC (a time in ms), a scroll timeline is PROGRESS-based (a percentage of its scroller's
//! range). What this engine can answer honestly for a scroll timeline is its `source` and `axis` --
//! both pure configuration, both recorded by Brave 151 -- and `null` for its current time, which is
//! the spec's answer for an INACTIVE timeline and is exactly what this engine's is: nothing here
//! drives progress from a scroll offset.
//!
//! `currentTime` is `performance.now()` offset by the timeline's `originTime`. That is deliberately
//! the SAME clock `performance.now()` reads (the kernel's one `spec::PerfClock`, a monotonic clock
//! quantized to 100 µs; `op_perf_now` in korvus-runtime until that op was deleted): a second
//! clock would drift from it, and `document.timeline.currentTime -
//! performance.now()` is a two-line probe. It also means the value ADVANCES between event-loop
//! turns, which is what makes a running animation's `currentTime` advance.
//!
//! The three `interface!` tables at the bottom of this file are the three interfaces' ONE definition
//! site: names, order, kinds, lengths, nullability and descriptor flags VERBATIM
//! from the three `install/generated_registry.rs` blocks they replace. `DocumentTimeline` has NO own
//! members -- the capture gives its prototype zero, and it inherits `currentTime`/`duration` from
//! `AnimationTimeline` -- and its table exists so the interface leaves the registry with its family
//! and so the `document.timeline` mint has a prototype to register.

use deno_core::v8;

use crate::binding::protos::{get_prop, str_val, throw_type_error};
use crate::install::MemberDef;

use super::{set_timeline_origin, timeline_now, timeline_origin};
use crate::install::kits::branded;

pub(crate) fn cb_document_timeline_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("DocumentTimeline"));
        return;
    }
    // `constructor(optional DocumentTimelineOptions options = {})` — `originTime` defaults to 0, so
    // `new DocumentTimeline()` is legal and its current time is the document's own.
    let origin = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|o| get_prop(scope, o, "originTime"))
        .filter(|v| !v.is_undefined())
        .and_then(|v| v.number_value(scope))
        .filter(|n| n.is_finite())
        .unwrap_or(0.0);
    let this = args.this();
    set_timeline_origin(scope, this, origin);
    rv.set(this.into());
}

// ── the brand check the timeline rows share ─────────────────────────────────────────────────────
//
// THE PROBLEM THESE FOUR GETTERS HAD, measured 2026-09-11. None of them checked its receiver, and
// every one of them has an "empty" answer -- `currentTime` reads null for a timeline with no origin
// slot, `duration` is always null, `source`/`axis` fall back to null/"block" for a `ViewTimeline`,
// whose own constructor writes neither slot. So `Object.getOwnPropertyDescriptor(
// AnimationTimeline.prototype, "currentTime").get.call({})` ANSWERED null where Chrome throws, and
// four rows of the recorded surface were readable off any object at all -- a fingerprint in exactly
// the direction the one-table rule exists to remove.
//
// Headless Chrome 150.0.7871.46 (--dump-dom over a loopback server), verbatim:
//   AnimationTimeline.currentTime / duration on {}                 -> TypeError: Illegal invocation
//   ScrollTimeline.source / axis on {}                             -> TypeError: Illegal invocation
//   ScrollTimeline.source on document.timeline                     -> TypeError: Illegal invocation
//   ScrollTimeline.source on new ViewTimeline()                    -> null
//   AnimationTimeline.currentTime on new ScrollTimeline()          -> null
//   AnimationTimeline.currentTime on document.timeline             -> a number
//
// A SLOT cannot be the brand: `ViewTimeline` is still a registry shell whose constructor writes no
// slot at all, and Chrome answers for one. The INTERFACE PROTOTYPE can: each accessor is built with
// its own prototype as the function's `data`, so the check is "is that object on this receiver's
// prototype chain" -- which accepts every instance of the interface and its subclasses (a
// DocumentTimeline, a ScrollTimeline and a ViewTimeline for `AnimationTimeline`'s two rows; a
// ScrollTimeline and a ViewTimeline, but NOT a DocumentTimeline, for `ScrollTimeline`'s), and
// rejects `{}`, `window`, `document` and every other object a page can hand it. The prototype
// travels in the function's data rather than through `globalThis.AnimationTimeline`, so a page that
// overwrites the global cannot move the brand.
//
// RESIDUAL, stated rather than hidden: `Object.create(AnimationTimeline.prototype)` passes this check
// and throws in Chrome, which brands on the wrapper rather than the chain. Closing that needs a slot
// (or a table) for `ViewTimeline`, which is not this lane's interface.


/// Throw Chrome's message for a receiver that is not an instance of the interface.
fn illegal_invocation(scope: &mut v8::PinScope) {
    throw_type_error(scope, "Illegal invocation");
}

/// `AnimationTimeline.currentTime` — a `double?`, and on a document timeline always a Number
/// (Brave 151 records `Object.prototype.toString.call(document.timeline.currentTime)` as
/// `[object Number]`).
///
/// The `originTime` slot is what distinguishes a monotonic timeline from a progress-based one: only
/// `DocumentTimeline`'s constructor writes it. A `ScrollTimeline`/`ViewTimeline` has none, and gets
/// `null` -- the spec's value for an inactive timeline, and the honest one here, where no scroll
/// offset drives progress. Answering a millisecond count for a percentage would be worse than
/// answering nothing.
fn cb_timeline_current_time(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal_invocation(scope);
        return;
    }
    let Some(origin) = timeline_origin(scope, args.this()) else {
        rv.set_null();
        return;
    };
    match timeline_now(scope) {
        Some(now) => rv.set(v8::Number::new(scope, now - origin).into()),
        None => rv.set_null(),
    }
}

/// `AnimationTimeline.duration` — the intrinsic length of a PROGRESS-based timeline (a view or
/// scroll timeline, which measure 0..100%). A document timeline is monotonic and has no end, so it
/// is null, which is the recorded answer.
fn cb_timeline_duration(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal_invocation(scope);
        return;
    }
    rv.set_null();
}

/// `document.timeline` — the one `DocumentTimeline` every animation shares.
pub(crate) fn cb_document_timeline_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match super::document_timeline(scope) {
        Some(t) => rv.set(t.into()),
        None => rv.set_null(),
    }
}

// ── ScrollTimeline ──────────────────────────────────────────────────────────────────────────────

/// The `ScrollAxis` enum.
const AXES: [&str; 4] = ["block", "inline", "x", "y"];

const AXIS_SLOT: &str = "korvus_scroll_axis";
const SOURCE_SLOT: &str = "korvus_scroll_source";

fn priv_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// `document.scrollingElement` — the default `source` of a scroll timeline. In a standards-mode
/// document that is `documentElement`, which is why Brave 151 records
/// `Object.prototype.toString.call(new ScrollTimeline().source)` as `[object HTMLHtmlElement]`.
fn scrolling_element<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let doc = crate::binding::protos::global_get(scope, "document")
        .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())?;
    get_prop(scope, doc, "scrollingElement")
        .filter(|v| v.is_object())
        .or_else(|| get_prop(scope, doc, "documentElement"))
        .filter(|v| v.is_object())
}

pub(crate) fn cb_scroll_timeline_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("ScrollTimeline"));
        return;
    }
    // `constructor(optional ScrollTimelineOptions options = {})`: `source` defaults to the document's
    // scrolling element, `axis` to "block".
    let dict = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let axis = match dict.and_then(|d| get_prop(scope, d, "axis")).filter(|v| !v.is_undefined()) {
        Some(v) => {
            let s = v.to_rust_string_lossy(scope);
            if !AXES.contains(&s.as_str()) {
                throw_type_error(scope, &format!("Failed to construct 'ScrollTimeline': The provided value '{s}' is not a valid enum value of type ScrollAxis."));
                return;
            }
            s
        }
        None => "block".to_string(),
    };
    let source = match dict.and_then(|d| get_prop(scope, d, "source")) {
        Some(v) if v.is_null() => None,
        Some(v) if v.is_object() => Some(v),
        _ => scrolling_element(scope),
    };
    let this = args.this();
    let akey = priv_key(scope, AXIS_SLOT);
    let av = str_val(scope, &axis);
    this.set_private(scope, akey, av);
    let skey = priv_key(scope, SOURCE_SLOT);
    let sv = source.unwrap_or_else(|| v8::null(scope).into());
    this.set_private(scope, skey, sv);
    rv.set(this.into());
}

/// One of `ScrollTimeline`'s two slot readers, read off the instance's private slots.
///
/// A BRANDED receiver with no slots -- a `ViewTimeline`, whose own constructor derives its source
/// from a subject this engine cannot resolve -- gets the WebIDL defaults rather than a throw: `null`
/// and "block", which is what Chrome answers for a view timeline with no subject. An UNBRANDED one
/// (a `{}`, a `DocumentTimeline`) throws, which is what Chrome answers for those.
fn scroll_slot(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, slot: &str, fallback_null: bool) {
    if !branded(scope, args) {
        illegal_invocation(scope);
        return;
    }
    let key = priv_key(scope, slot);
    match args.this().get_private(scope, key).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        None if fallback_null => rv.set_null(),
        None => {
            let v = str_val(scope, "block");
            rv.set(v);
        }
    }
}

/// `ScrollTimeline.source`: the scroller whose progress drives the timeline, `null` for one with no
/// resolvable subject.
fn cb_scroll_timeline_source(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    scroll_slot(scope, &args, &mut rv, SOURCE_SLOT, true);
}

/// `ScrollTimeline.axis`: the `ScrollAxis` the timeline measures along, "block" by WebIDL default.
fn cb_scroll_timeline_axis(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    scroll_slot(scope, &args, &mut rv, AXIS_SLOT, false);
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────

/// Mount one readonly row whose body brand-checks against the prototype it is installed on: the
/// prototype IS the accessor's bound data (see the brand-check block above), so these four rows use
/// this rather than `core::animation::row_get`, which binds the member name.
/// The prototype goes through a `Global` and back so the accessor's `data` carries the SCOPE's
/// lifetime: an `InstallFn` receives `proto` with a lifetime of its own, which `build_fn_data` (whose
/// result outlives the call) cannot borrow from.
fn brand_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = crate::install::kits::proto_data_fn(scope, proto, cb, &format!("get {}", m.name), 0);
    crate::install::kits::define_accessor(scope, proto, m, g, None);
}

/// Mount one `AnimationTimeline` row -- the two members every timeline has, which `DocumentTimeline`,
/// `ScrollTimeline` and (through `ScrollTimeline`) `ViewTimeline` all inherit.
pub fn install_animation_timeline(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "currentTime" => brand_row(scope, proto, member, cb_timeline_current_time),
        "duration" => brand_row(scope, proto, member, cb_timeline_duration),
        other => debug_assert!(false, "install_animation_timeline: unexpected member {other}"),
    }
}

/// Mount one `ScrollTimeline` row -- `source` and `axis`, both pure configuration written by the
/// constructor and read back off the instance's private slots.
pub fn install_scroll_timeline(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "source" => brand_row(scope, proto, member, cb_scroll_timeline_source),
        "axis" => brand_row(scope, proto, member, cb_scroll_timeline_axis),
        other => debug_assert!(false, "install_scroll_timeline: unexpected member {other}"),
    }
}

// `AnimationTimeline` is ABSTRACT -- Brave 151 records `new AnimationTimeline()` as a TypeError --
// so the table's `Model::Interface` interface object is a real constructor that throws "Illegal
// constructor", and nothing mints one.
webapi::interface! {
    "AnimationTimeline" [interface, window] {
    ctor "AnimationTimeline" len 0                          = stub,
    attr "currentTime" : Object readonly nullable           = install_animation_timeline,
    attr "duration" : Object readonly nullable              = install_animation_timeline
    }
}

// ZERO own members, by the capture: everything a `DocumentTimeline` answers is
// `AnimationTimeline`'s, reached through the chain. The table exists so the interface has ONE
// definition site like its two siblings, and so `install_window_animations` has a prototype to register as
// `PROTO_DOCUMENT_TIMELINE` -- which is what `document.timeline` and every `element.animate()` mint
// their timeline on.
webapi::interface! {
    "DocumentTimeline" : "AnimationTimeline" [value, window] {
    ctor "DocumentTimeline" len 0 = stub
    }
}

webapi::interface! {
    "ScrollTimeline" : "AnimationTimeline" [value, window] {
    ctor "ScrollTimeline" len 0                             = stub,
    attr "source" : Object readonly nullable                = install_scroll_timeline,
    attr "axis" : Object readonly                           = install_scroll_timeline
    }
}

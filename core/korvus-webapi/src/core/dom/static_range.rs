//! `core/dom/static_range.rs` -- the `StaticRange` table and its native constructor.
//!
//! `StaticRange` is the lightweight [`super::abstract_range`] snapshot: four boundary members read
//! from the instance's private SLOTS by `AbstractRange.prototype`'s accessors, and nothing of its
//! own. The capture agrees -- `StaticRange.prototype`'s only own key is `constructor` -- so this
//! table is a constructor and a parent and no rows at all.
//!
//! WHAT IT REPLACES: `core/korvus-runtime/src/js/host.js`'s
//! `class StaticRange extends globalThis.AbstractRange`, the LAST of W4.e's remains, and with it the
//! file's `SLOT` helper and the three `KB.__evt*` bridge slots that served it. The old constructor
//! wrote the same four slots through `KB.__evtSlotSet`; this one calls
//! [`crate::core::events::event::evt_slot_write`] directly, which is the function the bridge wrapped.
//!
//! # What the table fixes, measured on `google-chrome --headless=new` 150.0.7871.46
//!
//! The JS class's boundary VALUES were already right (the slots and the getters are shared with
//! `Range`, which is Rust). Every error path was its own invention:
//!
//!   * `StaticRange({...})` without `new` said "Class constructor StaticRange cannot be invoked
//!     without 'new'" -- a JS class tell. Chrome: "Failed to construct 'StaticRange': Please use the
//!     'new' operator, this DOM object constructor cannot be called as a function."
//!   * `new StaticRange()` and `new StaticRange(5)` BOTH said "StaticRange: init required". Chrome
//!     separates the WebIDL arity failure ("1 argument required, but only 0 present.") from the
//!     dictionary conversion ("The provided value is not of type 'StaticRangeInit'."), and treats
//!     `null` and `undefined` as the LATTER -- `StaticRangeInit` is a required argument, so there is
//!     no "absent dictionary" default to fall back to.
//!   * a missing member said "StaticRange: missing endContainer". Chrome: "Failed to read the
//!     'endContainer' property from 'StaticRangeInit': Required member is undefined."
//!   * a member that is not a `Node` (`{startContainer: 5}`, `{startContainer: null}`) was ACCEPTED
//!     -- the JS only tested `=== undefined || === null`, so `null` passed the first three checks and
//!     `nodeType` of a number is `undefined`, which is neither 10 nor 2. The range was constructed
//!     over a number. Chrome: "Failed to read the 'startContainer' property from 'StaticRangeInit':
//!     Failed to convert value to 'Node'."
//!   * THE MEMBERS ARE READ IN LEXICOGRAPHIC ORDER, which is WebIDL's rule for dictionary members and
//!     is observable through WHICH name the first error blames: Chrome answers `endContainer` for
//!     `new StaticRange({})`, not `startContainer`. The JS read them in boundary order
//!     (start, startOffset, end, endOffset) and blamed `startContainer`.
//!   * the offsets were stored VERBATIM. They are `unsigned long`, and WebIDL's default (no
//!     `[EnforceRange]`, no `[Clamp]`) is the MODULAR conversion: Chrome turns -1 into 4294967295,
//!     4294967296 into 0, 1.7 into 1 and NaN into 0, and none of those throw. The JS stored -1, 1.7
//!     and NaN as themselves, so `staticRange.startOffset` could be fractional or NaN.
//!   * the `InvalidNodeTypeError` message was "Invalid node type for range boundary"; Chrome's is
//!     "Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a
//!     DocumentType or Attribute node." The ORDER is also Chrome's: every dictionary member is read
//!     and converted first, and only then are the two containers node-type-checked.
//!
//! An offset BEYOND the container's length is not an error and is not clamped -- `new StaticRange`
//! over `{startOffset: 9999}` on a two-child body keeps 9999 and reports `collapsed` true (verified).
//! That is the whole point of a *static* range: it is a snapshot, not a validated live position.

use deno_core::v8;

use crate::binding::protos::{get_prop, throw_dom, throw_type_error};
use crate::core::events::event::evt_slot_write;
use crate::install::webidl::{is_instance_of, require_args, require_new};

/// The four `StaticRangeInit` members, IN WEBIDL ORDER -- which is lexicographic, not boundary
/// order, and is observable in the first error's name (see the module docs).
///
/// The bool is "this member is a `Node`"; the other two are `unsigned long`.
const INIT_MEMBERS: [(&str, bool); 4] =
    [("endContainer", true), ("endOffset", false), ("startContainer", true), ("startOffset", false)];

/// `new StaticRange(StaticRangeInit init)`.
///
/// `init` is REQUIRED and is not nullable, so `null` fails the dictionary conversion rather than
/// defaulting -- which is why this does not go through `webidl::init_dict` (that helper is for the
/// `optional X init = {}` shape every event constructor has, where `null` and `undefined` mean "all
/// defaults").
pub(crate) fn cb_static_range_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::install::webidl::brand;
    if !require_new(scope, &args, "StaticRange") || !require_args(scope, &args, "StaticRange", 1) {
        return;
    }
    let raw = args.get(0);
    // `null` included: a required, non-nullable dictionary argument has no "absent" case.
    if !raw.is_object() {
        throw_type_error(scope, "Failed to construct 'StaticRange': The provided value is not of type 'StaticRangeInit'.");
        return;
    }
    let Ok(init) = v8::Local::<v8::Object>::try_from(raw) else { return };

    // Pass 1: read and convert all four, in WebIDL order. Nothing is written until every member has
    // converted, so a throw half way leaves no partly-built range behind.
    let mut nodes: Vec<(&str, v8::Local<v8::Value>)> = Vec::with_capacity(2);
    let mut offsets: Vec<(&str, f64)> = Vec::with_capacity(2);
    for (key, is_node) in INIT_MEMBERS {
        let v = match get_prop(scope, init, key) {
            Some(v) if !v.is_undefined() => v,
            _ => {
                throw_type_error(
                    scope,
                    &format!(
                        "Failed to construct 'StaticRange': Failed to read the '{key}' property from 'StaticRangeInit': Required member is undefined."
                    ),
                );
                return;
            }
        };
        if is_node {
            if !is_instance_of(scope, v, "Node") {
                throw_type_error(
                    scope,
                    &format!(
                        "Failed to construct 'StaticRange': Failed to read the '{key}' property from 'StaticRangeInit': Failed to convert value to 'Node'."
                    ),
                );
                return;
            }
            nodes.push((key, v));
        } else {
            // `unsigned long`, MODULAR: no throw for a negative, a fraction, NaN or an out-of-range
            // value -- see the module docs for the four measured answers.
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            offsets.push((key, modular_u32(n)));
        }
    }

    // Pass 2: the node-type check, AFTER every conversion, which is the order Chrome reports.
    // A DocumentType (10) or an Attr (2) is not a valid boundary container.
    for (_, n) in &nodes {
        // The arena's node type (an Attr is no arena node: its own check), not the `nodeType` getter.
        let ty = if crate::core::dom::attr::is_attr(scope, *n) { 2.0 } else { crate::page::node_type_of(scope, *n).unwrap_or(0) as f64 };
        if ty == 10.0 || ty == 2.0 {
            throw_dom(
                scope,
                "Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a DocumentType or Attribute node.",
                "InvalidNodeTypeError",
            );
            return;
        }
    }

    // The four boundary members are INTERNAL SLOTS, not own properties: `AbstractRange.prototype`
    // carries getter-only native accessors over exactly these, and `Range` writes the same four as it
    // moves (`super::range::set_point`), so both subclasses share one storage mechanism and
    // `Object.getOwnPropertyNames(new StaticRange(...))` is `[]` as it is in Chrome.
    let this = args.this();
    for (key, v) in nodes {
        evt_slot_write(scope, this, key, v);
    }
    for (key, n) in offsets {
        let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
        evt_slot_write(scope, this, key, v);
    }
    brand(scope, this, "StaticRange");
    rv.set(this.into());
}

/// WebIDL `unsigned long` without `[EnforceRange]` or `[Clamp]`: truncate toward zero, then take the
/// result modulo 2^32 into `[0, 2^32)`. A non-finite value is 0.
///
/// `install::webidl::modular` is the same rule and is private to that module; this is the one caller
/// outside its dictionary helpers, and it needs the 32-bit unsigned case only.
fn modular_u32(n: f64) -> f64 {
    if !n.is_finite() {
        return 0.0;
    }
    const M: f64 = 4294967296.0;
    let mut r = n.trunc() % M;
    if r < 0.0 {
        r += M;
    }
    r
}

// The capture's `StaticRange` block: a constructor of length 1, the `AbstractRange` parent, and no
// members -- `StaticRange.prototype`'s only own key is `constructor` (measured, and the registry row
// `I98` this replaces carried `members: &[]` too).
webapi::interface! {
    "StaticRange" : "NodeRange" [value, window] {
    ctor "StaticRange" len 1 = stub
    }
}

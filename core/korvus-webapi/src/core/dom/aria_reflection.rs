//! `core/dom/aria_reflection.rs` -- the seven ARIA ELEMENT-reflection attributes.
//!
//! `ariaActionsElements`, `ariaControlsElements`, `ariaDescribedByElements`, `ariaDetailsElements`,
//! `ariaErrorMessageElements`, `ariaFlowToElements`, `ariaLabelledByElements` are rows of the
//! `Element` table (`core/dom/element.rs`) and were generated-shell accessors answering `undefined`
//! before W0.10's JS block, which is what this replaces.
//!
//! WHY THEY ARE NOT `reflect_element`. The `reflect_*` kits read and write a CONTENT ATTRIBUTE; these
//! seven do not. An ARIA element reflection is an "explicitly set attr-element" list -- a real
//! internal slot holding ELEMENT REFERENCES, which is why assigning `[c]` and reading back gives you
//! `c` ITSELF and not the element `document.getElementById(el.getAttribute("aria-labelledby"))` would
//! find. Chrome keeps identity; an attribute round-trip could not.
//!
//! CHROME 150, recorded over loopback 2026-09-11, and two of these the JS got wrong:
//!
//! * unset reads `null` (not `undefined`, not `[]`);
//! * assigned reads back an ARRAY with element IDENTITY preserved (`o.ariaLabelledByElements = [c]`
//!   then `[0] === c`);
//! * the array is FROZEN (`Object.isFrozen` is true) -- the JS handed back a plain `v.slice()`, which
//!   a page could push onto;
//! * two reads give the SAME object (`el.x !== el.x` is FALSE) -- the JS rebuilt a fresh `slice()`
//!   per read, so a page comparing two reads saw two different arrays where Chrome shows one;
//! * assigning `null` clears it back to `null`;
//! * the element owns NO property for any of this (`Object.getOwnPropertyNames(el)` is `[]`), which is
//!   why the store is a `v8::Private`;
//! * the accessor pair is `{get, set, enumerable: true, configurable: true}` with the getter named
//!   `get <name>` / length 0 and the setter `set <name>` / length 1, and a foreign receiver is
//!   `TypeError: Illegal invocation`.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, str_val, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{el_id_of, priv_set};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

/// One private per member name, so the seven lists do not share a slot.
fn slot(name: &str) -> String {
    format!("korvus_aria_{name}")
}

/// The receiver must be an ELEMENT: these are `Element.prototype` rows, and Chrome answers
/// `Illegal invocation` for anything else.
fn ok_receiver(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if el_id_of(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn cb_aria_els_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !ok_receiver(scope, this) {
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    match priv_get(scope, this, &slot(&name)) {
        // The stored value IS the frozen array handed out before: two reads are one object, which is
        // what Chrome does and what a `slice()` per read cannot do.
        Some(v) if v.is_array() => rv.set(v),
        _ => rv.set_null(),
    }
}

/// The elements a `FrozenArray<Element>?` setter was handed, in order: an array's, or an array-LIKE's (WebIDL
/// accepts any iterable; `length` and the indices are what `Array.prototype.slice.call(v)` reads). `None` for
/// null, undefined or a primitive, which clear the list. Shared with `ElementInternals`' rows.
pub(crate) fn given_elements<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let len = match v8::Local::<v8::Array>::try_from(v) {
        Ok(a) => a.length(),
        Err(_) => crate::binding::protos::get_prop(scope, o, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0),
    };
    Some((0..len).filter_map(|i| o.get_index(scope, i)).collect())
}

fn cb_aria_els_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if !ok_receiver(scope, this) {
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let key = slot(&name);
    let Some(items) = given_elements(scope, args.get(0)) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        priv_set(scope, this, &key, u);
        return;
    };
    // HTML's "cached attr-associated elements" (Blink's `Element::GetElementArrayAttribute`): while the elements are
    // the ones the array already lists, that array IS the answer -- `el.x = [a, b]` twice hands out one array, and
    // `el.x = [b]` a new one (Chrome 150 measured, C2).
    if let Some(old) = priv_get(scope, this, &key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let same = old.length() as usize == items.len() && items.iter().enumerate().all(|(i, x)| old.get_index(scope, i as u32).is_some_and(|y| y == *x));
        if same {
            return;
        }
    }
    // A COPY, frozen (`install::frozen_array`): the page's own array must not stay aliased to the element's list (a
    // later `push` would change what the element reports), and Chrome's is frozen.
    let copy = crate::install::frozen_array::frozen(scope, &items);
    priv_set(scope, this, &key, copy.into());
}

/// The seven `aria*Elements` rows of the `Element` table.
pub(crate) fn install_aria_element_reflection(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let d = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_aria_els_get, &format!("get {}", member.name), 0, d);
    let d2 = str_val(scope, member.name);
    let s = build_fn_data(scope, cb_aria_els_set, &format!("set {}", member.name), 1, d2);
    define_accessor(scope, proto, member, g, Some(s));
}

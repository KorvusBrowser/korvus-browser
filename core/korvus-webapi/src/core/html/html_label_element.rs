//! `core/html/html_label_element.rs` -- `HTMLLabelElement` and `HTMLLegendElement` as two
//! `interface!` tables.
//!
//! ONE FILE FOR TWO INTERFACES because they are the same shape, and it is the shape this lane is
//! about: **an element whose form owner is ANOTHER element's**. A label's is its labeled control's; a
//! legend's is its `<fieldset>` parent's. Neither is a reflection and neither is the "nearest ancestor
//! `<form>`" rule every other form-associated element uses, which is why `dom/09c-small-elements.js`
//! carried three `KB.__` bridge slots for them (`__labelControl`, `__labelForm`, `__legendForm`) behind
//! `op_helper_get` native masks. All three are gone; the rows below are mounted by THE installer
//! (`install::live::install_window_element_refs`) with names, order, kinds, nullability and descriptor flags
//! VERBATIM from the `HTMLLabelElement` and `HTMLLegendElement` blocks of
//! `install/generated_registry.rs`.
//!
//! ## `label.control` -- the two rules DO NOT COMPOSE
//!
//! A label's labeled control is found EITHER explicitly, through `for` naming a labelable element in
//! the label's own node tree, OR implicitly, as the label's first labelable descendant -- and an
//! explicit `for` that finds nothing means the label has **NO control**, not a fallback to the
//! descendants. This is the part a re-implementation gets wrong, so it is measured rather than
//! assumed. Chrome 150, over loopback (`--headless --dump-dom`):
//!
//! ```text
//! <label for="c1">          with <input id=c1> present   -> control is that input
//! <label for="nosuch"><input></label>                     -> control is NULL  (NO fallback)
//! <label for="divx"><input></label>  with <div id=divx>   -> control is NULL  (a div is not labelable)
//! <label for=""><input></label>                           -> control is NULL  (an empty `for` still counts)
//! <label><input></label>                                  -> control is the input   (implicit)
//! <label><input type=hidden><input id=x></label>          -> control is #x    (hidden is NOT labelable)
//! document.createElement("label") with htmlFor="c1"       -> control is NULL  (not in the document's tree)
//! ```
//!
//! The labelable elements are `button`, `input` (except `type=hidden`), `meter`, `output`,
//! `progress`, `select`, `textarea` -- AND a FORM-ASSOCIATED CUSTOM ELEMENT, which is why the set is
//! not a selector: whether `<my-control>` is one is a property of its definition
//! (`static formAssociated = true`), and no selector can ask that. `is_labelable` below is the only
//! place that decides, and `core/html/html_input_element.rs::labels_of` -- the `labels` production
//! for all eight -- walks from the other end by calling `label_control` here rather than keeping a
//! second copy of the set.
//!
//! ## `label.form` -- the CONTROL's form owner, not the label's ancestor
//!
//! "The form IDL attribute ... returns the label's form owner", and a label's form owner is whatever
//! its labeled control's is. A label OUTSIDE a form pointing at a control INSIDE one reports that
//! form; a label with no control reports null even when it sits inside a form. Measured:
//!
//! ```text
//! <form id=f1><input id=c1></form><label for="c1">   -> label.form is #f1   (label is outside it)
//! <label for="nosuch"><input></label>                -> label.form is null
//! ```
//!
//! ## `legend.form` -- the `<fieldset>` PARENT's form owner
//!
//! Only a direct `<fieldset>` parent counts; a legend anywhere else has no form. And it is the
//! fieldset's form OWNER, not the fieldset: `<form id=f2><fieldset><legend>` reports `#f2`, while a
//! fieldset outside any form gives null.
//!
//! ## Receiver checks
//!
//! All three getters are `Illegal invocation` on a foreign receiver in Chrome (measured for
//! `HTMLLabelElement.prototype.control`, `.form` and `HTMLLegendElement.prototype.form` on `{}`), and
//! all three answered `undefined`/`null` before this lane because the deleted JS bodies simply read
//! properties off whatever they were handed.

use deno_core::v8;

use crate::binding::protos::{pristine_call, pristine_get, str_val, throw_type_error};
use crate::core::dom::element::{el_id_of, id_ref_in_tree};
use crate::install::live::getter_row;
use crate::install::MemberDef;

/// The labelable elements are NOT a selector, and treating them as one is what
/// `"button,input,meter,output,progress,select,textarea"` got wrong here (and in a second copy in
/// `html_input_element.rs`): **a form-associated custom element is labelable**, and whether it is one
/// is a property of its DEFINITION (`static formAssociated = true`), which no selector can ask. So
/// the implicit search walks every descendant in tree order and [`is_labelable`] decides -- one
/// production, and the only one. `element_internals.rs::form_associated` already reads the flag;
/// `is_labelable` simply never asked it.
const ANY_DESCENDANT: &str = "*";

/// `true` when the receiver is an element with `tag`; otherwise Chrome's `Illegal invocation`.
fn this_tagged(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, tag: &str) -> bool {
    let this = args.this();
    let ok = el_id_of(scope, this).is_some()
        && crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case(tag));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// Is this value a LABELABLE element? Every `<input>` except `type=hidden`, the six others, and a
/// FORM-ASSOCIATED CUSTOM ELEMENT -- the one the fixed selector could not express.
pub(crate) fn is_labelable(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(el) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let Some(tag) = crate::page::tag_name_of(scope, (el).into()) else { return false };
    if tag.eq_ignore_ascii_case("input") {
        let a = str_val(scope, "type");
        let ty = crate::core::dom::element::attr_call(scope, el.into(), "getAttribute", &[a])
            .filter(|v| !v.is_null_or_undefined())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default();
        return !ty.eq_ignore_ascii_case("hidden");
    }
    if ["button", "meter", "output", "progress", "select", "textarea"].iter().any(|t| tag.eq_ignore_ascii_case(t)) {
        return true;
    }
    // A custom element name always contains a hyphen, so this asks the registry only about elements
    // that could possibly be in it -- `<div>` never reaches `form_associated`. An UNDEFINED element
    // (`<x-foo>`) has no definition and is not labelable, which is WPT
    // `custom-elements/form-associated/ElementInternals-labels.html`'s last assertion.
    tag.contains('-') && crate::core::html::element_internals::form_associated(scope, el)
}

/// The label's labeled control, or `None`. The whole rule, both halves, in one place -- and it is
/// THE place: `input.labels` (and the six other `labels` rows, and `ElementInternals.labels`) ask
/// this same function from the other end rather than re-deciding what a label points at.
pub(crate) fn label_control<'s>(scope: &mut v8::PinScope<'s, '_>, label: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let has_for = {
        let a = str_val(scope, "for");
        crate::core::dom::element::attr_call(scope, label.into(), "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false)
    };
    if has_for {
        // An explicit `for` that names nothing labelable means NO control. It does NOT fall back to
        // the descendants -- the two rules do not compose.
        let a = str_val(scope, "for");
        let id = crate::core::dom::element::attr_call(scope, label.into(), "getAttribute", &[a])
            .filter(|v| !v.is_null_or_undefined())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default();
        if id.is_empty() {
            return None;
        }
        let found = id_ref_in_tree(scope, label.into(), &id)?;
        return if is_labelable(scope, found) { Some(found) } else { None };
    }
    // No `for`: the first labelable DESCENDANT, in tree order. `querySelectorAll` is already in tree
    // order, so the filter is the whole decision -- `input type=hidden` is dropped and a
    // form-associated custom element is kept.
    // the realm's own query, never the page's `querySelectorAll` / `length`
    let sel = str_val(scope, ANY_DESCENDANT);
    let all = pristine_call(scope, label.into(), "Element.querySelectorAll", &[sel])?;
    let all = v8::Local::<v8::Object>::try_from(all).ok()?;
    let n = pristine_get(scope, all.into(), "NodeList.length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    for i in 0..n {
        let Some(c) = all.get_index(scope, i) else { continue };
        if is_labelable(scope, c) {
            return Some(c);
        }
    }
    None
}

fn cb_label_control(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "label") {
        return;
    }
    match label_control(scope, args.this()) {
        Some(c) => rv.set(c),
        None => rv.set_null(),
    }
}

/// `label.form` -- the form owner of the label's CONTROL, not the label's own nearest ancestor form.
fn cb_label_form(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "label") {
        return;
    }
    // The control's FORM OWNER, asked of the one production rather than read off the control's own
    // `form` IDL attribute -- a form-associated custom element has no `form` property at all (it is
    // `ElementInternals.form`), so reading one answered `undefined` and the label reported null.
    let f = label_control(scope, args.this())
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| crate::core::html::html_input_element::form_owner_of(scope, c));
    match f {
        Some(f) => rv.set(f),
        None => rv.set_null(),
    }
}

/// `legend.form` -- the form owner of its `<fieldset>` PARENT, and only a direct parent counts.
fn cb_legend_form(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "legend") {
        return;
    }
    let f = pristine_get(scope, args.this().into(), "Node.parentNode")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .filter(|p| {
            crate::page::tag_name_of(scope, (*p).into()).is_some_and(|t| t.eq_ignore_ascii_case("fieldset"))
        })
        .and_then(|p| pristine_get(scope, p.into(), "HTMLFieldSetElement.form"))
        .filter(|v| !v.is_null_or_undefined());
    match f {
        Some(f) => rv.set(f),
        None => rv.set_null(),
    }
}

pub fn install_html_label_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "form" => getter_row(scope, proto, member, cb_label_form),
        "control" => getter_row(scope, proto, member, cb_label_control),
        other => debug_assert!(false, "install_html_label_element: unexpected member {other}"),
    }
}

pub fn install_html_legend_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "form" => getter_row(scope, proto, member, cb_legend_form),
        other => debug_assert!(false, "install_html_legend_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLLabelElement" : "HTMLElement" [element, window] {
        ctor   "HTMLLabelElement"                 len 0    = stub,
        attr   "form"    : Object readonly nullable        = install_html_label_element,
        attr   "htmlFor" : Object cfg "for"                = reflect_str,
        attr   "control" : Object readonly nullable        = install_html_label_element
    }
}

webapi::interface! {
    "HTMLLegendElement" : "HTMLElement" [element, window] {
        ctor   "HTMLLegendElement"                len 0    = stub,
        attr   "form"    : Object readonly nullable        = install_html_legend_element,
        attr   "align"   : Object cfg "align"              = reflect_str
    }
}

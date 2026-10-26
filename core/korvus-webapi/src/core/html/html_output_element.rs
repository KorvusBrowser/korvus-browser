//! `core/html/html_output_element.rs` -- `HTMLOutputElement` and `HTMLFieldSetElement` as two
//! `interface!` tables.
//!
//! `dom/08y-output-fieldset.js` (110 lines, four `KB.__out*` bridge slots, three `op_helper_*` native
//! masks, three `Object.defineProperty` sites) and the `HTMLOutputElement` line of
//! `dom/08b-reflected.js` are gone. Every row below is mounted by THE installer
//! (`install::live::install_window_forms`), with names, order, kinds, lengths, `required` counts,
//! nullability and descriptor flags VERBATIM from the two blocks of `install/generated_registry.rs`.
//!
//! ONE FILE FOR TWO INTERFACES, for the reason the deleted JS gave: they are the same shape -- a
//! form-associated element that carries the whole constraint-validation mixin and NEVER validates. Both
//! record `willValidate === false` unconditionally, both answer `true` from `checkValidity` for that
//! reason, and both get it from the arena's one `will_validate` rule (`korvus_dom::forms`,)
//! (which never makes `<output>`, `<fieldset>` or `<object>` a candidate) rather than
//! from a copy. `form`, `willValidate`, `validity`, `validationMessage`, `labels`, `checkValidity`,
//! `reportValidity` and `setCustomValidity` are that file's callbacks, named by these rows through
//! `install_form_control_shared`.
//!
//! `type` on each is a CONSTANT -- `"output"` and `"fieldset"` -- not a reflection. There is no `type`
//! content attribute involved; the IDL attribute exists so that a script walking a form's controls can
//! tell what it is holding.
//!
//! `output.value` / `output.defaultValue` are the only real logic here, and they are subtler than they
//! look. Neither is an attribute: both are the element's DESCENDANT TEXT CONTENT, and they diverge once
//! script assigns to `value`. The spec keeps a "default value override" that starts null: `value`'s
//! setter captures the current text into the override BEFORE replacing the text, so `defaultValue` keeps
//! reporting what the markup said while `value` reports what script set. The deleted JS kept the
//! override in a module-level `WeakMap`; here it is a V8 PRIVATE SLOT on the element wrapper -- the
//! shape W5.1 L3 established -- so the state lives with the element instead of in a side table, and it
//! never appears in `getOwnPropertyNames`/`for-in`.
//!
//! `fieldset.elements` is the live `HTMLCollection` mint that already exists in
//! `core/dom/collections.rs` (`Kind::FormControls`); this row calls it rather than building a second
//! list. It is a plain `HTMLCollection`, not the `HTMLFormControlsCollection` `<form>` hands out
//! (Chrome 151 agrees, and the bundle's oracle records it).
//!
//! RECEIVER CHECKS. Every body written here rejects a receiver that is not its own element with Chrome's
//! `TypeError: Illegal invocation` (measured in Chrome 151 over loopback for `output.type`,
//! `fieldset.elements` and the rest). Before this lane `HTMLOutputElement.prototype.type` on `{}`
//! answered `"output"` and `HTMLFieldSetElement.prototype.elements` on `{}` answered `undefined`.

use deno_core::v8;

use crate::binding::protos::{set_prop, str_val, throw_type_error};
use crate::core::dom::collections::{make_live, Kind};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{el_id_of, priv_set};
use crate::core::html::html_input_element::install_form_control_shared;
use crate::install::live::getter_row;
use crate::install::MemberDef;

/// The default value override, per element. An ABSENT slot means "null" in the spec's sense: no
/// assignment to `value` has happened yet, so the default IS whatever the text currently says.
const OUTPUT_DEFAULT_PRIV: &str = "korvus_output_default_override";

/// `true` when the receiver is an element with `tag`; otherwise Chrome's `Illegal invocation` TypeError.
fn this_tagged(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, tag: &str) -> bool {
    let this = args.this();
    let ok = crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case(tag))
        && el_id_of(scope, this).is_some();
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// The element's descendant TEXT content -- the concatenation of its child Text nodes' data, which is
/// what `<output>`'s value is defined over (not `textContent`, which descends).
/// Read off the arena ([`crate::page::child_text_content`]), not through page-visible getters.
fn text_of(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> String {
    crate::page::child_text_content(scope, el.into())
}

fn set_text(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, s: &str) {
    let v = str_val(scope, s);
    set_prop(scope, el, "textContent", v);
}

// ── the three constant `type` getters and the output value pair ─────────────────────────────────

fn cb_output_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "output") {
        return;
    }
    let v = str_val(scope, "output");
    rv.set(v);
}

fn cb_fieldset_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "fieldset") {
        return;
    }
    let v = str_val(scope, "fieldset");
    rv.set(v);
}

fn cb_output_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "output") { return }
    let el = args.this();
    let s = text_of(scope, el);
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_output_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "output") { return }
    let el = args.this();
    // "Set this element's default value override to its default value" -- BEFORE the text changes, so
    // the override captures the markup's value and not the new one.
    if priv_get(scope, el, OUTPUT_DEFAULT_PRIV).is_none() {
        let cur = text_of(scope, el);
        let v = str_val(scope, &cur);
        priv_set(scope, el, OUTPUT_DEFAULT_PRIV, v);
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    set_text(scope, el, &s);
}

fn cb_output_default_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "output") { return }
    let el = args.this();
    match priv_get(scope, el, OUTPUT_DEFAULT_PRIV) {
        Some(v) => rv.set(v),
        None => {
            let s = text_of(scope, el);
            let v = str_val(scope, &s);
            rv.set(v);
        }
    }
}

fn cb_output_default_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "output") { return }
    let el = args.this();
    let s = args.get(0).to_rust_string_lossy(scope);
    // With no override yet, assigning the default IS assigning the text; with one, only the override
    // moves and the rendered text stays where `value` put it.
    if priv_get(scope, el, OUTPUT_DEFAULT_PRIV).is_some() {
        let v = str_val(scope, &s);
        priv_set(scope, el, OUTPUT_DEFAULT_PRIV, v);
    } else {
        set_text(scope, el, &s);
    }
}

/// `fieldset.elements` -- the live listed-controls collection, minted as a plain `HTMLCollection`.
fn cb_fieldset_elements(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_tagged(scope, &args, "fieldset") { return }
    let el = args.this();
    let Some(id) = el_id_of(scope, el) else { return };
    let list = make_live(scope, "HTMLCollection", id, Kind::FormControls, "", None);
    rv.set(list);
}

pub fn install_html_output_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::html_input_element::acc2;
    if install_form_control_shared(scope, proto, member) {
        return;
    }
    match member.name {
        "type" => getter_row(scope, proto, member, cb_output_type),
        "defaultValue" => acc2(scope, proto, member, cb_output_default_get, None, cb_output_default_set, None),
        "value" => acc2(scope, proto, member, cb_output_value_get, None, cb_output_value_set, None),
        other => debug_assert!(false, "install_html_output_element: unexpected member {other}"),
    }
}

pub fn install_html_field_set_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if install_form_control_shared(scope, proto, member) {
        return;
    }
    match member.name {
        "type" => getter_row(scope, proto, member, cb_fieldset_type),
        "elements" => getter_row(scope, proto, member, cb_fieldset_elements),
        other => debug_assert!(false, "install_html_field_set_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLOutputElement" : "HTMLElement" [element, window] {
        ctor   "HTMLOutputElement"                   len 0              = stub,
        // [SameObject, PutForwards=value, Reflect="for"] -- a token list over `for`, like link.relList.
        attr   "htmlFor"           : Object    cfg "for"                = reflect_tokens,
        attr   "form"              : Object    readonly nullable        = install_html_output_element,
        attr   "name"              : DomString cfg "name"               = reflect_str,
        attr   "type"              : DomString readonly                 = install_html_output_element,
        attr   "defaultValue"      : DomString                          = install_html_output_element,
        attr   "value"             : DomString                          = install_html_output_element,
        attr   "willValidate"      : Boolean   readonly                 = install_html_output_element,
        attr   "validity"          : Object    readonly                 = install_html_output_element,
        attr   "validationMessage" : DomString readonly                 = install_html_output_element,
        attr   "labels"            : Object    readonly                 = install_html_output_element,
        method "checkValidity"     : Boolean   len 0                    = install_html_output_element,
        method "reportValidity"    : Boolean   len 0                    = install_html_output_element,
        method "setCustomValidity" : Undefined len 1 required 1         = install_html_output_element
    }
}

webapi::interface! {
    "HTMLFieldSetElement" : "HTMLElement" [element, window] {
        ctor   "HTMLFieldSetElement"                 len 0              = stub,
        attr   "disabled"          : Boolean   cfg "disabled"           = reflect_bool,
        attr   "form"              : Object    readonly nullable        = install_html_field_set_element,
        attr   "name"              : DomString cfg "name"               = reflect_str,
        attr   "type"              : DomString readonly                 = install_html_field_set_element,
        attr   "elements"          : Object    readonly                 = install_html_field_set_element,
        attr   "willValidate"      : Boolean   readonly                 = install_html_field_set_element,
        attr   "validity"          : Object    readonly                 = install_html_field_set_element,
        attr   "validationMessage" : DomString readonly                 = install_html_field_set_element,
        method "checkValidity"     : Boolean   len 0                    = install_html_field_set_element,
        method "reportValidity"    : Boolean   len 0                    = install_html_field_set_element,
        method "setCustomValidity" : Undefined len 1 required 1         = install_html_field_set_element
    }
}

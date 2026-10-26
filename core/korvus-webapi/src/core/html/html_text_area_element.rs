//! `core/html/html_text_area_element.rs` -- `HTMLTextAreaElement` as one `interface!` table.
//!
//! `dom/09b-textarea-element.js` (77 lines, 3 `KB.__` bridge slots -- `__taDefaultGet`,
//! `__taDefaultSet`, `__taTextLength` -- behind 3 `op_helper_get`/`op_helper_set` native masks) is
//! gone, and so are the two lines of `dom/08b-reflected.js` that installed this interface's `type`
//! and `value`. Every row below is mounted by THE installer (`install::live::install_window_small_elements`),
//! with names, order, kinds, lengths, `required` counts, nullability and descriptor flags VERBATIM
//! from the `HTMLTextAreaElement` block of `install/generated_registry.rs`.
//!
//! Almost nothing here is textarea-specific machinery. The constraint-validation mixin and the
//! text-selection mixin are the SAME callbacks `<input>`, `<button>`, `<output>` and `<fieldset>`
//! share (`html_input_element.rs`): a textarea is a submittable, labelable element with text to
//! select, and the rules are not per-element -- `input_type()` reports "text" for anything that is
//! not an `<input>`, which is exactly the branch the selection callbacks want.
//!
//! THE THREE THAT ARE GENUINELY TEXTAREA'S OWN, and the three bridge slots this file replaces:
//!
//!   * `defaultValue` is the element's CHILD TEXT CONTENT, not an attribute -- `<input>`'s reflects
//!     the `value` attribute, and a textarea has no `value` attribute at all: the markup's value is
//!     what is written between the tags. The getter concatenates the DIRECT Text children (which is
//!     what "child text content" means, and what the deleted `KB.__taDefaultGet` did); the setter
//!     goes through the real `textContent` setter, so the Node mutation callbacks fire and
//!     MutationObserver sees it.
//!   * `textLength` is the API value's length in UTF-16 code units, so it follows `value` rather than
//!     the markup -- a textarea whose value script has replaced reports the new length. It reads the
//!     `value` row rather than re-deriving it, so the two can never disagree.
//!   * `autocomplete` reads back the AUTOFILL FIELD NAME, so an unrecognised value stores on the
//!     attribute and still reads "". The same rule, and the same body, as `<input>`'s.
//!
//! `type` is a read-only FIXED IDL value ("textarea"), not the `type` content attribute -- get-only,
//! which is what Chrome has and why `08b-reflected.js` had to override the reflected pair by hand.

use deno_core::v8;

use crate::binding::protos::{cb_const_str_get, str_val, unwrap_id};
use crate::binding::host_dom::HostDom;
use crate::core::dom::element::cb_reflect_str_set;
use crate::core::html::html_input_element::{
    acc1, acc2, cb_control_value_get, cb_control_value_set, cb_input_autocomplete_get,
    install_form_control_shared, install_selection_shared,
};
use crate::install::MemberDef;

/// `defaultValue`'s getter: the element's CHILD TEXT CONTENT -- the concatenation of its direct Text
/// children, which is not the same as `textContent` (that walks the whole subtree).
fn cb_ta_default_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut out = String::new();
    if let Some(id) = unwrap_id(scope, args.this().into()) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        for c in dom.children(id) {
            // "Child text content" is the concatenation of the CHILD **Text** nodes -- and
            // `interface CDATASection : Text`, so a CDATA section is one of them. This filtered on
            // `node_type(c) == 3` and so dropped every CDATASection(4), which is the THIRD copy of
            // the defect `CONF DOM2.1` closed in `Dom::collect_text` (whose comment already reads
            // "Text(3) and CDATASection(4) -- the two Text kinds"). WPT's
            // `value-defaultValue-textContent-xhtml.xhtml` appends ONE CDATA section and read "".
            if dom.is_text_node(c) {
                out.push_str(&dom.text_content(c));
            }
        }
    }
    let v = str_val(scope, &out);
    rv.set(v);
}

/// `defaultValue`'s setter: "set the element's child text content", i.e. replace the children with a
/// single Text node. Through the real `textContent` setter, so the mutation callbacks fire.
fn cb_ta_default_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    let v = str_val(scope, &s);
    // the realm's own setter, never the page's `textContent`
    crate::binding::protos::pristine_set(scope, args.this().into(), "Node.textContent", v);
}

/// `textLength`: the API value's length in UTF-16 code units. Reads the `value` row, so it cannot
/// disagree with it.
fn cb_ta_text_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = match crate::binding::protos::pristine_get(scope, args.this().into(), "HTMLTextAreaElement.value") {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => String::new(),
    };
    rv.set_int32(s.encode_utf16().count() as i32);
}

pub fn install_html_text_area_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if install_form_control_shared(scope, proto, member) || install_selection_shared(scope, proto, member) {
        return;
    }
    match member.name {
        "autocomplete" => acc2(scope, proto, member, cb_input_autocomplete_get, None, cb_reflect_str_set, Some("autocomplete")),
        "type" => acc1(scope, proto, member, cb_const_str_get, Some("textarea")),
        "defaultValue" => acc2(scope, proto, member, cb_ta_default_value_get, None, cb_ta_default_value_set, None),
        "value" => acc2(scope, proto, member, cb_control_value_get, None, cb_control_value_set, None),
        "textLength" => acc1(scope, proto, member, cb_ta_text_length, None),
        other => debug_assert!(false, "install_html_text_area_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLTextAreaElement" : "HTMLElement" [element, window] {
        ctor   "HTMLTextAreaElement"              len 0                 = stub,
        attr   "autocomplete"       : Object cfg "autocomplete"         = install_html_text_area_element,
        attr   "cols"               : Object cfg "cols|20|positive"     = reflect_ulong,
        attr   "dirName"            : Object cfg "dirname"              = reflect_str,
        attr   "disabled"           : Object cfg "disabled"             = reflect_bool,
        attr   "form"               : Object readonly nullable          = install_html_text_area_element,
        attr   "maxLength"          : Object cfg "maxlength|-1|nonneg"  = reflect_long,
        attr   "minLength"          : Object cfg "minlength|-1|nonneg"  = reflect_long,
        attr   "name"               : Object cfg "name"                 = reflect_str,
        attr   "placeholder"        : Object cfg "placeholder"          = reflect_str,
        attr   "readOnly"           : Object cfg "readonly"             = reflect_bool,
        attr   "required"           : Object cfg "required"             = reflect_bool,
        attr   "rows"               : Object cfg "rows|2|positive"      = reflect_ulong,
        attr   "wrap"               : Object cfg "wrap"                 = reflect_str,
        attr   "type"               : Object readonly                   = install_html_text_area_element,
        attr   "defaultValue"       : Object                            = install_html_text_area_element,
        attr   "value"              : Object                            = install_html_text_area_element,
        attr   "textLength"         : Object readonly                   = install_html_text_area_element,
        attr   "willValidate"       : Object readonly                   = install_html_text_area_element,
        attr   "validity"           : Object readonly                   = install_html_text_area_element,
        attr   "validationMessage"  : Object readonly                   = install_html_text_area_element,
        attr   "labels"             : Object readonly                   = install_html_text_area_element,
        attr   "selectionStart"     : Object                            = install_html_text_area_element,
        attr   "selectionEnd"       : Object                            = install_html_text_area_element,
        attr   "selectionDirection" : Object                            = install_html_text_area_element,
        method "checkValidity"      : Object len 0                      = install_html_text_area_element,
        method "reportValidity"     : Object len 0                      = install_html_text_area_element,
        method "select"             : Object len 0                      = install_html_text_area_element,
        method "setCustomValidity"  : Object len 1 required 1           = install_html_text_area_element,
        method "setRangeText"       : Object len 1 required 1           = install_html_text_area_element,
        method "setSelectionRange"  : Object len 2 required 2           = install_html_text_area_element,
        // Chrome 152 (`HTMLTextAreaElement includes OpaqueRangeCreation`), last on the prototype.
        method "createValueRange"   : Object len 2 required 2           = install_html_text_area_element
    }
}

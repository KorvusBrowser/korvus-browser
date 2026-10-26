//! `core/html/html_option_element.rs` -- `HTMLOptionElement` as one `interface!` table.
//!
//! The `<option>` half of `dom/09a-media-parts.js` -- it rode along there because it was the last of
//! the small form-element interfaces, not because it is media. Its rules are its own, and they are
//! the reason it gets its own file: five of its eight members are IDL attributes that are NOT content
//! attributes, and four of those read the element's own text or its parent `<select>`.
//!
//! Rows are VERBATIM from the `HTMLOptionElement` block of `install/generated_registry.rs`; mounted
//! by THE installer (`install::live::install_window_media`). Six `KB.__opt*` bridge slots and three
//! `op_helper_get`/`op_helper_set` native masks went with the file, and both ops were deleted
//! outright once the bridge was empty -- nothing in the tree builds a native function over a
//! `KB.__*` slot any more.
//!
//! THE FIVE RULES, each recorded rather than guessed:
//!
//!   * `text` is the option's DESCENDANT TEXT CONTENT with ASCII whitespace stripped and collapsed --
//!     not an attribute, and not raw `textContent`. The setter sets the child text content.
//!   * `label` is the `label` attribute when present, and the option's `text` when not, so an option
//!     with no label attribute labels itself with what it says. The setter always writes the
//!     attribute.
//!   * `value` is the `value` attribute when present, and the option's `text` when not.
//!   * `selected` is per-element state that starts out tracking the `selected` content attribute and
//!     parts company with it the moment script assigns -- the same dirty-flag shape as
//!     `input.checked`. (`defaultSelected` is the attribute itself, and stays a plain reflection.)
//!   * `index` is 0 for an option that is in no list at all -- NOT -1, which is what every other
//!     positional member in the DOM answers. The spec says so and Chrome agrees.
//!
//! `form` is the shared form-association scan `<input>`, `<button>` and `<textarea>` use: an
//! `<option>`'s form owner is its `<select>`'s, which the ancestor walk finds because the `<select>`
//! is one of them.

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{build_fn_data, pristine_call, pristine_get, pristine_set, str_val};
use crate::core::dom::collections::is_html_named;
use crate::core::html::html_element::local_name_in;
use crate::core::html::html_input_element::{acc1, cb_input_form_get};
use crate::install::MemberDef;

/// One option's SELECTEDNESS -- the arena's (`korvus_dom::forms`), which `:checked` and
/// `HTMLSelectElement.selectedIndex` read too: the dirty value when script has assigned, else the
/// `selected` attribute, with a single-selection `<select>`'s "last one on wins, else the first option
/// that is not disabled" applied (it was a V8 private slot the cascade could not see).
pub(crate) fn option_selectedness(scope: &mut v8::PinScope, opt: v8::Local<v8::Object>) -> bool {
    let Some(id) = crate::binding::protos::unwrap_id(scope, opt.into()) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.option_selectedness(id);
    v
}

/// HTML's `option.text`: *"the descendant text content of the option element, with ASCII whitespace
/// stripped and collapsed, **excluding any Text node descendants of `script` or SVG `script`
/// element descendants***."
///
/// IT READ `textContent`, WHICH HAS NO EXCLUSION. `textContent` is the descendant text content and
/// nothing else, so `<option>before<script>script</script>after</option>` answered
/// `"before script after"` -- the option's label included the body of a script that is not rendered
/// anywhere. The exclusion is the whole difference between the two algorithms, and it is a SUBTREE
/// exclusion, not a child one: the `<span> Some <script> script </script></span>` case in WPT's
/// `option-text-recurse.html` puts the script two levels down.
///
/// ONLY those two. A MathML `script` and a null-namespace `script` are NOT excluded -- both are
/// separate subtests in the same file, and both expect the script's text to appear -- so the test is
/// the local name `script` in the HTML or the SVG namespace, which is what [`is_html_named`] and the
/// namespace on the arena node answer. The walk is over the arena rather than through `textContent`
/// because there is no way to express "all of it except these subtrees" as a property read.
fn option_text(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    let Some(id) = crate::binding::protos::unwrap_id(scope, this.into()) else { return String::new() };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let mut raw = String::new();
    collect_option_text(&st, id, &mut raw);
    collapse_ascii(&raw)
}

/// What a drop-down `<select>` shows for option `id` (Blink's `HTMLOptionElement::DisplayLabel`): its `label`
/// attribute when that is not empty, else its text as [`option_text`] reads it.
pub(crate) fn option_display_label(st: &deno_core::OpState, id: u32) -> String {
    if let Some(l) = st.borrow::<HostDom>().0.get_attribute(id, "label").filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    let mut raw = String::new();
    collect_option_text(st, id, &mut raw);
    collapse_ascii(&raw)
}

/// The descendant-text walk `option_text` needs: `id`'s subtree in tree order, skipping any child
/// that is a `script` element in the HTML or the SVG namespace (subtree and all). The receiver
/// itself is never skipped -- `option.text` on an `<option>` that is INSIDE a `<script>` still reads
/// its own text, which is three more subtests in the same file.
fn collect_option_text(st: &deno_core::OpState, id: u32, out: &mut String) {
    for c in st.borrow::<HostDom>().0.children(id) {
        if is_script_subtree(st, c) {
            continue;
        }
        // `text_content` is the arena's descendant-or-self text content, so a Text or CDATASection
        // child contributes its own data and a Comment or PI contributes nothing (DOM2.1).
        if st.borrow::<HostDom>().0.node_type(c) == 1 {
            collect_option_text(st, c, out);
        } else {
            out.push_str(&st.borrow::<HostDom>().0.text_content(c));
        }
    }
}

/// A `script` element in the HTML or the SVG namespace -- the two subtrees `option.text` skips.
fn is_script_subtree(st: &deno_core::OpState, id: u32) -> bool {
    if is_html_named(st, id, "script") {
        return true;
    }
    let svg = matches!(st.borrow::<HostDom>().0.element_ns(id), Some(korvus_dom::ElementNs::Svg));
    svg && local_name_in(st, id).is_some_and(|n| n == "script")
}

/// `s.replace(/[ \t\n\f\r]+/g, " ").trim()` -- the JS this replaces, verbatim in behaviour. Note that
/// the character class is HTML's ASCII whitespace, not Rust's `char::is_whitespace` (which would also
/// eat U+00A0 and the Unicode spaces, and so would answer differently for a non-breaking space).
fn collapse_ascii(s: &str) -> String {
    let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r');
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if ws(c) {
            in_ws = true;
            continue;
        }
        if in_ws && !out.is_empty() {
            out.push(' ');
        }
        in_ws = false;
        out.push(c);
    }
    out
}

pub(crate) fn has_attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> bool {
    let a = str_val(scope, name);
    crate::core::dom::element::attr_call(scope, this.into(), "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

fn get_attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> String {
    let a = str_val(scope, name);
    match crate::core::dom::element::attr_call(scope, this.into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => String::new(),
    }
}

/// `text`'s getter.
fn cb_opt_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = option_text(scope, args.this());
    let v = str_val(scope, &t);
    rv.set(v);
}
/// `text`'s setter: "set the element's child text content", through the realm's own `textContent` setter,
/// so the Node mutation callbacks fire and MutationObserver sees it -- never the page's.
fn cb_opt_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    let v = str_val(scope, &s);
    pristine_set(scope, args.this().into(), "Node.textContent", v);
}

/// `label` and `value` share one body: the named attribute when present, the option's `text` when
/// not. `data` is the attribute name.
fn cb_opt_attr_or_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    let out = if has_attr(scope, this, &attr) { get_attr(scope, this, &attr) } else { option_text(scope, this) };
    let v = str_val(scope, &out);
    rv.set(v);
}
/// Their setter: always the attribute (`data` is its name).
fn cb_opt_attr_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let s = args.get(0).to_rust_string_lossy(scope);
    let a = str_val(scope, &attr);
    let v = str_val(scope, &s);
    crate::core::dom::element::attr_call(scope, args.this().into(), "setAttribute", &[a, v]);
}

/// `selected`'s getter: the dirty slot when script has assigned, else the `selected` attribute.
fn cb_opt_selected_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let b = option_selectedness(scope, args.this());
    rv.set_bool(b);
}

/// `selected = v`: set this option's selectedness and dirtiness and, when it went TRUE inside a
/// single-selection `<select>`, turn every other option of it off -- which is why `option.selected =
/// true` moves `select.selectedIndex` in a browser. The arena does both (`Dom::set_option_selectedness`).
fn cb_opt_selected_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let b = args.get(0).boolean_value(scope);
    let Some(id) = crate::binding::protos::unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_option_selectedness(id, b);
}

/// `index`: "If the option is not in a list of options, return 0" -- not -1. An `<option>` inside an
/// `<optgroup>` is in its grandparent `<select>`'s list, which is why the walk takes one step up
/// through a group.
fn cb_opt_index<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let tag_of = |scope: &mut v8::PinScope, o: v8::Local<v8::Object>| -> String {
        crate::page::tag_name_of(scope, (o).into()).unwrap_or_default()
    };
    // the tree read with the realm's own members, never the page's
    let up = |scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>| -> Option<v8::Local<'s, v8::Object>> {
        pristine_get(scope, o.into(), "Node.parentNode")
            .filter(|v| !v.is_null_or_undefined())
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    };
    let Some(parent) = up(scope, this) else {
        rv.set_int32(0);
        return;
    };
    let owner = if tag_of(scope, parent) == "OPTGROUP" {
        match up(scope, parent) {
            Some(g) => g,
            None => {
                rv.set_int32(0);
                return;
            }
        }
    } else {
        parent
    };
    if tag_of(scope, owner) != "SELECT" {
        rv.set_int32(0);
        return;
    }
    let sel = str_val(scope, "option");
    let Some(list) =
        pristine_call(scope, owner.into(), "Element.querySelectorAll", &[sel]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        rv.set_int32(0);
        return;
    };
    let len = pristine_get(scope, list.into(), "NodeList.length").and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u32;
    let me: v8::Local<v8::Value> = this.into();
    for i in 0..len {
        if list.get_index(scope, i).map(|v| v == me).unwrap_or(false) {
            rv.set_int32(i as i32);
            return;
        }
    }
    rv.set_int32(0);
}

/// A row whose getter/setter share one callback and are told apart by `data`.
fn acc_attr<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    attr: &str,
) {
    let gd = str_val(scope, attr);
    let sd = str_val(scope, attr);
    let g = build_fn_data(scope, cb_opt_attr_or_text_get, &format!("get {}", m.name), 0, gd);
    let s = build_fn_data(scope, cb_opt_attr_set, &format!("set {}", m.name), 1, sd);
    crate::install::kits::define_accessor(scope, proto, m, g, Some(s));
}

pub fn install_html_option_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::html_input_element::acc2;
    match member.name {
        "form" => acc1(scope, proto, member, cb_input_form_get, None),
        "label" => acc_attr(scope, proto, member, "label"),
        "value" => acc_attr(scope, proto, member, "value"),
        "selected" => acc2(scope, proto, member, cb_opt_selected_get, None, cb_opt_selected_set, None),
        "text" => acc2(scope, proto, member, cb_opt_text_get, None, cb_opt_text_set, None),
        "index" => acc1(scope, proto, member, cb_opt_index, None),
        other => debug_assert!(false, "install_html_option_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLOptionElement" : "HTMLElement" [element, window] {
    ctor            "HTMLOptionElement" len 0                         = stub,
    attr            "disabled" : Object cfg "disabled"                = reflect_bool,
    attr            "form" : Object readonly nullable                 = install_html_option_element,
    attr            "label" : Object cfg "label"                      = install_html_option_element,
    attr            "defaultSelected" : Object cfg "selected"         = reflect_bool,
    attr            "selected" : Object                               = install_html_option_element,
    attr            "value" : Object cfg "value"                      = install_html_option_element,
    attr            "text" : Object                                   = install_html_option_element,
    attr            "index" : Object readonly                         = install_html_option_element
    }
}

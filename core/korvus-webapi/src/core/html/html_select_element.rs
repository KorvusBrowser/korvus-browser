//! `core/html/html_select_element.rs` -- `HTMLSelectElement`, `HTMLDataListElement` and
//! `HTMLProgressElement` as three `interface!` tables.
//!
//! THREE INTERFACES IN ONE FILE, for the reason the deleted `dom/08b-reflected.js` gave by writing
//! them as two loops: `<select>.options` and `<datalist>.options` are ONE live list rule
//! (`Kind::Options`) with two interface names, and `<select>.value` and `<progress>.value` are ONE
//! pair of control-value callbacks. Splitting them would copy each rule.
//!
//! Six rows across the three carry the Rust body the deleted JS mounted, using the same callbacks:
//! `type`, `options`, `selectedIndex` and `value` on `<select>`, `options` on `<datalist>`, `value`
//! on `<progress>`. Eight MORE rows on `<select>` gain one they never had -- the constraint-validation
//! and `labels` mixin `install_form_control_shared` already gives `<input>`, `<button>`, `<object>`
//! and `<textarea>`; see the install for why that is this lane's to close. Every remaining row keeps
//! exactly the body the generated registry gave it: five shared reflect kits and nine honest D2
//! stubs, transcribed with their `cfg`s, lengths and `required` counts.
//!
//! `selectedIndex`'s two bodies are the port of dom/03-element-glue.js's bridge helpers: the OPTIONS are
//! the `<option>` descendants in tree order (what `querySelectorAll("option")` answered, optgroups
//! included), "selected" is the `selected` content attribute, and the setter writes that attribute
//! through the native attribute choke point so a MutationObserver sees it exactly as it saw the JS
//! helper's `setAttribute`.

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::unwrap_id;
use crate::install::live::{getter_row, method_row, pair_row};
use crate::install::MemberDef;

/// `selectedIndex`: the index of the first selected option in the list of options, else -1. The
/// selection is the arena's (`korvus_dom::forms`): Chrome 150 answers 1 for
/// `<select><option disabled>1<option>2</select>`, whose first non-disabled option is selected.
fn cb_select_selected_index_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { rv.set_int32(-1); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let options = dom.list_of_options(id);
    let i = dom.selected_options(id).first().and_then(|s| options.iter().position(|o| o == s));
    rv.set_int32(i.map_or(-1, |i| i as i32));
}

/// `selectedIndex = n`: the option at `n` on, every other off. `n` is the WebIDL `long` conversion
/// of the value (ToInt32), as Chrome applies it. Selectedness, not the `selected` attribute: Chrome
/// leaves `defaultSelected` alone.
fn cb_select_selected_index_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let Some(idx) = args.get(0).int32_value(scope) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_selected_index(id, idx);
}

// ── the options-list members ────────────────────────────────────────────────────────────────────
//
// `length`, `add`, `remove(index)`, `item`, `namedItem` and the indexed getter/setter "act like
// [their] namesake method on that same select element's options collection" (HTML §4.10.7), so each
// body below is the receiver check and a call into the collection's own (`collections.rs`). Measured
// in Chrome 151 on `tools/realweb/seed/sel`: all of them were D2 stubs here, so
// `select.add(new Option())` -- docs.python.org's version switcher -- added nothing, and `sel[1]`
// answered undefined.

/// The receiver as an HTML `<select>`, or the WebIDL "Illegal invocation" TypeError.
fn select_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let id = unwrap_id(scope, this.into()).filter(|&id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_html_ns(id) && dom.tag_name(id) == Some("select")
    });
    if id.is_none() {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
    }
    id
}

fn cb_select_length_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_len, select_options};
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(c) = select_options(scope, id) else { return };
    rv.set_uint32(options_len(scope, c) as u32);
}

fn cb_select_length_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_set_length, select_options};
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(want) = args.get(0).uint32_value(scope) else { return };
    let Some(c) = select_options(scope, id) else { return };
    options_set_length(scope, c, want);
}

fn cb_select_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_add, select_options};
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(c) = select_options(scope, id) else { return };
    options_add(scope, c, "HTMLSelectElement", args.get(0), args.get(1));
}

/// Two overloads: `remove()` is the select removing ITSELF (ChildNode's), `remove(index)` removes an
/// option. WebIDL picks by argument count, so `sel.remove(undefined)` is `remove(0)`.
fn cb_select_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_remove, select_options};
    if args.length() == 0 {
        crate::core::dom::node::cb_remove_self(scope, args, rv);
        return;
    }
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(i) = args.get(0).int32_value(scope) else { return };
    let Some(c) = select_options(scope, id) else { return };
    options_remove(scope, c, i);
}

fn cb_select_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_item, select_options};
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(i) = args.get(0).uint32_value(scope) else { return };
    let Some(c) = select_options(scope, id) else { return };
    match options_item(scope, c, i) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

fn cb_select_named_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::dom::collections::{options_named, select_options};
    let Some(id) = select_recv(scope, args.this()) else { return };
    let Some(name) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let Some(c) = select_options(scope, id) else { return };
    match options_named(scope, c, &name) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

// ── the indexed interceptor a `<select>` wrapper carries ────────────────────────────────────────
//
// `getter HTMLOptionElement? item(unsigned long index)` and `setter undefined (unsigned long index,
// HTMLOptionElement? option)` make `sel[i]` an option and `sel[i] = opt` an insertion. V8 fixes a
// wrapper's interceptors when it is instantiated, so `<select>` wrappers are minted from their own
// template (`binding::protos::wrap_cached`); these are its callbacks. An index is supported while it
// is below the option count, and it is writable, enumerable and configurable, because the interface
// declares a setter (Chrome 151: `Object.getOwnPropertyDescriptor(sel, 0)` is all three).

fn holder_options<'s>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = unwrap_id(scope, holder.into())?;
    crate::core::dom::collections::select_options(scope, id)
}

fn cb_select_idx_get<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let Some(c) = holder_options(scope, args.holder()) else { return v8::Intercepted::kNo };
    match crate::core::dom::collections::options_item(scope, c, index) {
        Some(v) => {
            rv.set(v);
            v8::Intercepted::kYes
        }
        None => v8::Intercepted::kNo,
    }
}

fn cb_select_idx_query<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(c) = holder_options(scope, args.holder()) else { return v8::Intercepted::kNo };
    if index as usize >= crate::core::dom::collections::options_len(scope, c) {
        return v8::Intercepted::kNo;
    }
    let attrs = v8::Integer::new(scope, v8::PropertyAttribute::NONE.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

fn cb_select_idx_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let n = match holder_options(scope, args.holder()) {
        Some(c) => crate::core::dom::collections::options_len(scope, c),
        None => 0,
    };
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let k = v8::Integer::new_from_unsigned(scope, i as u32);
        arr.set_index(scope, i as u32, k.into());
    }
    rv.set(arr);
}

fn cb_select_idx_set<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, value: v8::Local<'s, v8::Value>, args: v8::PropertyCallbackArguments<'s>, _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let Some(c) = holder_options(scope, args.holder()) else { return v8::Intercepted::kNo };
    crate::core::dom::collections::options_set_index(scope, c, "HTMLSelectElement", index, value);
    v8::Intercepted::kYes
}

/// WebIDL [[DefineOwnProperty]] with an indexed setter: a data descriptor runs the setter, anything
/// else is refused with Blink's message.
fn cb_select_idx_define<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, desc: &v8::PropertyDescriptor, args: v8::PropertyCallbackArguments<'s>, _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let Some(c) = holder_options(scope, args.holder()) else { return v8::Intercepted::kNo };
    if desc.has_value() {
        let v = desc.value();
        crate::core::dom::collections::options_set_index(scope, c, "HTMLSelectElement", index, v);
    } else {
        crate::binding::protos::throw_type_error(scope, &format!("Failed to set an indexed property [{index}] on 'HTMLSelectElement': Accessor properties are not allowed."));
    }
    v8::Intercepted::kYes
}

/// A supported index cannot be deleted (no indexed deleter); one past the end deletes fine.
fn cb_select_idx_delete<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let Some(c) = holder_options(scope, args.holder()) else { return v8::Intercepted::kNo };
    if index as usize >= crate::core::dom::collections::options_len(scope, c) {
        return v8::Intercepted::kNo;
    }
    let f = v8::Boolean::new(scope, false);
    rv.set(f);
    v8::Intercepted::kYes
}

/// The indexed handler `binding::protos` puts on the `<select>` wrapper template.
pub(crate) fn select_indexed_config<'s>() -> v8::IndexedPropertyHandlerConfiguration<'s> {
    use deno_core::v8::MapFnTo;
    v8::IndexedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(idx_get, "HTMLSelectElement", cb_select_idx_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(idx_query, "HTMLSelectElement", cb_select_idx_query).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, "HTMLSelectElement", cb_select_idx_enum).map_fn_to()))
        .setter_raw(crate::binding::refs::c(icpt!(idx_set, "HTMLSelectElement", cb_select_idx_set).map_fn_to()))
        .definer_raw(crate::binding::refs::c(icpt!(idx_define, "HTMLSelectElement", cb_select_idx_define).map_fn_to()))
        .deleter_raw(crate::binding::refs::c(icpt!(idx_delete, "HTMLSelectElement", cb_select_idx_delete).map_fn_to()))
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────

/// `<select>.options` is an `HTMLOptionsCollection`; `<datalist>.options` is a plain
/// `HTMLCollection`. Same live list (`Kind::Options`), two interface names -- measured in Chrome 151:
/// `Object.prototype.toString.call(datalist.options)` is `[object HTMLCollection]`. One rule with two
/// answers is why the deleted JS installed them in one loop, and why the two tables share this file.
fn options_row(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str, mut rv: v8::ReturnValue) {
    let Some(id) = crate::core::dom::element::el_id_of(scope, args.this()) else { return };
    if let Some(list) = crate::core::dom::collections::owned_options(scope, id, iface) {
        rv.set(list.into());
    }
}

fn cb_select_options(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    options_row(scope, &args, "HTMLOptionsCollection", rv);
}

fn cb_datalist_options(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    options_row(scope, &args, "HTMLCollection", rv);
}

pub fn install_html_select_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::html_element::cb_select_type_get;
    use crate::core::html::html_input_element::{cb_control_value_get, cb_control_value_set, install_form_control_shared};
    // `<select>` is a listed, submittable, VALIDATING control, so the eight constraint-validation and
    // `labels` members are the ones `<input>`, `<button>`, `<object>` and `<textarea>` already share --
    // one definition, branching on the element. They were fifteen registry D2 stubs before this table,
    // and the stub is why `korvus-runtime/tests/forms_live.rs` recorded a MEASURED DEFECT: a form
    // containing any `<select>` reported itself invalid, because `form.checkValidity()` delegates to
    // each control's own and `!undefined` is truthy. Chrome 151 (recorded in that test): both are
    // `true`. Closing it here is this lane's, because this is the lane that gives `<select>` a table.
    if install_form_control_shared(scope, proto, member) {
        return;
    }
    match member.name {
        // A read-only COMPUTED value ("select-one" / "select-multiple"), not the `type` content
        // attribute -- so a get-only accessor, which is what Chrome has (measured: the descriptor's
        // `set` is undefined). The registry's own row is a stub; the body is `html_element.rs`'s.
        "type" => getter_row(scope, proto, member, cb_select_type_get),
        "options" => getter_row(scope, proto, member, cb_select_options),
        "selectedIndex" => pair_row(scope, proto, member, cb_select_selected_index_get, cb_select_selected_index_set),
        "value" => pair_row(scope, proto, member, cb_control_value_get, cb_control_value_set),
        "length" => pair_row(scope, proto, member, cb_select_length_get, cb_select_length_set),
        "add" => method_row(scope, proto, member, cb_select_add),
        "remove" => method_row(scope, proto, member, cb_select_remove),
        "item" => method_row(scope, proto, member, cb_select_item),
        "namedItem" => method_row(scope, proto, member, cb_select_named_item),
        "selectedOptions" => getter_row(scope, proto, member, cb_select_selected_options),
        "showPicker" => method_row(scope, proto, member, cb_select_show_picker),
        // The autofill reflection `<input>` already answers: an invalid token list reads "".
        "autocomplete" => {
            let d = crate::binding::protos::str_val(scope, "autocomplete");
            crate::core::dom::shared::row_accessor_data(scope, proto, member, crate::core::html::html_input_element::cb_input_autocomplete_get, d, crate::core::dom::element::cb_reflect_str_set, d)
        }
        other => debug_assert!(false, "install_html_select_element: unexpected member {other}"),
    }
}

pub fn install_html_data_list_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "options" => getter_row(scope, proto, member, cb_datalist_options),
        other => debug_assert!(false, "install_html_data_list_element: unexpected member {other}"),
    }
}

/// A `<progress>`'s `max`: the attribute as a double "limited to only positive numbers" with fallback
/// 1.0 (HTML §4.10.13) -- an absent, unparsable, zero or negative attribute all read 1.
fn progress_max(dom: &korvus_dom::Dom, id: u32) -> f64 {
    dom.get_attribute(id, "max")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|m| m.is_finite() && *m > 0.0)
        .unwrap_or(1.0)
}

fn cb_progress_max_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let m = progress_max(&op_state.borrow().borrow::<HostDom>().0, id);
    rv.set_double(m);
}

/// `max = v`: a positive finite number is written to the attribute (its JS serialization); anything
/// else is ignored. A non-finite value is the WebIDL `double` TypeError.
fn cb_progress_max_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let Some(n) = args.get(0).to_number(scope) else { return };
    let v = n.value();
    if !v.is_finite() {
        crate::binding::protos::throw_type_error(scope, "Failed to set the 'max' property on 'HTMLProgressElement': The provided double value is non-finite.");
        return;
    }
    if v <= 0.0 {
        return;
    }
    let text = n.to_rust_string_lossy(scope);
    let el: v8::Local<v8::Value> = args.this().into();
    let name = crate::binding::protos::str_val(scope, "max");
    let val = crate::binding::protos::str_val(scope, &text);
    crate::core::dom::element::attr_call(scope, el, "setAttribute", &[name, val]);
    let _ = id;
}

/// `<progress>.value` -- a `double`, never the attribute's text (Blink's `HTMLProgressElement::value`):
/// 0 when the `value` attribute is absent or not a number or negative, else that number clamped to
/// `max`. It was the shared control-value string (Chrome 150, the Chrome oracle 2026-10-08: `0`, `1`,
/// `0.5`, `0` for -1, `max` for a larger value, where this read "", "1", "0.5", "-1", "9").
fn cb_progress_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let v = dom
        .get_attribute(id, "value")
        .and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v.min(progress_max(dom, id)))
        .unwrap_or(0.0);
    rv.set_double(v);
}

/// `value = v`: a WebIDL `double` (a non-finite one is a TypeError), written to the attribute as its
/// JS serialization -- whatever its sign; the getter does the clamping.
fn cb_progress_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(n) = args.get(0).to_number(scope) else { return };
    if !n.value().is_finite() {
        crate::binding::protos::throw_type_error(scope, "Failed to set the 'value' property on 'HTMLProgressElement': The provided double value is non-finite.");
        return;
    }
    let text = n.to_rust_string_lossy(scope);
    let el: v8::Local<v8::Value> = args.this().into();
    let name = crate::binding::protos::str_val(scope, "value");
    let val = crate::binding::protos::str_val(scope, &text);
    crate::core::dom::element::attr_call(scope, el, "setAttribute", &[name, val]);
}

/// `labels` -- a `<progress>` is labelable: the shared production (`labels_of`).
fn cb_progress_labels<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    match crate::core::html::html_input_element::labels_of(scope, args.this()) {
        Some(l) => rv.set(l),
        None => rv.set_null(),
    }
}

/// `selectedOptions` -- `[SameObject]`, a LIVE `HTMLCollection` of the select's options whose
/// selectedness is true, in tree order (HTML). It was a stub answering `null`.
fn cb_select_selected_options(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = crate::core::dom::element::el_id_of(scope, args.this()) else { return };
    if let Some(list) = crate::core::dom::collections::owned_selected_options(scope, id) {
        rv.set(list.into());
    }
}

/// `showPicker()` -- Blink's checks in order: a cross-origin frame's call is a SecurityError, a
/// disabled control's an InvalidStateError, and without transient user activation (this engine grants
/// none to script) a NotAllowedError. It was a stub that did nothing.
fn cb_select_show_picker(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::core::dom::element::el_id_of(scope, args.this()) else { return };
    if crate::core::html::browsing_context::realm_cross_origin_to_top(scope) {
        crate::binding::protos::throw_dom(scope, "Failed to execute 'showPicker' on 'HTMLSelectElement': showPicker() called from cross-origin iframe.", "SecurityError");
        return;
    }
    let disabled = crate::page::with_host_dom(scope, |d| d.actually_disabled(id)).unwrap_or(false);
    if disabled {
        crate::binding::protos::throw_dom(scope, "Failed to execute 'showPicker' on 'HTMLSelectElement': showPicker() cannot be used on immutable controls.", "InvalidStateError");
        return;
    }
    crate::binding::protos::throw_dom(scope, "Failed to execute 'showPicker' on 'HTMLSelectElement': showPicker() requires a user gesture.", "NotAllowedError");
}

/// `position`: -1 for an indeterminate bar (no `value` attribute), else the value clamped to
/// `[0, max]` over `max` -- Chrome 150: `value=3 max=20` reads 0.15.
fn cb_progress_position(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let Some(raw) = dom.get_attribute(id, "value") else {
        rv.set_double(-1.0);
        return;
    };
    let max = progress_max(dom, id);
    let value = raw.trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, max);
    rv.set_double(value / max);
}

pub fn install_html_progress_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "value" => pair_row(scope, proto, member, cb_progress_value_get, cb_progress_value_set),
        "max" => pair_row(scope, proto, member, cb_progress_max_get, cb_progress_max_set),
        "position" => getter_row(scope, proto, member, cb_progress_position),
        "labels" => getter_row(scope, proto, member, cb_progress_labels),
        other => debug_assert!(false, "install_html_progress_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLSelectElement` block of `install/generated_registry.rs`: the
    // capture's order, its kinds, its `cfg`s and its `required` counts. Four rows carry a Rust body
    // (the four the deleted JS mounted); the other twenty keep exactly the body the registry gave
    // them -- five shared reflect kits, and fifteen honest D2 stubs.
    "HTMLSelectElement" : "HTMLElement" [element, window] {
        ctor   "HTMLSelectElement"                        len 0           = stub,
        attr   "autocomplete"      : DomString cfg "autocomplete"         = install_html_select_element,
        attr   "disabled"          : Boolean   cfg "disabled"             = reflect_bool,
        attr   "form"              : Object    readonly nullable          = install_html_select_element,
        attr   "multiple"          : Boolean   cfg "multiple"             = reflect_bool,
        attr   "name"              : DomString cfg "name"                 = reflect_str,
        attr   "required"          : Boolean   cfg "required"             = reflect_bool,
        attr   "size"              : Integer   cfg "size|0|"              = reflect_ulong,
        attr   "type"              : DomString readonly                   = install_html_select_element,
        attr   "options"           : Object    readonly                   = install_html_select_element,
        attr   "length"            : Integer                              = install_html_select_element,
        attr   "selectedOptions"   : Object    readonly                   = install_html_select_element,
        attr   "selectedIndex"     : Integer                              = install_html_select_element,
        attr   "value"             : DomString                            = install_html_select_element,
        attr   "willValidate"      : Boolean   readonly                   = install_html_select_element,
        attr   "validity"          : Object    readonly                   = install_html_select_element,
        attr   "validationMessage" : DomString readonly                   = install_html_select_element,
        attr   "labels"            : Object    readonly                   = install_html_select_element,
        method "add"               : Undefined len 1 required 1           = install_html_select_element,
        method "checkValidity"     : Boolean   len 0                      = install_html_select_element,
        method "item"              : Object    len 1 required 1 nullable   = install_html_select_element,
        method "namedItem"         : Object    len 1 required 1 nullable   = install_html_select_element,
        method "remove"            : Undefined len 0                      = install_html_select_element,
        method "reportValidity"    : Boolean   len 0                      = install_html_select_element,
        method "setCustomValidity" : Undefined len 1 required 1           = install_html_select_element,
        method "showPicker"        : Undefined len 0                      = install_html_select_element,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "HTMLDataListElement" : "HTMLElement" [element, window] {
        ctor "HTMLDataListElement"           len 0 = stub,
        attr "options" : Object    readonly        = install_html_data_list_element
    }
}

webapi::interface! {
    // `value` is the one row with a body -- `<progress value>` is a `double` reflection, and the
    // shared control-value callbacks are what the deleted JS mounted for it and for `<select>`, in
    // one loop, which is why the two tables share a file. Chrome 151: `p.value = 0.25` reads back
    // 0.25 AND writes `value="0.25"`.
    "HTMLProgressElement" : "HTMLElement" [element, window] {
        ctor "HTMLProgressElement"            len 0 = stub,
        attr "value"    : Double                    = install_html_progress_element,
        attr "max"      : Double                    = install_html_progress_element,
        attr "position" : Double   readonly         = install_html_progress_element,
        attr "labels"   : Object   readonly         = install_html_progress_element
    }
}

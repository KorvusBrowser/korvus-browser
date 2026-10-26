//! `core/html/element_internals.rs` -- `ElementInternals` and `CustomStateSet`, the Rust
//! form of what `dom/09l-element-internals.js` did behind twenty bridge slots and eleven native masks.
//!
//! An internals object is minted NATIVELY by `HTMLElement.attachInternals` ([`attach_internals`]): a
//! plain object on `ElementInternals.prototype` whose state lives in `v8::Private` slots -- the host
//! element, the ARIA default-semantics map, the `CustomStateSet` -- so the instance owns no key a page
//! can enumerate. The three mechanisms the JS header named are kept exactly:
//!
//!   1. DEFAULT SEMANTICS ARE NOT THE ELEMENT'S ATTRIBUTES. `internals.ariaLabel` is a per-internals
//!      slot: null until written, a string once written, never the host's own `aria-label`. That is why
//!      the 48 `aria*`/`role` rows name [`install_element_internals`] and NOT the `reflect_nullable_str`
//!      kit the registry gave them -- the kit reads a content attribute off the receiver, and the
//!      receiver here is not an element.
//!   2. FORM ASSOCIATION IS A PROPERTY OF THE DEFINITION. The seven form members and the two
//!      validity methods throw `NotSupportedError` unless the host's definition declares
//!      `static formAssociated = true` ([`form_associated`]).
//!   3. ARITY BEFORE THE GATE. `setFormValue()` / `setValidity()` with no argument are a WebIDL
//!      TypeError before the body runs, so a non-form-associated element reports the TypeError, not the
//!      NotSupportedError.
//!
//! `CustomStateSet` is a setlike over a `v8::Array` held in a private: `size`, `add`, `clear`,
//! `delete`, `has`, `entries`/`keys`/`values`/`forEach`, and `@@iterator === values`. Its iterators are
//! their OWN kind -- `Object.prototype.toString.call(set.keys())` is "[object CustomStateSet Iterator]",
//! which the capture records three times -- built over %IteratorPrototype% with a native `next`.
//!
//! The value gate is the bundle's oracle (`ElementInternals` 120 cases, `CustomStateSet` 19), run by
//! `tests/behavior_runner_async.rs` and `tests/dom_spine_live.rs`.

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{
    brand_shell_instance, get_prop, global_get, require_args, str_val, throw_dom,
    throw_type_error, wrap_cached,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{el_id_of, priv_set};
use crate::core::dom::shared::{row_getter, row_method, row_method_data};
use crate::core::html::custom_element_registry::get as ce_get;
use crate::install::kits::define_value;
use crate::install::MemberDef;

/// The host element (its wrapper) an internals object was attached to.
const EI_HOST: &str = "korvus_ei_host";
/// The ARIA default-semantics slot map (a null-prototype object).
const EI_ARIA: &str = "korvus_ei_aria";
/// The `CustomStateSet`, minted on first read and kept (`internals.states === internals.states`).
const EI_STATES: &str = "korvus_ei_states";
/// Set on the HOST once `attachInternals()` has run: a second call is a NotSupportedError.
const EI_ATTACHED: &str = "korvus_ei_attached";
/// The set's items, a `v8::Array` of strings in insertion order.
const CSS_ITEMS: &str = "korvus_css_items";

fn proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let c = global_get(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// A branded empty instance of `iface` -- what `MK(Iface.prototype)` minted in JS (`ValidityState`).
fn branded_instance<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = proto_of(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    Some(o)
}

// ── attachInternals ──────────────────────────────────────────────────────────────────────────────

/// `HTMLElement.attachInternals()` for `host`: the NotSupportedError branches (a non-custom element;
/// a second attach), then a fresh internals object over `host`. `None` means the exception is pending.
pub(crate) fn attach_internals<'s>(scope: &mut v8::PinScope<'s, '_>, host: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let Some(id) = el_id_of(scope, host) else {
        throw_type_error(scope, "Illegal invocation");
        return None;
    };
    let tag = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.tag_name(id).map(|t| t.to_ascii_lowercase()).unwrap_or_default()
    };
    let name_v = str_val(scope, &tag);
    let defined = ce_get(scope, name_v).is_some();
    if !defined {
        throw_dom(scope, "Failed to execute 'attachInternals' on 'HTMLElement': Unable to attach ElementInternals to non-custom elements.", "NotSupportedError");
        return None;
    }
    if priv_get(scope, host, EI_ATTACHED).map(|v| v.is_true()).unwrap_or(false) {
        throw_dom(scope, "Failed to execute 'attachInternals' on 'HTMLElement': ElementInternals for the specified element was already attached.", "NotSupportedError");
        return None;
    }
    let Some(proto) = proto_of(scope, "ElementInternals") else {
        throw_dom(scope, "Failed to execute 'attachInternals' on 'HTMLElement': ElementInternals is not available.", "NotSupportedError");
        return None;
    };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    priv_set(scope, o, EI_HOST, host.into());
    let aria = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    aria.set_prototype(scope, null);
    priv_set(scope, o, EI_ARIA, aria.into());
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, host, EI_ATTACHED, t);
    // TELL THE ARENA. A form-associated custom element is a listed, labelable, submittable element
    // and a constraint-validation candidate, and every one of those questions is answered in
    // `korvus-dom::forms` by matching on a TAG NAME -- which says nothing here, the page picked it.
    // This is the one moment `static formAssociated === true` is known to hold for THIS element, so
    // it is where the flag is recorded; `label.control`, `:disabled`, `:valid`/`:invalid`,
    // `willValidate`, `validity` and a form's own validity all read it from there afterwards.
    if form_associated(scope, host) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<HostDom>().0.set_form_associated_custom(id);
    }
    Some(o)
}

/// The receiver as an internals object, or a TypeError "Illegal invocation".
fn internals_host<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    match priv_get(scope, this, EI_HOST).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(h) => Some(h),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The definition's `formAssociated` flag, looked up through the registry by the host's local name.
/// `pub(crate)` because "is this element form-associated?" is asked from outside too:
/// `html_label_element.rs::is_labelable` needs it, a form-associated custom element being labelable.
pub(crate) fn form_associated(scope: &mut v8::PinScope, host: v8::Local<v8::Object>) -> bool {
    let Some(id) = el_id_of(scope, host) else { return false };
    let tag = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.tag_name(id).map(|t| t.to_ascii_lowercase()).unwrap_or_default()
    };
    let name_v = str_val(scope, &tag);
    let Some(def) = ce_get(scope, name_v).map(|f| -> v8::Local<v8::Object> { f.into() }) else { return false };
    get_prop(scope, def, "formAssociated").map(|v| v.is_true()).unwrap_or(false)
}

/// The gate every form-association member runs: the host, when its definition is form-associated;
/// otherwise Chrome's NotSupportedError (`member` ends in `()` for a method), and `None`.
fn gated_host<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>, member: &str) -> Option<v8::Local<'s, v8::Object>> {
    let host = internals_host(scope, this)?;
    if form_associated(scope, host) {
        return Some(host);
    }
    let what = match member.strip_suffix("()") {
        Some(m) => format!("execute '{m}'"),
        None => format!("read the '{member}' property"),
    };
    throw_dom(scope, &format!("Failed to {what} on 'ElementInternals': The target element is not a form-associated custom element."), "NotSupportedError");
    None
}

/// `form` -- the host's FORM OWNER, which is the `form=` content attribute's target when it has one
/// and the nearest ancestor `<form>` otherwise. It was `host.closest("form")`, the second half only,
/// so `<custom-input form="custom-form">` outside the form reported null (WPT
/// `custom-elements/form-associated/ElementInternals-form.html`). `korvus_dom::forms::form_owner`
/// is the one production for the whole rule and every other form-associated element already uses it.
fn cb_ei_form<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = gated_host(scope, args.this(), "form") else { return };
    match crate::core::html::html_input_element::form_owner_of(scope, host) {
        Some(f) => rv.set(f),
        None => rv.set_null(),
    }
}
/// `willValidate` -- the FOUR barring conditions, not one. It read the host's own `disabled`
/// attribute and nothing else; the `readonly` attribute, a `<datalist>` ancestor and a disabled
/// ancestor `<fieldset>` bar it too. All four live in `korvus_dom::forms::will_validate`, which every
/// built-in control already asks, and which now knows what a form-associated custom element is.
fn cb_ei_will_validate<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = gated_host(scope, args.this(), "willValidate") else { return };
    let Some(id) = el_id_of(scope, host) else { rv.set_bool(false); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ok = { op_state.borrow().borrow::<HostDom>().0.will_validate(id) };
    rv.set_bool(ok);
}
/// `validity` -- the HOST's ValidityState, the same `[SameObject]` view `input.validity` hands back,
/// with the host in its owner slot. It used to mint a bare `branded_instance("ValidityState")` with
/// NO owner, and `cb_validity_flag` answers `false` to every flag of an ownerless one -- including
/// `valid`, so a brand-new custom control reported itself INVALID.
fn cb_ei_validity<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = gated_host(scope, args.this(), "validity") else { return };
    match crate::core::html::html_input_element::validity_object_of(scope, host) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}
/// `validationMessage` -- the message `setValidity()` stored, and "" when the element is barred from
/// constraint validation or satisfies its constraints. It was the empty string unconditionally.
fn cb_ei_validation_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = gated_host(scope, args.this(), "validationMessage") else { return };
    let msg = match el_id_of(scope, host) {
        Some(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let m = { op_state.borrow().borrow::<HostDom>().0.face_validation_message(id) };
            m
        }
        None => String::new(),
    };
    let e = str_val(scope, &msg);
    rv.set(e);
}
/// `labels` -- THE `labels` production, asked about the HOST. It is the same question `input.labels`
/// asks (`html_input_element.rs::labels_of`), so it is the same function: a label association is
/// decided by `label.control`, whichever end you start from.
///
/// The copy that was here queried `label[for="<id>"]` on the document and returned an EMPTY list for
/// an element with no id -- which threw away every IMPLICIT association, the
/// `<label><span><my-control>` nesting WPT
/// `custom-elements/form-associated/ElementInternals-labels.html` opens with, and scoped the search
/// to the main document rather than to the element's own node tree.
fn cb_ei_labels<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = gated_host(scope, args.this(), "labels") else { return };
    match crate::core::html::html_input_element::labels_of(scope, host) {
        Some(l) => rv.set(l),
        None => rv.set_null(),
    }
}
/// `checkValidity()` / `reportValidity()` -- HTML's check-validity steps on the host, which is the
/// same production `input.checkValidity()` runs (`html_input_element.rs::check_validity_of`): fire
/// `invalid` and answer false when the host is a candidate that does not satisfy its constraints.
/// Both were a hardcoded `rv.set_bool(true)`, so a control that had just called
/// `setValidity({customError: true})` still reported itself valid and fired no `invalid` event.
/// `reportValidity` differs only in also showing the UA's validation bubble, and there is no widget
/// layer here -- the same callback, exactly as for `<input>`.
fn cb_ei_check_validity<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = if crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "reportValidity" { "reportValidity()" } else { "checkValidity()" };
    let Some(host) = gated_host(scope, args.this(), member) else { return };
    let ok = crate::core::html::html_input_element::check_validity_of(scope, host);
    rv.set_bool(ok);
}
fn cb_ei_set_form_value<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "ElementInternals", "setFormValue") { return; }
    let _ = gated_host(scope, args.this(), "setFormValue()");
}
/// `setValidity(flags, message, anchor)` -- and it STORES, which it did not: the body was
/// `require_args` then `let _ = gated_host(...)`, so every call was accepted and discarded and the
/// validity it was setting could not be read back by anything.
///
/// The flags go to the arena (`korvus_dom::forms`), not to a slot on the internals object, because
/// four different readers need them: `ElementInternals.validity`, `ElementInternals.checkValidity()`,
/// the owner `<form>`'s validity, and the CASCADE's `:valid`/`:invalid` -- and the selector engine
/// sees only a `Dom`. That is the same reason checkedness lives there.
fn cb_ei_set_validity<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    // The recorded browser's arity, checked here: the bundle's row carries `required 0` for this one.
    if !require_args(scope, &args, 1, "ElementInternals", "setValidity") { return; }
    let Some(host) = gated_host(scope, args.this(), "setValidity()") else { return };
    // `ValidityStateFlags` is a dictionary: every member is optional and defaults to false, and a
    // non-object `flags` (including undefined) converts to one with nothing present -- so the
    // no-argument reading of it and `setValidity({})` are the same thing, and both CLEAR the state.
    let flags = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let mut flag = |name: &str| -> bool {
        flags.map(|o| get_prop(scope, o, name).map(|x| x.boolean_value(scope)).unwrap_or(false)).unwrap_or(false)
    };
    let v = korvus_dom::forms::Validity {
        value_missing: flag("valueMissing"),
        type_mismatch: flag("typeMismatch"),
        pattern_mismatch: flag("patternMismatch"),
        too_long: flag("tooLong"),
        too_short: flag("tooShort"),
        range_underflow: flag("rangeUnderflow"),
        range_overflow: flag("rangeOverflow"),
        step_mismatch: flag("stepMismatch"),
        bad_input: flag("badInput"),
        custom_error: flag("customError"),
    };
    // "If flags contains one or more true values and message is not given or is the empty string,
    // then throw a TypeError."
    let message = if args.length() >= 2 && !args.get(1).is_undefined() {
        args.get(1).to_rust_string_lossy(scope)
    } else {
        String::new()
    };
    if !v.valid() && message.is_empty() {
        throw_type_error(scope, "Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true.");
        return;
    }
    // `anchor`, when given, must be an HTMLElement (TypeError) and a shadow-including descendant of
    // the host, or the host itself (NotFoundError).
    if args.length() >= 3 && !args.get(2).is_null_or_undefined() {
        let anchor = args.get(2);
        let Some(aid) = v8::Local::<v8::Object>::try_from(anchor).ok().and_then(|o| el_id_of(scope, o)) else {
            throw_type_error(scope, "Failed to execute 'setValidity' on 'ElementInternals': parameter 3 is not of type 'HTMLElement'.");
            return;
        };
        let is_html = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.borrow::<HostDom>().0.is_html_ns(aid)
        };
        if !is_html {
            throw_type_error(scope, "Failed to execute 'setValidity' on 'ElementInternals': parameter 3 is not of type 'HTMLElement'.");
            return;
        }
        let Some(hid) = el_id_of(scope, host) else { return };
        if !shadow_including_ancestor(scope, hid, aid) {
            throw_dom(scope, "Failed to execute 'setValidity' on 'ElementInternals': The validation anchor is not a shadow-including descendant of the target element.", "NotFoundError");
            return;
        }
    }
    let Some(id) = el_id_of(scope, host) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_face_validity(id, v, &message);
}

/// Is `host` a shadow-including inclusive ancestor of `node`? Walks parents, crossing every shadow
/// boundary through the root's host -- the "shadow-including descendant" `setValidity`'s anchor must
/// satisfy, and the reason a `<div>` in the host's own shadow tree is a legal anchor.
fn shadow_including_ancestor(scope: &mut v8::PinScope, host: u32, node: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n == host {
            return true;
        }
        cur = dom.parent(n).or_else(|| dom.shadow_host_of(dom.root_of(n)));
    }
    false
}
/// `shadowRoot` is the host's shadow root whatever its mode (internals see a closed one too).
fn cb_ei_shadow_root<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(host) = internals_host(scope, args.this()) else { return };
    let Some(id) = el_id_of(scope, host) else { rv.set_null(); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = { op_state.borrow().borrow::<HostDom>().0.shadow_root_of(id) };
    match root {
        Some(r) => { let o = wrap_cached(scope, &op_state, r); rv.set(o.into()); }
        None => rv.set_null(),
    }
}
fn cb_ei_states<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(_host) = internals_host(scope, this) else { return };
    if let Some(s) = priv_get(scope, this, EI_STATES).filter(|v| v.is_object()) {
        rv.set(s);
        return;
    }
    match mint_custom_state_set(scope) {
        Some(s) => { priv_set(scope, this, EI_STATES, s.into()); rv.set(s.into()); }
        None => rv.set_null(),
    }
}

/// The ARIA default-semantics slot: `null` until written; a write stores the stringified value, or the
/// value itself for the element-reference members (`aria*Element`, `aria*Elements`); null/undefined
/// clears it.
fn cb_ei_aria_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(_host) = internals_host(scope, this) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(map) = priv_get(scope, this, EI_ARIA).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { rv.set_null(); return };
    match get_prop(scope, map, &name) {
        Some(v) if !v.is_undefined() => rv.set(v),
        _ => rv.set_null(),
    }
}
fn cb_ei_aria_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let Some(_host) = internals_host(scope, this) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(map) = priv_get(scope, this, EI_ARIA).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let v = args.get(0);
    let stored: v8::Local<v8::Value> = if v.is_null_or_undefined() {
        v8::null(scope).into()
    } else if name.ends_with("Elements") {
        // `attribute FrozenArray<Element>? aria…Elements`: Blink keeps a NEW frozen array of the given elements on
        // every set (`ElementInternals::SetElementArrayAttribute`) and hands that one back until the next -- this kept
        // the page's own array, writable and still the page's (C2, `install::frozen_array`).
        match crate::core::dom::aria_reflection::given_elements(scope, v) {
            Some(items) => crate::install::frozen_array::frozen(scope, &items).into(),
            None => v8::null(scope).into(),
        }
    } else if name.ends_with("Element") {
        v
    } else {
        let s = v.to_rust_string_lossy(scope);
        str_val(scope, &s)
    };
    if let Some(k) = v8::String::new(scope, &name) {
        map.set(scope, k.into(), stored);
    }
}

/// The `ElementInternals` rows: the seven gated form members, `states`, `shadowRoot`, the two validity
/// methods, the two form-value methods, and the 56 ARIA default-semantics slots.
pub fn install_element_internals(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "form" => row_getter(scope, proto, member, cb_ei_form),
        "willValidate" => row_getter(scope, proto, member, cb_ei_will_validate),
        "validity" => row_getter(scope, proto, member, cb_ei_validity),
        "validationMessage" => row_getter(scope, proto, member, cb_ei_validation_message),
        "labels" => row_getter(scope, proto, member, cb_ei_labels),
        "states" => row_getter(scope, proto, member, cb_ei_states),
        "shadowRoot" => row_getter(scope, proto, member, cb_ei_shadow_root),
        // ONE body for both, with the row's own name as `data` so the NotSupportedError message
        // still names the member that was called.
        "checkValidity" | "reportValidity" => {
            let d = str_val(scope, member.name);
            row_method_data(scope, proto, member, cb_ei_check_validity, d)
        }
        "setFormValue" => row_method(scope, proto, member, cb_ei_set_form_value),
        "setValidity" => row_method(scope, proto, member, cb_ei_set_validity),
        n if n == "role" || n.starts_with("aria") => {
            let d = str_val(scope, n);
            let g = crate::binding::protos::build_fn_data(scope, cb_ei_aria_get, &format!("get {n}"), 0, d);
            let s = crate::binding::protos::build_fn_data(scope, cb_ei_aria_set, &format!("set {n}"), 1, d);
            crate::install::kits::define_accessor(scope, proto, member, g, Some(s));
        }
        other => debug_assert!(false, "install_element_internals: unexpected member {other}"),
    }
}

// ── CustomStateSet ───────────────────────────────────────────────────────────────────────────────

/// A fresh `CustomStateSet` over an empty item array.
fn mint_custom_state_set<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let o = branded_instance(scope, "CustomStateSet")?;
    let items = v8::Array::new(scope, 0);
    priv_set(scope, o, CSS_ITEMS, items.into());
    Some(o)
}

fn css_items<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    match priv_get(scope, this, CSS_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => Some(a),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn css_index_of(scope: &mut v8::PinScope, items: v8::Local<v8::Array>, want: &str) -> Option<u32> {
    for i in 0..items.length() {
        if let Some(v) = items.get_index(scope, i) {
            if v.to_rust_string_lossy(scope) == want {
                return Some(i);
            }
        }
    }
    None
}

fn css_snapshot<'s>(scope: &mut v8::PinScope<'s, '_>, items: v8::Local<'s, v8::Array>) -> Vec<String> {
    (0..items.length()).filter_map(|i| items.get_index(scope, i)).map(|v| v.to_rust_string_lossy(scope)).collect()
}

fn css_replace(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, vals: &[String]) {
    let arr = v8::Array::new(scope, vals.len() as i32);
    for (i, s) in vals.iter().enumerate() {
        let v = str_val(scope, s);
        arr.set_index(scope, i as u32, v);
    }
    priv_set(scope, this, CSS_ITEMS, arr.into());
}

fn cb_css_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = css_items(scope, args.this()) else { return };
    rv.set_uint32(items.length());
}
/// Plain WebIDL DOMString coercion and nothing more: the capture records `add("busy")` succeeding, so
/// the spec's `<dashed-ident>` restriction is not what this browser does.
fn cb_css_add<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "CustomStateSet", "add") { return; }
    let this = args.this();
    let Some(items) = css_items(scope, this) else { return };
    let s = args.get(0).to_rust_string_lossy(scope);
    if css_index_of(scope, items, &s).is_none() {
        let mut vals = css_snapshot(scope, items);
        vals.push(s);
        css_replace(scope, this, &vals);
    }
}
fn cb_css_clear<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let Some(_items) = css_items(scope, this) else { return };
    css_replace(scope, this, &[]);
}
fn cb_css_delete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "CustomStateSet", "delete") { return; }
    let this = args.this();
    let Some(items) = css_items(scope, this) else { return };
    let s = args.get(0).to_rust_string_lossy(scope);
    match css_index_of(scope, items, &s) {
        Some(i) => {
            let mut vals = css_snapshot(scope, items);
            vals.remove(i as usize);
            css_replace(scope, this, &vals);
            rv.set_bool(true);
        }
        None => rv.set_bool(false),
    }
}
fn cb_css_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "CustomStateSet", "has") { return; }
    let Some(items) = css_items(scope, args.this()) else { return };
    let s = args.get(0).to_rust_string_lossy(scope);
    rv.set_bool(css_index_of(scope, items, &s).is_some());
}
fn cb_css_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(items) = css_items(scope, args.this()) else { return };
    let states: Vec<v8::Local<'s, v8::Value>> = css_snapshot(scope, items).iter().map(|s| str_val(scope, s)).collect();
    crate::core::dom::webidl_iter::for_each(scope, &args, "CustomStateSet", |_, i| states.get(i).map(|v| (*v, *v)));
}

/// A setlike iterator over a snapshot of the set: `kind` 0 keys, 1 values, 2 entries (`[v, v]`) -- the
/// one WebIDL iterator (`webidl_iter::make_iter`, a setlike's: `%SetIteratorPrototype%` beneath the
/// "CustomStateSet Iterator" prototype). It had a private copy of that machinery, which chained to
/// `%IteratorPrototype%` and answered `{value, done}` (Chrome 150.0.7871.46: `%SetIteratorPrototype%`,
/// `{done, value}`).
fn css_make_iter<'s>(scope: &mut v8::PinScope<'s, '_>, items: v8::Local<'s, v8::Array>, kind: u32) -> Option<v8::Local<'s, v8::Object>> {
    let snap = css_snapshot(scope, items);
    let values: Vec<v8::Local<'s, v8::Value>> = snap
        .iter()
        .map(|s| {
            let v = str_val(scope, s);
            if kind == 2 {
                crate::core::dom::webidl_iter::pair(scope, v, v)
            } else {
                v
            }
        })
        .collect();
    crate::core::dom::webidl_iter::make_iter(scope, "CustomStateSet", crate::core::dom::webidl_iter::IterKind::Set, &values)
}

fn cb_css_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = css_items(scope, args.this()) else { return };
    if let Some(it) = css_make_iter(scope, items, 0) { rv.set(it.into()); }
}
fn cb_css_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = css_items(scope, args.this()) else { return };
    if let Some(it) = css_make_iter(scope, items, 1) { rv.set(it.into()); }
}
fn cb_css_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = css_items(scope, args.this()) else { return };
    if let Some(it) = css_make_iter(scope, items, 2) { rv.set(it.into()); }
}

/// The `CustomStateSet` rows. `@@iterator` is the SAME function object as `values` (every setlike),
/// so it is mounted after `values` in table order and reads it back off the prototype.
pub fn install_custom_state_set(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => row_getter(scope, proto, member, cb_css_size),
        "add" => row_method(scope, proto, member, cb_css_add),
        "clear" => row_method(scope, proto, member, cb_css_clear),
        "delete" => row_method(scope, proto, member, cb_css_delete),
        "has" => row_method(scope, proto, member, cb_css_has),
        "forEach" => row_method(scope, proto, member, cb_css_for_each),
        "keys" => row_method(scope, proto, member, cb_css_keys),
        "values" => row_method(scope, proto, member, cb_css_values),
        "entries" => row_method(scope, proto, member, cb_css_entries),
        "@@iterator" => {
            let Some(values) = get_prop(scope, proto, "values") else { return };
            define_value(scope, proto, member, values, true);
        }
        other => debug_assert!(false, "install_custom_state_set: unexpected member {other}"),
    }
}

// The two tables. Rows are the capture's (`generated_registry.rs` at the cutover), verbatim; the
// `@@iterator` row is the setlike's value iterator, which the registry does not carry (it skips symbols).
webapi::interface! {
    "ElementInternals" [interface, window] {
        ctor "ElementInternals" len 0 = stub,
        attr "form" : Object readonly nullable = install_element_internals,
        attr "willValidate" : Object readonly = install_element_internals,
        attr "validity" : Object readonly = install_element_internals,
        attr "validationMessage" : Object readonly = install_element_internals,
        attr "labels" : Object readonly = install_element_internals,
        attr "states" : Object readonly = install_element_internals,
        attr "shadowRoot" : Object readonly nullable = install_element_internals,
        attr "role" : Object nullable cfg "role" = install_element_internals,
        attr "ariaAtomic" : Object nullable cfg "aria-atomic" = install_element_internals,
        attr "ariaAutoComplete" : Object nullable cfg "aria-autocomplete" = install_element_internals,
        attr "ariaBusy" : Object nullable cfg "aria-busy" = install_element_internals,
        attr "ariaBrailleLabel" : Object nullable cfg "aria-braillelabel" = install_element_internals,
        attr "ariaBrailleRoleDescription" : Object nullable cfg "aria-brailleroledescription" = install_element_internals,
        attr "ariaChecked" : Object nullable cfg "aria-checked" = install_element_internals,
        attr "ariaColCount" : Object nullable cfg "aria-colcount" = install_element_internals,
        attr "ariaColIndex" : Object nullable cfg "aria-colindex" = install_element_internals,
        attr "ariaColSpan" : Object nullable cfg "aria-colspan" = install_element_internals,
        attr "ariaCurrent" : Object nullable cfg "aria-current" = install_element_internals,
        attr "ariaDescription" : Object nullable cfg "aria-description" = install_element_internals,
        attr "ariaDisabled" : Object nullable cfg "aria-disabled" = install_element_internals,
        attr "ariaExpanded" : Object nullable cfg "aria-expanded" = install_element_internals,
        attr "ariaHasPopup" : Object nullable cfg "aria-haspopup" = install_element_internals,
        attr "ariaHidden" : Object nullable cfg "aria-hidden" = install_element_internals,
        attr "ariaInvalid" : Object nullable cfg "aria-invalid" = install_element_internals,
        attr "ariaKeyShortcuts" : Object nullable cfg "aria-keyshortcuts" = install_element_internals,
        attr "ariaLabel" : Object nullable cfg "aria-label" = install_element_internals,
        attr "ariaLevel" : Object nullable cfg "aria-level" = install_element_internals,
        attr "ariaLive" : Object nullable cfg "aria-live" = install_element_internals,
        attr "ariaModal" : Object nullable cfg "aria-modal" = install_element_internals,
        attr "ariaMultiLine" : Object nullable cfg "aria-multiline" = install_element_internals,
        attr "ariaMultiSelectable" : Object nullable cfg "aria-multiselectable" = install_element_internals,
        attr "ariaOrientation" : Object nullable cfg "aria-orientation" = install_element_internals,
        attr "ariaPlaceholder" : Object nullable cfg "aria-placeholder" = install_element_internals,
        attr "ariaPosInSet" : Object nullable cfg "aria-posinset" = install_element_internals,
        attr "ariaPressed" : Object nullable cfg "aria-pressed" = install_element_internals,
        attr "ariaReadOnly" : Object nullable cfg "aria-readonly" = install_element_internals,
        attr "ariaRelevant" : Object nullable cfg "aria-relevant" = install_element_internals,
        attr "ariaRequired" : Object nullable cfg "aria-required" = install_element_internals,
        attr "ariaRoleDescription" : Object nullable cfg "aria-roledescription" = install_element_internals,
        attr "ariaRowCount" : Object nullable cfg "aria-rowcount" = install_element_internals,
        attr "ariaRowIndex" : Object nullable cfg "aria-rowindex" = install_element_internals,
        attr "ariaRowSpan" : Object nullable cfg "aria-rowspan" = install_element_internals,
        attr "ariaSelected" : Object nullable cfg "aria-selected" = install_element_internals,
        attr "ariaSetSize" : Object nullable cfg "aria-setsize" = install_element_internals,
        attr "ariaSort" : Object nullable cfg "aria-sort" = install_element_internals,
        attr "ariaValueMax" : Object nullable cfg "aria-valuemax" = install_element_internals,
        attr "ariaValueMin" : Object nullable cfg "aria-valuemin" = install_element_internals,
        attr "ariaValueNow" : Object nullable cfg "aria-valuenow" = install_element_internals,
        attr "ariaValueText" : Object nullable cfg "aria-valuetext" = install_element_internals,
        method "checkValidity" : Object len 0 = install_element_internals,
        method "reportValidity" : Object len 0 = install_element_internals,
        method "setFormValue" : Object len 1 required 1 = install_element_internals,
        method "setValidity" : Object len 1 = install_element_internals,
        attr "ariaColIndexText" : Object nullable cfg "aria-colindextext" = install_element_internals,
        attr "ariaRowIndexText" : Object nullable cfg "aria-rowindextext" = install_element_internals,
        attr "ariaActionsElements" : Object = install_element_internals,
        attr "ariaActiveDescendantElement" : Object nullable = install_element_internals,
        attr "ariaControlsElements" : Object nullable = install_element_internals,
        attr "ariaDescribedByElements" : Object nullable = install_element_internals,
        attr "ariaDetailsElements" : Object nullable = install_element_internals,
        attr "ariaErrorMessageElements" : Object nullable = install_element_internals,
        attr "ariaFlowToElements" : Object nullable = install_element_internals,
        attr "ariaLabelledByElements" : Object nullable = install_element_internals
    }
}

webapi::interface! {
    "CustomStateSet" [interface, window] {
        ctor "CustomStateSet" len 0 = stub,
        attr "size" : Object readonly = install_custom_state_set,
        method "add" : Object len 1 = install_custom_state_set,
        method "clear" : Object len 0 = install_custom_state_set,
        method "delete" : Object len 1 = install_custom_state_set,
        method "entries" : Object len 0 = install_custom_state_set,
        method "forEach" : Object len 1 = install_custom_state_set,
        method "has" : Object len 1 = install_custom_state_set,
        method "keys" : Object len 0 = install_custom_state_set,
        method "values" : Object len 0 = install_custom_state_set,
        method "@@iterator" : Object len 0 hidden = install_custom_state_set
    }
}

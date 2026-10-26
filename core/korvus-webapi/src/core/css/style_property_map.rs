//! `core/css/style_property_map.rs` -- `StylePropertyMapReadOnly`, `CSSStyleValue` and
//! `Element.computedStyleMap()`, the Rust form of ~120 lines of
//! `dom/09-postshells.js`.
//!
//! WHAT IT IS. `el.computedStyleMap()` is the Typed OM view of the SAME resolved values
//! `getComputedStyle(el)` returns, so the two cannot disagree -- this reads through
//! `globalThis.getComputedStyle`, which is the one place the cascade is resolved
//! (`core/css/css_style_declaration.rs`, over korvus-page). Before this lane the twelve members of the
//! two interfaces answered `undefined` for prototypes nothing ever wore, and then answered from JS
//! behind five `op_nativize` masks.
//!
//! CHROME 150, recorded over loopback 2026-09-11:
//!
//! * a DETACHED element's map is EMPTY: `size` 0, `has("x")` false, `get("x")` undefined,
//!   `getAll("x")` `[]` -- which is exactly the bundle's ten recorded cases, all of which use
//!   `document.createElement("div").computedStyleMap()`;
//! * an ATTACHED element's map has one entry per resolved property, `keys()` in cascade order, and
//!   `get("color")` is a `CSSStyleValue` whose `String()` is the computed value;
//! * `values()` yields ARRAYS (`[[CSSStyleValue]]`), and `entries()` yields `[name, [CSSStyleValue]]`
//!   -- a `StylePropertyMapReadOnly` is `maplike<DOMString, sequence<CSSStyleValue>>`;
//! * the iterators are `[object StylePropertyMapReadOnly Iterator]` and `@@iterator === entries`;
//! * every member of both prototypes throws `Illegal invocation` for a foreign receiver.
//!
//! TWO RESIDUALS, stated rather than papered over. Both are what shipped before this lane; neither is
//! made worse, and closing either needs a fact this engine does not have.
//!
//! 1. `has("zzz")` for a name that is not a CSS property at all is `TypeError: Failed to execute 'has'
//!    on 'StylePropertyMapReadOnly': Invalid propertyName: zzz` in Chrome, on an attached element AND
//!    on a detached one. Answering that needs the set of valid CSS property names, and this engine has
//!    no such registry -- korvus-page resolves a cascade, it does not enumerate the language. Writing
//!    the list here would be a fabricated rule, so `has` answers `false` for an unknown name, which is
//!    what the JS answered and what the bundle's `has("x") == false` case records (`x` IS a real CSS
//!    property, so that case does not discriminate).
//! 2. `CSSStyleValue.parse` / `parseAll` throw for every input. Chrome parses a real property/value
//!    pair (`parse("color", "red")` is a `CSSStyleValue`) and throws `The value provided ('x') could
//!    not be parsed as a 'x'` otherwise. Without the property registry the two cases cannot be told
//!    apart; the bundle records only the throwing one (`parse("x", "x")`), and the message is now
//!    Chrome's rather than the invented "Invalid propertyName: x" the JS threw.

use deno_core::v8;

use crate::binding::protos::{
    brand_shell_instance, build_fn_data, call_method, get_prop, global_get, require_args, str_val, throw_type_error,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{el_id_of, priv_set};
use crate::core::dom::shared::{row_getter, row_method, row_method_recv_first};
use crate::core::dom::webidl_iter::{make_iter, pair, IterKind};
use crate::install::kits::define_value;
use crate::install::MemberDef;

/// The `CSSStyleDeclaration` a map reads through. Its presence IS the brand.
const SPM_DECL: &str = "korvus_spm_decl";
/// A `CSSStyleValue`'s text. Its presence IS the brand.
const CSV_TEXT: &str = "korvus_csv_text";

fn proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let c = global_get(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

// ── CSSStyleValue ────────────────────────────────────────────────────────────────────────────────

/// A `CSSStyleValue` over `text`. No own properties: the text is a `v8::Private`, so
/// `Object.getOwnPropertyNames(v)` is `[]` as it is in Chrome.
fn mint_value<'s>(scope: &mut v8::PinScope<'s, '_>, text: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = proto_of(scope, "CSSStyleValue")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    let t = str_val(scope, text);
    priv_set(scope, o, CSV_TEXT, t);
    Some(o)
}

fn cb_csv_to_string<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if let Some(v) = priv_get(scope, args.this(), CSV_TEXT) {
        rv.set(v);
        return;
    }
    // A Typed OM value (`CSSUnitValue`, `CSSMathSum`, a transform, ...) serializes itself.
    match crate::core::css::typed_om::style_value_text(scope, args.this()) {
        Some(t) => rv.set(str_val(scope, &t)),
        None => throw_type_error(scope, "Illegal invocation"),
    }
}

/// The value a map hands back for `prop`: its Typed OM object where Blink reifies one (a unit value,
/// a keyword, a math value, a custom property's unparsed value), else a `CSSStyleValue` over the text.
fn style_value<'s>(scope: &mut v8::PinScope<'s, '_>, prop: &str, text: &str) -> Option<v8::Local<'s, v8::Object>> {
    match crate::core::css::typed_om::reify(scope, prop, text) {
        Some(o) => Some(o),
        None => mint_value(scope, text),
    }
}

/// A property name a map accepts: a known property (longhand or shorthand) or a custom property.
fn known_property(name: &str) -> bool {
    name.starts_with("--") || page::style::property::PropertyId::from_name(name).is_some() || page::style::shorthand::is_shorthand(&name.to_ascii_lowercase())
}

/// `CSSStyleValue.parse(property, cssText)` / `parseAll(...)`. The message is Chrome's; see residual 2
/// in this module's header for why every input reaches it.
fn cb_csv_parse<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if !require_args(scope, &args, 2, "CSSStyleValue", &member) {
        return;
    }
    let prop = args.get(0).to_rust_string_lossy(scope);
    let val = args.get(1).to_rust_string_lossy(scope);
    let lower = prop.to_ascii_lowercase();
    let name = if prop.starts_with("--") { prop.as_str() } else { lower.as_str() };
    if !known_property(name) || !page::style::supports::supports_declaration(name, &val) {
        throw_type_error(
            scope,
            &format!("Failed to execute '{member}' on 'CSSStyleValue': The value provided ('{val}') could not be parsed as a '{prop}'."),
        );
        return;
    }
    let Some(v) = style_value(scope, name, val.trim()) else { return };
    if member == "parseAll" {
        let a = v8::Array::new_with_elements(scope, &[v.into()]);
        rv.set(a.into());
    } else {
        rv.set(v.into());
    }
}

pub fn install_css_style_value(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toString" => row_method_recv_first(scope, target, member, cb_csv_to_string),
        "parse" | "parseAll" => {
            let d = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_csv_parse, member.name, member.length as i32, d);
            define_value(scope, target, member, f.into(), true);
        }
        other => debug_assert!(false, "install_css_style_value: unexpected member {other}"),
    }
}

// ── StylePropertyMapReadOnly ─────────────────────────────────────────────────────────────────────

/// The receiver as a map: its backing `CSSStyleDeclaration`, or a pending `Illegal invocation`.
fn decl<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    match priv_get(scope, this, SPM_DECL).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(d) => Some(d),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The declaration's property names, in cascade order, read through its own WebIDL indexed getter --
/// `decl.item(i)`, not `decl[i]`. The indexed ACCESS is a V8 interceptor on the JS object and a Rust
/// `get_index` does not enter it, which is why `[...map.keys()][0]` came back `"undefined"` while
/// `map.size === getComputedStyle(el).length` was already true.
fn names<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>) -> Vec<String> {
    let n = get_prop(scope, d, "length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let idx: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, i).into();
        if let Some(v) = call_method(scope, d.into(), "item", &[idx]) {
            out.push(v.to_rust_string_lossy(scope));
        }
    }
    out
}

/// `decl.getPropertyValue(prop)`, or `""`. An EMPTY declaration (a detached element) answers `""` for
/// everything without looking, which is the recorded detached behaviour.
fn value_of<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>, prop: &str) -> String {
    let n = get_prop(scope, d, "length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    if n == 0 {
        return String::new();
    }
    // A `computedStyleMap()` (its declaration is a computed one) answers the COMPUTED value of a
    // longhand, as Blink does (`width: 50%` is `50%` there, `500px` in getComputedStyle).
    if let Some(Some(node)) = crate::core::css::css_style_declaration::csd_computed(scope, d) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let computed = crate::core::css::cssom::css_computed_phase_one(&mut op_state.borrow_mut(), node, prop);
        if let Some(v) = computed {
            return v;
        }
    }
    let p = str_val(scope, prop);
    call_method(scope, d.into(), "getPropertyValue", &[p]).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

fn cb_spm_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(d) = decl(scope, args.this()) else { return };
    let n = get_prop(scope, d, "length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    rv.set_uint32(n);
}

fn cb_spm_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(d) = decl(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "StylePropertyMapReadOnly", "has") {
        return;
    }
    let prop = args.get(0).to_rust_string_lossy(scope);
    if !known_property(&prop) {
        throw_type_error(scope, &format!("Failed to execute 'has' on 'StylePropertyMapReadOnly': Invalid propertyName: {prop}"));
        return;
    }
    rv.set_bool(!value_of(scope, d, &prop).is_empty());
}

fn cb_spm_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(d) = decl(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "StylePropertyMapReadOnly", "get") {
        return;
    }
    let prop = args.get(0).to_rust_string_lossy(scope);
    if !known_property(&prop) {
        throw_type_error(scope, &format!("Failed to execute 'get' on 'StylePropertyMapReadOnly': Invalid propertyName: {prop}"));
        return;
    }
    let v = value_of(scope, d, &prop);
    if v.is_empty() {
        return;
    }
    if let Some(o) = style_value(scope, &prop, &v) {
        rv.set(o.into());
    }
}

fn cb_spm_get_all<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(d) = decl(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "StylePropertyMapReadOnly", "getAll") {
        return;
    }
    let prop = args.get(0).to_rust_string_lossy(scope);
    if !known_property(&prop) {
        throw_type_error(scope, &format!("Failed to execute 'getAll' on 'StylePropertyMapReadOnly': Invalid propertyName: {prop}"));
        return;
    }
    let v = value_of(scope, d, &prop);
    let arr = v8::Array::new(scope, if v.is_empty() { 0 } else { 1 });
    if !v.is_empty() {
        if let Some(o) = style_value(scope, &prop, &v) {
            arr.set_index(scope, 0, o.into());
        }
    }
    rv.set(arr.into());
}

/// The `sequence<CSSStyleValue>` a maplike entry holds: a one-element array, per property.
fn value_seq<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>, prop: &str) -> v8::Local<'s, v8::Value> {
    let text = value_of(scope, d, prop);
    let arr = v8::Array::new(scope, 1);
    if let Some(o) = style_value(scope, prop, &text) {
        arr.set_index(scope, 0, o.into());
    }
    arr.into()
}

fn spm_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, kind: u8) {
    let Some(d) = decl(scope, args.this()) else { return };
    let ns = names(scope, d);
    let out: Vec<v8::Local<v8::Value>> = ns
        .iter()
        .map(|n| match kind {
            0 => str_val(scope, n),
            1 => value_seq(scope, d, n),
            _ => {
                let k = str_val(scope, n);
                let v = value_seq(scope, d, n);
                pair(scope, k, v)
            }
        })
        .collect();
    if let Some(it) = make_iter(scope, "StylePropertyMapReadOnly", IterKind::Pair, &out) {
        rv.set(it.into());
    }
}

fn cb_spm_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    spm_iter(scope, &args, &mut rv, 0);
}
fn cb_spm_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    spm_iter(scope, &args, &mut rv, 1);
}
fn cb_spm_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    spm_iter(scope, &args, &mut rv, 2);
}

/// `forEach(cb, thisArg)` -- `cb(value, key, map)`, with `value` the sequence. The capture's row is
/// `required: 0`, so a bare `forEach()` reaches the body; Chrome's message for it names one argument.
fn cb_spm_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = decl(scope, args.this()) else { return };
    // the names as they stood when the walk began (Chrome 150 too: a property deleted mid-walk is still visited)
    let props = names(scope, d);
    crate::core::dom::webidl_iter::for_each(scope, &args, "StylePropertyMapReadOnly", |scope, i| {
        let n = props.get(i)?;
        Some((value_seq(scope, d, n), str_val(scope, n)))
    });
}

pub fn install_style_property_map(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => row_getter(scope, proto, member, cb_spm_size),
        "get" => row_method_recv_first(scope, proto, member, cb_spm_get),
        "getAll" => row_method_recv_first(scope, proto, member, cb_spm_get_all),
        "has" => row_method_recv_first(scope, proto, member, cb_spm_has),
        "entries" => row_method(scope, proto, member, cb_spm_entries),
        "forEach" => row_method(scope, proto, member, cb_spm_for_each),
        "keys" => row_method(scope, proto, member, cb_spm_keys),
        "values" => row_method(scope, proto, member, cb_spm_values),
        "@@iterator" => {
            let Some(entries) = get_prop(scope, proto, "entries") else { return };
            define_value(scope, proto, member, entries, true);
        }
        other => debug_assert!(false, "install_style_property_map: unexpected member {other}"),
    }
}

// ── StylePropertyMap's writes ────────────────────────────────────────────────────────────────────

/// Blink's repeated (list-valued) properties, the ones `append` extends: `css_properties.json5`'s
/// entries with a `separator` (Chromium 150).
const REPEATED: &[&str] = &[
    "animation-composition", "animation-delay", "animation-direction", "animation-duration", "animation-fill-mode",
    "animation-iteration-count", "animation-name", "animation-play-state", "animation-range-start", "animation-range-end",
    "animation-timeline", "animation-timing-function", "animation-trigger", "timeline-trigger-name",
    "timeline-trigger-activation-range-start", "timeline-trigger-activation-range-end", "timeline-trigger-active-range-start",
    "timeline-trigger-active-range-end", "timeline-trigger-source", "transition-delay", "transition-duration", "transition-behavior",
    "transition-timing-function", "background-attachment", "background-blend-mode", "background-clip", "background-image",
    "background-origin", "background-size", "content", "grid-auto-columns", "grid-auto-rows", "overflow-clip-margin",
    "scroll-timeline-axis", "scroll-timeline-name", "text-decoration-skip-spaces", "timeline-scope", "view-timeline-axis",
    "view-timeline-inset", "view-timeline-name",
];

/// `set(property, ...values)` / `append(property, ...values)` / `delete(property)` / `clear()` on an
/// element's `attributeStyleMap` or a rule's `styleMap`, written through the declaration they view (so
/// `el.style` reads what the map wrote). Blink's checks, Chrome 150's messages: an unknown name is
/// "Invalid propertyName: x"; `set` with no value or one the property does not take is "Invalid type
/// for property"; `append` to a property that is not a list is "Property does not support multiple
/// values"; `append(name)` with no values does nothing.
fn cb_spm_write<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(d) = decl(scope, args.this()) else { return };
    let ctx = format!("Failed to execute '{method}' on 'StylePropertyMap': ");
    if method == "clear" {
        let e = str_val(scope, "");
        let k = str_val(scope, "cssText");
        d.set(scope, k, e);
        return;
    }
    if !require_args(scope, &args, 1, "StylePropertyMap", &method) {
        return;
    }
    let Some(raw) = args.get(0).to_string(scope) else { return };
    let raw = raw.to_rust_string_lossy(scope);
    let prop = if raw.starts_with("--") { raw.clone() } else { raw.to_ascii_lowercase() };
    if !known_property(&prop) {
        throw_type_error(scope, &format!("{ctx}Invalid propertyName: {raw}"));
        return;
    }
    let p = str_val(scope, &prop);
    if method == "delete" {
        call_method(scope, d.into(), "removeProperty", &[p]);
        return;
    }
    let mut texts = Vec::new();
    for i in 1..args.length() {
        match crate::core::css::typed_om::value_text(scope, args.get(i)) {
            Some(t) => texts.push(t),
            None => {
                throw_type_error(scope, &format!("{ctx}Invalid type for property"));
                return;
            }
        }
    }
    let repeated = REPEATED.contains(&prop.as_str());
    if method == "append" {
        if texts.is_empty() {
            return;
        }
        if !repeated {
            throw_type_error(scope, &format!("{ctx}Property does not support multiple values"));
            return;
        }
        let current = value_of(scope, d, &prop);
        if !current.is_empty() {
            texts.insert(0, current);
        }
    } else if texts.is_empty() || (texts.len() > 1 && !repeated) {
        throw_type_error(scope, &format!("{ctx}Invalid type for property"));
        return;
    }
    let text = texts.join(", ");
    if !prop.starts_with("--") && !page::style::supports::supports_declaration(&prop, &text) {
        throw_type_error(scope, &format!("{ctx}Invalid type for property"));
        return;
    }
    let v = str_val(scope, &text);
    call_method(scope, d.into(), "setProperty", &[p, v]);
}

pub fn install_style_property_map_writes(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let d = str_val(scope, member.name);
    let f = build_fn_data(scope, cb_spm_write, member.name, member.length as i32, d);
    define_value(scope, proto, member, f.into(), true);
}

/// `CSSStyleRule.styleMap`: the `StylePropertyMap` over the rule's `style` declaration, `[SameObject]`.
pub(crate) fn rule_map<'s>(scope: &mut v8::PinScope<'s, '_>, rule: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    const RULE_MAP: &str = "korvus_rule_style_map";
    if let Some(m) = priv_get(scope, rule, RULE_MAP).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(m);
    }
    let d = get_prop(scope, rule, "style").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = proto_of(scope, "StylePropertyMap")?;
    let map = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, map);
    priv_set(scope, map, SPM_DECL, d.into());
    priv_set(scope, rule, RULE_MAP, map.into());
    Some(map)
}

// ── Element.computedStyleMap() ───────────────────────────────────────────────────────────────────

/// `Element.computedStyleMap()` -- a `StylePropertyMapReadOnly` over the SAME resolved values
/// `getComputedStyle(this)` returns, so the two views of one element cannot disagree.
///
/// A row of the `Element` table (`core/dom/element.rs`), not a kit: only this interface has it.
pub(crate) fn cb_el_computed_style_map<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if el_id_of(scope, this).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let Some(proto) = proto_of(scope, "StylePropertyMapReadOnly") else { return };
    let global = scope.get_current_context().global(scope);
    let el: v8::Local<v8::Value> = this.into();
    let Some(d) = call_method(scope, global.into(), "getComputedStyle", &[el]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    let map = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, map);
    priv_set(scope, map, SPM_DECL, d.into());
    rv.set(map.into());
}

/// `el.attributeStyleMap` -- the `StylePropertyMap` over the element's INLINE declaration, the one
/// `el.style` answers, so the two views of the style attribute cannot disagree (a write through one reads
/// back through the other). It was minted without its declaration, so every member refused it with
/// `Illegal invocation` -- `size`, `get`, `keys()` alike -- where Chrome 150.0.7871.46 answers the inline
/// style (measured).
pub(crate) fn inline_map<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let d = crate::core::dom::element::inline_style_decl(scope, el)?;
    let proto = proto_of(scope, "StylePropertyMap")?;
    let map = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, map);
    priv_set(scope, map, SPM_DECL, d.into());
    Some(map)
}

// The two tables. Rows are the capture's verbatim; the `@@iterator` row is the maplike's entry
// iterator, which the registry cannot carry because `Object.getOwnPropertyNames` skips symbols
// (Chrome 150: `StylePropertyMapReadOnly.prototype[Symbol.iterator] === ...entries`).
webapi::interface! {
    "StylePropertyMapReadOnly" [interface, window] {
        ctor "StylePropertyMapReadOnly" len 0 = stub,
        attr "size" : Any readonly = install_style_property_map,
        method "get" : Any len 1 required 1 = install_style_property_map,
        method "getAll" : Any len 1 required 1 = install_style_property_map,
        method "has" : Any len 1 required 1 = install_style_property_map,
        method "entries" : Any len 0 = install_style_property_map,
        method "forEach" : Any len 1 = install_style_property_map,
        method "keys" : Any len 0 = install_style_property_map,
        method "values" : Any len 0 = install_style_property_map,
        method "@@iterator" : Any len 0 hidden = install_style_property_map
    }
}

webapi::interface! {
    "CSSStyleValue" [interface, window] {
        ctor "CSSStyleValue" len 0 = stub,
        method "toString" : Any len 0 = install_css_style_value,
        static_method "parse" : Any len 2 required 2 = install_css_style_value,
        static_method "parseAll" : Any len 2 required 2 = install_css_style_value
    }
}

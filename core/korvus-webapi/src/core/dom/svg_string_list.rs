//! `core/dom/svg_string_list.rs` -- `SVGStringList` and the two attributes that hand one out,
//! the Rust form of 90 lines of `dom/09-postshells.js`.
//!
//! WHAT A `SVGStringList` IS. `svg.requiredExtensions` and `svg.systemLanguage` are `[SameObject]`
//! live views of a CONTENT ATTRIBUTE -- the list and the attribute are one fact, and every mutating
//! member writes the attribute back. The capture declares the pair on exactly four interfaces
//! (`SVGGraphicsElement`, `SVGPatternElement`, `SVGMaskElement`, `SVGAnimationElement`), which is what
//! Chrome 150 exposes, verified by sweeping `Object.getOwnPropertyNames` of nine SVG prototypes.
//!
//! WHAT THE JS DID INSTEAD, and why this is not a like-for-like port. Its list was a DETACHED scratch
//! array in a `WeakMap`: `svg.setAttribute("requiredExtensions", "p q")` left the list empty, and
//! `list.appendItem("a")` never reached the attribute. So the nine members were real code over storage
//! no other part of the engine could see -- the brief's failure mode 3, a body reachable where the
//! spec has checks. The bundle's sixteen recorded cases all run on a FRESH `<svg>` with no attribute,
//! so they passed either way; Chrome 150 over loopback is what says which is right.
//!
//! CHROME'S ANSWERS, recorded 2026-09-11, each a case of `tests/svg_string_list_live.rs`:
//!
//! * `requiredExtensions` is SPACE-separated and `systemLanguage` is COMMA-separated, in BOTH
//!   directions and not interchangeably: `requiredExtensions="a,b"` is ONE item `"a,b"`, and two
//!   appends to `systemLanguage` write `"en,fr"`.
//! * `clear()` REMOVES the attribute (`getAttribute` is then `null`), it does not leave an empty one.
//! * An index is WebIDL `unsigned long`: `getItem(-1)` reports index 4294967295. Out of range is
//!   `IndexSizeError` with "The index provided (N) is greater than the maximum bound (LEN)" -- the JS
//!   said "greater than or EQUAL TO the maximum bound", which is not Chrome's sentence.
//! * `insertItemBefore(v, 99)` CLAMPS to the end rather than throwing.
//! * The list holds duplicates (`appendItem("a")` twice is two items) -- it is not a set.
//! * `[SameObject]` survives an attribute write: the list read before and after `setAttribute` is the
//!   same object, and reports the new contents.
//!
//! RESIDUAL, stated rather than hidden: Chrome's list also has an INDEXED property getter
//! (`Object.getOwnPropertyNames(list)` is `["0","1"]`, `list[0]` is the item, and `@@iterator` is
//! `Array.prototype.values`, so `[...list]` works). That is a V8 `ObjectTemplate` interceptor, the
//! machinery `core/dom/collections.rs` carries for the ten platform lists, and the capture's member
//! list cannot record it (`Object.getOwnPropertyNames` of a PROTOTYPE never shows an interceptor). The
//! JS had none either, so this lane neither adds nor removes it; it is the one shape difference left
//! on this interface, and it belongs with the collections machinery, not here.

use deno_core::v8;

use crate::binding::protos::{brand_shell_instance, get_prop, require_args, str_val, throw_dom, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::dom::shared::{row_getter, row_method_recv_first};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

/// The element the list is a view of. Its presence IS the brand.
const SL_OWNER: &str = "korvus_svgsl_owner";
/// The content attribute the list is a view of (`requiredExtensions` / `systemLanguage`).
const SL_ATTR: &str = "korvus_svgsl_attr";

/// The `[SameObject]` cache, on the ELEMENT: one private per attribute.
fn cache_key(attr: &str) -> &'static str {
    match attr {
        "systemLanguage" => "korvus_svgsl_systemLanguage",
        _ => "korvus_svgsl_requiredExtensions",
    }
}

/// `requiredExtensions` is a space-separated list and `systemLanguage` a comma-separated one -- not
/// interchangeably, measured in Chrome 150 in both directions.
fn comma_separated(attr: &str) -> bool {
    attr == "systemLanguage"
}

// ── the attribute IS the list ────────────────────────────────────────────────────────────────────

/// The receiver as a list: `(owner element, attribute name)`, or a pending `Illegal invocation`.
fn list_of<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<(v8::Local<'s, v8::Object>, String)> {
    let owner = priv_get(scope, this, SL_OWNER).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let attr = priv_get(scope, this, SL_ATTR).map(|v| v.to_rust_string_lossy(scope));
    match (owner, attr) {
        (Some(o), Some(a)) => Some((o, a)),
        _ => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The list's items, parsed out of the content attribute. An absent or empty attribute is an empty
/// list; empty separators collapse (`"p  q"` is two items).
fn items<'s>(scope: &mut v8::PinScope<'s, '_>, owner: v8::Local<'s, v8::Object>, attr: &str) -> Vec<String> {
    let key = str_val(scope, attr);
    let raw = match crate::core::dom::element::attr_call(scope, owner.into(), "getAttribute", &[key]) {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => return Vec::new(),
    };
    let parts: Vec<&str> =
        if comma_separated(attr) { raw.split(',').collect() } else { raw.split_ascii_whitespace().collect() };
    parts.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Write `vals` back to the content attribute -- or REMOVE it when the list is empty, which is what
/// Chrome does for `clear()` (`getAttribute` reads `null`, not `""`).
fn write<'s>(scope: &mut v8::PinScope<'s, '_>, owner: v8::Local<'s, v8::Object>, attr: &str, vals: &[String]) {
    let key = str_val(scope, attr);
    if vals.is_empty() {
        crate::core::dom::element::attr_call(scope, owner.into(), "removeAttribute", &[key]);
        return;
    }
    let joined = vals.join(if comma_separated(attr) { "," } else { " " });
    let v = str_val(scope, &joined);
    crate::core::dom::element::attr_call(scope, owner.into(), "setAttribute", &[key, v]);
}

/// A WebIDL `unsigned long` index, range-checked against `len`. Chrome checks the range BEFORE it
/// coerces the value, which is why `replaceItem("x", 1)` on an empty list is `IndexSizeError` and not
/// a `TypeError`, and the message names the list's LENGTH as the bound.
fn index_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, len: usize, member: &str) -> Option<usize> {
    let n = v.uint32_value(scope).unwrap_or(0);
    if n as usize >= len {
        throw_dom(
            scope,
            &format!(
                "Failed to execute '{member}' on 'SVGStringList': The index provided ({n}) is greater than the maximum bound ({len})."
            ),
            "IndexSizeError",
        );
        return None;
    }
    Some(n as usize)
}

// ── the nine members ─────────────────────────────────────────────────────────────────────────────

/// `length` and `numberOfItems` are the same count under two names (the SVG DOM's own alias).
fn cb_sl_length<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    rv.set_uint32(items(scope, owner, &attr).len() as u32);
}

fn cb_sl_clear<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    write(scope, owner, &attr, &[]);
}

fn cb_sl_initialize<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "SVGStringList", "initialize") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    write(scope, owner, &attr, std::slice::from_ref(&s));
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_sl_get_item<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "SVGStringList", "getItem") {
        return;
    }
    let vals = items(scope, owner, &attr);
    let Some(i) = index_of(scope, args.get(0), vals.len(), "getItem") else { return };
    let v = str_val(scope, &vals[i]);
    rv.set(v);
}

fn cb_sl_append_item<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "SVGStringList", "appendItem") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    let mut vals = items(scope, owner, &attr);
    vals.push(s.clone());
    write(scope, owner, &attr, &vals);
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `insertItemBefore(v, i)` CLAMPS: an index past the end appends rather than throwing (recorded --
/// `insertItemBefore("z", 99)` on a one-item list gives `["a", "z"]`).
fn cb_sl_insert_item_before<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 2, "SVGStringList", "insertItemBefore") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    let mut vals = items(scope, owner, &attr);
    let at = (args.get(1).uint32_value(scope).unwrap_or(0) as usize).min(vals.len());
    vals.insert(at, s.clone());
    write(scope, owner, &attr, &vals);
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_sl_replace_item<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 2, "SVGStringList", "replaceItem") {
        return;
    }
    let mut vals = items(scope, owner, &attr);
    let Some(i) = index_of(scope, args.get(1), vals.len(), "replaceItem") else { return };
    let s = args.get(0).to_rust_string_lossy(scope);
    vals[i] = s.clone();
    write(scope, owner, &attr, &vals);
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_sl_remove_item<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((owner, attr)) = list_of(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "SVGStringList", "removeItem") {
        return;
    }
    let mut vals = items(scope, owner, &attr);
    let Some(i) = index_of(scope, args.get(0), vals.len(), "removeItem") else { return };
    let gone = vals.remove(i);
    write(scope, owner, &attr, &vals);
    let v = str_val(scope, &gone);
    rv.set(v);
}

/// The `SVGStringList` rows.
pub fn install_svg_string_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" | "numberOfItems" => row_getter(scope, proto, member, cb_sl_length),
        "appendItem" => row_method_recv_first(scope, proto, member, cb_sl_append_item),
        "clear" => row_method_recv_first(scope, proto, member, cb_sl_clear),
        "getItem" => row_method_recv_first(scope, proto, member, cb_sl_get_item),
        "initialize" => row_method_recv_first(scope, proto, member, cb_sl_initialize),
        "insertItemBefore" => row_method_recv_first(scope, proto, member, cb_sl_insert_item_before),
        "removeItem" => row_method_recv_first(scope, proto, member, cb_sl_remove_item),
        "replaceItem" => row_method_recv_first(scope, proto, member, cb_sl_replace_item),
        other => debug_assert!(false, "install_svg_string_list: unexpected member {other}"),
    }
}

// ── the two attributes that hand one out ─────────────────────────────────────────────────────────

fn cb_sl_attr_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // The receiver must be an element: `getAttribute` is the only thing this list can be a view of,
    // and a foreign object does not have one. Chrome throws `Illegal invocation` here too.
    if crate::core::dom::element::el_id_of(scope, this).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let key = cache_key(&attr);
    if let Some(cached) = priv_get(scope, this, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        rv.set(cached.into());
        return;
    }
    let Some(proto) = crate::binding::protos::iface_object(scope, "SVGStringList")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    let list = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, list);
    priv_set(scope, list, SL_OWNER, this.into());
    let a = str_val(scope, &attr);
    priv_set(scope, list, SL_ATTR, a);
    priv_set(scope, this, key, list.into());
    rv.set(list.into());
}

/// `readonly attribute SVGStringList requiredExtensions` / `systemLanguage` -- one kit, because four
/// REGISTRY interfaces declare the pair and none of the four is a table. `tools/sync/gen_shells.py`
/// claims the rows by scanning the capture for the two member names, so the claim is derived from the
/// bundle rather than written down (the shape `gen_svg.animated_rows` established in W5.1 L16b).
pub(crate) fn install_svg_string_list_attr(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::binding::protos::build_fn_data;
    let d = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_sl_attr_get, &format!("get {}", member.name), 0, d);
    define_accessor(scope, proto, member, g, None);
}

// The table. Rows are the capture's (`install/generated_registry.rs` at the cutover), verbatim.
webapi::interface! {
    "SVGStringList" [interface, window] {
        ctor "SVGStringList" len 0 = stub,
        attr "length" : Any readonly = install_svg_string_list,
        attr "numberOfItems" : Any readonly = install_svg_string_list,
        method "appendItem" : Any len 1 required 1 = install_svg_string_list,
        method "clear" : Any len 0 = install_svg_string_list,
        method "getItem" : Any len 1 required 1 = install_svg_string_list,
        method "initialize" : Any len 1 required 1 = install_svg_string_list,
        method "insertItemBefore" : Any len 2 required 2 = install_svg_string_list,
        method "removeItem" : Any len 1 required 1 = install_svg_string_list,
        method "replaceItem" : Any len 2 required 2 = install_svg_string_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

//! `core/dom/named_node_map.rs` — `Element.attributes` (WHATWG DOM §2.9, `interface NamedNodeMap`)
//! over the arena. Extracted verbatim from bindings.rs (file-move STEP 1) and since made spec-true:
//! every member enforces its WebIDL arity (`item()` is a `TypeError`, never a defaulted `null`),
//! `setNamedItem(NS)` converts its argument to `Attr` (non-Attr is a `TypeError`), the
//! already-in-use check throws `InUseAttributeError`, and `removeNamedItem(NS)` throws
//! `NotFoundError` with Chrome's message when nothing matches. Blink reference:
//! `core/dom/named_node_map.cc` (semantics, not line-by-line).
//!
//! `length` and `item` are NOT here: they are the shared list members every one of the ten list
//! interfaces carries (`core/dom/collections.rs`), reading the SAME `NNM_EID` private this file
//! defines, and the instance itself is minted from the indexed+named interceptor template there.

use deno_core::v8;
use korvus_dom::Dom;
use crate::core::dom::attr::{attr_eid_of, attr_name_of, make_attr};
use crate::core::css::css_style_declaration::{priv_get};
use crate::core::dom::element::{normalize_attr_name};
use crate::core::dom::shared::{remove_attribute_notify, set_attribute_notify};
use crate::binding::protos::{get_prop, require_args, throw_dom, throw_type_error};
use crate::binding::host_dom::{HostDom};

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// ---- Element.attributes -> NamedNodeMap + Element.getAttributeNode -> Attr (arena-backed) ----
// Attr carries its owner element id + attribute name on v8 Privates; its live value reads/writes the
// arena. NamedNodeMap carries the element id and returns Attr objects for the element's attributes.
// Build an Attr object (DOMTokenList-style: proto from globalThis.Attr, privates for eid+name).
// Attr getters: name/localName -> the attribute name; value -> live arena value; namespaceURI/prefix ->
// null (HTML); ownerElement -> the wrapped element; specified -> true.
// NamedNodeMap over the element's attributes.

pub(crate) const NNM_EID: &str = "korvus_nnm_eid";

pub(crate) fn nnm_eid(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    priv_get(scope, this, NNM_EID).and_then(|v| v.uint32_value(scope))
}

/// The receiver's element id, or an "Illegal invocation" `TypeError` (WebIDL: a platform-object
/// method called on a receiver that does not implement the interface). `None` => already thrown.
fn nnm_receiver(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    match nnm_eid(scope, args.this()) {
        Some(e) => Some(e),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// WHATWG "get an attribute" by (namespace, localName): the first attribute whose stored qualified
/// name IS `local` or whose local part (after the last ':') is `local`. The namespace argument is
/// advisory here because the arena records attributes by qualified name only — exactly the lookup
/// `Element.getAttributeNS` uses, so `nnm.getNamedItemNS(ns, l)` and `el.getAttributeNS(ns, l)`
/// always agree.
fn find_by_local(dom: &Dom, eid: u32, local: &str) -> Option<String> {
    dom.attribute_names(eid)
        .into_iter()
        .find(|n| n == local || n.rsplit(':').next() == Some(local))
}


/// `DOMString` (WebIDL): ToString. `None` => ToString threw (Symbol); the pending exception
/// propagates.
fn string_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> Option<String> {
    args.get(i).to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

/// `DOMString?` (WebIDL): `null`/`undefined` convert to `None`, everything else to `ToString`.
fn nullable_string_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> Option<Option<String>> {
    let v = args.get(i);
    if v.is_null_or_undefined() { return Some(None); }
    v.to_string(scope).map(|s| Some(s.to_rust_string_lossy(scope)))
}



pub(crate) fn cb_nnm_get_named(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let eid = match nnm_receiver(scope, &args) { Some(e) => e, None => return };
    if !require_args(scope, &args, 1, "NamedNodeMap", "getNamedItem") { return; }
    let Some(raw) = string_arg(scope, &args, 0) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // WHATWG "get an attribute": HTML-namespace element in an HTML document => ASCII-lowercase the
    // qualified name before the exact-match arena lookup (same as Element.getAttribute).
    let name = { let st = op_state.borrow(); normalize_attr_name(&st, eid, &raw) };
    let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some() };
    if has { if let Some(a) = make_attr(scope, eid, &name) { rv.set(a.into()); return; } }
    rv.set_null();
}

pub(crate) fn cb_nnm_get_named_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let eid = match nnm_receiver(scope, &args) { Some(e) => e, None => return };
    if !require_args(scope, &args, 2, "NamedNodeMap", "getNamedItemNS") { return; }
    let (Some(_ns), Some(local)) = (nullable_string_arg(scope, &args, 0), string_arg(scope, &args, 1)) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let name = { let st = op_state.borrow(); find_by_local(&st.borrow::<HostDom>().0, eid, &local) };
    match name { Some(n) => { if let Some(a) = make_attr(scope, eid, &n) { rv.set(a.into()); } } None => rv.set_null() }
}

/// Shared `setNamedItem`/`setNamedItemNS` body. `ns_aware` picks the storage convention of the
/// matching Element member: the non-NS path ASCII-lowercases for HTML-in-HTML (like setAttribute),
/// the NS path stores the qualified name verbatim (like setAttributeNS).
fn nnm_set_named(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, member: &str, ns_aware: bool) {
    let eid = match nnm_receiver(scope, &args) { Some(e) => e, None => return };
    if !require_args(scope, &args, 1, "NamedNodeMap", member) { return; }
    // WebIDL: `Attr attr` — anything that is not one of our Attr objects is a TypeError.
    let attr = match v8::Local::<v8::Object>::try_from(args.get(0)) {
        Ok(o) => o,
        Err(_) => { throw_type_error(scope, &format!("Failed to execute '{member}' on 'NamedNodeMap': parameter 1 is not of type 'Attr'.")); return; }
    };
    let Some(attr_qname) = attr_name_of(scope, attr) else {
        throw_type_error(scope, &format!("Failed to execute '{member}' on 'NamedNodeMap': parameter 1 is not of type 'Attr'."));
        return;
    };
    // WHATWG: "If attr's element is neither null nor this's element, then throw an
    // InUseAttributeError." An Attr wrapper whose owner no longer holds the attribute counts as
    // detached (owner null) — removal is what severs ownership.
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(owner) = attr_eid_of(scope, attr) {
        if owner != eid {
            let attached = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(owner, &attr_qname).is_some() };
            if attached {
                throw_dom(scope, "The node provided is an attribute node that is already an attribute of another Element; attribute nodes must be explicitly cloned.", "InUseAttributeError");
                return;
            }
        }
    }
    let value = get_prop(scope, attr, "value").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let name = if ns_aware {
        attr_qname
    } else {
        let st = op_state.borrow();
        normalize_attr_name(&st, eid, &attr_qname)
    };
    // The Attr to return is the OLD one (spec: set-an-attribute returns the replaced attribute, or
    // null when there was none). Our Attr wrappers are live views, so the returned object reads the
    // arena's post-set value — the identity/shape is right, the detached value snapshot is not.
    let old = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some() }.then(|| make_attr(scope, eid, &name)).flatten();
    set_attribute_notify(scope, &op_state, eid, &name, &value);
    match old { Some(a) => rv.set(a.into()), None => rv.set_null() }
}

pub(crate) fn cb_nnm_set_named(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    nnm_set_named(scope, &args, &mut rv, "setNamedItem", false);
}

pub(crate) fn cb_nnm_set_named_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    nnm_set_named(scope, &args, &mut rv, "setNamedItemNS", true);
}

/// Shared `removeNamedItem`/`removeNamedItemNS` body. `miss_msg` is Chrome's NotFoundError message
/// for the not-found case (Blink: "No item with name '<name>' was found." / "<ns>::<local>").
fn nnm_remove_named(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, member: &str, ns_aware: bool) {
    let eid = match nnm_receiver(scope, &args) { Some(e) => e, None => return };
    let need = if ns_aware { 2 } else { 1 };
    if !require_args(scope, &args, need, "NamedNodeMap", member) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (name, miss_msg) = if ns_aware {
        let (Some(ns), Some(local)) = (nullable_string_arg(scope, &args, 0), string_arg(scope, &args, 1)) else { return };
        let miss = format!("No item with name '{}::{}' was found.", ns.as_deref().unwrap_or(""), local);
        let hit = { let st = op_state.borrow(); find_by_local(&st.borrow::<HostDom>().0, eid, &local) };
        (hit, miss)
    } else {
        let Some(raw) = string_arg(scope, &args, 0) else { return };
        let miss = format!("No item with name '{raw}' was found.");
        let hit = { let st = op_state.borrow(); let name = normalize_attr_name(&st, eid, &raw); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some().then_some(name) };
        (hit, miss)
    };
    // WHATWG: remove-an-attribute; nothing matched => NotFoundError (Chrome's message, verbatim).
    let Some(name) = name else { throw_dom(scope, &miss_msg, "NotFoundError"); return; };
    let old = make_attr(scope, eid, &name);
    remove_attribute_notify(scope, &op_state, eid, &name);
    if let Some(a) = old { rv.set(a.into()); }
}

pub(crate) fn cb_nnm_remove_named(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    nnm_remove_named(scope, &args, &mut rv, "removeNamedItem", false);
}

pub(crate) fn cb_nnm_remove_named_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    nnm_remove_named(scope, &args, &mut rv, "removeNamedItemNS", true);
}

//! `core/dom/attr.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::core::dom::element::{priv_set};
use crate::core::css::css_style_declaration::{priv_get};
use crate::binding::protos::{get_prop, int_val, str_val, wrap_cached};
use crate::binding::host_dom::{HostDom};

pub(crate) const ATTR_EID: &str = "korvus_attr_eid";
pub(crate) const ATTR_NAME: &str = "korvus_attr_name";
/// A DETACHED Attr's own value: one `createAttribute` made, or one `removeAttributeNode` took off its
/// element. An attached Attr has `ATTR_EID` instead and reads its element's attribute.
pub(crate) const ATTR_VALUE: &str = "korvus_attr_value";

pub(crate) fn make_attr<'s>(scope: &mut v8::PinScope<'s, '_>, eid: u32, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "Attr").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype")).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let eidv = int_val(scope, eid as i64); priv_set(scope, o, ATTR_EID, eidv);
    let nv = str_val(scope, name); priv_set(scope, o, ATTR_NAME, nv);
    // `nodeType` (2), `nodeName` and `nodeValue` come from Node.prototype's polymorphic getters, which
    // recognise an Attr by its privates (node.rs) -- as does everything else, through [`is_attr`]. An
    // own `__isAttr` property used to mark it, visible to `Object.keys(attr)` (Chrome: `[]`).
    Some(o)
}
/// Whether `v` is an Attr made here (attached or detached: both carry the name private).
pub(crate) fn is_attr(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    v8::Local::<v8::Object>::try_from(v).ok().is_some_and(|o| attr_name_of(scope, o).is_some())
}

/// An Attr with no element (`document.createAttribute`): its value lives on the object until
/// `setAttributeNode` attaches it. It used to be a stub that returned a plain `{}`, so every Node
/// member read off it threw "Illegal invocation".
pub(crate) fn make_detached_attr<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, value: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "Attr").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype")).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let nv = str_val(scope, name); priv_set(scope, o, ATTR_NAME, nv);
    let vv = str_val(scope, value); priv_set(scope, o, ATTR_VALUE, vv);
    Some(o)
}

/// Point `attr` at element `eid` (setAttributeNode), or detach it keeping `value` (removal).
pub(crate) fn attach_attr(scope: &mut v8::PinScope, attr: v8::Local<v8::Object>, eid: Option<u32>, value: &str) {
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match eid {
        Some(e) => {
            let ev = int_val(scope, e as i64); priv_set(scope, attr, ATTR_EID, ev);
            priv_set(scope, attr, ATTR_VALUE, undef);
        }
        None => {
            priv_set(scope, attr, ATTR_EID, undef);
            let vv = str_val(scope, value); priv_set(scope, attr, ATTR_VALUE, vv);
        }
    }
}

fn detached_value(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    priv_get(scope, this, ATTR_VALUE).map(|v| v.to_rust_string_lossy(scope))
}
pub(crate) fn attr_name_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    priv_get(scope, this, ATTR_NAME).map(|v| v.to_rust_string_lossy(scope))
}
pub(crate) fn attr_eid_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    priv_get(scope, this, ATTR_EID).and_then(|v| v.uint32_value(scope))
}
/// The Attr's current value (its owner's attribute), for the Node members an Attr answers through.
pub(crate) fn attr_value_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    let Some(eid) = attr_eid_of(scope, this) else { return detached_value(scope, this) };
    let name = attr_name_of(scope, this)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    Some(st.borrow::<HostDom>().0.get_attribute(eid, &name).unwrap_or("").to_string())
}

pub(crate) fn cb_attr_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match attr_name_of(scope, args.this()) { Some(n) => { let v = str_val(scope, &n); rv.set(v); } None => rv.set_null() }
}
pub(crate) fn cb_attr_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let (eid, name) = match (attr_eid_of(scope, this), attr_name_of(scope, this)) {
        (Some(e), Some(n)) => (e, n),
        _ => { let v = detached_value(scope, this).unwrap_or_default(); let e = str_val(scope, &v); rv.set(e); return; }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let val = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).unwrap_or("").to_string() };
    let v = str_val(scope, &val); rv.set(v);
}
/// `attr.value = x` is an attribute write like any other, so it goes through the SAME notify choke
/// point `setAttribute` and `classList.add` use: it went straight to the arena and stayed invisible to
/// an `attributes` MutationObserver, which is why WPT `MutationObserver-attributes` timed out waiting
/// for the record ("Element.attributes.value: update mutation").
pub(crate) fn cb_attr_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let val = args.get(0).to_rust_string_lossy(scope);
    let (eid, name) = match (attr_eid_of(scope, this), attr_name_of(scope, this)) {
        (Some(e), Some(n)) => (e, n),
        (None, Some(_)) => { let v = str_val(scope, &val); priv_set(scope, this, ATTR_VALUE, v); return; }
        _ => return,
    };
    // an attached Attr's value is an attribute write, and Trusted Types checks it as one
    let Some(val) = crate::core::dom::element::attribute_trusted_value(scope, eid, &name, None, val, None, "Failed to set the 'value' property on 'Attr'") else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    crate::core::dom::shared::set_attribute_notify(scope, &op_state, eid, &name, &val);
}

const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// An attribute's namespace, prefix and local name. The arena keeps an attribute as its qualified name, so these
/// are read off the name and the owner, as the parsers decided them (Chrome 150 measured):
///
/// * an HTML element of an HTML document: no namespace, no prefix, the whole name local (`xmlns`, `xml:lang`,
///   `xlink:href` on `<html>` or `<p>` included);
/// * a foreign (SVG / MathML) element of an HTML document: HTML's "adjust foreign attributes" -- `xlink:actuate`
///   / `arcrole` / `href` / `role` / `show` / `title` / `type` in XLink, `xml:lang` / `xml:space` in XML,
///   `xmlns` and `xmlns:xlink` in XMLNS;
/// * an element of an XML document: `xmlns` / `xmlns:*` in XMLNS, `xml:*` in XML, any other prefix the
///   namespace an `xmlns:<prefix>` of the element or an ancestor binds (unbound: no namespace, the name whole).
///
/// An attribute `setAttributeNS` gave a namespace keeps only its qualified name, so it reads as above.
pub(crate) fn attr_name_parts(dom: &korvus_dom::Dom, owner: u32, name: &str) -> (Option<String>, Option<String>, String) {
    let whole = || (None, None, name.to_string());
    let xml_doc = dom.is_in_xml(owner);
    if !xml_doc && dom.is_html_ns(owner) {
        return whole();
    }
    if name == "xmlns" {
        return (Some(XMLNS_NS.into()), None, name.to_string());
    }
    let Some((p, l)) = name.split_once(':').filter(|(p, l)| !p.is_empty() && !l.is_empty()) else { return whole() };
    let parts = |ns: &str| (Some(ns.to_string()), Some(p.to_string()), l.to_string());
    if !xml_doc {
        return match (p, l) {
            ("xlink", "actuate" | "arcrole" | "href" | "role" | "show" | "title" | "type") => parts(XLINK_NS),
            ("xml", "lang" | "space") => parts(XML_NS),
            ("xmlns", "xlink") => parts(XMLNS_NS),
            _ => whole(),
        };
    }
    match p {
        "xmlns" => parts(XMLNS_NS),
        "xml" => parts(XML_NS),
        _ => {
            let decl = format!("xmlns:{p}");
            let mut cur = Some(owner);
            while let Some(n) = cur {
                if dom.node_type(n) == 1 {
                    if let Some(v) = dom.get_attribute(n, &decl) {
                        return if v.is_empty() { whole() } else { parts(v) };
                    }
                }
                cur = dom.parent(n);
            }
            whole()
        }
    }
}

/// `Attr.namespaceURI` / `prefix` / `localName` ([`attr_name_parts`]; `data` names which).
pub(crate) fn cb_attr_name_part(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    let Some(name) = attr_name_of(scope, this) else { rv.set_null(); return };
    let parts = match attr_eid_of(scope, this) {
        Some(eid) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            match st.try_borrow::<HostDom>() {
                Some(d) => attr_name_parts(&d.0, eid, &name),
                None => (None, None, name.clone()),
            }
        }
        // a detached attribute (`document.createAttribute`) has its name whole
        None => (None, None, name.clone()),
    };
    let v = match which.as_str() {
        "namespaceURI" => parts.0,
        "prefix" => parts.1,
        _ => Some(parts.2),
    };
    match v {
        Some(v) => { let v = str_val(scope, &v); rv.set(v); }
        None => rv.set_null(),
    }
}
pub(crate) fn cb_attr_specified(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let _ = scope; rv.set_bool(true); }
pub(crate) fn cb_attr_owner(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let eid = match attr_eid_of(scope, args.this()) { Some(e) => e, None => { rv.set_null(); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let obj = wrap_cached(scope, &op_state, eid); rv.set(obj.into());
}

// ── the Attr table ─────────────────────────────────────────────────────────────────

use crate::core::dom::shared::{row_accessor, row_getter};
use crate::install::MemberDef;

/// The `Attr` rows: a view over the owner element's attribute map, keyed by the two privates
/// [`make_attr`] stamps. `Attr.prototype` chains through `Node.prototype` (the installer's splice).
pub fn install_attr(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "namespaceURI" | "prefix" | "localName" => {
            let d = str_val(scope, member.name);
            crate::core::dom::shared::row_getter_data(scope, proto, member, cb_attr_name_part, d);
        }
        "name" => row_getter(scope, proto, member, cb_attr_name),
        "value" => row_accessor(scope, proto, member, cb_attr_value_get, cb_attr_value_set),
        "ownerElement" => row_getter(scope, proto, member, cb_attr_owner),
        "specified" => row_getter(scope, proto, member, cb_attr_specified),
        other => debug_assert!(false, "install_attr: unexpected member {other}"),
    }
}

webapi::interface! {
    "Attr" : "Node" [interface, window] {
        ctor "Attr" len 0 = stub,
        attr "namespaceURI" : Object readonly nullable = install_attr,
        attr "prefix" : Object readonly nullable = install_attr,
        attr "localName" : Object readonly = install_attr,
        attr "name" : Object readonly = install_attr,
        attr "value" : Object = install_attr,
        attr "ownerElement" : Object readonly nullable = install_attr,
        attr "specified" : Object readonly = install_attr
    }
}

//! `core/xml/xml_serializer.rs` -- DOM Parsing's XML serialization
//! (<https://w3c.github.io/DOM-Parsing/#dfn-xml-serialization>), over the arena, as Blink's `MarkupAccumulator`
//! runs it for `XMLSerializer.serializeToString` and for the `innerHTML` / `outerHTML` of a node in an XML
//! document.
//!
//! It answered the HTML serialization through the page-visible `outerHTML` / `nodeName` / `nodeType` getters:
//! no `xmlns` on the root element, `<br>` not `<br />`, `<rect></rect>` not `<rect/>`, text unescaped, a comment
//! as its bare data, a doctype or a CDATA section as nothing. Measured on stock Chrome 150 (2026-10-09), and
//! what is here:
//!
//! * the namespace bookkeeping of the spec -- the namespace prefix map, the local prefixes, the inherited
//!   namespace -- so an HTML element at the root gains `xmlns="http://www.w3.org/1999/xhtml"`, its HTML
//!   children do not, and a null-namespace child of one says `xmlns=""`;
//! * Blink's attribute prefixes: an attribute keeps its own prefix and declares it when the map has none for
//!   its namespace (`xmlns:xlink="..." xlink:href="#a"`), where the spec would generate `ns1`;
//! * Blink's escaping: text `&` `<` `>`; attribute values those, `"`, and TAB / LF / CR as `&#9;` / `&#10;` /
//!   `&#13;`;
//! * an empty element of the HTML namespace closes as `<br />` when void and `<p></p>` when not; any other
//!   empty element as `<rect/>`; a `<template>` serializes its contents.
//!
//! The arena keeps no namespace for an ATTRIBUTE (`setAttributeNS` stores the qualified name), so an
//! attribute's namespace is read off its prefix: `xmlns` / `xmlns:*` are namespace declarations, `xml:*` the
//! XML namespace, `xlink:*` on a non-HTML element XLink, and any other prefix the namespace an in-scope
//! declaration binds it to -- unbound, the name is written as it is. An attribute `setAttributeNS` gave a
//! namespace no declaration names is the one case that differs from Chrome (no `xmlns:z` is invented).

use std::collections::HashMap;

use deno_core::v8;
use korvus_dom::Dom;

use crate::binding::protos::{str_val, throw_type_error};
use crate::core::dom::shared::row_method;
use crate::install::MemberDef;

const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// HTML's void elements, as Blink's `ElementCannotHaveEndTag` lists them for serialization.
const VOID: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input", "keygen", "link",
    "menuitem", "meta", "param", "source", "track", "wbr",
];

/// The spec's namespace prefix map: (namespace, prefix) in the order they were added, `None` the null namespace.
#[derive(Clone)]
struct PrefixMap(Vec<(Option<String>, String)>);

impl PrefixMap {
    /// The map every serialization starts from: the XML namespace bound to `xml`.
    fn initial() -> PrefixMap {
        PrefixMap(vec![(Some(XML_NS.to_string()), "xml".to_string())])
    }
    fn has(&self, ns: &Option<String>, prefix: &str) -> bool {
        self.0.iter().any(|(n, p)| n == ns && p == prefix)
    }
    fn add(&mut self, ns: Option<String>, prefix: String) {
        self.0.push((ns, prefix));
    }
    /// "Retrieving a preferred prefix string": the preferred one when `ns` has it, else the last of `ns`'s.
    fn preferred(&self, ns: &Option<String>, preferred: Option<&str>) -> Option<String> {
        let mut last = None;
        for (n, p) in &self.0 {
            if n != ns {
                continue;
            }
            if Some(p.as_str()) == preferred {
                return Some(p.clone());
            }
            last = Some(p.clone());
        }
        last
    }
    /// The namespace `prefix` is bound to now, if any.
    fn ns_of(&self, prefix: &str) -> Option<Option<String>> {
        self.0.iter().rev().find(|(_, p)| p == prefix).map(|(n, _)| n.clone())
    }
}

/// One attribute as the serializer sees it.
struct Attr {
    ns: Option<String>,
    prefix: Option<String>,
    local: String,
    value: String,
}

struct Ser<'a> {
    dom: &'a Dom,
    qnames: Option<&'a HashMap<u32, (Option<String>, String)>>,
    prefix_index: u32,
    out: String,
}

fn esc_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn esc_attr(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
}

impl<'a> Ser<'a> {
    fn generate_prefix(&mut self, map: &mut PrefixMap, ns: &str) -> String {
        self.prefix_index += 1;
        let p = format!("ns{}", self.prefix_index);
        map.add(Some(ns.to_string()), p.clone());
        p
    }

    /// The element's prefix and local name: what `createElementNS` recorded, else -- for an element an XML
    /// document's parser made -- its qualified name split at the colon; an HTML document's element name is
    /// its local name whole (`<a:b>` in HTML has no prefix).
    fn element_name(&self, id: u32) -> (Option<String>, String) {
        if let Some((p, l)) = self.qnames.and_then(|q| q.get(&id)) {
            return (p.clone(), l.clone());
        }
        let name = self.dom.tag_name(id).unwrap_or_default().to_string();
        if self.dom.is_in_xml(id) {
            if let Some((p, l)) = name.split_once(':') {
                if !p.is_empty() && !l.is_empty() {
                    return (Some(p.to_string()), l.to_string());
                }
            }
        }
        (None, name)
    }

    fn attrs(&self, id: u32, html_element: bool) -> Vec<Attr> {
        self.dom
            .attribute_names(id)
            .into_iter()
            .map(|name| {
                let value = self.dom.get_attribute(id, &name).unwrap_or_default().to_string();
                if name == "xmlns" {
                    return Attr { ns: Some(XMLNS_NS.to_string()), prefix: None, local: name, value };
                }
                match name.split_once(':') {
                    Some(("xmlns", l)) if !l.is_empty() => Attr { ns: Some(XMLNS_NS.to_string()), prefix: Some("xmlns".into()), local: l.into(), value },
                    Some(("xml", l)) if !l.is_empty() => Attr { ns: Some(XML_NS.to_string()), prefix: Some("xml".into()), local: l.into(), value },
                    Some(("xlink", l)) if !l.is_empty() && !html_element => {
                        Attr { ns: Some(XLINK_NS.to_string()), prefix: Some("xlink".into()), local: l.into(), value }
                    }
                    // any other prefix: resolved against the in-scope declarations once they are recorded
                    Some((p, l)) if !p.is_empty() && !l.is_empty() && !html_element => {
                        Attr { ns: None, prefix: Some(p.into()), local: l.into(), value }
                    }
                    _ => Attr { ns: None, prefix: None, local: name, value },
                }
            })
            .collect()
    }

    fn node(&mut self, id: u32, ctx_ns: Option<String>, map: &PrefixMap) {
        match self.dom.node_type(id) {
            1 => self.element(id, ctx_ns, map),
            3 => {
                let t = self.dom.node_value(id).unwrap_or_default();
                esc_text(&mut self.out, &t);
            }
            4 => {
                self.out.push_str("<![CDATA[");
                self.out.push_str(&self.dom.node_value(id).unwrap_or_default());
                self.out.push_str("]]>");
            }
            7 => {
                let (target, data) = self.dom.pi_fields(id);
                self.out.push_str("<?");
                self.out.push_str(&target);
                self.out.push(' ');
                self.out.push_str(&data);
                self.out.push_str("?>");
            }
            8 => {
                self.out.push_str("<!--");
                self.out.push_str(&self.dom.node_value(id).unwrap_or_default());
                self.out.push_str("-->");
            }
            10 => {
                let (name, public_id, system_id) = self.dom.doctype_fields(id);
                self.out.push_str("<!DOCTYPE ");
                self.out.push_str(&name);
                if !public_id.is_empty() {
                    self.out.push_str(" PUBLIC \"");
                    self.out.push_str(&public_id);
                    self.out.push('"');
                } else if !system_id.is_empty() {
                    self.out.push_str(" SYSTEM");
                }
                if !system_id.is_empty() {
                    self.out.push_str(" \"");
                    self.out.push_str(&system_id);
                    self.out.push('"');
                }
                self.out.push('>');
            }
            // a document and a fragment: their children, in order
            _ => {
                for c in self.dom.children(id) {
                    self.node(c, ctx_ns.clone(), map);
                }
            }
        }
    }

    fn element(&mut self, id: u32, ctx_ns: Option<String>, parent_map: &PrefixMap) {
        let mut map = parent_map.clone();
        let ns: Option<String> = self.dom.namespace_uri(id).map(str::to_string);
        let html = ns.as_deref() == Some(XHTML_NS);
        let (el_prefix, local) = self.element_name(id);
        let mut attrs = self.attrs(id, html);
        // "Recording the namespace information": the element's own declarations, into the map and the local
        // prefixes; the default one's value aside.
        let mut local_prefixes: HashMap<String, Option<String>> = HashMap::new();
        let mut local_default: Option<String> = None;
        for a in &attrs {
            if a.ns.as_deref() != Some(XMLNS_NS) {
                continue;
            }
            match &a.prefix {
                None => local_default = Some(a.value.clone()),
                Some(_) => {
                    let def_ns = if a.value.is_empty() { None } else { Some(a.value.clone()) };
                    if def_ns.as_deref() == Some(XML_NS) || map.has(&def_ns, &a.local) {
                        continue;
                    }
                    map.add(def_ns.clone(), a.local.clone());
                    local_prefixes.insert(a.local.clone(), def_ns);
                }
            }
        }
        // an attribute's other prefixes, against the declarations now in scope
        for a in attrs.iter_mut() {
            if a.ns.is_none() {
                if let Some(p) = &a.prefix {
                    match map.ns_of(p) {
                        Some(n) => a.ns = n,
                        None => {
                            a.local = format!("{p}:{}", a.local);
                            a.prefix = None;
                        }
                    }
                }
            }
        }
        let mut inherited = ctx_ns;
        let mut ignore_ns_def = false;
        let qualified: String;
        self.out.push('<');
        if inherited == ns {
            if local_default.is_some() {
                ignore_ns_def = true;
            }
            qualified = if ns.as_deref() == Some(XML_NS) { format!("xml:{local}") } else { local.clone() };
            self.out.push_str(&qualified);
        } else {
            let mut candidate = map.preferred(&ns, el_prefix.as_deref());
            if el_prefix.as_deref() == Some("xmlns") {
                candidate = el_prefix.clone();
            }
            if let Some(c) = candidate {
                qualified = format!("{c}:{local}");
                if let Some(ld) = local_default.as_deref().filter(|ld| *ld != XML_NS) {
                    inherited = if ld.is_empty() { None } else { Some(ld.to_string()) };
                }
                self.out.push_str(&qualified);
            } else if let Some(p) = el_prefix.clone() {
                let p = if local_prefixes.contains_key(&p) {
                    let n = ns.clone().unwrap_or_default();
                    self.generate_prefix(&mut map, &n)
                } else {
                    map.add(ns.clone(), p.clone());
                    p
                };
                qualified = format!("{p}:{local}");
                self.out.push_str(&qualified);
                self.out.push_str(" xmlns:");
                self.out.push_str(&p);
                self.out.push_str("=\"");
                esc_attr(&mut self.out, ns.as_deref().unwrap_or(""));
                self.out.push('"');
                if let Some(ld) = local_default.as_deref() {
                    inherited = if ld.is_empty() { None } else { Some(ld.to_string()) };
                }
            } else if local_default.is_none() || local_default != ns {
                ignore_ns_def = true;
                qualified = local.clone();
                inherited = ns.clone();
                self.out.push_str(&qualified);
                self.out.push_str(" xmlns=\"");
                esc_attr(&mut self.out, ns.as_deref().unwrap_or(""));
                self.out.push('"');
            } else {
                qualified = local.clone();
                inherited = ns.clone();
                self.out.push_str(&qualified);
            }
        }
        // the attributes
        for a in &attrs {
            let mut candidate: Option<String> = None;
            if let Some(ans) = &a.ns {
                candidate = map.preferred(&a.ns, a.prefix.as_deref());
                if ans == XMLNS_NS {
                    // a declaration is written only as the recording kept it: the default one unless this element
                    // already declared its namespace, a prefixed one only if it was recorded here with this value
                    let recorded_here = match local_prefixes.get(&a.local) {
                        Some(v) => v.as_deref().unwrap_or("") == a.value,
                        None => false,
                    };
                    let skip = a.value == XML_NS || (a.prefix.is_none() && ignore_ns_def) || (a.prefix.is_some() && !recorded_here);
                    if skip {
                        continue;
                    }
                    if a.prefix.as_deref() == Some("xmlns") {
                        candidate = Some("xmlns".to_string());
                    }
                } else if candidate.is_none() {
                    // Blink keeps the attribute's own prefix when it is free, and declares it
                    let own = a.prefix.clone().filter(|p| map.ns_of(p).is_none());
                    let c = match own {
                        Some(p) => {
                            map.add(a.ns.clone(), p.clone());
                            p
                        }
                        None => self.generate_prefix(&mut map, ans),
                    };
                    self.out.push_str(" xmlns:");
                    self.out.push_str(&c);
                    self.out.push_str("=\"");
                    esc_attr(&mut self.out, ans);
                    self.out.push('"');
                    candidate = Some(c);
                }
            }
            self.out.push(' ');
            if let Some(c) = &candidate {
                self.out.push_str(c);
                self.out.push(':');
            }
            self.out.push_str(&a.local);
            self.out.push_str("=\"");
            esc_attr(&mut self.out, &a.value);
            self.out.push('"');
        }
        // the children: a template's are its contents'
        let template = html && local == "template";
        let kids = match template.then(|| self.dom.template_content_of(id)).flatten() {
            Some(content) => self.dom.children(content),
            None => self.dom.children(id),
        };
        if kids.is_empty() {
            if html && VOID.contains(&local.as_str()) {
                self.out.push_str(" />");
                return;
            }
            if !html {
                self.out.push_str("/>");
                return;
            }
        }
        self.out.push('>');
        for c in kids {
            self.node(c, inherited.clone(), &map);
        }
        self.out.push_str("</");
        self.out.push_str(&qualified);
        self.out.push('>');
    }
}

/// The XML serialization of `id`: the node itself (`XMLSerializer`, `outerHTML`) or, with `children_only`,
/// its children (`innerHTML`).
pub(crate) fn serialize(dom: &Dom, qnames: Option<&HashMap<u32, (Option<String>, String)>>, id: u32, children_only: bool) -> String {
    let mut s = Ser { dom, qnames, prefix_index: 0, out: String::new() };
    let map = PrefixMap::initial();
    if children_only {
        // the children, each from the null namespace and a fresh map -- Chrome 150's `innerHTML` of an XML
        // document's `<root xmlns="urn:d" xmlns:a="urn:a">` re-declares both on the children
        let html = dom.namespace_uri(id) == Some(XHTML_NS);
        let kids = match (html && dom.tag_name(id) == Some("template")).then(|| dom.template_content_of(id)).flatten() {
            Some(content) => dom.children(content),
            None => dom.children(id),
        };
        for c in kids {
            s.node(c, None, &map);
        }
    } else {
        s.node(id, None, &map);
    }
    s.out
}

/// [`serialize`] for the node `v` names, with the arena and the recorded qualified names borrowed.
fn serialize_value(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let id = match crate::core::dom::shared::document_node_id(scope, v) {
        Some(d) => d,
        None => crate::binding::protos::arg_node_id(scope, v)?,
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.try_borrow::<crate::binding::HostDom>()?.0;
    let id = if id == crate::core::events::shared::ON_DOCUMENT_ID { dom.root() } else { id };
    let q = st.try_borrow::<crate::binding::QNameMap>().map(|q| q.0.borrow());
    Some(serialize(dom, q.as_deref(), id, false))
}

fn cb_xml_serialize(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let node = args.get(0);
    // an Attr serializes as its value, escaped as an attribute value is (DOM Parsing; Chrome 150: `9000`)
    if crate::core::dom::attr::is_attr(scope, node) {
        let Ok(o) = v8::Local::<v8::Object>::try_from(node) else { return };
        let value = crate::core::dom::attr::attr_value_of(scope, o).unwrap_or_default();
        let mut out = String::new();
        esc_attr(&mut out, &value);
        rv.set(str_val(scope, &out));
        return;
    }
    match serialize_value(scope, node) {
        Some(s) => rv.set(str_val(scope, &s)),
        None => throw_type_error(scope, "Failed to execute 'serializeToString' on 'XMLSerializer': parameter 1 is not of type 'Node'."),
    }
}

pub fn install_xml_serializer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "serializeToString" => row_method(scope, proto, member, cb_xml_serialize),
        other => eprintln!("[korvus] XMLSerializer install gap: {other}"),
    }
}

webapi::interface! {
    "XMLSerializer" [value, window] {
        ctor "XMLSerializer" len 0 = stub,
        method "serializeToString" : Object len 1 required 1 = install_xml_serializer
    }
}

// This file declares no op: `serializeToString` is a table row with a Rust body, and the factory op
// that used to hand the callback to `dom/08h-abort-attrs.js` had no other caller.

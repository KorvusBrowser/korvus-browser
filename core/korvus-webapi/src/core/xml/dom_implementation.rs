//! `core/xml/dom_implementation.rs` -- `DOMImplementation`, and the "validate and extract" every
//! qualified-name API shares (was `dom/05-xml.js`'s `makeImpl` + `KB.__validateExtract`).
//!
//! `document.implementation` is per DOCUMENT, not one singleton: the nodes `createDocumentType` and
//! `createDocument` make are owner-marked against the document the implementation came from, so the
//! page's `document.implementation` and a `createHTMLDocument()` result's must not be the same object.
//! An instance therefore carries its owner document's arena root, in a private slot no page key list
//! can see, and `document.implementation` caches one per root so `[SameObject]` holds.

use std::cell::RefCell;
use std::collections::HashMap;

use deno_core::v8;

use crate::binding::protos::{brand_shell_instance, get_prop, throw_type_error, wrap_cached};
use crate::binding::{HostDom, OwnerMap, QNameMap};
use crate::core::dom::document::{doc_root_slot, mint_document_for, DocKind, DocOrigin, PAGE_DOC_ROOT};
use crate::core::dom::shared::row_method;
use crate::install::MemberDef;

const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";
const NS_XMLNS: &str = "http://www.w3.org/2000/xmlns/";
const NS_XHTML: &str = "http://www.w3.org/1999/xhtml";
const NS_SVG: &str = "http://www.w3.org/2000/svg";

fn is_ascii_ws(c: u32) -> bool {
    matches!(c, 9 | 10 | 12 | 13 | 32)
}

/// "Validate and extract" (WHATWG DOM, mirrored from Blink's `Document::ParseQualifiedName` +
/// `HasValidNamespaceForElements`): split a qualified name on the FIRST colon, validate the
/// prefix/local-name characters, and check the namespace against the reserved prefixes.
///
/// `Ok((prefix, localName))` (prefix `None` when there is none), or `Err((message, exception name))`
/// for `InvalidCharacterError` / `NamespaceError`. Shared by `DOMImplementation.createDocument` and
/// `document.createElementNS`, which spec-require identical validation.
pub(crate) fn validate_and_extract(
    namespace: Option<&str>,
    qualified_name: &str,
) -> Result<(Option<String>, String), (String, &'static str)> {
    validate_and_extract_in(namespace, qualified_name, false)
}

/// "Validate and extract" in the ATTRIBUTE context (`createAttributeNS`, `setAttributeNS`): the local
/// name is checked as a [valid attribute local name](attribute_local_name_error), not an element's.
pub(crate) fn validate_and_extract_attr(
    namespace: Option<&str>,
    qualified_name: &str,
) -> Result<(Option<String>, String), (String, &'static str)> {
    validate_and_extract_in(namespace, qualified_name, true)
}

/// The first character that makes `local` not a VALID ELEMENT LOCAL NAME (WHATWG DOM, 2025), `None`
/// when it is one; an empty name is invalid and answers `Some('\0')`. Measured on Chrome 150
/// (2026-10-05): `createElement` refuses `1bad`, `-x`, `.a`, `""`, `a b`, `a/b`, `a>b`, NUL and tab, and
/// takes `:a`, `_a`, `a@b`, `a=b`, `·a`, `é`, U+2028 first.
pub(crate) fn element_local_name_error(local: &str) -> Option<char> {
    let is_alpha = |u: u32| (97..=122).contains(&u) || (65..=90).contains(&u);
    let is_digit = |u: u32| (48..=57).contains(&u);
    let mut it = local.chars();
    let Some(first_c) = it.next() else { return Some('\0') };
    let first = first_c as u32;
    if is_alpha(first) {
        it.find(|&c| {
            let u = c as u32;
            u == 0 || u == 0x2f || u == 0x3e || is_ascii_ws(u)
        })
    } else if first < 0x80 && first != 0x3a && first != 0x5f {
        Some(first_c)
    } else {
        it.find(|&c| {
            let u = c as u32;
            u < 0x80 && !is_alpha(u) && !is_digit(u) && u != 0x2d && u != 0x2e && u != 0x3a && u != 0x5f
        })
    }
}

/// The first character that makes `name` not a VALID ATTRIBUTE LOCAL NAME (WHATWG DOM, 2025): any
/// ASCII whitespace, NUL, `/`, `>` or `=`; an empty name answers `Some('\0')`. Chrome 150 (2026-10-05):
/// `setAttribute` / `toggleAttribute` / `createAttribute` take `1bad`, `-x`, `.a`, `a@b`, `a<b`, `·a`
/// and refuse `""`, `a b`, `a=b`, `a/b`, `a>b`, NUL and tab.
pub(crate) fn attribute_local_name_error(name: &str) -> Option<char> {
    if name.is_empty() {
        return Some('\0');
    }
    name.chars().find(|&c| {
        let u = c as u32;
        u == 0 || u == 0x2f || u == 0x3e || u == 0x3d || is_ascii_ws(u)
    })
}

fn validate_and_extract_in(
    namespace: Option<&str>,
    qualified_name: &str,
    attribute: bool,
) -> Result<(Option<String>, String), (String, &'static str)> {
    let q = qualified_name;
    let bad_name = |why: &str| -> (String, &'static str) {
        (format!("The qualified name provided ('{q}') {why}"), "InvalidCharacterError")
    };
    let bad_ns = || -> (String, &'static str) {
        (
            format!(
                "The namespace URI provided ('{}') is not valid for the qualified name provided ('{q}').",
                namespace.unwrap_or("")
            ),
            "NamespaceError",
        )
    };
    if q.is_empty() {
        // Chrome 150's wording, measured: "The qualified name provided is empty." (no quoted name).
        return Err(("The qualified name provided is empty.".to_string(), "InvalidCharacterError"));
    }
    let chars: Vec<char> = q.chars().collect();
    let colon = chars.iter().position(|&c| c == ':');
    let (prefix, local): (Option<String>, String) = match colon {
        // The FIRST colon splits; later colons stay in the local name.
        Some(i) => (Some(chars[..i].iter().collect()), chars[i + 1..].iter().collect()),
        None => (None, q.to_string()),
    };
    if let Some(p) = &prefix {
        if p.is_empty() {
            return Err(bad_name("has an empty namespace prefix."));
        }
        for c in p.chars() {
            let u = c as u32;
            if u == 0 || u == 0x2f || u == 0x3e || is_ascii_ws(u) {
                return Err(bad_name(&format!("contains the invalid character '{c}'.")));
            }
        }
    }
    if local.is_empty() {
        return Err(bad_name("has an empty local name."));
    }
    // valid element local name (dom.spec.whatwg.org/#valid-element-local-name), or in the attribute
    // context a valid attribute local name
    let bad = if attribute { attribute_local_name_error(&local) } else { element_local_name_error(&local) };
    if let Some(c) = bad {
        return Err(bad_name(&format!("contains the invalid character '{c}'.")));
    }
    // namespace checks (DOM Level 2/3, as Blink's HasValidNamespaceForElements)
    let ns_null = namespace.is_none_or(str::is_empty);
    if prefix.is_some() && ns_null {
        return Err(bad_ns());
    }
    if prefix.as_deref() == Some("xml") && namespace != Some(NS_XML) {
        return Err(bad_ns());
    }
    // An attribute's xmlns refusal is worded by Blink as "'<ns>' is an invalid namespace for attributes."
    // (Chrome 150, measured: `setAttributeNS('urn:x', 'xmlns', '')`).
    let bad_xmlns = || -> (String, &'static str) {
        if attribute {
            (format!("'{}' is an invalid namespace for attributes.", namespace.unwrap_or("")), "NamespaceError")
        } else {
            bad_ns()
        }
    };
    if prefix.as_deref() == Some("xmlns") || (prefix.is_none() && local == "xmlns") {
        if namespace != Some(NS_XMLNS) {
            return Err(bad_xmlns());
        }
    } else if namespace == Some(NS_XMLNS) {
        return Err(bad_xmlns());
    }
    Ok((prefix, local))
}

// ---- the instance -----------------------------------------------------------------------------

/// The private slot every `DOMImplementation` carries: the DOCUMENT it belongs to, by identity, so a
/// node it creates records the very object `node.ownerDocument` must be `===` to. Not a property --
/// a `v8::Private`, which no key list, no `Object.keys` and no `getOwnPropertyNames` can see.
fn owner_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "korvus_impl_owner").unwrap();
    v8::Private::for_api(scope, Some(name))
}

/// `document.implementation`, one per document root ([SameObject]).
#[derive(Default)]
pub struct DomImplementations(RefCell<HashMap<u32, v8::Global<v8::Object>>>);

/// The owner document of `this` DOMImplementation, for owner-marking a created node. `None` for the
/// PAGE's implementation -- the page document is every node's default owner and needs no record.
fn owner_document<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let key = owner_key(scope);
    let owner = obj.get_private(scope, key).filter(|v| v.is_object())?;
    if doc_root_slot(scope, owner) == PAGE_DOC_ROOT {
        return None;
    }
    Some(owner)
}

/// Mark `id` as owned by the implementation's document (a no-op for the page's).
fn mark_owner(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, id: u32) {
    let Some(doc) = owner_document(scope, this) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = v8::Global::new(scope, doc);
    let st = op_state.borrow();
    st.borrow::<OwnerMap>().0.borrow_mut().insert(id, g);
}

/// `document.implementation` -- THIS document's own DOMImplementation, cached per root.
pub(crate) fn cb_doc_implementation(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DomImplementations>().is_none() {
            st.put(DomImplementations::default());
        }
    }
    let cached = op_state.borrow().borrow::<DomImplementations>().0.borrow().get(&slot).cloned();
    if let Some(g) = cached {
        let l = v8::Local::new(scope, &g);
        rv.set(l.into());
        return;
    }
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(obj) = new_implementation(scope, this) else { return };
    let g = v8::Global::new(scope, obj);
    op_state.borrow().borrow::<DomImplementations>().0.borrow_mut().insert(slot, g);
    rv.set(obj.into());
}

/// A `DOMImplementation` for `owner`: a real instance on the interface's prototype, branded, carrying
/// its owner document in the private slot.
fn new_implementation<'s>(scope: &mut v8::PinScope<'s, '_>, owner: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "DOMImplementation")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    // from the interface's template
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, obj);
    let key = owner_key(scope);
    obj.set_private(scope, key, owner);
    Some(obj)
}

// ---- the four members --------------------------------------------------------------------------

/// `hasFeature()` -- true for everything, forever (DOM: "useless; always returns true").
fn cb_has_feature(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::Boolean::new(scope, true).into());
}

/// `createDocumentType(qualifiedName, publicId, systemId)`: a detached DocumentType. The name must be
/// a valid doctype name (no ASCII whitespace, NUL, or '>'); publicId/systemId are free-form strings
/// (null stringifies to "null" per WebIDL DOMString conversion). -- WPT
/// DOMImplementation-createDocumentType (the full valid/invalid name table).
fn cb_create_document_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 3 {
        throw_type_error(scope, &format!("Failed to execute 'createDocumentType' on 'DOMImplementation': 3 arguments required, but only {} present.", args.length()));
        return;
    }
    let q = args.get(0).to_rust_string_lossy(scope);
    for c in q.chars() {
        let u = c as u32;
        if u == 0 || u == 0x3e || is_ascii_ws(u) {
            crate::binding::protos::throw_dom(
                scope,
                &format!("The provided doctype name ('{q}') contains an invalid character."),
                "InvalidCharacterError",
            );
            return;
        }
    }
    let public_id = args.get(1).to_rust_string_lossy(scope);
    let system_id = args.get(2).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_doctype(&q, &public_id, &system_id);
    let node = wrap_cached(scope, &op_state, id);
    mark_owner(scope, args.this().into(), id);
    rv.set(node.into());
}

/// `createHTMLDocument(title)`: an HTML document with doctype/html/head/body. A MISSING title argument
/// means NO `<title>` element at all (undefined would otherwise stringify into one); a present title
/// becomes a text node, so its exact characters (\r, \t) survive -- parsing a `<title>` markup string
/// would normalize them away. -- WPT DOMImplementation-createHTMLDocument (title table + structure).
fn cb_create_html_document(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // WebIDL `optional DOMString title`: undefined counts as ABSENT (Blink's null String -> no
    // <title> element); explicit null stringifies to "null" and gets a title. -- WPT title table.
    let has_title = args.length() > 0 && !args.get(0).is_undefined();
    let title = if has_title { args.get(0).to_rust_string_lossy(scope) } else { String::new() };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = op_state
        .borrow_mut()
        .borrow_mut::<HostDom>()
        .0
        .parse_into("<!doctype html><html><head></head><body></body></html>");
    // Chrome 152 builds the tree (`DOMImplementation::createHTMLDocument` under the stable
    // `CreateHTMLDocumentReadyState`): no parser is left open, so a later `write()` opens the document afresh
    // (`core/dom/document.rs`, `CreatedDocParsers`).
    let doc = mint_document_for(scope, root, DocKind::Html, DocOrigin::CreateHtml);
    if has_title {
        let ids = {
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            let t = dom.create_element("title");
            let text = dom.create_text_node(&title);
            dom.append_child(t, text);
            (t, text)
        };
        let head = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            dom.query_selector_all_in(root, "head").ok().and_then(|v| v.first().copied())
        };
        if let Some(head) = head {
            op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(head, ids.0);
        }
    }
    // The parsed subtree belongs to this document now (ownerDocument on every node in it).
    let doc_v: v8::Local<v8::Value> = doc.into();
    let all: Vec<u32> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut out: Vec<u32> = dom.children(root);
        let mut i = 0;
        while i < out.len() {
            out.extend(dom.children(out[i]));
            i += 1;
        }
        out
    };
    let g = v8::Global::new(scope, doc_v);
    {
        let st = op_state.borrow();
        let mut map = st.borrow::<OwnerMap>().0.borrow_mut();
        for id in all {
            map.insert(id, g.clone());
        }
    }
    rv.set(doc.into());
}

/// `createDocument(namespace, qualifiedName, doctype)`: an XML document. qualifiedName is
/// `[LegacyNullToEmptyString]` (null -> "", meaning NO root element); a non-empty name is validated via
/// the shared "validate and extract" (the same table createElementNS uses); the doctype (a DocumentType
/// or null -- anything else is a WebIDL TypeError) is appended BEFORE the root element, and appending
/// ADOPTS it (ownerDocument becomes this document). -- WPT DOMImplementation-createDocument.
fn cb_create_document(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 2 {
        throw_type_error(scope, &format!("Failed to execute 'createDocument' on 'DOMImplementation': 2 arguments required, but only {} present.", args.length()));
        return;
    }
    let ns_v = args.get(0);
    let ns: Option<String> = if ns_v.is_null_or_undefined() { None } else { Some(ns_v.to_rust_string_lossy(scope)) };
    // [LegacyNullToEmptyString]: ONLY null maps to "" (no root element). undefined is a real DOMString
    // -> "undefined" (WPT: createDocument(null, undefined, null) yields an <undefined>).
    let q_v = args.get(1);
    let q = if q_v.is_null() { String::new() } else { q_v.to_rust_string_lossy(scope) };
    let dt_v = args.get(2);
    let doctype_id = if dt_v.is_null_or_undefined() {
        None
    } else {
        match crate::binding::protos::unwrap_id(scope, dt_v) {
            Some(id) => {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let is_dt = op_state.borrow().borrow::<HostDom>().0.node_type(id) == 10;
                if !is_dt {
                    throw_type_error(scope, "Failed to execute 'createDocument' on 'DOMImplementation': The provided value is not of type '(DocumentType or null)'.");
                    return;
                }
                Some(id)
            }
            None => {
                throw_type_error(scope, "Failed to execute 'createDocument' on 'DOMImplementation': The provided value is not of type '(DocumentType or null)'.");
                return;
            }
        }
    };
    // The content type follows the NAMESPACE the document was created with (Blink creates XHTML/SVG
    // XMLDocuments for those namespaces), not the root element -- it must read right even when
    // qualifiedName is "" and there is no element at all. -- WPT "metadata" subtests.
    let ct = match ns.as_deref() {
        Some(NS_XHTML) => "application/xhtml+xml",
        Some(NS_SVG) => "image/svg+xml",
        _ => "application/xml",
    };
    let mut element: Option<u32> = None;
    if !q.is_empty() {
        let (prefix, local) = match validate_and_extract(ns.as_deref(), &q) {
            Ok(v) => v,
            Err((msg, name)) => {
                crate::binding::protos::throw_dom(scope, &format!("Failed to execute 'createDocument' on 'DOMImplementation': {msg}"), name);
                return;
            }
        };
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        // The "" namespace IS null, which is what `ElementNs::from_uri` makes of it.
        let id = {
            let mut st = op_state.borrow_mut();
            st.borrow_mut::<HostDom>().0.create_element_ns(&q, korvus_dom::ElementNs::from_uri(ns.as_deref()))
        };
        {
            let st = op_state.borrow();
            st.borrow::<QNameMap>().0.borrow_mut().insert(id, (prefix, local));
        }
        element = Some(id);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_xml_document();
    let doc = mint_document_for(scope, root, DocKind::Xml(ct.to_string()), crate::core::dom::document::DocOrigin::CreatedAboutBlank);
    let doc_v: v8::Local<v8::Value> = doc.into();
    let g = v8::Global::new(scope, doc_v);
    if let Some(dt) = doctype_id {
        op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(root, dt);
        op_state.borrow().borrow::<OwnerMap>().0.borrow_mut().insert(dt, g.clone());
    }
    if let Some(el) = element {
        op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(root, el);
        op_state.borrow().borrow::<OwnerMap>().0.borrow_mut().insert(el, g.clone());
        // The namespace and the qualified name were recorded at creation, above, so the wrap reads the
        // right interface -- and rehomes it into the document's realm. A second `node_proto` here put
        // the PAGE's prototype back: a frame's `createDocument(...).documentElement` was not the frame's
        // `Element` (Chrome 150: it is, measured 2026-09-28).
        wrap_cached(scope, &op_state, el);
    }
    rv.set(doc.into());
}

pub fn install_dom_implementation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "createDocument" => row_method(scope, proto, member, cb_create_document),
        "createDocumentType" => row_method(scope, proto, member, cb_create_document_type),
        "createHTMLDocument" => row_method(scope, proto, member, cb_create_html_document),
        "hasFeature" => row_method(scope, proto, member, cb_has_feature),
        other => eprintln!("[korvus] DOMImplementation install gap: {other}"),
    }
}

webapi::interface! {
    "DOMImplementation" [interface, window] {
        ctor "DOMImplementation" len 0 = stub,
        method "createDocument" : Object len 2 required 2 = install_dom_implementation,
        method "createDocumentType" : Object len 3 required 3 = install_dom_implementation,
        method "createHTMLDocument" : Object len 0 = install_dom_implementation,
        method "hasFeature" : Object len 0 = install_dom_implementation
    }
}

// `op_doc_implementation`, the JS-facing builder for `document.implementation`'s accessor, IS GONE.
// Its own doc already said the accessor is "the `Document` table's row body", and that
// row (`core/dom/document.rs`, member "implementation") names [`cb_doc_implementation`] directly, so
// the op wrapped a callback that already had its real caller. This file declares no ops.

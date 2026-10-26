//! `core/dom/element.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use std::ffi::CStr;

use deno_core::{v8, GarbageCollected, OpState};
use korvus_dom::{AttachShadowError, NodeId, ShadowRootMode, ShadowSlotAssignment};
use crate::binding::{HostDom, QNameMap};
use crate::core::dom::notify_childlist_batch;
use crate::core::dom::shared::{remove_attribute_notify, set_attribute_notify};
use crate::core::css::css_style_declaration::{priv_get, CSD_CACHE};
use crate::modules::crypto::shared::{crypto_num};
use crate::core::dom::attr::{make_attr};
// The seven `aria*Elements` rows' body. It is NOT a `reflect_*` kit -- an ARIA element
// reflection holds ELEMENT REFERENCES in an internal slot, not a content attribute, which is what
// keeps `o.ariaLabelledByElements = [c]` reading back `c` itself.
use crate::core::dom::aria_reflection::install_aria_element_reflection;
use crate::binding::protos::{arg_node_id, build_fn_data, call_method, get_prop, require_args, set_bool, str_val, throw_dom, unwrap_id, wrap_cached};
use crate::core::dom::document::{JsDocument};
use crate::binding::protos::throw_type_error;

pub(crate) fn cb_el_click<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    // HTML `click()`, step 1: a DISABLED form control does nothing -- no event, so no activation either (WPT
    // `Event-dispatch-click`'s "disabled checkbox still has activation behavior" cases, and a label's click,
    // which reaches its control through `click()`; Chrome 150 passes both files' cases).
    if is_disabled_form_control(scope, this) {
        return;
    }
    let is_fn = |v: &v8::Local<v8::Value>| v.is_function();
    let ctor = crate::binding::protos::iface_object(scope, "PointerEvent").filter(is_fn)
        .or_else(|| crate::binding::protos::iface_object(scope, "MouseEvent").filter(is_fn))
        .or_else(|| crate::binding::protos::iface_object(scope, "Event"));
    let ctor = match ctor.and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { Some(c) => c, None => return };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true); set_bool(scope, init, "cancelable", true);
    // Blink's script-made simulated click (`SimulatedEventUtil`, kFromScript): composed, the window as `view`,
    // and the reserved non-pointer id -1 -- without `composed` a `click()` inside a shadow tree would stop at its
    // shadow root now that the dispatch walks shadow roots as DOM says (FIXER C6).
    set_bool(scope, init, "composed", true);
    let view: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    crate::binding::protos::set_prop(scope, init, "view", view);
    let pid: v8::Local<v8::Value> = v8::Integer::new(scope, -1).into();
    crate::binding::protos::set_prop(scope, init, "pointerId", pid);
    let ty = str_val(scope, "click");
    if let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) {
        let evv: v8::Local<v8::Value> = ev.into();
        let not_cancelled = crate::binding::protos::engine_call(scope, this.into(), "dispatchEvent", &[evv]).map(|v| v.boolean_value(scope)).unwrap_or(true);
        // THE ACTIVATION BEHAVIOUR runs in the dispatch itself
        // (`core/html/activation.rs`), for this click and for any other: a submit button submits, a link is
        // followed, a checkbox toggles, a label reaches its control.
        let _ = not_cancelled;
    }
}

/// A form control -- `button`, `input`, `select`, `textarea`, `fieldset` -- that is `:disabled`.
fn is_disabled_form_control(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    let Some(id) = el_id_of(scope, el) else { return false };
    let control = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| {
            h.0.is_html_ns(id)
                && h.0.tag_name(id).is_some_and(|t| ["button", "input", "select", "textarea", "fieldset"].iter().any(|c| t.eq_ignore_ascii_case(c)))
        })
    };
    if !control {
        return false;
    }
    let sel = str_val(scope, ":disabled");
    crate::binding::protos::pristine_call(scope, el.into(), "Element.matches", &[sel]).is_some_and(|v| v.boolean_value(scope))
}

pub(crate) fn el_id_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id)
}

fn token_list_for(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, attr: &str, rv: &mut v8::ReturnValue) {
    let this = args.this();
    let id = match el_id_of(scope, this) { Some(i) => i, None => return };
    // [SameObject]: `el.classList` must return the SAME DOMTokenList each time. Cache it on the element
    // wrapper (stable via NodeCache) under a private; create it on first access. The list is LIVE -- its
    // indices come from an interceptor over the arena, so the cached object always reports the current
    // token set and there is nothing to reseed.
    // The key's NAME without a `format!` per read for the list every page reads.
    let key_name = match attr {
        "class" => std::borrow::Cow::Borrowed("korvus_dtl_cache_class"),
        other => std::borrow::Cow::Owned(format!("korvus_dtl_cache_{other}")),
    };
    let cache_key = crate::binding::protos::private_key(scope, &key_name);
    let cached = this.get_private(scope, cache_key).filter(|v| v.is_object()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let obj = match cached {
        Some(o) => o,
        None => {
            // made in the element's realm, whichever realm's getter reads it first (`collections::in_relevant_realm`)
            let made = crate::core::dom::collections::in_relevant_realm(scope, this, |scope| crate::core::dom::collections::make_token_list(scope, id, attr));
            let Some(o) = made else { return };
            this.set_private(scope, cache_key, o.into());
            o
        }
    };
    rv.set(obj.into());
}

/// `element.classList` — the `class` attribute's token list.
pub(crate) fn cb_el_class_list(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    token_list_for(scope, &args, "class", &mut rv);
}

/// A token list over ANY content attribute: `link.relList`, `link.sizes`, `iframe.sandbox`,
/// `output.htmlFor`, `td.headers`. `classList` was the only one this engine could build until now,
/// because the whole DOMTokenList implementation had `"class"` written into it; the attribute is now
/// a slot on the list object, and this is the accessor that stamps it.
///
/// `[SameObject]` still holds per attribute: the cache private is keyed by attribute name, so
/// `el.relList === el.relList` while `el.relList !== el.classList`.
pub(crate) fn cb_el_token_list(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    token_list_for(scope, &args, &attr, &mut rv);
}

pub(crate) fn priv_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let key = crate::binding::protos::private_key(scope, name);
    o.set_private(scope, key, v);
}

pub(crate) fn cb_el_namespace_uri(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_null(); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // One read of the one place the namespace lives: the arena node. A parser-created `<rect>` inside
    // `<svg>` answers the SVG namespace here for the same reason a `createElementNS` one does.
    let uri = { let st = op_state.borrow(); st.borrow::<HostDom>().0.namespace_uri(id).map(str::to_string) };
    match uri {
        Some(uri) => { let v = str_val(scope, &uri); rv.set(v); }
        None => rv.set_null(),
    }
}

/// Element.prefix: the qualified-name prefix recorded by "validate and extract" (createElementNS /
/// DOMImplementation.createDocument), or null when the element has none (the common case: HTML
/// elements and unprefixed names). Absent QNameMap entry = no prefix, same as before the map existed.
pub(crate) fn cb_el_prefix(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_null(); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let q = { let st = op_state.borrow(); let m = st.borrow::<QNameMap>(); let r = m.0.borrow().get(&id).cloned(); r };
    let p = match q {
        Some((p, _)) => p,
        // an XML document's parser keeps the qualified name (`<a:c>`: prefix `a`)
        None => {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            dom.tag_name(id).and_then(|n| crate::core::html::html_element::xml_qualified_split(dom, id, n)).map(|(p, _)| p.to_string())
        }
    };
    match p { Some(p) => { let v = str_val(scope, &p); rv.set(v); } None => rv.set_null() }
}

pub(crate) fn cb_el_set_attribute_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => return };
    // each argument converted in turn, and a throw ends the call there ([`ns_arg`])
    let Some(ns) = ns_arg(scope, args.get(0)) else { return };
    let Some(qname) = args.get(1).to_string(scope) else { return };
    let qname = qname.to_rust_string_lossy(scope);
    // the `TrustedType` overload: any Trusted value is its own string, nothing on it called
    let Some((value, value_kind)) = crate::core::trusted_types::tables::trusted_or_string(scope, args.get(2)) else { return };
    // "Validate and extract" (attribute context) first: korvus set any name at all (`""`, `"a b"`,
    // `":a"`), where Chrome 150 throws Blink's sentence for each (measured 2026-10-05).
    if let Err((msg, name)) = crate::core::xml::dom_implementation::validate_and_extract_attr(ns.as_deref(), &qname) {
        throw_dom(scope, &format!("Failed to execute 'setAttributeNS' on 'Element': {msg}"), name);
        return;
    }
    let local = qname.rsplit_once(':').map_or(qname.as_str(), |(_, l)| l).to_string();
    let Some(value) = attribute_trusted_value(scope, id, &local, ns.as_deref(), value, value_kind, "Failed to execute 'setAttributeNS' on 'Element'") else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // DOM's "set an attribute value": the attribute already in this namespace with this local name takes the value,
    // under the prefix it has (`setAttributeNS(XL, 'q:href', v)` over a parsed `xlink:href` changes that one; Chrome
    // 152, 2026-10-23, probe r2/l3/r1/www/c4.html: `href` in XLink, old value its own, the list `xlink:href=#q`), where
    // korvus added a second attribute `q:href`. A null namespace keeps the name as it comes: it has no prefix.
    let existing = ns.as_deref().and_then(|n| attr_by_ns(&op_state.borrow(), id, Some(n), &local));
    let key = existing.unwrap_or(qname);
    // the record names the attribute by its local name in its namespace (`shared::notify_attributes`)
    crate::core::dom::shared::set_attribute_notify_as(scope, &op_state, id, &key, Some((&local, ns.as_deref())), &value);
}

/// A `namespace` argument (`DOMString?`): `Some(None)` for null and undefined, and for `""`, which DOM's "validate
/// and extract" makes null; `None` when its ToString throws, the exception left pending for the caller to return
/// with. `removeAttributeNS` converted it with a lossy `""` and went on to remove `localName` in no namespace -- with
/// a record and a reaction that swallowed the exception -- and `setAttributeNS` set it. Chrome 152 (2026-10-23, probe
/// r2/l3/r1/www/b4b.html, a namespace whose `toString` throws): both throw, before `localName` is read, and change
/// nothing.
fn ns_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Option<String>> {
    if v.is_null_or_undefined() {
        return Some(None);
    }
    let s = v.to_string(scope)?.to_rust_string_lossy(scope);
    Some(Some(s).filter(|s| !s.is_empty()))
}
pub(crate) fn cb_el_get_attribute_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_null(); return; } };
    let local = args.get(1).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
    // Match by local name against either the bare name or a prefixed `p:local` attribute.
    let found = dom.attribute_names(id).into_iter().find(|n| n == &local || n.rsplit(':').next() == Some(local.as_str()))
        .and_then(|n| dom.get_attribute(id, &n).map(str::to_string));
    match found { Some(v) => { let vv = str_val(scope, &v); rv.set(vv); } None => rv.set_null() }
}

/// The attribute whose local name is `local`, matching either a bare name or a `prefix:local` one --
/// the same rule `getAttributeNS` uses, factored out so has/remove cannot drift from get.
fn attr_by_local(dom: &korvus_dom::Dom, id: u32, local: &str) -> Option<String> {
    dom.attribute_names(id)
        .into_iter()
        .find(|n| n == local || n.rsplit(':').next() == Some(local))
}

/// `Element.hasAttributes()` — whether this element has any attribute at all.
pub(crate) fn cb_el_has_attributes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_bool(false); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    rv.set_bool(!st.borrow::<HostDom>().0.attribute_names(id).is_empty());
}

/// `Element.hasAttributeNS(namespace, localName)`.
pub(crate) fn cb_el_has_attribute_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Element", "hasAttributeNS") { return; }
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_bool(false); return; } };
    let local = args.get(1).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    rv.set_bool(attr_by_local(&st.borrow::<HostDom>().0, id, &local).is_some());
}

/// `Element.removeAttributeNS(namespace, localName)` — returns undefined either way, as the IDL says.
///
/// A removal like any other ([`crate::core::dom::shared::remove_attribute_notify_as`]): it went straight to the
/// arena, so it queued no MutationObserver record and ran no reaction. Chrome 152 (2026-10-23, probe
/// r2/l3/www/b4.html): `removeAttributeNS('urn:x', 'foo')` of `setAttributeNS('urn:x', 'p:foo', '1')` queues
/// `foo` in `urn:x` with old value `"1"`; `removeAttributeNS(null, 'foo')` of a `foo` queues `foo` with `"1"`;
/// an absent one queues nothing.
///
/// It removes the attribute in THAT namespace with that local name ([`attr_by_ns`]) and nothing else. It took the
/// first whose local name matched in any namespace, which once removals were heard ran a record, a reaction, a
/// canvas reset and a frame navigation for a removal Chrome never makes. Chrome 152 (2026-10-23, probes
/// r2/l3/r1/www/a4.html, b4.html, c4.html): `removeAttributeNS('urn:x', 'foo')` of a plain `foo`, `(null, 'foo')`
/// of `p:foo` in `urn:x`, `(null, 'href')` of a parsed `xlink:href`, `(XL, 'xlink:href')`, `(XL, 'href')` of a
/// `setAttribute('xlink:href')` on an SVG `<use>`: each removes nothing and queues nothing; `(null, 'href')` on
/// `<use xlink:href=#a href=#b>` removes `href`, old value `"#b"`.
pub(crate) fn cb_el_remove_attribute_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Element", "removeAttributeNS") { return; }
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => return };
    // each argument converted in turn, and a throw ends the call there ([`ns_arg`])
    let Some(ns) = ns_arg(scope, args.get(0)) else { return };
    let Some(local) = args.get(1).to_string(scope) else { return };
    let local = local.to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let name = { let st = op_state.borrow(); attr_by_ns(&st, id, ns.as_deref(), &local) };
    if let Some(name) = name {
        crate::core::dom::shared::remove_attribute_notify_as(scope, &op_state, id, &name, Some((&local, ns.as_deref())));
    }
}

// ── attribute namespaces ─────────────────────────────────────────────────────────────────────────

/// The namespace of each attribute a script made that the arena cannot tell from its qualified name, by element and
/// qualified name: `setAttributeNS`'s (`p:foo` or `foo` in `urn:x`, `xlink:href` in XLink on a `<div>`), and a
/// `setAttribute` of a prefixed name -- or `xmlns` -- on an element whose parser would have put that prefix in a
/// namespace (an SVG `<use>`'s `xlink:href`, an XML document's `p:foo`): no namespace, as DOM's `setAttribute` makes
/// every attribute. Every other attribute is in the namespace [`attr_name_parts`] reads off its name and owner, as
/// the parsers decided it.
///
/// The arena keeps an attribute as its qualified name and nothing else, so `removeAttributeNS` matched a local name
/// in any namespace and the records of `setAttribute` / `removeAttribute` named a namespaced attribute by its
/// qualified name in none. Chrome 152 (2026-10-23, probes r2/l3/r1/www/b4.html, c4.html): `setAttribute('p:foo')`,
/// `toggleAttribute('p:foo')` and an `Attr`'s `value` over `setAttributeNS('urn:x', 'p:foo')` queue `foo` in `urn:x`;
/// `removeAttribute('xlink:href')` of a parsed one queues `href` in XLink.
///
/// [`attr_name_parts`]: crate::core::dom::attr::attr_name_parts
#[derive(Default)]
pub(crate) struct AttrNamespaces {
    /// The arena ([`korvus_dom::Dom::instance`]) the ids below are of: a new document numbers its nodes from zero.
    dom: u64,
    /// Element id -> (qualified name, namespace), `None` the null namespace. A write asks whether ITS element keeps
    /// the name ([`ns_before`]), under the borrow it takes for the old value anyway: a thread-wide "a bare name was
    /// ever kept" flag sent every later `setAttribute` of a page down the table's path after one d3
    /// `attr('xlink:href', v)` (`setAttributeNS(XLINK, 'href', v)`). A forgotten name leaves its slot empty (no
    /// attribute name is), for the next one the element keeps to fill without allocating: a page that sets and
    /// removes a namespaced attribute in a loop allocated a list, a name and a namespace and freed them on each turn.
    of: crate::install::templates::FxMap<u32, Vec<(String, Option<std::rc::Rc<str>>)>>,
    /// Every namespace kept, once: a page uses a handful, and each slot shares its copy.
    nss: Vec<std::rc::Rc<str>>,
}

thread_local! {
    /// Whether this thread ever made an [`AttrNamespaces`]: nearly no page does, and a clone then copies nothing
    /// ([`clone_attr_namespaces`]).
    static ATTR_NS_KEPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A bit per element id, modulo 4096, set for every element [`AttrNamespaces`] was given a name for and never
    /// cleared: a clear bit answers "this element keeps none" with no table lookup. The `OpState` type-map lookup and
    /// the hash probe it saves were `+2.8 %` of a `setAttribute('data-k', i)` loop on a page with one
    /// `setAttributeNS('urn:x', 'foo')` (2026-10-23, r2/l3/f2/perf: 4464 -> 4344 instructions a write, what it costs
    /// on a page that kept none).
    static KEPT_IDS: [std::cell::Cell<u64>; 64] = const { [const { std::cell::Cell::new(0) }; 64] };
}

/// Whether element `id` may have a name in [`AttrNamespaces`] ([`KEPT_IDS`]).
#[inline(always)]
fn may_keep(id: u32) -> bool {
    KEPT_IDS.with(|b| b[(id as usize >> 6) & 63].get() >> (id & 63) & 1 != 0)
}

/// Mark element `id` as one that may have a name in [`AttrNamespaces`].
fn mark_kept(id: u32) {
    KEPT_IDS.with(|b| {
        let w = &b[(id as usize >> 6) & 63];
        w.set(w.get() | 1 << (id & 63));
    });
}

/// Whether `name` reads as namespaced on some element ([`attr_name_parts`]: a prefix, or `xmlns`).
///
/// [`attr_name_parts`]: crate::core::dom::attr::attr_name_parts
#[inline]
fn prefixed(name: &str) -> bool {
    // a loop the call inlines, not `memchr`'s call: an attribute name is a few bytes, and every `setAttribute` asks
    name.bytes().any(|b| b == b':') || name == "xmlns"
}

/// What [`AttrNamespaces`] holds for `id`'s attribute `name`, `dom` being `st`'s arena: `Some(namespace)` for one it
/// keeps. One bit read for an element never given one.
#[inline]
fn kept_ns<'a>(st: &'a OpState, dom: &korvus_dom::Dom, id: u32, name: &str) -> Option<&'a Option<std::rc::Rc<str>>> {
    if !may_keep(id) {
        return None;
    }
    let t = st.try_borrow::<AttrNamespaces>().filter(|t| t.dom == dom.instance())?;
    t.of.get(&id)?.iter().find(|(n, _)| n == name).map(|(_, ns)| ns)
}

/// The local name of an attribute `setAttributeNS` named `name`, as it cut it (`cb_el_set_attribute_ns`).
fn local_of(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// The table, made if there is none and emptied first when the document it numbered is gone.
fn ns_table(st: &mut OpState) -> &mut AttrNamespaces {
    let dom = st.borrow::<HostDom>().0.instance();
    if st.try_borrow::<AttrNamespaces>().is_none() {
        st.put(AttrNamespaces::default());
        ATTR_NS_KEPT.with(|k| k.set(true));
    }
    let t = st.borrow_mut::<AttrNamespaces>();
    if t.dom != dom {
        t.of.clear();
        t.nss.clear();
        t.dom = dom;
    }
    t
}

/// Keep `ns` as the namespace of `id`'s attribute `name`.
fn keep_ns(st: &mut OpState, id: u32, name: &str, ns: Option<&str>) {
    let t = ns_table(st);
    let ns = ns.map(|ns| match t.nss.iter().find(|k| ***k == *ns) {
        Some(k) => k.clone(),
        None => {
            let k = std::rc::Rc::<str>::from(ns);
            // bounded: a page minting namespaces gets each its own copy
            if t.nss.len() < 64 {
                t.nss.push(k.clone());
            }
            k
        }
    });
    mark_kept(id);
    let list = t.of.entry(id).or_default();
    if let Some(e) = list.iter_mut().find(|(n, _)| n == name) {
        e.1 = ns;
    } else if let Some(e) = list.iter_mut().find(|(n, _)| n.is_empty()) {
        e.0.push_str(name);
        e.1 = ns;
    } else {
        list.push((name.to_string(), ns));
    }
}

/// Forget `id`'s attribute `name` -- it is gone, or in the namespace its name reads as -- emptying its slot.
fn forget_ns(st: &mut OpState, id: u32, name: &str) {
    let dom = st.borrow::<HostDom>().0.instance();
    let Some(t) = st.try_borrow_mut::<AttrNamespaces>().filter(|t| t.dom == dom) else { return };
    if let Some(e) = t.of.get_mut(&id).and_then(|l| l.iter_mut().find(|(n, _)| n == name)) {
        e.0.clear();
        e.1 = None;
    }
}

/// Whether `id` is an HTML element of an HTML document, which reads every attribute name whole and in no namespace
/// ([`attr_name_parts`]).
///
/// [`attr_name_parts`]: crate::core::dom::attr::attr_name_parts
#[inline]
fn reads_names_whole(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.is_html_ns(id) && !dom.is_in_xml(id)
}

/// The arena key of `id`'s attribute in namespace `ns` with local name `local`: DOM's "get an attribute by namespace
/// and local name", the first in the list. Each one's namespace is [`AttrNamespaces`]'s, else as the parsers decide.
pub(crate) fn attr_by_ns(st: &OpState, id: u32, ns: Option<&str>, local: &str) -> Option<String> {
    let dom = &st.borrow::<HostDom>().0;
    dom.attribute_pairs(id)
        .iter()
        .map(|(k, _)| k)
        // only a name whose local part can be `local` is looked into: the whole name, or what follows a `:`
        .filter(|k| *k == local || k.strip_suffix(local).is_some_and(|p| p.ends_with(':')))
        .find(|k| match kept_ns(st, dom, id, k) {
            // `setAttribute`'s local name is the whole name
            Some(kns) => kns.as_deref() == ns && (if kns.is_some() { local_of(k) } else { k.as_str() }) == local,
            None => {
                let (kns, _, kl) = crate::core::dom::attr::attr_name_parts(dom, id, k);
                kl == local && kns.as_deref() == ns
            }
        })
        .cloned()
}

/// What a write to `id`'s attribute `name` needs of its namespace, read by [`ns_before`] under the borrow its writer
/// takes for the old value -- before the write, so a removal still has the attribute to read.
pub(crate) enum NsBefore {
    /// Nothing to keep or forget, and in no namespace for a writer that names none: a name with no `:` (and not
    /// `xmlns`) that its element does not keep -- every attribute of nearly every page -- or any name an HTML element
    /// of an HTML document does not keep.
    Plain,
    /// [`AttrNamespaces`] keeps it, in this namespace.
    Kept(Option<std::rc::Rc<str>>),
    /// A prefixed name, not kept, on an element whose parser reads prefixes (a foreign element, an XML document's):
    /// `Some((namespace, local name))` as its parser read it, for one the element has and reads as namespaced.
    Read(Option<Box<(String, String)>>),
}

/// [`NsBefore`] for a write to `id`'s attribute `name`, `dom` being `st`'s arena: `read` for a writer that names no
/// namespace (a `setAttributeNS` / `removeAttributeNS` needs only what is kept), `exists` whether the element has it.
/// A name with no `:` costs a byte scan and a bit read, inline, for an element never given a namespace -- and the
/// writer then skips [`attr_written`] -- a lookup of its element for one that was; a prefixed one on an HTML
/// element, the element's namespace and document.
#[inline(always)]
pub(crate) fn ns_before(st: &OpState, dom: &korvus_dom::Dom, id: u32, name: &str, read: bool, exists: bool) -> NsBefore {
    if !may_keep(id) && !(read && prefixed(name)) {
        return NsBefore::Plain;
    }
    ns_looked_up(st, dom, id, name, read, exists)
}

/// [`ns_before`] past its flag.
#[inline(never)]
fn ns_looked_up(st: &OpState, dom: &korvus_dom::Dom, id: u32, name: &str, read: bool, exists: bool) -> NsBefore {
    if let Some(ns) = kept_ns(st, dom, id, name) {
        return NsBefore::Kept(ns.clone());
    }
    if !read || !prefixed(name) || reads_names_whole(dom, id) {
        return NsBefore::Plain;
    }
    NsBefore::Read(if exists {
        let (ns, _, local) = crate::core::dom::attr::attr_name_parts(dom, id, name);
        ns.map(|ns| Box::new((ns, local)))
    } else {
        None
    })
}

/// The namespace bookkeeping of a write that set `id`'s attribute `name`, `before` what [`ns_before`] read: `named`
/// is `setAttributeNS`'s namespace, `None` for every writer that names none (`setAttribute`, a reflected IDL
/// attribute, `Attr.value`), and `made` whether the attribute was not there before. Answers how the record of a
/// writer that names no namespace names it: `Some((namespace, local name))` for a namespaced attribute. Nothing to
/// do for a [`NsBefore::Plain`] write in no namespace, which its writer does not call this for.
#[inline(never)]
pub(crate) fn attr_written(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str, named: Option<Option<&str>>, made: bool, before: NsBefore) -> Option<(String, String)> {
    match (named, before) {
        (None, NsBefore::Plain) | (Some(None), NsBefore::Plain) => None,
        // changed in place, it keeps its namespace
        (None, NsBefore::Kept(ns)) if !made => ns.map(|ns| (ns.to_string(), local_of(name).to_string())),
        // MADE, it was not there: what the table held is a leftover of a removal that went around it (`delete
        // el.dataset.k` removes in the arena), and DOM's `setAttribute` makes every attribute in no namespace. Chrome
        // 152 (2026-10-23, probe r2/l3/rr2/www/x1.html datasetDelete): `setAttributeNS('urn:x', 'data-k', '1')`,
        // `delete d.dataset.k`, `setAttribute('data-k', '2')` queues `data-k` in null, where korvus named `urn:x`.
        (None, NsBefore::Kept(_)) => {
            let mut st = op_state.borrow_mut();
            let whole = !prefixed(name) || reads_names_whole(&st.borrow::<HostDom>().0, id);
            if whole { forget_ns(&mut st, id, name) } else { keep_ns(&mut st, id, name, None) }
            None
        }
        // one made on an element whose parser would have put the prefix in a namespace is kept as no namespace
        (None, NsBefore::Read(ns)) => {
            if made {
                keep_ns(&mut op_state.borrow_mut(), id, name, None);
            }
            ns.map(|b| *b)
        }
        // the namespace it already keeps: d3's `attr('xlink:href', v)` on every redraw
        (Some(Some(ns)), NsBefore::Kept(Some(k))) if *k == *ns => None,
        (Some(Some(ns)), _) => {
            keep_ns(&mut op_state.borrow_mut(), id, name, Some(ns));
            None
        }
        // `setAttributeNS(null, ..)`: no prefix, so the namespace the name reads as
        (Some(None), _) => {
            forget_ns(&mut op_state.borrow_mut(), id, name);
            None
        }
    }
}

/// After `id`'s attribute `name` left the arena, `before` what [`ns_before`] read while it was there: its namespace
/// goes with it. Answers how the record of a writer that names no namespace (`read`) names it, as [`attr_written`].
#[inline(never)]
pub(crate) fn attr_removed(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str, read: bool, before: NsBefore) -> Option<(String, String)> {
    match before {
        NsBefore::Plain => None,
        NsBefore::Kept(ns) => {
            forget_ns(&mut op_state.borrow_mut(), id, name);
            ns.filter(|_| read).map(|ns| (ns.to_string(), local_of(name).to_string()))
        }
        NsBefore::Read(ns) => ns.map(|b| *b),
    }
}

/// DOM's "clone a node" copies each attribute with its namespace: `to` is a clone of `from` (deep or not), the two
/// subtrees walked in step as `Dom::clone_node` made them -- into a `<template>`'s content too, which a deep clone
/// copies. Walking the children alone, `template.cloneNode(true).content`'s attributes lost their namespaces:
/// Chrome 152 (2026-10-23, probe r2/l3/rr2/www/x1.html templateClone) removes the clone's `p:foo` in `urn:x` with
/// `removeAttributeNS('urn:x', 'foo')` (`foo` in `urn:x`, old value `"1"`), where korvus removed nothing.
pub(crate) fn clone_attr_namespaces(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, from: u32, to: u32) {
    if !ATTR_NS_KEPT.with(std::cell::Cell::get) {
        return;
    }
    let mut st = op_state.borrow_mut();
    let pairs: Vec<(u32, Vec<(String, Option<std::rc::Rc<str>>)>)> = {
        let dom = &st.borrow::<HostDom>().0;
        let inst = dom.instance();
        let Some(t) = st.try_borrow::<AttrNamespaces>().filter(|t| t.dom == inst) else { return };
        let mut out = Vec::new();
        let mut stack = vec![(from, to)];
        while let Some((a, b)) = stack.pop() {
            if let Some(list) = t.of.get(&a).filter(|l| l.iter().any(|(n, _)| !n.is_empty())) {
                out.push((b, list.iter().filter(|(n, _)| !n.is_empty()).cloned().collect()));
            }
            stack.extend(dom.children(a).into_iter().zip(dom.children(b)));
            if let (Some(fa), Some(fb)) = (dom.template_content_of(a), dom.template_content_of(b)) {
                stack.push((fa, fb));
            }
        }
        out
    };
    let t = ns_table(&mut st);
    for (id, list) in pairs {
        mark_kept(id);
        t.of.insert(id, list);
    }
}

pub(crate) fn cb_el_style<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if let Some(decl) = inline_style_decl(scope, args.this()) {
        rv.set(decl.into());
    }
}

/// The element's inline `CSSStyleDeclaration` -- what `el.style` answers, read without the page-patchable
/// getter (`el.attributeStyleMap` is a view of it: `style_property_map::inline_map`).
pub(crate) fn inline_style_decl<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = el_id_of(scope, el)?;
    // `el.style === el.style` in every browser: the declaration is a property of the element, not a
    // value minted per read. Memoised on a private slot of the (interned) element wrapper. Read BEFORE
    // the prototype is resolved: a hit never uses it, and the sealed-map lookup plus `prototype` read
    // were most of an `el.style` read (9,765 -> 1,354 instructions, 2026-10-23). The lookup cannot fail
    // for a realm whose elements have a `style` getter (the constructor is sealed, or made from its
    // template in a lazy realm; `prototype` is read-only), so a hit answers what it answered after it.
    if let Some(cached) = priv_get(scope, el, CSD_CACHE).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(cached);
    }
    let proto = crate::binding::protos::iface_object(scope, "CSSStyleDeclaration").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype")).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    // Named CSS props (`style.color`) route through a native named-property interceptor on the
    // declaration itself -- see css_style_declaration::make_style_decl. It used to be a JS Proxy that
    // handed back `t[method].bind(t)`, a fresh bound function on every read.
    let decl = crate::core::css::css_style_declaration::make_style_decl(scope, id, proto);
    priv_set(scope, el, CSD_CACHE, decl.into());
    Some(decl)
}


// ---- Reflected DOM attributes (the `cb_reflect_*` kit): one native callback per attribute KIND, the
// attribute name bound as function data -- lets one callback serve many attrs. `install/kits.rs` is the
// one caller, pairing a getter and a setter per `MemberDef`; a handful of interface files reach for an
// individual callback where a member mixes kinds (`html_anchor_element.rs`'s url-get/str-set `href`).
// (build_fn_data itself is a shared bindings helper and stays there.)
// Reflected string attribute getter: this.getAttribute(attr) || "" (attr in args.data()).
pub(crate) fn cb_reflect_str_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let v = attr_call(scope, this.into(), "getAttribute", &[attr]);
    match v { Some(x) if !x.is_null_or_undefined() => rv.set(x), _ => { let e = str_val(scope, ""); rv.set(e); } }
}

// A reflected URL attribute getter (anchor href): the attribute verbatim, EXCEPT an absolute URL is
// normalized/percent-encoded exactly like the baseline's URL machinery (a non-ASCII query comes back
// percent-encoded -- WPT DOMImplementation-createHTMLDocument "URL parsing"). Relative values echo
// verbatim: the historical behavior, which the rest of the corpus relies on (there is no
// per-document base URL machinery to resolve against yet).
/// A `[ReflectURL]` attribute: `a.href`, `link.href`, `base.href`, `img.src`, `iframe.src`.
///
/// The IDL value is the attribute PARSED AGAINST THE DOCUMENT BASE URL, not the attribute. That is
/// the whole point of the extended attribute, and it is what makes `<a href="/x">.href` report
/// `http://host/x` rather than `/x`. This used to normalise only already-absolute values and hand
/// back everything else verbatim, so every relative href and src on the page read as the raw markup
/// -- visible to any script that compares `a.href` against `location.origin`, which is a very
/// ordinary thing for a page to do.
///
/// The base and the parser are the same ones HTMLHyperlinkElementUtils uses (core/html/hyperlink.rs):
/// the real `url` crate against the document's BASE url (`core/dom/document.rs::document_base_url`,
/// which is `<base href>` applied and falls back to the document url and then to "about:blank"). It
/// read `CookieUrl` -- the document url -- directly once, which is why every reflected URL
/// on a page carrying a `<base>` was resolved against the wrong thing. An attribute that does not
/// parse even against the base is returned as-is, which is the spec's "if parsing fails, return the
/// attribute value".
pub(crate) fn cb_reflect_url_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let v = attr_call(scope, this.into(), "getAttribute", &[attr]);
    let x = match v { Some(x) if !x.is_null_or_undefined() => x, _ => { let e = str_val(scope, ""); rv.set(e); return; } };
    let s = x.to_rust_string_lossy(scope);
    // ABSENT and PRESENT-BUT-EMPTY are different, and collapsing them was worth 1,311 failing subtests.
    // The recordings that made this return "" are all the ABSENT case -- `createElement("link").href`
    // with no attribute at all -- which is handled above, where getAttribute answers null. A PRESENT
    // empty value is a relative URL that resolves to the document's own URL, exactly like any other
    // relative reference, so it falls through to the resolve below. Only a document with no base at
    // all keeps "".
    // The BASE url, not the document url: `<base href="/sub/">` moves every reflected URL on the
    // page, and `core/dom/document.rs::document_base_url` is the one place that knows how (W9.15).
    // The element's OWN document's base: a frame's markup resolves against the frame
    // (`subresource::node_base_url`).
    let base = match arg_node_id(scope, this.into()) {
        Some(id) => crate::core::html::subresource::node_base_url(scope, id),
        None => crate::core::dom::base_url(scope),
    };
    // THE BASE IS THE PARSER'S BASE, not a fallback for a failed absolute parse -- this line was the
    // THIRD copy of that mistake (`core/html/hyperlink.rs` had two) and it disagreed with the `URL`
    // constructor, which always had it right. `a.href` and `a.pathname` are the same element's
    // members over the same href and they must not answer from two different parsers.
    let parsed = crate::modules::url::components::resolve(&base, &s);
    match parsed {
        Some(u) => { let out = str_val(scope, u.as_str()); rv.set(out); }
        None => rv.set(x),
    }
}

// Reflected string attribute setter: this.setAttribute(attr, String(v)).
/// `HTMLOrSVGElement.nonce`: the element's `[[CryptographicNonce]]` slot (`kits::reflect_str` routes the row here).
pub(crate) fn cb_nonce_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_id_of(scope, args.this()) else {
        rv.set(str_val(scope, ""));
        return;
    };
    let v = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.nonce(id).to_string()
    };
    rv.set(str_val(scope, &v));
}

/// The `nonce` setter: the slot only -- no attribute is made or changed.
pub(crate) fn cb_nonce_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(v) = args.get(0).to_string(scope) else { return };
    let v = v.to_rust_string_lossy(scope);
    let Some(id) = el_id_of(scope, args.this()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_nonce(id, &v);
}

pub(crate) fn cb_reflect_str_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let Some(val) = reflected_text(scope, this, attr, args.get(0)) else { return };
    let sv = str_val(scope, &val);
    attr_call(scope, this.into(), "setAttribute", &[attr, sv]);
}

/// The value a reflected attribute is set to: WebIDL's ToString -- except where Trusted Types gives that
/// element's attribute a type (`script.src`, `embed.src`, `object.data` and `object.codeBase` take a
/// TrustedScriptURL): a value of that type is read as itself, nothing on it called (Chrome 150; this called
/// the value's `toString`), and a string goes through the document's Trusted Types
/// (`"HTMLScriptElement src"` to the default policy when its CSP asks). `None`: an
/// exception is pending.
fn reflected_text(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, attr: v8::Local<v8::Value>, v: v8::Local<v8::Value>) -> Option<String> {
    let attr = attr.to_rust_string_lossy(scope);
    if matches!(attr.as_str(), "src" | "data" | "codebase") {
        if let Some(id) = el_id_of(scope, this) {
            let typed = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                let dom = &st.borrow::<HostDom>().0;
                dom.tag_name(id).and_then(|tag| {
                    let kind = crate::core::trusted_types::attribute_type(tag, &attr, dom.namespace_uri(id), None)?;
                    Some((kind, tt_attribute_iface(tag, dom.namespace_uri(id), kind)))
                })
            };
            if let Some((kind, iface)) = typed {
                // the IDL attribute's own name, which the sample and the sentence carry
                let prop = if attr == "codebase" { "codeBase" } else { attr.as_str() };
                let (sample, context) = (format!("{iface} {prop}"), format!("Failed to set the '{prop}' property on '{iface}'"));
                let sink = crate::core::trusted_types::tables::Sink { kind, sample: &sample, context: &context };
                return crate::core::trusted_types::tables::compliant_text(scope, v, &sink, crate::core::trusted_types::tables::SinkAt::Node(id));
            }
        }
    }
    let s = v.to_string(scope)?;
    Some(s.to_rust_string_lossy(scope))
}

/// The interface Blink names a Trusted-Types attribute under (`GetCheckedAttributeTypes`): `Element` for an
/// event handler, else the element's own -- `HTMLScriptElement src`, `HTMLIFrameElement srcdoc`,
/// `HTMLObjectElement data`, `HTMLEmbedElement src`, `SVGScriptElement href`.
fn tt_attribute_iface(tag: &str, ns: Option<&str>, kind: &str) -> &'static str {
    if kind == "TrustedScript" {
        return "Element";
    }
    match tag.to_ascii_lowercase().as_str() {
        "script" if ns == Some("http://www.w3.org/2000/svg") => "SVGScriptElement",
        "script" => "HTMLScriptElement",
        "iframe" => "HTMLIFrameElement",
        "object" => "HTMLObjectElement",
        "embed" => "HTMLEmbedElement",
        _ => "Element",
    }
}

/// An attribute value on its way into element `el` through `setAttribute`, `setAttributeNS`,
/// `setAttributeNode` or `Attr.value` (Blink `Element::TrustedTypesCheckForAttribute`): when Trusted Types
/// gives the attribute a type -- an event handler, `script src`, `iframe srcdoc`, ... -- a value of that type
/// passes as it is and anything else goes through the document's default policy with the attribute's sample
/// (`"Element onclick"`, `"HTMLScriptElement src"`); a refusal throws `context`'s sentence. Any other
/// attribute takes the value as it is. `None`: an exception is pending.
pub(crate) fn attribute_trusted_value(scope: &mut v8::PinScope, el: u32, name: &str, attr_ns: Option<&str>, value: String, value_kind: Option<String>, context: &str) -> Option<String> {
    attribute_trusted_value_cow(scope, el, name, attr_ns, std::borrow::Cow::Owned(value), value_kind, context).map(std::borrow::Cow::into_owned)
}

/// Whether a write of attribute `local` can be a Trusted Types sink at all: `on*`, `src`, `srcdoc`, `data`,
/// `codebase`, `href`, ASCII case-insensitively -- the test the lowercased copy of the name answered, without the
/// copy (`to_ascii_lowercase` maps only A-Z, so the two agree on every string).
fn tt_candidate(local: &str) -> bool {
    (local.len() >= 2 && local.as_bytes()[..2].eq_ignore_ascii_case(b"on"))
        || ["src", "srcdoc", "data", "codebase", "href"].iter().any(|k| local.eq_ignore_ascii_case(k))
}

/// [`attribute_trusted_value`] over a value the caller may hold borrowed (`setAttribute`'s string value, read into
/// a stack buffer): a write no Trusted Types check applies to hands the same value back, copying nothing.
pub(crate) fn attribute_trusted_value_cow<'v>(
    scope: &mut v8::PinScope,
    el: u32,
    name: &str,
    attr_ns: Option<&str>,
    value: std::borrow::Cow<'v, str>,
    value_kind: Option<String>,
    context: &str,
) -> Option<std::borrow::Cow<'v, str>> {
    let local = name.rsplit_once(':').map_or(name, |(_, l)| l);
    // only these names are ever typed: the check stays off every other write's path
    if !tt_candidate(local) {
        return Some(value);
    }
    let typed = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let (ns, _, local) = match attr_ns {
            Some(_) => (attr_ns.map(str::to_string), None, local.to_string()),
            None => crate::core::dom::attr::attr_name_parts(dom, el, name),
        };
        dom.tag_name(el).and_then(|tag| {
            let kind = crate::core::trusted_types::attribute_type(tag, &local, dom.namespace_uri(el), ns.as_deref())?;
            Some((kind, tt_attribute_iface(tag, dom.namespace_uri(el), kind), local))
        })
    };
    let Some((kind, iface, local)) = typed else { return Some(value) };
    if value_kind.as_deref() == Some(kind) {
        return Some(value);
    }
    let sample = format!("{iface} {local}");
    let sink = crate::core::trusted_types::tables::Sink { kind, sample: &sample, context };
    crate::core::trusted_types::tables::check_text(scope, value.into_owned(), &sink, crate::core::trusted_types::tables::SinkAt::Node(el)).map(std::borrow::Cow::Owned)
}
// Nullable reflected string (ARIAMixin `role` + `aria-*`): unlike the plain reflector these are IDL
// `DOMString?` -- the getter returns `null` (not "") when the content attribute is absent, and the
// setter REMOVES the attribute for null/undefined (else sets String(v)). (WICG AOM / ARIA reflection.)
pub(crate) fn cb_reflect_nullable_str_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let v = attr_call(scope, this.into(), "getAttribute", &[attr]);
    match v { Some(x) if !x.is_null_or_undefined() => rv.set(x), _ => { let n: v8::Local<v8::Value> = v8::null(scope).into(); rv.set(n); } }
}
pub(crate) fn cb_reflect_nullable_str_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let v = args.get(0);
    if v.is_null_or_undefined() { attr_call(scope, this.into(), "removeAttribute", &[attr]); }
    else { let val = v.to_rust_string_lossy(scope); let sv = str_val(scope, &val); attr_call(scope, this.into(), "setAttribute", &[attr, sv]); }
}
// Reflected boolean attribute getter: this.hasAttribute(attr).
/// An ENUMERATED reflected attribute -- `img.loading`, `a.referrerPolicy`, `script.fetchPriority`.
///
/// These are the reflections `gen_reflected.py` deliberately skips, because a plain string
/// reflection gets them wrong in a way that looks right: `img.loading` with no attribute is "auto"
/// in Chrome, not "". WebIDL spells the rule out per attribute
/// (`[ReflectOnly=("eager","lazy"), ReflectMissing="auto", ReflectInvalid="auto"]`), so the config
/// travels with the getter rather than being guessed:
///
/// ```text
///   attr | keyword,keyword,... | missing-default | invalid-default
/// ```
///
/// The getter LIMITS: an attribute whose value is not one of the keywords (ASCII-case-insensitively)
/// answers the invalid default, and an absent one answers the missing default. The setter stores
/// verbatim -- limiting is the getter's job, which is why `img.decoding = "x"` round-trips to "auto"
/// rather than being rejected.
pub(crate) fn cb_reflect_enum_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let mut parts = cfg.split('|');
    let attr = parts.next().unwrap_or("");
    let keywords: Vec<&str> = parts.next().unwrap_or("").split(',').filter(|k| !k.is_empty()).collect();
    let missing = parts.next().unwrap_or("");
    let invalid = parts.next().unwrap_or(missing);

    let a = str_val(scope, attr);
    let current = attr_call(scope, args.this().into(), "getAttribute", &[a]);
    let answer = match current {
        Some(v) if !v.is_null_or_undefined() => {
            let got = v.to_rust_string_lossy(scope);
            match keywords.iter().find(|k| k.eq_ignore_ascii_case(&got)) {
                Some(k) => (*k).to_string(),
                None => invalid.to_string(),
            }
        }
        _ => missing.to_string(),
    };
    let out = str_val(scope, &answer);
    rv.set(out);
}

/// The NULLABLE enumerated reflection: `attr|keywords|invalid-default`.
///
/// Same limited-value rules as `cb_reflect_enum_get`, with one difference that no missing-default
/// string can express: an absent attribute reflects as `null`, not as a string. `crossOrigin` is the
/// member this exists for -- `script.crossOrigin` is `null` until the attribute is there, and then
/// an unrecognised value reflects as "anonymous" rather than as itself.
pub(crate) fn cb_reflect_nullable_enum_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let mut parts = cfg.split('|');
    let attr = parts.next().unwrap_or("");
    let keywords: Vec<&str> = parts.next().unwrap_or("").split(',').filter(|k| !k.is_empty()).collect();
    let invalid = parts.next().unwrap_or("");

    let a = str_val(scope, attr);
    match attr_call(scope, args.this().into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => {
            let got = v.to_rust_string_lossy(scope);
            let answer = match keywords.iter().find(|k| k.eq_ignore_ascii_case(&got)) {
                Some(k) => (*k).to_string(),
                None => invalid.to_string(),
            };
            let out = str_val(scope, &answer);
            rv.set(out);
        }
        _ => rv.set_null(),
    }
}

/// An `unsigned long` reflected attribute -- `img.hspace`, `td.colSpan`, `ol.start`.
///
/// Config is `attr|default`. The getter parses a NON-NEGATIVE integer and falls back to the default
/// when the attribute is absent, empty, negative or not a number at all -- the defaults differ per
/// member (`hspace` is 0, `colSpan` is 1) which is exactly why they are not a single generated rule.
/// The setter writes the number back as a decimal string.
/// HTML's *rules for parsing integers* (and, with `signed=false`, non-negative integers).
///
/// Not `str::trim().parse()`, which is wrong at both ends. Rust's `trim` strips UNICODE whitespace,
/// so it eats U+000B VERTICAL TAB -- which HTML's ASCII-whitespace set (TAB, LF, FF, CR, SPACE) does
/// NOT contain, making `hspace="\v7"` parse as 7 where a browser gives the default. And Rust's `parse`
/// rejects any trailing character, where HTML stops at the first non-digit and keeps what it has, so
/// `tabindex="5%"` is 5 and `tabindex="1.5"` is 1.
///
/// Returns None when there is no digit to read, which is the spec's error return and the caller's cue
/// to use the attribute's missing-value default.
pub(crate) fn parse_html_integer(s: &str, signed: bool) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b'\t' | b'\n' | 0x0C | b'\r' | b' ') {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        if b[i] == b'-' {
            if !signed {
                return None; // a non-negative integer attribute rejects a sign outright
            }
            neg = true;
        }
        i += 1;
    }
    let start = i;
    let mut n: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        // Saturate rather than wrap: an absurdly long digit run is out of range, and the callers clamp
        // to i32/u32 anyway.
        n = n.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(if neg { -n } else { n })
}

/// Config for a numeric reflection: `attr|default|mode|min:max`, every field after `attr` optional.
///
/// `mode` is the IDL's reflection flavour, which changes what a PRESENT but unsuitable value reads
/// back as -- not just what an absent one does:
///   ""         plain [Reflect]
///   "positive" [ReflectPositive] / [ReflectPositiveWithFallback] -- a value of 0 falls back
///   "nonneg"   [ReflectNonNegative] -- a negative value falls back (e.g. maxLength, default -1)
///
/// `min:max` is [ReflectRange], which CLAMPS rather than falls back: `<td colspan="9999">` reads
/// back as 1000, not as the default 1.
fn num_cfg(cfg: &str) -> (&str, i64, &str, Option<(i64, i64)>) {
    let mut it = cfg.splitn(4, '|');
    let attr = it.next().unwrap_or("");
    let default = it.next().and_then(|d| d.parse::<i64>().ok()).unwrap_or(0);
    let mode = it.next().unwrap_or("");
    let range = it.next().and_then(|r| r.split_once(':')).and_then(|(a, b)| {
        Some((a.parse::<i64>().ok()?, b.parse::<i64>().ok()?))
    });
    (attr, default, mode, range)
}

/// Apply [ReflectRange] to a parsed value: out-of-range clamps to the nearer bound.
fn clamp_range(n: i64, range: Option<(i64, i64)>) -> i64 {
    match range {
        Some((lo, hi)) => n.max(lo).min(hi),
        None => n,
    }
}

pub(crate) fn cb_reflect_ulong_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (attr, default, mode, range) = num_cfg(&cfg);
    let fallback = if default < 0 { 0 } else { default as u32 };
    let a = str_val(scope, attr);
    let n = match attr_call(scope, args.this().into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => parse_html_integer(&v.to_rust_string_lossy(scope), false)
            .filter(|n| *n <= u32::MAX as i64)
            .filter(|n| !(mode == "positive" && *n == 0))
            .map(|n| clamp_range(n, range) as u32)
            .unwrap_or(fallback),
        _ => fallback,
    };
    rv.set(v8::Integer::new_from_unsigned(scope, n).into());
}

pub(crate) fn cb_reflect_ulong_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // Takes the same `attr|default|mode` config as the getter so one table row configures the pair.
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (attr, _default, _mode, _range) = num_cfg(&cfg);
    // WebIDL converts to `unsigned long` first, so a negative or non-numeric assignment lands on 0.
    let n = args.get(0).number_value(scope).unwrap_or(0.0);
    let n = if n.is_finite() && n > 0.0 { n as u32 } else { 0 };
    let a = str_val(scope, attr);
    let v = str_val(scope, &n.to_string());
    attr_call(scope, args.this().into(), "setAttribute", &[a, v]);
}

/// A SIGNED `long` reflected attribute whose absent/invalid answer is a NEGATIVE sentinel —
/// `input.maxLength`, `input.minLength`, `textarea.maxLength` (`[ReflectNonNegative] attribute long`).
///
/// It cannot borrow `cb_reflect_ulong_get`: the whole point of the kind is that "no maxlength" reads
/// back as `-1`, which no unsigned reflector can express. The content attribute itself is still parsed
/// as a NON-NEGATIVE integer (that is what `[ReflectNonNegative]` means) — `maxlength="-3"` and
/// `maxlength="x"` are both invalid and answer the default, not `-3`.
///
/// Config is `attr|default`.
pub(crate) fn cb_reflect_long_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (attr, default, mode, range) = num_cfg(&cfg);
    let fallback = default as i32;
    let a = str_val(scope, attr);
    let n = match attr_call(scope, args.this().into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => parse_html_integer(&v.to_rust_string_lossy(scope), true)
            .filter(|n| *n >= i32::MIN as i64 && *n <= i32::MAX as i64)
            .filter(|n| !(mode == "positive" && *n <= 0))
            .filter(|n| !(mode == "nonneg" && *n < 0))
            .map(|n| clamp_range(n, range) as i32)
            .unwrap_or(fallback),
        _ => fallback,
    };
    rv.set(v8::Integer::new(scope, n).into());
}

/// The matching setter. Config is `attr` or `attr|nonneg`; with `nonneg`, a negative assignment is an
/// `IndexSizeError` rather than a stored value — the spec's own rule for `[ReflectNonNegative]`
/// ("if the given value is negative, then throw an IndexSizeError"), and observably different from
/// silently clamping to 0 the way the unsigned setter does.
pub(crate) fn cb_reflect_long_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let cfg = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (attr, _default, mode, _range) = num_cfg(&cfg);
    // WebIDL `long` conversion first: ToNumber then truncate toward zero (NaN/±Infinity -> 0).
    let raw = args.get(0).number_value(scope).unwrap_or(0.0);
    let n = if raw.is_finite() { raw.trunc() as i64 as i32 } else { 0 };
    if mode == "nonneg" && n < 0 {
        throw_dom(scope, "The value provided is negative.", "IndexSizeError");
        return;
    }
    let a = str_val(scope, attr);
    let v = str_val(scope, &n.to_string());
    attr_call(scope, args.this().into(), "setAttribute", &[a, v]);
}

/// Resolve an ID reference the way attr-element reflection does: the FIRST element with that id in
/// `origin`'s node tree (its root, not its document). The distinction is visible on a detached
/// element — `document.createElement("input").popoverTargetElement` must be null even when the
/// document does contain an element with that id, because the input is not in the document's tree.
pub(crate) fn id_ref_in_tree<'s>(scope: &mut v8::PinScope<'s, '_>, origin: v8::Local<v8::Value>, id: &str) -> Option<v8::Local<'s, v8::Value>> {
    if id.is_empty() { return None; }
    // the realm's own tree steps throughout, never the page's (`label.control` called
    // `getRootNode` and `getElementById` as a page would)
    let root = crate::binding::protos::pristine_call(scope, origin, "Node.getRootNode", &[])?;
    // A Document root has the fast path the spec's "in tree order" wording describes; a DocumentFragment
    // (a ShadowRoot among them) has `getElementById` too.
    let by_id = if crate::core::dom::shared::document_node_id(scope, root).is_some() {
        Some("Document.getElementById")
    } else {
        let kind = arg_node_id(scope, root).and_then(|rid| {
            let os = deno_core::JsRuntime::op_state_from(scope);
            let st = os.borrow();
            st.try_borrow::<HostDom>().map(|d| (d.0.node_type(rid), d.0.is_shadow_root(rid)))
        });
        match kind {
            Some((9, false)) => Some("Document.getElementById"),
            Some((11, _)) | Some((9, true)) => Some("DocumentFragment.getElementById"),
            _ => None,
        }
    };
    if let Some(member) = by_id {
        let idv = str_val(scope, id);
        return crate::binding::protos::pristine_call(scope, root, member, &[idv]).filter(|v| !v.is_null_or_undefined());
    }
    // Otherwise the root is the subtree itself (a detached element, or a ShadowRoot without the
    // getElementById shorthand): check the root, then its descendants, in tree order.
    let idattr = str_val(scope, "id");
    if let Some(own) = attr_call(scope, root, "getAttribute", &[idattr]) {
        if !own.is_null_or_undefined() && own.to_rust_string_lossy(scope) == id { return Some(root); }
    }
    // CSS attribute selector with the id as a quoted string: safe for ids that would not be valid
    // identifiers, provided the two characters that could close the string early are escaped.
    let sel = format!("[id=\"{}\"]", id.replace('\\', "\\\\").replace('"', "\\\""));
    let selv = str_val(scope, &sel);
    crate::binding::protos::pristine_call(scope, root, "Element.querySelector", &[selv]).filter(|v| !v.is_null_or_undefined())
}

/// An ELEMENT-REFERENCE reflected attribute — `input.popoverTargetElement`, `button.commandForElement`
/// (`[Reflect] attribute Element?`). The content attribute holds an id; the IDL attribute is the
/// element that id names, resolved in THIS element's tree.
///
/// The pair also carries the spec's "explicitly set attr-element": assigning an element records the
/// object itself, so `a.popoverTargetElement = b` reads back `b` even when `b` has no id at all. The
/// recorded element only wins while it is still in the same tree as this element; otherwise the getter
/// falls back to the id lookup, exactly as the reflection algorithm says.
const ATTR_EL_PRIV_PREFIX: &str = "korvus_attr_el_";
pub(crate) fn cb_reflect_element_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if let Some(explicit) = priv_get(scope, this, &format!("{ATTR_EL_PRIV_PREFIX}{attr}")) {
        if explicit.is_null() { rv.set_null(); return; }
        // Same-tree check: both roots must be the same node.
        let mine = crate::binding::protos::pristine_call(scope, this.into(), "Node.getRootNode", &[]);
        let theirs = crate::binding::protos::pristine_call(scope, explicit, "Node.getRootNode", &[]);
        if let (Some(a), Some(b)) = (mine, theirs) {
            if a == b { rv.set(explicit); return; }
        }
    }
    let a = str_val(scope, &attr);
    let id = match attr_call(scope, this.into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => { rv.set_null(); return; }
    };
    match id_ref_in_tree(scope, this.into(), &id) { Some(el) => rv.set(el), None => rv.set_null() }
}
pub(crate) fn cb_reflect_element_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let a = str_val(scope, &attr);
    let v = args.get(0);
    let slot = format!("{ATTR_EL_PRIV_PREFIX}{attr}");
    if v.is_null_or_undefined() {
        let n: v8::Local<v8::Value> = v8::null(scope).into();
        priv_set(scope, this, &slot, n);
        attr_call(scope, this.into(), "removeAttribute", &[a]);
        return;
    }
    if unwrap_id(scope, v).is_none() {
        throw_type_error(scope, &format!("Failed to set the '{attr}' property: The provided value is not of type 'Element'."));
        return;
    }
    priv_set(scope, this, &slot, v);
    // The reflection algorithm sets the content attribute to the empty string (it is the element
    // reference, not the id, that is authoritative once explicitly set).
    let empty = str_val(scope, "");
    attr_call(scope, this.into(), "setAttribute", &[a, empty]);
}

pub(crate) fn cb_reflect_bool_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    let v = attr_call(scope, this.into(), "hasAttribute", &[attr]);
    rv.set_bool(v.map(|x| x.boolean_value(scope)).unwrap_or(false));
}
// Reflected boolean attribute setter: v ? setAttribute(attr,"") : removeAttribute(attr).
pub(crate) fn cb_reflect_bool_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this(); let attr = crate::binding::protos::body_data(scope, &args);
    if args.get(0).boolean_value(scope) { let e = str_val(scope, ""); attr_call(scope, this.into(), "setAttribute", &[attr, e]); }
    else { attr_call(scope, this.into(), "removeAttribute", &[attr]); }
}

/// Native `Element`, keyed by its arena `NodeId`.
#[repr(C)]
pub struct JsElement {
    pub(crate) id: NodeId, // pub(crate): moved interfaces (e.g. modules::fetch::form_data) read the node id.
}

unsafe impl GarbageCollected for JsElement {
    fn trace(&self, _v: &mut deno_core::v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static CStr {
        // Instances are the most-derived element type; the JS spine (dom.js) inserts Element and
        // Node prototypes above JsElement.prototype, so this native class is HTMLElement. Every
        // element-returning native op (getElementById/createElement/querySelector) thus yields an
        // HTMLElement-typed instance with no JS retyping.
        c"HTMLElement"
    }
}

// The `#[op2] impl JsElement` that stood here declared thirteen arena-plumbing members
// (`appendChildId`, `nodeId`, `querySelectorAllIds`, ...) on a deno_core class template that the bootstrap then
// stripped them off again (`unexpose_internal_plumbing`, deleted with it): no page and no Rust code reached
// them. An element wrapper is minted by `binding::protos::make_wrapper`, which needs only the cppgc type.

// ---- Element.attributes / Element.getAttributeNode (the Attr/NamedNodeMap machinery they build on --
// make_attr, cb_attr_*/cb_nnm_* -- stays in bindings.rs). Each used to carry an op wrapper beside it,
// written as an `op_dtl_like!` invocation in bindings.rs before it was expanded here; both ops are
// gone and the Element table mounts these callbacks directly. ----
pub(crate) fn cb_el_attributes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let eid = match el_id_of(scope, args.this()) { Some(i) => i, None => return };
    // The indexed (`attrs[0]`) and named (`attrs["data-x"]`) Attr properties used to be SNAPSHOTTED onto
    // the instance as real own properties -- so the map went stale the moment an attribute changed, and
    // `Object.getOwnPropertyNames` read a frozen list. They are interceptors now: live, and the object
    // owns no key of its own (core/dom/collections.rs).
    let Some(o) = crate::core::dom::collections::make_named_node_map(scope, eid) else { return };
    rv.set(o.into());
}
pub(crate) fn cb_el_get_attribute_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let eid = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_null(); return; } };
    let name = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some() };
    if has { if let Some(a) = make_attr(scope, eid, &name) { rv.set(a.into()); return; } }
    rv.set_null();
}

// ---- Node.lookupNamespaceURI / isDefaultNamespace + Element.setAttributeNS / getAttributeNS ----
// createElementNS preserves the qualified name as the tag, so an element's prefix is the part of its
// tag before ':'. Covered: an element answering its own prefix (step 1, prefix vs the node's namespace),
// the always-bound reserved prefixes (xml/xmlns), the ancestor xmlns:/xmlns attribute chain (which
// `setAttributeNS` stores verbatim), the HTML default namespace for HTML elements on the null prefix,
// and the null answer for DocumentFragment/DocumentType/detached nodes.
const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";
const NS_XMLNS: &str = "http://www.w3.org/2000/xmlns/";
// This file re-exported `korvus_dom::NS_SVG` for one reader, the op deleted just below `cb_get_html`,
// which compared a late-recorded namespace against it. The retyping that comparison guarded now
// happens at creation (`document.rs::cb_doc_create_element_ns` passes `ElementNs::from_uri` straight
// to `create_element_ns`, and `node_proto` reads it back), so nothing here needs the constant.

/// `Element.tagName` / element `nodeName` with namespace-aware casing. Per DOM, the qualified name is
/// ASCII-uppercased only when the element is in the HTML namespace AND its document is an HTML
/// document; otherwise its case is preserved (SVG/MathML/foreign elements keep their as-created case).
/// The namespace is the arena node's own ([`korvus_dom::ElementNs`]), so this is right for a
/// parser-created `<clipPath>` (which stays `clipPath`) as well as a `createElementNS` one.
/// Returns `None` for non-element nodes (caller falls back to the arena node name).
pub(crate) fn element_display_name(state: &OpState, id: u32) -> Option<String> {
    let dom = &state.borrow::<HostDom>().0;
    let raw = dom.tag_name(id)?;
    Some(if dom.is_html_ns(id) && !dom.is_in_xml(id) { raw.to_ascii_uppercase() } else { raw.to_string() })
}

/// Normalize an attribute qualified name for the get/set/has/removeAttribute family AND the
/// non-namespace NamedNodeMap members (getNamedItem / setNamedItem / removeNamedItem): DOM lower-cases
/// the qualifiedName when the element is in the HTML namespace AND its document is an HTML document
/// (WHATWG "get an attribute" step 2.1), so HTML attributes are case-insensitive
/// (`hasAttribute("data-E2")` matches the parsed `data-e2`); foreign (SVG/MathML) and XML-document
/// elements keep the given case. The NS paths (getAttributeNS/setAttributeNS/getNamedItemNS/...)
/// store and match the qualified name verbatim -- no case folding. -- WPT Element-hasAttribute.
pub(crate) fn normalize_attr_name(state: &OpState, id: u32, name: &str) -> String {
    let dom = &state.borrow::<HostDom>().0;
    if dom.is_html_ns(id) && !dom.is_in_xml(id) { name.to_ascii_lowercase() } else { name.to_string() }
}
/// The parent ELEMENT, which is where both namespace walks go next -- not the parent node.
///
/// The distinction is the whole bug. The spec's "locate a namespace prefix" and "locate a namespace"
/// both end at step "if element's parent element is null, return null", so the walk stops at the
/// document element. Using the parent NODE instead sends `<html>` to the Document, and the Document
/// case sends it back to `<html>`: an infinite loop in native code, reachable from one line of page
/// script (`document.body.lookupPrefix("nope")`). V8's terminate() cannot break it, so it wedged a
/// whole WPT shard for twenty minutes on dom/nodes/Node-lookupPrefix.xhtml.
fn parent_element(dom: &korvus_dom::Dom, n: u32) -> Option<u32> {
    let p = dom.parent(n)?;
    if dom.node_type(p) == 1 { Some(p) } else { None }
}

/// A Document's element context is its documentElement -- its first element child, whatever it is
/// called. `get_elements_by_tag_name("html")` was both too narrow (an XML document's root is not
/// `html`) and, in the loop above, the other half of the cycle.
fn document_element(dom: &korvus_dom::Dom, doc: u32) -> Option<u32> {
    dom.children(doc).iter().copied().find(|&c| dom.node_type(c) == 1)
}

/// A namespace URI [`lookup_ns_uri`] found: one of the engine's own (`xml`, `xmlns`, HTML, SVG, MathML) as its literal,
/// so the caller hands it out as a string made once ([`crate::binding::protos::key_val`]), or one a document declared.
pub(crate) enum NsUri {
    Known(&'static str),
    Declared(String),
}

impl NsUri {
    fn of(ns: &korvus_dom::ElementNs) -> Option<NsUri> {
        Some(match ns {
            korvus_dom::ElementNs::Html => NsUri::Known(korvus_dom::NS_HTML),
            korvus_dom::ElementNs::Svg => NsUri::Known(korvus_dom::NS_SVG),
            korvus_dom::ElementNs::MathMl => NsUri::Known(korvus_dom::NS_MATHML),
            korvus_dom::ElementNs::Null => return None,
            korvus_dom::ElementNs::Other(u) => NsUri::Declared(u.as_str().to_string()),
        })
    }

    fn as_str(&self) -> &str {
        match self {
            NsUri::Known(s) => s,
            NsUri::Declared(s) => s,
        }
    }

    /// The URI as a JS string: a well-known one is the same string every time. `el.lookupNamespaceURI(null)` on an
    /// HTML element -- 2.4 % of www.costco.com's main thread (2026-10-23) -- copied the XHTML URI into a `String` and
    /// made a V8 string of it per call: 148-154 ns where Chrome 152 answers in 16-30.
    fn to_v8<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
        match self {
            NsUri::Known(s) => crate::binding::protos::key_val(scope, s),
            NsUri::Declared(s) => str_val(scope, s),
        }
    }
}

fn lookup_ns_uri(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32, prefix: Option<&str>) -> Option<NsUri> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut cur = Some(id);
    while let Some(n) = cur {
        match dom.node_type(n) {
            1 => {
                let raw = dom.tag_name(n).unwrap_or("");
                if raw.eq_ignore_ascii_case("#fragment") { return None; }
                // The xml/xmlns prefixes are always bound for an element (and for any node whose
                // locate-a-namespace reaches one). A node with no element context -- fragment, doctype,
                // documentElement-less document, disconnected attr -- never reaches here and returns null.
                if prefix == Some("xml") { return Some(NsUri::Known(NS_XML)); }
                if prefix == Some("xmlns") { return Some(NsUri::Known(NS_XMLNS)); }
                // Step 1: the element's own namespace when its prefix matches the query prefix. The
                // prefix is the part of the tag before ':' (null when there is none); the namespace is
                // the node's own (null for an XML element with no `xmlns` in scope).
                let el_prefix: Option<&str> = raw.split_once(':').map(|(p, _)| p);
                if el_prefix == prefix {
                    if let Some(ns) = dom.element_ns(n).and_then(NsUri::of) { return Some(ns); }
                }
                // Step 2: an xmlns:prefix (or xmlns for the null prefix) declaration on this element.
                let attr = match prefix { Some(p) => format!("xmlns:{p}"), None => "xmlns".to_string() };
                if let Some(v) = dom.get_attribute(n, &attr) { return if v.is_empty() { None } else { Some(NsUri::Declared(v.to_string())) }; }
                cur = parent_element(dom, n);
            }
            9 => { cur = document_element(dom, n); } // Document -> documentElement
            10 | 11 => return None,                                              // Doctype / DocumentFragment
            _ => { cur = dom.parent(n); }                                        // Text/Comment/PI -> parent
        }
    }
    None
}

/// `Node.lookupPrefix(namespace)` — the WHATWG "locate a namespace prefix" algorithm.
///
/// The mirror of [`lookup_ns_uri`]: the same ancestor walk over the same declarations, asked the other
/// way round. For each element from the receiver upwards, the element's own prefix wins when its
/// namespace matches; otherwise any `xmlns:p` attribute whose VALUE is the namespace names the prefix.
///
/// Null namespace, or the empty string, has no prefix by definition and returns null without walking.
fn lookup_prefix(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32, ns: &str) -> Option<String> {
    if ns.is_empty() {
        return None;
    }
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut cur = Some(id);
    while let Some(n) = cur {
        match dom.node_type(n) {
            1 => {
                let raw = dom.tag_name(n).unwrap_or("");
                let el_ns: Option<String> = dom.namespace_uri(n).map(str::to_string);
                if el_ns.as_deref() == Some(ns) {
                    if let Some((prefix, _)) = raw.split_once(':') {
                        return Some(prefix.to_string());
                    }
                }
                for name in dom.attribute_names(n) {
                    let Some(prefix) = name.strip_prefix("xmlns:") else { continue };
                    if dom.get_attribute(n, &name).is_some_and(|v| v == ns) {
                        return Some(prefix.to_string());
                    }
                }
                cur = parent_element(dom, n);
            }
            // Document delegates to its document element; a doctype or fragment has no element
            // context and so no prefix. Everything else asks its parent, as the spec's
            // "locate a namespace prefix" does.
            9 => cur = document_element(dom, n),
            10 | 11 => return None,
            _ => cur = dom.parent(n),
        }
    }
    None
}

/// The node a `Node` member's receiver is: an element or character data ([`unwrap_id`]), or a Document -- its root.
/// The three namespace lookups unwrapped elements alone, so on a Document they answered as if it had no element
/// context: `document.lookupNamespaceURI(null)` read null where Chrome 152 reads the XHTML namespace (the
/// document element's; r3a.html, 2026-10-23).
fn lookup_receiver(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<u32> {
    if let Some(id) = unwrap_id(scope, this) {
        return Some(id);
    }
    let slot = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, this).map(|p| unsafe { p.as_ref() }.root)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    Some(crate::core::dom::document::resolve_doc_root(&st.borrow::<HostDom>().0, slot))
}

pub(crate) fn cb_lookup_prefix(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "lookupPrefix") { return; }
    let id = match lookup_receiver(scope, args.this().into()) { Some(i) => i, None => { rv.set_null(); return; } };
    let a0 = args.get(0);
    // A null namespace has no prefix; every other value is stringified first, as WebIDL's
    // `DOMString?` requires.
    if a0.is_null_or_undefined() { rv.set_null(); return; }
    let ns = a0.to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match lookup_prefix(&op_state, id, &ns) {
        Some(p) => { let v = str_val(scope, &p); rv.set(v); }
        None => rv.set_null(),
    }
}

pub(crate) fn cb_lookup_namespace_uri(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "lookupNamespaceURI") { return; }
    let id = match lookup_receiver(scope, args.this().into()) { Some(i) => i, None => { rv.set_null(); return; } };
    let a0 = args.get(0);
    let prefix = if a0.is_null_or_undefined() { None } else { Some(a0.to_rust_string_lossy(scope)) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match lookup_ns_uri(&op_state, id, prefix.as_deref().filter(|s| !s.is_empty())) {
        Some(u) => { let v = u.to_v8(scope); rv.set(v); }
        None => rv.set_null(),
    }
}
pub(crate) fn cb_is_default_namespace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "isDefaultNamespace") { return; }
    let id = match lookup_receiver(scope, args.this().into()) { Some(i) => i, None => { rv.set_bool(false); return; } };
    let a0 = args.get(0);
    // "" and null/undefined both mean "no namespace" for isDefaultNamespace.
    let ns = if a0.is_null_or_undefined() { None } else { let s = a0.to_rust_string_lossy(scope); if s.is_empty() { None } else { Some(s) } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let default = lookup_ns_uri(&op_state, id, None);
    rv.set_bool(default.as_ref().map(NsUri::as_str) == ns.as_deref());
}
// Element.setAttributeNS(namespace, qualifiedName, value): Korvus keys attributes by qualified name, so
// this stores the qualified name verbatim (the namespace argument is honored only insofar as it lets the
// xmlns:/xmlns declarations round-trip through lookupNamespaceURI). getAttributeNS looks up by localName.

// ---- Shadow DOM members (Korvus GS ②-A3): shadowRoot / host / mode and the four init flags.
// (attachShadow and isConnected stay in bindings.rs with the wrap_cached/NodeCache spine.) All node
// wrappers ride `wrap_cached`, so a shadow-root result is ShadowRoot-typed with canonical identity.
//
// `shadowRoot.querySelector[All]` used to live here too, as a second pair of shadow-scoped callbacks.
// They were removed: `ShadowRoot` inherits both members from `DocumentFragment`, whose
// rows mount `core/dom/shared.rs`'s `cb_query_selector` / `cb_query_selector_all`, and those already
// branch on `dom.is_shadow_root(id)` into the same `shadow_descendants_matching` walk -- so the pair
// here was reachable from nothing and had been for some time. ----
/// `element.shadowRoot` — the host's OPEN shadow root, or null. Closed shadow roots are hidden from
/// script (the encapsulation contract): shadow_mode must be Open to return the wrapper.
pub(crate) fn cb_shadow_root_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let host_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => { rv.set_null(); return; }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let open_root: Option<u32> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.shadow_root_of(host_id).filter(|&r| dom.shadow_mode(r) == Some(ShadowRootMode::Open))
    };
    match open_root {
        Some(r) => { let o = wrap_cached(scope, &op_state, r); rv.set(o.into()); }
        None => rv.set_null(),
    }
}

/// `shadowRoot.host` — the host element (always present for a real shadow root).
pub(crate) fn cb_shadow_host_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let root_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => { rv.set_null(); return; }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let host = { op_state.borrow().borrow::<HostDom>().0.shadow_host_of(root_id) };
    match host {
        Some(h) => { let o = wrap_cached(scope, &op_state, h); rv.set(o.into()); }
        None => rv.set_null(),
    }
}

/// `shadowRoot.mode` — "open" | "closed".
pub(crate) fn cb_shadow_mode_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let root_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => { rv.set_null(); return; }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mode = { op_state.borrow().borrow::<HostDom>().0.shadow_mode(root_id) };
    match mode {
        Some(ShadowRootMode::Open) => { let v = str_val(scope, "open"); rv.set(v); }
        Some(ShadowRootMode::Closed) => { let v = str_val(scope, "closed"); rv.set(v); }
        None => rv.set_null(),
    }
}

/// `shadowRoot.delegatesFocus`.
pub(crate) fn cb_shadow_delegates_focus_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { rv.set(v8::Boolean::new(scope, false).into()); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = { op_state.borrow().borrow::<HostDom>().0.shadow_delegates_focus(root_id) }.unwrap_or(false);
    rv.set(v8::Boolean::new(scope, v).into());
}

/// `shadowRoot.slotAssignment` — "named" | "manual".
pub(crate) fn cb_shadow_slot_assignment_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { rv.set(str_val(scope, "named")); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = { op_state.borrow().borrow::<HostDom>().0.shadow_slot_assignment(root_id) };
    let name = match v { Some(ShadowSlotAssignment::Manual) => "manual", _ => "named" };
    rv.set(str_val(scope, name));
}

/// `shadowRoot.referenceTarget` (Chrome 152): the ID references to the host resolve to inside the shadow tree, or null.
pub(crate) fn cb_shadow_reference_target_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { rv.set_null(); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = { op_state.borrow().borrow::<HostDom>().0.shadow_reference_target(root_id).map(str::to_string) };
    match v {
        Some(t) => rv.set(str_val(scope, &t)),
        None => rv.set_null(),
    }
}

/// `shadowRoot.referenceTarget = v`: `attribute DOMString?` -- null and undefined clear it, anything else is its string.
pub(crate) fn cb_shadow_reference_target_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { return };
    let v = args.get(0);
    let target = if v.is_null_or_undefined() {
        None
    } else {
        let Some(s) = v.to_string(scope) else { return };
        Some(s.to_rust_string_lossy(scope))
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_shadow_reference_target(root_id, target.as_deref());
}

/// `shadowRoot.clonable`.
pub(crate) fn cb_shadow_clonable_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { rv.set(v8::Boolean::new(scope, false).into()); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = { op_state.borrow().borrow::<HostDom>().0.shadow_clonable(root_id) }.unwrap_or(false);
    rv.set(v8::Boolean::new(scope, v).into());
}

/// `shadowRoot.serializable`.
pub(crate) fn cb_shadow_serializable_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root_id) = unwrap_id(scope, args.this().into()) else { rv.set(v8::Boolean::new(scope, false).into()); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = { op_state.borrow().borrow::<HostDom>().0.shadow_serializable(root_id) }.unwrap_or(false);
    rv.set(v8::Boolean::new(scope, v).into());
}

/// The <slot> `slot_id`'s assigned node ids (tree order). `flatten` -> the flattened rendered
/// children (assigned nodes, or the slot's own fallback children when nothing is assigned);
/// otherwise the directly-assigned nodes. A non-<slot> / detached <slot> -> empty vec.
fn slot_assigned_ids(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, slot_id: u32, flatten: bool) -> Vec<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if flatten { dom.slot_rendered_children(slot_id) } else { dom.assigned_nodes(slot_id) }.unwrap_or_default()
}

/// `slot.assignedNodes({flatten})` — the slottables assigned to this <slot>, as an array of interned
/// node wrappers in tree order.
pub(crate) fn cb_assigned_nodes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let slot_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => return,
    };
    let flatten = v8::Local::<v8::Object>::try_from(args.get(0)).ok()
        .and_then(|o| get_prop(scope, o, "flatten"))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ids = slot_assigned_ids(&op_state, slot_id, flatten);
    let arr = v8::Array::new(scope, ids.len() as i32);
    for (i, cid) in ids.iter().enumerate() {
        let w = wrap_cached(scope, &op_state, *cid);
        arr.set_index(scope, i as u32, w.into());
    }
    rv.set(arr.into());
}

/// `slot.assignedElements({flatten})` — like assignedNodes but keeping only Element nodes (text
/// slottables are dropped). `tag_name(id).is_some()` is the element test (text nodes return None).
pub(crate) fn cb_assigned_elements(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let slot_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => return,
    };
    let flatten = v8::Local::<v8::Object>::try_from(args.get(0)).ok()
        .and_then(|o| get_prop(scope, o, "flatten"))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ids: Vec<u32> = {
        let all = slot_assigned_ids(&op_state, slot_id, flatten);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        all.into_iter().filter(|&n| dom.tag_name(n).is_some()).collect()
    };
    let arr = v8::Array::new(scope, ids.len() as i32);
    for (i, cid) in ids.iter().enumerate() {
        let w = wrap_cached(scope, &op_state, *cid);
        arr.set_index(scope, i as u32, w.into());
    }
    rv.set(arr.into());
}

// Element-scoped getElementsByTagName / getElementsByClassName used to have a pair of callbacks here,
// each snapshotting matches into a bare `v8::Array`. Both were removed with the ops that
// were their only name: the Element table mounts `collections.rs`'s `cb_el_by_name` for both rows
// (`Kind::ElByTag` / `Kind::ElByClass`), which returns a LIVE HTMLCollection over the arena -- so the
// pair here answered a different type from the one the surface actually has.

// ==================== Element.scrollTop / scrollLeft (native accessors) ====================
// No layout engine, so the values are just per-element state (arena node id -> position). A change
// schedules a coalesced 'scroll' then 'scrollend' (many sync writes -> one 'scroll'; a write inside the
// handler continues/restarts the cycle), matching the JS these replace.
#[derive(Default)]
pub(crate) struct ScrollState {
    m: std::collections::HashMap<u32, ScrollRec>,
}
#[derive(Default, Clone, Copy)]
struct ScrollRec { top: f64, left: f64, scheduled: bool, dirty: bool }

/// An element's own scroll offset, `(scrollLeft, scrollTop)` -- `(0, 0)` for one never scrolled. For
/// the lazy-load geometry (`core/html/lazy_load.rs`), which has to shift a scroller's content by it.
pub(crate) fn element_scroll_offset(st: &deno_core::OpState, id: u32) -> (f64, f64) {
    st.try_borrow::<ScrollState>().and_then(|s| s.m.get(&id)).map(|r| (r.left, r.top)).unwrap_or((0.0, 0.0))
}

/// Every element scrolled away from its origin, `(id, (scrollLeft, scrollTop))` -- what a layout is
/// told before it answers in client coordinates (`geometry_queries::scroll_snapshot`).
pub(crate) fn scrolled_elements(st: &deno_core::OpState) -> Vec<(u32, (f64, f64))> {
    st.try_borrow::<ScrollState>()
        .map(|s| s.m.iter().filter(|(_, r)| r.left != 0.0 || r.top != 0.0).map(|(id, r)| (*id, (r.left, r.top))).collect())
        .unwrap_or_default()
}

/// Does this element have a scrolling box at all?
///
/// CSSOM opens both `scrollTop` and `scrollLeft` with "if the element does not have any associated
/// box, return zero" -- and, for the setter, "terminate these steps". A detached element never has
/// one, so `document.createElement("div").scrollTop = 1` leaves scrollTop at 0 in Chrome; Korvus
/// stored the 1 and handed it back, which is a plausible-looking answer to a question CSSOM says has
/// only one.
///
/// Connectedness is what this can honestly check. A CONNECTED element with `display: none` also has
/// no box and is not caught here -- that needs layout, which lives in korvus-page, not in the arena.
/// Narrowing the lie is worth doing even where it cannot be removed.
///
/// "In a document" is any document a browsing context shows, not only the page's: a frame's
/// `documentElement` scrolls its frame (Chrome, `frame.contentWindow.scrollTo(0, 70)`
/// leaves the frame at `scrollY` 70 and the page at 0). A node in a document nothing lays out (a
/// `DOMParser` result) passes this test and is still stopped by the clamp, whose extent is zero there.
fn has_scroll_box(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.is_in_document(id) || {
        let root = dom.shadow_including_root(id);
        root != id && dom.node_type(root) == 9
    }
}

/// `Element.checkVisibility(options)` — CSSOM-View: is this element rendered at all?
///
/// The spec's answer is false unless the element has an associated box, and a detached element never
/// does. Korvus can see connectedness; it cannot see `display: none` on a connected one, which needs
/// layout (korvus-page). The options bag is read for its shape -- a non-object argument is a WebIDL
/// TypeError -- but nothing in it can change the answer for an element with no box at all.
pub(crate) fn cb_el_check_visibility(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match arg_node_id(scope, args.this().into()) { Some(i) => i, None => { rv.set_bool(false); return; } };
    // `optional CheckVisibilityOptions options = {}`, converted (Chrome 150): its five members read
    // once in WebIDL's order -- they cannot change the answer for an element with no box, but a page's getters run
    // (korvus checked the argument's type and read nothing)
    if crate::install::ctor_dicts::convert_dict(scope, args.get(0), "CheckVisibilityOptions", "Failed to execute 'checkVisibility' on 'Element': ").is_err() {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    // connected: in the page's document, or in a document a frame shows
    let connected = {
        let dom = &st.borrow::<HostDom>().0;
        let root = dom.shadow_including_root(id);
        dom.is_in_document(id) || (root != id && dom.node_type(root) == 9 && crate::core::html::browsing_context::frame_element_showing(&st, root).is_some())
    };
    if !connected {
        rv.set_bool(false);
        return;
    }
    // A CONNECTED element still has no box when its computed `display` is `none`, or when an
    // ancestor's is. The old body stopped at connectedness under a comment saying the rest "needs
    // layout (korvus-page)" -- which was true when it was written and has not been for a long time:
    // `getComputedStyle` resolves through `page::Page::computed_value` on this very DOM, and this
    // reads the same answer through the same handle. The axis reported the member as answering
    // `true` for both a visible element and a `display:none` one, which is one answer for two
    // states the browser distinguishes.
    let page = st
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<page::PageHandle>())
        .map(|h| h.0.clone());
    let Some(p) = page else {
        // No page attached (a detached document in a test harness). Connectedness is all we know,
        // and claiming more would be the invention this change removes.
        rv.set_bool(true);
        return;
    };
    // A FRAME document's element is styled against its frame (its own media, its own sheets), as
    // `getComputedStyle` styles it: read through the page's style basis, every element of a frame answered
    // `display: none` here, so Playwright's actionability check called a frame's buttons invisible.
    let in_frame = {
        let dom = &st.borrow::<HostDom>().0;
        dom.shadow_including_root(id) != dom.root()
    };
    if in_frame {
        drop(st);
        let mut st = op_state.borrow_mut();
        let mut n = Some(id);
        while let Some(cur) = n {
            let element = st.borrow::<HostDom>().0.node_type(cur) == 1;
            if element && crate::core::css::cssom::css_computed_one(&mut st, cur, "display") == "none" {
                rv.set_bool(false);
                return;
            }
            n = st.borrow::<HostDom>().0.parent(cur);
        }
        drop(p);
        rv.set_bool(true);
        return;
    }
    let dom = &st.borrow::<HostDom>().0;
    // The document's own media, as every other style read takes it (`getComputedStyle`, `getBoundingClientRect`).
    // It was `MediaContext::default()` -- a 1024x768 screen with no theme -- so a `@media` rule answered for
    // another viewport here, and every call moved the page's style basis: www.ticketmaster.com alternates
    // `checkVisibility()` with `getBoundingClientRect()`, and 975 of its 997 layouts restyled every element
    // (perf counters).
    let media = crate::core::dom::geometry_queries::media_context(&st);
    let Some(display) = page::style::property::PropertyId::from_name("display") else {
        rv.set_bool(true);
        return;
    };
    // `display:none` on the element OR on any ancestor removes the box, which is what
    // `checkVisibility()` reports with its default options.
    let mut n = Some(id);
    while let Some(cur) = n {
        // `None` layout: `display` is not a resolved-value property, so the parameter
        // added is never reached from here and no box tree is built.
        if p.computed_value(dom, cur, &media, display, None).as_deref() == Some("none") {
            rv.set_bool(false);
            return;
        }
        n = dom.parent(cur);
    }
    rv.set_bool(true);
}

/// `Element.currentCSSZoom` — the effective `zoom` of this element's box, 1 when there is none.
///
/// Korvus applies no zoom, so the honest constant is 1. It is a readonly double and Chrome answers 1
/// for an unzoomed element, detached or not.
pub(crate) fn cb_el_current_css_zoom(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = &args;
    let n = v8::Number::new(scope, 1.0);
    rv.set(n.into());
}

/// `Element.pseudo(type)` — the CSSPseudoElement for `::before` and friends, or null.
///
/// Korvus generates no pseudo-element boxes, so the answer is always null -- which is also Chrome's
/// answer for an element with no matching pseudo. The argument is REQUIRED (`pseudo(CSSOMString type)`),
/// so a zero-argument call is a TypeError, and that half is not a stub: it is the whole observable
/// difference between a member that exists and one that does not.
pub(crate) fn cb_el_pseudo(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Element", "pseudo") { return; }
    rv.set_null();
}

/// Is this node the DOCUMENT ELEMENT -- the one whose scrolling box is the viewport's?
///
/// In a no-quirks document `document.scrollingElement` is the document element, and its
/// `scrollTop`/`scrollLeft` ARE `window.scrollY`/`scrollX`. Measured on Chrome 150.0.7871.46 over
/// CDP (2026-09-13) on a 4300px page in a 993px viewport: `documentElement.scrollTop = 2e6` left
/// `scrollY` at 3307, and `scrollTo(0, 2e6)` left `documentElement.scrollTop` at 3307 -- one store,
/// readable through either member. Korvus kept two, so `scrollY` moved while
/// `documentElement.scrollTop` stayed 0 and the other way round: an incoherence a page reads by
/// comparing the two numbers.
///
/// `<body>` is NOT that element here: in the same measurement `body.scrollTop = 2e6` left both
/// `body.scrollTop` and `scrollY` at 0. Quirks mode is where a browser moves the role to `<body>`,
/// and Korvus parses nothing in quirks mode (`Document.compatMode` is the constant `CSS1Compat`),
/// so the document element is the only case there is.
fn is_document_element(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    crate::core::dom::document::doc_element_child(dom, dom.root()) == Some(id)
}

/// The document element's node id, and the element object to dispatch its scroll events at.
///
/// `pub(crate)` for `geometry_queries::viewport_client_size`: `visualViewport`'s
/// `width`/`height` are the document element's client area, and asking for that node a second way
/// would be a second answer to "which element is the viewport".
pub(crate) fn document_element_id(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    crate::core::dom::document::doc_element_child(dom, dom.root())
}

/// How far this node can be scrolled on `axis` (0 = vertical, 1 = horizontal).
///
/// No branch for the document element, even though its position is stored elsewhere: its box IS the
/// root box, and `LaidOut::scroll_extent` already treats the root as scrollable whatever its
/// `overflow` says (`html { overflow: hidden }` still scrolls — measured). A second "is this the
/// document element" test here would be a second chance to answer it differently.
///
/// Zero for a box that does not scroll and for a build with no page pipeline: nothing moves, so
/// nothing may be stored.
fn scroll_limit(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32, axis: u8) -> f64 {
    let mut st = op_state.borrow_mut();
    let (x, y) = crate::core::dom::geometry_queries::element_scroll_extent(&mut st, id);
    if axis == 0 { y } else { x }
}

fn scroll_read(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, axis: u8) -> f64 {
    let id = match arg_node_id(scope, this) { Some(i) => i, None => return 0.0 };
    // `Element::scrollTop` runs `UpdateStyleAndLayoutForNode` first, so a transition becomes due here as it does at
    // any other layout read (a bot-detection service D7; `animation::css::flush_style_for`).
    crate::core::animation::css::flush_style_for(scope, id);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !has_scroll_box(&op_state, id) { return 0.0; }
    if is_document_element(&op_state, id) {
        let st = op_state.borrow();
        let (x, y) = crate::core::html::window_geometry::scroll_position(&st);
        return if axis == 0 { y } else { x };
    }
    let st = op_state.borrow();
    st.borrow::<ScrollState>().m.get(&id).map(|r| if axis == 0 { r.top } else { r.left }).unwrap_or(0.0)
}

/// Write one axis of one element's scroll position, clamped to what it can actually reach.
///
/// The clamp is the half this file was missing. `el.scrollTop = 2e6` stored 2000000 on EVERY element
/// -- a `<div>` with `overflow: visible`, `<body>`, the document element -- and handed it back.
/// Chrome, measured on three 100px divs each holding a 900px child: `visible` stays 0, `clip` stays
/// 0, `hidden` clamps at 800 = `scrollHeight - clientHeight`; `body` stays 0; the document element
/// goes to 3307 and takes `window.scrollY` with it.
fn scroll_write(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, axis: u8, v: f64) {
    let id = match arg_node_id(scope, this) { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !has_scroll_box(&op_state, id) { return; }
    let nv = clamp_scroll(v, scroll_limit(&op_state, id, axis));
    if store_scroll(&op_state, id, axis, nv) {
        scroll_mark(scope, this, id);
    }
}

/// `+v`, floored at 0 and ceilinged at what the box can reach. NaN is 0, as it was.
fn clamp_scroll(v: f64, limit: f64) -> f64 {
    if v.is_nan() || v < 0.0 { 0.0 } else { v.min(limit) }
}

/// Store one axis of one element's scroll position; `true` if it moved.
///
/// The document element's position lives in [`ScrollPosition`] -- the same cell `window.scrollX` and
/// `scrollY` read -- because they are the same position (see [`is_document_element`]). Its scroll
/// EVENT bookkeeping stays in [`ScrollState`] alongside every other element's, because that is what
/// `scroll_mark` and `cb_scroll_flush` coalesce on; only the two numbers move.
fn store_scroll(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32, axis: u8, nv: f64) -> bool {
    let root = is_document_element(op_state, id);
    let mut st = op_state.borrow_mut();
    if root {
        if st.try_borrow::<crate::core::html::window_geometry::ScrollPosition>().is_none() {
            st.put(crate::core::html::window_geometry::ScrollPosition::default());
        }
        let pos = st.borrow_mut::<crate::core::html::window_geometry::ScrollPosition>();
        let cur = if axis == 0 { pos.y } else { pos.x };
        if nv == cur { return false; }
        if axis == 0 { pos.y = nv; } else { pos.x = nv; }
        st.borrow_mut::<ScrollState>().m.entry(id).or_default().dirty = true;
        return true;
    }
    let rec = st.borrow_mut::<ScrollState>().m.entry(id).or_default();
    let cur = if axis == 0 { rec.top } else { rec.left };
    if nv == cur { return false; }
    if axis == 0 { rec.top = nv; } else { rec.left = nv; }
    rec.dirty = true;
    true
}

/// `window.scrollTo` / `scroll` / `scrollBy`, as the DOCUMENT ELEMENT's scroll.
///
/// One entry point rather than a second copy of the clamp in `window_geometry.rs`, because the two
/// members write the same position: `window.scrollTo(0, 2e6)` and `documentElement.scrollTop = 2e6`
/// are one operation in Chrome and are one here. `None` on an axis means "leave it alone", which is
/// what `scrollTo` does with an absent argument; `by` adds instead of assigning, and has no such
/// case (`scrollBy` treats an absent delta as 0).
pub(crate) fn scroll_document(scope: &mut v8::PinScope, x: Option<f64>, y: Option<f64>, by: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = document_element_id(&op_state);
    // The DOCUMENT's extent, asked without a node id -- which is the honest shape for the window's
    // own members. A document with no element at all keeps the bookkeeping `scrollX`/`scrollY` read
    // back, and its extent is zero, so every write lands on 0.
    let (max_x, max_y) = {
        let mut st = op_state.borrow_mut();
        crate::core::dom::geometry_queries::document_scroll_extent(&mut st)
    };
    let (cur_x, cur_y) = {
        let st = op_state.borrow();
        crate::core::html::window_geometry::scroll_position(&st)
    };
    let target = |cur: f64, arg: Option<f64>, max: f64| -> f64 {
        match (by, arg) {
            (true, a) => clamp_scroll(cur + a.unwrap_or(0.0), max),
            (false, Some(a)) => clamp_scroll(a, max),
            (false, None) => cur,
        }
    };
    let (nx, ny) = (target(cur_x, x, max_x), target(cur_y, y, max_y));
    let Some(id) = id else {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<crate::core::html::window_geometry::ScrollPosition>().is_none() {
            st.put(crate::core::html::window_geometry::ScrollPosition::default());
        }
        let pos = st.borrow_mut::<crate::core::html::window_geometry::ScrollPosition>();
        pos.x = nx;
        pos.y = ny;
        return;
    };
    let moved = store_scroll(&op_state, id, 1, nx) | store_scroll(&op_state, id, 0, ny);
    if moved {
        let el = crate::binding::protos::wrap_cached(scope, &op_state, id);
        scroll_mark(scope, el.into(), id);
    }
}
/// `scrollTo` / `scroll` / `scrollBy` on a nested browsing context's WINDOW: the scroll of the frame's
/// own document element, clamped to the frame document's extent. The page's scroll is
/// [`scroll_document`] and is not touched: Chrome, measured, leaves the page at `(0, 0)` after
/// `frame.contentWindow.scrollTo(0, 70)` moved the frame to `(0, 70)`.
pub(crate) fn scroll_frame_document(scope: &mut v8::PinScope, document_element: u32, x: Option<f64>, y: Option<f64>, by: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = crate::binding::protos::wrap_cached(scope, &op_state, document_element);
    let (cur_x, cur_y) = {
        let st = op_state.borrow();
        element_scroll_offset(&st, document_element)
    };
    let target = |cur: f64, arg: Option<f64>| -> Option<f64> {
        match (by, arg) {
            (true, a) => Some(cur + a.unwrap_or(0.0)),
            (false, a) => a,
        }
    };
    if let Some(nx) = target(cur_x, x) {
        scroll_write(scope, el.into(), 1, nx);
    }
    if let Some(ny) = target(cur_y, y) {
        scroll_write(scope, el.into(), 0, ny);
    }
}

/// THE WHEEL'S DEFAULT ACTION: scroll. A `wheel` event no listener cancelled scrolls the first box on the chain from
/// the element under the pointer up to the viewport that the USER may scroll (`overflow` `auto`, `scroll` or
/// `overlay`; the viewport unless the root or the body it propagates from is `hidden` or `clip`) and that can still
/// move the way the wheel turned -- each axis on its own, by the wheel's pixel delta, clamped to the box's extent.
/// `scroll` and `scrollend` follow as for any scroll (`scroll_mark`). `true` if anything moved.
///
/// The event was all a wheel did: `Input.dispatchMouseEvent` `mouseWheel` reached the page's listeners and left
/// `scrollY` at 0, where Chrome scrolls the page (measured 2026-10-22, D72) -- a front end driving the page by CDP
/// could not scroll it. `overflow: hidden` boxes scroll from script (`scrollTop`, clamped) but never from the wheel.
pub(crate) fn wheel_scroll(scope: &mut v8::PinScope, target: u32, dx: f64, dy: f64) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut left = [dy, dx]; // by axis: 0 vertical, 1 horizontal
    let mut moved = false;
    let mut node = Some(target);
    while let Some(n) = node {
        if left == [0.0, 0.0] {
            break;
        }
        let (is_element, parent, parent_is_document, is_page_root) = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            let parent = dom.parent(n);
            let parent_is_document = parent.is_some_and(|p| dom.node_type(p) == 9);
            (dom.node_type(n) == 1, parent, parent_is_document, parent == Some(dom.root()))
        };
        if is_element {
            let viewport = parent_is_document;
            for axis in 0..2u8 {
                let d = left[axis as usize];
                if d == 0.0 || !user_scrollable(&op_state, n, axis, viewport) {
                    continue;
                }
                let limit = if viewport && is_page_root {
                    let mut st = op_state.borrow_mut();
                    let (x, y) = crate::core::dom::geometry_queries::document_scroll_extent(&mut st);
                    if axis == 0 { y } else { x }
                } else {
                    scroll_limit(&op_state, n, axis)
                };
                let cur = {
                    let st = op_state.borrow();
                    let (x, y) = if viewport && is_page_root {
                        crate::core::html::window_geometry::scroll_position(&st)
                    } else {
                        element_scroll_offset(&st, n)
                    };
                    if axis == 0 { y } else { x }
                };
                let nv = clamp_scroll(cur + d, limit);
                if nv == cur {
                    continue; // at its end this way: the next box up takes the delta
                }
                if viewport && is_page_root {
                    let (x, y) = if axis == 0 { (None, Some(nv)) } else { (Some(nv), None) };
                    scroll_document(scope, x, y, false);
                } else {
                    let el = crate::binding::protos::wrap_cached(scope, &op_state, n);
                    scroll_write(scope, el.into(), axis, nv);
                }
                left[axis as usize] = 0.0;
                moved = true;
            }
        }
        // A frame's viewport is the last box of its own document's chain.
        node = if parent_is_document { None } else { parent };
    }
    moved
}

/// May the user scroll `id` on `axis` (0 vertical, 1 horizontal)? A box: its own `overflow-y`/`overflow-x` is
/// `auto`, `scroll` or `overlay`. The viewport (`viewport`, the document element): unless its `overflow` is `hidden`
/// or `clip` -- the root's own, or the body's when the root's is `visible` (CSS Overflow 3, "overflow viewport
/// propagation").
fn user_scrollable(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32, axis: u8, viewport: bool) -> bool {
    let prop = if axis == 0 { "overflow-y" } else { "overflow-x" };
    let mut st = op_state.borrow_mut();
    let own = crate::core::css::cssom::css_computed_one(&mut st, id, prop);
    if !viewport {
        return matches!(own.as_str(), "auto" | "scroll" | "overlay");
    }
    let shown = if own == "visible" {
        let body = {
            let dom = &st.borrow::<HostDom>().0;
            let mut c = dom.first_child(id);
            let mut found = None;
            while let Some(n) = c {
                if dom.node_type(n) == 1 && dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("body")) {
                    found = Some(n);
                    break;
                }
                c = dom.next_sibling(n);
            }
            found
        };
        body.map(|b| crate::core::css::cssom::css_computed_one(&mut st, b, prop)).unwrap_or(own)
    } else {
        own
    };
    !matches!(shown.as_str(), "hidden" | "clip")
}

fn scroll_mark(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, id: u32) {
    // A scroll moves content relative to the viewport, so a `loading=lazy` element may now be near it.
    crate::core::html::lazy_load::scrolled(scope);
    let already = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let rec = st.borrow_mut::<ScrollState>().m.entry(id).or_default();
        if rec.scheduled { true } else { rec.scheduled = true; false }
    };
    if already { return; }
    let this_g = v8::Global::new(scope, this);
    let this_s = v8::Local::new(scope, &this_g);
    let cb = build_fn_data(scope, cb_scroll_flush, "", 0, this_s);
    // The engine's task, not the page's `setTimeout`.
    crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
}
/// Fire `scroll` / `scrollend` for a scroll of `el`. A box's go to the box and do not bubble. The VIEWPORT's -- the
/// document element's scroll -- go to the DOCUMENT and bubble, so they reach `window` (CSSOM View, "scroll steps"):
/// fired at the document element without bubbling, they never reached a `window` listener, which is where infinite
/// scrolling, sticky headers and lazy loaders listen (Chrome 152 answers one per `scrollTo`; korvus none, measured
/// 2026-10-23). Neither is cancelable.
fn scroll_fire(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, ty: &str) {
    let viewport_doc = arg_node_id(scope, el).and_then(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.parent(id).filter(|&p| dom.node_type(p) == 9)
    });
    let (target, bubbles): (v8::Local<v8::Value>, bool) = match viewport_doc {
        Some(doc) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            (crate::binding::protos::wrap_cached(scope, &op_state, doc).into(), true)
        }
        None => (el, false),
    };
    if let Some(ev) = scroll_event(scope, ty, bubbles) {
        crate::binding::protos::engine_call(scope, target, "dispatchEvent", &[ev]);
    }
}

/// A trusted, non-cancelable `scroll` / `scrollend` event.
fn scroll_event<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, bubbles: bool) -> Option<v8::Local<'s, v8::Value>> {
    let evc = v8::Local::<v8::Function>::try_from(crate::binding::protos::iface_object(scope, "Event")?).ok()?;
    let ty_s = str_val(scope, ty);
    let opts = crate::binding::protos::init_dict(scope);
    set_bool(scope, opts, "bubbles", bubbles);
    let ev = evc.new_instance(scope, &[ty_s, opts.into()])?;
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    Some(ev.into())
}
fn scroll_dirty(scope: &mut v8::PinScope, id: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.borrow::<ScrollState>().m.get(&id).map(|r| r.dirty).unwrap_or(false)
}
pub(crate) fn cb_scroll_flush(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let el_g = v8::Global::new(scope, __body_data);
    let el = v8::Local::new(scope, &el_g);
    let id = match arg_node_id(scope, el) { Some(i) => i, None => return };
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(rec) = st.borrow_mut::<ScrollState>().m.get_mut(&id) { rec.scheduled = false; rec.dirty = false; }
    }
    scroll_fire(scope, el, "scroll");
    if scroll_dirty(scope, id) { scroll_mark(scope, el, id); return; } // changed during 'scroll' -> keep scrolling
    scroll_fire(scope, el, "scrollend");
    if scroll_dirty(scope, id) { scroll_mark(scope, el, id); } // changed during 'scrollend' -> new sequence
}
pub(crate) fn cb_scroll_top_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let v = scroll_read(scope, args.this().into(), 0); let n = crypto_num(scope, v); rv.set(n); }
pub(crate) fn cb_scroll_top_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { let v = args.get(0).number_value(scope).unwrap_or(0.0); scroll_write(scope, args.this().into(), 0, v); }
pub(crate) fn cb_scroll_left_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let v = scroll_read(scope, args.this().into(), 1); let n = crypto_num(scope, v); rv.set(n); }
pub(crate) fn cb_scroll_left_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { let v = args.get(0).number_value(scope).unwrap_or(0.0); scroll_write(scope, args.this().into(), 1, v); }

/// THE ONE ANSWER every scroll method gives: a promise for `{ interrupted: false }`.
///
/// `scrollTo` / `scroll` / `scrollBy` / `scrollIntoView` all answer it, on `Window` and on `Element`
/// alike, which is why it is one function. It was not one before: `core/dom/element.rs` had THREE
/// copies of `resolved_promise(undefined)` -- the same wrong value written out three times -- and
/// `core/html/window_geometry.rs::cb_scroll` had no copy at all and answered `undefined`.
///
/// MEASURED, not read off a spec (Chrome
/// 150.0.7871.46 `--headless=new`). The capture records only the TYPE -- every
/// `Object.prototype.toString.call(o.scrollTo(...))` in
/// `reference/captures/brave-151-linux-raw-20260907T190618.json` reads `[object Promise]` and there
/// is not one `await` entry -- so the three things the type cannot say were measured separately:
///
///   * WHAT it resolves to is NOT `undefined`, which is what this engine answered and what the
///     obvious reading of the old spec text says. It is a fresh plain object (prototype
///     `Object.prototype`) with ONE own property, `interrupted`, a writable/enumerable/configurable
///     boolean. Two calls answer two distinct objects.
///   * `interrupted` is `true` exactly when a LATER scroll cut this one short: measured `true` for
///     the first of two back-to-back smooth scrolls, for a smooth scroll superseded 120ms in, and for
///     a smooth scroll cut off by an instant one. Nothing here can produce that -- this engine has no
///     animation, so every scroll it performs runs to completion in its own call -- so `false` is not
///     a placeholder, it is the right answer for every scroll this engine can do.
///   * WHETHER it can reject: yes, but never for the scroll. Chrome turns the ARGUMENT and RECEIVER
///     failures into rejections rather than synchronous throws. Interruption does NOT reject -- it
///     fulfils with `interrupted: true`. The two rejecting shapes, measured on all seven members:
///       - a bad `ScrollBehavior` enum, `scrollTo({behavior:"bogus"})`;
///       - a foreign receiver, `<fn>.call({}, 0, 0)` and `.call(Window.prototype, 0, 0)` alike,
///         with `TypeError: Failed to execute '<name>' on '<Window|Element>': Illegal invocation`
///         -- which `tests/fixtures/p12g-frame-sweep.chrome-150.json` recorded independently as
///         `"foreign": "rej:..."` for the three window names.
///     NEITHER is implemented here: every receiver gets this resolved promise. They need an enum
///     validator and a receiver brand, not a value, so they are a separate unit rather than a line
///     in this one, and they are why `scroll`/`scrollTo`/`scrollBy` stay in `frame_globals_live.rs`'s
///     `NOT_ASSERTED`.
///
/// KNOWN DIVERGENCE, deliberate and measured rather than assumed. Chrome never settles this promise
/// in the microtask drain that called it -- not even for an instant scroll with nothing to move (the
/// probe's generation counter reads its ceiling in every case, including a no-op scroll and an
/// unscrollable document). It settles in a task that runs after the drain and before both a
/// `setTimeout(0)` queued alongside it and the next animation frame. This engine resolves it in the
/// drain instead. Deferring it would mean handing the promise to `rendering::queue_task`, which
/// answers silently and does nothing in a realm with no timer factory -- and a scroll promise that
/// NEVER settles is a far worse failure than one that settles early. The difference is observable
/// only in the ordering against other microtasks.
pub(crate) fn scroll_promise<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let o = v8::Object::new(scope);
    set_bool(scope, o, "interrupted", false);
    crate::binding::protos::resolved_promise(scope, o.into())
}

/// The CSSOM-View scroll methods on an element: `scroll(x, y)` / `scrollTo(x, y)` (absolute) and
/// `scrollBy(dx, dy)` (relative), each also taking the `{left, top}` dictionary form.
///
/// They were `#[fast] fn scroll(&self) {}` -- no arguments read, nothing written, and op2 renders
/// Rust `()` as null, so `el.scroll(1, 1)` moved nothing and answered null. The scroll POSITION was
/// already real (`ScrollState`, behind `scrollTop`/`scrollLeft`, gated on the element actually having
/// a scroll box), so these are the same two writes the property setters make, through the same
/// `scroll_write` -- which means an element with no scroll box is still unmoved, and a change still
/// marks the element for its `scroll`/`scrollend` events.
///
/// The return value is a Promise, which is what the recorded browser answers for all three on
/// Element AND on Window -- consistently, and from a `[native code]` function, so it is the engine's
/// behaviour and not a page's polyfill. It resolves immediately because this scroll IS immediate:
/// there is no compositor to animate it and nothing to wait for. `behavior: "smooth"` would be the
/// case that needs more than that, and this engine has no frame loop to smooth it over.
/// Which axes a `scroll`/`scrollTo`/`scrollBy` call names, and to what. `None` is "leave this axis
/// alone" (`scrollTo`) or "no delta" (`scrollBy`), which is why they are Options rather than zeros.
///
/// WHICH OVERLOAD IS CHOSEN BY ARITY, and that is measured, not inferred. Chrome 150.0.7871.46 over
/// CDP, 2026-09-13, from (100, 100) on a page with room to move — identical answers for
/// `window.scrollTo` and `Element.prototype.scrollTo`, which is why there is one parser:
///
/// ```text
/// scrollTo()                  (100, 100)   0 args -> the DICTIONARY overload, empty: nothing moves
/// scrollTo(10)                (100, 100)   1 arg is still the dictionary overload -- `10` is not
///                                          a ScrollToOptions, so it names no axis at all
/// scrollTo(10, undefined)      (10,   0)   2 args -> POSITIONAL, and both axes are written
/// scrollTo(undefined, 7)        (0,   7)
/// scrollTo(null, 7)             (0,   7)   null is 0 here, not "leave it alone"
/// scrollTo(NaN, 7)              (0,   7)   and so is every non-finite number
/// scrollTo(Infinity, 7)         (0,   7)
/// scrollTo("x", "y")            (0,   0)
/// scrollTo({})                (100, 100)   an absent member leaves its axis alone
/// scrollTo({top: 7})          (100,   7)
/// scrollTo({left: 3})           (3, 100)
/// scrollTo({left: null, top: 7}) (0,  7)   ...but a member that is THERE and null is 0
/// scrollBy()                  (100, 100)   the same arity rule: no axis named, nothing added
/// scrollBy(5)                 (100, 100)
/// scrollBy(null, 5)           (100, 105)
/// scrollBy({top: 5})          (100, 105)
/// ```
///
/// Korvus disagreed on five of those: it read one positional argument as `x`, and it treated a
/// positional `null`/`undefined` and a dictionary member explicitly set to `null` as "leave this
/// axis alone" where Chrome writes 0. `window_dims.rs` had `scrollTo(null, 7)` pinned at the old
/// answer.
///
/// The one-argument form is `optional ScrollToOptions options = {}`, converted as WebIDL converts it (Chrome 150):
/// `behavior` (an enum), `left`, `top`, each read once; anything but an object, `null` or
/// `undefined` is no dictionary. These methods answer a promise, so a failed conversion is a REJECTION: `Err` holds
/// the exception (`prefix` names the method, "Failed to execute 'scrollTo' on 'Element': "). korvus read `left` and
/// `top` alone and took `scrollTo(5)` and `{behavior: 'x'}` for a scroll.
pub(crate) fn scroll_args<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, prefix: &str) -> Result<(Option<f64>, Option<f64>), v8::Local<'s, v8::Value>> {
    if args.length() >= 2 {
        let (x, y) = (args.get(0), args.get(1));
        return Ok((Some(scroll_coord(scope, x)), Some(scroll_coord(scope, y))));
    }
    let obj = crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "ScrollToOptions", prefix)?;
    let mut member = |k: &str| -> Option<f64> {
        let v = get_prop(scope, obj, k).filter(|v| !v.is_undefined())?;
        Some(scroll_coord(scope, v))
    };
    let left = member("left");
    let top = member("top");
    Ok((left, top))
}

/// One coordinate of a scroll call: its number, or 0 for anything that is not a finite one.
fn scroll_coord(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> f64 {
    v.number_value(scope).filter(|n| n.is_finite()).unwrap_or(0.0)
}

pub(crate) fn cb_el_scroll_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    el_scroll_to(scope, &args, rv, "Failed to execute 'scrollTo' on 'Element': ");
}

/// `el.scroll(..)`: `scrollTo` under its own name (the name its refusal carries).
pub(crate) fn cb_el_scroll(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    el_scroll_to(scope, &args, rv, "Failed to execute 'scroll' on 'Element': ");
}

fn el_scroll_to(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, prefix: &str) {
    let (x, y) = match scroll_args(scope, args, prefix) {
        Ok(xy) => xy,
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let this: v8::Local<v8::Value> = args.this().into();
    if let Some(x) = x { scroll_write(scope, this, 1, x); }
    if let Some(y) = y { scroll_write(scope, this, 0, y); }
    rv.set(scroll_promise(scope));
}


/// `el.scrollIntoView()` — scrolls the element's scrolling box to it and answers a PROMISE.
///
/// The generated shell answered `null`. Brave 151 answers `[object Promise]`, which is the recorded
/// fact and outranks the older reading of the spec that has this member return undefined; scroll
/// completion became observable when smooth scrolling did. Nothing here could see the difference,
/// because the corpus had one synchronous case for it and `null` and a promise are both "not a
/// throw" to a question that only reads the value's type once.
///
/// The scroll itself is [`scroll_into_view`]: every scrolling box between the element and the viewport
/// moves, innermost first, so the element ends where `block` and `inline` put it. It used to write the
/// ELEMENT's own scroll position to zero and move nothing above it, so a button below the fold stayed
/// there -- and Playwright's click, which scrolls its target into view through the same steps, refused
/// it as "outside of the viewport".
pub(crate) fn cb_el_scroll_into_view(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // `optional (ScrollIntoViewOptions or boolean) arg = {}`, converted as WebIDL converts the union (Chrome 150):
    // an object, `null` or `undefined` is the dictionary -- `behavior`, `block`, `container`,
    // `inline`, the enums checked, a refusal rejecting the promise -- and anything else a boolean
    // (`true`: block start, `false`: block end; inline nearest either way)
    let a0 = args.get(0);
    let (mut block, mut inline, mut nearest_only) = (Align::Start, Align::Nearest, false);
    if a0.is_null_or_undefined() || a0.is_object() {
        match crate::install::ctor_dicts::convert_dict_caught(scope, a0, "ScrollIntoViewOptions", "Failed to execute 'scrollIntoView' on 'Element': ") {
            Err(e) => {
                let p = crate::binding::protos::rejected_promise(scope, e);
                rv.set(p);
                return;
            }
            // the converted dictionary is the engine's own object: reading it runs no page getter
            Ok(dict) => {
                let member = |scope: &mut v8::PinScope, name: &str| {
                    let k = v8::String::new(scope, name)?;
                    dict.get(scope, k.into()).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
                };
                if let Some(b) = member(scope, "block") { block = Align::parse(&b).unwrap_or(block); }
                if let Some(i) = member(scope, "inline") { inline = Align::parse(&i).unwrap_or(inline); }
                nearest_only = member(scope, "container").as_deref() == Some("nearest");
            }
        }
    } else if !a0.boolean_value(scope) {
        block = Align::End;
    }
    if let Some(id) = arg_node_id(scope, args.this().into()) {
        scroll_into_view(scope, id, block, inline, nearest_only, None);
    }
    rv.set(scroll_promise(scope));
}

/// `el.scrollIntoViewIfNeeded(centerIfNeeded = true)`, Chrome's own member: `undefined`, and a scroll only
/// on an axis where the element is not wholly visible -- centred (`true`) or to the nearest edge (`false`),
/// Blink's `ScrollAlignment::CenterIfNeeded` / `ToEdgeIfNeeded`.
pub(crate) fn cb_el_scroll_into_view_if_needed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let a0 = args.get(0);
    let center = a0.is_undefined() || a0.boolean_value(scope);
    let align = if center { Align::CenterIfNeeded } else { Align::Nearest };
    if let Some(id) = arg_node_id(scope, args.this().into()) {
        scroll_into_view(scope, id, align, align, false, None);
    }
}

/// Where a target lands in a scrolling box: CSSOM View's `ScrollLogicalPosition`, plus Blink's
/// "centre it unless it is already wholly visible" (`scrollIntoViewIfNeeded`, CDP's `DOM.scrollIntoViewIfNeeded`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Align {
    Start,
    Center,
    End,
    Nearest,
    CenterIfNeeded,
}

impl Align {
    fn parse(s: &str) -> Option<Align> {
        Some(match s {
            "start" => Align::Start,
            "center" => Align::Center,
            "end" => Align::End,
            "nearest" => Align::Nearest,
            _ => return None,
        })
    }

    /// How far a scrolling box must scroll on one axis so the target `[t, t + tl)` sits where this alignment
    /// puts it within the scrollport `[p, p + pl)`; both in the same (viewport) coordinates.
    pub(crate) fn delta(self, t: f64, tl: f64, p: f64, pl: f64) -> f64 {
        let (te, pe) = (t + tl, p + pl);
        let center = (t + tl / 2.0) - (p + pl / 2.0);
        match self {
            Align::Start => t - p,
            Align::End => te - pe,
            Align::Center => center,
            // CSSOM View's "nearest": a target covering the scrollport stays; one sticking out at the start (and
            // no larger) or at the end (and larger) aligns its start; the other two cases align its end
            Align::Nearest => {
                if t < p && te > pe {
                    0.0
                } else if (t < p && tl <= pl) || (te > pe && tl > pl) {
                    t - p
                } else if (t < p && tl > pl) || (te > pe && tl <= pl) {
                    te - pe
                } else {
                    0.0
                }
            }
            Align::CenterIfNeeded => {
                if t >= p && te <= pe {
                    0.0
                } else {
                    center
                }
            }
        }
    }
}

/// CSSOM View's "scroll an element into view": every scrolling box from `id`'s nearest one out to the viewport
/// (only the nearest with `nearest_only`, `container: "nearest"`) scrolls so the target -- the element's border
/// box, or `rect` within it (CDP's `DOM.scrollIntoViewIfNeeded`) -- lands where `block` (vertical) and `inline`
/// (horizontal) say. Instant: this engine has no scroll animation, so `behavior` changes nothing. A detached
/// element, or one with no box, moves nothing.
///
/// Each box's move is measured after the inner ones moved (the target's client rect already reflects them), and
/// is the same clamped write `scrollTo` and `scrollTop` make, so the box's `scroll` event follows as theirs does.
/// The viewport is the document element's scrolling box, as everywhere else here; an element of a frame
/// document moves its own ancestors only.
pub(crate) fn scroll_into_view(scope: &mut v8::PinScope, id: u32, block: Align, inline: Align, nearest_only: bool, rect: Option<(f64, f64, f64, f64)>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = document_element_id(&op_state);
    let (in_page, ancestors) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut chain = Vec::new();
        let mut cur = dom.parent(id);
        while let Some(p) = cur {
            if Some(p) == root {
                break;
            }
            if dom.node_type(p) == 1 {
                chain.push(p);
            }
            cur = dom.parent(p);
        }
        (dom.is_in_document(id), chain)
    };
    let mut boxes: Vec<u32> = Vec::new();
    for a in ancestors {
        let (x, y) = crate::core::dom::geometry_queries::element_scroll_extent(&mut op_state.borrow_mut(), a);
        if x > 0.0 || y > 0.0 {
            boxes.push(a);
        }
    }
    if in_page {
        boxes.extend(root);
    }
    for container in boxes {
        let (ex, ey, ew, eh) = crate::core::dom::geometry_queries::element_viewport_rect(&mut op_state.borrow_mut(), id);
        if ew == 0.0 && eh == 0.0 {
            return;
        }
        let (tx, ty, tw, th) = match rect {
            Some((rx, ry, rw, rh)) => (ex + rx, ey + ry, rw, rh),
            None => (ex, ey, ew, eh),
        };
        let port = if Some(container) == root {
            let (w, h) = crate::core::dom::geometry_queries::viewport_client_size(scope);
            Some((0.0, 0.0, w, h))
        } else {
            crate::core::dom::geometry_queries::element_viewport_padding_box(&mut op_state.borrow_mut(), container)
        };
        let Some((px, py, pw, ph)) = port else { continue };
        let (dx, dy) = (inline.delta(tx, tw, px, pw), block.delta(ty, th, py, ph));
        let obj: v8::Local<v8::Value> = wrap_cached(scope, &op_state, container).into();
        // a whole pixel, half up: Chrome 150 centres a 50px target in a 993px viewport at 2537, not 2536.5
        if dy != 0.0 {
            let y = scroll_read(scope, obj, 0);
            scroll_write(scope, obj, 0, (y + dy).round());
        }
        if dx != 0.0 {
            let x = scroll_read(scope, obj, 1);
            scroll_write(scope, obj, 1, (x + dx).round());
        }
        if nearest_only {
            break;
        }
    }
}

pub(crate) fn cb_el_scroll_by(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (dx, dy) = match scroll_args(scope, &args, "Failed to execute 'scrollBy' on 'Element': ") {
        Ok(d) => d,
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let this: v8::Local<v8::Value> = args.this().into();
    if let Some(dx) = dx { let cur = scroll_read(scope, this, 1); scroll_write(scope, this, 1, cur + dx); }
    if let Some(dy) = dy { let cur = scroll_read(scope, this, 0); scroll_write(scope, this, 0, cur + dy); }
    rv.set(scroll_promise(scope));
}

/// `requestFullscreen(options)` as Blink 150 runs it (Fullscreen::RequestFullscreen; 2026-10-11, measured):
/// `FullscreenOptions` converted -- a promise operation, so a bad one rejects -- then the conditions in Blink's order,
/// each a TypeError rejection in Blink's words, answered at once: an element neither HTML nor an SVG `<svg>`, a
/// `<dialog>`, one not connected, one whose document may not use the "fullscreen" feature, an open popover. Past them
/// only a user activation could allow it, and this engine has none: the browser's permission check answers a task
/// later, "Permissions check failed". korvus refused every element -- `document.documentElement` too -- as not
/// connected, in a sentence of its own.
pub(crate) fn cb_el_request_fullscreen(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Err(e) = crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "FullscreenOptions", "Failed to execute 'requestFullscreen' on 'Element': ") {
        rv.set(crate::binding::protos::rejected_promise(scope, e));
        return;
    }
    let Some(id) = el_id_of(scope, args.this()) else { return };
    let (html_or_svg, dialog, connected, popover) = crate::page::with_host_dom(scope, |d| {
        let local = d.tag_name(id).unwrap_or("");
        let ns = d.element_ns(id);
        (
            matches!(ns, Some(korvus_dom::ElementNs::Html)) || (matches!(ns, Some(korvus_dom::ElementNs::Svg)) && local == "svg"),
            matches!(ns, Some(korvus_dom::ElementNs::Html)) && local == "dialog",
            d.node_type(d.shadow_including_root(id)) == 9,
            d.is_popover_open(id),
        )
    })
    .unwrap_or((false, false, false, false));
    let refusal = if !html_or_svg {
        Some("Element is not an HTML or SVG element")
    } else if dialog {
        Some("Dialog elements are invalid")
    } else if !connected {
        Some("Element is not connected")
    } else if {
        let ctx = crate::core::html::browsing_context::context_of_element(scope, id);
        !crate::core::feature_policy::enabled_in(scope, "fullscreen", ctx)
    } {
        Some("Disallowed by permissions policy")
    } else if popover {
        Some("The element is already open as a Popover, and therefore cannot be opened via the fullscreen API.")
    } else {
        None
    };
    match refusal {
        Some(msg) => rv.set(crate::binding::protos::rejected_type_error(scope, msg)),
        None => {
            let m = str_val(scope, "Permissions check failed");
            let Ok(m) = v8::Local::<v8::String>::try_from(m) else { return };
            let e = v8::Exception::type_error(scope, m);
            let p = crate::binding::protos::settle_later(scope, e, true, crate::binding::protos::Settle::Task(0.0));
            rv.set(p);
        }
    }
}

/// `requestPointerLock(options)` as Blink 150 runs it (PointerLockController::RequestPointerLock; 2026-10-11,
/// measured): `PointerLockOptions` converted (a promise operation: a bad one rejects); an element not connected is
/// refused at once, WrongDocumentError "Target Element removed from DOM"; any other request goes to the browser, which
/// refuses a page that holds no focus the user gave it a task later -- WrongDocumentError "The root document of this
/// element is not valid for pointer lock." korvus refused `document.body` as not connected, at once.
pub(crate) fn cb_el_request_pointer_lock(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Err(e) = crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "PointerLockOptions", "Failed to execute 'requestPointerLock' on 'Element': ") {
        rv.set(crate::binding::protos::rejected_promise(scope, e));
        return;
    }
    let Some(id) = el_id_of(scope, args.this()) else { return };
    let connected = crate::page::with_host_dom(scope, |d| d.node_type(d.shadow_including_root(id)) == 9).unwrap_or(false);
    let (msg, later) = if connected {
        ("The root document of this element is not valid for pointer lock.", true)
    } else {
        ("Target Element removed from DOM", false)
    };
    let Some(e) = crate::binding::protos::make_dom_exc(scope, msg, "WrongDocumentError") else { return };
    let p = if later {
        crate::binding::protos::settle_later(scope, e, true, crate::binding::protos::Settle::Task(0.0))
    } else {
        crate::binding::protos::rejected_promise(scope, e)
    };
    rv.set(p);
}

/// `setPointerCapture(pointerId)` (FIXER C6, Blink `Element::setPointerCapture`): a pointer id no pointer has is a
/// `NotFoundError`; the MOUSE (id 1) is always an active pointer, and a disconnected element is an
/// `InvalidStateError`; otherwise the capture is pending -- taken only while a mouse button is down -- and moves at
/// the next pointer event (`core/events/pointer_input.rs`). It threw `InvalidStateError` for every id, which a
/// drag handler calling it from `pointerdown` saw as an exception Chrome never raises (measured: `setPointerCapture(99)`
/// is "NotFoundError: ... No active pointer with the given id is found."; `setPointerCapture(1)` then
/// `hasPointerCapture(1)` is true inside the `pointerdown`).
fn pointer_id_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<i32> {
    args.get(0).int32_value(scope)
}

fn no_active_pointer(scope: &mut v8::PinScope, method: &str) {
    throw_dom(scope, &format!("Failed to execute '{method}' on 'Element': No active pointer with the given id is found."), "NotFoundError");
}

pub(crate) fn cb_el_set_pointer_capture(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Element", "setPointerCapture") { return; }
    let Some(pointer_id) = pointer_id_arg(scope, &args) else { return };
    let Some(id) = el_id_of(scope, args.this()) else { return };
    if pointer_id != crate::core::events::pointer_input::MOUSE_POINTER_ID {
        no_active_pointer(scope, "setPointerCapture");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let connected = op_state.borrow().try_borrow::<HostDom>().is_some_and(|h| {
        let root = h.0.shadow_including_root(id);
        h.0.node_type(root) == 9 && !h.0.is_shadow_root(root)
    });
    if !connected {
        throw_dom(scope, "Failed to execute 'setPointerCapture' on 'Element': InvalidStateError", "InvalidStateError");
        return;
    }
    let _ = crate::core::events::pointer_input::set_pointer_capture(&op_state, id, pointer_id);
}

pub(crate) fn cb_el_has_pointer_capture(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Element", "hasPointerCapture") { return; }
    let Some(pointer_id) = pointer_id_arg(scope, &args) else { return };
    let Some(id) = el_id_of(scope, args.this()) else { rv.set_bool(false); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    rv.set_bool(crate::core::events::pointer_input::has_pointer_capture(&op_state, id, pointer_id));
}

/// `element.customElementRegistry` -- the registry the element's node document uses to define
/// custom elements, which in this engine is the one global `window.customElements`. It answered
/// null, which claims the element can never be upgraded.
pub(crate) fn cb_el_custom_element_registry(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // the realm's registry itself, not whatever a page assigned to the `[Replaceable]` global
    match crate::core::html::window_globals::window_singleton(scope, "customElements") {
        Some(v) if !v.is_undefined() => rv.set(v),
        _ => rv.set_null(),
    }
}


// ── moved out of bindings.rs (STEP 3 dissolution) ──
/// `HTMLTemplateElement.content` — the template's DocumentFragment (WPT dom/traversal
/// TreeWalker-nextNode-detached-currentNode). The fragment is an unlinked arena node reached
/// only through this getter (the host<->content edge lives in `Dom::template_contents`, the same
/// shape as a shadow root), so light-tree walks and document queries never cross it. Any children
/// that found their way into the template's light list (an `appendChild` on the template itself)
/// are moved in on access — a template's own child list is always empty in the DOM.
pub(crate) fn cb_template_content_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The receiver check, added with the `HTMLTemplateElement` table: Chrome 150
    // answers `TypeError: Illegal invocation` for `...content.get.call(Object.create(proto))` and this
    // returned `undefined` -- the brief's failure mode 2, found by sweeping the prototype.
    let Some(id) = unwrap_id(scope, args.this().into()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let frag = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.move_children_into_template_content(id) };
    let obj = wrap_cached(scope, &op_state, frag);
    rv.set(obj.into());
}

/// `Element/ShadowRoot.setHTMLUnsafe(html)` (unit: declarative-shadow-dom) — the one fragment-
/// parsing entry point that DOES allow a top-level `<template shadowrootmode>` to attach a
/// declarative shadow root, onto `this` itself (`Dom::parse_fragment_unsafe`'s `target`). Same
/// "replace all children" shape and MutationObserver notification as the `innerHTML` setter.
/// The interface an `innerHTML` / `setHTMLUnsafe` member runs as on node `id`: `ShadowRoot` for a shadow
/// root, `Element` otherwise (the sample and the sentence a Trusted Types refusal names).
fn html_host_iface(scope: &mut v8::PinScope, id: u32) -> &'static str {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    if st.borrow::<HostDom>().0.is_shadow_root(id) { "ShadowRoot" } else { "Element" }
}

/// `(TrustedHTML or DOMString)`, converted as Blink's binding converts it: a TrustedHTML as itself, anything else
/// ToString (a Symbol refused with the operation's context `prefix`).
pub(crate) fn html_union<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'_, v8::Value>, prefix: &str) -> Option<v8::Local<'s, v8::Value>> {
    let v = v8::Local::new(scope, v);
    if crate::core::trusted_types::tables::is_trusted(scope, v, "TrustedHTML") {
        return Some(v);
    }
    let s = crate::binding::class::idl_string(scope, v, prefix)?;
    Some(str_val(scope, &s))
}

pub(crate) fn cb_set_html_unsafe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Element", "setHTMLUnsafe") {
        return;
    }
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let iface = html_host_iface(scope, id);
    let (sample, context) = (format!("{iface} setHTMLUnsafe"), format!("Failed to execute 'setHTMLUnsafe' on '{iface}'"));
    // the binding's conversions before the operation's steps (Chrome 150): `(TrustedHTML or
    // DOMString) html` -- a TrustedHTML as itself, anything else ToString -- then `SetHTMLUnsafeOptions` (`runScripts`,
    // `sanitizer`; korvus read neither); the Trusted Types check is a step of the operation
    let Some(html_v) = html_union(scope, args.get(0), &format!("{context}: ")) else { return };
    if crate::install::ctor_dicts::convert_dict(scope, args.get(1), "SetHTMLUnsafeOptions", &format!("{context}: ")).is_err() {
        return;
    }
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: &sample, context: &context };
    let Some(html) = crate::core::trusted_types::tables::compliant_text(scope, html_v, &sink, crate::core::trusted_types::tables::SinkAt::Node(id)) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (removed, added) = { op_state.borrow_mut().borrow_mut::<HostDom>().0.set_inner_html_unsafe(id, &html) };
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &added);
    notify_childlist_batch(scope, &op_state, id, &added, &removed);
    crate::core::html::custom_element_registry::upgrade_created(scope, &added);
}

/// `Element/ShadowRoot.getHTML(options)` — like `innerHTML`, but a shadow root nested inside is
/// serialized as its declarative `<template shadowrootmode>` form when EITHER
/// `options.serializableShadowRoots` is true and the root's own `.serializable` is true, OR the
/// root is named in `options.shadowRoots` (serialized regardless of `serializableShadowRoots` --
/// WPT gethtml.html: "If we provide the shadow root, serialize it, regardless of
/// serializableShadowRoots."). No options (or `serializableShadowRoots: false` and no explicit
/// list) makes this identical to `innerHTML` -- also asserted there.
pub(crate) fn cb_get_html(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { rv.set(str_val(scope, "")); return };
    let opts = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let serializable_shadow_roots =
        opts.and_then(|o| get_prop(scope, o, "serializableShadowRoots")).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let mut forced: Vec<u32> = Vec::new();
    if let Some(arr) = opts
        .and_then(|o| get_prop(scope, o, "shadowRoots"))
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
    {
        for i in 0..arr.length() {
            if let Some(item) = arr.get_index(scope, i) {
                if let Some(rid) = unwrap_id(scope, item) {
                    forced.push(rid);
                }
            }
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let html = { op_state.borrow().borrow::<HostDom>().0.get_html(id, serializable_shadow_roots, &forced) };
    rv.set(str_val(scope, &html));
}

/// `element.attachShadow({mode})` — attach a shadow root to the host element and return it (a
/// ShadowRoot wrapper). `mode:"closed"` records Closed; anything else (incl. the required "open")
/// records Open. Errors map to DOM exceptions: a second attach -> NotSupportedError; a non-element
/// host -> NotSupportedError (spec attachShadow only exists on Element, so this is defensive).
pub(crate) fn cb_attach_shadow(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let host_id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id, None => return,
    };
    // `ShadowRootInit`, converted as WebIDL converts it (Chrome 150): `clonable`,
    // `customElementRegistry`, `delegatesFocus`, `mode` (REQUIRED, an enum), `serializable`, `slotAssignment`, each read
    // once, before any shadow root is created -- `attachShadow({})` and a bad `mode` refused in Blink's sentences, a
    // non-object (`null` included: the dictionary has a required member) "not of type". korvus read `mode` first and
    // the rest after the host checks. (`attachShadow({})` once quietly took the `open` branch and left the element
    // hosting a root, so the next call failed with NotSupportedError for the wrong reason.)
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "ShadowRootInit", "Failed to execute 'attachShadow' on 'Element': ") else { return };
    let mode = match get_prop(scope, init, "mode").map(|v| v.to_rust_string_lossy(scope)).as_deref() {
        Some("closed") => ShadowRootMode::Closed,
        _ => ShadowRootMode::Open,
    };
    let init = Some(init);
    // "ATTACH A SHADOW ROOT", STEPS 1-2, and neither existed (CONF ELEM1.5). Measured before the
    // change on the release binary at `eb9c6a40`, one page, with controls:
    //
    //   <input> <br> <img> <table> <select> <textarea> <button> <li> <td>  -> ShadowRoot returned
    //   createElementNS(SVG, "svg")                                       -> ShadowRoot returned
    //   the 18 safelisted names, <my-el>, <x-a.b_c-d>                     -> ShadowRoot (correct)
    //
    // `attach_shadow_full` (korvus-dom `tree.rs`) refuses only a non-element host and a second
    // root, so every one of those returned a real ShadowRoot where every browser throws.
    //
    // Step 1 is the HTML namespace. Step 2 is a safelisted local name OR a valid custom element
    // name -- `valid_name` is the tree's ONE production for the latter and is borrowed rather than
    // copied. The safelist is the spec's, and WPT states the same 18 names in
    // `html/resources/common.js:40` (`HTML5_SHADOW_ALLOWED_ELEMENTS`), which is what
    // `shadow-dom/Element-interface-attachShadow.html` tests against.
    //
    // It goes HERE, after the `mode` conversion and before anything is created: WebIDL converts
    // the dictionary first, so `<input>.attachShadow({})` is a TypeError and `<input>.attachShadow(
    // {mode:"open"})` is the NotSupportedError.
    const SHADOW_SAFELIST: [&str; 18] = [
        "article", "aside", "blockquote", "body", "div", "footer", "h1", "h2", "h3", "h4", "h5",
        "h6", "header", "main", "nav", "p", "section", "span",
    ];
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The LOCAL name, not `Dom::tag_name` -- via `local_name_of`, the same production
    // `Element.localName` answers from.
    //
    // AND IT CAN STILL FAIL TO RESOLVE, WHICH IS WHY THE REFUSAL IS CONDITIONAL. korvus's arena
    // stores an element's QUALIFIED name (`korvus-dom/src/tree.rs:93` says so outright) and the
    // `(prefix, local)` split lives only in `QNameMap`, which `createElementNS` writes and the XML
    // PARSER does not. So `<xhtml:div>` parsed from an XHTML document is an HTML-namespace `div`
    // whose only available name is "xhtml:div" -- and the first cut of this check refused it,
    // taking `shadow-dom/host-with-namespace.xhtml` from 2/2 to 1/2.
    //
    // A name we cannot split is a name we cannot judge, so it is NOT refused. That is the
    // conservative side: a missing refusal leaves today's behaviour, a wrong one breaks a page.
    // Splitting on the colon here instead would be wrong in the other direction --
    // `document.createElement("a:b")` in an HTML document has the LOCAL name "a:b", colon and all.
    // The real fix is the arena gap, which is not this lane's to decide (goal §9).
    let local = crate::core::html::html_element::local_name_of(&op_state, host_id)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let unresolved_qname = local.contains(':');
    let host_ok = unresolved_qname || {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_html_ns(host_id)
            && (SHADOW_SAFELIST.contains(&local.as_str())
                || crate::core::html::custom_element_registry::valid_name(&local))
    };
    if !host_ok {
        throw_dom(
            scope,
            "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow",
            "NotSupportedError",
        );
        return;
    }
    let delegates_focus = init.and_then(|o| get_prop(scope, o, "delegatesFocus")).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let clonable = init.and_then(|o| get_prop(scope, o, "clonable")).map(|v| v.boolean_value(scope)).unwrap_or(false);
    // `ShadowRootInit.referenceTarget` (Chrome 152, `DOMString?`): absent leaves the root's null, null is null,
    // anything else its string -- converted in the dictionary's member order, before `serializable`.
    let reference_target: Option<Option<String>> = match init.and_then(|o| get_prop(scope, o, "referenceTarget")) {
        None => None,
        Some(v) if v.is_undefined() => None,
        Some(v) if v.is_null() => Some(None),
        Some(v) => {
            let Some(s) = v.to_string(scope) else { return };
            Some(Some(s.to_rust_string_lossy(scope)))
        }
    };
    let serializable = init.and_then(|o| get_prop(scope, o, "serializable")).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let slot_assignment = {
        let s = init.and_then(|o| get_prop(scope, o, "slotAssignment")).map(|v| v.to_rust_string_lossy(scope));
        match s.as_deref() { Some("manual") => ShadowSlotAssignment::Manual, _ => ShadowSlotAssignment::Named }
    };
    let res = {
        op_state.borrow_mut().borrow_mut::<HostDom>().0
            .attach_shadow_full(host_id, mode, delegates_focus, slot_assignment, clonable, serializable, false)
    };
    match res {
        Ok(root) => {
            if let Some(t) = reference_target {
                op_state.borrow_mut().borrow_mut::<HostDom>().0.set_shadow_reference_target(root, t.as_deref());
            }
            let o = wrap_cached(scope, &op_state, root);
            rv.set(o.into());
        }
        Err(AttachShadowError::HostAlreadyHasShadowRoot) =>
            throw_dom(scope, "The element already hosts a shadow root.", "NotSupportedError"),
        Err(AttachShadowError::HostNotElement) =>
            throw_dom(scope, "This element does not support attachShadow.", "NotSupportedError"),
    }
}

/// `Node.isConnected` — shadow-INCLUDING connectivity: the node's shadow-including root is A DOCUMENT --
/// the page's, a frame's, or one no frame shows (`DOMParser`, `createHTMLDocument`; Chrome 150
/// answers true there, and gives such a document's `<style>` its sheet). An element resolves to its own
/// connectivity; the Document is always connected. Whether a node is RENDERED (subresources, observers,
/// computed style) is `connected_anywhere`'s question, not this one.
pub(crate) fn cb_is_connected(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let el_id = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id);
    let connected = match el_id {
        Some(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().is_some_and(|h| h.0.node_type(h.0.shadow_including_root(id)) == 9)
        }
        None => deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).is_some(),
    };
    rv.set(v8::Boolean::new(scope, connected).into());
}

pub(crate) fn cb_outer_html_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    // (TrustedHTML or [LegacyNullToEmptyString] DOMString), as `innerHTML`: `null` is "" (it was the text "null")
    let a0 = args.get(0);
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "Element outerHTML", context: "Failed to set the 'outerHTML' property on 'Element'" };
    let at = crate::core::trusted_types::tables::SinkAt::Node(id);
    let html = if a0.is_null() {
        let Some(h) = crate::core::trusted_types::tables::check_text(scope, String::new(), &sink, at) else { return };
        h
    } else {
        let Some(h) = crate::core::trusted_types::tables::compliant_text(scope, a0, &sink, at) else { return };
        h
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; dom.parent(id).map(|p| (p, dom.node_type(p))) };
    // "If parent is null, return" -- a parentless element's outerHTML= is a no-op (Chrome, 2026-09-10);
    // a Document parent is the spec's NoModificationAllowedError.
    let parent = match parent {
        None => return,
        Some((_, 9)) => { throw_dom(scope, "Failed to set the 'outerHTML' property on 'Element': This element's parent is of type '#document', which is not an element.", "NoModificationAllowedError"); return; }
        Some((p, _)) => p,
    };
    // WHATWG "set the outerHTML": the fragment is parsed with `this`'s PARENT as context element,
    // not `this`. That is what makes `tbody.outerHTML = '<tr>…'` keep its row (table context) and,
    // for an `<html>` parent, what synthesizes the `<head>`/`<body>` DOMPurify's first
    // feature-detect reads back.
    let ids = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.parse_fragment_in(parent, &html) };
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &nid in &ids { dom.insert_before(parent, nid, id); }
        dom.remove_child(parent, id);
    }
    // outerHTML= is a childList mutation on the parent: the parsed nodes are added, `this` is removed --
    // notify so an observer of the parent sees it (the arena insert/remove above are invisible to it).
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &ids);
    notify_childlist_batch(scope, &op_state, parent, &ids, &[id]);
    crate::core::html::custom_element_registry::upgrade_created(scope, &ids);
}

// ── the Element table ──────────────────────────────────────────────────────────────
//
// Every own member of `Element.prototype` is a row below with a Rust body. The twins of the members
// the `#[op2] impl JsElement` used to carry (`tagName`, `getAttribute`, `setAttribute`, `innerHTML`,
// `matches`, ...) are raw callbacks over the same arena calls; the rest are the callbacks the JS
// mounts in 03/08b/08c/08h/09-postshells used to hand-place, reached from the table instead.

use crate::core::animation::targets::{cb_element_animate, cb_element_get_animations};
use crate::core::dom::geometry_queries::{cb_el_bounding_client_rect, cb_el_client_rects, cb_metric, cb_scroll_size, metric_index};
use crate::core::dom::attr::{attr_eid_of, attr_name_of};
use crate::core::dom::collections::{builder_data, cb_by_tag_ns, cb_el_by_name, cb_el_collection, Kind};
use crate::core::dom::node::{
    cb_after, cb_append, cb_before, cb_insert_adjacent_element, cb_insert_adjacent_html, cb_insert_adjacent_text, cb_move_before,
    cb_prepend, cb_remove_self, cb_replace_children, cb_replace_with,
};
use crate::core::dom::shared::{
    cb_assigned_slot, cb_closest, cb_first_element_child, cb_last_element_child, cb_next_element_sibling, cb_previous_element_sibling,
    cb_query_selector, cb_query_selector_all, row_accessor, row_getter, row_getter_data, row_method, row_method_data,
};
use crate::core::html::html_element::cb_local_name;
use crate::install::MemberDef;

/// The element receiver of a twin, or the "Illegal invocation" TypeError a WebIDL member throws on a
/// receiver that is not a platform object of its interface -- what the `#[op2] impl JsElement` members
/// these callbacks replace threw, and what Chrome throws.
pub(crate) fn el_this(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let id = el_id_of(scope, args.this());
    if id.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    id
}

/// `Element.tagName`: the namespace-aware display name (upper-cased only for HTML elements in an HTML
/// document -- see [`element_display_name`]).
pub(crate) fn cb_el_tag_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let name = { let st = op_state.borrow(); element_display_name(&st, id).unwrap_or_default() };
    let v = str_val(scope, &name);
    rv.set(v);
}

pub(crate) fn cb_el_inner_html_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = markup_of(&op_state, id, true);
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The fragment serializing algorithm: a node in an HTML document serializes as HTML, one in an XML document as
/// XML (`core/xml/xml_serializer.rs`) -- Chrome 150's `outerHTML` of a parsed XML document's `<a:c d="1"/>` is
/// that, where the HTML serializer wrote `<a:c d="1"></a:c>`.
fn markup_of(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, children_only: bool) -> String {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if dom.is_in_xml(id) {
        let q = st.try_borrow::<QNameMap>().map(|q| q.0.borrow());
        return crate::core::xml::xml_serializer::serialize(dom, q.as_deref(), id, children_only);
    }
    if children_only { dom.inner_html(id) } else { dom.outer_html(id) }
}

/// `innerHTML = html` replaces every child with the parsed fragment -- one childList mutation, which is
/// why it notifies through the same batch path `append`/`replaceChildren` use. `[LegacyNullToEmptyString]`:
/// `null` is the empty string, not "null" (Chrome, 2026-09-10).
pub(crate) fn cb_el_inner_html_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let a0 = args.get(0);
    // (TrustedHTML or [LegacyNullToEmptyString] DOMString): a TrustedHTML read as itself, nothing on it called;
    // a string -- `null` is "" -- through the document's Trusted Types (the default policy when its CSP asks)
    let iface = html_host_iface(scope, id);
    let (sample, context) = (format!("{iface} innerHTML"), format!("Failed to set the 'innerHTML' property on '{iface}'"));
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: &sample, context: &context };
    let at = crate::core::trusted_types::tables::SinkAt::Node(id);
    let html = if a0.is_null() {
        let Some(h) = crate::core::trusted_types::tables::check_text(scope, String::new(), &sink, at) else { return };
        h
    } else {
        let Some(h) = crate::core::trusted_types::tables::compliant_text(scope, a0, &sink, at) else { return };
        h
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (removed, added) = op_state.borrow_mut().borrow_mut::<HostDom>().0.set_inner_html(id, &html);
    // The fragment parser marks its scripts "already started": `innerHTML` never runs a script, not
    // even an external one, and not later when the element is moved.
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &added);
    notify_childlist_batch(scope, &op_state, id, &added, &removed);
    // A DETACHED element's new children are constructed too; a connected one's already
    // were, by the insertion steps above.
    crate::core::html::custom_element_registry::upgrade_created(scope, &added);
}

pub(crate) fn cb_el_outer_html_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = markup_of(&op_state, id, false);
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The attribute name an `Element` method was handed: WebIDL's ToString (`setAttribute("title", 7)`
/// stores "7", `removeAttribute(undefined)` removes "undefined"), then the HTML-document lower-casing
/// [`normalize_attr_name`] applies. Stringified BEFORE any OpState borrow -- ToString can reenter JS.
///
/// A string argument -- nearly every call -- is read into the caller's `buf` rather than a heap
/// `String`, and lowercased only when it has an upper-case letter to lower (the name used to
/// cost two allocations per call).
fn attr_arg<'b>(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    id: u32,
    v: v8::Local<v8::Value>,
    buf: &'b mut [std::mem::MaybeUninit<u8>; 64],
) -> std::borrow::Cow<'b, str> {
    let raw: std::borrow::Cow<'b, str> = match v8::Local::<v8::String>::try_from(v) {
        Ok(s) => s.to_rust_cow_lossy(scope, buf),
        Err(_) => v.to_rust_string_lossy(scope).into(),
    };
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if dom.is_html_ns(id) && !dom.is_in_xml(id) && raw.bytes().any(|b| b.is_ascii_uppercase()) {
        raw.to_ascii_lowercase().into()
    } else {
        raw
    }
}

/// An attribute's value copied out of the arena, so the state is not borrowed while a V8 string is
/// made from it (allocating one can collect garbage): into `buf` when it fits, onto the heap when not.
enum CopiedValue {
    Inline(usize),
    Heap(String),
}

impl CopiedValue {
    fn of(v: &str, buf: &mut [u8; 128]) -> CopiedValue {
        match buf.get_mut(..v.len()) {
            Some(dst) => {
                dst.copy_from_slice(v.as_bytes());
                CopiedValue::Inline(v.len())
            }
            None => CopiedValue::Heap(v.to_string()),
        }
    }

    fn as_str<'a>(&'a self, buf: &'a [u8; 128]) -> &'a str {
        match self {
            CopiedValue::Inline(n) => std::str::from_utf8(&buf[..*n]).unwrap_or(""),
            CopiedValue::Heap(s) => s,
        }
    }
}

pub(crate) fn cb_el_get_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, args.get(0), &mut name_buf);
    let mut value_buf = [0u8; 128];
    let val = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &name).map(|v| CopiedValue::of(v, &mut value_buf)) };
    match val {
        Some(c) => { let v = str_val(scope, c.as_str(&value_buf)); rv.set(v); }
        None => rv.set_null(),
    }
}

pub(crate) fn cb_el_set_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    // Both arguments are stringified first (either can reenter JS through a `toString`), in WebIDL's order
    // -- the name, then the value (this converted the value first) -- then the arena is touched once, then
    // the observers are told. The value is the `TrustedType` overload's: any Trusted value is its own string.
    let a0 = args.get(0);
    let name_s: v8::Local<v8::Value> = if a0.is_string() { a0 } else { let Some(n) = a0.to_string(scope) else { return }; n.into() };
    // A string value is its own ToString and no Trusted value (`trusted_or_string`'s answer for it), read into a
    // stack buffer: the common write copies the value into no heap `String`. Nothing runs between this read and
    // the arena write, so reading it here is reading it where the conversion did.
    let a1 = args.get(1);
    let mut value_buf = [std::mem::MaybeUninit::<u8>::uninit(); 128];
    let (value, value_kind) = match v8::Local::<v8::String>::try_from(a1) {
        Ok(s) => (s.to_rust_cow_lossy(scope, &mut value_buf), None),
        Err(_) => {
            let Some((v, k)) = crate::core::trusted_types::tables::trusted_or_string(scope, a1) else { return };
            (std::borrow::Cow::Owned(v), k)
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, name_s, &mut name_buf);
    if !valid_attr_name(scope, &name, "setAttribute") {
        return;
    }
    let Some(value) = attribute_trusted_value_cow(scope, id, &name, None, value, value_kind, "Failed to execute 'setAttribute' on 'Element'") else { return };
    set_attribute_notify(scope, &op_state, id, &name, &value);
}

/// `setAttribute` / `toggleAttribute` step 1: a name that is not a valid attribute local name throws
/// (WHATWG DOM, 2025). Korvus set ANY name -- `""`, `"a b"`, `"a=b"` -- where Chrome 150 throws this,
/// verbatim (measured 2026-10-05). `false`: thrown.
fn valid_attr_name(scope: &mut v8::PinScope, name: &str, method: &str) -> bool {
    if crate::core::xml::dom_implementation::attribute_local_name_error(name).is_none() {
        return true;
    }
    throw_dom(scope, &format!("Failed to execute '{method}' on 'Element': '{name}' is not a valid attribute name."), "InvalidCharacterError");
    false
}

pub(crate) fn cb_el_remove_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, args.get(0), &mut name_buf);
    remove_attribute_notify(scope, &op_state, id, &name);
}

pub(crate) fn cb_el_has_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, args.get(0), &mut name_buf);
    let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &name).is_some() };
    rv.set_bool(has);
}

/// `toggleAttribute(name, force)`: `force` absent/undefined toggles; `true` ensures present (set to ""
/// only when absent), `false` ensures absent. Answers whether the attribute is present afterwards.
pub(crate) fn cb_el_toggle_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let a1 = args.get(1);
    let force = if a1.is_undefined() { None } else { Some(a1.boolean_value(scope)) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, args.get(0), &mut name_buf);
    if !valid_attr_name(scope, &name, "toggleAttribute") {
        return;
    }
    let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &name).is_some() };
    if force == Some(true) || (force.is_none() && !has) {
        if !has { set_attribute_notify(scope, &op_state, id, &name, ""); }
        rv.set_bool(true);
    } else {
        if has { remove_attribute_notify(scope, &op_state, id, &name); }
        rv.set_bool(false);
    }
}

/// `recv.<method>(args)` for the four Element attribute methods, performed natively instead of being
/// looked up on the object. Engine code -- reflection, form controls, SVG, dialog, the script runner --
/// must never call the page-visible `Element.prototype.getAttribute`: a page may have replaced it, and
/// Chrome's reflection never calls it (0 calls on Chrome 150 where korvus made 4 gets and 3 sets,
/// `tools/realweb/seed/refl`). Osano's consent manager wraps `getAttribute` and read a reflected
/// property inside the wrapper, which recursed until the stack overflowed. Same result
/// shape as `call_method`: the method's return value, `None` only when `recv` is not a node.
pub fn attr_call<'s>(scope: &mut v8::PinScope<'s, '_>, recv: v8::Local<v8::Value>, method: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    let Some(id) = el_id_of(scope, v8::Local::<v8::Object>::try_from(recv).ok()?) else {
        return call_method(scope, recv, method, args);
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let name_v: v8::Local<v8::Value> = match args.first() { Some(v) => *v, None => v8::undefined(scope).into() };
    let mut name_buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = attr_arg(scope, &op_state, id, name_v, &mut name_buf);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match method {
        "getAttribute" => {
            let val = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &name).map(str::to_string) };
            Some(match val { Some(s) => str_val(scope, &s), None => v8::null(scope).into() })
        }
        "hasAttribute" => {
            let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &name).is_some() };
            Some(v8::Boolean::new(scope, has).into())
        }
        "setAttribute" => {
            let value = match args.get(1) { Some(v) => v.to_rust_string_lossy(scope), None => "undefined".to_string() };
            set_attribute_notify(scope, &op_state, id, &name, &value);
            Some(undef)
        }
        "removeAttribute" => {
            remove_attribute_notify(scope, &op_state, id, &name);
            Some(undef)
        }
        _ => call_method(scope, recv, method, args),
    }
}

pub(crate) fn cb_el_get_attribute_names(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let names: Vec<String> = { let st = op_state.borrow(); st.borrow::<HostDom>().0.attribute_names(id) };
    let arr = v8::Array::new(scope, names.len() as i32);
    for (i, n) in names.iter().enumerate() {
        let s = str_val(scope, n);
        arr.set_index(scope, i as u32, s);
    }
    rv.set(arr.into());
}

/// `matches(selectors)` and its legacy alias `webkitMatchesSelector` (two function objects, one body).
/// An invalid selector is a DOM `SyntaxError`, thrown -- the strict parse the op2 twin had.
pub(crate) fn cb_el_matches(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    el_matches(scope, args, rv, "matches");
}

/// `webkitMatchesSelector`: `matches`, with its own name in a refusal (Blink names the member called).
pub(crate) fn cb_el_webkit_matches(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    el_matches(scope, args, rv, "webkitMatchesSelector");
}

fn el_matches(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, method: &str) {
    let Some(id) = el_this(scope, &args) else { return };
    let sel = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let r = { let st = op_state.borrow(); st.borrow::<HostDom>().0.matches_selector(id, &sel).ok() };
    match r {
        Some(b) => rv.set_bool(b),
        None => crate::core::dom::shared::throw_selector_error(scope, method, "Element", &sel),
    }
}

pub(crate) fn cb_el_child_element_count(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = el_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let n = op_state.borrow().borrow::<HostDom>().0.child_elements(id).len();
    rv.set_uint32(n as u32);
}

/// `releasePointerCapture(pointerId)`: a pointer id no pointer has is a `NotFoundError` (as `setPointerCapture`);
/// for the mouse only the element its next event goes to can release it (Blink `ReleasePointerCapture`).
pub(crate) fn cb_el_release_pointer_capture(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Element", "releasePointerCapture") { return; }
    let Some(pointer_id) = pointer_id_arg(scope, &args) else { return };
    let Some(id) = el_id_of(scope, args.this()) else { return };
    if pointer_id != crate::core::events::pointer_input::MOUSE_POINTER_ID {
        no_active_pointer(scope, "releasePointerCapture");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let _ = crate::core::events::pointer_input::release_pointer_capture(&op_state, id, pointer_id);
}

/// `webkitRequestFullScreen()`, `webkitRequestFullscreen()`: the legacy void members. Chrome answers
/// `undefined` for both (2026-09-10); there is no display to fill.
pub(crate) fn cb_el_legacy_void(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// `getAttributeNodeNS(namespace, localName)`: the arena holds no namespaced attributes (a
/// `setAttributeNS` value is stored under its qualified name), so only the null namespace can match
/// -- Chrome: `getAttributeNodeNS(null, "x")` is the Attr, `getAttributeNodeNS("u", "x")` null.
pub(crate) fn cb_el_get_attribute_node_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(eid) = el_id_of(scope, args.this()) else { rv.set_null(); return };
    let ns = args.get(0);
    if !(ns.is_null_or_undefined() || (ns.is_string() && ns.to_rust_string_lossy(scope).is_empty())) {
        rv.set_null();
        return;
    }
    let name = args.get(1).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some() };
    if has { if let Some(a) = make_attr(scope, eid, &name) { rv.set(a.into()); return; } }
    rv.set_null();
}

/// The Attr argument of `setAttributeNode`/`removeAttributeNode`: its (owner element, name), or a
/// TypeError -- an Attr here is always a view over an owner's attribute map (`attr.rs`).
fn attr_arg_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, method: &str) -> Option<(u32, String)> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok();
    let pair = obj.and_then(|o| { let e = attr_eid_of(scope, o)?; let n = attr_name_of(scope, o)?; Some((e, n)) });
    if pair.is_none() {
        throw_type_error(scope, &format!("Failed to execute '{method}' on 'Element': parameter 1 is not of type 'Attr'."));
    }
    pair
}

/// `setAttributeNode(attr)` / `setAttributeNodeNS(attr)`. Every Attr this engine has is owned (there is no
/// `createAttribute`), so an Attr of ANOTHER element is the spec's InUseAttributeError, and this
/// element's own Attr is returned as-is -- both as Chrome answers (2026-09-10).
pub(crate) fn cb_el_set_attribute_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(eid) = el_id_of(scope, args.this()) else { rv.set_null(); return };
    let attr_v = args.get(0);
    let attr_o = v8::Local::<v8::Object>::try_from(attr_v).ok().filter(|_| crate::core::dom::attr::is_attr(scope, attr_v));
    let Some(attr_o) = attr_o else {
        throw_type_error(scope, "Failed to execute 'setAttributeNode' on 'Element': parameter 1 is not of type 'Attr'.");
        return;
    };
    match attr_eid_of(scope, attr_o) {
        Some(owner) if owner == eid => { rv.set(attr_v); return; }
        Some(_) => {
            throw_dom(
                scope,
                "Failed to execute 'setAttributeNode' on 'Element': The node provided is an attribute node that is already an attribute of another Element; attribute nodes must be explicitly cloned.",
                "InUseAttributeError",
            );
            return;
        }
        None => {}
    }
    // A detached Attr (`createAttribute`, or one removed earlier) is attached: its value becomes the
    // element's attribute and the Attr now reads and writes through the element. The Attr it
    // replaces, if any, is returned detached with its old value.
    let name = attr_name_of(scope, attr_o).unwrap_or_default();
    let value = crate::core::dom::attr::attr_value_of(scope, attr_o).unwrap_or_default();
    let Some(value) = attribute_trusted_value(scope, eid, &name, None, value, None, "Failed to execute 'setAttributeNode' on 'Element'") else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).map(str::to_string) };
    let replaced = old.and_then(|v| crate::core::dom::attr::make_detached_attr(scope, &name, &v));
    set_attribute_notify(scope, &op_state, eid, &name, &value);
    crate::core::dom::attr::attach_attr(scope, attr_o, Some(eid), "");
    match replaced { Some(a) => rv.set(a.into()), None => rv.set_null() }
}

/// `removeAttributeNode(attr)`: the attribute is removed and the Attr handed back; an Attr of another
/// element is the spec's NotFoundError (Chrome's message, 2026-09-10).
pub(crate) fn cb_el_remove_attribute_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(eid) = el_id_of(scope, args.this()) else { rv.set_null(); return };
    let Some((owner, name)) = attr_arg_of(scope, args.get(0), "removeAttributeNode") else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let has = owner == eid && { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).is_some() };
    if !has {
        throw_dom(scope, "Failed to execute 'removeAttributeNode' on 'Element': The node provided is owned by another element.", "NotFoundError");
        return;
    }
    let old = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, &name).unwrap_or("").to_string() };
    remove_attribute_notify(scope, &op_state, eid, &name);
    // The removed Attr keeps its value and no longer has an element (`ownerElement` null).
    if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        crate::core::dom::attr::attach_attr(scope, o, None, &old);
    }
    rv.set(args.get(0));
}

/// `ariaNotify(announcement, options)`: a real call with a real arity, onto a queue no assistive
/// technology is listening to (`undefined`, as Chrome answers).
pub(crate) fn cb_el_aria_notify(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _ = require_args(scope, &args, 1, "Element", "ariaNotify");
}

/// The `Element` rows.
pub fn install_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "namespaceURI" => row_getter(scope, proto, member, cb_el_namespace_uri),
        "prefix" => row_getter(scope, proto, member, cb_el_prefix),
        "localName" => row_getter(scope, proto, member, cb_local_name),
        "tagName" => row_getter(scope, proto, member, cb_el_tag_name),
        // `[PutForwards=value]`: the setter forwards to the list's `value` (kits::put_forwards).
        "classList" => { let d = str_val(scope, "class"); let g = build_fn_data(scope, cb_el_class_list, "get classList", 0, d); crate::install::kits::put_forwards(scope, proto, member, g, "value") }
        "attributes" => row_getter(scope, proto, member, cb_el_attributes),
        "shadowRoot" => row_getter(scope, proto, member, cb_shadow_root_get),
        "part" => { let d = str_val(scope, "part"); let g = build_fn_data(scope, cb_el_token_list, "get part", 0, d); crate::install::kits::put_forwards(scope, proto, member, g, "value") }
        "assignedSlot" => row_getter(scope, proto, member, cb_assigned_slot),
        "innerHTML" => row_accessor(scope, proto, member, cb_el_inner_html_get, cb_el_inner_html_set),
        "outerHTML" => row_accessor(scope, proto, member, cb_el_outer_html_get, cb_outer_html_set),
        "scrollTop" => row_accessor(scope, proto, member, cb_scroll_top_get, cb_scroll_top_set),
        "scrollLeft" => row_accessor(scope, proto, member, cb_scroll_left_get, cb_scroll_left_set),
        // The scrolling box's size, over the same page pipeline one line below.
        "scrollWidth" | "scrollHeight" => {
            let d: v8::Local<v8::Value> = v8::Boolean::new(scope, member.name == "scrollHeight").into();
            row_getter_data(scope, proto, member, cb_scroll_size, d)
        }
        // The layout metrics, over the page pipeline (core/dom/geometry_queries.rs, W5.0d).
        "clientTop" | "clientLeft" | "clientWidth" | "clientHeight" => {
            let d: v8::Local<v8::Value> = v8::Integer::new(scope, metric_index(member.name)).into();
            row_getter_data(scope, proto, member, cb_metric, d)
        }
        "children" => { let d = builder_data(scope, "HTMLCollection", Kind::Children); row_getter_data(scope, proto, member, cb_el_collection, d) }
        "firstElementChild" => row_getter(scope, proto, member, cb_first_element_child),
        "lastElementChild" => row_getter(scope, proto, member, cb_last_element_child),
        "childElementCount" => row_getter(scope, proto, member, cb_el_child_element_count),
        "previousElementSibling" => row_getter(scope, proto, member, cb_previous_element_sibling),
        "nextElementSibling" => row_getter(scope, proto, member, cb_next_element_sibling),
        "after" => row_method(scope, proto, member, cb_after),
        "animate" => row_method(scope, proto, member, cb_element_animate),
        "append" => row_method(scope, proto, member, cb_append),
        "attachShadow" => row_method(scope, proto, member, cb_attach_shadow),
        "before" => row_method(scope, proto, member, cb_before),
        "checkVisibility" => row_method(scope, proto, member, cb_el_check_visibility),
        "closest" => row_method(scope, proto, member, cb_closest),
        "getAnimations" => row_method(scope, proto, member, cb_element_get_animations),
        "getAttribute" => row_method(scope, proto, member, cb_el_get_attribute),
        "getAttributeNS" => row_method(scope, proto, member, cb_el_get_attribute_ns),
        "getAttributeNames" => row_method(scope, proto, member, cb_el_get_attribute_names),
        "getAttributeNode" => row_method(scope, proto, member, cb_el_get_attribute_node),
        "getAttributeNodeNS" => row_method(scope, proto, member, cb_el_get_attribute_node_ns),
        "getBoundingClientRect" => row_method(scope, proto, member, cb_el_bounding_client_rect),
        "getClientRects" => row_method(scope, proto, member, cb_el_client_rects),
        "getElementsByClassName" => { let d = builder_data(scope, "HTMLCollection", Kind::ElByClass); row_method_data(scope, proto, member, cb_el_by_name, d) }
        "getElementsByTagName" => { let d = builder_data(scope, "HTMLCollection", Kind::ElByTag); row_method_data(scope, proto, member, cb_el_by_name, d) }
        "getElementsByTagNameNS" => { let d = builder_data(scope, "HTMLCollection", Kind::ElByTagNs); row_method_data(scope, proto, member, cb_by_tag_ns, d) }
        "getHTML" => row_method(scope, proto, member, cb_get_html),
        "hasAttribute" => row_method(scope, proto, member, cb_el_has_attribute),
        "hasAttributeNS" => row_method(scope, proto, member, cb_el_has_attribute_ns),
        "hasAttributes" => row_method(scope, proto, member, cb_el_has_attributes),
        "hasPointerCapture" => row_method(scope, proto, member, cb_el_has_pointer_capture),
        "insertAdjacentElement" => row_method(scope, proto, member, cb_insert_adjacent_element),
        "insertAdjacentHTML" => row_method(scope, proto, member, cb_insert_adjacent_html),
        "insertAdjacentText" => row_method(scope, proto, member, cb_insert_adjacent_text),
        "matches" => row_method(scope, proto, member, cb_el_matches),
        "webkitMatchesSelector" => row_method(scope, proto, member, cb_el_webkit_matches),
        "moveBefore" => row_method(scope, proto, member, cb_move_before),
        "prepend" => row_method(scope, proto, member, cb_prepend),
        "querySelector" => row_method(scope, proto, member, cb_query_selector),
        "querySelectorAll" => row_method(scope, proto, member, cb_query_selector_all),
        "releasePointerCapture" => row_method(scope, proto, member, cb_el_release_pointer_capture),
        "remove" => row_method(scope, proto, member, cb_remove_self),
        "removeAttribute" => row_method(scope, proto, member, cb_el_remove_attribute),
        "removeAttributeNS" => row_method(scope, proto, member, cb_el_remove_attribute_ns),
        "removeAttributeNode" => row_method(scope, proto, member, cb_el_remove_attribute_node),
        "replaceChildren" => row_method(scope, proto, member, cb_replace_children),
        "replaceWith" => row_method(scope, proto, member, cb_replace_with),
        "requestFullscreen" => row_method(scope, proto, member, cb_el_request_fullscreen),
        "requestPointerLock" => row_method(scope, proto, member, cb_el_request_pointer_lock),
        "scroll" => row_method(scope, proto, member, cb_el_scroll),
        "scrollTo" => row_method(scope, proto, member, cb_el_scroll_to),
        "scrollBy" => row_method(scope, proto, member, cb_el_scroll_by),
        "scrollIntoView" => row_method(scope, proto, member, cb_el_scroll_into_view),
        "scrollIntoViewIfNeeded" => row_method(scope, proto, member, cb_el_scroll_into_view_if_needed),
        "webkitRequestFullScreen" | "webkitRequestFullscreen" => row_method(scope, proto, member, cb_el_legacy_void),
        // The last two rows `dom/09-postshells.js` overlaid. Both are
        // `row_method_recv_first`, because Chrome checks the receiver BEFORE the arity -- and both
        // take no required argument, so the two mounts are equivalent here; using it states the order.
        "computedStyleMap" => crate::core::dom::shared::row_method_recv_first(
            scope, proto, member, crate::core::css::style_property_map::cb_el_computed_style_map),
        "startViewTransition" => crate::core::dom::shared::row_method_recv_first(
            scope, proto, member, crate::core::css::view_transition::cb_el_start_view_transition),
        "setAttribute" => row_method(scope, proto, member, cb_el_set_attribute),
        "setAttributeNS" => row_method(scope, proto, member, cb_el_set_attribute_ns),
        "setAttributeNode" | "setAttributeNodeNS" => row_method(scope, proto, member, cb_el_set_attribute_node),
        "setHTMLUnsafe" => row_method(scope, proto, member, cb_set_html_unsafe),
        "setPointerCapture" => row_method(scope, proto, member, cb_el_set_pointer_capture),
        "toggleAttribute" => row_method(scope, proto, member, cb_el_toggle_attribute),
        "currentCSSZoom" => row_getter(scope, proto, member, cb_el_current_css_zoom),
        "customElementRegistry" => row_getter(scope, proto, member, cb_el_custom_element_registry),
        "ariaNotify" => row_method(scope, proto, member, cb_el_aria_notify),
        "pseudo" => row_method(scope, proto, member, cb_el_pseudo),
        other => debug_assert!(false, "install_element: unexpected member {other}"),
    }
}

// The capture's 152 rows, verbatim and in its order (`generated_registry.rs` at the cutover). The
// ARIA reflections are `reflect_nullable_str` kit rows; `id`/`className`/`slot`/`elementTiming` are
// `reflect_str`; the handlers `event_handler`. The seven `aria*Elements` ELEMENT reflections were the
// last rows a JS phase overlaid (`dom/09-postshells.js`); they have Rust bodies now, so
// nothing overlays an `Element` row any more. `activeViewTransition`, `computedStyleMap`,
// `startViewTransition` and `setHTML` are still honest `stub`s.
webapi::interface! {
    // `[interface]`, NOT `[element]`: `Model::Element` now means "WebIDL gives this
    // interface `[HTMLConstructor]`", and webref gives `Element` none. Measured, Chrome 150 over
    // loopback: `Element()` answers the bare `TypeError: Illegal constructor`, where every
    // `[HTMLConstructor]` interface answers `Failed to construct 'X': Please use the 'new'
    // operator, …`. `new Element()` is `Failed to construct 'Element': Illegal constructor` either
    // way, which is why the mislabel was invisible until the two shapes were measured apart.
    "Element" : "Node" [interface, window] {
        ctor "Element" len 0 = stub,
        attr "namespaceURI" : Object readonly nullable = install_element,
        attr "prefix" : Object readonly nullable = install_element,
        attr "localName" : Object readonly = install_element,
        attr "tagName" : Object readonly = install_element,
        attr "id" : Object cfg "id" = reflect_str,
        attr "className" : Object cfg "class" = reflect_str,
        attr "classList" : Object = install_element,
        attr "slot" : Object = reflect_str,
        attr "attributes" : Object readonly = install_element,
        attr "shadowRoot" : Object readonly nullable = install_element,
        attr "part" : Object = install_element,
        attr "assignedSlot" : Object readonly nullable = install_element,
        attr "innerHTML" : Object = install_element,
        attr "outerHTML" : Object = install_element,
        attr "scrollTop" : Object = install_element,
        attr "scrollLeft" : Object = install_element,
        attr "scrollWidth" : Object readonly = install_element,
        attr "scrollHeight" : Object readonly = install_element,
        attr "clientTop" : Object readonly = install_element,
        attr "clientLeft" : Object readonly = install_element,
        attr "clientWidth" : Object readonly = install_element,
        attr "clientHeight" : Object readonly = install_element,
        handler "onbeforecopy" = event_handler,
        handler "onbeforecut" = event_handler,
        handler "onbeforepaste" = event_handler,
        handler "onsearch" = event_handler,
        attr "elementTiming" : Object cfg "elementtiming" = reflect_str,
        handler "onfullscreenchange" = event_handler,
        handler "onfullscreenerror" = event_handler,
        handler "onwebkitfullscreenchange" = event_handler,
        handler "onwebkitfullscreenerror" = event_handler,
        attr "activeViewTransition" : Object readonly nullable = stub,
        attr "role" : Object nullable cfg "role" = reflect_nullable_str,
        attr "ariaAtomic" : Object nullable cfg "aria-atomic" = reflect_nullable_str,
        attr "ariaAutoComplete" : Object nullable cfg "aria-autocomplete" = reflect_nullable_str,
        attr "ariaBusy" : Object nullable cfg "aria-busy" = reflect_nullable_str,
        attr "ariaBrailleLabel" : Object nullable cfg "aria-braillelabel" = reflect_nullable_str,
        attr "ariaBrailleRoleDescription" : Object nullable cfg "aria-brailleroledescription" = reflect_nullable_str,
        attr "ariaChecked" : Object nullable cfg "aria-checked" = reflect_nullable_str,
        attr "ariaColCount" : Object nullable cfg "aria-colcount" = reflect_nullable_str,
        attr "ariaColIndex" : Object nullable cfg "aria-colindex" = reflect_nullable_str,
        attr "ariaColSpan" : Object nullable cfg "aria-colspan" = reflect_nullable_str,
        attr "ariaCurrent" : Object nullable cfg "aria-current" = reflect_nullable_str,
        attr "ariaDescription" : Object nullable cfg "aria-description" = reflect_nullable_str,
        attr "ariaDisabled" : Object nullable cfg "aria-disabled" = reflect_nullable_str,
        attr "ariaExpanded" : Object nullable cfg "aria-expanded" = reflect_nullable_str,
        attr "ariaHasPopup" : Object nullable cfg "aria-haspopup" = reflect_nullable_str,
        attr "ariaHidden" : Object nullable cfg "aria-hidden" = reflect_nullable_str,
        attr "ariaInvalid" : Object nullable cfg "aria-invalid" = reflect_nullable_str,
        attr "ariaKeyShortcuts" : Object nullable cfg "aria-keyshortcuts" = reflect_nullable_str,
        attr "ariaLabel" : Object nullable cfg "aria-label" = reflect_nullable_str,
        attr "ariaLevel" : Object nullable cfg "aria-level" = reflect_nullable_str,
        attr "ariaLive" : Object nullable cfg "aria-live" = reflect_nullable_str,
        attr "ariaModal" : Object nullable cfg "aria-modal" = reflect_nullable_str,
        attr "ariaMultiLine" : Object nullable cfg "aria-multiline" = reflect_nullable_str,
        attr "ariaMultiSelectable" : Object nullable cfg "aria-multiselectable" = reflect_nullable_str,
        attr "ariaOrientation" : Object nullable cfg "aria-orientation" = reflect_nullable_str,
        attr "ariaPlaceholder" : Object nullable cfg "aria-placeholder" = reflect_nullable_str,
        attr "ariaPosInSet" : Object nullable cfg "aria-posinset" = reflect_nullable_str,
        attr "ariaPressed" : Object nullable cfg "aria-pressed" = reflect_nullable_str,
        attr "ariaReadOnly" : Object nullable cfg "aria-readonly" = reflect_nullable_str,
        attr "ariaRelevant" : Object nullable cfg "aria-relevant" = reflect_nullable_str,
        attr "ariaRequired" : Object nullable cfg "aria-required" = reflect_nullable_str,
        attr "ariaRoleDescription" : Object nullable cfg "aria-roledescription" = reflect_nullable_str,
        attr "ariaRowCount" : Object nullable cfg "aria-rowcount" = reflect_nullable_str,
        attr "ariaRowIndex" : Object nullable cfg "aria-rowindex" = reflect_nullable_str,
        attr "ariaRowSpan" : Object nullable cfg "aria-rowspan" = reflect_nullable_str,
        attr "ariaSelected" : Object nullable cfg "aria-selected" = reflect_nullable_str,
        attr "ariaSetSize" : Object nullable cfg "aria-setsize" = reflect_nullable_str,
        attr "ariaSort" : Object nullable cfg "aria-sort" = reflect_nullable_str,
        attr "ariaValueMax" : Object nullable cfg "aria-valuemax" = reflect_nullable_str,
        attr "ariaValueMin" : Object nullable cfg "aria-valuemin" = reflect_nullable_str,
        attr "ariaValueNow" : Object nullable cfg "aria-valuenow" = reflect_nullable_str,
        attr "ariaValueText" : Object nullable cfg "aria-valuetext" = reflect_nullable_str,
        attr "children" : Object readonly = install_element,
        attr "firstElementChild" : Object readonly nullable = install_element,
        attr "lastElementChild" : Object readonly nullable = install_element,
        attr "childElementCount" : Object readonly = install_element,
        attr "previousElementSibling" : Object readonly nullable = install_element,
        attr "nextElementSibling" : Object readonly nullable = install_element,
        method "after" : Object len 0 = install_element,
        method "animate" : Object len 1 required 1 = install_element,
        method "append" : Object len 0 = install_element,
        method "attachShadow" : Object len 1 required 1 = install_element,
        method "before" : Object len 0 = install_element,
        method "checkVisibility" : Object len 0 = install_element,
        method "closest" : Object len 1 required 1 nullable = install_element,
        method "computedStyleMap" : Object len 0 = install_element,
        method "getAnimations" : Object len 0 = install_element,
        method "getAttribute" : Object len 1 required 1 nullable = install_element,
        method "getAttributeNS" : Object len 2 required 2 nullable = install_element,
        method "getAttributeNames" : Object len 0 = install_element,
        method "getAttributeNode" : Object len 1 required 1 nullable = install_element,
        method "getAttributeNodeNS" : Object len 2 required 2 nullable = install_element,
        method "getBoundingClientRect" : Object len 0 = install_element,
        method "getClientRects" : Object len 0 = install_element,
        method "getElementsByClassName" : Object len 1 required 1 = install_element,
        method "getElementsByTagName" : Object len 1 required 1 = install_element,
        method "getElementsByTagNameNS" : Object len 2 required 2 = install_element,
        method "getHTML" : Object len 0 = install_element,
        method "hasAttribute" : Object len 1 required 1 = install_element,
        method "hasAttributeNS" : Object len 2 required 2 = install_element,
        method "hasAttributes" : Object len 0 = install_element,
        method "hasPointerCapture" : Object len 1 required 1 = install_element,
        method "insertAdjacentElement" : Object len 2 required 2 nullable = install_element,
        method "insertAdjacentHTML" : Object len 2 required 2 = install_element,
        method "insertAdjacentText" : Object len 2 required 2 = install_element,
        method "matches" : Object len 1 required 1 = install_element,
        method "moveBefore" : Object len 2 required 2 = install_element,
        method "prepend" : Object len 0 = install_element,
        method "querySelector" : Object len 1 required 1 nullable = install_element,
        method "querySelectorAll" : Object len 1 required 1 = install_element,
        method "releasePointerCapture" : Object len 1 required 1 = install_element,
        method "remove" : Object len 0 = install_element,
        method "removeAttribute" : Object len 1 required 1 = install_element,
        method "removeAttributeNS" : Object len 2 required 2 = install_element,
        method "removeAttributeNode" : Object len 1 required 1 = install_element,
        method "replaceChildren" : Object len 0 = install_element,
        method "replaceWith" : Object len 0 = install_element,
        method "requestFullscreen" : Promise len 0 = install_element,
        method "requestPointerLock" : Promise len 0 = install_element,
        method "scroll" : Promise len 0 = install_element,
        method "scrollBy" : Promise len 0 = install_element,
        method "scrollIntoView" : Promise len 0 = install_element,
        method "scrollIntoViewIfNeeded" : Object len 0 = install_element,
        method "scrollTo" : Promise len 0 = install_element,
        method "setAttribute" : Object len 2 required 2 = install_element,
        method "setAttributeNS" : Object len 3 required 3 = install_element,
        method "setAttributeNode" : Object len 1 required 1 nullable = install_element,
        method "setAttributeNodeNS" : Object len 1 required 1 nullable = install_element,
        method "setHTMLUnsafe" : Object len 1 required 1 = install_element,
        method "setPointerCapture" : Object len 1 required 1 = install_element,
        method "startViewTransition" : Object len 0 = install_element,
        method "toggleAttribute" : Object len 1 required 1 = install_element,
        method "webkitMatchesSelector" : Object len 1 required 1 = install_element,
        method "webkitRequestFullScreen" : Object len 0 = install_element,
        method "webkitRequestFullscreen" : Object len 0 = install_element,
        attr "currentCSSZoom" : Object readonly = install_element,
        attr "customElementRegistry" : Object readonly nullable = install_element,
        attr "ariaColIndexText" : Object nullable cfg "aria-colindextext" = reflect_nullable_str,
        attr "ariaRowIndexText" : Object nullable cfg "aria-rowindextext" = reflect_nullable_str,
        attr "ariaActionsElements" : Object = install_aria_element_reflection,
        attr "ariaActiveDescendantElement" : Object nullable = stub,
        attr "ariaControlsElements" : Object nullable = install_aria_element_reflection,
        attr "ariaDescribedByElements" : Object nullable = install_aria_element_reflection,
        attr "ariaDetailsElements" : Object nullable = install_aria_element_reflection,
        attr "ariaErrorMessageElements" : Object nullable = install_aria_element_reflection,
        attr "ariaFlowToElements" : Object nullable = install_aria_element_reflection,
        attr "ariaLabelledByElements" : Object nullable = install_aria_element_reflection,
        method "ariaNotify" : Object len 1 required 1 = install_element,
        method "pseudo" : Object len 1 required 1 nullable = install_element,
        // `undefined`-returning (the Sanitizer API); the stub is typed so the D2 stub answers undefined.
        method "setHTML" : Undefined len 1 required 1 = stub,
        method "@@unscopables" : Object len 0 hidden cfg "after,append,before,prepend,remove,replaceChildren,replaceWith,slot" = unscopables
    }
}

#[cfg(test)]
mod tests {
    use super::tt_candidate;

    #[test]
    fn tt_candidate_is_the_lowercased_test() {
        let old = |local: &str| {
            let lower = local.to_ascii_lowercase();
            lower.starts_with("on") || matches!(lower.as_str(), "src" | "srcdoc" | "data" | "codebase" | "href")
        };
        for n in ["ONCLICK", "onx", "o", "Src", "srcDOC", "Data", "codebase", "HREF", "href2", "xlink", "title", "données", "", "On", "ön", "oN\u{e9}"] {
            assert_eq!(tt_candidate(n), old(n), "{n}");
        }
    }
}

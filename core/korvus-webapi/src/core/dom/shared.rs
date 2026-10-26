//! `core/dom/shared.rs` — the declarative accessor machinery shared across the DOM interfaces.
//!
//! These three `macro_rules!` are how most of Node/Element's arena accessors are written: one line per
//! member (`node_getter!(cb_first_child, |dom, id| dom.first_child(id))`) expanding to the raw v8
//! callback an `interface!` row names. They live here rather than in node.rs or element.rs because
//! attr.rs and named_node_map.rs use them too — the Placement rule's "lowest-common-ancestor of the
//! callers" is this domain, so `<domain>/shared.rs` is the home.
//!
//! They each USED TO emit a second item beside the callback: a `#[op2]` fn that handed the same
//! callback to JS as a native function, back when a JS shell layer asked Rust for its members by
//! name. That layer is gone and no invocation wanted the op, so the macros stopped
//! emitting one at all — twenty-three registered ops in four edits. A fourth macro, `op_dtl_like!`,
//! emitted ONLY the op and is deleted outright; the six `cb_attr_*` callbacks it wrapped are table
//! rows of `core/dom/attr.rs` and are untouched.
//!
//! The invocations sit here with the definitions: a `macro_rules!` is textually scoped, so splitting
//! them would mean exporting each macro and importing it back into four files to save nothing.

use deno_core::{v8, OpState};
use korvus_dom::Dom;
use crate::binding::protos::{build_fn, wrap_cached, throw_dom};
use crate::binding::host_dom::HostDom;
use crate::core::dom::document::{JsDocument, PAGE_DOC_ROOT, doc_root_slot, resolve_doc_root};
use crate::core::dom::element::JsElement;
use crate::core::events::shared::{ON_DOCUMENT_ID};
use crate::binding::protos::{global_get, str_val};
// `el.style`'s memoisation helpers, reused verbatim for `childNodes`'s `[SameObject]`.
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;

macro_rules! node_getter {
    ($cb:ident, |$dom:ident, $id:ident| $target:expr) => {
        pub(crate) fn $cb(
            scope: &mut v8::PinScope,
            args: v8::FunctionCallbackArguments,
            mut rv: v8::ReturnValue,
        ) {
            let this = args.this();
            // Element (its id) OR the document (Document->Node navigation resolves against the arena
            // root: parent None, next/prev-sibling None, first/last-child + children as expected).
            let el_id = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id);
            let is_doc = el_id.is_none() && deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).is_some();
            if el_id.is_none() && !is_doc { return; }
            // A Document receiver navigates from ITS OWN root, which for the page document is the
            // arena root and for a nested browsing context is that frame's parsed root.
            let slot = if is_doc { doc_root_slot(scope, this.into()) } else { PAGE_DOC_ROOT };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let target: Option<u32> = {
                let st = op_state.borrow();
                let $dom = &st.borrow::<HostDom>().0;
                let $id = el_id.unwrap_or_else(|| resolve_doc_root($dom, slot));
                $target
            };
            match target {
                Some(c) => {
                    let obj = wrap_cached(scope, &op_state, c);
                    rv.set(obj.into());
                }
                None => rv.set_null(),
            }
        }
    };
}
node_getter!(cb_first_element_child, |dom, id| dom.first_element_child(id));
node_getter!(cb_last_element_child, |dom, id| {
    // The arena has a forward element-child walker and no reverse one, so this steps back from the
    // last child over the non-elements -- the same traversal as first_element_child, mirrored.
    let mut cur = dom.last_child(id);
    while let Some(n) = cur {
        if dom.node_type(n) == 1 { break; }
        cur = dom.previous_sibling(n);
    }
    cur
});
node_getter!(cb_next_element_sibling, |dom, id| dom.next_element_sibling(id));
node_getter!(cb_previous_element_sibling, |dom, id| {
    // The arena walks element siblings forwards only, so this steps back over the non-elements --
    // the same traversal, mirrored. Installed as a native getter rather than reached through
    // JsElement.prototype, because 06-misc interns that one behind a JS wrapper and the surface
    // then reads `previousElementSibling` as not-[native code].
    let mut cur = dom.previous_sibling(id);
    while let Some(n) = cur {
        if dom.node_type(n) == 1 { break; }
        cur = dom.previous_sibling(n);
    }
    cur
});
node_getter!(cb_parent_element, |dom, id| dom.parent(id).filter(|&p| dom.tag_name(p).is_some()));
node_getter!(cb_first_child, |dom, id| dom.first_child(id));
node_getter!(cb_last_child, |dom, id| dom.last_child(id));
node_getter!(cb_next_sibling, |dom, id| dom.next_sibling(id));
node_getter!(cb_previous_sibling, |dom, id| dom.previous_sibling(id));
/// `Node.parentNode` -- like `parentElement` EXCEPT that a node whose parent is the document
/// answers the document, not null (`document.documentElement.parentNode === document`, and the
/// doctype's too).
///
/// `parentElement` used to be installed under this name to dodge the identity problem: the page
/// document is an arena node, and interning it as an element would hand JS a second, non-`Document`
/// object for `document`. The fix is to return the `document` singleton for that one parent, not to
/// answer null -- answering null stopped every walk to a node's ROOT one node short, and the root
/// is what `Range`'s WrongDocumentError rule and `furthestAncestor` are both built on.
pub(crate) fn cb_parent_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
        Some(p) => unsafe { p.as_ref() }.id,
        // A Document receiver has no parent -- null, never an unset (undefined) return value.
        None => {
            if deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).is_some() { rv.set_null(); }
            return;
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = {
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.parent(id)
    };
    // A DOCUMENT parent needs no branch here. Every Document object is interned in the node cache
    // under its arena root when it is minted (`document.rs::intern_document`), so `wrap_cached`
    // answers the document itself -- the page's, a frame's, or a created one's. This used to carry a
    // page-root special case and answer a second, DocumentFragment-typed object for every other
    // document; `documentElement.parentNode` on a `createHTMLDocument()` result was the visible form.
    match parent {
        None => rv.set_null(),
        Some(p) => { let obj = wrap_cached(scope, &op_state, p); rv.set(obj.into()); }
    }
}
node_getter!(cb_assigned_slot, |dom, id| dom.assigned_slot(id));
/// The SyntaxError a selector method throws for a selector that does not parse, in Blink's words
/// (Chrome 150, measured 2026-10-05): `Failed to execute 'querySelector' on 'Document': '<<<' is not a
/// valid selector.`, `… The provided selector is empty.` for `""`. Korvus threw the bare `'<<<' is not
/// a valid selector`, so the `onerror` message and the `stack` of every such throw read differently.
pub(crate) fn throw_selector_error(scope: &mut v8::PinScope, method: &str, iface: &str, sel: &str) {
    let why = if sel.is_empty() { "The provided selector is empty.".to_string() } else { format!("'{sel}' is not a valid selector.") };
    throw_dom(scope, &format!("Failed to execute '{method}' on '{iface}': {why}"), "SyntaxError");
}

/// The interface Blink names in a selector method's message for node `id`: `DocumentFragment` for a
/// fragment and a shadow root (measured: a ShadowRoot's `querySelector` says `on 'DocumentFragment'`),
/// `Element` otherwise.
pub(crate) fn node_iface(scope: &mut v8::PinScope, id: u32) -> &'static str {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(d) = st.try_borrow::<HostDom>() else { return "Element" };
    if d.0.is_shadow_root(id) {
        return "DocumentFragment";
    }
    match d.0.node_type(id) {
        11 => "DocumentFragment",
        9 => "Document",
        _ => "Element",
    }
}

macro_rules! node_query {
    ($cb:ident, $method:literal, |$dom:ident, $id:ident, $sel:ident| $target:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let this = args.this();
            let $id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
                Some(p) => unsafe { p.as_ref() }.id, None => return,
            };
            let $sel = match args.get(0).to_string(scope) {
                Some(s) => s.to_rust_string_lossy(scope), None => return,
            };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let target: Result<Option<u32>, _> = { let st = op_state.borrow(); let $dom = &st.borrow::<HostDom>().0; $target };
            match target {
                Ok(Some(c)) => { let o = wrap_cached(scope, &op_state, c); rv.set(o.into()); }
                Ok(None) => rv.set_null(),
                Err(_) => { let iface = node_iface(scope, $id); throw_selector_error(scope, $method, iface, &$sel) }
            }
        }
    };
}
// querySelector / querySelectorAll DISPATCH ON THE RECEIVER rather than living twice.
//
// A shadow root's queries are shadow-scoped (`:host` resolves, and the light tree is invisible), and
// the engine used to express that by installing shadow-scoped copies as OWN properties of
// `ShadowRoot.prototype`. The behaviour was right and the SHAPE was wrong: Chrome has neither
// `querySelector`, `querySelectorAll` nor `getElementById` on ShadowRoot.prototype -- all three are
// inherited from DocumentFragment.prototype, and the scoping comes from the receiver. Three own
// properties Chrome does not have are three extra names in
// `Object.getOwnPropertyNames(ShadowRoot.prototype)`, which is one line for a page to read.
// (`getElementById` needed no dispatch at all: ShadowRoot was installing the SAME body it inherits,
// `cb_frag_get_element_by_id` — the DocumentFragment row in `core/dom/document_fragment.rs`.)
node_query!(cb_query_selector, "querySelector", |dom, id, sel| if dom.is_shadow_root(id) { dom.shadow_descendants_matching(id, &sel).map(|v| v.into_iter().next()) } else { dom.descendants_matching(id, &sel).map(|v| v.into_iter().next()) });
node_query!(cb_closest, "closest", |dom, id, sel| dom.closest(id, &sel));
/// `Node.childNodes`'s memoised list, and the arena root it was minted over, on the node wrapper.
const CHILD_NODES_CACHE: &str = "korvus_child_nodes";
const CHILD_NODES_ROOT: &str = "korvus_child_nodes_root";

/// `Node.childNodes` -- a LIVE NodeList over the receiver's child nodes.
///
/// It used to be a bare `v8::Array`: `Object.prototype.toString.call(node.childNodes)` read
/// "[object Array]", `instanceof NodeList` was false, the page could `push` onto it, and it was a
/// SNAPSHOT -- a list read before an `appendChild` never saw the new child. It is the platform type
/// now, minted from the same interceptor template as every other list (core/dom/collections.rs), and
/// live: the arena query re-runs on every access.
///
/// The receiver may be an Element (its id) or a Document (which navigates from ITS OWN root -- the
/// arena root for the page document, a frame's parsed root for a nested browsing context).
///
/// `[SameObject]` (see the two privates above): `node.childNodes === node.childNodes`. The list is a
/// PROPERTY of the node, not a
/// value minted per read -- exactly `el.style` and `el.classList`, and memoised the same way, on a
/// private slot of the (interned, via `NodeCache`) node wrapper. It used to call `make_live` on
/// every access, so the identity failed with the message that gives the defect away: *expected
/// object "[object NodeList]" but got object "[object NodeList]"*.
///
/// Nothing needs reseeding: the cached object is LIVE (its length and indices come from an
/// interceptor that re-runs the arena query), so it always reports the current children. The one
/// thing that can go stale is the ROOT it was minted over -- a Document receiver resolves
/// `PAGE_DOC_ROOT` through `Dom::root()`, which moves when the arena is swapped -- so the root is
/// stored beside the list and a mismatch re-mints.
pub(crate) fn cb_child_nodes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let el_id = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id);
    let is_doc = el_id.is_none() && deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).is_some();
    if el_id.is_none() && !is_doc { return; }
    let slot = if is_doc { doc_root_slot(scope, this.into()) } else { PAGE_DOC_ROOT };
    let root = match el_id {
        Some(id) => id,
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            resolve_doc_root(&st.borrow::<HostDom>().0, slot)
        }
    };
    let cached_root = priv_get(scope, this, CHILD_NODES_ROOT).and_then(|v| v.int32_value(scope));
    if cached_root == Some(root as i32) {
        if let Some(list) = priv_get(scope, this, CHILD_NODES_CACHE).filter(|v| v.is_object()) {
            rv.set(list);
            return;
        }
    }
    // made in the node's realm, whichever realm's getter reads it first (`collections::in_relevant_realm`)
    let list = crate::core::dom::collections::in_relevant_realm(scope, this, |scope| {
        crate::core::dom::collections::make_live(scope, "NodeList", root, crate::core::dom::collections::Kind::ChildNodes, "", None)
    });
    priv_set(scope, this, CHILD_NODES_CACHE, list);
    let r = v8::Integer::new(scope, root as i32);
    priv_set(scope, this, CHILD_NODES_ROOT, r.into());
    rv.set(list);
}

macro_rules! node_query_list {
    // The ONE member this builds: `querySelectorAll`, whose IDL return type is NodeList and whose
    // answer is a STATIC list -- the snapshot the spec requires, not a re-query.
    //
    // It used to carry a second arm that returned a bare `v8::Array`, invoked for
    // `getElementsByTagName`/`getElementsByClassName`, marked "re-installed over the live collection
    // factory in js/dom/08c-collections.js, so wrapping them would be the wrong interface AND dead
    // work". That JS file is gone and the prediction came true: BOTH members are `interface!` rows
    // over `core/dom/collections.rs`'s live HTMLCollection (`cb_el_by_name` on Element,
    // `cb_doc_by_name` on Document), so the arm and its two callbacks had no referrer at all and
    // WERE DELETED with the two ops they emitted. Nothing else ever wanted a bare Array.
    ($cb:ident, $method:literal, |$dom:ident, $id:ident, $sel:ident| $ids:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let this = args.this();
            let $id = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()) {
                Some(p) => unsafe { p.as_ref() }.id, None => return,
            };
            let $sel = match args.get(0).to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => return };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let ids: Result<Vec<u32>, _> = { let st = op_state.borrow(); let $dom = &st.borrow::<HostDom>().0; $ids };
            let ids = match ids { Ok(v) => v, Err(_) => { let iface = node_iface(scope, $id); throw_selector_error(scope, $method, iface, &$sel); return; } };
            let arr = v8::Array::new(scope, ids.len() as i32);
            for (i, cid) in ids.iter().enumerate() {
                let w = wrap_cached(scope, &op_state, *cid);
                arr.set_index(scope, i as u32, w.into());
            }
            let list = crate::core::dom::collections::make_static(scope, "NodeList", arr);
            rv.set(list);
        }
    };
}
node_query_list!(cb_query_selector_all, "querySelectorAll", |dom, id, sel| if dom.is_shadow_root(id) { dom.shadow_descendants_matching(id, &sel) } else { dom.descendants_matching(id, &sel) });


/// The arena id a Document receiver/argument names, or `None` when `v` is not a native Document.
///
/// The page document keeps its `ON_DOCUMENT_ID` (`u32::MAX`) name -- the whole binding layer, and
/// `norm_doc`/`dom_root`/`node_path` below, are built on it. A Document that owns a root (a nested
/// browsing context's, a `DOMParser` result's) names THAT root instead: it is a real arena node, so
/// there is no reason to hide it behind a sentinel, and doing so is what made a frame's document
/// compare equal to the page's.
pub(crate) fn document_node_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let p = deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, v)?;
    let root = unsafe { p.as_ref() }.root;
    Some(if root == PAGE_DOC_ROOT { ON_DOCUMENT_ID } else { root })
}

/// The arena id a *boundary container* names, over and above `arg_node_id`.
///
/// A Range boundary may legally be a document that is not the page document -- `foreignDoc` in
/// WPT's `dom/common.js` is `document.implementation.createHTMLDocument("")`, and its endpoints
/// appear in `testRanges`/`testPoints` dozens of times. A created document is a cppgc `JsDocument`
/// naming its own arena root, so [`document_node_id`] answers for it and this is two unwraps.
///
/// It used to end with a third clause reading `__docKind` and `_root` off a plain object -- the
/// created-document FACADE of `js/dom/05-xml.js`, which minted documents as object literals. W5.0e
/// deleted that file and the object shape with it; nothing in this tree has written either property
/// since, so the clause could not fire (grep: the only remaining mentions were its own, and
/// `range.rs`'s twin `facade_root`, deleted in the same commit).
pub(crate) fn boundary_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    if let Some(id) = document_node_id(scope, v) { return Some(id); }
    deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, v).map(|p| unsafe { p.as_ref() }.id)
}

// ── moved out of bindings.rs (STEP 3 dissolution) ──
pub(crate) fn range_point(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, container_key: &str, offset_key: &str) -> Option<(u32, i64)> {
    // The boundary's own slots (where `set_point` writes and the `AbstractRange` getters read), never
    // `this.startContainer`: a page that wraps those getters must not see every Range method read them.
    let node = crate::core::events::event::evt_slot_read(scope, this, container_key)?;
    let id = boundary_id(scope, node)?;
    let offset = crate::core::events::event::evt_slot_read(scope, this, offset_key)?.integer_value(scope).unwrap_or(0);
    Some((id, offset))
}

/// `Dom::node_type` under the `ON_DOCUMENT_ID` convention.
///
/// The document IS a real arena node (the arena root), but the binding layer names it with the
/// `u32::MAX` sentinel rather than its arena id -- `arg_node_id`/`range_point` both return the sentinel
/// for a `JsDocument`. Ask the arena directly with that and you index `nodes[u32::MAX]`, which used to
/// SIGABRT (a panic in a v8 callback frame cannot unwind). `node_path`/`node_len` here have always
/// guarded it; `range.rs` and the NodeIterator flatten did not (the flatten goes through
/// `tw_accept_node`, which asks here, since CONF CE1.2).
///
/// Answering 9 (Node.DOCUMENT_NODE) is not a fallback, it is the right answer -- the caller really is
/// asking about the document.
pub(crate) fn node_type_of(dom: &Dom, id: u32) -> u32 {
    if id == ON_DOCUMENT_ID { 9 } else { dom.node_type(id) }
}

/// `Dom::children` under the same convention: the document's children are the arena root's.
pub(crate) fn children_of(dom: &Dom, id: u32) -> Vec<u32> {
    if id == ON_DOCUMENT_ID { dom.children(dom.root()) } else { dom.children(id) }
}

/// The page document has TWO names: `ON_DOCUMENT_ID` (what the binding layer hands out for a
/// `JsDocument`) and `dom.root()` (what the arena's parent chain walks up to). Anything that
/// compares containers or roots has to agree on one, or a range whose container is `document`
/// looks like it lives in a different tree from a range on `document.body`.
pub(crate) fn norm_doc(dom: &Dom, id: u32) -> u32 {
    if id == dom.root() { ON_DOCUMENT_ID } else { id }
}

/// A node's DOM **root**: its topmost inclusive ancestor (DOM §4.2.2).
///
/// The whole point is that this walks the parent chain to the END -- a detached node is its own
/// root, a node in a created document has that document as its root, and only a node actually in
/// the page tree gets `ON_DOCUMENT_ID`. Stopping the walk at the first non-element parent (which is
/// what this used to do) made every attached node's root the `<html>` element and the page document
/// a root of its own, so `compareBoundaryPoints` threw `WrongDocumentError` between two ranges in
/// the same tree.
pub(crate) fn dom_root(dom: &Dom, id: u32) -> u32 {
    if id == ON_DOCUMENT_ID { return ON_DOCUMENT_ID; }
    let mut cur = id;
    // The parent chain is acyclic (the insertion guards keep it so); cap anyway, as `contains` does.
    let mut budget = 1 << 20;
    while let Some(p) = dom.parent(cur) {
        cur = p;
        budget -= 1;
        if budget == 0 { break; }
    }
    norm_doc(dom, cur)
}

pub(crate) fn node_path(dom: &Dom, id: u32) -> Vec<u32> {
    if id == ON_DOCUMENT_ID { return Vec::new(); }
    let mut path = Vec::new();
    let mut cur = id;
    // Every parent, not just element parents: two children of a DocumentFragment (or of a created
    // document) are ordered by their indices under it, and a path that stopped at the fragment
    // reported them equal.
    while let Some(p) = dom.parent(cur) {
        // the index under `p`: the siblings before it, counted over the links (making `p`'s child list for each
        // level was nearly all of a `compareDocumentPosition`)
        let mut idx = 0u32;
        let mut s = dom.previous_sibling(cur);
        while let Some(x) = s {
            idx += 1;
            s = dom.previous_sibling(x);
        }
        path.push(idx);
        cur = p;
    }
    path.reverse();
    path
}

/// A node's **length** (DOM §4.4): 0 for a DocumentType, the data length for a CharacterData node,
/// the child count for anything else. CharacterData is Text(3), CDATASection(4),
/// ProcessingInstruction(7) and Comment(8) -- counting a PI's or a CDATA section's CHILDREN (always
/// zero) rejected every legal offset into one, which is why `rangeFromEndpoints` threw on
/// `[processingInstruction, 0, processingInstruction, 4]` and took a whole range out of the corpus.
pub(crate) fn node_len(dom: &Dom, id: u32) -> i64 {
    // The document's length is its child count, like any other non-CharacterData node.
    if id == ON_DOCUMENT_ID { return dom.children(dom.root()).len() as i64; }
    match dom.node_type(id) {
        10 => 0,
        3 | 4 | 7 | 8 => dom.node_value(id).map(|s| s.encode_utf16().count() as i64).unwrap_or(0),
        _ => dom.children(id).len() as i64,
    }
}

pub(crate) fn cmp_points(dom: &Dom, ca: u32, oa: i64, cb: u32, ob: i64) -> i32 {
    let (ca, cb) = (norm_doc(dom, ca), norm_doc(dom, cb));
    if ca == cb { return (oa - ob).signum() as i32; }
    let mut a = node_path(dom, ca); a.push(oa.max(0) as u32);
    let mut b = node_path(dom, cb); b.push(ob.max(0) as u32);
    for i in 0..a.len().min(b.len()) {
        if a[i] != b[i] { return if a[i] < b[i] { -1 } else { 1 }; }
    }
    (a.len() as i32 - b.len() as i32).signum()
}

pub(crate) fn collect_text(dom: &Dom, id: u32, out: &mut Vec<u32>) {
    if dom.node_type(id) == 3 { out.push(id); } else { for c in dom.children(id) { collect_text(dom, c, out); } }
}

/// Does `selector` match an element right now? (existence check the real-time click scheduler runs up
/// front, so `click_selector` can return `None` before it starts moving the cursor.)
pub fn selector_exists(scope: &mut v8::PinScope, selector: &str) -> bool {
    let doc = match global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(d) => d, None => return false,
    };
    let sel = str_val(scope, selector);
    matches!(crate::binding::protos::pristine_call(scope, doc.into(), "Document.querySelector", &[sel]), Some(v) if !v.is_null_or_undefined())
}

/// `document.querySelector(selector)` → the element, or None if no match / not an object.
///
/// The realm's ORIGINAL `querySelector` ([`crate::binding::protos::pristine_call`]), as for [`selector_exists`]: these
/// two serve the AUTOMATION (`click_selector`, the humanised click), and a page that wrapped
/// `Document.prototype.querySelector` saw the client's selector arrive with no JS caller on the stack.
pub(crate) fn query_el<'s>(scope: &mut v8::PinScope<'s, '_>, selector: &str) -> Option<v8::Local<'s, v8::Object>> {
    let doc = global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let sel = str_val(scope, selector);
    let v = crate::binding::protos::pristine_call(scope, doc.into(), "Document.querySelector", &[sel])?;
    if v.is_null_or_undefined() {
        return None;
    }
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// Like node_getter! but for a string-arg method (querySelector/closest): reads args[0] as the
/// selector. `$target` yields `Result<Option<u32>, _>`; an `Err` (invalid selector) throws a DOM
/// `SyntaxError` (T2'b: strict parse).

/// Node-list member taking a string arg (selector / class / tag). `$ids` yields
/// `Result<Vec<u32>, _>`; an `Err` (invalid selector) throws a DOM `SyntaxError`. The selector-strict
/// members (`querySelectorAll`) propagate the parse error; the `getElementsBy*` members never parse a
/// selector, so they wrap their `Vec` in `Ok(..)` and can never throw.

// ---- Live collections ----
// `getElementsByTagName`/`ClassName`, `children`, `document.forms` and the rest are LIVE collections.
// Their liveness used to be a JS `Proxy` re-running a native re-query op on every property access;
// both are gone. The query is `core/dom/collections.rs::query_ids`, reached directly from the
// interceptors of a native exotic object, and the op that existed only so JS could call it went with
// the Proxy.

// `HTMLCollection.item` / `namedItem` are rows of that interface's `interface!` table
// (core/dom/collections.rs): the direct WebIDL indexed/named getters over a re-query, the receiver
// type-check Chrome enforces, and the arity guard the row's `required 1` mounts.

// `call_mutobs` IS DELETED. It called `KB.__mutobs.<method>(args)` because the
// MutationObserver registry lived in JS -- the same bridge pattern `KB.__ranges` had before W4.e
// moved it. The registry is `core/dom/mutation_observer.rs` now, so the three `notify_*` functions
// below call `record_child_list` / `record_attributes` / `record_character_data` as plain Rust, and
// there is no bridge slot, no name lookup, and no "cheap no-op when `__mutobs` is not installed": a
// realm with no registry simply has no entries.
/// As notify_childlist, plus the record's `previousSibling`/`nextSibling` (the nodes flanking the
/// added/removed run; None -> null). MutationObserver childList records expose these.
pub(crate) fn notify_childlist_sib(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>], removed: &[v8::Local<v8::Value>], prev: Option<v8::Local<v8::Value>>, next: Option<v8::Local<v8::Value>>) {
    use crate::core::dom::mutation_observer::Sib;
    notify_childlist_sibs(scope, parent, added, removed, Sib::of(prev), Sib::of(next));
}

/// [`notify_childlist_sib`] with the siblings by arena id: wrapped only if an observer records them
/// (`mutation_observer::Sib`). For a caller that captured the ids and runs nothing that can run script between
/// that and this call.
pub(crate) fn notify_childlist_sib_ids(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>], removed: &[v8::Local<v8::Value>], prev: Option<u32>, next: Option<u32>) {
    use crate::core::dom::mutation_observer::Sib;
    notify_childlist_sibs(scope, parent, added, removed, Sib::of_id(prev), Sib::of_id(next));
}

fn notify_childlist_sibs(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>], removed: &[v8::Local<v8::Value>], prev: crate::core::dom::mutation_observer::Sib, next: crate::core::dom::mutation_observer::Sib) {
    // a frame this insertion adds begins its navigation as it is inserted (`browsing_context::navigation_begins_now`)
    let outer = (!added.is_empty()).then(|| crate::core::html::browsing_context::insertion_began(scope));
    notify_childlist_inner(scope, parent, added, removed, prev, next);
    if let Some(outer) = outer {
        crate::core::html::browsing_context::insertion_ended(scope, outer);
    }
}

fn notify_childlist_inner(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>], removed: &[v8::Local<v8::Value>], prev: crate::core::dom::mutation_observer::Sib, next: crate::core::dom::mutation_observer::Sib) {
    crate::core::dom::mutation_observer::record_child_list(scope, parent, added, removed, prev, next);
    // Insertion is the OTHER thing that starts a subresource load: a `<script src>` or a `<link>`
    // fetches only once it is in a document, however it got there. Every insertion path in the arena
    // funnels through this function with its added nodes, which makes it the one place that sees them
    // all -- appendChild, insertBefore, replaceChild, prepend/append/after/before, insertAdjacent*.
    for n in added {
        notify_resource_inserted(scope, *n);
    }
    // A node inserted INTO a script element is that script's children-changed steps.
    if !added.is_empty() {
        crate::core::html::html_script_element::script_children_changed(scope, parent);
    }
    // THE DISCONNECT REACTIONS THE REMOVAL OWES, and they are here for the reason the
    // line above is: removal is the other thing every arena mutation path funnels through this
    // function to report, with its removed nodes, which makes it the one place that sees them all.
    // The connect half sits at the symmetric point (`node.rs::notify_connected`), and a second site
    // for either is the two-mechanisms defect. `notify_disconnected` decides whether the old parent
    // was connected and walks each removed subtree itself.
    crate::core::dom::node::notify_disconnected(scope, parent, removed);
}

/// Tell the subresource loader (`core/html/subresource.rs`) that `node` (and its subtree) was
/// inserted. A no-op unless the node is an element that loads from one of its attributes.
pub(crate) fn notify_resource_inserted(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    crate::core::html::subresource::resource_inserted(scope, node);
}
/// Notify the MutationObserver registry of an attributes mutation on `target` (attribute name + the
/// value BEFORE the change, for observers that requested `attributeOldValue`).
///
/// `name` is the attribute's key in the arena (its qualified name); the record and the reaction name it `local` in
/// `ns` -- the same name and no namespace for every writer but `setAttributeNS` / `removeAttributeNS`, which name the
/// attribute by its local name in its namespace as Chrome 152 does: `setAttributeNS('urn:x', 'p:foo', '1')` is
/// `foo` in `urn:x` (2026-10-23, probe r2/l3/www/b4.html), where korvus named it `p:foo` in no namespace.
fn notify_attributes(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, name: &str, local: &str, ns: Option<&str>, old: Option<&str>) {
    crate::core::dom::mutation_observer::record_attributes(scope, target, local, ns, old);
    // THE CUSTOM-ELEMENT ATTRIBUTE REACTION. It rides the MutationObserver's choke
    // point rather than being bolted onto `setAttribute`, exactly as `notify_resource_attr` does one
    // function below -- `setAttribute`, `removeAttribute`, `toggleAttribute` and every reflected IDL
    // setter reach the page's `attributeChangedCallback` through this one call, and the paths that
    // do not produce a MutationObserver record do not produce a reaction either. The NEW value is
    // read back from the arena rather than passed in, because both callers write the arena before
    // they notify: a set reads back `Some(new)`, a removal reads back `None`, and that IS the
    // spec's newValue/`null`.
    // No definition, no reaction (`attribute_changed`'s own first test), so the new value is read back only for a
    // page that defines a custom element: every other attribute write skips the re-read.
    if crate::core::html::custom_element_registry::nothing_defined(scope) {
        return;
    }
    let new = v8::Local::<v8::Object>::try_from(target).ok().and_then(|o| crate::core::dom::element::el_id_of(scope, o)).and_then(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.get_attribute(id, name).map(str::to_string)
    });
    // AN ATTRIBUTE THAT WAS NOT THERE AND IS STILL NOT THERE DID NOT CHANGE. `removeAttribute` on a
    // name the element never had reaches this function (the arena write is a no-op, and so is the
    // observer record it queues), and firing the reaction for it would be a callback for nothing:
    // Chrome logs `attr:a:1>N` for the first `removeAttribute("a")` and NOTHING for the second.
    if old.is_some() || new.is_some() {
        crate::core::html::custom_element_registry::attribute_changed(scope, target, local, old.map(str::to_owned), new);
    }
}
/// Set `name` = `value` on `id`'s arena attributes AND record an `attributes` mutation with the
/// pre-change value -- the single choke point every reflected IDL attribute setter (el.id/.className/
/// .dir/.lang/.title/.hidden/.autofocus/.tabIndex/toggleAttribute) funnels through, alongside
/// Element.setAttribute, so an `attributes` MutationObserver sees a record no matter which JS-visible
/// API changed the attribute -- not just setAttribute.
///
/// The per-tag reflected props (`checked`/`src`/`href` and the rest) already notify for free, and
/// still do: `element.rs`'s `cb_reflect_str_set` / `cb_reflect_bool_set` set the attribute by calling
/// the receiver's own `setAttribute`, which lands right here. This used to say that mechanism was
/// "the rstr/rbool mechanism dom.js uses"; there is no `dom.js` in this tree, and those callbacks are
/// reflected-attribute table rows installed from `element.rs`.
///
/// Mirrors set_attribute/remove_attribute's reentrancy discipline: all OpState borrows are dropped
/// before the notify. That notify used to be a call into the JS helper `KB.__mutobs`; it has been
/// Rust since the observer moved into `core/dom/mutation_observer.rs`, and the call below is
/// `record_attributes`, not JS and not an op.
pub(crate) fn set_attribute_notify(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str, value: &str) {
    set_attribute_notify_as(scope, op_state, id, name, None, value);
}

/// [`set_attribute_notify`] for `setAttributeNS`: `named` is the attribute's local name and namespace, which its
/// record names ([`notify_attributes`]); `None` for a writer that names none, whose record names the attribute as
/// `element::attr_written` knows it -- by its qualified name in no namespace, but for a namespaced one.
pub(crate) fn set_attribute_notify_as(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str, named: Option<(&str, Option<&str>)>, value: &str) {
    // the old value and what the attribute-namespace table needs, under one borrow
    let (old, before) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let old = dom.get_attribute(id, name).map(str::to_string);
        let before = crate::core::dom::element::ns_before(&st, dom, id, name, named.is_none(), old.is_some());
        (old, before)
    };
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_attribute(id, name, value);
    let known = match before {
        // in no namespace, written by a writer that names none (or null): nearly every write
        crate::core::dom::element::NsBefore::Plain if named.is_none_or(|(_, ns)| ns.is_none()) => None,
        before => crate::core::dom::element::attr_written(op_state, id, name, named.map(|(_, ns)| ns), old.is_none(), before),
    };
    let (local, ns) = named.unwrap_or_else(|| known.as_ref().map_or((name, None), |(ns, l)| (l.as_str(), Some(ns.as_str()))));
    let target = wrap_cached(scope, op_state, id);
    notify_attributes(scope, target.into(), name, local, ns, old.as_deref());
    notify_resource_attr(scope, target.into(), name);
    // A canvas's `width`/`height` resets its bitmap.
    crate::core::html::html_canvas_element::canvas_attr_changed(scope, target.into(), name);
}

/// Tell the subresource loader (`dom/09j-subresource.js`) that `target`'s `name` attribute was
/// written. Setting `img.src` -- however the page did it -- is what starts an image load in a browser,
/// even on an element that was never inserted, so this rides the same choke point the MutationObserver
/// notification does rather than being bolted onto `setAttribute` alone. A no-op for the overwhelming
/// majority of attributes: the JS side returns immediately unless the (tag, attribute) pair is one
/// that loads.
pub(crate) fn notify_resource_attr(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, name: &str) {
    crate::core::html::subresource::resource_attr(scope, target, name);
}
/// Same as `set_attribute_notify` but for attribute removal (toggleAttribute(false) et al).
pub(crate) fn remove_attribute_notify(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str) {
    remove_attribute_notify_as(scope, op_state, id, name, None);
}

/// [`remove_attribute_notify`] for `removeAttributeNS`: `named` is the attribute's local name and namespace, which its
/// record names ([`notify_attributes`]); `None` for a writer that names none (as [`set_attribute_notify_as`]).
pub(crate) fn remove_attribute_notify_as(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, name: &str, named: Option<(&str, Option<&str>)>) {
    let (old, before) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let old = dom.get_attribute(id, name).map(str::to_string);
        // an attribute the element does not have is no removal: nothing to read or forget
        let before = match old {
            Some(_) => crate::core::dom::element::ns_before(&st, dom, id, name, named.is_none(), true),
            None => crate::core::dom::element::NsBefore::Plain,
        };
        (old, before)
    };
    op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_attribute(id, name);
    let known = match before {
        crate::core::dom::element::NsBefore::Plain => None,
        before => crate::core::dom::element::attr_removed(op_state, id, name, named.is_none(), before),
    };
    let (local, ns) = named.unwrap_or_else(|| known.as_ref().map_or((name, None), |(ns, l)| (l.as_str(), Some(ns.as_str()))));
    let target = wrap_cached(scope, op_state, id);
    let removed = old.is_some();
    // Removing an attribute the element does not have changes nothing, so it queues no `attributes` record: DOM's
    // "remove an attribute by name" returns when there is no attribute, and so does Blink's
    // `Element::removeAttribute` (kNotFound) -- `removeAttribute`, `toggleAttribute(n, false)` and `hidden = false`
    // on an absent attribute each queued one here. (The custom-element reaction already skipped it: old and new
    // both absent.)
    if removed {
        notify_attributes(scope, target.into(), name, local, ns, old.as_deref());
    }
    crate::core::html::html_canvas_element::canvas_attr_changed(scope, target.into(), name);
    // a frame that loses its `srcdoc` navigates (only a removal that removed something)
    if removed {
        crate::core::html::subresource::resource_attr_removed(scope, target.into(), name);
    }
}
/// Notify the MutationObserver registry of a characterData mutation on `target` (a Text/Comment/
/// ProcessingInstruction node): the data value BEFORE the change, for observers that requested
/// `characterDataOldValue`. Mirrors `notify_attributes`; called from every native characterData-mutation
/// path (CharacterData.data=/.appendData/.insertData/.deleteData/.replaceData, Node.nodeValue=,
/// Node.textContent= on a Text/Comment node, and Range.deleteContents/extractContents truncating a
/// boundary text node) so a `characterData` MutationObserver sees a record uniformly.
pub(crate) fn notify_character_data(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, old: Option<String>) {
    crate::core::dom::mutation_observer::record_character_data(scope, target, old.as_deref());
}

// ── the row mounts the spine tables name ───────────────────────────────────────────
//
// A hand table's `= install_x` body mounts ONE member from its row: the WebIDL `.length`, the
// descriptor flags and -- for a method row with `required > 0` -- the arity guard come from the row,
// so a raw callback written for `build_fn` needs nothing else to become a table member. The same
// shape `core/dom/collections.rs::method_row` has, shared here because eleven tables use it.

use crate::binding::protos::{build_fn_data, make_arity_guard};
use crate::install::kits::{define_accessor, define_value};
use crate::install::MemberDef;

/// The interface a row belongs to, from its `"Iface::member"` label -- what Blink's arity message names.
pub(crate) fn row_iface(member: &MemberDef) -> &'static str {
    member.label.split("::").next().unwrap_or(member.name)
}

fn row_method_fn<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, f: v8::Local<'s, v8::Function>) {
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, row_iface(member), member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    define_value(scope, proto, member, v, true);
}

/// Mount one method row from a raw callback.
pub(crate) fn row_method<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let f = build_fn(scope, cb, member.name, member.length as i32);
    row_method_fn(scope, proto, member, f);
}

/// Mount one method row WITHOUT the installer's arity guard, because the body does the receiver check
/// and the arity check itself, in THAT order -- which is Chrome's.
///
/// Measured in Chrome 150 over loopback, 2026-09-11: `SVGStringList.prototype.getItem.call(foreign)`
/// with NO arguments is `TypeError: Illegal invocation`, not "1 argument required" -- and the same for
/// `Node.prototype.appendChild`, `Element.prototype.setAttribute`, `Highlight.prototype.add` and
/// `HighlightRegistry.prototype.set`. The receiver check comes first everywhere. [`row_method`]'s
/// guard wraps the body, so it runs BEFORE the receiver check and reports the arity error for a
/// foreign receiver; W5.1 L9 hit the same order problem and answered it with `branded_method_row`.
///
/// A row mounted here still DECLARES its `required` count -- the capture's column, copied verbatim --
/// and enforces it with `binding::protos::require_args`, which builds the identical message from the
/// identical numbers. The mounted function is a plain native `build_fn`, so name, length and
/// `[native code]` are unchanged either way.
pub(crate) fn row_method_recv_first<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let f = build_fn(scope, cb, member.name, member.length as i32);
    define_value(scope, proto, member, f.into(), true);
}

/// Mount one method row from a raw callback that reads `data` (`args.data()`).
pub(crate) fn row_method_data<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>, data: v8::Local<'s, v8::Value>) {
    let f = build_fn_data(scope, cb, member.name, member.length as i32, data);
    row_method_fn(scope, proto, member, f);
}

/// Mount one readonly attribute row: a `get <name>` native getter, no setter.
pub(crate) fn row_getter<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, get: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, get, &format!("get {}", member.name), 0);
    define_accessor(scope, proto, member, g, None);
}

/// [`row_getter`] with `data` bound to the getter.
pub(crate) fn row_getter_data<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, get: impl v8::MapFnTo<v8::FunctionCallback>, data: v8::Local<'s, v8::Value>) {
    let g = build_fn_data(scope, get, &format!("get {}", member.name), 0, data);
    define_accessor(scope, proto, member, g, None);
}

/// Mount one writable attribute row: the `get <name>` / `set <name>` native pair.
pub(crate) fn row_accessor<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, get: impl v8::MapFnTo<v8::FunctionCallback>, set: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, get, &format!("get {}", member.name), 0);
    let s = build_fn(scope, set, &format!("set {}", member.name), 1);
    define_accessor(scope, proto, member, g, Some(s));
}

/// [`row_accessor`] with `gdata` / `sdata` bound to the two halves.
pub(crate) fn row_accessor_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    get: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: v8::Local<'s, v8::Value>,
    set: impl v8::MapFnTo<v8::FunctionCallback>,
    sdata: v8::Local<'s, v8::Value>,
) {
    let g = build_fn_data(scope, get, &format!("get {}", member.name), 0, gdata);
    let s = build_fn_data(scope, set, &format!("set {}", member.name), 1, sdata);
    define_accessor(scope, proto, member, g, Some(s));
}

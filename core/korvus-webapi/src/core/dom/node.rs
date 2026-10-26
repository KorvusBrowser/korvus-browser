//! `core/dom/node.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{OpState, v8};
use korvus_dom::{Dom, NodeId};
use crate::binding::{HostDom, OwnerMap};
use crate::core::dom::attr::{attr_eid_of, attr_name_of, attr_value_of};
use crate::core::dom::document::{doc_root_slot, mint_document_for, resolve_doc_root, root_origin, DocKind, DocKinds, DocOrigin, JsDocument};
use crate::core::dom::shared::{cmp_points, node_type_of, notify_childlist_sib, notify_childlist_sib_ids, notify_character_data};
use crate::binding::protos::{global_get, require_args, throw_dom, unwrap_id, wrap_cached};
use crate::core::dom::{JsElement, element_display_name, notify_childlist};
use crate::binding::protos::{arg_node_id, get_prop, str_val};
use crate::core::intersection_observer::IntersectionObserver;

/// `Doc` carries the receiver's ROOT SLOT (`PAGE_DOC_ROOT` for the page document), so a member
/// reached through a nested browsing context's document resolves against that frame's tree.
pub(crate) enum NodeRecv { El(NodeId), Doc(u32), Other }
pub(crate) fn node_recv(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> NodeRecv {
    if let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, v) { return NodeRecv::El(unsafe { p.as_ref() }.id); }
    if let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, v) { return NodeRecv::Doc(unsafe { p.as_ref() }.root); }
    NodeRecv::Other
}
pub(crate) fn node_recv_id(r: &NodeRecv, dom: &Dom) -> Option<NodeId> {
    match r {
        NodeRecv::El(id) => Some(*id),
        NodeRecv::Doc(slot) => Some(crate::core::dom::document::resolve_doc_root(dom, *slot)),
        NodeRecv::Other => None,
    }
}
pub(crate) fn cb_node_type_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) {
        // An Attr (a view over an owner's attribute map, `attr.rs`) is a Node of type 2.
        if crate::core::dom::attr::is_attr(scope, args.this().into()) { rv.set_uint32(2); }
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A shadow root is a Document node in the arena and a DocumentFragment (11) to the page.
    let nt = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let id = node_recv_id(&r, dom).unwrap();
        if dom.is_shadow_root(id) { 11 } else { dom.node_type(id) }
    };
    rv.set(v8::Integer::new_from_unsigned(scope, nt).into());
}
pub(crate) fn cb_node_name_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) {
        // An Attr's nodeName is its qualified name.
        if let Some(n) = attr_name_of(scope, args.this()) { let v = str_val(scope, &n); rv.set(v); }
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let name = {
        let st = op_state.borrow();
        let id = node_recv_id(&r, &st.borrow::<HostDom>().0).unwrap();
        // Elements get namespace-aware casing (HTML uppercases, foreign preserves); other node kinds
        // (#text, #comment, doctype name, #document) keep the arena node name verbatim.
        if st.borrow::<HostDom>().0.is_shadow_root(id) {
            "#document-fragment".to_string()
        } else {
            element_display_name(&st, id).unwrap_or_else(|| st.borrow::<HostDom>().0.node_name(id))
        }
    };
    rv.set(v8::String::new(scope, &name).unwrap().into());
}

pub(crate) fn cb_node_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) {
        Some(i) => i,
        // A Document receiver unwraps as JsDocument (not JsElement); its nodeValue is null, not
        // undefined (`unwrap_id` None must not fall through to an unset return value). -- WPT Node-nodeValue.
        // An Attr's nodeValue is its value.
        None => {
            if deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).is_some() { rv.set_null(); }
            else if let Some(v) = attr_value_of(scope, this) { let s = str_val(scope, &v); rv.set(s); }
            return;
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let nv = op_state.borrow().borrow::<HostDom>().0.node_value(id);
    match nv { Some(s) => { let v = v8::String::new(scope, &s).unwrap(); rv.set(v.into()); } None => rv.set_null() }
}

pub(crate) fn cb_node_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let a0 = args.get(0);
    let s = if a0.is_null_or_undefined() { String::new() } else { a0.to_rust_string_lossy(scope) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // node_value() is None (and set_node_value() a no-op) for anything but CharacterData -- only notify
    // when this is really a data mutation (Text/Comment/ProcessingInstruction), matching the spec's
    // nodeValue setter, which "does nothing" for other interfaces.
    let old = op_state.borrow().borrow::<HostDom>().0.node_value(id);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_node_value(id, &s);
    if let Some(old) = old {
        // "Replace data" with offset 0 and count = the old length: every LIVE RANGE boundary inside
        // this node collapses to 0. `CharacterData.data =` already notified; `nodeValue =` reached
        // the same arena mutation and told the ranges nothing, so a range over the node kept an
        // offset into text that no longer existed. -- WPT Range-mutations-dataChange.
        crate::core::dom::range::ranges_text_replaced(scope, id, old.encode_utf16().count() as i64, s.encode_utf16().count() as i64);
        notify_character_data(scope, this.into(), Some(old));
    }
}

// -------------------------------------------------------------------------------------------------
// Argument-count checks lead these callbacks because WebIDL runs them FIRST, before the receiver
// is resolved: `isEqualNode(Node? otherNode)` declares a NULLABLE argument, not an optional one,
// so `o.isEqualNode()` is a TypeError in Chrome and was quietly answering `false` here -- a
// shell's answer, from an implementation that had simply never been asked the question.
// STEP 2 (verbatim move from bindings.rs): polymorphic Node methods (contains/isSameNode/
// isEqualNode/compareDocumentPosition) + the tree-mutation surface (appendChild/insertBefore/
// removeChild/replaceChild, append/replaceChildren, ChildNode before/after/replaceWith,
// insertAdjacent*) with their notify/validity helpers. Re-exported via bindings.rs.
// -------------------------------------------------------------------------------------------------

pub(crate) fn cb_contains_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "contains") { return; }
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let o = node_recv(scope, args.get(0));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let res = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let sid = node_recv_id(&r, dom).unwrap();
        match node_recv_id(&o, dom) { Some(oid) => oid == sid || dom.contains(sid, oid), None => false } };
    rv.set(v8::Boolean::new(scope, res).into());
}
pub(crate) fn cb_is_same_node_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "isSameNode") { return; }
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let o = node_recv(scope, args.get(0));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let res = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let sid = node_recv_id(&r, dom).unwrap();
        matches!(node_recv_id(&o, dom), Some(oid) if oid == sid) };
    rv.set(v8::Boolean::new(scope, res).into());
}
pub(crate) fn cb_is_equal_node_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "isEqualNode") { return; }
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let o = node_recv(scope, args.get(0));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The namespace dimension the spec requires is inside `dom.is_equal_node` now: it is a field on
    // the element node, so the arena's own comparison covers it and the parallel side-map walk this
    // used to do is gone.
    let res = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let sid = node_recv_id(&r, dom).unwrap();
        matches!(node_recv_id(&o, dom), Some(oid) if dom.is_equal_node(sid, oid)) };
    rv.set(v8::Boolean::new(scope, res).into());
}
pub(crate) fn cb_compare_doc_pos_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "compareDocumentPosition") { return; }
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let o = node_recv(scope, args.get(0));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let res: u32 = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let sid = node_recv_id(&r, dom).unwrap();
        // DOM §4.4 step 2: when the two nodes are in DIFFERENT TREES the result is
        // DISCONNECTED | IMPLEMENTATION_SPECIFIC | (PRECEDING or FOLLOWING) -- 35 or 37, never a
        // bare 1. Both disconnected arms here set only DISCONNECTED, which is why
        // `dom/nodes/Node-compareDocumentPosition.html` scored 200/1444: one defect, 1,242
        // subtests, every one of them `assert_in_array: value 1 not in array [35, 37]`.
        //
        // THE DIRECTION IS NOT FREE, WHICH IS WHY THE SPEC ATTACHES IMPLEMENTATION_SPECIFIC TO IT.
        // The order between two disconnected trees is the implementation's to choose, but it must
        // be CONSISTENT: the same pair must answer the same way every time, and `a.compare(b)` and
        // `b.compare(a)` must be opposites. Comparing the two TREE ROOTS by arena id gives both --
        // the roots are stable for the life of the nodes, and the comparison is antisymmetric by
        // construction. It is the same rule the same-tree arm already uses one line below
        // (`oid < sid`), lifted to the root so that every node of one tree answers consistently
        // with every node of the other.
        const DISCONNECTED: u32 = 1;
        const IMPL_SPECIFIC: u32 = 32;
        const PRECEDING: u32 = 2;
        const FOLLOWING: u32 = 4;
        let disconnected = |other_root: Option<u32>| -> u32 {
            // No id at all (a node this arena never saw) sorts as PRECEDING, deterministically.
            let dir = match other_root {
                Some(oroot) if oroot < dom.root_of(sid) => PRECEDING,
                Some(_) => FOLLOWING,
                None => PRECEDING,
            };
            DISCONNECTED | IMPL_SPECIFIC | dir
        };
        match node_recv_id(&o, dom) {
            None => disconnected(None),
            Some(oid) if oid == sid => 0,
            Some(oid) => {
                if dom.contains(sid, oid) { 16 | 4 }        // CONTAINED_BY | FOLLOWING
                else if dom.contains(oid, sid) { 8 | 2 }    // CONTAINS | PRECEDING
                else if dom.root_of(sid) != dom.root_of(oid) { disconnected(Some(dom.root_of(oid))) }
                // SAME TREE, NEITHER CONTAINS THE OTHER: the answer is DOCUMENT order, and this
                // line read CREATION order (`oid < sid`, the arena id). The two disagree whenever a
                // node is created before another and attached after it. `dom/common.js:121-124` is
                // exactly that case and it is what `Node-compareDocumentPosition.html`'s last two
                // subtests check:
                //
                //   foreignComment  = createComment(...);  foreignDoc.appendChild(it)   -> LAST child
                //   foreignTextNode = createTextNode(...); foreignDoc.body.appendChild(it)
                //
                // The comment is created FIRST (smaller id) and ends up AFTER the text node, which
                // sits inside `body` inside the document's first child. So `textNode.compare(comment)`
                // must be FOLLOWING and this answered PRECEDING.
                //
                // `cmp_points` (shared.rs) is the real thing: it compares `node_path`s -- the child
                // index chain from the root -- lexicographically. Range has been using it all along;
                // Node had its own id comparison. Offset 0 on both sides asks purely "which node
                // comes first in tree order", which is the question here.
                else if cmp_points(dom, oid, 0, sid, 0) < 0 { 2 } else { 4 }
            }
        } };
    rv.set(v8::Integer::new_from_unsigned(scope, res).into());
}

/// `Node.baseURI` — the node document's base URL.
///
/// `Node.baseURI` -- the document's base URL, and nothing else.
///
/// THIS USED TO BE HALF AN IMPLEMENTATION, and said so: it honoured an ABSOLUTE `<base href>`,
/// deliberately ignored a relative one, and explained that "the webapi crate has no URL resolver".
/// That had stopped being true -- the crate depends on `url = "2"` and `cb_reflect_url_get` already
/// joined reflected URLs with it -- and the half that was missing is the half real pages use:
/// `<base href="/sub/">` is by far the commonest form. The whole rule lives in
/// `core/dom/document.rs::document_base_url`, which is now the ONE place that knows it; this row, the
/// reflectors, the hyperlink utils, `<form action>`, the subresource loader, the classic-script fetch
/// pass and `Request` all read it instead of reaching for `CookieUrl` on their own.
pub(crate) fn cb_base_uri_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    // The node document's base URL (`subresource::node_base_url`): a frame's nodes answer the frame's.
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        node_recv_id(&r, &st.borrow::<HostDom>().0)
    };
    let base = match id {
        // A created document with an about:blank base (createHTMLDocument / createDocument /
        // parseHTMLUnsafe) and every node in it answer `about:blank` (measured
        // Chrome 150). A DOMParser result keeps the creating URL, which `node_base_url` already gives.
        Some(id) if crate::core::dom::document::node_in_about_blank_doc(scope, id) => "about:blank".to_string(),
        Some(id) => crate::core::html::subresource::node_base_url(scope, id),
        None => crate::core::dom::base_url(scope),
    };
    let v = crate::binding::protos::str_val(scope, &base);
    rv.set(v);
}

/// `Node.normalize()` — coalesce adjacent text nodes over the subtree.
///
/// A raw callback rather than a cppgc method for two reasons. It is a NODE method, so a Document
/// receiver has to work; the cppgc version was reached only through JsElement and could not answer on
/// a document. And a cppgc method returning `()` sets the return value to null, where WebIDL's
/// `undefined normalize()` means undefined -- a one-word difference that a recorded browser answer
/// notices and nothing else does.
pub(crate) fn cb_node_normalize(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let st = op_state.borrow();
        match node_recv_id(&r, &st.borrow::<HostDom>().0) { Some(i) => i, None => return }
    };
    let removals = op_state.borrow_mut().borrow_mut::<HostDom>().0.normalize(id);
    // Reentrant on purpose: each coalesced parent owes its observers a childList record, and an async
    // WPT test on normalize() hangs forever without them.
    for (parent, removed) in removals {
        notify_childlist_batch(scope, &op_state, parent, &[], &removed);
    }
    // `_rv` is left untouched: an unset return value IS undefined.
}

/// The live-range side of the DOM's "insert" algorithm: a node arrived in `parent_id` at `idx`.
/// This was a `KB.__ranges.nodeInserted` call through the bridge; the registry is Rust.
fn notify_insert(scope: &mut v8::PinScope, parent_id: u32, idx: u32) {
    crate::core::dom::range::ranges_node_inserted(scope, parent_id, idx as i64);
}

/// The live-range side of the DOM's "remove" algorithm, for the MOVE that every `appendChild` /
/// `insertBefore` / `replaceChild` of an already-parented node performs before it inserts.
///
/// The DOM's insertion algorithms are "remove, then insert", and only the insert half was ever
/// reported: a range INSIDE the moved node was left pointing into a subtree that had changed
/// parents, and a range in the OLD parent past the moved node kept an offset one too large. WPT
/// measured both -- `Range-mutations-appendChild.html`, `-insertBefore.html` and `-replaceChild.html`
/// together declare 96 subtests of the shape `testDiv.appendChild(testDiv.lastChild)`, and the
/// failure text was "Wrong start container" / "Wrong end offset expected 6 but got 7".
///
/// Called with the OLD parent and the index the node occupied there, captured before the arena move.
fn notify_move_out(scope: &mut v8::PinScope, old_parent: u32, node: u32, idx: u32) {
    crate::core::dom::range::ranges_node_removed(scope, old_parent, node, idx as i64);
}

/// The nodes a single insertion CONNECTS -- or a single removal DISCONNECTS -- in shadow-including
/// tree order: `node` itself, then the contents of its shadow root if it hosts one, then its
/// children, recursively.
///
/// DOM §4.2.3 "insert" runs its per-node steps for *each shadow-including inclusive descendant* of
/// the inserted node, not for the inserted node alone -- which is the whole of [`notify_connected`]'s
/// defect below. The SHADOW HALF and its POSITION are measured, not read off the spec: Chrome 150,
/// one `host.appendChild(wrapper)` over a wrapper holding a shadow host with a custom element inside
/// its shadow root, one in its light children and one sibling after it, logs
/// `in-shadow`, `light-child-of-shadowhost`, `sibling-after` -- so a host's shadow tree is traversed
/// BEFORE its own children.
///
/// ONE WALK FOR BOTH HALVES, AND THAT IS MEASURED TOO. "remove" walks the same set in
/// the same order, and Chrome agrees on both the self-first rule and the shadow position: removing a
/// custom element that itself holds one logs `disc:top disc:inner`, and removing a wrapper holding a
/// shadow host logs `disc:in-shadow disc:light-child-of-shadowhost disc:sibling-after` -- the
/// disconnect order is the connect order, name for name.
///
/// The shadow root itself is a DocumentFragment and is never a custom element, so only its children
/// are walked; it is not pushed.
fn reaction_ids(dom: &Dom, node: NodeId, out: &mut Vec<NodeId>) {
    out.push(node);
    if let Some(root) = dom.shadow_root_of(node) {
        for c in dom.children(root) { reaction_ids(dom, c, out); }
    }
    for c in dom.children(node) { reaction_ids(dom, c, out); }
}

/// Notify the custom-element registry that the `added` subtrees are newly connected, when their new
/// parent (`parent_id`) is itself part of the main document tree (Dom::is_in_document). Calls
/// `custom_element_registry::connected(node)` for each node the insertion connected (that
/// was `KB.__customElements.connected`, in `host.js`); that registry decides whether the node's tag
/// is defined and its connectedCallback runs. Cheap no-op when nothing is registered (worker scope,
/// no DOM), the parent isn't connected, or `added` is empty.
///
/// THE RULE, AND THE DEFECT IT REPLACES. The reaction belongs to every node the
/// insertion CONNECTS -- `connect_reaction_ids` above -- and this ran it for the inserted node only,
/// so a custom element that arrived as a child, a grandchild, or through an `innerHTML` parse never
/// heard `connectedCallback` at all. Measured on Chrome 150 and korvus over the same fixture served
/// from `web-platform.test`, eight insertion shapes, korvus before this change in brackets:
///
/// ```text
///   host.appendChild(el)                       connected   [connected]
///   host.appendChild(div) with el inside       connected   [SILENT]
///   ... with el a grandchild                   connected   [SILENT]
///   host.appendChild(fragment) holding el      connected   [connected]
///   connectedParent.innerHTML = "<el>"         connected   [SILENT]
///   connectedParent.insertAdjacentHTML(...)    connected   [SILENT]
///   host.replaceChild(div-holding-el, old)     connected   [SILENT]
/// ```
///
/// The single-node shapes passed because `appendChild`/`insertBefore`/`replaceChild` splice a
/// fragment's children into `added` one by one, which happens to be the whole subtree when the
/// subtree is one node deep. That is why no fixture caught this: every fixture inserted one element.
///
/// SCRIPTS: EVERY SCRIPT THE INSERTION CONNECTS, descendants and shadow trees included -- Chrome runs
/// a `<script>` inside an appended `<div>` and one inside a shadow root of a host being connected.
/// What keeps `innerHTML` scripts inert is HTML's "already started" flag, which the fragment parser
/// sets ([`crate::core::html::html_script_element::mark_parsed_scripts`]); a walk restricted to the
/// top-level node used to stand in for that flag.
fn notify_connected(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, parent_id: u32, added: &[NodeId]) {
    if added.is_empty() { return; }
    // DOM's pre-insert adopts `node` into the PARENT's node document before it inserts it, and the
    // reaction that reports it is `adoptedCallback` -- so it runs here, ahead of the connect gate,
    // because a node adopted into a DETACHED element of another document is still adopted. The
    // parent's own document is the one it hands down; a parent in the page tree hands down `None`,
    // which is every node's default, so the whole same-document case costs one map lookup.
    let target = owner_doc(op_state, parent_id);
    notify_adopted(scope, op_state, target, added);
    // SHADOW-INCLUDING, not light-tree. This asked `is_in_document`, whose own doc
    // comment says it is deliberately false for shadow content -- so an insertion straight into an
    // ATTACHED shadow root connected nothing. Measured on Chrome 150 over the fixture,
    // `host.attachShadow({mode:"open"}).appendChild(el)` on a connected host logs `conn` then, on
    // `root.removeChild(el)`, `disc`; korvus logged neither. `Dom::is_connected` is the
    // shadow-including form `Node.isConnected` already answers with, and the two halves must share
    // one gate or the pairing this unit exists for cannot hold.
    //
    // CONNECTED MEANS IN ANY BROWSING CONTEXT'S DOCUMENT: `Dom::is_connected` is the
    // page's tree alone, so an insertion into a FRAME's document ran no inline script and no
    // `connectedCallback` -- a frame's `head.appendChild(s)` with `s.text` set did nothing, where
    // Chrome runs it (its external scripts ran only because the loader has its own gate).
    if !crate::core::html::subresource::connected_anywhere(scope, parent_id) { return; }
    // Every node list is taken BEFORE any reaction runs, because a `connectedCallback` is free to
    // mutate the tree and the reactions of one insertion are the ones that insertion enqueued. One
    // list per inserted node, so "is this the top-level node" is its index and not a search.
    // Only a script or a POSSIBLE custom element has anything to run here -- an autonomous one's
    // local name has a hyphen, a customized built-in carries `is` -- so only those are wrapped: an
    // `innerHTML` of 60,000 plain nodes minted 60,000 JS wrappers here for nothing.
    let subtrees: Vec<Vec<(NodeId, bool)>> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let is_script = |n: NodeId| dom.node_type(n) == 1 && dom.tag_name(n).is_some_and(|t| t.rsplit(':').next().unwrap_or(t).eq_ignore_ascii_case("script"));
        let maybe_custom = |n: NodeId| dom.node_type(n) == 1 && (dom.tag_name(n).is_some_and(|t| t.contains('-')) || dom.get_attribute(n, "is").is_some());
        added
            .iter()
            .map(|&n| {
                let mut v = Vec::new();
                reaction_ids(dom, n, &mut v);
                v.into_iter().filter_map(|id| {
                    let script = is_script(id);
                    (script || maybe_custom(id)).then_some((id, script))
                }).collect()
            })
            .collect()
    };
    for sub in &subtrees {
        for &(id, script) in sub.iter() {
            let node: v8::Local<v8::Value> = wrap_cached(scope, op_state, id).into();
            // Inserting a <script> into the document RUNS it. Nothing did that before: scripts
            // executed only if the parser found them at load, so `document.body.appendChild(s)` --
            // how every analytics snippet, every polyfill loader, and 464 subtests of
            // html/semantics/scripting-1 inject code -- silently did nothing.
            //
            // The whole of it is in `core/html/html_script_element.rs`: the type
            // test, the already-started flag and the top-level compile+run. It used to be a JS body
            // behind `KB.__runInsertedScript`, on the reasoning that reaching global scope needed
            // indirect `eval` -- which `core/html/subresource.rs` had already disproved for the
            // `<script src>` path with a plain `v8::Script::compile`.
            //
            // Every connected script -- see the doc comment.
            if script {
                crate::core::html::html_script_element::run_inserted_script(scope, node);
            }
            // UPGRADE BEFORE THE REACTION. An element the FRAGMENT PARSER created arrives wearing
            // its definition's prototype but with its constructor never run, so `connectedCallback`
            // would execute against a half-built instance -- see
            // `custom_element_registry::upgrade_one` for the measurement and for the page that
            // raised `TypeError: this.render is not a function` the moment the walk below started
            // reaching those elements. `upgrade_one` is one-shot and a no-op for an element already
            // upgraded (`createElement`, or `define`'s own pass), so this costs a private-slot read
            // on every inserted node and nothing else.
            if let Ok(el) = v8::Local::<v8::Object>::try_from(node) {
                crate::core::html::custom_element_registry::upgrade_one(scope, el);
            }
            crate::core::html::custom_element_registry::connected(scope, node);
        }
    }
}

/// The document a node BELONGS TO, as this engine records it: the `OwnerMap` override a non-main
/// document's factories wrote, or `None` for the page document, which is every node's default and
/// carries no entry. Nothing else in this arena knows about documents -- one arena holds them all --
/// so this map IS "node document" here, and moving a node between documents is moving its entry.
pub(crate) fn owner_doc(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<v8::Global<v8::Value>> {
    let st = op_state.borrow();
    let m = st.borrow::<OwnerMap>().0.borrow();
    m.get(&id).cloned()
}

/// The JS object for a recorded owner: the override, or the page `document` for `None`.
fn doc_value<'s>(scope: &mut v8::PinScope<'s, '_>, g: &Option<v8::Global<v8::Value>>) -> v8::Local<'s, v8::Value> {
    match g {
        Some(g) => v8::Local::new(scope, g),
        None => crate::binding::protos::global_get(scope, "document").unwrap_or_else(|| v8::null(scope).into()),
    }
}

fn same_doc(scope: &mut v8::PinScope, a: &Option<v8::Global<v8::Value>>, b: &Option<v8::Global<v8::Value>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => v8::Local::new(scope, x) == v8::Local::new(scope, y),
        _ => false,
    }
}

/// Make `node` and every shadow-including inclusive descendant of it `target`'s (`None`: the page's) --
/// what DOM's "clone a node" does with its `document` argument, with no adoption steps: the copy is
/// CREATED in that document (`importNode`), not moved into it.
pub(crate) fn set_node_document(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, target: Option<v8::Global<v8::Value>>, node: NodeId) {
    let ids: Vec<NodeId> = {
        let st = op_state.borrow();
        let mut v = Vec::new();
        reaction_ids(&st.borrow::<HostDom>().0, node, &mut v);
        v
    };
    let st = op_state.borrow();
    let mut m = st.borrow::<OwnerMap>().0.borrow_mut();
    for id in ids {
        match &target {
            Some(g) => {
                m.insert(id, g.clone());
            }
            None => {
                m.remove(&id);
            }
        }
    }
}

/// DOM's "adopt": hand `nodes` and every shadow-including inclusive descendant of each to `target`'s
/// document, and tell each custom element among them with `adoptedCallback(oldDocument, newDocument)`.
///
/// **WHAT THIS ENGINE'S "NODE DOCUMENT" IS.** One arena holds the page, every `<iframe>`'s document
/// and every `DOMParser` / `createHTMLDocument` result, and the only thing that distinguishes them
/// per node is the `OwnerMap` `ownerDocument` already answers from. So adoption is exactly: rewrite
/// that entry for the subtree, and fire the reaction where it changed. Before this, `adoptNode` was
/// documented as reducing to its removal -- `d2.adoptNode(el)` left `el.ownerDocument` pointing at
/// the page, measured, where Chrome 150 answers `d2`.
///
/// **THE ORDER IS MEASURED**, Chrome 150 over one fixture on `web-platform.test`:
///
/// ```text
///   d2.adoptNode(detachedEl)              adop
///   d2.adoptNode(connectedEl)             disc  adop
///   d2.body.appendChild(connectedEl)      disc  adop  conn
/// ```
///
/// so the adopt sits between the removal and the insertion, which is where DOM's "pre-insert" puts
/// it -- after the implied remove, before the insert. A node that is only a DESCENDANT of the
/// adopted node hears it too.
pub(crate) fn notify_adopted(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, target: Option<v8::Global<v8::Value>>, nodes: &[NodeId]) {
    if nodes.is_empty() { return; }
    for &n in nodes {
        let old = owner_doc(op_state, n);
        if same_doc(scope, &old, &target) { continue; }
        let ids: Vec<NodeId> = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            let mut v = Vec::new();
            reaction_ids(dom, n, &mut v);
            v
        };
        {
            let st = op_state.borrow();
            let mut m = st.borrow::<OwnerMap>().0.borrow_mut();
            for &id in &ids {
                match &target {
                    Some(g) => { m.insert(id, g.clone()); }
                    None => { m.remove(&id); }
                }
            }
        }
        // `adoptedCallback` exists only on a custom element: wrap just the possible ones.
        let ids: Vec<NodeId> = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            ids.into_iter()
                .filter(|&n| dom.node_type(n) == 1 && (dom.tag_name(n).is_some_and(|t| t.contains('-')) || dom.get_attribute(n, "is").is_some()))
                .collect()
        };
        for id in ids {
            let node: v8::Local<v8::Value> = wrap_cached(scope, op_state, id).into();
            let o = doc_value(scope, &old);
            let t = doc_value(scope, &target);
            crate::core::html::custom_element_registry::adopted(scope, node, o, t);
        }
    }
}

/// The DISCONNECT half of [`notify_connected`], and the whole of the first third: run
/// `disconnectedCallback` for every node a removal took out of a connected tree.
///
/// **WHERE IT IS CALLED FROM, AND WHY EXACTLY THERE.** `core/dom/shared.rs::notify_childlist_sib`,
/// with that call's `removed` list -- the one function every removal in this arena already funnels
/// through, as its own doc comment says of the insertion side. `removeChild`, `remove()`,
/// `replaceChild`, `replaceChildren`, `replaceWith`, `normalize`, the `innerHTML` / `outerHTML` /
/// `textContent` setters, the document-level removals and the remove half of every MOVE all report
/// there and nowhere else. A per-call-site copy is how the connect half grew into three sites
/// instead of one, and FP14 had to collapse them.
///
/// **THE GATE IS THE OLD PARENT'S CONNECTEDNESS, read after the unlink.** DOM's "remove" enqueues
/// the reaction only when the parent is connected, and the parent is exactly the node the removal
/// did not move -- so asking it after the fact is the same answer as asking it before. Measured:
/// removing a custom element from a parent that was never in the document logs nothing on Chrome
/// (and nothing here), while removing one from a connected parent logs `disc`.
///
/// **A MOVE IS A REMOVE FOLLOWED BY AN INSERT, and both halves fire.** `b.appendChild(e)` on an `e`
/// already under `a` logs `disc conn` on Chrome 150; so does `b.insertBefore`, so does
/// `host.appendChild(existingChild)` reordering within ONE parent, and so -- against a plain reading
/// of the spec's state-preserving move -- does `b.moveBefore(e, null)`. Every one of those reaches
/// this function because every one of them reports its removal through `notify_childlist_sib`.
pub(crate) fn notify_disconnected(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, removed: &[v8::Local<v8::Value>]) {
    if removed.is_empty() { return; }
    let Some((parent_id, _)) = mutation_recv(scope, parent) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !crate::core::html::subresource::connected_anywhere(scope, parent_id) { return; }
    let ids: Vec<NodeId> = removed.iter().filter_map(|&n| unwrap_id(scope, n)).collect();
    if ids.is_empty() { return; }
    // Taken before any reaction runs, for the reason [`notify_connected`] takes its lists early: a
    // `disconnectedCallback` is free to mutate the tree, and the reactions of one removal are the
    // ones that removal enqueued.
    // `disconnectedCallback` exists only on a custom element: wrap just the possible ones.
    let mut frame_owner = false;
    let subtrees: Vec<Vec<NodeId>> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let maybe_custom = |n: NodeId| dom.node_type(n) == 1 && (dom.tag_name(n).is_some_and(|t| t.contains('-')) || dom.get_attribute(n, "is").is_some());
        let owns_frame = |n: NodeId| dom.node_type(n) == 1 && dom.tag_name(n).is_some_and(|t| ["iframe", "frame", "object", "embed"].iter().any(|f| t.eq_ignore_ascii_case(f)));
        ids.iter()
            .map(|&n| {
                let mut v = Vec::new();
                reaction_ids(dom, n, &mut v);
                frame_owner |= v.iter().any(|&id| owns_frame(id));
                v.retain(|&id| maybe_custom(id));
                v
            })
            .collect()
    };
    for sub in &subtrees {
        for &id in sub {
            let node: v8::Local<v8::Value> = wrap_cached(scope, &op_state, id).into();
            crate::core::html::custom_element_registry::disconnected(scope, node);
        }
    }
    // A frame taken out of the document takes its IndexedDB connections with it, now.
    if frame_owner {
        crate::modules::indexeddb::idb_factory::idb_reap_detached(scope);
    }
}


/// Node.appendChild(child): native, with fragment handling, cycle check (throws HierarchyRequestError),
/// arena mutation, and the live-range + MutationObserver notifications. Returns the child.
/// The arena id a MUTATION method's receiver names: an element by its own id, a Document by its arena
/// ROOT. `unwrap_id` answers only for a `JsElement`, so before this every `appendChild` /`insertBefore` /
/// `removeChild` / `replaceChild` / `append` / `prepend` / `replaceChildren` called on a Document was a
/// SILENT no-op -- `foreignDoc.appendChild(comment)` left `parentNode` null and the node nowhere. The JS
/// facade W5.0e deleted forwarded those calls to the root (`05-xml.js`: `appendChild: node => root
/// .appendChild(node)`), and WPT `dom/nodes/Node-properties.html` measures it (`foreignDoc.childNodes`).
///
/// A Document parent is not an ordinary parent: `doc_insert_ok` enforces the DOM's pre-insertion
/// validity (no Text, no second element, one doctype, never a doctype after an element) that the facade
/// enforced as `docValidate`. Allowing the mutation WITHOUT it is worse than the no-op it replaced --
/// Next.js hydrates into `document` itself, so React inserted a text node a browser refuses and then
/// reported a hydration mismatch (a production site, React #418, measured).
///
/// The second half of the pair is whether the receiver was a DOCUMENT object. The flag cannot be
/// recovered from the id: a shadow root and a `<template>`'s content fragment reuse the arena's
/// document node kind (`09-postshells.js` registers `DocumentFragment.prototype` for nodeType 9), so
/// asking the arena "is this node a document?" answered yes for them and the Document insertion rules
/// refused a second element inside a shadow root -- github.com, measured.
pub(crate) fn mutation_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<(u32, bool)> {
    if let Some(id) = unwrap_id(scope, this) {
        return Some((id, false));
    }
    let slot = deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this).map(|p| unsafe { p.as_ref() }.root)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    Some((crate::core::dom::document::resolve_doc_root(&st.borrow::<HostDom>().0, slot), true))
}

/// DOM "pre-insertion validity" for a DOCUMENT parent (WHATWG DOM 4.2.1, steps 2 and 6), the half a
/// browser refuses: a Text child, a second element child, a second doctype, a doctype after an element,
/// or a fragment carrying text / more than one element. `parent` is the document's arena ROOT and
/// `dropping` is the child that is being replaced or removed in the same operation (its element or
/// doctype no longer counts), `None` for a plain insertion. Returns false when the caller must throw
/// HierarchyRequestError.
fn doc_insert_ok(dom: &Dom, parent: NodeId, node: NodeId, dropping: Option<NodeId>) -> bool {
    let (mut els, mut doctypes) = (0usize, 0usize);
    for k in dom.children(parent) {
        if Some(k) == dropping { continue; }
        match dom.node_type(k) { 1 => els += 1, 10 => doctypes += 1, _ => {} }
    }
    match dom.node_type(node) {
        3 | 9 => false,
        10 => doctypes == 0 && els == 0,
        11 => {
            let kids = dom.children(node);
            let text = kids.iter().any(|&c| dom.node_type(c) == 3);
            let fe = kids.iter().filter(|&&c| dom.node_type(c) == 1).count();
            !text && fe + els <= 1
        }
        1 => els == 0,
        _ => true,
    }
}

/// [`doc_insert_ok`] for a variadic insert: `pending` are the ids this same call has already accepted
/// (they count toward the one-element rule), and `fresh` says the document's existing children are being
/// replaced wholesale (`replaceChildren`), so only `pending` counts.
fn doc_insert_ok_batch(
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
    parent: u32,
    node: u32,
    pending: &[u32],
    fresh: bool,
) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let (mut els, mut doctypes) = if fresh {
        (0usize, 0usize)
    } else {
        let mut e = 0; let mut d = 0;
        for k in dom.children(parent) { match dom.node_type(k) { 1 => e += 1, 10 => d += 1, _ => {} } }
        (e, d)
    };
    for &k in pending { match dom.node_type(k) { 1 => els += 1, 10 => doctypes += 1, _ => {} } }
    match dom.node_type(node) {
        3 | 9 => false,
        10 => doctypes == 0 && els == 0,
        11 => {
            let kids = dom.children(node);
            !kids.iter().any(|&c| dom.node_type(c) == 3)
                && kids.iter().filter(|&&c| dom.node_type(c) == 1).count() + els <= 1
        }
        1 => els == 0,
        _ => true,
    }
}

/// WHATWG DOM §4.2.3 "ensure pre-insertion validity", as ONE function the three mutators share.
///
/// WHY IT IS ONE FUNCTION. The spec has one algorithm and `insertBefore`, `appendChild` and
/// `replaceChild` all invoke it. Here the pieces were scattered into three different subsets, so
/// which rule you got depended on which method you called (CONF NODE1.3, measured 2026-09-22):
///
/// ```text
/// a.appendChild(d)            d is a's ancestor   -> THREW    (inline check, appendChild only)
/// a.insertBefore(d, null)     d is a's ancestor   -> no throw
/// d.appendChild(document)     a Document as child -> no throw
/// d.replaceChild(document,a)  a Document as child -> no throw
/// d.insertBefore(new, other)  ref is not a child  -> no throw (spec: NotFoundError)
/// ```
///
/// `doc_insert_ok` existed but is only consulted when the PARENT is a document, which is why
/// putting a Document inside a `<div>` was never refused by anything. `insertion_cycles` existed
/// too, and the variadic callbacks (`append`/`prepend`/`before`/`after`/`replaceWith`) called it
/// while `insertBefore` and `replaceChild` did not.
///
/// THE RULES IMPLEMENTED HERE, and each is one the reproduction above showed missing:
///   * step 2  -- `node` is a host-including inclusive ancestor of `parent` -> HierarchyRequestError
///   * step 3  -- `child` is not a child of `parent`                        -> NotFoundError
///   * step 4  -- `node` is not a Fragment/Doctype/Element/CharacterData    -> HierarchyRequestError
///                (a Document, node type 9, is the case sites actually hit)
///
/// Step 6's Document-parent counting stays where it is (`doc_insert_ok`), with the scope limit its
/// own comment already records: korvus has no distinct DocumentFragment node kind, so a fragment is
/// an Element and those cases are not modelled.
///
/// Returns `true` WHEN IT HAS THROWN, so a caller reads `if pre_insert_invalid(..) { return; }`.
fn pre_insert_invalid(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
    parent: u32,
    node: u32,
    child: Option<u32>,
    method: &str,
) -> bool {
    let (is_ancestor, node_type, child_is_kid) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (
            node == parent || dom.host_including_contains(node, parent),
            // `node_type_of`, NOT `dom.node_type`. The page document's id is the SENTINEL
            // `ON_DOCUMENT_ID` (u32::MAX), which the arena has never heard of -- `dom.node_type`
            // on it does not answer 9, so the type rule below never fired and
            // `d.appendChild(document)` stayed a silent no-op even after the check was added.
            // `shared.rs:270` exists to translate exactly that sentinel, the same way `norm_doc`
            // does for the other direction.
            node_type_of(dom, node),
            child.map(|c| dom.children(parent).contains(&c)),
        )
    };
    if is_ancestor {
        throw_dom(
            scope,
            &format!("Failed to execute '{method}' on 'Node': The new child element contains the parent."),
            "HierarchyRequestError",
        );
        return true;
    }
    if child_is_kid == Some(false) {
        throw_dom(
            scope,
            &format!("Failed to execute '{method}' on 'Node': The node before which the new node is to be inserted is not a child of this node."),
            "NotFoundError",
        );
        return true;
    }
    // Fragment(11) / Doctype(10) / Element(1) / Text(3) / CDATA(4) / PI(7) / Comment(8) may be
    // inserted. A Document (9) may not be a child of anything, which is the one sites reach.
    if !matches!(node_type, 1 | 3 | 4 | 7 | 8 | 10 | 11) {
        throw_dom(
            scope,
            &format!("Failed to execute '{method}' on 'Node': Nodes of type '#document' may not be inserted inside nodes of this type."),
            "HierarchyRequestError",
        );
        return true;
    }
    false
}

/// A REQUIRED `Node` argument. Every mutator here declares one -- `appendChild(Node node)`,
/// `insertBefore(Node node, Node? child)`, `replaceChild(Node node, Node child)`,
/// `removeChild(Node child)` -- so WebIDL converts before the body runs and `null`, `undefined`
/// and any non-Node are a plain `TypeError`.
///
/// THE THIRD APPEARANCE OF ONE STRUCTURAL DEFECT (CONF NODE1.4, 2026-09-22). A resolution helper
/// answers `None` and the caller treats it as "do nothing", so the method returns its own argument
/// and the page is told nothing. Measured here before the change:
///
/// ```text
/// d.insertBefore(null, null)   -> no throw     d.appendChild("x")      -> no throw
/// d.insertBefore(new, 42)      -> no throw     d.replaceChild(null, a) -> no throw
/// d.removeChild(null)          -> no throw     d.replaceChild(new,null)-> no throw
/// ```
///
/// RANGE1.2 closed the same thing for `Range`'s seven methods; `arg_node_id`'s two-case resolution
/// is shared by every DOM method that takes a node, so this is where it shows up next.
///
/// AN ATTR IS NOT A TYPEERROR, for the reason RANGE1.1 recorded: an `Attr` IS a `Node` in the IDL
/// and korvus models it as a plain object carrying its OWNER's arena id, with no id of its own. It
/// keeps today's behaviour -- the caller's `None` branch -- and stays open by name rather than
/// being converted into a spec violation that merely looks tidier.
fn required_node_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, method: &str) -> Option<u32> {
    if let Some(id) = arg_node_id(scope, v) {
        return Some(id);
    }
    let is_attr = crate::core::dom::attr::is_attr(scope, v);
    if !is_attr {
        throw_type(scope, &format!(
            "Failed to execute '{method}' on 'Node': parameter 1 is not of type 'Node'."));
    }
    None
}

/// A plain `TypeError`, the WebIDL conversion failure. `throw_dom` cannot express one. Blink's context is
/// applied after the error is made (`protos::throw_type_error`).
fn throw_type(scope: &mut v8::PinScope, msg: &str) {
    crate::binding::protos::throw_type_error(scope, msg);
}

pub(crate) fn cb_append_child(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "appendChild") { return; }
    let this = args.this();
    let (this_id, this_is_doc) = match mutation_recv(scope, this.into()) { Some(r) => r, None => return };
    let child = args.get(0);
    // INLINE `try_unwrap_cppgc_object::<JsElement>`, which is `unwrap_id`'s body written out here --
    // so it resolved an Element and nothing else, a Document answered None, and this handed the
    // argument back UNINSERTED and UNREFUSED. `arg_node_id` resolves a Document as well, which is
    // what lets `pre_insert_invalid` below see node type 9 and throw the way the spec says.
    let child_id = match required_node_arg(scope, child, "appendChild") { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A Document parent refuses what a browser refuses (see `doc_insert_ok`).
    {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if this_is_doc && !doc_insert_ok(dom, this_id, child_id, None) {
            drop(st);
            throw_dom(scope, "Failed to execute 'appendChild' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
            return;
        }
    }
    let is_frag = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.node_type(child_id) == 11 || dom.node_name(child_id).eq_ignore_ascii_case("#fragment")
    };
    let parent_v: v8::Local<v8::Value> = this.into();
    if is_frag {
        // Fragment: move its children in (matching the glue, which does NOT range-notify this path).
        let kids: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(child_id).to_vec();
        for &c in &kids {
            op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(this_id, c);
        }
        // childList record: the fragment's former children are the addedNodes (the fragment itself is not).
        let mut wrapped: Vec<v8::Local<v8::Value>> = Vec::with_capacity(kids.len());
        for &c in &kids { wrapped.push(wrap_cached(scope, &op_state, c).into()); }
        notify_childlist(scope, parent_v, &wrapped, &[]);
        // The fragment lost its children -> notify an observer on the fragment of the removal.
        if !wrapped.is_empty() { let frag_v = wrap_cached(scope, &op_state, child_id); notify_childlist(scope, frag_v.into(), &[], &wrapped); }
        notify_connected(scope, &op_state, this_id, &kids);
        rv.set(child);
        return;
    }
    // Was an inline ancestor check that no other mutator shared; now the same function they all use.
    if pre_insert_invalid(scope, &op_state, this_id, child_id, None, "appendChild") { return; }
    // A node already in the tree is MOVED: queue a removal record (old parent + old flanking siblings)
    // FIRST, then the insertion record. Capture the old context before the mutation.
    let (old_parent, old_idx, old_prev, old_next) = old_place(&op_state, child_id);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(this_id, child_id);
    let idx = index_if_ranges(scope, &op_state, this_id, child_id);
    // Live ranges, in the spec's order: the MOVE's remove half (which the insert half below used to
    // be the only report of), then the insert.
    if let Some(op) = old_parent { let n = norm_parent(&op_state, op); notify_move_out(scope, n, child_id, old_idx); }
    notify_insert(scope, norm_parent(&op_state, this_id), idx);
    // The records' siblings go by id: wrapped only if an observer records them (`notify_childlist_sib_ids`).
    if let Some(op) = old_parent {
        let opv = wrap_cached(scope, &op_state, op);
        notify_childlist_sib_ids(scope, opv.into(), &[], &[child], old_prev, old_next);
    }
    // appended at the end: previousSibling = the former last child, nextSibling = null.
    let prev = { let st = op_state.borrow(); st.borrow::<HostDom>().0.previous_sibling(child_id) };
    notify_childlist_sib_ids(scope, parent_v, &[child], &[], prev, None);
    notify_connected(scope, &op_state, this_id, &[child_id]);
    rv.set(child);
}

fn is_fragment(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.node_type(id) == 11
        || dom.node_name(id).eq_ignore_ascii_case("#fragment")
        || dom.node_name(id).eq_ignore_ascii_case("#document-fragment")
}

/// Flatten a DocumentFragment argument into its children, in order, for the variadic insertion ops
/// (`before`/`after`/`replaceWith`/`prepend`/`append`). A fragment is never itself inserted -- HTML's
/// "insert" moves its children in and leaves it empty -- and `Node.appendChild`/`insertBefore`
/// already did this; these five did not, so `el.before(frag)` planted the fragment NODE in the tree
/// (a `#document-fragment` where a browser has its children). A non-fragment id passes through. The
/// children are snapshotted here; the caller relinks each, which unlinks it from the fragment.
fn expand_fragments(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, ids: &[u32]) -> Vec<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let frag = dom.node_type(id) == 11
            || dom.node_name(id).eq_ignore_ascii_case("#fragment")
            || dom.node_name(id).eq_ignore_ascii_case("#document-fragment");
        if frag { out.extend(dom.children(id).iter().copied()); } else { out.push(id); }
    }
    out
}
/// The live-range side of the DOM's "remove" algorithm, in Rust like its two siblings.
fn notify_remove(scope: &mut v8::PinScope, parent_id: u32, child_id: u32, idx: u32) {
    crate::core::dom::range::ranges_node_removed(scope, parent_id, child_id, idx as i64);
}

/// A mutation receiver's arena id under the convention every boundary reader uses: the page document
/// is `ON_DOCUMENT_ID`, not the arena root. `mutation_recv` hands back the raw root for a `Document`,
/// and a live range whose container IS the document reads as `ON_DOCUMENT_ID` -- so without this the
/// two never compare equal and a range on `document` saw no mutation at all.
fn norm_parent(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> u32 {
    let st = op_state.borrow();
    crate::core::dom::shared::norm_doc(&st.borrow::<HostDom>().0, id)
}

/// Where `node` sits right now: its (raw) parent, its index under it, and its flanking siblings.
/// Read BEFORE a move, because the move is what changes all four.
fn old_place(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, node: u32) -> (Option<u32>, u32, Option<u32>, Option<u32>) {
    // The old index only feeds live-range updates; `LiveRanges` empty means nobody reads it (see
    // `range::live_ranges_exist`), and walking the old parent's children made every MOVE O(n).
    let ranges = op_state.borrow().try_borrow::<crate::core::dom::range::LiveRanges>().is_some_and(|r| r.has_any());
    let st = op_state.borrow();
    let d = &st.borrow::<HostDom>().0;
    match d.parent(node) {
        Some(p) => (
            Some(p),
            if ranges { d.children(p).iter().position(|&c| c == node).unwrap_or(0) as u32 } else { 0 },
            d.previous_sibling(node),
            d.next_sibling(node),
        ),
        None => (None, 0, None, None),
    }
}

/// `child`'s index under `parent`, read from the arena as it stands.
/// `child`'s index under `parent`, computed only when a live Range needs it (see `live_ranges_exist`).
fn index_if_ranges(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, parent: u32, child: u32) -> u32 {
    if crate::core::dom::range::live_ranges_exist(scope) { child_index(op_state, parent, child) } else { 0 }
}

fn child_index(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, parent: u32, child: u32) -> u32 {
    let st = op_state.borrow();
    st.borrow::<HostDom>().0.children(parent).iter().position(|&c| c == child).unwrap_or(0) as u32
}

pub(crate) fn cb_insert_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Node", "insertBefore") { return; }
    let this = args.this();
    let (this_id, this_is_doc) = match mutation_recv(scope, this.into()) { Some(r) => r, None => return };
    let new_node = args.get(0);
    let new_id = match required_node_arg(scope, new_node, "insertBefore") { Some(i) => i, None => return };
    // `child` IS nullable (`insertBefore(node, null)` appends), so `null`/`undefined` pass; anything
    // else that is not a Node is still a TypeError.
    let ref_arg = args.get(1);
    let ref_id = if ref_arg.is_null_or_undefined() { None } else {
        match required_node_arg(scope, ref_arg, "insertBefore") { Some(i) => Some(i), None => return }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if this_is_doc && !doc_insert_ok(dom, this_id, new_id, None) {
            drop(st);
            throw_dom(scope, "Failed to execute 'insertBefore' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
            return;
        }
    }
    if pre_insert_invalid(scope, &op_state, this_id, new_id, ref_id, "insertBefore") { return; }
    insert_with_steps(scope, &op_state, this_id, this.into(), new_id, new_node, ref_id);
    rv.set(new_node);
}

/// DOM "insert" after pre-insert validation: the tree edit, live-range updates, MutationObserver
/// records (a move queues the old parent's removal first), and the insertion steps of every node it
/// connects -- scripts, iframes, stylesheets, custom elements. `insertBefore`, `appendChild`'s
/// reference-free twin and every `insertAdjacent*` end here, so none of them can skip a step: an
/// `insertAdjacentElement`'d module script used to be inserted and never run (11st.co.kr).
fn insert_with_steps(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    parent: u32,
    parent_v: v8::Local<v8::Value>,
    new_id: u32,
    new_node: v8::Local<v8::Value>,
    before: Option<u32>,
) {
    let reference = before.unwrap_or(NodeId::MAX);
    if is_fragment(op_state, new_id) {
        let kids: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(new_id).to_vec();
        for &c in &kids { op_state.borrow_mut().borrow_mut::<HostDom>().0.insert_before(parent, c, reference); }
        let mut wrapped: Vec<v8::Local<v8::Value>> = Vec::with_capacity(kids.len());
        for &c in &kids { wrapped.push(wrap_cached(scope, op_state, c).into()); }
        notify_childlist(scope, parent_v, &wrapped, &[]);
        // The fragment lost its children -> notify an observer on the fragment of the removal.
        if !wrapped.is_empty() { let frag_v = wrap_cached(scope, op_state, new_id); notify_childlist(scope, frag_v.into(), &[], &wrapped); }
        notify_connected(scope, op_state, parent, &kids);
        return;
    }
    // A node already in the tree is MOVED: per spec that queues a removal record (old parent + old
    // flanking siblings) FIRST, then an insertion record (new parent + new siblings) -- even for a move
    // within the same parent (2 records). Capture the old context before the mutation.
    let (old_parent, old_idx, old_prev, old_next) = old_place(op_state, new_id);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.insert_before(parent, new_id, reference);
    // The insert index is read AFTER the mutation, from the node's own position. Reading the
    // reference child's index BEFORE it (what this did) is a different number whenever the moved
    // node was already an earlier sibling of the reference: the removal half shifts everything after
    // it left by one, and the spec's insert index is the one in the tree the removal left behind.
    let idx = index_if_ranges(scope, op_state, parent, new_id);
    if let Some(op) = old_parent { let n = norm_parent(op_state, op); notify_move_out(scope, n, new_id, old_idx); }
    notify_insert(scope, norm_parent(op_state, parent), idx);
    if let Some(op) = old_parent {
        let opv = wrap_cached(scope, op_state, op);
        notify_childlist_sib_ids(scope, opv.into(), &[], &[new_node], old_prev, old_next);
    }
    // previousSibling/nextSibling flank the inserted node (derived from the tree post-insert).
    let (psib, nsib) = { let st = op_state.borrow(); let d = &st.borrow::<HostDom>().0; (d.previous_sibling(new_id), d.next_sibling(new_id)) };
    notify_childlist_sib_ids(scope, parent_v, &[new_node], &[], psib, nsib);
    notify_connected(scope, op_state, parent, &[new_id]);
}

pub(crate) fn cb_remove_child(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Node", "removeChild") { return; }
    let this = args.this();
    let (this_id, _this_is_doc) = match mutation_recv(scope, this.into()) { Some(r) => r, None => return };
    let child = args.get(0);
    // (An earlier revision left an `appendChild` comment sitting here; the code was right and the
    // comment described a different function. Replaced with this one's own truth.)
    let child_id = match required_node_arg(scope, child, "removeChild") { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // DOM §4.2.3 "pre-remove": if `child`'s parent is not `this`, throw NotFoundError. It is the
    // SAME RULE `pre_insert_invalid` applies to `insertBefore`'s reference child (NODE1.3), and
    // `removeChild` has its own path so it never reached it. Measured before this, with a control:
    //
    //   d.removeChild(a)   a IS a child      -> removed, d.children 1 -> 0   CONTROL
    //   d.removeChild(s)   s is body's child -> no throw, s stayed in BODY
    //   d.removeChild(document.createElement('i'))  detached  -> no throw
    //   document.body.removeChild(createTextNode) detached    -> no throw
    //   (element with no children).removeChild(s)             -> no throw
    //
    // Nothing was corrupted -- the node simply was not removed and the caller was not told. That is
    // this goal's recurring shape: the method is called, returns as if it worked, and does nothing.
    {
        let st = op_state.borrow();
        let is_child = st.borrow::<HostDom>().0.children(this_id).contains(&child_id);
        drop(st);
        if !is_child {
            throw_dom(
                scope,
                "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
                "NotFoundError",
            );
            return;
        }
    }
    // Capture index + flanking siblings BEFORE removal (previousSibling/nextSibling of the record).
    let (idx, psib, nsib): (Option<u32>, Option<u32>, Option<u32>) = { let st = op_state.borrow(); let d = &st.borrow::<HostDom>().0;
        (d.children(this_id).iter().position(|&x| x == child_id).map(|i| i as u32), d.previous_sibling(child_id), d.next_sibling(child_id)) };
    op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(this_id, child_id);
    if let Some(i) = idx {
        notify_remove(scope, norm_parent(&op_state, this_id), child_id, i);
        notify_childlist_sib_ids(scope, this.into(), &[], &[child], psib, nsib);
    }
    rv.set(child);
}

pub(crate) fn cb_replace_child(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Node", "replaceChild") { return; }
    let this = args.this();
    let (this_id, this_is_doc) = match mutation_recv(scope, this.into()) { Some(r) => r, None => return };
    let new_id = match required_node_arg(scope, args.get(0), "replaceChild") { Some(i) => i, None => return };
    let old = args.get(1);
    let new_node = args.get(0);
    let old_id = match required_node_arg(scope, old, "replaceChild") { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if this_is_doc && !doc_insert_ok(dom, this_id, new_id, Some(old_id)) {
            drop(st);
            throw_dom(scope, "Failed to execute 'replaceChild' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
            return;
        }
    }
    if pre_insert_invalid(scope, &op_state, this_id, new_id, Some(old_id), "replaceChild") { return; }
    // A DocumentFragment is REPLACED BY ITS CHILDREN (DOM §4.2.3 "replace", node a fragment): the
    // fragment is emptied and its children take `old_node`'s place, as ONE childList mutation whose
    // addedNodes are those children and removedNodes is `old_node`. `replace_child` below moves a
    // single node, so without this `parent.replaceChild(templateContent, old)` -- how creepjs's whole
    // UI (and every `patch()` of an `html` template) renders -- planted the fragment NODE in the tree
    // (`isConnected` true, `parentElement` null, zero layout) instead of its content; the page then
    // showed an empty body. appendChild/insertBefore already flatten a fragment ([`insert_with_steps`]);
    // replaceChild was the one insertion path that did not.
    if is_fragment(&op_state, new_id) {
        let kids: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(new_id).to_vec();
        // Flanking siblings of the replacement point, captured pre-mutation: they end up flanking the
        // inserted run once `old_node` is gone.
        let (psib, nsib) = { let st = op_state.borrow(); let d = &st.borrow::<HostDom>().0; (d.previous_sibling(old_id), d.next_sibling(old_id)) };
        let old_idx = { let st = op_state.borrow(); st.borrow::<HostDom>().0.children(this_id).iter().position(|&x| x == old_id).map(|i| i as u32) };
        // Insert each child before `old_node`, then remove `old_node`: the children sit where it was.
        for &c in &kids { op_state.borrow_mut().borrow_mut::<HostDom>().0.insert_before(this_id, c, old_id); }
        op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(this_id, old_id);
        let here = norm_parent(&op_state, this_id);
        if let Some(i) = old_idx { notify_remove(scope, here, old_id, i); }
        for &c in &kids { let ins = index_if_ranges(scope, &op_state, this_id, c); notify_insert(scope, here, ins); }
        let mut wrapped: Vec<v8::Local<v8::Value>> = Vec::with_capacity(kids.len());
        for &c in &kids { wrapped.push(wrap_cached(scope, &op_state, c).into()); }
        notify_childlist_sib_ids(scope, this.into(), &wrapped, &[old], psib, nsib);
        // The fragment lost its children -> an observer on the fragment sees the removal.
        if !wrapped.is_empty() { let frag_v = wrap_cached(scope, &op_state, new_id); notify_childlist(scope, frag_v.into(), &[], &wrapped); }
        notify_connected(scope, &op_state, this_id, &kids);
        rv.set(old);
        return;
    }
    // If the replacement was already in the tree it is moved -- capture its old parent so that parent's
    // observer sees the removal (WPT "replaceChild: removal mutation").
    let new_old_parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(new_id).filter(|&p| p != this_id) };
    // The three live-range effects of DOM §4.2.3 "replace", in its order and with ITS indices --
    // `replaceChild` reported NONE of them before, so every range touching either node went stale.
    // Step 3 removes `node` from wherever it was; step 4 removes `child` from this parent, and its
    // index is the one AFTER step 3 (which is why the pre-state child list is replayed rather than
    // read); step 5 inserts `node` at the position the final tree shows.
    let (rep_old_parent, rep_old_idx, _, _) = old_place(&op_state, new_id);
    let old_idx = {
        let st = op_state.borrow();
        let mut c: Vec<u32> = st.borrow::<HostDom>().0.children(this_id).to_vec();
        if rep_old_parent == Some(this_id) { c.retain(|&x| x != new_id); }
        c.iter().position(|&x| x == old_id).map(|i| i as u32)
    };
    op_state.borrow_mut().borrow_mut::<HostDom>().0.replace_child(this_id, new_id, old_id);
    {
        let here = norm_parent(&op_state, this_id);
        if let Some(op) = rep_old_parent { let n = norm_parent(&op_state, op); notify_move_out(scope, n, new_id, rep_old_idx); }
        if let Some(i) = old_idx { notify_remove(scope, here, old_id, i); }
        let ins = index_if_ranges(scope, &op_state, this_id, new_id);
        notify_insert(scope, here, ins);
    }
    // The record's siblings flank the replacement point (the new node's siblings post-replace).
    let (psib, nsib) = { let st = op_state.borrow(); let d = &st.borrow::<HostDom>().0; (d.previous_sibling(new_id), d.next_sibling(new_id)) };
    // Wrapped NOW when the moved node's removal is reported first: that report runs its disconnect reactions --
    // page script -- before this record, and the record's siblings are the wrappers they were here. Otherwise
    // they go by id, wrapped only if an observer records them.
    let sibs = new_old_parent.is_some().then(|| {
        let prev = psib.map(|s| { let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, s).into(); w });
        let next = nsib.map(|s| { let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, s).into(); w });
        (prev, next)
    });
    // THE MOVED NODE'S REMOVAL COMES FIRST. DOM "replace" removes `node` from
    // wherever it was before it removes `child` from this parent, and Chrome's records say so --
    // one observer on each parent, `b.replaceChild(moved, old)` where `moved` was under `a`:
    //
    // ```text
    //   chrome   a:+0-1, b:+1-1
    //   korvus   b:+1-1, a:+0-1    <- before this
    // ```
    //
    // The order matters twice over now that a removal carries a reaction: it is what makes
    // `disc:moved` precede `disc:old`.
    if let Some(op) = new_old_parent { let opv = wrap_cached(scope, &op_state, op); notify_childlist(scope, opv.into(), &[], &[new_node]); }
    match sibs {
        Some((prev, next)) => notify_childlist_sib(scope, this.into(), &[new_node], &[old], prev, next),
        None => notify_childlist_sib_ids(scope, this.into(), &[new_node], &[old], psib, nsib),
    }
    notify_connected(scope, &op_state, this_id, &[new_id]);
    rv.set(old);
}

/// One argument of a `(Node or DOMString or TrustedScript)...` method, as the binding converted it.
pub(crate) enum NodeArg {
    Node(u32),
    Text(String),
}

/// The binding's conversion of every argument of a `(Node or DOMString or TrustedScript)...` method, in order and
/// before any of the method's steps: a Node keeps its identity; a TrustedScript is its own script, nothing on it
/// called (`trusted_types::tables::sink_string`); anything else -- string, null, undefined, number -- is WebIDL's
/// ToString, so `el.append(null)` is a "null" Text node (WPT ParentNode-append/prepend, ChildNode-before/after/
/// replaceWith). A `toString` may run, and throw: `None`, and no step runs. Chrome 150 converts them all first --
/// `before()` on a parentless node still stringifies its arguments, and a `replaceChildren()` whose argument
/// throws keeps its children (this converted inside each method's loop, after its checks).
pub(crate) fn node_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<Vec<NodeArg>> {
    let mut out = Vec::with_capacity(args.length().max(0) as usize);
    for i in 0..args.length() {
        let v = args.get(i);
        if v.is_object() {
            if let Some(id) = unwrap_id(scope, v) {
                out.push(NodeArg::Node(id));
                continue;
            }
        }
        out.push(NodeArg::Text(crate::core::trusted_types::tables::sink_text(scope, v, "TrustedScript")?));
    }
    Some(out)
}

/// DOM's "convert nodes into a node", for one converted argument: a string is a new Text node whose node document
/// is `of`'s -- made by the arena. This called the page's `document.createTextNode`, read off the page's
/// `document`: a page that replaced it saw every `append("x")`, and a node in a template's contents or a parsed
/// document got a Text node of the page's document.
pub(crate) fn node_arg_id(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, a: &NodeArg, of: u32) -> u32 {
    let text = match a {
        NodeArg::Node(id) => return *id,
        NodeArg::Text(t) => t,
    };
    let doc = match node_document(scope, op_state, of) {
        // `of` is the document itself
        Some(d) if d.is_null() => Some(wrap_cached(scope, op_state, of).into()),
        d => d,
    };
    let id = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_text_node(text);
    if let Some(d) = doc {
        crate::core::dom::document::own_node(scope, d, id);
    }
    id
}

/// True when inserting `new_id` under `parent` would make the tree cyclic (`new_id` is an inclusive
/// ancestor of `parent`) — the WHATWG "host-including inclusive ancestor" hierarchy error. The arena
/// mutators refuse such an insertion unconditionally (keeping the tree acyclic so parent-chain walks
/// like root_of() can never hang); the variadic insertion callbacks below call this to *also* throw
/// HierarchyRequestError, so `assert_throws_dom` sees the spec-mandated exception (matching the guard
/// JsElement.appendChild already applies). Checked before the first mutation; the WPT hierarchy tests
/// pass the offending node as the sole argument, so throwing on it precedes any arena change.
pub(crate) fn insertion_cycles(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, parent: u32, new_id: u32) -> bool {
    op_state.borrow().borrow::<HostDom>().0.host_including_contains(new_id, parent)
}

/// WHATWG "ensure pre-insertion validity" TYPE rules (the ancestor-cycle case is `insertion_cycles`):
/// the node must be a DocumentFragment/DocumentType/Element/Text/(CDATA)/PI/Comment; a Text node may not
/// go into a Document; a Doctype may not go into a non-Document. Callers throw HierarchyRequestError.
/// (WPT `preInsertionValidateHierarchy`, shared by append/prepend/before/after/replaceWith.) The fuller
/// Document-child-count rules (spec step 6) are not modeled -- Korvus has no real DocumentFragment kind
/// (a fragment is a `#fragment` Element, node_type 1), so those Document-parent cases stay out of scope.
/// `parent_is_doc` is the caller's, not the arena's: a shadow root and a `<template>`'s content reuse
/// the document node kind, so `dom.node_type(parent) == 9` calls them documents and this refused a text
/// node inside a shadow root (`sr.append(el, "text")` -> HierarchyRequestError, where Chrome appends).
/// A receiver-driven caller passes what it unwrapped; a caller whose parent comes from the arena passes
/// `dom.node_type(parent) == 9 && !dom.is_shadow_root(parent)`.
/// Is an arena node a DOCUMENT for insertion purposes? A shadow root shares the kind and is not one.
/// (A `<template>`'s content fragment shares it too; it is reached through a receiver, never here.)
pub(crate) fn arena_parent_is_doc(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, parent: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.node_type(parent) == 9 && !dom.is_shadow_root(parent)
}

pub(crate) fn insertion_type_invalid(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, parent: u32, new_id: u32, parent_is_doc: bool) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let nt = dom.node_type(new_id);
    let _ = parent;
    if !matches!(nt, 1 | 3 | 4 | 7 | 8 | 10 | 11) { return true; } // step 4: not an insertable node kind (e.g. Document, Attr)
    if nt == 3 && parent_is_doc { return true; } // step 5: a Text node into a Document
    if nt == 10 && !parent_is_doc { return true; } // step 5: a Doctype into a non-Document
    false
}

/// After a variadic insert (append/prepend/before/after/replaceWith/innerHTML), notify a childList
/// MutationObserver of the batch on `parent` (`added_ids` inserted, `removed_ids` removed), **and run
/// the connect reactions the insertion owes** ([`notify_connected`]). These ops
/// mutate the arena directly (unlike Node.appendChild, whose JS wrapper already notifies), so without
/// this an observer of the parent never saw them and the async WPT MO tests hung. The caller must hold
/// no OpState borrow. One record per call (the whole batch), matching the spec's single childList record.
///
/// THE CONNECT HALF IS NEW AND IT IS HERE ON PURPOSE. Before it, the DOM's insertion
/// steps ran their custom-element reaction from exactly three methods -- `appendChild`,
/// `insertBefore`, `replaceChild` -- so `append`, `prepend`, `replaceChildren`, `before`, `after`,
/// `replaceWith` and the `innerHTML` setter connected elements the registry never heard about. The
/// reaction belongs to the INSERTION, not to the method that performed it, so it goes at the one
/// point every batch insertion already reports through; a per-call-site copy is how the three grew
/// into three instead of one. The MutationObserver record is queued, not delivered, so running the
/// synchronous reactions after it is the spec's order and not a race.
///
/// `moveBefore` REACHES THIS TOO, AND THAT IS MEASURED, NOT ASSUMED. The spec's state-preserving
/// move runs the moving steps rather than the removing and inserting ones, which reads as "no
/// custom-element reactions". Chrome 150 disagrees with that reading: over a fixture on
/// `web-platform.test`, `b.moveBefore(el, null)` logs `disconnectedCallback` then
/// `connectedCallback`, for the moved element AND for a custom element that is only a DESCENDANT of
/// it. So korvus firing the connect half here is Chrome's behaviour; the `disconnectedCallback` it
/// still owes is the separate hole (this engine implements none of the disconnect, adopt or
/// attribute-changed reactions), not a reason to special-case the move.
pub(crate) fn notify_childlist_batch(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, parent: u32, added_ids: &[u32], removed_ids: &[u32]) {
    if added_ids.is_empty() && removed_ids.is_empty() { return; }
    let parent_v = wrap_cached(scope, op_state, parent);
    let added: Vec<v8::Local<v8::Value>> = added_ids.iter().map(|&c| wrap_cached(scope, op_state, c).into()).collect();
    let removed: Vec<v8::Local<v8::Value>> = removed_ids.iter().map(|&c| wrap_cached(scope, op_state, c).into()).collect();
    // previousSibling/nextSibling flank the inserted run (derived from the tree post-insert). A pure
    // removal has no inserted node to anchor on here, so it reports null (callers needing accurate
    // removal siblings pre-capture and use notify_childlist_sib directly).
    let (prev_id, next_id) = if let (Some(&f), Some(&l)) = (added_ids.first(), added_ids.last()) {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        (dom.previous_sibling(f), dom.next_sibling(l))
    } else { (None, None) };
    notify_childlist_sib_ids(scope, parent_v.into(), &added, &removed, prev_id, next_id);
    notify_connected(scope, op_state, parent, added_ids);
}

pub(crate) fn cb_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (this_id, this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut arg_ids: Vec<u32> = Vec::new();
    for a in &nodes {
        {
            let id = node_arg_id(scope, &op_state, a, this_id);
            if this_is_doc && !doc_insert_ok_batch(&op_state, this_id, id, &arg_ids, false) {
                throw_dom(scope, "Failed to execute 'append' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
                return;
            }
            if insertion_cycles(&op_state, this_id, id) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
            if insertion_type_invalid(&op_state, this_id, id, this_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
            arg_ids.push(id);
        }
    }
    // A fragment argument contributes its children, not itself.
    let added = expand_fragments(&op_state, &arg_ids);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &id in &added { dom.append_child(this_id, id); }
    }
    notify_childlist_batch(scope, &op_state, this_id, &added, &[]);
}

pub(crate) fn cb_remove_self(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let (this_id, _this_is_doc) = match mutation_recv(scope, this.into()) { Some(r) => r, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Capture the parent AND the removed node's flanking siblings BEFORE the mutation (they are the
    // record's previousSibling/nextSibling and would be lost once the node is detached).
    // ANY parent: DOM's `remove()` is "if this's parent is null, return", and a fragment,
    // a shadow root or the document is as much a parent as an element -- Svelte and Lit remove the
    // top-level nodes they own from exactly those.
    let (parent, psib, nsib) = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        (dom.parent(this_id), dom.previous_sibling(this_id), dom.next_sibling(this_id)) };
    if let Some(p) = parent {
        op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(p, this_id);
        // ChildNode.remove() is a childList mutation on the (former) parent -- notify observers, else an
        // observer on the parent (or a Range.deleteContents/extractContents built on remove()) hangs.
        let pv = wrap_cached(scope, &op_state, p);
        notify_childlist_sib_ids(scope, pv.into(), &[], &[this.into()], psib, nsib);
    }
}

pub(crate) fn cb_replace_children(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (this_id, this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // DOM: convert the nodes into a node, ensure its pre-insertion validity, THEN replace all -- this emptied
    // the parent first, so an argument that failed left it empty.
    let mut added: Vec<u32> = Vec::new();
    for a in &nodes {
        let id = node_arg_id(scope, &op_state, a, this_id);
        // Every old child goes, so only what this call adds counts.
        if this_is_doc && !doc_insert_ok_batch(&op_state, this_id, id, &added, true) {
            throw_dom(scope, "Failed to execute 'replaceChildren' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
            return;
        }
        if insertion_cycles(&op_state, this_id, id) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
        if insertion_type_invalid(&op_state, this_id, id, this_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
        added.push(id);
    }
    let removed: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(this_id).to_vec();
    for &c in &removed { op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(this_id, c); }
    for &id in &added { op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(this_id, id); }
    // childList record: the old children were removed and the arguments added (a single record).
    notify_childlist_batch(scope, &op_state, this_id, &added, &removed);
}

// --- ChildNode mixin: before/after/replaceWith (variadic nodes|strings, positioned in the parent).
// THE PARENT IS ANY PARENT. This required `parentElement` and did nothing otherwise, so a
// node whose parent is a DocumentFragment, a ShadowRoot or the Document never moved: Svelte 5 builds
// every component in fragments with `anchor.before(fragment)`, and crates.io rendered its anchors and
// nothing else (66 elements against Chrome's 717, no error anywhere). Chrome 150, measured: created,
// cloned and template-content fragments, a shadow root and the document all mutate, and
// `documentElement.before(element)` throws HierarchyRequestError -- the document's pre-insertion
// validity, which `append` already asks. String args become text nodes. ---
pub(crate) fn cb_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(id) };
    let parent = match parent { Some(p) => p, None => return };
    let parent_is_doc = arena_parent_is_doc(&op_state, parent);
    let mut arg_ids: Vec<u32> = Vec::new();
    for a in &nodes {
        {
            let nid = node_arg_id(scope, &op_state, a, id);
            if insertion_cycles(&op_state, parent, nid) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
            if insertion_type_invalid(&op_state, parent, nid, parent_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
            if parent_is_doc && !doc_insert_ok_batch(&op_state, parent, nid, &arg_ids, false) {
                throw_dom(scope, "Failed to execute 'before' on 'Element': Only one element on document allowed.", "HierarchyRequestError");
                return;
            }
            arg_ids.push(nid);
        }
    }
    // A fragment argument contributes its children, not itself (HTML "insert").
    let added = expand_fragments(&op_state, &arg_ids);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &nid in &added { dom.insert_before(parent, nid, id); }
    }
    notify_childlist_batch(scope, &op_state, parent, &added, &[]);
}

pub(crate) fn cb_after(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(id) };
    let parent = match parent { Some(p) => p, None => return };
    let parent_is_doc = arena_parent_is_doc(&op_state, parent);
    // Convert every argument to a node id FIRST (strings/null/... -> Text; a Node keeps its id and is
    // not moved yet), rejecting a cycle before any mutation. Then compute the *viable* next sibling
    // against that final argument set and insert the whole batch before it (in order) -- a single
    // stable reference, not a per-argument one that a sibling-argument would invalidate.
    let mut arg_ids: Vec<u32> = Vec::with_capacity(args.length() as usize);
    for a in &nodes {
        {
            let nid = node_arg_id(scope, &op_state, a, id);
            if insertion_cycles(&op_state, parent, nid) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
            if insertion_type_invalid(&op_state, parent, nid, parent_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
            if parent_is_doc && !doc_insert_ok_batch(&op_state, parent, nid, &arg_ids, false) {
                throw_dom(scope, "Failed to execute 'after' on 'Element': Only one element on document allowed.", "HierarchyRequestError");
                return;
            }
            arg_ids.push(nid);
        }
    }
    // A fragment argument contributes its children, not itself. The viable next sibling is computed
    // against the expanded set so a sibling that is one of the inserted children is skipped.
    let arg_ids = expand_fragments(&op_state, &arg_ids);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let refn = dom.viable_next_sibling(id, &arg_ids);
        for &nid in &arg_ids {
            match refn { Some(r) => { dom.insert_before(parent, nid, r); } None => { dom.append_child(parent, nid); } }
        }
    }
    notify_childlist_batch(scope, &op_state, parent, &arg_ids, &[]);
}

pub(crate) fn cb_replace_with(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(id) };
    let parent = match parent { Some(p) => p, None => return };
    let parent_is_doc = arena_parent_is_doc(&op_state, parent);
    // Same viable-sibling batch as `after`, then remove `this` -- but keep it when it is itself one
    // of the arguments (`el.replaceWith(x, el)` re-inserts el and must not then delete it: WPT
    // `replaceWith() with one sibling of child and child itself as arguments`).
    let mut arg_ids: Vec<u32> = Vec::with_capacity(args.length() as usize);
    for a in &nodes {
        {
            let nid = node_arg_id(scope, &op_state, a, id);
            if insertion_cycles(&op_state, parent, nid) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
            if insertion_type_invalid(&op_state, parent, nid, parent_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
            // A document parent: `this` is being replaced, so its element does not count (DOM
            // "replace" validity, `doc_insert_ok`'s `dropping`).
            if parent_is_doc {
                let ok = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
                    arg_ids.iter().chain(std::iter::once(&nid)).all(|&k| doc_insert_ok(dom, parent, k, Some(id))) };
                if !ok {
                    throw_dom(scope, "Failed to execute 'replaceWith' on 'Element': Only one element on document allowed.", "HierarchyRequestError");
                    return;
                }
            }
            arg_ids.push(nid);
        }
    }
    // A fragment argument contributes its children, not itself. `this` stays removed unless it is
    // itself a direct argument (a fragment cannot contain the live `this`), so the contains-check is
    // over the expanded set.
    let arg_ids = expand_fragments(&op_state, &arg_ids);
    let removed: Vec<u32> = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let refn = dom.viable_next_sibling(id, &arg_ids);
        for &nid in &arg_ids {
            match refn { Some(r) => { dom.insert_before(parent, nid, r); } None => { dom.append_child(parent, nid); } }
        }
        if !arg_ids.contains(&id) { dom.remove_child(parent, id); vec![id] } else { Vec::new() }
    };
    notify_childlist_batch(scope, &op_state, parent, &arg_ids, &removed);
}

// --- insertAdjacentElement/Text/HTML: position a node/text/parsed-markup relative to this element.
// Positions match the glue (afterbegin for element/text = before firstElementChild; for HTML =
// prepend). ---
/// The four positions all three `insertAdjacent*` members accept, ASCII case-insensitively.
///
/// Every one of them begins by matching `position` against exactly this list and throwing a
/// `SyntaxError` when it does not match -- it is one algorithm, shared, and the arena helper below
/// silently ignored an unknown position instead. `el.insertAdjacentText("x", "x")` therefore
/// inserted nothing and returned undefined where the recorded browser throws. Returns `false` (with
/// the exception already thrown) when the caller must stop.
fn adjacent_position_ok(scope: &mut v8::PinScope, member: &str, pos: &str) -> bool {
    if matches!(pos, "beforebegin" | "afterbegin" | "beforeend" | "afterend") {
        return true;
    }
    throw_dom(
        scope,
        &format!("Failed to execute '{member}' on 'Element': The value provided ('{pos}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."),
        "SyntaxError",
    );
    false
}

/// WHERE each of the four positions puts the node: the (parent, before-child) pair that "insert
/// adjacent" step 1 resolves, and nothing else -- the insertion itself is a PRE-INSERT and lives in
/// [`insert_adjacent_pre_insert`] (CONF ELEM1.2). `None` is the spec's "return null" for an outside
/// position on a node with no parent, NOT an error.
///
/// THE POSITIONS ARE IN NODES, NOT IN ELEMENTS (CONF ELEM1.1). This used `first_element_child` for `afterbegin` and `next_element_sibling` for `afterend`, so
/// any non-element in the way was stepped over and the node landed AFTER it. Measured on
/// `<div id=target>T</div>`, whose first child is the text "T":
///
/// ```text
///   t.insertAdjacentElement("afterbegin", s1)   -> t.firstChild was still #text, s1 came second
///   t.insertAdjacentText("afterbegin", "HELLO") -> t.firstChild was still the old "T"
/// ```
///
/// WPT asserts on `target.firstChild.id` and `target.nextSibling.id`, not on the element-only
/// accessors (`Element-insertAdjacentElement.html:40,60`). Same shape as RANGE1.3's
/// `commonAncestorContainer`: a node-type filter in a walk the spec states without one.
///
/// THE PARENT FILTER GOES FOR THE SAME REASON. `beforebegin`/`afterend` required
/// `node_type(parent) == 1`, so `document.documentElement.insertAdjacentElement("afterend", ..)`
/// -- whose parent is the DOCUMENT -- silently did nothing where the spec throws
/// HierarchyRequestError. Refusing a parentless node stays (a node with no parent has nowhere to
/// put a sibling); refusing a DOCUMENT parent was never the rule -- the Document's own insertion
/// rules are the pre-insert's job, which is what ELEM1.2 then had to add.
fn insert_adjacent_target(dom: &Dom, this_id: u32, pos: &str) -> Option<(u32, Option<u32>)> {
    match pos {
        "beforebegin" => Some((dom.parent(this_id)?, Some(this_id))),
        "afterbegin" => Some((this_id, dom.first_child(this_id))),
        "beforeend" => Some((this_id, None)),
        "afterend" => Some((dom.parent(this_id)?, dom.next_sibling(this_id))),
        _ => None,
    }
}

/// "Pre-insert `node` into `parent` before `child`" -- the second half of "insert adjacent", and
/// the half that did not exist (CONF ELEM1.2).
///
/// THE FIFTH APPEARANCE OF ONE STRUCTURAL DEFECT: one spec algorithm implemented as several
/// different subsets (NODE1.3 `pre_insert_invalid`, NODE1.5 `NotFoundError`, RANGE1.3 `cmp_points`).
/// `insert_adjacent_arena` -- deleted by ELEM1.2, and split into [`insert_adjacent_target`] plus
/// this function -- resolved the position pair AND called the raw arena mutator itself, so this
/// path ran NO pre-insertion validity at all: not the ancestor/type rules (`pre_insert_invalid`)
/// and not the Document-parent counting (`doc_insert_ok`). Measured before the change, on the
/// release binary at ELEM1.1 (`b9313189`):
///
/// ```text
/// documentElement.insertAdjacentElement("beforebegin", span) -> no throw (span landed beside <html>)
/// documentElement.insertAdjacentElement("afterend",    span) -> no throw
/// documentElement.insertAdjacentText  ("beforebegin", "t1")  -> no throw (a Text child of #document)
/// documentElement.insertAdjacentText  ("afterend",    "t4")  -> no throw
/// ```
///
/// All four must be HierarchyRequestError (`Element-insertAdjacentElement.html:68`,
/// `Element-insertAdjacentText.html:57`), while `afterbegin`/`beforeend` on the same element keep
/// working -- their parent is the ELEMENT, not the document, which is why the position pair has to
/// be resolved before the rules are applied rather than the receiver being asked.
///
/// The document branch is Blink's `Document::CanAcceptChild`
/// (`third_party/blink/renderer/core/dom/document.cc:5492`), reached from `ContainerNode::
/// CheckAcceptChild` (`container_node.cc:290`) whenever the PARENT is a document; its two messages
/// are copied from `document.cc:5540` and `document.cc:5592`.
///
/// Returns false WHEN IT HAS THROWN, so a caller reads `if !insert_adjacent_pre_insert(..) { return; }`.
fn insert_adjacent_pre_insert(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
    parent: u32,
    before: Option<u32>,
    node_id: u32,
    method: &str,
) -> bool {
    if pre_insert_invalid(scope, op_state, parent, node_id, before, method) {
        return false;
    }
    if arena_parent_is_doc(op_state, parent) {
        let refusal = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            if doc_insert_ok(dom, parent, node_id, None) {
                None
            } else {
                Some(match dom.node_type(node_id) {
                    3 | 4 | 9 | 11 => format!(
                        "Nodes of type '{}' may not be inserted inside nodes of type '#document'.",
                        dom.node_name(node_id)
                    ),
                    10 => "Only one doctype on document allowed.".to_string(),
                    _ => "Only one element on document allowed.".to_string(),
                })
            }
        };
        if let Some(detail) = refusal {
            throw_dom(
                scope,
                &format!("Failed to execute '{method}' on 'Element': {detail}"),
                "HierarchyRequestError",
            );
            return false;
        }
    }
    // "Pre-insert" step 3: if the reference child IS the node being inserted, the reference moves
    // to the node's next sibling -- `el.insertAdjacentElement("afterbegin", el.firstChild)`.
    let before = if before == Some(node_id) {
        op_state.borrow().borrow::<HostDom>().0.next_sibling(node_id)
    } else {
        before
    };
    let parent_v: v8::Local<v8::Value> = wrap_cached(scope, op_state, parent).into();
    let node_v: v8::Local<v8::Value> = wrap_cached(scope, op_state, node_id).into();
    insert_with_steps(scope, op_state, parent, parent_v, node_id, node_v, before);
    true
}

pub(crate) fn cb_insert_adjacent_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (this_id, _this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let pos = args.get(0).to_rust_string_lossy(scope).to_lowercase();
    let el = args.get(1);
    if !adjacent_position_ok(scope, "insertAdjacentElement", &pos) { return; }
    let node_id = match unwrap_id(scope, el) { Some(i) => i, None => { rv.set(el); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target = { let st = op_state.borrow(); insert_adjacent_target(&st.borrow::<HostDom>().0, this_id, &pos) };
    // "beforebegin"/"afterend" on a node with NO parent: the spec returns null, it does not throw
    // (WHATWG "insert adjacent", step 1's first and last cases). This returned the element.
    let Some((parent, before)) = target else { let n = v8::null(scope); rv.set(n.into()); return };
    if !insert_adjacent_pre_insert(scope, &op_state, parent, before, node_id, "insertAdjacentElement") { return; }
    rv.set(el);
}

pub(crate) fn cb_insert_adjacent_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (this_id, _this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let pos = args.get(0).to_rust_string_lossy(scope).to_lowercase();
    let text = args.get(1).to_rust_string_lossy(scope);
    if !adjacent_position_ok(scope, "insertAdjacentText", &pos) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The spec creates the Text node BEFORE pre-inserting it, so a refused insert leaves an
    // orphaned node -- which is what a browser does too (it is unreachable and collected).
    let (target, node_id) = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        (insert_adjacent_target(dom, this_id, &pos), dom.create_text_node(&text))
    };
    let Some((parent, before)) = target else { return };
    insert_adjacent_pre_insert(scope, &op_state, parent, before, node_id, "insertAdjacentText");
}

pub(crate) fn cb_insert_adjacent_html(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (this_id, _this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let pos = args.get(0).to_rust_string_lossy(scope).to_lowercase();
    // (TrustedHTML or DOMString): a TrustedHTML read as itself, nothing on it called; a string through the
    // element's document's Trusted Types ("Element insertAdjacentHTML")
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "Element insertAdjacentHTML", context: "Failed to execute 'insertAdjacentHTML' on 'Element'" };
    let at = match crate::binding::protos::unwrap_id(scope, args.this().into()) {
        Some(n) => crate::core::trusted_types::tables::SinkAt::Node(n),
        None => crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context()),
    };
    let Some(html) = crate::core::trusted_types::tables::compliant_text(scope, args.get(1), &sink, at) else { return };
    if !adjacent_position_ok(scope, "insertAdjacentHTML", &pos) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // WHATWG "insert adjacent HTML" step 2: the context is `this` for the two INSIDE positions and
    // `this`'s PARENT for the two OUTSIDE ones -- and for those, a null or Document parent is a
    // NoModificationAllowedError, not a silent no-op (WPT domparsing/insert_adjacent_html.html's
    // `testThrowingNoParent`; Chrome 151 throws for both, 2026-09-11).
    let outside = matches!(pos.as_str(), "beforebegin" | "afterend");
    let candidate = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if outside { dom.parent(this_id).filter(|&p| dom.node_type(p) != 9) } else { Some(this_id) }
    };
    let Some(ctx) = candidate else {
        throw_dom(
            scope,
            "Failed to execute 'insertAdjacentHTML' on 'Element': The element has no parent.",
            "NoModificationAllowedError",
        );
        return;
    };
    let mut st = op_state.borrow_mut();
    let dom = &mut st.borrow_mut::<HostDom>().0;
    // Step 3: a context that is not an Element -- or IS the `<html>` element of an HTML document --
    // is replaced by a `<body>`. Without that clause an `<html>` context runs the "before head"
    // insertion mode and every call synthesizes a fresh head+body pair, so
    // `document.body.insertAdjacentHTML('afterend', '<p>')` grows a second `<head>` and `<body>`
    // (Chrome 151 keeps one of each and inserts the `<p>`, 2026-09-11).
    let ctx_is_html_element = dom.node_type(ctx) == 1
        && dom.tag_name(ctx).is_some_and(|t| t.eq_ignore_ascii_case("html"))
        && !dom.is_in_xml(ctx);
    let ids = if dom.node_type(ctx) != 1 || ctx_is_html_element {
        dom.parse_fragment_in_body(&html)
    } else {
        dom.parse_fragment_in(ctx, &html)
    };
    // The PARENT the parsed nodes land under, which is `this` for the two inside positions and `ctx`
    // for the two outside ones. Kept because the connect reactions below are owed to that parent's
    // insertion, and because `ids` is consumed by the match.
    let landed_in = if outside { ctx } else { this_id };
    match pos.as_str() {
        "beforeend" => { for &nid in &ids { dom.append_child(this_id, nid); } }
        "afterbegin" => { for &nid in ids.iter().rev() { dom.prepend_child(this_id, nid); } }
        // `ctx` IS the parent for these two, and step 2 above proved it is neither null nor a
        // Document -- so a DocumentFragment parent inserts here rather than being dropped.
        "beforebegin" => { for &nid in &ids { dom.insert_before(ctx, nid, this_id); } }
        // The next SIBLING, not the next element: `<b>x</b> text` + afterend lands before " text".
        "afterend" => { let refn = dom.next_sibling(this_id); for &nid in &ids { match refn { Some(r) => { dom.insert_before(ctx, nid, r); } None => { dom.append_child(ctx, nid); } } } }
        _ => {}
    }
    drop(st);
    // The fragment parser sets "already started" on every script it creates, so these stay inert.
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &ids);
    // One childList record for the parsed nodes, then their insertion steps. It reported nothing
    // before, so an `<iframe>` or `<link>` written in this way never loaded: loading
    // hangs off the childList notification.
    notify_childlist_batch(scope, &op_state, landed_in, &ids, &[]);
    crate::core::html::custom_element_registry::upgrade_created(scope, &ids);
}


// ── moved out of bindings.rs (STEP 3 dissolution) ──
pub(crate) fn cb_io_root(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = match deno_core::cppgc::try_unwrap_cppgc_object::<IntersectionObserver>(scope, args.this().into()) { Some(o) => o, None => { rv.set_null(); return; } };
    let r = unsafe { io.as_ref() }.root.borrow().clone();
    match r { Some(g) => { let l = v8::Local::new(scope, &g); rv.set(l.into()); } None => rv.set_null() }
}

/// `DocumentFragment.prototype.getElementById(id)`: the NonElementParentNode form scoped to the
/// fragment's own subtree (tree order, empty id never matches). Identity via wrap_cached so
/// `frag.getElementById(x) === frag.childNodes[i]` holds. -- WPT DocumentFragment-getElementById.
pub(crate) fn cb_frag_get_element_by_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let root = match deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id) {
        Some(i) => i,
        None => { rv.set_null(); return; }
    };
    let id_str = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let nid = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_element_by_id_in_subtree(root, &id_str) };
    match nid { Some(n) => { let obj = wrap_cached(scope, &op_state, n); rv.set(obj.into()); } None => rv.set_null() }
}

pub(crate) fn cb_get_root_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let el_id = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, this.into()).map(|p| unsafe { p.as_ref() }.id);
    // A DOCUMENT receiver walks from ITS OWN root, which is the whole reason `JsDocument` carries one.
    // Reading `dom.root()` for every document receiver meant `createHTMLDocument("t").getRootNode()`
    // answered the PAGE's document -- Chrome 150, measured 2026-09-12: it is the created document
    // itself. The page's answer was right by accident (its slot resolves to `dom.root()`).
    let doc_root: Option<u32> = if el_id.is_none() {
        deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this.into()).map(|_| doc_root_slot(scope, this.into()))
    } else {
        None
    };
    if el_id.is_none() && doc_root.is_none() { return; }
    let composed = v8::Local::<v8::Object>::try_from(args.get(0)).ok()
        .and_then(|o| get_prop(scope, o, "composed"))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target: u32 = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let start = match el_id {
            Some(id) => id,
            None => resolve_doc_root(dom, doc_root.expect("checked above")),
        };
        if composed { dom.shadow_including_root(start) } else { dom.root_of(start) }
    };
    // A document root's canonical object IS its Document -- the one object every other path hands out
    // (`el.ownerDocument === document`, `html.parentNode === document`). Wrapping the root id used to
    // mint a SECOND object for it, retyped through the nodeType-9 map to DocumentFragment.prototype,
    // with no Document member on it. React 19's hoistable hydration does `container.getRootNode()`,
    // then `root.ownerDocument || root`, then `.getElementsByTagName(...)` on that -- and threw on
    // react.dev, overreacted.io and a production site because of exactly this (corpus, 2026-09-10). The
    // page-root branch that closed it for the PAGE is gone: every Document is interned in the node
    // cache under its arena root (`document.rs::intern_document`), so `wrap_cached` answers it here
    // for a created and a frame document too -- which the branch never did.
    let obj = wrap_cached(scope, &op_state, target);
    rv.set(obj.into());
}

pub(crate) fn cb_has_child_nodes_p(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = node_recv(scope, args.this().into());
    if matches!(r, NodeRecv::Other) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let has = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; !dom.children(node_recv_id(&r, dom).unwrap()).is_empty() };
    rv.set(v8::Boolean::new(scope, has).into());
}

/// `Node.cloneNode(deep)` — WHATWG DOM §4.4 "clone a node". The copy is the arena clone's interned,
/// retyped wrapper.
///
/// It resolved its receiver with a bare `try_unwrap_cppgc_object::<JsElement>` and `None => return`,
/// so EVERY receiver that is not an ordinary node wrapper -- a Document above all -- got no answer
/// at all: `document.implementation.createDocument(...).cloneNode()` was `undefined`, and the next
/// line of the test read `.nodeType` off it. `node_recv` is the resolution this file already uses
/// for `prepend`, `append` and the rest, and it names the Document receiver instead of dropping it.
///
/// Two spec steps were missing with it:
///
/// * **a shadow root is not clonable** -- "if node is a shadow root, then throw a NotSupportedError".
///   A shadow root wears the ordinary node wrapper, so it came through the Element arm and cloned
///   into a loose `#document` node (WPT shadow-dom/Node-prototype-cloneNode, both modes);
/// * **the copy is a node "that implements the same interfaces"** -- an XMLDocument's clone is an
///   XMLDocument, an HTMLDocument's an HTMLDocument. That is what `mint_document_for` stamps, from
///   the kind recorded for the ORIGINAL at its creation (`DocKinds`), so the copy answers the same
///   `contentType` (WPT dom/nodes/Node-cloneNode-XMLDocument, and `implementation.createDocument`'s
///   contentType/charset/URL/compatMode assertions).
pub(crate) fn cb_clone_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let deep = args.get(0).boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match node_recv(scope, this.into()) {
        NodeRecv::El(id) => {
            if op_state.borrow().borrow::<HostDom>().0.is_shadow_root(id) {
                throw_dom(
                    scope,
                    "Failed to execute 'cloneNode' on 'Node': ShadowRoot nodes are not clonable.",
                    "NotSupportedError",
                );
                return;
            }
            let new_id = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.clone_node(id, deep) };
            crate::core::html::html_script_element::clone_started(&op_state, id, new_id);
            crate::core::dom::element::clone_attr_namespaces(&op_state, id, new_id);
            // DOM "clone a node": the copy's node document is the source's. Recorded only for a source
            // outside the page's document -- an absent owner entry IS the page's, and adoption compares
            // entries -- so a clone of a `createHTMLDocument()` node is not the page's.
            if let Some(doc) = node_document(scope, &op_state, id) {
                let doc_root = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, doc).map(|p| unsafe { p.as_ref() }.root);
                let page_root = op_state.borrow().borrow::<HostDom>().0.root();
                if doc_root.is_some_and(|r| r != page_root) {
                    let g = v8::Global::new(scope, doc);
                    op_state.borrow().borrow::<OwnerMap>().0.borrow_mut().insert(new_id, g);
                }
            }
            let w = wrap_cached(scope, &op_state, new_id);
            // DOM "clone a node" creates each element with "create an element" against the copy's node
            // document: constructed before `cloneNode` returns when that is the page's.
            crate::core::html::custom_element_registry::upgrade_created(scope, &[new_id]);
            rv.set(w.into());
        }
        NodeRecv::Doc(slot) => {
            let root = { let st = op_state.borrow(); resolve_doc_root(&st.borrow::<HostDom>().0, slot) };
            let new_root = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.clone_node(root, deep) };
            crate::core::html::html_script_element::clone_started(&op_state, root, new_root);
            crate::core::dom::element::clone_attr_namespaces(&op_state, root, new_root);
            // The kind recorded for the original. The PAGE's document has none recorded (it is the
            // page, not a `createDocument`/`DOMParser` product) and is an HTML document.
            let kind = op_state
                .borrow()
                .try_borrow::<DocKinds>()
                .and_then(|m| m.0.borrow().get(&slot).cloned())
                .unwrap_or(DocKind::Html);
            // The copy keeps the original's origin (its baseURI/readyState follow it).
            let origin = root_origin(scope, slot).unwrap_or(DocOrigin::CreatedAboutBlank);
            let doc = mint_document_for(scope, new_root, kind, origin);
            // "Set copy's node document to copy" for the whole subtree: the arena is SHARED with the
            // page, so without the OwnerMap record every node in the copy would report the PAGE
            // document as its `ownerDocument` -- the same reason `createDocument` owns what it makes.
            let mut stack: Vec<u32> = { let st = op_state.borrow(); st.borrow::<HostDom>().0.children(new_root) };
            while let Some(n) = stack.pop() {
                {
                    let st = op_state.borrow();
                    let kids = st.borrow::<HostDom>().0.children(n);
                    stack.extend_from_slice(&kids);
                }
                crate::core::dom::document::own_node(scope, doc.into(), n);
            }
            rv.set(doc.into());
        }
        // An Attr wrapper is neither, and `attr.rs` has no clone of its own -- unchanged here.
        NodeRecv::Other => {}
    }
}

pub(crate) fn cb_prepend(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (this_id, this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let Some(nodes) = node_args(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut arg_ids: Vec<u32> = Vec::new();
    for a in &nodes {
        {
            let id = node_arg_id(scope, &op_state, a, this_id);
            if this_is_doc && !doc_insert_ok_batch(&op_state, this_id, id, &arg_ids, false) {
                throw_dom(scope, "Failed to execute 'prepend' on 'Node': Only one element on document allowed.", "HierarchyRequestError");
                return;
            }
            if insertion_cycles(&op_state, this_id, id) { throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError"); return; }
            if insertion_type_invalid(&op_state, this_id, id, this_is_doc) { throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError"); return; }
            arg_ids.push(id);
        }
    }
    // A fragment argument contributes its children, not itself. Insert the whole expanded set before
    // the current first child (in order), which is `prepend` for one or many nodes alike.
    let added = expand_fragments(&op_state, &arg_ids);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let refn = dom.children(this_id).first().copied();
        for &id in &added {
            match refn { Some(r) => { dom.insert_before(this_id, id, r); } None => { dom.append_child(this_id, id); } }
        }
    }
    notify_childlist_batch(scope, &op_state, this_id, &added, &[]);
}

/// ProcessingInstruction.prototype.target: the arena PI node's target (native getter, PI proto only).
pub(crate) fn cb_pi_target(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = match arg_node_id(scope, args.this().into()) {
        Some(id) => { let op = deno_core::JsRuntime::op_state_from(scope); let t = op.borrow().borrow::<HostDom>().0.pi_fields(id).0; t }
        None => String::new(),
    };
    let v = str_val(scope, &t); rv.set(v);
}
/// Node.ownerDocument: native getter. A node created or adopted by a non-main document
/// (DOMImplementation.createDocument / createHTMLDocument) reports that document via the
/// OwnerMap override; everything else — the whole main tree and everything createElement'd on the
/// page document — reports the page document, which is exactly what it did before the map existed.
///
/// The map is written from Rust only: `core/xml/dom_implementation.rs` (the created document, its
/// documentElement and its doctype) and `core/dom/document.rs`. The JS-facing writer `cb_set_owner`
/// and its `op_set_owner` door WERE the fifth writer and are deleted — nothing called
/// either, and the four Rust writers above already covered every node that gets a non-page owner.
pub(crate) fn cb_owner_document(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = match node_recv(scope, this.into()) {
        // Only a Document answers null (DOM "ownerDocument"). Reading `nodeType` off the receiver to
        // decide it -- what this did -- asked the page's own getter, and a ShadowRoot (a Document node
        // in the arena) answered 9 and got null too.
        NodeRecv::Doc(_) => { rv.set_null(); return; }
        NodeRecv::El(id) => id,
        // An Attr's document is its element's; a detached one's is the page's.
        NodeRecv::Other => match attr_eid_of(scope, this) {
            Some(eid) => eid,
            None => { page_document_or_null(scope, &mut rv); return; }
        },
    };
    match node_document(scope, &op_state, id) {
        Some(d) => rv.set(d),
        None => page_document_or_null(scope, &mut rv),
    }
}

fn page_document_or_null(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue) {
    match global_get(scope, "document") { Some(d) => rv.set(d), None => rv.set_null() }
}

/// A node's node document, read off the tree: null for a Document; the Document at the top of its
/// shadow-including root when there is one (so a frame document's `<body>` answers the FRAME's
/// document); for `<template>` content, the template's inert contents owner document;
/// otherwise the document that created or last adopted it (`OwnerMap`). `None` when none of those
/// says, which callers read as the page's document.
pub(crate) fn node_document<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    let (is_document, root, root_is_document, template_host) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let is_doc = |n: u32| dom.node_type(n) == 9 && !dom.is_shadow_root(n);
        let root = dom.shadow_including_root(id);
        (is_doc(id), root, root != id && is_doc(root), dom.template_host_of(dom.root_of(id)))
    };
    if is_document {
        return Some(v8::null(scope).into());
    }
    if root_is_document {
        return Some(wrap_cached(scope, op_state, root).into());
    }
    if let Some(host) = template_host {
        // The template contents owner of the TEMPLATE's node document, wherever that template is: in
        // a document, detached (a clone -- its recorded owner), or inside another template's contents
        // (whose node document is already an inert document, its own owner). Keyed on the tree root
        // instead, a detached clone and a nested template both got the PAGE's inert document (WPT
        // templates-copy-document-owner, template-content-node-document).
        let host_doc = node_document(scope, op_state, host);
        return Some(template_contents_owner(scope, op_state, host_doc));
    }
    // A detached subtree's nodes share its root's recorded owner.
    let owner = {
        let st = op_state.borrow();
        let m = st.borrow::<OwnerMap>();
        let map = m.0.borrow();
        let g = map.get(&id).or_else(|| map.get(&root)).cloned();
        g
    };
    owner.map(|g| v8::Local::new(scope, &g)).filter(|l| !l.is_null() && !l.is_undefined())
}

/// Per document (by root), its associated inert template document -- HTML's "appropriate template
/// contents owner document": an empty HTMLDocument with no browsing context, made on first use --
/// and the roots of those inert documents, each of which is its OWN owner ("a Document created by
/// this algorithm").
#[derive(Default)]
struct InertTemplateDocs {
    by_doc: std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Value>>>,
    inert: std::cell::RefCell<std::collections::HashSet<u32>>,
}

fn template_contents_owner<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, doc: Option<v8::Local<'s, v8::Value>>) -> v8::Local<'s, v8::Value> {
    // A template whose document nothing records is the page's.
    let doc = match doc.filter(|d| !d.is_null()) {
        Some(d) => d,
        None => global_get(scope, "document").unwrap_or_else(|| v8::null(scope).into()),
    };
    let Some(key) = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, doc).map(|p| unsafe { p.as_ref() }.root) else {
        return doc;
    };
    let known = {
        let st = op_state.borrow();
        st.try_borrow::<InertTemplateDocs>().map(|m| (m.inert.borrow().contains(&key), m.by_doc.borrow().get(&key).cloned()))
    };
    match known {
        Some((true, _)) => return doc,
        Some((false, Some(g))) => return v8::Local::new(scope, &g),
        _ => {}
    }
    let root = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_html_document();
    let inert: v8::Local<v8::Value> = mint_document_for(scope, root, DocKind::Html, DocOrigin::Parsed).into();
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<InertTemplateDocs>().is_none() {
        st.put(InertTemplateDocs::default());
    }
    let m = st.borrow::<InertTemplateDocs>();
    m.by_doc.borrow_mut().insert(key, v8::Global::new(scope, inert));
    m.inert.borrow_mut().insert(root);
    inert
}

// ── the Node table ─────────────────────────────────────────────────────────────────

use crate::core::dom::element::{cb_is_connected, cb_is_default_namespace, cb_lookup_namespace_uri, cb_lookup_prefix};
use crate::core::dom::shared::{
    cb_child_nodes, cb_first_child, cb_last_child, cb_next_sibling, cb_parent_element, cb_parent_node, cb_previous_sibling,
    row_accessor, row_getter, row_method,
};
use crate::core::dom::text::{cb_text_content_get, cb_text_content_set};
use crate::install::MemberDef;

/// `Node.moveBefore(node, child)` on an Element or DocumentFragment receiver -- the atomic move (the
/// node keeps its state instead of being removed and re-inserted); the Document form is
/// `document.rs::cb_doc_move_before`. WebIDL declares `(Node node, Node? child)`: a non-Node is a
/// TypeError; a node that is in no tree cannot be moved (HierarchyRequestError); `child` must be a
/// child of this (NotFoundError).
pub(crate) fn cb_move_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Node", "moveBefore") { return; }
    let (this_id, _this_is_doc) = match mutation_recv(scope, args.this().into()) { Some(r) => r, None => return };
    let node = match unwrap_id(scope, args.get(0)) {
        Some(i) => i,
        None => { crate::binding::protos::throw_type_error(scope, "Failed to execute 'moveBefore' on 'Node': parameter 1 is not of type 'Node'."); return; }
    };
    let a1 = args.get(1);
    let child = if a1.is_null() { None } else {
        match unwrap_id(scope, a1) {
            Some(i) => Some(i),
            None => { crate::binding::protos::throw_type_error(scope, "Failed to execute 'moveBefore' on 'Node': parameter 2 is not of type 'Node'."); return; }
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if insertion_type_invalid(&op_state, this_id, node, _this_is_doc) || insertion_cycles(&op_state, this_id, node) {
        throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError");
        return;
    }
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(node) };
    let parent = match parent {
        Some(p) => p,
        None => { throw_dom(scope, "The node to be moved is not in a tree.", "HierarchyRequestError"); return; }
    };
    if let Some(c) = child {
        let ok = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(c) == Some(this_id) };
        if !ok { throw_dom(scope, "The node before which the new node is to be inserted is not a child of this node.", "NotFoundError"); return; }
    }
    // The old place, read BEFORE the move -- the removal record's previousSibling/nextSibling.
    let (_, _, old_prev, old_next) = old_place(&op_state, node);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        dom.remove_child(parent, node);
        match child { Some(c) => dom.insert_before(this_id, node, c), None => dom.append_child(this_id, node) }
    }
    // THE REMOVE HALF OF THE MOVE, WHICH WAS REPORTED NOWHERE. `moveBefore` is a
    // removal followed by an insertion and it owes a childList record on the OLD parent as well as
    // the new one; this reported only the new one. Measured on Chrome 150 over the fixture, one
    // observer on each of two parents and `b.moveBefore(e, null)`:
    //
    // ```text
    //   chrome   a:+0-1, b:+1-0
    //   korvus   b:+1-0            <- before this
    // ```
    //
    // Reporting it here is also what routes the move's `disconnectedCallback` through the same
    // choke point every other removal uses, instead of teaching `moveBefore` its own reaction. That
    // the reactions fire at all for a "state-preserving" move is FP14's measurement, re-measured
    // here: `disc` then `conn`, for the moved node and for a custom element merely descended from
    // it.
    let old_pv = wrap_cached(scope, &op_state, parent);
    let nv: v8::Local<v8::Value> = wrap_cached(scope, &op_state, node).into();
    let oprev = old_prev.map(|s| { let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, s).into(); w });
    let onext = old_next.map(|s| { let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, s).into(); w });
    crate::core::html::html_script_element::set_moving(&op_state, true);
    notify_childlist_sib(scope, old_pv.into(), &[], &[nv], oprev, onext);
    notify_childlist_batch(scope, &op_state, this_id, &[node], &[]);
    crate::core::html::html_script_element::set_moving(&op_state, false);
}

/// The `Node` rows: the fourteen accessors, the twelve constants (on both holders, from the table), and
/// the fourteen methods -- every body a callback that already answered for the page (the `_p`
/// polymorphic ops resolve a Document receiver to its arena root, which is what lets `Document` chain
/// through `Node`).
pub fn install_node(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "nodeType" => row_getter(scope, proto, member, cb_node_type_p),
        "nodeName" => row_getter(scope, proto, member, cb_node_name_p),
        "baseURI" => row_getter(scope, proto, member, cb_base_uri_get),
        "isConnected" => row_getter(scope, proto, member, cb_is_connected),
        "ownerDocument" => row_getter(scope, proto, member, cb_owner_document),
        "parentNode" => row_getter(scope, proto, member, cb_parent_node),
        "parentElement" => row_getter(scope, proto, member, cb_parent_element),
        "childNodes" => row_getter(scope, proto, member, cb_child_nodes),
        "firstChild" => row_getter(scope, proto, member, cb_first_child),
        "lastChild" => row_getter(scope, proto, member, cb_last_child),
        "previousSibling" => row_getter(scope, proto, member, cb_previous_sibling),
        "nextSibling" => row_getter(scope, proto, member, cb_next_sibling),
        "nodeValue" => row_accessor(scope, proto, member, cb_node_value_get, cb_node_value_set),
        "textContent" => row_accessor(scope, proto, member, cb_text_content_get, cb_text_content_set),
        "appendChild" => row_method(scope, proto, member, cb_append_child),
        "cloneNode" => row_method(scope, proto, member, cb_clone_node),
        "compareDocumentPosition" => row_method(scope, proto, member, cb_compare_doc_pos_p),
        "contains" => row_method(scope, proto, member, cb_contains_p),
        "getRootNode" => row_method(scope, proto, member, cb_get_root_node),
        "hasChildNodes" => row_method(scope, proto, member, cb_has_child_nodes_p),
        "insertBefore" => row_method(scope, proto, member, cb_insert_before),
        "isDefaultNamespace" => row_method(scope, proto, member, cb_is_default_namespace),
        "isEqualNode" => row_method(scope, proto, member, cb_is_equal_node_p),
        "isSameNode" => row_method(scope, proto, member, cb_is_same_node_p),
        "lookupNamespaceURI" => row_method(scope, proto, member, cb_lookup_namespace_uri),
        "lookupPrefix" => row_method(scope, proto, member, cb_lookup_prefix),
        "normalize" => row_method(scope, proto, member, cb_node_normalize),
        "removeChild" => row_method(scope, proto, member, cb_remove_child),
        "replaceChild" => row_method(scope, proto, member, cb_replace_child),
        other => debug_assert!(false, "install_node: unexpected member {other}"),
    }
}

// The capture's rows, verbatim (`generated_registry.rs` at the cutover): 14 accessors, then the 18
// constants, then 14 methods -- the recorded enumeration order, which the installer reproduces by
// mounting in table order with `constructor` last. A WebIDL constant is two rows (prototype + interface
// object), both `{ writable: false, enumerable: true, configurable: false }` from the first moment --
// the reason the bootstrap's constant-sealing pass existed is gone.
webapi::interface! {
    "Node" : "EventTarget" [interface, window] {
        ctor "Node" len 0 = stub,
        attr "nodeType" : Object readonly = install_node,
        attr "nodeName" : Object readonly = install_node,
        attr "baseURI" : Object readonly = install_node,
        attr "isConnected" : Object readonly = install_node,
        attr "ownerDocument" : Object readonly nullable = install_node,
        attr "parentNode" : Object readonly nullable = install_node,
        attr "parentElement" : Object readonly nullable = install_node,
        attr "childNodes" : Object readonly = install_node,
        attr "firstChild" : Object readonly nullable = install_node,
        attr "lastChild" : Object readonly nullable = install_node,
        attr "previousSibling" : Object readonly nullable = install_node,
        attr "nextSibling" : Object readonly nullable = install_node,
        attr "nodeValue" : Object nullable = install_node,
        attr "textContent" : Object nullable = install_node,
        constant "ELEMENT_NODE" : Integer = 1,
        constant "ATTRIBUTE_NODE" : Integer = 2,
        constant "TEXT_NODE" : Integer = 3,
        constant "CDATA_SECTION_NODE" : Integer = 4,
        constant "ENTITY_REFERENCE_NODE" : Integer = 5,
        constant "ENTITY_NODE" : Integer = 6,
        constant "PROCESSING_INSTRUCTION_NODE" : Integer = 7,
        constant "COMMENT_NODE" : Integer = 8,
        constant "DOCUMENT_NODE" : Integer = 9,
        constant "DOCUMENT_TYPE_NODE" : Integer = 10,
        constant "DOCUMENT_FRAGMENT_NODE" : Integer = 11,
        constant "NOTATION_NODE" : Integer = 12,
        constant "DOCUMENT_POSITION_DISCONNECTED" : Integer = 1,
        constant "DOCUMENT_POSITION_PRECEDING" : Integer = 2,
        constant "DOCUMENT_POSITION_FOLLOWING" : Integer = 4,
        constant "DOCUMENT_POSITION_CONTAINS" : Integer = 8,
        constant "DOCUMENT_POSITION_CONTAINED_BY" : Integer = 16,
        constant "DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC" : Integer = 32,
        method "appendChild" : Object len 1 required 1 = install_node,
        method "cloneNode" : Object len 0 = install_node,
        method "compareDocumentPosition" : Object len 1 required 1 = install_node,
        method "contains" : Object len 1 required 1 = install_node,
        method "getRootNode" : Object len 0 = install_node,
        method "hasChildNodes" : Object len 0 = install_node,
        method "insertBefore" : Object len 2 required 2 = install_node,
        method "isDefaultNamespace" : Object len 1 required 1 = install_node,
        method "isEqualNode" : Object len 1 required 1 = install_node,
        method "isSameNode" : Object len 1 required 1 = install_node,
        method "lookupNamespaceURI" : Object len 1 required 1 nullable = install_node,
        method "lookupPrefix" : Object len 1 required 1 nullable = install_node,
        method "normalize" : Object len 0 = install_node,
        method "removeChild" : Object len 1 required 1 = install_node,
        method "replaceChild" : Object len 2 required 2 = install_node,
        static_constant "ELEMENT_NODE" : Integer = 1,
        static_constant "ATTRIBUTE_NODE" : Integer = 2,
        static_constant "TEXT_NODE" : Integer = 3,
        static_constant "CDATA_SECTION_NODE" : Integer = 4,
        static_constant "ENTITY_REFERENCE_NODE" : Integer = 5,
        static_constant "ENTITY_NODE" : Integer = 6,
        static_constant "PROCESSING_INSTRUCTION_NODE" : Integer = 7,
        static_constant "COMMENT_NODE" : Integer = 8,
        static_constant "DOCUMENT_NODE" : Integer = 9,
        static_constant "DOCUMENT_TYPE_NODE" : Integer = 10,
        static_constant "DOCUMENT_FRAGMENT_NODE" : Integer = 11,
        static_constant "NOTATION_NODE" : Integer = 12,
        static_constant "DOCUMENT_POSITION_DISCONNECTED" : Integer = 1,
        static_constant "DOCUMENT_POSITION_PRECEDING" : Integer = 2,
        static_constant "DOCUMENT_POSITION_FOLLOWING" : Integer = 4,
        static_constant "DOCUMENT_POSITION_CONTAINS" : Integer = 8,
        static_constant "DOCUMENT_POSITION_CONTAINED_BY" : Integer = 16,
        static_constant "DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC" : Integer = 32
    }
}

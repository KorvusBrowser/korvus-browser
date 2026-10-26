//! `core/dom/tree_walker.rs` — `TreeWalker`, the second DOM-Traversal cursor
//! (dom.spec.whatwg.org/#interface-treewalker).
//!
//! A walker is LIVE state over the arena tree — `root`, `whatToShow`, `filter`, and a
//! `currentNode` that each traversal method moves — not a flattened node list. The list snapshot
//! the first cut took (flatten at `createTreeWalker` time, then index arithmetic) answered
//! `nextNode`/`previousNode` on a static tree, but WPT dom/traversal grades the live semantics:
//! filters must run lazily on EVERY traverse (including re-`Get`ting `acceptNode`), REJECT prunes
//! vs SKIP descends, `parentNode`/`firstChild`/`lastChild`/`nextSibling`/`previousSibling` walk
//! the actual sibling edges (they did not exist at all), a throwing filter must PROPAGATE (not
//! silently REJECT), re-entrant traversal throws `InvalidStateError`, and a currentNode outside
//! the root still walks (the root only bounds the upward climb). The five traversal algorithms
//! below mirror Blink's `tree_walker.cc` control flow — the reference the task points at — which
//! is the spec's prose already de-bugged by a decade of layout tests.
//!
//! The document is carried as [`ON_DOCUMENT_ID`] (`u32::MAX`) in `root`/`current_node` so
//! `walker.root === document` keeps the global Document object's identity; every arena access
//! funnels through the `tw_*` helpers below, which map that sentinel onto the arena's real root
//! and never index the arena with it.

use deno_core::{v8, CppgcBase, OpState};
use macros::webapi;

use crate::modules::crypto::shared::crypto_num;
use crate::binding::host_dom::HostDom;
use crate::binding::protos::{arg_node_id, get_prop, global_get, throw_dom, throw_type_error, wrap_cached};
use crate::core::dom::shared::node_type_of;
use crate::core::events::shared::ON_DOCUMENT_ID;

#[repr(C)]
#[derive(CppgcBase)]
pub struct TreeWalker {
    pub(crate) root: std::cell::Cell<u32>,
    pub(crate) what_to_show: std::cell::Cell<u32>,
    pub(crate) filter: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    /// The walker's live position: any node (possibly one outside the root's subtree, which the
    /// spec allows and the WPT currentNode tests demand).
    pub(crate) current_node: std::cell::Cell<u32>,
    /// DOM §traversal's recursion guard: a filter that re-enters this walker throws
    /// `InvalidStateError` instead of recursing.
    pub(crate) active: std::cell::Cell<bool>,
}

// TreeWalker has no WebIDL constructor (only `document.createTreeWalker` builds one): the interface
// object is the table's non-newable one (`core/dom/spine.rs`), with Chrome's two sentences. An OWN class
// with no member of its own -- every row is the table's -- so the struct is simply
// cppgc-wrappable, and `binding::class::mint` puts a walker on the realm's `TreeWalker.prototype`. (Until
// P6.2 this was an op2 class whose `#[constructor]` threw, kept so deno_core realised a class template
// to mint on.)
#[webapi(base, own)]
impl TreeWalker {}

// ---- the sentinel-aware arena view -----------------------------------------------------------

/// `parent` — the document sentinel has no parent (it IS the root).
fn tw_parent(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if id == ON_DOCUMENT_ID { None } else { dom.parent(id) }
}

/// `firstChild` — the sentinel's children are the arena root's.
fn tw_first_child(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if id == ON_DOCUMENT_ID { dom.first_child(dom.root()) } else { dom.first_child(id) }
}

/// `lastChild` — see [`tw_first_child`].
fn tw_last_child(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if id == ON_DOCUMENT_ID { dom.last_child(dom.root()) } else { dom.last_child(id) }
}

/// `nextSibling` — the sentinel has no sibling.
fn tw_next_sibling(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if id == ON_DOCUMENT_ID { None } else { dom.next_sibling(id) }
}

/// `previousSibling` — see [`tw_next_sibling`].
fn tw_previous_sibling(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    if id == ON_DOCUMENT_ID { None } else { dom.previous_sibling(id) }
}

/// Node identity under the sentinel convention: the sentinel and the arena's real root are the
/// same node (the document), so a boundary comparison like `node == root` must treat them so.
fn tw_same(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, a: u32, b: u32) -> bool {
    if a == b { return true; }
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let real = dom.root();
    (a == ON_DOCUMENT_ID && b == real) || (b == ON_DOCUMENT_ID && a == real)
}

// ---- filtering (DOM §traversal "filtering") ---------------------------------------------------

/// Apply the walker's `whatToShow` + `filter` to `node`: `1` ACCEPT, `2` REJECT, `3` SKIP.
///
/// Spec order: recursion guard → whatToShow bit → null filter → call. The call resolves the
/// callback FRESH each time (`Get(filter, "acceptNode")` for object filters — WPT
/// "performs `Get` on every traverse"), passes the filter value as `this` and the node as the
/// only argument, and propagates whatever the filter throws verbatim (WPT "filter function that
/// throws": a throw must escape `firstChild()`, not become a REJECT). A non-callable `acceptNode`
/// is the WebIDL callback TypeError, thrown at call time (the object was accepted at the factory).
pub(crate) fn tw_accept_node(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    node_id: u32,
) -> Result<u32, ()> {
    if active.get() {
        throw_dom(scope, "Filter function can't be recursive", "InvalidStateError");
        return Err(());
    }
    // whatToShow: bit (nodeType - 1). Filtered before the callback ever runs (so an unshown node
    // costs no JS), and for every node kind -- the first cut only mapped four types.
    let nt = { let st = op_state.borrow(); node_type_of(&st.borrow::<HostDom>().0, node_id) };
    let shown = nt.checked_sub(1).and_then(|s| 1u32.checked_shl(s)).map(|bit| (bit & wts) != 0).unwrap_or(false);
    if !shown { return Ok(3); }
    let Some(fg) = filter else { return Ok(1) };
    let fl = v8::Local::new(scope, fg);
    active.set(true);
    let r = (|| -> Result<u32, ()> {
        // A callable filter IS the operation (its own `acceptNode` property must never be
        // consulted); an object filter's operation is looked up per call.
        let f = if fl.is_function() {
            match v8::Local::<v8::Function>::try_from(fl) { Ok(f) => f, Err(_) => return Err(()) }
        } else {
            match v8::Local::<v8::Object>::try_from(fl) {
                Ok(o) => match get_prop(scope, o, "acceptNode") {
                    // A throwing `acceptNode` getter rethrows (its exception is already pending).
                    Some(p) => match v8::Local::<v8::Function>::try_from(p) {
                        Ok(f) => f,
                        Err(_) => {
                            throw_type_error(scope, "Failed to execute 'acceptNode' on 'NodeFilter': The callback provided as parameter 3 is not a function.");
                            return Err(());
                        }
                    },
                    None => return Err(()),
                },
                Err(_) => return Err(()),
            }
        };
        // Through [`tw_node_value`], not `wrap_cached`: a filter must be handed the SAME object the
        // traversal would return, and for the document sentinel that is the global `document`.
        let node = tw_node_value(scope, node_id).ok_or(())?;
        // recorded as Chromium labels the call (`NodeFilter`); the side-by-side saw none of a challenge's 95
        #[cfg(feature = "analysis")]
        let out = crate::analysis_hook::call_traced(scope, "NodeFilter", f, fl, &[node]).ok_or(())?;
        #[cfg(not(feature = "analysis"))]
        let out = f.call(scope, fl, &[node]).ok_or(())?;
        // WebIDL `unsigned short` return conversion (ToUint32, then truncate to 16 bits).
        Ok(out.uint32_value(scope).ok_or(())? & 0xFFFF)
    })();
    active.set(false);
    r
}

// ---- the traversal algorithms (Blink tree_walker.cc control flow) ------------------------------

/// `TreeWalker.parentNode()`: walk currentNode's ancestors up to (not past) the root, returning
/// the first that passes the filter.
fn tw_parent_node(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    current: u32,
) -> Result<Option<u32>, ()> {
    let mut node = current;
    loop {
        if tw_same(op_state, node, root) { return Ok(None); }
        node = match tw_parent(op_state, node) { Some(p) => p, None => return Ok(None) };
        if tw_accept_node(scope, op_state, wts, filter, active, node)? == 1 {
            return Ok(Some(node));
        }
    }
}

/// The shared shape of `firstChild`/`lastChild`: scan the current node's children in `dir`
/// (first/last), where ACCEPT returns, SKIP descends into the node's own children first, and
/// REJECT skips the subtree — advancing with the do-while sibling/ancestor walk that stays inside
/// `current`'s child list.
fn tw_child_scan(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    current: u32,
    forward: bool,
) -> Result<Option<u32>, ()> {
    let first = if forward { tw_first_child(op_state, current) } else { tw_last_child(op_state, current) };
    let mut node = match first { Some(c) => c, None => return Ok(None) };
    loop {
        match tw_accept_node(scope, op_state, wts, filter, active, node)? {
            1 => return Ok(Some(node)),
            3 => {
                // SKIP: the node's own children are candidates, before its siblings.
                let deeper = if forward { tw_first_child(op_state, node) } else { tw_last_child(op_state, node) };
                if let Some(c) = deeper { node = c; continue; }
            }
            _ => {} // REJECT: skip the subtree outright.
        }
        // Advance to the next (or previous) node in `current`'s descendant-sibling order: next
        // sibling if there is one, else climb to the parent and take ITS next sibling — never
        // leaving `current` (and never stepping over the root).
        loop {
            let side = if forward { tw_next_sibling(op_state, node) } else { tw_previous_sibling(op_state, node) };
            if let Some(s) = side { node = s; break; }
            let parent = match tw_parent(op_state, node) { Some(p) => p, None => return Ok(None) };
            if tw_same(op_state, parent, root) || tw_same(op_state, parent, current) {
                return Ok(None);
            }
            node = parent;
        }
    }
}

/// `TreeWalker.nextSibling()` / `previousSibling()` — DOM §"concept-traverse-siblings".
fn tw_traverse_siblings(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    current: u32,
    forward: bool,
) -> Result<Option<u32>, ()> {
    let mut node = current;
    if tw_same(op_state, node, root) { return Ok(None); }
    loop {
        let mut sibling = if forward { tw_next_sibling(op_state, node) } else { tw_previous_sibling(op_state, node) };
        while let Some(s) = sibling {
            node = s;
            let result = tw_accept_node(scope, op_state, wts, filter, active, node)?;
            if result == 1 { return Ok(Some(node)); }
            // A SKIP'd sibling's own subtree is still in play; a REJECT's is not.
            sibling = if forward { tw_first_child(op_state, node) } else { tw_last_child(op_state, node) };
            if result == 2 || sibling.is_none() {
                sibling = if forward { tw_next_sibling(op_state, node) } else { tw_previous_sibling(op_state, node) };
            }
        }
        node = match tw_parent(op_state, node) { Some(p) => p, None => return Ok(None) };
        if tw_same(op_state, node, root) { return Ok(None); }
        if tw_accept_node(scope, op_state, wts, filter, active, node)? == 1 { return Ok(None); }
    }
}

/// `TreeWalker.previousNode()`: walk backwards through document order from `current`, bounded by
/// the root (which itself CAN be returned, when reached via the parent climb).
fn tw_previous_node(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    current: u32,
) -> Result<Option<u32>, ()> {
    let mut node = current;
    loop {
        if tw_same(op_state, node, root) { return Ok(None); }
        while let Some(ps) = tw_previous_sibling(op_state, node) {
            node = ps;
            let mut result = tw_accept_node(scope, op_state, wts, filter, active, node)?;
            if result == 2 { continue; } // REJECT: try the next earlier sibling.
            // Descend to the LAST accepted-or-skipped descendant (a REJECT stops the descent).
            while let Some(lc) = tw_last_child(op_state, node) {
                node = lc;
                result = tw_accept_node(scope, op_state, wts, filter, active, node)?;
                if result == 2 { break; }
            }
            if result == 1 { return Ok(Some(node)); }
        }
        let parent = match tw_parent(op_state, node) { Some(p) => p, None => return Ok(None) };
        node = parent;
        if tw_accept_node(scope, op_state, wts, filter, active, node)? == 1 {
            return Ok(Some(node));
        }
    }
}

/// `nextSkippingChildren(node, stayWithin)` — Blink's `NodeTraversal` helper: the next node in
/// document order ignoring `node`'s children, bounded by the `stayWithin` ancestor.
fn tw_next_skipping_children(
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    node: u32,
    stay_within: u32,
) -> Option<u32> {
    if tw_same(op_state, node, stay_within) { return None; }
    if let Some(ns) = tw_next_sibling(op_state, node) { return Some(ns); }
    let mut p = tw_parent(op_state, node);
    while let Some(par) = p {
        if tw_same(op_state, par, stay_within) { return None; }
        if let Some(ns) = tw_next_sibling(op_state, par) { return Some(ns); }
        p = tw_parent(op_state, par);
    }
    None
}

/// `TreeWalker.nextNode()`: descend first children, then next-siblings-skipping-children,
/// bounded by the root on the upward climb only (a currentNode outside the root still walks).
fn tw_next_node(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    wts: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    current: u32,
) -> Result<Option<u32>, ()> {
    let mut node = current;
    'outer: loop {
        // Children: descend first children until an ACCEPT (return) or a REJECT (stop this leg).
        while let Some(fc) = tw_first_child(op_state, node) {
            node = fc;
            match tw_accept_node(scope, op_state, wts, filter, active, node)? {
                1 => return Ok(Some(node)),
                2 => break,
                _ => {}
            }
        }
        // Siblings: the next node in document order; a SKIP re-enters its children (`goto Children`).
        while let Some(ns) = tw_next_skipping_children(op_state, node, root) {
            node = ns;
            match tw_accept_node(scope, op_state, wts, filter, active, node)? {
                1 => return Ok(Some(node)),
                3 => continue 'outer,
                _ => {}
            }
        }
        return Ok(None);
    }
}

// ---- the callbacks + ops ----------------------------------------------------------------------

/// Unwrap the TreeWalker receiver, throwing Chrome's "Illegal invocation" TypeError when `this`
/// is not one (a detached prototype method call).
fn tw_receiver(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<deno_core::cppgc::UnsafePtr<TreeWalker>> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<TreeWalker>(scope, args.this().into()) {
        Some(tw) => Some(tw),
        None => { throw_type_error(scope, "Illegal invocation"); None }
    }
}

/// Run one traversal method: on `Ok(Some(id))` the walker's currentNode moves to `id`; on
/// `Ok(None)` it returns null; on `Err(())` an exception is already pending.
fn tw_traverse(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    walk: fn(&mut v8::PinScope, &std::rc::Rc<std::cell::RefCell<OpState>>, u32, u32, &Option<v8::Global<v8::Value>>, &std::cell::Cell<bool>, u32) -> Result<Option<u32>, ()>,
) {
    let Some(tw) = tw_receiver(scope, args) else { return };
    let tw = unsafe { tw.as_ref() };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (root, wts, current) = (tw.root.get(), tw.what_to_show.get(), tw.current_node.get());
    let filter = tw.filter.borrow().clone();
    match walk(scope, &op_state, root, wts, &filter, &tw.active, current) {
        Ok(Some(id)) => { tw.current_node.set(id); tw_set_node(scope, Some(id), &mut rv); }
        Ok(None) => { let n = v8::null(scope); rv.set(n.into()); }
        Err(()) => {}
    }
}

pub(crate) fn cb_tw_parent_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, tw_parent_node);
}

pub(crate) fn cb_tw_first_child(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, |s, o, root, wts, filt, active, cur| tw_child_scan(s, o, root, wts, filt, active, cur, true));
}

pub(crate) fn cb_tw_last_child(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, |s, o, root, wts, filt, active, cur| tw_child_scan(s, o, root, wts, filt, active, cur, false));
}

pub(crate) fn cb_tw_next_sibling(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, |s, o, root, wts, filt, active, cur| tw_traverse_siblings(s, o, root, wts, filt, active, cur, true));
}

pub(crate) fn cb_tw_previous_sibling(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, |s, o, root, wts, filt, active, cur| tw_traverse_siblings(s, o, root, wts, filt, active, cur, false));
}

pub(crate) fn cb_tw_next_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, tw_next_node);
}

pub(crate) fn cb_tw_previous_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    tw_traverse(scope, &args, rv, tw_previous_node);
}

// ---- attribute accessors ----------------------------------------------------------------------

pub(crate) fn cb_tw_current_node_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = tw_receiver(scope, &args).map(|tw| unsafe { tw.as_ref() }.current_node.get());
    tw_set_node(scope, id, &mut rv);
}

/// `currentNode` setter: any Node, under the root or not (the spec allows positioning outside
/// the root; the traversal methods define what happens from there). A non-Node is the WebIDL
/// attribute TypeError.
pub(crate) fn cb_tw_current_node_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(tw) = tw_receiver(scope, &args) else { return };
    let nid = match arg_node_id(scope, args.get(0)) {
        Some(i) => i,
        None => {
            throw_type_error(scope, "Failed to set the 'currentNode' property on 'TreeWalker': The provided value is not of type 'Node'.");
            return;
        }
    };
    unsafe { tw.as_ref() }.current_node.set(nid);
}

pub(crate) fn cb_tw_root_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = tw_receiver(scope, &args).map(|tw| unsafe { tw.as_ref() }.root.get());
    tw_set_node(scope, id, &mut rv);
}

pub(crate) fn cb_tw_what_to_show_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(tw) = tw_receiver(scope, &args) {
        let n = crypto_num(scope, unsafe { tw.as_ref() }.what_to_show.get() as f64);
        rv.set(n);
    }
}

pub(crate) fn cb_tw_filter_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let f = tw_receiver(scope, &args).and_then(|tw| unsafe { tw.as_ref() }.filter.borrow().clone());
    match f {
        Some(g) => { let v = v8::Local::new(scope, &g); rv.set(v); }
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}


// ── moved out of bindings.rs (STEP 3 dissolution) ──
/// Wrap a NodeId result (or null) into rv. The document sentinel maps to the global `document`
/// object itself, so `walker.root === document` holds by identity (the sentinel must never reach
/// the arena: indexing `nodes[u32::MAX]` aborts).
pub(crate) fn tw_set_node(scope: &mut v8::PinScope, id: Option<u32>, rv: &mut v8::ReturnValue) {
    match id.and_then(|id| tw_node_value(scope, id)) {
        Some(v) => rv.set(v),
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}

/// The JS value for an arena id under the sentinel convention (see [`ON_DOCUMENT_ID`]): the
/// document sentinel IS the global `document` object, because a traversal's identity assertions
/// (`iter.root === document`) are written against it; every other id is the cached node wrapper.
///
/// ONE mapping, shared: what a traversal RETURNS and what it hands a FILTER must be the same
/// object, and `tw_accept_node` used to call `wrap_cached` directly -- which for the sentinel would
/// have minted a `JsElement { id: u32::MAX }` husk. Unreachable from `TreeWalker` (no traversal
/// passes the root to the filter), reachable the moment a NodeIterator rooted at `document`
/// filters its own root, which is what `document.rs`'s collection now does.
pub(crate) fn tw_node_value<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    if id == ON_DOCUMENT_ID {
        return global_get(scope, "document");
    }
    let op = deno_core::JsRuntime::op_state_from(scope);
    Some(wrap_cached(scope, &op, id).into())
}

// ── the TreeWalker and NodeFilter tables ───────────────────────────────────────────

use crate::core::dom::shared::{row_accessor, row_getter, row_method};
use crate::install::MemberDef;

/// The `TreeWalker` rows. The interface is the table's ordinary non-newable one (`core/dom/spine.rs`), whose
/// constructor answers Chrome's two sentences for `new X()` and `X()` -- which a `#[constructor]` body could
/// not tell apart (the reason for `over_class`, deleted with the op2 class).
pub fn install_tree_walker(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "root" => row_getter(scope, proto, member, cb_tw_root_get),
        "whatToShow" => row_getter(scope, proto, member, cb_tw_what_to_show_get),
        "filter" => row_getter(scope, proto, member, cb_tw_filter_get),
        "currentNode" => row_accessor(scope, proto, member, cb_tw_current_node_get, cb_tw_current_node_set),
        "firstChild" => row_method(scope, proto, member, cb_tw_first_child),
        "lastChild" => row_method(scope, proto, member, cb_tw_last_child),
        "nextNode" => row_method(scope, proto, member, cb_tw_next_node),
        "nextSibling" => row_method(scope, proto, member, cb_tw_next_sibling),
        "parentNode" => row_method(scope, proto, member, cb_tw_parent_node),
        "previousNode" => row_method(scope, proto, member, cb_tw_previous_node),
        "previousSibling" => row_method(scope, proto, member, cb_tw_previous_sibling),
        other => debug_assert!(false, "install_tree_walker: unexpected member {other}"),
    }
}

webapi::interface! {
    "TreeWalker" [interface, window] {
        ctor "TreeWalker" len 0 = stub,
        attr "root" : Object readonly = install_tree_walker,
        attr "whatToShow" : Object readonly = install_tree_walker,
        attr "filter" : Object readonly nullable = install_tree_walker,
        attr "currentNode" : Object = install_tree_walker,
        method "firstChild" : Object len 0 nullable = install_tree_walker,
        method "lastChild" : Object len 0 nullable = install_tree_walker,
        method "nextNode" : Object len 0 nullable = install_tree_walker,
        method "nextSibling" : Object len 0 nullable = install_tree_walker,
        method "parentNode" : Object len 0 nullable = install_tree_walker,
        method "previousNode" : Object len 0 nullable = install_tree_walker,
        method "previousSibling" : Object len 0 nullable = install_tree_walker
    }
}

// `NodeFilter` is a CALLBACK interface with constants: a function-typed object with no `prototype`,
// the sixteen constants as own properties (Chrome, 2026-09-10: `own = length,name,FILTER_ACCEPT,...`,
// `new NodeFilter()` "is not a constructor", `NodeFilter()` "Illegal constructor"). The bundle's
// surface has no member list for it (nothing to enumerate on a prototype it does not have), so the
// rows are WebIDL's, and only the static half exists: `core/dom/spine.rs` mounts them onto a plain
// native function through the one member mount.
webapi::interface! {
    "NodeFilter" [interface, window] {
        ctor "NodeFilter" len 0 = stub,
        static_constant "FILTER_ACCEPT" : Integer = 1,
        static_constant "FILTER_REJECT" : Integer = 2,
        static_constant "FILTER_SKIP" : Integer = 3,
        static_constant "SHOW_ALL" : Integer = 4294967295,
        static_constant "SHOW_ELEMENT" : Integer = 1,
        static_constant "SHOW_ATTRIBUTE" : Integer = 2,
        static_constant "SHOW_TEXT" : Integer = 4,
        static_constant "SHOW_CDATA_SECTION" : Integer = 8,
        static_constant "SHOW_ENTITY_REFERENCE" : Integer = 16,
        static_constant "SHOW_ENTITY" : Integer = 32,
        static_constant "SHOW_PROCESSING_INSTRUCTION" : Integer = 64,
        static_constant "SHOW_COMMENT" : Integer = 128,
        static_constant "SHOW_DOCUMENT" : Integer = 256,
        static_constant "SHOW_DOCUMENT_TYPE" : Integer = 512,
        static_constant "SHOW_DOCUMENT_FRAGMENT" : Integer = 1024,
        static_constant "SHOW_NOTATION" : Integer = 2048
    }
}

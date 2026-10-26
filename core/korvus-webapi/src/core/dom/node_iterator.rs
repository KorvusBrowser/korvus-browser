//! `core/dom/node_iterator.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::core::dom::tree_walker::{tw_set_node};
use crate::modules::crypto::shared::{crypto_num};

#[repr(C)]
#[derive(CppgcBase)]
pub struct NodeIterator {
    pub(crate) root: std::cell::Cell<u32>,
    pub(crate) what_to_show: std::cell::Cell<u32>,
    pub(crate) filter: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) list: std::cell::RefCell<Vec<u32>>,
    pub(crate) idx: std::cell::Cell<isize>,
    pub(crate) ref_node: std::cell::Cell<u32>,
    pub(crate) pointer_before: std::cell::Cell<bool>,
}

// NodeIterator has no WebIDL constructor either (only document.createNodeIterator builds one),
// and the interface object is now exposed -- so `new` must throw Chrome's TypeError, not the
// behavior-stub's DOMException (which is what the shell it replaced threw).
//
// `globalThis.NodeIterator` is the table's non-newable interface object (`core/dom/spine.rs`), because
// Chrome answers `new X()` and `X()` differently. An OWN class with no member of its own:
// the struct is cppgc-wrappable and `binding::class::mint` puts an iterator on the realm's
// `NodeIterator.prototype`. (Until P6.2 this was an op2 class whose `#[constructor]` threw, kept so
// deno_core realised a class template to mint on.)
#[webapi(base, own)]
impl NodeIterator {}

pub(crate) fn cb_ni_next_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()) {
        Some(it) => { let it = unsafe { it.as_ref() }; let len = it.list.borrow().len() as isize; let i = it.idx.get(); if i + 1 < len { let ni = it.list.borrow()[(i + 1) as usize]; it.idx.set(i + 1); it.ref_node.set(ni); it.pointer_before.set(false); Some(ni) } else { None } }
        None => return,
    };
    tw_set_node(scope, id, &mut rv);
}
pub(crate) fn cb_ni_previous_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()) {
        Some(it) => { let it = unsafe { it.as_ref() }; let i = it.idx.get(); if i >= 0 { let pi = it.list.borrow()[i as usize]; it.ref_node.set(pi); it.pointer_before.set(true); it.idx.set(i - 1); Some(pi) } else { None } }
        None => return,
    };
    tw_set_node(scope, id, &mut rv);
}
pub(crate) fn cb_ni_detach(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}
pub(crate) fn cb_ni_reference_node_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()).map(|it| unsafe { it.as_ref() }.ref_node.get());
    tw_set_node(scope, id, &mut rv);
}
pub(crate) fn cb_ni_pointer_before_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(it) = deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()) { let b: v8::Local<v8::Value> = v8::Boolean::new(scope, unsafe { it.as_ref() }.pointer_before.get()).into(); rv.set(b); }
}
pub(crate) fn cb_ni_root_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()).map(|it| unsafe { it.as_ref() }.root.get());
    tw_set_node(scope, id, &mut rv);
}
pub(crate) fn cb_ni_what_to_show_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(it) = deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()) { let n = crypto_num(scope, unsafe { it.as_ref() }.what_to_show.get() as f64); rv.set(n); }
}
pub(crate) fn cb_ni_filter_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match deno_core::cppgc::try_unwrap_cppgc_object::<NodeIterator>(scope, args.this().into()).and_then(|it| unsafe { it.as_ref() }.filter.borrow().clone()) {
        Some(g) => { let v = v8::Local::new(scope, &g); rv.set(v); } None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}


// ── the NodeIterator table ─────────────────────────────────────────────────────────

use crate::core::dom::shared::{row_getter, row_method};
use crate::install::MemberDef;

/// The `NodeIterator` rows, installed onto the cppgc class prototype.
pub fn install_node_iterator(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "root" => row_getter(scope, proto, member, cb_ni_root_get),
        "referenceNode" => row_getter(scope, proto, member, cb_ni_reference_node_get),
        "pointerBeforeReferenceNode" => row_getter(scope, proto, member, cb_ni_pointer_before_get),
        "whatToShow" => row_getter(scope, proto, member, cb_ni_what_to_show_get),
        "filter" => row_getter(scope, proto, member, cb_ni_filter_get),
        "detach" => row_method(scope, proto, member, cb_ni_detach),
        "nextNode" => row_method(scope, proto, member, cb_ni_next_node),
        "previousNode" => row_method(scope, proto, member, cb_ni_previous_node),
        other => debug_assert!(false, "install_node_iterator: unexpected member {other}"),
    }
}

webapi::interface! {
    "NodeIterator" [interface, window] {
        ctor "NodeIterator" len 0 = stub,
        attr "root" : Object readonly = install_node_iterator,
        attr "referenceNode" : Object readonly = install_node_iterator,
        attr "pointerBeforeReferenceNode" : Object readonly = install_node_iterator,
        attr "whatToShow" : Object readonly = install_node_iterator,
        attr "filter" : Object readonly nullable = install_node_iterator,
        method "detach" : Object len 0 = install_node_iterator,
        method "nextNode" : Object len 0 nullable = install_node_iterator,
        method "previousNode" : Object len 0 nullable = install_node_iterator
    }
}

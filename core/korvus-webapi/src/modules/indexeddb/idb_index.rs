//! `modules/indexeddb/idb_index.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use super::*;            // sibling Idb* interfaces (re-exported by mod.rs)
use crate::binding::protos::str_val;
use crate::modules::indexeddb::backend::{IdbBackend, idb_l, idb_recv, idb_throw};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbIndex {
    pub(crate) db_name: String,
    pub(crate) store_name: String,
    pub(crate) index_name: std::cell::RefCell<String>, // mutable: the `name` setter renames the index in place
    pub(crate) store: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) tx: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) key_path_cache: std::cell::RefCell<Option<v8::Global<v8::Value>>>, // a sequence `keyPath`'s array, handed out again
}

pub(crate) trait IdbIndexBehavior {
   fn construct() -> IdbIndex where Self: Sized { stub!("IdbIndex::constructor") }
   fn name(&self) -> String { stub!("IdbIndex::name") }
}

#[webapi(base, own, name = "IDBIndex")]
impl IdbIndex {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbIndex, ::deno_error::JsErrorBox> { dispatch!("IdbIndex::constructor", <IdbIndex as IdbIndexBehavior>::construct()) }
    #[getter]
    #[string]
    fn name(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbIndex::name", <IdbIndex as IdbIndexBehavior>::name(self)) }
}

impl IdbIndexBehavior for IdbIndex {
    fn construct() -> IdbIndex { IdbIndex { db_name: String::new(), store_name: String::new(), index_name: std::cell::RefCell::new(String::new()), store: std::cell::RefCell::new(None), tx: std::cell::RefCell::new(None), key_path_cache: std::cell::RefCell::new(None) } }
    fn name(&self) -> String { self.index_name.borrow().clone() }
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `name`'s GETTER was an `#[op2] #[getter]` member of the class template above until the W5.1 L7
// cutover, and its setter was grafted alongside it from JS. An instance now carries the `IDBIndex`
// TABLE's prototype, and the row is a plain read/write accessor over these two callbacks.
pub(crate) fn cb_index_name_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ixh = idb_recv!(scope, args.this().into(), IdbIndex);
    let n = unsafe { ixh.as_ref() }.index_name.borrow().clone();
    let v = str_val(scope, &n);
    rv.set(v);
}

pub(crate) fn cb_index_set_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let ixh = idb_recv!(scope, this.into(), IdbIndex);
    let db_name = unsafe { ixh.as_ref() }.db_name.clone();
    let store_name = unsafe { ixh.as_ref() }.store_name.clone();
    let old = unsafe { ixh.as_ref() }.index_name.borrow().clone();
    let tx_g = match unsafe { ixh.as_ref() }.tx.borrow().clone() { Some(t) => t, None => { idb_throw(scope, "InvalidStateError", "the index has been deleted"); return; } };
    let new_name = args.get(0).to_rust_string_lossy(scope);
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    // The setter's steps, in the spec's order -- the SAME three `cb_store_set_name` runs, and the
    // same two defects `CONF IDB1.1` fixed there. IDB1 named this copy and left it because it was
    // outside its file.
    //
    // 1. "has been deleted". Missing entirely: `store.deleteIndex(name)` leaves the handle's `tx` in
    //    place, so the `None` branch above is dead code and a rename of a deleted index fell through
    //    to the backend, found no record, and returned SILENTLY (status 2).
    if crate::modules::indexeddb::idb_object_store::idb_source_deleted(scope, &db_name, &store_name, Some(&old)) { idb_throw(scope, "InvalidStateError", "the index has been deleted"); return; }
    // 2. "not an upgrade transaction". `finished` was folded in HERE, and a finished transaction is
    //    an INACTIVE one, not a non-upgrade one -- so a rename in a finished transaction reported
    //    InvalidStateError where the spec says TransactionInactiveError.
    if !unsafe { txh.as_ref() }.is_upgrade.get() { idb_throw(scope, "InvalidStateError", "the index's name can only be set in a versionchange transaction"); return; }
    // 3. "not active".
    if unsafe { txh.as_ref() }.finished.get() || !unsafe { txh.as_ref() }.active.get() { idb_throw(scope, "TransactionInactiveError", "the transaction is not active"); return; }
    if new_name == old { return; }
    let status: u8 = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.borrow_mut::<IdbBackend>().db_mut(&db_name).and_then(|d| d.stores.get_mut(&store_name)) {
            Some(s) if s.indexes.iter().any(|i| i.name == new_name) => 1,
            Some(s) => match s.indexes.iter_mut().find(|i| i.name == old) { Some(ix) => { ix.name = new_name.clone(); 0 } None => 2 },
            None => 2,
        }
    };
    if status == 1 { idb_throw(scope, "ConstraintError", "an index with the given name already exists"); return; }
    if status == 2 { return; }
    *unsafe { ixh.as_ref() }.index_name.borrow_mut() = new_name.clone();
    let store_g = unsafe { ixh.as_ref() }.store.borrow().clone();
    if let Some(sg) = store_g {
        let sv = idb_l(scope, &sg);
        if let Some(osh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, sv) {
            let mut ih = unsafe { osh.as_ref() }.index_handles.borrow_mut();
            if let Some(h) = ih.remove(&old) { ih.insert(new_name.clone(), h); }
        }
    }
}
fn idb_index_meta(scope: &mut v8::PinScope, db: &str, store: &str, index: &str) -> Option<(String, bool, bool)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let i = st.borrow::<IdbBackend>().db(db)?.stores.get(store)?.indexes.iter().find(|x| x.name == index)?;
    Some((i.key_path.clone(), i.unique, i.multi_entry))
}
/// (db_name, store_name, index_name, objectStore handle) from an IdbIndex `this`.
///
/// A receiver that is NOT an `IDBIndex` is the WebIDL TypeError ("Illegal invocation"), which is what
/// the bundle's oracle records for all five of this interface's attributes; it used to answer
/// `undefined` because the failed unwrap was swallowed by the `?`.
fn idb_index_ctx(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<(String, String, String, Option<v8::Global<v8::Value>>)> {
    let ixh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbIndex>(scope, this) {
        Some(h) => h,
        None => {
            crate::binding::protos::throw_type_error(scope, "Illegal invocation");
            return None;
        }
    };
    let r = unsafe { ixh.as_ref() };
    let (db, store, index) = (r.db_name.clone(), r.store_name.clone(), r.index_name.borrow().clone());
    let s = r.store.borrow().clone();
    Some((db, store, index, s))
}
pub(crate) fn cb_index_key_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (db, store, index, _s) = match idb_index_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    match idb_index_meta(scope, &db, &store, &index) {
        Some((kp, _, _)) => {
            if kp.contains('\u{0}') {
                // the same array each read (`index.keyPath === index.keyPath`, Chrome 150)
                let this: v8::Local<v8::Value> = args.this().into();
                let handle = deno_core::cppgc::try_unwrap_cppgc_object::<IdbIndex>(scope, this);
                if let Some(cached) = handle.as_ref().and_then(|h| unsafe { h.as_ref() }.key_path_cache.borrow().clone()) {
                    let v = v8::Local::new(scope, &cached);
                    rv.set(v);
                    return;
                }
                let arr = crate::modules::indexeddb::backend::key_path_array(scope, &kp);
                if let Some(h) = handle {
                    *unsafe { h.as_ref() }.key_path_cache.borrow_mut() = Some(v8::Global::new(scope, arr));
                }
                rv.set(arr);
            } else { let v = str_val(scope, &kp); rv.set(v); }
        }
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}
pub(crate) fn cb_index_unique(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (db, store, index, _s) = match idb_index_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    rv.set_bool(idb_index_meta(scope, &db, &store, &index).map(|(_, u, _)| u).unwrap_or(false));
}
pub(crate) fn cb_index_multi_entry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (db, store, index, _s) = match idb_index_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    rv.set_bool(idb_index_meta(scope, &db, &store, &index).map(|(_, _, m)| m).unwrap_or(false));
}
pub(crate) fn cb_index_object_store(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (_db, _store, _index, s) = match idb_index_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    if let Some(s) = s { let v = idb_l(scope, &s); rv.set(v); }
}


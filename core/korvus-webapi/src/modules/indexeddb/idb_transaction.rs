//! `modules/indexeddb/idb_transaction.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::modules::indexeddb::backend::{IdbBackend, IdbPending, IdbUndo, idb_dom_exception, idb_finish_tx, idb_g, idb_l, idb_make_object_store, idb_new_tx, idb_recv, idb_string_list, idb_throw};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbTransaction {
    pub(crate) db_name: String,
    pub(crate) db: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) mode: String,
    pub(crate) scope: std::cell::RefCell<Vec<String>>,
    pub(crate) pending: std::cell::RefCell<std::collections::VecDeque<IdbPending>>,
    pub(crate) undo: std::cell::RefCell<Vec<IdbUndo>>,
    pub(crate) error: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) open_req: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) store_handles: std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Value>>>,
    pub(crate) active: std::cell::Cell<bool>,
    pub(crate) finished: std::cell::Cell<bool>,
    pub(crate) committing: std::cell::Cell<bool>,
    pub(crate) aborted: std::cell::Cell<bool>,
    pub(crate) is_upgrade: std::cell::Cell<bool>,
    pub(crate) pumping: std::cell::Cell<bool>,
    pub(crate) upgrade_old: std::cell::Cell<f64>,
}

pub(crate) trait IdbTransactionBehavior {
   fn construct() -> IdbTransaction where Self: Sized { stub!("IdbTransaction::constructor") }
   fn mode(&self) -> String { stub!("IdbTransaction::mode") }
   fn durability(&self) -> String { stub!("IdbTransaction::durability") }
}

#[webapi(base, own, name = "IDBTransaction")]
impl IdbTransaction {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbTransaction, ::deno_error::JsErrorBox> { dispatch!("IdbTransaction::constructor", <IdbTransaction as IdbTransactionBehavior>::construct()) }
    #[getter]
    #[string]
    fn mode(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbTransaction::mode", <IdbTransaction as IdbTransactionBehavior>::mode(self)) }
    #[getter]
    #[string]
    fn durability(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbTransaction::durability", <IdbTransaction as IdbTransactionBehavior>::durability(self)) }
}

impl IdbTransactionBehavior for IdbTransaction {
    fn construct() -> IdbTransaction { idb_new_tx(String::new(), Vec::new(), "readonly".to_string()) }
    fn mode(&self) -> String { self.mode.clone() }
    fn durability(&self) -> String { "default".to_string() }
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `mode` and `durability` were `#[op2] #[getter]` members of the cppgc class template above until the
// W5.1 L7 cutover. An instance now carries the TABLE's prototype, so the template's own members are not
// reachable from any realm; these two raw callbacks are the bodies the `IDBTransaction` table names, and
// they answer the same values (`self.mode`; `"default"`, which Chrome confirms for both a
// `versionchange` and a `readwrite` transaction -- probed 2026-09-10).

pub(crate) fn cb_tx_mode(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let txh = idb_recv!(scope, args.this().into(), IdbTransaction);
    let m = unsafe { txh.as_ref() }.mode.clone();
    let v = crate::binding::protos::str_val(scope, &m);
    rv.set(v);
}

pub(crate) fn cb_tx_durability(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbTransaction);
    let v = crate::binding::protos::str_val(scope, "default");
    rv.set(v);
}

pub(crate) fn cb_tx_object_store(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let txh = idb_recv!(scope, this.into(), IdbTransaction);
    if unsafe { txh.as_ref() }.finished.get() { idb_throw(scope, "InvalidStateError", "transaction finished"); return; }
    let name = args.get(0).to_rust_string_lossy(scope);
    if let Some(h) = unsafe { txh.as_ref() }.store_handles.borrow().get(&name) {
        let v = idb_l(scope, h);
        rv.set(v);
        return;
    }
    let db_name = unsafe { txh.as_ref() }.db_name.clone();
    let exists = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&db_name).map(|d| d.stores.contains_key(&name)).unwrap_or(false)
    };
    if !exists { idb_throw(scope, "NotFoundError", "no object store named"); return; }
    let os = idb_make_object_store(scope, &db_name, &name, this.into(), None);
    unsafe { txh.as_ref() }.store_handles.borrow_mut().insert(name, idb_g(scope, os));
    rv.set(os);
}
pub(crate) fn cb_tx_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let tx_val = args.this().into();
    let txh = idb_recv!(scope, tx_val, IdbTransaction);
    if unsafe { txh.as_ref() }.committing.get() { idb_throw(scope, "InvalidStateError", "transaction is committing"); return; }
    if unsafe { txh.as_ref() }.finished.get() { idb_throw(scope, "InvalidStateError", "transaction finished"); return; }
    let ex = idb_dom_exception(scope, "AbortError", "aborted");
    *unsafe { txh.as_ref() }.error.borrow_mut() = Some(idb_g(scope, ex));
    idb_finish_tx(scope, tx_val, "abort");
}
pub(crate) fn cb_tx_commit(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let txh = idb_recv!(scope, args.this().into(), IdbTransaction);
    if unsafe { txh.as_ref() }.finished.get() { idb_throw(scope, "InvalidStateError", "transaction finished"); return; }
    unsafe { txh.as_ref() }.committing.set(true);
}
pub(crate) fn cb_tx_store_names(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let txh = idb_recv!(scope, args.this().into(), IdbTransaction);
    let names = unsafe { txh.as_ref() }.scope.borrow().clone();
    let list = idb_string_list(scope, &names);
    rv.set(list);
}
pub(crate) fn cb_tx_db(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let txh = idb_recv!(scope, args.this().into(), IdbTransaction);
    let db = unsafe { txh.as_ref() }.db.borrow().clone();
    if let Some(db) = db {
        let v = idb_l(scope, &db);
        rv.set(v);
    }
}

// `error` is in the capture and the field has been on this struct all along -- only the getter was
// missing, so a page could never see WHY a transaction aborted. Recorded from Brave: a get-only
// enumerable accessor, null while nothing has failed.
pub(crate) fn cb_tx_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = idb_recv!(scope, args.this().into(), IdbTransaction);
    let err = unsafe { t.as_ref() }.error.borrow().clone();
    match err {
        Some(g) => { let v = v8::Local::new(scope, &g); rv.set(v); }
        None => rv.set_null(),
    }
}


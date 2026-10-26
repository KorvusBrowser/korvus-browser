//! `modules/indexeddb/idb_database.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use super::*;            // sibling Idb* interfaces (re-exported by mod.rs)
use crate::binding::protos::{build_fn_data, get_prop, str_val};
use crate::modules::indexeddb::backend::{IdbBackend, cb_idb_clear_active, StoreRec, idb_g, idb_l, idb_make_object_store, idb_make_transaction, idb_new_tx, idb_pump, idb_queue_microtask, idb_recv, idb_string_list, idb_throw};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbDatabase {
    pub(crate) db_name: String,
    pub(crate) closed: std::cell::Cell<bool>,
    pub(crate) active_upgrade_tx: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
}

pub(crate) trait IdbDatabaseBehavior {
   fn construct() -> IdbDatabase where Self: Sized { stub!("IdbDatabase::constructor") }
   fn name(&self) -> String { stub!("IdbDatabase::name") }
}

#[webapi(base, own, name = "IDBDatabase")]
impl IdbDatabase {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbDatabase, ::deno_error::JsErrorBox> { dispatch!("IdbDatabase::constructor", <IdbDatabase as IdbDatabaseBehavior>::construct()) }
    #[getter]
    #[string]
    fn name(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbDatabase::name", <IdbDatabase as IdbDatabaseBehavior>::name(self)) }
}

impl IdbDatabaseBehavior for IdbDatabase {
    fn construct() -> IdbDatabase { IdbDatabase { db_name: String::new(), closed: std::cell::Cell::new(false), active_upgrade_tx: std::cell::RefCell::new(None) } }
    fn name(&self) -> String { self.db_name.clone() }
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `name` was an `#[op2] #[getter]` member of the class template above until the W5.1 L7 cutover. An
// instance now carries the `IDBDatabase` TABLE's prototype, so the template's own member is not
// reachable from any realm and this raw callback is the row's body.
pub(crate) fn cb_db_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    // the database's own name: a storage bucket's database is keyed by its bucket too (`idb_factory::backend_key`)
    let name = crate::modules::indexeddb::idb_factory::display_name(&unsafe { dbh.as_ref() }.db_name).to_string();
    let v = str_val(scope, &name);
    rv.set(v);
}

pub(crate) fn cb_db_version(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    let name = unsafe { dbh.as_ref() }.db_name.clone();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let v = st.borrow::<IdbBackend>().db(&name).map(|d| d.version).unwrap_or(0.0);
    rv.set_double(v);
}
pub(crate) fn cb_db_store_names(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    let name = unsafe { dbh.as_ref() }.db_name.clone();
    let names: Vec<String> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&name).map(|d| d.stores.keys().cloned().collect()).unwrap_or_default()
    };
    let list = idb_string_list(scope, &names);
    rv.set(list);
}
pub(crate) fn cb_db_create_store(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    let db_name = unsafe { dbh.as_ref() }.db_name.clone();
    let up = unsafe { dbh.as_ref() }.active_upgrade_tx.borrow().clone();
    let tx_g = match up { Some(t) => t, None => { idb_throw(scope, "InvalidStateError", "createObjectStore outside a versionchange transaction"); return } };
    let name = args.get(0).to_rust_string_lossy(scope);
    // `IDBObjectStoreParameters`, converted as WebIDL converts it (Chrome 150): `autoIncrement`,
    // then `keyPath`, each read once (korvus read `keyPath` first)
    let Ok(opts) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "IDBObjectStoreParameters", "Failed to execute 'createObjectStore' on 'IDBDatabase': ") else { return };
    let (key_path, auto_inc) = idb_parse_store_opts(scope, opts.into());
    // exists?
    let exists = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&db_name).map(|d| d.stores.contains_key(&name)).unwrap_or(false)
    };
    // the spec's order: an invalid key path, then a name in use, then autoIncrement over an empty or sequence path
    if key_path.as_deref().is_some_and(|kp| !crate::modules::indexeddb::backend::idb_valid_key_path(kp)) {
        idb_throw(scope, "SyntaxError", "The keyPath option is not a valid key path.");
        return;
    }
    if exists { idb_throw(scope, "ConstraintError", "object store exists"); return; }
    if let Some(kp) = &key_path {
        if auto_inc && (kp.is_empty() || kp.contains('\u{0}')) {
            idb_throw(scope, "InvalidAccessError", "The autoIncrement option was set but the keyPath option was empty or an array.");
            return;
        }
    }
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(d) = st.borrow_mut::<IdbBackend>().db_mut(&db_name) {
            d.stores.insert(name.clone(), StoreRec { key_path, auto_increment: auto_inc, key_gen: 1.0, records: Vec::new(), indexes: Vec::new() });
        }
    }
    let tx_val = idb_l(scope, &tx_g);
    if let Some(txh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) {
        let mut sc = unsafe { txh.as_ref() }.scope.borrow_mut();
        if !sc.contains(&name) { sc.push(name.clone()); }
    }
    let os = idb_make_object_store(scope, &db_name, &name, tx_val, None);
    // Cache the handle on the tx so tx.objectStore(name) returns this very object (identity).
    if let Some(txh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) {
        unsafe { txh.as_ref() }.store_handles.borrow_mut().insert(name.clone(), idb_g(scope, os));
    }
    rv.set(os);
}
fn idb_parse_store_opts(scope: &mut v8::PinScope, opts: v8::Local<v8::Value>) -> (Option<String>, bool) {
    if let Ok(o) = v8::Local::<v8::Object>::try_from(opts) {
        // `(DOMString or sequence<DOMString>)? keyPath = null`: a sequence as its parts joined by NUL, as an index's
        // (YouTube's `ResponseStore`: `["key", "clientName"]`, which was taken for no key path at all)
        let kp = get_prop(scope, o, "keyPath")
            .filter(|v| !v.is_null_or_undefined())
            .map(|v| crate::modules::indexeddb::idb_object_store::idb_index_key_path(scope, v));
        let ai = get_prop(scope, o, "autoIncrement").map(|v| v.boolean_value(scope)).unwrap_or(false);
        (kp, ai)
    } else {
        (None, false)
    }
}
pub(crate) fn cb_db_delete_store(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    let db_name = unsafe { dbh.as_ref() }.db_name.clone();
    if unsafe { dbh.as_ref() }.active_upgrade_tx.borrow().is_none() { idb_throw(scope, "InvalidStateError", "deleteObjectStore outside a versionchange transaction"); return; }
    let name = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(d) = st.borrow_mut::<IdbBackend>().db_mut(&db_name) {
        if d.stores.remove(&name).is_none() { drop(st); idb_throw(scope, "NotFoundError", "no object store named"); }
    }
}
pub(crate) fn cb_db_transaction(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let dbh = idb_recv!(scope, this.into(), IdbDatabase);
    if unsafe { dbh.as_ref() }.closed.get() { idb_throw(scope, "InvalidStateError", "connection is closed"); return; }
    if unsafe { dbh.as_ref() }.active_upgrade_tx.borrow().is_some() { idb_throw(scope, "InvalidStateError", "a versionchange transaction is running"); return; }
    let db_name = unsafe { dbh.as_ref() }.db_name.clone();
    let names_arg = args.get(0);
    let names: Vec<String> = if names_arg.is_array() {
        let a = v8::Local::<v8::Array>::try_from(names_arg).unwrap();
        (0..a.length()).filter_map(|i| a.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope))).collect()
    } else {
        vec![names_arg.to_rust_string_lossy(scope)]
    };
    // `optional IDBTransactionMode mode = "readonly"` (an enum: anything else refused in Blink's sentence) and
    // `optional IDBTransactionOptions options = {}` -- `durability` read once (Chrome 150; korvus
    // took any string for a mode and read no options)
    let mode = if args.get(1).is_undefined() { "readonly".to_string() } else {
        let Some(m) = crate::binding::class::idl_string(scope, args.get(1), "Failed to execute 'transaction' on 'IDBDatabase': ") else { return };
        if !matches!(m.as_str(), "readonly" | "readwrite" | "versionchange") {
            crate::binding::protos::throw_type_error(scope, &format!("Failed to execute 'transaction' on 'IDBDatabase': The provided value '{m}' is not a valid enum value of type IDBTransactionMode."));
            return;
        }
        m
    };
    if crate::install::ctor_dicts::convert_dict(scope, args.get(2), "IDBTransactionOptions", "Failed to execute 'transaction' on 'IDBDatabase': ").is_err() {
        return;
    }
    // Current with the context before it reads; a write transaction then holds its records as this realm's.
    super::sync::pull(scope, &db_name);
    if mode != "readonly" {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(backend) = st.try_borrow_mut::<IdbBackend>() {
            super::sync::writer_began(backend, &db_name);
        }
    }
    let tx = idb_new_tx(db_name, names, mode);
    let txo = idb_make_transaction(scope, tx);
    let tx_val: v8::Local<v8::Value> = txo.into();
    if let Some(txh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) {
        *unsafe { txh.as_ref() }.db.borrow_mut() = Some(idb_g(scope, this.into()));
        unsafe { txh.as_ref() }.active.set(true);
    }
    // Close the active window on a microtask (so it survives this task's checkpoint), then pump.
    // ONE clear-the-active-flag callback for all three sites that need it -- `backend.rs`'s
    // `cb_idb_clear_active`. This file used to carry a private second copy of the same three lines.
    let closer = build_fn_data(scope, cb_idb_clear_active, "", 0, tx_val);
    idb_queue_microtask(scope, closer);
    idb_pump(scope, tx_val);
    rv.set(tx_val);
}
pub(crate) fn cb_db_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let dbh = idb_recv!(scope, args.this().into(), IdbDatabase);
    unsafe { dbh.as_ref() }.closed.set(true);
    let name = unsafe { dbh.as_ref() }.db_name.clone();
    crate::modules::indexeddb::idb_factory::idb_resume_waiters(scope, &name);
}


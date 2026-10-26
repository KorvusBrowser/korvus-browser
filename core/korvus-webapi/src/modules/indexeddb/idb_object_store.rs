//! `modules/indexeddb/idb_object_store.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::idb::*;      // IndexedDB backend (Key/KeyRange/…)
use super::*;            // sibling Idb* interfaces (re-exported by mod.rs)
use crate::binding::protos::{get_prop, str_val};
use crate::modules::indexeddb::backend::{IdbBackend, IdbPending, IdxRec, extract_key_val, idb_clone_global, idb_do_delete, idb_do_write, idb_g, idb_index_keys, idb_l, idb_make_request, idb_pump, idb_recv, idb_set_proto, idb_string_list, idb_structured_clone, idb_throw, js_to_key, key_to_js};
use crate::modules::indexeddb::tables::idb_make_record;

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// IDBObjectStore getters.
// IDBObjectStore.name / IDBIndex.name setters (rename). Per spec the name may only be set inside an
// active `versionchange` (upgrade) transaction; a rename re-keys the backend record AND every cached
// handle (the store handle's own name, tx.scope, tx.store_handles, the parent store's index_handles)
// so identity, tx.objectStoreNames, db.objectStoreNames and objectStore(newName)/index(newName) stay
// coherent afterwards -- exactly the cascade createObjectStore/createIndex perform on insert.

fn extract_key<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'s, v8::Value>, key_path: &str) -> Option<Key> {
    let v = extract_key_val(scope, value, key_path)?;
    js_to_key(scope, v)
}

/// Normalize a query arg (key / IDBKeyRange / null) into a KeyRange; `Err` = DataError (invalid key).
fn to_range_arg(scope: &mut v8::PinScope, q: v8::Local<v8::Value>) -> Result<KeyRange, ()> {
    if q.is_null_or_undefined() {
        return Ok(KeyRange::all());
    }
    if let Some(kr) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbKeyRange>(scope, q) {
        return Ok(unsafe { kr.as_ref() }.to_range());
    }
    match js_to_key(scope, q) {
        Some(k) => Ok(KeyRange::only(k)),
        None => Err(()),
    }
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbObjectStore {
    pub(crate) db_name: String,
    pub(crate) store_name: std::cell::RefCell<String>, // mutable: the `name` setter renames the store in place
    pub(crate) tx: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) index_name: std::cell::RefCell<Option<String>>, // set when this handle is an index-backed source
    pub(crate) index_handles: std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Value>>>, // per-store index handle cache (identity)
    pub(crate) key_path_cache: std::cell::RefCell<Option<v8::Global<v8::Value>>>, // a sequence `keyPath`'s array, handed out again
}

pub(crate) trait IdbObjectStoreBehavior {
   fn construct() -> IdbObjectStore where Self: Sized { stub!("IdbObjectStore::constructor") }
   fn name(&self) -> String { stub!("IdbObjectStore::name") }
}

#[webapi(base, own, name = "IDBObjectStore")]
impl IdbObjectStore {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbObjectStore, ::deno_error::JsErrorBox> { dispatch!("IdbObjectStore::constructor", <IdbObjectStore as IdbObjectStoreBehavior>::construct()) }
    #[getter]
    #[string]
    fn name(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbObjectStore::name", <IdbObjectStore as IdbObjectStoreBehavior>::name(self)) }
}

impl IdbObjectStoreBehavior for IdbObjectStore {
    fn construct() -> IdbObjectStore { IdbObjectStore { db_name: String::new(), store_name: std::cell::RefCell::new(String::new()), tx: std::cell::RefCell::new(None), index_name: std::cell::RefCell::new(None), index_handles: std::cell::RefCell::new(std::collections::HashMap::new()), key_path_cache: std::cell::RefCell::new(None) } }
    fn name(&self) -> String { self.store_name.borrow().clone() }
}

/// Collect {key, primaryKey, value-clone} entries a query walks (store or index source), sorted.
fn idb_collect(scope: &mut v8::PinScope, backend: &IdbBackend, db: &str, store: &str, index: Option<&str>, range: &KeyRange) -> Vec<(Key, Key, v8::Global<v8::Value>)> {
    let s = match backend.db(db).and_then(|d| d.stores.get(store)) { Some(s) => s, None => return Vec::new() };
    let mut out: Vec<(Key, Key, v8::Global<v8::Value>)> = Vec::new();
    if let Some(iname) = index {
        let idx = match s.indexes.iter().find(|x| x.name == iname) { Some(x) => x, None => return Vec::new() };
        for (pk, val) in &s.records {
            let vloc = v8::Local::new(scope, val);
            for ik in idb_index_keys(scope, idx, vloc) {
                if range.includes(&ik) {
                    out.push((ik, pk.clone(), idb_clone_global(scope, val)));
                }
            }
        }
        out.sort_by(|a, b| { let c = a.0.cmp(&b.0); if c != std::cmp::Ordering::Equal { c } else { a.1.cmp(&b.1) } });
    } else {
        for (k, val) in &s.records {
            if range.includes(k) {
                out.push((k.clone(), k.clone(), idb_clone_global(scope, val)));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
    }
    out
}

/// Put a collected record list into a cursor direction's order: `unique` keeps ONE record per key,
/// then `prev` reverses.
///
/// THE TWO STEPS WERE THE WRONG WAY ROUND, IN BOTH COPIES. "First of each equal-key run" is the
/// LOWEST primary key only while the run is ascending; reversed first, `prevunique` kept the HIGHEST
/// -- `expected "Bob:1" but got "Bob:2"`. The spec picks the record with the lowest primary key for
/// BOTH unique directions and only then walks the index backwards.
///
/// AND THERE WERE TWO COPIES: `idb_read_all` (getAll/getAllKeys/getAllRecords, which take a
/// `direction` in their options dictionary) and `idb_open_cursor` each had their own, with the same
/// two statements in the same wrong order. One spec step, one function.
fn idb_apply_direction(entries: &mut Vec<(Key, Key, v8::Global<v8::Value>)>, dir: &str) {
    if dir.ends_with("unique") {
        let mut deduped: Vec<(Key, Key, v8::Global<v8::Value>)> = Vec::new();
        for e in std::mem::take(entries).into_iter() {
            if deduped.last().map(|l| l.0.cmp(&e.0) == std::cmp::Ordering::Equal).unwrap_or(false) { continue; }
            deduped.push(e);
        }
        *entries = deduped;
    }
    if dir.starts_with("prev") { entries.reverse(); }
}

/// Whether a generated key can be injected at `key_path`: the value must be an object and every
/// intermediate segment must be absent or itself an object (a primitive intermediate is a DataError).
fn idb_can_inject_key(scope: &mut v8::PinScope, obj: v8::Local<v8::Value>, key_path: &str) -> bool {
    let mut cur = match v8::Local::<v8::Object>::try_from(obj) { Ok(o) => o, Err(_) => return false };
    let parts: Vec<&str> = key_path.split('.').collect();
    for p in &parts[..parts.len() - 1] {
        match get_prop(scope, cur, p) {
            None => return true,
            Some(v) if v.is_undefined() || v.is_null() => return true,
            Some(v) => match v8::Local::<v8::Object>::try_from(v) { Ok(o) => cur = o, Err(_) => return false },
        }
    }
    true
}
/// Inject a generated key into a cloned value at `key_path` (creates intermediate objects like injectKey).
fn idb_inject_key(scope: &mut v8::PinScope, obj: v8::Local<v8::Value>, key_path: &str, key: f64) {
    let parts: Vec<&str> = key_path.split('.').collect();
    let mut cur = match v8::Local::<v8::Object>::try_from(obj) { Ok(o) => o, Err(_) => return };
    for p in &parts[..parts.len() - 1] {
        let existing = get_prop(scope, cur, p);
        match existing.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            Some(o) => cur = o,
            None => {
                let o = v8::Object::new(scope);
                if let Some(k) = v8::String::new(scope, p) { cur.set(scope, k.into(), o.into()); }
                cur = o;
            }
        }
    }
    if let Some(k) = v8::String::new(scope, parts[parts.len() - 1]) {
        let n = v8::Number::new(scope, key);
        cur.set(scope, k.into(), n.into());
    }
}

/// (tx, db_name, store_name, index_name) from an IdbObjectStore OR IdbIndex `this` (both are query sources).
fn idb_store_ctx(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<(v8::Global<v8::Value>, String, String, Option<String>)> {
    if let Some(osh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this) {
        let r = unsafe { osh.as_ref() };
        let tx = r.tx.borrow().clone()?;
        let (db, store, idx) = (r.db_name.clone(), r.store_name.borrow().clone(), r.index_name.borrow().clone());
        return Some((tx, db, store, idx));
    }
    if let Some(ixh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbIndex>(scope, this) {
        let r = unsafe { ixh.as_ref() };
        let tx = r.tx.borrow().clone()?;
        return Some((tx, r.db_name.clone(), r.store_name.clone(), Some(r.index_name.borrow().clone())));
    }
    // NOT an IDBObjectStore or IDBIndex. This used to answer `None`, and every caller turned that
    // into a silent `undefined` -- so `Object.getOwnPropertyDescriptor(IDBObjectStore.prototype,
    // "name").get.call(Object.create(IDBObjectStore.prototype))` returned undefined where every
    // browser throws "Illegal invocation". 40 members across the IDB family disagreed in that one
    // shape; the shell members already check their receiver and these are the implemented ones that
    // did not.
    //
    // A receiver that IS one of the two but whose transaction is gone still answers None without a
    // throw, which is the other branch above and a different question.
    if !this.is_object()
        || deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this).is_none()
            && deno_core::cppgc::try_unwrap_cppgc_object::<IdbIndex>(scope, this).is_none()
    {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
    }
    None
}

fn idb_store_meta(scope: &mut v8::PinScope, db: &str, store: &str) -> Option<(Option<String>, bool, f64)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let s = st.borrow::<IdbBackend>().db(db)?.stores.get(store)?;
    Some((s.key_path.clone(), s.auto_increment, s.key_gen))
}

/// IndexedDB's "has been deleted" predicate, for an object-store source or an index source: the
/// record is gone from a DATABASE RECORD THAT STILL EXISTS.
///
/// The qualification matters. An aborted upgrade of a brand-new database removes the whole database
/// record (`idb_finish_tx`, backend.rs), and a handle left over from one of those is "not active",
/// not "deleted" -- so a missing database answers `false` here and the caller falls through to the
/// TransactionInactiveError every browser reports for it.
pub(crate) fn idb_source_deleted(scope: &mut v8::PinScope, db: &str, store: &str, index: Option<&str>) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let d = match st.borrow::<IdbBackend>().db(db) { Some(d) => d, None => return false };
    let s = match d.stores.get(store) { Some(s) => s, None => return true };
    match index { Some(i) => !s.indexes.iter().any(|x| x.name == i), None => false }
}

/// Which prologue a member runs. The three shapes differ ONLY in which extra step they add and where
/// -- the order itself is the spec's and is not the caller's to choose.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum IdbOp {
    /// get/getKey/count/getAll/getAllKeys/getAllRecords/openCursor/openKeyCursor.
    Read,
    /// add/put/delete/clear: the read steps, then the "read only" check.
    Write,
    /// createIndex/deleteIndex: the "upgrade transaction" check comes FIRST, ahead of "deleted".
    Schema,
}

/// The prologue EVERY IDBObjectStore / IDBIndex member that reaches the store shares, in the order
/// IndexedDB fixes:
///
///   1. receiver            -> TypeError "Illegal invocation"      (`idb_store_ctx`)
///   2. upgrade transaction -> InvalidStateError            (`Schema` only, and it is step 1 there)
///   3. has been deleted    -> InvalidStateError
///   4. not active          -> TransactionInactiveError
///   5. read only           -> ReadOnlyError                       (`Write` only)
///
/// Eleven members used to open-code steps 1+4 inline and every one of them omitted step 3, so a
/// handle to a deleted store reported TransactionInactiveError where the spec -- and
/// `idbobjectstore-query-exception-order.any.js` by name -- requires InvalidStateError. The order is
/// observable, so it belongs in ONE place; `Some` means all of it passed.
///
/// Returns `(tx, db_name, store_name, index_name)`. `None` = an exception is already thrown (or the
/// receiver has no transaction at all) and the caller must return at once.
fn idb_op_ctx(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, op: IdbOp) -> Option<(v8::Global<v8::Value>, String, String, Option<String>)> {
    let (tx_g, db_name, store_name, index_name) = idb_store_ctx(scope, this)?;
    let tx_val = idb_l(scope, &tx_g);
    let txh = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val)?;
    if op == IdbOp::Schema && !unsafe { txh.as_ref() }.is_upgrade.get() {
        idb_throw(scope, "InvalidStateError", "not a versionchange transaction");
        return None;
    }
    if idb_source_deleted(scope, &db_name, &store_name, index_name.as_deref()) {
        let what = if index_name.is_some() { "the index has been deleted" } else { "the object store has been deleted" };
        idb_throw(scope, "InvalidStateError", what);
        return None;
    }
    if unsafe { txh.as_ref() }.finished.get() || !unsafe { txh.as_ref() }.active.get() {
        idb_throw(scope, "TransactionInactiveError", "transaction is not active");
        return None;
    }
    if op == IdbOp::Write && unsafe { txh.as_ref() }.mode == "readonly" {
        idb_throw(scope, "ReadOnlyError", "transaction is read-only");
        return None;
    }
    Some((tx_g, db_name, store_name, index_name))
}

/// `optional [EnforceRange] unsigned long count`, for getAll / getAllKeys / getAllRecords.
///
/// `Err(())` = a TypeError is thrown and the caller must return. `Ok(None)` = UNBOUNDED, which is
/// what both an absent argument and an explicit `0` mean ("retrieve multiple" reads 0 as unbounded,
/// not as "take zero" -- `idbobjectstore_getAll.any.js` calls that case "zero maxCount").
///
/// This used to be `.filter(|n| *n >= 0.0)`, which swallowed NaN, -Infinity and every out-of-range
/// value into a silent "no count" instead of the TypeError `[EnforceRange]` demands.
fn idb_count_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<Option<usize>, ()> {
    if v.is_undefined() { return Ok(None); }
    // The range check itself is `idb_enforce_range_u32` -- the SAME WebIDL type `IDBCursor.advance`
    // takes, which had its own weaker copy. What differs here is only what an absent argument and an
    // explicit `0` mean, and that is this member's business, not the conversion's.
    let t = crate::modules::indexeddb::backend::idb_enforce_range_u32(scope, v)?;
    Ok(if t == 0 { None } else { Some(t as usize) })
}

fn idb_store_write(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, is_add: bool, mut rv: v8::ReturnValue) {
    let (tx_g, db_name, store_name, _idx) = match idb_op_ctx(scope, args.this().into(), IdbOp::Write) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let (key_path, auto_inc, key_gen) = match idb_store_meta(scope, &db_name, &store_name) { Some(m) => m, None => return };
    let value = { let g = v8::Global::new(scope, args.get(0)); v8::Local::new(scope, &g) };
    let key_arg = args.get(1);
    let key_given = !key_arg.is_undefined();

    let use_key: Key;
    let store_val: v8::Global<v8::Value>;
    let mut new_key_gen: Option<f64> = None;
    if let Some(kp) = &key_path {
        if key_given { idb_throw(scope, "DataError", "in-line keyed store takes no key argument"); return; }
        match extract_key(scope, value, kp) {
            Some(k) => { use_key = k; store_val = idb_structured_clone(scope, value); }
            None => {
                if !auto_inc { idb_throw(scope, "DataError", "no key and no key generator"); return; }
                if !idb_can_inject_key(scope, value, kp) { idb_throw(scope, "DataError", "cannot inject key at key path"); return; }
                store_val = idb_structured_clone(scope, value);
                let clone_local = idb_l(scope, &store_val);
                idb_inject_key(scope, clone_local, kp, key_gen);
                use_key = Key::Number(key_gen);
                new_key_gen = Some(key_gen + 1.0);
            }
        }
    } else if key_given {
        match js_to_key(scope, key_arg) {
            Some(k) => {
                if auto_inc { if let Key::Number(n) = &k { if *n >= key_gen { new_key_gen = Some(n.floor() + 1.0); } } }
                use_key = k;
                store_val = idb_structured_clone(scope, value);
            }
            None => { idb_throw(scope, "DataError", "invalid key"); return; }
        }
    } else {
        if !auto_inc { idb_throw(scope, "DataError", "no key and no key generator"); return; }
        use_key = Key::Number(key_gen);
        new_key_gen = Some(key_gen + 1.0);
        store_val = idb_structured_clone(scope, value);
    }

    let source = idb_g(scope, args.this().into());
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);

    // Do the write now; the observable result is delivered by the drain.
    let write_res = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        if let Some(g) = new_key_gen {
            if let Some(s) = backend.db_mut(&db_name).and_then(|d| d.stores.get_mut(&store_name)) { s.key_gen = g; }
        }
        idb_do_write(scope, backend, &db_name, &store_name, use_key.clone(), store_val, is_add)
    };
    let pending = match write_res {
        Ok(undo) => {
            unsafe { txh.as_ref() }.undo.borrow_mut().push(undo);
            let kv = key_to_js(scope, &use_key);
            IdbPending { req: idb_g(scope, req), result: Some(idb_g(scope, kv)), error: None }
        }
        Err((name, msg)) => IdbPending { req: idb_g(scope, req), result: None, error: Some((name, msg)) },
    };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_store_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_store_write(scope, &args, true, rv) }
pub(crate) fn cb_store_put(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_store_write(scope, &args, false, rv) }

fn idb_read(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, query: v8::Local<v8::Value>, count_arg: v8::Local<v8::Value>, kind: u8, mut rv: v8::ReturnValue) {
    let (tx_g, db_name, store_name, index_name) = match idb_op_ctx(scope, this, IdbOp::Read) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let range = match to_range_arg(scope, query) { Ok(r) => r, Err(()) => { idb_throw(scope, "DataError", "invalid query key"); return; } };
    let count_limit: Option<usize> = match idb_count_arg(scope, count_arg) { Ok(c) => c, Err(()) => return };

    // Collect entries (drop the backend borrow before building JS results / requests).
    let entries: Vec<(Key, Key, v8::Global<v8::Value>)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        idb_collect(scope, st.borrow::<IdbBackend>(), &db_name, &store_name, index_name.as_deref(), &range)
    };

    let result: v8::Local<v8::Value> = match kind {
        0 => entries.first().map(|e| idb_l(scope, &e.2)).unwrap_or_else(|| v8::undefined(scope).into()),
        1 => entries.first().map(|e| key_to_js(scope, &e.1)).unwrap_or_else(|| v8::undefined(scope).into()),
        2 => v8::Number::new(scope, entries.len() as f64).into(),
        3 | 4 => {
            let limited: Vec<&(Key, Key, v8::Global<v8::Value>)> = match count_limit { Some(n) => entries.iter().take(n).collect(), None => entries.iter().collect() };
            let arr = v8::Array::new(scope, limited.len() as i32);
            for (i, e) in limited.iter().enumerate() {
                let v = if kind == 3 { idb_l(scope, &e.2) } else { key_to_js(scope, &e.1) };
                arr.set_index(scope, i as u32, v);
            }
            arr.into()
        }
        _ => v8::undefined(scope).into(),
    };

    let source = idb_g(scope, this);
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    let pending = IdbPending { req: idb_g(scope, req), result: Some(idb_g(scope, result)), error: None };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_store_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { let this = args.this().into(); let q = args.get(0); let u: v8::Local<v8::Value> = v8::undefined(scope).into(); idb_read(scope, this, q, u, 0, rv) }
pub(crate) fn cb_store_get_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { let this = args.this().into(); let q = args.get(0); let u: v8::Local<v8::Value> = v8::undefined(scope).into(); idb_read(scope, this, q, u, 1, rv) }
pub(crate) fn cb_store_count(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { let this = args.this().into(); let q = args.get(0); let u: v8::Local<v8::Value> = v8::undefined(scope).into(); idb_read(scope, this, q, u, 2, rv) }

fn idb_read_all(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, kind: u8, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let (tx_g, db_name, store_name, index_name) = match idb_op_ctx(scope, this, IdbOp::Read) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    // Distinguish an options dictionary from a query key: a plain object that is neither a key
    // (Date/ArrayBuffer/typed-array) nor an IDBKeyRange is the dictionary overload.
    let arg0 = args.get(0);
    let is_options = arg0.is_object() && !arg0.is_array()
        && deno_core::cppgc::try_unwrap_cppgc_object::<IdbKeyRange>(scope, arg0).is_none()
        && js_to_key(scope, arg0).is_none();
    let (range, direction, count): (KeyRange, String, Option<usize>) = if is_options {
        let o = v8::Local::<v8::Object>::try_from(arg0).unwrap();
        let q = get_prop(scope, o, "query").unwrap_or_else(|| v8::undefined(scope).into());
        let range = match to_range_arg(scope, q) { Ok(r) => r, Err(()) => { idb_throw(scope, "DataError", "invalid query key"); return; } };
        let dir = get_prop(scope, o, "direction").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_else(|| "next".to_string());
        let cv = get_prop(scope, o, "count").unwrap_or_else(|| v8::undefined(scope).into());
        let cnt = match idb_count_arg(scope, cv) { Ok(c) => c, Err(()) => return };
        (range, dir, cnt)
    } else {
        let range = match to_range_arg(scope, arg0) { Ok(r) => r, Err(()) => { idb_throw(scope, "DataError", "invalid query key"); return; } };
        let cnt = match idb_count_arg(scope, args.get(1)) { Ok(c) => c, Err(()) => return };
        (range, "next".to_string(), cnt)
    };

    let mut entries: Vec<(Key, Key, v8::Global<v8::Value>)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        idb_collect(scope, st.borrow::<IdbBackend>(), &db_name, &store_name, index_name.as_deref(), &range)
    };
    idb_apply_direction(&mut entries, &direction);
    let limited: Vec<&(Key, Key, v8::Global<v8::Value>)> = match count { Some(n) => entries.iter().take(n).collect(), None => entries.iter().collect() };
    let arr = v8::Array::new(scope, limited.len() as i32);
    for (i, e) in limited.iter().enumerate() {
        let v: v8::Local<v8::Value> = match kind {
            3 => idb_l(scope, &e.2),
            4 => key_to_js(scope, &e.1),
            // getAllRecords resolves with IDBRecord INSTANCES: the interface is in the capture, and
            // Chrome's records own no properties at all -- `key`/`primaryKey`/`value` are accessors on
            // IDBRecord.prototype (probed 2026-09-10: `T0=[object IDBRecord] own0=[] protoOK=true`).
            // This used to hand back a plain object with three own data properties.
            _ => {
                let k = key_to_js(scope, &e.0);
                let pk = key_to_js(scope, &e.1);
                let vv = idb_l(scope, &e.2);
                idb_make_record(scope, k, pk, vv)
            }
        };
        arr.set_index(scope, i as u32, v);
    }
    let result: v8::Local<v8::Value> = arr.into();
    let source = idb_g(scope, this);
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    let pending = IdbPending { req: idb_g(scope, req), result: Some(idb_g(scope, result)), error: None };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_store_get_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_read_all(scope, &args, 3, rv) }
pub(crate) fn cb_store_get_all_keys(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_read_all(scope, &args, 4, rv) }
pub(crate) fn cb_store_get_all_records(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_read_all(scope, &args, 5, rv) }

pub(crate) fn cb_store_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (tx_g, db_name, store_name, _i) = match idb_op_ctx(scope, args.this().into(), IdbOp::Write) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let range = match to_range_arg(scope, args.get(0)) { Ok(r) => r, Err(()) => { idb_throw(scope, "DataError", "invalid query"); return; } };
    let undos = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        idb_do_delete(scope, st.borrow_mut::<IdbBackend>(), &db_name, &store_name, &range)
    };
    for u in undos { unsafe { txh.as_ref() }.undo.borrow_mut().push(u); }
    let source = idb_g(scope, args.this().into());
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(IdbPending { req: idb_g(scope, req), result: None, error: None });
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_store_clear(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (tx_g, db_name, store_name, _i) = match idb_op_ctx(scope, args.this().into(), IdbOp::Write) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let undos = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        idb_do_delete(scope, st.borrow_mut::<IdbBackend>(), &db_name, &store_name, &KeyRange::all())
    };
    for u in undos { unsafe { txh.as_ref() }.undo.borrow_mut().push(u); }
    let source = idb_g(scope, args.this().into());
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(IdbPending { req: idb_g(scope, req), result: None, error: None });
    idb_pump(scope, tx_val);
    rv.set(req);
}

pub(crate) fn cb_store_key_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (_t, db, store, _i) = match idb_store_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    match idb_store_meta(scope, &db, &store).and_then(|(kp, _, _)| kp) {
        // a sequence key path reads back as the array of its parts -- the same array each read, as Chrome 150's
        // (`store.keyPath === store.keyPath`)
        Some(kp) if kp.contains('\u{0}') => {
            let this: v8::Local<v8::Value> = args.this().into();
            let handle = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this);
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
        }
        Some(kp) => { let v = str_val(scope, &kp); rv.set(v); }
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}
pub(crate) fn cb_store_auto_inc(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (_t, db, store, _i) = match idb_store_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    rv.set_bool(idb_store_meta(scope, &db, &store).map(|(_, ai, _)| ai).unwrap_or(false));
}
pub(crate) fn cb_store_transaction(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (tx_g, _d, _s, _i) = match idb_store_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    let v = idb_l(scope, &tx_g);
    rv.set(v);
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `name`'s GETTER was an `#[op2] #[getter]` member of the class template above until the W5.1 L7
// cutover, and its setter was grafted alongside it from JS (`attachName` in `dom/08e-indexeddb.js`).
// An instance now carries the `IDBObjectStore` TABLE's prototype, and the row is one read/write
// accessor over these two callbacks.
pub(crate) fn cb_store_name_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let osh = idb_recv!(scope, args.this().into(), IdbObjectStore);
    let n = unsafe { osh.as_ref() }.store_name.borrow().clone();
    let v = str_val(scope, &n);
    rv.set(v);
}

pub(crate) fn cb_store_set_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let osh = idb_recv!(scope, this.into(), IdbObjectStore);
    let db_name = unsafe { osh.as_ref() }.db_name.clone();
    let old = unsafe { osh.as_ref() }.store_name.borrow().clone();
    let tx_g = match unsafe { osh.as_ref() }.tx.borrow().clone() { Some(t) => t, None => { idb_throw(scope, "InvalidStateError", "the object store has been deleted"); return; } };
    let new_name = args.get(0).to_rust_string_lossy(scope);
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    // The setter's steps, in the spec's order -- the same three the data members run (`idb_op_ctx`),
    // except that the rename is not an operation ON the store so it cannot share the helper.
    //
    // 1. "has been deleted". Missing entirely until now: `db.deleteObjectStore(name)` leaves the
    //    handle's `tx` in place, so the deleted branch above never fired and a rename of a deleted
    //    store fell through to the backend and returned SILENTLY
    //    (`idbobjectstore-rename-errors.any.js`, "deleted object store rename throws").
    if idb_source_deleted(scope, &db_name, &old, None) { idb_throw(scope, "InvalidStateError", "the object store has been deleted"); return; }
    // 2. "not an upgrade transaction". `finished` used to be folded in HERE, and a finished
    //    transaction is an INACTIVE one, not a non-upgrade one -- which is why 'rename throws in an
    //    inactive transaction' read back InvalidStateError where the spec says
    //    TransactionInactiveError.
    if !unsafe { txh.as_ref() }.is_upgrade.get() { idb_throw(scope, "InvalidStateError", "the object store's name can only be set in a versionchange transaction"); return; }
    // 3. "not active".
    if unsafe { txh.as_ref() }.finished.get() || !unsafe { txh.as_ref() }.active.get() { idb_throw(scope, "TransactionInactiveError", "the transaction is not active"); return; }
    if new_name == old { return; }
    let status: u8 = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.borrow_mut::<IdbBackend>().db_mut(&db_name) {
            Some(d) if d.stores.contains_key(&new_name) => 1,
            Some(d) => match d.stores.remove(&old) { Some(rec) => { d.stores.insert(new_name.clone(), rec); 0 } None => 2 },
            None => 2,
        }
    };
    if status == 1 { idb_throw(scope, "ConstraintError", "an object store with the given name already exists"); return; }
    if status == 2 { return; }
    *unsafe { osh.as_ref() }.store_name.borrow_mut() = new_name.clone();
    let tr = unsafe { txh.as_ref() };
    for n in tr.scope.borrow_mut().iter_mut() { if *n == old { *n = new_name.clone(); } }
    let mut sh = tr.store_handles.borrow_mut();
    if let Some(h) = sh.remove(&old) { sh.insert(new_name.clone(), h); }
}

pub(crate) fn cb_store_index_names(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (_t, db, store, _i) = match idb_store_ctx(scope, args.this().into()) { Some(x) => x, None => return };
    let names: Vec<String> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&db).and_then(|d| d.stores.get(&store)).map(|s| s.indexes.iter().map(|i| i.name.clone()).collect()).unwrap_or_default()
    };
    let list = idb_string_list(scope, &names);
    rv.set(list);
}

fn idb_make_index<'s>(scope: &mut v8::PinScope<'s, '_>, db_name: &str, store_name: &str, index_name: &str, store_val: v8::Local<v8::Value>, tx_val: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let ix = IdbIndex {
        db_name: db_name.to_string(),
        store_name: store_name.to_string(),
        index_name: std::cell::RefCell::new(index_name.to_string()),
        store: std::cell::RefCell::new(Some(idb_g(scope, store_val))),
        tx: std::cell::RefCell::new(Some(idb_g(scope, tx_val))),
        key_path_cache: std::cell::RefCell::new(None),
    };
    let obj = crate::binding::class::mint(scope, ix);
    idb_set_proto(scope, obj, "IDBIndex");
    obj.into()
}
/// Read an index key-path arg: a string path, or an array of paths joined by '\0' (a compound path
/// marker the extractor treats as a sequence).
pub(crate) fn idb_index_key_path(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    if v.is_array() {
        if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
            let parts: Vec<String> = (0..a.length()).filter_map(|i| a.get_index(scope, i).map(|x| x.to_rust_string_lossy(scope))).collect();
            return parts.join("\u{0}");
        }
    }
    v.to_rust_string_lossy(scope)
}
pub(crate) fn cb_store_create_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let (tx_g, db_name, store_name, _i) = match idb_op_ctx(scope, this, IdbOp::Schema) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let name = args.get(0).to_rust_string_lossy(scope);
    let key_path = idb_index_key_path(scope, args.get(1));
    // `IDBIndexParameters`, converted as WebIDL converts it (Chrome 150): `multiEntry`, then
    // `unique`, each read once (korvus read `unique` first)
    let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(2), "IDBIndexParameters", "Failed to execute 'createIndex' on 'IDBObjectStore': ") else { return };
    let (unique, multi_entry) = (
        get_prop(scope, o, "unique").is_some_and(|v| v.is_true()),
        get_prop(scope, o, "multiEntry").is_some_and(|v| v.is_true()),
    );
    let exists = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&db_name).and_then(|d| d.stores.get(&store_name)).map(|s| s.indexes.iter().any(|i| i.name == name)).unwrap_or(false)
    };
    // THE VALIDATION PROLOGUE, which this member never had. All three steps are ordered by
    // `idbobjectstore_createIndex.any.js` BY NAME -- "Index name check should precede syntax check of
    // the key path" and "Syntax check should precede multiEntry check of the key path" -- so the
    // order below is not a choice.
    if exists { idb_throw(scope, "ConstraintError", "index already exists"); return; }
    if !crate::modules::indexeddb::backend::idb_valid_key_path(&key_path) { idb_throw(scope, "SyntaxError", "the key path is not a valid key path"); return; }
    // "If keyPath is a sequence and multiEntry is true, throw an InvalidAccessError." A sequence
    // reaches here as its parts joined by NUL (`idb_index_key_path`).
    if multi_entry && key_path.contains('\u{0}') { idb_throw(scope, "InvalidAccessError", "multiEntry is not allowed on a sequence key path"); return; }
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(s) = st.borrow_mut::<IdbBackend>().db_mut(&db_name).and_then(|d| d.stores.get_mut(&store_name)) {
            s.indexes.push(IdxRec { name: name.clone(), key_path, unique, multi_entry });
        }
    }
    let ix = idb_make_index(scope, &db_name, &store_name, &name, this, tx_val);
    if let Some(osh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this) {
        unsafe { osh.as_ref() }.index_handles.borrow_mut().insert(name.clone(), idb_g(scope, ix));
    }
    rv.set(ix);
}
pub(crate) fn cb_store_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let (tx_g, db_name, store_name, _i) = match idb_store_ctx(scope, this) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    if let Some(txh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) {
        if unsafe { txh.as_ref() }.finished.get() { idb_throw(scope, "InvalidStateError", "transaction finished"); return; }
    }
    let name = args.get(0).to_rust_string_lossy(scope);
    let exists = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().db(&db_name).and_then(|d| d.stores.get(&store_name)).map(|s| s.indexes.iter().any(|i| i.name == name)).unwrap_or(false)
    };
    if !exists { idb_throw(scope, "NotFoundError", "no index named"); return; }
    // Return the cached handle if this store handle already vended one (identity: store.index(n) === store.index(n)).
    if let Some(osh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this) {
        if let Some(h) = unsafe { osh.as_ref() }.index_handles.borrow().get(&name) { let v = idb_l(scope, h); rv.set(v); return; }
    }
    let ix = idb_make_index(scope, &db_name, &store_name, &name, this, tx_val);
    if let Some(osh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbObjectStore>(scope, this) {
        unsafe { osh.as_ref() }.index_handles.borrow_mut().insert(name.clone(), idb_g(scope, ix));
    }
    rv.set(ix);
}
pub(crate) fn cb_store_delete_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (_tx_g, db_name, store_name, _i) = match idb_op_ctx(scope, args.this().into(), IdbOp::Schema) { Some(x) => x, None => return };
    let name = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let found = st.borrow_mut::<IdbBackend>().db_mut(&db_name).and_then(|d| d.stores.get_mut(&store_name)).map(|s| { let before = s.indexes.len(); s.indexes.retain(|i| i.name != name); s.indexes.len() != before }).unwrap_or(false);
    if !found { drop(st); idb_throw(scope, "NotFoundError", "no index named"); }
}

/// openCursor (key_only=false) / openKeyCursor (key_only=true), on a store OR index source.
fn idb_open_cursor(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, key_only: bool, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let (tx_g, db_name, store_name, index_name) = match idb_op_ctx(scope, this, IdbOp::Read) { Some(x) => x, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let range = match to_range_arg(scope, args.get(0)) { Ok(r) => r, Err(()) => { idb_throw(scope, "DataError", "invalid query key"); return; } };
    let dir = if args.get(1).is_undefined() { "next".to_string() } else { args.get(1).to_rust_string_lossy(scope) };
    let mut entries: Vec<(Key, Key, v8::Global<v8::Value>)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        idb_collect(scope, st.borrow::<IdbBackend>(), &db_name, &store_name, index_name.as_deref(), &range)
    };
    idb_apply_direction(&mut entries, &dir);
    let empty = entries.is_empty();
    let source = idb_g(scope, this);
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    let cur = IdbCursor {
        req: std::cell::RefCell::new(Some(idb_g(scope, req))),
        source: std::cell::RefCell::new(Some(idb_g(scope, this))),
        tx: std::cell::RefCell::new(Some(idb_g(scope, tx_val))),
        entries: std::cell::RefCell::new(entries),
        pos: std::cell::Cell::new(0),
        // False until the request DELIVERS this cursor (`cb_idb_drain`, backend.rs) -- the spec's
        // "got value flag" is set by "iterate a cursor", not by opening one.
        got_value: std::cell::Cell::new(false),
        direction: dir,
        key_only,
        db_name,
        store_name,
    };
    let cursor_obj = crate::binding::class::mint(scope, cur);
    idb_set_proto(scope, cursor_obj, if key_only { "IDBCursor" } else { "IDBCursorWithValue" });
    let result_val: v8::Local<v8::Value> = if empty { v8::null(scope).into() } else { cursor_obj.into() };
    let pending = IdbPending { req: idb_g(scope, req), result: Some(idb_g(scope, result_val)), error: None };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_store_open_cursor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_open_cursor(scope, &args, false, rv) }
pub(crate) fn cb_store_open_key_cursor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { idb_open_cursor(scope, &args, true, rv) }


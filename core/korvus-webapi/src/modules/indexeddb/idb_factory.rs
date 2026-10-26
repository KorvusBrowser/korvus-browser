//! `modules/indexeddb/idb_factory.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use super::*;            // sibling Idb* interfaces (re-exported by mod.rs)
use crate::binding::protos::{build_fn_data, set_prop, str_val};
use crate::modules::indexeddb::backend::{DbRec, IdbBackend, IdbWaiter, idb_dom_exception, idb_fire, idb_g, idb_make_database, cb_idb_clear_active, idb_make_request, idb_make_transaction, idb_new_event, idb_new_tx, idb_new_version_change_event, idb_pump, idb_queue_microtask, idb_recv, idb_throw, idb_throw_type, js_to_key};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbFactory {
    pub(crate) _p: u8,
}

pub(crate) trait IdbFactoryBehavior {
   fn construct() -> IdbFactory where Self: Sized { stub!("IdbFactory::constructor") }
}

#[webapi(base, own, name = "IDBFactory")]
impl IdbFactory {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbFactory, ::deno_error::JsErrorBox> { dispatch!("IdbFactory::constructor", <IdbFactory as IdbFactoryBehavior>::construct()) }
}

impl IdbFactoryBehavior for IdbFactory {
    fn construct() -> IdbFactory { IdbFactory { _p: 0 } }
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// Each of the four enters through `idb_recv!`, so a foreign receiver
// (`IDBFactory.prototype.open.call({}, "x")`) is the WebIDL TypeError rather than a silent no-op. The
// arity guard the `open`/`deleteDatabase`/`cmp` rows carry runs BEFORE the body, so the `require_args`
// calls below are unreachable from JS -- kept because they are also the contract for a Rust caller.

// ── a storage bucket's factory ──────────────────────────────────────────────
//
// `StorageBucket.indexedDB` is an `IDBFactory` of its own over the BUCKET's databases (Blink
// `StorageBucket::indexedDB`, connected through the bucket's `BucketHost`): Chrome 150 lists a database a
// bucket's factory made in `bucket.indexedDB.databases()` and not in `indexedDB.databases()`. The backend keeps
// one map of databases by name; a bucket factory's databases are under a key that names the bucket
// ([`backend_key`]), and the names a page reads drop it ([`display_name`]).

/// The private slot naming the bucket an `IDBFactory` serves; absent on the realm's own `indexedDB`.
const BUCKET_SLOT: &str = "korvus.idb.bucket";

fn bucket_of(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<String> {
    let o = v8::Local::<v8::Object>::try_from(this).ok()?;
    let n = v8::String::new(scope, BUCKET_SLOT)?;
    let k = v8::Private::for_api(scope, Some(n));
    o.get_private(scope, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

/// The backend's key for database `name` of `bucket` (`None`: the default bucket's, its name itself).
pub(crate) fn backend_key(bucket: Option<&str>, name: &str) -> String {
    match bucket {
        Some(b) => format!("\u{0}{b}\u{0}{name}"),
        None => name.to_string(),
    }
}

/// The bucket a backend key belongs to (`None`: the default bucket) and the database's own name.
pub(crate) fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.strip_prefix('\u{0}').and_then(|r| r.split_once('\u{0}')) {
        Some((b, n)) => (Some(b), n),
        None => (None, key),
    }
}

/// The name a page reads for a backend key: the database's own.
pub(crate) fn display_name(key: &str) -> &str {
    split_key(key).1
}

/// A new `IDBFactory` over storage bucket `bucket`'s databases, on this realm's `IDBFactory` prototype.
pub(crate) fn mint_bucket_factory<'s>(scope: &mut v8::PinScope<'s, '_>, bucket: &str) -> v8::Local<'s, v8::Object> {
    let factory = crate::binding::class::mint(scope, IdbFactory { _p: 0 });
    crate::modules::indexeddb::backend::idb_set_proto(scope, factory, "IDBFactory");
    if let Some(n) = v8::String::new(scope, BUCKET_SLOT) {
        let k = v8::Private::for_api(scope, Some(n));
        let b = str_val(scope, bucket);
        factory.set_private(scope, k, b);
    }
    factory
}

/// Storage bucket `bucket` deleted: its databases go with it, unless a connection still holds one (the bucket's
/// `BucketHost` closes those in Chrome; a later `open` of the bucket starts empty either way).
pub(crate) fn delete_bucket_databases(state: &mut deno_core::OpState, bucket: &str) {
    let Some(backend) = state.try_borrow_mut::<IdbBackend>() else { return };
    let gone: Vec<String> = backend.dbs.keys().filter(|k| split_key(k).0 == Some(bucket)).cloned().collect();
    for k in gone {
        if backend.conns.get(&k).is_none_or(|c| c.is_empty()) {
            backend.dbs.remove(&k);
            backend.conns.remove(&k);
        }
    }
}

pub(crate) fn cb_idb_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbFactory);
    if !crate::binding::protos::require_args(scope, &args, 1, "IDBFactory", "open") { return; }
    let bucket = bucket_of(scope, args.this().into());
    let name = backend_key(bucket.as_deref(), &args.get(0).to_rust_string_lossy(scope));
    let varg = args.get(1);
    let version = if varg.is_undefined() {
        -1.0
    } else {
        let v = varg.number_value(scope).unwrap_or(0.0);
        if v.fract() != 0.0 || v < 1.0 {
            idb_throw_type(scope, "invalid version");
            return;
        }
        v
    };
    let req = idb_make_request(scope, None, None, true);
    let data = v8::Array::new(scope, 4);
    data.set_index(scope, 0, req);
    let ns = str_val(scope, &name);
    data.set_index(scope, 1, ns);
    let vn: v8::Local<v8::Value> = v8::Number::new(scope, version).into();
    data.set_index(scope, 2, vn);
    let fnv = build_fn_data(scope, cb_idb_run_open, "", 0, data.into());
    idb_enqueue_request(scope, &name, fnv, false);
    rv.set(req);
}

fn cb_idb_run_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = match v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) { Ok(a) => a, Err(_) => return };
    let req = match data.get_index(scope, 0) { Some(r) => r, None => return };
    let name = data.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let mut version = data.get_index(scope, 2).and_then(|v| v.number_value(scope)).unwrap_or(-1.0);
    // What the context's other pages (or this page's earlier documents) committed to it.
    super::sync::pull(scope, &name);

    let (existed, cur_ver) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let backend = st.borrow::<IdbBackend>();
        match backend.db(&name) { Some(d) => (true, d.version), None => (false, 0.0) }
    };
    if version < 0.0 {
        version = if existed { cur_ver } else { 1.0 };
    }
    if existed && version < cur_ver {
        // VersionError -> error on the request.
        if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, req) {
            let ex = idb_dom_exception(scope, "VersionError", "requested version < current");
            *unsafe { rh.as_ref() }.error.borrow_mut() = Some(idb_g(scope, ex));
            *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
        }
        if let Some(ev) = idb_new_event(scope, "error", req, true) {
            idb_fire(scope, req, ev, "error");
        }
        idb_request_processed(scope, &name);
        return;
    }
    // An upgrade waits for every other connection to close (IndexedDB "open a database connection",
    // steps 10.3-10.5): `versionchange` at each, `blocked` at this request if any stays open.
    if existed && version > cur_ver && idb_wait_for_others(scope, &name, req, data.into(), cur_ver, Some(version), false) {
        return;
    }
    // Ensure the db record exists.
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        backend.dbs.entry(name.clone()).or_insert_with(|| DbRec { version: 0.0, committed: 0.0, stores: std::collections::BTreeMap::new() });
    }
    let db = {
        let d = IdbDatabase { db_name: name.clone(), closed: std::cell::Cell::new(false), active_upgrade_tx: std::cell::RefCell::new(None) };
        idb_make_database(scope, d)
    };
    let db_val: v8::Local<v8::Value> = db.into();
    {
        let g = idb_g(scope, db_val);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.borrow_mut::<IdbBackend>().conns.entry(name.clone()).or_default().push(g);
    }
    let old_version = cur_ver;
    if version > old_version {
        // Bump version + run a versionchange (upgrade) transaction.
        {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            let backend = st.borrow_mut::<IdbBackend>();
            if let Some(d) = backend.db_mut(&name) { d.version = version; }
        }
        let tx = idb_new_tx(name.clone(), Vec::new(), "versionchange".to_string());
        {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            super::sync::writer_began(st.borrow_mut::<IdbBackend>(), &name);
        }
        let txo = idb_make_transaction(scope, tx);
        let tx_val: v8::Local<v8::Value> = txo.into();
        let txh = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val).unwrap();
        *unsafe { txh.as_ref() }.db.borrow_mut() = Some(idb_g(scope, db_val));
        *unsafe { txh.as_ref() }.open_req.borrow_mut() = Some(idb_g(scope, req));
        unsafe { txh.as_ref() }.upgrade_old.set(old_version);
        unsafe { txh.as_ref() }.active.set(true);
        // request.result = db, request.transaction = tx
        if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, req) {
            *unsafe { rh.as_ref() }.result.borrow_mut() = Some(idb_g(scope, db_val));
            *unsafe { rh.as_ref() }.transaction.borrow_mut() = Some(idb_g(scope, tx_val));
            *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
        }
        if let Some(dbh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbDatabase>(scope, db_val) {
            *unsafe { dbh.as_ref() }.active_upgrade_tx.borrow_mut() = Some(idb_g(scope, tx_val));
        }
        // Fire upgradeneeded. It is an IDBVersionChangeEvent, and the two versions are PROTOTYPE
        // accessors over the event's slots -- not the two own data properties this used to stamp on a
        // plain Event, which `Object.getOwnPropertyNames(ev)` printed to any page that looked (Chrome's
        // own instance owns exactly `["isTrusted"]`, probed 2026-09-10).
        if let Some(ev) = idb_new_version_change_event(scope, "upgradeneeded", req, old_version, Some(version)) {
            idb_fire(scope, req, ev, "upgradeneeded");
        }
        // Clear the active flag AFTER this task's microtask checkpoint, not on this line -- the same
        // spec rule `cb_idb_drain` follows (see `cb_idb_clear_active`). A `Promise.resolve().then()`
        // scheduled inside an `onupgradeneeded` handler is still inside the transaction's lifetime in
        // Chrome; clearing synchronously here showed it an already-inactive transaction.
        let clear = build_fn_data(scope, cb_idb_clear_active, "", 0, tx_val);
        idb_queue_microtask(scope, clear);
        idb_pump(scope, tx_val);
    } else {
        if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, req) {
            *unsafe { rh.as_ref() }.result.borrow_mut() = Some(idb_g(scope, db_val));
            *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
        }
        if let Some(ev) = idb_new_event(scope, "success", req, false) {
            idb_fire(scope, req, ev, "success");
        }
        idb_request_processed(scope, &name);
    }
}

pub(crate) fn cb_idb_delete_db(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbFactory);
    if !crate::binding::protos::require_args(scope, &args, 1, "IDBFactory", "deleteDatabase") { return; }
    let bucket = bucket_of(scope, args.this().into());
    let name = backend_key(bucket.as_deref(), &args.get(0).to_rust_string_lossy(scope));
    let req = idb_make_request(scope, None, None, true);
    let data = v8::Array::new(scope, 4);
    data.set_index(scope, 0, req);
    let ns = str_val(scope, &name);
    data.set_index(scope, 1, ns);
    let fnv = build_fn_data(scope, cb_idb_run_delete, "", 0, data.into());
    idb_enqueue_request(scope, &name, fnv, true);
    rv.set(req);
}
fn cb_idb_run_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = match v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) { Ok(a) => a, Err(_) => return };
    let req = match data.get_index(scope, 0) { Some(r) => r, None => return };
    let name = data.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    // A delete the backend already carried out -- resumed when the last connection blocking it closed
    // (`idb_resume_waiters`) -- only reports, with the version it deleted.
    let done = data.get_index(scope, DELETED_VERSION).filter(|v| v.is_number()).and_then(|v| v.number_value(scope));
    let old_version = match done {
        Some(v) => v,
        None => {
            let old_version = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.borrow::<IdbBackend>().db(&name).map(|d| d.version).unwrap_or(0.0)
            };
            // "delete a database", steps 5-8: the delete waits for every open connection to close. It used to
            // drop the stores at once, so a connection still open in another frame read "no object store
            // named" on its next transaction (YouTube's embeds on un.org delete each other's log databases).
            if idb_wait_for_others(scope, &name, req, data.into(), old_version, None, true) {
                return;
            }
            idb_delete_now(scope, &name);
            old_version
        }
    };
    if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, req) {
        *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
    }
    // Chrome: an IDBVersionChangeEvent with the deleted version and a null newVersion -- none for a request
    // whose frame is gone (Chrome 150: a delete a removed frame made completes when the frame's connections
    // close, and fires nothing).
    let live = v8::Local::<v8::Object>::try_from(req).map(|o| crate::core::html::browsing_context::object_realm_fully_active(scope, o)).unwrap_or(true);
    if live {
        if let Some(ev) = idb_new_version_change_event(scope, "success", req, old_version, None) {
            idb_fire(scope, req, ev, "success");
        }
    }
    idb_request_processed(scope, &name);
}

/// The slot of a delete request's data array holding the version the backend deleted, once it has.
const DELETED_VERSION: u32 = 4;

/// The backend's half of a delete: the database and its connections are gone.
fn idb_delete_now(scope: &mut v8::PinScope, name: &str) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        backend.dbs.remove(name);
        backend.conns.remove(name);
    }
    super::sync::push_deleted(scope, name);
}
pub(crate) fn cb_idb_cmp(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbFactory);
    if !crate::binding::protos::require_args(scope, &args, 2, "IDBFactory", "cmp") { return; }
    let a = js_to_key(scope, args.get(0));
    let b = js_to_key(scope, args.get(1));
    match (a, b) {
        (Some(a), Some(b)) => rv.set_int32(match a.cmp(&b) { std::cmp::Ordering::Less => -1, std::cmp::Ordering::Equal => 0, std::cmp::Ordering::Greater => 1 }),
        _ => idb_throw(scope, "DataError", "invalid key"),
    }
}
// `databases()` -- in the capture, absent here. One `{name, version}` per database the backend holds, which
// it has had all along in its `dbs` map. Recorded from Brave: `.length` is 0.
//
// WHEN, AND AS OF WHEN (Chrome 150 measured against timers): the backend answers it in
// its turn -- after a 0 ms timer queued right after the call, and after the requests made before it have
// answered (`open` of a new database: `upgradeneeded`, then this, then `success`; `deleteDatabase`: its
// `success`, then this, without the database) -- with what it has COMMITTED: a database whose first upgrade
// is running is absent, one being upgraded reads its old version. It settled from a microtask here, listed
// at call time, a new database included.
pub(crate) fn cb_idb_databases(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbFactory);
    // Connections of a document that went some other way than a removal (`notify_disconnected` reaps those
    // as they go) are closed first, so a delete they were blocking is carried out before the answer.
    idb_reap_detached(scope);
    let Some(resolver) = v8::PromiseResolver::new(scope) else { rv.set_undefined(); return };
    let p = resolver.get_promise(scope);
    // The backend's next job: after its work in hand, in call order.
    let ms = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        let now = std::time::Instant::now();
        let earliest = now + std::time::Duration::from_secs_f64(DATABASES_MS / 1000.0);
        let at = backend.busy_until.map_or(earliest, |b| earliest.max(b + std::time::Duration::from_micros(100)));
        backend.busy_until = Some(at);
        at.saturating_duration_since(now).as_secs_f64() * 1000.0
    };
    // the answer's data: the resolver, and the bucket whose databases it lists (a storage bucket's factory)
    let bucket = bucket_of(scope, args.this().into());
    let data = v8::Array::new(scope, 2);
    data.set_index(scope, 0, resolver.into());
    if let Some(b) = bucket {
        let bv = str_val(scope, &b);
        data.set_index(scope, 1, bv);
    }
    let f = build_fn_data(scope, cb_idb_answer_databases, "", 0, data.into());
    crate::core::intersection_observer::rendering::queue_task(scope, f, ms);
    rv.set(p.into());
}

/// `databases()`'s answer, in the backend's turn: every committed database of the factory's bucket.
fn cb_idb_answer_databases(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(first) = data.get_index(scope, 0) else { return };
    // SAFETY: `cb_idb_databases` bound a `v8::PromiseResolver` as the data's first element, and nothing else.
    let Some(resolver) = first.is_promise().then(|| unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(first) }) else { return };
    let bucket = data.get_index(scope, 1).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope));
    super::sync::pull_all(scope);
    let rows: Vec<(String, f64)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let backend = st.borrow::<IdbBackend>();
        backend
            .dbs
            .iter()
            .filter(|(n, d)| d.committed > 0.0 && split_key(n).0 == bucket.as_deref())
            .map(|(n, d)| (display_name(n).to_string(), d.committed))
            .collect()
    };
    let arr = v8::Array::new(scope, rows.len() as i32);
    for (i, (name, version)) in rows.iter().enumerate() {
        let o = v8::Object::new(scope);
        let nv = str_val(scope, name);
        set_prop(scope, o, "name", nv);
        let vv = v8::Number::new(scope, *version);
        set_prop(scope, o, "version", vv.into());
        arr.set_index(scope, i as u32, o.into());
    }
    resolver.resolve(scope, arr.into());
}

/// IndexedDB's "a document is no longer fully active": its connections are closed (forced), and the
/// requests they blocked run. A frame removed from the page took its connections with it in Chrome --
/// a `deleteDatabase` the frame's own open connection blocked completes once the frame is gone, and the
/// next `databases()` no longer lists it (the oracle's two-pass capture).
///
/// Run when a removal takes nodes out of the document (`core/dom/node.rs::notify_disconnected`), so it
/// happens WHEN the frame goes, as in Chrome, and not when someone next asks: a `databases()` asked
/// later answers from a backend the delete has already run against.
pub(crate) fn idb_reap_detached(scope: &mut v8::PinScope) {
    let names: Vec<String> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        match st.try_borrow::<IdbBackend>() {
            Some(b) => b.conns.keys().cloned().collect(),
            None => return,
        }
    };
    for n in names {
        idb_resume_waiters(scope, &n);
    }
}

/// IndexedDB's connection queue: the `open` and `deleteDatabase` requests for one
/// name are processed one at a time, in the order the page made them -- a request waits until every
/// request before it has been processed. Chrome 150, `open("q")`, `deleteDatabase("q")`, `open("q")` in one
/// task: the first open's `upgradeneeded` and `success`, then its connection's `versionchange` and the
/// delete's `blocked`, and nothing of the second open until the delete is done. Here every request ran a
/// task after it was made, so the second open succeeded beside the blocked delete (a new connection that
/// kept it blocked for good) and the first open's `success` came after the delete's `blocked`.
fn idb_enqueue_request(scope: &mut v8::PinScope, name: &str, run: v8::Local<v8::Function>, delete: bool) {
    let first = {
        let g = v8::Global::new(scope, run);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let q = st.borrow_mut::<IdbBackend>().queues.entry(name.to_string()).or_default();
        q.push_back((g, delete));
        q.len() == 1
    };
    if first {
        idb_start_request(scope, name, run, delete);
    }
}

/// The head of `name`'s connection queue has been processed -- its `success` or `error` fired: the next
/// request starts, in the realm that made it.
pub(crate) fn idb_request_processed(scope: &mut v8::PinScope, name: &str) {
    let next = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(backend) = st.try_borrow_mut::<IdbBackend>() else { return };
        let Some(q) = backend.queues.get_mut(name) else { return };
        q.pop_front();
        let next = q.front().cloned();
        if q.is_empty() {
            backend.queues.remove(name);
        }
        next
    };
    if let Some((g, delete)) = next {
        let f = v8::Local::new(scope, &g);
        let realm = f.get_creation_context(scope).unwrap_or_else(|| scope.get_current_context());
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        idb_start_request(scope, name, f, delete);
    }
}

/// How long Chrome 150's IndexedDB backend takes to answer on this machine (`open`,
/// `deleteDatabase` and `databases()` against 0-32 ms timers, rounds after a warm-up): an `open` of an existing
/// database succeeds between a 0 ms and a 1 ms timer, and so does a `databases()` with nothing before it; one
/// that creates the database fires `upgradeneeded` at 4-9 ms; a `deleteDatabase` succeeds at 3-9 ms; and
/// `databases()` answers right after the requests made before it.
const OPEN_EXISTING_MS: f64 = 0.5;
const OPEN_CREATING_MS: f64 = 5.0;
const DELETE_MS: f64 = 6.0;
const DATABASES_MS: f64 = 0.5;

/// Start a factory request -- the head of its connection queue -- as the backend answers it, after its
/// latency, and note when the backend is next free.
fn idb_start_request(scope: &mut v8::PinScope, name: &str, f: v8::Local<v8::Function>, delete: bool) {
    let ms = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        let ms = if delete { DELETE_MS } else if backend.db(name).is_some() { OPEN_EXISTING_MS } else { OPEN_CREATING_MS };
        let at = std::time::Instant::now() + std::time::Duration::from_secs_f64(ms / 1000.0);
        backend.busy_until = Some(backend.busy_until.map_or(at, |b| b.max(at)));
        ms
    };
    idb_queue_request_task(scope, f, ms);
}

/// Run a factory request's steps a TASK later -- after the backend's latency (`idb_start_request`) -- not a
/// microtask: IndexedDB processes `open` and `deleteDatabase` in parallel and queues a task with the outcome.
/// The connection queue keeps one name's requests in issue order.
fn idb_queue_request_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ms: f64) {
    crate::core::intersection_observer::rendering::queue_task(scope, f, ms);
}

/// The connections to `name` that are still open, in the order they were opened. Closed ones are
/// dropped from the backend's list on the way.
fn idb_open_conns<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Vec<v8::Local<'s, v8::Value>> {
    let globals: Vec<v8::Global<v8::Value>> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<IdbBackend>().conns.get(name).cloned().unwrap_or_default()
    };
    let mut open = Vec::new();
    let mut keep = Vec::new();
    for g in globals {
        let l = v8::Local::new(scope, &g);
        let mut is_open = deno_core::cppgc::try_unwrap_cppgc_object::<IdbDatabase>(scope, l)
            .is_some_and(|h| !unsafe { h.as_ref() }.closed.get());
        // A connection whose document is no longer fully active (its frame was removed) is closed.
        if is_open {
            let active = v8::Local::<v8::Object>::try_from(l).map(|o| crate::core::html::browsing_context::object_realm_fully_active(scope, o)).unwrap_or(true);
            if !active {
                if let Some(h) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbDatabase>(scope, l) {
                    unsafe { h.as_ref() }.closed.set(true);
                }
                is_open = false;
            }
        }
        if is_open {
            open.push(l);
            keep.push(g);
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.borrow_mut::<IdbBackend>().conns.insert(name.to_string(), keep);
    open
}

/// Fire an IDBVersionChangeEvent at `target` built in `target`'s own realm: a connection opened by a
/// frame is that frame's object, and its handler reads the frame's `IDBVersionChangeEvent`.
fn idb_fire_version_change(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, old: f64, new: Option<f64>) {
    let realm = v8::Local::<v8::Object>::try_from(target).ok().and_then(|o| o.get_creation_context(scope));
    let realm = realm.unwrap_or_else(|| scope.get_current_context());
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    if let Some(ev) = idb_new_version_change_event(scope, ty, target, old, new) {
        idb_fire(scope, target, ev, ty);
    }
}

/// Whether a delete or an upgrade of `name` must wait for other connections. The first time
/// (`data[3]` unset) each open connection gets `versionchange`, and if any is still open after its
/// handlers ran the request gets `blocked`; the request is then parked until `close()` resumes it.
fn idb_wait_for_others(
    scope: &mut v8::PinScope,
    name: &str,
    req: v8::Local<v8::Value>,
    data: v8::Local<v8::Value>,
    old: f64,
    new: Option<f64>,
    delete: bool,
) -> bool {
    let others = idb_open_conns(scope, name);
    if others.is_empty() {
        return false;
    }
    let arr = match v8::Local::<v8::Array>::try_from(data) { Ok(a) => a, Err(_) => return false };
    let resumed = arr.get_index(scope, 3).is_some_and(|v| v.is_true());
    if !resumed {
        for c in others {
            idb_fire_version_change(scope, c, "versionchange", old, new);
        }
        if idb_open_conns(scope, name).is_empty() {
            return false;
        }
        idb_fire_version_change(scope, req, "blocked", old, new);
        if idb_open_conns(scope, name).is_empty() {
            return false;
        }
        let t = v8::Boolean::new(scope, true);
        arr.set_index(scope, 3, t.into());
    }
    let data = idb_g(scope, data);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.borrow_mut::<IdbBackend>().waiters.push(IdbWaiter { name: name.to_string(), data, delete });
    true
}

/// `IDBDatabase.close()` has closed a connection to `name`: once none is left open, every request
/// parked on `name` runs again, each queued in its own request's realm.
pub(crate) fn idb_resume_waiters(scope: &mut v8::PinScope, name: &str) {
    if !idb_open_conns(scope, name).is_empty() {
        return;
    }
    let ready: Vec<IdbWaiter> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let waiters = &mut st.borrow_mut::<IdbBackend>().waiters;
        let (ready, rest) = std::mem::take(waiters).into_iter().partition(|w| w.name == name);
        *waiters = rest;
        ready
    };
    for w in ready {
        let data = v8::Local::new(scope, &w.data);
        let req = v8::Local::<v8::Array>::try_from(data).ok().and_then(|a| a.get_index(scope, 0));
        let realm = req
            .and_then(|r| v8::Local::<v8::Object>::try_from(r).ok())
            .and_then(|o| o.get_creation_context(scope))
            .unwrap_or_else(|| scope.get_current_context());
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        // A blocked delete is carried out NOW, when the last connection closes -- the backend continues it
        // there and then, so a `databases()` asked next no longer lists the database (Chrome 150: a frame
        // whose connection blocked a delete is removed, and `databases()` asked in that same task answers
        // without it) -- and its `success` follows with the backend's latency.
        if w.delete {
            if let Ok(arr) = v8::Local::<v8::Array>::try_from(data) {
                let old = {
                    let op_state = deno_core::JsRuntime::op_state_from(scope);
                    let st = op_state.borrow();
                    st.borrow::<IdbBackend>().db(name).map(|d| d.version).unwrap_or(0.0)
                };
                idb_delete_now(scope, name);
                let ov: v8::Local<v8::Value> = v8::Number::new(scope, old).into();
                arr.set_index(scope, DELETED_VERSION, ov);
            }
        }
        let f = if w.delete {
            build_fn_data(scope, cb_idb_run_delete, "", 0, data)
        } else {
            build_fn_data(scope, cb_idb_run_open, "", 0, data)
        };
        idb_start_request(scope, name, f, w.delete);
    }
}

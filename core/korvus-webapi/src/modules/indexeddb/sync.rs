//! A database reaches the context's other pages, and outlives its realm.
//!
//! The backend ([`IdbBackend`]) lives in a realm's `OpState` and holds its records as V8 values, so a second page of
//! the same browser context never saw a page's databases, and a cross-document navigation (a new isolate here) lost
//! them. In a page of a browser endpoint's context a committed database is now also a slot of the router's context
//! store (`transport::context_store`), its records turned into bytes with the codec a worker's messages cross isolates
//! with ([`crate::wire_serialize`]). Anywhere else -- `korvus serve`, `fetch`, an embedder's engines -- nothing here
//! runs, and the backend is the realm's as it always was.
//!
//! * **Pull** -- before an `open` is processed, before `databases()` answers and when a transaction starts, the
//!   database is brought current from its slot, unless a write transaction of this realm is still running on it
//!   (its uncommitted records are this realm's alone, and nothing may overwrite them).
//! * **Push** -- when a `readwrite` transaction completes or an upgrade commits, the database is written to its slot;
//!   a delete writes the database's tombstone (an empty slot).
//!
//! Two pages committing to ONE database at once: the later commit wins. Transactions of one page are serialized as
//! the spec has them; across pages this is a context-level last-writer-wins, recorded as a call (goal 1020, step 1).
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use deno_core::v8;
use serde::{Deserialize, Serialize};

use crate::idb::Key;
use crate::modules::indexeddb::backend::{DbRec, IdbBackend, IdxRec, StoreRec};

#[derive(Serialize, Deserialize)]
struct WireDb {
    version: f64,
    committed: f64,
    stores: Vec<(String, WireStore)>,
}

#[derive(Serialize, Deserialize)]
struct WireStore {
    key_path: Option<String>,
    auto_increment: bool,
    key_gen: f64,
    records: Vec<(Key, Vec<u8>)>,
    indexes: Vec<WireIdx>,
}

#[derive(Serialize, Deserialize)]
struct WireIdx {
    name: String,
    key_path: String,
    unique: bool,
    multi_entry: bool,
}

/// The calling realm's origin, the slots' key: its global's own `origin`.
fn origin(scope: &mut v8::PinScope) -> String {
    crate::binding::protos::realm_self_get(scope, "origin")
        .filter(|o| o.is_string())
        .map(|o| o.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "null".to_string())
}

fn slot(origin: &str, key: &str) -> String {
    format!("indexedDB\u{0}{origin}\u{0}{key}")
}

/// Every database name an origin's slots hold, for `databases()` to find the ones another page made.
fn names() -> MutexGuard<'static, HashMap<String, transport::context_store::Synced<Vec<String>>>> {
    static NAMES: OnceLock<Mutex<HashMap<String, transport::context_store::Synced<Vec<String>>>>> = OnceLock::new();
    NAMES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn with_names<R>(origin: &str, f: impl FnOnce(&mut Vec<String>) -> R) -> R {
    let key = format!("indexedDB-names\u{0}{origin}");
    names().entry(origin.to_string()).or_insert_with(|| transport::context_store::Synced::new(key)).update(f)
}

/// A write transaction of this realm began on database `key`: no pull overwrites it until it ends.
pub(crate) fn writer_began(backend: &mut IdbBackend, key: &str) {
    *backend.writers.entry(key.to_string()).or_default() += 1;
}

/// A write transaction of this realm on `key` ended (committed or aborted).
pub(crate) fn writer_ended(backend: &mut IdbBackend, key: &str) {
    if let Some(n) = backend.writers.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            backend.writers.remove(key);
        }
    }
    // An aborted write left the records as they were before it: the next pull re-reads the slot.
    backend.synced.remove(key);
}

/// Bring database `key` current with the context's slot.
pub(crate) fn pull(scope: &mut v8::PinScope, key: &str) {
    if !transport::context_store::is_remote() {
        return;
    }
    let origin = origin(scope);
    let have = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(backend) = st.try_borrow::<IdbBackend>() else { return };
        if backend.writers.contains_key(key) {
            return;
        }
        backend.synced.get(key).copied().unwrap_or(0)
    };
    let Some((rev, data)) = transport::context_store::get_raw(&slot(&origin, key), have) else { return };
    let rebuilt = match data {
        None => None,
        Some(bytes) if bytes.is_empty() => Some(None),
        Some(bytes) => match bincode::deserialize::<WireDb>(&bytes) {
            Ok(w) => Some(Some(decode(scope, w))),
            Err(_) => None,
        },
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let Some(backend) = st.try_borrow_mut::<IdbBackend>() else { return };
    if backend.writers.contains_key(key) {
        return;
    }
    match rebuilt {
        Some(Some(db)) => {
            backend.dbs.insert(key.to_string(), db);
        }
        Some(None) => {
            backend.dbs.remove(key);
        }
        None => {}
    }
    if rev != 0 {
        backend.synced.insert(key.to_string(), rev);
    }
}

/// Every database of the calling realm's origin the context knows, current (`databases()`).
pub(crate) fn pull_all(scope: &mut v8::PinScope) {
    if !transport::context_store::is_remote() {
        return;
    }
    let origin = origin(scope);
    let mut keys = with_names(&origin, |n| n.clone());
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if let Some(backend) = st.try_borrow::<IdbBackend>() {
            keys.extend(backend.dbs.keys().cloned());
        }
    }
    keys.sort();
    keys.dedup();
    for k in keys {
        pull(scope, &k);
    }
}

/// Database `key` committed: its slot holds it now.
pub(crate) fn push(scope: &mut v8::PinScope, key: &str) {
    if !transport::context_store::is_remote() {
        return;
    }
    let origin = origin(scope);
    // The records' handles first, out of the borrow: serializing a value is V8's work and must not run inside it.
    let snapshot = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(backend) = st.try_borrow::<IdbBackend>() else { return };
        let Some(db) = backend.db(key) else { return };
        let stores: Vec<(String, Option<String>, bool, f64, Vec<(Key, v8::Global<v8::Value>)>, Vec<WireIdx>)> = db
            .stores
            .iter()
            .map(|(name, s)| {
                let idx = s.indexes.iter().map(|i| WireIdx { name: i.name.clone(), key_path: i.key_path.clone(), unique: i.unique, multi_entry: i.multi_entry }).collect();
                (name.clone(), s.key_path.clone(), s.auto_increment, s.key_gen, s.records.clone(), idx)
            })
            .collect();
        (db.version, db.committed, stores)
    };
    let (version, committed, stores) = snapshot;
    let mut wire = WireDb { version, committed, stores: Vec::with_capacity(stores.len()) };
    for (name, key_path, auto_increment, key_gen, records, indexes) in stores {
        let mut out = Vec::with_capacity(records.len());
        for (k, g) in records {
            let v = v8::Local::new(scope, &g);
            out.push((k, crate::wire_serialize(scope, v, &[])));
        }
        wire.stores.push((name, WireStore { key_path, auto_increment, key_gen, records: out, indexes }));
    }
    let Ok(bytes) = bincode::serialize(&wire) else { return };
    let rev = transport::context_store::put_raw(&slot(&origin, key), bytes);
    if committed > 0.0 {
        with_names(&origin, |n| {
            if !n.iter().any(|x| x == key) {
                n.push(key.to_string());
            }
        });
    }
    if let Some(rev) = rev {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(backend) = st.try_borrow_mut::<IdbBackend>() {
            backend.synced.insert(key.to_string(), rev);
        }
    }
}

/// Database `key` deleted: its slot is a tombstone, and `databases()` no longer finds it.
pub(crate) fn push_deleted(scope: &mut v8::PinScope, key: &str) {
    if !transport::context_store::is_remote() {
        return;
    }
    let origin = origin(scope);
    let rev = transport::context_store::put_raw(&slot(&origin, key), Vec::new());
    with_names(&origin, |n| n.retain(|x| x != key));
    if let Some(rev) = rev {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(backend) = st.try_borrow_mut::<IdbBackend>() {
            backend.synced.insert(key.to_string(), rev);
        }
    }
}

fn decode(scope: &mut v8::PinScope, w: WireDb) -> DbRec {
    let mut stores = std::collections::BTreeMap::new();
    for (name, s) in w.stores {
        let mut records = Vec::with_capacity(s.records.len());
        for (k, bytes) in s.records {
            let v = crate::wire_deserialize(scope, &bytes, &[]);
            records.push((k, v8::Global::new(scope, v)));
        }
        let indexes = s.indexes.into_iter().map(|i| IdxRec { name: i.name, key_path: i.key_path, unique: i.unique, multi_entry: i.multi_entry }).collect();
        stores.insert(name, StoreRec { key_path: s.key_path, auto_increment: s.auto_increment, key_gen: s.key_gen, records, indexes });
    }
    DbRec { version: w.version, committed: w.committed, stores }
}

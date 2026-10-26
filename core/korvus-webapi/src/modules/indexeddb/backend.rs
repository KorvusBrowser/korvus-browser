//! `modules/indexeddb/backend.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::binding::protos::{build_fn_data, get_prop, set_bool, shell_proto, str_val};
// The v8-free key core this async layer sits on (src/idb.rs: the spec key model + total order).
use crate::idb::{exact_pos, insert_pos, Key, KeyRange};
// The cppgc interface classes this backend hands out to JS.
use crate::modules::indexeddb::{IdbDatabase, IdbObjectStore, IdbRequest, IdbTransaction};

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// A request whose operation already ran (sync, in-memory); its observable result/error is delivered on
// a microtask so IDBRequest.result stays pending until success/error fires (spec-observable timing).
// A rollback step applied in reverse on abort.

/// The receiver as a cppgc `$T`, or a `TypeError: Illegal invocation` and an early `return`.
///
/// WebIDL: a platform-object member entered with a receiver that does not implement the interface is a
/// TypeError, and the bundle's oracle records exactly that for 40-odd members of this family
/// (`Object.getOwnPropertyDescriptor(IDBRequest.prototype, "result").get.call(Object.create(
/// IDBRequest.prototype))` throws). Before the one-table cutover every one of these bodies answered
/// `undefined` instead, because the unwrap failure was swallowed by an `if let`.
///
/// The unwrap reads the object's cppgc wrapper tag, never its prototype, so re-prototyping an instance
/// to its TABLE's prototype (which every mint site now does) leaves the brand intact.
macro_rules! idb_recv {
    ($scope:expr, $this:expr, $T:ty) => {
        match deno_core::cppgc::try_unwrap_cppgc_object::<$T>($scope, $this) {
            Some(h) => h,
            None => {
                crate::binding::protos::throw_type_error($scope, "Illegal invocation");
                return;
            }
        }
    };
}
pub(crate) use idb_recv;

/// Extract an IndexedDB key from a JS value, or `None` if it is not a valid key (spec `DataError`).
///
/// A CYCLIC ARRAY IS NOT A KEY, and finding that out must not cost the process. `var a = []; a[0] = a;`
/// used to recurse until the thread's stack was gone and the runtime aborted -- not a hang the watchdog
/// could interrupt, a `fatal runtime error: stack overflow` that takes the whole scoreboard sweep with
/// it (measured 2026-09-11 on `IndexedDB/idbkeyrange-includes.any.js`, which constructs exactly that
/// value on purpose and expects `DataError`). The spec's "convert a value to a key" carries a `seen`
/// list for this reason and returns invalid when the input is already in it; `seen` below is that list.
pub(crate) fn js_to_key(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Key> {
    let mut seen: Vec<v8::Global<v8::Value>> = Vec::new();
    js_to_key_seen(scope, v, &mut seen)
}

fn js_to_key_seen(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, seen: &mut Vec<v8::Global<v8::Value>>) -> Option<Key> {
    if v.is_number() {
        let n = v.number_value(scope)?;
        if n.is_nan() { None } else { Some(Key::Number(n)) }
    } else if v.is_date() {
        let d = v8::Local::<v8::Date>::try_from(v).ok()?;
        let t = d.value_of();
        if t.is_nan() { None } else { Some(Key::Date(t)) }
    } else if v.is_string() {
        Some(Key::String(v.to_rust_string_lossy(scope)))
    } else if v.is_array_buffer() {
        let ab = v8::Local::<v8::ArrayBuffer>::try_from(v).ok()?;
        let store = ab.get_backing_store();
        let bytes: Vec<u8> = (0..store.byte_length()).map(|i| store[i].get()).collect();
        Some(Key::Binary(bytes))
    } else if v.is_array_buffer_view() {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
        let len = view.byte_length();
        let mut buf = vec![0u8; len];
        view.copy_contents(&mut buf);
        Some(Key::Binary(buf))
    } else if v.is_array() {
        // The spec's `seen` check, by IDENTITY: an array that contains itself (directly or through any
        // depth of nesting) is not a valid key, and the recursion below would otherwise never end.
        // `strict_equals` on two objects IS identity (`===`), which is what "seen" means here: a
        // structurally identical SIBLING array is a perfectly good key and must not be rejected.
        if seen.iter().any(|s| v8::Local::new(scope, s).strict_equals(v)) {
            return None;
        }
        let a = v8::Local::<v8::Array>::try_from(v).ok()?;
        seen.push(v8::Global::new(scope, v));
        let mut out = Vec::with_capacity(a.length() as usize);
        for i in 0..a.length() {
            let e = match a.get_index(scope, i) {
                Some(e) => e,
                None => {
                    seen.pop();
                    return None;
                }
            };
            match js_to_key_seen(scope, e, seen) {
                Some(k) => out.push(k),
                None => {
                    seen.pop();
                    return None;
                }
            }
        }
        seen.pop();
        Some(Key::Array(out))
    } else {
        None
    }
}

/// Reconstruct a JS value from an IndexedDB key (record keys are stored as `Key`, not the original JS).
pub(crate) fn key_to_js<'s>(scope: &mut v8::PinScope<'s, '_>, k: &Key) -> v8::Local<'s, v8::Value> {
    match k {
        Key::Number(n) => v8::Number::new(scope, *n).into(),
        Key::Date(t) => v8::Date::new(scope, *t).map(|d| d.into()).unwrap_or_else(|| v8::null(scope).into()),
        Key::String(s) => v8::String::new(scope, s).map(|s| s.into()).unwrap_or_else(|| v8::null(scope).into()),
        Key::Binary(b) => {
            let store = v8::ArrayBuffer::new_backing_store_from_vec(b.clone()).make_shared();
            v8::ArrayBuffer::with_backing_store(scope, &store).into()
        }
        Key::Array(items) => {
            let arr = v8::Array::new(scope, items.len() as i32);
            for (i, it) in items.iter().enumerate() {
                let v = key_to_js(scope, it);
                arr.set_index(scope, i as u32, v);
            }
            arr.into()
        }
    }
}

/// "Evaluate a key path on a value" (IndexedDB § 6.5): the JS value at the path, or `None` for the
/// algorithm's failure.
///
/// TWO BASE CASES WERE MISSING, and both are in the algorithm's first two steps.
///
///  1. THE EMPTY KEY PATH IS THE IDENTITY -- it evaluates to the value itself. `"".split('.')` yields
///     one element, the empty string, so the loop below ran once, asked the value for a property
///     named `""`, and answered `None`. Five WPT files build their index with
///     `objStore.createIndex('index', '')` -- the documented way to index a store of primitives -- so
///     `idb_index_keys` turned every record into `Vec::new()`, the index was EMPTY, and every cursor
///     over it was `null` at the first `onsuccess`. That is what `idbcursor-primarykey`,
///     `idbcursor-continue`, `idbcursor-advance` and `idbcursor_continue_invalid` were all reporting
///     as "Cannot read properties of null".
///
///  2. A STRING'S `length`. The algorithm reads `length` off a String value directly; any other
///     identifier on a primitive is a failure. The old `cur.is_string()` guard on the first line
///     pretended to allow this and was dead code: it admitted the string and the very next line,
///     `Local::<Object>::try_from`, rejected it. (An Array's `length` needs no case of its own -- it
///     is an own property of the object.)
pub(crate) fn extract_key_val<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'s, v8::Value>, key_path: &str) -> Option<v8::Local<'s, v8::Value>> {
    // A SEQUENCE key path (its parts joined by NUL, `idb_index_key_path`) evaluates each part, and the key is the array
    // of their values -- every part must give one. YouTube's `ResponseStore` is keyed by `["key", "clientName"]`; this
    // read the joined string as one dotted path, so every `put` threw `DataError` and Chrome's did not.
    if key_path.contains('\u{0}') {
        let parts: Vec<&str> = key_path.split('\u{0}').collect();
        let arr = v8::Array::new(scope, parts.len() as i32);
        for (i, p) in parts.iter().enumerate() {
            let v = extract_key_val(scope, value, p)?;
            arr.set_index(scope, i as u32, v);
        }
        return Some(arr.into());
    }
    if key_path.is_empty() {
        return Some(value);
    }
    let mut cur = value;
    for p in key_path.split('.') {
        if cur.is_string() {
            if p != "length" {
                return None;
            }
            let s = v8::Local::<v8::String>::try_from(cur).ok()?;
            cur = v8::Number::new(scope, s.length() as f64).into();
            continue;
        }
        if !cur.is_object() {
            return None;
        }
        let obj = v8::Local::<v8::Object>::try_from(cur).ok()?;
        let k = v8::String::new(scope, p)?;
        cur = obj.get(scope, k.into())?;
        if cur.is_undefined() {
            return None;
        }
    }
    Some(cur)
}

/// A sequence key path (its parts joined by NUL) as the array `keyPath` reads.
pub(crate) fn key_path_array<'s>(scope: &mut v8::PinScope<'s, '_>, key_path: &str) -> v8::Local<'s, v8::Value> {
    let parts: Vec<&str> = key_path.split('\u{0}').collect();
    let arr = v8::Array::new(scope, parts.len() as i32);
    for (i, p) in parts.iter().enumerate() {
        let s = crate::binding::protos::str_val(scope, p);
        arr.set_index(scope, i as u32, s);
    }
    arr.into()
}

/// WebIDL `[EnforceRange] unsigned long`: ToNumber, truncate, then the range check the extended
/// attribute demands -- NaN, ±Infinity and anything outside [0, 2^32) are a TypeError, never a
/// silent clamp.
///
/// `Err(())` means an exception is thrown and the caller must return; `None` out of `number_value`
/// is the CALLER'S `valueOf` having thrown, so that exception is left standing rather than replaced
/// with ours.
///
/// `IDBCursor.advance` and getAll's `count` are the same WebIDL type and had two different
/// treatments: `advance` read `.number_value().unwrap_or(0.0)` and rejected on `count < 1.0`, which
/// is FALSE for NaN and for +Infinity -- so `cursor.advance({})`, `advance(undefined)`,
/// `advance(NaN)` and `advance(Infinity)` all sailed through. They looked green only because the
/// five files that test them build their index with `createIndex('index', '')`, so the cursor was
/// `null` and the TypeError the test asserts came from dereferencing null.
pub(crate) fn idb_enforce_range_u32(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<u32, ()> {
    let n = match v.number_value(scope) { Some(n) => n, None => return Err(()) };
    let t = n.trunc();
    if !n.is_finite() || t < 0.0 || t > 4294967295.0 {
        crate::binding::protos::throw_type_error(scope, "the value is outside the unsigned long range");
        return Err(());
    }
    Ok(t as u32)
}

/// "Valid key path" (IndexedDB § 6.6): the empty string, an identifier, identifiers joined by `.`,
/// or a non-empty sequence of those. `createIndex` owes a `SyntaxError` to anything else and never
/// asked, so `store.createIndex("ab", ".")` built an index on a path that can never evaluate.
///
/// A SEQUENCE ARRIVES HERE AS ITS PARTS JOINED BY NUL (`idb_index_key_path`), which is the same
/// marker `IDBIndex.keyPath` splits on to hand the array back.
pub(crate) fn idb_valid_key_path(key_path: &str) -> bool {
    fn one(p: &str) -> bool {
        if p.is_empty() {
            return true;
        }
        p.split('.').all(|id| {
            let mut c = id.chars();
            match c.next() {
                Some(f) if f == '$' || f == '_' || f.is_alphabetic() => {}
                _ => return false,
            }
            c.all(|ch| ch == '$' || ch == '_' || ch.is_alphanumeric())
        })
    }
    if key_path.contains('\u{0}') {
        return key_path.split('\u{0}').all(one);
    }
    one(key_path)
}

pub(crate) struct IdxRec {
    pub(crate) name: String,
    pub(crate) key_path: String,
    pub(crate) unique: bool,
    pub(crate) multi_entry: bool,
}
pub(crate) struct StoreRec {
    pub(crate) key_path: Option<String>,
    pub(crate) auto_increment: bool,
    pub(crate) key_gen: f64,
    pub(crate) records: Vec<(Key, v8::Global<v8::Value>)>, // sorted by key
    pub(crate) indexes: Vec<IdxRec>,
}
pub(crate) struct DbRec {
    pub(crate) version: f64,
    /// The version the backend has COMMITTED -- what `databases()` reports. An upgrade
    /// raises `version` when it starts and this when it commits; 0 for a database whose first upgrade has not
    /// committed, which `databases()` does not list (Chrome 150: asked inside `upgradeneeded`, a database
    /// being created is absent and one being upgraded from 1 to 2 reads version 1).
    pub(crate) committed: f64,
    pub(crate) stores: std::collections::BTreeMap<String, StoreRec>,
}
#[derive(Default)]
pub(crate) struct IdbBackend {
    pub(crate) dbs: std::collections::BTreeMap<String, DbRec>, // pub(crate): read by modules::indexeddb.
    /// Every `IDBDatabase` a request handed out, per database name, in every realm of the page (a
    /// frame's connections share the page's databases). Closed ones are pruned when read.
    pub(crate) conns: std::collections::BTreeMap<String, Vec<v8::Global<v8::Value>>>,
    /// `deleteDatabase` and upgrading `open` requests that other open connections are blocking, in
    /// request order; `IDBDatabase.close()` resumes them once the last one closes.
    pub(crate) waiters: Vec<IdbWaiter>,
    /// IndexedDB's connection queues, one per database name: the `open` and
    /// `deleteDatabase` requests not yet processed, as their run callbacks, in the order the page made them.
    /// Only the head runs; the next starts once it is processed (`idb_factory::idb_request_processed`).
    pub(crate) queues: std::collections::BTreeMap<String, std::collections::VecDeque<(v8::Global<v8::Function>, bool)>>,
    /// When the backend has answered every factory request started so far (`idb_factory::idb_start_request`):
    /// a `databases()` answers after them.
    pub(crate) busy_until: Option<std::time::Instant>,
    /// The write transactions (`readwrite`, `versionchange`) running on each database: while any is, its records hold
    /// writes no commit has made the context's yet, and no pull replaces them ([`super::sync`]).
    pub(crate) writers: std::collections::BTreeMap<String, u32>,
    /// The context-store revision each database was last brought current at ([`super::sync`]).
    pub(crate) synced: std::collections::BTreeMap<String, u64>,
}
/// A blocked request: its database name and the `data` array its run callback reads.
pub(crate) struct IdbWaiter {
    pub(crate) name: String,
    pub(crate) data: v8::Global<v8::Value>,
    pub(crate) delete: bool,
}
impl IdbBackend {
    pub(crate) fn db(&self, name: &str) -> Option<&DbRec> { self.dbs.get(name) }
    pub(crate) fn db_mut(&mut self, name: &str) -> Option<&mut DbRec> { self.dbs.get_mut(name) }
}

pub(crate) struct IdbPending {
    pub(crate) req: v8::Global<v8::Value>,
    pub(crate) result: Option<v8::Global<v8::Value>>,
    pub(crate) error: Option<(String, String)>, // (DOMException name, message)
}

pub(crate) enum IdbUndo {
    Insert { store: String, key: Key },                       // undo an insert -> remove
    Replace { store: String, key: Key, old: v8::Global<v8::Value> }, // undo an overwrite -> restore
    Remove { store: String, key: Key, val: v8::Global<v8::Value> },  // undo a delete -> reinsert
}

pub(crate) fn idb_new_request() -> IdbRequest {
    IdbRequest {
        result: std::cell::RefCell::new(None),
        error: std::cell::RefCell::new(None),
        source: std::cell::RefCell::new(None),
        transaction: std::cell::RefCell::new(None),
        ready_state: std::cell::RefCell::new("pending".to_string()),
    }
}

pub(crate) fn idb_new_tx(db_name: String, scope: Vec<String>, mode: String) -> IdbTransaction {
    let is_upgrade = mode == "versionchange";
    IdbTransaction {
        db_name,
        db: std::cell::RefCell::new(None),
        mode,
        scope: std::cell::RefCell::new(scope),
        pending: std::cell::RefCell::new(std::collections::VecDeque::new()),
        undo: std::cell::RefCell::new(Vec::new()),
        error: std::cell::RefCell::new(None),
        open_req: std::cell::RefCell::new(None),
        store_handles: std::cell::RefCell::new(std::collections::HashMap::new()),
        active: std::cell::Cell::new(false),
        finished: std::cell::Cell::new(false),
        committing: std::cell::Cell::new(false),
        aborted: std::cell::Cell::new(false),
        is_upgrade: std::cell::Cell::new(is_upgrade),
        pumping: std::cell::Cell::new(false),
        upgrade_old: std::cell::Cell::new(0.0),
    }
}

pub(crate) fn idb_clone_global(scope: &mut v8::PinScope, g: &v8::Global<v8::Value>) -> v8::Global<v8::Value> {
    let l = v8::Local::new(scope, g);
    v8::Global::new(scope, l)
}

/// The index keys a value contributes (multiEntry array expands; deduped), mirroring `indexKeysFor`.
pub(crate) fn idb_index_keys<'s>(scope: &mut v8::PinScope<'s, '_>, idx: &IdxRec, value: v8::Local<'s, v8::Value>) -> Vec<Key> {
    let raw = match extract_key_val(scope, value, &idx.key_path) { Some(v) => v, None => return Vec::new() };
    if idx.multi_entry && raw.is_array() {
        let arr = match v8::Local::<v8::Array>::try_from(raw) { Ok(a) => a, Err(_) => return Vec::new() };
        let mut out: Vec<Key> = Vec::new();
        for i in 0..arr.length() {
            if let Some(e) = arr.get_index(scope, i) {
                if let Some(k) = js_to_key(scope, e) {
                    if !out.iter().any(|x| x.cmp(&k) == std::cmp::Ordering::Equal) { out.push(k); }
                }
            }
        }
        out
    } else {
        js_to_key(scope, raw).map(|k| vec![k]).unwrap_or_default()
    }
}

/// Store a record (add/put). Returns the undo step; `Err((name,msg))` = a DOMException the request fails
/// with (ConstraintError). Mutates the backend synchronously (the observable result is deferred by the drain).
pub(crate) fn idb_do_write(scope: &mut v8::PinScope, backend: &mut IdbBackend, db: &str, store: &str, key: Key, value: v8::Global<v8::Value>, is_add: bool) -> Result<IdbUndo, (String, String)> {
    // Precompute against an immutable view, then mutate, to keep borrows simple.
    let (pos, unique_bad) = {
        let s = match backend.db(db).and_then(|d| d.stores.get(store)) { Some(s) => s, None => return Err(("UnknownError".into(), "no store".into())) };
        let keys: Vec<Key> = s.records.iter().map(|(k, _)| k.clone()).collect();
        let pos = exact_pos(&keys, &key);
        let mut bad = false;
        for idx in &s.indexes {
            if !idx.unique { continue; }
            let vloc = v8::Local::new(scope, &value);
            let new_ik = idb_index_keys(scope, idx, vloc);
            for (rk, rv) in &s.records {
                if pos.is_some() && rk.cmp(&key) == std::cmp::Ordering::Equal { continue; }
                let rvloc = v8::Local::new(scope, rv);
                let ex_ik = idb_index_keys(scope, idx, rvloc);
                if new_ik.iter().any(|a| ex_ik.iter().any(|b| a.cmp(b) == std::cmp::Ordering::Equal)) { bad = true; break; }
            }
            if bad { break; }
        }
        (pos, bad)
    };
    if is_add && pos.is_some() { return Err(("ConstraintError".into(), "key already exists".into())); }
    if unique_bad { return Err(("ConstraintError".into(), "unique index constraint".into())); }
    let s = backend.db_mut(db).and_then(|d| d.stores.get_mut(store)).unwrap();
    if let Some(p) = pos {
        let old = idb_clone_global(scope, &s.records[p].1);
        s.records[p] = (key.clone(), value);
        Ok(IdbUndo::Replace { store: store.into(), key, old })
    } else {
        let keys: Vec<Key> = s.records.iter().map(|(k, _)| k.clone()).collect();
        let ip = insert_pos(&keys, &key);
        s.records.insert(ip, (key.clone(), value));
        Ok(IdbUndo::Insert { store: store.into(), key })
    }
}

/// Delete all records whose key is in `range`; returns undo steps (reinsertions).
pub(crate) fn idb_do_delete(scope: &mut v8::PinScope, backend: &mut IdbBackend, db: &str, store: &str, range: &KeyRange) -> Vec<IdbUndo> {
    let mut undos = Vec::new();
    let s = match backend.db_mut(db).and_then(|d| d.stores.get_mut(store)) { Some(s) => s, None => return undos };
    let mut i = 0;
    while i < s.records.len() {
        if range.includes(&s.records[i].0) {
            let (k, v) = s.records.remove(i);
            let saved = idb_clone_global(scope, &v);
            undos.push(IdbUndo::Remove { store: store.into(), key: k, val: saved });
        } else {
            i += 1;
        }
    }
    undos
}

pub(crate) fn idb_dom_exception<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, msg: &str) -> v8::Local<'s, v8::Value> {
    let ctx = scope.get_current_context();
    let global = ctx.global(scope);
    let ctor = v8::String::new(scope, "DOMException").and_then(|k| global.get(scope, k.into())).and_then(|c| v8::Local::<v8::Function>::try_from(c).ok());
    if let Some(ctor) = ctor {
        let m = str_val(scope, msg);
        let n = str_val(scope, name);
        if let Some(ex) = ctor.new_instance(scope, &[m, n]) {
            crate::binding::protos::capture_native_stack(scope, ex);
            return ex.into();
        }
    }
    str_val(scope, msg)
}

/// Build `new Event(type, {bubbles, cancelable: bubbles})` with `.target` set -- the REALM's `Event`
/// (measured: F-engine-mints-realm-protos): it was `window.Event`, a name the page
/// can write, and a page that replaced it got no IndexedDB event at all.
pub(crate) fn idb_new_event<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, target: v8::Local<v8::Value>, bubbles: bool) -> Option<v8::Local<'s, v8::Value>> {
    let evc = v8::Local::<v8::Function>::try_from(crate::binding::protos::iface_object(scope, "Event")?).ok()?;
    let ty_s = str_val(scope, ty);
    let opts = crate::binding::protos::init_dict(scope);
    set_bool(scope, opts, "bubbles", bubbles);
    set_bool(scope, opts, "cancelable", bubbles);
    let ev = evc.new_instance(scope, &[ty_s, opts.into()])?;
    // TRUSTED: the user agent fires it, so `isTrusted` is true, as on every IndexedDB event Chrome
    // dispatches (measured: F-idb-events-trusted); a `new Event` is not.
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    // `target` is a prototype accessor over a private slot now (core/events/event.rs), not a
    // property -- assigning to it would hit a getter-only descriptor and silently do nothing, which
    // is what broke every IndexedDB request handler that reads `event.target.result`.
    crate::core::events::event::evt_slot_write(scope, ev, "target", target);
    Some(ev.into())
}

/// `upgradeneeded` (and `versionchange`) is an **IDBVersionChangeEvent**, not a plain Event.
///
/// Chrome, probed 2026-09-10: `Object.prototype.toString.call(ev)` is `[object IDBVersionChangeEvent]`
/// and `Object.getOwnPropertyNames(ev)` is `["isTrusted"]` -- the two versions are read back through
/// `IDBVersionChangeEvent.prototype`'s accessors, not as own data properties. This builds exactly that:
/// an instance of the interface's own template (B34), with the four members on the event slots the
/// table's getters read.
pub(crate) fn idb_new_version_change_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ty: &str,
    target: v8::Local<v8::Value>,
    old_version: f64,
    new_version: Option<f64>,
) -> Option<v8::Local<'s, v8::Value>> {
    // An `IDBVersionChangeEvent` from its own template, the realm's, with `Event`'s constructor steps on it:
    // it was the base `Event` above re-prototyped.
    let proto = shell_proto(scope, "IDBVersionChangeEvent")?;
    let evo = crate::binding::protos::mint_on(scope, proto.into());
    let opts = crate::binding::protos::init_dict(scope);
    set_bool(scope, opts, "bubbles", false);
    set_bool(scope, opts, "cancelable", false);
    crate::core::events::ctor::init_event(scope, evo, ty, Some(opts));
    // trusted, as `idb_new_event`'s (B36)
    crate::core::html::html_dialog_element::mark_trusted(scope, evo);
    crate::core::events::event::evt_slot_write(scope, evo, "target", target);
    crate::modules::indexeddb::tables::vce_write_slots(scope, evo, old_version, new_version, "none", "");
    Some(evo.into())
}

/// Fire an event on `obj`: `obj.dispatchEvent(ev)`. The `on<type>` handler is an event handler
/// listener in the same list as addEventListener's, so dispatch runs it in registration order.
pub(crate) fn idb_fire(scope: &mut v8::PinScope, obj: v8::Local<v8::Value>, ev: v8::Local<v8::Value>, _ty: &str) {
    crate::binding::protos::engine_call(scope, obj, "dispatchEvent", &[ev]);
}

/// Schedule a drain of `tx` on a TASK (idempotent via the tx `pumping` flag).
///
/// A TASK, NOT A MICROTASK -- this is the scheduling defect that was found here.
///
/// IndexedDB's "asynchronously execute a request" says to **queue a task** to fire the success or error
/// event. Korvus queued a microtask, and the difference is not academic: V8 drains the microtask queue
/// to EXHAUSTION before it returns to the task queue, so a success handler that starts another request
/// re-arms the drain from inside the same checkpoint and the checkpoint never ends. Every timer on the
/// page is starved, forever, and each turn allocates another `IDBRequest`.
///
/// That is not a hypothetical shape: `IndexedDB/resources/support.js`'s `keep_alive()` is written to
/// spin exactly that way (`tx.objectStore(s).get(0).onsuccess = spin`), and it is released only from a
/// `setTimeout(…, 0)` -- so under a microtask drain the release can never run.
/// `IndexedDB/event-dispatch-active-flag.any.js` therefore never terminated, and grew at a measured
/// 13.2 MB/s (2026-09-11: 19 MB at 0 s, 1,191 MB at 85 s, still climbing when a 2 GB cap fired).
/// On a task queue the spin is exactly what a browser does with it: each success is one task, the
/// page's own `setTimeout(0)` interleaves with them, `release_tx()` runs, and the transaction commits.
///
/// It uses the page's own `setTimeout`, which is how every other deferred delivery in this crate
/// schedules a task (`message_port.rs`, `broadcast_channel.rs`, `cookie_store/jar.rs`,
/// `xml_http_request.rs`). A page that replaces `setTimeout` therefore changes IDB delivery -- in a
/// browser the database-access task source is not reachable from script. That is a known, shared
/// limitation of the task hop in this crate and not something this unit invents; closing it means
/// giving the kernel a host task source that all five sites use, which is a `Context` service change.
pub(crate) fn idb_pump<'s>(scope: &mut v8::PinScope<'s, '_>, tx_val: v8::Local<'s, v8::Value>) {
    if let Some(tx) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) {
        if unsafe { tx.as_ref() }.pumping.get() {
            return;
        }
        unsafe { tx.as_ref() }.pumping.set(true);
    } else {
        return;
    }
    idb_queue_drain_task(scope, tx_val);
}

/// `setTimeout(() => cb_idb_drain(tx), 0)` -- one turn of the transaction's request queue, as a task.
fn idb_queue_drain_task<'s>(scope: &mut v8::PinScope<'s, '_>, tx_val: v8::Local<'s, v8::Value>) {
    let fnv = build_fn_data(scope, cb_idb_drain, "", 0, tx_val);
    // The engine's task, not the page's `setTimeout`.
    crate::core::intersection_observer::rendering::engine_task(scope, fnv, 0.0);
}

/// Clear the transaction's active flag. Queued as a MICROTASK by the three sites that open an active
/// window (`cb_idb_drain`, `IDBDatabase.transaction`, the `versionchange` upgrade), so it runs at the
/// END of that task's checkpoint -- see the call sites.
///
/// It is also where a transaction ENTERS THE COMMITTING STATE. The spec starts the commit as soon as
/// the transaction's active flag is unset and its request list is empty, and `abort()` on a committing
/// transaction is an `InvalidStateError` -- which `IDBTransaction.abort` already implements, but
/// nothing ever set the flag except an explicit `commit()`. It went unnoticed while request events
/// were delivered on microtasks, because the whole queue then drained inside one checkpoint and the
/// transaction was already `finished` by the next task. Now that delivery is a task,
/// `IndexedDB/idbtransaction_abort.any.js`'s "Abort during auto-committing should throw
/// InvalidStateError" runs in the window the spec describes, and without this the abort succeeded.
pub(crate) fn cb_idb_clear_active(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    if let Some(tx) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, __body_data) {
        let t = unsafe { tx.as_ref() };
        t.active.set(false);
        if !t.finished.get() && t.pending.borrow().is_empty() {
            t.committing.set(true);
        }
    }
}

/// Drain one pending request of the front transaction, fire its success/error, reschedule or commit.
pub(crate) fn cb_idb_drain(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // Round-trip through a Global so tx_val has the scope lifetime (needed to re-bake the reschedule fn).
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let tx_g = v8::Global::new(scope, __body_data);
    let tx_val = v8::Local::new(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    if unsafe { txh.as_ref() }.finished.get() {
        unsafe { txh.as_ref() }.pumping.set(false);
        return;
    }
    let pending = unsafe { txh.as_ref() }.pending.borrow_mut().pop_front();
    match pending {
        Some(p) => {
            unsafe { txh.as_ref() }.active.set(true);
            let req_val = v8::Local::new(scope, &p.req);
            if let Some(reqh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, req_val) {
                *unsafe { reqh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
                if let Some((name, msg)) = p.error {
                    let ex = idb_dom_exception(scope, &name, &msg);
                    *unsafe { reqh.as_ref() }.error.borrow_mut() = Some(v8::Global::new(scope, ex));
                    let ev = idb_new_event(scope, "error", req_val, true);
                    if let Some(ev) = ev {
                        idb_fire(scope, req_val, ev, "error");
                        idb_fire(scope, tx_val, ev, "error");
                        if let Some(db) = unsafe { txh.as_ref() }.db.borrow().clone() {
                            let dbv = v8::Local::new(scope, &db);
                            idb_fire(scope, dbv, ev, "error");
                        }
                        let prevented = v8::Local::<v8::Object>::try_from(ev).ok().and_then(|o| get_prop(scope, o, "defaultPrevented")).map(|v| v.boolean_value(scope)).unwrap_or(false);
                        if !prevented && !unsafe { txh.as_ref() }.finished.get() {
                            if unsafe { txh.as_ref() }.error.borrow().is_none() {
                                *unsafe { txh.as_ref() }.error.borrow_mut() = Some(v8::Global::new(scope, ex));
                            }
                            unsafe { txh.as_ref() }.active.set(false);
                            idb_finish_tx(scope, tx_val, "abort");
                            return;
                        }
                    }
                } else {
                    if let Some(r) = &p.result {
                        // THE CURSOR'S "GOT VALUE FLAG" IS SET HERE, NOT WHERE THE STEP WAS QUEUED.
                        // The spec sets it when "iterate a cursor" finds a record and the request's
                        // result becomes the cursor -- which is this line. Setting it at
                        // `continue()`/`advance()` time instead would leave it true for the whole
                        // synchronous window those members are supposed to close, and the window IS
                        // the observable: `cursor.continue(); cursor.continue();` in one handler must
                        // throw InvalidStateError on the second call.
                        let rv = v8::Local::new(scope, r);
                        if let Some(ch) = deno_core::cppgc::try_unwrap_cppgc_object::<crate::modules::indexeddb::IdbCursor>(scope, rv) {
                            unsafe { ch.as_ref() }.got_value.set(true);
                        }
                        *unsafe { reqh.as_ref() }.result.borrow_mut() = Some(idb_clone_global(scope, r));
                    }
                    if let Some(ev) = idb_new_event(scope, "success", req_val, false) {
                        idb_fire(scope, req_val, ev, "success");
                    }
                }
            }
            // THE ACTIVE FLAG SURVIVES THE MICROTASK CHECKPOINT. Spec: a transaction's active flag is
            // set while the event is dispatched and unset "when control returns to the event loop" --
            // which is AFTER the microtask checkpoint of the task that dispatched it, not before it.
            // Clearing it on this line (which is what this code did) made
            // `assert_true(is_transaction_active(tx), 'Transaction should be active in microtasks')`
            // fail in `IndexedDB/event-dispatch-active-flag.any.js`, all four subtests: a
            // `Promise.resolve().then()` scheduled inside the success handler saw an inactive
            // transaction where Chrome shows an active one.
            //
            // Queueing the clear as a microtask HERE puts it behind every microtask the handler itself
            // scheduled (they were enqueued during the dispatch above), so the handler's promise jobs
            // still see an active transaction and the next TASK -- the `setTimeout(0)` the same test
            // asserts against -- sees an inactive one. That is the whole observable lifetime the file
            // is named after.
            let clear = build_fn_data(scope, cb_idb_clear_active, "", 0, tx_val);
            idb_queue_microtask(scope, clear);
            idb_queue_drain_task(scope, tx_val);
        }
        None => {
            idb_finish_tx(scope, tx_val, "complete");
            unsafe { txh.as_ref() }.pumping.set(false);
        }
    }
}

/// Commit or abort a transaction: undo on abort, fire complete/abort, resolve the open request.
pub(crate) fn idb_finish_tx(scope: &mut v8::PinScope, tx_val: v8::Local<v8::Value>, reason: &str) {
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    if unsafe { txh.as_ref() }.finished.get() {
        return;
    }
    unsafe { txh.as_ref() }.finished.set(true);
    unsafe { txh.as_ref() }.active.set(false);
    let db_name = unsafe { txh.as_ref() }.db_name.clone();
    // A write transaction's end: an abort leaves the records as they were, a `readwrite` commit makes them the
    // context's. An upgrade's commit comes later (`cb_idb_upgrade_committed`).
    let writes = unsafe { txh.as_ref() }.mode != "readonly";
    let upgrade_commit = reason != "abort" && unsafe { txh.as_ref() }.open_req.borrow().is_some();
    if writes && !upgrade_commit {
        {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            if let Some(backend) = st.try_borrow_mut::<IdbBackend>() {
                super::sync::writer_ended(backend, &db_name);
            }
        }
        if reason != "abort" {
            super::sync::push(scope, &db_name);
        }
    }
    // Clear the connection's active-upgrade pointer.
    if let Some(db) = unsafe { txh.as_ref() }.db.borrow().clone() {
        let dbv = v8::Local::new(scope, &db);
        if let Some(dbh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbDatabase>(scope, dbv) {
            unsafe { dbh.as_ref() }.active_upgrade_tx.borrow_mut().take();
        }
    }
    if reason == "abort" {
        // Undo data mutations in reverse.
        let undos: Vec<IdbUndo> = unsafe { txh.as_ref() }.undo.borrow_mut().drain(..).collect();
        {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            let backend = st.borrow_mut::<IdbBackend>();
            for u in undos.into_iter().rev() {
                match u {
                    IdbUndo::Insert { store, key } => {
                        if let Some(s) = backend.db_mut(&db_name).and_then(|d| d.stores.get_mut(&store)) {
                            let keys: Vec<Key> = s.records.iter().map(|(k, _)| k.clone()).collect();
                            if let Some(p) = exact_pos(&keys, &key) { s.records.remove(p); }
                        }
                    }
                    IdbUndo::Replace { store, key, old } => {
                        if let Some(s) = backend.db_mut(&db_name).and_then(|d| d.stores.get_mut(&store)) {
                            let keys: Vec<Key> = s.records.iter().map(|(k, _)| k.clone()).collect();
                            if let Some(p) = exact_pos(&keys, &key) { s.records[p] = (key, old); }
                        }
                    }
                    IdbUndo::Remove { store, key, val } => {
                        if let Some(s) = backend.db_mut(&db_name).and_then(|d| d.stores.get_mut(&store)) {
                            let keys: Vec<Key> = s.records.iter().map(|(k, _)| k.clone()).collect();
                            let ip = insert_pos(&keys, &key);
                            s.records.insert(ip, (key, val));
                        }
                    }
                }
            }
        }
        // Roll back schema for an aborted upgrade: a freshly-created db (old version 0) is removed
        // entirely; an existing-db upgrade reverts its version. So a re-open re-runs upgradeneeded.
        if unsafe { txh.as_ref() }.is_upgrade.get() {
            let old = unsafe { txh.as_ref() }.upgrade_old.get();
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            let backend = st.borrow_mut::<IdbBackend>();
            if old <= 0.0 { backend.dbs.remove(&db_name); }
            else if let Some(d) = backend.db_mut(&db_name) { d.version = old; }
        }
        unsafe { txh.as_ref() }.aborted.set(true);
        if let Some(ev) = idb_new_event(scope, "abort", tx_val, true) {
            idb_fire(scope, tx_val, ev, "abort");
        }
        if let Some(oreq) = unsafe { txh.as_ref() }.open_req.borrow().clone() {
            let oreqv = v8::Local::new(scope, &oreq);
            if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, oreqv) {
                let ex = idb_dom_exception(scope, "AbortError", "aborted");
                *unsafe { rh.as_ref() }.error.borrow_mut() = Some(v8::Global::new(scope, ex));
                *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
            }
            if let Some(ev) = idb_new_event(scope, "error", oreqv, true) {
                idb_fire(scope, oreqv, ev, "error");
            }
            crate::modules::indexeddb::idb_factory::idb_request_processed(scope, &db_name);
        }
    } else if unsafe { txh.as_ref() }.open_req.borrow().is_some() {
        // An upgrade commits the database's new metadata before its `complete` and the open's `success`,
        // which arrive together: Chrome 150 fires them 3-9 ms after `upgradeneeded`
        // (after a 2 ms timer queued in the `upgradeneeded` handler and before a 4 ms one when warm). They
        // came in the same task as the transaction's last request here.
        let tx_here = v8::Global::new(scope, tx_val);
        let tx_here = v8::Local::new(scope, &tx_here);
        let committed = build_fn_data(scope, cb_idb_upgrade_committed, "", 0, tx_here);
        crate::core::intersection_observer::rendering::queue_task(scope, committed, UPGRADE_COMMIT_MS);
    } else if let Some(ev) = idb_new_event(scope, "complete", tx_val, false) {
        idb_fire(scope, tx_val, ev, "complete");
    }
}

/// What an upgrade's commit takes (see `idb_finish_tx`).
const UPGRADE_COMMIT_MS: f64 = 3.0;

/// An upgrade committed: the transaction's `complete`, the open request's `success`, and the next request
/// of the database's connection queue.
fn cb_idb_upgrade_committed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let tx_val = crate::binding::protos::body_data(scope, &args);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let db_name = unsafe { txh.as_ref() }.db_name.clone();
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let backend = st.borrow_mut::<IdbBackend>();
        if let Some(d) = backend.db_mut(&db_name) {
            d.committed = d.version;
        }
        super::sync::writer_ended(backend, &db_name);
    }
    super::sync::push(scope, &db_name);
    if let Some(ev) = idb_new_event(scope, "complete", tx_val, false) {
        idb_fire(scope, tx_val, ev, "complete");
    }
    let open_req = unsafe { txh.as_ref() }.open_req.borrow().clone();
    if let Some(oreq) = open_req {
        let oreqv = v8::Local::new(scope, &oreq);
        if let Some(rh) = deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, oreqv) {
            *unsafe { rh.as_ref() }.ready_state.borrow_mut() = "done".to_string();
        }
        if let Some(ev) = idb_new_event(scope, "success", oreqv, false) {
            idb_fire(scope, oreqv, ev, "success");
        }
        crate::modules::indexeddb::idb_factory::idb_request_processed(scope, &db_name);
    }
}

pub(crate) fn idb_throw_type(scope: &mut v8::PinScope, msg: &str) {
    // Blink's context applied after the error is made
    crate::binding::protos::throw_type_error(scope, msg);
}

pub(crate) fn idb_g<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> v8::Global<v8::Value> { v8::Global::new(scope, v) }
pub(crate) fn idb_l<'s>(scope: &mut v8::PinScope<'s, '_>, g: &v8::Global<v8::Value>) -> v8::Local<'s, v8::Value> { v8::Local::new(scope, g) }

/// Set an object's prototype to the realm's prototype for `name` -- the TABLE's prototype, since the twelve
/// IndexedDB interfaces are `interface!` tables (`modules/indexeddb/tables.rs`) installed by THE
/// installer into both realms.
///
/// EVERY cppgc mint site in this module calls this. deno_core hands a fresh `make_cppgc_object` the
/// CLASS TEMPLATE's prototype, which no page can name any more (the `#[op2] impl` blocks are reachable
/// only through `ext:core/ops`, and nothing imports them now); without the re-prototype an instance
/// would answer a second, invisible prototype for `instanceof`, `constructor` and
/// `Object.getPrototypeOf`, which is the exact split-brain the one-table rule forbids. The cppgc brand
/// is unaffected -- `try_unwrap_cppgc_object` reads the wrapper tag, not the prototype chain.
pub(crate) fn idb_set_proto(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) {
    if let Some(p) = shell_proto(scope, name) {
        crate::binding::protos::adopt_as(scope, obj, p.into());
    }
}

pub(crate) fn idb_make_request<'s>(scope: &mut v8::PinScope<'s, '_>, source: Option<v8::Global<v8::Value>>, tx: Option<v8::Global<v8::Value>>, open: bool) -> v8::Local<'s, v8::Value> {
    let r = idb_new_request();
    *r.source.borrow_mut() = source;
    *r.transaction.borrow_mut() = tx;
    // an open request is an `IDBOpenDBRequest` from its own template
    let iface = if open { "IDBOpenDBRequest" } else { "IDBRequest" };
    let obj = crate::binding::class::mint_as(scope, r, iface);
    idb_set_proto(scope, obj, iface);
    obj.into()
}
pub(crate) fn idb_make_object_store<'s>(scope: &mut v8::PinScope<'s, '_>, db_name: &str, store_name: &str, tx_val: v8::Local<v8::Value>, index_name: Option<String>) -> v8::Local<'s, v8::Value> {
    let os = IdbObjectStore {
        db_name: db_name.to_string(),
        store_name: std::cell::RefCell::new(store_name.to_string()),
        tx: std::cell::RefCell::new(Some(idb_g(scope, tx_val))),
        index_name: std::cell::RefCell::new(index_name),
        index_handles: std::cell::RefCell::new(std::collections::HashMap::new()),
        key_path_cache: std::cell::RefCell::new(None),
    };
    let obj = crate::binding::class::mint(scope, os);
    idb_set_proto(scope, obj, "IDBObjectStore");
    obj.into()
}

/// Mint an `IDBTransaction` over the table's prototype (the two sites that start one: an upgrade in
/// `IDBFactory.open`, and `IDBDatabase.transaction`).
pub(crate) fn idb_make_transaction<'s>(scope: &mut v8::PinScope<'s, '_>, tx: IdbTransaction) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, tx);
    idb_set_proto(scope, obj, "IDBTransaction");
    obj
}

/// Mint an `IDBDatabase` connection over the table's prototype.
pub(crate) fn idb_make_database<'s>(scope: &mut v8::PinScope<'s, '_>, db: IdbDatabase) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, db);
    idb_set_proto(scope, obj, "IDBDatabase");
    obj
}

pub(crate) fn idb_string_list<'s>(scope: &mut v8::PinScope<'s, '_>, names: &[String]) -> v8::Local<'s, v8::Value> {
    let mut sorted: Vec<String> = names.to_vec();
    sorted.sort();
    let arr = v8::Array::new(scope, sorted.len() as i32);
    for (i, n) in sorted.iter().enumerate() {
        let s = str_val(scope, n);
        arr.set_index(scope, i as u32, s);
    }
    // A `DOMStringList` (the one-table static list over these names), not the Array this used to hand
    // out: a page calls `db.objectStoreNames.contains(name)` in `onupgradeneeded`, and an Array has no
    // `contains` (a production site's IDB handler threw exactly that, corpus 2026-09-10).
    crate::core::dom::collections::make_static(scope, "DOMStringList", arr)
}

pub(crate) fn idb_queue_microtask(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    if let Some(qm) = crate::binding::protos::intrinsic_fn(scope, "queueMicrotask") {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        qm.call(scope, undef, &[f.into()]);
    }
}

pub(crate) fn idb_structured_clone<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> v8::Global<v8::Value> {
    if let Some(sc) = crate::binding::protos::intrinsic_fn(scope, "structuredClone") {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        if let Some(c) = sc.call(scope, undef, &[v]) {
            return v8::Global::new(scope, c);
        }
    }
    v8::Global::new(scope, v)
}

/// Throw a DOMException(name) into JS from a raw callback.
pub(crate) fn idb_throw(scope: &mut v8::PinScope, name: &str, msg: &str) {
    let ctx = scope.get_current_context();
    let global = ctx.global(scope);
    let ex_ctor = v8::String::new(scope, "DOMException")
        .and_then(|k| global.get(scope, k.into()))
        .and_then(|c| v8::Local::<v8::Function>::try_from(c).ok());
    if let Some(ctor) = ex_ctor {
        let m = v8::String::new(scope, msg).unwrap_or_else(|| v8::String::empty(scope));
        let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
        if let Some(ex) = ctor.new_instance(scope, &[m.into(), n.into()]) {
            crate::binding::protos::capture_native_stack(scope, ex);
            scope.throw_exception(ex.into());
            return;
        }
    }
    let m = v8::String::new(scope, msg).unwrap_or_else(|| v8::String::empty(scope));
    scope.throw_exception(m.into());
}

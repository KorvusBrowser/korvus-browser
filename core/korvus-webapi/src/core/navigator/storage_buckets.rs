//! `core/navigator/storage_buckets.rs` -- `navigator.storageBuckets` and its `StorageBucket`s, as Chrome 150
//! answers them (a fingerprinting library reads `open(name)`, the bucket's `indexedDB` and `estimate()`).
//!
//! Until this file `open()` resolved a bucket with a `name` and nothing else: `keys()`, `delete()` and every
//! bucket member were the registry's D2 stubs (`estimate()` resolved `undefined`, `indexedDB` and `caches` read
//! `undefined`). Chromium's own algorithm, end to end:
//!
//! * the renderer (`modules/buckets/storage_bucket_manager.cc`): arguments converted first -- the name, then the
//!   `StorageBucketOptions` dictionary, whose failures reject; an opaque origin rejects with SecurityError; a name
//!   that is not 1-63 of `[a-z0-9_-]` without a leading `_`/`-` rejects at once (`IsValidName`, with the name
//!   QUOTED for `open` and not for `delete` -- the two messages differ in Chromium's source and in Chrome); a
//!   `quota` of 0 rejects;
//! * the browser (`content/browser/buckets/bucket_manager_host.cc`, `storage/browser/quota/quota_manager_impl.cc`):
//!   `open` is "update or create" -- an expiration not in the future is "The bucket expiration is invalid.", an
//!   expired bucket is deleted and made again; `persisted: true` is kept only with the persistent-storage
//!   permission, which a page that never asked has not (`persisted()` false, measured); `keys()` drops the
//!   expired buckets and lists the rest sorted (`DidGetBuckets` inserts in order); `delete()` of a name with no
//!   bucket succeeds;
//! * each bucket (`modules/buckets/storage_bucket.cc`, `content/browser/buckets/bucket_host.cc`): `persist()`
//!   false without the permission, `persisted()`, `expires()` (null until set), `setExpires(ms)`, `estimate()`
//!   (`{quota, usage, usageDetails: {}}`, the quota the bucket's own or the origin's), and the bucket's OWN
//!   `indexedDB` and `caches` (`[SameObject]` per bucket object) and file system (`getDirectory()`) -- a database
//!   or cache made there is not the default bucket's (measured). Every answer from the browser arrives as a task;
//!   a bucket object whose bucket was deleted answers as `BucketHost` does for an id that no longer exists.
//!
//! The buckets are the process's (a profile's), shared by every realm and worker of an origin.
//!
//! NOT modelled: `estimate().usage` counts nothing -- Chrome's is the bytes its backing stores hold (1236 after an
//! IndexedDB database with one empty store, measured), and this engine accounts no storage usage anywhere
//! (`navigator.storage.estimate()` answers the persona's recorded figures).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

use deno_core::v8;

use crate::binding::protos::{build_fn, make_dom_exc, rejected_promise, rejected_type_error, settle_later, str_val, Settle};
use crate::install::live::method_row;
use crate::install::MemberDef;

// ── the browser's side: the buckets of every origin ─────────────────────────────────────────────

#[derive(Clone)]
struct Bucket {
    /// `BucketId`: a bucket object keeps answering for the bucket it was opened on, not for a later one of the same
    /// name.
    id: u64,
    persisted: bool,
    /// The `quota` policy, in bytes.
    quota: Option<f64>,
    /// The expiration, in milliseconds since the epoch.
    expires: Option<f64>,
}

#[derive(Default)]
struct Registry {
    next: u64,
    /// storage key (the origin) -> name -> bucket; a `BTreeMap` for `keys()`'s order
    by_key: HashMap<String, BTreeMap<String, Bucket>>,
}

fn registry() -> MutexGuard<'static, Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

fn expired(b: &Bucket) -> bool {
    b.expires.is_some_and(|e| e <= now_ms())
}

/// The namespace a bucket's databases and caches live under in this isolate's stores (`idb_factory::backend_key`,
/// `cache::cache_key`): its origin and name, so two origins' buckets of one name stay apart.
fn namespace(key: &str, name: &str) -> String {
    format!("{key}/{name}")
}

/// Bucket `name` of `key` is gone: its databases, caches and files go with it.
fn drop_data(scope: &mut v8::PinScope, key: &str, name: &str) {
    let ns = namespace(key, name);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        crate::modules::indexeddb::idb_factory::delete_bucket_databases(&mut st, &ns);
    }
    crate::modules::cache_storage::cache::delete_bucket_caches(scope, &ns);
    crate::core::fileapi::opfs::delete_bucket_files(key, name);
}

// ── the renderer's side ─────────────────────────────────────────────────────────────────────────

/// `IsValidName` (`storage_bucket_manager.cc:28-50`): 1-63 characters, each a lowercase ASCII letter, a digit, or
/// -- not first -- `_` or `-`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 64
        && name.chars().enumerate().all(|(i, c)| c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && (c == '_' || c == '-')))
}

/// The realm's storage key: its own `origin` (a document's or a worker's), "null" for an opaque one.
fn storage_key(scope: &mut v8::PinScope) -> String {
    crate::binding::protos::realm_self_get(scope, "origin")
        .filter(|o| o.is_string())
        .map(|o| o.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "null".to_string())
}

/// A `DOMString` argument, or the rejection its conversion is: a Symbol cannot become one.
fn dom_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, ctx: &str, rv: &mut v8::ReturnValue) -> Option<String> {
    if v.is_symbol() {
        let p = rejected_type_error(scope, &format!("{ctx}Cannot convert a Symbol value to a string"));
        rv.set(p);
        return None;
    }
    Some(v.to_rust_string_lossy(scope))
}

fn rejected_dom(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        let p = rejected_promise(scope, e);
        rv.set(p);
    }
}

/// The browser's answer, a task later (a mojo round trip).
fn answer(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: v8::Local<v8::Value>) {
    let p = settle_later(scope, v, false, Settle::Task(0.0));
    rv.set(p);
}

fn answer_type_error(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str) {
    let Some(m) = v8::String::new(scope, msg) else { return };
    let e = v8::Exception::type_error(scope, m);
    let p = settle_later(scope, e, true, Settle::Task(0.0));
    rv.set(p);
}

fn answer_unknown_error(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str) {
    let Some(e) = make_dom_exc(scope, msg, "UnknownError") else { return };
    let p = settle_later(scope, e, true, Settle::Task(0.0));
    rv.set(p);
}

/// SecurityError for an opaque origin (`SecurityOrigin::CanAccessStorageBuckets`), rejected at once.
fn denied(scope: &mut v8::PinScope, key: &str, rv: &mut v8::ReturnValue) -> bool {
    if key != "null" {
        return false;
    }
    rejected_dom(scope, rv, "Access to Storage Buckets API is denied in this context.", "SecurityError");
    true
}

// ── StorageBucketManager ────────────────────────────────────────────────────────────────────────

const OPEN_CTX: &str = "Failed to execute 'open' on 'StorageBucketManager': ";

/// A converted dictionary's numeric member, when present.
fn dict_number(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> Option<f64> {
    crate::binding::protos::get_prop(scope, o, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope))
}

/// The manager's receiver check: anything but `navigator.storageBuckets` is "Illegal invocation", a rejection.
fn manager_or_reject(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, member: &str) -> bool {
    if crate::binding::protos::shell_receiver_ok_named(scope, args.this().into(), "StorageBucketManager") {
        return true;
    }
    let p = rejected_type_error(scope, &format!("Failed to execute '{member}' on 'StorageBucketManager': Illegal invocation"));
    rv.set(p);
    false
}

/// `open(name, options)`.
fn cb_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !manager_or_reject(scope, &args, &mut rv, "open") {
        return;
    }
    let Some(name) = dom_string(scope, args.get(0), OPEN_CTX, &mut rv) else { return };
    let opts = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(1), "StorageBucketOptions", OPEN_CTX) {
        Ok(o) => o,
        Err(e) => {
            let p = rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let quota = dict_number(scope, opts, "quota");
    let expires = dict_number(scope, opts, "expires");
    let key = storage_key(scope);
    if denied(scope, &key, &mut rv) {
        return;
    }
    if !valid_name(&name) {
        let p = rejected_type_error(scope, &format!("The bucket name '{name}' is not a valid name."));
        rv.set(p);
        return;
    }
    if quota == Some(0.0) {
        let p = rejected_type_error(scope, "The bucket's quota cannot equal zero.");
        rv.set(p);
        return;
    }
    // the browser: an expiration not in the future is refused; an expired bucket is made again
    if expires.is_some_and(|e| e <= now_ms()) {
        return answer_type_error(scope, &mut rv, "The bucket expiration is invalid.");
    }
    let (id, recreated) = {
        let mut r = registry();
        let next = r.next + 1;
        let buckets = r.by_key.entry(key.clone()).or_default();
        let recreated = buckets.get(&name).is_some_and(expired);
        if recreated {
            buckets.remove(&name);
        }
        let b = buckets.entry(name.clone()).or_insert_with(|| Bucket { id: next, persisted: false, quota, expires: None });
        if expires.is_some() {
            b.expires = expires;
        }
        let id = b.id;
        if id == next {
            r.next = next;
        }
        (id, recreated)
    };
    if recreated {
        drop_data(scope, &key, &name);
    }
    let Some(o) = mint_bucket(scope, &key, &name, id) else { return };
    answer(scope, &mut rv, o.into());
}

/// `keys()`: the origin's bucket names, sorted, the expired ones deleted first.
fn cb_keys(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !manager_or_reject(scope, &args, &mut rv, "keys") {
        return;
    }
    let key = storage_key(scope);
    if denied(scope, &key, &mut rv) {
        return;
    }
    let (names, gone) = {
        let mut r = registry();
        let buckets = r.by_key.entry(key.clone()).or_default();
        let gone: Vec<String> = buckets.iter().filter(|(_, b)| expired(b)).map(|(n, _)| n.clone()).collect();
        for n in &gone {
            buckets.remove(n);
        }
        (buckets.keys().cloned().collect::<Vec<_>>(), gone)
    };
    for n in &gone {
        drop_data(scope, &key, n);
    }
    let items: Vec<v8::Local<v8::Value>> = names.iter().map(|n| str_val(scope, n)).collect();
    let arr = v8::Array::new_with_elements(scope, &items);
    answer(scope, &mut rv, arr.into());
}

/// `delete(name)`.
fn cb_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !manager_or_reject(scope, &args, &mut rv, "delete") {
        return;
    }
    let Some(name) = dom_string(scope, args.get(0), "Failed to execute 'delete' on 'StorageBucketManager': ", &mut rv) else { return };
    let key = storage_key(scope);
    if denied(scope, &key, &mut rv) {
        return;
    }
    if !valid_name(&name) {
        // `Delete` spells it without the quotes `open` has (`storage_bucket_manager.cc:179-181`)
        let p = rejected_type_error(scope, &format!("The bucket name {name} is not a valid name."));
        rv.set(p);
        return;
    }
    let existed = registry().by_key.get_mut(&key).and_then(|b| b.remove(&name)).is_some();
    if existed {
        drop_data(scope, &key, &name);
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    answer(scope, &mut rv, u);
}

// ── StorageBucket ───────────────────────────────────────────────────────────────────────────────

fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus.bucket.{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// A new `StorageBucket` object over bucket `id` (`name` of storage key `key`), on this realm's prototype.
fn mint_bucket<'s>(scope: &mut v8::PinScope<'s, '_>, key: &str, name: &str, id: u64) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "StorageBucket")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, "StorageBucket");
    for (k, v) in [("name", name), ("key", key)] {
        let s = slot(scope, k);
        let v = str_val(scope, v);
        o.set_private(scope, s, v);
    }
    let s = slot(scope, "id");
    let v: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    o.set_private(scope, s, v);
    Some(o)
}

/// What a bucket object is: its storage key, name and bucket id. `None` for any other receiver.
struct Held {
    key: String,
    name: String,
    id: u64,
}

fn held(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<Held> {
    if !crate::install::webidl::has_brand(scope, this.into(), "StorageBucket") {
        return None;
    }
    let name = slot_string(scope, this, "name")?;
    let key = slot_string(scope, this, "key")?;
    let s = slot(scope, "id");
    let id = this.get_private(scope, s).filter(|v| v.is_number()).and_then(|v| v.number_value(scope))? as u64;
    Some(Held { key, name, id })
}

fn slot_string(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, k: &str) -> Option<String> {
    let s = slot(scope, k);
    this.get_private(scope, s).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

/// The bucket a bucket object is for, while it exists (`BucketHost`'s `GetBucketById`). An EXPIRED bucket still
/// exists until `keys()` or `open()` purges it (`DidGetBucketsCheckExpiration`, `DidGetBucketCheckExpiration`):
/// Chrome 150 answers `estimate()`, its databases and caches after `setExpires(1000)`, and `keys()` then omits it
/// (measured).
fn live(h: &Held) -> Option<Bucket> {
    let r = registry();
    let b = r.by_key.get(&h.key).and_then(|m| m.get(&h.name)).filter(|b| b.id == h.id).cloned();
    b
}

/// The receiver of a promise member, or its rejection.
fn bucket_or_reject(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, member: &str) -> Option<Held> {
    let h = held(scope, args.this());
    if h.is_none() {
        let p = rejected_type_error(scope, &format!("Failed to execute '{member}' on 'StorageBucket': Illegal invocation"));
        rv.set(p);
    }
    h
}

fn cb_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = held(scope, args.this()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let v = str_val(scope, &h.name);
    rv.set(v);
}

/// `persist()`: the bucket is made persistent only with the persistent-storage permission, which no page here has
/// been granted -- false, as Chrome 150 answers a page that never asked (measured), unless it already is.
fn cb_persist(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "persist") else { return };
    let persisted = live(&h).is_some_and(|b| b.persisted);
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, persisted).into();
    answer(scope, &mut rv, v);
}

fn cb_persisted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "persisted") else { return };
    match live(&h) {
        Some(b) => {
            let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b.persisted).into();
            answer(scope, &mut rv, v);
        }
        None => answer_unknown_error(scope, &mut rv, "Unknown error occurred while getting persisted."),
    }
}

/// `estimate()`: `{quota, usage, usageDetails}` in the dictionary's order -- the bucket's own `quota` policy, else
/// the origin's (Chrome 150: the same as `navigator.storage.estimate().quota`, measured). Usage is not accounted
/// (see the module header).
fn cb_estimate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "estimate") else { return };
    let Some(b) = live(&h) else {
        return answer_unknown_error(scope, &mut rv, "Unknown error occurred while getting estimate.");
    };
    let origin_quota = super::navigator::client_hints(scope).storage_quota.map(|(q, _)| q).unwrap_or(0.0);
    let o = v8::Object::new(scope);
    let quota: v8::Local<v8::Value> = v8::Number::new(scope, b.quota.unwrap_or(origin_quota)).into();
    let usage: v8::Local<v8::Value> = v8::Number::new(scope, 0.0).into();
    let details: v8::Local<v8::Value> = v8::Object::new(scope).into();
    for (k, v) in [("quota", quota), ("usage", usage), ("usageDetails", details)] {
        let key = str_val(scope, k);
        o.set(scope, key, v);
    }
    answer(scope, &mut rv, o.into());
}

/// `setExpires(expires)`: a `DOMHighResTimeStamp`, a restricted double.
fn cb_set_expires(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "setExpires") else { return };
    let v = args.get(0);
    if v.is_symbol() {
        let p = rejected_type_error(scope, "Failed to execute 'setExpires' on 'StorageBucket': Cannot convert a Symbol value to a number");
        rv.set(p);
        return;
    }
    let Some(ms) = v.number_value(scope) else { return };
    if !ms.is_finite() {
        let p = rejected_type_error(scope, "Failed to execute 'setExpires' on 'StorageBucket': The provided double value is non-finite.");
        rv.set(p);
        return;
    }
    let set = {
        let mut r = registry();
        match r.by_key.get_mut(&h.key).and_then(|b| b.get_mut(&h.name)).filter(|b| b.id == h.id) {
            Some(b) => {
                b.expires = Some(ms);
                true
            }
            None => false,
        }
    };
    if !set {
        return answer_unknown_error(scope, &mut rv, "Unknown error occurred while setting expires.");
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    answer(scope, &mut rv, u);
}

/// `expires()`: the expiration, or null.
fn cb_expires(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "expires") else { return };
    let Some(b) = live(&h) else {
        return answer_unknown_error(scope, &mut rv, "Unknown error occurred while getting expires.");
    };
    let v: v8::Local<v8::Value> = match b.expires {
        Some(ms) => v8::Number::new(scope, ms).into(),
        None => v8::null(scope).into(),
    };
    answer(scope, &mut rv, v);
}

/// `[SameObject]` per bucket object: what slot `name` already holds, once the first read made it.
fn kept<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let s = slot(scope, name);
    this.get_private(scope, s).filter(|v| v.is_object())
}

fn keep(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let s = slot(scope, name);
    this.set_private(scope, s, v);
}

/// `indexedDB`: an `IDBFactory` over the bucket's own databases (`idb_factory::mint_bucket_factory`).
fn cb_indexed_db(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(h) = held(scope, this) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    if let Some(v) = kept(scope, this, "indexedDB") {
        rv.set(v);
        return;
    }
    let f = crate::modules::indexeddb::idb_factory::mint_bucket_factory(scope, &namespace(&h.key, &h.name));
    keep(scope, this, "indexedDB", f.into());
    rv.set(f.into());
}

/// `caches`: a `CacheStorage` over the bucket's own caches (`cache::mint_bucket_caches`).
fn cb_caches(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(h) = held(scope, this) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    if let Some(v) = kept(scope, this, "caches") {
        rv.set(v);
        return;
    }
    let Some(c) = crate::modules::cache_storage::cache::mint_bucket_caches(scope, &namespace(&h.key, &h.name)) else { return };
    keep(scope, this, "caches", c.into());
    rv.set(c.into());
}

/// `getDirectory()`: the root of the bucket's own file system (`opfs::bucket_directory`).
fn cb_get_directory(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(h) = bucket_or_reject(scope, &args, &mut rv, "getDirectory") else { return };
    crate::core::fileapi::opfs::bucket_directory(scope, &h.name, &mut rv);
}

fn getter(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, cb, &format!("get {}", member.name), 0);
    crate::install::kits::define_accessor(scope, target, member, g, None);
}

/// The kit (`tools/sync/gen_shells.py` claims every row of the two interfaces for it).
pub(crate) fn install(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.label {
        "StorageBucketManager::open" => method_row(scope, target, member, cb_open),
        "StorageBucketManager::keys" => method_row(scope, target, member, cb_keys),
        "StorageBucketManager::delete" => method_row(scope, target, member, cb_delete),
        "StorageBucket::name" => getter(scope, target, member, cb_name),
        "StorageBucket::indexedDB" => getter(scope, target, member, cb_indexed_db),
        "StorageBucket::caches" => getter(scope, target, member, cb_caches),
        "StorageBucket::estimate" => method_row(scope, target, member, cb_estimate),
        "StorageBucket::expires" => method_row(scope, target, member, cb_expires),
        "StorageBucket::getDirectory" => method_row(scope, target, member, cb_get_directory),
        "StorageBucket::persisted" => method_row(scope, target, member, cb_persisted),
        "StorageBucket::setExpires" => method_row(scope, target, member, cb_set_expires),
        "StorageBucket::persist" => method_row(scope, target, member, cb_persist),
        other => debug_assert!(false, "storage_buckets::install: unexpected member {other}"),
    }
}

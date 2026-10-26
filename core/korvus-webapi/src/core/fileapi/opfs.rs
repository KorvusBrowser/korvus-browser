//! `core/fileapi/opfs.rs` -- the origin private file system: `navigator.storage.getDirectory()`,
//! the handles, `FileSystemWritableFileStream` and the worker's `FileSystemSyncAccessHandle`, over the store in
//! [`super::opfs_store`].
//!
//! It was a stub whose handles resolved `undefined` from `getFile()`, `createWritable()` and
//! `createSyncAccessHandle()`. a challenge provider a challenge widget's challenge worker writes through a sync access handle and
//! reported "Cannot read properties of undefined (reading 'write')" -- a signal no browser sends.
//!
//! Every answer here is Chrome 150.0.7871.46's, measured page-side and worker-side (`tools/realweb/seed/opfs`,
//! and the probes behind it: `c6/repro/opfs/probe{,2,3}.html`), messages verbatim. What is not modelled:
//! - a `File` from `getFile()` keeps its bytes when the file changes later (Chrome's then fails to read with
//!   NotReadableError);
//! - `estimate()`'s usage does not count these files.

use std::cell::RefCell;
use std::collections::HashMap;

use deno_core::v8;

use super::opfs_store::{self as store, Fail, Lock};
use crate::binding::protos::{build_fn_data, get_prop, make_dom_exc, rejected_type_error, str_val};
use crate::install::live::{getter_row, method_row};
use crate::install::webidl::{brand, has_brand};
use crate::install::MemberDef;

// ── handles ──────────────────────────────────────────────────────────────────────────────────────

fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus.opfs.{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn slot_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let k = slot(scope, name);
    o.get_private(scope, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

fn set_slot_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: &str) {
    let k = slot(scope, name);
    let s = str_val(scope, v);
    o.set_private(scope, k, s);
}

/// What a handle addresses: the origin's store and the path from its root.
struct Target {
    origin: String,
    path: Vec<String>,
    kind: String,
}

fn split(path: &str) -> Vec<String> {
    if path.is_empty() { Vec::new() } else { path.split('/').map(str::to_string).collect() }
}

fn target_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Target> {
    if !has_brand(scope, v, "FileSystemHandle") {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    Some(Target { origin: slot_str(scope, o, "origin")?, path: split(&slot_str(scope, o, "path")?), kind: slot_str(scope, o, "kind")? })
}

fn set_target(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, t: &Target) {
    set_slot_str(scope, o, "origin", &t.origin);
    set_slot_str(scope, o, "path", &t.path.join("/"));
    set_slot_str(scope, o, "kind", &t.kind);
}

/// Stamp `iface` on a minted instance: the interface's init brand, and the template brand the generated rows'
/// receiver checks read (`binding::brand`). The template brand is the most derived interface's, which passes
/// for every interface above it -- a `FileSystemWritableFileStream` is minted as a `WritableStream`, and without
/// its own id its generated `write` row answered "Illegal invocation".
fn stamp(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, iface: &str) {
    brand(scope, o, iface);
    if let Some(id) = crate::binding::brand::id_of(iface) {
        crate::binding::brand::set_brand(scope, o, id);
    }
}

/// A handle of `t`'s kind over `t`, on this realm's prototype.
fn mint_handle<'s>(scope: &mut v8::PinScope<'s, '_>, t: &Target) -> Option<v8::Local<'s, v8::Object>> {
    let iface = if t.kind == "file" { "FileSystemFileHandle" } else { "FileSystemDirectoryHandle" };
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    brand(scope, o, "FileSystemHandle");
    stamp(scope, o, iface);
    set_target(scope, o, t);
    Some(o)
}

/// This realm's origin, the store's key: the global's own `origin` (a document's or a worker's) -- not its
/// `location`'s, which Chrome 150 answers "null" for every `about:srcdoc` frame, same-origin ones included,
/// while `self.origin` is the parent's (measured, `c7/repro/sandbox`). "null" is an opaque origin: a sandboxed
/// frame without `allow-same-origin`.
fn realm_origin(scope: &mut v8::PinScope) -> String {
    crate::binding::protos::realm_self_get(scope, "origin")
        .filter(|o| o.is_string())
        .map(|o| o.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "null".to_string())
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

// ── promises ─────────────────────────────────────────────────────────────────────────────────────

/// An operation's answer: a task, once the browser's storage service has answered ([`super::opfs_timing`]).
fn resolve(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: v8::Local<v8::Value>) {
    settle(scope, rv, v, false);
}

/// `v` settles the promise when the storage service answers the realm's key.
fn settle(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: v8::Local<v8::Value>, reject: bool) {
    let key = realm_origin(scope);
    let ms = super::opfs_timing::answer_ms(&key, super::opfs_timing::Op::Backend);
    let p = crate::binding::protos::settle_later(scope, v, reject, crate::binding::protos::Settle::Task(ms));
    rv.set(p);
}

/// [`resolve`] once the storage service's own disk work for the call (`done`) has ended too.
fn resolve_after(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: v8::Local<v8::Value>, done: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let key = realm_origin(scope);
    let ms = super::opfs_timing::answer_ms(&key, super::opfs_timing::Op::Backend);
    let p = crate::binding::protos::settle_later(scope, v, false, crate::binding::protos::Settle::TaskGated(ms, done));
    rv.set(p);
}

/// How long `navigator.storage.estimate()` waits for the storage service in this realm ([`super::opfs_timing`]).
pub(crate) fn estimate_ms(scope: &mut v8::PinScope) -> f64 {
    let key = realm_origin(scope);
    super::opfs_timing::answer_ms(&key, super::opfs_timing::Op::Estimate)
}

fn resolve_undefined(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue) {
    let u = v8::undefined(scope).into();
    resolve(scope, rv, u);
}

fn reject_value(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, e: v8::Local<v8::Value>) {
    let Some(r) = v8::PromiseResolver::new(scope) else { return };
    r.reject(scope, e);
    rv.set(r.get_promise(scope).into());
}

fn reject_dom(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        reject_value(scope, rv, e);
    }
}

fn reject_type(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str) {
    rv.set(rejected_type_error(scope, msg));
}

/// A store failure as Chrome words it for operation `op` of `iface`: a lock names the operation, the others
/// are the bare sentence. The storage service found it, so it arrives when the service answers (Chrome 150:
/// a missing file's `NotFoundError` 0.1 ms after the call, 3.5-5.2 ms as a key's first operation).
fn reject_fail(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, f: Fail, op: &str, iface: &str) {
    let msg = match f {
        Fail::Locked => format!("Failed to execute '{op}' on '{iface}': {}", f.message()),
        _ => f.message().to_string(),
    };
    if let Some(e) = make_dom_exc(scope, &msg, f.name()) {
        settle(scope, rv, e, true);
    }
}

fn illegal(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue) {
    reject_type(scope, rv, "Illegal invocation");
}

/// A dictionary member of `opts` (`undefined` when `opts` is not an object or lacks it).
fn member<'s>(scope: &mut v8::PinScope<'s, '_>, opts: v8::Local<'s, v8::Value>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let o = v8::Local::<v8::Object>::try_from(opts).ok()?;
    get_prop(scope, o, key).filter(|v| !v.is_undefined())
}

fn member_bool<'s>(scope: &mut v8::PinScope<'s, '_>, opts: v8::Local<'s, v8::Value>, key: &str) -> bool {
    member(scope, opts, key).is_some_and(|v| v.boolean_value(scope))
}

/// An enum dictionary member: `Ok(None)` when absent, `Err(message)` for a value outside `allowed`.
fn member_enum<'s>(scope: &mut v8::PinScope<'s, '_>, opts: v8::Local<'s, v8::Value>, key: &str, allowed: &[&str], dict: &str, ty: &str) -> Result<Option<String>, String> {
    let Some(v) = member(scope, opts, key) else { return Ok(None) };
    let s = v.to_rust_string_lossy(scope);
    if allowed.contains(&s.as_str()) {
        Ok(Some(s))
    } else {
        Err(format!("Failed to read the '{key}' property from '{dict}': The provided value '{s}' is not a valid enum value of type {ty}."))
    }
}

// ── StorageManager.getDirectory ──────────────────────────────────────────────────────────────────

fn cb_get_directory(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let origin = realm_origin(scope);
    if origin == "null" {
        reject_dom(scope, &mut rv, "Storage directory access is denied because the context is sandboxed and lacks the 'allow-same-origin' flag.", "SecurityError");
        return;
    }
    let t = Target { origin, path: Vec::new(), kind: "directory".to_string() };
    let ms = super::opfs_timing::answer_ms(&t.origin, super::opfs_timing::Op::OpenRoot);
    // and its disk work, as the browser's storage service does it when it opens the bucket -- the answer waits for it
    let opened = store::open_bucket_in_background(&t.origin);
    let Some(h) = mint_handle(scope, &t) else { return };
    // the browser opens the key's bucket: its first time a few ms, then one answer ([`super::opfs_timing`])
    let p = crate::binding::protos::settle_later(scope, h.into(), false, crate::binding::protos::Settle::TaskGated(ms, opened));
    rv.set(p);
}

/// The store key of storage bucket `bucket`'s file system in `origin`.
fn bucket_key(origin: &str, bucket: &str) -> String {
    format!("{origin}\u{0}{bucket}")
}

/// `StorageBucket.getDirectory()`: the root of storage bucket `bucket`'s OWN file system -- another tree than
/// `navigator.storage.getDirectory()`'s (Chrome 150, measured: `isSameEntry` with it is false, `kind` "directory",
/// `name` "") -- answered as the origin's root is ([`cb_get_directory`]).
pub(crate) fn bucket_directory(scope: &mut v8::PinScope, bucket: &str, rv: &mut v8::ReturnValue) {
    let origin = realm_origin(scope);
    if origin == "null" {
        reject_dom(scope, rv, "Storage directory access is denied because the context is sandboxed and lacks the 'allow-same-origin' flag.", "SecurityError");
        return;
    }
    let t = Target { origin: bucket_key(&origin, bucket), path: Vec::new(), kind: "directory".to_string() };
    let ms = super::opfs_timing::answer_ms(&t.origin, super::opfs_timing::Op::OpenRoot);
    let opened = store::open_bucket_in_background(&t.origin);
    let Some(h) = mint_handle(scope, &t) else { return };
    let p = crate::binding::protos::settle_later(scope, h.into(), false, crate::binding::protos::Settle::TaskGated(ms, opened));
    rv.set(p);
}

/// Storage bucket `bucket` of `origin` deleted: its file system goes with it.
pub(crate) fn delete_bucket_files(origin: &str, bucket: &str) {
    store::with_root(&bucket_key(origin, bucket), |root| root.clear());
}

// ── FileSystemHandle ─────────────────────────────────────────────────────────────────────────────

fn cb_kind<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = target_of(scope, args.this().into()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    rv.set(str_val(scope, &t.kind));
}

fn cb_name<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = target_of(scope, args.this().into()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    rv.set(str_val(scope, t.path.last().map(String::as_str).unwrap_or("")));
}

fn cb_is_same_entry<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(a) = target_of(scope, args.this().into()) else { return illegal(scope, &mut rv) };
    let Some(b) = target_of(scope, args.get(0)) else {
        return reject_type(scope, &mut rv, "Failed to execute 'isSameEntry' on 'FileSystemHandle': parameter 1 is not of type 'FileSystemHandle'.");
    };
    let same = a.origin == b.origin && a.kind == b.kind && a.path == b.path;
    let v = v8::Boolean::new(scope, same).into();
    resolve(scope, &mut rv, v);
}

/// `queryPermission()` / `requestPermission()`: an origin's own files are always granted.
fn cb_permission<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if target_of(scope, args.this().into()).is_none() {
        return illegal(scope, &mut rv);
    }
    let g = str_val(scope, "granted");
    resolve(scope, &mut rv, g);
}

fn cb_remove<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = target_of(scope, args.this().into()) else { return illegal(scope, &mut rv) };
    let recursive = member_bool(scope, args.get(0), "recursive");
    let Some((name, parent)) = t.path.split_last() else {
        return reject_fail(scope, &mut rv, Fail::NotFound, "remove", "FileSystemHandle");
    };
    let r = store::check(&t.origin, &t.path, &t.kind).and_then(|_| store::remove_child(&t.origin, parent, name, recursive));
    match r {
        Ok(()) => resolve_undefined(scope, &mut rv),
        Err(f) => reject_fail(scope, &mut rv, f, "remove", "FileSystemHandle"),
    }
}

// ── FileSystemDirectoryHandle ───────────────────────────────────────────────────────────────────

fn dir_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<Target> {
    target_of(scope, args.this().into()).filter(|t| t.kind == "directory")
}

/// `getFileHandle(name, { create })` / `getDirectoryHandle(name, { create })`.
fn get_child<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, kind: &str, op: &str) {
    let Some(dir) = dir_of(scope, args) else { return illegal(scope, rv) };
    let name = args.get(0).to_rust_string_lossy(scope);
    if !store::name_allowed(&name) {
        return reject_type(scope, rv, &format!("Failed to execute '{op}' on 'FileSystemDirectoryHandle': Name is not allowed."));
    }
    let create = member_bool(scope, args.get(1), "create");
    match store::get_child(&dir.origin, &dir.path, &name, kind, create, now_ms()) {
        Ok(()) => {
            let mut path = dir.path.clone();
            path.push(name);
            // a file made here is on disk, its creation committed, when the browser answers
            let made = (create && kind == "file").then(|| store::create_in_background(&dir.origin, &path));
            let Some(h) = mint_handle(scope, &Target { origin: dir.origin, path, kind: kind.to_string() }) else { return };
            match made {
                Some(done) => resolve_after(scope, rv, h.into(), done),
                None => resolve(scope, rv, h.into()),
            }
        }
        Err(f) => reject_fail(scope, rv, f, op, "FileSystemDirectoryHandle"),
    }
}

fn cb_get_file_handle<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    get_child(scope, &args, &mut rv, "file", "getFileHandle");
}

fn cb_get_directory_handle<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    get_child(scope, &args, &mut rv, "directory", "getDirectoryHandle");
}

fn cb_remove_entry<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(dir) = dir_of(scope, &args) else { return illegal(scope, &mut rv) };
    let name = args.get(0).to_rust_string_lossy(scope);
    if !store::name_allowed(&name) {
        return reject_type(scope, &mut rv, "Failed to execute 'removeEntry' on 'FileSystemDirectoryHandle': Name is not allowed.");
    }
    let recursive = member_bool(scope, args.get(1), "recursive");
    match store::remove_child(&dir.origin, &dir.path, &name, recursive) {
        Ok(()) => resolve_undefined(scope, &mut rv),
        Err(f) => reject_fail(scope, &mut rv, f, "removeEntry", "FileSystemDirectoryHandle"),
    }
}

/// `resolve(possibleDescendant)`: the names from this directory down to it, `[]` for itself, null for a
/// handle outside it.
fn cb_resolve<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(dir) = dir_of(scope, &args) else { return illegal(scope, &mut rv) };
    let Some(t) = target_of(scope, args.get(0)) else {
        return reject_type(scope, &mut rv, "Failed to execute 'resolve' on 'FileSystemDirectoryHandle': parameter 1 is not of type 'FileSystemHandle'.");
    };
    if t.origin != dir.origin || t.path.len() < dir.path.len() || t.path[..dir.path.len()] != dir.path[..] {
        let n = v8::null(scope).into();
        return resolve(scope, &mut rv, n);
    }
    let names: Vec<v8::Local<v8::Value>> = t.path[dir.path.len()..].iter().map(|s| str_val(scope, s)).collect();
    let a = v8::Array::new_with_elements(scope, &names).into();
    resolve(scope, &mut rv, a);
}

// The async iterator of `entries()` / `keys()` / `values()` / `@@asyncIterator`: the listing is taken when
// the iterator is made (Chrome 150: an entry created before the first `next()` is not seen), and a directory
// that is gone answers NotFoundError from `next()`.

const ITER_PROTO: &str = "korvus_opfs_dir_iter_proto";

struct DirIter {
    origin: String,
    dir: Vec<String>,
    /// `Err` when the directory was gone at creation.
    list: Result<Vec<(String, &'static str)>, Fail>,
    at: usize,
    /// 0 keys, 1 values, 2 entries.
    what: u8,
    /// The listing has arrived: Chrome 150 fetches a directory's entries in one answer and the later `next()`s
    /// settle from it (three entries and the end: 0.1-0.2 ms in all; korvus took a task per step, 3 ms).
    fetched: bool,
}

thread_local! {
    static ITERS: RefCell<HashMap<u32, DirIter>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

fn next_id() -> u32 {
    NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1).max(1));
        id
    })
}

fn iter_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(p) = crate::binding::protos::realm_slot(scope, ITER_PROTO).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(p);
    }
    let aip = crate::core::streams::readable_stream::async_iterator_prototype(scope)?;
    let proto = crate::binding::protos::mint_on(scope, aip.into());
    let next = crate::binding::protos::template_fn(scope, cb_iter_next, "next", 0, None);
    let nk = v8::String::new(scope, "next")?;
    let mut d = v8::PropertyDescriptor::new_from_value_writable(next.into(), true);
    d.set_enumerable(true);
    d.set_configurable(true);
    proto.define_property(scope, nk.into(), &d);
    let tag: v8::Local<v8::Value> = v8::String::new(scope, "FileSystemDirectoryHandle AsyncIterator")?.into();
    let mut td = v8::PropertyDescriptor::new_from_value_writable(tag, false);
    td.set_enumerable(false);
    td.set_configurable(true);
    let tk = v8::Symbol::get_to_string_tag(scope);
    proto.define_property(scope, tk.into(), &td);
    crate::binding::protos::set_realm_slot(scope, ITER_PROTO, proto.into());
    Some(proto)
}

fn make_iter(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, what: u8) {
    let Some(dir) = dir_of(scope, args) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let Some(proto) = iter_proto(scope) else { return };
    let it = crate::binding::protos::mint_on(scope, proto.into());
    let id = next_id();
    let list = store::list(&dir.origin, &dir.path);
    ITERS.with(|m| m.borrow_mut().insert(id, DirIter { origin: dir.origin, dir: dir.path, list, at: 0, what, fetched: false }));
    let k = slot(scope, "iter");
    let v = v8::Integer::new_from_unsigned(scope, id).into();
    it.set_private(scope, k, v);
    rv.set(it.into());
}

fn cb_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    make_iter(scope, &args, &mut rv, 2);
}
fn cb_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    make_iter(scope, &args, &mut rv, 0);
}
fn cb_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    make_iter(scope, &args, &mut rv, 1);
}

fn iter_result<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'s, v8::Value>, done: bool) -> v8::Local<'s, v8::Value> {
    let o = v8::Object::new(scope);
    let vk = str_val(scope, "value");
    o.set(scope, vk, value);
    let dk = str_val(scope, "done");
    let d = v8::Boolean::new(scope, done).into();
    o.set(scope, dk, d);
    o.into()
}

fn cb_iter_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let k = slot(scope, "iter");
    let Some(id) = args.this().get_private(scope, k).and_then(|v| v.uint32_value(scope)).filter(|&i| i > 0) else {
        return reject_type(scope, &mut rv, "Failed to execute 'next' on 'AsyncIterator_FileSystemDirectoryHandle': Illegal invocation");
    };
    let fetched = ITERS.with(|m| m.borrow_mut().get_mut(&id).map(|it| std::mem::replace(&mut it.fetched, true))).unwrap_or(true);
    let step = ITERS.with(|m| {
        let mut m = m.borrow_mut();
        let it = m.get_mut(&id)?;
        Some(match &it.list {
            Err(f) => Err(*f),
            Ok(list) => {
                let item = list.get(it.at).cloned();
                it.at += 1;
                Ok((item, it.origin.clone(), it.dir.clone(), it.what))
            }
        })
    });
    match step {
        None => {
            let u = v8::undefined(scope).into();
            let r = iter_result(scope, u, true);
            iter_settle(scope, &mut rv, r, fetched);
        }
        Some(Err(f)) => {
            ITERS.with(|m| m.borrow_mut().remove(&id));
            reject_fail(scope, &mut rv, f, "next", "AsyncIterator_FileSystemDirectoryHandle");
        }
        Some(Ok((None, ..))) => {
            ITERS.with(|m| m.borrow_mut().remove(&id));
            let u = v8::undefined(scope).into();
            let r = iter_result(scope, u, true);
            iter_settle(scope, &mut rv, r, fetched);
        }
        Some(Ok((Some((name, kind)), origin, dir, what))) => {
            let key = str_val(scope, &name);
            let value = if what == 0 {
                key
            } else {
                let mut path = dir;
                path.push(name);
                let Some(h) = mint_handle(scope, &Target { origin, path, kind: kind.to_string() }) else { return };
                if what == 1 {
                    h.into()
                } else {
                    v8::Array::new_with_elements(scope, &[key, h.into()]).into()
                }
            };
            let r = iter_result(scope, value, false);
            iter_settle(scope, &mut rv, r, fetched);
        }
    }
}

/// A `next()` result: when the storage service answers the first, from the fetched listing after that.
fn iter_settle(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, r: v8::Local<v8::Value>, fetched: bool) {
    if fetched {
        let p = crate::binding::protos::settle_later(scope, r, false, crate::binding::protos::Settle::Microtask);
        rv.set(p);
    } else {
        resolve(scope, rv, r);
    }
}

// ── FileSystemFileHandle ────────────────────────────────────────────────────────────────────────

fn file_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<Target> {
    target_of(scope, args.this().into()).filter(|t| t.kind == "file")
}

/// `getFile().type`: the file's extension, the way Chrome 150 maps it (ASCII case-insensitive; measured).
fn mime_of(name: &str) -> &'static str {
    let Some((_, ext)) = name.rsplit_once('.') else { return "" };
    match ext.to_ascii_lowercase().as_str() {
        "txt" | "text" => "text/plain",
        "html" | "htm" | "shtml" | "shtm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "text/xml",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" | "pjpeg" | "pjp" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" | "svgz" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "bin" | "exe" | "com" => "application/octet-stream",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "",
    }
}

fn cb_get_file<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = file_of(scope, &args) else { return illegal(scope, &mut rv) };
    match store::with_file(&t.origin, &t.path, |fd| (fd.bytes.clone(), fd.modified)) {
        Ok((bytes, modified)) => {
            let name = t.path.last().cloned().unwrap_or_default();
            let Some(f) = super::file::mint_file(scope, bytes, mime_of(&name), &name, modified) else { return };
            resolve(scope, &mut rv, f.into());
        }
        Err(f) => reject_fail(scope, &mut rv, f, "getFile", "FileSystemFileHandle"),
    }
}

// ── FileSystemWritableFileStream ────────────────────────────────────────────────────────────────

/// An open writable: its swap copy (Chrome's `<name>.crswap`, which a directory listing shows while it is
/// open), the cursor, and what it is committed to.
struct Writable {
    origin: String,
    path: Vec<String>,
    /// The swap entry's name in the file's directory.
    swap: String,
    buf: Vec<u8>,
    cursor: usize,
    exclusive: bool,
}

thread_local! {
    static WRITABLES: RefCell<HashMap<u32, Writable>> = RefCell::new(HashMap::new());
    static ACCESS: RefCell<HashMap<u32, Access>> = RefCell::new(HashMap::new());
}

fn cb_create_writable<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = file_of(scope, &args) else { return illegal(scope, &mut rv) };
    let opts = args.get(0);
    let keep = member_bool(scope, opts, "keepExistingData");
    let mode = match member_enum(scope, opts, "mode", &["siloed", "exclusive"], "FileSystemCreateWritableOptions", "FileSystemWritableFileStreamMode") {
        Ok(m) => m.unwrap_or_else(|| "siloed".to_string()),
        Err(msg) => return reject_type(scope, &mut rv, &format!("Failed to execute 'createWritable' on 'FileSystemFileHandle': {msg}")),
    };
    let exclusive = mode == "exclusive";
    if let Err(f) = store::lock(&t.origin, &t.path, if exclusive { Lock::Exclusive } else { Lock::Writable(1) }) {
        return reject_fail(scope, &mut rv, f, "createWritable", "FileSystemFileHandle");
    }
    let buf = if keep { store::with_file(&t.origin, &t.path, |fd| fd.bytes.clone()).unwrap_or_default() } else { Vec::new() };
    let swap = store::open_swap(&t.origin, &t.path);
    let id = next_id();
    WRITABLES.with(|m| m.borrow_mut().insert(id, Writable { origin: t.origin, path: t.path, swap, buf, cursor: 0, exclusive }));
    let Some(ws) = mint_writable(scope, id) else { return };
    resolve(scope, &mut rv, ws.into());
}

/// The stream: a `WritableStream` on `FileSystemWritableFileStream.prototype` whose sink is this writable.
fn mint_writable<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    let obj = crate::core::streams::writable_stream::mint_stream(scope);
    let proto = crate::binding::protos::shell_proto(scope, "FileSystemWritableFileStream")?;
    obj.set_prototype(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, obj);
    stamp(scope, obj, "FileSystemWritableFileStream");
    let k = slot(scope, "writable");
    let v: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, id).into();
    obj.set_private(scope, k, v);
    let sink = v8::Object::new(scope);
    let w = build_fn_data(scope, cb_sink_write, "write", 1, v);
    let c = build_fn_data(scope, cb_sink_close, "close", 0, v);
    let a = build_fn_data(scope, cb_sink_abort, "abort", 1, v);
    for (name, f) in [("write", w), ("close", c), ("abort", a)] {
        let key = str_val(scope, name);
        sink.set(scope, key, f.into());
    }
    crate::core::streams::writable_stream::ws_init(scope, obj.into(), sink.into());
    Some(obj)
}

/// One write command, converted (WebIDL's `FileSystemWriteChunkType`).
enum Command {
    /// `data`: `None` absent, `Some(None)` null.
    Write { data: Option<Option<Vec<u8>>>, position: Option<u64> },
    Seek(Option<u64>),
    Truncate(Option<u64>),
}

/// Convert a chunk. `Err` is a TypeError message (without the operation prefix): a CONVERSION failure, which
/// through `FileSystemWritableFileStream.write` rejects without touching the stream.
fn convert(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<Command, String> {
    if v.is_null_or_undefined() {
        return Err("The provided value is not of type 'WriteParams'.".to_string());
    }
    if let Some(b) = bytes_of(scope, v) {
        return Ok(Command::Write { data: Some(Some(b)), position: None });
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        let s = v.to_string(scope).ok_or_else(|| "Cannot convert a Symbol value to a string".to_string())?;
        return Ok(Command::Write { data: Some(Some(s.to_rust_string_lossy(scope).into_bytes())), position: None });
    };
    let ty = get_prop(scope, o, "type").filter(|t| !t.is_undefined());
    let Some(ty) = ty else {
        return Err("Failed to read the 'type' property from 'WriteParams': Required member is undefined.".to_string());
    };
    let ty = ty.to_rust_string_lossy(scope);
    let num = |scope: &mut v8::PinScope, key: &str| -> Option<u64> {
        let x = get_prop(scope, o, key).filter(|x| !x.is_null_or_undefined())?;
        x.number_value(scope).map(|n| if n.is_finite() && n > 0.0 { n.trunc() as u64 } else { 0 })
    };
    match ty.as_str() {
        "write" => {
            let position = num(scope, "position");
            let data = get_prop(scope, o, "data").filter(|d| !d.is_undefined()).map(|d| {
                (!d.is_null()).then(|| bytes_of(scope, d).unwrap_or_else(|| d.to_rust_string_lossy(scope).into_bytes()))
            });
            Ok(Command::Write { data, position })
        }
        "seek" => Ok(Command::Seek(num(scope, "position"))),
        "truncate" => Ok(Command::Truncate(num(scope, "size"))),
        other => Err(format!("Failed to read the 'type' property from 'WriteParams': The provided value '{other}' is not a valid enum value of type WriteCommandType.")),
    }
}

/// The bytes of a BufferSource or a Blob.
fn bytes_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let mut out = vec![0u8; len];
        if let Some(p) = ab.data() {
            out.copy_from_slice(unsafe { std::slice::from_raw_parts(p.as_ptr() as *const u8, len) });
        }
        return Some(out);
    }
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut out = vec![0u8; view.byte_length()];
        view.copy_contents(&mut out);
        return Some(out);
    }
    super::blob::blob_bytes(scope, v)
}

/// Apply one command to writable `id`. `Err` is `(name, message)` of the error a semantic failure ERRORS the
/// stream with (Chrome 150: a missing argument is a SyntaxError, a null `data` a TypeError).
fn apply(id: u32, cmd: Command) -> Result<(), (&'static str, &'static str)> {
    WRITABLES.with(|m| {
        let mut m = m.borrow_mut();
        let Some(w) = m.get_mut(&id) else { return Ok(()) };
        match cmd {
            Command::Write { data: None, .. } => Err(("SyntaxError", "Invalid params passed. write requires a data argument")),
            Command::Write { data: Some(None), .. } => Err(("TypeError", "Invalid params passed. write requires a non-null data")),
            Command::Write { data: Some(Some(d)), position } => {
                if let Some(p) = position {
                    w.cursor = p as usize;
                }
                let at = w.cursor;
                w.cursor += store::write_at(&mut w.buf, at, &d);
                Ok(())
            }
            Command::Seek(None) => Err(("SyntaxError", "Invalid params passed. seek requires a position argument")),
            Command::Seek(Some(p)) => {
                w.cursor = p as usize;
                Ok(())
            }
            Command::Truncate(None) => Err(("SyntaxError", "Invalid params passed. truncate requires a size argument")),
            Command::Truncate(Some(n)) => {
                w.buf.resize(n as usize, 0);
                w.cursor = w.cursor.min(n as usize);
                Ok(())
            }
        }
    })
}

fn writable_id(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<u32> {
    let k = slot(scope, "writable");
    o.get_private(scope, k).and_then(|v| v.uint32_value(scope)).filter(|&i| i > 0)
}

/// `write` / `seek` / `truncate` on the stream: convert, then (unless a writer holds the stream) write through
/// its sink, so a closed or errored stream answers as one.
fn stream_command(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, op: &str, cmd: Result<Command, String>) {
    let this = args.this();
    let Some(id) = writable_id(scope, this) else { return illegal(scope, rv) };
    let cmd = match cmd {
        Ok(c) => c,
        Err(msg) => return reject_type(scope, rv, &format!("Failed to execute '{op}' on 'FileSystemWritableFileStream': {msg}")),
    };
    let Some(ws) = crate::core::streams::writable_stream::as_writable(scope, this.into()) else { return illegal(scope, rv) };
    if ws.locked.get() {
        return reject_type(scope, rv, &format!("Failed to execute '{op}' on 'FileSystemWritableFileStream': Cannot create writer when WritableStream is locked"));
    }
    if let Some(e) = ws.errored.borrow().clone() {
        let e = v8::Local::new(scope, &e);
        return reject_value(scope, rv, e);
    }
    if ws.closed.get() {
        return reject_type(scope, rv, "Cannot write to a CLOSED writable stream");
    }
    match apply(id, cmd) {
        Ok(()) => resolve_undefined(scope, rv),
        Err((name, msg)) => {
            let full = format!("Failed to execute '{op}' on 'FileSystemWritableFileStream': {msg}");
            let e = if name == "TypeError" {
                // Blink's context applied after the error is made
                deno_core::error::native_error(scope, "TypeError", &full)
            } else {
                let Some(e) = make_dom_exc(scope, &full, name) else { return };
                e
            };
            crate::core::streams::writable_stream::ws_error(scope, ws, e);
            reject_value(scope, rv, e);
        }
    }
}

fn cb_ws_write<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let cmd = convert(scope, args.get(0));
    stream_command(scope, &args, &mut rv, "write", cmd);
}

fn cb_ws_seek<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let p = args.get(0).number_value(scope).map(|n| if n.is_finite() && n > 0.0 { n.trunc() as u64 } else { 0 });
    stream_command(scope, &args, &mut rv, "seek", Ok(Command::Seek(p)));
}

fn cb_ws_truncate<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let n = args.get(0).number_value(scope).map(|n| if n.is_finite() && n > 0.0 { n.trunc() as u64 } else { 0 });
    stream_command(scope, &args, &mut rv, "truncate", Ok(Command::Truncate(n)));
}

fn cb_ws_mode<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = writable_id(scope, args.this()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let ex = WRITABLES.with(|m| m.borrow().get(&id).map(|w| w.exclusive)).unwrap_or(false);
    rv.set(str_val(scope, if ex { "exclusive" } else { "siloed" }));
}

fn data_id(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> u32 {
    crate::binding::protos::body_data(scope, args).uint32_value(scope).unwrap_or(0)
}

/// The sink's `write(chunk)` -- what `getWriter().write(chunk)` reaches. Here a conversion failure throws
/// too, and a sink that throws errors the stream (Chrome 150: the writer's second write rejects with the
/// first's error).
fn cb_sink_write<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let id = data_id(scope, &args);
    match convert(scope, args.get(0)) {
        Err(msg) => crate::binding::protos::throw_type_error(scope, &format!("Failed to execute 'write' on 'WritableStreamDefaultWriter': {msg}")),
        Ok(cmd) => {
            if let Err((name, msg)) = apply(id, cmd) {
                let full = format!("Failed to execute 'write' on 'WritableStreamDefaultWriter': {msg}");
                if name == "TypeError" {
                    crate::binding::protos::throw_type_error(scope, &full);
                } else {
                    crate::binding::protos::throw_dom(scope, &full, name);
                }
            }
        }
    }
}

/// The sink's `close()`: the swap copy becomes the file's content.
fn cb_sink_close<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let id = data_id(scope, &args);
    if let Some(w) = WRITABLES.with(|m| m.borrow_mut().remove(&id)) {
        let _ = store::with_file(&w.origin, &w.path, |fd| {
            fd.bytes = w.buf;
            fd.modified = now_ms();
        });
        store::close_swap(&w.origin, &w.path, &w.swap);
        store::unlock(&w.origin, &w.path);
    }
}

/// The sink's `abort(reason)`: the swap copy is dropped.
fn cb_sink_abort<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let id = data_id(scope, &args);
    if let Some(w) = WRITABLES.with(|m| m.borrow_mut().remove(&id)) {
        store::close_swap(&w.origin, &w.path, &w.swap);
        store::unlock(&w.origin, &w.path);
    }
}

// ── FileSystemSyncAccessHandle (workers) ────────────────────────────────────────────────────────

struct Access {
    origin: String,
    path: Vec<String>,
    cursor: usize,
    mode: &'static str,
    closed: bool,
}

fn cb_create_sync_access_handle<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = file_of(scope, &args) else { return illegal(scope, &mut rv) };
    let mode = match member_enum(scope, args.get(0), "mode", &["readwrite", "read-only", "readwrite-unsafe"], "FileSystemCreateSyncAccessHandleOptions", "FileSystemSyncAccessHandleMode") {
        Ok(m) => m.unwrap_or_else(|| "readwrite".to_string()),
        Err(msg) => return reject_type(scope, &mut rv, &format!("Failed to execute 'createSyncAccessHandle' on 'FileSystemFileHandle': {msg}")),
    };
    let (mode, lock): (&'static str, Lock) = match mode.as_str() {
        "read-only" => ("read-only", Lock::ReadOnly(1)),
        "readwrite-unsafe" => ("readwrite-unsafe", Lock::Unsafe(1)),
        _ => ("readwrite", Lock::Exclusive),
    };
    if let Err(f) = store::lock(&t.origin, &t.path, lock) {
        let msg = match f {
            Fail::Locked => "Failed to execute 'createSyncAccessHandle' on 'FileSystemFileHandle': Access Handles cannot be created if there is another open Access Handle or Writable stream associated with the same file.".to_string(),
            _ => f.message().to_string(),
        };
        return reject_dom(scope, &mut rv, &msg, f.name());
    }
    // The file is ON DISK once a handle is open on it, as Chrome's is from creation: a later `flush()`
    // syncs data into an existing file instead of also committing the file's creation. The browser does
    // that, not the worker: it is written off this thread (Chrome 150 answers in 0.2-0.4 ms; korvus's
    // synchronous create + fsync here took 2.3-10.9 ms), and a `flush()` waits for it.
    store::persist_in_background(&t.origin, &t.path);
    let Some(proto) = crate::binding::protos::shell_proto(scope, "FileSystemSyncAccessHandle") else { return };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    stamp(scope, o, "FileSystemSyncAccessHandle");
    let id = next_id();
    ACCESS.with(|m| m.borrow_mut().insert(id, Access { origin: t.origin, path: t.path, cursor: 0, mode, closed: false }));
    let k = slot(scope, "access");
    let v = v8::Integer::new_from_unsigned(scope, id).into();
    o.set_private(scope, k, v);
    resolve(scope, &mut rv, o.into());
}

fn access_id(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let k = slot(scope, "access");
    args.this().get_private(scope, k).and_then(|v| v.uint32_value(scope)).filter(|&i| i > 0)
}

fn ah_throw(scope: &mut v8::PinScope, op: &str, msg: &str) {
    crate::binding::protos::throw_type_error(scope, &format!("Failed to execute '{op}' on 'FileSystemSyncAccessHandle': {msg}"));
}

/// The handle, open -- else the InvalidStateError Chrome throws (`read`/`write` say the access handle, the
/// others the file).
fn open_access(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, op: &str) -> Option<u32> {
    let Some(id) = access_id(scope, args) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return None;
    };
    let closed = ACCESS.with(|m| m.borrow().get(&id).is_none_or(|a| a.closed));
    if closed {
        let what = if op == "read" || op == "write" { "The access handle was already closed" } else { "The file was already closed" };
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{op}' on 'FileSystemSyncAccessHandle': {what}"), "InvalidStateError");
        return None;
    }
    Some(id)
}

/// `{ at }` ([EnforceRange] unsigned long long): `Err` after throwing.
fn at_option<'s>(scope: &mut v8::PinScope<'s, '_>, opts: v8::Local<'s, v8::Value>, op: &str) -> Result<Option<usize>, ()> {
    let Some(v) = member(scope, opts, "at") else { return Ok(None) };
    let n = v.number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() || n < 0.0 || n > 9007199254740991.0 {
        ah_throw(scope, op, "Failed to read the 'at' property from 'FileSystemReadWriteOptions': Value is outside the 'unsigned long long' value range.");
        return Err(());
    }
    Ok(Some(n.trunc() as usize))
}

/// A BufferSource's backing store, its offset and its length.
fn view_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::ArrayBuffer>, usize, usize)> {
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        return Some((ab, 0, len));
    }
    let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
    let (off, len) = (view.byte_offset(), view.byte_length());
    Some((view.buffer(scope)?, off, len))
}

fn cb_ah_read<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = open_access(scope, &args, "read") else { return };
    if args.length() < 1 {
        return ah_throw(scope, "read", "1 argument required, but only 0 present.");
    }
    let Some((ab, off, len)) = view_of(scope, args.get(0)) else {
        return ah_throw(scope, "read", "The provided value is not of type '([AllowShared] ArrayBuffer or [AllowShared] ArrayBufferView)'.");
    };
    let Ok(at) = at_option(scope, args.get(1), "read") else { return };
    let n = ACCESS.with(|m| {
        let mut m = m.borrow_mut();
        let a = m.get_mut(&id)?;
        let start = at.unwrap_or(a.cursor);
        let mut tmp = vec![0u8; len];
        let n = store::with_file(&a.origin, &a.path, |fd| store::read_at(&fd.bytes, start, &mut tmp)).unwrap_or(0);
        a.cursor = start + n;
        Some((tmp, n))
    });
    let Some((tmp, n)) = n else { return };
    if n > 0 {
        if let Some(p) = ab.data() {
            unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), (p.as_ptr() as *mut u8).add(off), n) };
        }
    }
    rv.set(v8::Number::new(scope, n as f64).into());
}

fn cb_ah_write<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = open_access(scope, &args, "write") else { return };
    if args.length() < 1 {
        return ah_throw(scope, "write", "1 argument required, but only 0 present.");
    }
    let Some((ab, off, len)) = view_of(scope, args.get(0)) else {
        return ah_throw(scope, "write", "parameter 1 is not of type 'ArrayBuffer'.");
    };
    let Ok(at) = at_option(scope, args.get(1), "write") else { return };
    let read_only = ACCESS.with(|m| m.borrow().get(&id).is_some_and(|a| a.mode == "read-only"));
    if read_only {
        crate::binding::protos::throw_dom(scope, "Failed to execute 'write' on 'FileSystemSyncAccessHandle': Cannot write to access handle in 'read-only' mode", "NoModificationAllowedError");
        return;
    }
    let mut data = vec![0u8; len];
    if len > 0 {
        if let Some(p) = ab.data() {
            unsafe { std::ptr::copy_nonoverlapping((p.as_ptr() as *const u8).add(off), data.as_mut_ptr(), len) };
        }
    }
    let n = ACCESS.with(|m| {
        let mut m = m.borrow_mut();
        let a = m.get_mut(&id)?;
        let start = at.unwrap_or(a.cursor);
        let n = store::with_file(&a.origin, &a.path, |fd| {
            fd.modified = now_ms();
            store::write_at(&mut fd.bytes, start, &data)
        })
        .unwrap_or(0);
        a.cursor = start + n;
        Some(n)
    });
    rv.set(v8::Number::new(scope, n.unwrap_or(0) as f64).into());
}

fn cb_ah_truncate<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = open_access(scope, &args, "truncate") else { return };
    if args.length() < 1 {
        return ah_throw(scope, "truncate", "1 argument required, but only 0 present.");
    }
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() || n < 0.0 || n > 9007199254740991.0 {
        return ah_throw(scope, "truncate", "Value is outside the 'unsigned long long' value range.");
    }
    let size = n.trunc() as usize;
    ACCESS.with(|m| {
        if let Some(a) = m.borrow_mut().get_mut(&id) {
            let _ = store::with_file(&a.origin, &a.path, |fd| {
                fd.bytes.resize(size, 0);
                fd.modified = now_ms();
            });
            a.cursor = a.cursor.min(size);
        }
    });
}

fn cb_ah_get_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = open_access(scope, &args, "getSize") else { return };
    let n = ACCESS.with(|m| m.borrow().get(&id).and_then(|a| store::with_file(&a.origin, &a.path, |fd| fd.bytes.len()).ok())).unwrap_or(0);
    rv.set(v8::Number::new(scope, n as f64).into());
}

/// `flush()`: the file's contents made durable on disk ([`store::persist`]), synchronously, as Chrome's is.
fn cb_ah_flush<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = open_access(scope, &args, "flush") else { return };
    let at = ACCESS.with(|m| m.borrow().get(&id).map(|a| (a.origin.clone(), a.path.clone())));
    if let Some((origin, path)) = at {
        let _ = store::persist(&origin, &path);
    }
}

fn cb_ah_close<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = access_id(scope, &args) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let release = ACCESS.with(|m| {
        let mut m = m.borrow_mut();
        let a = m.get_mut(&id)?;
        if a.closed {
            return None;
        }
        a.closed = true;
        Some((a.origin.clone(), a.path.clone()))
    });
    if let Some((origin, path)) = release {
        store::unlock(&origin, &path);
    }
}

fn cb_ah_mode<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = access_id(scope, &args) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let m = ACCESS.with(|m| m.borrow().get(&id).map(|a| a.mode)).unwrap_or("readwrite");
    rv.set(str_val(scope, m));
}

// ── move ─────────────────────────────────────────────────────────────────────────────────────────

/// `move(name)`, `move(directory)`, `move(directory, name)`: the entry goes there and THIS handle follows it.
fn cb_move<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(t) = target_of(scope, args.this().into()) else { return illegal(scope, &mut rv) };
    let (dest_dir, name) = match target_of(scope, args.get(0)).filter(|d| d.kind == "directory") {
        Some(d) => {
            let name = if args.length() > 1 { args.get(1).to_rust_string_lossy(scope) } else { t.path.last().cloned().unwrap_or_default() };
            (d.path, name)
        }
        None => (t.path[..t.path.len().saturating_sub(1)].to_vec(), args.get(0).to_rust_string_lossy(scope)),
    };
    if !store::name_allowed(&name) || t.path.is_empty() {
        return reject_type(scope, &mut rv, "Failed to execute 'move' on 'FileSystemFileHandle'");
    }
    match store::move_entry(&t.origin, &t.path, &dest_dir, &name) {
        Ok(()) => {
            let mut path = dest_dir;
            path.push(name);
            set_target(scope, args.this(), &Target { origin: t.origin, path, kind: t.kind });
            resolve_undefined(scope, &mut rv);
        }
        Err(f) => reject_fail(scope, &mut rv, f, "move", "FileSystemFileHandle"),
    }
}

// ── the kit ──────────────────────────────────────────────────────────────────────────────────────

/// The rows `tools/sync/gen_shells.py` claims as `opfs`.
pub(crate) fn install(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.label {
        "StorageManager::getDirectory" => method_row(scope, target, member, cb_get_directory),
        "FileSystemHandle::kind" => getter_row(scope, target, member, cb_kind),
        "FileSystemHandle::name" => getter_row(scope, target, member, cb_name),
        "FileSystemHandle::isSameEntry" => method_row(scope, target, member, cb_is_same_entry),
        "FileSystemHandle::queryPermission" | "FileSystemHandle::requestPermission" => method_row(scope, target, member, cb_permission),
        "FileSystemHandle::remove" => method_row(scope, target, member, cb_remove),
        "FileSystemDirectoryHandle::getFileHandle" => method_row(scope, target, member, cb_get_file_handle),
        "FileSystemDirectoryHandle::getDirectoryHandle" => method_row(scope, target, member, cb_get_directory_handle),
        "FileSystemDirectoryHandle::removeEntry" => method_row(scope, target, member, cb_remove_entry),
        "FileSystemDirectoryHandle::resolve" => method_row(scope, target, member, cb_resolve),
        "FileSystemDirectoryHandle::entries" => method_row(scope, target, member, cb_entries),
        "FileSystemDirectoryHandle::keys" => method_row(scope, target, member, cb_keys),
        "FileSystemDirectoryHandle::values" => method_row(scope, target, member, cb_values),
        "FileSystemFileHandle::getFile" => method_row(scope, target, member, cb_get_file),
        "FileSystemFileHandle::createWritable" => method_row(scope, target, member, cb_create_writable),
        "FileSystemFileHandle::createSyncAccessHandle" => method_row(scope, target, member, cb_create_sync_access_handle),
        "FileSystemFileHandle::move" => method_row(scope, target, member, cb_move),
        "FileSystemWritableFileStream::write" => method_row(scope, target, member, cb_ws_write),
        "FileSystemWritableFileStream::seek" => method_row(scope, target, member, cb_ws_seek),
        "FileSystemWritableFileStream::truncate" => method_row(scope, target, member, cb_ws_truncate),
        "FileSystemWritableFileStream::mode" => getter_row(scope, target, member, cb_ws_mode),
        "FileSystemSyncAccessHandle::read" => method_row(scope, target, member, cb_ah_read),
        "FileSystemSyncAccessHandle::write" => method_row(scope, target, member, cb_ah_write),
        "FileSystemSyncAccessHandle::truncate" => method_row(scope, target, member, cb_ah_truncate),
        "FileSystemSyncAccessHandle::getSize" => method_row(scope, target, member, cb_ah_get_size),
        "FileSystemSyncAccessHandle::flush" => method_row(scope, target, member, cb_ah_flush),
        "FileSystemSyncAccessHandle::close" => method_row(scope, target, member, cb_ah_close),
        "FileSystemSyncAccessHandle::mode" => getter_row(scope, target, member, cb_ah_mode),
        other => debug_assert!(false, "opfs::install: unexpected member {other}"),
    }
}

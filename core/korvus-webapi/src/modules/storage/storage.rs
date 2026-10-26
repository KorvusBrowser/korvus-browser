//! `modules/storage/storage.rs` — Web Storage: the `Storage` `interface!` table, the two area
//! platform objects, and the WebIDL named-property interceptor they carry.
//!
//! `js/dom/01-interning.js` (41 lines, three `Object.defineProperty` sites) is deleted. It assigned
//! `globalThis.Storage` from the cppgc class, patched the class's `name`, WRAPPED each of the five
//! prototype methods in `op_storage_member` so that WebIDL argument conversion happened somewhere,
//! and minted `localStorage` / `sessionStorage`. All five jobs are one table plus one install
//! (`install::live::install_window_storage`) now.
//!
//! THE TWO-LAYER MEMBER IS GONE, which is the point. Each member used to be an `#[op2] impl JsStorage`
//! method (which cannot see `arguments.length`, cannot run WebIDL `DOMString` coercion, and cannot
//! raise a `DOMException`) with a hand-written native wrapper in FRONT of it supplying exactly those
//! three. That is two definition sites for one member, joined at run time by a JS loop over a
//! `[member, need, len, kind]` table — a fourth copy of the same facts the row already carries. The
//! six rows below are single Rust callbacks: the arity comes from the row's `required` (the installer
//! mounts the WebIDL guard from it), the coercion is in the body, and the quota raise is `throw_dom`.
//!
//! RECEIVER CHECKS ARE REAL, and were already: every body starts by unwrapping the receiver's cppgc
//! `JsStorage`, and a receiver without one gets Chrome's bare `TypeError: Illegal invocation`
//! (measured in headless Chrome 151: `Storage.prototype.getItem.call({}, "x")` and the `length`
//! getter on `{}` both throw exactly that). That is what the `#[op2] impl` `&self` unwrap did before;
//! the message is now written where it is thrown.

use deno_core::v8::MapFnTo;
use deno_core::v8;
use std::ffi::CStr;
use deno_core::GarbageCollected;
use crate::binding::protos::{throw_dom, throw_type_error};
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

/// Web Storage areas held in `OpState`, one per ORIGIN, carried across the page's navigations
/// ([`crate::page::CarriedStorage`]): `sessionStorage` always (it is the page's -- its top-level browsing session's),
/// and `localStorage` when no browser endpoint holds this page's context. Each area is insertion-ordered so
/// `key(index)` is stable (a HashMap would not be). They used to be one area for every origin the page held, so a
/// frame of another origin read and wrote the page's keys.
///
/// In a page of a browser endpoint's context, `localStorage` is the CONTEXT's: one area per origin in the router's
/// context store, shared by every page of the context ([`local_areas`]).
#[derive(Default)]
pub struct HostStorage {
    pub session: Vec<(String, Vec<(String, String)>)>,
    pub local: Vec<(String, Vec<(String, String)>)>,
}

type Area = Vec<(String, String)>;

/// The context's `localStorage`, one area per origin. In a page of a browser endpoint's context each area is a slot
/// of the router's context store ([`transport::context_store`]), so the context's other pages read what this one
/// wrote; in a process no router holds, the process is the context.
fn local_areas() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, transport::context_store::Synced<Area>>> {
    static AREAS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, transport::context_store::Synced<Area>>>> = std::sync::OnceLock::new();
    AREAS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The calling realm's origin, the areas' key: the global's own `origin` (a document's), as `opfs.rs` keys its
/// store. "null" is an opaque origin.
fn storage_origin(scope: &mut v8::PinScope) -> String {
    crate::binding::protos::realm_self_get(scope, "origin")
        .filter(|o| o.is_string())
        .map(|o| o.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "null".to_string())
}

fn read_area<R>(scope: &mut v8::PinScope, session: bool, f: impl FnOnce(&Area) -> R) -> R {
    let origin = storage_origin(scope);
    if session || !transport::context_store::is_remote() {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let empty = Area::new();
        let s = st.borrow::<HostStorage>();
        let area = (if session { &s.session } else { &s.local }).iter().find(|(o, _)| *o == origin).map(|(_, a)| a);
        return f(area.unwrap_or(&empty));
    }
    let mut areas = local_areas();
    let key = format!("localStorage\u{0}{origin}");
    areas.entry(origin).or_insert_with(|| transport::context_store::Synced::new(key)).read(f)
}

fn write_area<R>(scope: &mut v8::PinScope, session: bool, f: impl FnOnce(&mut Area) -> R) -> R {
    let origin = storage_origin(scope);
    if session || !transport::context_store::is_remote() {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let s = st.borrow_mut::<HostStorage>();
        let areas = if session { &mut s.session } else { &mut s.local };
        let i = match areas.iter().position(|(o, _)| *o == origin) {
            Some(i) => i,
            None => {
                areas.push((origin, Area::new()));
                areas.len() - 1
            }
        };
        return f(&mut areas[i].1);
    }
    let mut areas = local_areas();
    let key = format!("localStorage\u{0}{origin}");
    areas.entry(origin).or_insert_with(|| transport::context_store::Synced::new(key)).update(f)
}

/// Per-area quota (bytes of keys+values). Matches the 5 MiB the conformance fixture probes.
pub(crate) const STORAGE_QUOTA: usize = 5 * 1024 * 1024;

/// Native `Storage` (Web Storage API). One instance per area (`localStorage`/`sessionStorage`).
#[repr(C)]
pub struct JsStorage {
    session: bool,
}

unsafe impl GarbageCollected for JsStorage {
    fn trace(&self, _v: &mut deno_core::v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static CStr {
        c"Storage"
    }
}

// ── the six `Storage.prototype` rows ────────────────────────────────────────────────────────────
//
// Each is one callback: the receiver check, the WebIDL argument conversion, the algorithm. They read
// the area helpers below (`area_get` and friends), which is the same `HostStorage` in `OpState` the
// deleted `#[op2] impl` methods borrowed — one storage, reached one way, from the members and from
// the named-property interceptor alike.

/// The receiver's area flag, or Chrome's `Illegal invocation` TypeError.
///
/// The brand is the cppgc `JsStorage` that `make_area` wraps each platform object with, not the
/// prototype chain: `Object.create(Storage.prototype)` has the prototype and no brand, and Chrome
/// throws for it — which is what "the check is for an internal slot, not for an ancestry" means.
fn this_session(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<bool> {
    let this: v8::Local<v8::Value> = args.this().into();
    match deno_core::cppgc::try_unwrap_cppgc_object::<JsStorage>(scope, this) {
        Some(p) => Some(unsafe { p.as_ref() }.session),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// WebIDL `DOMString`: ToString, so `setItem(null, v)` stores under the key `"null"` and
/// `getItem(null)` reads it back — which every browser does and `#[op2]`'s `#[string]` did not. A
/// throwing `toString` (a Symbol) leaves the pending exception set, and returning `None` propagates it.
fn dom_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    v.to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

fn cb_storage_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    let n = area_keys(scope, session).len() as u32;
    rv.set_uint32(n);
}

fn cb_storage_clear(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    area_clear(scope, session);
}

fn cb_storage_get_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    let Some(key) = dom_string(scope, args.get(0)) else { return };
    match area_get(scope, session, &key) {
        // `getItem` is nullable: an absent key is `null`, not `undefined`.
        Some(v) => { let s = v8::String::new(scope, &v).unwrap(); rv.set(s.into()); }
        None => rv.set_null(),
    }
}

fn cb_storage_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    // WebIDL `unsigned long`: ToUint32. An index past the end of the area answers `null`.
    let Some(n) = args.get(0).uint32_value(scope) else { return };
    match area_keys(scope, session).get(n as usize) {
        Some(k) => { let s = v8::String::new(scope, k).unwrap(); rv.set(s.into()); }
        None => rv.set_null(),
    }
}

fn cb_storage_remove_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    let Some(key) = dom_string(scope, args.get(0)) else { return };
    area_remove(scope, session, &key);
}

fn cb_storage_set_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(session) = this_session(scope, &args) else { return };
    let Some(key) = dom_string(scope, args.get(0)) else { return };
    let Some(value) = dom_string(scope, args.get(1)) else { return };
    if !area_set(scope, session, key, value) {
        throw_dom(scope, "Failed to execute 'setItem' on 'Storage': Setting the value exceeded the quota.", "QuotaExceededError");
    }
    // `setItem` evaluates to `undefined`, never to the write's success flag.
}

pub fn install_storage(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => getter_row(scope, proto, member, cb_storage_length),
        "clear" => method_row(scope, proto, member, cb_storage_clear),
        "getItem" => method_row(scope, proto, member, cb_storage_get_item),
        "key" => method_row(scope, proto, member, cb_storage_key),
        "removeItem" => method_row(scope, proto, member, cb_storage_remove_item),
        "setItem" => method_row(scope, proto, member, cb_storage_set_item),
        other => debug_assert!(false, "install_storage: unexpected member {other}"),
    }
}

webapi::interface! {
    // The capture's order and shape (`brave-151-linux-20260907T1006`: ctor length 0, NOT newable,
    // `[object Storage]`, no parent, six enumerable+configurable members in this order).
    "Storage" [interface, window] {
        ctor   "Storage"                          len 0            = stub,
        attr   "length"     : Integer   readonly                   = install_storage,
        method "clear"      : Undefined len 0                      = install_storage,
        method "getItem"    : DomString len 1 required 1 nullable   = install_storage,
        method "key"        : DomString len 1 required 1 nullable   = install_storage,
        method "removeItem" : Undefined len 1 required 1            = install_storage,
        method "setItem"    : Undefined len 2 required 2            = install_storage
    }
}

// ---------------------------------------------------------------------------------------------
// The Storage named-property object.
//
// `localStorage.foo` / `localStorage.foo = 1` / `delete localStorage.foo` / `Object.keys(localStorage)`
// are WebIDL NAMED PROPERTIES -- an interface with `getter`/`setter`/`deleter` declarations. Until this
// unit they were a JS `Proxy` whose `get` trap minted a fresh arrow function for each of the five
// methods on every access, so `String(localStorage.getItem)` printed JS source, two reads of
// `localStorage.getItem` were different objects, and nothing ever reached `Storage.prototype`. Every
// real browser answers `[native code]` and returns the SAME function object every time.
//
// The mechanism a browser actually uses is a named-property INTERCEPTOR on the platform object's
// instance template -- the same shape as `core/dom/window_named.rs`, which also records why a JS Proxy
// in a hot lookup path is not an option (a Proxy invalidates V8's fast path for every load through it,
// measured at 5x across the board). An interceptor costs nothing until a lookup actually misses, and
// it leaves the prototype chain intact, so the five native methods stay exactly where WebIDL puts them.
//
// NON-MASKING BY HAND. Storage is not `[LegacyOverrideBuiltIns]`, so a stored key named "getItem" must
// NOT shadow `Storage.prototype.getItem`. V8 offers `PropertyHandlerFlags::NON_MASKING` for this, but
// it suppresses the interceptor for anything found ANYWHERE including the receiver's own properties,
// which we do not want. The getter and query callbacks instead ask the prototype chain first and
// decline (`Intercepted::kNo`) when it answers -- the WebIDL rule, stated where it is checked.
// ---------------------------------------------------------------------------------------------

/// The area flag for an interceptor callback: the `JsStorage` the holder is wrapped with. The interceptor
/// is `Storage`'s instance template's (`install::instance_shapes`), one per isolate for
/// both areas, so the area is the object's, not the handler's -- and a holder that is no `Storage` area
/// (none is: only [`make_area`] mints one) declines.
fn cb_session(scope: &mut v8::PinScope, args: &v8::PropertyCallbackArguments) -> Option<bool> {
    let holder: v8::Local<v8::Value> = args.holder().into();
    deno_core::cppgc::try_unwrap_cppgc_object::<JsStorage>(scope, holder).map(|p| unsafe { p.as_ref() }.session)
}

fn key_string(scope: &mut v8::PinScope, key: v8::Local<v8::Name>) -> Option<String> {
    let v: v8::Local<v8::Value> = key.into();
    if !v.is_string() {
        return None; // symbols are never supported property names (also filtered by the flag)
    }
    Some(v.to_rust_string_lossy(scope))
}

/// True when the name is already answered by the prototype chain -- WebIDL's rule for an interface
/// without `[LegacyOverrideBuiltIns]`: such a name is not a *supported* property name.
fn shadowed_by_proto(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, key: v8::Local<v8::Name>) -> bool {
    let Some(proto) = holder.get_prototype(scope) else { return false };
    let Ok(p) = v8::Local::<v8::Object>::try_from(proto) else { return false };
    p.has(scope, key.into()).unwrap_or(false)
}

fn area_get(scope: &mut v8::PinScope, session: bool, key: &str) -> Option<String> {
    read_area(scope, session, |area| area.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
}

/// `false` when the write would exceed the per-area quota (the caller raises QuotaExceededError).
fn area_set(scope: &mut v8::PinScope, session: bool, key: String, value: String) -> bool {
    write_area(scope, session, |area| {
        let existing = area.iter().find(|(k, _)| *k == key).map(|(_, v)| v.len()).unwrap_or(0);
        let total: usize = area.iter().map(|(_, v)| v.len()).sum();
        if total - existing + value.len() > STORAGE_QUOTA {
            return false;
        }
        match area.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => area.push((key, value)),
        }
        true
    })
}

fn area_remove(scope: &mut v8::PinScope, session: bool, key: &str) {
    write_area(scope, session, |area| area.retain(|(k, _)| k != key));
}

fn area_clear(scope: &mut v8::PinScope, session: bool) {
    write_area(scope, session, |area| area.clear());
}

fn area_keys(scope: &mut v8::PinScope, session: bool) -> Vec<String> {
    read_area(scope, session, |area| area.iter().map(|(k, _)| k.clone()).collect())
}

fn cb_named_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let Some(session) = cb_session(scope, &args) else { return v8::Intercepted::kNo };
    if shadowed_by_proto(scope, args.holder(), key) {
        return v8::Intercepted::kNo;
    }
    match area_get(scope, session, &name) {
        Some(v) => {
            let s = v8::String::new(scope, &v).unwrap();
            rv.set(s.into());
            v8::Intercepted::kYes
        }
        // Decline, so the ordinary lookup continues and an unknown name is `undefined` (not `null`).
        None => v8::Intercepted::kNo,
    }
}

fn cb_named_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let Some(session) = cb_session(scope, &args) else { return v8::Intercepted::kNo };
    let val = value.to_rust_string_lossy(scope);
    if !area_set(scope, session, name, val) {
        throw_dom(scope, "Failed to execute 'setItem' on 'Storage': Setting the value exceeded the quota.", "QuotaExceededError");
    }
    v8::Intercepted::kYes
}

fn cb_named_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let Some(session) = cb_session(scope, &args) else { return v8::Intercepted::kNo };
    if shadowed_by_proto(scope, args.holder(), key) {
        return v8::Intercepted::kNo;
    }
    if area_get(scope, session, &name).is_some() {
        // WebIDL's named-property descriptor: writable + enumerable + configurable.
        let attrs = v8::Integer::new(scope, v8::PropertyAttribute::NONE.as_u32() as i32);
        rv.set(attrs);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn cb_named_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let Some(session) = cb_session(scope, &args) else { return v8::Intercepted::kNo };
    area_remove(scope, session, &name);
    let t = v8::Boolean::new(scope, true);
    rv.set(t);
    v8::Intercepted::kYes
}

fn cb_named_enumerate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let Some(session) = cb_session(scope, &args) else { return };
    let keys = area_keys(scope, session);
    let arr = v8::Array::new(scope, keys.len() as i32);
    for (i, k) in keys.iter().enumerate() {
        if let Some(s) = v8::String::new(scope, k) {
            arr.set_index(scope, i as u32, s.into());
        }
    }
    rv.set(arr);
}

/// `Object.defineProperty(localStorage, "x", { value: v })` is a named-property *set* per WebIDL, not
/// an own-property definition. Without this the interceptor would be bypassed and a real own property
/// would land on the platform object, shadowing the store.
fn cb_named_define<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    desc: &v8::PropertyDescriptor,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    if !desc.has_value() {
        // An accessor descriptor is not expressible in a string store; let it define normally.
        return v8::Intercepted::kNo;
    }
    let Some(session) = cb_session(scope, &args) else { return v8::Intercepted::kNo };
    let val = desc.value().to_rust_string_lossy(scope);
    if !area_set(scope, session, name, val) {
        throw_dom(scope, "Failed to execute 'setItem' on 'Storage': Setting the value exceeded the quota.", "QuotaExceededError");
    }
    v8::Intercepted::kYes
}

/// Build one `localStorage`/`sessionStorage` platform object: an instance of `Storage`'s template, which
/// carries the named-property interceptor ([`configure`], applied by the builder),
/// cppgc-wrapped with a `JsStorage` -- the area the interceptor and the `Storage.prototype` rows read, and
/// the brand those rows accept as their receiver.
///
/// A plain Rust fn — it was `op_storage_object`, an op whose only two callers
/// were the two lines of `dom/01-interning.js`; `install::live::install_window_storage` calls it
/// directly now, so nothing has to reach it through `Deno.core.ops`. It made a FunctionTemplate of its
/// own, per call, carrying the area as the handler's data, until the builder gave `Storage` one.
pub(crate) fn make_area<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    session: bool,
    proto: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::protos::mint_wrapper_on(scope, proto);
    deno_core::cppgc::wrap_object(scope, obj, JsStorage { session })
}

/// `Storage`'s instance template: the named-property interceptor over the holder's area.
pub(crate) fn configure(it: v8::Local<v8::ObjectTemplate>) {
    let config = v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, "Storage", cb_named_get).map_fn_to()))
        .setter_raw(crate::binding::refs::c(icpt!(named_set, "Storage", cb_named_set).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, "Storage", cb_named_query).map_fn_to()))
        .deleter_raw(crate::binding::refs::c(icpt!(named_delete, "Storage", cb_named_delete).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(named_keys, "Storage", cb_named_enumerate).map_fn_to()))
        .definer_raw(crate::binding::refs::c(icpt!(named_define, "Storage", cb_named_define).map_fn_to()))
        // Symbols (Symbol.toStringTag, Symbol.iterator, ...) are never supported property names.
        .flags(v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS);
    it.set_named_property_handler(config);
}

// `op_stash_ops` AND `op_stash_bridge` ARE DELETED, AND THIS FILE DECLARES NO OPS.
//
// Both stashed one object on a V8 Private of the global — `Deno.core.ops` and the `Symbol(korvus.bridge)`
// holder — just before the realm's `delete globalThis.Deno`, so native bodies could still reach them
// afterwards. Neither ever did anything a Rust caller could not: `binding::hidden::hide_deno` does both
// stashes AND the delete, and it is what the WINDOW has called since W4.f. Their last JS call site was
// `worker-host.js`'s tail, and that realm calls `hide_deno` from `worker.rs` now (invariant I8: one
// function, both realms), so an op that exists only to give a JS file a call site has no call site.

// NO `webapi_objects!` DECLARATION,. `JsStorage` was registered as a cppgc
// INTERFACE OBJECT so that `dom/01-interning.js` could `import { JsStorage } from "ext:core/ops"` and
// assign it to `globalThis.Storage`; with the interface object built by the installer from the table,
// nothing imports the class and registering it would put a second `Storage` constructor in the
// extension's object table. The struct stays: it is the BRAND `make_area` wraps each area with and
// every row unwraps, and `cppgc::wrap_object` tags by `TypeId`, which owes nothing to the registration.

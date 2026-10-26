//! The twelve IndexedDB interfaces, as `interface!` tables (invariants I1/I2/I3/I8).
//!
//! WHAT THIS REPLACES. `dom/08e-indexeddb.js`: 147 lines that imported the eight cppgc classes from
//! `ext:core/ops`, gave each a throwing JS wrapper sharing the class template's prototype, minted
//! `globalThis.indexedDB`, spliced three `EventTarget` chains by hand, built `IDBOpenDBRequest` /
//! `IDBCursorWithValue` prototypes with `MK`, declared `IDBVersionChangeEvent` as a JS `class ... extends
//! Event`, and then installed all 95 members with seven `Object.defineProperty` sites. Every one of those
//! members is now a row of one of the tables below, mounted by THE installer
//! (`install::live::install_window_indexeddb` / `install_worker_indexeddb`) in BOTH realms.
//!
//! ROWS ARE THE CAPTURE'S. Names, order, kinds and descriptor flags are copied verbatim from the twelve
//! blocks the generated registry carried before the cutover (which the generator emitted from
//! `surface.json`); headless Chrome 150.0.7871.46 on this machine confirmed the prototype orders
//! member-for-member on 2026-09-10, `constructor` last on each:
//!
//! ```text
//! IDBDatabase    name,version,objectStoreNames,onabort,onclose,onerror,onversionchange,close,
//!                createObjectStore,deleteObjectStore,transaction,constructor
//! IDBRequest     result,error,source,transaction,readyState,onsuccess,onerror,constructor
//! IDBTransaction objectStoreNames,mode,durability,db,error,onabort,oncomplete,onerror,abort,commit,
//!                objectStore,constructor
//! IDBObjectStore name,keyPath,indexNames,transaction,autoIncrement,add,clear,count,createIndex,delete,
//!                deleteIndex,get,getAll,getAllKeys,getAllRecords,getKey,index,openCursor,openKeyCursor,
//!                put,constructor
//! IDBCursor      source,direction,key,primaryKey,request,advance,continue,continuePrimaryKey,delete,
//!                update,constructor
//! IDBRecord      key,primaryKey,value,constructor
//! IDBKeyRange    (statics) bound[2],lowerBound[1],only[1],upperBound[1]
//! ```
//!
//! BODIES ARE THE RUST CALLBACKS THAT ALREADY EXISTED — the `cb_*` half of the `op_*` builders the JS
//! imported. The builders are gone; the callbacks are mounted directly from the rows, so a member is
//! `[native code]` by construction and there is no `Deno.core.ops` name for a page to reach.
//!
//! RECEIVER CHECKS. Every body enters through [`idb_recv`](crate::modules::indexeddb::backend::idb_recv)
//! (or the equivalent brand test), so `IDBRequest.prototype.result`'s getter called on
//! `Object.create(IDBRequest.prototype)` throws `TypeError: Illegal invocation` — which is what the
//! bundle's oracle records for 40-odd members across this family, and what the pre-cutover bodies
//! answered `undefined` to.
//!
//! IDENTITY. A cppgc instance keeps its cppgc brand (the unwrap reads the object's wrapper tag, never its
//! prototype) and is re-prototyped at every mint site to the TABLE's prototype, so `instanceof`,
//! `constructor` and `Object.getPrototypeOf` answer the interface object a page can see. The class
//! templates' own `#[op2]` `#[constructor]`/`#[getter]` members are no longer reachable from any realm.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, global_get, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;
use crate::install::live::getter_row;
use crate::install::live::method_row;

// ── row mounts ──────────────────────────────────────────────────────────────────────────────────

/// The interface a row belongs to, from its `"Iface::member"` label -- what Blink's arity message names.
fn row_iface(member: &MemberDef) -> &'static str {
    member.label.split("::").next().unwrap_or(member.name)
}


/// Mount one read/write attribute row (`IDBObjectStore.name`, `IDBIndex.name` -- renamable in an upgrade).
fn pair_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    get_cb: impl v8::MapFnTo<v8::FunctionCallback>,
    set_cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let g = build_fn(scope, get_cb, &format!("get {}", member.name), 0);
    let s = build_fn(scope, set_cb, &format!("set {}", member.name), 1);
    define_accessor(scope, proto, member, g, Some(s));
}

// ── the on<type> handlers ───────────────────────────────────────────────────────────────────────
//
// The eleven handler rows of the four IndexedDB event targets go through THE handler store --
// `et_on_get` / `et_on_set`, the same listener list `addEventListener` writes and `idb_fire`'s
// `dispatchEvent` reads -- behind the WebIDL brand check the shared `event_handler` kit cannot make.
//
// The kit accepts any receiver, because most of the surface's `on*` members sit on prototypes whose
// instances are not cppgc-branded. These four are, and the bundle's oracle records `TypeError` for all
// twenty-two of their handler getters and setters entered with `Object.create(Iface.prototype)`. The
// brand is the cppgc class, so an `IDBRequest` also satisfies `IDBOpenDBRequest`'s two rows (Chrome
// distinguishes them; nothing in the corpus or the oracle asks).
fn on_data<'s>(scope: &mut v8::PinScope<'s, '_>, member: &'static MemberDef) -> v8::Local<'s, v8::Value> {
    let ty = member.name.strip_prefix("on").unwrap_or(member.name);
    let cls = match row_iface(member) {
        "IDBTransaction" => "tx",
        "IDBDatabase" => "db",
        _ => "req",
    };
    str_val(scope, &format!("{cls}|{ty}"))
}

/// The event type from the accessor's `"<class>|<type>"` data, once `this` is confirmed to carry that
/// class's cppgc brand; otherwise the WebIDL TypeError and `None`.
fn on_check(scope: &mut v8::PinScope, data: &str, this: v8::Local<v8::Value>) -> Option<String> {
    use crate::modules::indexeddb::{IdbDatabase, IdbRequest, IdbTransaction};
    let (cls, ty) = data.split_once('|')?;
    let ok = match cls {
        "tx" => deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, this).is_some(),
        "db" => deno_core::cppgc::try_unwrap_cppgc_object::<IdbDatabase>(scope, this).is_some(),
        _ => deno_core::cppgc::try_unwrap_cppgc_object::<IdbRequest>(scope, this).is_some(),
    };
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return None;
    }
    Some(ty.to_string())
}

fn cb_idb_on_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(ty) = on_check(scope, &data, this) else { return };
    let v = crate::core::events::event_target::et_on_get(scope, this, &ty);
    rv.set(v);
}

fn cb_idb_on_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(ty) = on_check(scope, &data, this) else { return };
    let v = args.get(0);
    crate::core::events::event_target::et_on_set(scope, this, &ty, v);
}

/// The body every `handler` row of the four IndexedDB event targets names.
pub fn install_idb_handler(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let ty = member.name.strip_prefix("on").unwrap_or(member.name);
    let gd = on_data(scope, member);
    let g = build_fn_data(scope, cb_idb_on_get, &format!("get on{ty}"), 0, gd);
    let sd = on_data(scope, member);
    let s = build_fn_data(scope, cb_idb_on_set, &format!("set on{ty}"), 1, sd);
    define_accessor(scope, proto, member, g, Some(s));
}

// ── IDBRecord: a Rust-minted instance over the table's prototype ─────────────────────────────────
//
// `getAllRecords()` used to resolve with plain objects carrying three OWN data properties. Chrome
// resolves with `IDBRecord` instances whose own-property list is EMPTY and whose three members are
// prototype accessors (probed 2026-09-10: `T0=[object IDBRecord] own0=[] protoOK=true`). So the record
// is minted here: a bare object re-prototyped to `IDBRecord.prototype`, its three values on `v8::Private`
// slots one shared native getter reads back -- the shape `SecurityPolicyViolationEvent` uses.

/// The private-slot namespace an `IDBRecord`'s three values live in (never a page-visible key).
const REC_SLOT: &str = "korvus_idbrec_";
/// The brand every Rust-minted `IDBRecord` carries, so a foreign receiver is told apart from a real one.
const REC_BRAND: &str = "korvus_idbrec_brand";

fn rec_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn rec_set(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, member: &str, v: v8::Local<v8::Value>) {
    let k = rec_key(scope, &format!("{REC_SLOT}{member}"));
    obj.set_private(scope, k, v);
}

/// Mint one `IDBRecord` over the table's prototype: `{key, primaryKey, value}` on hidden slots.
pub(crate) fn idb_make_record<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Value>,
    primary_key: v8::Local<'s, v8::Value>,
    value: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Value> {
    let obj = v8::Object::new(scope);
    crate::modules::indexeddb::backend::idb_set_proto(scope, obj, "IDBRecord");
    let brand = rec_key(scope, REC_BRAND);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    obj.set_private(scope, brand, t);
    rec_set(scope, obj, "key", key);
    rec_set(scope, obj, "primaryKey", primary_key);
    rec_set(scope, obj, "value", value);
    obj.into()
}

/// The one getter behind `IDBRecord`'s three members; `args.data()` is the slot name, bound at install.
fn cb_record_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let brand = rec_key(scope, REC_BRAND);
    if !this.get_private(scope, brand).map(|v| v.is_true()).unwrap_or(false) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let Ok(name) = v8::Local::<v8::String>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let k = v8::Private::for_api(scope, Some(name));
    match this.get_private(scope, k) {
        Some(v) => rv.set(v),
        None => rv.set_undefined(),
    }
}

pub fn install_record(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    debug_assert!(matches!(member.name, "key" | "primaryKey" | "value"), "install_record: unexpected member {}", member.name);
    let d = str_val(scope, &format!("{REC_SLOT}{}", member.name));
    let g = build_fn_data(scope, cb_record_get, &format!("get {}", member.name), 0, d);
    define_accessor(scope, proto, member, g, None);
}

// ── IDBVersionChangeEvent: the one constructible interface of the twelve ─────────────────────────

/// The four members' hidden slots are the event slot namespace (`core/events/event.rs`), so they read
/// back through the same machinery every other event member does and the instance owns nothing but
/// `isTrusted` -- which is what Chrome's own instance owns (probed 2026-09-10: `own = isTrusted`).
fn cb_vce_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::events::event::evt_slot_read;
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    match evt_slot_read(scope, args.this(), &name) {
        Some(v) => rv.set(v),
        // Chrome throws for a receiver that is not an IDBVersionChangeEvent (the bundle's oracle records
        // it for `dataLoss` and `dataLossMessage`); a real one always has all four slots written.
        None => throw_type_error(scope, "Illegal invocation"),
    }
}

pub fn install_version_change_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    debug_assert!(
        matches!(member.name, "oldVersion" | "newVersion" | "dataLoss" | "dataLossMessage"),
        "install_version_change_event: unexpected member {}",
        member.name
    );
    let d = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_vce_get, &format!("get {}", member.name), 0, d);
    define_accessor(scope, proto, member, g, None);
}

/// Write the four `IDBVersionChangeEvent` slots onto an event object.
///
/// One place, so the page-constructed event and the `upgradeneeded` the engine fires carry identical
/// state. `data_loss` is the `IDBDataLossAmount` enum ("none" | "total").
pub(crate) fn vce_write_slots(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, old_version: f64, new_version: Option<f64>, data_loss: &str, data_loss_message: &str) {
    use crate::core::events::event::{evt_slot_write, evt_slot_write_null, evt_slot_write_num};
    evt_slot_write_num(scope, ev, "oldVersion", old_version);
    match new_version {
        Some(n) => evt_slot_write_num(scope, ev, "newVersion", n),
        None => evt_slot_write_null(scope, ev, "newVersion"),
    }
    let dl = str_val(scope, data_loss);
    evt_slot_write(scope, ev, "dataLoss", dl);
    let dlm = str_val(scope, data_loss_message);
    evt_slot_write(scope, ev, "dataLossMessage", dlm);
}

/// `new IDBVersionChangeEvent(type, init)`.
///
/// The base is constructed through the realm's own `Event` constructor -- literally what `super(type)`
/// did in the JS class this replaces -- so `isTrusted` keeps its own `[LegacyUnforgeable]` accessor,
/// `timeStamp` its clock and the nine base slots their writer. The result is then re-prototyped to
/// `new.target.prototype`, which is what makes `class M extends IDBVersionChangeEvent {}` produce an `M`
/// (Chrome, probed 2026-09-10: `Object.getPrototypeOf(new M("z")) === M.prototype`).
///
/// The init dictionary's EventInit half is deliberately NOT forwarded: Chrome drops it. Probed
/// 2026-09-10 -- `new IDBVersionChangeEvent("v", {oldVersion: 5, newVersion: 7, bubbles: true}).bubbles`
/// is `false` there, while `{dataLoss: "total"}` IS honoured. Blink's constructor reaches the base with
/// its own non-bubbling, non-cancelable form and reads only its own members from the dictionary.
fn cb_vce_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("IDBVersionChangeEvent"));
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'IDBVersionChangeEvent': 1 argument required, but only 0 present.");
        return;
    }
    // The arguments as WebIDL converts them, in order (Chrome 150): `type` to a string, then
    // `IDBVersionChangeEventInit` -- `bubbles`, `cancelable`, `composed`, `dataLoss`, `newVersion`, `oldVersion`, each
    // read once and converted as it is read (`dataLoss` checked as the enum it is). `dataLossMessage` is no member of
    // Chrome's init dictionary: a page's is never read, and the event's stays "". korvus read the dictionary before
    // `type`, its own members alone (and `dataLossMessage`), clamped a negative version to 0 where the conversion is
    // modular, and answered a non-object with a message of its own.
    let Some(ty) = crate::core::events::ctor::arg_string(scope, &args, 0, "IDBVersionChangeEvent") else { return };
    let Ok(d) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "IDBVersionChangeEventInit", "Failed to construct 'IDBVersionChangeEvent': ") else { return };
    let data_loss = get_prop(scope, d, "dataLoss").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_else(|| "none".into());
    let data_loss_message = String::new();
    let old_version = get_prop(scope, d, "oldVersion").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let new_version = get_prop(scope, d, "newVersion").filter(|v| v.is_number()).and_then(|v| v.number_value(scope));

    // `Event`'s constructor steps, on THIS: the instance V8 made from `IDBVersionChangeEvent`'s template, on
    // `new.target`'s prototype. The dictionary's EventInit half is read (above) and NOT applied:
    // Blink's constructor reaches the base with its own non-bubbling, non-cancelable form.
    let ev = args.this();
    crate::core::events::ctor::init_event(scope, ev, &ty, None);
    vce_write_slots(scope, ev, old_version, new_version, &data_loss, &data_loss_message);
    rv.set(ev.into());
}

/// Build `IDBVersionChangeEvent` -- the one interface object of the twelve that is a real constructor --
/// with `build_newable`, the one builder of that shape: the table's rows on a fresh prototype, spliced
/// onto `Event`, through the template step (it was a hand-rolled `build_ctor` + fresh
/// prototype + `overwrite_prototype`, which no template recorded).
pub fn install_version_change_event_interface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    ledger: &mut crate::install::installer::InstallLedger,
) {
    crate::install::live::rows::build_newable(scope, global, &IDBVERSIONCHANGEEVENT, cb_vce_ctor, Some("Event"), None, ledger);
}

/// `(prototype, constructor)` of an interface already on this realm's global -- the `parent` argument
/// `installer::create_interface` splices onto.
pub(crate) fn parent_of<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)> {
    let ctor = global_get(scope, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor.into(), "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    Some((proto, ctor))
}

// ── the per-interface bodies ────────────────────────────────────────────────────────────────────

pub fn install_request(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_request::{cb_req_error, cb_req_ready_state, cb_req_result, cb_req_source, cb_req_transaction};
    match member.name {
        "result" => getter_row(scope, proto, member, cb_req_result),
        "error" => getter_row(scope, proto, member, cb_req_error),
        "source" => getter_row(scope, proto, member, cb_req_source),
        "transaction" => getter_row(scope, proto, member, cb_req_transaction),
        "readyState" => getter_row(scope, proto, member, cb_req_ready_state),
        other => debug_assert!(false, "install_request: unexpected member {other}"),
    }
}

pub fn install_transaction(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_transaction::{
        cb_tx_abort, cb_tx_commit, cb_tx_db, cb_tx_durability, cb_tx_error, cb_tx_mode, cb_tx_object_store, cb_tx_store_names,
    };
    match member.name {
        "objectStoreNames" => getter_row(scope, proto, member, cb_tx_store_names),
        "mode" => getter_row(scope, proto, member, cb_tx_mode),
        "durability" => getter_row(scope, proto, member, cb_tx_durability),
        "db" => getter_row(scope, proto, member, cb_tx_db),
        "error" => getter_row(scope, proto, member, cb_tx_error),
        "abort" => method_row(scope, proto, member, cb_tx_abort),
        "commit" => method_row(scope, proto, member, cb_tx_commit),
        "objectStore" => method_row(scope, proto, member, cb_tx_object_store),
        other => debug_assert!(false, "install_transaction: unexpected member {other}"),
    }
}

pub fn install_database(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_database::{
        cb_db_close, cb_db_create_store, cb_db_delete_store, cb_db_name, cb_db_store_names, cb_db_transaction, cb_db_version,
    };
    match member.name {
        "name" => getter_row(scope, proto, member, cb_db_name),
        "version" => getter_row(scope, proto, member, cb_db_version),
        "objectStoreNames" => getter_row(scope, proto, member, cb_db_store_names),
        "close" => method_row(scope, proto, member, cb_db_close),
        "createObjectStore" => method_row(scope, proto, member, cb_db_create_store),
        "deleteObjectStore" => method_row(scope, proto, member, cb_db_delete_store),
        "transaction" => method_row(scope, proto, member, cb_db_transaction),
        other => debug_assert!(false, "install_database: unexpected member {other}"),
    }
}

pub fn install_factory(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_factory::{cb_idb_cmp, cb_idb_databases, cb_idb_delete_db, cb_idb_open};
    match member.name {
        "cmp" => method_row(scope, proto, member, cb_idb_cmp),
        "databases" => method_row(scope, proto, member, cb_idb_databases),
        "deleteDatabase" => method_row(scope, proto, member, cb_idb_delete_db),
        "open" => method_row(scope, proto, member, cb_idb_open),
        other => debug_assert!(false, "install_factory: unexpected member {other}"),
    }
}

pub fn install_object_store(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_object_store::{
        cb_store_add, cb_store_auto_inc, cb_store_clear, cb_store_count, cb_store_create_index, cb_store_delete,
        cb_store_delete_index, cb_store_get, cb_store_get_all, cb_store_get_all_keys, cb_store_get_all_records,
        cb_store_get_key, cb_store_index, cb_store_index_names, cb_store_key_path, cb_store_name_get,
        cb_store_open_cursor, cb_store_open_key_cursor, cb_store_put, cb_store_set_name, cb_store_transaction,
    };
    match member.name {
        "name" => pair_row(scope, proto, member, cb_store_name_get, cb_store_set_name),
        "keyPath" => getter_row(scope, proto, member, cb_store_key_path),
        "indexNames" => getter_row(scope, proto, member, cb_store_index_names),
        "transaction" => getter_row(scope, proto, member, cb_store_transaction),
        "autoIncrement" => getter_row(scope, proto, member, cb_store_auto_inc),
        "add" => method_row(scope, proto, member, cb_store_add),
        "clear" => method_row(scope, proto, member, cb_store_clear),
        "count" => method_row(scope, proto, member, cb_store_count),
        "createIndex" => method_row(scope, proto, member, cb_store_create_index),
        "delete" => method_row(scope, proto, member, cb_store_delete),
        "deleteIndex" => method_row(scope, proto, member, cb_store_delete_index),
        "get" => method_row(scope, proto, member, cb_store_get),
        "getAll" => method_row(scope, proto, member, cb_store_get_all),
        "getAllKeys" => method_row(scope, proto, member, cb_store_get_all_keys),
        "getAllRecords" => method_row(scope, proto, member, cb_store_get_all_records),
        "getKey" => method_row(scope, proto, member, cb_store_get_key),
        "index" => method_row(scope, proto, member, cb_store_index),
        "openCursor" => method_row(scope, proto, member, cb_store_open_cursor),
        "openKeyCursor" => method_row(scope, proto, member, cb_store_open_key_cursor),
        "put" => method_row(scope, proto, member, cb_store_put),
        other => debug_assert!(false, "install_object_store: unexpected member {other}"),
    }
}

/// `IDBIndex` shares eight query members with `IDBObjectStore`: the SAME callbacks, because a store
/// handle and an index handle are both query sources (`idb_store_ctx` reads either). The arity guard's
/// interface name comes from the row's label, so `IDBIndex.prototype.get()` names IDBIndex.
pub fn install_index(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_index::{
        cb_index_key_path, cb_index_multi_entry, cb_index_name_get, cb_index_object_store, cb_index_set_name, cb_index_unique,
    };
    use crate::modules::indexeddb::idb_object_store::{
        cb_store_count, cb_store_get, cb_store_get_all, cb_store_get_all_keys, cb_store_get_all_records, cb_store_get_key,
        cb_store_open_cursor, cb_store_open_key_cursor,
    };
    match member.name {
        "name" => pair_row(scope, proto, member, cb_index_name_get, cb_index_set_name),
        "objectStore" => getter_row(scope, proto, member, cb_index_object_store),
        "keyPath" => getter_row(scope, proto, member, cb_index_key_path),
        "multiEntry" => getter_row(scope, proto, member, cb_index_multi_entry),
        "unique" => getter_row(scope, proto, member, cb_index_unique),
        "count" => method_row(scope, proto, member, cb_store_count),
        "get" => method_row(scope, proto, member, cb_store_get),
        "getAll" => method_row(scope, proto, member, cb_store_get_all),
        "getAllKeys" => method_row(scope, proto, member, cb_store_get_all_keys),
        "getAllRecords" => method_row(scope, proto, member, cb_store_get_all_records),
        "getKey" => method_row(scope, proto, member, cb_store_get_key),
        "openCursor" => method_row(scope, proto, member, cb_store_open_cursor),
        "openKeyCursor" => method_row(scope, proto, member, cb_store_open_key_cursor),
        other => debug_assert!(false, "install_index: unexpected member {other}"),
    }
}

pub fn install_cursor(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_cursor::{
        cb_cursor_advance, cb_cursor_continue, cb_cursor_continue_primary_key, cb_cursor_delete, cb_cursor_direction,
        cb_cursor_key, cb_cursor_primary_key, cb_cursor_request, cb_cursor_source, cb_cursor_update,
    };
    match member.name {
        "source" => getter_row(scope, proto, member, cb_cursor_source),
        "direction" => getter_row(scope, proto, member, cb_cursor_direction),
        "key" => getter_row(scope, proto, member, cb_cursor_key),
        "primaryKey" => getter_row(scope, proto, member, cb_cursor_primary_key),
        "request" => getter_row(scope, proto, member, cb_cursor_request),
        "advance" => method_row(scope, proto, member, cb_cursor_advance),
        "continue" => method_row(scope, proto, member, cb_cursor_continue),
        "continuePrimaryKey" => method_row(scope, proto, member, cb_cursor_continue_primary_key),
        "delete" => method_row(scope, proto, member, cb_cursor_delete),
        "update" => method_row(scope, proto, member, cb_cursor_update),
        other => debug_assert!(false, "install_cursor: unexpected member {other}"),
    }
}

pub fn install_cursor_with_value(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    debug_assert_eq!(member.name, "value");
    getter_row(scope, proto, member, crate::modules::indexeddb::idb_cursor::cb_cursor_value);
}

pub fn install_key_range(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::modules::indexeddb::idb_key_range::{
        cb_kr_bound, cb_kr_includes, cb_kr_lower, cb_kr_lower_bound, cb_kr_lower_open, cb_kr_only, cb_kr_upper,
        cb_kr_upper_bound, cb_kr_upper_open,
    };
    match member.name {
        "lower" => getter_row(scope, target, member, cb_kr_lower),
        "upper" => getter_row(scope, target, member, cb_kr_upper),
        "lowerOpen" => getter_row(scope, target, member, cb_kr_lower_open),
        "upperOpen" => getter_row(scope, target, member, cb_kr_upper_open),
        "includes" => method_row(scope, target, member, cb_kr_includes),
        // The four statics land on the interface OBJECT: `put_member` passes it as `target`.
        "bound" => method_row(scope, target, member, cb_kr_bound),
        "lowerBound" => method_row(scope, target, member, cb_kr_lower_bound),
        "only" => method_row(scope, target, member, cb_kr_only),
        "upperBound" => method_row(scope, target, member, cb_kr_upper_bound),
        other => debug_assert!(false, "install_key_range: unexpected member {other}"),
    }
}

// ── the twelve tables ───────────────────────────────────────────────────────────────────────────
//
// `[interface, both]`: eleven of the twelve are non-constructible interface objects (`new IDBRequest()`
// throws "Illegal constructor" -- the bundle's oracle records the TypeError for every one of them), and
// all twelve are in the bundle's WINDOW and WORKER surfaces with byte-identical member lists, so one
// table serves both realms. `ty` is `Object` on every row, verbatim from the registry blocks these
// replace (the capture records descriptors, not IDL types; only `Undefined` would change behaviour, by
// making the arity guard force an undefined return, and no row had it).

webapi::interface! {
    "IDBFactory" [interface, both] {
        ctor   "IDBFactory"                                 len 0 = stub,
        method "cmp"            : Object len 2 required 2         = install_factory,
        method "databases"      : Promise len 0                   = install_factory,
        method "deleteDatabase" : Object len 1 required 1         = install_factory,
        method "open"           : Object len 1 required 1         = install_factory
    }
}

webapi::interface! {
    "IDBRequest" : "EventTarget" [interface, both] {
        ctor    "IDBRequest"                        len 0 = stub,
        attr    "result"      : Object readonly            = install_request,
        attr    "error"       : Object readonly nullable   = install_request,
        attr    "source"      : Object readonly nullable   = install_request,
        attr    "transaction" : Object readonly nullable   = install_request,
        attr    "readyState"  : Object readonly            = install_request,
        handler "onsuccess"                                = install_idb_handler,
        handler "onerror"                                  = install_idb_handler
    }
}

webapi::interface! {
    "IDBOpenDBRequest" : "IDBRequest" [interface, both] {
        ctor    "IDBOpenDBRequest"  len 0 = stub,
        handler "onblocked"               = install_idb_handler,
        handler "onupgradeneeded"         = install_idb_handler
    }
}

webapi::interface! {
    "IDBTransaction" : "EventTarget" [interface, both] {
        ctor    "IDBTransaction"                         len 0 = stub,
        attr    "objectStoreNames" : Object readonly           = install_transaction,
        attr    "mode"             : Object readonly           = install_transaction,
        attr    "durability"       : Object readonly           = install_transaction,
        attr    "db"               : Object readonly           = install_transaction,
        attr    "error"            : Object readonly nullable  = install_transaction,
        handler "onabort"                                      = install_idb_handler,
        handler "oncomplete"                                   = install_idb_handler,
        handler "onerror"                                      = install_idb_handler,
        method  "abort"            : Object len 0              = install_transaction,
        method  "commit"           : Object len 0              = install_transaction,
        method  "objectStore"      : Object len 1 required 1   = install_transaction
    }
}

webapi::interface! {
    "IDBDatabase" : "EventTarget" [interface, both] {
        ctor    "IDBDatabase"                                len 0 = stub,
        attr    "name"              : Object readonly              = install_database,
        attr    "version"           : Object readonly              = install_database,
        attr    "objectStoreNames"  : Object readonly              = install_database,
        handler "onabort"                                          = install_idb_handler,
        handler "onclose"                                          = install_idb_handler,
        handler "onerror"                                          = install_idb_handler,
        handler "onversionchange"                                  = install_idb_handler,
        method  "close"             : Object len 0                 = install_database,
        method  "createObjectStore" : Object len 1 required 1       = install_database,
        method  "deleteObjectStore" : Object len 1 required 1       = install_database,
        method  "transaction"       : Object len 1 required 1       = install_database
    }
}

webapi::interface! {
    "IDBObjectStore" [interface, both] {
        ctor   "IDBObjectStore"                            len 0 = stub,
        attr   "name"          : Object                          = install_object_store,
        attr   "keyPath"       : Object readonly                 = install_object_store,
        attr   "indexNames"    : Object readonly                 = install_object_store,
        attr   "transaction"   : Object readonly                 = install_object_store,
        attr   "autoIncrement" : Object readonly                 = install_object_store,
        method "add"           : Object len 1 required 1          = install_object_store,
        method "clear"         : Object len 0                     = install_object_store,
        method "count"         : Object len 0                     = install_object_store,
        method "createIndex"   : Object len 2 required 2          = install_object_store,
        method "delete"        : Object len 1 required 1          = install_object_store,
        method "deleteIndex"   : Object len 1 required 1          = install_object_store,
        method "get"           : Object len 1 required 1          = install_object_store,
        method "getAll"        : Object len 0                     = install_object_store,
        method "getAllKeys"    : Object len 0                     = install_object_store,
        method "getAllRecords" : Object len 0                     = install_object_store,
        method "getKey"        : Object len 1 required 1          = install_object_store,
        method "index"         : Object len 1 required 1          = install_object_store,
        method "openCursor"    : Object len 0                     = install_object_store,
        method "openKeyCursor" : Object len 0                     = install_object_store,
        method "put"           : Object len 1 required 1          = install_object_store
    }
}

webapi::interface! {
    "IDBIndex" [interface, both] {
        ctor   "IDBIndex"                                 len 0 = stub,
        attr   "name"          : Object                         = install_index,
        attr   "objectStore"   : Object readonly                = install_index,
        attr   "keyPath"       : Object readonly                = install_index,
        attr   "multiEntry"    : Object readonly                = install_index,
        attr   "unique"        : Object readonly                = install_index,
        method "count"         : Object len 0                    = install_index,
        method "get"           : Object len 1 required 1         = install_index,
        method "getAll"        : Object len 0                    = install_index,
        method "getAllKeys"    : Object len 0                    = install_index,
        method "getAllRecords" : Object len 0                    = install_index,
        method "getKey"        : Object len 1 required 1         = install_index,
        method "openCursor"    : Object len 0                    = install_index,
        method "openKeyCursor" : Object len 0                    = install_index
    }
}

webapi::interface! {
    "IDBCursor" [interface, both] {
        ctor   "IDBCursor"                                     len 0 = stub,
        attr   "source"             : Object readonly                = install_cursor,
        attr   "direction"          : Object readonly                = install_cursor,
        attr   "key"                : Object readonly                = install_cursor,
        attr   "primaryKey"         : Object readonly                = install_cursor,
        attr   "request"            : Object readonly                = install_cursor,
        method "advance"            : Object len 1 required 1        = install_cursor,
        method "continue"           : Object len 0                   = install_cursor,
        method "continuePrimaryKey" : Object len 2 required 2        = install_cursor,
        method "delete"             : Object len 0                   = install_cursor,
        method "update"             : Object len 1 required 1        = install_cursor
    }
}

webapi::interface! {
    "IDBCursorWithValue" : "IDBCursor" [interface, both] {
        ctor "IDBCursorWithValue"      len 0 = stub,
        attr "value" : Object readonly       = install_cursor_with_value
    }
}

webapi::interface! {
    // The four statics are `static_method` rows: the installer mounts them on the interface OBJECT.
    // Their WebIDL lengths are the capture's (bound 2, the other three 1) -- the deleted JS glue passed
    // 4/2/2 for three of them, which Chrome contradicts (probed 2026-09-10).
    "IDBKeyRange" [interface, both] {
        ctor          "IDBKeyRange"                            len 0 = stub,
        attr          "lower"      : Object readonly                 = install_key_range,
        attr          "upper"      : Object readonly                 = install_key_range,
        attr          "lowerOpen"  : Object readonly                 = install_key_range,
        attr          "upperOpen"  : Object readonly                 = install_key_range,
        method        "includes"   : Object len 1 required 1         = install_key_range,
        static_method "bound"      : Object len 2 required 2         = install_key_range,
        static_method "lowerBound" : Object len 1 required 1         = install_key_range,
        static_method "only"       : Object len 1 required 1         = install_key_range,
        static_method "upperBound" : Object len 1 required 1         = install_key_range
    }
}

webapi::interface! {
    "IDBRecord" [interface, both] {
        ctor "IDBRecord"                    len 0 = stub,
        attr "key"        : Object readonly       = install_record,
        attr "primaryKey" : Object readonly       = install_record,
        attr "value"      : Object readonly       = install_record
    }
}

webapi::interface! {
    "IDBVersionChangeEvent" : "Event" [value, both] {
        ctor "IDBVersionChangeEvent"              len 1 = stub,
        attr "oldVersion"      : Object readonly          = install_version_change_event,
        attr "newVersion"      : Object readonly nullable = install_version_change_event,
        attr "dataLoss"        : Object readonly          = install_version_change_event,
        attr "dataLossMessage" : Object readonly          = install_version_change_event
    }
}

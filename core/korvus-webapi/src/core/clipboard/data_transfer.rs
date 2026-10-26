//! `core/clipboard/data_transfer.rs` -- the `DataTransfer` / `DataTransferItemList` /
//! `DataTransferItem` interfaces: their cppgc value classes and their three `interface!` tables.
//!
//! WHAT THIS REPLACES. The `DataTransfer` half of `dom/08d-datatransfer.js`: five
//! `Object.defineProperty` sites and four `method()` helper calls over `op_dt_*` / `op_dtil_*` /
//! `op_dti_*` builder round-trips, onto the cppgc CLASS TEMPLATES' own prototypes, plus two
//! `Object.defineProperty(fn, "length", ...)` corrections of op2's arity metadata, plus
//! `globalThis.DataTransfer = DataTransfer` (the raw class) and its two siblings in
//! `dom/00-bootstrap.js`. Every member is a row of the three tables below now, mounted by THE
//! installer (`install::live::install_window_datatransfer`) onto prototypes the installer owns;
//! `clearData` len 0 and `setDragImage` len 3 come from the rows, so the two `length` corrections
//! are gone with the file.
//!
//! The seven `#[op2]` members of `DataTransfer` (`dropEffect`, `effectAllowed`, `types`, `getData`,
//! `setData`, `clearData`, `setDragImage`) were on the class template's prototype, which the table's
//! prototype replaces -- so they are raw callbacks in this file now, like the ten that already were.
//! Nothing a page can name is left on an op2 impl.
//!
//! Instances keep their cppgc identity and gain the right prototype: every mint site
//! ([`dt_make_item`], [`cb_dt_items`], [`cb_dt_files`], [`cb_dt_ctor`]) re-prototypes to the TABLE's
//! prototype, and the cppgc unwrap reads the wrapper tag rather than the prototype chain, so the
//! brand is untouched while `instanceof`, `constructor` and `Object.getPrototypeOf` answer the
//! interface object the page can see.
//!
//! Every body checks its receiver. `DataTransfer.prototype.getData.call(Object.create(
//! DataTransfer.prototype), "x")` throws "Illegal invocation" now, where the op2 members answered
//! `undefined` and the raw callbacks silently did nothing -- which is what headless Chrome
//! 150.0.7871.46 on this machine answers for all three interfaces (probed 2026-09-10).
//!
//! `DataTransferItemList` owns `@@iterator === Array.prototype.values` -- the SAME function object,
//! confirmed against that Chrome -- instead of the hand-built `op_dtil_iter`, deleted with the JS by
//! Which snapshotted the
//! items into a fresh Array and handed back ITS iterator (a different function object every read).

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::core::fileapi::file_list::mint_file_list; // sibling domain (DataTransfer.files builds a FileList)
use crate::binding::protos::{get_prop, str_val, throw_type_error};
use crate::install::live::{getter_row, method_row, pair_row, set_table_proto};
use crate::install::MemberDef;

fn dt_normalize_type(t: &str) -> String {
    match t {
        "text" => "text/plain".to_string(),
        "url" => "text/uri-list".to_string(),
        other => other.to_string(),
    }
}

struct DtItem {
    kind: String, // "string" | "file"
    ty: String,
    str: Option<String>,
    file: Option<v8::Global<v8::Value>>,
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct DataTransfer {
    items: std::cell::RefCell<Vec<DtItem>>,
    drop_effect: std::cell::RefCell<String>,
    effect_allowed: std::cell::RefCell<String>,
    /// How many times the drag data store's item list has changed: Blink's `OnItemListChanged`, which raises
    /// `hasDataStoreItemListChanged` -- the `[CachedAttribute]` predicate of `types` (C2). Every `DataObject`
    /// operation that notifies bumps it: `setData` always (it clears the type, then adds), `clearData` and the
    /// item list's `remove` / `clear` only when an item went, and every `add`.
    list_changes: std::cell::Cell<u32>,
    /// The drag data store's mode (HTML 6.11.2): [`DT_READ_WRITE`] for a constructed one and a `dragstart`'s,
    /// [`DT_READ_ONLY`] for a `drop`'s, [`DT_PROTECTED`] for every other drag event's -- which shows the types and
    /// none of the data, and takes no change.
    mode: std::cell::Cell<u8>,
}

/// [`DataTransfer::mode`]'s values.
pub(crate) const DT_READ_WRITE: u8 = 0;
pub(crate) const DT_READ_ONLY: u8 = 1;
pub(crate) const DT_PROTECTED: u8 = 2;

const DT_DROP: [&str; 4] = ["none", "copy", "link", "move"];
const DT_ALLOWED: [&str; 9] = ["none", "copy", "copyLink", "copyMove", "link", "linkMove", "move", "all", "uninitialized"];

pub(crate) trait DataTransferBehavior {
   fn construct() -> DataTransfer where Self: Sized { stub!("DataTransfer::constructor") }
}

#[webapi(base, own)]
impl DataTransfer {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<DataTransfer, ::deno_error::JsErrorBox> { dispatch!("DataTransfer::constructor", <DataTransfer as DataTransferBehavior>::construct()) }
}

impl DataTransferBehavior for DataTransfer {
    /// `effectAllowed` starts at "none", not "uninitialized". HTML's `new DataTransfer()` says "set
    /// the effectAllowed to 'none'" -- "uninitialized" is the drag data store's value BEFORE a
    /// `dragstart`, which a constructed object never had. The bundle's oracle records "none" for both
    /// of its `effectAllowed` cases, and headless Chrome 150.0.7871.46 on this machine answers "none"
    /// (2026-09-10); this engine answered "uninitialized" and failed both.
    fn construct() -> DataTransfer { DataTransfer {
            items: std::cell::RefCell::new(Vec::new()),
            drop_effect: std::cell::RefCell::new("none".to_string()),
            effect_allowed: std::cell::RefCell::new("none".to_string()),
            list_changes: std::cell::Cell::new(0),
            mode: std::cell::Cell::new(DT_READ_WRITE),
        } }
}

/// The drag data store operations, unchanged from the `DataTransferBehavior` bodies the seven op2
/// members dispatched to -- only their callers moved (an op2 member on a class template that no page
/// can reach any more, to a row body of the table).
impl DataTransfer {
    fn types(&self) -> Vec<String> {
        let items = self.items.borrow();
        let mut t: Vec<String> = items.iter().filter(|x| x.kind == "string").map(|x| x.ty.clone()).collect();
        if items.iter().any(|x| x.kind == "file") { t.push("Files".to_string()); }
        t
    }
    fn get_data(&self, ty: &str) -> String {
        if self.mode.get() == DT_PROTECTED {
            return String::new();
        }
        let t = dt_normalize_type(ty);
        self.items.borrow().iter().find(|x| x.kind == "string" && x.ty == t).and_then(|x| x.str.clone()).unwrap_or_default()
    }
    fn set_data(&self, ty: &str, data: String) {
        if self.mode.get() != DT_READ_WRITE {
            return;
        }
        let t = dt_normalize_type(ty);
        let mut items = self.items.borrow_mut();
        items.retain(|x| !(x.kind == "string" && x.ty == t)); // one string per type
        items.push(DtItem { kind: "string".to_string(), ty: t, str: Some(data), file: None });
        self.item_list_changed();
    }
    fn clear_data(&self, ty: Option<String>) {
        if self.mode.get() != DT_READ_WRITE {
            return;
        }
        let mut items = self.items.borrow_mut();
        let before = items.len();
        match ty {
            None => items.retain(|x| x.kind != "string"),
            Some(t) => { let t = dt_normalize_type(&t); items.retain(|x| !(x.kind == "string" && x.ty == t)); }
        }
        if items.len() != before {
            self.item_list_changed();
        }
    }
    /// Blink's `DataTransfer::OnItemListChanged` (see `list_changes`).
    fn item_list_changed(&self) {
        self.list_changes.set(self.list_changes.get().wrapping_add(1));
    }
}

/// A back-ref to a DataTransfer's items (DataTransferItemList) or its file subset (FileList).
#[repr(C)]
#[derive(CppgcBase)]
pub struct DataTransferItemList {
    dt: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
}

pub(crate) trait DataTransferItemListBehavior {
   fn construct() -> DataTransferItemList where Self: Sized { stub!("DataTransferItemList::constructor") }
}

#[webapi(base, own)]
impl DataTransferItemList {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<DataTransferItemList, ::deno_error::JsErrorBox> { dispatch!("DataTransferItemList::constructor", <DataTransferItemList as DataTransferItemListBehavior>::construct()) }
}

impl DataTransferItemListBehavior for DataTransferItemList {
    fn construct() -> DataTransferItemList { DataTransferItemList { dt: std::cell::RefCell::new(None) } }
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct DataTransferItem {
    kind: String,
    ty: String,
    str: std::cell::RefCell<Option<String>>,
    file: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
}

pub(crate) trait DataTransferItemBehavior {
   fn construct() -> DataTransferItem where Self: Sized { stub!("DataTransferItem::constructor") }
}

#[webapi(base, own)]
impl DataTransferItem {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<DataTransferItem, ::deno_error::JsErrorBox> { dispatch!("DataTransferItem::constructor", <DataTransferItem as DataTransferItemBehavior>::construct()) }
}

impl DataTransferItemBehavior for DataTransferItem {
    fn construct() -> DataTransferItem { DataTransferItem { kind: String::new(), ty: String::new(), str: std::cell::RefCell::new(None), file: std::cell::RefCell::new(None) } }
}

// ── the receiver checks ─────────────────────────────────────────────────────────────────────────

/// The receiver as a `DataTransfer`, or the WebIDL `TypeError` a foreign one gets.
fn dt_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<*const DataTransfer> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<DataTransfer>(scope, this) {
        Some(d) => Some(unsafe { d.as_ref() } as *const DataTransfer),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The receiver as a `DataTransferItem`, or the WebIDL `TypeError`.
fn dti_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<*const DataTransferItem> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<DataTransferItem>(scope, this) {
        Some(i) => Some(unsafe { i.as_ref() } as *const DataTransferItem),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The owning DataTransfer behind a `DataTransferItemList` receiver -- a list that is not one of ours
/// (or whose back-ref is unset, which no mint site produces) gets the WebIDL `TypeError`.
fn dtil_owner<'s>(scope: &mut v8::PinScope<'s, '_>, list_this: v8::Local<v8::Value>) -> Option<*const DataTransfer> {
    let owner = deno_core::cppgc::try_unwrap_cppgc_object::<DataTransferItemList>(scope, list_this)
        .and_then(|list| unsafe { list.as_ref() }.dt.borrow().clone())
        .map(|g| v8::Local::new(scope, &g));
    match owner.and_then(|o| deno_core::cppgc::try_unwrap_cppgc_object::<DataTransfer>(scope, o)) {
        Some(d) => Some(unsafe { d.as_ref() } as *const DataTransfer),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

pub(crate) fn dt_clone_global<'s>(scope: &mut v8::PinScope<'s, '_>, g: &v8::Global<v8::Value>) -> v8::Global<v8::Value> {
    let l = v8::Local::new(scope, g);
    v8::Global::new(scope, l)
}

/// Mint a `DataTransferItem` on the TABLE's prototype.
fn dt_make_item<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str, ty: &str, str: Option<String>, file: Option<v8::Global<v8::Value>>) -> v8::Local<'s, v8::Value> {
    let item = DataTransferItem { kind: kind.to_string(), ty: ty.to_string(), str: std::cell::RefCell::new(str), file: std::cell::RefCell::new(file) };
    let obj = crate::binding::class::mint(scope, item);
    set_table_proto(scope, obj, "DataTransferItem");
    obj.into()
}

/// One own index property of a list instance, with Chrome's descriptor (`enumerable`, `configurable`,
/// NOT writable -- probed 2026-09-10), where `set_index` had made them writable.
fn dt_set_index(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, i: usize, v: v8::Local<v8::Value>) {
    let mut d = v8::PropertyDescriptor::new_from_value_writable(v, false);
    d.set_enumerable(true);
    d.set_configurable(true);
    if let Some(k) = v8::String::new(scope, &i.to_string()) {
        obj.define_property(scope, k.into(), &d);
    }
}

// ── DataTransfer ────────────────────────────────────────────────────────────────────────────────

/// `new DataTransfer()` -- the `ctor` row's body. The class template's `#[constructor]` is
/// unreachable once the table owns the interface object, so the construction lives here: mint the
/// cppgc value, then re-prototype it to `new.target.prototype`, which is the table's prototype for a
/// plain `new` and a subclass's for `class D extends DataTransfer {}`.
pub fn cb_dt_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("DataTransfer"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <DataTransfer as DataTransferBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "DataTransfer"),
    }
    rv.set(obj.into());
}

/// A drag event's `DataTransfer` (`core/events/pointer_input.rs`'s drag): its string items, its mode, and the effects
/// the drag carries, on the table's prototype of the current realm.
pub(crate) fn dt_mint<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[(String, String)], mode: u8, effect_allowed: &str, drop_effect: &str) -> v8::Local<'s, v8::Value> {
    let dt = <DataTransfer as DataTransferBehavior>::construct();
    for (ty, data) in items {
        dt.items.borrow_mut().push(DtItem { kind: "string".to_string(), ty: dt_normalize_type(ty), str: Some(data.clone()), file: None });
    }
    *dt.effect_allowed.borrow_mut() = effect_allowed.to_string();
    *dt.drop_effect.borrow_mut() = drop_effect.to_string();
    dt.mode.set(mode);
    let obj = crate::binding::class::mint(scope, dt);
    set_table_proto(scope, obj, "DataTransfer");
    obj.into()
}

/// What a drag event's `DataTransfer` holds after the page has had it: its string items, `effectAllowed` and
/// `dropEffect`. `None` for anything else.
pub(crate) fn dt_read(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<(Vec<(String, String)>, String, String)> {
    let d = deno_core::cppgc::try_unwrap_cppgc_object::<DataTransfer>(scope, v)?;
    let dt = unsafe { d.as_ref() };
    let items = dt.items.borrow().iter().filter(|x| x.kind == "string").map(|x| (x.ty.clone(), x.str.clone().unwrap_or_default())).collect();
    let allowed = dt.effect_allowed.borrow().clone();
    let effect = dt.drop_effect.borrow().clone();
    Some((items, allowed, effect))
}

fn cb_dt_drop_effect_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let s = unsafe { &*d }.drop_effect.borrow().clone();
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_dt_drop_effect_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let v = args.get(0).to_rust_string_lossy(scope);
    if DT_DROP.contains(&v.as_str()) { *unsafe { &*d }.drop_effect.borrow_mut() = v; }
}
fn cb_dt_effect_allowed_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let s = unsafe { &*d }.effect_allowed.borrow().clone();
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_dt_effect_allowed_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let v = args.get(0).to_rust_string_lossy(scope);
    if DT_ALLOWED.contains(&v.as_str()) { *unsafe { &*d }.effect_allowed.borrow_mut() = v; }
}
/// `[CachedAttribute=hasDataStoreItemListChanged] readonly attribute FrozenArray<DOMString> types`: one frozen
/// array until the item list changes (`install::frozen_array`; Chrome 150 measured: `setData` of a type already
/// there makes a new one, `clearData` of an absent type does not). It was a new, writable array per read (C2).
fn cb_dt_types(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let dt = unsafe { &*d };
    let version = dt.list_changes.get() as f64;
    let answer = crate::install::frozen_array::answer(scope, args.this(), "DataTransfer", "types", |_| version, |scope| {
        Some(dt.types().iter().map(|s| str_val(scope, s)).collect())
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}
fn cb_dt_get_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let ty = args.get(0).to_rust_string_lossy(scope);
    let s = unsafe { &*d }.get_data(&ty);
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_dt_set_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let ty = args.get(0).to_rust_string_lossy(scope);
    let data = args.get(1).to_rust_string_lossy(scope);
    unsafe { &*d }.set_data(&ty, data);
}
fn cb_dt_clear_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(d) = dt_recv(scope, args.this().into()) else { return };
    let ty = if args.length() > 0 && !args.get(0).is_undefined() { Some(args.get(0).to_rust_string_lossy(scope)) } else { None };
    unsafe { &*d }.clear_data(ty);
}
/// `setDragImage(image, x, y)` -- the drag feedback image. Three required arguments (the row's guard
/// supplies the TypeError the bundle's oracle records for a bare call), and no observable effect:
/// there is no drag source and no compositor here to show one.
fn cb_dt_set_drag_image(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _ = dt_recv(scope, args.this().into());
}

// DataTransfer.items -> a fresh DataTransferItemList (back-ref to this DataTransfer + numeric props).
fn cb_dt_items(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(dt) = dt_recv(scope, this.into()) else { return };
    let this_v: v8::Local<v8::Value> = this.into();
    let list = crate::binding::class::mint(scope, DataTransferItemList { dt: std::cell::RefCell::new(Some(v8::Global::new(scope, this_v))) });
    set_table_proto(scope, list, "DataTransferItemList");
    let snapshot: Vec<(String, String, Option<String>, Option<v8::Global<v8::Value>>)> = {
        let src = unsafe { &*dt }.items.borrow();
        src.iter().map(|it| (it.kind.clone(), it.ty.clone(), it.str.clone(), it.file.as_ref().map(|g| dt_clone_global(scope, g)))).collect()
    };
    for (i, (kind, ty, s, f)) in snapshot.into_iter().enumerate() {
        let item = dt_make_item(scope, &kind, &ty, s, f);
        dt_set_index(scope, list, i, item);
    }
    rv.set(list.into());
}

// DataTransfer.files -> a FileList of the current file items.
fn cb_dt_files(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(dt) = dt_recv(scope, args.this().into()) else { return };
    let files: Vec<v8::Global<v8::Value>> = {
        let src = unsafe { &*dt }.items.borrow();
        src.iter().filter(|x| x.kind == "file").filter_map(|x| x.file.as_ref().map(|g| dt_clone_global(scope, g))).collect()
    };
    let fl = mint_file_list(scope, files);
    rv.set(fl.into());
}

/// The body every non-`ctor` row of [`DATATRANSFER`] names.
pub fn install_data_transfer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "dropEffect" => pair_row(scope, proto, member, cb_dt_drop_effect_get, cb_dt_drop_effect_set),
        "effectAllowed" => pair_row(scope, proto, member, cb_dt_effect_allowed_get, cb_dt_effect_allowed_set),
        "items" => getter_row(scope, proto, member, cb_dt_items),
        "types" => getter_row(scope, proto, member, cb_dt_types),
        "files" => getter_row(scope, proto, member, cb_dt_files),
        "clearData" => method_row(scope, proto, member, cb_dt_clear_data),
        "getData" => method_row(scope, proto, member, cb_dt_get_data),
        "setData" => method_row(scope, proto, member, cb_dt_set_data),
        "setDragImage" => method_row(scope, proto, member, cb_dt_set_drag_image),
        other => debug_assert!(false, "install_data_transfer: unexpected member {other}"),
    }
}

// ── DataTransferItemList ────────────────────────────────────────────────────────────────────────

fn cb_dtil_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(dt) = dtil_owner(scope, args.this().into()) else { return };
    let n = unsafe { &*dt }.items.borrow().len() as u32;
    rv.set_uint32(n);
}

fn cb_dtil_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(dt) = dtil_owner(scope, args.this().into()) else { return };
    let data = args.get(0);
    // A `File` of any realm (B27: Chrome adds a frame's File as a file item); the sealed interface, not the
    // page-writable global (`protos::implements`).
    let is_file = crate::binding::protos::implements(scope, data, "File");
    let item = if is_file {
        let ty = v8::Local::<v8::Object>::try_from(data).ok()
            .and_then(|o| get_prop(scope, o, "type")).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let stored = v8::Global::new(scope, data);
        let ret = dt_clone_global(scope, &stored);
        unsafe { &*dt }.items.borrow_mut().push(DtItem { kind: "file".to_string(), ty: ty.clone(), str: None, file: Some(stored) });
        unsafe { &*dt }.item_list_changed();
        dt_make_item(scope, "file", &ty, None, Some(ret))
    } else {
        // WebIDL overload resolution: with ONE argument the only candidate is `add(File data)` -- the string
        // form takes two -- so anything else is refused, as Chrome 150 does (F-cross-realm-arguments).
        if args.length() < 2 {
            throw_type_error(scope, "Failed to execute 'add' on 'DataTransferItemList': parameter 1 is not of type 'File'.");
            return;
        }
        let s = data.to_rust_string_lossy(scope);
        let ty = dt_normalize_type(&args.get(1).to_rust_string_lossy(scope));
        {
            let mut items = unsafe { &*dt }.items.borrow_mut();
            items.retain(|x| !(x.kind == "string" && x.ty == ty));
            items.push(DtItem { kind: "string".to_string(), ty: ty.clone(), str: Some(s.clone()), file: None });
        }
        unsafe { &*dt }.item_list_changed();
        dt_make_item(scope, "string", &ty, Some(s), None)
    };
    rv.set(item);
}

fn cb_dtil_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(dt) = dtil_owner(scope, args.this().into()) else { return };
    let i = args.get(0).number_value(scope).unwrap_or(-1.0) as i64;
    let mut items = unsafe { &*dt }.items.borrow_mut();
    if i >= 0 && (i as usize) < items.len() {
        items.remove(i as usize);
        unsafe { &*dt }.item_list_changed();
    }
}

fn cb_dtil_clear(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(dt) = dtil_owner(scope, args.this().into()) else { return };
    let dt = unsafe { &*dt };
    // `DataObject::ClearAll`: an empty list stays unchanged, and so does `types`
    let had = !dt.items.borrow().is_empty();
    dt.items.borrow_mut().clear();
    if had {
        dt.item_list_changed();
    }
}

/// The body every non-`@@iterator` row of [`DATATRANSFERITEMLIST`] names.
pub fn install_data_transfer_item_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => getter_row(scope, proto, member, cb_dtil_length),
        "add" => method_row(scope, proto, member, cb_dtil_add),
        "clear" => method_row(scope, proto, member, cb_dtil_clear),
        "remove" => method_row(scope, proto, member, cb_dtil_remove),
        other => debug_assert!(false, "install_data_transfer_item_list: unexpected member {other}"),
    }
}

// ── DataTransferItem ────────────────────────────────────────────────────────────────────────────

fn cb_dti_kind(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(it) = dti_recv(scope, args.this().into()) else { return };
    let s = unsafe { &*it }.kind.clone();
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_dti_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(it) = dti_recv(scope, args.this().into()) else { return };
    let s = unsafe { &*it }.ty.clone();
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_dti_get_as_file(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(it) = dti_recv(scope, args.this().into()) else { return };
    let f = unsafe { &*it }.file.borrow().as_ref().map(|g| v8::Local::new(scope, g));
    match f {
        Some(v) => rv.set(v),
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}
/// `webkitGetAsEntry()` -- in the capture, absent here. It returns a FileSystemEntry for an item that
/// came from a real drag of a real file, and NULL for everything else; a string item is always null,
/// which is what Chrome answers for the only kind of item this engine can produce (probed
/// 2026-09-10: `dt.items.add("a","text/a")` then `webkitGetAsEntry()` is null).
///
/// This is null-always ON PURPOSE and it is a KNOWN BOUNDARY, not a stub pretending to be finished:
/// there is no drag source and no FileSystemEntry interface here, so there is no entry that could
/// honestly be returned. When either arrives, this is where it attaches -- and until then, answering
/// null is what the reference browser does for every item we can construct.
fn cb_dti_webkit_get_as_entry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(_) = dti_recv(scope, args.this().into()) else { return };
    rv.set_null();
}
fn cb_dti_get_as_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(it) = dti_recv(scope, args.this().into()) else { return };
    let b = unsafe { &*it };
    let s = if b.kind == "string" { b.str.borrow().clone() } else { None };
    if let (Some(s), Ok(cb)) = (s, v8::Local::<v8::Function>::try_from(args.get(0))) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let sv: v8::Local<v8::Value> = str_val(scope, &s);
        cb.call(scope, undef, &[sv]);
    }
}

/// The body every row of [`DATATRANSFERITEM`] names.
pub fn install_data_transfer_item(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "kind" => getter_row(scope, proto, member, cb_dti_kind),
        "type" => getter_row(scope, proto, member, cb_dti_type),
        "getAsFile" => method_row(scope, proto, member, cb_dti_get_as_file),
        "getAsString" => method_row(scope, proto, member, cb_dti_get_as_string),
        "webkitGetAsEntry" => method_row(scope, proto, member, cb_dti_webkit_get_as_entry),
        other => debug_assert!(false, "install_data_transfer_item: unexpected member {other}"),
    }
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `DataTransfer` / `DataTransferItemList` / `DataTransferItem` blocks of
// `install/generated_registry.rs` (window-only interfaces: neither the worker registry nor the
// bundle's `worker.surface.json` carries any of the three), in the capture's order, with the
// capture's kinds, lengths, `required` counts and descriptor flags. `ty` is `Object` on every row as
// the registry has it (only `Undefined` would change behaviour, by making the arity guard force an
// undefined return).

// `[value, window]`: the capture records `newable: true` and the bundle's oracle
// `new DataTransfer()` -> `[object DataTransfer]`.
webapi::interface! {
    "DataTransfer" [value, window] {
        ctor   "DataTransfer"                                 len 0 = stub,
        attr   "dropEffect"    : Object                               = install_data_transfer,
        attr   "effectAllowed" : Object                               = install_data_transfer,
        attr   "items"         : Object readonly                      = install_data_transfer,
        attr   "types"         : Object readonly                      = install_data_transfer,
        attr   "files"         : Object readonly                      = install_data_transfer,
        method "clearData"     : Object len 0                         = install_data_transfer,
        method "getData"       : Object len 1 required 1              = install_data_transfer,
        method "setData"       : Object len 2 required 2              = install_data_transfer,
        method "setDragImage"  : Object len 3 required 3              = install_data_transfer
    }
}

// `[interface, window]`: `new DataTransferItemList()` throws "Illegal constructor" (the bundle's
// oracle records the TypeError, and it USED to work). `iterable: true` in the capture -- the
// `@@iterator` row.
webapi::interface! {
    "DataTransferItemList" [interface, window] {
        ctor   "DataTransferItemList"                              len 0 = stub,
        attr   "length"     : Object readonly                              = install_data_transfer_item_list,
        method "add"        : Object len 1 required 1 nullable             = install_data_transfer_item_list,
        method "clear"      : Object len 0                                 = install_data_transfer_item_list,
        method "remove"     : Object len 1 required 1                      = install_data_transfer_item_list,
        method "@@iterator" : Object len 0 hidden cfg "values"             = array_proto_fn
    }
}

// `[interface, window]`: `new DataTransferItem()` throws "Illegal constructor" (the bundle's oracle
// records the TypeError, and it USED to work).
webapi::interface! {
    "DataTransferItem" [interface, window] {
        ctor   "DataTransferItem"                            len 0 = stub,
        attr   "kind"             : Object readonly                  = install_data_transfer_item,
        attr   "type"             : Object readonly                  = install_data_transfer_item,
        method "getAsFile"        : Object len 0 nullable            = install_data_transfer_item,
        method "getAsString"      : Object len 1 required 1          = install_data_transfer_item,
        method "webkitGetAsEntry" : Object len 0 nullable            = install_data_transfer_item,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. D2 stubs.
        method "getAsFileSystemHandle" : Promise len 0                = stub
    }
}

// The rule moved to `core/clipboard/clipboard_item.rs::supports`, where the
// `ClipboardItem` table's `supports` row names it directly, and four divergences from Chrome 150 were
// closed on the way (it lowercased, it trimmed, and it rejected every `web `-prefixed CUSTOM format).
// The op was `dom/09-postshells.js`'s only caller and is deleted with it.


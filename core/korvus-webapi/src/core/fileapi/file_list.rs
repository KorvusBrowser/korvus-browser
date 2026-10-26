//! `core/fileapi/file_list.rs` -- the `FileList` interface: its cppgc value class and its
//! `interface!` table.
//!
//! WHAT THIS REPLACES. The `FileList` block of `dom/08d-datatransfer.js`: two
//! `Object.defineProperty` sites (a `length` getter and a `Symbol.iterator`) plus a `method()` helper
//! call, all over `op_fl_*` builder round-trips, onto the cppgc CLASS TEMPLATE's prototype, with
//! `globalThis.FileList = FileList` (the raw class) as the interface object in `dom/00-bootstrap.js`.
//! All three rows are the table's now, mounted by THE installer
//! (`install::live::install_window_datatransfer` / `install_worker_datatransfer`) in BOTH realms onto
//! a prototype the installer owns; [`mint_file_list`] re-prototypes every instance onto it, so the
//! cppgc brand and the page-visible identity belong to one object.
//!
//! `@@iterator` is `Array.prototype.values` -- the SAME function object, not an approximation --
//! because WebIDL gives an interface with an indexed property getter the value iterator. Headless
//! Chrome 150.0.7871.46 on this machine (2026-09-10):
//! `FileList.prototype[Symbol.iterator] === Array.prototype.values` is `true`, the descriptor is
//! `{writable, non-enumerable, configurable}`. So the hand-built `op_fl_iter` (which snapshotted the
//! files into a fresh Array and returned ITS iterator -- a different function object every read) is
//! gone.
//!
//! `new FileList()` used to WORK, because the class template's `#[constructor]` was the interface
//! object; the bundle's oracle records a `TypeError`, which is what the table's non-newable interface
//! object now throws.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::throw_type_error;
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct FileList {
    pub(crate) files: std::cell::RefCell<Vec<v8::Global<v8::Value>>>, // pub(crate): the mint sites fill it.
}

pub(crate) trait FileListBehavior {
   fn construct() -> FileList where Self: Sized { stub!("FileList::constructor") }
}

#[webapi(base, own)]
impl FileList {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<FileList, ::deno_error::JsErrorBox> { dispatch!("FileList::constructor", <FileList as FileListBehavior>::construct()) }
}

impl FileListBehavior for FileList {
    fn construct() -> FileList { FileList { files: std::cell::RefCell::new(Vec::new()) } }
}

/// Mint a `FileList` over `files`: the cppgc value, the TABLE's prototype, and one own index property
/// per file.
///
/// The index descriptor is Chrome's -- `{enumerable, configurable, NOT writable}` (probed
/// 2026-09-10: `Object.getOwnPropertyDescriptor(new DataTransfer().files, "0")` is
/// `e=1 c=1 w=0`) -- where `set_index` had made them writable. `Object.keys()` is exactly the
/// indices, because the file vector lives in Rust, not on a property.
pub(crate) fn mint_file_list<'s>(scope: &mut v8::PinScope<'s, '_>, files: Vec<v8::Global<v8::Value>>) -> v8::Local<'s, v8::Object> {
    let fl = crate::binding::class::mint(scope, <FileList as FileListBehavior>::construct());
    set_table_proto(scope, fl, "FileList");
    for (i, g) in files.iter().enumerate() {
        let l = v8::Local::new(scope, g);
        let mut d = v8::PropertyDescriptor::new_from_value_writable(l, false);
        d.set_enumerable(true);
        d.set_configurable(true);
        if let Some(k) = v8::String::new(scope, &i.to_string()) {
            fl.define_property(scope, k.into(), &d);
        }
    }
    if let Some(h) = deno_core::cppgc::try_unwrap_cppgc_object::<FileList>(scope, fl.into()) {
        *unsafe { h.as_ref() }.files.borrow_mut() = files;
    }
    fl
}

/// The receiver as a `FileList`, or the WebIDL `TypeError` a foreign one gets. `length` used to
/// answer `0` and `item` `null` for any object at all, `FileList.prototype` itself included; Chrome
/// throws "Illegal invocation" for both (probed 2026-09-10).
fn fl_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<*const FileList> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<FileList>(scope, this) {
        Some(f) => Some(unsafe { f.as_ref() } as *const FileList),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn cb_fl_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(f) = fl_recv(scope, args.this().into()) else { return };
    let n = unsafe { &*f }.files.borrow().len() as u32;
    rv.set_uint32(n);
}

fn cb_fl_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(f) = fl_recv(scope, args.this().into()) else { return };
    let i = args.get(0).number_value(scope).unwrap_or(-1.0) as i64;
    let got = {
        let b = unsafe { &*f }.files.borrow();
        if i >= 0 && (i as usize) < b.len() { Some(v8::Local::new(scope, &b[i as usize])) } else { None }
    };
    match got {
        Some(v) => rv.set(v),
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}

/// The body every non-`@@iterator` row of [`FILELIST`] names.
pub fn install_file_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => getter_row(scope, proto, member, cb_fl_length),
        "item" => method_row(scope, proto, member, cb_fl_item),
        other => debug_assert!(false, "install_file_list: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `FileList` block of `install/generated_registry.rs` (the WINDOW one: the
// worker registry's `item` row carried `required: 0`, and Chrome requires the argument in BOTH realms
// -- `new DataTransfer().files.item()` is "Failed to execute 'item' on 'FileList': 1 argument
// required, but only 0 present.", probed 2026-09-10). `[interface, both]`: `new FileList()` throws
// "Illegal constructor" (the bundle's oracle records the TypeError) and both captures list the
// interface with these two members, in this order, plus `iterable: true` -- the `@@iterator` row.
webapi::interface! {
    "FileList" [interface, both] {
        ctor   "FileList"                                         len 0 = stub,
        attr   "length"     : Object readonly                             = install_file_list,
        method "item"       : Object len 1 required 1 nullable            = install_file_list,
        method "@@iterator" : Object len 0 hidden cfg "values"            = array_proto_fn
    }
}


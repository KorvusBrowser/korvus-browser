//! `modules/indexeddb/idb_key_range.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::idb::*;      // IndexedDB backend (Key/KeyRange/…)
use crate::modules::indexeddb::backend::{idb_recv, idb_set_proto, idb_throw, js_to_key, key_to_js};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbKeyRange {
    lower: std::cell::RefCell<Option<Key>>,
    upper: std::cell::RefCell<Option<Key>>,
    lower_open: std::cell::Cell<bool>,
    upper_open: std::cell::Cell<bool>,
}

pub(crate) trait IdbKeyRangeBehavior {
   fn construct() -> IdbKeyRange where Self: Sized { stub!("IdbKeyRange::constructor") }
   fn lower_open(&self) -> bool { stub!("IdbKeyRange::lowerOpen") }
   fn upper_open(&self) -> bool { stub!("IdbKeyRange::upperOpen") }
}

#[webapi(base, own, name = "IDBKeyRange")]
impl IdbKeyRange {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbKeyRange, ::deno_error::JsErrorBox> { dispatch!("IdbKeyRange::constructor", <IdbKeyRange as IdbKeyRangeBehavior>::construct()) }
    #[getter]
    fn lower_open(&self) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("IdbKeyRange::lowerOpen", <IdbKeyRange as IdbKeyRangeBehavior>::lower_open(self)) }
    #[getter]
    fn upper_open(&self) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("IdbKeyRange::upperOpen", <IdbKeyRange as IdbKeyRangeBehavior>::upper_open(self)) }
}

impl IdbKeyRangeBehavior for IdbKeyRange {
    fn construct() -> IdbKeyRange { IdbKeyRange {
            lower: std::cell::RefCell::new(None),
            upper: std::cell::RefCell::new(None),
            lower_open: std::cell::Cell::new(false),
            upper_open: std::cell::Cell::new(false),
        } }
    fn lower_open(&self) -> bool { self.lower_open.get() }
    fn upper_open(&self) -> bool { self.upper_open.get() }
}
impl IdbKeyRange {
    pub(crate) fn to_range(&self) -> KeyRange {
        KeyRange {
            lower: self.lower.borrow().clone(),
            upper: self.upper.borrow().clone(),
            lower_open: self.lower_open.get(),
            upper_open: self.upper_open.get(),
        }
    }
}
fn make_key_range<'s>(scope: &mut v8::PinScope<'s, '_>, r: KeyRange) -> v8::Local<'s, v8::Value> {
    let kr = IdbKeyRange {
        lower: std::cell::RefCell::new(r.lower),
        upper: std::cell::RefCell::new(r.upper),
        lower_open: std::cell::Cell::new(r.lower_open),
        upper_open: std::cell::Cell::new(r.upper_open),
    };
    let obj = crate::binding::class::mint(scope, kr);
    idb_set_proto(scope, obj, "IDBKeyRange");
    obj.into()
}

// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `lowerOpen` / `upperOpen` were `#[op2] #[getter]` members of the class template above until the W5.1
// L7 cutover; an instance now carries the `IDBKeyRange` TABLE's prototype, so these raw callbacks are
// the rows' bodies. The four STATICS are called on the interface object, so they take no receiver check.

pub(crate) fn cb_kr_lower(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let kr = idb_recv!(scope, args.this().into(), IdbKeyRange);
    let k = unsafe { kr.as_ref() }.lower.borrow().clone();
    match k { Some(k) => { let v = key_to_js(scope, &k); rv.set(v); } None => { let u = v8::undefined(scope); rv.set(u.into()); } }
}
pub(crate) fn cb_kr_upper(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let kr = idb_recv!(scope, args.this().into(), IdbKeyRange);
    let k = unsafe { kr.as_ref() }.upper.borrow().clone();
    match k { Some(k) => { let v = key_to_js(scope, &k); rv.set(v); } None => { let u = v8::undefined(scope); rv.set(u.into()); } }
}
pub(crate) fn cb_kr_lower_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let kr = idb_recv!(scope, args.this().into(), IdbKeyRange);
    rv.set_bool(unsafe { kr.as_ref() }.lower_open.get());
}
pub(crate) fn cb_kr_upper_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let kr = idb_recv!(scope, args.this().into(), IdbKeyRange);
    rv.set_bool(unsafe { kr.as_ref() }.upper_open.get());
}
pub(crate) fn cb_kr_includes(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let krh = idb_recv!(scope, args.this().into(), IdbKeyRange);
    let range = unsafe { krh.as_ref() }.to_range();
    match js_to_key(scope, args.get(0)) {
        Some(k) => rv.set_bool(range.includes(&k)),
        None => { idb_throw(scope, "DataError", "invalid key"); }
    }
}
pub(crate) fn cb_kr_bound(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let lo = js_to_key(scope, args.get(0)); let hi = js_to_key(scope, args.get(1));
    let (lo, hi) = match (lo, hi) { (Some(a), Some(b)) => (a, b), _ => { idb_throw(scope, "DataError", "invalid bound"); return } };
    let lower_open = args.get(2).boolean_value(scope); let upper_open = args.get(3).boolean_value(scope);
    match KeyRange::bound(lo, hi, lower_open, upper_open) {
        Ok(r) => { let v = make_key_range(scope, r); rv.set(v); }
        Err(m) => idb_throw(scope, "DataError", m),
    }
}
pub(crate) fn cb_kr_only(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match js_to_key(scope, args.get(0)) { Some(k) => { let v = make_key_range(scope, KeyRange::only(k)); rv.set(v); } None => idb_throw(scope, "DataError", "invalid key") }
}
pub(crate) fn cb_kr_lower_bound(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match js_to_key(scope, args.get(0)) { Some(k) => { let open = args.get(1).boolean_value(scope); let v = make_key_range(scope, KeyRange { lower: Some(k), upper: None, lower_open: open, upper_open: true }); rv.set(v); } None => idb_throw(scope, "DataError", "invalid key") }
}
pub(crate) fn cb_kr_upper_bound(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match js_to_key(scope, args.get(0)) { Some(k) => { let open = args.get(1).boolean_value(scope); let v = make_key_range(scope, KeyRange { lower: None, upper: Some(k), lower_open: true, upper_open: open }); rv.set(v); } None => idb_throw(scope, "DataError", "invalid key") }
}


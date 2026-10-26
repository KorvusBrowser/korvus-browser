//! `modules/indexeddb/idb_request.rs` — extracted from bindings.rs (file-move STEP 1, declarative-macro interface).
//! Pure code move.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::str_val;
use crate::modules::indexeddb::backend::{idb_l, idb_new_request, idb_recv};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbRequest {
    // pub(crate): the moved modules::indexeddb interfaces settle/read this request (IdbRequest stays
    // in bindings.rs -- it uses the idb_req_global_getter! macro, migrated in Phase 1c).
    pub(crate) result: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) error: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) source: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) transaction: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) ready_state: std::cell::RefCell<String>,
}
// ===== IdbRequest (migrated to scaffold) =====
pub(crate) trait IdbRequestBehavior {
   fn construct() -> IdbRequest where Self: Sized { stub!("IdbRequest::constructor") }
}

#[webapi(base, own, name = "IDBRequest")]
impl IdbRequest {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbRequest, ::deno_error::JsErrorBox> { dispatch!("IdbRequest::constructor", <IdbRequest as IdbRequestBehavior>::construct()) }
}

impl IdbRequestBehavior for IdbRequest {
    fn construct() -> IdbRequest { idb_new_request() }
}

// The five `IDBRequest` attribute bodies. Each is a row of the `IDBRequest` table
// (`modules/indexeddb/tables.rs`); the `op_req_*` builders that used to wrap them for
// `dom/08e-indexeddb.js` are gone with that file.
macro_rules! idb_req_global_getter {
    ($cb:ident, $field:ident) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let rh = idb_recv!(scope, args.this().into(), IdbRequest);
            let g = unsafe { rh.as_ref() }.$field.borrow().clone();
            match g { Some(v) => { let l = idb_l(scope, &v); rv.set(l); } None => { let u = if stringify!($field) == "result" { v8::undefined(scope).into() } else { let n: v8::Local<v8::Value> = v8::null(scope).into(); n }; rv.set(u); } }
        }
    };
}
idb_req_global_getter!(cb_req_result, result);
idb_req_global_getter!(cb_req_error, error);
idb_req_global_getter!(cb_req_source, source);
idb_req_global_getter!(cb_req_transaction, transaction);
pub(crate) fn cb_req_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let rh = idb_recv!(scope, args.this().into(), IdbRequest);
    let s = unsafe { rh.as_ref() }.ready_state.borrow().clone();
    let v = str_val(scope, &s);
    rv.set(v);
}


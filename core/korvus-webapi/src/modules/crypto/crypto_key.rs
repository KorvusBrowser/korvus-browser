//! `modules/crypto/crypto_key.rs` -- the `CryptoKey` interface: its cppgc value class and its
//! `interface!` table.
//!
//! WHAT THIS REPLACES. The `CryptoKey` half of `dom/08f-crypto.js`: four `Object.defineProperty`
//! sites that mounted `type`/`algorithm`/`usages` + `Symbol.toStringTag` through `op_ck_*` builder
//! round-trips onto the cppgc CLASS TEMPLATE's prototype, plus a throwing JS wrapper that shadowed
//! the class while SHARING that prototype. Every member is a row of the table below now, mounted by
//! THE installer (`install::live::install_window_crypto_xhr` / `install_worker_crypto_xhr`) in both realms onto a
//! prototype the installer owns; `subtle_crypto::make_crypto_key` re-prototypes each instance it
//! mints onto it, so the cppgc brand and the page-visible identity belong to one object.
//!
//! `extractable` was the class template's `#[op2] #[getter]`; it is a row body here like the other
//! three, so nothing a page can name is left on the op2 impl.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::{str_val, throw_type_error};
use crate::install::live::getter_row;
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct CryptoKey {
    // pub(crate): `subtle_crypto` (same domain) constructs CryptoKey values from key material.
    pub(crate) key_type: String,
    pub(crate) algorithm: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) extractable: std::cell::Cell<bool>,
    pub(crate) usages: std::cell::RefCell<Vec<String>>,
    pub(crate) secret: std::cell::RefCell<Vec<u8>>, // raw key bytes -- never exposed to JS
    pub(crate) hash: std::cell::RefCell<String>,
}

pub(crate) trait CryptoKeyBehavior {
   fn construct() -> CryptoKey where Self: Sized { stub!("CryptoKey::constructor") }
}

#[webapi(base, own)]
impl CryptoKey {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<CryptoKey, ::deno_error::JsErrorBox> { dispatch!("CryptoKey::constructor", <CryptoKey as CryptoKeyBehavior>::construct()) }
}

impl CryptoKeyBehavior for CryptoKey {
    fn construct() -> CryptoKey { CryptoKey { key_type: String::new(), algorithm: std::cell::RefCell::new(None), extractable: std::cell::Cell::new(false), usages: std::cell::RefCell::new(Vec::new()), secret: std::cell::RefCell::new(Vec::new()), hash: std::cell::RefCell::new(String::new()) } }
}

/// The receiver as a `CryptoKey`, or the WebIDL `TypeError` a foreign one gets. The four getters used
/// to answer `undefined` for any object at all, `CryptoKey.prototype` itself included.
fn ck_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<*const CryptoKey> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, this) {
        Some(k) => Some(unsafe { k.as_ref() } as *const CryptoKey),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn cb_ck_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(k) = ck_recv(scope, args.this().into()) else { return };
    let t = unsafe { &*k }.key_type.clone();
    let v = str_val(scope, &t);
    rv.set(v);
}
fn cb_ck_extractable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(k) = ck_recv(scope, args.this().into()) else { return };
    let b = unsafe { &*k }.extractable.get();
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
    rv.set(v);
}
fn cb_ck_algorithm(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(k) = ck_recv(scope, args.this().into()) else { return };
    let g = unsafe { &*k }.algorithm.borrow().clone();
    if let Some(g) = g { let v = v8::Local::new(scope, &g); rv.set(v); }
}
fn cb_ck_usages(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(k) = ck_recv(scope, args.this().into()) else { return };
    let u = unsafe { &*k }.usages.borrow().clone();
    let arr = v8::Array::new(scope, u.len() as i32);
    for (i, s) in u.iter().enumerate() { let sv = str_val(scope, s); arr.set_index(scope, i as u32, sv); }
    rv.set(arr.into());
}

/// The body every row of [`CRYPTOKEY`] names.
pub fn install_crypto_key(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "type" => getter_row(scope, proto, member, cb_ck_type),
        "extractable" => getter_row(scope, proto, member, cb_ck_extractable),
        "algorithm" => getter_row(scope, proto, member, cb_ck_algorithm),
        "usages" => getter_row(scope, proto, member, cb_ck_usages),
        other => debug_assert!(false, "install_crypto_key: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `CryptoKey` blocks of `install/generated_registry.rs` and
// `generated_worker_registry.rs` (which the generator emitted from the bundle's `surface.json` /
// `worker.surface.json`): the same names, order, kinds and descriptor flags in both realms.
// `[interface, both]`: `new CryptoKey()` throws "Illegal constructor" (the bundle's oracle records
// the TypeError) and both captures list the interface with these four members, in this order.
webapi::interface! {
    "CryptoKey" [interface, both, secure] {
        ctor "CryptoKey"                            len 0 = stub,
        attr "type"        : Object readonly              = install_crypto_key,
        attr "extractable" : Object readonly              = install_crypto_key,
        attr "algorithm"   : Object readonly              = install_crypto_key,
        attr "usages"      : Object readonly              = install_crypto_key
    }
}


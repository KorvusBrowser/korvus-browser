//! `modules/crypto/shared.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::modules::indexeddb::backend::{idb_dom_exception};
use crate::binding::protos::set_prop;

/// Read an ArrayBuffer / TypedArray / DataView's bytes; None if `v` is neither.
pub(crate) fn crypto_bytes(_scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if v.is_array_buffer() {
        let ab = v8::Local::<v8::ArrayBuffer>::try_from(v).ok()?;
        let store = ab.get_backing_store();
        Some((0..store.byte_length()).map(|i| store[i].get()).collect())
    } else if v.is_array_buffer_view() {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        Some(buf)
    } else {
        None
    }
}
/// A JS ArrayBuffer owning `bytes` (SubtleCrypto operations resolve with ArrayBuffers).
pub(crate) fn crypto_ab<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &store).into()
}
pub(crate) fn crypto_num<'s>(scope: &mut v8::PinScope<'s, '_>, n: f64) -> v8::Local<'s, v8::Value> { v8::Number::new(scope, n).into() }
pub(crate) fn crypto_obj<'s>(scope: &mut v8::PinScope<'s, '_>, pairs: &[(&str, v8::Local<v8::Value>)]) -> v8::Local<'s, v8::Value> {
    let o = v8::Object::new(scope);
    for (k, v) in pairs { set_prop(scope, o, k, *v); }
    o.into()
}

/// A SubtleCrypto failure -> a rejected promise: a synchronous TypeError, or a DOMException(name).
pub(crate) enum ScErr { Type(String), Dom(String, String) }
/// Settle a SubtleCrypto op into a resolved/rejected Promise.
pub(crate) fn sc_settle<'s>(scope: &mut v8::PinScope<'s, '_>, r: Result<v8::Local<'s, v8::Value>, ScErr>) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    match r {
        Ok(v) => { resolver.resolve(scope, v); }
        Err(e) => {
            let ex = match e {
                // Blink's context applied after the error is made
                ScErr::Type(m) => deno_core::error::native_error(scope, "TypeError", &m),
                ScErr::Dom(n, m) => idb_dom_exception(scope, &n, &m),
            };
            resolver.reject(scope, ex);
        }
    }
    promise.into()
}

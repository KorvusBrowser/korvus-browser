//! `modules/broadcastchannel/serialize.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;

/// A cross-isolate BroadcastChannel message: V8-serialized with the shared codec, so a
/// serializable platform object (DOMPoint, DOMRect, DOMMatrix, DOMException, Blob, ...) arrives as itself.
pub(crate) fn bc_serialize(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Vec<u8> {
    crate::modules::clone_state::wire_serialize(scope, value, &[])
}
pub(crate) fn bc_deserialize<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: &[u8]) -> v8::Local<'s, v8::Value> {
    crate::modules::clone_state::wire_deserialize(scope, bytes, &[])
}

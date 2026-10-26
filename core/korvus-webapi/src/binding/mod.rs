//! `binding` — the DOM engine binding tier, split out of `bindings.rs` (file-move STEP).
//!
//! [`host_dom`] holds the DOM engine infra structs (moved VERBATIM from `bindings.rs`); the
//! `pub(crate)` re-exports below are the crate-internal path to them for `bindings` and the moved
//! domain files. `bindings` keeps re-exporting `HostDom` publicly (the korvus-runtime path).

/// Hiding the engine's plumbing: the `Deno` global and the bridge symbol.
pub mod hidden;
/// The runtime half of an OWN class: a cppgc struct whose bindings `#[webapi(own)]` generated.
pub mod class;
/// A `#[serde]` / `#[object]` result as the WebIDL value Blink makes of it: ordinary objects and arrays of the right
/// realm.
pub mod idl_value;
// Which interface made a platform object, in any realm (the template-carried brand).
pub(crate) mod brand;
// The address memo in front of the engine's per-name V8 keys.
pub(crate) mod keys;
pub mod exposure;
pub(crate) mod host_dom;
// Per-instance state for a hand table's rows, in a v8 private slot.
pub(crate) mod instance_state;
// The two `v8::ObjectTemplate` entry points `rusty_v8` 150.4.0 does not wrap and `document.all`
// cannot be faithful without: `MarkAsUndetectable` and `SetCallAsFunctionHandler`.
pub(crate) mod v8_legacy;
// The four BoringSSL entry points `btls` 0.5.6 does not wrap; see the file header.
pub(crate) mod btls_ffi;

pub(crate) use host_dom::{UnknownElementProto,
    DocumentGlobal, HostDom, MathmlElementProto, MathmlTagProtos, NodeCache, NodeProtos, OwnerMap, QNameMap,
    ShadowRootProto, SvgElementProto, SvgTagProtos, TagProtos, ElementProto,
};
// `protos` is `pub` for ONE reason: it holds THE implementation of a handful of v8 primitives for
// this workspace, and korvus-runtime needs the same one rather than a second copy of it. Four items
// are `pub`: `build_fn`/`build_fn_data` ("make a genuine native function", which the kernel's
// singletons and its page pipeline both mint), and `set_prop`/`call_method`/`get_prop`/`str_val`
// (read and write a property, call a method -- what the kernel's page drivers do to `document` and
// `window` now that they are Rust rather than a JS string). Every other item in the
// module stays `pub(crate)`.
pub mod protos;
/// The native pointers this binary hands V8, for the context snapshot's external references.
pub mod refs;

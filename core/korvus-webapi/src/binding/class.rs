//! `binding::class` -- the runtime half of an OWN class: a native interface whose Rust
//! struct is cppgc-wrapped, and whose bindings are this engine's rather than deno_core's `#[op2]`.
//!
//! `#[webapi(own)]` (`macros/src/lib.rs`) turns an interface `impl` into: the struct's
//! `GarbageCollected` impl; one V8 callback per member, which converts arguments and results through the
//! SAME `deno_core::_ops` helpers the op2 expansion called (so a page sees the same conversions); one
//! install function per JS name, which a table's `cppgc` row names (`interface!`'s `class` header word);
//! and an [`OwnClass`] impl naming the interface. The interface object is then built like any table
//! interface -- `install::live::rows::build_newable` over the class's constructor callback -- so the
//! TemplateBuilder records it, and every realm gets its own from the isolate's template.
//!
//! WHAT deno_core KEEPS DOING: the cppgc heap and the wrapping (`deno_core::cppgc::wrap_object`,
//! `try_unwrap_cppgc_object`), which are the isolate's, not a class registry. What it no longer does for
//! an own class: build a class template, realise it on `Deno.core.ops`, and mint instances from it
//! (`make_cppgc_object`) -- [`mint`] does that here, on the calling realm's prototype.

use deno_core::v8;

/// A cppgc-backed interface class whose bindings `#[webapi(own)]` generated.
pub trait OwnClass: deno_core::GarbageCollected + 'static {
    /// The WebIDL interface a wrapper of this struct is an instance of (`DOMPoint`; `TextEncoder` for
    /// `JsTextEncoder`).
    const IFACE: &'static str;
}

/// A new wrapper of `value`, IN THE CALLING REALM: an API object (what `v8::Object::Wrap` needs) on that
/// realm's `T::IFACE` prototype. What `deno_core::cppgc::make_cppgc_object` did from deno_core's class
/// template, whose prototype was the one realm's that template was realised in.
pub fn mint<'s, T: OwnClass>(scope: &mut v8::PinScope<'s, '_>, value: T) -> v8::Local<'s, v8::Object> {
    // from the interface's template, so V8 brands it as a `T::IFACE` in every realm
    let obj = match crate::binding::protos::shell_proto(scope, T::IFACE) {
        Some(p) => crate::binding::protos::mint_wrapper_on(scope, p.into()),
        None => crate::binding::protos::anon_wrapper_object(scope),
    };
    deno_core::cppgc::wrap_object(scope, obj, value)
}

/// [`mint`] for an instance of `iface`, an interface that shares `T`'s Rust state -- a subclass of
/// `T::IFACE` (`IDBOpenDBRequest` over `IDBRequest`) -- made from `iface`'s own template, so it is that
/// interface's instance from birth rather than `T::IFACE`'s re-branded.
pub fn mint_as<'s, T: OwnClass>(scope: &mut v8::PinScope<'s, '_>, value: T, iface: &str) -> v8::Local<'s, v8::Object> {
    let obj = match crate::binding::protos::shell_proto(scope, iface) {
        Some(p) => crate::binding::protos::mint_wrapper_on(scope, p.into()),
        None => crate::binding::protos::anon_wrapper_object(scope),
    };
    deno_core::cppgc::wrap_object(scope, obj, value)
}

/// A member called on a receiver that is not a wrapper of its class: Chrome's sentence, and every WebIDL
/// binding's. (op2 threw `TypeError: expected <RustType>` with `code: "ERR_INVALID_THIS"`, a Node-ism.)
pub fn illegal_invocation(scope: &mut v8::PinScope) {
    crate::binding::protos::throw_type_error(scope, "Illegal invocation");
}

/// WebIDL's DOMString conversion of `v` (ToString, a `toString` called, its exception propagating), a Symbol refused
/// with Chrome's message, the member's context `prefix` in front. `None` after throwing.
pub fn idl_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, prefix: &str) -> Option<String> {
    if v.is_symbol() {
        crate::binding::protos::throw_type_error(scope, &format!("{prefix}Cannot convert a Symbol value to a string"));
        return None;
    }
    if let Ok(s) = v8::Local::<v8::String>::try_from(v) {
        return Some(s.to_rust_string_lossy(scope));
    }
    Some(v.to_string(scope)?.to_rust_string_lossy(scope))
}

/// WebIDL's `unrestricted double` conversion of `v` (ToNumber, `valueOf`/`toString` called, their exceptions
/// propagating), a Symbol or a BigInt refused with Chrome's message -- the member's context `prefix` in front, as V8's
/// exception propagation callback has Blink put it. `None` after throwing.
pub fn idl_double(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, prefix: &str) -> Option<f64> {
    let refused = if v.is_symbol() {
        "Cannot convert a Symbol value to a number"
    } else if v.is_big_int() {
        "Cannot convert a BigInt value to a number"
    } else {
        return v.number_value(scope);
    };
    crate::binding::protos::throw_type_error(scope, &format!("{prefix}{refused}"));
    None
}

/// An argument conversion op2 refused (`"expected f64"` and its siblings): the same `TypeError`, the same
/// words (`deno_core::_ops::throw_error_one_byte_info`).
pub fn throw_expected(scope: &mut v8::PinScope, what: &str) {
    crate::binding::protos::throw_type_error(scope, what);
}

/// A WebIDL constructor called without `new`.
pub fn throw_use_new(scope: &mut v8::PinScope, iface: &str) {
    let msg = crate::binding::protos::use_new_msg(iface);
    crate::binding::protos::throw_type_error(scope, &msg);
}

/// One method row over a generated callback (the arity guard from the row's `required`).
pub fn mount_method(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    crate::install::live::rows::method_row(scope, holder, m, cb);
}

/// One readonly attribute row over a generated getter.
pub fn mount_getter(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, get: impl v8::MapFnTo<v8::FunctionCallback>) {
    crate::install::live::rows::getter_row(scope, holder, m, get);
}

/// One read/write attribute row over a generated getter and setter.
pub fn mount_pair(
    scope: &mut v8::PinScope,
    holder: v8::Local<v8::Object>,
    m: &'static crate::install::MemberDef,
    get: impl v8::MapFnTo<v8::FunctionCallback>,
    set: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    crate::install::live::rows::pair_row(scope, holder, m, get, set);
}

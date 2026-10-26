//! `binding/instance_state.rs` — per-instance state for a table's rows, in a v8 PRIVATE SLOT.
//!
//! WHY THIS EXISTS. A hand `interface!` table installs one body per row on a shared prototype, so a
//! body that needs per-object state has to find it from the receiver. The JS this goal deletes used
//! module-private `WeakMap`s for exactly that. The Rust equivalent is a `v8::Private` key on the
//! object itself: it is invisible to `Object.keys`, to `getOwnPropertyNames`, to
//! `getOwnPropertySymbols` and to a page walking the object, which is what makes
//! `Object.getOwnPropertyNames(new MediaStream())` answer `[]` the way a browser does — an own
//! property parked on an instance is exactly the instance-tell the quality gate counts.
//!
//! THE SLOT IS ALSO THE BRAND. A member reached on something that is not an instance of its interface
//! — the prototype itself, an `Object.create` of it, a plain object — must answer Chrome's
//! `TypeError: Illegal invocation`, and the absence of the slot is that check. It is strictly tighter
//! than the `instanceof` the deleted JS used in places, because `Object.create(Iface.prototype)` is an
//! `instanceof` of the interface and is NOT an instance of it.
//!
//! ONE KEY PER INTERFACE, named after the interface, so a `MediaStream` body cannot read a
//! `MediaStreamTrack`'s state even though both are objects with a state slot.

use deno_core::v8;

use crate::binding::protos::str_val;

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_state_{iface}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Mint the state object for a freshly constructed instance and hang it off the private slot.
pub(crate) fn attach<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, iface: &str) -> v8::Local<'s, v8::Object> {
    let st = v8::Object::new(scope);
    let k = key(scope, iface);
    this.set_private(scope, k, st.into());
    st
}

/// The receiver's state, or `None` when the receiver is not an instance of `iface`.
pub(crate) fn state<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let k = key(scope, iface);
    let v = obj.get_private(scope, k)?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// True when `v` is an instance of `iface` — the brand check a row runs over an ARGUMENT, where the
/// deleted JS ran `WeakMap.has`.
pub(crate) fn is_a(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str) -> bool {
    state(scope, v, iface).is_some()
}

/// The receiver's state, or a thrown `Illegal invocation` — the shape almost every row wants.
pub(crate) fn state_or_throw<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Value>,
    iface: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    match state(scope, this, iface) {
        Some(s) => Some(s),
        None => {
            crate::binding::protos::throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

// ---- reading and writing one field of a state object ---------------------------------------------

pub(crate) fn get<'s>(scope: &mut v8::PinScope<'s, '_>, st: v8::Local<v8::Object>, name: &str) -> v8::Local<'s, v8::Value> {
    let k = str_val(scope, name);
    st.get(scope, k).unwrap_or_else(|| v8::undefined(scope).into())
}

pub(crate) fn set(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = str_val(scope, name);
    st.set(scope, k, v);
}

pub(crate) fn set_null(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) {
    let v: v8::Local<v8::Value> = v8::null(scope).into();
    set(scope, st, name, v);
}

pub(crate) fn set_str(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, s: &str) {
    let v = str_val(scope, s);
    set(scope, st, name, v);
}

pub(crate) fn set_num(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, n: f64) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    set(scope, st, name, v);
}

pub(crate) fn set_bool(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, b: bool) {
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
    set(scope, st, name, v);
}

pub(crate) fn get_str(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) -> String {
    let v = get(scope, st, name);
    v.to_rust_string_lossy(scope)
}

pub(crate) fn get_bool(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) -> bool {
    let v = get(scope, st, name);
    v.boolean_value(scope)
}

pub(crate) fn get_num(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) -> f64 {
    let v = get(scope, st, name);
    v.number_value(scope).unwrap_or(0.0)
}

pub(crate) fn get_array<'s>(scope: &mut v8::PinScope<'s, '_>, st: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Array>> {
    v8::Local::<v8::Array>::try_from(get(scope, st, name)).ok()
}

// ---- small v8 conveniences the table bodies share -------------------------------------------------

/// A dictionary member: `undefined` and `null` both mean "absent", which is WebIDL's rule for a member
/// with no default.
pub(crate) fn dict_get<'s>(scope: &mut v8::PinScope<'s, '_>, dict: v8::Local<v8::Value>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(dict).ok()?;
    let k = str_val(scope, name);
    obj.get(scope, k).filter(|v| !v.is_null_or_undefined())
}

/// WebIDL `DOMString`: ToString. `None` means the conversion threw (a Symbol) and the throw is already
/// pending on the scope.
pub(crate) fn to_str(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    v.to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

/// WebIDL `unsigned long`: ToNumber, truncate, modulo 2^32.
pub(crate) fn to_ulong(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    let n = v.number_value(scope).unwrap_or(0.0);
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// A fresh JS array over the values given.
///
/// Built by index rather than `Array::new_with_elements`, whose signature ties the ELEMENTS' handle
/// lifetime to the scope's -- which makes every caller that holds a handle from its own arguments
/// (`args.get(0)`, a `FunctionCallbackArguments<'a>`) fail to borrow-check.
pub(crate) fn array<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[v8::Local<v8::Value>]) -> v8::Local<'s, v8::Array> {
    let arr = v8::Array::new(scope, items.len() as i32);
    for (i, v) in items.iter().enumerate() {
        arr.set_index(scope, i as u32, *v);
    }
    arr
}

/// Every element of a JS array, as a Vec.
pub(crate) fn array_items<'s>(scope: &mut v8::PinScope<'s, '_>, arr: v8::Local<v8::Array>) -> Vec<v8::Local<'s, v8::Value>> {
    let n = arr.length();
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        if let Some(v) = arr.get_index(scope, i) {
            out.push(v);
        }
    }
    out
}

/// A v4 UUID, for the ids `MediaStream` and `MediaStreamTrack` mint. The source is the same CSPRNG
/// `crypto.randomUUID()` reads, so two ids never collide and neither is a fixed tell.
pub(crate) fn uuid() -> String {
    let v = crate::crypto::random_bytes(16);
    let mut b = [0u8; 16];
    b.copy_from_slice(&v);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |x: &[u8]| x.iter().map(|c| format!("{c:02x}")).collect::<String>();
    format!("{}-{}-{}-{}-{}", h(&b[0..4]), h(&b[4..6]), h(&b[6..8]), h(&b[8..10]), h(&b[10..16]))
}

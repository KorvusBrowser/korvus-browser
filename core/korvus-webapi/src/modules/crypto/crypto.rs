//! `modules/crypto/crypto.rs` -- the `Crypto` interface and the `crypto` singleton's identity.
//!
//! WHAT THIS REPLACES. `shared-globals.js`'s `globalThis.crypto = { getRandomValues, randomUUID,
//! get [Symbol.toStringTag] }` as the DEFINITION of those two members, and the `rehome(G.crypto,
//! G.Crypto)` call in `dom/09i-singletons.js` that moved them onto `Crypto.prototype` and re-parented
//! the singleton. The object literal stays where it is -- `core/korvus-runtime/src/js` is W4.c's --
//! but nothing a page can reach comes from it any more: this install replaces every member and then
//! strips the singleton bare, which is what the JS re-home did, one layer up.
//!
//! Lane L10 already owned the `subtle` ROW ([`CRYPTO_SUBTLE`]) and already re-parented `crypto` in
//! both realms, with the note that "the other two have JS bodies in `shared-globals.js`, which is
//! W4.c's". This is those other two, so `Crypto` becomes ONE table carrying all three rows and L10's
//! separate `CRYPTO_SUBTLE` mount is deleted with it.
//!
//! THE THREE BODIES, each probed against headless Chrome 150.0.7871.46 on this machine over a
//! loopback server (2026-09-11):
//!
//! ```text
//! crypto.getRandomValues(null)               TypeError: Failed to execute 'getRandomValues' on
//!                                            'Crypto': parameter 1 is not of type 'ArrayBufferView'.
//! crypto.getRandomValues(new Float32Array(4)) DOMException: ... The provided ArrayBufferView is of
//!                                            type 'Float32', which is not an integer array type.
//! Crypto.prototype.randomUUID.call({})       TypeError: Illegal invocation
//! Object.getOwnPropertyNames(crypto)         []
//! ```
//!
//! The float case is new: `shared-globals.js` filled a `Float32Array` with random BYTES, which
//! WebCrypto forbids (the spec throws `TypeMismatchError` for a float view) and which silently handed
//! a page NaNs and denormals where Chrome refuses outright.

use deno_core::v8;

use crate::binding::protos::{build_fn, throw_dom, throw_type_error};
use crate::install::live::parent_of;
use crate::install::MemberDef;

/// Is `this` the `crypto` singleton (or anything that inherits `Crypto.prototype`) -- OF ANY REALM? Anything
/// else throws Chrome's `Illegal invocation`, which is what all three of its members do there. The shared
/// check (`protos::table_receiver_ok`) accepts another realm's `crypto`, as Chrome's V8 signature does: this
/// file's own chain walk compared with the CALLING realm's prototype only, so once every frame was a realm
/// of its own a frame's `getRandomValues` refused the page's `crypto` -- shopee.com.br's
/// security module calls exactly that, and its app never started (B26, F-cross-realm-receivers).
fn recv(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let ok = parent_of(scope, "Crypto").is_some_and(|(proto, _)| crate::binding::protos::table_receiver_ok(scope, this.into(), proto.into()));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// `getRandomValues(array)` -- fill an INTEGER `ArrayBufferView` with cryptographically strong random
/// bytes and return it.
fn cb_get_random_values(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let a = args.get(0);
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(a) else {
        throw_type_error(scope, "Failed to execute 'getRandomValues' on 'Crypto': parameter 1 is not of type 'ArrayBufferView'.");
        return;
    };
    // WebCrypto's "integer array type" list. A DataView is not one either -- Chrome names it the same
    // way -- and the float and clamped kinds are refused rather than filled with bytes.
    let kind = if a.is_int8_array() {
        None
    } else if a.is_uint8_array() {
        None
    } else if a.is_uint8_clamped_array() {
        None
    } else if a.is_int16_array() || a.is_uint16_array() || a.is_int32_array() || a.is_uint32_array() || a.is_big_int64_array() || a.is_big_uint64_array() {
        None
    } else if a.is_float32_array() {
        Some("Float32")
    } else if a.is_float64_array() {
        Some("Float64")
    } else {
        Some("DataView")
    };
    if let Some(name) = kind {
        let msg = format!(
            "Failed to execute 'getRandomValues' on 'Crypto': The provided ArrayBufferView is of type '{name}', which is not an integer array type."
        );
        throw_dom(scope, &msg, "TypeMismatchError");
        return;
    }
    let len = view.byte_length();
    if len > 0 {
        let mut buf = vec![0u8; len];
        let _ = getrandom::getrandom(&mut buf);
        let offset = view.byte_offset();
        let Some(ab) = view.buffer(scope) else { return };
        let store = ab.get_backing_store();
        if let Some(data) = store.data() {
            // The write goes through the VIEW's own offset and length, so `new Uint8Array(buf, 4, 8)`
            // fills only its own window of the buffer.
            // SAFETY: `data` is the buffer's allocation and `offset + len` is inside it by the view's
            // construction; nothing runs between the borrow and the write.
            unsafe {
                let dst = (data.as_ptr() as *mut u8).add(offset);
                std::ptr::copy_nonoverlapping(buf.as_ptr(), dst, len);
            }
        }
    }
    rv.set(a);
}

/// `randomUUID()` -- a v4 UUID string, the same construction the engine's `op_uuid` uses.
fn cb_random_uuid(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    let s = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    );
    let v = crate::binding::protos::str_val(scope, &s);
    rv.set(v);
}

/// `subtle` -- the one `[SameObject]` instance, minted once per realm by L10's
/// [`crate::modules::crypto::subtle_crypto::mint_subtle`] and read back through a branded getter.
fn cb_subtle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    // [SameObject], per realm: the instance `mint_subtle` parked on this realm's global at install
    if let Some(v) = crate::modules::crypto::subtle_crypto::realm_subtle(scope) {
        rv.set(v);
    }
}

// The receiver check, the arity check and the body in ONE callback, in that order -- the order the two
// wrappers this replaces applied them in (receiver, then `make_arity_guard`'s count). The wrappers
// carried the inner function as `data`, which a V8 template cannot hold.
fn cb_crypto_get_random_values(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    if !crate::binding::protos::require_args(scope, &args, 1, "Crypto", "getRandomValues") {
        return;
    }
    cb_get_random_values(scope, args, rv);
}

fn cb_crypto_random_uuid(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    cb_random_uuid(scope, args, rv);
}

/// The body every row of [`CRYPTO`] names.
pub fn install_crypto(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getRandomValues" | "randomUUID" => {
            // the callbacks spell the row's arity themselves; a row that changes it must change them
            debug_assert_eq!(member.required, if member.name == "getRandomValues" { 1 } else { 0 });
            let f = if member.name == "getRandomValues" {
                build_fn(scope, cb_crypto_get_random_values, member.name, member.length as i32)
            } else {
                build_fn(scope, cb_crypto_random_uuid, member.name, member.length as i32)
            };
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        "subtle" => {
            // minted at install as before (the global private is its per-realm home), read per call
            let _ = crate::modules::crypto::subtle_crypto::mint_subtle(scope);
            let g = build_fn(scope, cb_subtle, "get subtle", 0);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        other => debug_assert!(false, "install_crypto: unexpected member {other}"),
    }
}

// `CRYPTO_MEMBERS` WAS HERE: the capture's member order for `Crypto`, written down
// because the installer had no way to read it. `install::installer::join` reads it off the ACTIVE
// capture now, in whichever realm is installing, so the list is derived rather than copied
// and a bundle that orders these three differently is followed rather than overridden.

/// Build `Crypto` from its table and make the running `crypto` an instance of it: strip the object
/// literal `shared-globals.js` built down to nothing, then re-parent it.
///
/// A real singleton OWNS NOTHING -- `Object.getOwnPropertyNames(crypto)` is `[]` in Chrome, probed --
/// so the own properties go, INCLUDING the literal's `Symbol.toStringTag` getter, which would
/// otherwise shadow the prototype's tag with a JS function a page can print.
pub fn install_crypto_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, window_realm: bool) {
    // No selector: `installer::join` reads `Crypto`'s three member names off the ACTIVE capture,
    // in whichever realm is installing. `CRYPTO_MEMBERS` used to be handed in here (see above).
    use crate::install::installer::{self, Flavor};
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &CRYPTO, "Crypto", None, None, Flavor::Table, &mut ledger);
    installer::put_ledger(scope, ledger);
    let (proto, gaps) = (built.proto, built.gaps);
    for g in &gaps {
        eprintln!("[korvus] crypto install gap: {}.{}", g.interface, g.member);
    }
    // The singleton is MINTED here. `shared-globals.js` used to build an object literal of three
    // members that this very code then deleted (`strip_own` runs before the re-parent), so the literal
    // was dead code kept alive only by the object's identity. An OWN one already on the global is
    // still adopted, so an embedder that provides one keeps working.
    let Some(k) = v8::String::new(scope, "crypto") else { return };
    let existing = global
        .get_own_property_descriptor(scope, k.into())
        .filter(|d| !d.is_undefined())
        .and_then(|_| global.get(scope, k.into()))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let c = match existing {
        Some(c) => c,
        // from the interface's template
        None => crate::binding::protos::mint_on(scope, proto.into()),
    };
    crate::install::live::strip_own(scope, c);
    crate::binding::protos::adopt_as(scope, c, proto.into());
    place_crypto(scope, global, k, c, window_realm);
}

/// Put the one `crypto` object where THIS REALM keeps it.
///
/// In a WORKER, `crypto` is a `WorkerGlobalScope` ROW — an inherited native getter, never an own
/// property of the global — so this realm does not define the member at all: it PUBLISHES the object
/// and the table's row reads it (`core/workers/worker_global_scope.rs`). That inverts what
/// this function used to do, which was to define the accessor on `WorkerGlobalScope.prototype`
/// itself, over the top of the registry shell's stub: two writers for one member, which is exactly
/// what invariant I2 forbids and what the install ledger counts.
///
/// THE FAILURE THAT PAID FOR THIS COMMENT, because it is invisible from the code:
/// `global.set("crypto", obj)` in a worker SILENTLY DOES NOTHING. `WorkerGlobalScope.prototype`
/// carries a getter-only `crypto` accessor, and an ordinary Set walks the prototype chain, finds an
/// accessor with no setter, and returns false without writing. The whole worker crypto suite failed
/// with `crypto` undefined and no error anywhere — three tests, one of them a fixture with six cases.
/// Publishing to a private slot cannot fail that way; there is no chain to walk.
fn place_crypto<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::String>,
    c: v8::Local<'s, v8::Object>,
    window_realm: bool,
) {
    if !window_realm {
        crate::core::workers::worker_global_scope::publish_scope_singleton(scope, "crypto", c.into());
        // ...and NO own copy on the global, which is what makes `getOwnPropertyNames(globalThis)`
        // match a real worker.
        global.delete(scope, key.into());
        return;
    }
    // THE WINDOW: an own ACCESSOR, as Chrome's is. This was an own DATA property -- what
    // `globalThis.crypto = {…}` produced -- and `Object.getOwnPropertyDescriptor(window, "crypto")` is
    // a standard bot check. Measured on stock Chrome 150.0.7871.46 over a loopback page:
    //
    // ```text
    // korvus  own data      writable=true                   enumerable=true configurable=true
    // chrome  own accessor  get="get crypto"/0 set=undefined enumerable=true configurable=true
    // ```
    //
    // The object lives in the global's slot for the name and the getter hands back that one object
    // on every read, behind the Window receiver check -- the one window-singleton install every such
    // global goes through (`core/html/window_globals.rs`).
    let _ = key;
    crate::core::html::window_globals::define_window_singleton(scope, global, "crypto", c.into(), crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::Plain);
}

// Rows VERBATIM from the `Crypto` block of `install/generated_registry.rs`, which the worker registry
// matches member for member. `[singleton, both]`: one table, both realms, the same three rows.
webapi::interface! {
    "Crypto" [singleton, both] {
        ctor   "Crypto"                                 len 0 = stub,
        method "getRandomValues" : Any len 1 required 1       = install_crypto,
        attr   "subtle"          : Any readonly               = install_crypto,
        method "randomUUID"      : Any len 0                  = install_crypto
    }
}

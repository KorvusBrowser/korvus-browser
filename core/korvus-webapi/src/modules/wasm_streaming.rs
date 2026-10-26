//! `WebAssembly.instantiateStreaming` / `compileStreaming` as a two-row table.
//!
//! # Why these exist at all
//!
//! `deno_core` only wires V8's WASM streaming host callback when it is embedded with WASM support
//! configured; ours is not, so the default callback is left unset and V8 invoking it **panics the
//! process** — "wasm streaming callback invoked before the JS handler was set", an abort rather than a
//! catchable error. One `WebAssembly.instantiateStreaming(fetch(…))` on a page (vitejs.dev, any
//! esbuild-wasm site) therefore took the whole load down.
//!
//! **In EITHER realm.** The callback is unset per `JsRuntime`, and a worker isolate is one, so the
//! abort was never a window-realm property — only the one call site was. See
//! [`install_wasm_streaming_rows`].
//!
//! The streaming forms are pure sugar for "fetch the bytes, then instantiate/compile", and the
//! non-streaming operations are plain V8 with no host callback — so this is the same result by a road
//! that cannot reach the unset callback. A `Response` is awaited for its `arrayBuffer()`; a bare
//! `BufferSource`, or a promise of one, is accepted too, which is what pages actually pass.
//!
//! # What changed
//!
//! The bodies were two `async function` expressions inside the kernel's `BOOTSTRAP` string, assigned
//! onto `WebAssembly` and then handed to `KB.__markNative` — the `[native code]` masking sweep, which
//! invariant I3 deletes. These are `build_fn` natives, installed from an `interface!` table by the one
//! installer, so `String(WebAssembly.instantiateStreaming)` reads `[native code]` by construction and
//! no mask is involved.
//!
//! The chain is built with V8's own promise combinators rather than an `async` Rust body: the member
//! must answer a promise SYNCHRONOUSLY (pages do `WebAssembly.instantiateStreaming(fetch(u)).then(…)`
//! on the same tick), and `Promise.resolve(source).then(bytesOf).then(finish)` is exactly what the
//! deleted `await` desugared to. An error anywhere — a rejected `source`, a throwing `arrayBuffer()`,
//! an invalid module — rejects the returned promise, which is what `await` did too.
//!
//! # The shape, measured
//!
//! `google-chrome --headless=new` 141, 2026-09-12:
//!
//! ```text
//! Object.getOwnPropertyDescriptor(WebAssembly, "instantiateStreaming")
//!   {enumerable: true, configurable: true, writable: true}, value.length 1,
//!   value.name "instantiateStreaming"    (compileStreaming identical)
//! ```
//!
//! — which is the `interface!` default for a method row, so the rows carry no flag overrides. Both
//! keys already exist on V8's `WebAssembly` object, and `define_property` on an existing key does not
//! MOVE it, so their position among `Object.getOwnPropertyNames(WebAssembly)` is V8's own.
//!
//! Neither row declares `required`: a Promise-returning operation turns an argument error into a
//! REJECTION, not a synchronous throw, so an arity guard would be the wrong shape.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, call_method, get_prop};
use crate::install::{installer, MemberDef};

/// `source` -> its bytes: a `Response` (anything with a callable `arrayBuffer`) is read, anything
/// else is passed straight through as a `BufferSource`.
fn cb_bytes_of(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let src = args.get(0);
    if let Ok(o) = v8::Local::<v8::Object>::try_from(src) {
        if get_prop(scope, o, "arrayBuffer").is_some_and(|v| v.is_function()) {
            // A throw inside `arrayBuffer()` leaves the exception pending; leaving `rv` untouched
            // propagates it, and the promise this runs inside turns it into a rejection.
            if let Some(v) = call_method(scope, src, "arrayBuffer", &[]) {
                rv.set(v);
            }
            return;
        }
    }
    rv.set(src);
}

/// The last link: call the non-streaming operation captured at install time.
/// `data` is `[operation, WebAssembly, importObject]`.
fn cb_finish(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(op) = data.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let Some(recv) = data.get_index(scope, 1) else { return };
    let Some(import_object) = data.get_index(scope, 2) else { return };
    let bytes = args.get(0);
    if let Some(v) = op.call(scope, recv, &[bytes, import_object]) {
        rv.set(v);
    }
}

/// The realm map ([`crate::binding::protos::realm_map`]) the two streaming forms keep what they delegate to
/// in: `[operation, WebAssembly]` under the operation's name, bound at install time.
const BOUND: &str = "korvus_wasm_streaming";

/// Both rows' body. `data` is the operation's name, whose `[operation, WebAssembly]` the function's realm
/// keeps ([`BOUND`]) -- the operation is bound at INSTALL time, so a page that later replaces
/// `WebAssembly.instantiate` does not redirect the streaming form, which is what
/// `WA.instantiate.bind(WA)` bought the deleted version.
fn cb_streaming(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let op_name = crate::binding::protos::body_data(scope, &args);
    let map = crate::binding::protos::realm_map(scope, BOUND);
    let Some(bound) = map.get(scope, op_name).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let Some(op) = bound.get_index(scope, 0) else { return };
    let Some(wa) = bound.get_index(scope, 1) else { return };
    let finish_data = v8::Array::new(scope, 3);
    finish_data.set_index(scope, 0, op);
    finish_data.set_index(scope, 1, wa);
    finish_data.set_index(scope, 2, args.get(1));
    let finish = build_fn_data(scope, cb_finish, "", 1, finish_data.into());
    let bytes_of = build_fn(scope, cb_bytes_of, "", 1);
    let Some(p0) = crate::binding::protos::call_intrinsic(scope, "Promise.resolve", &[args.get(0)]) else { return };
    let Some(p1) = call_method(scope, p0, "then", &[bytes_of.into()]) else { return };
    let Some(p2) = call_method(scope, p1, "then", &[finish.into()]) else { return };
    rv.set(p2);
}

/// The body both rows of [`WEBASSEMBLY`] name. The non-streaming operation each one delegates to is
/// read off the live `WebAssembly` object HERE, once, and carried in the function's data.
pub fn install_wasm_streaming(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    let op_name = match member.name {
        "instantiateStreaming" => "instantiate",
        "compileStreaming" => "compile",
        other => {
            debug_assert!(false, "install_wasm_streaming: unexpected member {other}");
            return;
        }
    };
    let Some(op) = get_prop(scope, target, op_name).filter(|v| v.is_function()) else {
        // No `WebAssembly.instantiate` in this build: installing a streaming form that cannot
        // delegate would be worse than leaving V8's own (panicking) one alone, which is what the
        // deleted glue's early `return` did too.
        return;
    };
    let bound = v8::Array::new(scope, 2);
    bound.set_index(scope, 0, op);
    bound.set_index(scope, 1, target.into());
    let map = crate::binding::protos::realm_map(scope, BOUND);
    let key = crate::binding::protos::str_val(scope, op_name);
    map.set(scope, key, bound.into());
    // From its template: what it delegates to is the realm's, so it is kept by the realm.
    let f = crate::binding::protos::template_fn(scope, cb_streaming, member.name, member.length as i32, Some(op_name));
    crate::install::kits::define_value(scope, target, member, f.into(), true);
}

/// The two rows, onto V8's own `WebAssembly` object — IN WHICHEVER REALM THIS RUNS.
///
/// `WebAssembly` is a JS/WASM-spec namespace, not a Web interface: the capture lists it in `globals`
/// and has no `interfaces` entry for it, so there is nothing for `create_interface` to build and
/// nothing in either generated registry to collide with. The members go on through
/// [`installer::install_members`] — the same `put_member` every other table's rows take, recorded in
/// the same install ledger — which is the shape `install::live::css` uses for the `CSS` namespace.
///
/// # ONE BODY, TWO CALL SITES
///
/// It was named `install_window_wasm_streaming` and called from `engine.rs` alone, which left the
/// WORKER realm on V8's own streaming path — the one deno_core leaves without a host callback. A
/// worker that reached it took the WHOLE PROCESS down with `SIGABRT`, exactly as the window did
/// before this module existed; measured on a corpus row whose wasm runtime instantiates in a worker.
///
/// The reason the fix is one line and not a second module is visible in the signature this function
/// used to carry: it took a `global` it never read, and found `WebAssembly` through `global_get` off
/// whatever context it was invoked in. The body was realm-neutral all along and only its NAME and its
/// one caller were not. So the name lost the realm, the unused parameter went, and `worker.rs` calls
/// the same function — no worker-realm copy of `cb_streaming` exists to drift from this one.
pub fn install_wasm_streaming_rows(scope: &mut v8::PinScope) {
    let Some(wa) = crate::binding::protos::intrinsic(scope, "WebAssembly").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return;
    };
    let names: Vec<&str> = WEBASSEMBLY.members.iter().map(|m| m.name).collect();
    for g in installer::install_members(scope, wa, &WEBASSEMBLY, &names) {
        eprintln!("[korvus] WebAssembly install gap: {}.{}", g.interface, g.member);
    }
}

webapi::interface! {
    // `tag none`: V8's `WebAssembly` already carries its own `Symbol.toStringTag` ("WebAssembly",
    // measured), so this table must not define a second one.
    // `both`, and that is a measurement rather than a widening: Chrome 150 exposes both members in
    // the WORKER realm with the descriptor, length, name and `[native code]` the window realm has --
    // all 15 shape fields byte-identical across its two realms (measured 2026-09-20 with
    // one probe text evaluated in both). The word was `window` only because the one call site was.
    "WebAssembly" [singleton, both, tag none] {
        static_method "compileStreaming"     : Promise len 1 = install_wasm_streaming,
        static_method "instantiateStreaming" : Promise len 1 = install_wasm_streaming
    }
}

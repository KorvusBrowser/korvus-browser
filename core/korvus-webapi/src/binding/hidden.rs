//! Hiding the engine's own plumbing from the page: the `Deno` global, the `__bootstrap` holder and
//! the bridge symbol.
//!
//! All three were the tail of the kernel's `BOOTSTRAP` string literal. Two of them are simple deletes
//! and belong in Rust because invariant I9 says the kernel's strings define nothing; the third is a
//! pair of ES intrinsics this engine replaces, and its story is longer.
//!
//! # `Deno` and `__bootstrap`
//!
//! `typeof Deno` and `Deno.core.ops` are a one-shot deno_core tell, and `__bootstrap` is an init
//! artifact left on the global that no real page has. Both go, AFTER the two things the engine still
//! needs out of them are stashed where a page cannot look:
//!
//! * `Deno.core.ops`, on the `korvus_ops` V8 PRIVATE of the global, because native bodies reach ops
//!   through it — `core/events/report.rs` forwards a worker error that way, and so does XHR. The
//!   stash is [`hide_deno`]'s own line and the read is [`ops_object`]; `op_stash_ops` did the stash
//!   from `BOOTSTRAP` until it was deleted with the JS strip.
//! `Deno.core.evalContext` was stashed here too, as `__evalScript`, because page scripts must run as
//! CLASSIC SCRIPTS and the page drivers were JS. The drivers are Rust now and they compile a
//! `v8::Script` themselves, so neither `__evalScript` nor `__ops` is put on the bridge any more --
//! only the private slot, which is not a property.
//!
//! # The bridge symbol: the REWRITE is gone, because the thing it hid is gone
//!
//! The page realm used to carry a holder at `globalThis[Symbol.for("korvus.bridge")]`, so
//! `Object.getOwnPropertySymbols(window)` and `Reflect.ownKeys(window)` would both have listed
//! `Symbol(korvus.bridge)` — a one-line detect, and the loudest tell this engine can emit. This file
//! answered that by REPLACING both intrinsics with functions that filtered that one symbol out of the
//! global's answer and left every other object alone. Successive versions of this header said the
//! filter would go "when the bridge does", and named what was keeping it: at the end, four slots
//! written from Rust by `bridge_set` — `__curScript`, `__loadFired`, `__bodyHandlerAttr` and a dead
//! read of `__workerScripts`.
//!
//! ALL FOUR MOVED AND THE BLOCK IS DELETED. `__curScript` is a V8 Private, `__loadFired` is a
//! field of `OpState`, `__bodyHandlerAttr` had had no reader since W5.1 L19, and `__workerScripts`
//! was never on the holder to begin with. With `bridge_get`/`bridge_set` deleted, nothing in the
//! window realm mints a holder, so there is no symbol on the global — and two intrinsics that are
//! V8's own beat two that merely report `[native code]` honestly. `no_mask_live`'s
//! `the_bridge_symbol_is_filtered_by_native_functions` kept every one of its assertions across the
//! deletion, which is the evidence: what used to require a filter is what the real intrinsics say
//! once the global has nothing to find.
//!
//! # What is left, and where it is counted
//!
//! The WORKER realm still has a holder. `korvus-runtime/src/worker_recv.rs` mints its own and mounts
//! two native functions on it (three in a ServiceWorker), because their callers are the boot scripts
//! `worker.rs` injects — which run after the `Deno` strip and so have no op table to reach. Those
//! callers are three of the nine declared rows of `tests/fixtures/js_strings.manifest`, and the slots
//! themselves are declared in `korvus-runtime/tests/fixtures/bridge_slots.manifest` and asserted,
//! per realm and in both directions, by `korvus-runtime/tests/bridge_slots.rs`.
//!
//! THAT TEST EXISTS BECAUSE THE RATCHET CANNOT DO IT. `freeze.sh`'s `kb_slots` greps `KB.__x =` out of
//! `.js` files and kernel Rust *string literals*, so a `bridge_set(scope, "__x", …)` call read as
//! nothing at all; it sat at 0, honestly, while four slots lived under it. A grep over source can
//! always be spelled around. The holder's own key list, read out of a running realm, cannot.

use deno_core::v8;

use crate::binding::protos::{get_prop, global_get, str_val};

/// The `Deno.core.ops` object, wherever it currently is: the hidden Private slot [`hide_deno`]
/// stashed it in, or the live `Deno` global before that ran. The READER lives beside the WRITER on
/// purpose -- a caller that hard-codes one of the two answers works in one realm or one phase and
/// not the other, which is exactly how `Headers` came to exist in the page realm and not in a worker.
pub(crate) fn ops_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    if let Some(pname) = v8::String::new(scope, "korvus_ops") {
        let key = v8::Private::for_api(scope, Some(pname));
        if let Some(v) = global.get_private(scope, key) {
            if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                return Some(o);
            }
        }
    }
    let deno = global_get(scope, "Deno").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let core = get_prop(scope, deno, "core").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, core, "ops").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

// `native_class` / `prime_realm_classes` / `realm_class` and the class-thunk registry WERE HERE: the route to
// a deno_core `objects = [...]` class template off `Deno.core.ops`, per realm.
// Every class is an own class built from its table (`binding::class`), so nothing registers one.

/// Hand a new realm's global the same `korvus_ops` stash the creating realm's has: a frame
/// realm never had `Deno`, and its install and its native bodies reach ops through [`ops_object`] on
/// ITS OWN global. The ops are the page realm's functions either way -- there is one op table.
///
/// And the realm's captured `Deno.core.unrefOpPromise`: a frame's parked op -- a worker's receive,
/// an ICE server's STUN transaction -- must not hold the page's event loop open any more than the page's do.
pub(crate) fn share_ops(scope: &mut v8::PinScope, from: v8::Local<v8::Object>, to: v8::Local<v8::Object>) {
    for name in ["korvus_ops", "korvus_unref_op_promise"] {
        let Some(pname) = v8::String::new(scope, name) else { return };
        let key = v8::Private::for_api(scope, Some(pname));
        if let Some(v) = from.get_private(scope, key).filter(|v| v.is_object()) {
            to.set_private(scope, key, v);
        }
    }
}

/// Stash what the engine needs out of `Deno`, then remove `Deno` and `__bootstrap` from the page's
/// global. Called once, right after the bootstrap script.
pub fn hide_deno(scope: &mut v8::PinScope) {
    let global = { let ctx = scope.get_current_context(); ctx.global(scope) };
    let core = global_get(scope, "Deno")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|d| get_prop(scope, d, "core"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    // THE `korvus_bridge` PRIVATE STASH IS GONE. It copied the bridge holder onto a
    // private so that native code could still find it after `Deno` went, and the only two readers it
    // ever had were `binding::protos`'s `bridge_get`/`bridge_set`, which are deleted. The WORKER
    // realm's holder -- the one that still has slots -- is reached by `worker_recv.rs`'s own
    // `bridge_holder`, which looks the symbol up directly and never used this.
    if let Some(core) = core {
        if let Some(ops) = get_prop(scope, core, "ops").filter(|v| v.is_object()) {
            if let Some(pname) = v8::String::new(scope, "korvus_ops") {
                let key = v8::Private::for_api(scope, Some(pname));
                global.set_private(scope, key, ops);
            }
        }
        // `__ops` AND `__evalScript` ARE NOT PUT ON THE BRIDGE ANY MORE. The whole op
        // table used to be readable off the holder, and `Deno.core.evalContext` beside it, for one
        // reason: the page pipeline was five JS strings that had to reach both from inside the page's
        // realm. That pipeline is Rust (`core/korvus-runtime/src/{navigation,script_exec}.rs`) --
        // it calls `webapi::page` directly and compiles a classic script with a `v8::Script` of its
        // own -- so the holder no longer carries either, and an engine-internal call is no longer a
        // property lookup on an object a page can reach if it ever finds the symbol.
        //
        // The PRIVATE `korvus_ops` slot above stays: `core/events/report.rs` and XHR forward through
        // it, and a V8 private is not a property.
    }
    for name in ["Deno", "__bootstrap"] {
        let k = str_val(scope, name);
        // Kept on a private, so a snapshot of the realm can put it back ([`reseat_deno`])
        if let (Some(v), Some(pk)) = (global.get(scope, k.into()).filter(|v| !v.is_undefined()), hidden_key(scope, name)) {
            global.set_private(scope, pk, v);
        }
        global.delete(scope, k);
    }
}

/// The private [`hide_deno`] keeps a hidden global on.
fn hidden_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Private>> {
    let n = v8::String::new(scope, &format!("korvus.hidden.{name}"))?;
    Some(v8::Private::for_api(scope, Some(n)))
}

/// The context snapshot (slice 3): put back what [`hide_deno`] took off the global, in the realm a snapshot is
/// made of. deno reads `Deno.core`'s callbacks off the global on EVERY `JsRuntime::new`, one made from a snapshot
/// included (`store_js_callbacks`), so the snapshot must carry `Deno`; the restore hides it again before any page
/// script runs.
#[cfg(not(feature = "analysis"))]
pub(crate) fn reseat_deno(scope: &mut v8::PinScope) {
    let global = {
        let ctx = scope.get_current_context();
        ctx.global(scope)
    };
    for name in ["Deno", "__bootstrap"] {
        let Some(pk) = hidden_key(scope, name) else { continue };
        let Some(v) = global.get_private(scope, pk).filter(|v| !v.is_undefined()) else { continue };
        let k = str_val(scope, name);
        global.set(scope, k.into(), v);
    }
}

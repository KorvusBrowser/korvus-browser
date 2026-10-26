//! `Navigator` (window) and `WorkerNavigator` (worker) -- WHERE the one table mounts, in each realm.
//!
//! The table, the persona-backed scalar getters and every member body are
//! [`crate::core::navigator`]; this file is the domain's entry in the live-install index
//! ([`super`]), and it is also the public face of the module for `korvus-runtime`, which calls one of
//! these two functions per realm and reads the two selectors in its gates.
//!
//! WHEN IT RUNS, and why that is part of the domain:
//!
//! * WINDOW -- from the runtime's post-`BOOTSTRAP` block, AFTER the whole ESM layer. The device
//!   singletons are minted on the prototypes `js/generated_shells.js` creates, so the interfaces must
//!   already exist; `install_window_plugins` (the four `PluginArray` interfaces) runs just BEFORE, so
//!   that the `plugins` / `mimeTypes` rows can build their family from real prototypes.
//! * WORKER -- from `worker.rs`'s `spawn_on_thread`, after the worker's ESM layer (so
//!   `worker-host.js` has created the `navigator` instance and `generated_worker_shells.js` the
//!   device interfaces) and after the parent persona has been seeded into the worker's `OpState`.
//!   Only in a REAL DOM worker: a bare worker has no persona and no navigator, exactly as before.

use deno_core::v8;

use crate::install::installer::{self, Flavor};

/// The table, re-exported here so `korvus-runtime` and its `navigator_shape` / `navigator_live`
/// gates have ONE path to the domain. THE TWO REALM SELECTORS ARE GONE: the member
/// list of each realm is the ACTIVE capture's, read by `install::installer::join` under the name
/// that realm exposes, so there is nothing left here for a gate to compare against but the bundle.
pub use crate::core::navigator::navigator::NAVIGATOR;

/// Build the window `Navigator` prototype from the table and make the running `navigator` from its
/// template ([`navigator_from_template`]). [`install_singleton`] also mints the constructor-less `Navigator` interface
/// object (throws on `new`, `[native code]`), sets it on the global, and wires
/// `prototype.constructor` + `Symbol.toStringTag`. Drift and gaps are logged -- they would be a
/// table/capture mismatch, never fatal to a page load.
pub fn install_window_navigator<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    mount(scope, global, "Navigator", "korvus");
}

/// The mirror in the other realm: the SAME `NAVIGATOR` table, with the worker selector (the 23-member
/// subset) and the exposed name `WorkerNavigator`. One table, two selectors -- invariant I8.
pub fn install_worker_navigator<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    mount(scope, global, "WorkerNavigator", "korvus-worker");
    // The audit below must be the LAST thing this realm does, so everything that SHAPES the realm
    // runs before it — the scope tables (`install_worker_global_scope`, which reparents the global
    // and mounts the `navigator` ROW) among them.
    // THE WORKER REALM'S AUDIT SLOT. The window realm reads its install ledger back
    // against the live surface from `installer::finalize_surface`, which the worker global has no
    // counterpart to: `worker.rs` assembles its realm from the ESM layer plus this one Rust call,
    // and this call is the LAST of them -- after `worker-host.js` and
    // `generated_worker_shells.js` have run, which is the same position `finalize_surface` occupies
    // on the window side. So the audit runs from here, which is also the only place in this crate
    // that knows the worker realm has finished being assembled. See
    // `installer::audit_surface`; `core/korvus-runtime/tests/install_ledger.rs` is what reads it.
    if crate::install::installer::install_audit_enabled() {
        crate::install::installer::audit_surface(scope, global);
    }
}

fn mount<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    iface: &str,
    realm: &str,
) {
    // STRAIGHT THROUGH THE ONE CREATE, WITH NO SELECTOR. This used to be
    // `installer::install_singleton(.., WINDOW_NAVIGATOR_MEMBERS)` / `(.., WORKER_NAVIGATOR_MEMBERS)`:
    // two hand-written copies of one capture's member lists, which pinned both realms to that one
    // device. `installer::join` reads each realm's list off the ACTIVE capture under the name that
    // realm exposes (`Navigator` here, `WorkerNavigator` there), so the table serves both realms and
    // every bundle. `install_singleton` keeps its selector argument for its remaining caller,
    // `korvus-runtime`'s `History`, which this unit does not own.
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &NAVIGATOR, iface, None, None, Flavor::Table, &mut ledger);
    installer::put_ledger(scope, ledger);
    let (proto, gaps) = (built.proto, built.gaps);
    for g in &gaps {
        eprintln!("[{realm}] navigator install gap: {}.{}", g.interface, g.member);
    }
    navigator_from_template(scope, global, proto);
}

/// The realm's `navigator`, made from the interface's template.
///
/// The realm minted one before this interface existed -- the window's registry walk, a worker's bootstrap
/// -- and all `binding::class::mint` could make then was the nameless wrapper, which this adopted onto the
/// table's prototype. Nothing has read it (the page's scripts run after the install), so it is REPLACED
/// where the realm keeps it: the window's `navigator` slot and `clientInformation`'s (the same object), a
/// worker's scope singleton (the `WorkerGlobalScope` row's). A `navigator` some template already made is
/// only re-pointed, as before.
fn navigator_from_template<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, proto: v8::Local<'s, v8::Object>) {
    let Some(key) = v8::String::new(scope, "navigator") else { return };
    let Some(old) = global.get(scope, key.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    if crate::binding::brand::brand_of(scope, old.into()).is_some() {
        crate::binding::protos::adopt_as(scope, old, proto.into());
        return;
    }
    let obj = crate::binding::protos::mint_wrapper_on(scope, proto.into());
    let fresh = deno_core::cppgc::wrap_object(scope, obj, crate::core::navigator::navigator::JsNavigator);
    let (ov, fv): (v8::Local<v8::Value>, v8::Local<v8::Value>) = (old.into(), fresh.into());
    for name in ["navigator", "clientInformation"] {
        if crate::core::html::window_globals::window_slot_get(scope, name) == Some(ov) {
            crate::core::html::window_globals::window_slot_set(scope, name, fv);
        }
    }
    if crate::core::workers::worker_global_scope::scope_singleton(scope, "navigator") == ov {
        crate::core::workers::worker_global_scope::publish_scope_singleton(scope, "navigator", fv);
    }
    // wherever else it was kept, it would still answer: that is a place this function does not know
    if global.get(scope, key.into()) != Some(fv) {
        eprintln!("[korvus] navigator: the old object is still reachable -- a place `navigator_from_template` does not replace");
        crate::binding::protos::adopt_as(scope, old, proto.into());
    }
}

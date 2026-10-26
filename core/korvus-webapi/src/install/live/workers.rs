//! The WORKER FAMILY: `Worker` (both realms) and `SharedWorker` (the window's).
//!
//! WHERE THIS RUNS, and why. In the WINDOW it is a statement of the runtime's PRE-BOOTSTRAP block,
//! and two things pin it there:
//!
//!   * `Deno.core.unrefOpPromise` has to be captured while the `Deno` global still exists —
//!     `BOOTSTRAP` deletes it. `host.js` captured the same function at ESM-evaluation time for the
//!     same reason; the install is the next thing that runs after the ESM layer, so this is the same
//!     position.
//!   * the table's parent is `EventTarget`, which exists from the ESM layer (`events.js`) and whose
//!     prototype the `EventTarget` table takes over later — the splice is by OBJECT, so a later
//!     overwrite of that same prototype object is invisible here.
//!
//! Nothing decorates `Worker.prototype` after this: `host.js`'s two `Object.defineProperty` mounts of
//! `postMessage`/`terminate` were the only other writer and they are this table's rows now, the name
//! leaves BOTH generated registries with the table (so the registry create/backfill/order passes skip
//! it), and `BOOTSTRAP` names it nowhere (grepped).

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer;

/// The WINDOW's `Worker` and `SharedWorker`.
pub fn install_window_workers<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::workers::shared_worker::{cb_shared_worker_ctor, SHAREDWORKER};
    use crate::core::workers::worker::{capture_unref, cb_worker_ctor, WORKER};
    capture_unref(scope);
    let mut ledger = installer::take_ledger(scope);
    build_newable(scope, global, &WORKER, cb_worker_ctor, Some("EventTarget"), None, &mut ledger);
    // `SharedWorker` is WINDOW-ONLY: `worker.surface.json` does not list it.
    build_newable(scope, global, &SHAREDWORKER, cb_shared_worker_ctor, Some("EventTarget"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The WINDOW's three ServiceWorker interfaces.
///
/// Non-constructible interfaces, so `create_interface` builds both the interface object and the
/// prototype — nothing else creates them, because the three names left the generated registry with
/// these tables. `ServiceWorker` and `ServiceWorkerRegistration` are installed BEFORE
/// `ServiceWorkerContainer` only for readability; none of the three references another's prototype at
/// install time (the container mints its `ServiceWorker`s at `register()` time, through
/// `shell_proto`, which resolves then — from the realm's SEALED prototype map, and
/// from the live global before this install block's seal runs).
///
/// WHERE THIS RUNS: the pre-bootstrap block, beside [`install_window_workers`]. The prototypes have
/// to exist before the first read of `navigator.serviceWorker`, which is a lazy row and so can be as
/// early as the page's first statement. `host.js` built the same three prototypes' members on that
/// first read instead (`prepSwProtos`), which is why it had to check whether the shells existed yet;
/// installed here they always do.
pub fn install_window_service_workers<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::workers::service_worker::{SERVICEWORKER, SERVICEWORKERCONTAINER, SERVICEWORKERREGISTRATION};
    use crate::install::installer::Flavor;
    // All three inherit `EventTarget` in both captures (`proto: ["EventTarget"]`), so the WebIDL
    // splice needs the parent by OBJECT -- `create_interface` does not read `def.parent` itself.
    let et = super::rows::parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    for (def, name) in [
        (&SERVICEWORKER, "ServiceWorker"),
        (&SERVICEWORKERREGISTRATION, "ServiceWorkerRegistration"),
        (&SERVICEWORKERCONTAINER, "ServiceWorkerContainer"),
    ] {
        let built = installer::create_interface(scope, global, def, name, et, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] service-worker install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

/// The WORKER isolate's `ServiceWorkerRegistration` — the same table, filtered to the rows this realm
/// exposes.
///
/// BOTH captures list the interface, so taking it out of the generated registry without this call
/// would delete it from the worker surface. `create_interface_in` installs only the rows whose
/// `exposed` admits `Worker`, which drops exactly one: `cookies`, the window-only member that needed
/// the macro's per-member exposure word.
pub fn install_worker_service_workers<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::workers::service_worker::SERVICEWORKERREGISTRATION;
    use crate::install::installer::Flavor;
    use crate::install::Exposed;
    let global = scope.get_current_context().global(scope);
    let et = super::rows::parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    installer::create_interface_in(
        scope,
        global,
        &SERVICEWORKERREGISTRATION,
        "ServiceWorkerRegistration",
        et,
        None,
        Flavor::Table,
        Exposed::Worker,
        &mut ledger,
    );
    installer::put_ledger(scope, ledger);
}

/// The WORKER isolate's `Worker` — the same table, from `worker.rs`.
///
/// Both captures list `Worker`, so the interface exists in this realm too; none of the spawn ops does,
/// because a nested worker is not implemented. The constructor brands and returns, and the two method
/// rows land as D2 stubs — the shape the worker registry's shell gave, now counted as stubs instead of
/// being indistinguishable from an implementation.
pub fn install_worker_workers<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::workers::worker::{cb_worker_ctor, WORKER};
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    build_newable(scope, global, &WORKER, cb_worker_ctor, Some("EventTarget"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

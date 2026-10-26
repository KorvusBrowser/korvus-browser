//! `URL` and `URLSearchParams`.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use super::rows::{build_newable, parent_of};
use crate::install::installer;

/// `URLSearchParams` (and, once the second half of the lane lands, `URL`) from their `interface!`
/// tables, in the WINDOW.
///
/// `URLSearchParams` was a cppgc class assigned onto the global by `dom/00-bootstrap.js` with its
/// fourteen members mounted by `dom/08g-observers.js` over fourteen `op_usp_*` builder round-trips,
/// three `Object.defineProperty` sites and the `KB.__uspBind` bridge slot. All of that is deleted;
/// [`crate::modules::url::url_search_params::URLSEARCHPARAMS`] is its only source.
///
/// WHY THIS RUNS AT CONTEXT CREATION, before the bootstrap ESM layer: it must be the LAST writer of
/// the prototype, and `dom/00-bootstrap.js` (which used to assign the class onto the global) runs
/// after. The assignment is deleted in the same commit, so nothing later touches the name -- grepped
/// across every remaining `.js` file, `BOOTSTRAP` and `host.js` for `URLSearchParams` and for each of
/// the fourteen member names.
pub fn install_window_url<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    install_url_interfaces(scope, global, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The SAME tables in the WORKER realm (invariant I8), called from `core/korvus-runtime/src/worker.rs`
/// beside [`super::install_worker_observers`].
///
/// `worker.surface.json` lists `URLSearchParams` with a member list byte-identical to the window's.
/// Before this call the worker's `URLSearchParams` was the generated registry's D2 stub -- measured:
/// `new URLSearchParams("b=2&a=1")` answered `undefined` for every read -- so this is the first realm
/// in which a worker's params object actually works.
pub fn install_worker_url<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    install_url_interfaces(scope, global, &mut ledger);
    installer::put_ledger(scope, ledger);
}

fn install_url_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::modules::url::url::{cb_url_ctor, URL};
    use crate::modules::url::url_search_params::{cb_usp_ctor, install_usp_iterator, URLSEARCHPARAMS};
    build_newable(scope, global, &URLSEARCHPARAMS, cb_usp_ctor, None, None, ledger);
    // The one member no row can carry: see `install_usp_iterator`.
    if let Some((proto, _)) = parent_of(scope, "URLSearchParams") {
        install_usp_iterator(scope, proto);
    }
    // `URL` AFTER `URLSearchParams`: `URL::searchParams` mints onto the params table's prototype
    // (`set_table_proto` reads `globalThis.URLSearchParams.prototype`), so that has to be the table's
    // by the time a page can call the getter. Nothing here reads it during the install itself, but the
    // order makes the dependency visible rather than incidental.
    build_newable(scope, global, &URL, cb_url_ctor, None, None, ledger);
}

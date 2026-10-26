//! The Performance / Intersection / Resize observer interfaces.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Flavor};

// `bind_po_bridge` WAS HERE: it bound `__poTypes` / `__poCb` on this realm's bridge so that the JS
// performance timeline (`host.js` / `worker-host.js`) could read an observer's types and callback out
// of a native function. The timeline itself is in Rust
// (`core/timing/performance.rs`), so the notification loop reaches the observer directly and both
// bridge names are deleted -- with `__perfObs`, the slot the observer LIST used to live on.

/// The six observer interfaces from their `interface!` tables, in the WINDOW.
///
/// `PerformanceObserver` + `PerformanceObserverEntryList` (`core/timing/performance_observer.rs`),
/// `IntersectionObserver` + `IntersectionObserverEntry` (`core/intersection_observer/`),
/// `ResizeObserver` + `ResizeObserverEntry` (`core/resize_observer/`) -- 33 rows that were the
/// PerformanceObserver / IntersectionObserver / ResizeObserver blocks of `dom/08g-observers.js` plus
/// the two `globalThis.X = <cppgc class>` lines of `dom/00-bootstrap.js`: three native masks (two
/// `op_nativize_ctor`, one `op_nativize`), four `KB.__` bridge slots and eleven `#[op2] impl` members
/// on two cppgc class templates. This is their only source.
///
/// Runs at context creation BESIDE [`super::install_window_streams`], BEFORE the bootstrap script -- the
/// position the deleted JS ran from, and the LAST writer of all six prototypes (grepped: nothing in
/// `BOOTSTRAP`, `host.js` or a later JS phase defines or decorates a member of the six; `host.js`'s
/// `enumerateProto` sweep only makes a non-enumerable configurable key enumerable, and every row here
/// is already enumerable).
pub fn install_window_observers<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::intersection_observer::intersection_observer::{cb_io_ctor, INTERSECTIONOBSERVER, INTERSECTIONOBSERVERENTRY};
    use crate::core::resize_observer::resize_observer::{cb_ro_ctor, RESIZEOBSERVER, RESIZEOBSERVERENTRY};
    let mut ledger = installer::take_ledger(scope);
    install_performance_observers(scope, global, "korvus", &mut ledger);
    for def in [&INTERSECTIONOBSERVERENTRY, &RESIZEOBSERVERENTRY] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] observers install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &INTERSECTIONOBSERVER, cb_io_ctor, None, None, &mut ledger);
    build_newable(scope, global, &RESIZEOBSERVER, cb_ro_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The `[Exposed=(Window,Worker)]` half of the lane in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_streams`].
///
/// `worker.surface.json` lists exactly two of the six -- `PerformanceObserver` and
/// `PerformanceObserverEntryList` -- with member lists byte-identical to the window's, and both left
/// the generated worker registry with the tables, so this is their only source in this realm. It is
/// the SAME table and the SAME native constructor, and it replaces `worker-host.js`'s duplicate
/// installer outright.
pub fn install_worker_observers<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    install_performance_observers(scope, global, "korvus-worker", &mut ledger);
    installer::put_ledger(scope, ledger);
}

fn install_performance_observers<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str, ledger: &mut installer::InstallLedger) {
    use crate::core::timing::performance_observer::{cb_po_ctor, PERFORMANCEOBSERVER, PERFORMANCEOBSERVERENTRYLIST};
    let built = installer::create_interface(scope, global, &PERFORMANCEOBSERVERENTRYLIST, "PerformanceObserverEntryList", None, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[{realm}] observers install gap: {}.{}", g.interface, g.member);
    }
    build_newable(scope, global, &PERFORMANCEOBSERVER, cb_po_ctor, None, None, ledger);
}

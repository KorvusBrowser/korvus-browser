//! The performance TIMELINE: `Performance`, the entry family, Navigation Timing L1 and
//! `EventCounts`, from their `interface!` tables, in both realms.

use deno_core::v8;

use super::rows::{build_newable, rechain_orphans};
// (`build_fn_data` and `get_prop` left with the worker accessor this file used to define.)
use crate::core::timing::performance::{
    cb_performance_mark_ctor, mint_performance, EVENTCOUNTS, PERFORMANCE, PERFORMANCEENTRY, PERFORMANCEMARK,
    PERFORMANCEMEASURE, PERFORMANCENAVIGATION, PERFORMANCETIMING,
};
use crate::install::installer::{self, Flavor};

/// The seven timeline interfaces and the `performance` singleton in the WINDOW.
///
/// Called from `korvus-runtime/src/lib.rs` at context creation, in the slot
/// `singletons::install_window_singletons` occupied — AFTER the whole ESM layer, for the reason that
/// module's header gave: `generated_shells.js` rebuilds every interface's prototype chain when it
/// runs, so anything done before it is silently undone. It replaces
/// `singletons::rehome_performance` outright: there is no JS object literal left to re-home, the
/// eighteen rows ARE the prototype, and the singleton is minted here wearing it.
///
/// All seven left the generated registry with the tables (the `interface!` inventory is name-based,
/// so `scripts/sync.sh` drops them from `generated_registry.rs` and, for the three
/// `[Exposed=(Window,Worker)]` ones, from `generated_worker_registry.rs` too) — so this is their only
/// source in this realm.
///
/// ORDER INSIDE THIS FUNCTION IS THE WEBIDL CHAIN: `PerformanceEntry` first, because
/// `PerformanceMark` and `PerformanceMeasure` splice onto its prototype; `Performance` onto
/// `EventTarget`'s. `rechain_orphans` last, because `PerformanceEntry`'s registry CHILDREN
/// (`PerformanceNavigationTiming`, `PerformanceResourceTiming`, `PerformancePaintTiming`, …) were
/// chained by the ESM-layer pass onto whatever `PerformanceEntry.prototype` was then, and this
/// install replaces it.
pub fn install_window_performance<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    install_family(scope, global, "korvus", &mut ledger);
    // The three `[Exposed=Window]` partials' interfaces.
    for def in [&PERFORMANCETIMING, &PERFORMANCENAVIGATION, &EVENTCOUNTS] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] performance install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
    put_singleton(scope, global, true);
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
}

/// The three `[Exposed=(Window,Worker)]` interfaces the WORKER capture lists — `Performance` (its
/// thirteen rows), `PerformanceEntry`, `PerformanceMark`, `PerformanceMeasure` — from the SAME tables
/// the window uses (invariant I8), and the worker's own timeline.
///
/// Called from `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_observers`]. This
/// realm gets its OWN `Performance` instance: `worker.performance !== page.performance` is spec, and
/// `tests/fixtures/an agent-browser reference/performance-worker.js` asserts it. `timing`, `navigation`,
/// `eventCounts`, `memory` and `interactionCount` are `[Exposed=Window]` partials the worker capture
/// does not list, so the selector leaves them off and the same fixture's
/// `window_only_partials_hidden` case sees `'eventCounts' in performance === false`.
pub fn install_worker_performance<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    // NO HAND SELECTOR ANY MORE. The realm's member list used to be written down
    // here, copied out of one capture; `install::installer::join` reads it off the ACTIVE
    // capture instead, so the list is derived and a bundle swap moves it.
    install_family(scope, global, "korvus-worker", &mut ledger);
    installer::put_ledger(scope, ledger);
    put_singleton(scope, global, false);
    rechain_orphans(scope, global, crate::install::generated_registry::WORKER_ORDER);
}

/// `PerformanceEntry` → `PerformanceMark` / `PerformanceMeasure` → `Performance`, in chain order.
fn install_family<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    realm: &str,
    ledger: &mut installer::InstallLedger,
) {
    let report = |realm: &str, gaps: &[installer::Gap]| {
        for g in gaps {
            eprintln!("[{realm}] performance install gap: {}.{}", g.interface, g.member);
        }
    };
    let entry = installer::create_interface(scope, global, &PERFORMANCEENTRY, "PerformanceEntry", None, None, Flavor::Table, ledger);
    report(realm, &entry.gaps);
    let entry_pair = Some((entry.proto, entry.ctor));
    // `PerformanceMark` is the one CONSTRUCTIBLE entry type (`new PerformanceMark("x")` is in the
    // bundle's oracle), so it is a `build_newable` over the table's own constructor body rather than
    // a `create_interface` "Illegal constructor".
    build_newable(scope, global, &PERFORMANCEMARK, cb_performance_mark_ctor, Some("PerformanceEntry"), None, ledger);
    let measure = installer::create_interface(scope, global, &PERFORMANCEMEASURE, "PerformanceMeasure", entry_pair, None, Flavor::Table, ledger);
    report(realm, &measure.gaps);
    // `PerformanceResourceTiming` was created by the registry walk at its capture position
    // (`install::live::ORDERED_HAND`); it is spliced onto the `PerformanceEntry` just built.
    let _ = realm;
    crate::core::timing::resource_timing::chain_to_performance_entry(scope, global);
    let et = super::rows::parent_of(scope, "EventTarget");
    let perf = installer::create_interface(scope, global, &PERFORMANCE, "Performance", et, None, Flavor::Table, ledger);
    report(realm, &perf.gaps);
}

/// Mint this realm's timeline and put it where the realm keeps `performance`.
///
/// WINDOW: an own property of the global, which is where `globalThis.performance = …` in `host.js`
/// put it and what the surface snapshot records, so the cutover does not move the name.
///
/// WORKER: a native getter on `WorkerGlobalScope.prototype`, because a real worker global has no own
/// `performance` (the surface-parity scoreboard diffs `Object.getOwnPropertyNames(globalThis)`).
/// `worker-finalize.js` used to do exactly this, with `op_nativize` over a JS arrow closing over the
/// object `worker-host.js` had built; it captured that object during the ESM layer, which is BEFORE
/// this install runs, so the capture-then-define moves here with the object it was capturing.
fn put_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, window_realm: bool) {
    let perf = mint_performance(scope, window_realm);
    if window_realm {
        // An own `[Replaceable]` ACCESSOR on the window, as in every browser of the family:
        // `performance = 1` replaces it with a data property.
        crate::core::html::window_globals::define_window_singleton(scope, global, "performance", perf.into(), crate::core::html::window_globals::WindowSetter::Replaceable, true, crate::core::html::window_globals::Mint::With(mint_window_performance));
        return;
    }
    // `performance` is a `WorkerGlobalScope` ROW, so this install does not define the
    // member -- it PUBLISHES the timeline and the table's row reads it. This used to define an
    // accessor on `WorkerGlobalScope.prototype` over the top of the registry shell's stub, which is
    // two writers for one member (invariant I2) and made the install ORDER load-bearing: the row is
    // mounted whenever the scope table installs, and it answers this object whenever it is asked.
    crate::core::workers::worker_global_scope::publish_scope_singleton(scope, "performance", perf.into());
    // Nothing of ours is left own on the global; an own data property would shadow the inherited
    // accessor and BE the tell. Assert it rather than assume it.
    debug_assert!(
        v8::String::new(scope, "performance")
            .and_then(|k| global.get_own_property_descriptor(scope, k.into()))
            .map(|v| v.is_undefined())
            .unwrap_or(true),
        "the worker global has an own `performance` -- it would shadow WorkerGlobalScope.prototype's"
    );
}

/// This WINDOW realm's `performance`.
fn mint_window_performance<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    Some(mint_performance(scope, true).into())
}

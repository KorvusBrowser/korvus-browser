//! The twelve IndexedDB interfaces (`modules/indexeddb/tables.rs`) and the `indexedDB` global, in
//! both realms from the one set of tables.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// The eleven non-constructible IndexedDB interfaces, from their `interface!` tables
/// (`modules/indexeddb/tables.rs`), in WebIDL inheritance order so each child can be spliced onto its
/// parent's prototype AND constructor. `IDBVersionChangeEvent` -- the twelfth, and the only one with a
/// real constructor -- is built by its own installer afterwards.
///
/// Shared by both realms: all twelve are in the bundle's window AND worker surfaces with byte-identical
/// member lists, and the inventory is name-based, so the tables left BOTH generated registries.
fn install_indexeddb_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str) {
    use crate::modules::indexeddb::tables::{
        install_version_change_event_interface, parent_of, IDBCURSOR, IDBCURSORWITHVALUE, IDBDATABASE, IDBFACTORY, IDBINDEX,
        IDBKEYRANGE, IDBOBJECTSTORE, IDBOPENDBREQUEST, IDBRECORD, IDBREQUEST, IDBTRANSACTION,
    };

    let mut ledger = installer::take_ledger(scope);
    let et = parent_of(scope, "EventTarget");
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>|
     -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>) {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[{realm}] indexeddb install gap: {}.{}", g.interface, g.member);
        }
        (built.proto, built.ctor)
    };

    build(scope, &mut ledger, &IDBFACTORY, None);
    let req = build(scope, &mut ledger, &IDBREQUEST, et);
    build(scope, &mut ledger, &IDBOPENDBREQUEST, Some(req));
    build(scope, &mut ledger, &IDBTRANSACTION, et);
    build(scope, &mut ledger, &IDBDATABASE, et);
    build(scope, &mut ledger, &IDBOBJECTSTORE, None);
    build(scope, &mut ledger, &IDBINDEX, None);
    let cur = build(scope, &mut ledger, &IDBCURSOR, None);
    build(scope, &mut ledger, &IDBCURSORWITHVALUE, Some(cur));
    build(scope, &mut ledger, &IDBKEYRANGE, None);
    build(scope, &mut ledger, &IDBRECORD, None);
    install_version_change_event_interface(scope, global, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// Build the twelve IndexedDB interfaces from their tables and put them on the WINDOW, plus the
/// `indexedDB` global -- a Rust-minted `IDBFactory` instance carrying the table's prototype, which is
/// what `globalThis.indexedDB = new IdbFactory()` in the deleted `dom/08e-indexeddb.js` produced.
///
/// Runs at context creation beside [`super::install_window_collections`], AFTER the bootstrap: `EventTarget`
/// and `Event` must already exist for the three `EventTarget` chains and the `IDBVersionChangeEvent`
/// splice, and `DOMStringList` for `objectStoreNames`/`indexNames`.
pub fn install_window_indexeddb<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    install_indexeddb_interfaces(scope, global, "korvus");

    // `indexedDB`: an own ACCESSOR on the global, as Chrome's is. The capture records the
    // NAME in `surface.json`'s `globals` and no descriptor, so this used to reproduce the own DATA
    // property the deleted JS created. Measured on stock Chrome 150.0.7871.46 over a loopback page:
    //
    // ```text
    // korvus  own data      writable=true                      enumerable=true configurable=true
    // chrome  own accessor  get="get indexedDB"/0 set=undefined enumerable=true configurable=true
    // ```
    let Some(factory) = mint_idb_factory(scope) else { return };
    //
    // The one window-singleton install: the slot, and the getter behind the Window
    // receiver check Chrome applies.
    crate::core::html::window_globals::define_window_singleton(scope, global, "indexedDB", factory, crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::With(mint_idb_factory));
}

/// This realm's `indexedDB`: a native `IdbFactory` on the `IDBFactory` prototype.
fn mint_idb_factory<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    use crate::modules::indexeddb::backend::idb_set_proto;
    use crate::modules::indexeddb::idb_factory::IdbFactory;
    let factory = crate::binding::class::mint(scope, IdbFactory { _p: 0 });
    idb_set_proto(scope, factory, "IDBFactory");
    Some(factory.into())
}

/// The same twelve tables in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_collections`].
///
/// No `indexedDB` GLOBAL here: the bundle's `worker.surface.json` `globals` list does not carry one,
/// and the bundle is the denominator -- adding an own property would be a structural diff against the
/// capture. `indexedDB` in a worker is an INHERITED `WorkerGlobalScope` row (the capture records it on
/// the prototype, and Chrome answers an `IDBFactory` object from it), so this PUBLISHES the factory
/// and the table's row exposes it. It used to answer `undefined` from a D2 stub.
pub fn install_worker_indexeddb<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::modules::indexeddb::backend::idb_set_proto;
    use crate::modules::indexeddb::idb_factory::IdbFactory;

    let global = scope.get_current_context().global(scope);
    install_indexeddb_interfaces(scope, global, "korvus-worker");
    let factory = crate::binding::class::mint(scope, IdbFactory { _p: 0 });
    idb_set_proto(scope, factory, "IDBFactory");
    crate::core::workers::worker_global_scope::publish_scope_singleton(scope, "indexedDB", factory.into());
}

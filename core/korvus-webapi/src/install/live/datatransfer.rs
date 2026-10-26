//! The `DataTransfer` family and `FileList`.
//!
//! The four interfaces `dom/08d-datatransfer.js` used to install with five `Object.defineProperty`
//! sites and four `method()` helper calls over thirteen `op_*` builder round-trips, onto the cppgc
//! class templates' own prototypes, with `dom/00-bootstrap.js` putting the raw class templates on the
//! global as the interface objects. Each is an `interface!` table in its own interface file now
//! (`core/clipboard/data_transfer.rs`, `core/fileapi/file_list.rs`), and this is where THE installer
//! mounts them: `DataTransfer` -- the only newable one -- through `build_newable`, the other three
//! through `create_interface`.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Flavor};

/// Build the four `DataTransfer`-family tables and put them on the WINDOW.
///
/// Order is `FileList` first, because `DataTransfer.files` mints one through
/// [`crate::core::fileapi::file_list::mint_file_list`], which reads the table's prototype off the
/// global. None of the four inherits anything, so there is no splice; `DataTransferItemList` and
/// `FileList` own `@@iterator === Array.prototype.values` as rows.
pub fn install_window_datatransfer<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::clipboard::data_transfer::{cb_dt_ctor, DATATRANSFER, DATATRANSFERITEM, DATATRANSFERITEMLIST};
    use crate::core::fileapi::file_list::FILELIST;

    let mut ledger = installer::take_ledger(scope);
    for def in [&FILELIST, &DATATRANSFERITEMLIST, &DATATRANSFERITEM] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] datatransfer install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &DATATRANSFER, cb_dt_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// `FileList` in the WORKER realm (invariant I8), called from `core/korvus-runtime/src/worker.rs`
/// beside [`super::install_worker_crypto_xhr`].
///
/// Only `FileList`: the bundle's `worker.surface.json` lists it (`[Exposed=(Window,Worker)]`, with a
/// member list byte-identical to the window's) and lists none of the three `DataTransfer` interfaces,
/// and the bundle is the denominator. The inventory is name-based, so the table left BOTH generated
/// registries -- without this call the worker would lose `FileList` entirely.
pub fn install_worker_datatransfer<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::fileapi::file_list::FILELIST;
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &FILELIST, "FileList", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] datatransfer install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

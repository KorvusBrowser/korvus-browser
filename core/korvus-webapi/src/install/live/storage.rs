//! Web Storage: the `Storage` interface object and the two area platform objects.
//!
//! Replaces `js/dom/01-interning.js`.

use deno_core::v8;

use crate::install::installer::{self, Flavor};

/// `Storage`, `localStorage` and `sessionStorage`, from the one table.
///
/// `dom/01-interning.js` did all three, in this order, and so does this: the interface object and its
/// prototype first (the areas chain to that prototype, so it has to exist), then the two areas.
///
/// WHERE THIS RUNS, and what moves because of it. The deleted file was the SECOND phase of the JS
/// bootstrap, so `Storage` / `localStorage` / `sessionStorage` were among the earliest own names of
/// the global. Every table install runs AFTER the whole ESM layer — the webapi extension has no Rust
/// context-creation hook of its own — so the three names move to the tail of
/// `Object.getOwnPropertyNames(globalThis)`, beside every other table-installed interface
/// (`Window`, `Range`, `Selection`, the IndexedDB family, `HTMLCollection`, `NodeList`, `Navigator`
/// …). That is the position already taken for `Observable`/`Subscriber`, and the global
/// SET is unchanged; only the order within it is, and it already does not match the capture's.
///
/// Window-only: `Storage` is in the bundle's window `globals` and not in its worker list, and neither
/// `localStorage` nor `sessionStorage` is exposed to a worker at all.
///
/// NOTHING ELSE DEFINES THESE THREE NAMES. `Storage` left `generated_registry.rs` with the table (the
/// inventory makes `gen_shells.py` skip it), and the two area globals were only ever assigned by the
/// deleted file — `BOOTSTRAP`, `host.js` and every remaining `.js` were grepped for `localStorage`,
/// `sessionStorage` and `Storage` and answer nothing. So there is no later JS to be discarded by this
/// install and no later JS to discard it.
pub fn install_window_storage<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::modules::storage::storage::{make_area, STORAGE};

    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &STORAGE, STORAGE.name, None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] storage install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);

    let proto: v8::Local<v8::Value> = built.proto.into();
    let mints: [crate::core::html::window_globals::MintFn; 2] = [mint_local_storage, mint_session_storage];
    for ((session, name), mint) in [(false, "localStorage"), (true, "sessionStorage")].into_iter().zip(mints) {
        let area = make_area(scope, session, proto);
        // An own ACCESSOR on the window, as in every browser of the family.
        crate::core::html::window_globals::define_window_singleton(scope, global, name, area.into(), crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::With(mint));
    }
}

/// A sandboxed frame without `allow-same-origin` has no storage: its origin is opaque. Chrome 150's
/// SecurityError, measured (`c7/repro/sandbox`); `true` when it was thrown.
fn refused(scope: &mut v8::PinScope, name: &str) -> bool {
    let global: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    if !crate::core::html::browsing_context::realm_is_sandboxed(scope, global) {
        return false;
    }
    crate::binding::protos::throw_dom(scope, &format!("Failed to read the '{name}' property from 'Window': The document is sandboxed and lacks the 'allow-same-origin' flag."), "SecurityError");
    true
}

/// This realm's `localStorage`.
fn mint_local_storage<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    if refused(scope, "localStorage") {
        return None;
    }
    let proto = crate::binding::protos::shell_proto(scope, "Storage")?;
    Some(crate::modules::storage::storage::make_area(scope, false, proto.into()).into())
}

/// This realm's `sessionStorage`.
fn mint_session_storage<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    if refused(scope, "sessionStorage") {
        return None;
    }
    let proto = crate::binding::protos::shell_proto(scope, "Storage")?;
    Some(crate::modules::storage::storage::make_area(scope, true, proto.into()).into())
}

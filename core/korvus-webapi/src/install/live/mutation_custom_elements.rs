//! Mutation observation, the custom-element registry and `<dialog>`.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use super::rows::{build_newable, parent_of};
use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// The interfaces `dom/08g-observers.js` mounted by hand, from their `interface!` tables.
///
/// Runs at context creation BEFORE [`crate::install::dom_chain::fill_dom_protos`], so `HTMLDialogElement` --
/// spliced here onto the REAL `HTMLElement.prototype` -- is the prototype the `dialog` tag maps onto
/// through the ONE `korvus_dom::tags` pass instead of a hand write, exactly as the L2 table family
/// does for `<table>`.
///
/// Window-only: the bundle's `worker.surface.json` lists none of these four, and the four left BOTH
/// generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_mutation_custom_elements<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::dom::mutation_observer::MUTATIONRECORD;
    use crate::core::html::custom_element_registry::{make_custom_elements, CUSTOMELEMENTREGISTRY};
    use crate::core::html::html_dialog_element::HTMLDIALOGELEMENT;

    let mut ledger = installer::take_ledger(scope);
    let he = parent_of(scope, "HTMLElement");
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>| {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] mutation/custom-elements install gap: {}.{}", g.interface, g.member);
        }
    };
    build(scope, &mut ledger, &CUSTOMELEMENTREGISTRY, None);
    build(scope, &mut ledger, &MUTATIONRECORD, None);
    build(scope, &mut ledger, &HTMLDIALOGELEMENT, he);
    // `MutationObserver` -- the one newable of the four -- is built in the bootstrap's class slot
    // ([`install_mutation_observer_interface`]), where its global key has always sat.
    installer::put_ledger(scope, ledger);

    // `customElements`: an own getter-only ACCESSOR of the window holding one `CustomElementRegistry`
    // on the table's prototype, as in every browser of the family. It was the
    // {writable:false} data property the deleted `Object.defineProperty(globalThis, ...)` wrote.
    let ce = make_custom_elements(scope);
    crate::core::html::window_globals::define_window_singleton(scope, global, "customElements", ce.into(), crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::With(mint_custom_elements));
}

/// `MutationObserver` from its table, on the calling realm's global: the one newable interface of this lane.
/// `new MutationObserver(cb)` mints the cppgc instance in `cb_mo_ctor`, which also carries the WebIDL
/// callback check the deleted JS wore an `op_nativize_ctor` mask to present as native.
///
/// IN THE BOOTSTRAP'S CLASS SLOT (`bootstrap::expose_native_classes`), not beside the other three below:
/// a realm's global key order is fingerprint surface, and `MutationObserver`'s key has sat there since
/// `dom/00-bootstrap.js` assigned deno_core's class (the table interface replaced it after the registry
/// install, keeping the position). The class is an own class now, so the table interface takes the slot.
pub(crate) fn install_mutation_observer_interface<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::dom::mutation_observer::{cb_mo_ctor, MUTATIONOBSERVER};
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    build_newable(scope, global, &MUTATIONOBSERVER, cb_mo_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// This realm's `customElements`.
fn mint_custom_elements<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    Some(crate::core::html::custom_element_registry::make_custom_elements(scope).into())
}

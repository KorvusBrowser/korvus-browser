//! `HTMLBodyElement`.
//!
//! Replaces `js/dom/08q-body-element.js`.

use deno_core::v8;

use super::rows::parent_of;
use crate::install::installer::{self, Flavor};

/// `HTMLBodyElement` from its `interface!` table: thirty rows — six presentation
/// reflections, twenty-two window-forwarding handler pairs and the two gamepad handlers Chrome keeps
/// element-local.
///
/// IT USED TO BIND A BRIDGE SLOT HERE, and the bind was deleted because nothing read it. The
/// slot held a NATIVE function over `core::html::html_body_element::body_handler_attr`, under the
/// name the runtime's `ACTIVATE_BODY_HANDLERS` driver called when that driver was JavaScript. Both of
/// its callers reach the Rust function directly and have for some time --
/// `korvus-runtime/src/navigation.rs::activate_body_handlers` through `webapi::page`, and
/// `core/html/subresource.rs::resource_attr` in-crate -- so the write was a native
/// function mounted on the page realm for no reader at all. `doc_body.rs`'s markup-`onload` test is
/// what proves the parser path still works without it.
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT, beside the other element lanes: the same two order
/// dependencies `install_window_script_element` records. `install::dom_chain::fill_dom_protos` runs
/// after the bootstrap and points the `body` tag at `globalThis.HTMLBodyElement.prototype`, and
/// `core/dom/spine.rs::create_html_element_shells` — which pre-creates the bare `HTML*Element`
/// objects FROM THE REGISTRY — no longer creates this one, because the name left the registry with
/// the table.
///
/// THE FORWARDING TARGET HAS TO EXIST BY THE TIME A PAGE READS ONE, not by the time this runs: every
/// row reads `globalThis.on<type>` through `get_prop` at CALL time, so it does not matter that
/// `install_window_window` (which installs the Window's 125 handler rows) runs later in the same
/// block. Nothing here captures the Window.
///
/// Window-only: the bundle's worker `globals` list has no `HTMLBodyElement`.
pub fn install_window_body_element<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_body_element::HTMLBODYELEMENT;

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &HTMLBODYELEMENT, HTMLBODYELEMENT.name, he, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] body element install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

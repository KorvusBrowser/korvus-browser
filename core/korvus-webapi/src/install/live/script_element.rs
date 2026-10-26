//! `HTMLScriptElement`.
//!
//! Replaces `js/dom/08t-script-element.js`.

use deno_core::v8;

use super::rows::parent_of;
use crate::install::installer::{self, Flavor};

/// `HTMLScriptElement` from its `interface!` table: seventeen rows, sixteen on the
/// prototype in the capture's order and `supports` on the interface object.
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT, beside the other element lanes. Two reasons, both
/// order dependencies rather than preferences:
///
///   * `install::dom_chain::fill_dom_protos` runs AFTER the bootstrap and points the `script` tag at
///     `globalThis.HTMLScriptElement.prototype`. The interface has to be on the global by then, or
///     every `<script>` wrapper wears `HTMLElement.prototype` and none of these rows is reachable.
///   * `core/dom/spine.rs::create_html_element_shells` pre-creates the bare `HTML*Element` interface
///     objects from the REGISTRY, and `HTMLScriptElement` left the registry with this table — so
///     nothing creates it any more except this line. (That is the same trade every element lane
///     made; it is written down here because the symptom of getting it wrong is a missing global
///     rather than a missing member.)
///
/// NOTHING ELSE DEFINES A MEMBER OF IT. `BOOTSTRAP`, `host.js` and every remaining `.js` were
/// grepped for `HTMLScriptElement` and for the member names a later pass could plausibly decorate
/// (`src`, `text`, `async`, `supports`); the only hits were the deleted file's. With the name out of
/// the registry, neither `backfill_members` — which is what used to supply `blocking` and
/// `innerText` — nor `install_arity_guards` can reach it either, so the table is the sole writer and
/// its order is the final order. (There was an `install_member_order` in that list until it was
/// measured reordering nothing and deleted with its table.)
pub fn install_window_script_element<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_script_element::HTMLSCRIPTELEMENT;

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &HTMLSCRIPTELEMENT, HTMLSCRIPTELEMENT.name, he, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] script element install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

//! The eleven small element interfaces.

use deno_core::v8;

use super::rows::{build_newable, parent_of};
use crate::install::installer::{self, Flavor};

/// The eleven interfaces of lane L1 from their `interface!` tables, in the WINDOW.
///
/// `HTMLInputElement`, `HTMLButtonElement`, `HTMLObjectElement`, `HTMLEmbedElement`,
/// `HTMLLinkElement`, `HTMLIFrameElement`, `HTMLFrameElement`, `HTMLTextAreaElement`,
/// `HTMLAnchorElement` (`core/html/*.rs`), `SpeechSynthesisUtterance` (`core/speech/`) and
/// `SecurityPolicyViolationEvent` (`core/events/`) -- 251 rows that were nine `.js` files
/// (`dom/08l`, `08n`, `08o`, `08p`, `08r`, `08u`, `08v`, `09b`, `09d`, 754 lines) plus four lines of
/// `dom/08b-reflected.js`, one block of `dom/09c-small-elements.js` and half of
/// `dom/08c-collections.js`'s hyperlink loop. All of that is deleted; this is their only source.
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT, and not after it like `install_window_tables`.
/// `HTMLLinkElement.sheet` has no Rust body: the live one is `korvus-runtime`'s CSSOM bootstrap
/// block, which re-defines `sheet` on `HTMLLinkElement.prototype` and `HTMLStyleElement.prototype`.
/// Installed AFTER that, this table's honest `stub` row would win and `link.sheet` would go
/// undefined; installed before it, `Object.defineProperty` on the already-present key replaces the
/// descriptor without MOVING it, so the row is what puts `sheet` in the capture's position and the
/// kernel's body is what answers -- the same arrangement W5.1 L12 uses for
/// `CSSStyleDeclaration.cssText`, and W4.f retires both. Nothing else in `BOOTSTRAP` or `host.js`
/// touches a member of these eleven (grepped for all eleven names).
///
/// Still before [`crate::install::dom_chain::fill_dom_protos`], which maps every tag `korvus_dom::tags` names
/// onto `globalThis[iface].prototype`, so `input`, `button`, `object`, `embed`, `link`, `iframe`,
/// `frame`, `textarea` and `a` land on these prototypes through the ONE tag pass.
///
/// Window-only: the bundle's `worker.surface.json` lists none of the eleven, and all eleven left
/// BOTH generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_small_elements<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::events::security_policy_violation_event::{cb_spve_ctor, SECURITYPOLICYVIOLATIONEVENT};
    use crate::core::html::html_anchor_element::HTMLANCHORELEMENT;
    use crate::core::html::html_button_element::HTMLBUTTONELEMENT;
    use crate::core::html::html_iframe_element::{HTMLFRAMEELEMENT, HTMLIFRAMEELEMENT};
    use crate::core::html::html_input_element::HTMLINPUTELEMENT;
    use crate::core::html::html_link_element::HTMLLINKELEMENT;
    use crate::core::html::html_object_element::{HTMLEMBEDELEMENT, HTMLOBJECTELEMENT};
    use crate::core::html::html_text_area_element::HTMLTEXTAREAELEMENT;
    use crate::core::speech::speech_synthesis_utterance::{cb_ssu_ctor, SPEECHSYNTHESISUTTERANCE};

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    for def in [
        &HTMLINPUTELEMENT,
        &HTMLBUTTONELEMENT,
        &HTMLOBJECTELEMENT,
        &HTMLEMBEDELEMENT,
        &HTMLLINKELEMENT,
        &HTMLIFRAMEELEMENT,
        &HTMLFRAMEELEMENT,
        &HTMLTEXTAREAELEMENT,
        &HTMLANCHORELEMENT,
    ] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, he, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] small-elements install gap: {}.{}", g.interface, g.member);
        }
    }
    // The two NEWABLE ones: both have a real native constructor, so the interface object is built
    // over it (`build_newable`) rather than the installer's "Illegal constructor" placeholder.
    build_newable(scope, global, &SPEECHSYNTHESISUTTERANCE, cb_ssu_ctor, Some("EventTarget"), None, &mut ledger);
    build_newable(scope, global, &SECURITYPOLICYVIOLATIONEVENT, cb_spve_ctor, Some("Event"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// `SecurityPolicyViolationEvent` in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_abort_style`].
///
/// The one interface of this lane the worker capture lists -- `worker.surface.json` records it with
/// a member list byte-identical to the window's and lists none of the other ten. The inventory is
/// name-based, so the table left BOTH generated registries; without this call the worker would lose
/// the interface entirely. It is the SAME table and the SAME native constructor, so a violation
/// event constructed in a worker answers exactly what one constructed in the window does.
pub fn install_worker_small_elements<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::events::security_policy_violation_event::{cb_spve_ctor, SECURITYPOLICYVIOLATIONEVENT};
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    build_newable(scope, global, &SECURITYPOLICYVIOLATIONEVENT, cb_spve_ctor, Some("Event"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

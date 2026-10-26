//! The forms family: `<form>`, `<output>`, `<fieldset>`, `<meter>`.

use deno_core::v8;

use super::rows::parent_of;
use crate::install::installer::{self, Flavor};

/// The four forms interfaces from their `interface!` tables, in the WINDOW.
///
/// `HTMLFormElement` (18 rows), `HTMLOutputElement` (13), `HTMLFieldSetElement` (11) and
/// `HTMLMeterElement` (7) -- 49 rows that were three `.js` files (`dom/08w-form-element.js` 174 lines,
/// `dom/08y-output-fieldset.js` 110, `dom/08m-meter-element.js` 15) plus six lines of
/// `dom/08b-reflected.js`. Six `KB.__` bridge slots, six `op_helper_*` native masks, seven JS
/// `Object.defineProperty` sites and eleven op builders go with them; this is their only source.
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT, beside
/// [`super::install_window_small_elements`]. Nothing else in the tree defines a member of these
/// four: every remaining `.js` file, `BOOTSTRAP` and `host.js` were grepped
/// for all four interface names and for the member names that could plausibly be decorated
/// (`elements`, `checkValidity`, `requestSubmit`, `defaultValue`, `htmlFor`, `optimum`), and the only
/// hits were the six `dom/08b-reflected.js` lines this lane deletes. So there is no later JS to be
/// discarded by the table and no later JS to discard the table -- the position is free, and the early
/// one is chosen so that the tag map (`crate::install::dom_chain::fill_dom_protos`, which runs after the
/// bootstrap) points `form`, `output`, `fieldset` and `meter` at THESE prototypes.
///
/// Window-only: the bundle's `worker.surface.json` lists none of the four, and all four left BOTH
/// generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_forms<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_form_element::HTMLFORMELEMENT;
    use crate::core::html::html_meter_element::HTMLMETERELEMENT;
    use crate::core::html::html_output_element::{HTMLFIELDSETELEMENT, HTMLOUTPUTELEMENT};

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    for def in [&HTMLFORMELEMENT, &HTMLOUTPUTELEMENT, &HTMLFIELDSETELEMENT, &HTMLMETERELEMENT] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, he, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] forms install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

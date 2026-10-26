//! The five elements whose missing members LOOK AT OTHER ELEMENTS -- `<area>`, `<canvas>`,
//! `<label>`, `<map>`, `<legend>`.

use deno_core::v8;

use super::rows::{parent_of, rechain_orphans};
use crate::install::installer::{self, Flavor};

/// `HTMLAreaElement`, `HTMLCanvasElement`, `HTMLLabelElement`, `HTMLMapElement` and
/// `HTMLLegendElement` from their `interface!` tables.
///
/// 37 rows that were `dom/09c-small-elements.js` (142 lines, four `Object.defineProperty` sites, five
/// `KB.__` bridge slots -- `__mapAreas`, `__labelControl`, `__labelForm`, `__legendForm`,
/// `__areaToString` -- behind `op_helper_get` / `op_helper_method` native masks) plus the `<area>`
/// half of `dom/08c-collections.js`'s HyperlinkElementUtils loop (two more `defineProperty` sites).
/// All of it is deleted; these tables are their only source.
///
/// WHAT THE FIVE HAVE IN COMMON, and the reason they are one lane: the members plain attribute
/// reflection never reached are the ones that LOOK AT OTHER ELEMENTS -- a map's areas, a legend's
/// form, a label's control. Those are algorithms, and they are Rust now
/// (`core/html/html_map_element.rs`, `core/html/html_label_element.rs`).
///
/// WHY THIS RUNS FROM [`super::install_window_bootstrap`] AND NOT LATER IN THE PRE-BOOTSTRAP BLOCK,
/// unlike [`super::install_window_small_elements`]. It runs before
/// [`crate::install::dom_chain::fill_dom_protos`] -- see the paragraph below, which is now the whole
/// reason.
///
/// THE REASON THIS PARAGRAPH USED TO GIVE IS SPENT, and it was WRONG rather than merely outdated
/// (on W7.4's measurement). It said `getContext`, `toDataURL` and `toBlob` had to be
/// honest `stub` rows that only put each name in the capture's position, because `getContext` is
/// built over the persona's `has_webgl` "which no table row can carry" -- so a later
/// `wire_canvas_oracle` pass had to replace the descriptor in place and supply the body, and
/// installing this lane after that pass would let the stubs win and take the whole canvas/WebGL
/// surface silent.
///
/// That ceiling was true of the INSTALL moment and irrelevant to the CALL moment. `has_webgl` is read
/// per call out of `OpState` by [`crate::core::html::webgl::oracle_has_webgl`], as the oracle-backed
/// WebGL rows have done since W5.1 L17, so one call-time read spends the entire install-order
/// argument. W7.4 moved all four bodies into their rows: `HTMLCanvasElement`'s `getContext`,
/// `toDataURL`, `toBlob` and `captureStream` are `native` in `tools/status/data/layers.json`, not `stub`,
/// and they were four of the twelve second writers that took the install ledger's manifest to zero.
///
/// Kept as a record because the shape recurs: a comment that explains why something CANNOT be done
/// has to be re-tested whenever the thing it blames is deleted or moved, and this one went on
/// asserting its ceiling in its own voice for three tracks after the fact changed.
///
/// Still BEFORE [`crate::install::dom_chain::fill_dom_protos`], which maps every tag
/// `korvus_dom::tags` names onto `globalThis[iface].prototype`, so `area`, `canvas`, `map`,
/// `label` and `legend` land on THESE
/// prototypes through the ONE tag pass.
///
/// Nothing else in the tree defines a member of the five: every remaining `.js` file, `BOOTSTRAP` and
/// `host.js` were grepped for all five interface names and for `areas`, `control`, `htmlFor`,
/// `noHref`, `interestForElement` and `transferControlToOffscreen`; after this lane's deletions the
/// only hits left are `htmlFor` on `HTMLTemplateElement` and `HTMLScriptElement`, which are other
/// interfaces.
///
/// Window-only: the bundle's `worker.surface.json` lists none of the five, and all five left BOTH
/// generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_element_refs<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_area_element::HTMLAREAELEMENT;
    use crate::core::html::html_canvas_element::HTMLCANVASELEMENT;
    use crate::core::html::html_label_element::{HTMLLABELELEMENT, HTMLLEGENDELEMENT};
    use crate::core::html::html_map_element::HTMLMAPELEMENT;

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    for def in [&HTMLAREAELEMENT, &HTMLCANVASELEMENT, &HTMLLABELELEMENT, &HTMLMAPELEMENT, &HTMLLEGENDELEMENT] {
        let built = installer::create_interface(scope, global, def, def.name, he, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] element-refs install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
    // Symmetry with L4: repair any registry child whose parent is a name this lane took out of the
    // registry. Derived from the registry rather than from a written-down list, and it touches only a
    // child whose prototype is still empty.
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
}


//! The CSSOM: `StyleSheet`, `CSSStyleSheet`, `MediaList`, `CSSRule` and the three rule interfaces.
//!
//! Replaces the ~300-line CSSOM block of `core/korvus-runtime/src/lib.rs`'s
//! `BOOTSTRAP` string literal — nineteen `Object.defineProperty` sites, eleven `KB.__*` bridge slots
//! and twenty-five `op_helper_*` native masks, all of it installed by something that is not the
//! installer.

use deno_core::v8;

use super::rows::{build_newable, parent_of, rechain_orphans};
use crate::install::installer::{self, Flavor};

/// The seven CSSOM interfaces from their `interface!` tables, in the WINDOW.
///
/// WHERE THIS RUNS, and why it is not load-bearing any more. It is called from the runtime's
/// pre-bootstrap block, right after `install_window_abort_style` — which is the lane that owns
/// `CSSStyleDeclaration`, and `CSSPositionTryDescriptors` inherits from it, so the parent has to
/// exist when [`build_chained`] splices the child. That is the ONLY ordering this install has.
///
/// It is worth saying what ordering it no longer has, because three files carried a comment about it.
/// `install_window_small_elements` (HTMLLinkElement), `install_window_per_tag_elements`
/// (HTMLStyleElement) and `install_window_abort_style` (CSSStyleDeclaration) were each pinned to run
/// BEFORE the bootstrap script so the kernel's CSSOM block could DECORATE a row they had installed —
/// `sheet` on the two elements, `cssText` on the declaration. A define does not move an existing key,
/// so the honest `stub` row put the member in the capture's position and the kernel's later define
/// supplied the body. There is no later define now: `sheet` and `cssText` are real rows of their own
/// tables. The three installs stay exactly where they are (moving one is a measurement), and their
/// comments now record that the reason is spent.
///
/// Window-only: the bundle's `worker.surface.json` lists none of the seven.
pub fn install_window_cssom<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::css::css_rule::{
        CSSCOUNTERSTYLERULE, CSSPOSITIONTRYDESCRIPTORS, CSSPOSITIONTRYRULE, CSSRULE, CSSSTYLERULE,
    };
    use crate::core::css::style_sheet::{cb_sheet_ctor, CSSSTYLESHEET, MEDIALIST, STYLESHEET};

    use crate::core::css::media_query_list::MEDIAQUERYLIST;

    let mut ledger = installer::take_ledger(scope);
    // `MediaQueryList`, replacing `host.js`'s `__wireMQL` block -- which REDEFINED five of
    // this prototype's members at `INSTALL_DOCUMENT` time, over the registry's own rows. It inherits
    // `EventTarget`, which exists from the ESM layer, and nothing else in this lane touches it.
    {
        let et = super::rows::parent_of(scope, "EventTarget");
        let built = installer::create_interface(scope, global, &MEDIAQUERYLIST, "MediaQueryList", et, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] cssom install gap: {}.{}", g.interface, g.member);
        }
    }
    // The two roots first: `CSSStyleSheet` inherits from `StyleSheet`, and the three rule interfaces
    // from `CSSRule`.
    for def in [&STYLESHEET, &CSSRULE, &MEDIALIST] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] cssom install gap: {}.{}", g.interface, g.member);
        }
    }
    // `CSSStyleSheet` is the one NEWABLE interface of the seven (`surface.json`: `newable: true`), and
    // `new CSSStyleSheet()` is a real constructed style sheet -- the shape `@position-try` and
    // `@counter-style` are tested through, and the one CSS-in-JS uses for adopted sheets.
    build_newable(scope, global, &CSSSTYLESHEET, cb_sheet_ctor, Some("StyleSheet"), None, &mut ledger);
    for (def, parent) in [
        (&CSSSTYLERULE, "CSSRule"),
        (&CSSPOSITIONTRYRULE, "CSSRule"),
        (&CSSCOUNTERSTYLERULE, "CSSRule"),
        (&CSSPOSITIONTRYDESCRIPTORS, "CSSStyleDeclaration"),
    ] {
        let p = parent_of(scope, parent);
        if p.is_none() {
            eprintln!("[korvus] cssom install: {} has no parent {parent} in this realm", def.name);
        }
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, p, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] cssom install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
    // `rechain_orphans` LAST, for the reason seven other lanes end with it, and here it matters more
    // than for most: `CSSRule` has FOURTEEN registry CHILDREN this engine still builds as shells --
    // CSSFontFaceRule, CSSImportRule, CSSKeyframeRule, CSSKeyframesRule, CSSGroupingRule,
    // CSSLayerBlockRule, CSSLayerStatementRule, CSSMarginRule, CSSNamespaceRule,
    // CSSNestedDeclarations, CSSPropertyRule, CSSViewTransitionRule, CSSFontPaletteValuesRule and
    // CSSFontFeatureValuesRule. The registry's own chain pass ran during the ESM layer, when
    // `globalThis.CSSRule` did not exist yet, so all fourteen sat on the bare `Object.prototype`;
    // this sweep is the repair, and without it `document.styleSheets[0].cssRules[0] instanceof
    // CSSRule` would be true while `new FontFace(…)`-adjacent rule shells answered false.
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
    // `SVGStyleElement.prototype.sheet` is the registry's own row over `HTMLStyleElement`'s body;
    // the pass that minted a getter for it here after the install is deleted.
}

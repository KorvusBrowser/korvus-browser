//! Everything `js/dom/09-postshells.js` did after the shell pass, and the two finalize runs.

use deno_core::v8;

use super::rows::{build_newable, parent_of, rechain_orphans};
use crate::install::installer::{self, Flavor};

/// Everything `js/dom/09-postshells.js` did after the shell pass, in Rust, at the same point in the
/// order.
///
/// WHY THIS IS THE FIRST LINE OF THE RUNTIME'S PRE-BOOTSTRAP BLOCK, and why that IS the JS file's
/// slot. The webapi extension's ESM layer was the LAST thing evaluated inside `JsRuntime::new`; the
/// pre-bootstrap block in `korvus-runtime/src/engine.rs` is the very next statement after that
/// constructor returns. Nothing runs in between, so a call placed at the top of that block occupies
/// the identical position in the order -- after [`super::install_window_bootstrap`]'s backfill,
/// registry install and canvas oracle (the lines above it, which is what `generated_reflect.js` and
/// `generated_shells.js` used to be), after [`super::svg`]'s borrow pass (which is what
/// `dom/09k-svg.js` used to be), and before every other table install (L5, L12, L1, L11b/c/d, L13,
/// L4, Window).
///
/// WHAT IT ENDS WITH, and why the ordering this lane inherited is PRESERVED rather than retired.
/// [`installer::finalize_surface`] is the last thing this function does, for the reason the JS file
/// was structurally last: the WebIDL arity guards wrap whatever owns each member slot at the end of
/// the bootstrap. (A third pass, `install_member_order`, needed the finished prototype for the same
/// reason; it was measured reordering nothing and deleted with its table.) This lane's tables
/// leave the generated registry, so neither pass reaches their rows any more -- each row carries its
/// own guard from [`super::rows::method_row`] and sits in the capture's position by construction -- but the passes
/// still own every REGISTRY interface, which is most of the surface, so the slot has to stay exactly
/// where it was. Measured, not assumed: `core/korvus-runtime/tests/postshells_live.rs` reads a
/// guarded member's no-argument TypeError and a prototype's enumeration order back off the live
/// surface at the end of bootstrap.
pub fn install_window_postshells<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::clipboard::clipboard_item::{cb_clipboard_item_ctor, CLIPBOARDITEM};
    use crate::core::css::highlight::{cb_highlight_ctor, HIGHLIGHT, HIGHLIGHTREGISTRY};
    use crate::core::css::style_property_map::{CSSSTYLEVALUE, STYLEPROPERTYMAPREADONLY};
    use crate::core::css::view_transition::{VIEWTRANSITION, VIEWTRANSITIONTYPESET};
    use crate::core::dom::svg_string_list::SVGSTRINGLIST;
    use crate::core::html::html_template_element::HTMLTEMPLATEELEMENT;

    let mut ledger = installer::take_ledger(scope);
    // `HTMLTemplateElement`, spliced onto the real `HTMLElement` chain. This is the interface
    // `dom/09-postshells.js` existed for -- the shell pass created its prototype (and a `content`
    // stub) only at the END of the bootstrap, so a native `content` had to be installed after it.
    // As a table there is nothing to run after: THE installer builds the prototype from the rows and
    // the shell is never created. BEFORE `install::dom_chain::fill_dom_protos` (post-bootstrap), so
    // the `template` tag maps onto THIS prototype through the one `korvus_dom::tags` pass.
    let he = parent_of(scope, "HTMLElement");
    let built = installer::create_interface(scope, global, &HTMLTEMPLATEELEMENT, "HTMLTemplateElement", he, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] postshells install gap: {}.{}", g.interface, g.member);
    }
    // `ClipboardItem`: the `supports` static a feature-detecting page calls, and a real constructor -- `types` is
    // the record's keys (C2), so the constructor has to keep them, which the table's placeholder could not.
    build_newable(scope, global, &CLIPBOARDITEM, cb_clipboard_item_ctor, None, None, &mut ledger);
    // `SVGStringList`: the prototype the four SVG element interfaces' `requiredExtensions` /
    // `systemLanguage` rows mint from. Those rows are REGISTRY rows naming `kits::svg_string_list_attr`
    // (claimed by `gen_shells.py` from the capture), and the registry install ran during the ESM layer
    // -- but they only mint a list when a page READS one, which is always after this, so the
    // prototype existing by now is the whole requirement.
    let built = installer::create_interface(scope, global, &SVGSTRINGLIST, "SVGStringList", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] postshells install gap: {}.{}", g.interface, g.member);
    }
    // The CSS Custom Highlight API. `HighlightRegistry` inherits nothing and is not newable (`new
    // HighlightRegistry()` is "Illegal constructor", recorded); `Highlight` is newable and variadic.
    // Both left the generated registry with these tables, so nothing else defines a member of either.
    let built = installer::create_interface(scope, global, &HIGHLIGHTREGISTRY, "HighlightRegistry", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] postshells install gap: {}.{}", g.interface, g.member);
    }
    build_newable(scope, global, &HIGHLIGHT, cb_highlight_ctor, None, None, &mut ledger);
    // The Presentation API pair: `PresentationAvailability` is not newable, and
    // `PresentationRequest` is; both are `EventTarget`s and `[SecureContext]` (the row's `secure`,
    // which the two publish sites gate on). Both left the generated registry with these tables.
    {
        use crate::core::html::presentation::{cb_presentation_request_ctor, PRESENTATIONAVAILABILITY, PRESENTATIONREQUEST};
        let et = parent_of(scope, "EventTarget");
        let built = installer::create_interface(scope, global, &PRESENTATIONAVAILABILITY, "PresentationAvailability", et, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] postshells install gap: {}.{}", g.interface, g.member);
        }
        build_newable(scope, global, &PRESENTATIONREQUEST, cb_presentation_request_ctor, Some("EventTarget"), None, &mut ledger);
    }
    // The Typed OM pair behind `Element.computedStyleMap()`, and the view-transition pair behind
    // `Element.startViewTransition()`. None of the four is newable and none inherits.
    for def in [&STYLEPROPERTYMAPREADONLY, &CSSSTYLEVALUE, &VIEWTRANSITIONTYPESET, &VIEWTRANSITION] {
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] postshells install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
    // Symmetry with L4/L14: repair any registry child whose parent is a name this lane took out of
    // the registry (`StylePropertyMap` inherits `StylePropertyMapReadOnly`; the CSSStyleValue family
    // -- CSSUnparsedValue, CSSTransformValue, CSSKeywordValue, CSSNumericValue, CSSImageValue,
    // CSSPositionValue -- all inherit `CSSStyleValue`). Derived from the registry, never a list, and
    // it touches only a child whose prototype is still empty.
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
    // `CSS.highlights` -- the realm's one registry, on the CSS namespace object. After the two
    // prototypes exist, because the instance wears `HighlightRegistry.prototype`.
    crate::core::css::highlight::install_css_highlights(scope);
    installer::finalize_surface(scope, global);
}

/// The SECOND finalize run, behind the runtime's own bootstrap (`Engine::install_document`) -- the
/// slot the `KB.__finalizeSurface` bridge slot held. See [`installer::finalize_surface`] for why two
/// runs are still necessary and what silently swallowed the JS one for weeks.
pub fn finalize_surface<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    installer::finalize_surface(scope, global);
}

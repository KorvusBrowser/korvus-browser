//! The six element interfaces whose members the registry's kit rows did not carry:
//! `<select>`, `<datalist>`, `<progress>`, `<style>`, `<slot>` and `<title>`.
//!
//! Replaces the last sixteen mounts of `js/dom/08b-reflected.js`.

use deno_core::v8;

use super::rows::parent_of;
use crate::install::installer::{self, Flavor};

/// The five tables the registry could not body.
///
/// WHAT THEY HAVE IN COMMON is exactly what the deleted file's header said: they are the per-tag
/// members neither the registry's `[Reflect]` kit rows nor any element lane defines, so a hand pass
/// had to mount them on the shell prototype after the registry built it. Each is a table row now, and
/// the interfaces left the registry with their tables — so `backfill_members`, the pass that used to
/// finish these prototypes, cannot reach them, and the table's order is the final order. (The
/// enumeration-order pass that also stood behind it is gone entirely.)
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT. Three reasons, and the third is a measured contract
/// rather than a preference:
///
///   * that is where the deleted JS ran (during `dom.js`, before this script);
///   * `install::dom_chain::fill_dom_protos` runs AFTER the bootstrap and points the `select`,
///     `datalist`, `progress`, `style` and `slot` tags at these prototypes — and, since the five
///     names left the registry, `core/dom/spine.rs::create_html_element_shells` no longer pre-creates
///     them either, so this line is the only thing that puts them on the global;
///   * `BOOTSTRAP`'s CSSOM block DECORATES `HTMLStyleElement.prototype.sheet` with a real
///     `CSSStyleSheet`, and a define does not move an existing key. The table's honest `stub` row puts
///     `sheet` in the capture's POSITION and the decorator supplies the body. Installed AFTER the
///     bootstrap, the stub would win and `style.sheet` would go undefined — the identical contract
///     `super::small_elements` records for `HTMLLinkElement.sheet`.
///
/// Window-only: none of the five is in the bundle's worker `globals` list.
pub fn install_window_per_tag_elements<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_select_element::{HTMLDATALISTELEMENT, HTMLPROGRESSELEMENT, HTMLSELECTELEMENT};
    use crate::core::html::html_slot_element::HTMLSLOTELEMENT;
    use crate::core::html::html_style_element::HTMLSTYLEELEMENT;
    use crate::core::html::html_title_element::HTMLTITLEELEMENT;
    use crate::core::html::html_marquee_element::HTMLMARQUEEELEMENT;

    let he = parent_of(scope, "HTMLElement");
    let mut ledger = installer::take_ledger(scope);
    for def in [&HTMLSELECTELEMENT, &HTMLDATALISTELEMENT, &HTMLPROGRESSELEMENT, &HTMLSTYLEELEMENT, &HTMLSLOTELEMENT, &HTMLTITLEELEMENT, &HTMLMARQUEEELEMENT] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, he, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] per-tag element install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

//! The six `<table>` element interfaces (`core/html/html_table_elements.rs`).

use deno_core::v8;

use crate::install::installer::{self, Flavor};

/// Build the six `<table>` interfaces from their `interface!` tables (`core/html/html_table_elements.rs`)
/// and put them on the window.
///
/// Each is spliced onto the REAL `HTMLElement.prototype` the spine built, so the recorded chain
/// `HTMLTableElement.prototype -> HTMLElement -> Element -> Node -> EventTarget` holds. Nothing
/// re-registers a tag here: this runs BEFORE [`crate::install::dom_chain::fill_dom_protos`], which reads
/// `globalThis[iface].prototype` for every tag `korvus_dom::tags` names, so `table`, `tr`, `td`, `th`,
/// `thead`, `tbody`, `tfoot`, `col`, `colgroup` and `caption` land on these prototypes by the ONE
/// mechanism rather than by ten hand writes.
///
/// Window-only: the bundle's `worker.surface.json` lists none of the six, and the six left BOTH
/// generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_tables<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::html_table_elements::{
        HTMLTABLECAPTIONELEMENT, HTMLTABLECELLELEMENT, HTMLTABLECOLELEMENT, HTMLTABLEELEMENT, HTMLTABLEROWELEMENT,
        HTMLTABLESECTIONELEMENT,
    };

    let he = crate::binding::protos::iface_object(scope, "HTMLElement").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let parent = he.and_then(|he| {
        let pk = v8::String::new(scope, "prototype")?;
        let hep = he.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        Some((hep, he))
    });

    let mut ledger = installer::take_ledger(scope);
    for def in [
        &HTMLTABLEELEMENT,
        &HTMLTABLEROWELEMENT,
        &HTMLTABLESECTIONELEMENT,
        &HTMLTABLECELLELEMENT,
        &HTMLTABLECOLELEMENT,
        &HTMLTABLECAPTIONELEMENT,
    ] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] table install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

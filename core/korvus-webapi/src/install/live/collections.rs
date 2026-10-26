//! The platform LIST interfaces -- `HTMLCollection`, `NodeList`, `DOMTokenList`, `NamedNodeMap`,
//! `CSSRuleList`, `DOMRectList`, `StyleSheetList`, `DOMStringList` and the three subclasses
//! (`core/dom/collections.rs`), in both realms.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// Build the ten platform LIST interfaces from their `interface!` tables
/// (`core/dom/collections.rs`) and put them on the window.
///
/// Order is the WebIDL inheritance order: `HTMLCollection` and `NodeList` first, so
/// `HTMLFormControlsCollection` / `HTMLOptionsCollection` / `RadioNodeList` can be spliced onto their
/// real parents' prototype and constructor. Every row goes through the ONE member mount, so each
/// prototype is `constructor` LAST, then `Symbol.toStringTag` -- the hand-table shape -- and each
/// interface object throws "Illegal constructor" for `new`, which is what the capture records.
pub fn install_window_collections<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::dom::collections::{
        CSSRULELIST, DOMRECTLIST, DOMSTRINGLIST, DOMTOKENLIST, HTMLALLCOLLECTION, HTMLCOLLECTION,
        HTMLFORMCONTROLSCOLLECTION, HTMLOPTIONSCOLLECTION, NAMEDNODEMAP, NODELIST, RADIONODELIST,
        STYLESHEETLIST,
    };

    let mut ledger = installer::take_ledger(scope);
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>|
     -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>) {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] collections install gap: {}.{}", g.interface, g.member);
        }
        (built.proto, built.ctor)
    };

    let hc = build(scope, &mut ledger, &HTMLCOLLECTION, None);
    // `HTMLAllCollection` is NOT an `HTMLCollection` subclass -- its prototype's proto is
    // `Object.prototype` (measured in Chrome 150) -- so it is built with no parent.
    build(scope, &mut ledger, &HTMLALLCOLLECTION, None);
    build(scope, &mut ledger, &HTMLFORMCONTROLSCOLLECTION, Some(hc));
    build(scope, &mut ledger, &HTMLOPTIONSCOLLECTION, Some(hc));
    let nl = build(scope, &mut ledger, &NODELIST, None);
    build(scope, &mut ledger, &RADIONODELIST, Some(nl));
    build(scope, &mut ledger, &NAMEDNODEMAP, None);
    build(scope, &mut ledger, &DOMTOKENLIST, None);
    build(scope, &mut ledger, &CSSRULELIST, None);
    build(scope, &mut ledger, &DOMRECTLIST, None);
    build(scope, &mut ledger, &STYLESHEETLIST, None);
    build(scope, &mut ledger, &DOMSTRINGLIST, None);
    installer::put_ledger(scope, ledger);
}


/// The `[Exposed=(Window,Worker)]` list tables in the WORKER realm -- today `DOMStringList` (IndexedDB's
/// `objectStoreNames`/`indexNames`, which workers have). The inventory is name-based, so a table leaves
/// BOTH registries; the same table is installed here so the worker keeps the interface.
pub fn install_worker_collections<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::dom::collections::DOMSTRINGLIST;
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &DOMSTRINGLIST, "DOMStringList", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] collections install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

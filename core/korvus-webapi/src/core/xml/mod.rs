//! `xml` Web-API interfaces: `DOMParser`, `XMLSerializer`, `DOMImplementation`, and the two names a
//! document's kind is read through, `HTMLDocument` / `XMLDocument`.
//!
//! All five are hand `interface!` tables with Rust bodies. They were `dom/05-xml.js`:
//! a JS class per interface, a per-document object literal standing in for a Document, and three
//! bridge slots (`KB.__makeDoc`, `KB.__validateExtract`, `KB.__domImpl`) the Rust side called back
//! into. Nothing of that survives -- a parsed document is the same native mint the page document is,
//! and the qualified-name validator is a Rust function two callers share.

pub(crate) mod dom_implementation;
pub(crate) mod dom_parser;
pub(crate) mod document_kinds;
pub(crate) mod xml_serializer;
/// `XPathEvaluator` / `XPathExpression` / `XPathResult` and the Document members.
pub(crate) mod xpath;
pub(crate) mod xpath_eval;
pub(crate) mod xpath_parse;

use deno_core::v8;

use crate::install::installer::{create_interface, Flavor, InstallLedger};

/// Install the five tables. Called from `core/dom/spine.rs` at the point the spine has just created
/// `Document`: `HTMLDocument`/`XMLDocument` are spliced onto it (their prototypes chain to
/// `Document.prototype`, so a parsed document wearing one answers every Document member), and the
/// three standalone interfaces are created beside it.
pub(crate) fn install_xml<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    document: (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>),
    ledger: &mut InstallLedger,
) {
    for def in [
        &document_kinds::HTMLDOCUMENT,
        &document_kinds::XMLDOCUMENT,
    ] {
        let built = create_interface(scope, global, def, def.name, Some(document), None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] xml install gap: {}.{}", g.interface, g.member);
        }
    }
    for def in [
        &dom_parser::DOMPARSER,
        &xml_serializer::XMLSERIALIZER,
        &dom_implementation::DOMIMPLEMENTATION,
    ] {
        let built = create_interface(scope, global, def, def.name, None, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] xml install gap: {}.{}", g.interface, g.member);
        }
    }
}

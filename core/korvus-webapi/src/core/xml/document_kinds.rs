//! `core/xml/document_kinds.rs` -- `HTMLDocument` and `XMLDocument` (was `dom/05-xml.js`).
//!
//! Two interfaces with no members of their own: everything they answer comes from
//! `Document.prototype`, which their prototypes chain to. What they ARE is the pair of names a
//! document's kind is read through -- `Object.prototype.toString.call(doc)` reads the
//! `Symbol.toStringTag` this table's prototype carries, and `doc instanceof HTMLDocument` reads the
//! prototype chain.
//!
//! WHAT THIS REPLACES. The JS built each as a function whose `Symbol.hasInstance` was a hand-written
//! predicate over a `__docKind` marker property, because a parsed document was a plain object literal
//! and could not be recognised any other way. A parsed document is now a real native Document wearing
//! this interface's prototype (`core/dom/document.rs::mint_document_for`), so the ordinary prototype
//! walk answers `instanceof` -- and the custom `@@hasInstance`, which REPLACES that walk rather than
//! adding to it, is gone with the trap it was.
//!
//! `new HTMLDocument()` / `new XMLDocument()` throw "Illegal constructor" (the recorded browser, and
//! what `Model::Interface` gives every table).

// `install_document_kind` was here, and its own doc line said what was wrong with it: "Neither
// interface has an own member; the installer never calls this." It was an `InstallFn` that no row
// names, whose whole body was an `eprintln!` reporting a gap that cannot occur. Deleted.
// If either table ever grows a member row, that row names its own body like every other row does.

webapi::interface! {
    "HTMLDocument" : "Document" [interface, window] {
        ctor "HTMLDocument" len 0 = stub
    }
}

webapi::interface! {
    "XMLDocument" : "Document" [interface, window] {
        ctor "XMLDocument" len 0 = stub
    }
}

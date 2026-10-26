//! `core/xml/dom_parser.rs` -- `DOMParser` (the table and the Rust body).
//!
//! `parseFromString(str, type)` parses into the SAME arena as the page, on its own detached Document
//! root, so scoped element queries work and the result is a real node tree rather than a second DOM.
//! The document it hands back is the ONE native mint (`core/dom/document.rs::mint_document_for`) --
//! the same function the runtime's `INSTALL_DOCUMENT` uses for the page document -- wearing
//! `HTMLDocument.prototype` or `XMLDocument.prototype` according to the type argument.

use deno_core::v8;

use crate::binding::protos::wrap_cached;
use crate::binding::HostDom;
use crate::core::dom::document::{mint_document_for, DocKind, DocOrigin};
use crate::core::dom::shared::row_method;
use crate::install::MemberDef;

/// An HTML document parsed from `html` exactly as `parseFromString(html, "text/html")` makes one,
/// without going through the page's `DOMParser` -- which a page may have replaced. A frame's document
/// is built from this.
/// An `iframe srcdoc` document when `srcdoc`: never in a quirks mode.
pub(crate) fn parse_html_document_as<'s>(scope: &mut v8::PinScope<'s, '_>, html: &str, srcdoc: bool) -> v8::Local<'s, v8::Object> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root_id = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        if srcdoc { dom.parse_srcdoc_into(html) } else { dom.parse_into(html) }
    };
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &[root_id]);
    let _ = wrap_cached(scope, &op_state, root_id);
    // A DOMParser/frame document keeps the creating document's URL as its base
    // (`Parsed`, not about:blank); a frame's own nodes resolve via its frame URL, never this branch.
    mint_document_for(scope, root_id, DocKind::Html, DocOrigin::Parsed)
}

fn cb_domparser_parse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // (DOMString or TrustedHTML): a TrustedHTML read as itself, nothing on it called
    let trusted = crate::core::trusted_types::tables::is_trusted(scope, args.get(0), "TrustedHTML");
    let Some(s) = crate::core::trusted_types::tables::sink_text(scope, args.get(0), "TrustedHTML") else { return };
    // `SupportedType`: the type converted to a string, then one of the five enum values exactly (Chrome 150:
    // `"text/foo"`, `"TEXT/HTML"` and `""` are TypeErrors in Blink's words; this parsed each as HTML).
    let Some(ty_s) = args.get(1).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    if !matches!(ty_s.as_str(), "text/html" | "text/xml" | "application/xml" | "application/xhtml+xml" | "image/svg+xml") {
        crate::binding::protos::throw_type_error(
            scope,
            &format!("Failed to execute 'parseFromString' on 'DOMParser': The provided value '{ty_s}' is not a valid enum value of type SupportedType."),
        );
        return;
    }
    // then, in the operation's body, a string goes through the DOMParser's realm's Trusted Types
    // ("DOMParser parseFromString"; Blink checks it against the parser's window)
    let s = if trusted {
        s
    } else {
        let realm = args.this().get_creation_context(scope).unwrap_or_else(|| scope.get_current_context());
        let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "DOMParser parseFromString", context: "Failed to execute 'parseFromString' on 'DOMParser'" };
        let Some(s) = crate::core::trusted_types::tables::check_text(scope, s, &sink, crate::core::trusted_types::tables::SinkAt::Realm(realm)) else { return };
        s
    };
    let is_xml = ty_s != "text/html";
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root_id = {
        let mut st = op_state.borrow_mut();
        if is_xml {
            st.borrow_mut::<HostDom>().parse_xml_document(&s)
        } else {
            st.borrow_mut::<HostDom>().0.parse_into(&s)
        }
    };
    // Interning the root's wrapper keeps node identity for it the way every other arena node has it.
    // A parsed document's scripts are prepared where scripting is disabled, which sets "already
    // started" and runs nothing: moved into the page they stay inert (HTML "prepare the script
    // element", steps 11 and 14).
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &[root_id]);
    let _ = wrap_cached(scope, &op_state, root_id);
    // `Document.contentType` is the type this call was GIVEN, verbatim -- Chrome 150, measured:
    // `parseFromString(s, t).contentType === t` for text/xml, application/xml, application/xhtml+xml
    // and image/svg+xml alike, whatever the root element's namespace turns out to be.
    let kind = if is_xml { DocKind::Xml(ty_s.clone()) } else { DocKind::Html };
    let doc = mint_document_for(scope, root_id, kind, DocOrigin::Parsed);
    rv.set(doc.into());
}

pub fn install_dom_parser(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "parseFromString" => row_method(scope, proto, member, cb_domparser_parse),
        other => eprintln!("[korvus] DOMParser install gap: {other}"),
    }
}

webapi::interface! {
    "DOMParser" [value, window] {
        ctor "DOMParser" len 0 = stub,
        method "parseFromString" : Object len 2 required 2 = install_dom_parser
    }
}

// This file owns no native ops and so declares no `webapi_ops![]`. It had one,
// `op_domparser_parse`, which handed `cb_domparser_parse` to `build_fn` for JS to fetch out of the
// op table; the table row above binds the same callback directly and nothing ever fetched it, so
// The op is deleted.

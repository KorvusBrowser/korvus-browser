//! `core/html/html_object_element.rs` -- `HTMLObjectElement` and `HTMLEmbedElement` as two
//! `interface!` tables.
//!
//! `dom/08r-object-element.js` (81 lines, 26 members behind one `Object.defineProperty` shape) and
//! the `install("HTMLEmbedElement", ...)` block of `dom/09c-small-elements.js` (7 members) are gone.
//! Every row below is mounted by THE installer (`install::live::install_window_small_elements`), with names,
//! order, kinds, lengths, `required` counts, nullability and descriptor flags VERBATIM from the
//! `HTMLObjectElement` / `HTMLEmbedElement` blocks of `install/generated_registry.rs`.
//!
//! ONE FILE FOR TWO INTERFACES. `<object>` and `<embed>` are the two plugin elements: they share
//! `getSVGDocument`, the `src`/`data` URL reflection and the "no nested browsing context, so no
//! document" answer, and the deleted JS shared the reasoning by writing the same comment twice.
//! Splitting them would put that one rule in two places, which is what this goal exists to stop.
//!
//! `<object>` is one of the three members of the constraint-validation mixin that never validate
//! (`<output>` and `<fieldset>` are the others): it carries every one of those members and is ALWAYS
//! barred, with no attribute that can change it -- `willValidate` is unconditionally false,
//! `validationMessage` "" and `checkValidity`/`reportValidity` unconditionally true. The bodies are
//! `<input>`'s own (`html_input_element.rs`), which ask the arena: `korvus_dom::forms`'s
//! `will_validate` is where "always barred" lives, so the rule is written once.
//!
//! `getSVGDocument()` answers null because Korvus has no nested browsing context to hold a document
//! -- and because a real Chrome answers null for an `<object>`/`<embed>` that has loaded nothing,
//! which is the only state one can be in here. `contentDocument`/`contentWindow` are
//! `core/html/browsing_context.rs`'s getters, the same ones `<iframe>` uses (W5.0d owns those
//! bodies; the rows only name them).
//!
//! ONE ROW DIFFERS FROM THE GENERATED CFG: `border` is a plain string reflection. webref carries no
//! `[Reflect]` for it, so `gen_reflected.py` left the row `stub` and the deleted JS installed it by
//! hand; the bundle's oracle records `(o.border = "x", o.border) === "x"`, so a `stub` row here
//! would be a regression dressed as fidelity.

use deno_core::v8;

use crate::binding::protos::cb_null_get;
use crate::core::html::browsing_context::{cb_frame_content_document_get, cb_frame_content_window_get};
use crate::core::html::html_input_element::{acc1, install_form_control_shared};
use crate::install::live::method_row;
use crate::install::MemberDef;

pub fn install_html_object_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if install_form_control_shared(scope, proto, member) {
        return;
    }
    match member.name {
        "contentDocument" => acc1(scope, proto, member, cb_frame_content_document_get, None),
        "contentWindow" => acc1(scope, proto, member, cb_frame_content_window_get, None),
        // No nested browsing context exists to hold a document -- see the file comment.
        "getSVGDocument" => method_row(scope, proto, member, cb_null_get),
        other => debug_assert!(false, "install_html_object_element: unexpected member {other}"),
    }
}

pub fn install_html_embed_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getSVGDocument" => method_row(scope, proto, member, cb_null_get),
        other => debug_assert!(false, "install_html_embed_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLObjectElement" : "HTMLElement" [element, window] {
        ctor   "HTMLObjectElement"                len 0             = stub,
        attr   "data"              : Object cfg "data"              = reflect_url,
        attr   "type"              : Object cfg "type"              = reflect_str,
        attr   "name"              : Object cfg "name"              = reflect_str,
        attr   "useMap"            : Object cfg "usemap"            = reflect_str,
        attr   "form"              : Object readonly nullable       = install_html_object_element,
        attr   "width"             : Object cfg "width"             = reflect_str,
        attr   "height"            : Object cfg "height"            = reflect_str,
        attr   "contentDocument"   : Object readonly nullable       = install_html_object_element,
        attr   "contentWindow"     : Object readonly nullable       = install_html_object_element,
        attr   "willValidate"      : Object readonly                = install_html_object_element,
        attr   "validity"          : Object readonly                = install_html_object_element,
        attr   "validationMessage" : Object readonly                = install_html_object_element,
        attr   "align"             : Object cfg "align"             = reflect_str,
        attr   "archive"           : Object cfg "archive"           = reflect_str,
        attr   "code"              : Object cfg "code"              = reflect_str,
        attr   "declare"           : Object cfg "declare"           = reflect_bool,
        attr   "hspace"            : Object cfg "hspace|0|"         = reflect_ulong,
        attr   "standby"           : Object cfg "standby"           = reflect_str,
        attr   "vspace"            : Object cfg "vspace|0|"         = reflect_ulong,
        attr   "codeBase"          : Object cfg "codebase"          = reflect_url,
        attr   "codeType"          : Object cfg "codetype"          = reflect_str,
        attr   "border"            : Object cfg "border"            = reflect_str,
        method "checkValidity"     : Object len 0                   = install_html_object_element,
        method "getSVGDocument"    : Object len 0 nullable          = install_html_object_element,
        method "reportValidity"    : Object len 0                   = install_html_object_element,
        method "setCustomValidity" : Object len 1 required 1        = install_html_object_element
    }
}

webapi::interface! {
    "HTMLEmbedElement" : "HTMLElement" [element, window] {
        ctor   "HTMLEmbedElement"                 len 0             = stub,
        attr   "src"               : Object cfg "src"               = reflect_url,
        attr   "type"              : Object cfg "type"               = reflect_str,
        attr   "width"             : Object cfg "width"              = reflect_str,
        attr   "height"            : Object cfg "height"             = reflect_str,
        attr   "align"             : Object cfg "align"              = reflect_str,
        attr   "name"              : Object cfg "name"               = reflect_str,
        method "getSVGDocument"    : Object len 0 nullable           = install_html_embed_element
    }
}

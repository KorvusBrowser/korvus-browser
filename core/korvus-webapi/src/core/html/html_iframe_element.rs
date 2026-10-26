//! `core/html/html_iframe_element.rs` -- `HTMLIFrameElement` and `HTMLFrameElement` as two
//! `interface!` tables.
//!
//! `dom/08v-iframe-element.js` (81 lines, 24 members behind one `Object.defineProperty` shape) is
//! gone, and so are the two `install(HTMLFrameElement, contentDocument/contentWindow)` lines of
//! `dom/08b-reflected.js`. Every row below is mounted by THE installer
//! (`install::live::install_window_small_elements`), with names, order, kinds, lengths, nullability and
//! descriptor flags VERBATIM from the `HTMLIFrameElement` / `HTMLFrameElement` blocks of
//! `install/generated_registry.rs`.
//!
//! ONE FILE FOR TWO INTERFACES: `<iframe>` and the legacy `<frame>` are the two frame elements, and
//! every member `<frame>` has is a member `<iframe>` has under the same rule -- `contentDocument`,
//! `contentWindow`, `src`, `longDesc`, `marginHeight`/`marginWidth`, `scrolling`, `frameBorder`,
//! `name`. `08b-reflected.js` said so out loud ("the legacy <frame> element has no file and mounts
//! the same native getters"). Splitting them would put one rule in two places.
//!
//! `contentWindow`, `contentDocument`, `src` and `srcdoc` are W5.0d's bodies in
//! `core/html/browsing_context.rs`; the rows only NAME them. `src` reflects like any URL attribute on
//! the way out but assigning it NAVIGATES the frame, and `srcdoc` navigates to `about:srcdoc` with
//! the attribute's own markup -- which is why neither is a plain reflect pair. `<frame>`'s `src` is
//! the generated `reflect_url` row: the legacy element never had the navigating setter here, and the
//! bundle's oracle records only its absent-attribute answer.
//!
//! THREE MEMBERS THE WebIDL DECLARES ARE DELIBERATELY ABSENT from the capture and so from this table
//! -- `connectionAllowlist`, `permissionsPolicy`, `privateToken`. A real Brave does not have them and
//! the recordings say so in the plainest way: reading gives `undefined`, writing then reading gives
//! the assigned value back, which is what a plain expando does. `featurePolicy` IS in the capture,
//! and it has a real answer now: `core/feature_policy/` is the permissions-policy subsystem
//! this header used to say did not exist, and the row hands back the element's CONTAINER policy --
//! the document's, with this element's `allow` attribute applied, read live off the element. It was
//! a `stub` row answering `null` until then, which is what the capture's `stub` row meant.
//!
//! FIVE ROWS DIFFER FROM THE GENERATED CFG: `csp`, `marginHeight` and `marginWidth` are plain string
//! reflections (webref has no `[Reflect]`, so the generator left them `stub`); `referrerPolicy` and
//! `loading` are enumerated. The bundle's oracle records the round trip for `csp`, `marginHeight` and
//! `marginWidth` on BOTH elements (`(o.marginHeight = "x", o.marginHeight) === "x"`), and for
//! `loading` both the missing and the invalid default ("auto", not the spec's "eager") -- so the
//! four `<frame>` cases that failed before this lane, all four of them margin cases, pass now.

use deno_core::v8;

use crate::core::dom::element::{cb_reflect_str_get, cb_reflect_url_get};
use crate::core::html::browsing_context::{
    cb_frame_content_document_get, cb_frame_content_window_get, cb_frame_src_set, cb_frame_srcdoc_set,
};
use crate::core::html::html_input_element::{acc1, acc2};
use crate::core::html::html_link_element::token_list_row;
use crate::install::live::method_row;
use crate::install::MemberDef;
use crate::binding::protos::cb_null_get;
use crate::core::feature_policy::feature_policy::cb_element_feature_policy;

/// The rows `<iframe>` and `<frame>` share.
fn install_frame_shared(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) -> bool {
    match member.name {
        "contentDocument" => acc1(scope, proto, member, cb_frame_content_document_get, None),
        "contentWindow" => acc1(scope, proto, member, cb_frame_content_window_get, None),
        _ => return false,
    }
    true
}

pub fn install_html_iframe_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if install_frame_shared(scope, proto, member) {
        return;
    }
    match member.name {
        // The generic reflection only stored the attribute, so `iframe.src = url` produced no
        // document and no load event; the setter is browsing_context.rs's, which sets the attribute
        // itself and then loads.
        "src" => acc2(scope, proto, member, cb_reflect_url_get, Some("src"), cb_frame_src_set, None),
        // A navigation too -- to `about:srcdoc`, with the attribute's own markup as the document. No
        // fetch is involved, so unlike `src` this loads immediately.
        "srcdoc" => acc2(scope, proto, member, cb_reflect_str_get, Some("srcdoc"), cb_frame_srcdoc_set, None),
        "sandbox" => token_list_row(scope, proto, member),
        // Null for the same reason it is null on <object>: no nested browsing context exists to hold
        // a document, and a real Chrome answers null for any iframe that has not loaded an SVG.
        "getSVGDocument" => method_row(scope, proto, member, cb_null_get),
        // The CONTAINER policy -- this element's `allow` over the document's
        // own policy. `<frame>` has no `allow` attribute and no `featurePolicy` row.
        "featurePolicy" => acc1(scope, proto, member, cb_element_feature_policy, None),
        other => debug_assert!(false, "install_html_iframe_element: unexpected member {other}"),
    }
}

pub fn install_html_frame_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if !install_frame_shared(scope, proto, member) {
        debug_assert!(false, "install_html_frame_element: unexpected member {}", member.name);
    }
}

webapi::interface! {
    "HTMLIFrameElement" : "HTMLElement" [element, window] {
        ctor   "HTMLIFrameElement"                len 0                = stub,
        attr   "src"                 : Object cfg "src"                = install_html_iframe_element,
        attr   "srcdoc"              : Object cfg "srcdoc"             = install_html_iframe_element,
        attr   "name"                : Object cfg "name"               = reflect_str,
        attr   "sandbox"             : Object cfg "sandbox"            = install_html_iframe_element,
        attr   "allowFullscreen"     : Object cfg "allowfullscreen"    = reflect_bool,
        attr   "width"               : Object cfg "width"              = reflect_str,
        attr   "height"              : Object cfg "height"             = reflect_str,
        attr   "contentDocument"     : Object readonly nullable        = install_html_iframe_element,
        attr   "contentWindow"       : Object readonly nullable        = install_html_iframe_element,
        attr   "referrerPolicy"      : Object cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||" = reflect_enum,
        attr   "csp"                 : Object cfg "csp"                = reflect_str,
        attr   "allow"               : Object cfg "allow"              = reflect_str,
        attr   "featurePolicy"       : Object readonly                 = install_html_iframe_element,
        attr   "loading"             : Object cfg "loading|lazy,eager|auto|auto" = reflect_enum,
        attr   "align"               : Object cfg "align"              = reflect_str,
        attr   "scrolling"           : Object cfg "scrolling"          = reflect_str,
        attr   "frameBorder"         : Object cfg "frameborder"        = reflect_str,
        attr   "longDesc"            : Object cfg "longdesc"           = reflect_url,
        attr   "marginHeight"        : Object cfg "marginheight"       = reflect_str,
        attr   "marginWidth"         : Object cfg "marginwidth"        = reflect_str,
        method "getSVGDocument"      : Object len 0 nullable           = install_html_iframe_element,
        attr   "credentialless"      : Object cfg "credentialless"     = reflect_bool,
        attr   "allowPaymentRequest" : Object cfg "allowpaymentrequest" = reflect_bool,
        attr   "adAuctionHeaders"    : Object cfg "adauctionheaders"   = reflect_bool,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. `privateToken`
        // and `browsingTopics` are `[Reflect]` in Blink's IDL (Chrome 150
        // reads back `(f.privateToken = "x", f.privateToken)` as "x").
        attr   "privateToken"        : Object cfg "privatetoken"       = reflect_str,
        attr   "browsingTopics"      : Object cfg "browsingtopics"     = reflect_bool,
        attr   "sharedStorageWritable" : Object cfg "sharedstoragewritable" = reflect_bool
    }
}

webapi::interface! {
    "HTMLFrameElement" : "HTMLElement" [element, window] {
        ctor   "HTMLFrameElement"                 len 0                = stub,
        attr   "name"                : Object cfg "name"               = reflect_str,
        attr   "scrolling"           : Object cfg "scrolling"          = reflect_str,
        attr   "src"                 : Object cfg "src"                = reflect_url,
        attr   "frameBorder"         : Object cfg "frameborder"        = reflect_str,
        attr   "longDesc"            : Object cfg "longdesc"           = reflect_url,
        attr   "noResize"            : Object cfg "noresize"           = reflect_bool,
        attr   "contentDocument"     : Object readonly nullable        = install_html_frame_element,
        attr   "contentWindow"       : Object readonly nullable        = install_html_frame_element,
        attr   "marginHeight"        : Object cfg "marginheight"       = reflect_str,
        attr   "marginWidth"         : Object cfg "marginwidth"        = reflect_str
    }
}

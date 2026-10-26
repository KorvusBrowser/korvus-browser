//! `core/html/html_anchor_element.rs` -- `HTMLAnchorElement` as one `interface!` table.
//!
//! `dom/09d-anchor-element.js` (126 lines, 3 `KB.__` bridge slots -- `__anchorTextGet`,
//! `__anchorTextSet`, `__anchorToString` -- behind `op_helper_get`/`op_helper_set`/
//! `op_helper_method` native masks) is gone, and so is `HTMLAnchorElement`'s half of
//! `dom/08c-collections.js`'s HyperlinkElementUtils loop (`<area>` keeps it until its own lane).
//! Every row below is mounted by THE installer (`install::live::install_window_small_elements`), with names,
//! order, kinds, lengths, nullability and descriptor flags VERBATIM from the `HTMLAnchorElement`
//! block of `install/generated_registry.rs`.
//!
//! THE ORDER IS THE RECORDED BROWSER'S, not the WebIDL declaration order: Chrome puts the obsolete
//! partial (`coords`/`charset`/`name`/`rev`/`shape`) between `text` and the HyperlinkElementUtils
//! block, and ends with `interestForElement`, the stringifier, and `hrefTranslate`. Before this lane
//! that order was a fact about the BUILD -- three bootstrap phases installed pieces of it and one of
//! them deleted the whole set to put it back. It is a fact about the table now.
//!
//! THE URL DECOMPOSITION IS NOT REIMPLEMENTED. `origin`, `protocol`, `username`, `password`, `host`,
//! `hostname`, `port`, `pathname`, `search` and `hash` are COMPONENTS of the element's href parsed as
//! a URL, and every one of them is `core/html/hyperlink.rs`'s callback -- the same parser
//! `Location` uses. `origin` is readonly: a serialized origin has no round trip to write back.
//!
//! `text` AND `toString` are the two members no reflection or URL component expresses:
//!
//!   * `text` is the element's child text content -- the getter is `textContent` and the setter is
//!     `textContent =`, both already native on `Node.prototype`, so this forwards rather than
//!     duplicating. It is a NON-NULLABLE `DOMString`, so WebIDL's ToString runs before the write:
//!     `a.text = null` stores "null" and `a.text = undefined` stores "undefined", where `textContent`
//!     is nullable and maps both to "".
//!   * `toString` is the `stringifier` on `href`: `String(a)` and `a.toString()` are the href getter.
//!     Chrome exposes it as an OWN `toString` on the prototype (length 0), so the interface's own
//!     stringifier -- not `Object.prototype.toString` -- is what `"" + a` reaches. Its BODY moved to
//!     `core/html/hyperlink.rs` with the rest of the mixin when `<area>` got -- the
//!     other includer of `HTMLHyperlinkElementUtils` -- its own table: one algorithm, one definition
//!     site, two rows naming it.
//!
//! FOUR ROWS DIFFER FROM THE GENERATED CFG: `href` is `[ReflectSetter]` (only the setter is a bare
//! reflect; the getter parses the attribute against the document base URL, so a relative href reads
//! back absolute); `relList` is a `[SameObject, PutForwards=value, Reflect="rel"]` DOMTokenList;
//! `referrerPolicy` is enumerated with "" for both missing and invalid; and `interestForElement` /
//! `hrefTranslate` are not in the vendored WebIDL at all but Chrome ships both and the capture
//! records them -- an attr-element reference over `interestfor` and a plain reflection of
//! `hreftranslate`.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::core::dom::element::{cb_reflect_str_set, cb_reflect_url_get};
use crate::core::html::html_input_element::{acc1, acc2};
use crate::core::html::html_link_element::token_list_row;
use crate::core::html::hyperlink::{
    cb_hl_hash_get, cb_hl_hash_set, cb_hl_host_get, cb_hl_host_set, cb_hl_hostname_get, cb_hl_hostname_set,
    cb_hl_origin_get, cb_hl_password_get, cb_hl_password_set, cb_hl_pathname_get, cb_hl_pathname_set,
    cb_hl_port_get, cb_hl_port_set, cb_hl_protocol_get, cb_hl_protocol_set, cb_hl_search_get, cb_hl_search_set,
    cb_hl_username_get, cb_hl_username_set, to_string_row,
};
use crate::install::MemberDef;

/// `a.text` -- the element's child text content, forwarded to the native `textContent` getter.
fn cb_anchor_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The element's text content off the arena, not through the page-visible `textContent` getter.
    let s = match crate::page::node_id(scope, args.this().into()) {
        Some(id) => crate::page::with_host_dom(scope, |d| d.text_content(id)).unwrap_or_default(),
        None => String::new(),
    };
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `a.text = v` -- WebIDL's ToString, then the native `textContent` setter.
fn cb_anchor_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    crate::core::dom::text::set_text_content_of(scope, args.this(), &s);
}

pub fn install_html_anchor_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "relList" => token_list_row(scope, proto, member),
        "text" => acc2(scope, proto, member, cb_anchor_text_get, None, cb_anchor_text_set, None),
        "href" => acc2(scope, proto, member, cb_reflect_url_get, Some("href"), cb_reflect_str_set, Some("href")),
        "origin" => acc1(scope, proto, member, cb_hl_origin_get, None),
        "protocol" => acc2(scope, proto, member, cb_hl_protocol_get, None, cb_hl_protocol_set, None),
        "username" => acc2(scope, proto, member, cb_hl_username_get, None, cb_hl_username_set, None),
        "password" => acc2(scope, proto, member, cb_hl_password_get, None, cb_hl_password_set, None),
        "host" => acc2(scope, proto, member, cb_hl_host_get, None, cb_hl_host_set, None),
        "hostname" => acc2(scope, proto, member, cb_hl_hostname_get, None, cb_hl_hostname_set, None),
        "port" => acc2(scope, proto, member, cb_hl_port_get, None, cb_hl_port_set, None),
        "pathname" => acc2(scope, proto, member, cb_hl_pathname_get, None, cb_hl_pathname_set, None),
        "search" => acc2(scope, proto, member, cb_hl_search_get, None, cb_hl_search_set, None),
        "hash" => acc2(scope, proto, member, cb_hl_hash_get, None, cb_hl_hash_set, None),
        // The hyperlink stringifier, shared with `<area>` -- ONE body, in the mixin's own file, aimed
        // at THIS interface's tag (`HTMLAnchorElement.prototype.toString.call(area)` is Chrome's
        // `Illegal invocation`, measured over loopback).
        "toString" => to_string_row(scope, proto, member, "a"),
        other => debug_assert!(false, "install_html_anchor_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLAnchorElement" : "HTMLElement" [element, window] {
        ctor   "HTMLAnchorElement"              len 0                = stub,
        attr   "download"           : Object cfg "download"          = reflect_str,
        attr   "ping"               : Object cfg "ping"              = reflect_str,
        attr   "rel"                : Object cfg "rel"               = reflect_str,
        attr   "relList"            : Object cfg "rel"               = install_html_anchor_element,
        attr   "referrerPolicy"     : Object cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||" = reflect_enum,
        attr   "text"               : Object                         = install_html_anchor_element,
        attr   "coords"             : Object cfg "coords"            = reflect_str,
        attr   "charset"            : Object cfg "charset"           = reflect_str,
        attr   "name"               : Object cfg "name"              = reflect_str,
        attr   "rev"                : Object cfg "rev"               = reflect_str,
        attr   "shape"              : Object cfg "shape"             = reflect_str,
        attr   "target"             : Object cfg "target"            = reflect_str,
        attr   "href"               : Object cfg "href"              = install_html_anchor_element,
        attr   "origin"             : Object readonly                = install_html_anchor_element,
        attr   "protocol"           : Object                         = install_html_anchor_element,
        attr   "username"           : Object                         = install_html_anchor_element,
        attr   "password"           : Object                         = install_html_anchor_element,
        attr   "host"               : Object                         = install_html_anchor_element,
        attr   "hostname"           : Object                         = install_html_anchor_element,
        attr   "port"               : Object                         = install_html_anchor_element,
        attr   "pathname"           : Object                         = install_html_anchor_element,
        attr   "search"             : Object                         = install_html_anchor_element,
        attr   "hash"               : Object                         = install_html_anchor_element,
        attr   "hreflang"           : Object cfg "hreflang"          = reflect_str,
        attr   "type"               : Object cfg "type"              = reflect_str,
        attr   "interestForElement" : Object cfg "interestfor"       = reflect_element,
        method "toString"           : Object len 0                   = install_html_anchor_element,
        attr   "hrefTranslate"      : Object cfg "hreftranslate"     = reflect_str,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. D2 stubs.
        attr   "attributionSrc"     : Any                            = stub
    }
}

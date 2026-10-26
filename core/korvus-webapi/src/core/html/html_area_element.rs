//! `core/html/html_area_element.rs` -- `HTMLAreaElement` as one `interface!` table.
//!
//! `<area>` was the LAST interface split across two bootstrap phases: `dom/09c-small-elements.js`
//! installed its twelve non-URL members (and a `KB.__areaToString` bridge slot behind an
//! `op_helper_method` native mask), while `dom/08c-collections.js`'s HyperlinkElementUtils loop
//! installed the ten URL components plus `href`. Both are gone. Every row below is mounted by THE
//! installer (`install::live::install_window_element_refs`), with names, order, kinds, lengths, nullability
//! and descriptor flags VERBATIM from the `HTMLAreaElement` block of `install/generated_registry.rs`,
//! which is byte-for-byte the capture bundle's member list.
//!
//! THE ORDER IS THE RECORDED BROWSER'S. Chrome interleaves the mixin: the obsolete/geometry members
//! first (`alt`, `coords`, `download`, `shape`, `ping`, `rel`, `relList`, `referrerPolicy`), then the
//! whole HyperlinkElementUtils block from `origin` to `hash`, then `noHref` and `target`, THEN
//! `href`, and finally `interestForElement` and the stringifier. Before this lane that order was a
//! fact about which bootstrap phase ran last; it is a fact about this table now.
//!
//! NOTHING IS REIMPLEMENTED HERE. There is no `install_html_area_element` at all:
//!
//!   * The ten URL components are `core/html/hyperlink.rs`'s callbacks -- the SAME parser
//!     `HTMLAnchorElement` and `Location` use. `<a>` and `<area>` are the two includers of
//!     `HTMLHyperlinkElementUtils`, and there is one body per component for both.
//!   * `href` is `[ReflectURL]`: the getter resolves the content attribute against the document base
//!     URL, the setter writes the attribute back verbatim. That pair IS `kits::reflect_url`, so the
//!     row names the kit rather than a hand branch (`HTMLAnchorElement` spells the same pair out by
//!     hand for a `[ReflectSetter]` reason that does not apply here).
//!   * `toString` is the mixin's stringifier, `hyperlink::to_string_row` -- the same body
//!     `HTMLAnchorElement`'s row names, aimed at the `area` tag. Two definition sites for one
//!     algorithm is exactly what this goal exists to remove, so `<area>` got a ROW, not a copy.
//!   * `relList` is `[SameObject, PutForwards=value, Reflect="rel"]`, which is `kits::reflect_tokens`;
//!     `referrerPolicy` is the enumerated reflection with "" for both missing and invalid;
//!     `interestForElement` is an attr-element reference over `interestfor`; `noHref` is the boolean
//!     reflection of the obsolete `nohref`.
//!
//! `origin` is readonly -- a serialized origin has no round trip to write back -- which is what the
//! capture records and what the row says.

use deno_core::v8;

use crate::core::html::html_input_element::{acc1, acc2};
use crate::core::html::hyperlink::{
    cb_hl_hash_get, cb_hl_hash_set, cb_hl_host_get, cb_hl_host_set, cb_hl_hostname_get, cb_hl_hostname_set,
    cb_hl_origin_get, cb_hl_password_get, cb_hl_password_set, cb_hl_pathname_get, cb_hl_pathname_set,
    cb_hl_port_get, cb_hl_port_set, cb_hl_protocol_get, cb_hl_protocol_set, cb_hl_search_get, cb_hl_search_set,
    cb_hl_username_get, cb_hl_username_set, to_string_row,
};
use crate::install::MemberDef;

/// The eleven rows that are not a reflection: the ten URL components of `href`, and the stringifier.
/// Every body is `core/html/hyperlink.rs`'s, so this only says which row is which -- the interface
/// gets a table and no second implementation.
pub fn install_html_area_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
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
        "toString" => to_string_row(scope, proto, member, "area"),
        other => debug_assert!(false, "install_html_area_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLAreaElement" : "HTMLElement" [element, window] {
        ctor   "HTMLAreaElement"                len 0                = stub,
        attr   "alt"                : Object cfg "alt"               = reflect_str,
        attr   "coords"             : Object cfg "coords"            = reflect_str,
        attr   "download"           : Object cfg "download"          = reflect_str,
        attr   "shape"              : Object cfg "shape"             = reflect_str,
        attr   "ping"               : Object cfg "ping"              = reflect_str,
        attr   "rel"                : Object cfg "rel"               = reflect_str,
        attr   "relList"            : Object cfg "rel"               = reflect_tokens,
        attr   "referrerPolicy"     : Object cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||" = reflect_enum,
        attr   "origin"             : Object readonly                = install_html_area_element,
        attr   "protocol"           : Object                         = install_html_area_element,
        attr   "username"           : Object                         = install_html_area_element,
        attr   "password"           : Object                         = install_html_area_element,
        attr   "host"               : Object                         = install_html_area_element,
        attr   "hostname"           : Object                         = install_html_area_element,
        attr   "port"               : Object                         = install_html_area_element,
        attr   "pathname"           : Object                         = install_html_area_element,
        attr   "search"             : Object                         = install_html_area_element,
        attr   "hash"               : Object                         = install_html_area_element,
        attr   "noHref"             : Object cfg "nohref"            = reflect_bool,
        attr   "target"             : Object cfg "target"            = reflect_str,
        attr   "href"               : Object cfg "href"              = reflect_url,
        attr   "interestForElement" : Object cfg "interestfor"       = reflect_element,
        method "toString"           : Object len 0                   = install_html_area_element,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. D2 stubs.
        attr   "attributionSrc"     : Any                            = stub
    }
}

//! `core/html/html_link_element.rs` -- `HTMLLinkElement` as one `interface!` table.
//!
//! `dom/08u-link-element.js` (79 lines, 20 members behind one `Object.defineProperty` shape, one
//! `op_helper_get` native mask) is gone. Every row below is mounted by THE installer
//! (`install::live::install_window_small_elements`), with names, order, kinds, lengths, nullability and
//! descriptor flags VERBATIM from the `HTMLLinkElement` block of `install/generated_registry.rs` --
//! Chrome's own prototype order, which the bundle's `surface.json` records.
//!
//! THREE DOMTokenLists -- `relList`, `sizes`, `blocking` -- over three different content attributes.
//! `[PutForwards=value]` means assigning to the property assigns to the list's `value`, which IS
//! writing the attribute, hence the plain string setter beside each list getter. The list builder is
//! shared with `Element.classList` (`core/dom/element.rs`'s `cb_el_token_list`), which carries the
//! attribute name on the list object, so there is one token-list implementation and not four.
//!
//! FOUR ROWS DIFFER FROM THE GENERATED CFG, each an enumerated or token reflection webref does not
//! describe well enough for `gen_reflected.py` to emit, and each reproducing exactly what the deleted
//! JS installed:
//!
//!   * `crossOrigin` is a NULLABLE enumerated reflection: null while the attribute is absent.
//!   * `as`'s keyword set is Blink's own, from the vendored `html_link_element.idl`
//!     (`[ReflectOnly=("script","style","image","track","font","fetch")]`). That vendored tree is
//!     older than the surface baseline and a newer Chrome accepts more destinations; the recordings
//!     pin down only the INVALID path (`link.as = "x"` reads back ""), which every version agrees
//!     on, so this follows the one list that can be cited rather than a guess at the current one.
//!   * `referrerPolicy` and `fetchPriority` are enumerated with "" and "auto" missing-defaults.
//!
//! `sheet` IS A REAL ROW. It was a `stub` whose body the kernel's `BOOTSTRAP` string
//! supplied by re-defining `HTMLLinkElement.prototype.sheet` after this lane had installed it --
//! `Object.defineProperty` on an existing key does not MOVE it, so the stub row put `sheet` in the
//! capture's position (between `target` and `integrity`) and the later define supplied the body.
//! That is why this lane was pinned to install BEFORE the bootstrap script; installed after it, the
//! stub would have WON and `link.sheet` would have gone undefined. The body is
//! `core/css/style_sheet.rs`'s `cb_element_sheet` now, named by the row, and the ordering contract
//! is spent (the install has not moved -- moving one is a measurement).

use deno_core::v8;

use crate::core::dom::element::cb_el_token_list;
use crate::install::MemberDef;

/// A `[SameObject, PutForwards=value]` DOMTokenList row: the getter hands back the same list every
/// time (viewing `cfg`'s attribute), the setter writes that attribute, which is what `list.value = x`
/// does. Shared with `<iframe>`'s `sandbox` and `<a>`'s `relList`.
pub(crate) fn token_list_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let attr = if member.cfg.is_empty() { member.name } else { member.cfg };
    // `[PutForwards=value]` on every row that reaches here (relList, sizes, blocking, sandbox).
    let d = crate::binding::protos::str_val(scope, attr);
    let getter = crate::binding::protos::build_fn_data(scope, cb_el_token_list, &format!("get {}", member.name), 0, d);
    crate::install::kits::put_forwards(scope, proto, member, getter, "value");
}

pub fn install_html_link_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "relList" | "sizes" | "blocking" => token_list_row(scope, proto, member),
        // `<link>.sheet` -- the SAME body `<style>.sheet` has; the LinkStyle mixin is one member.
        "sheet" => crate::install::live::getter_row(scope, proto, member, crate::core::css::style_sheet::cb_element_sheet),
        other => debug_assert!(false, "install_html_link_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLLinkElement" : "HTMLElement" [element, window] {
        ctor   "HTMLLinkElement"                len 0                  = stub,
        attr   "disabled"       : Object cfg "disabled"                = reflect_bool,
        attr   "href"           : Object cfg "href"                    = reflect_url,
        attr   "crossOrigin"    : Object nullable cfg "crossorigin|anonymous,use-credentials|anonymous" = reflect_nullable_enum,
        attr   "rel"            : Object cfg "rel"                     = reflect_str,
        attr   "relList"        : Object cfg "rel"                     = install_html_link_element,
        attr   "media"          : Object cfg "media"                   = reflect_str,
        attr   "hreflang"       : Object cfg "hreflang"                = reflect_str,
        attr   "type"           : Object cfg "type"                    = reflect_str,
        attr   "as"             : Object cfg "as|script,style,image,track,font,fetch||" = reflect_enum,
        attr   "referrerPolicy" : Object cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||" = reflect_enum,
        attr   "sizes"          : Object cfg "sizes"                   = install_html_link_element,
        attr   "fetchPriority"  : Object cfg "fetchpriority|high,low,auto|auto|auto" = reflect_enum,
        attr   "imageSrcset"    : Object cfg "imagesrcset"             = reflect_str,
        attr   "imageSizes"     : Object cfg "imagesizes"              = reflect_str,
        attr   "charset"        : Object cfg "charset"                 = reflect_str,
        attr   "rev"            : Object cfg "rev"                     = reflect_str,
        attr   "target"         : Object cfg "target"                  = reflect_str,
        attr   "sheet"          : Object readonly nullable             = install_html_link_element,
        attr   "integrity"      : Object cfg "integrity"               = reflect_str,
        attr   "blocking"       : Object cfg "blocking"                = install_html_link_element
    }
}

//! `core/html/html_template_element.rs` -- `HTMLTemplateElement` as one `interface!` table.
//!
//! WHY THIS INTERFACE WAS THE REASON `dom/09-postshells.js` EXISTED AT ALL. That file's header names
//! it: the shell pass created `HTMLTemplateElement` (and its `content` accessor stub) only at the END
//! of the bootstrap, so a native `content` had to be installed after it -- which is what made the
//! module the extension's last ESM entry. With the interface as a table the ordering problem is gone
//! rather than accommodated: THE installer builds the prototype from these rows, the shell that had to
//! be replaced is never created, and nothing has to run after anything.
//!
//! The nine rows are the capture's, verbatim, in the capture's order (Chrome 152's:
//! `content, shadowRootMode, shadowRootDelegatesFocus, shadowRootClonable, shadowRootSerializable,
//! shadowRootSlotAssignment, shadowRootCustomElementRegistry, htmlFor, shadowRootReferenceTarget`). Five keep the body the generated registry gave them --
//! three `reflect_bool` and one `reflect_str` with their recorded `cfg`s, and `htmlFor` an honest
//! `stub` -- so the cutover moves the definition SITE without changing a single answer for them.
//!
//! THE TWO THIS LANE GIVES BODIES:
//!
//! * `content` is a real native getter over the arena (`core/dom/element.rs`'s
//!   `cb_template_content_get`): create-or-return the template's `DocumentFragment`, the same object
//!   on every read.
//! * `shadowRootMode` is NOT a plain `[Reflect]` string, which is why the generator left it a stub.
//!   The setter writes the content attribute verbatim and the GETTER limits it to the enumerated
//!   values, so the pair does not round-trip: Chrome 150, recorded, answers `"open"` after
//!   `t.shadowRootMode = "OPEN"` while `getAttribute("shadowrootmode")` still reads `"OPEN"`, and
//!   answers `""` after `t.shadowRootMode = "zz"` while the attribute reads `"zz"`. There is no extra
//!   internal slot -- an unrecognised value, or no attribute at all, is `""` both ways. The parser is
//!   the only thing that ever turns this attribute into a real shadow root; assigning the IDL
//!   attribute afterwards never does (WPT declarative-shadow-dom-basic.html, "Shadowrootmode
//!   reflection, setter").

use deno_core::v8;

use crate::binding::protos::{build_fn_data, str_val};
use crate::core::dom::element::{cb_reflect_enum_get, cb_reflect_str_set, cb_template_content_get};
use crate::core::dom::shared::row_getter;
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

/// The `shadowrootmode` content attribute, its keyword list, and the missing/invalid defaults -- the
/// `attr|keywords|missing|invalid` config `cb_reflect_enum_get` reads. Both defaults are `""`.
const SHADOW_ROOT_MODE_CFG: &str = "shadowrootmode|open,closed||";

/// The two rows this lane owns; the other five name their registry kit directly in the table.
pub fn install_html_template_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "content" => row_getter(scope, proto, member, cb_template_content_get),
        "shadowRootMode" => {
            let gdata = str_val(scope, SHADOW_ROOT_MODE_CFG);
            let g = build_fn_data(scope, cb_reflect_enum_get, "get shadowRootMode", 0, gdata);
            let sdata = str_val(scope, "shadowrootmode");
            let s = build_fn_data(scope, cb_reflect_str_set, "set shadowRootMode", 1, sdata);
            define_accessor(scope, proto, member, g, Some(s));
        }
        other => debug_assert!(false, "install_html_template_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLTemplateElement" : "HTMLElement" [element, window] {
        ctor "HTMLTemplateElement" len 0 = stub,
        attr "content" : Any readonly = install_html_template_element,
        attr "shadowRootMode" : Any = install_html_template_element,
        attr "shadowRootDelegatesFocus" : Any cfg "shadowrootdelegatesfocus" = reflect_bool,
        attr "shadowRootClonable" : Any cfg "shadowrootclonable" = reflect_bool,
        attr "shadowRootSerializable" : Any cfg "shadowrootserializable" = reflect_bool,
        // Chrome 152: `[Reflect, ReflectOnly=("named","manual"), ReflectMissing="named", ReflectInvalid="named"]`.
        attr "shadowRootSlotAssignment" : Object cfg "shadowrootslotassignment|named,manual|named|named" = reflect_enum,
        attr "shadowRootCustomElementRegistry" : Any cfg "shadowrootcustomelementregistry" = reflect_str,
        // Chrome 150 answers `""` (a String of length 0) and the generated registry gave it no body.
        // It is NOT `<label for>`'s reflection -- `HTMLTemplateElement` has no `for` content attribute
        // -- and guessing which attribute it mirrors would be the invention this campaign deletes, so
        // it stays an honest stub, counted in tools/status/data/layers.json, exactly as it was.
        attr "htmlFor" : Any = stub,
        // Chrome 152: `[Reflect] attribute DOMString?`, last on the prototype.
        attr "shadowRootReferenceTarget" : Object nullable cfg "shadowrootreferencetarget" = reflect_nullable_str
    }
}

//! `core/html/html_title_element.rs` — `HTMLTitleElement` as one `interface!` table.
//!
//! One member, `text`, and it was a registry `stub` answering `undefined`. HTML defines it over the
//! element's CHILD text content -- not `textContent`, which is the descendant text -- and its setter
//! is "string replace all", which is exactly `textContent = v`. Measured on stock Chrome
//! 150.0.7871.46: a `<title>` holding `" A "`, `<b>BOLD</b>`, `" C "` reads `text` `" A  C "`
//! (unstripped: the strip-and-collapse is `document.title`'s, `core/dom/document.rs`), and
//! `text = "z"` leaves one child.

use deno_core::v8;

use crate::binding::protos::{str_val, unwrap_id};
use crate::binding::HostDom;
use crate::install::MemberDef;

fn cb_title_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = {
        let st = op_state.borrow();
        crate::core::dom::document::child_text_content(&st.borrow::<HostDom>().0, id)
    };
    let v = str_val(scope, &s);
    rv.set(v);
}

/// "Set the child text content" by the realm's own steps -- this assigned through the page's `textContent`
/// setter.
fn cb_title_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(v) = args.get(0).to_string(scope) else { return };
    let v = v.to_rust_string_lossy(scope);
    crate::core::dom::text::set_text_content_of(scope, args.this(), &v);
}

pub fn install_html_title_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "text" => crate::core::dom::shared::row_accessor(scope, proto, member, cb_title_text_get, cb_title_text_set),
        other => eprintln!("[korvus] title element install gap: {other}"),
    }
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLTitleElement` block of `install/generated_registry.rs`.
    "HTMLTitleElement" : "HTMLElement" [element, window] {
        ctor "HTMLTitleElement"          len 0 = stub,
        attr "text" : DomString                = install_html_title_element
    }
}

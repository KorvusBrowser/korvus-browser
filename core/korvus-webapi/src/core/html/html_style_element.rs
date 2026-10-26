//! `core/html/html_style_element.rs` — `HTMLStyleElement` as one `interface!` table.
//!
//! Five rows, and the only one the deleted `dom/08b-reflected.js` mounted is `disabled`, which is an
//! ordinary `[Reflect] boolean` over the `disabled` content attribute — so it is the shared
//! `reflect_bool` kit with the `cfg` the JS passed, not a hand body. The registry's own row was a
//! stub, which is why the JS had to mount anything at all.
//!
//! `sheet` IS A REAL ROW. It was an honest `stub` whose body the kernel's `BOOTSTRAP`
//! string supplied by DECORATING this prototype afterwards — which is why this table's install had to
//! run BEFORE the bootstrap script (a define does not move an existing key, so the stub row put
//! `sheet` in the capture's position and the later define supplied the body). The body is
//! `core/css/style_sheet.rs`'s `cb_element_sheet` now, named by this row, and that install-order
//! contract is spent.

use deno_core::v8;

use crate::install::MemberDef;

/// The one row of this table with a hand body: `sheet`. The other four are shared kits.
pub fn install_html_style_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "sheet" => crate::install::live::getter_row(scope, proto, member, crate::core::css::style_sheet::cb_element_sheet),
        "disabled" => crate::install::live::pair_row(scope, proto, member, cb_style_disabled_get, cb_style_disabled_set),
        other => eprintln!("[korvus] style element install gap: {other}"),
    }
}

/// `disabled` is NOT a reflected attribute (the Chrome oracle ratchet): Blink's
/// `HTMLStyleElement::disabled()` / `SVGStyleElement::disabled()` answer the SHEET's flag -- `false`
/// with no sheet -- and the setter disables the sheet when there is one, else does nothing. Chrome 150:
/// a detached `<style>` with `disabled = true` still reads `false`, and no `disabled` attribute appears.
fn cb_style_disabled_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let b = crate::core::css::style_sheet::element_sheet(scope, this).map(|s| crate::core::css::style_sheet::sheet_disabled(scope, s)).unwrap_or(false);
    rv.set_bool(b);
}
fn cb_style_disabled_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let b = args.get(0).boolean_value(scope);
    if let Some(s) = crate::core::css::style_sheet::element_sheet(scope, this) {
        crate::core::css::style_sheet::set_sheet_disabled(scope, s, b);
    }
}

/// `SVGStyleElement`'s `type` and `media`: the reflected attribute, `""` when it is ABSENT -- plain reflections,
/// as the HTML element's are (`SVGStyleElement::type()` / `media()` under Chromium 152's stable
/// `SvgStyleElementReflectTypeAndMedia`; Chromium 150 answered its defaults, `"text/css"` and `"all"`).
pub fn install_svg_style_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let name = crate::binding::protos::str_val(scope, member.name);
    let g = crate::binding::protos::build_fn_data(scope, cb_svg_style_get, &format!("get {}", member.name), 0, name);
    let name = crate::binding::protos::str_val(scope, member.name);
    let s = crate::binding::protos::build_fn_data(scope, cb_svg_style_set, &format!("set {}", member.name), 1, name);
    crate::install::kits::define_accessor(scope, proto, member, g, Some(s));
}
fn cb_svg_style_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let k = crate::binding::protos::str_val(scope, &attr);
    let got = crate::core::dom::element::attr_call(scope, args.this().into(), "getAttribute", &[k]).filter(|v| !v.is_null_or_undefined());
    match got {
        Some(v) => rv.set(v),
        None => {
            let d = crate::binding::protos::str_val(scope, "");
            rv.set(d);
        }
    }
}
fn cb_svg_style_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(v) = args.get(0).to_string(scope) else { return };
    let k = crate::binding::protos::str_val(scope, &attr);
    crate::core::dom::element::attr_call(scope, args.this().into(), "setAttribute", &[k, v.into()]);
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLStyleElement` block of `install/generated_registry.rs`.
    "HTMLStyleElement" : "HTMLElement" [element, window] {
        ctor "HTMLStyleElement"                       len 0 = stub,
        attr "disabled" : Boolean                           = install_html_style_element,
        attr "media"    : DomString cfg "media"             = reflect_str,
        attr "type"     : DomString cfg "type"              = reflect_str,
        attr "sheet"    : Object    readonly nullable       = install_html_style_element,
        attr "blocking" : Object    cfg "blocking"          = reflect_tokens
    }
}

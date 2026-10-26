//! `core/html/html_button_element.rs` — the one member of `HTMLButtonElement` that no generic
//! reflection expresses.
//!
//! Everything else the interface has is either a generic reflected attribute (`core/dom/element.rs`)
//! or a constraint-validation member shared with `<input>` (`html_input_element.rs`, exposed there
//! under names without `input` in them). `command` is the exception: it is neither a plain string
//! reflection nor a plain enumerated one.

use deno_core::v8;
use crate::binding::protos::{str_val};

/// The built-in command keywords. A value outside this set reflects as the empty string — EXCEPT a
/// custom command, which the spec defines as any value starting with `--` and which reflects
/// verbatim so that a page can dispatch on its own names.
const COMMANDS: [&str; 6] = [
    "toggle-popover", "show-popover", "hide-popover", "close", "request-close", "show-modal",
];

/// `command` — an enumerated reflection with an escape hatch.
///
/// What the recordings pin down (behavior-cases/HTMLButtonElement.json) is the invalid path:
/// `button.command` is `""` with no attribute, and stays `""` after `button.command = "x"`. The
/// keyword and `--custom` paths are not recorded — Chrome shipped `command` after the Blink source
/// vendored under `reference/` was cut, so there is nothing to read there either — and they follow
/// the HTML standard's own definition rather than a guess: a known keyword reflects lowercased, a
/// `--`-prefixed value reflects as-is, anything else is `""`.
pub(crate) fn cb_button_command_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let a = str_val(scope, "command");
    let raw = match crate::core::dom::element::attr_call(scope, args.this().into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => { let e = str_val(scope, ""); rv.set(e); return; }
    };
    if raw.starts_with("--") { let v = str_val(scope, &raw); rv.set(v); return; }
    let lower = raw.trim().to_ascii_lowercase();
    let out = if COMMANDS.contains(&lower.as_str()) { lower } else { String::new() };
    let v = str_val(scope, &out);
    rv.set(v);
}

// This file registers no `op_*`: `command`'s getter is a raw callback the `HTMLButtonElement`
// table names directly, so nothing needs a JS-facing builder.

// ── the table ───────────────────────────────────────────────────────────────
//
// `dom/08p-button-element.js` (87 lines) is gone. Its 22 members are the rows below, mounted by THE
// installer (`install::live::install_window_small_elements`). Names, order, kinds, lengths, `required` counts,
// nullability and descriptor flags are VERBATIM from the `HTMLButtonElement` block of
// `install/generated_registry.rs` -- Chrome's own prototype order, which is NOT the WebIDL
// declaration order (the three methods come last, `commandForElement` before `command`, and
// `interestForElement` between `command` and `checkValidity`).
//
// The constraint-validation half is `html_input_element.rs`'s `install_form_control_shared` -- the
// SAME callbacks `<input>` uses, which branch on the element rather than assuming an input, so a
// `<button>` has no required/pattern/min/max semantics and `customError` is the only constraint it
// can fail. Sharing the body rather than copying it is what keeps the two interfaces from drifting.
//
// THREE ROWS DIFFER FROM THE GENERATED CFG: `type` is an ENUMERATED reflection whose missing AND
// invalid default is "submit" (webref carries no `[ReflectOnly]`, so the generator emitted a plain
// `reflect_str`, which would make `b.type = "x"` read back "x" where Chrome answers "submit");
// `popoverTargetAction` is likewise enumerated; and `interestForElement` is an attr-ELEMENT
// reference over `interestfor` (not in the vendored WebIDL at all, but Chrome ships it and the
// capture records it).

pub fn install_html_button_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::core::dom::element::cb_reflect_str_set;
    use crate::core::html::html_input_element::{acc2, install_form_control_shared};
    if install_form_control_shared(scope, proto, member) {
        return;
    }
    match member.name {
        // `command`'s getter is the one member here no generic reflection expresses (see above); the
        // SETTER is a bare reflect, so an unknown command stores verbatim and reads back "".
        "command" => acc2(scope, proto, member, cb_button_command_get, None, cb_reflect_str_set, Some("command")),
        other => debug_assert!(false, "install_html_button_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLButtonElement" : "HTMLElement" [element, window] {
        ctor   "HTMLButtonElement"                len 0                 = stub,
        attr   "disabled"             : Object cfg "disabled"           = reflect_bool,
        attr   "form"                 : Object readonly nullable        = install_html_button_element,
        attr   "formAction"           : Object cfg "formaction"         = reflect_str,
        attr   "formEnctype"          : Object cfg "formenctype|application/x-www-form-urlencoded,multipart/form-data,text/plain||application/x-www-form-urlencoded" = reflect_enum,
        attr   "formMethod"           : Object cfg "formmethod|get,post,dialog||get" = reflect_enum,
        attr   "formNoValidate"       : Object cfg "formnovalidate"     = reflect_bool,
        attr   "formTarget"           : Object cfg "formtarget"         = reflect_str,
        attr   "name"                 : Object cfg "name"               = reflect_str,
        attr   "type"                 : Object cfg "type|submit,reset,button|submit|submit" = reflect_enum,
        attr   "value"                : Object cfg "value"              = reflect_str,
        attr   "willValidate"         : Object readonly                 = install_html_button_element,
        attr   "validity"             : Object readonly                 = install_html_button_element,
        attr   "validationMessage"    : Object readonly                 = install_html_button_element,
        attr   "labels"               : Object readonly                 = install_html_button_element,
        attr   "popoverTargetElement" : Object nullable cfg "popovertarget" = reflect_element,
        attr   "popoverTargetAction"  : Object cfg "popovertargetaction|toggle,show,hide|toggle|toggle" = reflect_enum,
        attr   "commandForElement"    : Object nullable cfg "commandfor" = reflect_element,
        attr   "command"              : Object cfg "command"            = install_html_button_element,
        attr   "interestForElement"   : Object cfg "interestfor"        = reflect_element,
        method "checkValidity"        : Object len 0                    = install_html_button_element,
        method "reportValidity"       : Object len 0                    = install_html_button_element,
        method "setCustomValidity"    : Object len 1 required 1         = install_html_button_element
    }
}

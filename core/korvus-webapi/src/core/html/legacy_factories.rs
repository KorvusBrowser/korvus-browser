//! `core/html/legacy_factories.rs` -- `Audio` and `Option`, the two legacy element FACTORIES.
//!
//! WHAT THIS REPLACES. The `Image`/`Audio`/`Option` block of `dom/09i-singletons.js` -- its last
//! `op_nativize_ctor` call site, and the JS bodies behind it. (`Image` left that block in Phase 3
//! Track C; it is `install::live::install_window_html_image_element`'s, and this file is the same
//! shape for its two siblings.)
//!
//! WHAT A LEGACY FACTORY IS. A distinct function object that SHARES the element interface's
//! prototype, so `new Audio() instanceof HTMLAudioElement` holds and
//! `Object.getOwnPropertyNames(Option.prototype)` reads back HTMLOptionElement's members. The shells
//! built them the only way they could -- as ordinary interfaces with a prototype of their own -- so
//! `new Option()` threw "expected JsElement" and the capture showed members missing from
//! `Option.prototype` that were never missing at all, just on the wrong object.
//!
//! NATIVE, BY CONSTRUCTION. `template_ctor` makes a real `v8::Function` -- the instance of one template
//! per isolate -- with `ConstructorBehavior::Allow`, so `Function.prototype.toString` reads `[native code]` -- headless
//! Chrome 150.0.7871.46 on this machine prints exactly `function Audio() { [native code] }` (probed
//! 2026-09-11, over a loopback server). `op_nativize_ctor`, which the JS needed for this and which
//! this deletes the last window-realm call site of, built the same thing from a JS body.
//!
//! The constructor `length` is 0 for both, which is what the capture records
//! (`interfaces.Audio.length` and `interfaces.Option.length`) and what the deleted JS read off the
//! shell it was replacing. It is READ FROM THE ROW now rather than written here
//! (`installer::row_ctor_length`): the third factory, `Image`, had the same number written at its own
//! call site and had it wrong, so the number comes from the one place that carries it.

use deno_core::v8;

use crate::binding::protos::{global_get, pristine_call, pristine_set, str_val, template_ctor, throw_type_error};

/// `document.createElement(tag)` -- a real arena element, so every IDL accessor resolves its node id. The
/// realm's own `createElement`, and its own setters below: a factory's steps call nothing a page can wrap
/// (`new Option()` called the page's `createElement` and `text` setter).
fn create_element<'s>(scope: &mut v8::PinScope<'s, '_>, tag: &str) -> Option<v8::Local<'s, v8::Object>> {
    let document = global_get(scope, "document").filter(|v| v.is_object())?;
    let t: v8::Local<v8::Value> = str_val(scope, tag);
    let el = pristine_call(scope, document, "Document.createElement", &[t])?;
    v8::Local::<v8::Object>::try_from(el).ok()
}

fn set(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, member: &str, v: v8::Local<v8::Value>) {
    pristine_set(scope, el.into(), member, v);
}

/// `new Audio(src)` -- `<audio preload="auto">`, with `src` set when an argument was PASSED (an
/// explicit `undefined` is skipped, as the deleted JS's `src !== undefined` test skipped it).
fn cb_audio_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("Audio"));
        return;
    }
    let Some(el) = create_element(scope, "audio") else { return };
    let auto: v8::Local<v8::Value> = str_val(scope, "auto");
    set(scope, el, "HTMLMediaElement.preload", auto);
    if args.length() >= 1 && !args.get(0).is_undefined() {
        let s = args.get(0).to_rust_string_lossy(scope);
        let sv: v8::Local<v8::Value> = str_val(scope, &s);
        set(scope, el, "HTMLMediaElement.src", sv);
    }
    rv.set(el.into());
}

/// `new Option(text, value, defaultSelected, selected)`.
///
/// `value` and `defaultSelected` go through `setAttribute`, not through the IDL members: HTML's
/// factory says "set an attribute", and `option.value` with no `value` attribute reflects the TEXT,
/// so writing the IDL member would make `new Option("a")` and `new Option("a", "a")` indistinguishable
/// the moment the text changed. `selected` is the IDL member, which is the selectedness state rather
/// than the attribute. This is the deleted JS's body, unchanged.
fn cb_option_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("Option"));
        return;
    }
    let Some(el) = create_element(scope, "option") else { return };
    if args.length() >= 1 && !args.get(0).is_undefined() {
        let s = args.get(0).to_rust_string_lossy(scope);
        let sv: v8::Local<v8::Value> = str_val(scope, &s);
        set(scope, el, "HTMLOptionElement.text", sv);
    }
    if args.length() >= 2 && !args.get(1).is_undefined() {
        let s = args.get(1).to_rust_string_lossy(scope);
        let k: v8::Local<v8::Value> = str_val(scope, "value");
        let v: v8::Local<v8::Value> = str_val(scope, &s);
        crate::core::dom::element::attr_call(scope, el.into(), "setAttribute", &[k, v]);
    }
    if args.length() >= 3 && args.get(2).boolean_value(scope) {
        let k: v8::Local<v8::Value> = str_val(scope, "selected");
        let v: v8::Local<v8::Value> = str_val(scope, "");
        crate::core::dom::element::attr_call(scope, el.into(), "setAttribute", &[k, v]);
    }
    if args.length() >= 4 && args.get(3).boolean_value(scope) {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        set(scope, el, "HTMLOptionElement.selected", t);
    }
    rv.set(el.into());
}

/// Put one factory on the global: a native constructable whose non-writable `.prototype` IS the
/// element interface's prototype. From the isolate's template for it; the `prototype`
/// is the realm's, defined on the realm's instance.
fn install_factory<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, iface: &str, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let Some((proto, _)) = crate::install::live::parent_of(scope, iface) else { return };
    let f = template_ctor(scope, cb, name, crate::install::installer::row_ctor_length(name), None);
    if let Some(pk) = v8::String::new(scope, "prototype") {
        f.define_own_property(
            scope,
            pk.into(),
            proto.into(),
            v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE,
        );
    }
    {
        crate::install::installer::publish_on_global(scope, global, name, f.into());
    }
}

/// `Audio` and `Option`, overwriting the shells' same-named interfaces.
///
/// The shell's `prototype` is non-writable, so the whole FUNCTION is replaced rather than re-pointed
/// -- the same reason the deleted JS replaced it.
pub fn install_window_legacy_factories<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    install_factory(scope, global, "Audio", "HTMLAudioElement", cb_audio_ctor);
    install_factory(scope, global, "Option", "HTMLOptionElement", cb_option_ctor);
}

//! The `CSS` namespace object.
//!
//! Replaces `js/dom/06-misc.js`.

use deno_core::v8;

use crate::install::installer;

/// `globalThis.CSS` — a WebIDL NAMESPACE, so a bare object carrying its members as OWN properties.
///
/// There is no `CSS` constructor and no `CSS.prototype`: `typeof CSS` is `"object"`,
/// `Object.getPrototypeOf(CSS)` is `Object.prototype`, and `escape`/`supports` sit on the object
/// itself. `create_interface` cannot express that — it always builds a constructor — so the object is
/// minted here and its rows go on through [`installer::install_members`], which is the SAME
/// `put_member` every other table's rows take and which records them in the install ledger.
///
/// THREE SHAPE FACTS, each measured in headless Chrome 151 rather than assumed, and each one the
/// deleted object literal got wrong:
///   * `Object.prototype.toString.call(CSS)` is `[object CSS]` — the namespace object carries a
///     `Symbol.toStringTag` (non-writable, non-enumerable, configurable, like every WebIDL tag). The
///     literal answered `[object Object]`.
///   * `window.CSS` is writable, NOT enumerable, configurable. `globalThis.CSS = {…}` made it
///     enumerable, so `for (const k in window)` listed it.
///   * `String(CSS.escape)` is `[native code]`. A JS method body printed its source.
///
/// WHERE THIS RUNS: FIRST in the runtime's pre-bootstrap block, BEFORE
/// [`super::install_window_postshells`], and that position is a dependency rather than a preference —
/// `install_window_postshells` ends by calling `core::css::highlight::install_css_highlights`, which
/// reads `globalThis.CSS` and defines `highlights` on it. Installed after postshells, that read finds
/// nothing and `CSS.highlights` silently disappears. The deleted JS ran in the ESM layer, which is
/// before the whole block, so this is the same relative order.
///
/// Window-only: the bundle's worker `globals` list has no `CSS`.
pub fn install_window_css<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::css::css_supports::CSS;

    let obj = v8::Object::new(scope);
    // `highlights` is Chrome's FIRST own property (measured on 150): it is
    // installed later (`core::css::highlight::install_css_highlights`), so its slot is taken here and
    // that install redefines it in place.
    if let Some(k) = v8::String::new(scope, "highlights") {
        let u = v8::undefined(scope).into();
        obj.define_own_property(scope, k.into(), u, v8::PropertyAttribute::NONE);
    }
    let names: Vec<&str> = CSS.members.iter().map(|m| m.name).collect();
    for g in installer::install_members(scope, obj, &CSS, &names) {
        eprintln!("[korvus] CSS install gap: {}.{}", g.interface, g.member);
    }
    if let Some(tag) = CSS.tag {
        let Some(tv) = v8::String::new(scope, tag) else { return };
        let mut d = v8::PropertyDescriptor::new_from_value_writable(tv.into(), false);
        d.set_enumerable(false);
        d.set_configurable(true);
        let key = v8::Symbol::get_to_string_tag(scope);
        obj.define_property(scope, key.into(), &d);
    }
    // ... and `paintWorklet` its LAST.
    crate::core::css::typed_om::install_paint_worklet(scope, obj);
    let Some(k) = v8::String::new(scope, "CSS") else { return };
    global.define_own_property(
        scope,
        k.into(),
        obj.into(),
        v8::PropertyAttribute::DONT_ENUM,
    );
}

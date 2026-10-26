//! The `Window` / `Document` surface: the `[Global]` rows, the named-properties splice, the plugin
//! and speech singletons, and the legacy element factories.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

/// Splice the Window named-properties object into the prototype chain (`dom/09h-window-named.js`,
/// deleted). The whole body is [`crate::core::dom::window_named::splice_window_named`]; this is the
/// public entry `korvus-runtime` calls, because `crate::core` is `pub(crate)`.
pub fn install_window_named_properties<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::dom::window_named::splice_window_named(scope, global);
}

/// `PluginArray` / `Plugin` / `MimeTypeArray` / `MimeType` and the two `navigator` singletons over
/// them (`dom/09i-singletons.js`'s plugin block). The whole body is
/// [`crate::core::html::plugins::install_window_plugins`]; this is the public entry
/// `korvus-runtime` calls, because `crate::core` is `pub(crate)`.
pub fn install_window_plugins<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::html::plugins::install_window_plugins(scope, global);
}

/// `SpeechSynthesis` and the `speechSynthesis` singleton (`dom/09i-singletons.js`'s speech block).
/// The whole body is [`crate::core::speech::speech_synthesis::install_window_speech_synthesis`].
pub fn install_window_speech_synthesis<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::speech::speech_synthesis::install_window_speech_synthesis(scope, global);
}

/// The legacy `Audio` / `Option` element factories (`dom/09i-singletons.js`'s factory block, and its
/// last `op_nativize_ctor` call site). The whole body is
/// [`crate::core::html::legacy_factories::install_window_legacy_factories`].
pub fn install_window_legacy_factories<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::html::legacy_factories::install_window_legacy_factories(scope, global);
}


/// The `Window` interface's own members -- the `[Global]` rows on `globalThis`, the two constants on
/// `Window.prototype` and the interface object, and the global's re-parent onto that prototype
/// (`dom/09f-window.js`, deleted). The whole body is
/// [`crate::core::html::window::install_window_window`].
pub fn install_window_window<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::html::window::install_window_window(scope, global);
}

/// `window` / `self` / `top` / `parent` / `frames` and the `navigator` / `screen` / `location`
/// singletons, replacing the top of `host.js`. The whole body -- including why the three
/// interface OBJECTS beside them could NOT come with them -- is
/// [`crate::core::html::window_globals::install_window_globals`].
pub fn install_window_globals<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::html::window_globals::install_window_globals(scope, global);
}

/// Put this realm's `Document` behind the window's `document` accessor -- `Engine::install_document`,
/// on every navigation. The accessor itself is `[LegacyUnforgeable]` and defined once, at context
/// creation, by [`install_window_globals`]; assigning `globalThis.document` cannot reach it.
pub fn set_window_document<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<'s, v8::Value>) {
    crate::core::html::window_globals::window_slot_set(scope, "document", doc);
}

/// Fire a TRUSTED `popstate` / `hashchange` at the window, built by its own table's constructor from
/// `init` -- the kernel's session history and `Location` call it. The body is
/// [`crate::core::events::navigation_events::fire_window_event`].
pub fn fire_window_event(scope: &mut v8::PinScope, iface: &str, ty: &str, init: v8::Local<v8::Object>) {
    crate::core::events::navigation_events::fire_window_event(scope, iface, ty, init);
}

/// A static, empty `DOMStringList` -- `location.ancestorOrigins` of a top-level document.
pub fn make_empty_dom_string_list<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, 0);
    crate::core::dom::collections::make_static(scope, "DOMStringList", arr)
}

/// Has [`set_window_location`] run in this realm.
pub fn window_location_minted(scope: &mut v8::PinScope) -> bool {
    crate::core::html::window_globals::window_slot_get(scope, "location").is_some()
}

/// Put this realm's `Location` object behind the window's `location` accessor -- the runtime's
/// `reshape_location`, once, at context creation.
pub fn set_window_location<'s>(scope: &mut v8::PinScope<'s, '_>, loc: v8::Local<'s, v8::Value>) {
    crate::core::html::window_globals::window_slot_set(scope, "location", loc);
}

/// THE WINDOW'S REMAINING OWN GLOBALS: the 43 names the worn capture records that no
/// other install creates (`external`, `trustedTypes`, `visualViewport`, the six `BarProp`s, the five
/// GPU constant namespaces, the twelve webkit aliases, and the rest). The whole body, and every
/// measurement behind it, is [`crate::core::html::window_extras::install_window_extras`].
///
/// POSITION: after the registry install, because every one of these instances stands on an interface
/// prototype the registry publishes, and before
/// [`crate::install::installer::seat_globals_in_capture_order`], which puts each new name at the
/// capture's own position so the order ratchet stays at 0. Within that window the position is free.
pub fn install_window_extras<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::core::html::window_extras::install_window_extras(scope, global);
}

/// APPLY THE `[SecureContext]` NAMES' EXPOSURE CONDITION, now that the document's
/// origin is known.
///
/// [`install_window_extras`] runs in `Engine::new`, when the realm's URL is still `about:blank` and
/// every name it installs is therefore correctly exposed; this runs at the PER-NAVIGATION document
/// install, after `HostLocation` has the document's URL and before any of that document's script. A
/// browser decides this when it creates the realm, which for this engine is these two moments rather
/// than one. The names, the measurement and the removal-never-restoration asymmetry are all in
/// [`crate::core::html::exposure`].
pub fn apply_secure_context_exposure(scope: &mut v8::PinScope) -> Vec<String> {
    crate::core::html::exposure::apply_secure_context_exposure(scope)
}

/// Put a slot-backed, getter-only window singleton on the global as the own accessor every browser of
/// the family has -- `history`, from `korvus-runtime`'s singleton install.
pub fn define_window_getter_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, value: v8::Local<'s, v8::Value>) {
    use crate::core::html::window_globals::{define_window_singleton, Mint, WindowSetter};
    // An ordinary object on the table's prototype (`History`), in a realm built from templates too.
    define_window_singleton(scope, global, name, value, WindowSetter::None, true, Mint::Plain);
}

/// `window.chrome` -- the Chrome-proprietary `{loadTimes, csi, app}` object, replacing the IIFE at
/// `host.js` 92-128. The whole body, and every measurement behind it, is
/// [`crate::modules::chrome_object::install_window_chrome`].
pub fn install_window_chrome<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    crate::modules::chrome_object::install_window_chrome(scope, global);
    // IMMEDIATELY AFTER `chrome`, because that is where the device puts them: real Opera 135's
    // own-global sequence reads `… chrome, g_opr, WebAssembly, opr …`. Nothing is installed under the
    // other three bundles — the persona produces no namespaces for a capture that records none. The
    // whole body, and every measurement behind it, is
    // [`crate::core::html::window_globals::install_window_namespaces`].
    crate::core::html::window_globals::install_window_namespaces(scope, global);
}

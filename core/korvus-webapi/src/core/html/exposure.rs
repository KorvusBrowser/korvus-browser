//! `core/html/exposure.rs` — A NAME'S EXPOSURE CONDITION (extended by FP5.9).
//!
//! # The rule
//!
//! > A name must arrive with its **exposure conditions**, not just with its body.
//!
//! FP5 seated 33 own globals with the shape the device gives them and every one of them arrived
//! unconditionally. Two of them are not unconditional on any browser in the family, and the WPT core
//! tier says so in one line:
//!
//! ```text
//! fetch/fetch-later/non-secure.window.js
//!   test(() => { assert_false(window.hasOwnProperty('fetchLater')); },
//!        `fetchLater() is not supported in non-secure context.`);
//! ```
//!
//! # ONE GATE, THREE KINDS OF PUBLISHER
//!
//! FP5.7 closed twelve hand-installed globals here. The other **231** are published somewhere else —
//! 229 of them are the REGISTRY's interface objects (~60 `GPU*`, the whole `XR*`, `USB*`, `MIDI*`,
//! `Presentation*` and `Sensor` families, `CacheStorage`, `SubtleCrypto`, `ServiceWorker*`,
//! `PublicKeyCredential`, `WebTransport*`), and the rest are `caches`, `cookieStore` and four `on*`
//! rows of the `[Global]` `Window` table. **A second place to express exposure is a second table,
//! and a second table is where a LIST comes back**, so there is one [`gate`] and it takes
//! [`Publishes`] as an argument:
//!
//! ```text
//! Publishes::Interface   installer::create_interface_in / live::rows::build_newable
//!                        condition from InterfaceDef::exposure  (generated, or the `secure` word)
//! Publishes::Global      exposure::gate(..) at a hand singleton, and Window's own rows
//!                        condition from the call site, or from MemberDef::exposure
//! ```
//!
//! Each publisher states the condition BESIDE the name; none of them holds a list; all of them
//! record into one realm-private array and are removed by the one pass below, which reads
//! trustworthiness from the one function `isSecureContext` answers from.
//!
//! # An exposure condition is a PLATFORM fact, and it is MEASURED rather than read off the spec
//!
//! It is the same across the Chromium family — four browsers, 248 names, not one disagreement about
//! a name two of them both have — it does not vary with the persona, and it is therefore NOT a
//! bundle field: the bundle says which names this browser has, and the row says under what condition
//! each of them appears. The capture is a recording of ONE origin — a loopback page, which is
//! potentially trustworthy — so its `globals` list is the surface of a SECURE context and cannot
//! answer the question at all.
//!
//! **It is not `[SecureContext]` from the IDL.** Measured against webref, the spec is wrong in both
//! directions for 53 of these names: it marks `AudioWorkletNode`, `GeolocationPosition`,
//! `PushManager`, `SpeechRecognition`, `Navigator` and `Window` `[SecureContext]` and every browser
//! KEEPS them on an insecure origin, and it is silent about the 26 `XR*` layer/hand/plane
//! interfaces, `Worklet`, `FontData`, `KeyboardLayoutMap`, `caches`, `cookieStore`, `queryLocalFonts`,
//! the three file pickers and the four `on*` handlers, every one of which the browsers REMOVE. The
//! reading is recorded once, in `tools/sync/data/secure_context.json`, and both the generator that
//! stamps the registry rows and `core/korvus-runtime/tests/window_exposure_live.rs` read that one
//! file — a second copy of a measurement is a second definition.
//!
//! # MEASURED — FP5.7's twelve, both origins, 2026-09-19
//!
//! One `http.server`, two hostnames, so the only difference between the runs is trustworthiness:
//!
//! ```text
//! secure     http://127.0.0.1:<p>/          isSecureContext true    (loopback is potentially-trustworthy)
//! insecure   http://web-platform.test:<p>/  isSecureContext false   (resolves to loopback; is not `localhost`)
//! ```
//!
//! Real Chrome 150.0.7871.46 and real Brave 151.1.93.138, headed over CDP, `navigator.webdriver`
//! false in all four runs. `Object.prototype.hasOwnProperty.call(window, name)` on each of the 38
//! names FP5 installs (33 of them under any one bundle). **Twelve are gone on the insecure origin and
//! twenty-six are not**:
//!
//! ```text
//! [SecureContext] -- absent on an insecure origin, on BOTH browsers
//!   fetchLater  getScreenDetails  queryLocalFonts
//!   showDirectoryPicker  showOpenFilePicker  showSaveFilePicker       (Brave has none of the three
//!                                                                      on either origin -- a BUNDLE
//!                                                                      fact, already handled)
//!   sharedStorage                                                     (Chrome/Edge/Opera only)
//!   GPUBufferUsage  GPUColorWrite  GPUMapMode  GPUShaderStage  GPUTextureUsage
//!
//! unconditional -- present on BOTH origins, on BOTH browsers
//!   the eight webkit interface aliases, the two webkit rAF aliases, clientInformation,
//!   offscreenBuffering, external, styleMedia, launchQueue, fence, find, webkitRequestFileSystem,
//!   webkitResolveLocalFileSystemURL, the six BarProps, viewport
//! ```
//!
//! Two of those readings contradict what an IDL reading would guess and are recorded because of it:
//!
//! * **`launchQueue` is NOT `[SecureContext]`.** `window.launchQueue` is present on an insecure
//!   origin on both browsers. (Its `LaunchQueue` *interface object* is not on the global at all, on
//!   either origin, which is why nothing here is inconsistent.)
//! * **`fence` is NOT `[SecureContext]` on Chrome 150.** `window.fence` is present on the insecure
//!   origin. Brave has it on neither, which is again the bundle's business.
//!
//! # MEASURED — the whole window, four browsers, both origins, 2026-09-19 (FP5.9)
//!
//! The same server and the same two hostnames, `Object.getOwnPropertyNames(window)` entire, Edge and
//! Opera added. **Nothing is gained on the insecure origin, on any browser**:
//!
//! ```text
//!                 secure   insecure   removed   this engine removes, wearing that bundle
//!   chrome-150     1224       980       244      243
//!   brave-151      1206       972       234      229
//!   edge-151       1227       984       243      242
//!   opera-135      1219       975       244      243
//! ```
//!
//! The engine's number is one short under three bundles and five short under `brave-151` for a reason
//! that is not this gate's: it does not INSTALL `documentPictureInPicture` (one of the six FP5 left
//! unseated on purpose) or Brave's four injected wallet globals, and a name that is not there cannot
//! be taken away. Every name this engine does publish and the browser gates is gated, under all four
//! bundles — `window_exposure_live.rs` asserts exactly that, and names the shortfall so it can only
//! shrink.
//!
//! # WHEN the condition is evaluated, and why it is not at install time
//!
//! A browser creates a realm per document and knows the document's origin when it does. This engine
//! installs the whole window surface once in `Engine::new`, before any URL exists — `HostLocation` is
//! still `about:blank`, which IS potentially trustworthy — and `Engine::set_url` arrives later. So
//! the install records which names carry a condition ([`gate`]) and
//! [`apply_secure_context_exposure`] applies it at the one moment the origin becomes known: the
//! per-navigation document install, before any page script of that document runs.
//!
//! **It REMOVES; it never restores.** A cross-document navigation gets a fresh realm
//! (`Engine::successor`), so the realm that has seen an insecure document never goes on to host a
//! secure one on any path a page can take. A test that calls `load_page` twice on one engine, insecure
//! then secure, gets the name left out — which is the safe direction of the asymmetry FP5 argued:
//! an absent name costs a fingerprint point, a name whose body cannot work costs the page.
//!
//! # The order ratchet
//!
//! `installer::seat_globals_in_capture_order` has already run when this pass does, and it seats only
//! the names that are `present`. Deleting an own property afterwards does not renumber the ones
//! around it — V8 keeps the enumeration index of every other own name — so an empty seat is just an
//! empty seat. `global_order.rs` and `worker_surface.rs` read 0 out of place in both realms under all
//! four bundles, which is measured on a secure origin and unchanged by this file.

use deno_core::v8;

use crate::binding::protos::str_val;

/// A name's exposure condition, as the BROWSER answers it.
///
/// This is deliberately not a boolean. The call sites that pass [`Exposure::SecureContext`] read as
/// the IDL reads, and a third condition (`[Exposed=...]` beyond window/worker, a runtime-enabled
/// flag) gets a variant rather than a second mechanism.
///
/// It is `Copy` and lives on the row — [`crate::install::InterfaceDef::exposure`] and
/// [`crate::install::MemberDef::exposure`] — so a generated interface, a hand table's member and a
/// hand `define_window_singleton` all state it the same way: beside the name, never in a list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exposure {
    /// No condition gates the name: it is on the global wherever the interface is exposed.
    Unconditional,
    /// `[SecureContext]` — the name does not exist at all on a non-potentially-trustworthy origin.
    /// Not present-and-throwing and not present-and-undefined: `window.hasOwnProperty(name)` is
    /// `false`.
    SecureContext,
}

/// WHICH of the active capture's two lists answers "does this device have the name at all".
///
/// The engine publishes an own name on the global from three kinds of place — the registry's
/// interface objects, the hand singletons, and the `[Global]` Window table's own rows — and they ask
/// a different question of the capture. They pass through ONE [`gate`] all the same, because the
/// second half of the answer (the exposure condition) must be stated at every one of them or a name
/// sneaks in without one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Publishes {
    /// An interface OBJECT. The capture records the interface's member list
    /// ([`crate::install::surface::publishes_interface`]).
    Interface,
    /// Any other own property of the global: a singleton, a constant namespace, a Window row.
    /// The capture's own-globals list answers it ([`crate::install::surface::publishes_global`]).
    Global,
}

fn record_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = str_val(scope, "korvus.exposure.secureContext");
    let n = v8::Local::<v8::String>::try_from(n).expect("a string value is a string");
    v8::Private::for_api(scope, Some(n))
}

/// THE ONE GATE a hand-installed own global passes through, and it takes the exposure condition as an
/// ARGUMENT so that a name cannot be installed without one being stated beside it.
///
/// Two questions, and they are different in kind:
///
/// 1. **Does this browser have the name at all?** The active capture's own-globals list answers it,
///    and it is a persona fact: `sharedStorage` exists under `chrome-150`, `edge-151` and `opera-135`
///    and not under `brave-151`; Brave publishes none of the three file pickers. The engine reads the
///    bundle; it never asks which bundle it is wearing.
/// 2. **Under what condition does it appear?** `exposure` answers it, and it is a platform fact.
///
/// A `[SecureContext]` name is INSTALLED here and recorded on the realm; whether it survives to the
/// document is [`apply_secure_context_exposure`]'s answer, because this runs before any URL exists.
///
/// `publishes` says which of the capture's two lists answers question 1 — see [`Publishes`]. It is
/// an argument rather than two functions so that there is literally ONE gate: the registry's ~230
/// interface objects and FP5.7's hand globals record into the same realm-private list
/// and are removed by the same pass, reading trustworthiness from the same function
/// `isSecureContext` answers from.
pub fn gate(scope: &mut v8::PinScope, publishes: Publishes, name: &str, exposure: Exposure) -> bool {
    let has = match publishes {
        Publishes::Interface => crate::install::surface::publishes_interface(scope, name),
        Publishes::Global => crate::install::surface::publishes_global(scope, name),
    };
    if !has {
        return false;
    }
    if exposure == Exposure::SecureContext {
        let global = scope.get_current_context().global(scope);
        let key = record_key(scope);
        let list = match global.get_private(scope, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
            Some(a) => a,
            None => {
                let a = v8::Array::new(scope, 0);
                global.set_private(scope, key, a.into());
                a
            }
        };
        let v = str_val(scope, name);
        list.set_index(scope, list.length(), v);
    }
    true
}

/// Every `[SecureContext]` name [`gate`] installed in this realm, in install order.
///
/// Derived from the installs rather than from a second list beside them: a hand-kept copy would be a
/// second definition competing with the first, and the two would disagree the first time a name moved.
fn recorded(scope: &mut v8::PinScope) -> Vec<String> {
    let global = scope.get_current_context().global(scope);
    let key = record_key(scope);
    let Some(a) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else {
        return Vec::new();
    };
    (0..a.length()).filter_map(|i| a.get_index(scope, i)).map(|v| v.to_rust_string_lossy(scope)).collect()
}

/// APPLY the `[SecureContext]` condition to this realm, now that the document's origin is known.
///
/// Called from the per-navigation document install, after `HostLocation` is set to the document's URL
/// and before any of that document's script runs. The trustworthiness reading is
/// [`crate::core::html::window::potentially_trustworthy`] over
/// [`crate::core::html::window::href`] — the SAME function and the SAME string `isSecureContext`
/// answers from, so a page cannot find a name whose condition its own `isSecureContext` denies.
///
/// Returns the names it removed, which is what the tests assert against.
pub fn apply_secure_context_exposure(scope: &mut v8::PinScope) -> Vec<String> {
    let href = crate::core::html::window::href(scope);
    if crate::core::html::window::potentially_trustworthy(&href) {
        return Vec::new();
    }
    let global = scope.get_current_context().global(scope);
    let mut removed = Vec::new();
    for name in recorded(scope) {
        let Some(k) = v8::String::new(scope, &name) else { continue };
        if global.has_own_property(scope, k.into()) == Some(true) && global.delete(scope, k.into()) == Some(true) {
            removed.push(name);
        }
    }
    removed
}

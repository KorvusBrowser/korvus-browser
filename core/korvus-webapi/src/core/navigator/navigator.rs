//! The one-table `Navigator` / `WorkerNavigator` definition.
//!
//! ONE ROW PER MEMBER, ONE BODY PER ROW, ALL OF THEM RUST. The table below is the only place any of
//! the 66 members is named; each row names the Rust function that mounts it, and every body is a
//! native `v8::Function`, so `[native code]` is a fact of construction rather than a mask
//! (invariants I1 / I3). Four bodies serve the whole surface:
//!
//!   * [`install_scalar`] -- the ~18 persona reads (userAgent/platform/languages/...) and the 3
//!     recording constants (globalPrivacyControl=true, doNotTrack=null). One getter
//!     callback keyed by the member name, reading the persona LIVE through the kernel `Context`'s
//!     [`spec::PersonaHandle`] service ([`persona`]), so the running value is the persona's and not a
//!     copy. An isolate with no persona service (a bare test runtime) gets the shape default.
//!   * [`install_lazy_object`] -- the 31 members that hand back an OBJECT: the persona-sourced ones
//!     ([`super::persona_objects`]), the shape-only device singletons and the hidden-constructor
//!     objects ([`super::devices`]), the plugin family (`core/html/plugins.rs`) and
//!     `serviceWorker` ([`crate::core::workers::service_worker`], whose three tables it mints from).
//!   * [`super::methods::install_method`] and [`super::methods::install_legacy_get_user_media`] --
//!     the 16 methods.
//!
//! WHAT IT REPLACED. Until W4.a these 45 intricate members had their bodies in
//! `core/korvus-runtime/src/js/nav-glue.js`: JS functions wrapped in `op_nativize` so they would
//! READ `[native code]`, handed to this installer as a v8 object through `op_nav_glue_register` and
//! read back out by name. That was a Web API definition site inside the kernel crate (I9), a fourth
//! definition layer for half of `Navigator` (I1), and 45 native masks (I3). The file, the op, the
//! holder and the read-a-body-out-by-name installer are all gone.
//!
//! The installer joins this union with a bundle selector; the SHAPE (names, order, kinds,
//! descriptors) is proven byte-identical to Brave's 66-member window surface + 23-member worker
//! surface by `korvus-runtime`'s `tests/navigator_shape.rs`, and the LIVE running navigator byte-
//! and value-identical by `tests/navigator_live.rs` / `tests/worker_navigator_live.rs`.

use deno_core::v8;
use deno_core::JsRuntime;

use crate::binding::protos::build_fn_data;
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::MemberDef;

use super::devices;
use super::methods::{install_legacy_get_user_media, install_method};
use super::persona_objects;
use crate::core::workers::service_worker;

webapi::interface! {
    // Navigator does not inherit EventTarget (surface.json proto == []). `both` -> the same rows
    // serve the window `Navigator` and the worker `WorkerNavigator`; each global's member SUBSET and
    // ORDER come from its own bundle selector, which the installer walks -- not from this table.
    "Navigator" [singleton, both, alias "WorkerNavigator"] {
        attr    "vendorSub"                      : DomString  readonly = install_scalar,
        attr    "productSub"                     : DomString  readonly = install_scalar,
        attr    "vendor"                         : DomString  readonly = install_scalar,
        attr    "maxTouchPoints"                 : Integer    readonly = install_scalar,
        attr    "scheduling"                     : Object     readonly = install_lazy_object,
        attr    "userActivation"                 : Object     readonly = install_lazy_object,
        attr    "geolocation"                    : Object     readonly = install_lazy_object,
        attr    "doNotTrack"                     : DomString  readonly = install_scalar,
        attr    "webkitTemporaryStorage"         : Object     readonly = install_lazy_object,
        attr    "webkitPersistentStorage"        : Object     readonly = install_lazy_object,
        attr    "windowControlsOverlay"          : Object     readonly = install_lazy_object,
        attr    "brave"                          : Object     readonly = install_lazy_object,
        attr    "globalPrivacyControl"           : Boolean    readonly = install_scalar,
        attr    "hardwareConcurrency"            : Integer    readonly = install_scalar,
        attr    "cookieEnabled"                  : Boolean    readonly = install_scalar,
        attr    "appCodeName"                    : DomString  readonly = install_scalar,
        attr    "appName"                        : DomString  readonly = install_scalar,
        attr    "appVersion"                     : DomString  readonly = install_scalar,
        attr    "platform"                       : DomString  readonly = install_scalar,
        attr    "product"                        : DomString  readonly = install_scalar,
        attr    "userAgent"                      : DomString  readonly = install_scalar,
        attr    "language"                       : DomString  readonly = install_scalar,
        attr    "languages"                      : Sequence   readonly = install_scalar,
        attr    "onLine"                         : Boolean    readonly = install_scalar,
        attr    "webdriver"                      : Boolean    readonly = install_scalar,
        attr    "plugins"                        : Object     readonly = install_lazy_object,
        attr    "mimeTypes"                      : Object     readonly = install_lazy_object,
        attr    "pdfViewerEnabled"               : Boolean    readonly = install_scalar,
        method  "getGamepads"                    : Sequence   len 0 = install_method,
        method  "javaEnabled"                    : Boolean    len 0 = install_method,
        method  "sendBeacon"                     : Boolean    len 1 = install_method,
        method  "vibrate"                        : Boolean    len 1 = install_method,
        // Chrome 152 (`navigator_cpu_performance.idl`), from the capture; not origin-gated, as `clipboard` is not.
        attr    "cpuPerformance"                 : Integer    window readonly = install_scalar,
        attr    "clipboard"                      : Object     readonly = install_lazy_object,
        attr    "credentials"                    : Object     readonly = install_lazy_object,
        attr    "keyboard"                       : Object     readonly = install_lazy_object,
        attr    "managed"                        : Object     readonly = install_lazy_object,
        attr    "mediaDevices"                   : Object     readonly = install_lazy_object,
        attr    "serviceWorker"                  : Object     readonly = install_lazy_object,
        attr    "virtualKeyboard"                : Object     readonly = install_lazy_object,
        attr    "wakeLock"                       : Object     readonly = install_lazy_object,
        attr    "deviceMemory"                   : Integer    readonly = install_scalar,
        attr    "userAgentData"                  : Object     readonly = install_lazy_object,
        attr    "locks"                          : Object     readonly = install_lazy_object,
        attr    "storage"                        : Object     readonly = install_lazy_object,
        attr    "gpu"                            : Object     readonly = install_lazy_object,
        attr    "ink"                            : Object     readonly = install_lazy_object,
        attr    "mediaCapabilities"              : Object     readonly = install_lazy_object,
        attr    "permissions"                    : Object     readonly = install_lazy_object,
        attr    "devicePosture"                  : Object     readonly = install_lazy_object,
        attr    "hid"                            : Object     readonly = install_lazy_object,
        attr    "mediaSession"                   : Object     readonly = install_lazy_object,
        attr    "presentation"                   : Object     readonly = install_lazy_object,
        attr    "serial"                         : Object     readonly = install_lazy_object,
        attr    "usb"                            : Object     readonly = install_lazy_object,
        attr    "xr"                             : Object     readonly = install_lazy_object,
        attr    "storageBuckets"                 : Object     readonly = install_lazy_object,
        method  "clearAppBadge"                  : Promise    len 0 = install_method,
        method  "getBattery"                     : Promise    len 0 = install_method,
        method  "getUserMedia"                   : Undefined  len 3 = install_legacy_get_user_media,
        method  "requestMIDIAccess"              : Promise    len 0 = install_method,
        method  "requestMediaKeySystemAccess"    : Promise    len 2 = install_method,
        method  "setAppBadge"                    : Promise    len 0 = install_method,
        method  "webkitGetUserMedia"             : Undefined  len 3 = install_legacy_get_user_media,
        method  "getInstalledRelatedApps"        : Promise    len 0 = install_method,
        method  "registerProtocolHandler"        : Undefined  len 2 = install_method,
        method  "unregisterProtocolHandler"      : Undefined  len 2 = install_method,

        // ── THE OTHER OS'S ROWS (Windows-family) ─────────────────────────────────
        //
        // Present on the Windows Chrome capture (`chrome-152-windows-...`) and absent from the Linux
        // one, on exactly the same terms as the Brave/Chrome rows below: `install::installer::join`
        // mounts them only for a bundle whose capture lists them, so Linux keeps none of them and the
        // order a page reads is still its own capture's, not this table's.
        //
        // `bluetooth` is a shape-only device singleton -- the `Bluetooth` interface, its members
        // (`getAvailability`, `requestDevice`) taken from the capture exactly as `usb`/`serial`/`hid`
        // take theirs (`DEVICE_INTERFACE`). Web Share's `share`/`canShare` are window-only methods
        // Chrome exposes on Windows and Mac but not on desktop Linux; both take an optional `ShareData`
        // (`length` 0) and `install_method` hands back the type's shape (a `Promise`, a `false`).
        attr    "bluetooth"                      : Object     window readonly = install_lazy_object,
        // D2 stubs, as the ad-auction rows below: this engine shares nothing and pairs no device, and
        // the stub body says so with the row's declared shape -- `canShare` hands back `false`, and
        // `share`, a promise-returning member, refuses by rejecting (Chrome's own answer to a share
        // with no transient activation), both as `[native code]`.
        method  "canShare"                       : Boolean    window len 0 = stub,
        method  "share"                          : Promise    window len 0 = stub,

        // ── THE OTHER DEVICE'S ROWS ──────────────────────────────────────────────
        //
        // Fifteen members `chrome-152-linux-20261016T1022` records on `Navigator` and
        // `brave-151-linux-20260907T1006` does not: Protected Audience (thirteen), FedCM (`login`)
        // and Network Information (`connection`, the one of the fifteen Chrome exposes in BOTH
        // realms -- `worker.surface.json` lists `WorkerNavigator.connection`).
        //
        // THEY LIVE HERE AND ARE NOT ALWAYS INSTALLED, which is the whole of P3. `brave` sits three
        // dozen lines above them on exactly the same terms; neither device grows the other's surface,
        // because `install::installer::join` mounts only what the ACTIVE capture lists. Deleting a
        // row to make one bundle match would take it away from the bundle that has it.
        //
        // APPENDED, NOT INTERLEAVED, and that is not laziness: this table's order is only the
        // FALLBACK a realm with no bundle selector walks (a bare runtime). The order a page reads is
        // its own capture's, and Chrome's puts `connection` between `pdfViewerEnabled` and
        // `getGamepads` while Brave has no such member at all -- one table cannot hold both
        // sequences, and it does not have to.
        //
        // The METHODS are D2 stubs: this engine implements no ad auction and no federated login,
        // and saying so with the row's declared shape is the honest answer. The three ATTRIBUTES are
        // what stock Chrome 150 hands back unprompted (measured 2026-10-08): a `ProtectedAudience`
        // and a `NavigatorLogin` (shape-only singletons, `super::devices`) and `false`. The
        // type tags are `gen_shells.type_tag`'s own answers for these names, so moving this table
        // into or out of the generated registry later is byte-neutral. `connection` is a real
        // `NetworkInformation` over the persona's recorded estimate
        // (`super::network_information`).
        attr    "connection"                     : Any        readonly = install_lazy_object,
        attr    "deprecatedRunAdAuctionEnforcesKAnonymity" : Any window readonly = install_scalar,
        attr    "protectedAudience"              : Any        window readonly = install_lazy_object,
        attr    "login"                          : Any        window readonly = install_lazy_object,
        method  "adAuctionComponents"            : Sequence   window len 1 required 1 = install_ad_auction_components,
        method  "runAdAuction"                   : Promise    window len 1 required 1 = stub,
        method  "canLoadAdAuctionFencedFrame"    : Any        window len 0 = stub,
        method  "clearOriginJoinedAdInterestGroups" : Promise window len 1 required 1 = stub,
        method  "createAuctionNonce"             : Promise    window len 0 = stub,
        method  "joinAdInterestGroup"            : Promise    window len 1 required 1 = stub,
        method  "leaveAdInterestGroup"           : Promise    window len 0 = stub,
        method  "updateAdInterestGroups"         : Any        window len 0 = stub,
        method  "deprecatedReplaceInURN"         : Promise    window len 2 required 2 = stub,
        method  "deprecatedURNToURL"             : Promise    window len 1 required 1 = stub,
        method  "getInterestGroupAdAuctionData"  : Promise    window len 1 required 1 = stub,
    }
}

// `WINDOW_NAVIGATOR_MEMBERS` (66 names) AND `WORKER_NAVIGATOR_MEMBERS` (23) WERE HERE. DELETED BY
// AND THE DELETION IS THE POINT.
//
// They were Brave's two member lists, transcribed out of `brave-151-linux-20260907T1006`'s two
// surface files and handed to the installer as the per-realm selector. That made this file the
// second place the browser's `Navigator` was described -- a hand copy of a recording (the
// list is DERIVED, never counted) -- and it made the selector a statement about ONE device: booted
// under `chrome-152-linux-20261016T1022` the engine mounted Brave's 66 names, including `brave`, and
// left Chrome's fifteen unmounted, because the list said so.
//
// `install::installer::join` reads the list off the ACTIVE capture now, under the name the realm
// exposes -- `interfaces.Navigator` in the window surface, `interfaces.WorkerNavigator` in the
// worker's -- so one table serves both realms and every bundle, and `core/korvus-runtime/tests/
// navigator_shape.rs` asks the bundle rather than this file.

// ---------------------------------------------------------------------------------------------------
// Body source 1: native scalar getters (persona reads + recording constants).
// ---------------------------------------------------------------------------------------------------

/// The value a scalar getter yields, computed from `HostPersona` (or the shape default when the
/// persona is absent), then realised as a V8 value. Extracted as owned Rust so the op_state borrow is
/// released before any V8 allocation.
enum Scalar {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Langs(Vec<String>),
}

/// The persona this realm wears, read from the kernel `Context`'s [`spec::PersonaHandle`] service --
/// the I9 seam: the runtime PROVIDES the identity, the Web API table READS it. `None` in an isolate
/// with no persona service (a bare test runtime).
pub(crate) fn persona(scope: &mut v8::PinScope) -> Option<std::sync::Arc<dyn spec::Persona>> {
    let op_state = JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)
        .map(|h| h.0.clone())
}

/// Compute one scalar member's value from the persona. Mirrors the `JsNavigator` getters in lib.rs
/// field-for-field; the recording constants (globalPrivacyControl/doNotTrack) need no
/// persona. Unknown names (never reached from the table) yield `Null`.
fn scalar_value(p: Option<&std::sync::Arc<dyn spec::Persona>>, name: &str) -> Scalar {
    // Constants first -- they must answer even in a persona-less isolate.
    match name {
        "globalPrivacyControl" => return Scalar::Bool(true),
        // host.js answered `null` unconditionally (Chrome's default when DNT is off); match it.
        "doNotTrack" => return Scalar::Null,
        // korvus's own identity says a client automates the page (goal 1020 D11); a dressed one does not
        "webdriver" => return Scalar::Bool(p.is_some_and(|p| p.webdriver())),
        "onLine" => return Scalar::Bool(!spec::devtools::offline()),
        // Chrome 150 answers `false` (measured 2026-10-08); a capture without the member never mounts it.
        "deprecatedRunAdAuctionEnforcesKAnonymity" => return Scalar::Bool(false),
        _ => {}
    }
    let Some(p) = p else {
        // Persona-less isolate (a bare test runtime): the D2 shape default for the declared type.
        return match name {
            "hardwareConcurrency" | "maxTouchPoints" | "deviceMemory" | "cpuPerformance" => Scalar::Num(0.0),
            "cookieEnabled" | "pdfViewerEnabled" => Scalar::Bool(false),
            "languages" => Scalar::Langs(Vec::new()),
            _ => Scalar::Str(String::new()),
        };
    };
    match name {
        "userAgent" => Scalar::Str(spec::devtools::navigator(p.navigator()).user_agent),
        "platform" => Scalar::Str(spec::devtools::navigator(p.navigator()).platform),
        "hardwareConcurrency" => Scalar::Num(spec::devtools::navigator(p.navigator()).hardware_concurrency as f64),
        "deviceMemory" => Scalar::Num(spec::devtools::navigator(p.navigator()).device_memory),
        "languages" => Scalar::Langs(spec::devtools::navigator(p.navigator()).languages),
        "language" => Scalar::Str(spec::devtools::navigator(p.navigator()).languages.first().cloned().unwrap_or_default()),
        "appVersion" => {
            let ua = spec::devtools::navigator(p.navigator()).user_agent;
            Scalar::Str(ua.strip_prefix("Mozilla/").map(str::to_string).unwrap_or(ua))
        }
        "cookieEnabled" => Scalar::Bool(spec::devtools::navigator_extra(p.navigator_extra()).cookie_enabled),
        "maxTouchPoints" => Scalar::Num(spec::devtools::navigator_extra(p.navigator_extra()).max_touch_points as f64),
        "vendor" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).vendor),
        "vendorSub" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).vendor_sub),
        "appName" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).app_name),
        "appCodeName" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).app_code_name),
        "product" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).product),
        "productSub" => Scalar::Str(spec::devtools::navigator_extra(p.navigator_extra()).product_sub),
        "pdfViewerEnabled" => Scalar::Bool(spec::devtools::navigator_extra(p.navigator_extra()).pdf_viewer_enabled),
        "cpuPerformance" => Scalar::Num(spec::devtools::navigator_extra(p.navigator_extra()).cpu_performance as f64),
        _ => Scalar::Null,
    }
}

/// The single scalar getter callback: reads the member name (its associated data), computes the
/// value from the persona, returns it. A genuine `v8::Function`, so `.toString()` is `[native code]`.
/// The hostname of the current document, the key Brave farbles each signal per. Read from
/// `CookieUrl` -- the same per-origin string the cookie store keys on -- and reduced to the authority's
/// host (no scheme, userinfo or port), so `https://localhost:8443/x` and `http://localhost/y` share a
/// draw but `127.0.0.1` does not, matching the capture's `keyed_on`. `None` on `about:blank`/opaque.
fn current_host(scope: &mut v8::PinScope) -> Option<String> {
    let op_state = JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    // A frame farbles by the TOP-level page's host, as an in-process frame does (it shares the page's
    // `CookieUrl`); a remote frame keeps the top's URL in its embedding.
    let url = match st.try_borrow::<crate::core::html::remote_frame::FrameEmbedding>() {
        Some(e) if !e.top_url.is_empty() => e.top_url.clone(),
        _ => st.try_borrow::<crate::core::dom::document::CookieUrl>()?.0.clone(),
    };
    let after = url.split_once("://")?.1;
    let authority = after.split(['/', '?', '#']).next().unwrap_or(after);
    let hostport = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    // Strip a :port, but not the colons of an IPv6 literal ([::1]).
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split_once(']').map(|(h, _)| h).unwrap_or(rest).to_string()
    } else {
        hostport.rsplit_once(':').map(|(h, _)| h).unwrap_or(hostport).to_string()
    };
    (!host.is_empty()).then_some(host)
}

/// Brave's per-origin farbled value for a scalar, or `None` to serve the static capture:
/// `None` when the persona does not farble (no `farbling.json` -> Chrome/Edge/Opera), when the signal is
/// not in the policy, when no session seed has been provided, or on an opaque origin. The seed is an
/// engine service in `OpState`; the policy's supports come from the bundle (I4).
fn farbled_scalar(scope: &mut v8::PinScope, name: &str, p: &std::sync::Arc<dyn spec::Persona>) -> Option<f64> {
    let pol = p.farbling()?;
    let seed = {
        let op_state = JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        *st.try_borrow::<spec::farble::SessionSeed>()?
    };
    let host = current_host(scope)?;
    match name {
        "hardwareConcurrency" => pol.hardware_concurrency_max.map(|n| spec::farble::uniform_range(seed, &host, 2, n as i64) as f64),
        "deviceMemory" if !pol.device_memory_set.is_empty() => Some(spec::farble::uniform_set(seed, &host, &pol.device_memory_set)),
        _ => None,
    }
}

fn cb_scalar(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let p = persona(scope);
    // Brave redraws hardwareConcurrency/deviceMemory per (session, host). When the worn
    // persona farbles (its bundle ships farbling.json) and a seed + origin are present, the farbled draw
    // replaces the static capture; otherwise the static value stands (a non-farbling browser, or before
    // any navigation). Same seed + host always draws the same value, so two reads on one page agree.
    let v = match (&p, matches!(name.as_str(), "hardwareConcurrency" | "deviceMemory")) {
        (Some(p), true) => match farbled_scalar(scope, &name, p) {
            Some(n) => Scalar::Num(n),
            None => scalar_value(Some(p), &name),
        },
        _ => scalar_value(p.as_ref(), &name),
    };
    match v {
        Scalar::Str(s) => {
            let s = v8::String::new(scope, &s).unwrap_or_else(|| v8::String::empty(scope));
            rv.set(s.into());
        }
        Scalar::Num(n) => rv.set(v8::Number::new(scope, n).into()),
        Scalar::Bool(b) => rv.set(v8::Boolean::new(scope, b).into()),
        Scalar::Null => rv.set_null(),
        // `[CachedAttribute=IsLanguagesDirty] readonly attribute FrozenArray<DOMString> languages` (Blink's
        // NavigatorLanguage, both realms): ONE frozen array, handed out again until the language list changes
        // (`install::frozen_array`). This built a new, writable array on every read -- the shape of an anti-detect
        // browser's `get: () => [...]` override, which is exactly how Fingerprint Pro scored it (C2). Nothing here
        // changes the list at run time; the version is the list's digest, so a change would be seen as Blink's
        // `SetLanguagesDirty` makes it seen.
        Scalar::Langs(langs) => {
            let version = crate::install::frozen_array::digest(&langs);
            let answer = crate::install::frozen_array::answer(scope, args.this(), "Navigator", "languages", |_| version, |scope| {
                Some(langs.iter().map(|l| crate::binding::protos::str_val(scope, l)).collect())
            });
            if let Some(a) = answer {
                rv.set(a);
            }
        }
    }
}

/// Install a persona-backed scalar / constant as a readonly accessor whose getter is [`cb_scalar`].
/// WebIDL descriptor for an interface-prototype attribute: `{ get, enumerable: true, configurable: true }`.
/// `navigator.adAuctionComponents(n)`: Chromium 152's `NavigatorAuction::adAuctionComponents` answers an
/// empty sequence, whatever the document -- a new empty array each call (Chrome 150 threw InvalidStateError
/// "This frame was not loaded with the result of an interest group auction." on any ordinary page). The
/// row's own argument check (`n` is required) runs first, as before.
fn cb_ad_auction_components(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::implements(scope, args.this().into(), "Navigator") {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let empty = v8::Array::new(scope, 0);
    rv.set(empty.into());
}

pub fn install_ad_auction_components(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::install::live::method_row(scope, proto, member, cb_ad_auction_components);
}

pub fn install_scalar(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data: v8::Local<v8::Value> = match v8::String::new(scope, member.name) {
        Some(s) => s.into(),
        None => return,
    };
    let getter = build_fn_data(scope, cb_scalar, &format!("get {}", member.name), 0, data);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut desc = v8::PropertyDescriptor::new_from_get_set(getter.into(), undef);
    desc.set_enumerable(true);
    desc.set_configurable(true);
    if let Some(k) = v8::String::new(scope, member.name) {
        proto.define_property(scope, k.into(), &desc);
    }
}

// ── the shared pieces every body in this folder uses ─────────────────────────────────────────────

/// The persona's UA client hints, or the empty set in an isolate with no persona service -- exactly
/// what `op_persona_hints` handed the JS before it was removed, read as a kernel service instead of as an op (I9).
pub(crate) fn client_hints(scope: &mut v8::PinScope) -> spec::ClientHints {
    spec::devtools::client_hints(persona(scope).map(|p| p.client_hints()).unwrap_or_default())
}

/// One own data property with an object literal's descriptor (`{writable, enumerable, configurable}`
/// all true) -- the shape every property of the JS-built objects had.
pub(crate) fn own_prop(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, value: v8::Local<v8::Value>) {
    let Some(k) = v8::String::new(scope, key) else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(value, true);
    d.set_enumerable(true);
    d.set_configurable(true);
    o.define_property(scope, k.into(), &d);
}

/// `false` -- `Scheduling.isInputPending()`, `UserActivation.hasBeenActive` / `.isActive`, and the
/// `dispatchEvent` of every object these members hand back.
pub(crate) fn cb_false(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::Boolean::new(scope, false).into());
}

/// `Promise.resolve(true)` -- `navigator.brave.isBrave()`.
pub(crate) fn cb_resolved_true(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    let p = crate::binding::protos::resolved_promise(scope, t);
    rv.set(p);
}

/// The JS `domError(msg, name)`: a real `DOMException` when the realm exposes one, and otherwise a
/// plain `Error` carrying the name -- the same fallback the JS body had for a bare isolate.
pub(crate) fn dom_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    if let Some(e) = crate::binding::protos::make_dom_exc(scope, msg, name) {
        return Some(e);
    }
    let m = v8::String::new(scope, msg)?;
    let e = v8::Exception::error(scope, m);
    if let Ok(o) = v8::Local::<v8::Object>::try_from(e) {
        let n = crate::binding::protos::str_val(scope, name);
        if let Some(k) = v8::String::new(scope, "name") {
            o.set(scope, k.into(), n);
        }
    }
    Some(e)
}

// ── the one body every object-valued row has ─────────────────────────────────────────────────────

/// `once(fn)` from `nav-glue.js`, per REALM and per member: build on FIRST READ, then hand back the
/// same value forever. `navigator.usb !== navigator.usb` is its own tell, so every one of these is a
/// per-navigator singleton.
///
/// FIRST READ, not install time, and that is a measurement rather than a preference. Three of these
/// builders write on an interface PROTOTYPE -- `Scheduling.isInputPending`, `UserActivation`'s two
/// accessors, and (through host.js) the `ServiceWorkerContainer` family -- and the surface-finishing
/// passes (`installer::finalize_surface`, arity guards + constant seal -- there was an
/// enumeration-order pass too) run again per document, AFTER this install. The JS closures ran at page-script time, behind those
/// passes; building eagerly here would move the writes in front of them. Keeping the moment keeps
/// the answer.
///
/// The cell is a ONE-ELEMENT ARRAY in a v8 Private on the realm's global -- an array rather than the
/// value itself so that `undefined` and `null` are remembered like any other answer, and a Private
/// so nothing is visible to `getOwnPropertyNames`. The global is the right holder because the JS
/// memo was a module closure: one per realm, not one per receiver.
fn cb_lazy_object(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let global = scope.get_current_context().global(scope);
    let slot = format!("navOnce_{name}");
    if let Some(cell) = evt_slot_read(scope, global, &slot).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        if let Some(v) = cell.get_index(scope, 0) {
            rv.set(v);
        }
        return;
    }
    let value = build_object(scope, &name);
    let cell = v8::Array::new(scope, 1);
    cell.set_index(scope, 0, value);
    evt_slot_write(scope, global, &slot, cell.into());
    rv.set(value);
}

/// Build one object-valued member's answer. Every arm is a body ported from `nav-glue.js`; the
/// fall-through is the 22 shape-only device singletons, which differ only in the interface they are
/// minted on.
fn build_object<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Value> {
    match name {
        "userAgentData" => persona_objects::build_ua_data(scope).into(),
        "mediaDevices" => persona_objects::build_media_devices(scope).into(),
        "permissions" => persona_objects::build_permissions(scope).into(),
        "plugins" | "mimeTypes" => crate::core::html::plugins::navigator_plugin_family(scope, name),
        "serviceWorker" => service_worker::build_container(scope),
        "connection" => super::network_information::build_connection(scope),
        "keyboard" => super::keyboard::build_keyboard(scope),
        // The persona describes this one, name and all -- see `devices::build_hidden`.
        "brave" => devices::build_hidden(scope, name)
            .map(|o| o.into())
            .unwrap_or_else(|| v8::null(scope).into()),
        "webkitTemporaryStorage" | "webkitPersistentStorage" => devices::build_quota(scope).into(),
        other => devices::build_device(scope, other),
    }
}

/// The table body for every `Navigator` member that hands back an OBJECT: a `[native code]` getter
/// over [`cb_lazy_object`], with the member name as its bound data.
pub fn install_lazy_object(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = crate::binding::protos::str_val(scope, member.name);
    let g = build_fn_data(scope, cb_lazy_object, &format!("get {}", member.name), 0, data);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

/// The cppgc object `navigator` IS -- in both realms -- and nothing else: an OWN class
/// with no member of its own. Its prototype is the one-table `Navigator` / `WorkerNavigator`
/// (`binding::class::mint` puts a window's on `Navigator.prototype`; `install_worker_navigator`
/// re-points a worker's). It was `korvus-runtime`'s `#[op2]` class `JsNavigator`, whose ~18 persona
/// getters deleted as unreachable copies of this table's; the class stayed
/// because the INSTANCE does -- a real cppgc-backed object is what a member's receiver check and
/// `Object.prototype.toString` want, not an `Object.create`.
#[repr(C)]
pub struct JsNavigator;

#[macros::webapi(own, name = "Navigator")]
impl JsNavigator {}

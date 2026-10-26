//! `modules/chrome_object.rs` — `window.chrome`, the Chrome-proprietary object every real desktop
//! Chrome carries.
//!
//! WHAT THIS REPLACES: 37 lines of `core/korvus-runtime/src/js/host.js` — an IIFE building an object
//! literal of six JS functions, handed to the `BOOTSTRAP` masking sweep so they would report
//! `[native code]`. Five of the forty-four names in `core/korvus-runtime/tests/no_mask_live.rs`'s
//! roster are this block's (`chrome.loadTimes`, `chrome.csi`, and four `chrome.app` methods); they
//! are native by BEING native now, which is invariant I3, and they leave the kernel, which is I9.
//!
//! WHY IT EXISTS AT ALL. The ABSENCE of `window.chrome` is a top-tier headless tell: detectors read
//! `!!window.chrome` and then its `{loadTimes, csi, app}` shape. `chrome.runtime` is NOT here and the
//! three keys are in that ORDER on purpose — `Object.keys(chrome)` is read directly, string keys
//! enumerate in insertion order, and `runtime` is an EXTENSION surface a page with no extension
//! talking to it never sees.
//!
//! # What the JS got wrong, measured on `google-chrome --headless=new` 150.0.7871.46 (loopback
//! server, 2026-09-12) rather than reasoned about
//!
//! * `chrome.loadTimes.name` and `chrome.csi.name` were `"loadTimes"` / `"csi"` (named function
//!   expressions). Chrome's are the EMPTY STRING, so `String(chrome.loadTimes)` reads
//!   `function () { [native code] }` — the masker, told the name, printed
//!   `function loadTimes() { [native code] }`. Two one-line detects.
//! * `Object.keys(chrome.app)` was `["isInstalled","InstallState","RunningState","getDetails",
//!   "getIsInstalled","installState","runningState"]`; Chrome's is `["isInstalled","getDetails",
//!   "getIsInstalled","installState","runningState","InstallState","RunningState"]`. The same class
//!   of defect the `Object.keys(chrome)` order comment already records one level up.
//! * `Object.keys(chrome.csi())` was `["onloadT","pageT","startE","tran"]`; Chrome's is
//!   `["startE","onloadT","pageT","tran"]`.
//! * `Object.getOwnPropertyDescriptor(window,"chrome").configurable`: Chrome 150's was **false**
//!   (`delete window.chrome` answered `false` and the object stayed); Chrome 152's is **true** --
//!   `delete window.chrome` answers `true` and removes it (measured 2026-10-16, headed on the desktop,
//!   no DevTools, beside Chrome 150's `false`).
//! * `chrome.app.installState(cb)` called `cb` SYNCHRONOUSLY. Chrome calls it in a later TASK —
//!   measured as ordering after `setTimeout(…, 0)`, not at a microtask checkpoint.
//! * `chrome.app.getDetails("x")`, `getIsInstalled("x")`, `runningState("x")` and
//!   `installState(123)` were all accepted. Chrome's extension bindings throw
//!   `TypeError: Error in invocation of app.<name>(<signature>): ` for a call that does not match
//!   the signature. (`loadTimes` and `csi` are NOT extension bindings and do accept extra
//!   arguments — measured, and that asymmetry is kept.)
//! * `loadTimes()` was built from `Date.now()` with hardcoded offsets, so **it answered differently
//!   on every call** (Chrome's is stable: two calls deep-compare equal) and disagreed with this
//!   engine's own `performance.timing` by half a second. The fields are DERIVED from the realm's
//!   clock here — the same `spec::PerfClock` reading `PerformanceTiming` is built from
//!   (`core/timing/performance.rs`) — so the two layers cannot drift apart.
//!
//! # The mapping, measured field by field
//!
//! Against a loopback `http://127.0.0.1/` navigation, Chrome's `loadTimes()` is exactly Navigation
//! Timing L1 in SECONDS: `requestTime*1000 === timing.navigationStart`, `commitLoadTime*1000 ===
//! timing.responseStart`, `finishDocumentLoadTime*1000 === timing.domContentLoadedEventEnd`,
//! `finishLoadTime*1000 === timing.loadEventEnd`, and `csi().startE === timing.navigationStart`,
//! `csi().onloadT === timing.domContentLoadedEventEnd`, `csi().pageT === performance.now()`,
//! `csi().tran === 15`. Before the load event the two "finish" fields and `onloadT` are 0.
//!
//! Chromium reads them off the SAME Navigation Timing L1 values (`WebPerformanceMetricsForReporting`:
//! `base::Milliseconds(timing->navigationStart()).InSecondsF()`, ...), so they are read here from
//! `performance::timing_fields` -- the fields `performance.timing` answers -- and cannot disagree with it.
//!
//! `firstPaintTime` / `firstPaintAfterLoadTime` are 0, which is what headless Chrome reports (no
//! paint), and `navigationType` is `"Other"`.
//!
//! # The one thing this unit could not derive
//!
//! `wasFetchedViaSpdy` / `wasNpnNegotiated` / `npnNegotiatedProtocol` / `connectionInfo` describe the
//! CONNECTION, and nothing in this engine knows it: `plugins/korvus-transport`'s `FetchResponse`
//! carries status, url, headers and body and no negotiated ALPN, and `PerformanceNavigationTiming`'s
//! `nextHopProtocol` is `""` for the same reason. The four values are carried over VERBATIM from the
//! JS (`true`, `true`, `"h2"`, `"h2"`) — see the `Ruling` in this file's commit. Answering `""` /
//! `false` instead would be a new tell, not honesty: no Chrome reports `connectionInfo: ""`.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, set_prop, str_val, throw_type_error};

/// `chrome.csi().tran` — Chrome's `PageTransition` code for a normal navigation (`TRANSITION_OTHER`).
/// Measured as 15 on every navigation this unit probed, including the load-event-less case.
const CSI_TRAN: f64 = 15.0;

/// An own `{writable:true, enumerable:true, configurable:true}` data property, which is what every
/// key of `chrome` and of `chrome.app` carries in Chrome (measured). A plain `Set` on a fresh object
/// with `Object.prototype` in its chain produces exactly that, so this is `set_prop` under a name
/// that says which descriptor was measured.
fn own<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, key: &str, v: v8::Local<'s, v8::Value>) {
    set_prop(scope, o, key, v);
}

fn own_str<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, key: &str, v: &str) {
    let s = str_val(scope, v);
    own(scope, o, key, s);
}

fn own_num<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, key: &str, v: f64) {
    let n: v8::Local<v8::Value> = v8::Number::new(scope, v).into();
    own(scope, o, key, n);
}

fn own_bool<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, key: &str, v: bool) {
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, v).into();
    own(scope, o, key, b);
}

/// Chrome's extension-binding arity error, verbatim including the trailing `": "`:
/// `TypeError: Error in invocation of app.getDetails(): `. `signature` is the binding's declared
/// argument list, which Chrome prints inside the parentheses (`""` for the three no-argument
/// members, `"optional function callback"` for `installState`).
fn binding_arity_error(scope: &mut v8::PinScope, member: &str, signature: &str) {
    throw_type_error(scope, &format!("Error in invocation of app.{member}({signature}): "));
}

// ── chrome.loadTimes / chrome.csi ────────────────────────────────────────────────────────────────

/// `chrome.loadTimes()` — Navigation Timing L1 in seconds, in Chrome's recorded key order.
///
/// NO receiver check and no arity check: measured, `chrome.loadTimes.call(null)` answers normally and
/// `chrome.loadTimes("x")` returns the same thirteen keys. It is a plain V8 extras function in
/// Chrome, not an extension binding, so it has neither.
fn cb_load_times(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The SAME fields `performance.timing` answers, as Chromium's binding reads them: each L1 value in
    // seconds, `base::Milliseconds(ms).InSecondsF()` -- 0 for one that has not happened.
    let f = crate::core::timing::performance::timing_fields(scope);
    let secs = |name: &str| {
        let ms = f.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(0.0) as i64;
        (ms * 1000) as f64 / 1_000_000.0
    };
    let o = v8::Object::new(scope);
    own_num(scope, o, "requestTime", secs("navigationStart"));
    own_num(scope, o, "startLoadTime", secs("navigationStart"));
    own_num(scope, o, "commitLoadTime", secs("responseStart"));
    own_num(scope, o, "finishDocumentLoadTime", secs("domContentLoadedEventEnd"));
    own_num(scope, o, "finishLoadTime", secs("loadEventEnd"));
    // Headless Chrome paints nothing and reports both as 0 (measured, twice).
    own_num(scope, o, "firstPaintTime", 0.0);
    own_num(scope, o, "firstPaintAfterLoadTime", 0.0);
    own_str(scope, o, "navigationType", "Other");
    // The four connection facts this engine cannot derive — see the module header.
    own_bool(scope, o, "wasFetchedViaSpdy", true);
    own_bool(scope, o, "wasNpnNegotiated", true);
    own_str(scope, o, "npnNegotiatedProtocol", "h2");
    own_bool(scope, o, "wasAlternateProtocolAvailable", false);
    own_str(scope, o, "connectionInfo", "h2");
    rv.set(o.into());
}

/// `chrome.loadTimes` / `chrome.csi`: an anonymous, constructible function of length 0 over `cb` -- from a
/// template (`install::templates::standalone_fn`, `ConstructorBehavior::Allow`), so V8 gives each realm's its
/// own `prototype` with a `constructor` back to it, as the extension's function expressions have in Chrome.
///
/// What an API function cannot copy (reported, not hidden): Chromium compiles the two as extension SCRIPT, so
/// they are sloppy-mode functions whose `fn.arguments` and `fn.caller` read `null` (Chrome 150, measured);
/// reading either off an API function throws TypeError. Matching that needs the functions compiled as a V8
/// extension, which the embedder API here does not expose.
fn load_times_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>) -> v8::Local<'s, v8::Function> {
    crate::install::templates::standalone_fn(scope, v8::MapFnTo::map_fn_to(cb), "", 0, None, true, true)
}

/// `chrome.csi()` — four numbers, in Chrome's recorded key order (`startE` first; the JS had it
/// third).
fn cb_csi(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let f = crate::core::timing::performance::timing_fields(scope);
    let field = |name: &str| f.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(0.0);
    // NOT `performance.now()`: Chromium's binding measures `pageT` off the WALL clock
    // from the floored `navigationStart`, at microsecond resolution and with no Spectre clamp — so it
    // has three decimal places where a `DOMHighResTimeStamp` has the clamped tick, and it carries the
    // origin's sub-millisecond fraction that `now()` does not. `spec::PerfClock::legacy_page_ms` has
    // the measurement.
    let page_t = crate::core::timing::performance::clock_legacy_page_ms(scope);
    let o = v8::Object::new(scope);
    own_num(scope, o, "startE", field("navigationStart"));
    // `timing.domContentLoadedEventEnd` (0 before it), the same field `finishDocumentLoadTime` reports.
    own_num(scope, o, "onloadT", field("domContentLoadedEventEnd"));
    own_num(scope, o, "pageT", page_t);
    own_num(scope, o, "tran", CSI_TRAN);
    rv.set(o.into());
}

// ── chrome.app ───────────────────────────────────────────────────────────────────────────────────

/// `chrome.app.getDetails()` — `null` for a page that is not a hosted app, which every page here is.
fn cb_get_details(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() > 0 {
        binding_arity_error(scope, "getDetails", "");
        return;
    }
    rv.set_null();
}

fn cb_get_is_installed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() > 0 {
        binding_arity_error(scope, "getIsInstalled", "");
        return;
    }
    rv.set_bool(false);
}

fn cb_running_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() > 0 {
        binding_arity_error(scope, "runningState", "");
        return;
    }
    let s = str_val(scope, "cannot_run");
    rv.set(s);
}

/// `chrome.app.installState(optional function callback)` — returns `undefined` and delivers
/// `"not_installed"` to the callback in a LATER TASK.
///
/// The task, not a microtask: measured, the callback runs after a `setTimeout(…, 0)` queued before
/// it, which a microtask never would. The JS called it synchronously. `globalThis.setTimeout` is the
/// same route `Window.postMessage`'s delivery takes (`core/html/window.rs`), for the same reason —
/// this engine's task queue is the runtime's, reached through the row that already owns it.
fn cb_install_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let cb = args.get(0);
    if args.length() > 1 || (args.length() == 1 && !cb.is_function()) {
        binding_arity_error(scope, "installState", "optional function callback");
        return;
    }
    if !cb.is_function() {
        return;
    }
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    // The Global round trip is the elided-lifetime gotcha `core/html/window.rs::post_message`
    // documents: `args.get(0)` borrows the CALLBACK's argument lifetime, which is not the scope's, so
    // it cannot be handed straight to `build_fn_data`.
    let held = v8::Global::new(scope, cb);
    let data: v8::Local<v8::Value> = v8::Local::new(scope, &held);
    let deliver = build_fn_data(scope, cb_deliver_install_state, "", 0, data);
    let ms: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[deliver.into(), ms]);
}

fn cb_deliver_install_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(f) = v8::Local::<v8::Function>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let state = str_val(scope, "not_installed");
    f.call(scope, undef, &[state]);
}

/// One of the two frozen-looking enum objects (`InstallState`, `RunningState`). They are ordinary
/// `{w,e,c}` objects in Chrome, not frozen — measured.
fn enum_object<'s>(scope: &mut v8::PinScope<'s, '_>, pairs: &[(&str, &str)]) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    for (k, v) in pairs {
        own_str(scope, o, k, v);
    }
    o
}

// ── the install ──────────────────────────────────────────────────────────────────────────────────

/// `chrome.search()` — Opera's fourth key, and the only member of this object that is not Chrome's.
///
/// MEASURED on the installed Opera 135.0.5973.133 (headed under Xvfb, 2026-09-13): a native anonymous
/// function, `length` 2, `String(chrome.search)` `"function () { [native code] }"`, and it answers
/// **undefined** for zero, one and two arguments alike — no arity error, unlike `chrome.app`'s four
/// extension bindings, which is a difference worth keeping rather than smoothing over.
fn cb_search(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_undefined();
}

/// The KEY ORDER of `window.chrome`, joined from the device and this file.
///
/// P3, one object below an interface: the bundle says WHICH keys and in what ORDER
/// (`Persona::chrome_object_keys`, straight off the capture's `values.json`), this file says HOW each
/// one answers. Before the fourth bundle the two were one thing — a fixed sequence of `own(...)`
/// statements — and that was invisible while every capture said `[loadTimes, csi, app]`. Opera 135
/// says `[loadTimes, csi, search, app]`, with the extra key THIRD rather than appended, so an engine
/// that owned the order could not have worn it.
///
/// A device that names no keys (no bundle) gets every body this file has, which is the reading
/// `install::surface` gives a realm with no active capture. A key the device names and this file has
/// no body for is reported like any other install gap and installed as nothing — never silently
/// skipped, because an unnoticed hole here is a `Object.keys(chrome)` a page can read.
fn install_chrome_keys<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    chrome: v8::Local<'s, v8::Object>,
    keys: &[String],
) {
    // The union of bodies, in the union's own order — which is Opera's, since it is the device with
    // every key. Used verbatim only when the device cannot say.
    const UNION: [&str; 4] = ["loadTimes", "csi", "search", "app"];
    let wanted: Vec<&str> =
        if keys.is_empty() { UNION.to_vec() } else { keys.iter().map(String::as_str).collect() };
    for k in wanted {
        match k {
            // `.name` is the EMPTY STRING on all three functions, which is what Chrome reports and
            // what makes `String(chrome.loadTimes)` read `function () { [native code] }`.
            //
            // `loadTimes` and `csi` are CONSTRUCTORS, each with its own `prototype` (Chrome
            // 150 measured: own names `length,name,prototype`, `prototype` `{writable:true, enumerable:false,
            // configurable:false}` owning only `constructor`, and `new chrome.csi()` the four-key object).
            // Chromium defines them as plain function expressions in the `v8/LoadTimes` extension
            // (`chrome/renderer/loadtimes_extension_bindings.cc:56-63`), and a function expression is
            // constructible; returning an object from it makes `new` answer that object, which an API
            // function's callback does too. `search` and `chrome.app`'s four are extension or embedder
            // natives, not constructors (measured: `new chrome.app.getDetails()` is "not a constructor").
            "loadTimes" => {
                let f = load_times_fn(scope, cb_load_times);
                own(scope, chrome, "loadTimes", f.into());
            }
            "csi" => {
                let f = load_times_fn(scope, cb_csi);
                own(scope, chrome, "csi", f.into());
            }
            "search" => {
                let f = build_fn(scope, cb_search, "", 2);
                own(scope, chrome, "search", f.into());
            }
            "app" => {
                let app = build_app(scope);
                own(scope, chrome, "app", app.into());
            }
            other => eprintln!("[korvus] chrome install gap: chrome.{other}"),
        }
    }
}

/// Build `window.chrome` and define it on the global.
///
/// KEY ORDER IS THE SURFACE. `Object.keys(chrome)` and `Object.keys(chrome.app)` are read directly by
/// detectors and string keys enumerate in insertion order, so the order the keys are inserted in IS
/// the answer — `chrome.app`'s is the measured one below, and `chrome`'s own is the DEVICE's, joined
/// by [`install_chrome_keys`].
pub(crate) fn install_window_chrome<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let chrome = v8::Object::new(scope);
    let keys = persona_chrome_keys(scope);
    install_chrome_keys(scope, chrome, &keys);

    // `{writable:true, enumerable:true, configurable:true}` — Chrome 152's, measured (Chrome 150's
    // was configurable:false, which this install then locked at the seat). The registry walk's seat
    // places it at the capture's position: Chrome has `chrome` after V8's staged builtins, not where
    // this install runs.
    let value: v8::Local<v8::Value> = chrome.into();
    let mut d = v8::PropertyDescriptor::new_from_value_writable(value, true);
    d.set_enumerable(true);
    d.set_configurable(true);
    if let Some(k) = v8::String::new(scope, "chrome") {
        global.define_property(scope, k.into(), &d);
    }
}

// ── chrome.app waits for the frame's first navigation ────────────────────────────────────────────

/// `chrome.app` is an EXTENSION binding, and the extensions dispatcher defers a main-world context's bindings
/// when the context is made while its frame is requesting a navigation -- they need that navigation's URL --
/// until the navigation is ready to commit (`ExtensionFrameHelper::DidCreateScriptContext`,
/// `extension_frame_helper.cc:420-444`; `IsRequestingNavigation` is set by `BeginNavigationInternal` and cleared
/// at commit, `render_frame_impl.cc:6196, 5343`). `loadTimes` and `csi` are a V8 extension every context gets.
/// So a frame inserted with a `srcdoc` or a `src` to fetch shows its initial about:blank document with
/// `Object.keys(chrome)` `["loadTimes","csi"]`, and the same `chrome` object gains `app` when the document
/// commits (Chrome 150; a sensor cookie VM reads a `srcdoc` frame's window at
/// insertion). A frame with no navigation to make -- no `src`, `about:blank` -- has `app` from the start.
///
/// [`take_app`] takes it off `global`'s `chrome` when the frame's context is made that way and hands back the
/// object; [`restore_app`] puts the same object back, last, at the commit.
pub(crate) fn take_app<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let chrome = chrome_of(scope, global)?;
    let k = v8::String::new(scope, "app")?;
    if !chrome.has_own_property(scope, k.into()).unwrap_or(false) {
        return None;
    }
    let app = chrome.get(scope, k.into())?;
    chrome.delete(scope, k.into());
    Some(app)
}

/// [`take_app`]'s object back on `global`'s `chrome`, as the extension bindings install it at the commit: an
/// ordinary `{writable, enumerable, configurable}` key, after every key the object already has.
pub(crate) fn restore_app<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, app: v8::Local<'s, v8::Value>) {
    let Some(chrome) = chrome_of(scope, global) else { return };
    own(scope, chrome, "app", app);
}

/// `global`'s own `chrome` object -- whatever the page left there, as `GetOrCreateChrome` reads it -- or `None`.
fn chrome_of<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let k = v8::String::new(scope, "chrome")?;
    let v = global.get(scope, k.into())?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// The persona this realm wears, asked for its own `Object.keys(chrome)`. The same I9 seam every
/// other persona-driven value reads through: the runtime PROVIDES the identity, the Web API READS it,
/// and there is no `KORVUS_BUNDLE` anywhere on the path.
fn persona_chrome_keys(scope: &mut v8::PinScope) -> Vec<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)
        .map(|p| p.0.chrome_object_keys())
        .unwrap_or_default()
}

/// `chrome.app` — its own key order is the measured one (see the module header) and is NOT the
/// device's: all four captures record the same seven keys in the same order.
fn build_app<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let app = v8::Object::new(scope);
    own_bool(scope, app, "isInstalled", false);
    let get_details = build_fn(scope, cb_get_details, "getDetails", 0);
    own(scope, app, "getDetails", get_details.into());
    let get_is_installed = build_fn(scope, cb_get_is_installed, "getIsInstalled", 0);
    own(scope, app, "getIsInstalled", get_is_installed.into());
    let install_state = build_fn(scope, cb_install_state, "installState", 0);
    own(scope, app, "installState", install_state.into());
    let running_state = build_fn(scope, cb_running_state, "runningState", 0);
    own(scope, app, "runningState", running_state.into());
    let install_enum = enum_object(
        scope,
        &[("DISABLED", "disabled"), ("INSTALLED", "installed"), ("NOT_INSTALLED", "not_installed")],
    );
    own(scope, app, "InstallState", install_enum.into());
    let running_enum = enum_object(
        scope,
        &[("CANNOT_RUN", "cannot_run"), ("READY_TO_RUN", "ready_to_run"), ("RUNNING", "running")],
    );
    own(scope, app, "RunningState", running_enum.into());
    app
}

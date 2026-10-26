//! `core/html/window.rs` -- the `Window` interface's own members.
//!
//! WHAT THIS REPLACES. All of `dom/09f-window.js`: 340 lines, seven `Object.defineProperty` sites,
//! two `op_nativize` masks and the `KB.__potentiallyTrustworthy` bridge slot.
//!
//! WHERE THE MEMBERS LIVE. `Window` is a `[Global]` interface, and that moves its members: WebIDL
//! defines a `[Global]` interface's members as own properties of the GLOBAL OBJECT instead of on the
//! interface prototype object. The captured baseline says the same thing from the other side --
//! `Window.prototype` owns exactly `TEMPORARY` and `PERSISTENT` and nothing else, while `window`
//! itself owns `name`, `status`, `closed`, `length`, `opener` and the 125 `on*` handlers. So the two
//! `constant` rows below go on the prototype (and on the interface object, as WebIDL puts a constant
//! in both places) and EVERY other row is mounted on `globalThis`; putting any of them on
//! `Window.prototype` would be a structure difference against the baseline, not an improvement.
//! (`addEventListener` and friends are NOT Window members -- they are inherited from `EventTarget`,
//! which is why they stay on `EventTarget.prototype`.)
//!
//! NATIVE BY CONSTRUCTION. Every accessor and method here is a real `v8::Function` built by the one
//! installer from a Rust callback, so `Function.prototype.toString` reports `[native code]` and the
//! function carries no own `prototype`/`arguments`/`caller`. A plain `get() { ... }` on the global
//! object is a first-tier automation tell exactly because the real ones are native -- and it is the
//! single reason the first attempt at this interface was rejected. The deleted JS reached the same
//! place through `op_nativize`, which wrapped a JS body in a native shell; these bodies ARE Rust.
//!
//! WHAT IS DELIBERATELY ABSENT. `orientation`, `onorientationchange`, `fence`, `portalHost`,
//! `pushManager`, `getDigitalGoodsService`, `requestResize`, `minimize`/`maximize`/`restore`/
//! `setResizable`, and `oncopy`/`oncut`/`onpaste` are all missing from the baseline's `globals` list,
//! and the recordings agree with it: `window.fence` reads `undefined`, and `window.minimize()` throws
//! TypeError *because there is no such function*. Defining any of them -- even to throw -- would put a
//! property on `window` that a real Chrome 151 does not have. Absence is the implementation.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, global_get, make_arity_guard, str_val};
use crate::install::kits::{define_accessor, define_value};
use crate::install::MemberDef;

// ── the window's own writable state ──────────────────────────────────────────────────────────────

/// `name` -- the writable Window attribute whose value lives behind the accessor, in `OpState`, not on
/// the object: the deleted JS kept it in module scope for the same reason (no `__`-prefixed global, no
/// own data property shadowing the accessor), and a Rust struct is that module scope. (`opener` was
/// another: its setter replaces the accessor instead; `status` is per Window since
/// See [`cb_status_get`].)
#[derive(Default)]
pub struct WindowState {
    name: String,
}

fn with_window<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut WindowState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<WindowState>().is_none() {
        st.put(WindowState::default());
    }
    f(st.borrow_mut::<WindowState>())
}

/// WebIDL's ES-value-to-DOMString conversion, which is NOT `String(v)`. `String()` is specially
/// permitted to describe a Symbol -- `String(Symbol("x"))` is "Symbol(x)" -- while the WebIDL
/// conversion runs ToString, and ToString on a Symbol throws. So `window.name = Symbol()` is a
/// TypeError in every browser and used to store the string "Symbol()" here.
fn idl_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    if v.is_symbol() {
        crate::binding::protos::throw_type_error(scope, "Cannot convert a Symbol value to a string");
        return None;
    }
    Some(v.to_rust_string_lossy(scope))
}

// ── location-derived facts ───────────────────────────────────────────────────────────────────────
//
// `isSecureContext`, `originAgentCluster` and `origin` are answers ABOUT THE DOCUMENT'S ORIGIN, so
// they are computed from `location` rather than pinned. Parsing goes through the same WHATWG URL
// parser `location` itself uses, so the answers never disagree with `location.origin`.

/// This realm's `location.href`. `pub(crate)` because `core/html/exposure.rs` decides a
/// `[SecureContext]` name's presence from the SAME string this file answers `isSecureContext` from --
/// one reading, so the gate and the attribute cannot disagree about the document they are describing.
pub(crate) fn href(scope: &mut v8::PinScope) -> String {
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "location") else { return String::new() };
    let Some(loc) = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return String::new();
    };
    get_prop(scope, loc, "href").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// "Is url potentially trustworthy" (Secure Contexts sec. 3.2), verbatim in the parts that can arise
/// here: `about:blank`/`about:srcdoc` and `data:` are trustworthy by the URL rule; otherwise the
/// origin decides -- https/wss, file, and the loopback hosts are trustworthy and an opaque origin is
/// not. So a page served over http:// correctly reports FALSE; this is not a constant.
///
/// ONE definition, called directly by `core/html/isolation_policy.rs`, which used to reach it through
/// the `KB.__potentiallyTrustworthy` bridge slot -- so `isSecureContext` and "was this document's COOP
/// honoured" still cannot disagree, with one less indirection and one less bridge name.
///
/// `about:blank` / `about:srcdoc` are Chromium's `GURL::IsAboutBlank` / `IsAboutSrcdoc`: the `about` scheme with no
/// host, credentials or port and a path of exactly `blank` / `srcdoc` (or one trailing `/`) -- a query or a fragment
/// does not matter. It compared the whole string, so `location.hash = "x"` on an about:blank document
/// (`about:blank#x`) turned `isSecureContext` false and `Notification.permission` "denied".
pub fn potentially_trustworthy(u: &str) -> bool {
    let Ok(url) = url::Url::parse(u) else { return false };
    if url.scheme() == "about" && url.host().is_none() && url.username().is_empty() && url.password().is_none() && url.port().is_none() {
        let path = url.path();
        let about = |name: &str| path == name || (path.len() == name.len() + 1 && path.ends_with('/') && path.starts_with(name));
        if about("blank") || about("srcdoc") {
            return true;
        }
    }
    let scheme = url.scheme();
    if scheme == "data" || scheme == "https" || scheme == "wss" || scheme == "file" {
        return true;
    }
    let host = url.host_str().unwrap_or("");
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1" || host.ends_with(".localhost") || host.starts_with("127.")
}

/// The document's origin, serialized -- the same string `location.origin` gives, and "null" for an
/// opaque one.
fn origin_of(u: &str) -> Option<String> {
    let url = url::Url::parse(u).ok()?;
    let s = url.origin().ascii_serialization();
    Some(s)
}

// ── the receiver check ──────────────────────────────────────────────────────────────────────────

/// Is `this` a Window -- the realm's global, or a nested/auxiliary browsing context's window. The
/// whole check is [`crate::core::html::window_globals::window_receiver`], which every window
/// singleton accessor runs too, so the two can never disagree.
///
/// Probed rather than assumed: headless Chrome 150.0.7871.46 on this machine, over a loopback server
/// (2026-09-11), throws `Illegal invocation` from EVERY Window member given a foreign receiver --
/// `Object.getOwnPropertyDescriptor(window, "name").get.call({})`, `window.alert.call({})`,
/// `window.postMessage.call({}, 1, "*")`, the `closed` getter and the `onload` getter alike. The
/// deleted JS checked none of them (`nat("get name", 0, () => winName)` simply ignores `this`), so
/// every one of the 157 rows answered instead of throwing.
///
/// It used to accept anything with `Window.prototype` on its chain. A measurement found that on
/// all four browsers of the family: `get.call(Object.create(Window.prototype))` is `Illegal
/// invocation` for every window accessor (`tests/fixtures/p11a/recv.*.txt`) -- a prototype is not a
/// window.
fn recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    crate::core::html::window_globals::window_receiver(scope, this)
}

/// The receiver check as an OUTER wrapper. `data` is the inner function.
fn cb_recv_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(inner) = v8::Local::<v8::Function>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    recv_guard_run(scope, &args, rv, inner);
}

/// [`cb_recv_guard`]'s body (the install's wrapper; a template fuses [`recv_guard_check`] instead).
pub(crate) fn recv_guard_run(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, inner: v8::Local<v8::Function>) {
    if !recv_guard_check(scope, args) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    // `inner` throwing returns None with the exception pending; leaving `rv` untouched propagates it.
    if let Some(r) = inner.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// [`recv_guard_run`]'s check, shared with a fused member (`false`: thrown).
pub(crate) fn recv_guard_check(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    recv(scope, args.this().into())
}

/// Wrap `inner` in the receiver check, keeping the member's `name` and `length` -- the wrapper is what
/// the page sees, so its identity has to be the member's. OUTSIDE any arity guard, which is Chrome's
/// order (`Window.prototype`-level members throw `Illegal invocation` before counting arguments).
///
/// AND IT SAYS SO, which it did not until this was fixed -- independently, by TWO units, hours apart.
///
/// The pair carries `inner`'s arity brand for the reason `install::receiver`'s own wrapper does: a
/// guarded pair still counts arguments, so any later arity pass must SKIP it rather than wrap again
/// from the outside, which would put the count before the receiver and reverse Chrome's order. This
/// wrapper never stamped itself, so `install_arity_guards`'s WINQ pass -- which wrapped any member
/// whose OUTERMOST function was not stamped `korvus.arityGuarded` -- added a SECOND guard on the
/// outside of every row built guard-inside: `postMessage`, `reportError`, `moveBy`, `moveTo`,
/// `resizeBy`, `resizeTo`, and the catch-all arm's `required > 0` methods.
///
/// The result was measurable and wrong: `window.postMessage.call({})` answered "1 argument required"
/// and `window.moveBy.call({})` answered "2 arguments required, but only 0 present.", where Chrome
/// answers `Illegal invocation` -- measured with `google-chrome --headless=new` 150.0.7871.46 for all
/// THIRTEEN WINQ-guarded Window members, by one change, and independently by another for the six it
/// touched.
///
/// `binding::protos::stamp_arity_guarded`'s own doc comment describes this exact repair, and
/// `install::receiver`'s wrapper already did it. This one did not, and every Window method with a
/// required argument had the order reversed because of it.
///
/// The WINQ pass itself is GONE now (`generated_order.rs` and both its passes are deleted) and
/// `Window` is a hand table rather than a registry row, so nothing walks these slots today. The brand
/// stays so that remains true BY CONSTRUCTION rather than by nobody having written the pass.
///
/// EVERY `[Global]` interface's rows take it, not only `Window`'s: a worker scope's global is its realm's
/// global too, and [`recv`]'s check -- this realm's global, or another browsing context's window -- is the
/// worker's whole answer (Chrome 150.0.7871.46 throws `Illegal invocation` from each dedicated, shared and
/// service worker `[Global]` member given `{}` or `Object.create(<Scope>.prototype)`, measured)
pub(crate) fn guarded<'s>(scope: &mut v8::PinScope<'s, '_>, inner: v8::Local<'s, v8::Value>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    let g = build_fn_data(scope, cb_recv_guard, name, length, inner);
    // A GUARD, recorded as one (the Window global): the template fuses its check into the
    // member's one native call, and needs to know it is a check rather than a body with a function for data.
    if crate::install::templates::capturing() {
        if let Ok(f) = v8::Local::<v8::Function>::try_from(inner) {
            crate::install::templates::record_guard(scope, g, name, length, crate::install::templates::Guard::WindowRecv, f);
        }
    }
    if crate::binding::protos::is_arity_guarded(scope, inner) {
        crate::binding::protos::stamp_arity_guarded(scope, g);
    }
    g
}

/// F2 (goal 1021): a `Window` method built by `build_fn` -- a member mounted OUTSIDE every interface template --
/// that a loaded plugin claims (`Window.<name>`) is wrapped on `cb_claimed` (`templates::claim_untemplated`), so the
/// plugin's `on_call`/`on_call_pre` see it exactly as a template-built claimed member; otherwise the ordinary
/// `build_fn`, so an unclaimed member pays nothing (invariant I7). The arity/receiver guards still wrap the result
/// as before -- the plugin sees the call at the body's exit, after the guards have passed. `cb` is `Copy` (every
/// body here is a fn item), so it serves both the claim attempt and the fallback.
///
/// TODO(1021-verify): the `Window` attribute accessors and the 125 `on*` handlers are `build_fn_data`/kit-built
/// members mounted here too; `claim_untemplated` takes their body data and so can claim them the same way, left for
/// the integration pass to extend if a plugin needs to claim an accessor rather than a method.
fn win_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback> + Copy, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    // The claim seam is a release-build concept (an analysis build fuses every member already, `claim_untemplated`
    // is `cfg(not(analysis))`): compiled out of an analysis build, where this is just `build_fn`. `cb` is `Copy`, so
    // the claim attempt (which takes the raw callback) and the `build_fn` fallback each get their own copy.
    #[cfg(not(feature = "analysis"))]
    {
        if let Some(f) = crate::install::templates::claim_untemplated(scope, cb.map_fn_to(), None, "Window", name, length) {
            return f;
        }
    }
    build_fn(scope, cb, name, length)
}

// ── the bodies ───────────────────────────────────────────────────────────────────────────────────

fn cb_name_get(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = with_window(scope, |w| w.name.clone());
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_name_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(s) = idl_string(scope, a.get(0)) {
        with_window(scope, |w| w.name = s);
    }
}
/// `window.status` belongs to its WINDOW: a frame's assignment is not the top window's, and a new
/// frame starts at "" (Chrome 150, measured: `f.contentWindow.status = "x"` leaves `window.status` ""
/// and the next frame's ""). Kept on the realm's global under a private, not in the per-page state.
fn status_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus_window_status").unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn cb_status_get(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    let k = status_key(scope);
    match global.get_private(scope, k).filter(|v| v.is_string()) {
        Some(v) => rv.set(v),
        None => rv.set(str_val(scope, "")),
    }
}
fn cb_status_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(s) = idl_string(scope, a.get(0)) {
        let global = scope.get_current_context().global(scope);
        let k = status_key(scope);
        let v = str_val(scope, &s);
        global.set_private(scope, k, v);
    }
}
/// `get opener`: null. Only a script-opened popup has an opener, nothing in this engine opens one for
/// the page realm, and a set replaces this accessor rather than storing behind it.
/// `opener`: null for a window no script opened -- and, for a popup whose opener runs in another process (D74 step 3),
/// the opener's cross-origin WindowProxy (`remote_frame::opener_proxy`).
fn cb_opener_get(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match crate::core::html::remote_frame::opener_proxy(scope) {
        Some(p) => rv.set(p.into()),
        None => rv.set_null(),
    }
}

/// `close()`: a window a script opened may close itself (HTML "script-closable"); under the browser endpoint such a
/// window is a popup's own target, and closing it closes the target (`core/html/popup.rs`). Any other top-level
/// window ignores it, as Chrome does ("Scripts may close only the windows that were opened by them.").
fn cb_window_close(_scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if crate::core::html::popup::opened_by_script() {
        crate::core::html::popup::close_self();
    }
}
/// `set opener`: HTML's setter replaces the accessor with an own `{writable, enumerable, configurable}`
/// data property holding the value -- `null` included, which the spec additionally uses to disown the
/// opener. Measured on all four browsers of the family: `opener = {}` then
/// `window.opener === thatObject` from an own data property, and `opener = null` then an own data
/// property holding `null`. It used to store the value behind the accessor, which left the accessor
/// standing.
pub(crate) fn cb_opener_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = a.this();
    let Some(k) = v8::String::new(scope, "opener") else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(a.get(0), true);
    d.set_enumerable(true);
    d.set_configurable(true);
    this.define_property(scope, k.into(), &d);
}

/// `closed` is true once the browsing context has been DISCARDED. Korvus's page realm is the isolate
/// itself and nothing in the engine discards it while script can still run to ask -- there is no
/// `window.close()` path that could flip this, so it is false for the same reason `window.closed` is
/// false in every top-level tab that was not opened by script.
fn cb_false(_scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(false);
}

/// `frameElement` -- the element that embeds this window in its parent document. A top-level browsing
/// context has none, and the page realm is one, so this is null.
fn cb_null(_scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_null();
}

/// `length` is the number of NESTED BROWSING CONTEXTS, which is the count of frame-owning elements in
/// the document -- not a fixed 0. `<iframe>` and `<frame>` create one unconditionally, so they always
/// count; `<object>` and `<embed>` create one only once they have loaded a document, and this engine
/// never loads one for them, so counting them would claim nested contexts that do not exist.
fn cb_length(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = (|| {
        let doc = global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        let sel: v8::Local<v8::Value> = str_val(scope, "iframe, frame");
        // The realm's ORIGINAL `querySelectorAll` and `NodeList.length`: Chrome counts its child frames in C++, so a
        // page that wrapped either sees no call when it reads `window.length` (Chrome 150, measured).
        let list = crate::binding::protos::pristine_query_all(scope, doc.into(), sel)?;
        crate::binding::protos::pristine_get(scope, list.into(), "NodeList.length")?.uint32_value(scope)
    })()
    .unwrap_or(0);
    rv.set_double(n as f64);
}

/// The page document's `Origin-Agent-Cluster` header, when its response sent one (`?1` / `?0`).
#[derive(Default, Clone, Copy)]
pub(crate) struct DocOriginAgentCluster(pub Option<bool>);

/// Read the page document's `Origin-Agent-Cluster` from its delivered headers, per navigation.
pub fn set_document_origin_agent_cluster(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    let v = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("origin-agent-cluster"))
        .filter_map(|(_, v)| match v.trim() {
            "?1" => Some(true),
            "?0" => Some(false),
            _ => None,
        })
        .last();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(DocOriginAgentCluster(v));
}

/// Origin-keyed agent clusters. Chrome 150 keys agent clusters by ORIGIN unless the document's response
/// opts out with `Origin-Agent-Cluster: ?0` -- measured headed over loopback (`tools/realweb/seed/refperm`):
/// no header -> true, `?1` -> true, `?0` -> false, and a cross-origin frame of a `?0` page with no header
/// of its own -> true; all four bundles record true for a page with none. An OPAQUE origin is always
/// origin-keyed. It answered false for every tuple origin (no header pipeline), which a bot challenge
/// page -- served `Origin-Agent-Cluster: ?1` -- read in the page and in its frame.
fn cb_origin_agent_cluster(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let h = href(scope);
    if origin_of(&h).map_or(true, |o| o == "null") {
        rv.set_bool(true);
        return;
    }
    let declared = match crate::core::html::browsing_context::current_realm_ctx(scope) {
        Some(id) => crate::core::html::browsing_context::frame_delivered(scope, id).and_then(|d| d.origin_agent_cluster),
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<DocOriginAgentCluster>().and_then(|d| d.0)
        }
    };
    rv.set_bool(declared.unwrap_or(true));
}

fn cb_is_secure_context(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let h = href(scope);
    rv.set_bool(potentially_trustworthy(&h));
}

fn cb_origin(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A FRAME's window answers its own document's origin -- an `about:` one its creator's
    // (Chrome 150: a cross-origin frame's blank child reports the frame's `self.origin`).
    let this: v8::Local<v8::Value> = a.this().into();
    if v8::Local::<v8::Object>::try_from(this).ok().is_some_and(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some()) {
        let s = crate::core::html::browsing_context::realm_origin(scope, this);
        let v = str_val(scope, &s);
        rv.set(v);
        return;
    }
    let h = href(scope);
    let s = origin_of(&h).unwrap_or_else(|| "null".to_string());
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The nine user-interaction and navigation methods, whose answers are bounded by having no UI and no
/// second browsing context -- and each is the SPEC's own answer for the case we are in rather than an
/// invention:
///
/// ```text
/// alert/print/focus/blur/stop/close  undefined; there is no dialog to show and none to dismiss, and
///                                    `close` does nothing because the spec only closes a
///                                    script-opened, script-closable browsing context.
/// confirm                            false, the answer for a dismissed dialog.
/// prompt                             null, likewise.
/// moveBy/moveTo/resizeBy/resizeTo    undefined; there is no window manager to ask.
/// captureEvents/releaseEvents        undefined -- NOT stubs: the HTML standard defines both as doing
///                                    nothing, and Chrome's do nothing. A native no-op is the whole
///                                    implementation.
/// ```
fn cb_void(_scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// `stop` -- nothing for the page (its parse is over before any of its scripts runs here), and for a
/// frame's window the abort of that frame document's running parser.
fn cb_stop(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    crate::core::html::browsing_context::frame_window_stop(scope, a.this());
}

/// `print` — THE ENGINE'S OUTPUT SINK, and the one row of this table whose body is not a browser's.
///
/// `globalThis.print` is how every test, every `korvus fetch` probe and the conformance runner read a
/// value out of a realm: `op_kprint` appends to the kernel's `PrintBuffer`, which `Engine::take_output`
/// drains. It used to be installed by `BOOTSTRAP` as `globalThis.print = (s) => op_kprint(String(s))`
/// — a plain JS arrow, assigned over whatever this table had put in the slot — and that assignment is
/// the whole reason `install_window_window` is positioned BEFORE the bootstrap script: installed after
/// it, the table's `print` no-op won and the engine went silent while every test still passed green.
///
/// One row, one body, no order: this IS the sink now, so nothing overwrites it and the position of the
/// install stops mattering for `print`. (The install is still pre-bootstrap. Moving it is a
/// measurement, not a tidy-up, and this unit did not measure it.)
///
/// `String(s)` semantics are kept exactly: `print()` with no argument records `"undefined"`, which is
/// what the deleted arrow did, and `to_rust_string_lossy` runs the argument's own `toString` the same
/// way. The op is reached through the STASHED ops table (`op_stash_ops`), the way
/// `core/events/report.rs` reaches `op_worker_self_error` — webapi cannot name a kernel op directly.
fn cb_print(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = a.get(0).to_rust_string_lossy(scope);
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_kprint") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let msg = str_val(scope, &s);
    v8::tc_scope!(let tc, scope);
    f.call(tc, undef, &[msg]);
    tc.reset();
}
/// A dialog's `message` (and `prompt`'s `default`): WebIDL `optional DOMString = ""` -- an absent or `undefined`
/// argument is the empty string (`alert()` shows nothing, `alert(undefined)` too).
fn dialog_arg(scope: &mut v8::PinScope, a: &v8::FunctionCallbackArguments, i: i32) -> String {
    if a.length() <= i || a.get(i).is_undefined() {
        return String::new();
    }
    a.get(i).to_rust_string_lossy(scope)
}

/// `confirm(message)` -- false, the answer for a dismissed dialog, when no one answers it. Step 7 (ABI 3 wave 1): a
/// loaded plugin may answer the dialog instead (`plugin_host::on_dialog`); `Some((accept, _))` decides it. D88 step 2:
/// else a person may, through the endpoint (`dialog::ask`), and the page waits for them.
fn cb_confirm(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let message = dialog_arg(scope, &a, 0);
    if plugin_host::any() {
        if let Some((accept, _)) = plugin_host::on_dialog("confirm", &message) {
            rv.set_bool(accept);
            return;
        }
    }
    let accept = crate::core::html::dialog::ask("confirm", &message, "").is_some_and(|(accept, _)| accept);
    rv.set_bool(accept);
}
/// `prompt(message, default)` -- null, likewise. Step 7: a plugin may answer (`on_dialog`): `Some((true, text))` returns
/// the reply, `Some((false, _))` a dismissal (null). D88 step 2: else a person may: their text, `""` when they accepted
/// none (Chrome 150), null when they dismissed it.
fn cb_prompt(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let message = dialog_arg(scope, &a, 0);
    let answer = match plugin_host::any().then(|| plugin_host::on_dialog("prompt", &message)).flatten() {
        Some(a) => Some(a),
        None => {
            let default = dialog_arg(scope, &a, 1);
            crate::core::html::dialog::ask("prompt", &message, &default)
        }
    };
    match answer {
        Some((true, text)) => {
            let v = str_val(scope, &text);
            rv.set(v);
        }
        _ => rv.set_null(),
    }
}
/// `alert(message)` -- undefined. Step 7: a plugin is still told the dialog was raised (`on_dialog`), though alert's
/// answer is always undefined whatever the plugin decides. D88 step 2: unanswered by a plugin, a person is shown it,
/// and the page waits until they close it.
fn cb_alert(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let message = dialog_arg(scope, &a, 0);
    if plugin_host::any() && plugin_host::on_dialog("alert", &message).is_some() {
        return;
    }
    let _ = crate::core::html::dialog::ask("alert", &message, "");
}

/// A `[Replaceable]` readonly attribute: readonly in the IDL, and still given a setter whose entire
/// effect is to replace the accessor with an own data property. `window.event = 1` is a legal
/// assignment in every browser and a strict-mode TypeError without this. `data` is the member name.
fn cb_replaceable_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &a).to_rust_string_lossy(scope);
    let target = a.this();
    let Some(k) = v8::String::new(scope, &name) else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(a.get(0), true);
    d.set_enumerable(true);
    d.set_configurable(true);
    target.define_property(scope, k.into(), &d);
}

// ── postMessage ──────────────────────────────────────────────────────────────────────────────────

/// `postMessage(message, targetOrigin, transfer)` / `postMessage(message, options)`.
///
/// Overload resolution is Chrome's: a second argument that is a non-null OBJECT is the options
/// dictionary, anything else is the `USVString targetOrigin`. That is what makes `postMessage(1)`
/// return undefined (options overload, default `{}`) while `postMessage(1, "x")` throws -- "x" is a
/// targetOrigin, and a targetOrigin that is neither "*" nor "/" must parse as a URL.
///
/// Delivery is real: the message is structured-cloned and a `message` MessageEvent is dispatched at
/// this window on a later task, exactly as a same-window post does in a browser. A targetOrigin that
/// does not match this window's origin drops the message silently, which is the point of the
/// argument.
///
/// This is the deleted JS's body, line for line, with the same three ordering facts: the overload
/// test, the DataCloneError at CALL time (not on the delivery task), and the sender realm read
/// ([`crate::core::html::browsing_context::current_realm_window`], which the JS reached through an
/// `op_current_realm` wrapper, deleted for want of a caller) that lets a popup's
/// `opener.postMessage` set `event.source` to the popup.
fn cb_post_message<'s>(scope: &mut v8::PinScope<'s, '_>, a: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    let mut target_origin = "/".to_string();
    let mut transfer: Option<v8::Local<v8::Value>> = None;
    let mut from_options = false;
    if a.length() >= 2 {
        let second = a.get(1);
        // WebIDL's overload resolution: with two arguments, `undefined`, `null` and an object are
        // the `WindowPostMessageOptions` overload -- converted as WebIDL converts it: `transfer`,
        // `includeUserActivation`, `delegate`, `targetOrigin`, each read once (korvus read `targetOrigin` then
        // `transfer`, and took `null`/`undefined` for a target origin named "null"/"undefined")
        if a.length() == 2 && (second.is_null_or_undefined() || second.is_object()) {
            let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, second, "WindowPostMessageOptions", "Failed to execute 'postMessage' on 'Window': ") else { return };
            if let Some(t) = get_prop(scope, o, "targetOrigin").filter(|v| v.is_string()) {
                target_origin = t.to_rust_string_lossy(scope);
            }
            transfer = get_prop(scope, o, "transfer").filter(|v| !v.is_undefined());
            from_options = true;
        } else {
            target_origin = second.to_rust_string_lossy(scope);
            if a.length() >= 3 && !a.get(2).is_undefined() {
                transfer = Some(a.get(2));
            }
        }
    }
    // The transfer list is read and validated FIRST, whole -- before the target origin is even
    // parsed, before the message is serialized and before anything is neutered. That is Chrome's
    // order and it is observable: a bad list beats an unclonable message, and a throw leaves every
    // port named in it usable (`core/messaging/transfer.rs`).
    let Some(list) = crate::core::messaging::transfer::read_window_transfer(scope, transfer, from_options) else { return };
    if target_origin != "*" && target_origin != "/" {
        match url::Url::parse(&target_origin) {
            Ok(u) => target_origin = u.origin().ascii_serialization(),
            Err(_) => {
                let msg = format!(
                    "Failed to execute 'postMessage' on 'Window': Invalid target origin '{target_origin}' in a call to 'postMessage'."
                );
                crate::binding::protos::throw_dom(scope, &msg, "SyntaxError");
                return;
            }
        }
    }
    let self_origin = origin_of(&href(scope)).unwrap_or_else(|| "null".to_string());
    // "/" means "only if the source origin matches the target's" -- a window posting to itself always
    // matches. A concrete origin has to equal ours or the message is dropped.
    if target_origin != "*" && target_origin != "/" && target_origin != self_origin {
        return;
    }
    // StructuredSerializeWithTransfer happens at CALL time, so a DataCloneError for an unclonable
    // message is thrown to the caller here rather than swallowed on the delivery task.
    // A transferred port stays itself in the clone and becomes the port the receiver gets.
    let originals = crate::core::messaging::transfer::port_originals(&list);
    let Some(data) = crate::modules::structured_clone::clone_for_transfer(scope, a.get(0), "Window", "postMessage", &originals) else { return };
    // The transfer happens HERE, after the clone: each transferred port is neutered and replaced by
    // a fresh one for the receiver, and each buffer is detached. The old body passed the CALLER's
    // array straight through as `ports`, so the receiver got the sender's own live ports.
    let (ports, pairs) = crate::core::messaging::transfer::take_ports_mapped(scope, &list, None);
    let data = crate::modules::structured_clone::substitute(scope, data, &pairs);
    let ports: v8::Local<v8::Value> = ports.into();
    crate::core::messaging::transfer::detach_buffers(scope, &list);
    // The `origin`/`source` on the delivered event describe the SENDER, and the sender is not always
    // this window: a popup's script posting back through `opener` runs in the popup's realm, and the
    // opener has to be told so -- `event.source === thePopupHandle` is the check every cross-document
    // messaging test makes. The current realm is null whenever the page itself is the caller, which is
    // the overwhelming majority, and then this is exactly what it was.
    let (sender, sender_origin) = match crate::core::html::browsing_context::current_realm_window(scope) {
        Some(r) => {
            let o = crate::core::html::browsing_context::realm_origin(scope, r.into());
            let seen = crate::core::html::browsing_context::sender_seen_by_page(scope);
            (seen, o)
        }
        None => (global.into(), self_origin.clone()),
    };
    // `KORVUS_TRACE_PM`: who posted to the page, as whom (a diagnosis aid).
    if std::env::var_os("KORVUS_TRACE_PM").is_some() {
        crate::core::html::browsing_context::trace_post(scope, "page", &sender_origin, sender);
    }
    let init = crate::binding::protos::init_dict(scope);
    crate::binding::protos::set_prop(scope, init, "data", data);
    // an opaque origin serializes as "null" (HTML "postMessage": the incumbent origin's serialization) -- Chrome 150, a
    // sandboxed srcdoc frame's message reads `origin` "null"; korvus answered "" (a bot-detection script's risk frame)
    let ov: v8::Local<v8::Value> = str_val(scope, &sender_origin);
    crate::binding::protos::set_prop(scope, init, "origin", ov);
    crate::binding::protos::set_prop(scope, init, "ports", ports);
    let ty: v8::Local<v8::Value> = str_val(scope, "message");
    let Some(ev) = crate::binding::protos::new_global(scope, "MessageEvent", &[ty, init.into()]) else { return };
    // `source` into the slot directly -- a cross-origin frame's view is a Proxy the constructor refuses
    // -- and the UA's message event is trusted.
    crate::core::events::event::evt_slot_write(scope, ev, "source", sender);
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    // A later task, as a browser delivers it: the engine's own task (never the page's `setTimeout`).
    let evg = v8::Global::new(scope, ev);
    let data_holder: v8::Local<v8::Value> = v8::Local::new(scope, &evg).into();
    let f = build_fn_data(scope, cb_deliver_message, "", 0, data_holder);
    let ms: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    let gv: v8::Local<v8::Value> = global.into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[f.into(), ms]);
}

fn cb_deliver_message(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let __body_data = crate::binding::protos::body_data(scope, &a);
    crate::binding::protos::engine_call(scope, gv, "dispatchEvent", &[__body_data]);
}

// ── the kernel's timer service, as this table's rows ─────────────────────────────────────────────

/// Ask `korvus-runtime`'s `op_window_scope_fn` factory for one of the five timer members, as a genuine
/// native function carrying THIS row's own `name` and `length`.
///
/// The same cross-crate shape `core/workers/worker_global_scope.rs::kernel_row` has used since W6.2,
/// and for the same reason: the queue, the deadlines, the insertion order, HTML's nesting clamp and
/// the callback handles are the KERNEL's task source (invariant I9), and the shape -- which names
/// exist, with which arity and flags, on which holder -- is this table's.
///
/// `stamp_arity_guarded` because the factory's bodies do their own WebIDL argument checks, with the
/// holder's name in the message; a generic guard cannot produce `queueMicrotask`'s
/// "parameter 1 is not of type 'Function'." at all.
///
/// `None` when this realm registered no factory, at which point the caller mounts the D2 stub -- the
/// one not-implemented mechanism, counted (invariant I7).
fn kernel_timer<'s>(scope: &mut v8::PinScope<'s, '_>, member: &MemberDef) -> Option<v8::Local<'s, v8::Function>> {
    let factory = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_window_scope_fn")?;
    let name = str_val(scope, member.name);
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, member.length as i32).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    crate::binding::protos::stamp_arity_guarded(scope, f);
    Some(f)
}

// ── the row installer ────────────────────────────────────────────────────────────────────────────

/// The body every non-constant row of [`WINDOW`] names. `target` is the GLOBAL OBJECT, not a
/// prototype: `Window` is `[Global]`, so its members are own properties of `globalThis`.
pub fn install_window(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => {
            let g0 = build_fn(scope, cb_name_get, "get name", 0);
            let g = guarded(scope, g0.into(), "get name", 0);
            let s0 = build_fn(scope, cb_name_set, "set name", 1);
            let s = guarded(scope, s0.into(), "set name", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "status" => {
            let g0 = build_fn(scope, cb_status_get, "get status", 0);
            let g = guarded(scope, g0.into(), "get status", 0);
            let s0 = build_fn(scope, cb_status_set, "set status", 1);
            let s = guarded(scope, s0.into(), "set status", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "opener" => {
            let g0 = build_fn(scope, cb_opener_get, "get opener", 0);
            let g = guarded(scope, g0.into(), "get opener", 0);
            let s0 = build_fn(scope, cb_opener_set, "set opener", 1);
            let s = guarded(scope, s0.into(), "set opener", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "closed" | "credentialless" => {
            let g0 = build_fn(scope, cb_false, &format!("get {}", member.name), 0);
            let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
            define_accessor(scope, target, member, g, None);
        }
        "frameElement" => {
            let g0 = build_fn(scope, cb_null, "get frameElement", 0);
            let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
            define_accessor(scope, target, member, g, None);
        }
        "originAgentCluster" => {
            let g0 = build_fn(scope, cb_origin_agent_cluster, "get originAgentCluster", 0);
            let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
            define_accessor(scope, target, member, g, None);
        }
        "isSecureContext" => {
            let g0 = build_fn(scope, cb_is_secure_context, "get isSecureContext", 0);
            let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
            define_accessor(scope, target, member, g, None);
        }
        // `origin` is `[Replaceable]` (measured on all four browsers: `set origin`/1, and
        // `origin = 7` replaces it with a data property).
        "origin" => {
            let g0 = build_fn(scope, cb_origin, "get origin", 0);
            let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
            let d = str_val(scope, member.name);
            let s0 = build_fn_data(scope, cb_replaceable_set, "set origin", 1, d);
            let s = guarded(scope, s0.into(), "set origin", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "crossOriginIsolated" => {
            let g0 = build_fn(scope, crate::core::html::isolation_policy::cb_cross_origin_isolated, "get crossOriginIsolated", 0);
            let g = guarded(scope, g0.into(), "get crossOriginIsolated", 0);
            define_accessor(scope, target, member, g, None);
        }
        // `getComputedStyle` and the frame/idle quartet: `guarded` like every other method
        // of this table (Chrome throws `Illegal invocation` from all five for a foreign receiver,
        // measured), and behind the row's own arity guard, which is where the "1 argument required"
        // message comes from. The JS enforced arity by hand and checked no receiver at all.
        // The ORDER of the two wrappers is Chrome's and is observable: the receiver check is
        // OUTERMOST, so `getComputedStyle.call({})` answers `Illegal invocation` rather than
        // "1 argument required" -- which is what the first build of this arm got backwards, because
        // the arity guard was supplied later by the WINQ re-apply pass and therefore landed outside.
        // `is_void` is the row's return type: `undefined` for the two cancels, a value for the rest.
        //
        // WHICH FOUR the quartet is comes from `animation_frame::owns`, not from a second list here:
        // the file that holds the bodies is the one place that says which rows it owns,
        // exactly as `window_geometry::owns` does two arms below. Spelling the names again in this
        // arm made the pair drift-prone in the one direction nothing would catch -- a fifth row added
        // to `animation_frame.rs` and not to this arm falls through to the generic arms below and is
        // installed as something else.
        n if n == "getComputedStyle" || crate::core::html::animation_frame::owns(n) => {
            let f = if member.name == "getComputedStyle" {
                win_fn(scope, crate::core::css::cssom::cb_get_computed_style, member.name, member.length as i32)
            } else {
                crate::core::html::animation_frame::body(scope, member.name)
            };
            let a = make_arity_guard(scope, "Window", member.name, member.required as i32, member.length as i32, f.into(), member.ty);
            let v = guarded(scope, a.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // THE THIRTEEN GEOMETRY ROWS AND THE THREE SCROLL METHODS --
        // `core/html/window_geometry.rs`. Before the `length`/`event` arm below, because these are
        // `[Replaceable]` too and share its setter: one extended attribute, one implementation.
        n if crate::core::html::window_geometry::owns(n) => {
            if let Some(g0) = crate::core::html::window_geometry::getter(scope, member.name) {
                let d = str_val(scope, member.name);
                let s0 = build_fn_data(scope, cb_replaceable_set, &format!("set {}", member.name), 1, d);
                let g = guarded(scope, g0.into(), &format!("get {}", member.name), 0);
                let st = guarded(scope, s0.into(), &format!("set {}", member.name), 1);
                define_accessor(scope, target, member, g, Some(st));
            } else {
                // `scrollTo` / `scroll` / `scrollBy`. NOT `guarded`, and that is measured:
                // `window.scrollTo.call({})` does not throw in Chrome, where every accessor of this
                // group and every other method of this table does.
                let f = crate::core::html::window_geometry::scroll_method(scope, member.name);
                define_value(scope, target, member, f.into(), true);
            }
        }
        // The two `[Replaceable]` readonly attributes.
        "length" | "event" => {
            let g = if member.name == "length" {
                build_fn(scope, cb_length, "get length", 0)
            } else {
                build_fn(scope, crate::core::events::event_target::cb_window_event, "get event", 0)
            };
            let d = str_val(scope, member.name);
            let s0 = build_fn_data(scope, cb_replaceable_set, &format!("set {}", member.name), 1, d);
            let g = guarded(scope, g.into(), &format!("get {}", member.name), 0);
            let s = guarded(scope, s0.into(), &format!("set {}", member.name), 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "open" => {
            let f = win_fn(scope, crate::core::html::browsing_context::cb_window_open, member.name, member.length as i32);
            let g = make_arity_guard(scope, "Window", member.name, member.required as i32, member.length as i32, f.into(), member.ty);
            let v = guarded(scope, g.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        "reportError" => {
            let f = win_fn(scope, crate::core::events::report::cb_report_exception, member.name, member.length as i32);
            let g = make_arity_guard(scope, "Window", member.name, member.required as i32, member.length as i32, f.into(), member.ty);
            let v = guarded(scope, g.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        "postMessage" => {
            let f = win_fn(scope, cb_post_message, member.name, member.length as i32);
            let a = make_arity_guard(scope, "Window", member.name, member.required as i32, member.length as i32, f.into(), member.ty);
            let v = guarded(scope, a.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // The WebIDL required-argument counts of `open`, `moveBy`/`moveTo`/`resizeBy`/`resizeTo`,
        // `postMessage` and `reportError` are carried on the ROWS, which is the whole of it:
        // `generated_order::WINQ` listed the same counts in a second generated table and a
        // finalize pass re-applied them from the outside, and both are deleted.
        // The row replaces `globalThis.getSelection = () => theSelection` in
        // `core/korvus-runtime/src/js/host.js` -- a JS arrow function assigned onto the global, i.e.
        // a Web API member the KERNEL defined (I9) with a body that was not native (I3) and an
        // installer that was not the installer (I2). The body is
        // `core/editing/selection.rs`'s, which owns the one selection object.
        "getSelection" => {
            let f = win_fn(scope, crate::core::editing::selection::cb_window_get_selection, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // The body throws the ARITY and Symbol-conversion TypeErrors itself, with Chrome's
        // wording. `guarded` like every other method here: the row was left unguarded
        // because `matchMedia.call(null, q)` answers a `MediaQueryList` in Chrome -- which it still
        // does, V8 hands a native function called with null the global -- but `matchMedia.call({},
        // "all")` and `matchMedia.call({})` are `Illegal invocation` in Chrome 150.0.7871.46
        // (measured over loopback, in a page and inside a frame), and both answered here.
        "matchMedia" => {
            let f = win_fn(scope, crate::core::css::media_query_list::cb_match_media, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        "confirm" => {
            let f = win_fn(scope, cb_confirm, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        "prompt" => {
            let f = win_fn(scope, cb_prompt, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // The engine's output sink -- see `cb_print`. `BOOTSTRAP` used to assign over this row.
        "print" => {
            let f = win_fn(scope, cb_print, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // THE FIVE TIMER MEMBERS, the last five Web API members the KERNEL defined.
        // They were JS arrows in `engine.rs`'s `BOOTSTRAP` string behind `op_nativize`, which
        // reported `[native code]` for a body that was JavaScript -- not a table row (I1), a JS body
        // (I3), defined by the kernel (I9), all three at once. The bodies are
        // `korvus-runtime`'s `scope_timers.rs` over the ONE timer task source
        // (`scheduler.rs`), asked for through the `op_window_scope_fn` factory with THIS holder's
        // name; the worker's `WorkerGlobalScope` rows call the same five with `'WorkerGlobalScope'`.
        //
        // NOT `make_arity_guard`, and that is the one place this arm differs from every other method
        // above: the body throws its own WebIDL TypeErrors, because `queueMicrotask`'s non-callable
        // message ("parameter 1 is not of type 'Function'.") is not an arity message and has to carry
        // the same holder name. The factory stamps what it hands back as already-guarded, so `guarded`
        // carries the brand outward and no later pass wraps a second count around it.
        //
        // `guarded` IS here: `setTimeout.call({}, f, 0)` is `Illegal invocation` in Chrome (measured,
        // 150.0.7871.46) for all five, exactly as it is for every other method of this table, while
        // `setTimeout.call(null, f, 0)` is legal -- an undefined receiver is the global proxy.
        "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" | "queueMicrotask" => {
            let Some(f) = kernel_timer(scope, member) else {
                crate::install::installer::put_table_stub(scope, target, member, true);
                return;
            };
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        // The 125 `on<type>` handler rows. They do NOT name the shared `event_handler` kit, which
        // is the only reason this arm exists: the kit mounts an unguarded pair, and Chrome throws
        // `Illegal invocation` from a Window handler getter given a foreign receiver like every other
        // Window member. The bodies are the kit's own (`make_et_on_getter`/`make_et_on_setter` over
        // the ONE event-handler store), so the handler behaves identically -- unset reads null, a set
        // value reads back, and the setter registers the event handler listener in the same list as
        // addEventListener's, so order stays registration order.
        n if n.starts_with("on") => {
            let ty = n.strip_prefix("on").unwrap_or(n);
            let g0 = crate::core::events::event_target::make_et_on_getter(scope, ty);
            let s0 = crate::core::events::event_target::make_et_on_setter(scope, ty);
            let g = guarded(scope, g0.into(), &format!("get {n}"), 0);
            let s = guarded(scope, s0.into(), &format!("set {n}"), 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        // Step 7 (ABI 3 wave 1): `alert` consults the loaded plugins' `on_dialog` (`cb_alert`) rather than being a
        // bare void method; its own answer is still undefined. A dedicated arm so the dialog bodies (`alert`,
        // `confirm`, `prompt`) are the three that reach `on_dialog` and nothing else does.
        "alert" => {
            let f = win_fn(scope, cb_alert, member.name, member.length as i32);
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        _ => {
            // The void methods.
            let f = if member.name == "stop" {
                win_fn(scope, cb_stop, member.name, member.length as i32)
            } else if member.name == "close" {
                win_fn(scope, cb_window_close, member.name, member.length as i32)
            } else {
                win_fn(scope, cb_void, member.name, member.length as i32)
            };
            let inner: v8::Local<v8::Value> = if member.required > 0 {
                make_arity_guard(scope, "Window", member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
            } else {
                f.into()
            };
            let v = guarded(scope, inner, member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
    }
}
// The rows, in the ORDER `dom/09f-window.js` installed them -- which is the order these keys have
// on `globalThis` today, so the cutover moves no key relative to another. The two `constant` rows
// are VERBATIM from the `Window` block of `install/generated_registry.rs` (a WebIDL constant is
// carried twice, once for the prototype and once for the interface object); every other row is a
// `[Global]` member, whose NAMES the bundle records in `surface.json`'s flat `globals` list and
// whose WebIDL required-argument counts are the `required` column of the rows below (moved
// them off the second table, `generated_order::WINQ`, that used to carry them). There are no
// descriptors to copy for those -- the capture's globals entry carries none -- so the flags are
// WebIDL's rule for a member of a [Global] interface: enumerable and configurable, methods
// writable. That is the claim the deleted file's own comment made, and it is unchanged here.
webapi::interface! {
    "Window" : "EventTarget" [interface, window] {
        ctor     "Window"                           len 0 = stub,
        // The three writable attributes.
        attr     "name"                : DomString        = install_window,
        attr     "status"              : DomString        = install_window,
        attr     "opener"              : Any              = install_window,
        // The readonly attributes. `length` and `event` are `[Replaceable]`: readonly in the IDL,
        // and still given a setter whose whole effect is to replace the accessor with an own data
        // property, so `window.event = 1` is the legal assignment it is in every browser.
        attr     "closed"              : Boolean readonly = install_window,
        attr     "length"              : Integer          = install_window,
        attr     "frameElement"        : Object  readonly nullable = install_window,
        attr     "credentialless"      : Boolean readonly = install_window,
        attr     "originAgentCluster"  : Boolean readonly = install_window,
        attr     "isSecureContext"     : Boolean readonly = install_window,
        attr     "origin"              : UsvString        = install_window,
        attr     "crossOriginIsolated" : Boolean readonly = install_window,
        attr     "event"               : Any              = install_window,
        // THE THIRTEEN GEOMETRY ATTRIBUTES, replacing the last big
        // `Object.defineProperty(globalThis, …)` block of `core/korvus-runtime/src/js/host.js`.
        // Every one is `[Replaceable]` like `length` and `event` above -- readonly in the IDL and
        // still given a setter that deletes the accessor and leaves an own data property, so
        // `window.innerWidth = 5` sticks and `devicePixelRatio` stops being an accessor. Chrome
        // agrees on all thirteen (measured); the JS had the shape right and had no receiver check.
        //
        // The order is the JS's own assignment order, which is the order the global received them.
        attr     "innerWidth"          : Double           = install_window,
        attr     "innerHeight"         : Double           = install_window,
        attr     "outerWidth"          : Double           = install_window,
        attr     "outerHeight"         : Double           = install_window,
        attr     "devicePixelRatio"    : Double           = install_window,
        attr     "screenX"             : Double           = install_window,
        attr     "screenLeft"          : Double           = install_window,
        attr     "screenY"             : Double           = install_window,
        attr     "screenTop"           : Double           = install_window,
        attr     "scrollX"             : Double           = install_window,
        attr     "pageXOffset"         : Double           = install_window,
        attr     "scrollY"             : Double           = install_window,
        attr     "pageYOffset"         : Double           = install_window,
        // The methods.
        method   "alert"              : Undefined len 0  = install_window,
        method   "print"              : Undefined len 0  = install_window,
        method   "focus"              : Undefined len 0  = install_window,
        method   "blur"               : Undefined len 0  = install_window,
        method   "stop"               : Undefined len 0  = install_window,
        method   "close"              : Undefined len 0  = install_window,
        method   "confirm"            : Boolean   len 0  = install_window,
        method   "prompt"             : DomString len 0  = install_window,
        method   "open"               : Object    len 0 required 1 = install_window,
        method   "moveBy"             : Undefined len 2 required 2 = install_window,
        method   "moveTo"             : Undefined len 2 required 2 = install_window,
        method   "resizeBy"           : Undefined len 2 required 2 = install_window,
        method   "resizeTo"           : Undefined len 2 required 2 = install_window,
        method   "captureEvents"      : Undefined len 0  = install_window,
        method   "releaseEvents"      : Undefined len 0  = install_window,
        method   "reportError"        : Undefined len 1 required 1 = install_window,
        method   "postMessage"        : Undefined len 1 required 1 = install_window,
        // `getSelection` was the last Web API member `host.js` put on `globalThis` by
        // assignment. The capture lists it among Window's globals as a native method of length 0,
        // which is what this row installs; it goes LAST among the methods because that is where the
        // assignment put it -- after every key `dom/09f-window.js` had already written -- so the
        // cutover moves no key relative to another.
        method   "getSelection"       : Object    len 0  = install_window,
        // `matchMedia` was the last Web API member `host.js` put on `globalThis` by
        // `Object.defineProperty`. The capture lists it among Window's globals; the row installs it
        // with WebIDL's `required 1`, which the JS enforced by hand and Chrome enforces with the same
        // message. AFTER `getSelection` for the reason that row is last among the methods: that is
        // where the assignment put it.
        method   "matchMedia"         : Object    len 1 required 1 = install_window,
        // THE THREE SCROLL METHODS. `scrollTo(optional ScrollToOptions)` /
        // `scrollTo(double x, double y)` -> WebIDL length 0: every argument of every overload is
        // optional in the dictionary form, so a bare `scrollTo()` is legal. `scroll` is `scrollTo`'s
        // alias. AFTER `matchMedia` for the same reason that row is last: that is where the
        // assignment put them, so the cutover moves no key relative to another.
        // `: Promise` matches what the body now answers and what the capture records; it is NOT the
        // fix and must not be read as one. Every member mounted on the GLOBAL is unguarded on
        // purpose -- `guard_installed` answers `none:global` because `window` is an INSTANCE and
        // `Window.prototype` is not in its chain when the member loop runs (`receiver.rs`) -- and
        // these rows are `body = install`, so the BODY decides the return value. Changing this
        // column alone changes nothing observable.
        method   "scrollTo"           : Promise   len 0  = install_window,
        method   "scroll"             : Promise   len 0  = install_window,
        method   "scrollBy"           : Promise   len 0  = install_window,
        // `getComputedStyle(Element elt, optional CSSOMString? pseudoElt)`: `elt` is
        // required, so a bare call is a WebIDL arity TypeError rather than a declaration over
        // `undefined`. The body is `core/css/cssom.rs`'s, which owns the cascade query.
        method   "getComputedStyle"   : Object    len 1 required 1 = install_window,
        // ...and the frame/idle quartet (`core/html/animation_frame.rs`). Both `request*` take a
        // required callback and both `cancel*` a required handle, which is the WebIDL and what
        // Chrome enforces with these messages.
        method   "requestAnimationFrame" : Integer   len 1 required 1 = install_window,
        method   "cancelAnimationFrame"  : Undefined len 1 required 1 = install_window,
        method   "requestIdleCallback"   : Integer   len 1 required 1 = install_window,
        method   "cancelIdleCallback"    : Undefined len 1 required 1 = install_window,
        // THE FIVE TIMER MEMBERS, the last Web API members `korvus-runtime` defined.
        //
        // WebIDL's `WindowOrWorkerGlobalScope`, and Chrome agrees on every column (measured with
        // `google-chrome --headless=new` 150.0.7871.46): all five are own, writable, enumerable and
        // configurable properties of `window` and own properties of NOTHING else -- `Window.prototype`
        // carries none of them -- with `setTimeout`/`setInterval`/`queueMicrotask` at `length` 1 and
        // `clearTimeout`/`clearInterval` at `length` 0, because `clearTimeout(optional long handle = 0)`
        // takes an OPTIONAL handle and a bare `clearTimeout()` is therefore legal and answers undefined.
        //
        // LAST AMONG THE METHODS, where `BOOTSTRAP` put them: that script ran AFTER this whole table
        // and assigned the five onto the global, so this is the position closest to the order the keys
        // had before the cutover. It is not identical -- nothing in a table row can land after
        // `install_window_chrome` -- and own-global ORDER is a surface property no window-realm gate
        // measures (W10.3 is the unit that builds that instrument), so it was MEASURED either side of
        // the cutover rather than left as folklore: `Object.getOwnPropertyNames(globalThis).length` is
        // 1161 before and after, the four timer keys move from 1094-1097 (right after `chrome`) to
        // 968-971 (right after `cancelIdleCallback`, this row's own position), and NO key is added or
        // removed.
        //
        // `queueMicrotask` does not move at all: deno_core installs one at index 66, and redefining an
        // existing key does not reposition it. What DID change there is the descriptor -- deno_core's
        // was `enumerable: false`, because `BOOTSTRAP` reached the slot by ASSIGNMENT
        // (`globalThis[name] = ...`), which keeps an existing property's attributes. Chrome's is
        // enumerable (measured), so a `for (const k in window)` that skipped `queueMicrotask` was an
        // own-descriptor tell, and the row's flags close it.
        method   "setTimeout"         : Integer   len 1 required 1 = install_window,
        method   "clearTimeout"       : Undefined len 0  = install_window,
        method   "setInterval"        : Integer   len 1 required 1 = install_window,
        method   "clearInterval"      : Undefined len 0  = install_window,
        method   "queueMicrotask"     : Undefined len 1 required 1 = install_window,
        // The 125 `on<type>` event handler IDL attributes, in the baseline's own order. Each is
        // the native accessor pair over the ONE event-handler store (core/events/event_target.rs):
        // unset reads null, a set value reads back, and the setter registers the event handler
        // listener itself -- in the same list as addEventListener's, so order is registration order.
        handler  "onabort"                    = install_window,
        handler  "onafterprint"               = install_window,
        handler  "onanimationcancel"          = install_window,
        handler  "onanimationend"             = install_window,
        handler  "onanimationiteration"       = install_window,
        handler  "onanimationstart"           = install_window,
        handler  "onappinstalled"             = install_window,
        handler  "onauxclick"                 = install_window,
        handler  "onbeforeinput"              = install_window,
        handler  "onbeforeinstallprompt"      = install_window,
        handler  "onbeforematch"              = install_window,
        handler  "onbeforeprint"              = install_window,
        handler  "onbeforetoggle"             = install_window,
        handler  "onbeforeunload"             = install_window,
        handler  "onbeforexrselect"           = install_window,
        handler  "onblur"                     = install_window,
        handler  "oncancel"                   = install_window,
        handler  "oncanplay"                  = install_window,
        handler  "oncanplaythrough"           = install_window,
        handler  "onchange"                   = install_window,
        handler  "onclick"                    = install_window,
        handler  "onclose"                    = install_window,
        handler  "oncommand"                  = install_window,
        handler  "oncontentvisibilityautostatechange" = install_window,
        handler  "oncontextlost"              = install_window,
        handler  "oncontextmenu"              = install_window,
        handler  "oncontextrestored"          = install_window,
        handler  "oncuechange"                = install_window,
        handler  "ondblclick"                 = install_window,
        handler  "ondevicemotion"             secure = install_window,
        handler  "ondeviceorientation"        secure = install_window,
        handler  "ondeviceorientationabsolute" secure = install_window,
        handler  "ondrag"                     = install_window,
        handler  "ondragend"                  = install_window,
        handler  "ondragenter"                = install_window,
        handler  "ondragleave"                = install_window,
        handler  "ondragover"                 = install_window,
        handler  "ondragstart"                = install_window,
        handler  "ondrop"                     = install_window,
        handler  "ondurationchange"           = install_window,
        handler  "onemptied"                  = install_window,
        handler  "onended"                    = install_window,
        handler  "onerror"                    = install_window,
        handler  "onfocus"                    = install_window,
        handler  "onformdata"                 = install_window,
        handler  "ongamepadconnected"         = install_window,
        handler  "ongamepaddisconnected"      = install_window,
        handler  "ongotpointercapture"        = install_window,
        handler  "onhashchange"               = install_window,
        handler  "oninput"                    = install_window,
        handler  "oninvalid"                  = install_window,
        handler  "onkeydown"                  = install_window,
        handler  "onkeypress"                 = install_window,
        handler  "onkeyup"                    = install_window,
        handler  "onlanguagechange"           = install_window,
        handler  "onload"                     = install_window,
        handler  "onloadeddata"               = install_window,
        handler  "onloadedmetadata"           = install_window,
        handler  "onloadstart"                = install_window,
        handler  "onlostpointercapture"       = install_window,
        handler  "onmessage"                  = install_window,
        handler  "onmessageerror"             = install_window,
        handler  "onmousedown"                = install_window,
        handler  "onmouseenter"       lenient = install_window,
        handler  "onmouseleave"       lenient = install_window,
        handler  "onmousemove"                = install_window,
        handler  "onmouseout"                 = install_window,
        handler  "onmouseover"                = install_window,
        handler  "onmouseup"                  = install_window,
        handler  "onmousewheel"               = install_window,
        handler  "onoffline"                  = install_window,
        handler  "ononline"                   = install_window,
        handler  "onpagehide"                 = install_window,
        handler  "onpagereveal"               = install_window,
        handler  "onpageshow"                 = install_window,
        handler  "onpageswap"                 = install_window,
        handler  "onpause"                    = install_window,
        handler  "onplay"                     = install_window,
        handler  "onplaying"                  = install_window,
        handler  "onpointercancel"            = install_window,
        handler  "onpointerdown"              = install_window,
        handler  "onpointerenter"             = install_window,
        handler  "onpointerleave"             = install_window,
        handler  "onpointermove"              = install_window,
        handler  "onpointerout"               = install_window,
        handler  "onpointerover"              = install_window,
        handler  "onpointerrawupdate"         secure = install_window,
        handler  "onpointerup"                = install_window,
        handler  "onpopstate"                 = install_window,
        handler  "onprogress"                 = install_window,
        handler  "onratechange"               = install_window,
        handler  "onrejectionhandled"         = install_window,
        handler  "onreset"                    = install_window,
        handler  "onresize"                   = install_window,
        handler  "onscroll"                   = install_window,
        handler  "onscrollend"                = install_window,
        handler  "onscrollsnapchange"         = install_window,
        handler  "onscrollsnapchanging"       = install_window,
        handler  "onsearch"                   = install_window,
        handler  "onsecuritypolicyviolation"  = install_window,
        handler  "onseeked"                   = install_window,
        handler  "onseeking"                  = install_window,
        handler  "onselect"                   = install_window,
        handler  "onselectionchange"          = install_window,
        handler  "onselectstart"              = install_window,
        handler  "onslotchange"               = install_window,
        handler  "onstalled"                  = install_window,
        handler  "onstorage"                  = install_window,
        handler  "onsubmit"                   = install_window,
        handler  "onsuspend"                  = install_window,
        handler  "ontimeupdate"               = install_window,
        handler  "ontoggle"                   = install_window,
        handler  "ontransitioncancel"         = install_window,
        handler  "ontransitionend"            = install_window,
        handler  "ontransitionrun"            = install_window,
        handler  "ontransitionstart"          = install_window,
        handler  "onunhandledrejection"       = install_window,
        handler  "onunload"                   = install_window,
        handler  "onvolumechange"             = install_window,
        handler  "onwaiting"                  = install_window,
        handler  "onwebkitanimationend"       = install_window,
        handler  "onwebkitanimationiteration" = install_window,
        handler  "onwebkitanimationstart"     = install_window,
        handler  "onwebkittransitionend"      = install_window,
        handler  "onwheel"                    = install_window,
        // `Window.prototype` owns exactly these two, and the interface object carries them too.
        constant        "TEMPORARY"  : Integer          = 0,
        constant        "PERSISTENT" : Integer          = 1,
        static_constant "TEMPORARY"  : Integer          = 0,
        static_constant "PERSISTENT" : Integer          = 1
    }
}

// ── the install ──────────────────────────────────────────────────────────────────────────────────

/// Build `Window` from its table and mount its members where WebIDL puts them.
///
/// THE SPLIT this function exists for: the two `constant` rows go on `Window.prototype` and on the
/// `Window` interface object (`create_interface`'s job, driven by a selector of just those two names),
/// and EVERY other row is mounted on `globalThis` -- which no generic installer can do, because
/// `[Global]` is the one WebIDL extended attribute that moves a member off the prototype entirely.
///
/// It also does what `generated_shells.js`'s Window special case did during the ESM layer and can no
/// longer do (the interface object does not exist until this runs): point `globalThis` at
/// `Window.prototype`, so `window.addEventListener` is INHERITED from `EventTarget.prototype` rather
/// than an own property -- an own copy is a global tell real pages do not have -- and drop the three
/// own copies `dom.js` leaves behind, but only once the re-parent has genuinely taken and the chain
/// actually reaches them.
pub fn install_window_window<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::install::installer;
    use crate::install::live::parent_of;

    let mut ledger = installer::take_ledger(scope);
    let parent = parent_of(scope, "EventTarget");
    const CONSTS: &[&str] = &["TEMPORARY", "PERSISTENT"];
    // `create_global_scope`, the same call the worker realm's scope install makes (invariant I8): the
    // interface a realm's GLOBAL OBJECT is gets its name whatever the capture says. It changes nothing
    // here — `Window` is in every bundle's `surface.json` — and that is the point of routing it through
    // the same door: the rule is stated where it is true rather than holding by luck in one realm. See
    // `installer::create_global_scope` for the worker realm, where the capture genuinely cannot answer.
    // Every non-constant row, in table order, onto the GLOBAL OBJECT — THROUGH THE ONE GATE.
    //
    // `Window` is `[Global]`, so each of these rows is an OWN PROPERTY of `globalThis` and not a
    // prototype member: the same kind of thing the registry's interface objects and the hand
    // singletons are, and it asks the same two questions of the same gate. The names
    // are in the bundle's flat `globals` list, which answers the first; `m.exposure` answers the
    // second, and four of these rows carry a condition — `ondevicemotion`, `ondeviceorientation`,
    // `ondeviceorientationabsolute` and `onpointerrawupdate` are gone from
    // `Object.getOwnPropertyNames(window)` on an insecure origin on Chrome 150, Brave 151, Edge 151
    // and Opera 135 alike, while the ~90 handler rows beside them are not.
    //
    // THE CREATE MOUNTS THEM (the Window global), right after it puts `Window` on the
    // global -- the order this function's own loop had -- so they are recorded with the interface and
    // a realm that instantiates `Window`'s template takes them from it. The gate's answer names the
    // template, so an insecure document never reuses a secure one's.
    let rows: Vec<&'static crate::install::MemberDef> = WINDOW
        .members
        .iter()
        .filter(|m| !matches!(m.kind, crate::install::MemberKind::Const))
        .filter(|m| crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Global, m.name, m.exposure))
        .collect();
    let built = installer::create_global_scope_with_members(scope, global, &WINDOW, parent, Some(CONSTS), &rows, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] window install gap: {}.{}", g.interface, g.member);
    }
    // THE TWO STATIC CONSTANT ROWS WERE MOUNTED HERE, BY HAND (the loop is deleted). The note
    // that stood here was right about the cause -- a WebIDL constant is two rows with ONE name, and a
    // name selector matches the prototype's and stops -- and wrong about where to fix it: it made this
    // function a second writer onto a slot `create_interface` also reaches, which the install ledger
    // caught the moment the bundle selector started placing statics from the capture's own `statics`
    // list. `installer::create_interface` now places both halves of a two-row name, so the only writer
    // is the one create.
    installer::put_ledger(scope, ledger);

    // `globalThis` IS a Window instance in Chrome: `Object.getPrototypeOf(globalThis) ===
    // Window.prototype`, so `addEventListener`/`removeEventListener`/`dispatchEvent` are inherited up
    // the chain from `EventTarget.prototype`, never own properties.
    let protov: v8::Local<v8::Value> = built.proto.into();
    if global.get_prototype(scope).map(|p| p != protov).unwrap_or(true) {
        global.set_prototype(scope, built.proto.into());
    }
    // Drop the own copies ONLY once the global genuinely inherits them (the re-parent took AND the
    // chain reaches `EventTarget.prototype.addEventListener`), so `window.addEventListener` can never
    // become undefined if the re-parent were rejected.
    let inherits = global.get_prototype(scope).map(|p| p == protov).unwrap_or(false)
        && get_prop(scope, built.proto, "addEventListener").map(|v| v.is_function()).unwrap_or(false);
    if inherits {
        for n in ["addEventListener", "removeEventListener", "dispatchEvent"] {
            if let Some(k) = v8::String::new(scope, n) {
                if global.has_own_property(scope, k.into()).unwrap_or(false) {
                    global.delete(scope, k.into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::potentially_trustworthy;

    /// Chromium's `IsUrlPotentiallyTrustworthy` (services/network/public/cpp/is_potentially_trustworthy.cc) over
    /// `GURL::IsAboutBlank` / `IsAboutSrcdoc`: a query or fragment on about:blank keeps it trustworthy, another path,
    /// a host or a second slash does not.
    #[test]
    fn about_blank_and_srcdoc_are_trustworthy_whatever_their_query_or_fragment() {
        for u in ["about:blank", "about:blank#x", "about:blank?q=1", "about:blank/", "about:srcdoc", "about:srcdoc#f"] {
            assert!(potentially_trustworthy(u), "{u}");
        }
        for u in ["about:blankx", "about:blank//", "about:config", "about://host/blank", "http://example.com/", "", "blank"] {
            assert!(!potentially_trustworthy(u), "{u}");
        }
        for u in ["https://example.com/", "wss://example.com/", "file:///tmp/x", "data:text/html,x", "http://localhost/", "http://127.0.0.1:8080/", "http://a.localhost/"] {
            assert!(potentially_trustworthy(u), "{u}");
        }
    }
}

//! `core/html/window_extras.rs` — THE WINDOW'S REMAINING OWN GLOBALS.
//!
//! # What this file is, and why it is one file
//!
//! `Object.getOwnPropertyNames(window)` read 1,163 on this engine and 1,206 on the Brave 151 the
//! shipped bundle claims: **43 names missing, 0 extra**, and the same 43 in a nested browsing
//! context's window. They are not one API — they are the residue of every API whose INTERFACE this
//! engine already publishes and whose INSTANCE nobody ever minted. That is the thing they have in
//! common and it is why they are installed together.
//!
//! MEASURED, not assumed. Every descriptor, every prototype chain, every function `name`/`length`
//! and every answer below was read off real Brave 151.1.93.138 and real Chrome 150.0.7871.46 (headed,
//! over CDP, a page on 127.0.0.1, 2026-09-19 — headless Brave hangs on this box, FP5.1). Where the
//! reading contradicted the brief, the reading won and the contradiction is named in the comment.
//!
//! # The descriptor families, which is the whole taxonomy
//!
//! ```text
//! value  w=1 e=0 c=1   an interface object under a second name (webkitURL …) and the five GPU
//!                      constant namespaces -- WebIDL's own shape for an interface/namespace object
//! value  w=1 e=1 c=1   an operation on Window (find, fetchLater, queryLocalFonts, the two webkit
//!                      rAF aliases, the three file pickers, the two webkit FileSystem calls)
//! get/set e=1 c=1      a [Replaceable] attribute (external, clientInformation)
//! get     e=1 c=1      a readonly attribute (styleMedia, launchQueue, sharedStorage, fence)
//! ```
//!
//! and exactly one name sits outside it: `offscreenBuffering` is a get/set pair with **e=0**, which is
//! why `window_globals::define_window_custom_accessor` takes `enumerable` as a parameter.
//!
//! # Where the order comes from
//!
//! Nowhere in this file. The registry walk's last step is
//! ([`crate::install::installer::seat_globals_in_capture_order`]) seat every own global at the worn
//! capture's position, so a name created here lands where the device has it whatever order it was
//! created in, and `global_order.rs` / `worker_surface.rs` hold that at 0 out of place. What this file
//! must NOT do is create a name the capture does not list — hence [`publishes`] on every single one.
//!
//! # A stub that returns an empty value only MOVES the tell
//!
//! So each name here is one of two things, and both are recorded:
//!
//! 1. **Real, because the device's answer is reproducible here.** `external.AddSearchProvider()` is
//!    `undefined` on Brave and so is the D2 stub's answer for an `any`-typed row, so `external` needs
//!    an instance and no member code at all. `queryLocalFonts()` rejects `SecurityError: User
//!    activation is required.` — an engine that never has user activation gives the same answer for
//!    the same reason. `launchQueue.setConsumer(cb)` answers `undefined` and never fires the consumer,
//!    because this page was not launched as an installed app; that is the device's behaviour, not an
//!    omission.
//! 2. **Real, from this realm's own state.** `styleMedia.matchMedium(q)` runs the realm's own
//!    `matchMedia`, `find(s)` searches the document's own text, `clientInformation` IS `navigator`.
//!
//! # THE THIRTEEN NAMES THIS FILE DELIBERATELY DOES NOT INSTALL ITSELF, and exactly why
//!
//! ```text
//! locationbar menubar personalbar scrollbars statusbar toolbar   BarProp.visible must be `true`
//! viewport         segments must be one frozen DOMRect, not null        -- core/html/bar_props.rs
//! visualViewport   width/height must be clientWidth/clientHeight, not 0 -- core/html/visual_viewport.rs
//! scheduler        postTask must RUN the task and resolve with its value -- core/html/scheduler.rs
//! trustedTypes     createPolicy must return a working policy, not `undefined` -- core/trusted_types/
//! documentPictureInPicture   .window must be `null` and requestWindow() must REJECT
//!                                                                      -- core/html/document_pip.rs
//! crashReport      set()/delete() must throw InvalidStateError until initialize() runs
//!                                                                      -- core/html/crash_report.rs
//! navigation       currentEntry must be a real NavigationHistoryEntry   -- STILL OPEN (FP5.8)
//! ```
//!
//! Each name that has since been closed names the table that closed it above, and its install is
//! CALLED from the bottom of this file so the whole set still lands in one place, before the seat.
//! Five of them are closed; `navigation` is the one left, and it is five interfaces and 47
//! members with real session-history state behind them, which is why it is its own lane.
//!
//! Every one of those needs a BODY on a member of an interface the generated registry already
//! installs, and `tests/install_ledger.rs` refuses a second writer of a slot the installer mounted
//! (the invariant "a member is installed exactly once, by exactly one row"). Clobbering those
//! prototypes after the fact was tried and the ledger caught all 30 slots; that is the ratchet
//! working. The supported route is the one `core/feature_policy/feature_policy.rs` took: each of those
//! nine interfaces gets its own `interface!` table with real bodies and leaves
//! `install/generated_registry.rs` through `tools/sync`. That is FP5's named residual and it is
//! sequenced separately, because it is nine tables and ~47 rows — including the four `Handler` rows
//! `VisualViewport` and `Navigation` carry — not a shape fix.
//!
//! **Installing them WITHOUT their bodies would be worse than leaving them out**, and one of them
//! proves it: a page that finds `window.trustedTypes` and calls
//! `trustedTypes.createPolicy(n, o).createHTML(s)` throws on a `createPolicy` that answers the stub's
//! `undefined`, where the same page on an engine with no `trustedTypes` at all takes its fallback path
//! and works.

use deno_core::v8;

use crate::binding::protos::{
    brand_shell_instance, build_fn, build_fn_data, get_prop, global_get, make_dom_exc,
    rejected_promise, require_args, str_val,
};
use crate::core::html::exposure::Exposure;
use crate::core::html::window_globals::{
    define_window_custom_accessor, define_window_singleton, mint_shell, window_slot_get, Mint, WindowSetter,
};

// ── the gate every name passes through ──────────────────────────────────────────────────────────

/// Does this realm get `name`, and under what condition? TWO questions, and the gate takes the second
/// one as an argument so a name cannot be written here without its exposure condition beside it
/// (the whole of that unit is that a name must arrive with its conditions, not just
/// with its body).
///
/// * The ACTIVE capture's own-globals list answers the first, and it is a PERSONA fact: the engine
///   reads the bundle and never asks which bundle it is wearing. `sharedStorage` exists under
///   `chrome-150`, `edge-151` and `opera-135` and not under `brave-151`, for that reason and no other.
/// * [`Exposure`] answers the second, and it is a PLATFORM fact off the interface's own IDL —
///   the same on every browser in the family, which is why it is not a bundle field.
///
/// Every `Exposure` below was MEASURED on both browsers at two origins; the table and the two
/// readings that contradict an IDL guess are in [`crate::core::html::exposure`].
fn publishes(scope: &mut v8::PinScope, name: &str, exposure: Exposure) -> bool {
    crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Global, name, exposure)
}

// ── minting one instance of an interface the registry already published ─────────────────────────

/// An instance of `iface`: an object on the interface's own (sealed) prototype, carrying the brand
/// every registry instance carries, so the interface's members accept it as a receiver instead of
/// answering `TypeError: Illegal invocation` (`protos::shell_receiver_ok`).
///
/// `None` when the realm has no such interface — the honest answer for a bundle whose capture does not
/// record it, and why every caller below is already behind [`publishes`].
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    mint_shell(scope, iface)
}

/// A hidden prototype for an object whose INTERFACE the device does not expose on the global.
/// `styleMedia`'s `StyleMedia` is the only one: measured on both browsers, its prototype's own string
/// keys are exactly `["type", "matchMedium"]` — **no own `constructor`**, so `styleMedia.constructor`
/// is `Object`'s, inherited — its `[[Prototype]]` is `Object.prototype`, and it carries
/// `Symbol.toStringTag`, which is what makes `Object.prototype.toString.call(styleMedia)` read
/// `[object StyleMedia]`.
///
/// `crashReport` looked like a second case of this and is NOT: `CrashReportContext` IS in every
/// capture's own-globals list and the registry publishes it, so a hidden prototype of that name would
/// have put a SECOND `CrashReportContext` in the realm. The capture is what caught it.
fn hidden_proto<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Object> {
    let proto = v8::Object::new(scope);
    let tag = v8::Symbol::get_to_string_tag(scope);
    let v = str_val(scope, name);
    let mut d = v8::PropertyDescriptor::new_from_value_writable(v, false);
    d.set_enumerable(false);
    d.set_configurable(true);
    proto.define_property(scope, tag.into(), &d);
    proto
}

/// An instance on a hidden prototype, branded like any other.
fn hidden_instance<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    o
}

/// The `data` a hidden-prototype member carries: `{p: thatPrototype}`, so its receiver check reads the
/// prototype it was defined on rather than a global the page can overwrite.
fn recv_data<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Value> {
    let d = v8::Object::new(scope);
    if let Some(k) = v8::String::new(scope, "p") {
        d.set(scope, k.into(), proto.into());
    }
    d.into()
}

/// Is `this` an instance whose chain carries the prototype this member was defined on? The same check
/// the installer applies to every registry member, repeated here because these members are not the
/// installer's — they are on a prototype no registry row owns.
fn recv_ok(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let data = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, args)).unwrap_or_else(|_| v8::Object::new(scope));
    let Some(proto) = get_prop(scope, data, "p") else { return false };
    crate::binding::protos::shell_receiver_ok(scope, args.this().into(), proto)
}

/// Define a member on a hidden prototype: a plain `{w:1, e:1, c:1}` method, which is what
/// `styleMedia.matchMedium` carries on both browsers.
fn hidden_method<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<'s, v8::Object>,
    name: &str,
    length: i32,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let data = recv_data(scope, proto);
    let f = build_fn_data(scope, cb, name, length, data);
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
    pd.set_enumerable(true);
    pd.set_configurable(true);
    proto.define_property(scope, k.into(), &pd);
}

/// The same for a readonly attribute on a hidden prototype (`styleMedia.type`).
fn hidden_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<'s, v8::Object>,
    name: &str,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let data = recv_data(scope, proto);
    let get = build_fn_data(scope, cb, &format!("get {name}"), 0, data);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut pd = v8::PropertyDescriptor::new_from_get_set(get.into(), undef);
    pd.set_enumerable(true);
    pd.set_configurable(true);
    proto.define_property(scope, k.into(), &pd);
}

// ── the own-data-property helper the alias and operation families share ──────────────────────────

/// `globalThis[name] = value` as an own DATA property with the exact flags measured on the device.
fn define_data(
    scope: &mut v8::PinScope,
    global: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Value>,
    enumerable: bool,
) {
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(value, true);
    pd.set_enumerable(enumerable);
    // Configurable so the registry walk's seat can move it to the capture's position (P12f).
    pd.set_configurable(true);
    global.define_property(scope, k.into(), &pd);
}

// ── 1. the legacy `webkit` ALIASES: the SAME interface object under a second name ────────────────

/// `(alias, the interface object it IS)`. Measured on Brave 151 and Chrome 150: every one is `===` its
/// target — `webkitURL === URL` is `true`, so `webkitURL.name` is `"URL"` and `String(webkitURL)` is
/// `"function URL() { [native code] }"`. An alias that were a COPY would print its own name and fail
/// `===`, which is the detect this row shape avoids.
///
/// `webkitSpeechRecognitionError` is the odd one and the brief did not say so: it aliases
/// `SpeechRecognitionErrorEvent`. There is no `SpeechRecognitionError` interface in the family.
const WEBKIT_ALIASES: [(&str, &str); 9] = [
    ("webkitURL", "URL"),
    ("webkitRTCPeerConnection", "RTCPeerConnection"),
    ("webkitMediaStream", "MediaStream"),
    ("webkitSpeechGrammar", "SpeechGrammar"),
    ("webkitSpeechGrammarList", "SpeechGrammarList"),
    ("webkitSpeechRecognition", "SpeechRecognition"),
    ("webkitSpeechRecognitionError", "SpeechRecognitionErrorEvent"),
    ("webkitSpeechRecognitionEvent", "SpeechRecognitionEvent"),
    // Not a webkit name, the same `[LegacyWindowAlias]` mechanism: Chrome 152 renamed FeaturePolicy.
    ("FeaturePolicy", "PermissionsPolicy"),
];

fn install_webkit_aliases<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (alias, target) in WEBKIT_ALIASES {
        if !publishes(scope, alias, Exposure::Unconditional) {
            continue;
        }
        let Some(v) = global_get(scope, target) else { continue };
        // e=0: an interface object's own descriptor on the global, which the alias inherits.
        define_data(scope, global, alias, v, false);
    }
}

/// The two animation-frame names are NOT the same function object —
/// `webkitRequestAnimationFrame === requestAnimationFrame` is **false** on Brave 151, each carries its
/// own `name` and `length` 1, and calling it schedules a real frame. So they are wrappers that call
/// through to this realm's own rAF, and their descriptor is `{w:1, e:1, c:1}` — an operation on
/// `Window`, not an interface object.
fn cb_raf_alias(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let target = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(f) = global_get(scope, &target).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    let recv: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    if let Some(r) = f.call(scope, recv, &argv) {
        rv.set(r);
    }
}

const RAF_ALIASES: [(&str, &str); 2] = [
    ("webkitRequestAnimationFrame", "requestAnimationFrame"),
    ("webkitCancelAnimationFrame", "cancelAnimationFrame"),
];

fn install_raf_aliases<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (alias, target) in RAF_ALIASES {
        if !publishes(scope, alias, Exposure::Unconditional) {
            continue;
        }
        let data = str_val(scope, target);
        let f = build_fn_data(scope, cb_raf_alias, alias, 1, data);
        define_data(scope, global, alias, f.into(), true);
    }
}

// ── 2. `clientInformation` and `offscreenBuffering` ──────────────────────────────────────────────

/// `clientInformation` IS `navigator` — `clientInformation === navigator` is `true` on both browsers,
/// so it reads the same slot rather than minting a second `Navigator`. A second one would answer every
/// value the same and fail the identity test, which is the cheaper detect.
fn install_client_information<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    if !publishes(scope, "clientInformation", Exposure::Unconditional) {
        return;
    }
    let Some(nav) = window_slot_get(scope, "navigator") else { return };
    define_window_singleton(scope, global, "clientInformation", nav, WindowSetter::Replaceable, true, Mint::Alias("navigator"));
}

/// `offscreenBuffering` is `true`, and it is the ONE name in this file whose accessor is
/// **non-enumerable** (`{get, set, e:0, c:1}`, measured on both browsers). A legacy WebKit flag with no
/// behaviour behind it: `true` is not a stub, it is the answer.
fn cb_offscreen_buffering(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = v8::Boolean::new(scope, true);
    rv.set(t.into());
}

// ── 3. `external` ───────────────────────────────────────────────────────────────────────────────
//
// THE MOST-READ NAME IN THE SET: a behavioural sensor touches `window.external` 21 times in one page load
// (FP0 §L4). Its whole surface is two methods and BOTH answer `undefined` on Brave 151 --
// `external.AddSearchProvider()`, `external.IsSearchProviderInstalled()` and their one-argument forms
// alike, each with `length` 0. The brief expected `IsSearchProviderInstalled` to answer `0`; the
// browser answers `undefined`, MY READING WINS, and that is exactly what the D2 stub already gives an
// `any`-typed row. So `external` needs no member code at all -- only the instance.

// ── 4. `styleMedia` ─────────────────────────────────────────────────────────────────────────────

fn cb_style_media_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv_ok(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let v = str_val(scope, "screen");
    rv.set(v);
}

/// `styleMedia.matchMedium(q)` is `matchMedia(q).matches` (measured: `"screen"` → `true`, `"print"` →
/// `false`), `length` 0. THE REAL MEDIA QUERY, through this realm's own `matchMedia`, so a query the
/// engine evaluates one way through `window.matchMedia` cannot answer the other way through here.
fn cb_style_media_match(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv_ok(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    // the realm's own `matchMedia` and `matches` (sealed), never the page's
    let global = scope.get_current_context().global(scope);
    let q = args.get(0);
    let matches = crate::binding::protos::pristine_call(scope, global.into(), "self.matchMedia", &[q])
        .filter(|m| m.is_object())
        .and_then(|m| crate::binding::protos::pristine_get(scope, m, "MediaQueryList.matches"))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false);
    let b = v8::Boolean::new(scope, matches);
    rv.set(b.into());
}

fn install_style_media<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    if !publishes(scope, "styleMedia", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_style_media(scope) else { return };
    // Readonly: `Object.getOwnPropertyDescriptor(window, "styleMedia")` has no setter on either
    // browser, unlike most of this family.
    define_window_singleton(scope, global, "styleMedia", o, WindowSetter::None, true, Mint::With(mint_style_media));
}

/// This realm's `styleMedia`, on its own hidden `StyleMedia` prototype.
fn mint_style_media<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let proto = hidden_proto(scope, "StyleMedia");
    hidden_getter(scope, proto, "type", cb_style_media_type);
    hidden_method(scope, proto, "matchMedium", 0, cb_style_media_match);
    Some(hidden_instance(scope, proto).into())
}

// ── 5. the GATED operations: the device's own refusal, for the device's own reason ───────────────
//
// These REJECT on the real browser with no user activation, and this engine never has user activation
// -- so the rejection is not a stub, it is the same answer arrived at the same way. The sentences are
// verbatim (Brave 151 / Chrome 150, headed, no gesture):
//
// ```text
// getScreenDetails()      NotAllowedError  Transient activation is required to request permission.
// queryLocalFonts()       SecurityError    User activation is required.
// showOpenFilePicker()    SecurityError    Failed to execute 'showOpenFilePicker' on 'Window':
//                                          Must be handling a user gesture to show a file picker.
// ```

/// `(global name, arity, DOMException name, message, exposure, the operation, refused a task later)`. The message is the
/// whole of what a page can read, so it is stored rather than rebuilt. The arguments are converted first, as Blink's
/// binding converts them (korvus read none of the pickers' or `queryLocalFonts`' options); the
/// file pickers refuse at once, while the font and screen permissions are asked of the browser and refused a task
/// later (Chrome 150, measured: pending three microtasks on, rejected within a frame).
///
/// ALL FIVE ARE `[SecureContext]`, measured on both browsers at two origins (FP5.7) — and the column
/// is per-row rather than one constant in the loop because that is the rule this table is now obeying:
/// a name is written down with its condition, so the next row added here has to state one.
///
/// Note the two facts are independent and both are needed: Brave publishes NEITHER of the three file
/// pickers on either origin (a bundle fact, handled by the capture's own-globals list), while Chrome
/// publishes all three on a secure origin and none on an insecure one (this column).
type Gated = (&'static str, i32, &'static str, &'static str, Exposure, &'static str, bool);
const GATED_CALLS: [Gated; 5] = [
    ("getScreenDetails", 0, "NotAllowedError", "Transient activation is required to request permission.", Exposure::SecureContext, "Window::getScreenDetails", true),
    ("queryLocalFonts", 0, "SecurityError", "User activation is required.", Exposure::SecureContext, "Window::queryLocalFonts", true),
    (
        "showDirectoryPicker",
        0,
        "SecurityError",
        "Failed to execute 'showDirectoryPicker' on 'Window': Must be handling a user gesture to show a file picker.",
        Exposure::SecureContext,
        "Window::showDirectoryPicker",
        false,
    ),
    (
        "showOpenFilePicker",
        0,
        "SecurityError",
        "Failed to execute 'showOpenFilePicker' on 'Window': Must be handling a user gesture to show a file picker.",
        Exposure::SecureContext,
        "Window::showOpenFilePicker",
        false,
    ),
    (
        "showSaveFilePicker",
        0,
        "SecurityError",
        "Failed to execute 'showSaveFilePicker' on 'Window': Must be handling a user gesture to show a file picker.",
        Exposure::SecureContext,
        "Window::showSaveFilePicker",
        false,
    ),
];

fn cb_gated_reject(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // the row's index in [`GATED_CALLS`]: a number, which a template can carry
    let i = crate::binding::protos::body_data(scope, &args).uint32_value(scope).unwrap_or(0) as usize;
    let Some((global, _, name, msg, _, op, later)) = GATED_CALLS.get(i) else { return };
    let prefix = format!("Failed to execute '{global}' on 'Window': ");
    if let Err(e) = crate::install::ctor_dicts::convert_method_args_caught(scope, op, &args, &prefix) {
        rv.set(rejected_promise(scope, e));
        return;
    }
    let Some(e) = make_dom_exc(scope, msg, name) else { return };
    let p = if *later {
        crate::binding::protos::settle_later(scope, e, true, crate::binding::protos::Settle::Task(0.0))
    } else {
        rejected_promise(scope, e)
    };
    rv.set(p);
}

fn install_gated_calls<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (i, (name, length, _, _, exposure, _, _)) in GATED_CALLS.iter().enumerate() {
        if !publishes(scope, name, *exposure) {
            continue;
        }
        let data: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, i as u32).into();
        let f = build_fn_data(scope, cb_gated_reject, name, *length, data);
        define_data(scope, global, name, f.into(), true);
    }
}

/// `window.fence` is an ACCESSOR that answers **null** outside a fenced frame — measured on Chrome 150,
/// where the name exists and the value does not. Nothing is minted: `null` IS the answer.
fn cb_fence(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = v8::null(scope);
    rv.set(n.into());
}

// ── 6. `find` ───────────────────────────────────────────────────────────────────────────────────
//
// `window.find(s)` is a REAL text search on the device (`find("probe")` true, `find("zzz…")` false on
// the same page) and it is a real one here: the document's rendered text, case-insensitively, which is
// `find`'s default (`aCaseSensitive` defaults to false). `length` is 0 -- measured, and not the 1 an
// IDL reading would guess.
//
// WHAT IS NOT HERE: `find` also MOVES the selection on the device. This one does not, and FP5's report
// says so.

/// The document element and its rendered text, by the realm's own members -- never the page's
/// `documentElement` / `innerText` (a `find()` called both).
fn document_element<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let doc = window_slot_get(scope, "document")?;
    crate::binding::protos::pristine_get(scope, doc, "Document.documentElement").filter(|v| v.is_object())
}

fn cb_window_find(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let needle = if args.length() == 0 { String::new() } else { args.get(0).to_rust_string_lossy(scope) };
    let found = if needle.is_empty() {
        false
    } else {
        document_element(scope)
            .and_then(|de| {
                // `innerText` is an HTMLElement's; an SVG document's root has only `textContent`
                let html = crate::binding::protos::unwrap_id(scope, de).is_some_and(|id| {
                    let os = deno_core::JsRuntime::op_state_from(scope);
                    let st = os.borrow();
                    st.try_borrow::<crate::binding::host_dom::HostDom>().is_some_and(|d| d.0.is_html_ns(id))
                });
                if html {
                    crate::binding::protos::pristine_get(scope, de, "HTMLElement.innerText")
                } else {
                    crate::binding::protos::pristine_get(scope, de, "Node.textContent")
                }
            })
            .map(|v| v.to_rust_string_lossy(scope).to_lowercase().contains(&needle.to_lowercase()))
            .unwrap_or(false)
    };
    let b = v8::Boolean::new(scope, found);
    rv.set(b.into());
}

// ── 7. `fetchLater` and the two legacy FileSystem entry points ───────────────────────────────────
//
// `fetchLater(input)` has `length` 1 and throws WebIDL's own
// `TypeError: Failed to execute 'fetchLater' on 'Window': 1 argument required, but only 0 present.`
// with no arguments. With one it answers a `FetchLaterResult` whose `activated` is `false` -- which is
// the D2 stub's own answer for that Boolean row, so the object needs no member code.
//
// NOT HERE, and in FP5's report: the deferred request is never SENT. A deferred fetch is observable
// only by the server that receives it, and sending one from an engine with no unload pipeline would be
// a request the page did not make at a moment the page cannot see.

fn cb_fetch_later(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Window", "fetchLater") {
        return;
    }
    let Some(r) = mint(scope, "FetchLaterResult") else { return };
    rv.set(r.into());
}

/// `webkitResolveLocalFileSystemURL(url, cb, errcb)` calls the ERROR callback with a `SecurityError`
/// on the device for a URL that did not come from this origin's own filesystem — measured on Brave 151.
/// This engine hands out no filesystem, so every URL is such a URL and the answer is the same one for
/// the same reason.
fn cb_resolve_local_fs_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Window", "webkitResolveLocalFileSystemURL") {
        return;
    }
    let Ok(errcb) = v8::Local::<v8::Function>::try_from(args.get(2)) else { return };
    let Some(e) = make_dom_exc(scope, "A security error occurred", "SecurityError") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    errcb.call(scope, undef, &[e]);
}

/// `webkitRequestFileSystem(type, size, cb, errcb)`: the device SUCCEEDS and hands back a
/// `DOMFileSystem` — an interface no bundle in this tree publishes on the global and which this engine
/// has no table for, so there is nothing here to hand back and neither callback is called. The name,
/// `length` 3 and descriptor are the device's; the callback is named in FP5's report.
///
/// Its presence is not cosmetic: `worker_scope_live.rs`'s `NOT_YET` list held
/// `global webkitRequestFileSystem` because Chrome's WINDOW has it at `length` 3 while its worker has
/// it at 2, and this engine had only the worker's — so the realms agreed where Chrome's differ. That
/// list is empty now.
fn cb_request_file_system(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    require_args(scope, &args, 1, "Window", "webkitRequestFileSystem");
}

// ── THE INSTALL ─────────────────────────────────────────────────────────────────────────────────

/// Every own global of this realm that the registry walk and the domain installs do not create, for the
/// bundle this realm wears. Called once per window realm, after the registry install (so every
/// interface these instances stand on exists) and before
/// [`crate::install::installer::seat_globals_in_capture_order`], which puts each name at the capture's
/// position.
pub(crate) fn install_window_extras<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    install_webkit_aliases(scope, global);
    install_raf_aliases(scope, global);
    install_client_information(scope, global);

    if publishes(scope, "offscreenBuffering", Exposure::Unconditional) {
        let get = build_fn(scope, cb_offscreen_buffering, "get offscreenBuffering", 0);
        define_window_custom_accessor(scope, global, "offscreenBuffering", get, WindowSetter::Replaceable, false);
    }

    if publishes(scope, "external", Exposure::Unconditional) {
        if let Some(o) = mint(scope, "External") {
            define_window_singleton(scope, global, "external", o.into(), WindowSetter::Replaceable, true, Mint::Shell("External"));
        }
    }

    install_style_media(scope, global);

    if publishes(scope, "launchQueue", Exposure::Unconditional) {
        // NO MEMBER CODE, and that is the measurement rather than a shortcut: `setConsumer(cb)` answers
        // `undefined` on the device and never fires the consumer on a page that was not launched as an
        // installed app -- which is every page this engine loads. The registry's row is
        // `Undefined`-typed with one required argument, so the D2 stub answers `undefined` and the
        // arity guard gives WebIDL's own error. Both are the device's.
        if let Some(o) = mint(scope, "LaunchQueue") {
            define_window_singleton(scope, global, "launchQueue", o.into(), WindowSetter::None, true, Mint::Shell("LaunchQueue"));
        }
    }

    if publishes(scope, "sharedStorage", Exposure::SecureContext) {
        // Chrome/Edge/Opera only. The object is minted and its members stay the registry's D2 stubs:
        // every one either writes to a shared-storage database this engine does not have or runs a
        // worklet it cannot run, and the device's own answer to `sharedStorage.set(...)` on a page with
        // the feature disabled is already an `OperationError` rejection (measured on Chrome 150).
        // Named in FP5's report.
        if let Some(o) = mint(scope, "SharedStorage") {
            define_window_singleton(scope, global, "sharedStorage", o.into(), WindowSetter::None, true, Mint::Shell("SharedStorage"));
        }
    }

    if publishes(scope, "fence", Exposure::Unconditional) {
        let get = build_fn(scope, cb_fence, "get fence", 0);
        define_window_custom_accessor(scope, global, "fence", get, WindowSetter::None, true);
    }

    install_gated_calls(scope, global);

    if publishes(scope, "find", Exposure::Unconditional) {
        let f = build_fn(scope, cb_window_find, "find", 0);
        define_data(scope, global, "find", f.into(), true);
    }
    if publishes(scope, "fetchLater", Exposure::SecureContext) {
        let f = build_fn(scope, cb_fetch_later, "fetchLater", 1);
        define_data(scope, global, "fetchLater", f.into(), true);
    }
    if publishes(scope, "webkitRequestFileSystem", Exposure::Unconditional) {
        let f = build_fn(scope, cb_request_file_system, "webkitRequestFileSystem", 3);
        define_data(scope, global, "webkitRequestFileSystem", f.into(), true);
    }
    if publishes(scope, "webkitResolveLocalFileSystemURL", Exposure::Unconditional) {
        let f = build_fn(scope, cb_resolve_local_fs_url, "webkitResolveLocalFileSystemURL", 2);
        define_data(scope, global, "webkitResolveLocalFileSystemURL", f.into(), true);
    }

    super::gpu_namespaces::install_window_gpu_namespaces(scope, global);
    // The six `BarProp`s and `viewport`: their own `interface!` tables, because their one member each
    // needs a BODY and `install_ledger.rs` refuses a second writer of a slot the installer mounted.
    super::bar_props::install_window_bar_props(scope, global);
    // `visualViewport`, off a `VisualViewport` table of its own, for the same reason.
    // AFTER the registry install, because the table splices onto `EventTarget`.
    super::visual_viewport::install_window_visual_viewport(scope, global);
    // `crashReport` and `documentPictureInPicture`, likewise.
    super::crash_report::install_window_crash_report(scope, global);
    super::document_pip::install_window_document_pip(scope, global);
    // `trustedTypes` -- five tables, because a policy has to ROUND-TRIP.
    crate::core::trusted_types::tables::install_window_trusted_types(scope, global);
    // `scheduler` -- a real priority queue on this realm's own macrotask hook.
    super::scheduler::install_window_scheduler(scope, global);
    // `navigation`, the Navigation API's history-entry view.
    super::navigation_api::install_window_navigation(scope, global);
}

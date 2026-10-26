//! `core/navigator/devices.rs` -- the Navigator members that hand back a DEVICE or CAPABILITY
//! object.
//!
//! WHAT THIS REPLACES: the `stub(...)` and `hidden(...)` halves of `core/korvus-runtime/src/js/
//! nav-glue.js` -- 22 shape-only device singletons (`usb`, `serial`, `hid`, `xr`, `storage`, ...),
//! the two `DeprecatedStorageQuota` entry points and `navigator.brave`. Every one of them was a JS
//! body wrapped in `op_nativize` so it would read `[native code]`; here they are native by
//! construction (invariant I3) and the table row is the only place the member is named (I1).
//!
//! THREE SHAPES, all of them the capture's:
//!
//!  1. A SHAPE-ONLY SINGLETON is `Object.create(Iface.prototype)`, BRANDED so a shell member's
//!     receiver check accepts it -- `navigator.usb` really is the `USB` of this realm, and
//!     `Object.create(USB.prototype)` alone is not an instance in any browser. The interface's own
//!     members are still the generated registry's D2 stubs (W7 owns that ledger); what this file
//!     supplies is the INSTANCE, one per navigator, the same object on every read.
//!  2. TWO of those prototypes carry a body the JS also supplied, because the answer is knowable and
//!     `undefined` is a tell. Headless Chrome 150.0.7871.46 on this machine, asked over a loopback
//!     server on 2026-09-11, answers `navigator.scheduling.isInputPending()` FALSE and
//!     `navigator.userActivation.hasBeenActive` / `.isActive` FALSE in a page nobody has touched --
//!     the same three answers the JS wrote.
//!
//!     Ruling: those three rows are written HERE, over the generated registry's stub rows for
//!     `Scheduling` / `UserActivation`, which is exactly what `nav-glue.js` did from the same
//!     position in the order -- it patched the prototype on the first read of `navigator.scheduling`.
//!     Giving the two interfaces their own tables is the I1-clean answer and it is W7's, not this
//!     unit's: doing it here would pull two more interfaces out of the generated registry for three
//!     members. Cost if wrong: two interfaces keep a second writer for one more track -- the same
//!     standing `core/html/plugins.rs` recorded for its two `navigator` accessors.
//!  3. A HIDDEN-CONSTRUCTOR object is one Chrome reaches through `navigator` WITHOUT exposing its
//!     interface object on the global: `navigator.brave` and the two quota entry points. Its
//!     constructor is a real `[native code]` function that rejects `new`, its prototype carries
//!     `constructor` + `Symbol.toStringTag` + the methods, and the member hands back an instance.

use deno_core::v8;

use crate::binding::protos::{brand_shell_instance, build_fn, str_val};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::live::parent_of;

use super::navigator::{cb_false, cb_resolved_true, own_prop};

/// Navigator member name -> the interface whose prototype its singleton is minted on. VERBATIM from
/// `nav-glue.js`'s `stub("…")` calls, in the table's order.
const DEVICE_INTERFACE: &[(&str, &str)] = &[
    ("scheduling", "Scheduling"),
    ("userActivation", "UserActivation"),
    ("geolocation", "Geolocation"),
    ("windowControlsOverlay", "WindowControlsOverlay"),
    ("clipboard", "Clipboard"),
    ("credentials", "CredentialsContainer"),
    ("managed", "NavigatorManagedData"),
    ("virtualKeyboard", "VirtualKeyboard"),
    ("wakeLock", "WakeLock"),
    ("locks", "LockManager"),
    ("storage", "StorageManager"),
    ("gpu", "GPU"),
    ("ink", "Ink"),
    ("mediaCapabilities", "MediaCapabilities"),
    ("devicePosture", "DevicePosture"),
    ("hid", "HID"),
    ("mediaSession", "MediaSession"),
    ("presentation", "Presentation"),
    ("serial", "Serial"),
    ("usb", "USB"),
    // Windows/Mac Chrome; desktop Linux has no `navigator.bluetooth`. Mounted only for a capture that
    // lists it (`install::installer::join`), its `Bluetooth` members read from the bundle like the rest.
    ("bluetooth", "Bluetooth"),
    ("xr", "XRSystem"),
    ("storageBuckets", "StorageBucketManager"),
    // Chrome 150's two of the rows Brave lacks: stock Chrome hands back a
    // `ProtectedAudience` and a `NavigatorLogin`, not `undefined` (measured 2026-10-08).
    ("protectedAudience", "ProtectedAudience"),
    ("login", "NavigatorLogin"),
];

/// `protoMethod(proto, name, len, fn)` from `nav-glue.js`: a `[native code]` method as an
/// `{writable:true, enumerable:true, configurable:true}` data property.
fn proto_method(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    name: &str,
    len: i32,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let f = build_fn(scope, cb, name, len);
    own_prop(scope, proto, name, f.into());
}

/// The two prototypes whose stub members the JS patched with a knowable answer (see ruling 2 in the
/// module header). Reached once per realm, from the first read of each singleton.
fn patch_prototype(scope: &mut v8::PinScope, iface: &str, proto: v8::Local<v8::Object>) {
    match iface {
        "Scheduling" => proto_method(scope, proto, "isInputPending", 0, cb_false),
        // `estimate()` resolved `undefined` (the stub's default); the persona's source
        // browser answered `{quota, usage, usageDetails: {}}`. `persisted()`/`persist()` resolve
        // false, as they do for a site with no granted persistence.
        "StorageManager" => {
            if let Some((quota, usage)) = super::navigator::client_hints(scope).storage_quota {
                let d = v8::Array::new(scope, 2);
                let q: v8::Local<v8::Value> = v8::Number::new(scope, quota).into();
                let u: v8::Local<v8::Value> = v8::Number::new(scope, usage).into();
                d.set_index(scope, 0, q);
                d.set_index(scope, 1, u);
                let f = crate::binding::protos::build_fn_data(scope, cb_storage_estimate, "estimate", 0, d.into());
                own_prop(scope, proto, "estimate", f.into());
            }
            proto_method(scope, proto, "persisted", 0, cb_resolved_false);
            proto_method(scope, proto, "persist", 0, cb_resolved_false);
        }
        // The stub resolved `undefined` for every mode. Brave 151 / Chrome 150 on a desktop
        // with no headset: `inline` true, both immersive modes false, and a mode outside the
        // `XRSessionMode` enum a rejected TypeError (measured 2026-10-01, c8/repro/async/detail.html).
        "XRSystem" => proto_method(scope, proto, "isSessionSupported", 1, cb_xr_is_session_supported),
        // `Geolocation`'s methods are generated-registry stubs -- a `getCurrentPosition` that never calls
        // back, i.e. a prompt nobody answers. C5 (ABI 3 step 12): when a plugin is loaded, give it a real
        // body that resolves from `plugin_host::geolocation(ctx)` (`core/navigator/geolocation.rs`). Gated on
        // `plugin_host::any()` so the plugin-less engine keeps the stub byte-for-byte (I9/J2); the body itself
        // does nothing unless a plugin FILLS the geolocation slot, so a non-geo plugin changes nothing either.
        "Geolocation" if plugin_host::any() => super::geolocation::install(scope, proto),
        // `Clipboard`'s four methods are `context_apis.rs`: focus first, then the prompt.
        // The two answers are the page's activation record (`core/events/user_activation.rs`),
        // set by trusted key and pointer input -- FALSE in a page nobody has touched, as before.
        "UserActivation" => {
            use crate::core::events::user_activation::{cb_has_been_active, cb_is_active};
            for name in ["hasBeenActive", "isActive"] {
                let g = if name == "isActive" {
                    build_fn(scope, cb_is_active, &format!("get {name}"), 0)
                } else {
                    build_fn(scope, cb_has_been_active, &format!("get {name}"), 0)
                };
                let Some(k) = v8::String::new(scope, name) else { continue };
                let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
                let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), undef);
                d.set_enumerable(true);
                d.set_configurable(true);
                proto.define_property(scope, k.into(), &d);
            }
        }
        _ => {}
    }
}

fn cb_storage_estimate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let out = v8::Object::new(scope);
    for (i, k) in ["quota", "usage"].iter().enumerate() {
        let v = d.get_index(scope, i as u32).unwrap_or_else(|| v8::undefined(scope).into());
        own_prop(scope, out, k, v);
    }
    let details = v8::Object::new(scope);
    own_prop(scope, out, "usageDetails", details.into());
    // the browser's answer, when its storage service has given it (`fileapi::opfs_timing`: 0.3-0.5 ms with no
    // bucket, 6.2-6.6 ms as a bucket's first operation, Chrome 150)
    let ms = crate::core::fileapi::opfs::estimate_ms(scope);
    let p = crate::binding::protos::settle_later(scope, out.into(), false, crate::binding::protos::Settle::Task(ms));
    rv.set(p);
}

/// `true` when the receiver is an instance of `iface`; otherwise the promise is rejected with Chrome's
/// "Illegal invocation" for `member` -- the receiver check the generated row made (the templates lock
/// records it: `Clipboard.prototype.read.call(navigator)` rejects).
fn branded_or_reject(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, iface: &str, member: &str) -> bool {
    if crate::binding::protos::shell_receiver_ok_named(scope, args.this().into(), iface) {
        return true;
    }
    let p = crate::binding::protos::rejected_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': Illegal invocation"));
    rv.set(p);
    false
}

#[allow(dead_code)]
/// `Clipboard.readText()` / `read()` (V18): pending behind the permission prompt.
fn cb_clipboard_read_pending(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if !branded_or_reject(scope, &args, &mut rv, "Clipboard", &member) {
        return;
    }
    let p = crate::binding::protos::pending_promise(scope);
    rv.set(p);
}

fn cb_xr_is_session_supported(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::binding::protos::rejected_type_error;
    if !branded_or_reject(scope, &args, &mut rv, "XRSystem", "isSessionSupported") {
        return;
    }
    if args.length() < 1 {
        let p = rejected_type_error(scope, "Failed to execute 'isSessionSupported' on 'XRSystem': 1 argument required, but only 0 present.");
        rv.set(p);
        return;
    }
    // A Symbol cannot become the enum's string: its TypeError is pending and the call throws it.
    let Some(mode) = args.get(0).to_string(scope).map(|m| m.to_rust_string_lossy(scope)) else { return };
    let supported = match mode.as_str() {
        "inline" => true,
        "immersive-vr" | "immersive-ar" => false,
        _ => {
            let msg = format!("Failed to execute 'isSessionSupported' on 'XRSystem': The provided value '{mode}' is not a valid enum value of type XRSessionMode.");
            let p = rejected_type_error(scope, &msg);
            rv.set(p);
            return;
        }
    };
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, supported).into();
    let p = crate::binding::protos::resolved_promise(scope, v);
    rv.set(p);
}

fn cb_resolved_false(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    // `persisted()` / `persist()`: the browser's answer arrives a task later (Chrome 150, `binding::protos::Settle`)
    let p = crate::binding::protos::settle_later(scope, f, false, crate::binding::protos::Settle::Task(1.0));
    rv.set(p);
}

/// One shape-only device/capability singleton, or `null` when the realm does not expose the
/// interface at all -- which is what the JS `stub()` answered for the same case.
pub(super) fn build_device<'s>(scope: &mut v8::PinScope<'s, '_>, member: &str) -> v8::Local<'s, v8::Value> {
    let iface = DEVICE_INTERFACE.iter().find(|(m, _)| *m == member).map(|(_, i)| *i);
    debug_assert!(iface.is_some(), "build_device: no interface for Navigator.{member}");
    let Some(iface) = iface else { return v8::null(scope).into() };
    let Some((proto, _)) = parent_of(scope, iface) else { return v8::null(scope).into() };
    patch_prototype(scope, iface, proto);
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    o.into()
}

// ── hidden-constructor objects ───────────────────────────────────────────────────────────────────

/// `hidden(name, build)` from `nav-glue.js`: build the interface object Chrome does NOT put on the
/// global and hand back its prototype. The constructor is a `[native code]` function that rejects
/// `new` (WebIDL's answer for an interface with no constructor); `prototype` is `{w:0,e:0,c:0}`,
/// `constructor` is `{w:1,e:0,c:1}` and `Symbol.toStringTag` is `{w:0,e:0,c:1}`.
fn hidden_proto<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Object> {
    // A REAL `[[Construct]]` that throws its own name, like every other non-newable
    // interface object. Chrome's hidden ones answer exactly as its exposed ones do -- measured on the
    // one this engine and Chrome both have off the global, `CSSTransformComponent` reached through
    // `Object.getPrototypeOf(CSSSkewX.prototype).constructor` in a real worker: `new C()` is
    // `Failed to construct 'CSSTransformComponent': Illegal constructor`, `C()` is
    // `Illegal constructor`.
    let ctor = crate::binding::protos::build_illegal_ctor(scope, name, 0);
    let ctor_obj: v8::Local<v8::Object> = ctor.into();
    let proto = v8::Object::new(scope);
    if let Some(k) = v8::String::new(scope, "prototype") {
        let mut d = v8::PropertyDescriptor::new_from_value_writable(proto.into(), false);
        d.set_enumerable(false);
        d.set_configurable(false);
        ctor_obj.define_property(scope, k.into(), &d);
    }
    if let Some(k) = v8::String::new(scope, "constructor") {
        let mut d = v8::PropertyDescriptor::new_from_value_writable(ctor.into(), true);
        d.set_enumerable(false);
        d.set_configurable(true);
        proto.define_property(scope, k.into(), &d);
    }
    let tag_key = v8::Symbol::get_to_string_tag(scope);
    let tag = str_val(scope, name);
    let mut d = v8::PropertyDescriptor::new_from_value_writable(tag, false);
    d.set_enumerable(false);
    d.set_configurable(true);
    proto.define_property(scope, tag_key.into(), &d);
    proto
}

/// A `navigator` member whose object the PERSONA describes: the interface name its hidden
/// constructor carries, and the methods on its prototype. `navigator.brave` is the only one in the
/// Chromium family today -- a `Brave` instance whose `isBrave()` resolves `true`, reachable only as
/// `navigator.brave.constructor` because Brave exposes no `Brave` on the global.
///
/// THE NAMES USED TO BE HERE AND ARE NOT ANY MORE. `"Brave"` and `"isBrave"` are
/// browser-identifying literals, and P2 says those live in `plugins/korvus-persona/` so that swapping
/// the persona swaps the whole fingerprint rather than most of it -- `scripts/persona_literals.py` is
/// the ratchet. The presence of the ROW was already the capture's call (L7b/L7c); this moves the
/// last two strings. The split is the one-table split: the persona says WHICH member exists and WHAT
/// it answers ([`spec::HiddenAnswer`]), and the body stays a native Rust callback here, so
/// `[native code]` is still a fact of construction (invariant I3).
///
/// `None` means this device has no such object -- a Chrome capture, or a bare runtime with no
/// persona at all -- and the caller answers `null`, which is what the JS `stub()` answered for the
/// same case. It is also unreachable through a real page under such a capture, because the selector
/// does not install the row in the first place.
pub(super) fn build_hidden<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    member: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let def = persona_hidden_object(scope, member)?;
    let p = hidden_proto(scope, &def.interface);
    for m in &def.methods {
        let f = build_answer(scope, m);
        own_prop(scope, p, &m.name, f.into());
    }
    let o = crate::binding::protos::mint_on(scope, p.into());
    Some(o)
}

/// The native function that serves one [`spec::HiddenMethod`], built from the engine's own CLOSED
/// vocabulary of bodies.
///
/// The match is exhaustive on purpose: adding a [`spec::HiddenAnswer`] variant without writing its
/// Rust body does not compile, which is what keeps a persona from ever naming a body this engine does
/// not have (invariant I3 -- bodies are Rust, and nothing here is a script).
///
/// `fn_name` overrides the key when the real browser's function is not named after its slot --
/// `g_opr.scrap.name` is `""` on Opera 135, the same measurement `chrome.loadTimes` carries.
pub(crate) fn build_answer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    m: &spec::HiddenMethod,
) -> v8::Local<'s, v8::Function> {
    let name = m.fn_name.as_deref().unwrap_or(m.name.as_str());
    match m.answer {
        spec::HiddenAnswer::ResolvedTrue => build_fn(scope, cb_resolved_true, name, m.length as i32),
        spec::HiddenAnswer::Undefined => build_fn(scope, cb_undefined, name, m.length as i32),
    }
}

/// `undefined`, from a call that does nothing -- `g_opr.scrap()`'s measured answer on real Opera 135.
fn cb_undefined(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_undefined();
}

/// The persona this realm wears, asked what it hands back from `navigator.<member>`. The same I9
/// seam every other `Navigator` row reads its values through: the runtime PROVIDES the identity, the
/// Web API READS it, and there is no `KORVUS_BUNDLE` anywhere on the path.
fn persona_hidden_object(scope: &mut v8::PinScope, member: &str) -> Option<spec::HiddenObject> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)?
        .0
        .hidden_objects()
        .into_iter()
        .find(|h| h.member == member)
}

/// The shared `DeprecatedStorageQuota.prototype` behind BOTH `webkitTemporaryStorage` and
/// `webkitPersistentStorage` -- built once per realm and remembered in a v8 Private on the global,
/// because the two members are read independently and the JS memoised one prototype across them.
fn quota_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let global = scope.get_current_context().global(scope);
    if let Some(o) = evt_slot_read(scope, global, "navQuotaProto").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return o;
    }
    // `[LegacyNoInterfaceObject]` (Blink's deprecated_storage_quota.idl): there is no interface object
    // at all, so the prototype has no `constructor` and `.constructor` reads `Object` through the
    // chain -- Chrome 150 and Brave 151 both answer so (measured 2026-10-08).
    let p = hidden_proto(scope, "DeprecatedStorageQuota");
    if let Some(k) = v8::String::new(scope, "constructor") {
        p.delete(scope, k.into());
    }
    proto_method(scope, p, "queryUsageAndQuota", 1, cb_query_usage_and_quota);
    proto_method(scope, p, "requestQuota", 1, cb_request_quota);
    evt_slot_write(scope, global, "navQuotaProto", p.into());
    p
}

/// One of the two deprecated quota entry points: DISTINCT objects sharing one prototype, exactly as
/// the JS built them (`Object.create(quotaProto())` twice).
pub(super) fn build_quota<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let qp = quota_proto(scope);
    let o = crate::binding::protos::mint_on(scope, qp.into());
    let k = quota_brand_key(scope);
    let t = v8::Boolean::new(scope, true);
    o.set_private(scope, k, t.into());
    o
}

// ── DeprecatedStorageQuota's two methods (Blink `deprecated_storage_quota.cc`) ────────────────────────
//
// They were `cb_noop`: any receiver, no argument checks, and the callback never called -- a page waiting on
// `navigator.webkitTemporaryStorage.queryUsageAndQuota(cb)` waited forever. Chrome 150 (measured 2026-10-09)
// refuses a receiver that is not a `DeprecatedStorageQuota` with `TypeError: Illegal invocation`, and Blink
// answers from the quota manager -- the one `navigator.storage.estimate()` reads -- a task later:
//
//   queryUsageAndQuota(usageCallback, errorCallback?)   usageCallback(usage, quota)
//   requestQuota(newQuotaInBytes, quotaCallback?, errorCallback?)   quotaCallback(min(requested, quota))
//
// (`requestQuota` grants nothing: persistent storage is deprecated, crbug.com/1233525, so it answers the
// smaller of the request and the quota.) An opaque origin cannot store: `queryUsageAndQuota` queues the error
// callback with a `DOMError` NotSupportedError, `requestQuota` calls it at once with an AbortError one. Each
// callback is invoked with an undefined `this`, and what it throws is reported, not propagated
// (`InvokeAndReportException`).

/// The brand a `DeprecatedStorageQuota` carries: one of the two objects `navigator.webkit*Storage` minted.
fn quota_brand_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus_storage_quota").unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// The receiver check and the argument count (both methods need one argument), or the TypeError thrown.
fn quota_receiver_and_arity(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, op: &str) -> bool {
    let k = quota_brand_key(scope);
    if !args.this().get_private(scope, k).is_some_and(|v| v.is_true()) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return false;
    }
    if args.length() < 1 {
        let msg = crate::binding::protos::exec_arity_msg("DeprecatedStorageQuota", op, 1, 0);
        crate::binding::protos::throw_type_error(scope, &msg);
        return false;
    }
    true
}

/// The callback arguments at `(index, required)`, converted in order: `None` for an optional one absent or
/// null; a non-callable one is the TypeError thrown (and `None` overall).
fn quota_callbacks<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    op: &str,
    callbacks: &[(usize, bool)],
) -> Option<Vec<Option<v8::Local<'s, v8::Function>>>> {
    let mut out = Vec::new();
    for &(i, required) in callbacks {
        let v = args.get(i as i32);
        if !required && (v.is_undefined() || v.is_null()) {
            out.push(None);
            continue;
        }
        match v8::Local::<v8::Function>::try_from(v) {
            Ok(f) => out.push(Some(f)),
            Err(_) => {
                // a callback FUNCTION type's refusal in Blink's words (Chrome 150, measured 2026-10-09)
                let msg = format!("Failed to execute '{op}' on 'DeprecatedStorageQuota': parameter {} is not of type 'Function'.", i + 1);
                crate::binding::protos::throw_type_error(scope, &msg);
                return None;
            }
        }
    }
    Some(out)
}

/// ToNumber of an argument, in Chrome's words when it fails. V8 throws a Symbol's or a BigInt's refusal from
/// inside the binding, and Blink's `ExceptionPropagationCallback` puts the operation's context in front of it
/// ("Failed to execute 'requestQuota' on 'DeprecatedStorageQuota': Cannot convert a Symbol value to a number",
/// Chrome 150, measured 2026-10-09); an error the page's own `valueOf` throws goes through untouched. (The engine
/// cannot register that callback -- the V8 binding it links does not expose it -- so this member says it itself.)
fn to_number_in(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, op: &str) -> Option<f64> {
    let direct = v.is_symbol() || v.is_big_int();
    let (n, caught) = {
        v8::tc_scope!(let tc, scope);
        let n = v.to_number(tc).map(|n| n.value());
        let caught = match n {
            None if direct => tc.exception().map(|e| e.to_rust_string_lossy(tc)),
            None => {
                tc.rethrow();
                None
            }
            Some(_) => None,
        };
        (n, caught)
    };
    if let Some(e) = caught {
        let msg = e.strip_prefix("TypeError: ").unwrap_or(&e).to_string();
        crate::binding::protos::throw_type_error(scope, &format!("Failed to execute '{op}' on 'DeprecatedStorageQuota': {msg}"));
    }
    n
}

/// The quota manager's answer for this origin: `(usage, quota)`, the same numbers `estimate()` answers.
fn usage_and_quota(scope: &mut v8::PinScope) -> (f64, f64) {
    super::navigator::client_hints(scope).storage_quota.map(|(quota, usage)| (usage, quota)).unwrap_or((0.0, 0.0))
}

/// Is the running realm's origin opaque (a sandboxed frame)?
fn opaque_origin(scope: &mut v8::PinScope) -> bool {
    let g = scope.get_current_context().global(scope);
    crate::core::html::browsing_context::realm_origin(scope, g.into()) == "null"
}

/// A `DOMError` named `name` with Blink's message for it, made by this realm's own `DOMError`.
fn dom_error<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, message: &str) -> v8::Local<'s, v8::Value> {
    let n = str_val(scope, name);
    let m = str_val(scope, message);
    crate::binding::protos::iface_object(scope, "DOMError")
        .and_then(|c| v8::Local::<v8::Function>::try_from(c).ok())
        .and_then(|c| c.new_instance(scope, &[n, m]))
        .map(Into::into)
        .unwrap_or_else(|| v8::undefined(scope).into())
}

// The quota manager's answers, as Chrome 150 delivers them (measured 2026-10-09, 15 calls each, three runs): a
// realm's first quota call answered in 8.4 / 10.8 / 22.7 ms (the pipe is bound on first use, `GetQuotaHost`) --
// after a 0 ms timer set just after it -- and a later one in 0.1-0.3 ms; the answers come in the order the calls
// were made, across both `webkit*Storage` objects (one quota manager serves them in turn), a call without a
// callback included. The model: one queue per realm, drained one answer per task -- the first after 10.8 ms, each
// next 0.2 ms after the one before (the medians) -- so no answer can overtake another.

const QUOTA_FIRST_MS: f64 = 10.8;
const QUOTA_NEXT_MS: f64 = 0.2;

fn quota_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Queue one quota answer for this realm: `callback(values...)`, or nothing when the call passed no callback.
fn enqueue_quota_reply(scope: &mut v8::PinScope, callback: Option<v8::Local<v8::Function>>, values: &[v8::Local<v8::Value>]) {
    let global = scope.get_current_context().global(scope);
    let (qk, bk) = (quota_key(scope, "korvus_quota_queue"), quota_key(scope, "korvus_quota_bound"));
    let queue = match global.get_private(scope, qk).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => a,
        None => {
            let a = v8::Array::new(scope, 0);
            global.set_private(scope, qk, a.into());
            a
        }
    };
    let entry = v8::Array::new(scope, values.len() as i32 + 1);
    let cb: v8::Local<v8::Value> = match callback {
        Some(f) => f.into(),
        None => v8::null(scope).into(),
    };
    entry.set_index(scope, 0, cb);
    for (i, v) in values.iter().enumerate() {
        entry.set_index(scope, i as u32 + 1, *v);
    }
    let n = queue.length();
    queue.set_index(scope, n, entry.into());
    if n == 0 {
        let first = !global.get_private(scope, bk).is_some_and(|v| v.is_true());
        let t = v8::Boolean::new(scope, true);
        global.set_private(scope, bk, t.into());
        let f = crate::binding::protos::build_fn(scope, cb_quota_drain, "", 0);
        crate::core::intersection_observer::rendering::queue_task(scope, f, if first { QUOTA_FIRST_MS } else { QUOTA_NEXT_MS });
    }
}

/// Deliver the realm's oldest quota answer; the next one follows a task later.
fn cb_quota_drain(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    let qk = quota_key(scope, "korvus_quota_queue");
    let Some(queue) = global.get_private(scope, qk).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let n = queue.length();
    if n == 0 {
        return;
    }
    let head = queue.get_index(scope, 0);
    let rest = v8::Array::new(scope, n as i32 - 1);
    for i in 1..n {
        if let Some(v) = queue.get_index(scope, i) {
            rest.set_index(scope, i - 1, v);
        }
    }
    global.set_private(scope, qk, rest.into());
    if n > 1 {
        let f = crate::binding::protos::build_fn(scope, cb_quota_drain, "", 0);
        crate::core::intersection_observer::rendering::queue_task(scope, f, QUOTA_NEXT_MS);
    }
    let Some(entry) = head.and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let Some(cb) = entry.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let vals: Vec<v8::Local<v8::Value>> = (1..entry.length()).filter_map(|i| entry.get_index(scope, i)).collect();
    invoke_and_report(scope, cb, &vals);
}

/// Queue `callback(values...)` `ms` from now, as a task of its own (an opaque origin's error, which Blink posts
/// at once rather than asking the quota manager).
fn reply_later(scope: &mut v8::PinScope, ms: f64, callback: v8::Local<v8::Function>, values: &[v8::Local<v8::Value>]) {
    let d = v8::Array::new(scope, values.len() as i32 + 1);
    d.set_index(scope, 0, callback.into());
    for (i, v) in values.iter().enumerate() {
        d.set_index(scope, i as u32 + 1, *v);
    }
    let f = crate::binding::protos::build_fn_data(scope, cb_quota_reply, "", 0, d.into());
    crate::core::intersection_observer::rendering::queue_task(scope, f, ms);
}

fn cb_quota_reply(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(cb) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let vals: Vec<v8::Local<v8::Value>> = (1..d.length()).filter_map(|i| d.get_index(scope, i)).collect();
    invoke_and_report(scope, cb, &vals);
}

/// Blink's `InvokeAndReportException`: `this` undefined; a throw is reported at the realm, never propagated.
fn invoke_and_report(scope: &mut v8::PinScope, cb: v8::Local<v8::Function>, vals: &[v8::Local<v8::Value>]) {
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let exc = {
        v8::tc_scope!(let tc, scope);
        cb.call(tc, undef, vals);
        // reported where it was thrown (`report::caught_site`)
        let e = if tc.has_caught() {
            tc.exception().map(|e| {
                let m = tc.message();
                (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
            })
        } else {
            None
        };
        tc.reset();
        e
    };
    if let Some((e, site)) = exc {
        let e = v8::Local::new(scope, &e);
        crate::core::events::report::report_exception_at(scope, e, site);
    }
}

fn cb_query_usage_and_quota<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !quota_receiver_and_arity(scope, &args, "queryUsageAndQuota") {
        return;
    }
    let Some(cbs) = quota_callbacks(scope, &args, "queryUsageAndQuota", &[(0, true), (1, false)]) else { return };
    if opaque_origin(scope) {
        // `EnqueueStorageErrorCallback`: a task posted at once, not a quota-manager reply
        if let Some(err_cb) = cbs[1] {
            let e = dom_error(scope, "NotSupportedError", "The implementation did not support the requested type of object or operation.");
            reply_later(scope, 0.0, err_cb, &[e]);
        }
        return;
    }
    let (usage, quota) = usage_and_quota(scope);
    let u = v8::Number::new(scope, usage).into();
    let q = v8::Number::new(scope, quota).into();
    enqueue_quota_reply(scope, cbs[0], &[u, q]);
}

fn cb_request_quota<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !quota_receiver_and_arity(scope, &args, "requestQuota") {
        return;
    }
    // `unsigned long long newQuotaInBytes` (WebIDL ToNumber, truncate, modulo 2^64), converted before the callbacks
    let Some(n) = to_number_in(scope, args.get(0), "requestQuota") else { return };
    let requested = if n.is_finite() { n.trunc().rem_euclid(18_446_744_073_709_551_616.0) } else { 0.0 };
    let Some(cbs) = quota_callbacks(scope, &args, "requestQuota", &[(1, false), (2, false)]) else { return };
    if opaque_origin(scope) {
        if let Some(err_cb) = cbs[1] {
            let e = dom_error(scope, "AbortError", "The user aborted a request.");
            invoke_and_report(scope, err_cb, &[e]);
        }
        return;
    }
    let (_, quota) = usage_and_quota(scope);
    // `std::min(saturated_cast<int64_t>(requested), quota)`
    let granted = requested.min(9_223_372_036_854_775_807.0).min(quota);
    let g = v8::Number::new(scope, granted).into();
    enqueue_quota_reply(scope, cbs[0], &[g]);
}

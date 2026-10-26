//! WebGPU — the part of the `GPU*` family that is reachable on a machine with **no GPU adapter**,
//! native, on the shell prototypes.
//!
//! ## The one fact that decides what "correct" means here
//!
//! The recorded browser (`brave-151-linux-headless`) is a headless Brave with no GPU adapter, and
//! the recorded corpus reflects that with unusual clarity. Of the 35 `GPU*` interfaces and their 202
//! members in `brave-151.surface.json`, the oracle could reach a receiver for exactly four:
//!
//! ```text
//!   GPU                     navigator.gpu                                   3 members asked
//!   GPUCanvasContext        canvas.getContext("webgpu")                      5 members asked
//!   GPUError                new GPUValidationError("m")                      1 member  asked
//!   GPUPipelineError        new GPUPipelineError("m", {reason:"validation"}) 1 member  asked
//! ```
//!
//! Every other GPU interface's case file holds one line — `new X()` throws a TypeError — because
//! there is no expression that produces an instance. `gencases.rs` says why, in the recipe table
//! Track 1.1 vetted against the live browser:
//!
//! > WebGPU without an adapter. `requestAdapter()` is a promise and no expression can await it, so
//! > the adapter-side of the API stays correctly unasked -- but the canvas context, the error
//! > hierarchy and the language-feature set are all synchronous and all real here.
//!
//! So the recorded ceiling for this family is **10 members**, not 202. This file implements those
//! ten and the constructors they hang off. An adapter, a device, or a limits table would make the
//! other 192 *look* reachable; none of them is recorded anywhere, and inventing them is a fabricated
//! fingerprint rather than a feature.
//!
//! ## What the corpus actually says about `requestAdapter()`
//!
//! One case, and it is a shape question, not a value question:
//!
//! ```text
//! GPU.requestAdapter
//!   Object.prototype.toString.call(navigator.gpu.requestAdapter())  ->  "[object Promise]"
//! ```
//!
//! The resolution is not recorded and could not have been: the oracle evaluates one synchronous
//! expression. So the promise is real, and it resolves with `null` — the value a browser gives when
//! no adapter matches, and the only value this engine can hand back without claiming a device it
//! does not have.
//!
//! ## Why the three "always" answers on `GPUCanvasContext` are not stubs
//!
//! `configure()` takes a `GPUCanvasConfiguration` whose `device` member is a **required**
//! `GPUDevice`. `requestAdapter()` resolves `null` and `GPUDevice`'s constructor is illegal, so no
//! value a page can construct passes that check — `configure()` therefore always raises the
//! WebIDL TypeError, and the context is never configured. `getConfiguration()` returning `null` and
//! `getCurrentTexture()` raising `InvalidStateError` are the *consequences* of that, which is
//! exactly what the recorded browser answers for the same reason. There is deliberately no
//! configuration slot: a member that stores a value and reads it back without it reaching anything
//! is the shape this track is forbidden to ship.
//!
//! ## `wgslLanguageFeatures` is the shipped compiler's
//!
//! The set lists the optional WGSL language features of Chrome 150's shader compiler (Tint), which is
//! a fact of the browser build, not of a GPU: stock Chrome 150 answers the same ten with or without an
//! adapter (measured). They are vendored with their source
//! (`gen/blink/webgpu/wgsl_language_features.json`) in Chrome's enumeration order. No shader reaches a
//! compiler here -- `requestAdapter()` resolves `null`, so no `GPUDevice` and no `createShaderModule`
//! exists to exercise them.

use deno_core::v8;

use super::html_canvas_element::{priv_get, priv_set};
use crate::binding::protos::{
    get_prop, global_get, new_global, require_args,
    require_ctor_args, require_ctor_new_target, str_val,
    throw_dom, throw_type_error,
};
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;

/// The `<canvas>` a `GPUCanvasContext` belongs to. A private slot rather than an own data property:
/// a real context has no own properties, and `Object.getOwnPropertyNames(ctx)` is one expression.
const CTX_CANVAS_PRIV: &str = "korvus_gpu_canvas";
/// The per-`GPU` `WGSLLanguageFeatures` singleton (`navigator.gpu.wgslLanguageFeatures` is the same
/// object every read, as it is in a browser).
const GPU_WGSL_PRIV: &str = "korvus_gpu_wgsl";
/// A `GPUError`'s message, set by the three concrete error constructors.
const ERR_MSG_PRIV: &str = "korvus_gpu_errmsg";
/// A `GPUPipelineError`'s `reason`.
const PIPE_REASON_PRIV: &str = "korvus_gpu_reason";
/// A `GPUUncapturedErrorEvent`'s `error`.
const EVT_ERROR_PRIV: &str = "korvus_gpu_error";
/// The BRAND of a `WGSLLanguageFeatures`: every one of its six members is an operation on an internal
/// slot, and Chrome answers `Illegal invocation` for a receiver that has none (including
/// `Object.create(WGSLLanguageFeatures.prototype)`). This slot is that internal slot.
const WGSL_BRAND_PRIV: &str = "korvus_gpu_wgsl_brand";

/// The canvas format `getPreferredCanvasFormat()` answers. The recorded browser says `rgba8unorm`;
/// it is the one value this member has ever been observed to return on this engine's control, and
/// there is no captured field that would make it vary.
const PREFERRED_CANVAS_FORMAT: &str = "rgba8unorm";

// ── small helpers ─────────────────────────────────────────────────────────────────────────────

/// Is `v` an instance of the global interface `name`? Used where WebIDL demands an interface type
/// (a `GPUDevice`, a `GPUError`) and a plain object must be refused with the browser's TypeError.
fn instance_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, name: &str) -> bool {
    let Some(ctor) = global_get(scope, name).and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
    else {
        return false;
    };
    v.is_object() && v.instance_of(scope, ctor).unwrap_or(false)
}

/// The "Illegal invocation" TypeError a native accessor raises when `this` is not an instance of
/// the interface that owns it (`GPUCanvasContext.prototype.canvas` read off the prototype itself).
fn illegal_invocation(scope: &mut v8::PinScope) {
    throw_type_error(scope, "Illegal invocation");
}

/// The WebIDL "not enough arguments" TypeError for a CONSTRUCTOR — `require_args` words it for a
/// method ("Failed to execute"), and Chrome says "Failed to construct" here.
// ── GPU (navigator.gpu) ───────────────────────────────────────────────────────────────────────

/// `navigator.gpu.getPreferredCanvasFormat()` — the texture format a canvas should be configured
/// with on this platform. A per-platform constant in a browser too; recorded as `rgba8unorm`.
fn cb_gpu_preferred_format(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let s = str_val(scope, PREFERRED_CANVAS_FORMAT);
    rv.set(s);
}

/// `navigator.gpu.requestAdapter(options)` — a real promise, resolved with `null`.
///
/// `null` is what the WebGPU spec says to resolve with when no adapter matches the request, and
/// this engine has no adapter to match: there is no GPU backend behind `GPU*`, and the control this
/// surface is measured against is a headless browser with none either. Resolving with a fabricated
/// `GPUAdapter` would make 192 unreachable members look reachable and would publish adapter limits
/// nothing recorded.
fn cb_gpu_request_adapter(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let null = v8::null(scope);
    // no adapter, answered by the GPU process a few milliseconds later (Chrome 150: after a 4 ms timer queued right
    // after the call, before an 8 ms one -- `binding::protos::Settle`)
    let p = crate::binding::protos::settle_later(scope, null.into(), false, crate::binding::protos::Settle::Task(5.0));
    rv.set(p);
}

/// `navigator.gpu.wgslLanguageFeatures` — the same `WGSLLanguageFeatures` object every read.
///
/// THE RECEIVER IS CHECKED, and it was not before the foreign-receiver sweep found
/// it: this getter LAZILY CREATED a feature set on whatever `this` was and cached it there, so
/// `Object.getOwnPropertyDescriptor(GPU.prototype, "wgslLanguageFeatures").get.call({})` answered
/// `[object WGSLLanguageFeatures]` where Chrome throws `Illegal invocation` — and stamped a private
/// slot on the caller's object while doing it. The check is the shell stub's own
/// ([`shell_receiver_ok`] against the prototype the row is mounted on, passed as the callback's
/// `data`), which is what `navigator.gpu` satisfies: `js/nav-glue.js` builds it with
/// `Object.create(GPU.prototype)` plus the shell brand.
fn cb_gpu_wgsl_features(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let this = args.this();
    if !crate::binding::protos::shell_receiver_ok_named(scope, this.into(), "GPU") {
        illegal_invocation(scope);
        return;
    }
    if let Some(v) = priv_get(scope, this, GPU_WGSL_PRIV) {
        rv.set(v);
        return;
    }
    let obj = crate::install::live::mint_table(scope, "WGSLLanguageFeatures");
    let t = v8::Boolean::new(scope, true);
    priv_set(scope, obj, WGSL_BRAND_PRIV, t.into());
    let v: v8::Local<v8::Value> = obj.into();
    priv_set(scope, this, GPU_WGSL_PRIV, v);
    rv.set(v);
}

// ── WGSLLanguageFeatures (a setlike over the shipped compiler's features) ───────────────────────────────────────

/// The receiver as a `WGSLLanguageFeatures`, or a pending `TypeError: Illegal invocation`.
///
/// ADDED BY a foreign-receiver sweep. All six members answered for ANY receiver
/// -- `WGSLLanguageFeatures.prototype.size` read `0` off a bare `{}`, `has()` read `false`,
/// `keys()` handed back an iterator -- because an empty set needs no state and so had no brand to
/// check. Chrome throws for every one of them. The brand is the private slot the only mint site
/// writes, not the prototype chain, so `Object.create(WGSLLanguageFeatures.prototype).size` is
/// refused here exactly as it is there.
fn wgsl_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if priv_get(scope, this, WGSL_BRAND_PRIV).is_some() {
        return true;
    }
    illegal_invocation(scope);
    false
}

/// The vendored set, in Chrome 150's order.
fn wgsl_features() -> Vec<String> {
    const DATA: &str = include_str!("../../../gen/blink/webgpu/wgsl_language_features.json");
    serde_json::from_str::<serde_json::Value>(DATA)
        .ok()
        .and_then(|v| v.get("features").and_then(|f| f.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()))
        .unwrap_or_default()
}

fn cb_wgsl_size(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if !wgsl_ok(scope, args.this()) {
        return;
    }
    rv.set_uint32(wgsl_features().len() as u32);
}

/// `values()` / `keys()` (each feature) and `entries()` (`[feature, feature]`), a setlike's iterators.
fn cb_wgsl_iter(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if !wgsl_ok(scope, args.this()) {
        return;
    }
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let items: Vec<v8::Local<v8::Value>> = wgsl_features()
        .iter()
        .map(|f| {
            let s = str_val(scope, f);
            if which == "entries" { crate::core::dom::webidl_iter::pair(scope, s, s) } else { s }
        })
        .collect();
    if let Some(it) = crate::core::dom::webidl_iter::make_iter(scope, "WGSLLanguageFeatures", crate::core::dom::webidl_iter::IterKind::Set, &items) {
        rv.set(it.into());
    }
}

fn cb_wgsl_has(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if !wgsl_ok(scope, args.this()) {
        return;
    }
    if !require_args(scope, &args, 1, "WGSLLanguageFeatures", "has") {
        return;
    }
    let Some(k) = args.get(0).to_string(scope) else { return };
    let k = k.to_rust_string_lossy(scope);
    rv.set_bool(wgsl_features().iter().any(|f| *f == k));
}

/// `forEach(callback, thisArg)`: `callback(value, key, set)` per feature, the key the value again.
fn cb_wgsl_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !wgsl_ok(scope, args.this()) {
        return;
    }
    let feats: Vec<v8::Local<'s, v8::Value>> = wgsl_features().iter().map(|f| str_val(scope, f)).collect();
    crate::core::dom::webidl_iter::for_each(scope, &args, "WGSLLanguageFeatures", |_, i| feats.get(i).map(|v| (*v, *v)));
}

// ── GPUCanvasContext ──────────────────────────────────────────────────────────────────────────

/// Turn a freshly built context object into `canvas`'s WebGPU context: the back-reference lives on
/// a private slot, so the object keeps a browser's zero own properties.
pub(crate) fn init_canvas_context(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
    canvas: v8::Local<v8::Object>,
) {
    let v: v8::Local<v8::Value> = canvas.into();
    priv_set(scope, ctx, CTX_CANVAS_PRIV, v);
}

fn cb_ctx_canvas(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    match priv_get(scope, args.this(), CTX_CANVAS_PRIV) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// `ctx.configure(configuration)`.
///
/// `GPUCanvasConfiguration.device` is a required `GPUDevice`. No `GPUDevice` can exist here —
/// `requestAdapter()` resolves `null`, and the interface's own constructor is illegal — so the
/// conversion of that member always fails, which is the same TypeError a browser raises when the
/// member is missing or of the wrong type. That is why the context can never become configured,
/// and why the two readers below answer as they do.
fn cb_ctx_configure(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    if !require_args(scope, &args, 1, "GPUCanvasContext", "configure") {
        return;
    }
    if priv_get(scope, args.this(), CTX_CANVAS_PRIV).is_none() {
        illegal_invocation(scope);
        return;
    }
    let device = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|o| get_prop(scope, o, "device"))
        .filter(|d| !d.is_undefined());
    let ok = match device {
        Some(d) => instance_of(scope, d, "GPUDevice"),
        None => false,
    };
    if !ok {
        throw_type_error(
            scope,
            "Failed to execute 'configure' on 'GPUCanvasContext': Failed to read the 'device' property from 'GPUCanvasConfiguration': Required member is undefined.",
        );
    }
}

fn cb_ctx_unconfigure(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if priv_get(scope, args.this(), CTX_CANVAS_PRIV).is_none() {
        illegal_invocation(scope);
        return;
    }
    // Unconfiguring a context that was never configured is a no-op in the spec, not an error.
    rv.set_undefined();
}

/// `ctx.getConfiguration()` — `null` until `configure()` succeeds, and it never does (see above).
fn cb_ctx_get_configuration(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if priv_get(scope, args.this(), CTX_CANVAS_PRIV).is_none() {
        illegal_invocation(scope);
        return;
    }
    let null = v8::null(scope);
    rv.set(null.into());
}

/// `ctx.getCurrentTexture()` — `InvalidStateError` on an unconfigured context, which this one
/// always is.
fn cb_ctx_get_current_texture(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    if priv_get(scope, args.this(), CTX_CANVAS_PRIV).is_none() {
        illegal_invocation(scope);
        return;
    }
    throw_dom(
        scope,
        "Failed to execute 'getCurrentTexture' on 'GPUCanvasContext': context is not configured",
        "InvalidStateError",
    );
}

// ── GPUError and its three concrete subclasses ────────────────────────────────────────────────

fn cb_error_message(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    match priv_get(scope, args.this(), ERR_MSG_PRIV) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// `new GPUValidationError(message)` / `GPUOutOfMemoryError` / `GPUInternalError`. The message is a
/// required `DOMString`; it is stored on a private slot and read back through `GPUError`'s
/// prototype accessor, so the instance has no own properties.
pub(crate) fn cb_gpu_error_ctor(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let iface = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // Chrome puts the interface name in front of this sentence too ("Failed to construct
    // 'GPUValidationError': Please use the 'new' operator, ..."); this said only the tail until
    // the body went on the row and the message in `installer_parity`'s golden.
    if !require_ctor_new_target(scope, &args, &iface) {
        return;
    }
    if !require_ctor_args(scope, &args, 1, &iface) {
        return;
    }
    let msg = args.get(0).to_rust_string_lossy(scope);
    let v = str_val(scope, &msg);
    priv_set(scope, args.this(), ERR_MSG_PRIV, v);
}

// ── GPUPipelineError ──────────────────────────────────────────────────────────────────────────

fn cb_pipeline_reason(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    match priv_get(scope, args.this(), PIPE_REASON_PRIV) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// `new GPUPipelineError(message, options)`.
///
/// `GPUPipelineError` inherits from `DOMException`, and `DOMException`'s `message`/`name`/`code` are
/// accessors over instance state this engine keeps in a WeakMap keyed by the object the
/// `DOMException` constructor created. So the instance IS built by that constructor and then
/// re-parented onto `GPUPipelineError.prototype` — a constructor may return an object, and doing so
/// is what makes `err.message` and `err.name` real rather than the empty fallback a bare
/// re-prototyped object would read.
pub(crate) fn cb_pipeline_error_ctor(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if !require_ctor_new_target(scope, &args, "GPUPipelineError") {
        return;
    }
    // `constructor(optional DOMString message = "", GPUPipelineErrorInit options)` -- `options` is
    // required, so the argless form is a TypeError even though the first argument is optional.
    //
    // ONE, not two. WebIDL counts an interface object's `length` from the LONGEST leading
    // run of required arguments, and `message` is optional, so the count Chrome quotes is 1 -- which
    // is also this row's own `ctor.length` and what the capture recorded. Measured, headless Chrome
    // 150.0.7871.46 over loopback, 2026-09-12: `new GPUPipelineError()` -> `1 argument required, but
    // only 0 present.`, and `new GPUPipelineError("m")` gets past the arity step to a type error on
    // the missing dictionary. This said 2 and disagreed with its own row.
    if !require_ctor_args(scope, &args, 1, "GPUPipelineError") {
        return;
    }
    let msg = args.get(0).to_rust_string_lossy(scope);
    // `GPUPipelineErrorInit.reason` is a required `GPUPipelineErrorReason` enum.
    let reason = v8::Local::<v8::Object>::try_from(args.get(1))
        .ok()
        .and_then(|o| get_prop(scope, o, "reason"))
        .filter(|r| !r.is_undefined())
        .map(|r| r.to_rust_string_lossy(scope));
    let Some(reason) = reason else {
        throw_type_error(
            scope,
            "Failed to construct 'GPUPipelineError': Failed to read the 'reason' property from 'GPUPipelineErrorInit': Required member is undefined.",
        );
        return;
    };
    if reason != "validation" && reason != "internal" {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct 'GPUPipelineError': Failed to read the 'reason' property from 'GPUPipelineErrorInit': The provided value '{reason}' is not a valid enum value of type GPUPipelineErrorReason."
            ),
        );
        return;
    }
    let m = str_val(scope, &msg);
    let n = str_val(scope, "GPUPipelineError");
    let Some(exc) = new_global(scope, "DOMException", &[m, n]) else {
        throw_type_error(scope, "Failed to construct 'GPUPipelineError'");
        return;
    };
    set_table_proto(scope, exc, "GPUPipelineError");
    let r = str_val(scope, &reason);
    priv_set(scope, exc, PIPE_REASON_PRIV, r);
    rv.set(exc.into());
}

// ── GPUUncapturedErrorEvent ───────────────────────────────────────────────────────────────────

fn cb_uncaptured_error(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    match priv_get(scope, args.this(), EVT_ERROR_PRIV) {
        Some(v) => rv.set(v),
        None => illegal_invocation(scope),
    }
}

/// `new GPUUncapturedErrorEvent(type, init)` — `init.error` is a required `GPUError`, which is why
/// the recorded browser answers TypeError to both `new GPUUncapturedErrorEvent()` and
/// `new GPUUncapturedErrorEvent("x", {})`. A `GPUValidationError` IS constructable, so
/// `new GPUUncapturedErrorEvent("x", { error: new GPUValidationError("m") })` builds a real event.
pub(crate) fn cb_uncaptured_error_event_ctor(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if !require_ctor_new_target(scope, &args, "GPUUncapturedErrorEvent") {
        return;
    }
    if !require_ctor_args(scope, &args, 2, "GPUUncapturedErrorEvent") {
        return;
    }
    // The arguments as WebIDL converts them, in order (Chrome 150): `type` to a string, then
    // `GPUUncapturedErrorEventInit` -- `bubbles`, `cancelable`, `composed`, then `error`, required and a `GPUError`.
    // korvus read `error` alone and first, and a non-object dictionary answered its required-member message.
    const PREFIX: &str = "Failed to construct 'GPUUncapturedErrorEvent': ";
    let ty = args.get(0);
    if ty.is_symbol() {
        throw_type_error(scope, &format!("{PREFIX}Cannot convert a Symbol value to a string"));
        return;
    }
    let Some(ty) = ty.to_string(scope) else { return };
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "GPUUncapturedErrorEventInit", PREFIX) else { return };
    let Some(err) = get_prop(scope, init, "error") else { return };
    // The base event is built by the REAL `Event` constructor and then re-parented, for the same
    // reason `GPUPipelineError` is: `Event`'s nine members and its own unforgeable `isTrusted` live
    // in per-instance slots that only that constructor writes. An object merely given
    // `GPUUncapturedErrorEvent.prototype` would read `type` as undefined and carry no `isTrusted`.
    // It reads the converted dictionary, whose members are values, not the page's getters.
    let Some(ev) = new_global(scope, "Event", &[ty.into(), init.into()]) else {
        return;
    };
    set_table_proto(scope, ev, "GPUUncapturedErrorEvent");
    priv_set(scope, ev, EVT_ERROR_PRIV, err);
    rv.set(ev.into());
}

// ── install: the six tables ───────────────────────────────────────────────────────────────────
//
// Until this lane this file fetched six prototypes out of the generated shell
// registry and defined seventeen members onto them by hand -- a second install mechanism beside
// `install/installer.rs`, which is what invariant I2 forbids, and seventeen members the install
// ledger never saw. Every one is a table row now, and the installer owns all six prototypes.
//
// EVERY ROW OF ALL SIX IS IMPLEMENTED. This family is small and completely covered: `GPU`'s three,
// `WGSLLanguageFeatures`' six, `GPUCanvasContext`'s five, and one each on `GPUError`,
// `GPUPipelineError` and `GPUUncapturedErrorEvent`. The ~192 members of the family that stay shells
// (`GPUDevice`, `GPUBuffer`, `GPUTexture`, ...) are interfaces this file never touched and still does
// not: with no adapter there is nothing honest to answer, and they remain generated registry rows.

/// Every `GPU`, `WGSLLanguageFeatures`, `GPUCanvasContext`, `GPUError`, `GPUPipelineError` and
/// `GPUUncapturedErrorEvent` row.
///
/// One install fn for six interfaces because the member names do not collide and each body is its
/// own callback; the row's label says which interface it came from where that ever matters.
pub fn install_webgpu(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // GPU. The one row whose receiver check needs the PROTOTYPE (its instance carries no slot of
        // its own until the first read), so the getter is minted here with the prototype as `data`
        // rather than through the shared `getter_row`.
        "wgslLanguageFeatures" => {
            // the brand check is by interface name, so the getter carries no prototype
            let g = crate::binding::protos::build_fn(scope, cb_gpu_wgsl_features, "get wgslLanguageFeatures", 0);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "getPreferredCanvasFormat" => method_row(scope, proto, member, cb_gpu_preferred_format),
        "requestAdapter" => method_row(scope, proto, member, cb_gpu_request_adapter),
        // WGSLLanguageFeatures -- a setlike over the shipped compiler's features
        "size" => getter_row(scope, proto, member, cb_wgsl_size),
        "entries" | "keys" | "values" => {
            let d = str_val(scope, member.name);
            crate::core::dom::shared::row_method_data(scope, proto, member, cb_wgsl_iter, d);
        }
        "has" => method_row(scope, proto, member, cb_wgsl_has),
        "forEach" => method_row(scope, proto, member, cb_wgsl_for_each),
        // A setlike's `Symbol.iterator` IS its `values`, and WebIDL gives it
        // `{writable, non-enumerable, configurable}`. The row has to come AFTER `values` in the
        // table, because it reads the function object that row installed.
        "@@iterator" => {
            let Some(values) = get_prop(scope, proto, "values") else { return };
            crate::install::kits::define_value(scope, proto, member, values, true);
        }
        // GPUCanvasContext
        "canvas" => getter_row(scope, proto, member, cb_ctx_canvas),
        "configure" => method_row(scope, proto, member, cb_ctx_configure),
        "getConfiguration" => method_row(scope, proto, member, cb_ctx_get_configuration),
        "getCurrentTexture" => method_row(scope, proto, member, cb_ctx_get_current_texture),
        "unconfigure" => method_row(scope, proto, member, cb_ctx_unconfigure),
        // The three error interfaces
        "message" => getter_row(scope, proto, member, cb_error_message),
        "reason" => getter_row(scope, proto, member, cb_pipeline_reason),
        "error" => getter_row(scope, proto, member, cb_uncaptured_error),
        other => debug_assert!(false, "install_webgpu: unexpected member {other}"),
    }
}

// THE THREE CONCRETE `GPUError` CONSTRUCTORS ARE THEIR ROWS' NOW. This file used to
// carry `install_concrete_error_ctors`, which called `make_constructable_with_data` once per name
// from `wire_canvas_oracle` -- a SWAP of the interface object `create_interface` had already
// mounted, which is why the install ledger reported all three `constructor` slots as second writers.
// `install::ctors::row_ctor` names [`cb_gpu_error_ctor`] for the three instead, and the ONE create
// builds the interface object with it: the capture's `length` (1) and the prototype come from the
// row rather than being read back off the object about to be discarded.
//
// Their prototypes inherit `GPUError.prototype`, which is a TABLE -- so the registry's chain pass
// cannot find the parent and leaves them on the bare `Object.prototype`.
// `install::live::rows::rechain_orphans`, which `install_window_postshells` runs over the whole
// registry, repairs exactly that case (both halves: the prototype chain AND the interface object's),
// and `tests/webgpu_live.rs` asserts the repaired chain rather than assuming it.

webapi::interface! {
    "GPU" [interface, both, secure] {
        ctor "GPU" len 0 = stub,
        attr "wgslLanguageFeatures" : Any readonly = install_webgpu,
        method "getPreferredCanvasFormat" : Any = install_webgpu,
        method "requestAdapter" : Promise = install_webgpu
    }
}

webapi::interface! {
    "WGSLLanguageFeatures" [interface, both, secure] {
        ctor "WGSLLanguageFeatures" len 0 = stub,
        attr "size" : Any readonly = install_webgpu,
        method "entries" : Any = install_webgpu,
        method "forEach" : Any len 1 = install_webgpu,
        method "has" : Any len 1 = install_webgpu,
        method "keys" : Any = install_webgpu,
        method "values" : Any = install_webgpu,
        method "@@iterator" : Any hidden = install_webgpu
    }
}

webapi::interface! {
    "GPUCanvasContext" [interface, both, secure] {
        ctor "GPUCanvasContext" len 0 = stub,
        attr "canvas" : Any readonly = install_webgpu,
        method "configure" : Any len 1 required 1 = install_webgpu,
        method "getConfiguration" : Any nullable = install_webgpu,
        method "getCurrentTexture" : Any = install_webgpu,
        method "unconfigure" : Any = install_webgpu
    }
}

webapi::interface! {
    "GPUError" [interface, both, secure] {
        ctor "GPUError" len 0 = stub,
        attr "message" : Any readonly = install_webgpu
    }
}

webapi::interface! {
    "GPUPipelineError" : "DOMException" [value, both, secure] {
        ctor "GPUPipelineError" len 1 = stub,
        attr "reason" : Any readonly = install_webgpu
    }
}

webapi::interface! {
    "GPUUncapturedErrorEvent" : "Event" [value, both, secure] {
        ctor "GPUUncapturedErrorEvent" len 2 = stub,
        attr "error" : Any readonly = install_webgpu
    }
}
// ── the five WebGPU CONSTANT NAMESPACES ─────────────────────────────────────────────────────────
//
// `GPUBufferUsage` and friends are WebIDL `namespace` declarations, not interfaces: a plain object on
// the global holding read-only bitflag constants, with no constructor and no prototype of its own.
// The one-table model describes INTERFACES, so they are a table of DATA here rather than an
// `interface!` — five names, their constants, and one installer, in the WebGPU file.
//
// MEASURED on `google-chrome --headless=new` 150.0.7871.46 in a page AND in a real worker (the two
// dumps are identical), because the surface capture records the five NAMES in `globals` and nothing
// else: the global property is a NON-enumerable, configurable, writable data property; the object has
// `Object.prototype` for its prototype, a non-writable `Symbol.toStringTag`, and is EXTENSIBLE and
// NOT frozen; each constant is `{writable: false, enumerable: true, configurable: false}`.
//
// `core/korvus-runtime/src/js/worker-finalize.js` built these by hand and got four things wrong: the
// global property was enumerable (a tell on `Object.keys(self)`), non-writable and non-configurable,
// the objects had no `Symbol.toStringTag` (so `String(GPUBufferUsage)` read `[object Object]`), and
// `GPUTextureUsage` was missing `TRANSIENT_ATTACHMENT`.
type GpuNamespace = (&'static str, &'static [(&'static str, u32)]);

const GPU_NAMESPACES: &[GpuNamespace] = &[
    ("GPUBufferUsage", &[
        ("MAP_READ", 0x0001), ("MAP_WRITE", 0x0002), ("COPY_SRC", 0x0004), ("COPY_DST", 0x0008),
        ("INDEX", 0x0010), ("VERTEX", 0x0020), ("UNIFORM", 0x0040), ("STORAGE", 0x0080),
        ("INDIRECT", 0x0100), ("QUERY_RESOLVE", 0x0200),
    ]),
    ("GPUColorWrite", &[("RED", 0x1), ("GREEN", 0x2), ("BLUE", 0x4), ("ALPHA", 0x8), ("ALL", 0xF)]),
    ("GPUMapMode", &[("READ", 0x1), ("WRITE", 0x2)]),
    ("GPUShaderStage", &[("VERTEX", 0x1), ("FRAGMENT", 0x2), ("COMPUTE", 0x4)]),
    ("GPUTextureUsage", &[
        ("COPY_SRC", 0x01), ("COPY_DST", 0x02), ("TEXTURE_BINDING", 0x04), ("STORAGE_BINDING", 0x08),
        ("RENDER_ATTACHMENT", 0x10), ("TRANSIENT_ATTACHMENT", 0x20),
    ]),
];

/// Install the five namespace objects onto `global`, with the descriptors measured above.
pub fn install_gpu_namespaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (name, consts) in GPU_NAMESPACES {
        let Some(key) = v8::String::new(scope, name) else { continue };
        if global.has_own_property(scope, key.into()).unwrap_or(false) {
            continue;
        }
        let holder = v8::Object::new(scope);
        for (c, v) in *consts {
            let Some(ck) = v8::String::new(scope, c) else { continue };
            let cv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, *v).into();
            let mut cd = v8::PropertyDescriptor::new_from_value_writable(cv, false);
            cd.set_enumerable(true);
            cd.set_configurable(false);
            holder.define_property(scope, ck.into(), &cd);
        }
        let tagv: v8::Local<v8::Value> = v8::String::new(scope, name).unwrap().into();
        let mut td = v8::PropertyDescriptor::new_from_value_writable(tagv, false);
        td.set_enumerable(false);
        td.set_configurable(true);
        let tag_key = v8::Symbol::get_to_string_tag(scope);
        holder.define_property(scope, tag_key.into(), &td);
        let hv: v8::Local<v8::Value> = holder.into();
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(hv, true);
        pd.set_enumerable(false);
        pd.set_configurable(true);
        global.define_property(scope, key.into(), &pd);
    }
}

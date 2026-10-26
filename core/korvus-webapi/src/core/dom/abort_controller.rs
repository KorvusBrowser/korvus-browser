//! `core/dom/abort_controller.rs` — `AbortController` and `AbortSignal`.
//!
//! W5.1 L12 made both `interface!` tables. W4.c takes the last JS out of them: `host.js`'s
//! `class AbortSignal extends globalThis.EventTarget` and `class AbortController` are gone, the
//! constructor is a native callback, the state is in v8 privates, and every row is behind the
//! interface's brand.
//!
//! `class AbortSignal extends globalThis.EventTarget` is one of the THREE ESM-time `extends
//! EventTarget` declarations W4.b named as the reason `events.js` still exists. This file removes one
//! of them.
//!
//! WHAT THE JS CLASSES GOT WRONG, measured with `tests/fixtures/abort-probe.js` on
//! `google-chrome --headless=new` 150.0.7871.46:
//!
//! * `Object.getOwnPropertyNames(new AbortController())` was `["__signal"]` and
//!   `(new AbortController().signal)` was `["__aborted","__reason"]`. Chrome's are both `[]` —
//!   `getOwnPropertyNames` lists non-enumerable own properties, so "non-enumerable" hid them from
//!   `Object.keys` and from nothing else.
//! * every row answered its shape default for a foreign receiver, where Chrome throws
//!   `Illegal invocation` for all eight (four rows over `{}` and over the prototype itself).
//! * `AbortSignal.timeout(1.5)` threw. `[EnforceRange] unsigned long long` TRUNCATES a fractional
//!   value; only NaN, the infinities and an out-of-range value are TypeErrors, and Chrome's message
//!   for those is "Value is outside the 'unsigned long long' value range."
//! * `AbortSignal.any()`'s three failure messages were invented. Chrome's are
//!   "1 argument required, but only 0 present.", "The provided value cannot be converted to a
//!   sequence." and "Failed to convert value to 'AbortSignal'.", all under
//!   "Failed to execute 'any' on 'AbortSignal': ".
//!
//! SCHEDULING. `AbortSignal.timeout` calls THIS REALM'S `globalThis.setTimeout` — the window's comes
//! from `lib.rs`'s `BOOTSTRAP`, the worker's from `worker-host.js`, and a sibling change was
//! rebuilding the window's over a real task source. Calling it by NAME is the seam: this file does not
//! own a clock, a queue or a callback list, and it keeps working when that unit lands.
//!
//! `onabort` is the ONE event-handler store (`kits::evt_handler`, W5.0b + W4.b's brand): the abort
//! steps dispatch an `abort` Event and the handler runs as a registered listener, exactly once and in
//! registration order with `addEventListener`'s.

use deno_core::v8;

use crate::binding::protos::{
    build_fn_data, call_method, get_prop, illegal_invocation, make_dom_exc, str_val, throw_type_error,
};
use crate::install::webidl::{brand, branded_getter_row, branded_method_row, has_brand, receiver};
use crate::install::live::method_row;
use crate::install::MemberDef;

// ── the internal slots ──────────────────────────────────────────────────────────────────────────

const ABORTED_SLOT: &str = "korvus_abort_aborted";
const REASON_SLOT: &str = "korvus_abort_reason";
const SIGNAL_SLOT: &str = "korvus_abort_signal";
/// The signal's ABORT ALGORITHMS (DOM "signal abort" step 3): engine functions run, in order, after the
/// reason is set and before the `abort` event -- how `fetch` hears an abort. A private
/// slot, so a page can neither see nor remove them.
const ALGOS_SLOT: &str = "korvus_abort_algorithms";
/// The signals that follow this one (`AbortSignal.any`, a Request's signal), aborted after it.
const DEPENDENTS_SLOT: &str = "korvus_abort_dependents";

fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn slot_set(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let key = slot_key(scope, name);
    this.set_private(scope, key, v);
}
fn slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = slot_key(scope, name);
    this.get_private(scope, key).filter(|v| !v.is_undefined())
}

fn is_aborted(scope: &mut v8::PinScope, sig: v8::Local<v8::Object>) -> bool {
    slot_get(scope, sig, ABORTED_SLOT).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// Mint a signal on THIS realm's `AbortSignal.prototype`. WebIDL declares no constructor, so this is
/// the only way one exists: `new AbortController()`, `AbortSignal.abort/timeout/any`, and `fetch`'s
/// own internal signal all come through here.
pub(crate) fn mint_signal<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    mint_signal_as(scope, "AbortSignal")
}

/// [`mint_signal`] as an instance of `iface` -- `AbortSignal`, or its subclass `TaskSignal` -- from that
/// interface's template, the realm's.
fn mint_signal_as<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, iface)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    slot_set(scope, obj, ABORTED_SLOT, f);
    brand(scope, obj, "AbortSignal");
    Some(obj)
}

/// The reason an abort with no explicit one carries: `DOMException("signal is aborted without
/// reason", "AbortError")` — Chrome's exact message, measured.
fn default_reason<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    make_dom_exc(scope, "signal is aborted without reason", "AbortError").unwrap_or_else(|| v8::undefined(scope).into())
}

/// Set a signal's aborted flag and reason WITHOUT firing — the shape `AbortSignal.abort()` and
/// `AbortSignal.any()`'s already-aborted short-circuit need, since nothing can be listening yet.
fn mark_aborted(scope: &mut v8::PinScope, sig: v8::Local<v8::Object>, reason: v8::Local<v8::Value>) {
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    slot_set(scope, sig, ABORTED_SLOT, t);
    slot_set(scope, sig, REASON_SLOT, reason);
}

/// The spec's "signal abort" steps: if already aborted, do nothing; otherwise record the reason and
/// fire ONE `abort` event, which runs `onabort` and every `addEventListener("abort")` in registration
/// order (they share one store). `reason` may be `undefined`, which means the default above.
pub(crate) fn signal_abort(scope: &mut v8::PinScope, sig: v8::Local<v8::Object>, reason: v8::Local<v8::Value>) {
    if is_aborted(scope, sig) {
        return;
    }
    let r = if reason.is_undefined() { default_reason(scope) } else { reason };
    mark_aborted(scope, sig, r);
    // DOM "signal abort" steps 3-5: every dependent not yet aborted takes the reason NOW, but runs its
    // abort steps only after this signal's -- its `abort` event fires after the source's.
    let mut dependents = Vec::new();
    if let Some(list) = slot_get(scope, sig, DEPENDENTS_SLOT).and_then(|l| v8::Local::<v8::Array>::try_from(l).ok()) {
        for i in 0..list.length() {
            let Some(d) = list.get_index(scope, i).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
            if !is_aborted(scope, d) {
                mark_aborted(scope, d, r);
                dependents.push(d);
            }
        }
    }
    abort_steps(scope, sig);
    for d in dependents {
        abort_steps(scope, d);
    }
}

/// "Run the abort steps": the signal's algorithms, then its `abort` event.
fn abort_steps(scope: &mut v8::PinScope, sig: v8::Local<v8::Object>) {
    run_abort_algorithms(scope, sig);
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let ty = str_val(scope, "abort");
    let Some(ev) = ctor.new_instance(scope, &[ty]) else { return };
    // The UA fires it: `isTrusted` is true in Chrome.
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let sig_v: v8::Local<v8::Value> = sig.into();
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, sig_v, "dispatchEvent", &[evv]);
}

/// Is `v` an AbortSignal, and an aborted one? `Some(reason)` when it is.
pub(crate) fn aborted_reason<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let sig = v8::Local::<v8::Object>::try_from(v).ok().filter(|o| has_brand(scope, (*o).into(), "AbortSignal"))?;
    if !is_aborted(scope, sig) {
        return None;
    }
    Some(slot_get(scope, sig, REASON_SLOT).unwrap_or_else(|| v8::undefined(scope).into()))
}

/// Add an abort algorithm to the AbortSignal `v` (DOM "add" an algorithm to a signal); nothing when `v`
/// is not one, or is already aborted (the caller checks that first, as the spec's callers do).
pub(crate) fn add_abort_algorithm(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, algorithm: v8::Local<v8::Function>) {
    let Some(sig) = v8::Local::<v8::Object>::try_from(v).ok().filter(|o| has_brand(scope, (*o).into(), "AbortSignal")) else { return };
    if is_aborted(scope, sig) {
        return;
    }
    let list = match slot_get(scope, sig, ALGOS_SLOT).and_then(|l| v8::Local::<v8::Array>::try_from(l).ok()) {
        Some(l) => l,
        None => {
            let l = v8::Array::new(scope, 0);
            slot_set(scope, sig, ALGOS_SLOT, l.into());
            l
        }
    };
    let n = list.length();
    list.set_index(scope, n, algorithm.into());
}

/// Make `follower` follow `source` (DOM "signal follows parentSignal"): aborted now if `source` is,
/// else aborted with `source`'s reason when it aborts -- through the signal's own algorithm list, never
/// the page-visible `addEventListener`. Fetch gives every `Request` a signal that follows `init.signal`.
pub(crate) fn follow_signal(scope: &mut v8::PinScope, follower: v8::Local<v8::Object>, source: v8::Local<v8::Value>) {
    if let Some(reason) = aborted_reason(scope, source) {
        mark_aborted(scope, follower, reason);
        return;
    }
    let Some(src) = v8::Local::<v8::Object>::try_from(source).ok().filter(|o| has_brand(scope, (*o).into(), "AbortSignal")) else { return };
    let list = match slot_get(scope, src, DEPENDENTS_SLOT).and_then(|l| v8::Local::<v8::Array>::try_from(l).ok()) {
        Some(l) => l,
        None => {
            let l = v8::Array::new(scope, 0);
            slot_set(scope, src, DEPENDENTS_SLOT, l.into());
            l
        }
    };
    let n = list.length();
    list.set_index(scope, n, follower.into());
}

/// Run and drop the signal's abort algorithms, each with the signal's reason (an algorithm that throws
/// does not stop the rest, and its exception is reported, not thrown into the aborter).
fn run_abort_algorithms(scope: &mut v8::PinScope, sig: v8::Local<v8::Object>) {
    let Some(list) = slot_get(scope, sig, ALGOS_SLOT).and_then(|l| v8::Local::<v8::Array>::try_from(l).ok()) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    slot_set(scope, sig, ALGOS_SLOT, undef);
    let reason = slot_get(scope, sig, REASON_SLOT).unwrap_or(undef);
    for i in 0..list.length() {
        let Some(f) = list.get_index(scope, i).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) else { continue };
        v8::tc_scope!(let tc, scope);
        f.call(tc, undef, &[reason]);
        if let Some(e) = tc.exception() {
            // reported where it was thrown (`report::caught_site`)
            let m = tc.message();
            let site = crate::core::events::report::caught_site(tc, e, m);
            let e = v8::Global::new(tc, e);
            tc.reset();
            let e = v8::Local::new(tc, &e);
            crate::core::events::report::report_exception_at(tc, e, site);
        }
    }
}

// ── AbortSignal's rows ──────────────────────────────────────────────────────────────────────────

fn cb_abortsignal_aborted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "AbortSignal") else { return };
    let v = is_aborted(scope, this);
    rv.set_bool(v);
}

/// `reason` is `undefined` until the signal aborts (measured: `new AbortController().signal.reason`
/// is `undefined`, not `null`).
fn cb_abortsignal_reason(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "AbortSignal") else { return };
    if let Some(v) = slot_get(scope, this, REASON_SLOT) {
        rv.set(v);
    }
}

pub(crate) fn cb_abortsignal_throw_if_aborted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "AbortSignal") else { return };
    if !is_aborted(scope, this) {
        return;
    }
    let reason = match slot_get(scope, this, REASON_SLOT) {
        Some(r) => r,
        None => default_reason(scope),
    };
    scope.throw_exception(reason);
}

// ── AbortSignal's three statics ─────────────────────────────────────────────────────────────────

/// `AbortSignal.abort(optional any reason)` — an already-aborted signal. Nothing fires: no listener
/// can exist on a signal that does not exist yet.
fn cb_as_static_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(sig) = mint_signal(scope) else { return };
    let arg = args.get(0);
    let reason = if arg.is_undefined() { default_reason(scope) } else { arg };
    mark_aborted(scope, sig, reason);
    rv.set(sig.into());
}

/// WebIDL `[EnforceRange] unsigned long long`: ToNumber, then TRUNCATE. NaN, the infinities and
/// anything outside [0, 2^64-1] are a TypeError with Chrome's message; a fractional value is fine
/// (`AbortSignal.timeout(1.5)` works in Chrome and used to throw here).
fn enforce_range_u64(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, member: &str) -> Option<f64> {
    let n = v.number_value(scope)?;
    let t = n.trunc();
    if !t.is_finite() || t < 0.0 || t > 18446744073709551615.0 {
        throw_type_error(
            scope,
            &format!("Failed to execute '{member}' on '{iface}': Value is outside the 'unsigned long long' value range."),
        );
        return None;
    }
    Some(t)
}

/// The `setTimeout` callback a `timeout()` signal is armed with: abort with a `TimeoutError`. The
/// signal rides in the callback's bound data, which is what keeps it alive until the timer fires.
fn cb_timeout_fire(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(sig) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let reason = make_dom_exc(scope, "signal timed out", "TimeoutError").unwrap_or_else(|| v8::undefined(scope).into());
    signal_abort(scope, sig, reason);
}

/// `AbortSignal.timeout(unsigned long long milliseconds)`.
///
/// THE SCHEDULING IS THE REALM'S, BY NAME: `globalThis.setTimeout`. This file owns no clock and no
/// queue, so the window's timer rebuild and the worker's own implementation
/// both reach it unchanged. The worker's was `op_sleep`-backed until a timer callback became
/// a task rather than a promise job; it is the kernel's `op_timer_*` task source now.
fn cb_as_static_timeout(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(ms) = enforce_range_u64(scope, args.get(0), "AbortSignal", "timeout") else { return };
    let Some(sig) = mint_signal(scope) else { return };
    let cb = build_fn_data(scope, cb_timeout_fire, "", 0, sig.into());
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, ms).into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[cb.into(), delay]);
    rv.set(sig.into());
}

/// The `abort` listener `any()` puts on each source: abort the composite with the SOURCE's reason,
/// which `event.target` names.
fn cb_any_forward(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(composite) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let reason = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|ev| get_prop(scope, ev, "target"))
        .and_then(|t| v8::Local::<v8::Object>::try_from(t).ok())
        .and_then(|src| slot_get(scope, src, REASON_SLOT))
        .unwrap_or_else(|| v8::undefined(scope).into());
    signal_abort(scope, composite, reason);
}

/// `AbortSignal.any(sequence<AbortSignal> signals)`.
fn cb_as_static_any(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = args.get(0);
    // `sequence<AbortSignal>`: an OBJECT with a callable @@iterator, nothing else.
    let not_a_sequence = |scope: &mut v8::PinScope| {
        throw_type_error(scope, "Failed to execute 'any' on 'AbortSignal': The provided value cannot be converted to a sequence.");
    };
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        not_a_sequence(scope);
        return;
    };
    let it_key = v8::Symbol::get_iterator(scope);
    if !obj.get(scope, it_key.into()).map(|f| f.is_function()).unwrap_or(false) {
        not_a_sequence(scope);
        return;
    }
    let Some(list) = crate::binding::protos::call_intrinsic(scope, "Array.from", &[v])
        .and_then(|x| v8::Local::<v8::Array>::try_from(x).ok())
    else {
        return;
    };
    let mut sources: Vec<v8::Local<v8::Object>> = Vec::new();
    for i in 0..list.length() {
        let Some(el) = list.get_index(scope, i) else { continue };
        if !has_brand(scope, el, "AbortSignal") {
            throw_type_error(scope, "Failed to execute 'any' on 'AbortSignal': Failed to convert value to 'AbortSignal'.");
            return;
        }
        let Ok(o) = v8::Local::<v8::Object>::try_from(el) else { continue };
        sources.push(o);
    }
    let Some(composite) = mint_signal(scope) else { return };
    // An ALREADY-aborted source wins immediately and nothing is registered: the composite is born
    // aborted with that source's reason, and no event fires (nothing can be listening).
    for src in &sources {
        if is_aborted(scope, *src) {
            let reason = slot_get(scope, *src, REASON_SLOT).unwrap_or_else(|| v8::undefined(scope).into());
            mark_aborted(scope, composite, reason);
            rv.set(composite.into());
            return;
        }
    }
    // Through each source's own algorithm list, not its page-visible `addEventListener`: a page that
    // wraps `EventTarget.prototype.addEventListener` saw every `AbortSignal.any` call it.
    for src in sources {
        follow_signal(scope, composite, src.into());
    }
    rv.set(composite.into());
}

/// The body every row of [`ABORTSIGNAL`] names. The three statics are `method_row`s on the interface
/// object (a static is not called on an instance, so it carries the ARITY guard and no brand); the
/// instance rows check the receiver themselves.
pub fn install_abort_signal(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "aborted" => branded_getter_row(scope, holder, member, cb_abortsignal_aborted),
        "reason" => branded_getter_row(scope, holder, member, cb_abortsignal_reason),
        "throwIfAborted" => branded_method_row(scope, holder, member, cb_abortsignal_throw_if_aborted),
        "abort" => method_row(scope, holder, member, cb_as_static_abort),
        "any" => method_row(scope, holder, member, cb_as_static_any),
        "timeout" => method_row(scope, holder, member, cb_as_static_timeout),
        other => debug_assert!(false, "install_abort_signal: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `AbortSignal` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: the two readonly attributes, the
// `onabort` handler, `throwIfAborted`, then the three statics on the interface object. `[interface,
// both]`: `surface.json` and `worker.surface.json` both record `newable: false` and the prototype
// chain `AbortSignal.prototype -> EventTarget.prototype`.
webapi::interface! {
    "AbortSignal" : "EventTarget" [interface, both] {
        ctor          "AbortSignal"                    len 0     = stub,
        attr          "aborted"        : Boolean readonly        = install_abort_signal,
        attr          "reason"         : Any readonly            = install_abort_signal,
        handler       "onabort"                                  = evt_handler,
        method        "throwIfAborted" : Undefined len 0         = install_abort_signal,
        static_method "abort"          : Object len 0            = install_abort_signal,
        static_method "any"            : Object len 1 required 1 = install_abort_signal,
        static_method "timeout"        : Object len 1 required 1 = install_abort_signal
    }
}

// ── AbortController ─────────────────────────────────────────────────────────────────────────────

/// `new AbortController()` — `length` 0, and its `signal` is `[SameObject]`, so the one signal is
/// minted here and read back from the slot for the life of the controller.
pub(crate) fn cb_abort_controller_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !crate::install::webidl::require_new(scope, &args, "AbortController") {
        return;
    }
    let Some(sig) = mint_signal(scope) else { return };
    let this = args.this();
    slot_set(scope, this, SIGNAL_SLOT, sig.into());
    brand(scope, this, "AbortController");
    rv.set(this.into());
}

pub(crate) fn cb_abort_controller_signal(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "AbortController") else { return };
    if let Some(sig) = slot_get(scope, this, SIGNAL_SLOT) {
        rv.set(sig);
    }
}

pub(crate) fn cb_abort_controller_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "AbortController") else { return };
    let Some(sig) = slot_get(scope, this, SIGNAL_SLOT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        illegal_invocation(scope);
        return;
    };
    signal_abort(scope, sig, args.get(0));
}

/// The body both rows of [`ABORTCONTROLLER`] name.
pub fn install_abort_controller(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "signal" => branded_getter_row(scope, proto, member, cb_abort_controller_signal),
        "abort" => branded_method_row(scope, proto, member, cb_abort_controller_abort),
        other => debug_assert!(false, "install_abort_controller: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `AbortController` block of `install/generated_registry.rs` (identical to the
// worker registry's): the `[SameObject]` readonly `signal`, then `abort`. `[value, both]`: both
// captures record `newable: true`.
webapi::interface! {
    "AbortController" [value, both] {
        ctor   "AbortController"           len 0 = stub,
        attr   "signal" : Object readonly        = install_abort_controller,
        method "abort"  : Undefined len 0        = install_abort_controller
    }
}

// ── TaskController / TaskSignal ─────────────────────────────────────────────────────
//
// `new TaskController()` was a registry shell: construction worked, then `signal`, `setPriority` and
// `abort` threw "Illegal invocation" on the instance (airbnb's homepage constructs one). A
// TaskController IS an AbortController and its signal IS an AbortSignal (WebIDL inheritance), so
// both carry the parent's brand and state and every inherited member works on them; the priority
// is one more private on the signal.

const PRIORITY_SLOT: &str = "korvus_task_priority";
const PRIORITIES: [&str; 3] = ["user-blocking", "user-visible", "background"];

fn priority_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, context: &str) -> Option<String> {
    let s = v.to_rust_string_lossy(scope);
    if PRIORITIES.contains(&s.as_str()) {
        return Some(s);
    }
    throw_type_error(scope, &format!("{context}The provided value '{s}' is not a valid enum value of type TaskPriority."));
    None
}

/// A TaskSignal: an AbortSignal re-pointed at `TaskSignal.prototype`, with a priority.
fn mint_task_signal<'s>(scope: &mut v8::PinScope<'s, '_>, priority: &str) -> Option<v8::Local<'s, v8::Object>> {
    // the REALM's TaskSignal, from its template (measured: F-engine-mints-realm-protos):
    // it was an AbortSignal re-parented onto `window.TaskSignal.prototype`, a name the page can write
    let sig = mint_signal_as(scope, "TaskSignal")?;
    brand(scope, sig, "TaskSignal");
    let p = str_val(scope, priority);
    slot_set(scope, sig, PRIORITY_SLOT, p);
    Some(sig)
}

/// `new TaskController(optional TaskControllerInit init = {})`.
pub(crate) fn cb_task_controller_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !crate::install::webidl::require_new(scope, &args, "TaskController") {
        return;
    }
    let init = args.get(0);
    let mut priority = "user-visible".to_string();
    if !init.is_null_or_undefined() {
        let Ok(o) = v8::Local::<v8::Object>::try_from(init) else {
            throw_type_error(scope, "Failed to construct 'TaskController': The provided value is not of type 'TaskControllerInit'.");
            return;
        };
        if let Some(v) = get_prop(scope, o, "priority").filter(|v| !v.is_undefined()) {
            let ctx = "Failed to construct 'TaskController': Failed to read the 'priority' property from 'TaskControllerInit': ";
            let Some(p) = priority_arg(scope, v, ctx) else { return };
            priority = p;
        }
    }
    let Some(sig) = mint_task_signal(scope, &priority) else { return };
    let this = args.this();
    slot_set(scope, this, SIGNAL_SLOT, sig.into());
    brand(scope, this, "AbortController");
    brand(scope, this, "TaskController");
    rv.set(this.into());
}

/// `controller.setPriority(priority)`: a no-op for the same priority; otherwise the signal's priority
/// changes and `prioritychange` fires at it.
fn cb_task_controller_set_priority(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "TaskController") else { return };
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'setPriority' on 'TaskController': 1 argument required, but only 0 present.");
        return;
    }
    let ctx = "Failed to execute 'setPriority' on 'TaskController': ";
    let Some(p) = priority_arg(scope, args.get(0), ctx) else { return };
    let Some(sig) = slot_get(scope, this, SIGNAL_SLOT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let prev = slot_get(scope, sig, PRIORITY_SLOT).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    if prev == p {
        return;
    }
    let pv = str_val(scope, &p);
    slot_set(scope, sig, PRIORITY_SLOT, pv);
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let ty = str_val(scope, "prioritychange");
    let Some(ev) = ctor.new_instance(scope, &[ty]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let prev_v = str_val(scope, &prev);
    crate::binding::protos::set_prop(scope, ev, "previousPriority", prev_v);
    let sig_v: v8::Local<v8::Value> = sig.into();
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, sig_v, "dispatchEvent", &[evv]);
}

fn cb_task_signal_priority(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this().into(), "TaskSignal") else { return };
    let p = slot_get(scope, this, PRIORITY_SLOT).unwrap_or_else(|| str_val(scope, "user-visible"));
    rv.set(p);
}

/// `TaskSignal.any(signals, init)`: `AbortSignal.any` whose result is a TaskSignal with `init`'s
/// priority (a fixed priority; following another signal's priority is not modelled).
fn cb_task_signal_any(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let init = args.get(1);
    let mut priority = "user-visible".to_string();
    if let Ok(o) = v8::Local::<v8::Object>::try_from(init) {
        if let Some(v) = get_prop(scope, o, "priority").filter(|v| v.is_string()) {
            let ctx = "Failed to execute 'any' on 'TaskSignal': Failed to read the 'priority' property from 'TaskSignalAnyInit': ";
            let Some(p) = priority_arg(scope, v, ctx) else { return };
            priority = p;
        }
    }
    let Some(sig) = mint_task_signal(scope, &priority) else { return };
    let Ok(list) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_type_error(scope, "Failed to execute 'any' on 'TaskSignal': The provided value cannot be converted to a sequence.");
        return;
    };
    let Some(arr) = crate::binding::protos::call_intrinsic(scope, "Array.from", &[list.into()])
        .and_then(|x| v8::Local::<v8::Array>::try_from(x).ok())
    else {
        return;
    };
    let cb = build_fn_data(scope, cb_any_forward, "", 1, sig.into());
    let ty = str_val(scope, "abort");
    for i in 0..arr.length() {
        let Some(el) = arr.get_index(scope, i) else { continue };
        let Ok(src) = v8::Local::<v8::Object>::try_from(el) else { continue };
        if is_aborted(scope, src) {
            let reason = slot_get(scope, src, REASON_SLOT).unwrap_or_else(|| v8::undefined(scope).into());
            mark_aborted(scope, sig, reason);
            rv.set(sig.into());
            return;
        }
        let sv: v8::Local<v8::Value> = src.into();
        call_method(scope, sv, "addEventListener", &[ty, cb.into()]);
    }
    rv.set(sig.into());
}

pub fn install_task_signal(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "priority" => branded_getter_row(scope, holder, member, cb_task_signal_priority),
        "any" => method_row(scope, holder, member, cb_task_signal_any),
        other => debug_assert!(false, "install_task_signal: unexpected member {other}"),
    }
}

pub fn install_task_controller(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "setPriority" => branded_method_row(scope, proto, member, cb_task_controller_set_priority),
        other => debug_assert!(false, "install_task_controller: unexpected member {other}"),
    }
}

webapi::interface! {
    "TaskSignal" : "AbortSignal" [interface, both] {
        ctor          "TaskSignal"                        len 0     = stub,
        attr          "priority"         : Any readonly             = install_task_signal,
        handler       "onprioritychange"                            = evt_handler,
        static_method "any"              : Object len 1 required 1  = install_task_signal
    }
}

webapi::interface! {
    "TaskController" : "AbortController" [value, both] {
        ctor   "TaskController"                  len 0            = stub,
        method "setPriority" : Undefined len 1 required 1         = install_task_controller
    }
}

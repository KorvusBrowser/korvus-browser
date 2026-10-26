//! `core/events/observable_ops.rs` -- the Observable operators and `Observable.from`
//! (the Chrome oracle ratchet), as the WICG Observable spec and Blink's
//! `core/dom/observable.cc` (Chromium 150) define them.
//!
//! They were honest D2 stubs -- `map` answered null, `toArray` an already-resolved promise -- so
//! `observable.map(f).subscribe(...)` threw where Chrome delivers. Every operator here is an Observable
//! whose subscribe callback is NATIVE ([`super::observable::new_native_observable`]): subscribing to it
//! subscribes to the source with an internal observer whose `next` / `error` / `complete` are this
//! file's, forwarding to the downstream subscriber, and passing the downstream subscriber's signal on so
//! an unsubscribe unwinds upstream (spec: "subscribe to sourceObservable given sourceObserver and
//! options", options' signal = subscriber's signal).
//!
//! The Promise-returning operators (`toArray`, `forEach`, `first`, `last`, `some`, `every`, `find`,
//! `reduce`) subscribe with an internal signal that FOLLOWS `options.signal` and is aborted when the
//! promise settles early; an `options.signal` that aborts rejects with its reason.

use deno_core::v8;

use super::observable::{new_native_observable, sub_complete, sub_error, sub_next, sub_signal, subscribe_with, add_teardown, is_observable, sub_active};
use crate::binding::protos::{get_prop, str_val, throw_type_error, throw_range_error};
use crate::core::dom::abort_controller::{aborted_reason, add_abort_algorithm, follow_signal, mint_signal, signal_abort};

// ── small helpers ────────────────────────────────────────────────────────────────────────────────

fn arr<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[v8::Local<'s, v8::Value>]) -> v8::Local<'s, v8::Array> {
    v8::Array::new_with_elements(scope, items)
}

fn at<'s>(scope: &mut v8::PinScope<'s, '_>, a: v8::Local<v8::Array>, i: u32) -> v8::Local<'s, v8::Value> {
    a.get_index(scope, i).unwrap_or_else(|| v8::undefined(scope).into())
}

fn data_arr<'s>(_scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<v8::Local<'s, v8::Array>> {
    v8::Local::<v8::Array>::try_from(args.data()).ok()
}

fn func<'s>(v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Function>> {
    v8::Local::<v8::Function>::try_from(v).ok()
}

fn obj<'s>(v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// A native function closing over `data`.
fn native<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, data: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Function> {
    v8::Function::builder(crate::binding::refs::f(cb)).data(data).build(scope).expect("native function")
}

/// Call `f(args)`; `Err(exception)` when it threw.
fn try_call<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, args: &[v8::Local<'s, v8::Value>]) -> Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>> {
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r = {
        v8::tc_scope!(let tc, scope);
        match f.call(tc, undef, args) {
            Some(v) => Ok(v8::Global::new(tc, v)),
            None => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match r {
        Ok(g) => Ok(v8::Local::new(scope, g)),
        Err(Some(g)) => Err(v8::Local::new(scope, g)),
        Err(None) => Err(v8::undefined(scope).into()),
    }
}

/// The resolver a data array holds as its promise (V8's resolver IS the promise object).
fn as_resolver<'s>(v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    // SAFETY: the slot was written with a `v8::PromiseResolver` by `promise_op` and by nothing else.
    v.is_promise().then(|| unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(v) })
}

/// A mutable counter / flag kept on a plain state object.
fn state_num(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, k: &str) -> f64 {
    get_prop(scope, st, k).and_then(|v| v.number_value(scope)).filter(|n| n.is_finite()).unwrap_or(0.0)
}
fn state_set(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, k: &str, v: v8::Local<v8::Value>) {
    let key = str_val(scope, k);
    st.set(scope, key, v);
}
fn state_set_num(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, k: &str, n: f64) {
    let v = v8::Number::new(scope, n).into();
    state_set(scope, st, k, v);
}
fn state_get<'s>(scope: &mut v8::PinScope<'s, '_>, st: v8::Local<v8::Object>, k: &str) -> v8::Local<'s, v8::Value> {
    get_prop(scope, st, k).unwrap_or_else(|| v8::undefined(scope).into())
}
fn new_state<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let null = v8::null(scope).into();
    v8::Object::with_prototype_and_properties(scope, null, &[], &[])
}

/// An observer object `{next, error, complete}` of native callbacks sharing `data`.
fn observer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    data: v8::Local<'s, v8::Value>,
    next: impl v8::MapFnTo<v8::FunctionCallback>,
    error: impl v8::MapFnTo<v8::FunctionCallback>,
    complete: impl v8::MapFnTo<v8::FunctionCallback>,
) -> v8::Local<'s, v8::Object> {
    let o = new_state(scope);
    let n = native(scope, next, data);
    let e = native(scope, error, data);
    let c = native(scope, complete, data);
    state_set(scope, o, "next", n.into());
    state_set(scope, o, "error", e.into());
    state_set(scope, o, "complete", c.into());
    o
}

/// WebIDL `unsigned long long` (modular): `take(-1)` takes 2^64 - 1.
fn to_u64(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<f64> {
    let n = v.number_value(scope)?;
    if !n.is_finite() {
        return Some(0.0);
    }
    let m = 18446744073709551616.0_f64;
    let t = n.trunc() % m;
    Some(if t < 0.0 { t + m } else { t })
}

fn require_fn(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, method: &str) -> bool {
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{method}' on 'Observable': 1 argument required, but only 0 present."));
        return false;
    }
    if !args.get(0).is_function() {
        throw_type_error(scope, &format!("Failed to execute '{method}' on 'Observable': parameter 1 is not of type 'Function'."));
        return false;
    }
    true
}

fn require_this(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if is_observable(scope, this) {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

// ── the forwarding operators ─────────────────────────────────────────────────────────────────────
//
// Each is `new_native_observable(start)` with `start`'s data `[kind, source, arg]`. Subscribing runs
// `cb_op_start(subscriber)`, which subscribes to `source` with an internal observer whose data is
// `[kind, subscriber, arg, state]`.

fn op_observable<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str, source: v8::Local<'s, v8::Object>, arg: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let k = str_val(scope, kind);
    let d = arr(scope, &[k, source.into(), arg]);
    let start = native(scope, cb_op_start, d.into());
    new_native_observable(scope, start)
}

fn cb_op_start<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let kind = at(scope, d, 0).to_rust_string_lossy(scope);
    let Some(source) = obj(at(scope, d, 1)) else { return };
    let arg = at(scope, d, 2);
    let Some(sub) = obj(args.get(0)) else { return };
    let st = new_state(scope);
    let k = str_val(scope, &kind);
    let od = arr(scope, &[k, sub.into(), arg, st.into()]);
    match kind.as_str() {
        "take" => {
            let n = arg.number_value(scope).unwrap_or(0.0);
            if n == 0.0 {
                sub_complete(scope, sub);
                return;
            }
            state_set_num(scope, st, "left", n);
        }
        "drop" => {
            let n = arg.number_value(scope).unwrap_or(0.0);
            state_set_num(scope, st, "left", n);
        }
        "finally" => {
            // The callback runs when the downstream subscription closes, however it closes.
            if let Some(f) = func(arg) {
                let fd = arr(scope, &[f.into()]);
                let td = native(scope, cb_finally_teardown, fd.into());
                add_teardown(scope, sub, td);
            }
        }
        "inspect" => {
            // A `subscribe` callback runs before the source is subscribed to; an `abort` one when the
            // downstream signal aborts while the source is still live.
            if let Some(o) = obj(arg).filter(|_| !arg.is_function()) {
                if let Some(f) = get_prop(scope, o, "subscribe").and_then(func) {
                    if let Err(e) = try_call(scope, f, &[]) {
                        sub_error(scope, sub, e);
                        return;
                    }
                }
                if let Some(f) = get_prop(scope, o, "abort").and_then(func) {
                    let fd = arr(scope, &[f.into(), st.into()]);
                    let alg = native(scope, cb_inspect_abort, fd.into());
                    if let Some(sig) = sub_signal(scope, sub) {
                        add_abort_algorithm(scope, sig.into(), alg);
                    }
                }
            }
        }
        "takeUntil" => {
            // The notifier first: its first value (or error) completes the downstream subscription.
            let nd = arr(scope, &[sub.into()]);
            let nobs = observer(scope, nd.into(), cb_until_hit, cb_until_hit, cb_noop);
            if let Some(n) = obj(arg) {
                let sig = sub_signal(scope, sub);
                subscribe_with(scope, n, nobs.into(), sig);
            }
            if !sub_active(scope, sub) {
                return;
            }
        }
        "flatMap" | "switchMap" => {
            let q = v8::Array::new(scope, 0);
            state_set(scope, st, "queue", q.into());
        }
        _ => {}
    }
    let o = observer(scope, od.into(), cb_op_next, cb_op_error, cb_op_complete);
    let sig = sub_signal(scope, sub);
    subscribe_with(scope, source, o.into(), sig);
}

fn cb_noop(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

fn cb_until_hit<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(sub) = obj(at(scope, d, 0)) {
        sub_complete(scope, sub);
    }
}

fn cb_finally_teardown<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(f) = func(at(scope, d, 0)) {
        if let Err(e) = try_call(scope, f, &[]) {
            crate::core::events::report::report_exception(scope, e);
        }
    }
}

fn cb_inspect_abort<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let Some(st) = obj(at(scope, d, 1)) else { return };
    if state_get(scope, st, "closed").is_true() {
        return;
    }
    if let Some(f) = func(at(scope, d, 0)) {
        let reason = v8::undefined(scope).into();
        if let Err(e) = try_call(scope, f, &[reason]) {
            crate::core::events::report::report_exception(scope, e);
        }
    }
}

/// The downstream subscriber and the operator's pieces from an internal observer's data.
fn op_parts<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<(String, v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>)> {
    let d = data_arr(scope, args)?;
    let kind = at(scope, d, 0).to_rust_string_lossy(scope);
    let sub = obj(at(scope, d, 1))?;
    let arg = at(scope, d, 2);
    let st = obj(at(scope, d, 3))?;
    Some((kind, sub, arg, st))
}

fn cb_op_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((kind, sub, arg, st)) = op_parts(scope, &args) else { return };
    let v = args.get(0);
    let idx = state_num(scope, st, "i");
    state_set_num(scope, st, "i", idx + 1.0);
    let index: v8::Local<v8::Value> = v8::Number::new(scope, idx).into();
    match kind.as_str() {
        "map" => {
            let Some(f) = func(arg) else { return };
            match try_call(scope, f, &[v, index]) {
                Ok(r) => sub_next(scope, sub, r),
                Err(e) => sub_error(scope, sub, e),
            }
        }
        "filter" => {
            let Some(f) = func(arg) else { return };
            match try_call(scope, f, &[v, index]) {
                Ok(r) => {
                    if r.boolean_value(scope) {
                        sub_next(scope, sub, v);
                    }
                }
                Err(e) => sub_error(scope, sub, e),
            }
        }
        "take" => {
            let left = state_num(scope, st, "left");
            if left > 0.0 {
                sub_next(scope, sub, v);
                state_set_num(scope, st, "left", left - 1.0);
                if left - 1.0 <= 0.0 {
                    sub_complete(scope, sub);
                }
            }
        }
        "drop" => {
            let left = state_num(scope, st, "left");
            if left > 0.0 {
                state_set_num(scope, st, "left", left - 1.0);
            } else {
                sub_next(scope, sub, v);
            }
        }
        "inspect" => {
            let cb = if arg.is_function() { func(arg) } else { obj(arg).and_then(|o| get_prop(scope, o, "next")).and_then(func) };
            if let Some(f) = cb {
                if let Err(e) = try_call(scope, f, &[v]) {
                    sub_error(scope, sub, e);
                    return;
                }
            }
            sub_next(scope, sub, v);
        }
        "flatMap" => {
            let q = state_get(scope, st, "queue");
            if state_get(scope, st, "inner").is_true() {
                if let Ok(q) = v8::Local::<v8::Array>::try_from(q) {
                    let n = q.length();
                    q.set_index(scope, n, v);
                }
            } else {
                flat_inner(scope, sub, arg, st, v, index);
            }
        }
        "switchMap" => {
            // A new value aborts the inner subscription in flight and starts the next one.
            if let Some(prev) = obj(state_get(scope, st, "innerSignal")) {
                let r = v8::undefined(scope).into();
                signal_abort(scope, prev, r);
            }
            flat_inner(scope, sub, arg, st, v, index);
        }
        _ => sub_next(scope, sub, v),
    }
}

fn cb_op_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((kind, sub, arg, st)) = op_parts(scope, &args) else { return };
    let e = args.get(0);
    state_set(scope, st, "closed", v8::Boolean::new(scope, true).into());
    match kind.as_str() {
        "inspect" => {
            if let Some(f) = obj(arg).filter(|_| !arg.is_function()).and_then(|o| get_prop(scope, o, "error")).and_then(func) {
                if let Err(x) = try_call(scope, f, &[e]) {
                    sub_error(scope, sub, x);
                    return;
                }
            }
            sub_error(scope, sub, e);
        }
        "catch" => {
            // The callback's result, as an Observable, takes over the subscription.
            let Some(f) = func(arg) else { return };
            let r = match try_call(scope, f, &[e]) {
                Ok(r) => r,
                Err(x) => {
                    sub_error(scope, sub, x);
                    return;
                }
            };
            let inner = match from_value(scope, r) {
                Ok(o) => o,
                Err(x) => {
                    sub_error(scope, sub, x);
                    return;
                }
            };
            let fd = arr(scope, &[sub.into()]);
            let o = observer(scope, fd.into(), cb_fwd_next, cb_fwd_error, cb_fwd_complete);
            let sig = sub_signal(scope, sub);
            subscribe_with(scope, inner, o.into(), sig);
        }
        _ => sub_error(scope, sub, e),
    }
}

fn cb_op_complete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((kind, sub, arg, st)) = op_parts(scope, &args) else { return };
    state_set(scope, st, "closed", v8::Boolean::new(scope, true).into());
    match kind.as_str() {
        "inspect" => {
            if let Some(f) = obj(arg).filter(|_| !arg.is_function()).and_then(|o| get_prop(scope, o, "complete")).and_then(func) {
                if let Err(x) = try_call(scope, f, &[]) {
                    sub_error(scope, sub, x);
                    return;
                }
            }
            sub_complete(scope, sub);
        }
        "flatMap" | "switchMap" => {
            state_set(scope, st, "outerDone", v8::Boolean::new(scope, true).into());
            if !state_get(scope, st, "inner").is_true() {
                sub_complete(scope, sub);
            }
        }
        _ => sub_complete(scope, sub),
    }
}

/// Forward everything to the downstream subscriber (`catch`'s replacement).
fn cb_fwd_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(sub) = obj(at(scope, d, 0)) {
        sub_next(scope, sub, args.get(0));
    }
}
fn cb_fwd_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(sub) = obj(at(scope, d, 0)) {
        sub_error(scope, sub, args.get(0));
    }
}
fn cb_fwd_complete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(sub) = obj(at(scope, d, 0)) {
        sub_complete(scope, sub);
    }
}

/// `flatMap` / `switchMap`: map `v` to an Observable and subscribe to it as the inner subscription.
/// Its values go downstream; its completion runs the next queued value (`flatMap`) or, once the outer
/// one has completed too, completes downstream.
fn flat_inner<'s>(scope: &mut v8::PinScope<'s, '_>, sub: v8::Local<'s, v8::Object>, mapper: v8::Local<'s, v8::Value>, st: v8::Local<'s, v8::Object>, v: v8::Local<'s, v8::Value>, index: v8::Local<'s, v8::Value>) {
    let Some(f) = func(mapper) else { return };
    let r = match try_call(scope, f, &[v, index]) {
        Ok(r) => r,
        Err(e) => {
            sub_error(scope, sub, e);
            return;
        }
    };
    let inner = match from_value(scope, r) {
        Ok(o) => o,
        Err(e) => {
            sub_error(scope, sub, e);
            return;
        }
    };
    state_set(scope, st, "inner", v8::Boolean::new(scope, true).into());
    // The inner subscription's signal: aborted by the downstream signal, and by a switch.
    let Some(isig) = mint_signal(scope) else { return };
    if let Some(dsig) = sub_signal(scope, sub) {
        follow_signal(scope, isig, dsig.into());
    }
    state_set(scope, st, "innerSignal", isig.into());
    let id = arr(scope, &[sub.into(), mapper, st.into(), isig.into()]);
    let o = observer(scope, id.into(), cb_inner_next, cb_inner_error, cb_inner_complete);
    subscribe_with(scope, inner, o.into(), Some(isig));
}

fn inner_parts<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
    let d = data_arr(scope, args)?;
    let sub = obj(at(scope, d, 0))?;
    let mapper = at(scope, d, 1);
    let st = obj(at(scope, d, 2))?;
    let sig = obj(at(scope, d, 3))?;
    Some((sub, mapper, st, sig))
}

fn cb_inner_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((sub, _m, _st, _sig)) = inner_parts(scope, &args) else { return };
    sub_next(scope, sub, args.get(0));
}
fn cb_inner_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((sub, _m, _st, _sig)) = inner_parts(scope, &args) else { return };
    sub_error(scope, sub, args.get(0));
}
fn cb_inner_complete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((sub, mapper, st, sig)) = inner_parts(scope, &args) else { return };
    // A switched-away inner subscription's completion is not the current one's.
    if let Some(cur) = obj(state_get(scope, st, "innerSignal")) {
        if !cur.strict_equals(sig.into()) {
            return;
        }
    }
    state_set(scope, st, "inner", v8::Boolean::new(scope, false).into());
    let q = v8::Local::<v8::Array>::try_from(state_get(scope, st, "queue")).ok();
    if let Some(q) = q.filter(|q| q.length() > 0) {
        // `flatMap`'s queue: the oldest waiting value is mapped next.
        let first = at(scope, q, 0);
        let n = q.length();
        let rest: Vec<v8::Local<v8::Value>> = (1..n).map(|i| at(scope, q, i)).collect();
        let nq = arr(scope, &rest);
        state_set(scope, st, "queue", nq.into());
        let idx = state_num(scope, st, "i");
        let index = v8::Number::new(scope, idx - (n as f64 - 1.0) - 1.0).into();
        flat_inner(scope, sub, mapper, st, first, index);
        return;
    }
    if state_get(scope, st, "outerDone").is_true() {
        sub_complete(scope, sub);
    }
}

// ── Observable.from ───────────────────────────────────────────────────────────────────────────────

/// "Convert to an Observable" (WICG; Blink's `Observable::from`): an Observable as is; else an object's
/// `@@asyncIterator`, then its `@@iterator`; else a Promise; else TypeError.
pub(crate) fn from_value<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Result<v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>> {
    if let Some(o) = obj(v) {
        if is_observable(scope, o) {
            return Ok(o);
        }
        for (sym, kind) in [(v8::Symbol::get_async_iterator(scope), "async"), (v8::Symbol::get_iterator(scope), "sync")] {
            let m = {
                v8::tc_scope!(let tc, scope);
                match o.get(tc, sym.into()) {
                    Some(m) => Ok(v8::Global::new(tc, m)),
                    None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
                }
            };
            let m = match m {
                Ok(g) => v8::Local::new(scope, g),
                Err(e) => return Err(e.map(|g| v8::Local::new(scope, g)).unwrap_or_else(|| v8::undefined(scope).into())),
            };
            if m.is_null_or_undefined() {
                continue;
            }
            if !m.is_function() {
                let msg = if kind == "async" { "@@asyncIterator must be a callable." } else { "@@iterator must be a callable." };
                return Err(type_error(scope, msg));
            }
            let k = str_val(scope, kind);
            let d = arr(scope, &[k, v]);
            let start = native(scope, cb_from_start, d.into());
            return new_native_observable(scope, start).ok_or_else(|| v8::undefined(scope).into());
        }
    }
    if v.is_promise() {
        let k = str_val(scope, "promise");
        let d = arr(scope, &[k, v]);
        let start = native(scope, cb_from_start, d.into());
        return new_native_observable(scope, start).ok_or_else(|| v8::undefined(scope).into());
    }
    Err(type_error(scope, "Failed to execute 'from' on 'Observable': Cannot convert value to an Observable. Input value must be an Observable, async iterable, iterable, or Promise."))
}

/// Blink's context applied after the error is made (`deno_core::error::native_error`).
fn type_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str) -> v8::Local<'s, v8::Value> {
    deno_core::error::native_error(scope, "TypeError", msg)
}

fn cb_from_start<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let kind = at(scope, d, 0).to_rust_string_lossy(scope);
    let value = at(scope, d, 1);
    let Some(sub) = obj(args.get(0)) else { return };
    match kind.as_str() {
        "sync" => {
            // "From iterable": the iterator is re-acquired per subscription; each value is pushed until
            // the iterator is done or the subscriber closes, which RETURNS the iterator.
            let Some(o) = obj(value) else { return };
            let sym = v8::Symbol::get_iterator(scope);
            let it = match get_prop_sym(scope, o, sym).and_then(|m| func(m).map(|f| try_call_this(scope, f, o.into(), &[]).map_err(Some))).unwrap_or(Err(None)) {
                Ok(it) => it,
                Err(e) => {
                    let e = e.unwrap_or_else(|| v8::undefined(scope).into());
                    sub_error(scope, sub, e);
                    return;
                }
            };
            let Some(it) = obj(it) else { return };
            let Some(next) = get_prop(scope, it, "next").and_then(func) else { return };
            loop {
                if !sub_active(scope, sub) {
                    if let Some(ret) = get_prop(scope, it, "return").and_then(func) {
                        let _ = try_call_this(scope, ret, it.into(), &[]);
                    }
                    return;
                }
                let r = match try_call_this(scope, next, it.into(), &[]) {
                    Ok(r) => r,
                    Err(e) => {
                        sub_error(scope, sub, e);
                        return;
                    }
                };
                let Some(r) = obj(r) else { return };
                if get_prop(scope, r, "done").is_some_and(|d| d.boolean_value(scope)) {
                    sub_complete(scope, sub);
                    return;
                }
                let v = get_prop(scope, r, "value").unwrap_or_else(|| v8::undefined(scope).into());
                sub_next(scope, sub, v);
            }
        }
        "promise" => {
            let Ok(p) = v8::Local::<v8::Promise>::try_from(value) else { return };
            let d2 = arr(scope, &[sub.into()]);
            let ok = native(scope, cb_from_promise_ok, d2.into());
            let err = native(scope, cb_fwd_error, d2.into());
            let _ = p.then2(scope, ok, err);
        }
        _ => {
            // "From async iterable": `next()` is awaited before the next one is asked for.
            let Some(o) = obj(value) else { return };
            let sym = v8::Symbol::get_async_iterator(scope);
            let it = match get_prop_sym(scope, o, sym).and_then(|m| func(m).map(|f| try_call_this(scope, f, o.into(), &[]).map_err(Some))).unwrap_or(Err(None)) {
                Ok(it) => it,
                Err(e) => {
                    let e = e.unwrap_or_else(|| v8::undefined(scope).into());
                    sub_error(scope, sub, e);
                    return;
                }
            };
            let Some(it) = obj(it) else { return };
            async_step(scope, sub, it);
        }
    }
}

fn get_prop_sym<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, sym: v8::Local<v8::Symbol>) -> Option<v8::Local<'s, v8::Value>> {
    o.get(scope, sym.into())
}

fn try_call_this<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, this: v8::Local<'s, v8::Value>, args: &[v8::Local<'s, v8::Value>]) -> Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>> {
    let r = {
        v8::tc_scope!(let tc, scope);
        match f.call(tc, this, args) {
            Some(v) => Ok(v8::Global::new(tc, v)),
            None => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match r {
        Ok(g) => Ok(v8::Local::new(scope, g)),
        Err(Some(g)) => Err(v8::Local::new(scope, g)),
        Err(None) => Err(v8::undefined(scope).into()),
    }
}

fn cb_from_promise_ok<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let Some(sub) = obj(at(scope, d, 0)) else { return };
    sub_next(scope, sub, args.get(0));
    sub_complete(scope, sub);
}

fn async_step<'s>(scope: &mut v8::PinScope<'s, '_>, sub: v8::Local<'s, v8::Object>, it: v8::Local<'s, v8::Object>) {
    if !sub_active(scope, sub) {
        if let Some(ret) = get_prop(scope, it, "return").and_then(func) {
            let _ = try_call_this(scope, ret, it.into(), &[]);
        }
        return;
    }
    let Some(next) = get_prop(scope, it, "next").and_then(func) else { return };
    let r = match try_call_this(scope, next, it.into(), &[]) {
        Ok(r) => r,
        Err(e) => {
            sub_error(scope, sub, e);
            return;
        }
    };
    let Some(res) = v8::PromiseResolver::new(scope) else { return };
    res.resolve(scope, r);
    let p = res.get_promise(scope);
    let d = arr(scope, &[sub.into(), it.into()]);
    let ok = native(scope, cb_async_result, d.into());
    let err = native(scope, cb_async_error, d.into());
    let _ = p.then2(scope, ok, err);
}

fn cb_async_result<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let Some(sub) = obj(at(scope, d, 0)) else { return };
    let Some(it) = obj(at(scope, d, 1)) else { return };
    let Some(r) = obj(args.get(0)) else {
        let e = type_error(scope, "Iterator result is not an object.");
        sub_error(scope, sub, e);
        return;
    };
    if get_prop(scope, r, "done").is_some_and(|x| x.boolean_value(scope)) {
        sub_complete(scope, sub);
        return;
    }
    let v = get_prop(scope, r, "value").unwrap_or_else(|| v8::undefined(scope).into());
    sub_next(scope, sub, v);
    async_step(scope, sub, it);
}

fn cb_async_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    if let Some(sub) = obj(at(scope, d, 0)) {
        sub_error(scope, sub, args.get(0));
    }
}

// ── the Promise-returning operators ───────────────────────────────────────────────────────────────

/// Subscribe to `source` for a Promise operator: `data = [kind, resolver, arg, state, signal]`. The
/// internal signal follows `options.signal`; an already-aborted one rejects at once.
fn promise_op<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str, source: v8::Local<'s, v8::Object>, arg: v8::Local<'s, v8::Value>, options: v8::Local<'s, v8::Value>, init: Option<v8::Local<'s, v8::Value>>) -> Option<v8::Local<'s, v8::Promise>> {
    let res = v8::PromiseResolver::new(scope)?;
    let promise = res.get_promise(scope);
    let outer = obj(options).and_then(|o| get_prop(scope, o, "signal")).filter(|s| s.is_object());
    if let Some(s) = outer {
        if let Some(r) = aborted_reason(scope, s) {
            res.reject(scope, r);
            return Some(promise);
        }
    }
    let sig = mint_signal(scope)?;
    if let Some(s) = outer {
        follow_signal(scope, sig, s);
    }
    let st = new_state(scope);
    if let Some(i) = init {
        state_set(scope, st, "acc", i);
        state_set(scope, st, "hasAcc", v8::Boolean::new(scope, true).into());
    }
    let items = v8::Array::new(scope, 0);
    state_set(scope, st, "items", items.into());
    let k = str_val(scope, kind);
    let rp: v8::Local<v8::Value> = promise.into();
    let d = arr(scope, &[k, rp, arg, st.into(), sig.into()]);
    // The outer signal aborting rejects with its reason.
    let ad = arr(scope, &[rp, sig.into(), st.into()]);
    let alg = native(scope, cb_promise_abort, ad.into());
    add_abort_algorithm(scope, sig.into(), alg);
    let o = observer(scope, d.into(), cb_p_next, cb_p_error, cb_p_complete);
    subscribe_with(scope, source, o.into(), Some(sig));
    Some(promise)
}

fn cb_promise_abort<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(d) = data_arr(scope, &args) else { return };
    let Some(res) = as_resolver(at(scope, d, 0)) else { return };
    let Some(st) = obj(at(scope, d, 2)) else { return };
    if state_get(scope, st, "settled").is_true() {
        return;
    }
    let sig = at(scope, d, 1);
    let r = aborted_reason(scope, sig).unwrap_or_else(|| v8::undefined(scope).into());
    state_set(scope, st, "settled", v8::Boolean::new(scope, true).into());
    res.reject(scope, r);
}

fn p_parts<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<(String, v8::Local<'s, v8::PromiseResolver>, v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
    let d = data_arr(scope, args)?;
    let kind = at(scope, d, 0).to_rust_string_lossy(scope);
    let res = as_resolver(at(scope, d, 1))?;
    let arg = at(scope, d, 2);
    let st = obj(at(scope, d, 3))?;
    let sig = obj(at(scope, d, 4))?;
    Some((kind, res, arg, st, sig))
}

/// Settle the promise and stop the source (the internal signal aborts; it is not the page's).
fn settle<'s>(scope: &mut v8::PinScope<'s, '_>, res: v8::Local<v8::PromiseResolver>, st: v8::Local<v8::Object>, sig: v8::Local<v8::Object>, ok: bool, v: v8::Local<'s, v8::Value>) {
    if state_get(scope, st, "settled").is_true() {
        return;
    }
    state_set(scope, st, "settled", v8::Boolean::new(scope, true).into());
    if ok {
        res.resolve(scope, v);
    } else {
        res.reject(scope, v);
    }
    let r = v8::undefined(scope).into();
    signal_abort(scope, sig, r);
}

fn cb_p_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((kind, res, arg, st, sig)) = p_parts(scope, &args) else { return };
    if state_get(scope, st, "settled").is_true() {
        return;
    }
    let v = args.get(0);
    let idx = state_num(scope, st, "i");
    state_set_num(scope, st, "i", idx + 1.0);
    let index: v8::Local<v8::Value> = v8::Number::new(scope, idx).into();
    match kind.as_str() {
        "toArray" => {
            if let Ok(items) = v8::Local::<v8::Array>::try_from(state_get(scope, st, "items")) {
                let n = items.length();
                items.set_index(scope, n, v);
            }
        }
        "forEach" => {
            let Some(f) = func(arg) else { return };
            if let Err(e) = try_call(scope, f, &[v, index]) {
                settle(scope, res, st, sig, false, e);
            }
        }
        "first" => settle(scope, res, st, sig, true, v),
        "last" => {
            state_set(scope, st, "acc", v);
            state_set(scope, st, "hasAcc", v8::Boolean::new(scope, true).into());
        }
        "some" | "every" | "find" => {
            let Some(f) = func(arg) else { return };
            match try_call(scope, f, &[v, index]) {
                Err(e) => settle(scope, res, st, sig, false, e),
                Ok(r) => {
                    let pass = r.boolean_value(scope);
                    match kind.as_str() {
                        "some" if pass => {
                            let t = v8::Boolean::new(scope, true).into();
                            settle(scope, res, st, sig, true, t)
                        }
                        "every" if !pass => {
                            let f = v8::Boolean::new(scope, false).into();
                            settle(scope, res, st, sig, true, f)
                        }
                        "find" if pass => settle(scope, res, st, sig, true, v),
                        _ => {}
                    }
                }
            }
        }
        "reduce" => {
            let Some(f) = func(arg) else { return };
            if !state_get(scope, st, "hasAcc").is_true() {
                state_set(scope, st, "acc", v);
                state_set(scope, st, "hasAcc", v8::Boolean::new(scope, true).into());
                return;
            }
            let acc = state_get(scope, st, "acc");
            match try_call(scope, f, &[acc, v, index]) {
                Ok(r) => state_set(scope, st, "acc", r),
                Err(e) => settle(scope, res, st, sig, false, e),
            }
        }
        _ => {}
    }
}

fn cb_p_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((_kind, res, _arg, st, sig)) = p_parts(scope, &args) else { return };
    settle(scope, res, st, sig, false, args.get(0));
}

fn cb_p_complete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((kind, res, _arg, st, sig)) = p_parts(scope, &args) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match kind.as_str() {
        "toArray" => {
            let items = state_get(scope, st, "items");
            settle(scope, res, st, sig, true, items)
        }
        "forEach" | "find" => settle(scope, res, st, sig, true, undef),
        "some" => {
            let f = v8::Boolean::new(scope, false).into();
            settle(scope, res, st, sig, true, f)
        }
        "every" => {
            let t = v8::Boolean::new(scope, true).into();
            settle(scope, res, st, sig, true, t)
        }
        "first" | "last" | "reduce" => {
            if state_get(scope, st, "hasAcc").is_true() && kind != "first" {
                let acc = state_get(scope, st, "acc");
                settle(scope, res, st, sig, true, acc)
            } else if kind == "reduce" {
                let e = type_error(scope, "Failed to execute 'reduce' on 'Observable': Reduce of a valueless Observable with no initial value");
                settle(scope, res, st, sig, false, e)
            } else {
                let m = v8::String::new(scope, "No values in Observable").unwrap();
                let e = v8::Exception::range_error(scope, m);
                settle(scope, res, st, sig, false, e)
            }
        }
        _ => {}
    }
}

// ── the rows ──────────────────────────────────────────────────────────────────────────────────────

fn cb_obs_op<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if !require_this(scope, this) {
        return;
    }
    let a0 = args.get(0);
    let made = match method.as_str() {
        "map" | "filter" | "flatMap" | "switchMap" | "catch" | "finally" => {
            if !require_fn(scope, &args, &method) {
                return;
            }
            op_observable(scope, &method, this, a0)
        }
        "take" | "drop" => {
            if args.length() < 1 {
                throw_type_error(scope, &format!("Failed to execute '{method}' on 'Observable': 1 argument required, but only 0 present."));
                return;
            }
            let Some(n) = to_u64(scope, a0) else { return };
            let nv = v8::Number::new(scope, n).into();
            op_observable(scope, &method, this, nv)
        }
        "inspect" => op_observable(scope, "inspect", this, a0),
        "takeUntil" => {
            let n = match from_value(scope, a0) {
                Ok(o) => o,
                Err(e) => {
                    scope.throw_exception(e);
                    return;
                }
            };
            op_observable(scope, "takeUntil", this, n.into())
        }
        _ => None,
    };
    if let Some(o) = made {
        rv.set(o.into());
    }
}

fn cb_obs_promise_op<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if !require_this(scope, this) {
        return;
    }
    let (arg, options, init) = match method.as_str() {
        "toArray" | "first" | "last" => (v8::undefined(scope).into(), args.get(0), None),
        "reduce" => {
            if !require_fn(scope, &args, &method) {
                return;
            }
            let init = if args.length() >= 2 { Some(args.get(1)) } else { None };
            (args.get(0), args.get(2), init)
        }
        _ => {
            if !require_fn(scope, &args, &method) {
                return;
            }
            (args.get(0), args.get(1), None)
        }
    };
    if let Some(p) = promise_op(scope, &method, this, arg, options, init) {
        rv.set(p.into());
    }
}

fn cb_obs_from<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'from' on 'Observable': 1 argument required, but only 0 present.");
        return;
    }
    match from_value(scope, args.get(0)) {
        Ok(o) => rv.set(o.into()),
        Err(e) => {
            scope.throw_exception(e);
        }
    }
}

/// The table body of the Observable operator rows and `from`.
pub fn install_observable_ops(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    let name = str_val(scope, member.name);
    let f = match member.name {
        "from" => crate::binding::protos::build_fn(scope, cb_obs_from, member.name, member.length as i32),
        "toArray" | "forEach" | "first" | "last" | "some" | "every" | "find" | "reduce" => {
            crate::binding::protos::build_fn_data(scope, cb_obs_promise_op, member.name, member.length as i32, name)
        }
        _ => crate::binding::protos::build_fn_data(scope, cb_obs_op, member.name, member.length as i32, name),
    };
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}

#[allow(dead_code)]
fn _unused(scope: &mut v8::PinScope) {
    throw_range_error(scope, "");
}

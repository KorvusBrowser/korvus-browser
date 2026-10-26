//! `core/css/view_transition.rs` -- `ViewTransition`, `ViewTransitionTypeSet` and
//! `Element.startViewTransition()`, the Rust form of ~70 lines of
//! `dom/09-postshells.js`.
//!
//! WHAT IS HONEST HERE AND WHAT IS NOT. A view transition captures the old and new renderings and
//! cross-fades them; this engine has no compositor, so no transition is ever captured. What a page
//! actually DEPENDS on is the half that is real: the update callback RUNS, and `updateCallbackDone`
//! settles with its outcome. That is what this implements. `ready` rejects, because an engine that
//! captured nothing cannot honestly resolve it -- and that is exactly what Chrome does in a headless
//! render too, with the same `AbortError`.
//!
//! CHROME 150, recorded over loopback 2026-09-11, and THREE of these the JS got wrong:
//!
//! * the callback runs; `updateCallbackDone` resolves with **`undefined`** -- the JS resolved it with
//!   the callback's RETURN VALUE (`() => 42` gave `42`), which no browser does;
//! * when the callback THROWS, `updateCallbackDone` rejects with the error AND SO DOES `finished` --
//!   the JS resolved `finished` with `undefined` either way, so a page's `.catch` on it never fired;
//! * `ready` rejects with `DOMException` **`AbortError`**, message **"Transition was skipped"** --
//!   the JS invented `InvalidStateError` and "Skipped the view transition because the document was
//!   not rendered.";
//! * `types` is a `[SameObject]` `ViewTransitionTypeSet`, a real setlike: `add` returns
//!   **`undefined`** (not the set -- `Highlight.add` returns the set, this one does not, both
//!   measured), values are stringified (`add(7)` reads back `"7"`), and its iterators are
//!   `[object ViewTransitionTypeSet Iterator]`;
//! * a foreign receiver: `types`, `skipTransition`, `transitionRoot` and `waitUntil` throw
//!   `Illegal invocation`, but `finished`, `ready` and `updateCallbackDone` hand back a REJECTED
//!   PROMISE instead -- Blink turns an exception in a promise-typed attribute into a rejection, and
//!   the reason is `TypeError: Failed to read the 'finished' property from 'ViewTransition': Illegal
//!   invocation`. The JS threw from all three, which is a different observable.
//!
//! `document.startViewTransition()` answered `null` (`core/dom/document.rs`); its body
//! is [`cb_doc_start_view_transition`] below now, with the ORDER the browsers settle a document
//! transition's promises in -- which is not the `Element` transition's order above.

use deno_core::v8;

use crate::binding::protos::{
    brand_shell_instance, build_fn_data, get_prop, global_get, make_dom_exc, require_args, str_val, throw_type_error,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{el_id_of, priv_set};
use crate::core::dom::shared::{row_getter, row_method, row_method_recv_first};
use crate::core::dom::webidl_iter::{make_iter, pair, IterKind};
use crate::install::kits::{define_accessor, define_value};
use crate::install::MemberDef;

/// The three promises and the type set. `VT_UCD`'s presence IS the brand.
const VT_UCD: &str = "korvus_vt_updateCallbackDone";
const VT_READY: &str = "korvus_vt_ready";
const VT_FINISHED: &str = "korvus_vt_finished";
const VT_TYPES: &str = "korvus_vt_types";
/// A `ViewTransitionTypeSet`'s items, a `v8::Array` of strings in insertion order. Its presence IS the
/// brand.
const VTS_ITEMS: &str = "korvus_vts_items";

fn proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let c = global_get(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

// ── ViewTransitionTypeSet ────────────────────────────────────────────────────────────────────────

fn vts<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    match priv_get(scope, this, VTS_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => Some(a),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn vts_snapshot<'s>(scope: &mut v8::PinScope<'s, '_>, items: v8::Local<'s, v8::Array>) -> Vec<String> {
    (0..items.length()).filter_map(|i| items.get_index(scope, i)).map(|v| v.to_rust_string_lossy(scope)).collect()
}

fn vts_replace(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, vals: &[String]) {
    let arr = v8::Array::new(scope, vals.len() as i32);
    for (i, s) in vals.iter().enumerate() {
        let v = str_val(scope, s);
        arr.set_index(scope, i as u32, v);
    }
    priv_set(scope, this, VTS_ITEMS, arr.into());
}

fn cb_vts_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = vts(scope, args.this()) else { return };
    rv.set_uint32(items.length());
}

/// WebIDL `DOMString` coercion and nothing more (`add(7)` reads back `"7"`), and the return is
/// `undefined` -- measured, and NOT the set the way `Highlight.add` is.
fn cb_vts_add<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let Some(items) = vts(scope, this) else { return };
    if !require_args(scope, &args, 1, "ViewTransitionTypeSet", "add") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    let mut vals = vts_snapshot(scope, items);
    if !vals.iter().any(|v| *v == s) {
        vals.push(s);
        vts_replace(scope, this, &vals);
    }
}

fn cb_vts_clear<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if vts(scope, this).is_none() {
        return;
    }
    vts_replace(scope, this, &[]);
}

fn cb_vts_delete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(items) = vts(scope, this) else { return };
    if !require_args(scope, &args, 1, "ViewTransitionTypeSet", "delete") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    let mut vals = vts_snapshot(scope, items);
    match vals.iter().position(|v| *v == s) {
        Some(i) => {
            vals.remove(i);
            vts_replace(scope, this, &vals);
            rv.set_bool(true);
        }
        None => rv.set_bool(false),
    }
}

fn cb_vts_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = vts(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "ViewTransitionTypeSet", "has") {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    rv.set_bool(vts_snapshot(scope, items).iter().any(|v| *v == s));
}

fn vts_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, entries: bool) {
    let Some(items) = vts(scope, args.this()) else { return };
    let vals: Vec<v8::Local<v8::Value>> = vts_snapshot(scope, items)
        .iter()
        .map(|s| {
            let v = str_val(scope, s);
            if entries { pair(scope, v, v) } else { v }
        })
        .collect();
    if let Some(it) = make_iter(scope, "ViewTransitionTypeSet", IterKind::Set, &vals) {
        rv.set(it.into());
    }
}

fn cb_vts_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    vts_iter(scope, &args, &mut rv, false);
}
fn cb_vts_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    vts_iter(scope, &args, &mut rv, false);
}
fn cb_vts_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    vts_iter(scope, &args, &mut rv, true);
}

fn cb_vts_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(items) = vts(scope, args.this()) else { return };
    let vals: Vec<v8::Local<'s, v8::Value>> = vts_snapshot(scope, items).iter().map(|s| str_val(scope, s)).collect();
    crate::core::dom::webidl_iter::for_each(scope, &args, "ViewTransitionTypeSet", |_, i| vals.get(i).map(|v| (*v, *v)));
}

pub fn install_view_transition_type_set(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => row_getter(scope, proto, member, cb_vts_size),
        "add" => row_method(scope, proto, member, cb_vts_add),
        "clear" => row_method(scope, proto, member, cb_vts_clear),
        "delete" => row_method(scope, proto, member, cb_vts_delete),
        "entries" => row_method(scope, proto, member, cb_vts_entries),
        "forEach" => row_method(scope, proto, member, cb_vts_for_each),
        "has" => row_method(scope, proto, member, cb_vts_has),
        "keys" => row_method(scope, proto, member, cb_vts_keys),
        "values" => row_method(scope, proto, member, cb_vts_values),
        "@@iterator" => {
            let Some(values) = get_prop(scope, proto, "values") else { return };
            define_value(scope, proto, member, values, true);
        }
        other => debug_assert!(false, "install_view_transition_type_set: unexpected member {other}"),
    }
}

// ── ViewTransition ───────────────────────────────────────────────────────────────────────────────

/// A promise-typed attribute's receiver failure is a REJECTED PROMISE, not a throw -- Blink converts
/// the exception, and Chrome 150 answers `[object Promise]` for
/// `...finished.get.call(Object.create(ViewTransition.prototype))`. The reason carries Blink's own
/// sentence, which a page can read.
fn rejected_illegal<'s>(scope: &mut v8::PinScope<'s, '_>, member: &str) -> Option<v8::Local<'s, v8::Value>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    // Blink's context applied after the error is made (`deno_core::error::native_error`)
    let exc = deno_core::error::native_error(scope, "TypeError", &format!("Failed to read the '{member}' property from 'ViewTransition': Illegal invocation"));
    resolver.reject(scope, exc);
    Some(promise.into())
}

/// One of the three promise attributes: the stored promise, or a rejected one for a foreign receiver.
fn cb_vt_promise<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let slot = match member.as_str() {
        "ready" => VT_READY,
        "finished" => VT_FINISHED,
        _ => VT_UCD,
    };
    match priv_get(scope, args.this(), slot) {
        Some(p) => rv.set(p),
        None => {
            if let Some(p) = rejected_illegal(scope, &member) {
                rv.set(p);
            }
        }
    }
}

fn cb_vt_types<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    match priv_get(scope, args.this(), VT_TYPES) {
        Some(v) => rv.set(v),
        None => throw_type_error(scope, "Illegal invocation"),
    }
}

/// `skipTransition()` -- there is no capture to abandon, so there is nothing to do beyond the receiver
/// check. `ready` is already rejected with the AbortError a skip produces.
fn cb_vt_skip<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let t = args.this();
    if priv_get(scope, t, VT_UCD).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    // A DOCUMENT transition still has `ready` pending: the skip rejects it from a
    // microtask, once. The callback still runs and `finished` still follows the update (measured).
    if priv_get(scope, t, VT_R_READY).is_some() && !is_skipped(scope, t) {
        let yes: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        priv_set(scope, t, VT_SKIPPED, yes);
        let tv: v8::Local<v8::Value> = t.into();
        let f = build_fn_data(scope, cb_vt_skip_reject, "", 0, tv);
        scope.enqueue_microtask(f);
    }
}

pub fn install_view_transition(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "finished" | "ready" | "updateCallbackDone" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_vt_promise, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "types" => row_getter(scope, proto, member, cb_vt_types),
        "transitionRoot" => row_getter(scope, proto, member, cb_vt_transition_root),
        "skipTransition" => row_method_recv_first(scope, proto, member, cb_vt_skip),
        other => debug_assert!(false, "install_view_transition: unexpected member {other}"),
    }
}

// ── starting one ─────────────────────────────────────────────────────────────────────────────────

/// A `.then` fulfillment handler that DISCARDS the value: leaving `rv` untouched is `undefined`,
/// which is what `updateCallbackDone` settles with whatever the callback returned.
fn cb_to_undefined(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// A `.then` rejection handler that RE-THROWS, so the derived promise rejects with the same reason.
fn cb_rethrow<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let reason = args.get(0);
    scope.throw_exception(reason);
}

/// Start a view transition: mint the object, RUN the callback, and settle the three promises the way
/// Chrome 150 settles them.
pub(crate) fn start_view_transition<'s>(scope: &mut v8::PinScope<'s, '_>, cb: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = proto_of(scope, "ViewTransition")?;
    let t = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, t);

    // `types`: a real `[SameObject]` ViewTransitionTypeSet, empty.
    if let Some(tp) = proto_of(scope, "ViewTransitionTypeSet") {
        let s = crate::binding::protos::mint_on(scope, tp.into());
        brand_shell_instance(scope, s);
        let items = v8::Array::new(scope, 0);
        priv_set(scope, s, VTS_ITEMS, items.into());
        priv_set(scope, t, VT_TYPES, s.into());
    }

    // `ready` rejects: nothing is ever captured here, so it cannot honestly resolve. `AbortError` /
    // "Transition was skipped" is what Chrome 150 rejects with in a headless render, recorded.
    let ready_p = {
        let resolver = v8::PromiseResolver::new(scope)?;
        let p = resolver.get_promise(scope);
        if let Some(exc) = make_dom_exc(scope, "Transition was skipped", "AbortError") {
            resolver.reject(scope, exc);
        }
        p
    };
    // Mark it handled, so a page that never touches `ready` does not raise an unhandled rejection the
    // engine would report. The JS did the same with `ready.catch(() => {})`; nothing a page can
    // observe changes, because a later `.catch` still fires on the original promise.
    {
        let noop = crate::binding::protos::build_fn(scope, cb_to_undefined, "", 1);
        ready_p.then2(scope, noop, noop);
    }
    priv_set(scope, t, VT_READY, ready_p.into());

    // RUN the callback. This is the half a page actually depends on, and the only half this engine
    // can honestly do.
    //
    // `updateCallbackDone` settles with the callback's OUTCOME but never with its VALUE (Chrome
    // resolves `undefined` for `() => 42`, measured), and `finished` mirrors it -- including the
    // rejection, which the JS dropped on the floor by resolving `finished` either way.
    let (ucd_p, fin_p) = match v8::Local::<v8::Function>::try_from(cb) {
        Err(_) => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            (crate::binding::protos::resolved_promise(scope, u), {
                let u2: v8::Local<v8::Value> = v8::undefined(scope).into();
                crate::binding::protos::resolved_promise(scope, u2)
            })
        }
        Ok(f) => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            let called = {
                v8::tc_scope!(let tc, scope);
                let r = f.call(tc, undef, &[]);
                let out = match r {
                    Some(v) => Ok(v8::Global::new(tc, v)),
                    None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
                };
                tc.reset();
                out
            };
            match called {
                Err(exc) => {
                    let reason: v8::Local<v8::Value> = match exc {
                        Some(g) => v8::Local::new(scope, &g),
                        None => v8::undefined(scope).into(),
                    };
                    let a = crate::binding::protos::rejected_promise(scope, reason);
                    let b = crate::binding::protos::rejected_promise(scope, reason);
                    (a, b)
                }
                Ok(g) => {
                    let v = v8::Local::new(scope, &g);
                    match v8::Local::<v8::Promise>::try_from(v) {
                        // A promise: derive both from it, so a rejection reaches both. `then2` builds
                        // the derived promise, so no resolver has to be carried across the callback.
                        Ok(p) => {
                            let ok = crate::binding::protos::build_fn(scope, cb_to_undefined, "", 1);
                            let err = crate::binding::protos::build_fn(scope, cb_rethrow, "", 1);
                            let Some(ucd) = p.then2(scope, ok, err) else {
                                let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                                let a = crate::binding::protos::resolved_promise(scope, u);
                                let b = crate::binding::protos::resolved_promise(scope, u);
                                return finish(scope, t, a, b);
                            };
                            let ok2 = crate::binding::protos::build_fn(scope, cb_to_undefined, "", 1);
                            let err2 = crate::binding::protos::build_fn(scope, cb_rethrow, "", 1);
                            let fin = ucd.then2(scope, ok2, err2).map(|p| p.into()).unwrap_or_else(|| ucd.into());
                            (ucd.into(), fin)
                        }
                        // Anything else: both resolve with `undefined` -- NOT with the callback's
                        // return value, which is the answer the JS got wrong.
                        Err(_) => {
                            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                            let a = crate::binding::protos::resolved_promise(scope, u);
                            let b = crate::binding::protos::resolved_promise(scope, u);
                            (a, b)
                        }
                    }
                }
            }
        }
    };
    finish(scope, t, ucd_p, fin_p)
}

fn finish<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    t: v8::Local<'s, v8::Object>,
    ucd: v8::Local<'s, v8::Value>,
    fin: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Object>> {
    priv_set(scope, t, VT_UCD, ucd);
    priv_set(scope, t, VT_FINISHED, fin);
    Some(t)
}

/// `Element.startViewTransition(callback)` -- a row of the `Element` table.
pub(crate) fn cb_el_start_view_transition<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if el_id_of(scope, args.this()).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let cb = args.get(0);
    if let Some(t) = start_view_transition(scope, cb) {
        rv.set(t.into());
    }
}

// ── document.startViewTransition ───────────────────────────────────────────────────
//
// `document.startViewTransition(cb)` answered `null` here, so every page that did
// `document.startViewTransition(() => update()).finished.then(...)` threw on `.finished` of null and
// never ran its update -- a real `threw` row on the L14 corpus (C16). What a page can observe of a
// document view transition WITHOUT a compositor is the object, the callback, and the ORDER its three
// promises settle in. Measured on stock Chrome 150.0.7871.46, Brave 151.1.93.138 and Edge 151.0.4129.93
// over a loopback page, identical on every run (`tests/fixtures/p11a/*.vt*.txt`):
//
// ```text
// no callback      returned, microtask, updateCallbackDone ok, ready ok, finished ok
// sync callback    returned, microtask, callback, updateCallbackDone ok, ready ok, finished ok
// async callback   returned, microtask, callback, callback resolved, updateCallbackDone ok, ready ok, finished ok
// throwing         returned, microtask, callback, updateCallbackDone rejected TypeError,
//                  ready rejected TypeError, finished rejected TypeError
// skipTransition() returned, skipped, microtask, ready rejected AbortError, callback,
//   called at once updateCallbackDone ok, finished ok
// {update, types}  as sync, and `types.size` 2
// startViewTransition(5)  TypeError "Overload resolution failed."
// ```
//
// So the callback runs on a LATER TASK (after the caller's microtasks), a skip rejects `ready` from a
// microtask, and `ready` settles with the update's outcome -- there is nothing to capture and so
// nothing that can fail after it. `transitionRoot` is the document element.
//
// Opera 135 rejects `ready` with `InvalidStateError` for the ASYNC callback in 5 of 6 recordings and
// fulfils it in the sixth; two oracle runs that disagree are the oracle's noise (M2), and the test
// compares that line only where the recordings agree.

const VT_CALLBACK: &str = "korvus_vt_callback";
const VT_ROOT: &str = "korvus_vt_transitionRoot";
const VT_R_UCD: &str = "korvus_vt_resolver_ucd";
const VT_R_READY: &str = "korvus_vt_resolver_ready";
const VT_R_FINISHED: &str = "korvus_vt_resolver_finished";
const VT_SKIPPED: &str = "korvus_vt_skipped";

fn resolver_of<'s>(scope: &mut v8::PinScope<'s, '_>, t: v8::Local<v8::Object>, slot: &str) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    // A `Promise::Resolver` IS the promise object in V8, and the slot is private and written only by
    // `cb_doc_start_view_transition` with a resolver, so the checked-kind cast below is sound.
    let v = priv_get(scope, t, slot).filter(|v| v.is_promise())?;
    Some(unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(v) })
}

fn settle(scope: &mut v8::PinScope, t: v8::Local<v8::Object>, slot: &str, outcome: Result<(), v8::Local<v8::Value>>) {
    let Some(r) = resolver_of(scope, t, slot) else { return };
    match outcome {
        Ok(()) => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            r.resolve(scope, u);
        }
        Err(e) => {
            r.reject(scope, e);
        }
    }
}

fn is_skipped(scope: &mut v8::PinScope, t: v8::Local<v8::Object>) -> bool {
    priv_get(scope, t, VT_SKIPPED).map(|v| v.is_true()).unwrap_or(false)
}

/// The update finished (`Ok`) or failed (`Err(reason)`): settle the three promises in Chrome's order.
fn update_done(scope: &mut v8::PinScope, t: v8::Local<v8::Object>, outcome: Result<(), v8::Local<v8::Value>>) {
    settle(scope, t, VT_R_UCD, outcome);
    if !is_skipped(scope, t) {
        settle(scope, t, VT_R_READY, outcome);
    }
    settle(scope, t, VT_R_FINISHED, outcome);
}

fn cb_vt_update_ok<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Ok(t) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    update_done(scope, t, Ok(()));
}

fn cb_vt_update_err<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Ok(t) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    update_done(scope, t, Err(args.get(0)));
}

/// The task that runs the update callback.
fn cb_vt_run<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Ok(t) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(cb) = priv_get(scope, t, VT_CALLBACK).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        update_done(scope, t, Ok(()));
        return;
    };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let called = {
        v8::tc_scope!(let tc, scope);
        let out = match cb.call(tc, undef, &[]) {
            Some(v) => Ok(v8::Global::new(tc, v)),
            None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
        };
        tc.reset();
        out
    };
    match called {
        Err(e) => {
            let reason: v8::Local<v8::Value> = match e {
                Some(g) => v8::Local::new(scope, &g),
                None => v8::undefined(scope).into(),
            };
            update_done(scope, t, Err(reason));
        }
        Ok(g) => {
            let v = v8::Local::new(scope, &g);
            match v8::Local::<v8::Promise>::try_from(v) {
                Ok(p) => {
                    let tv: v8::Local<v8::Value> = t.into();
                    let ok = build_fn_data(scope, cb_vt_update_ok, "", 1, tv);
                    let err = build_fn_data(scope, cb_vt_update_err, "", 1, tv);
                    p.then2(scope, ok, err);
                }
                Err(_) => update_done(scope, t, Ok(())),
            }
        }
    }
}

/// The microtask a skip queues: `ready` rejects with `AbortError` "Transition was skipped".
fn cb_vt_skip_reject<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Ok(t) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    if let Some(exc) = make_dom_exc(scope, "Transition was skipped", "AbortError") {
        settle(scope, t, VT_R_READY, Err(exc));
    }
}

fn mark_handled(scope: &mut v8::PinScope, p: v8::Local<v8::Promise>) {
    let noop = crate::binding::protos::build_fn(scope, cb_to_undefined, "", 1);
    p.then2(scope, noop, noop);
}

/// `Document.startViewTransition(optional (ViewTransitionUpdateCallback or StartViewTransitionOptions))`.
pub(crate) fn cb_doc_start_view_transition<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    // Overload resolution: undefined -> no callback; a callable -> the callback; null or an object ->
    // the options dictionary; anything else fails (measured: `startViewTransition(5)`).
    let arg = args.get(0);
    let (callback, types): (Option<v8::Local<v8::Value>>, Vec<String>) = if arg.is_undefined() {
        (None, Vec::new())
    } else if arg.is_function() {
        (Some(arg), Vec::new())
    } else if arg.is_null() || arg.is_object() {
        let mut types = Vec::new();
        let mut update = None;
        // `StartViewTransitionOptions`, converted as WebIDL converts it (Chrome 150): `types` (a
        // sequence of strings), then `update` (a callback), each read once -- korvus read `update` first
        let Ok(converted) = crate::install::ctor_dicts::convert_dict(scope, arg, "StartViewTransitionOptions", "Failed to execute 'startViewTransition' on 'Document': ") else { return };
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v8::Local::<v8::Value>::from(converted)) {
            if let Some(u) = get_prop(scope, o, "update").filter(|v| !v.is_null_or_undefined()) {
                update = Some(u);
            }
            if let Some(ts) = get_prop(scope, o, "types").filter(|v| !v.is_null_or_undefined()) {
                let Ok(list) = v8::Local::<v8::Object>::try_from(ts) else {
                    throw_type_error(scope, "Failed to execute 'startViewTransition' on 'Document': Failed to read the 'types' property from 'StartViewTransitionOptions': The provided value cannot be converted to a sequence.");
                    return;
                };
                let len = get_prop(scope, list, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
                for i in 0..len {
                    let Some(item) = list.get_index(scope, i) else { return };
                    let Some(s) = item.to_string(scope) else { return };
                    let s = s.to_rust_string_lossy(scope);
                    if !types.contains(&s) {
                        types.push(s);
                    }
                }
            }
        }
        (update, types)
    } else {
        throw_type_error(scope, "Failed to execute 'startViewTransition' on 'Document': Overload resolution failed.");
        return;
    };

    let Some(proto) = proto_of(scope, "ViewTransition") else { return };
    let t = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, t);
    if let Some(tp) = proto_of(scope, "ViewTransitionTypeSet") {
        let s = crate::binding::protos::mint_on(scope, tp.into());
        brand_shell_instance(scope, s);
        vts_replace(scope, s, &types);
        priv_set(scope, t, VT_TYPES, s.into());
    }
    if let Some(root) = get_prop(scope, args.this(), "documentElement") {
        priv_set(scope, t, VT_ROOT, root);
    }
    if let Some(cb) = callback {
        priv_set(scope, t, VT_CALLBACK, cb);
    }
    for (rslot, pslot) in [(VT_R_UCD, VT_UCD), (VT_R_READY, VT_READY), (VT_R_FINISHED, VT_FINISHED)] {
        let Some(r) = v8::PromiseResolver::new(scope) else { return };
        let p = r.get_promise(scope);
        // `ready` and `finished` never surface as unhandled rejections -- a page that awaits only
        // `updateCallbackDone` is told about its own callback's failure once.
        if pslot != VT_UCD {
            mark_handled(scope, p);
        }
        priv_set(scope, t, pslot, p.into());
        priv_set(scope, t, rslot, r.into());
    }
    let tv: v8::Local<v8::Value> = t.into();
    let run = build_fn_data(scope, cb_vt_run, "", 0, tv);
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[run.into(), zero]);
    rv.set(t.into());
}

/// `transitionRoot` -- the element the transition is rooted at: the document element for a document
/// transition. `null` for an `Element.startViewTransition` one, which has no root recorded.
fn cb_vt_transition_root<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if priv_get(scope, args.this(), VT_UCD).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    match priv_get(scope, args.this(), VT_ROOT) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

// The two tables. Rows are the capture's verbatim; `waitUntil` has no body -- it extends a capture's
// lifetime, and there is no capture, so it stays an honest `stub`. (`transitionRoot` was one too until
// Document transitions have their root.) `@@iterator` is the
// setlike's value iterator (Chrome 150: `ViewTransitionTypeSet.prototype[Symbol.iterator] ===
// ...values`), which the registry cannot carry because it skips symbols.
webapi::interface! {
    "ViewTransitionTypeSet" [interface, window] {
        ctor "ViewTransitionTypeSet" len 0 = stub,
        attr "size" : Any readonly = install_view_transition_type_set,
        method "add" : Any len 1 = install_view_transition_type_set,
        method "clear" : Any len 0 = install_view_transition_type_set,
        method "delete" : Any len 1 = install_view_transition_type_set,
        method "entries" : Any len 0 = install_view_transition_type_set,
        method "forEach" : Any len 1 = install_view_transition_type_set,
        method "has" : Any len 1 = install_view_transition_type_set,
        method "keys" : Any len 0 = install_view_transition_type_set,
        method "values" : Any len 0 = install_view_transition_type_set,
        method "@@iterator" : Any len 0 hidden = install_view_transition_type_set
    }
}

webapi::interface! {
    "ViewTransition" [interface, window] {
        ctor "ViewTransition" len 0 = stub,
        attr "finished" : Promise readonly = install_view_transition,
        attr "ready" : Promise readonly = install_view_transition,
        attr "updateCallbackDone" : Promise readonly = install_view_transition,
        attr "types" : Any readonly = install_view_transition,
        attr "transitionRoot" : Any readonly = install_view_transition,
        method "skipTransition" : Any len 0 = install_view_transition,
        method "waitUntil" : Any len 1 required 1 = stub
    }
}

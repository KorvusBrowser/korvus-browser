//! `core/dom/webidl_iter.rs` -- the ONE WebIDL maplike/setlike/iterable iterator.
//!
//! A `maplike<K, V>` or `setlike<T>` declaration gives its interface `keys()`, `values()` and
//! `entries()`, and every one of them hands back an object of the interface's OWN iterator kind:
//! `Object.prototype.toString.call(CSS.highlights.keys())` is `"[object HighlightRegistry Iterator]"`,
//! which the capture records for five interfaces in this lane alone. A bare `Map`/`Set`/Array
//! iterator is observably a different object -- that is what `CustomStateSet` taught in W5.0c, and
//! `element_internals.rs` grew a private copy of this machinery to say it.
//!
//! This is that machinery, once, for every interface that needs it:
//!
//! * [`make_iter`] builds an iterator over a SNAPSHOT of the values to yield, on a per-tag prototype
//!   with a native `next` and the interface's own `Symbol.toStringTag`. Snapshot, not a live view: a
//!   WebIDL iterator is defined over the values present when it was created, and every recorded case
//!   reads it that way.
//! * The prototype's own prototype is the one Chrome gives the declaration's KIND ([`IterKind`]):
//!   `%MapIteratorPrototype%` for a maplike, `%SetIteratorPrototype%` for a setlike, `%IteratorPrototype%`
//!   for a pair iterable -- V8's intrinsics, so the iterators inherit the real `@@iterator` and spread
//!   works.
//! * A result is `{done, value}`, in that key order (Chrome 150.0.7871.46, every kind).
//! * [`pair`] builds the `[k, v]` array an `entries()` yields.
//!
//! The per-tag prototypes are kept by the realm ([`ITER_PROTOS`]), so
//! `set.keys()`'s prototype is the same object every call (a fresh one per call would be an identity
//! tell a page can see with two calls and a `===`).


use deno_core::v8;

use crate::binding::protos::{str_val, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;

/// The iterator's frozen snapshot, its position, and nothing else. Privates, not own properties: a
/// page that enumerates an iterator must see none of this.
const IT_ITEMS: &str = "korvus_wit_items";
const IT_IDX: &str = "korvus_wit_idx";

/// Which of WebIDL's iterator declarations an interface makes -- what its iterator prototype inherits:
/// `%MapIteratorPrototype%` (a `maplike`), `%SetIteratorPrototype%` (a `setlike`) or `%IteratorPrototype%`
/// (a pair `iterable`). Measured on Chrome 150.0.7871.46 for all ten interfaces this engine iterates;
/// every one inherited `%IteratorPrototype%` directly here.
#[derive(Clone, Copy)]
pub(crate) enum IterKind {
    Map,
    Set,
    Pair,
}

/// V8's intrinsic `which` in this realm: an object template carrying it as an intrinsic data property,
/// instantiated here and read back (the one API V8 gives an embedder for its intrinsics).
fn intrinsic<'s>(scope: &mut v8::PinScope<'s, '_>, which: v8::Intrinsic) -> Option<v8::Local<'s, v8::Object>> {
    let ot = v8::ObjectTemplate::new(scope);
    let k = v8::String::new(scope, "p")?;
    ot.set_intrinsic_data_property(k.into(), which, v8::PropertyAttribute::NONE);
    let holder = ot.new_instance(scope)?;
    holder.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// The realm map the iterator prototypes are kept in, one per `"<Iface> Iterator"` tag
/// ([`crate::binding::protos::realm_map`]). They were kept in `OpState`, per ISOLATE, so the first realm
/// to iterate an interface built its iterator prototype for every realm: once a frame had iterated a
/// `NodeList` first, the page's `NodeList` iterators chained to the FRAME's `%IteratorPrototype%`.
const ITER_PROTOS: &str = "korvus_iter_protos";

/// The `"<iface> Iterator"` prototype of this realm: a native `next` and the tag, over the intrinsic its
/// [`IterKind`] names. Cached, so two calls of `keys()` share one prototype.
fn iter_proto<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, kind: IterKind) -> Option<v8::Local<'s, v8::Object>> {
    let tag = format!("{iface} Iterator");
    let map = crate::binding::protos::realm_map(scope, ITER_PROTOS);
    let tag_v = str_val(scope, &tag);
    if let Some(p) = map.get(scope, tag_v).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(p);
    }
    let base = intrinsic(
        scope,
        match kind {
            IterKind::Map => v8::Intrinsic::MapIteratorPrototype,
            IterKind::Set => v8::Intrinsic::SetIteratorPrototype,
            IterKind::Pair => v8::Intrinsic::IteratorPrototype,
        },
    )?;
    let proto = crate::binding::protos::mint_on(scope, base.into());
    let tv = str_val(scope, &tag);
    let mut td = v8::PropertyDescriptor::new_from_value_writable(tv, false);
    td.set_enumerable(false);
    td.set_configurable(true);
    let tag_key = v8::Symbol::get_to_string_tag(scope);
    proto.define_property(scope, tag_key.into(), &td);
    // From its template: one per tag, so each iterator kind keeps a `next` of its own.
    let next = crate::binding::protos::template_fn(scope, cb_iter_next, "next", 0, Some(&tag));
    let nk = v8::String::new(scope, "next")?;
    let mut nd = v8::PropertyDescriptor::new_from_value_writable(next.into(), true);
    nd.set_enumerable(true);
    nd.set_configurable(true);
    proto.define_property(scope, nk.into(), &nd);
    map.set(scope, tag_v, proto.into());
    Some(proto)
}

fn cb_iter_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if let Some(target) = priv_get(scope, this, IT_TARGET) {
        let Some((done, value)) = live_next(scope, this, target) else {
            throw_type_error(scope, "Illegal invocation");
            return;
        };
        let result = v8::Object::new(scope);
        let (Some(dk), Some(vk)) = (v8::String::new(scope, "done"), v8::String::new(scope, "value")) else { return };
        let d: v8::Local<v8::Value> = v8::Boolean::new(scope, done).into();
        result.set(scope, dk.into(), d);
        result.set(scope, vk.into(), value);
        rv.set(result.into());
        return;
    }
    let (Some(items), Some(idx)) = (
        priv_get(scope, this, IT_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()),
        priv_get(scope, this, IT_IDX).and_then(|v| v.uint32_value(scope)),
    ) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let result = v8::Object::new(scope);
    let Some(vk) = v8::String::new(scope, "value") else { return };
    let Some(dk) = v8::String::new(scope, "done") else { return };
    // `done` first: Chrome's key order for every WebIDL iterator result (`Object.keys(it.next())`).
    if idx < items.length() {
        let item = items.get_index(scope, idx).unwrap_or_else(|| v8::undefined(scope).into());
        let next_idx: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, idx + 1).into();
        priv_set(scope, this, IT_IDX, next_idx);
        let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
        result.set(scope, dk.into(), f);
        result.set(scope, vk.into(), item);
    } else {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        result.set(scope, dk.into(), t);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        result.set(scope, vk.into(), u);
    }
    rv.set(result.into());
}

/// An iterator of `iface`'s own kind over `values` (already the exact sequence to yield).
pub(crate) fn make_iter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    kind: IterKind,
    values: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = iter_proto(scope, iface, kind)?;
    let it = crate::binding::protos::mint_on(scope, proto.into());
    let arr = v8::Array::new(scope, values.len() as i32);
    for (i, v) in values.iter().enumerate() {
        arr.set_index(scope, i as u32, *v);
    }
    priv_set(scope, it, IT_ITEMS, arr.into());
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    priv_set(scope, it, IT_IDX, zero);
    Some(it)
}

/// [`make_iter`] over the elements of `arr` -- the sequence an interface used to hand to an Array iterator
/// (`FormData`, `Headers`, `URLSearchParams`, `EventCounts`: `[object Array Iterator]`, `{value, done}`,
/// no receiver check, where Chrome 150.0.7871.46 gives each its own "<iface> Iterator").
pub(crate) fn array_as_iter<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, kind: IterKind, arr: v8::Local<'s, v8::Array>) -> v8::Local<'s, v8::Value> {
    let values: Vec<v8::Local<'s, v8::Value>> = (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect();
    match make_iter(scope, iface, kind, &values) {
        Some(it) => it.into(),
        None => v8::undefined(scope).into(),
    }
}

/// The entries a pair iterable walks NOW: the list as it stands at this step, read off the object each time.
/// WebIDL's default iterator and `forEach` are defined by INDEX over "the current
/// list of value pairs to iterate over", and Chrome 150.0.7871.46 walks them so: an entry the walk has not
/// reached yet and is deleted is never seen, one appended is, deleting the current entry skips the one after
/// it, and an iterator that reached the end yields an entry appended afterwards. `Headers`' list is its
/// sorted-and-combined one, recomputed at each step too.
pub(crate) fn live_pairs(scope: &mut v8::PinScope, iface: &str, target: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    match iface {
        "URLSearchParams" => crate::modules::url::url_search_params::usp_current(scope, target),
        "Headers" => crate::modules::fetch::headers::headers_pairs(scope, target),
        "FormData" => crate::modules::fetch::form_data::fd_pairs(scope, target),
        _ => None,
    }
}

/// The object a live pair iterator walks, what it yields (`0` keys, `1` values, `2` entries) and its
/// interface (which [`live_pairs`] source to read).
const IT_TARGET: &str = "korvus_wit_target";
const IT_WHAT: &str = "korvus_wit_what";
const IT_IFACE: &str = "korvus_wit_iface";

/// A pair iterable's `keys()`/`values()`/`entries()` (`what` 0/1/2): an iterator of `iface`'s own kind over
/// `target`'s CURRENT entries, by index (see [`live_pairs`]).
pub(crate) fn live_pair_iter<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, target: v8::Local<'s, v8::Value>, what: u32) -> v8::Local<'s, v8::Value> {
    // not one of `iface`'s objects (the receiver guard answers `Illegal invocation` before this is reached)
    if live_pairs(scope, iface, target).is_none() {
        return v8::undefined(scope).into();
    }
    let Some(proto) = iter_proto(scope, iface, IterKind::Pair) else { return v8::undefined(scope).into() };
    let it = crate::binding::protos::mint_on(scope, proto.into());
    priv_set(scope, it, IT_TARGET, target);
    let w: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, what).into();
    priv_set(scope, it, IT_WHAT, w);
    let tag = str_val(scope, iface);
    priv_set(scope, it, IT_IFACE, tag);
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    priv_set(scope, it, IT_IDX, zero);
    it.into()
}

/// [`live_pair_iter`]'s `next`: the entry at the iterator's index in the list as it stands now.
fn live_next<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>, target: v8::Local<'s, v8::Value>) -> Option<(bool, v8::Local<'s, v8::Value>)> {
    let idx = priv_get(scope, this, IT_IDX).and_then(|v| v.uint32_value(scope))?;
    let what = priv_get(scope, this, IT_WHAT).and_then(|v| v.uint32_value(scope))?;
    let iface = priv_get(scope, this, IT_IFACE)?.to_rust_string_lossy(scope);
    let pairs = live_pairs(scope, &iface, target)?;
    let Some((k, v)) = pairs.get(idx as usize) else {
        return Some((true, v8::undefined(scope).into()));
    };
    let next_idx: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, idx + 1).into();
    priv_set(scope, this, IT_IDX, next_idx);
    let (kv, vv) = (str_val(scope, k), str_val(scope, v));
    Some((false, match what {
        0 => kv,
        1 => vv,
        _ => pair(scope, kv, vv),
    }))
}

/// Blink's generated `forEach` of an iterable, maplike or setlike interface (Chrome 150.0.7871.46, measured on
/// all ten this engine implements): the argument count first ("1 argument required, but
/// only 0 present."), then the callback's type -- an IDL `Function`, so "parameter 1 is not of type
/// 'Function'.", not a callback interface's sentence -- then each entry `entry(i)` answers, called with
/// `thisArg` as `this` and `(value, key, object)`; a throw ends the walk and propagates. `entry(i)` is the
/// i-th entry of the list as it stands at that step for a pair iterable ([`live_pairs`]), a snapshot's for an
/// interface that iterates one. Recorded as Chromium labels the call, `ForEachIteratorCallback`. The receiver
/// is the caller's to check first: Chrome's `Illegal invocation` comes before the argument count.
pub(crate) fn for_each<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    iface: &str,
    mut entry: impl FnMut(&mut v8::PinScope<'s, '_>, usize) -> Option<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)>,
) {
    if !crate::binding::protos::require_args(scope, args, 1, iface, "forEach") {
        return;
    }
    let Ok(cb) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        throw_type_error(scope, &format!("Failed to execute 'forEach' on '{iface}': parameter 1 is not of type 'Function'."));
        return;
    };
    let this_arg = args.get(1);
    let obj: v8::Local<v8::Value> = args.this().into();
    let mut i = 0usize;
    while let Some((value, key)) = entry(scope, i) {
        #[cfg(feature = "analysis")]
        let r = crate::analysis_hook::call_traced(scope, "ForEachIteratorCallback", cb, this_arg, &[value, key, obj]);
        #[cfg(not(feature = "analysis"))]
        let r = cb.call(scope, this_arg, &[value, key, obj]);
        if r.is_none() {
            return;
        }
        i += 1;
    }
}

/// [`for_each`] over a pair iterable's live list (see [`live_pairs`]).
pub(crate) fn for_each_live_pairs<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, iface: &str) {
    let target: v8::Local<v8::Value> = args.this().into();
    for_each(scope, args, iface, |scope, i| {
        let pairs = live_pairs(scope, iface, target)?;
        let (k, v) = pairs.get(i)?;
        Some((str_val(scope, v), str_val(scope, k)))
    });
}

/// The `[k, v]` array an `entries()` iterator yields.
pub(crate) fn pair<'s>(scope: &mut v8::PinScope<'s, '_>, k: v8::Local<'s, v8::Value>, v: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Value> {
    let a = v8::Array::new(scope, 2);
    a.set_index(scope, 0, k);
    a.set_index(scope, 1, v);
    a.into()
}

//! `core/css/highlight.rs` -- `Highlight`, `HighlightRegistry` and the `CSS.highlights` singleton,
//! the Rust form of 100 lines of `dom/09-postshells.js`.
//!
//! THE SHAPE OF THE CSS CUSTOM HIGHLIGHT API. `Highlight` is a `setlike<AbstractRange>` with two extra
//! attributes (`priority`, `type`); `HighlightRegistry` is a `maplike<DOMString, Highlight>` with one
//! instance per realm, reached as `CSS.highlights`. Before this lane both existed only as generated
//! SHELLS -- 22 members answering `undefined` for objects nothing ever built -- and then as JS bodies
//! behind seven `op_nativize` masks and a `WeakMap`, which is what this file replaces.
//!
//! WHAT THE STATE IS, and why it is not own properties. The set's ranges, its priority and its type
//! live in `v8::Private` slots on the instance, so `Object.getOwnPropertyNames(new Highlight())` is
//! `[]` -- recorded from Chrome 150 over loopback, and an instance tell on Track 0.8's axis if it were
//! not. Same for the registry: `Object.getOwnPropertyNames(CSS.highlights)` is `[]`.
//!
//! CHROME'S ANSWERS, recorded 2026-09-11 (`google-chrome --headless=new --dump-dom` over a loopback
//! server; `file://` is refused), and each one is a row of `tests/highlight_live.rs`:
//!
//! * EVERY member of both prototypes throws `TypeError: Illegal invocation` for a foreign receiver --
//!   all 22, including the two accessors' setters. Measured on THIS tree before the cutover: not one
//!   threw; `HighlightRegistry.prototype.get.call({}, "k")` answered `undefined` and the JS `st2`
//!   helper ADOPTED any object whose prototype chain reached `Highlight.prototype`, so
//!   `Object.create(Highlight.prototype).add(r)` silently worked. This is the foreign-receiver check
//!   the brief says keeps paying, and it paid again: 22 of 22.
//! * `h.priority = 2.7` reads back `2` and `h.priority = NaN` reads back `0` (WebIDL `long`);
//!   `h.type = "nope"` leaves `type` at its previous value, it does not store the string.
//! * `o.add(r); o.add(r)` leaves `size` at 1 -- identity, not equality.
//! * `reg.get("nope")` is **`undefined`**. The JS answered `null`, which the bundle's own oracle
//!   (`HighlightRegistry.json`, the `get` case marked `"undefined": true`) already contradicted.
//! * `keys()`/`values()`/`entries()` hand back `[object Highlight Iterator]` /
//!   `[object HighlightRegistry Iterator]`, never a bare Map/Set iterator.
//! * The arity messages name 1 or 2 required arguments even though the capture-derived registry rows
//!   carry `required: 0` -- WebIDL's maplike/setlike declarations are not operations, so webref has no
//!   count to give. The rows below copy `required` VERBATIM (the capture is the denominator) and the
//!   BODIES do the check, which is the shape `CustomStateSet` established in W5.0c.

use deno_core::v8;

use crate::binding::protos::{
    brand_shell_instance, build_fn, get_prop, global_get, require_args, str_val, throw_type_error,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::dom::shared::{row_accessor, row_getter, row_method};
use crate::core::dom::webidl_iter::{make_iter, pair, IterKind};
use crate::install::kits::define_value;
use crate::install::MemberDef;

/// A `Highlight`'s ranges, in insertion order. Its presence IS the brand: an object that does not
/// carry it is not one of ours, which is the check Chrome makes for an internal slot.
const HL_RANGES: &str = "korvus_hl_ranges";
const HL_PRIORITY: &str = "korvus_hl_priority";
const HL_TYPE: &str = "korvus_hl_type";
/// A `HighlightRegistry`'s keys and values, index-aligned, in insertion order.
const HR_KEYS: &str = "korvus_hr_keys";
const HR_VALS: &str = "korvus_hr_vals";

/// The three values `Highlight.type` accepts. Anything else is IGNORED (not stored, not thrown on),
/// which is what an enumerated IDL attribute with a missing-value default does and what Chrome 150
/// answers for `h.type = "nope"`.
const HIGHLIGHT_TYPES: [&str; 3] = ["highlight", "spelling-error", "grammar-error"];

fn proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let c = global_get(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// Append `v` to the array in `slot`, rewriting the whole array. A `v8::Array` has no push, and the
/// sets this API holds are a handful of ranges, so rebuilding is cheaper than a side table.
fn slot_push<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>, slot: &str, v: v8::Local<'s, v8::Value>) {
    let cur = priv_get(scope, obj, slot).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let n = cur.map(|a| a.length()).unwrap_or(0);
    let arr = v8::Array::new(scope, (n + 1) as i32);
    for i in 0..n {
        if let Some(cur) = cur {
            if let Some(x) = cur.get_index(scope, i) {
                arr.set_index(scope, i, x);
            }
        }
    }
    arr.set_index(scope, n, v);
    priv_set(scope, obj, slot, arr.into());
}

/// Remove index `at` from the array in `slot`.
fn slot_remove<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>, slot: &str, at: u32) {
    let Some(cur) = priv_get(scope, obj, slot).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let arr = v8::Array::new(scope, (cur.length().saturating_sub(1)) as i32);
    let mut j = 0;
    for i in 0..cur.length() {
        if i == at {
            continue;
        }
        if let Some(x) = cur.get_index(scope, i) {
            arr.set_index(scope, j, x);
        }
        j += 1;
    }
    priv_set(scope, obj, slot, arr.into());
}

fn slot_clear<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>, slot: &str) {
    let arr = v8::Array::new(scope, 0);
    priv_set(scope, obj, slot, arr.into());
}

fn slot_items<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>, slot: &str) -> Vec<v8::Local<'s, v8::Value>> {
    match priv_get(scope, obj, slot).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect(),
        None => Vec::new(),
    }
}

// ── Highlight ────────────────────────────────────────────────────────────────────────────────────

/// The receiver as a `Highlight`, or a pending `TypeError: Illegal invocation`. The brand is the
/// private slot the constructor writes, NOT the prototype chain -- `Object.create(Highlight.prototype)`
/// is rejected here and by Chrome, and the JS this replaces accepted it.
fn hl<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    match priv_get(scope, this, HL_RANGES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => Some(a),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The index of `want` in the set, by IDENTITY (`===`): a setlike of object values dedupes on the
/// object, so `o.add(r); o.add(r)` is one entry and two distinct equal Ranges are two.
fn hl_index_of<'s>(scope: &mut v8::PinScope<'s, '_>, items: v8::Local<'s, v8::Array>, want: v8::Local<'s, v8::Value>) -> Option<u32> {
    (0..items.length()).find(|i| items.get_index(scope, *i).map(|v| v == want).unwrap_or(false))
}

/// `new Highlight(...ranges)` -- variadic, `length` 0, and the ranges are added in order with the same
/// identity dedupe `add()` uses (`new Highlight(r).size` is 1, recorded).
pub(crate) fn cb_highlight_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST, and here it is not a message defect but a CORRUPTION.
    // Without it a bare `Highlight()` ran this body with `this` bound to the global and reached the
    // `set_prototype` below -- so calling it RE-PROTOTYPED THE WINDOW onto `Highlight.prototype`.
    // Measured at `00b4bddd`: after `Highlight()`, `Object.prototype.toString.call(window)` answered
    // `[object Highlight]`, and every later bare constructor call in the same realm reported the
    // Window under that tag.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "Highlight") {
        return;
    }
    let this = args.this();
    let Some(proto) = proto_of(scope, "Highlight") else { return };
    this.set_prototype(scope, proto.into());
    brand_shell_instance(scope, this);
    let empty = v8::Array::new(scope, 0);
    priv_set(scope, this, HL_RANGES, empty.into());
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    priv_set(scope, this, HL_PRIORITY, zero);
    let t = str_val(scope, HIGHLIGHT_TYPES[0]);
    priv_set(scope, this, HL_TYPE, t);
    for i in 0..args.length() {
        let v = args.get(i);
        let Some(items) = priv_get(scope, this, HL_RANGES).and_then(|x| v8::Local::<v8::Array>::try_from(x).ok()) else { continue };
        if hl_index_of(scope, items, v).is_none() {
            slot_push(scope, this, HL_RANGES, v);
        }
    }
}

fn cb_hl_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = hl(scope, args.this()) else { return };
    rv.set_uint32(items.length());
}

fn cb_hl_priority_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    let n = priv_get(scope, this, HL_PRIORITY).and_then(|v| v.int32_value(scope)).unwrap_or(0);
    rv.set_int32(n);
}

/// WebIDL `long`: ToNumber then ToInt32, which truncates (`2.7` -> `2`) and maps NaN to 0 -- both
/// recorded from Chrome 150.
fn cb_hl_priority_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    let n = args.get(0).int32_value(scope).unwrap_or(0);
    let v: v8::Local<v8::Value> = v8::Integer::new(scope, n).into();
    priv_set(scope, this, HL_PRIORITY, v);
}

fn cb_hl_type_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    match priv_get(scope, this, HL_TYPE) {
        Some(v) => rv.set(v),
        None => {
            let v = str_val(scope, HIGHLIGHT_TYPES[0]);
            rv.set(v);
        }
    }
}

fn cb_hl_type_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    let s = args.get(0).to_rust_string_lossy(scope);
    if HIGHLIGHT_TYPES.contains(&s.as_str()) {
        let v = str_val(scope, &s);
        priv_set(scope, this, HL_TYPE, v);
    }
}

fn cb_hl_add<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(items) = hl(scope, this) else { return };
    if !require_args(scope, &args, 1, "Highlight", "add") {
        return;
    }
    let v = args.get(0);
    if hl_index_of(scope, items, v).is_none() {
        slot_push(scope, this, HL_RANGES, v);
    }
    rv.set(this.into());
}

fn cb_hl_clear<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    slot_clear(scope, this, HL_RANGES);
}

fn cb_hl_delete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(items) = hl(scope, this) else { return };
    if !require_args(scope, &args, 1, "Highlight", "delete") {
        return;
    }
    match hl_index_of(scope, items, args.get(0)) {
        Some(i) => {
            slot_remove(scope, this, HL_RANGES, i);
            rv.set_bool(true);
        }
        None => rv.set_bool(false),
    }
}

fn cb_hl_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(items) = hl(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "Highlight", "has") {
        return;
    }
    rv.set_bool(hl_index_of(scope, items, args.get(0)).is_some());
}

/// A setlike's `keys()` and `values()` are the same sequence; `entries()` yields `[v, v]`.
fn hl_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, entries: bool) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    let items = slot_items(scope, this, HL_RANGES);
    let vals: Vec<v8::Local<v8::Value>> =
        if entries { items.iter().map(|v| pair(scope, *v, *v)).collect() } else { items };
    if let Some(it) = make_iter(scope, "Highlight", IterKind::Set, &vals) {
        rv.set(it.into());
    }
}

fn cb_hl_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hl_iter(scope, &args, &mut rv, false);
}
fn cb_hl_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hl_iter(scope, &args, &mut rv, false);
}
fn cb_hl_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hl_iter(scope, &args, &mut rv, true);
}

fn cb_hl_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hl(scope, this).is_none() {
        return;
    }
    let ranges = slot_items(scope, this, HL_RANGES);
    crate::core::dom::webidl_iter::for_each(scope, &args, "Highlight", |_, i| ranges.get(i).map(|v| (*v, *v)));
}

/// `Highlight`'s rows. `@@iterator` is the SAME function object as `values` -- the setlike's value
/// iterator, which the capture-derived registry does not carry because
/// `Object.getOwnPropertyNames` skips symbols, and which Chrome 150 has
/// (`Highlight.prototype[Symbol.iterator] === Highlight.prototype.values`, recorded). Same shape as
/// `CustomStateSet`'s row in `core/html/element_internals.rs`.
pub fn install_highlight(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "priority" => row_accessor(scope, proto, member, cb_hl_priority_get, cb_hl_priority_set),
        "type" => row_accessor(scope, proto, member, cb_hl_type_get, cb_hl_type_set),
        "size" => row_getter(scope, proto, member, cb_hl_size),
        "add" => row_method(scope, proto, member, cb_hl_add),
        "clear" => row_method(scope, proto, member, cb_hl_clear),
        "delete" => row_method(scope, proto, member, cb_hl_delete),
        "entries" => row_method(scope, proto, member, cb_hl_entries),
        "forEach" => row_method(scope, proto, member, cb_hl_for_each),
        "has" => row_method(scope, proto, member, cb_hl_has),
        "keys" => row_method(scope, proto, member, cb_hl_keys),
        "values" => row_method(scope, proto, member, cb_hl_values),
        "@@iterator" => {
            let Some(values) = get_prop(scope, proto, "values") else { return };
            define_value(scope, proto, member, values, true);
        }
        other => debug_assert!(false, "install_highlight: unexpected member {other}"),
    }
}

// ── HighlightRegistry ────────────────────────────────────────────────────────────────────────────

/// The receiver as a `HighlightRegistry`, or a pending `TypeError: Illegal invocation`.
fn hr<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    match priv_get(scope, this, HR_KEYS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => Some(a),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn hr_index_of<'s>(scope: &mut v8::PinScope<'s, '_>, keys: v8::Local<'s, v8::Array>, want: &str) -> Option<u32> {
    (0..keys.length()).find(|i| keys.get_index(scope, *i).map(|v| v.to_rust_string_lossy(scope) == want).unwrap_or(false))
}

fn cb_hr_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(keys) = hr(scope, args.this()) else { return };
    rv.set_uint32(keys.length());
}

fn cb_hr_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(keys) = hr(scope, this) else { return };
    if !require_args(scope, &args, 2, "HighlightRegistry", "set") {
        return;
    }
    let k = args.get(0).to_rust_string_lossy(scope);
    match hr_index_of(scope, keys, &k) {
        Some(i) => {
            if let Some(vals) = priv_get(scope, this, HR_VALS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
                vals.set_index(scope, i, args.get(1));
            }
        }
        None => {
            let kv = str_val(scope, &k);
            slot_push(scope, this, HR_KEYS, kv);
            slot_push(scope, this, HR_VALS, args.get(1));
        }
    }
    rv.set(this.into());
}

/// `undefined` for a key the registry does not hold -- NOT `null`. The bundle's oracle records it
/// (`HighlightRegistry.json`'s `get` case is `"undefined": true`) and Chrome 150 confirms it; the JS
/// this replaces answered `null`, so a page doing `if (CSS.highlights.get(k) === undefined)` read the
/// wrong answer.
fn cb_hr_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(keys) = hr(scope, this) else { return };
    if !require_args(scope, &args, 1, "HighlightRegistry", "get") {
        return;
    }
    let k = args.get(0).to_rust_string_lossy(scope);
    let Some(i) = hr_index_of(scope, keys, &k) else { return };
    if let Some(v) = priv_get(scope, this, HR_VALS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()).and_then(|a| a.get_index(scope, i)) {
        rv.set(v);
    }
}

fn cb_hr_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(keys) = hr(scope, args.this()) else { return };
    if !require_args(scope, &args, 1, "HighlightRegistry", "has") {
        return;
    }
    let k = args.get(0).to_rust_string_lossy(scope);
    rv.set_bool(hr_index_of(scope, keys, &k).is_some());
}

fn cb_hr_delete<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(keys) = hr(scope, this) else { return };
    if !require_args(scope, &args, 1, "HighlightRegistry", "delete") {
        return;
    }
    let k = args.get(0).to_rust_string_lossy(scope);
    match hr_index_of(scope, keys, &k) {
        Some(i) => {
            slot_remove(scope, this, HR_KEYS, i);
            slot_remove(scope, this, HR_VALS, i);
            rv.set_bool(true);
        }
        None => rv.set_bool(false),
    }
}

fn cb_hr_clear<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hr(scope, this).is_none() {
        return;
    }
    slot_clear(scope, this, HR_KEYS);
    slot_clear(scope, this, HR_VALS);
}

/// A maplike's `keys()` yields the keys, `values()` the values, `entries()` the `[k, v]` pairs.
fn hr_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, kind: u8) {
    let this = args.this();
    if hr(scope, this).is_none() {
        return;
    }
    let keys = slot_items(scope, this, HR_KEYS);
    let vals = slot_items(scope, this, HR_VALS);
    let out: Vec<v8::Local<v8::Value>> = match kind {
        0 => keys,
        1 => vals,
        _ => keys.iter().zip(vals.iter()).map(|(k, v)| pair(scope, *k, *v)).collect(),
    };
    if let Some(it) = make_iter(scope, "HighlightRegistry", IterKind::Map, &out) {
        rv.set(it.into());
    }
}

fn cb_hr_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hr_iter(scope, &args, &mut rv, 0);
}
fn cb_hr_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hr_iter(scope, &args, &mut rv, 1);
}
fn cb_hr_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    hr_iter(scope, &args, &mut rv, 2);
}

fn cb_hr_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    if hr(scope, this).is_none() {
        return;
    }
    let keys = slot_items(scope, this, HR_KEYS);
    let vals = slot_items(scope, this, HR_VALS);
    crate::core::dom::webidl_iter::for_each(scope, &args, "HighlightRegistry", |_, i| Some((*vals.get(i)?, *keys.get(i)?)));
}

/// `highlightsFromPoint(x, y, options)`: the registered highlights whose
/// ranges cover the viewport point, each as a `HighlightHitResult` `{highlight, ranges}` holding the
/// ranges that do -- a range covers the point when one of its `getClientRects()` boxes contains it.
/// Highest `priority` first, a later registration first among equals (CSS Highlight API §6.3). An
/// empty registry is `[]` (Chrome 150, measured).
fn cb_hr_highlights_from_point<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if hr(scope, this).is_none() {
        return;
    }
    if !require_args(scope, &args, 2, "HighlightRegistry", "highlightsFromPoint") {
        return;
    }
    let Some(x) = args.get(0).number_value(scope) else { return };
    let Some(y) = args.get(1).number_value(scope) else { return };
    let mut hits: Vec<(i64, usize, v8::Local<v8::Value>, Vec<v8::Local<v8::Value>>)> = Vec::new();
    let vals = slot_items(scope, this, HR_VALS);
    for (i, h) in vals.iter().enumerate() {
        let Ok(ho) = v8::Local::<v8::Object>::try_from(*h) else { continue };
        let prio = priv_get(scope, ho, HL_PRIORITY).and_then(|v| v.integer_value(scope)).unwrap_or(0);
        let mut covering = Vec::new();
        for r in slot_items(scope, ho, HL_RANGES) {
            let Some(rects) = crate::binding::protos::call_method(scope, r, "getClientRects", &[]) else { continue };
            let Ok(ro) = v8::Local::<v8::Object>::try_from(rects) else { continue };
            let n = get_prop(scope, ro, "length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
            let mut hit = false;
            for j in 0..n {
                let Some(b) = ro.get_index(scope, j).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
                let f = |scope: &mut v8::PinScope, k: &str| get_prop(scope, b, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
                let (l, t, w, hgt) = (f(scope, "left"), f(scope, "top"), f(scope, "width"), f(scope, "height"));
                if x >= l && x < l + w && y >= t && y < t + hgt {
                    hit = true;
                    break;
                }
            }
            if hit {
                covering.push(r);
            }
        }
        if !covering.is_empty() {
            hits.push((prio, i, *h, covering));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out = Vec::new();
    for (_, _, h, ranges) in hits {
        let o = v8::Object::new(scope);
        let k = v8::String::new(scope, "highlight").unwrap();
        o.set(scope, k.into(), h);
        let arr = v8::Array::new_with_elements(scope, &ranges);
        let k = v8::String::new(scope, "ranges").unwrap();
        o.set(scope, k.into(), arr.into());
        out.push(o.into());
    }
    let arr = v8::Array::new_with_elements(scope, &out);
    rv.set(arr.into());
}

/// `HighlightRegistry`'s rows.
pub fn install_highlight_registry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => row_getter(scope, proto, member, cb_hr_size),
        "clear" => row_method(scope, proto, member, cb_hr_clear),
        "delete" => row_method(scope, proto, member, cb_hr_delete),
        "entries" => row_method(scope, proto, member, cb_hr_entries),
        "forEach" => row_method(scope, proto, member, cb_hr_for_each),
        "get" => row_method(scope, proto, member, cb_hr_get),
        "has" => row_method(scope, proto, member, cb_hr_has),
        "keys" => row_method(scope, proto, member, cb_hr_keys),
        "set" => row_method(scope, proto, member, cb_hr_set),
        "highlightsFromPoint" => row_method(scope, proto, member, cb_hr_highlights_from_point),
        "values" => row_method(scope, proto, member, cb_hr_values),
        "@@iterator" => {
            let Some(entries) = get_prop(scope, proto, "entries") else { return };
            define_value(scope, proto, member, entries, true);
        }
        other => debug_assert!(false, "install_highlight_registry: unexpected member {other}"),
    }
}

// ── CSS.highlights ───────────────────────────────────────────────────────────────────────────────

/// The realm's one registry, minted at install and handed back by every read, from the slot every window
/// singleton is kept in -- so a realm built from templates makes its own on the first read.
fn cb_css_highlights<'s>(scope: &mut v8::PinScope<'s, '_>, _args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if let Some(v) = crate::core::html::window_globals::window_singleton(scope, HIGHLIGHTS_SLOT) {
        rv.set(v);
    }
}

const HIGHLIGHTS_SLOT: &str = "CSS.highlights";

/// This realm's `HighlightRegistry`.
fn mint_highlight_registry<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let proto = crate::binding::protos::shell_proto(scope, "HighlightRegistry")?;
    let inst = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, inst);
    let keys = v8::Array::new(scope, 0);
    priv_set(scope, inst, HR_KEYS, keys.into());
    let vals = v8::Array::new(scope, 0);
    priv_set(scope, inst, HR_VALS, vals.into());
    Some(inst.into())
}

/// `CSS.highlights`: the realm's single `HighlightRegistry`, on the `CSS` NAMESPACE object.
///
/// Not a table row, because `CSS` is a WebIDL namespace and has no interface object or prototype for
/// the installer to walk -- the same shape every singleton accessor in `korvus-runtime/src/singletons.rs`
/// has. The descriptor is Chrome 150's, recorded: `{get, enumerable: true, configurable: true}`, the
/// getter named `get highlights` with length 0, and NOT receiver-checked -- Chrome answers the
/// registry for `Object.getOwnPropertyDescriptor(CSS, "highlights").get.call({})`, which is the one
/// place in this lane where a foreign receiver is not an `Illegal invocation`.
pub(crate) fn install_css_highlights<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let Some(css) = global_get(scope, "CSS").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(proto) = proto_of(scope, "HighlightRegistry") else { return };
    let inst = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, inst);
    let keys = v8::Array::new(scope, 0);
    priv_set(scope, inst, HR_KEYS, keys.into());
    let vals = v8::Array::new(scope, 0);
    priv_set(scope, inst, HR_VALS, vals.into());
    crate::core::html::window_globals::window_slot_set(scope, HIGHLIGHTS_SLOT, inst.into());
    crate::core::html::window_globals::register_mint(HIGHLIGHTS_SLOT, crate::core::html::window_globals::Mint::With(mint_highlight_registry));
    let g = build_fn(scope, cb_css_highlights, "get highlights", 0);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), undef);
    d.set_enumerable(true);
    d.set_configurable(true);
    if let Some(k) = v8::String::new(scope, "highlights") {
        css.define_property(scope, k.into(), &d);
    }
}

// The two tables. Rows are the capture's (`install/generated_registry.rs` at the cutover), verbatim --
// names, order, kinds, lengths, `required`, nullability and descriptor flags -- with the `@@iterator`
// row added for the reason `install_highlight` documents.
webapi::interface! {
    // `[value]`, not `[interface]`: the capture records `"newable": true` and the registry row it
    // replaces is `Model::Value` (webref gives `Highlight` a constructor). `new Highlight(r1, r2)`
    // really constructs, through `cb_highlight_ctor`.
    "Highlight" [value, window] {
        ctor "Highlight" len 0 = stub,
        attr "priority" : Any = install_highlight,
        attr "type" : Any = install_highlight,
        attr "size" : Any readonly = install_highlight,
        method "add" : Any len 1 = install_highlight,
        method "clear" : Any len 0 = install_highlight,
        method "delete" : Any len 1 = install_highlight,
        method "entries" : Any len 0 = install_highlight,
        method "forEach" : Any len 1 = install_highlight,
        method "has" : Any len 1 = install_highlight,
        method "keys" : Any len 0 = install_highlight,
        method "values" : Any len 0 = install_highlight,
        method "@@iterator" : Any len 0 hidden = install_highlight
    }
}

webapi::interface! {
    "HighlightRegistry" [interface, window] {
        ctor "HighlightRegistry" len 0 = stub,
        attr "size" : Any readonly = install_highlight_registry,
        method "clear" : Any len 0 = install_highlight_registry,
        method "delete" : Any len 1 = install_highlight_registry,
        method "entries" : Any len 0 = install_highlight_registry,
        method "forEach" : Any len 1 = install_highlight_registry,
        method "get" : Any len 1 = install_highlight_registry,
        method "has" : Any len 1 = install_highlight_registry,
        method "keys" : Any len 0 = install_highlight_registry,
        method "set" : Any len 2 = install_highlight_registry,
        method "values" : Any len 0 = install_highlight_registry,
        method "highlightsFromPoint" : Any len 2 required 2 = install_highlight_registry,
        method "@@iterator" : Any len 0 hidden = install_highlight_registry
    }
}

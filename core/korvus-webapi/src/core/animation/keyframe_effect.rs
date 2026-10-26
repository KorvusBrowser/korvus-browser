//! `core/animation/keyframe_effect.rs` — `AnimationEffect` and its one concrete subclass,
//! `KeyframeEffect`.
//!
//! `AnimationEffect` carries the timing (`getTiming`/`getComputedTiming`/`updateTiming`);
//! `KeyframeEffect` adds the target and the keyframes. Brave 151 records exactly three members on
//! `AnimationEffect.prototype` and five on `KeyframeEffect.prototype` — notably NO `parent` /
//! `previousSibling` / `nextSibling` / `before` / `after` / `replace` / `remove` (the group-effect
//! members from an earlier draft of the spec), and no `iterationComposite`. The capture is the
//! authority, so none of those are created here; the recorded answers for them are `undefined` and
//! "not a function", which is what an absent member already gives.
//!
//! The two `interface!` tables at the bottom of this file are the two interfaces' ONE definition site:
//! names, order, kinds, lengths, `required` counts, nullability and descriptor
//! flags VERBATIM from the two `install/generated_registry.rs` blocks they replace. THE installer
//! (`install::live::install_window_animations`) mounts every row, so the descriptor (name, length,
//! enumerable, configurable, `[native code]`) is the capture's by construction.

use deno_core::v8;

use crate::binding::protos::{get_prop, require_args, str_val, throw_dom, throw_type_error};
use crate::install::MemberDef;

use super::{
    css_property_name, effect_handle, row_get_set, row_method, set_effect_handle, set_num, set_num_or_null, set_str,
    to_finite, to_unrestricted, with_store, EffectRecord, Keyframe, PROTO_KEYFRAME_EFFECT,
};
use page::animation::{compute_timing, Easing, FillMode, PlaybackDirection, Timing};

/// The pseudo-elements a `KeyframeEffect` may target. Anything else is a `SyntaxError` — Brave 151
/// records `effect.pseudoElement = "x"` throwing one, because the setter parses its argument as a
/// `<pseudo-element-selector>` rather than storing whatever it is given.
const PSEUDOS: [&str; 12] = [
    "::before", "::after", "::marker", "::placeholder", "::first-line", "::first-letter",
    "::selection", "::backdrop", "::grammar-error", "::spelling-error", "::target-text",
    "::file-selector-button",
];

// ── keyframe processing ─────────────────────────────────────────────────────────────────────────

/// The three keyframe-level members that are NOT animatable properties.
const RESERVED: [&str; 3] = ["offset", "easing", "composite"];

fn own_enumerable_keys(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Vec<String> {
    let Some(names) = obj.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..names.length() {
        if let Some(k) = names.get_index(scope, i) {
            if k.is_string() {
                out.push(k.to_rust_string_lossy(scope));
            }
        }
    }
    out
}

/// Read `obj[key]` as a string, or `None` when it is absent/undefined.
fn opt_string(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, key: &str) -> Option<String> {
    let v = get_prop(scope, obj, key)?;
    (!v.is_undefined()).then(|| v.to_rust_string_lossy(scope))
}

/// Is `v` a sequence (has a callable `Symbol.iterator`)? That is the test the spec uses to choose
/// between the two keyframe argument forms — `[{opacity:0}]` versus `{opacity:[0,1]}`.
fn is_sequence(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let sym = v8::Symbol::get_iterator(scope);
    obj.get(scope, sym.into()).map(|f| f.is_function()).unwrap_or(false)
}

/// Materialise an iterable into a Vec of its entries.
fn iterate<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Vec<v8::Local<'s, v8::Value>> {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        return (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect();
    }
    // A non-Array iterable (a Set, a generator, an array-like with Symbol.iterator): run it through
    // `Array.from`, which is the same conversion WebIDL's `sequence<T>` performs.
    let from = crate::binding::protos::intrinsic_fn(scope, "Array.from");
    let Some(from) = from else { return Vec::new() };
    let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(arr) = from.call(scope, recv, &[v]).and_then(|r| v8::Local::<v8::Array>::try_from(r).ok()) else {
        return Vec::new();
    };
    (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect()
}

/// One of the property-indexed form's three reserved keys, as a list: the key may hold a single
/// value (which applies to every keyframe) or a list (one entry per keyframe).
fn per_key<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    key: &str,
) -> Vec<v8::Local<'s, v8::Value>> {
    match get_prop(scope, obj, key).filter(|v| !v.is_undefined()) {
        Some(v) if is_sequence(scope, v) && !v.is_string() => iterate(scope, v),
        Some(v) => vec![v],
        None => Vec::new(),
    }
}

/// "Compute missing keyframe offsets": the first is 0, the last is 1, and every unanchored keyframe
/// in between is distributed evenly across the gap it falls in.
fn compute_offsets(frames: &mut [Keyframe]) {
    let n = frames.len();
    if n == 0 {
        return;
    }
    if frames[0].offset.is_none() {
        frames[0].computed_offset = 0.0;
    }
    if frames[n - 1].offset.is_none() {
        frames[n - 1].computed_offset = 1.0;
    }
    let mut i = 0;
    while i < n {
        if frames[i].offset.is_some() || i == 0 || i == n - 1 {
            i += 1;
            continue;
        }
        // Find the next anchored keyframe and spread the run between the two anchors.
        let start = i - 1;
        let mut end = i;
        while end < n - 1 && frames[end].offset.is_none() {
            end += 1;
        }
        let (a, b) = (frames[start].computed_offset, frames[end].computed_offset);
        let span = (end - start) as f64;
        for (k, idx) in (start + 1..end).enumerate() {
            frames[idx].computed_offset = a + (b - a) * ((k + 1) as f64 / span);
        }
        i = end + 1;
    }
}

/// The spec's "process a keyframes argument". `Err` carries the TypeError message.
pub(crate) fn process_keyframes<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    iface: &str,
) -> Result<Vec<Keyframe>, String> {
    if v.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        return Err(format!("Failed to construct '{iface}': The provided value is not of type 'object'."));
    };
    let mut frames: Vec<Keyframe> = Vec::new();

    if is_sequence(scope, v) {
        // ── the array form: one object per keyframe ──
        for entry in iterate(scope, v) {
            if entry.is_null_or_undefined() {
                frames.push(Keyframe { easing: "linear".into(), composite: "auto".into(), ..Default::default() });
                continue;
            }
            let Ok(eo) = v8::Local::<v8::Object>::try_from(entry) else {
                return Err(format!("Failed to construct '{iface}': a keyframe is not an object."));
            };
            // The keyframe as a `BaseKeyframe` dictionary first, its members in WebIDL's order -- `composite`,
            // `easing`, `offset`, each converted as it is read -- and only then its own properties (Chrome 150;
            // korvus read `offset`, `easing`, `composite`).
            let composite = opt_string(scope, eo, "composite").unwrap_or_else(|| "auto".to_string());
            let easing = opt_string(scope, eo, "easing").unwrap_or_else(|| "linear".to_string());
            let offset_v = get_prop(scope, eo, "offset").filter(|o| !o.is_null_or_undefined());
            let offset_n = offset_v.map(|o| to_finite(scope, o));
            let offset = match offset_n {
                Some(Some(n)) => {
                    if !(0.0..=1.0).contains(&n) {
                        return Err("Offsets must be null or in the range [0,1].".to_string());
                    }
                    Some(n)
                }
                Some(None) => return Err("Offsets must be null or in the range [0,1].".to_string()),
                None => None,
            };
            if Easing::parse(&easing).is_none() {
                return Err(format!("'{easing}' is not a valid value for easing"));
            }
            let mut props = Vec::new();
            for k in own_enumerable_keys(scope, eo) {
                if RESERVED.contains(&k.as_str()) {
                    continue;
                }
                let Some(val) = get_prop(scope, eo, &k) else { continue };
                if val.is_undefined() {
                    continue;
                }
                props.push((k.clone(), css_property_name(&k), val.to_rust_string_lossy(scope)));
            }
            // The spec sorts the property list; `getKeyframes()` is otherwise insertion-ordered and
            // two pages writing the same keyframe in different key order would read back differently.
            props.sort_by(|a, b| a.0.cmp(&b.0));
            frames.push(Keyframe { offset, computed_offset: offset.unwrap_or(0.0), easing, composite, props });
        }
    } else {
        // ── the property-indexed form: `{ opacity: [0, 1], easing: "ease" }` ──
        // The object as a `BasePropertyIndexedKeyframe` dictionary first -- `composite`, `easing`, `offset`, each a
        // value or a sequence, read once in that order -- and only then its animatable properties (Chrome 150;
        // korvus walked the properties first and read the three after, in reverse)
        let composites_in = per_key(scope, obj, "composite");
        let easings_in = per_key(scope, obj, "easing");
        let offsets_in = per_key(scope, obj, "offset");
        let mut lists: Vec<(String, String, Vec<String>)> = Vec::new();
        for k in own_enumerable_keys(scope, obj) {
            if RESERVED.contains(&k.as_str()) {
                continue;
            }
            let Some(val) = get_prop(scope, obj, &k) else { continue };
            if val.is_undefined() {
                continue;
            }
            let values: Vec<String> = if is_sequence(scope, val) && !val.is_string() {
                iterate(scope, val).into_iter().map(|e| e.to_rust_string_lossy(scope)).collect()
            } else {
                vec![val.to_rust_string_lossy(scope)]
            };
            lists.push((k.clone(), css_property_name(&k), values));
        }
        lists.sort_by(|a, b| a.0.cmp(&b.0));

        let count = lists.iter().map(|(_, _, v)| v.len()).max().unwrap_or(0);
        for i in 0..count {
            let offset = offsets_in.get(i).copied().filter(|o| !o.is_null_or_undefined()).and_then(|o| to_finite(scope, o));
            let easing = easings_in
                .get(i)
                .or_else(|| easings_in.last().filter(|_| easings_in.len() == 1))
                .map(|e| e.to_rust_string_lossy(scope))
                .unwrap_or_else(|| "linear".to_string());
            if Easing::parse(&easing).is_none() {
                return Err(format!("'{easing}' is not a valid value for easing"));
            }
            let composite = composites_in
                .get(i)
                .or_else(|| composites_in.last().filter(|_| composites_in.len() == 1))
                .map(|e| e.to_rust_string_lossy(scope))
                .unwrap_or_else(|| "auto".to_string());
            let props: Vec<(String, String, String)> = lists
                .iter()
                .filter_map(|(idl, css, vals)| vals.get(i).map(|v| (idl.clone(), css.clone(), v.clone())))
                .collect();
            frames.push(Keyframe { offset, computed_offset: offset.unwrap_or(0.0), easing, composite, props });
        }
    }
    compute_offsets(&mut frames);
    Ok(frames)
}

// ── timing conversion ───────────────────────────────────────────────────────────────────────────

/// `(unrestricted double or <dict>)`, converted as WebIDL converts the union: an object, `null`
/// or `undefined` is the dictionary `dict` -- the `EffectTiming` members, then the derived dictionary's, each read
/// once in WebIDL's order and converted as read -- and anything else the duration, as is. The answer is what
/// [`apply_timing`] and the caller read in place of the page's value. `Err` after throwing.
pub(crate) fn timing_options<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, dict: &str, prefix: &str) -> Result<v8::Local<'s, v8::Value>, ()> {
    if v.is_null_or_undefined() || v.is_object() {
        return crate::install::ctor_dicts::convert_dict(scope, v, dict, prefix).map(|o| o.into());
    }
    Ok(v)
}

/// Apply an `EffectTiming` / `KeyframeEffectOptions` dictionary (or a bare duration) onto `t`.
/// `Err` carries the TypeError message.
pub(crate) fn apply_timing(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, t: &mut Timing) -> Result<(), String> {
    if v.is_undefined() {
        return Ok(());
    }
    if v.is_number() || (!v.is_object() && !v.is_null()) {
        let d = to_unrestricted(scope, v);
        if d.is_nan() || d < 0.0 {
            return Err(format!("Failed to set duration: '{d}' is not a valid duration."));
        }
        t.duration = Some(d);
        return Ok(());
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Ok(()) };

    if let Some(d) = get_prop(scope, o, "delay").filter(|x| !x.is_undefined()) {
        t.delay = to_finite(scope, d).ok_or("delay must be a finite number")?;
    }
    if let Some(d) = get_prop(scope, o, "endDelay").filter(|x| !x.is_undefined()) {
        t.end_delay = to_finite(scope, d).ok_or("endDelay must be a finite number")?;
    }
    if let Some(f) = opt_string(scope, o, "fill") {
        t.fill = FillMode::parse(&f)
            .ok_or_else(|| format!("The provided value '{f}' is not a valid enum value of type FillMode."))?;
    }
    if let Some(d) = get_prop(scope, o, "iterationStart").filter(|x| !x.is_undefined()) {
        let n = to_finite(scope, d).filter(|n| *n >= 0.0).ok_or("iterationStart must be non-negative and finite")?;
        t.iteration_start = n;
    }
    if let Some(d) = get_prop(scope, o, "iterations").filter(|x| !x.is_undefined()) {
        let n = to_unrestricted(scope, d);
        if n.is_nan() || n < 0.0 {
            return Err("iterations must be non-negative".to_string());
        }
        t.iterations = n;
    }
    if let Some(d) = get_prop(scope, o, "duration").filter(|x| !x.is_undefined()) {
        if d.is_string() {
            let s = d.to_rust_string_lossy(scope);
            if s != "auto" {
                return Err(format!("Failed to set duration: '{s}' is not a valid duration."));
            }
            t.duration = None;
        } else {
            let n = to_unrestricted(scope, d);
            if n.is_nan() || n < 0.0 {
                return Err(format!("Failed to set duration: '{n}' is not a valid duration."));
            }
            t.duration = Some(n);
        }
    }
    if let Some(f) = opt_string(scope, o, "direction") {
        t.direction = PlaybackDirection::parse(&f)
            .ok_or_else(|| format!("The provided value '{f}' is not a valid enum value of type PlaybackDirection."))?;
    }
    if let Some(e) = opt_string(scope, o, "easing") {
        let parsed = Easing::parse(&e).ok_or_else(|| format!("'{e}' is not a valid value for easing"))?;
        t.easing = parsed;
        t.easing_text = e;
    }
    Ok(())
}

/// Fill an object with the `EffectTiming` members — shared by `getTiming()` and
/// `getComputedTiming()` (whose result is a superset).
///
/// IN THE ORDER BLINK'S BINDINGS WRITE THEM (BrowserScan item 7): an IDL dictionary converts to an
/// object member by member, the inherited dictionary's first and each dictionary's own members in lexicographic order
/// (WebIDL "convert a dictionary to an ECMAScript value"; Blink's generated `FillWithMembers`), whatever order
/// `Timing::ConvertToEffectTiming` set them in. Chrome 150: `Object.keys(effect.getTiming())` is
/// `delay,direction,duration,easing,endDelay,fill,iterationStart,iterations`; this wrote `delay,endDelay,fill,...`.
fn write_timing(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, t: &Timing, resolved: bool) {
    set_num(scope, o, "delay", t.delay);
    set_str(scope, o, "direction", t.direction.as_str());
    match (t.duration, resolved) {
        (Some(d), _) => set_num(scope, o, "duration", d),
        (None, true) => set_num(scope, o, "duration", 0.0),
        (None, false) => set_str(scope, o, "duration", "auto"),
    }
    set_str(scope, o, "easing", &t.easing_text);
    set_num(scope, o, "endDelay", t.end_delay);
    set_str(scope, o, "fill", if resolved { t.fill.resolved().as_str() } else { t.fill.as_str() });
    set_num(scope, o, "iterationStart", t.iteration_start);
    set_num(scope, o, "iterations", t.iterations);
}

// ── the KeyframeEffect constructor ──────────────────────────────────────────────────────────────

/// Create the effect record + its JS object. Shared by `new KeyframeEffect(...)` and
/// `element.animate(...)`.
pub(crate) fn make_effect<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: Option<v8::Local<'s, v8::Object>>,
    target: Option<v8::Local<'s, v8::Object>>,
    target_id: Option<u32>,
    keyframes: Vec<Keyframe>,
    timing: Timing,
) -> Option<v8::Local<'s, v8::Object>> {
    let obj = match this {
        Some(o) => o,
        None => super::new_on_proto(scope, PROTO_KEYFRAME_EFFECT)?,
    };
    let target_obj = target.map(|t| v8::Global::new(scope, t));
    let handle = with_store(scope, |s| {
        let h = s.alloc();
        s.effects.borrow_mut().insert(
            h,
            EffectRecord {
                target: target_id,
                target_obj,
                pseudo: None,
                composite: "replace".to_string(),
                timing,
                keyframes,
                animation: None,
            },
        );
        h
    });
    set_effect_handle(scope, obj, handle);
    let g = v8::Global::new(scope, obj);
    with_store(scope, |s| s.effect_objs.borrow_mut().insert(handle, g));
    Some(obj)
}

pub(crate) fn cb_kfe_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("KeyframeEffect"));
        return;
    }
    // Two overloads, `(Element? target, object? keyframes, optional ... options)` and `(KeyframeEffect source)`, picked
    // by WebIDL's overload resolution on the argument count (Chrome 150): none is "1 argument required, but only 0
    // present.", one is the copy -- `new KeyframeEffect(null)` "parameter 1 is not of type 'KeyframeEffect'." korvus
    // asked two arguments of every call, in a method's sentence.
    if !crate::install::webidl::require_args(scope, &args, "KeyframeEffect", 1) {
        return;
    }
    if args.length() == 1 {
        copy_effect(scope, &args, &mut rv);
        return;
    }
    let target_v = args.get(0);
    let (target, target_id) = if target_v.is_null_or_undefined() {
        (None, None)
    } else {
        match (v8::Local::<v8::Object>::try_from(target_v).ok(), crate::binding::protos::arg_node_id(scope, target_v)) {
            (Some(o), Some(id)) => (Some(o), Some(id)),
            _ => {
                throw_type_error(scope, "Failed to construct 'KeyframeEffect': parameter 1 is not of type 'Element'.");
                return;
            }
        }
    };
    // `(unrestricted double or KeyframeEffectOptions) options` is converted with the other arguments -- before the
    // keyframes are processed, which is a step of the constructor (Chrome 150)
    let Ok(options) = timing_options(scope, args.get(2), "KeyframeEffectOptions", "Failed to construct 'KeyframeEffect': ") else { return };
    let frames = match process_keyframes(scope, args.get(1), "KeyframeEffect") {
        Ok(f) => f,
        Err(msg) => {
            throw_type_error(scope, &msg);
            return;
        }
    };
    let mut timing = Timing::default();
    if let Err(msg) = apply_timing(scope, options, &mut timing) {
        throw_type_error(scope, &msg);
        return;
    }
    let this = args.this();
    if make_effect(scope, Some(this), target, target_id, frames, timing).is_some() {
        rv.set(this.into());
    }
}

/// `new KeyframeEffect(source)`: a new effect with the source's target, pseudo-element, keyframes, composite operation
/// and specified timing (Blink `KeyframeEffect::Create(script_state, source)`), attached to no animation.
fn copy_effect<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue) {
    let Some(h) = effect_handle(scope, args.get(0)) else {
        throw_type_error(scope, "Failed to construct 'KeyframeEffect': parameter 1 is not of type 'KeyframeEffect'.");
        return;
    };
    let source = with_store(scope, |s| {
        s.effects.borrow().get(&h).map(|e| (e.target, e.target_obj.clone(), e.pseudo.clone(), e.composite.clone(), e.timing.clone(), e.keyframes.clone()))
    });
    let Some((target_id, target_obj, pseudo, composite, timing, keyframes)) = source else { return };
    let target = target_obj.map(|g| v8::Local::new(scope, &g));
    let this = args.this();
    let Some(obj) = make_effect(scope, Some(this), target, target_id, keyframes, timing) else { return };
    if let Some(nh) = effect_handle(scope, obj.into()) {
        with_store(scope, |s| {
            if let Some(e) = s.effects.borrow_mut().get_mut(&nh) {
                e.pseudo = pseudo;
                e.composite = composite;
            }
        });
    }
    rv.set(this.into());
}

// ── the members ─────────────────────────────────────────────────────────────────────────────────

/// The receiver's effect handle, or a TypeError (an illegal-invocation brand check).
fn this_effect(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<u32> {
    let this: v8::Local<v8::Value> = args.this().into();
    match effect_handle(scope, this) {
        Some(h) => Some(h),
        None => {
            throw_type_error(scope, &format!("Failed to read the '{member}' property: Illegal invocation"));
            None
        }
    }
}

fn cb_effect_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_effect(scope, &args, &member) else { return };
    match member.as_str() {
        "target" => {
            let g = with_store(scope, |s| s.effects.borrow().get(&h).and_then(|e| e.target_obj.clone()));
            match g {
                Some(g) => {
                    let o = v8::Local::new(scope, &g);
                    rv.set(o.into());
                }
                None => rv.set_null(),
            }
        }
        "pseudoElement" => {
            let p = with_store(scope, |s| s.effects.borrow().get(&h).and_then(|e| e.pseudo.clone()));
            match p {
                Some(p) => {
                    let v = str_val(scope, &p);
                    rv.set(v);
                }
                None => rv.set_null(),
            }
        }
        "composite" => {
            let c = with_store(scope, |s| s.effects.borrow().get(&h).map(|e| e.composite.clone())).unwrap_or_default();
            let v = str_val(scope, &c);
            rv.set(v);
        }
        _ => {}
    }
}

fn cb_effect_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_effect(scope, &args, &member) else { return };
    let value = args.get(0);
    match member.as_str() {
        "target" => {
            let (obj, id) = if value.is_null_or_undefined() {
                (None, None)
            } else {
                match (v8::Local::<v8::Object>::try_from(value).ok(), crate::binding::protos::arg_node_id(scope, value)) {
                    (Some(o), Some(id)) => (Some(o), Some(id)),
                    _ => {
                        throw_type_error(scope, "Failed to set the 'target' property on 'KeyframeEffect': The provided value is not of type 'Element'.");
                        return;
                    }
                }
            };
            let g = obj.map(|o| v8::Global::new(scope, o));
            with_store(scope, |s| {
                if let Some(e) = s.effects.borrow_mut().get_mut(&h) {
                    e.target = id;
                    e.target_obj = g;
                }
            });
        }
        "pseudoElement" => {
            let p = if value.is_null_or_undefined() {
                None
            } else {
                let s = value.to_rust_string_lossy(scope);
                // A `<pseudo-element-selector>` is parsed, not stored: `"x"` is a SyntaxError, which
                // is what Brave 151 records for `effect.pseudoElement = "x"`.
                let ok = PSEUDOS.contains(&s.as_str())
                    || (s.starts_with("::-webkit-") && s.len() > 10)
                    || s.starts_with("::part(")
                    || s.starts_with("::highlight(");
                if !ok {
                    throw_dom(
                        scope,
                        &format!("Failed to set the 'pseudoElement' property on 'KeyframeEffect': '{s}' is not a valid value for pseudoElement."),
                        "SyntaxError",
                    );
                    return;
                }
                Some(s)
            };
            with_store(scope, |s| {
                if let Some(e) = s.effects.borrow_mut().get_mut(&h) {
                    e.pseudo = p;
                }
            });
        }
        "composite" => {
            let s = value.to_rust_string_lossy(scope);
            if !["replace", "add", "accumulate"].contains(&s.as_str()) {
                throw_type_error(
                    scope,
                    &format!("Failed to set the 'composite' property on 'KeyframeEffect': The provided value '{s}' is not a valid enum value of type CompositeOperation."),
                );
                return;
            }
            with_store(scope, |st| {
                if let Some(e) = st.effects.borrow_mut().get_mut(&h) {
                    e.composite = s;
                }
            });
        }
        _ => {}
    }
}

fn cb_effect_method<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_effect(scope, &args, &member) else { return };
    match member.as_str() {
        "getTiming" => {
            let t = with_store(scope, |s| s.effects.borrow().get(&h).map(|e| e.timing.clone()));
            let Some(t) = t else { return };
            let o = v8::Object::new(scope);
            write_timing(scope, o, &t, false);
            rv.set(o.into());
        }
        "getComputedTiming" => {
            let (t, anim) = match with_store(scope, |s| s.effects.borrow().get(&h).map(|e| (e.timing.clone(), e.animation))) {
                Some(v) => v,
                None => return,
            };
            // The effect's local time is its animation's current time; an unattached effect has none.
            let local = match anim {
                Some(a) => super::animation::current_time_of(scope, a),
                None => None,
            };
            let c = compute_timing(&t, local);
            let o = v8::Object::new(scope);
            // `ComputedEffectTiming : EffectTiming`: the inherited members first, then its own, lexicographically
            // (see `write_timing`).
            write_timing(scope, o, &t, true);
            set_num(scope, o, "activeDuration", c.active_duration);
            set_num_or_null(scope, o, "currentIteration", c.current_iteration);
            set_num(scope, o, "endTime", c.end_time);
            set_num_or_null(scope, o, "localTime", c.local_time);
            set_num_or_null(scope, o, "progress", c.progress);
            rv.set(o.into());
        }
        "updateTiming" => {
            // `updateTiming()` with no argument is a legal no-op (the dictionary is optional and
            // every member of it is): Brave 151 records it returning undefined, not throwing.
            let mut t = match with_store(scope, |s| s.effects.borrow().get(&h).map(|e| e.timing.clone())) {
                Some(t) => t,
                None => return,
            };
            // `optional OptionalEffectTiming timing = {}`, converted (its members have no defaults: only the
            // given ones change)
            let Ok(timing) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "OptionalEffectTiming", "Failed to execute 'updateTiming' on 'AnimationEffect': ") else { return };
            if let Err(msg) = apply_timing(scope, timing.into(), &mut t) {
                throw_type_error(scope, &msg);
                return;
            }
            with_store(scope, |s| {
                if let Some(e) = s.effects.borrow_mut().get_mut(&h) {
                    e.timing = t;
                }
            });
        }
        "getKeyframes" => {
            let frames = with_store(scope, |s| s.effects.borrow().get(&h).map(|e| e.keyframes.clone())).unwrap_or_default();
            let arr = v8::Array::new(scope, frames.len() as i32);
            for (i, f) in frames.iter().enumerate() {
                // Built by hand in Blink, not a dictionary: `Keyframe::AddKeyframePropertiesToV8Object` writes `offset`,
                // `easing`, `composite`, then the keyframe's properties, and `KeyframeEffect::getKeyframes` adds
                // `computedOffset` last. Chrome 150: `offset,easing,composite,opacity,computedOffset`.
                let o = v8::Object::new(scope);
                set_num_or_null(scope, o, "offset", f.offset);
                set_str(scope, o, "easing", &f.easing);
                set_str(scope, o, "composite", &f.composite);
                for (idl, _css, val) in &f.props {
                    set_str(scope, o, idl, val);
                }
                set_num(scope, o, "computedOffset", f.computed_offset);
                arr.set_index(scope, i as u32, o.into());
            }
            rv.set(arr.into());
        }
        "setKeyframes" => {
            // `keyframes` is a REQUIRED (nullable) argument: `setKeyframes()` is a TypeError while
            // `setKeyframes(null)` empties the list. Brave 151 records both.
            if !require_args(scope, &args, 1, "KeyframeEffect", "setKeyframes") {
                return;
            }
            match process_keyframes(scope, args.get(0), "KeyframeEffect") {
                Ok(frames) => with_store(scope, |s| {
                    if let Some(e) = s.effects.borrow_mut().get_mut(&h) {
                        e.keyframes = frames;
                    }
                }),
                Err(msg) => throw_type_error(scope, &msg),
            }
        }
        _ => {}
    }
}

// ── the two tables ──────────────────────────────────────────────────────────────────────────────

/// Mount one `KeyframeEffect` row: three attributes (`cb_effect_get`/`cb_effect_set`, which read the
/// member name out of the function's `data`) and the two keyframe methods.
pub fn install_keyframe_effect(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "target" | "pseudoElement" | "composite" => row_get_set(scope, proto, member, cb_effect_get, cb_effect_set),
        "getKeyframes" | "setKeyframes" => row_method(scope, proto, member, cb_effect_method),
        other => debug_assert!(false, "install_keyframe_effect: unexpected member {other}"),
    }
}

/// Mount one `AnimationEffect` row -- the three timing methods, which `KeyframeEffect` inherits and
/// which are the only members Brave 151 records on this prototype.
pub fn install_animation_effect(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getComputedTiming" | "getTiming" | "updateTiming" => row_method(scope, proto, member, cb_effect_method),
        other => debug_assert!(false, "install_animation_effect: unexpected member {other}"),
    }
}

// `AnimationEffect` is an ABSTRACT base: Brave 151 records `new AnimationEffect()` as a TypeError,
// so the table's `Model::Interface` interface object -- a real constructor that throws "Illegal
// constructor" -- is the shape, and nothing here ever mints one. `KeyframeEffect` is its one
// concrete subclass and carries the real constructor.
webapi::interface! {
    "AnimationEffect" [interface, window] {
    ctor   "AnimationEffect" len 0                    = stub,
    method "getComputedTiming" : Object               = install_animation_effect,
    method "getTiming" : Object                       = install_animation_effect,
    method "updateTiming" : Object                    = install_animation_effect
    }
}

webapi::interface! {
    "KeyframeEffect" : "AnimationEffect" [value, window] {
    ctor   "KeyframeEffect" len 1                     = stub,
    attr   "target" : Object nullable                 = install_keyframe_effect,
    attr   "pseudoElement" : Object nullable          = install_keyframe_effect,
    attr   "composite" : Object                       = install_keyframe_effect,
    method "getKeyframes" : Object                    = install_keyframe_effect,
    method "setKeyframes" : Object len 1 required 1   = install_keyframe_effect
    }
}

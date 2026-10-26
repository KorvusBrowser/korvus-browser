//! `core/timing/performance_observer.rs` — `PerformanceObserver` and `PerformanceObserverEntryList`,
//! each one `interface!` table.
//!
//! Before this lane the interface was built TWICE in JS, once per realm: `dom/08g-observers.js`
//! (window) and `core/korvus-runtime/src/js/worker-host.js` (worker) each ran the same block —
//! three `method()` round-trips onto the cppgc class template's prototype, a JS
//! `Symbol.toStringTag` accessor, three `KB.__po*` bridge slots and a
//! `KB.__makePerformanceObserver(perf)` factory whose returned plain JS `function
//! PerformanceObserver(cb)` was written straight onto the global. A fourth JS site, an
//! `op_nativize` mask, wore `[native code]` over a JS arrow that returned the frozen
//! `supportedEntryTypes` array.
//!
//! All of that is rows of the two tables below, installed by `install::live::install_window_observers`
//! and `install_worker_observers` — the interface is `[Exposed=(Window,Worker)]` and both captures list
//! it, so both realms mount THE SAME table.
//!
//! `PerformanceEntry`'s table moved out of this file and into `core/timing/performance.rs`
//! with the rest of the timeline, and moved the OBSERVER LIST and the delivery loop in here: the two
//! `KB.__po*` bridge names the JS timeline read (`__poTypes`, `__poCb`) and the `__perfObs` slot it
//! kept its observers on are all gone, and an observer registers with the cppgc `Performance` it was
//! constructed against.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::{build_fn_data, get_prop, str_val, throw_dom, throw_type_error};
use crate::install::live::{method_row, set_table_proto};
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct PerformanceObserver {
    cb: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    types: std::cell::RefCell<Vec<String>>,
    perf: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    /// Entries queued for this observer's next delivery, and whether a delivery task is already
    /// scheduled. The JS timeline kept both in a `Map<observer, buffer>` closed over per realm; they
    /// belong to the observer, which is what makes one delivery per observer per task work without a
    /// per-realm side table.
    pending: std::cell::RefCell<Vec<v8::Global<v8::Value>>>,
    scheduled: std::cell::Cell<bool>,
    /// The spec's "observer type": 0 undefined, 1 "multiple" (`entryTypes`), 2 "single" (`type`). Set by the
    /// first `observe()` and kept for good -- `disconnect()` does not reset it (Chrome 150: an observer that
    /// once observed `{type}` refuses `{entryTypes}` after a disconnect).
    mode: std::cell::Cell<u8>,
    /// "Requires dropped entries": set by every `observe()`, cleared by the delivery after it, which hands the
    /// callback `{droppedEntriesCount}` (later ones `{}`).
    requires_dropped: std::cell::Cell<bool>,
}

pub(crate) trait PerformanceObserverBehavior {
   fn construct() -> PerformanceObserver where Self: Sized { stub!("PerformanceObserver::constructor") }
}

#[webapi(base, own)]
impl PerformanceObserver {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<PerformanceObserver, ::deno_error::JsErrorBox> { dispatch!("PerformanceObserver::constructor", <PerformanceObserver as PerformanceObserverBehavior>::construct()) }
}

impl PerformanceObserverBehavior for PerformanceObserver {
    fn construct() -> PerformanceObserver {
        PerformanceObserver {
            cb: std::cell::RefCell::new(None),
            types: std::cell::RefCell::new(Vec::new()),
            perf: std::cell::RefCell::new(None),
            pending: std::cell::RefCell::new(Vec::new()),
            scheduled: std::cell::Cell::new(false),
            mode: std::cell::Cell::new(0),
            requires_dropped: std::cell::Cell::new(false),
        }
    }
}

/// The list a `PerformanceObserver` callback receives.
///
/// The timeline used to hand the callback a plain three-key object literal
/// (`{ getEntries, getEntriesByType, getEntriesByName }`) built in `host.js`/`worker-host.js`, so the
/// interface's own three rows were D2 stubs on a prototype no object had ever worn. This is the real
/// thing: a cppgc value holding the delivered entries, wearing the table's prototype, so
/// `list.getEntries()` runs the table's row and `PerformanceObserverEntryList.prototype.getEntries
/// .call({})` throws — the bundle's oracle records exactly that TypeError for all three members.
#[repr(C)]
#[derive(CppgcBase)]
pub struct PerformanceObserverEntryList {
    entries: std::cell::RefCell<Vec<v8::Global<v8::Value>>>,
}

pub(crate) trait PerformanceObserverEntryListBehavior {
   fn construct() -> PerformanceObserverEntryList where Self: Sized { stub!("PerformanceObserverEntryList::constructor") }
}

#[webapi(base, own)]
impl PerformanceObserverEntryList {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<PerformanceObserverEntryList, ::deno_error::JsErrorBox> { dispatch!("PerformanceObserverEntryList::constructor", <PerformanceObserverEntryList as PerformanceObserverEntryListBehavior>::construct()) }
}

impl PerformanceObserverEntryListBehavior for PerformanceObserverEntryList {
    fn construct() -> PerformanceObserverEntryList { PerformanceObserverEntryList { entries: std::cell::RefCell::new(Vec::new()) } }
}

// ── PerformanceObserver ─────────────────────────────────────────────────────────────────────────

/// `new PerformanceObserver(cb)` — the JS factory body, in Rust.
///
/// `KB.__makePerformanceObserver(perf)` closed over the realm's timeline and bound it into each fresh
/// observer through `KB.__poInit`. There is nothing to close over: `globalThis.performance` IS the
/// current realm's timeline at the moment the page constructs an observer -- the installer assigns it
/// before any page script runs, in both realms -- and the observer keeps a handle to it, so
/// `observe()` reaches that realm's entry buffer without a per-realm side table.
pub fn cb_po_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("PerformanceObserver"));
        return;
    }
    // WebIDL's argument count before the callback's type (Chrome 150: "1 argument required, but only 0 present.")
    if !crate::install::webidl::require_args(scope, &args, "PerformanceObserver", 1) {
        return;
    }
    let cb = args.get(0);
    if !cb.is_function() {
        throw_type_error(scope, "Failed to construct 'PerformanceObserver': parameter 1 is not of type 'Function'.");
        return;
    }
    let obj = crate::binding::class::mint(scope, <PerformanceObserver as PerformanceObserverBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "PerformanceObserver"),
    }
    if let Some(po) = deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, obj.into()) {
        *unsafe { po.as_ref() }.cb.borrow_mut() = Some(v8::Global::new(scope, cb));
        // the realm's own `performance` (its sealed getter), never a page's replacement
        let perf = crate::binding::protos::realm_self_get(scope, "performance");
        *unsafe { po.as_ref() }.perf.borrow_mut() = match perf {
            Some(p) if p.is_object() => Some(v8::Global::new(scope, p)),
            _ => None,
        };
    }
    rv.set(obj.into());
}

/// `observer.observe(options)`.
///
/// The row is `length 0` / `required 0` (the capture's, and WebIDL's — the argument is
/// `optional PerformanceObserverInit options = {}`), so no arity guard runs and the WebIDL check is
/// the body's: Blink rejects an `observe()` that names neither `entryTypes` nor `type`, and the
/// bundle's oracle records the TypeError. The JS this replaces got that throw from an arity guard
/// installed with `need 1, len 1`, which also made `observe.length` read 1 where the capture says 0.
fn cb_po_observe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    const PFX: &str = "Failed to execute 'observe' on 'PerformanceObserver': ";
    let this_v: v8::Local<v8::Value> = args.this().into();
    let po = match deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, this_v) {
        Some(p) => p,
        None => { throw_type_error(scope, "Failed to execute 'observe' on 'PerformanceObserver': Illegal invocation"); return; }
    };
    // WebIDL `PerformanceObserverInit`: every member read ONCE, in code-point order, each converted as it is
    // read -- Chrome 150 through a logging Proxy: `buffered, durationThreshold, entryTypes, type`. It read
    // `entryTypes, type, buffered, type`, and took only a real Array for the sequence (a `Set` was "neither").
    let init = args.get(0);
    let mut buffered = false;
    let mut entry_types: Option<Vec<String>> = None;
    let mut single: Option<String> = None;
    if !init.is_null_or_undefined() {
        let Ok(o) = v8::Local::<v8::Object>::try_from(init) else {
            throw_type_error(scope, &format!("{PFX}The provided value is not of type 'PerformanceObserverInit'."));
            return;
        };
        let Some(b) = get_prop(scope, o, "buffered") else { return };
        buffered = b.boolean_value(scope);
        let Some(dt) = get_prop(scope, o, "durationThreshold") else { return };
        if !dt.is_undefined() && dt.number_value(scope).is_none() {
            return;
        }
        let Some(et) = get_prop(scope, o, "entryTypes") else { return };
        if !et.is_undefined() {
            let at = format!("{PFX}Failed to read the 'entryTypes' property from 'PerformanceObserverInit': ");
            match string_sequence(scope, et, &at) {
                Some(v) => entry_types = Some(v),
                None => return,
            }
        }
        let Some(t) = get_prop(scope, o, "type") else { return };
        if !t.is_undefined() {
            let Some(ts) = t.to_string(scope) else { return };
            single = Some(ts.to_rust_string_lossy(scope));
        }
    }
    // Performance Timeline's observe() steps, in Blink's order and words.
    if entry_types.is_none() && single.is_none() {
        throw_type_error(scope, &format!("{PFX}An observe() call must include either entryTypes or type arguments."));
        return;
    }
    if entry_types.is_some() && single.is_some() {
        throw_type_error(scope, &format!("{PFX}An observe() call must not include both entryTypes and type arguments."));
        return;
    }
    let me = unsafe { po.as_ref() };
    match (me.mode.get(), entry_types.is_some()) {
        (2, true) => {
            throw_dom(scope, &format!("{PFX}This PerformanceObserver has performed observe({{type:...}}, therefore it cannot perform observe({{entryTypes:...}})"), "InvalidModificationError");
            return;
        }
        (1, false) => {
            throw_dom(scope, &format!("{PFX}This observer has performed observe({{entryTypes:...}}, therefore it cannot perform observe({{type:...}})"), "InvalidModificationError");
            return;
        }
        _ => {}
    }
    me.mode.set(if entry_types.is_some() { 1 } else { 2 });
    me.requires_dropped.set(true);
    let supported = supported_types(scope);
    match &entry_types {
        // "multiple": the call's supported types REPLACE the observed set
        Some(list) => {
            let mut t = me.types.borrow_mut();
            t.clear();
            for ty in list {
                if supported.contains(&ty.as_str()) && !t.contains(ty) {
                    t.push(ty.clone());
                }
            }
        }
        // "single": one more observed type, if this realm supports it
        None => {
            let ty = single.clone().unwrap_or_default();
            if !supported.contains(&ty.as_str()) {
                return;
            }
            let mut t = me.types.borrow_mut();
            if !t.contains(&ty) {
                t.push(ty);
            }
        }
    }
    // The observer list lives on the TIMELINE (`core/timing/performance.rs`'s cppgc `Performance`),
    // reached through the observer's own `perf` field. It used to be `performance._observers` -- an
    // own member of the object literal, and one no real Performance has -- and then the
    // `KB.__perfObs` bridge slot, which `bridge_get` resolved per realm so that a Worker's timeline
    // found the Worker's own list. With one cppgc timeline per realm the list is simply a field of
    // the receiver, and the bridge slot is deleted (invariant I3).
    let perf = me.perf.borrow().clone();
    if let Some(g) = perf {
        let p = v8::Local::new(scope, &g);
        crate::core::timing::performance::add_observer(scope, p, this_v);
        // `buffered: true` with a single `type`: the matching entries already in the buffer are queued
        // to this observer now (measured for `resource`).
        if let (true, Some(ty)) = (buffered, single) {
            let found: Vec<v8::Global<v8::Object>> = crate::core::timing::performance::buffered_entries(scope, p, &ty).into_iter().map(|e| v8::Global::new(scope, e)).collect();
            for g in found {
                let e = v8::Local::new(scope, &g);
                let obs = v8::Local::new(scope, v8::Global::new(scope, this_v));
                queue_entry(scope, obs, e, &ty);
            }
        }
    }
}

/// WebIDL `sequence<DOMString>`: the iterator protocol over any iterable (a `Set` included), each item
/// `ToString`ed; Blink's sentences prefixed by `at` for a value that is not one. `None` with an exception
/// pending.
pub(crate) fn string_sequence(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, at: &str) -> Option<Vec<String>> {
    let not_seq = |scope: &mut v8::PinScope| throw_type_error(scope, &format!("{at}The provided value cannot be converted to a sequence."));
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        not_seq(scope);
        return None;
    };
    let sym = v8::Symbol::get_iterator(scope);
    let method = obj.get(scope, sym.into())?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
        not_seq(scope);
        return None;
    };
    let iter = method.call(scope, obj.into(), &[])?;
    let Ok(iter) = v8::Local::<v8::Object>::try_from(iter) else {
        not_seq(scope);
        return None;
    };
    let next = get_prop(scope, iter, "next")?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
        not_seq(scope);
        return None;
    };
    let mut out = Vec::new();
    loop {
        let r = next.call(scope, iter.into(), &[])?;
        let Ok(r) = v8::Local::<v8::Object>::try_from(r) else {
            not_seq(scope);
            return None;
        };
        if get_prop(scope, r, "done")?.boolean_value(scope) {
            break;
        }
        let x = get_prop(scope, r, "value")?;
        let s = x.to_string(scope)?;
        out.push(s.to_rust_string_lossy(scope));
    }
    Some(out)
}
fn cb_po_disconnect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this_v: v8::Local<v8::Value> = args.this().into();
    let po = match deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, this_v) {
        Some(p) => p,
        None => { throw_type_error(scope, "Failed to execute 'disconnect' on 'PerformanceObserver': Illegal invocation"); return; }
    };
    // "remove this from the registered observers, empty its buffer" -- the observer type stays
    unsafe { po.as_ref() }.pending.borrow_mut().clear();
    unsafe { po.as_ref() }.types.borrow_mut().clear();
    let perf = unsafe { po.as_ref() }.perf.borrow().clone();
    if let Some(g) = perf {
        let p = v8::Local::new(scope, &g);
        crate::core::timing::performance::remove_observer(scope, p, this_v);
    }
    unsafe { po.as_ref() }.types.borrow_mut().clear();
    unsafe { po.as_ref() }.pending.borrow_mut().clear();
}
/// `takeRecords()`: the observer's buffer, emptied -- the entries a delivery has not taken yet (it answered
/// `[]` always; Chrome 150 hands back the mark made one statement earlier).
fn cb_po_take_records(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(po) = deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, args.this().into()) else {
        throw_type_error(scope, "Failed to execute 'takeRecords' on 'PerformanceObserver': Illegal invocation");
        return;
    };
    let taken: Vec<v8::Global<v8::Value>> = std::mem::take(&mut *unsafe { po.as_ref() }.pending.borrow_mut());
    let items: Vec<v8::Local<v8::Value>> = taken.iter().map(|g| v8::Local::new(scope, g)).collect();
    let a = v8::Array::new_with_elements(scope, &items);
    rv.set(a.into());
}

/// `PerformanceObserver.supportedEntryTypes` — the one static, and the one member of the interface
/// that is a plain fact about the build rather than about an observer: every entry type whose entry
/// interface the realm exposes, in code-point order. Blink lists a type when its runtime feature is on
/// and the realm can produce it, which the realm's surface already records: Chrome 150 has no
/// `InteractionContentfulPaint` / `PerformanceSoftNavigation` and lists 13 types where Brave 151 lists
/// 15, and a worker exposes only the mark, measure and resource interfaces and lists those three
/// (measured, Chrome 150 headless, 2026-10-07; korvus-analysis U-4 -- the list was Brave's for every
/// persona and realm). Frozen, as Blink's is.
///
/// AND THE SAME ARRAY ON EVERY READ: `[SameObject, SaveSameObject] static readonly attribute FrozenArray<DOMString>`
/// -- the bindings keep the first array on the receiver, the interface object (`install::frozen_array`). This made a
/// new one per read, on the belief that Blink does; Chrome 150 answers `PerformanceObserver.supportedEntryTypes ===
/// PerformanceObserver.supportedEntryTypes` true in a window and in a worker (measured, C2).
fn cb_po_supported_entry_types(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let answer = crate::install::frozen_array::answer(scope, args.this(), "PerformanceObserver", "supportedEntryTypes", |_| 0.0, |scope| {
        let types = supported_types(scope);
        Some(types.iter().map(|t| str_val(scope, t)).collect())
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}

/// The entry types this realm supports, in code-point order ([`cb_po_supported_entry_types`]'s list, and the
/// filter `observe()` applies).
fn supported_types(scope: &mut v8::PinScope) -> Vec<&'static str> {
    const BY_INTERFACE: &[(&str, &str)] = &[
        ("element", "PerformanceElementTiming"),
        ("event", "PerformanceEventTiming"),
        ("first-input", "PerformanceEventTiming"),
        ("interaction-contentful-paint", "InteractionContentfulPaint"),
        ("largest-contentful-paint", "LargestContentfulPaint"),
        ("layout-shift", "LayoutShift"),
        ("long-animation-frame", "PerformanceLongAnimationFrameTiming"),
        ("longtask", "PerformanceLongTaskTiming"),
        ("mark", "PerformanceMark"),
        ("measure", "PerformanceMeasure"),
        ("navigation", "PerformanceNavigationTiming"),
        ("paint", "PerformancePaintTiming"),
        ("resource", "PerformanceResourceTiming"),
        ("soft-navigation", "PerformanceSoftNavigation"),
        ("visibility-state", "VisibilityStateEntry"),
    ];
    BY_INTERFACE.iter().filter(|(_, i)| crate::install::surface::publishes_interface(scope, i)).map(|(t, _)| *t).collect()
}

/// Queue one entry for `obs` if it observes that type, and schedule the delivery task.
///
/// Called by the timeline (`core/timing/performance.rs`'s `notify`) for every observer of the realm.
/// This was the JS `notify()` loop in `host.js` and its duplicate in `worker-host.js`, which read the
/// observed types back through the `KB.__poTypes` bridge slot and the page callback through
/// `KB.__poCb`; both slots are deleted with it.
pub(crate) fn queue_entry<'s>(scope: &mut v8::PinScope<'s, '_>, obs: v8::Local<'s, v8::Value>, entry: v8::Local<v8::Object>, entry_type: &str) {
    let Some(po) = deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, obs) else { return };
    if !unsafe { po.as_ref() }.types.borrow().iter().any(|t| t == entry_type) {
        return;
    }
    let ev: v8::Local<v8::Value> = entry.into();
    unsafe { po.as_ref() }.pending.borrow_mut().push(v8::Global::new(scope, ev));
    if unsafe { po.as_ref() }.scheduled.get() {
        return;
    }
    unsafe { po.as_ref() }.scheduled.set(true);
    // ONE delivery per observer per task, which is why entries marked in the same synchronous block
    // arrive in a single callback (the `performance-worker` fixture asserts exactly two). The task is
    // on the realm's OWN timer task source, so a Worker's callback fires on the Worker's scheduler --
    // queued through the engine's timer, not the page's `setTimeout`, which it may replace.
    // A realm with no timer (a bare worker) simply never delivers, as before.
    let flush = build_fn_data(scope, cb_po_flush, "", 0, obs);
    crate::core::intersection_observer::rendering::engine_task(scope, flush, 0.0);
}

/// The delivery task: hand the page's callback a real `PerformanceObserverEntryList` over the queued
/// entries, `callback(list, observer)`.
///
/// The list is the table's own interface (the table's prototype, the table's rows), where the JS
/// handed the callback a plain three-key object literal and the interface's three rows were D2 stubs
/// on a prototype no object had ever worn.
fn cb_po_flush(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let observer = crate::binding::protos::body_data(scope, &args);
    let Some(po) = deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserver>(scope, observer) else { return };
    unsafe { po.as_ref() }.scheduled.set(false);
    let entries: Vec<v8::Global<v8::Value>> = std::mem::take(&mut *unsafe { po.as_ref() }.pending.borrow_mut());
    if entries.is_empty() {
        return;
    }
    let Some(cb) = unsafe { po.as_ref() }.cb.borrow().clone() else { return };
    let Ok(page_cb) = v8::Local::<v8::Function>::try_from(v8::Local::new(scope, &cb)) else { return };
    let list = crate::binding::class::mint(scope, PerformanceObserverEntryList { entries: std::cell::RefCell::new(entries) });
    set_table_proto(scope, list, "PerformanceObserverEntryList");
    let lv: v8::Local<v8::Value> = list.into();
    // WebIDL "invoke" with the observer as the callback this value and `PerformanceObserverCallbackOptions`
    // third: `{droppedEntriesCount}` on the first delivery after an `observe()`, `{}` after (Chrome 150; it
    // was `callback(list, observer)` with this undefined -- korvus-analysis `ka`, 2026-10-09). No entry is ever
    // dropped from a buffer this engine keeps, so the count is 0.
    let opts = v8::Object::new(scope);
    if unsafe { po.as_ref() }.requires_dropped.replace(false) {
        let k = v8::String::new(scope, "droppedEntriesCount").unwrap();
        let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
        opts.create_data_property(scope, k.into(), zero);
    }
    let ov: v8::Local<v8::Value> = opts.into();
    // A frame's observer runs as the frame's realm.
    crate::core::html::browsing_context::as_callback_realm(scope, page_cb, |scope| {
        let _below = crate::core::html::task_queue::ScriptBelow::enter();
        #[cfg(feature = "analysis")]
        crate::analysis_hook::call_traced(scope, "PerformanceObserverCallback", page_cb, observer, &[lv, observer, ov]);
        #[cfg(not(feature = "analysis"))]
        page_cb.call(scope, observer, &[lv, observer, ov]);
    });
    // its microtasks before the next observer's callback (HTML "clean up after running a callback")
    crate::core::html::task_queue::clean_up_after_running_callback(scope);
}

/// The body every implemented row of [`PERFORMANCEOBSERVER`] names.
pub fn install_performance_observer(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "disconnect" => method_row(scope, holder, member, cb_po_disconnect),
        "observe" => method_row(scope, holder, member, cb_po_observe),
        "takeRecords" => method_row(scope, holder, member, cb_po_take_records),
        // The one static: `holder` is the interface object, not the prototype.
        "supportedEntryTypes" => crate::install::live::getter_row(scope, holder, member, cb_po_supported_entry_types),
        other => debug_assert!(false, "install_performance_observer: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `PerformanceObserver` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order. `[value, both]`: the capture records
// `newable: false` for the interface (`new PerformanceObserver()` with no callback throws, which is
// what "not newable" means to the capture rig) and both `surface.json` and `worker.surface.json`
// list it. `install::live::install_performance_observers` builds the interface object over `cb_po_ctor`.
webapi::interface! {
    "PerformanceObserver" [value, both] {
        ctor          "PerformanceObserver"               len 1 = stub,
        method        "disconnect"          : Object len 0       = install_performance_observer,
        method        "observe"             : Object len 0       = install_performance_observer,
        method        "takeRecords"         : Object len 0       = install_performance_observer,
        static_attr   "supportedEntryTypes" : Object readonly    = install_performance_observer
    }
}

// ── PerformanceObserverEntryList ────────────────────────────────────────────────────────────────

/// The entries this list holds, as a fresh JS array.
fn pel_entries<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, member: &str) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceObserverEntryList>(scope, this) {
        Some(l) => {
            let held = unsafe { l.as_ref() }.entries.borrow().clone();
            Some(held.iter().map(|g| v8::Local::new(scope, g)).collect())
        }
        None => {
            throw_type_error(scope, &format!("Failed to execute '{member}' on 'PerformanceObserverEntryList': Illegal invocation"));
            None
        }
    }
}

fn set_array<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[v8::Local<'s, v8::Value>], rv: &mut v8::ReturnValue) {
    let arr = v8::Array::new(scope, items.len() as i32);
    for (i, v) in items.iter().enumerate() { arr.set_index(scope, i as u32, *v); }
    rv.set(arr.into());
}

fn cb_pel_get_entries(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let items = match pel_entries(scope, args.this().into(), "getEntries") { Some(v) => v, None => return };
    set_array(scope, &items, &mut rv);
}

/// `getEntriesByType(type)` / `getEntriesByName(name)` — the two filters the timeline's object literal
/// carried as JS arrows. Each reads the entry's own `entryType` / `name` accessor, exactly as the
/// arrow did.
fn pel_filter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, key: &str, member: &str) {
    let items = match pel_entries(scope, args.this().into(), member) { Some(v) => v, None => return };
    let want = args.get(0).to_rust_string_lossy(scope);
    let mut out: Vec<v8::Local<v8::Value>> = Vec::new();
    for e in items {
        let got = v8::Local::<v8::Object>::try_from(e).ok().and_then(|o| get_prop(scope, o, key));
        if got.map(|v| v.to_rust_string_lossy(scope)) == Some(want.clone()) { out.push(e); }
    }
    set_array(scope, &out, &mut rv);
}
fn cb_pel_get_entries_by_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    pel_filter(scope, args, rv, "entryType", "getEntriesByType");
}
fn cb_pel_get_entries_by_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    pel_filter(scope, args, rv, "name", "getEntriesByName");
}

/// The body every row of [`PERFORMANCEOBSERVERENTRYLIST`] names.
pub fn install_performance_observer_entry_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getEntries" => method_row(scope, proto, member, cb_pel_get_entries),
        "getEntriesByName" => method_row(scope, proto, member, cb_pel_get_entries_by_name),
        "getEntriesByType" => method_row(scope, proto, member, cb_pel_get_entries_by_type),
        other => debug_assert!(false, "install_performance_observer_entry_list: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `PerformanceObserverEntryList` block of `install/generated_registry.rs`
// (identical to `generated_worker_registry.rs`'s), in the capture's order. `[interface, both]`: the
// capture records `newable: false`, so the installer's "Illegal constructor" interface object is the
// right one -- only a delivery mints a list.
webapi::interface! {
    "PerformanceObserverEntryList" [interface, both] {
        ctor   "PerformanceObserverEntryList"        len 0            = stub,
        method "getEntries"       : Object len 0                      = install_performance_observer_entry_list,
        method "getEntriesByName" : Object len 1 required 1           = install_performance_observer_entry_list,
        method "getEntriesByType" : Object len 1 required 1           = install_performance_observer_entry_list
    }
}

// This file declares no `webapi_ops!`: the six `op_po_*` builders it used to export existed only so
// that JS could ask for a native function by name, and every caller is gone. So are the last two
// bridge names -- `__poTypes` and `__poCb` -- which existed only for the JS timeline W4.d deleted.


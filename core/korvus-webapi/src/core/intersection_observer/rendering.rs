//! `core/intersection_observer/rendering.rs` -- the rendering opportunity: HTML's "update the rendering"
//! steps that belong to the two geometry observers, and the frame that runs them.
//!
//! # What was wrong
//!
//! `IntersectionObserver.observe` and `ResizeObserver.observe` were empty bodies. Nothing ever called a
//! callback back, so every page that waits for an observation waited forever: lazy sections, infinite
//! lists, prefetch-on-visible, sticky headers, chart libraries that wait for a size. L19 measured it on
//! 13 of 50 `shape` rows (11 alone) -- relay.dev renders 0 prefetch links where Chrome renders 266.
//!
//! # The frame
//!
//! Chrome runs, in one rendering task: the animation-frame callbacks, then the resize-observer loop,
//! then the intersection computation, whose callbacks are delivered right after. Measured on Chrome 150
//! (`tests/fixtures/p12c/chrome-150.order.txt`):
//!
//! ```text
//! sync, microtask, timeout0, rafA, rafB, ro, ro-microtask, io, io-microtask, io, io-microtask, io-timeout0, io-raf
//! ```
//!
//! This engine has no compositor; a frame is a task on the realm's own timer task source, at the same
//! cadence `requestAnimationFrame` uses ([`FRAME_MS`]). Three things make the order the browser's:
//!
//! * the frame is ARMED FROM A MICROTASK queued by `observe()`, so an animation frame requested later in
//!   the same script is armed before it and runs first (`rafB` above);
//! * resize-observer callbacks run inside the frame task, so their microtasks drain before any
//!   intersection callback;
//! * each intersection observer's callback is its OWN task, all queued together when the frame computes
//!   them, so one callback's microtasks drain before the next observer's callback and a `setTimeout(0)`
//!   queued from a callback runs after every observer has been told.
//!
//! The timer is the realm's PRISTINE `setTimeout` -- a fresh native one from the timer factory, never
//! the page's global -- so a page that wraps or fakes its timers neither sees these calls nor stalls
//! them.
//!
//! # What it costs
//!
//! Nothing, on a page with no observers: no frame is ever armed, and [`rendering_opportunity`] answers
//! from one `OpState` lookup. On a page WITH observers, a frame runs only when something could have
//! changed an answer: an observation was added, the previous frame delivered or skipped something, or
//! the GEOMETRY KEY moved -- the layout fingerprint (DOM revision, sheets, viewport, animations) plus
//! every scroll offset an observation depends on. When a frame finds nothing moved, no next frame is
//! armed, so an observed page that has settled leaves the event loop idle and `Engine::pump` can report
//! it drained. A change made after that -- a late timer, a CDP scroll -- is picked up at the next
//! rendering opportunity the HOST offers, which is `Engine::pump` calling [`rendering_opportunity`]:
//! it arms a frame only when the key moved.
//!
//! # Geometry
//!
//! `korvus-page` lays the document out in document coordinates; scrolling is this crate's state
//! (`window_geometry::ScrollPosition` for the document, `Element.scrollTop` for boxes). A client rect is
//! therefore the laid-out box less the scroll offsets of every scroll container above it, and less the
//! document's own offset unless the box is inside a `position: fixed` subtree. `LaidOut` exposes no
//! content or padding box; the resize observer reads the used padding and border widths through the
//! same resolved-value path `getComputedStyle` answers from.

use std::collections::HashMap;

use deno_core::{v8, OpState};

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{build_fn, build_fn_data, get_prop, global_get, str_val};
use crate::core::dom::geometry_queries::{refresh_layout_cache, LayoutCache};

/// Milliseconds from now to the next vsync -- the frame `requestAnimationFrame` callbacks run in
/// (`core/html/animation_frame.rs`). Frames fall on the display's refresh grid, a tick every
/// `1000 / refresh_rate` ms (the persona's `screen.refreshRate`, 60 Hz when not captured), as a
/// browser's begin-frames do: Chrome 150 here runs rAF every 16.7 ms (measured 2026-10-08), where a
/// fixed 16 ms timer from the request ran this engine's at 17.3.
/// The refresh interval of the persona's display, in ms.
pub(crate) fn frame_interval_ms(scope: &mut v8::PinScope) -> f64 {
    let hz = crate::core::navigator::navigator::persona(scope).map(|p| p.screen().refresh_rate).filter(|r| r.is_finite() && *r > 0.0).unwrap_or(60.0);
    1000.0 / hz
}

/// [`next_frame_ms`] for the runtime: the pump wakes on this grid to offer a frame-wanting plugin its frames
/// (`Engine::pump_slice`), on the same vsync the page's own frames fall on.
pub fn next_frame_in_ms(scope: &mut v8::PinScope) -> f64 {
    next_frame_ms(scope)
}

pub(crate) fn next_frame_ms(scope: &mut v8::PinScope) -> f64 {
    static VSYNC_ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let hz = crate::core::navigator::navigator::persona(scope).map(|p| p.screen().refresh_rate).filter(|r| r.is_finite() && *r > 0.0).unwrap_or(60.0);
    let period = 1000.0 / hz;
    let t = VSYNC_ORIGIN.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0;
    (t / period).floor() * period + period - t
}

/// A rectangle in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub const ZERO: R = R { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };

    /// The EDGE-INCLUSIVE intersection: two boxes that only touch still intersect (with zero area),
    /// which is how an intersection observer tells "adjacent" from "apart".
    pub fn intersect(self, o: R) -> Option<R> {
        let x1 = self.x.max(o.x);
        let y1 = self.y.max(o.y);
        let x2 = (self.x + self.w).min(o.x + o.w);
        let y2 = (self.y + self.h).min(o.y + o.h);
        (x2 >= x1 && y2 >= y1).then_some(R { x: x1, y: y1, w: x2 - x1, h: y2 - y1 })
    }

    pub fn area(self) -> f64 {
        self.w * self.h
    }
}

/// One intersection observation: a target and the last answer the observer was told.
pub(crate) struct IoObservation {
    pub node: u32,
    /// -1 until the first computation, so the first one always notifies.
    pub prev_index: i64,
    pub prev_intersecting: bool,
}

pub(crate) struct IoRegistration {
    pub id: u64,
    pub observer: v8::Global<v8::Object>,
    pub observations: Vec<IoObservation>,
    pub records: Vec<v8::Global<v8::Object>>,
    /// A delivery task is queued and has not run yet.
    pub delivery_queued: bool,
}

/// Which box a resize observation measures.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BoxKind {
    Content,
    Border,
    DevicePixelContent,
}

pub(crate) struct RoObservation {
    pub node: u32,
    pub box_kind: BoxKind,
    /// `(inline, block)` last reported; `(-1, -1)` before the first, which is why a zero-sized target
    /// still gets its initial callback (Chrome 150, measured: `display: none`, detached and inline
    /// targets all report `0x0` once).
    pub last: (f64, f64),
}

pub(crate) struct RoRegistration {
    pub id: u64,
    pub observer: v8::Global<v8::Object>,
    pub observations: Vec<RoObservation>,
}

/// The realm's observers and its frame, in `OpState`.
#[derive(Default)]
pub(crate) struct RenderingState {
    pub next_id: u64,
    pub io: Vec<IoRegistration>,
    pub ro: Vec<RoRegistration>,
    /// A frame is requested or armed and has not run.
    pub frame_pending: bool,
    /// The geometry key the last frame (or opportunity) computed from.
    pub last_key: Option<u64>,
    /// The scroll containers the last intersection computation read an offset from; their offsets
    /// are part of the key.
    pub scrollers: Vec<u32>,
    /// The page `document` these registrations belong to. A navigation replaces the DOM (and every
    /// node id in it) under the same realm, so a registry kept across one would hand the new document
    /// callbacks about nodes that no longer exist.
    pub document: Option<v8::Global<v8::Value>>,
    /// The pristine timer, once made.
    pub set_timeout: Option<v8::Global<v8::Function>>,
    /// The kernel's engine-task entry (`queueEngineTask`), cached like `set_timeout`.
    pub queue_task: Option<v8::Global<v8::Function>>,
    /// The kernel's entries for the rendering frame (`queueFrameTask`) and idle tasks (`queueIdleTask`), likewise.
    pub queue_frame: Option<v8::Global<v8::Function>>,
    pub queue_idle: Option<v8::Global<v8::Function>>,
    /// When the last frame ran (the realm's clock): the idle period right after one ends at the next vsync (Blink's
    /// short idle period).
    pub last_frame_at: Option<f64>,
    /// The page realm's OWN timer members by name (`setTimeout`, `setInterval`), for a frame realm's
    /// timers to forward to without reading the page's -- replaceable -- `window.setTimeout`.
    pub window_fns: std::collections::HashMap<String, v8::Global<v8::Function>>,
    /// The computed answers the observer passes read, kept from one pass to the next ([`StyleMemo`]).
    pub style_memo: StyleMemo,
}

/// The computed-style answers an observer pass reads per element -- `position` is `fixed`, an `overflow` is not
/// `visible`, `display` is `inline` -- kept ACROSS passes while the layout cache is keyed as it was when they were
/// read ([`Geometry::layout`]).
///
/// A frame's passes asked them again for every target's every ancestor: about three `getComputedStyle`-path reads
/// per ancestor, each a media context, an `Arc` clone and a memo lock -- and, on a frame that moved the DOM
/// revision without moving a box (detached churn), a fresh cascade, since the page's own style memo is keyed on
/// that revision. The cache's key is the page's geometry fingerprint, which moves with every input these three
/// properties of a page-connected element have: every change to the page's documents and shadow trees (less the
/// `style` rewrites that change only the transform family and `opacity`), the sheets, the viewport and theme,
/// the container sizes, and every layout-affecting animated value. An entry is read and stored only while the
/// pass's layout is fresh -- refreshed, with no style flush since -- so the key it is checked against is the
/// state it describes.
#[derive(Default)]
pub(crate) struct StyleMemo {
    /// `(Dom::instance, the layout cache's fingerprint)` the entries were read under.
    key: Option<(u64, u64)>,
    fixed: HashMap<u32, bool>,
    overflows: HashMap<u32, bool>,
    inline: HashMap<u32, bool>,
}

impl RenderingState {
    pub fn is_empty(&self) -> bool {
        self.io.is_empty() && self.ro.is_empty()
    }
}

pub(crate) fn with_state<T>(scope: &mut v8::PinScope, f: impl FnOnce(&mut RenderingState) -> T) -> T {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<RenderingState>().is_none() {
        st.put(RenderingState::default());
    }
    f(st.borrow_mut::<RenderingState>())
}

pub(crate) fn next_id(scope: &mut v8::PinScope) -> u64 {
    with_state(scope, |s| {
        s.next_id += 1;
        s.next_id
    })
}

/// Drop every registration that belongs to a document this realm no longer shows.
///
/// The PAGE's realm only: the rendering state is the runtime's, shared by every same-process frame, and a frame's
/// realm holds a different `document` -- asked there, the page's looked stale. An `observe()` in a frame, or a frame
/// task a frame's `requestAnimationFrame` built, then dropped every observer of the page and the pending frame's
/// flag, and a second chain of frames began beside the first: minea's frames ran rAF every 4-7 ms (Chrome: 16.7).
fn forget_stale_document(scope: &mut v8::PinScope) {
    if crate::core::html::browsing_context::realm_ctx(scope).is_some() {
        return;
    }
    // The window's `document` accessor, not the memoised `document_val`: the memo outlives a
    // navigation, the accessor answers the document `install_document` minted for it.
    let current = global_get(scope, "document").filter(|v| v.is_object());
    let stored = with_state(scope, |s| s.document.clone());
    let same = match (&stored, current) {
        (Some(g), Some(c)) => v8::Local::new(scope, g).strict_equals(c),
        (None, _) => true,
        _ => false,
    };
    if !same {
        with_state(scope, |s| {
            s.io.clear();
            s.ro.clear();
            s.last_key = None;
            s.scrollers.clear();
            s.document = None;
            // A frame armed for the old document may never run (its timer belonged to that page).
            s.frame_pending = false;
        });
    }
    if with_state(scope, |s| s.document.is_none()) {
        if let Some(c) = global_get(scope, "document").filter(|v| v.is_object()) {
            let g = v8::Global::new(scope, c);
            with_state(scope, |s| s.document = Some(g));
        }
    }
}

/// Called by every `observe()`: note the document the registration belongs to.
pub(crate) fn note_document(scope: &mut v8::PinScope) {
    forget_stale_document(scope);
}

// ── the timer ────────────────────────────────────────────────────────────────────────────────────

/// A native `setTimeout` from the realm's timer factory, made once. Not the page's global: see the
/// module header. The window's factory, else a worker's (a worker's MessagePort and
/// IndexedDB tasks need it too). `None` in a realm with neither, where no frame can be scheduled at all.
fn pristine_queue_task<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Function>> {
    if let Some(g) = with_state(scope, |s| s.queue_task.clone()) {
        return Some(v8::Local::new(scope, &g));
    }
    let factory = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_window_scope_fn")
        .or_else(|| crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_worker_scope_fn"))?;
    let name = str_val(scope, "queueEngineTask");
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, 2).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let g = v8::Global::new(scope, f);
    with_state(scope, |s| s.queue_task = Some(g));
    Some(f)
}

/// The kernel's entry `name` (`queueFrameTask`, `queueIdleTask`), made once from the realm's timer factory.
fn kernel_entry<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, slot: fn(&mut RenderingState) -> &mut Option<v8::Global<v8::Function>>) -> Option<v8::Local<'s, v8::Function>> {
    if let Some(g) = with_state(scope, |s| slot(s).clone()) {
        return Some(v8::Local::new(scope, &g));
    }
    let factory = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_window_scope_fn")
        .or_else(|| crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_worker_scope_fn"))?;
    let n = str_val(scope, name);
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, 2).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[n, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let g = v8::Global::new(scope, f);
    with_state(scope, |s| *slot(s) = Some(g));
    Some(f)
}

/// Queue the rendering frame `ms` from now: the kernel's frame entry, which idle periods wait for (the
/// runtime's `Scheduler::queue_frame_task`); an engine task where the realm has none.
fn queue_frame(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ms: f64) {
    let Some(q) = kernel_entry(scope, "queueFrameTask", |s| &mut s.queue_frame) else {
        queue_task(scope, f, ms);
        return;
    };
    let global = scope.get_current_context().global(scope);
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, ms).into();
    v8::tc_scope!(let tc, scope);
    q.call(tc, global.into(), &[f.into(), delay]);
    tc.reset();
}

/// Queue the idle task `f`, to run in an idle period no sooner than `not_before_ms` from now (the runtime's
/// `Scheduler::queue_idle_task`; `f` is called with the period's end on the process clock). An engine task
/// `not_before_ms` out where the realm has no idle entry.
pub(crate) fn queue_idle(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, not_before_ms: f64) {
    let Some(q) = kernel_entry(scope, "queueIdleTask", |s| &mut s.queue_idle) else {
        queue_task(scope, f, not_before_ms);
        return;
    };
    let global = scope.get_current_context().global(scope);
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, not_before_ms).into();
    v8::tc_scope!(let tc, scope);
    q.call(tc, global.into(), &[f.into(), delay]);
    tc.reset();
}

/// When the last rendering frame ran, on the current realm's clock.
pub(crate) fn last_frame_at(scope: &mut v8::PinScope) -> Option<f64> {
    with_state(scope, |s| s.last_frame_at)
}

fn pristine_set_timeout<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Function>> {
    if let Some(g) = with_state(scope, |s| s.set_timeout.clone()) {
        return Some(v8::Local::new(scope, &g));
    }
    let factory = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_window_scope_fn")
        .or_else(|| crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_worker_scope_fn"))?;
    let name = str_val(scope, "setTimeout");
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, 1).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let g = v8::Global::new(scope, f);
    with_state(scope, |s| s.set_timeout = Some(g));
    Some(f)
}

/// The page realm's own `name` timer member (made by the kernel's factory, cached), never the page's
/// possibly-replaced `window[name]`.
pub(crate) fn pristine_window_fn<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, len: i32) -> Option<v8::Local<'s, v8::Function>> {
    if let Some(g) = with_state(scope, |s| s.window_fns.get(name).cloned()) {
        return Some(v8::Local::new(scope, &g));
    }
    let factory = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_window_scope_fn")?;
    let n = str_val(scope, name);
    let l: v8::Local<v8::Value> = v8::Integer::new(scope, len).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[n, l]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let g = v8::Global::new(scope, f);
    with_state(scope, |s| {
        s.window_fns.insert(name.to_string(), g);
    });
    Some(f)
}

/// The pristine `setTimeout(f, ms)`: its return value (the timer id), `None` in a realm with no timer
/// factory. For engine code that needs a task and must not call the page's `setTimeout`.
pub(crate) fn pristine_timeout<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, ms: f64) -> Option<v8::Local<'s, v8::Value>> {
    // The ENGINE's task entry first: every caller is the engine scheduling its own work
    // -- a `window.postMessage` delivery, a frame's events, `scheduler.postTask`, `AbortSignal.timeout` --
    // none of which is a page timer, so none is nesting-clamped. `window.postMessage` ran at 217
    // deliveries/s (Chrome 150: ~314,000/s).
    let st = pristine_queue_task(scope).or_else(|| pristine_set_timeout(scope))?;
    let global = scope.get_current_context().global(scope);
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, ms).into();
    st.call(scope, global.into(), &[f.into(), delay])
}

/// Queue one of the ENGINE's tasks -- a MessagePort delivery, an animation frame, an IndexedDB event,
/// a cookie change -- `ms` from now on the realm's timer task source, never through the page's
/// `setTimeout`.
///
/// Those paths used to call `window.setTimeout` by name. A page may replace it, and instagram.com does:
/// its `replaceNativeTimer` routes every timer through FB's scheduler, whose host loop is a
/// `MessageChannel` -- so a port message waited on a timer that waited on a port message, and nothing
/// ever ran again (an empty shell, no error). Chrome 150 (`H-engine-tasks-stub` / `-fb`): with
/// `setTimeout` replaced by a stub that never calls back, MessagePort, rAF, rIC, IndexedDB and a
/// resource PerformanceObserver all still deliver. Falls back to the realm's own `setTimeout` only in a
/// realm with no timer factory at all.
pub(crate) fn engine_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ms: f64) {
    let global = scope.get_current_context().global(scope);
    let recv: v8::Local<v8::Value> = global.into();
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, ms).into();
    // A task, not a timer: never nesting-clamped -- see [`queue_task`].
    let st = match pristine_queue_task(scope).or_else(|| pristine_set_timeout(scope)) {
        Some(st) => st,
        None => match get_prop(scope, global, "setTimeout").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
            Some(st) => st,
            None => return,
        },
    };
    st.call(scope, recv, &[f.into(), delay]);
}

/// A BROWSER-PROCESS REPLY to a call made now: queued when the running task ends -- after every task the task queued
/// once it made the call (a message it posted, a timer) -- and `min_ms` after the call at the soonest. Queued at the
/// call itself, a reply due 1 ms later overtook a message the same task posted when the task took longer than that
/// to post it (a debug build under the full suite; `platform_answers_live`'s settle order, 2026-10-11).
pub(crate) fn queue_reply(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, min_ms: f64) {
    let due = crate::core::timing::performance::clock_now_ms(scope) + min_ms;
    let pack = v8::Array::new(scope, 2);
    pack.set_index(scope, 0, f.into());
    let d = v8::Number::new(scope, due);
    pack.set_index(scope, 1, d.into());
    let queue = crate::binding::protos::build_fn_data(scope, cb_queue_reply, "", 0, pack.into());
    scope.enqueue_microtask(queue);
}

fn cb_queue_reply(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(pack) = v8::Local::<v8::Array>::try_from(data) else { return };
    let Some(f) = pack.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let due = pack.get_index(scope, 1).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let now = crate::core::timing::performance::clock_now_ms(scope);
    queue_task(scope, f, (due - now).max(0.0));
}

/// Queue `f` as a task `ms` from now on the realm's timer task source.
pub(crate) fn queue_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ms: f64) {
    // The kernel's engine-task entry when the realm has one: a task, not a timer -- never nesting-clamped
    // (see the runtime's `Scheduler::queue_task`). The pristine `setTimeout` otherwise.
    let Some(st) = pristine_queue_task(scope).or_else(|| pristine_set_timeout(scope)) else { return };
    let global = scope.get_current_context().global(scope);
    let recv: v8::Local<v8::Value> = global.into();
    let delay: v8::Local<v8::Value> = v8::Number::new(scope, ms).into();
    v8::tc_scope!(let tc, scope);
    st.call(tc, recv, &[f.into(), delay]);
    tc.reset();
}

/// Ask for a frame. Idempotent while one is pending. The frame is armed from a MICROTASK, so every
/// animation frame the running script requests after this call is armed first (module header).
pub(crate) fn request_frame(scope: &mut v8::PinScope) {
    let already = with_state(scope, |s| std::mem::replace(&mut s.frame_pending, true));
    if already {
        return;
    }
    let arm = build_fn(scope, cb_arm_frame, "", 0);
    scope.enqueue_microtask(arm);
}

fn cb_arm_frame(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let frame = build_fn(scope, cb_frame, "", 0);
    let ms = next_frame_ms(scope);
    queue_frame(scope, frame, ms);
}

/// THE HOST'S RENDERING OPPORTUNITY. `Engine::pump` calls this between slices of the event loop: if
/// this realm has observations, no frame is pending, and the geometry key moved since the last frame,
/// a frame is armed and the answer is `true` (the loop has work again). Otherwise `false`, at the cost
/// of one `OpState` lookup when there are no observers at all.
pub fn rendering_opportunity(scope: &mut v8::PinScope) -> bool {
    // the deferred `loading=lazy` elements are observed at every opportunity too, observers or not
    crate::core::html::lazy_load::opportunity(scope);
    let idle = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        match st.try_borrow::<RenderingState>() {
            None => true,
            Some(s) => s.is_empty() || s.frame_pending,
        }
    };
    if idle {
        return false;
    }
    forget_stale_document(scope);
    if with_state(scope, |s| s.is_empty()) {
        return false;
    }
    let key = geometry_key(scope);
    if with_state(scope, |s| s.last_key == Some(key)) {
        return false;
    }
    with_state(scope, |s| s.frame_pending = true);
    let frame = build_fn(scope, cb_frame, "", 0);
    let ms = next_frame_ms(scope);
    queue_frame(scope, frame, ms);
    true
}

/// The frame task.
/// The document is RENDER-BLOCKED (HTML "render-blocked"): the parser waits for a render-blocking script
/// or stylesheet -- one in `<head>` that blocks the parser, or one marked `blocking=render` -- so no frame runs: no
/// animation frame callbacks, no observers, no paint. Chrome 150 draws nothing before them (WPT html/dom/render-blocking,
/// dom/nodes/moveBefore/preserve-render-blocking-script). Set by the runtime's page load while it waits.
pub struct RenderBlocked;

pub fn render_blocked(st: &deno_core::OpState) -> bool {
    st.try_borrow::<RenderBlocked>().is_some()
}

fn cb_frame(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if render_blocked(&deno_core::JsRuntime::op_state_from(scope).borrow()) {
        // the frame waits for the first one after the block lifts
        with_state(scope, |s| s.frame_pending = false);
        request_frame(scope);
        return;
    }
    forget_stale_document(scope);
    // This is the one "update the rendering" frame. Clear the pending flag FIRST (so an rAF
    // callback that requests another frame re-arms the next one), then run the ANIMATION-frame (rAF)
    // callbacks BEFORE the observer steps — HTML runs "the animation frame callbacks" ahead of
    // ResizeObserver/IntersectionObserver. rAF drives the frame even with no observers, so it must run
    // before the observer early-return below.
    with_state(scope, |s| s.frame_pending = false);
    let at = crate::core::timing::performance::clock_now_ms(scope);
    with_state(scope, |s| s.last_frame_at = Some(at));
    // HTML's "update animations and send events", BEFORE the animation frame callbacks: the CSS
    // animation and transition events, sampled at this frame's moment. While a record can still send one, its
    // clock asks for the next frame -- a clock that never keeps the event loop alive (`css::wake_after`).
    crate::core::animation::advance_frame_time(scope);
    if crate::core::animation::css::send_events(scope) {
        crate::core::animation::css::wake_after(scope);
    }
    let ran_raf = crate::core::html::animation_frame::run_frame_callbacks(scope);
    let (empty, fresh_io, fresh_ro) = with_state(scope, |s| {
        (
            s.is_empty(),
            s.io.iter().any(|r| r.observations.iter().any(|o| o.prev_index < 0)),
            s.ro.iter().any(|r| r.observations.iter().any(|o| o.last.0 < 0.0)),
        )
    });
    if empty {
        // No observers — only rAF drove this frame. Re-arm if a callback requested another frame.
        if crate::core::html::animation_frame::has_pending_frames(scope) {
            request_frame(scope);
        }
        paint_for_plugins(scope);
        return;
    }
    let key = geometry_key(scope);
    let changed = with_state(scope, |s| std::mem::replace(&mut s.last_key, Some(key)) != Some(key));
    let mut activity = false;
    if changed || fresh_ro {
        activity |= crate::core::resize_observer::resize_observer::run_resize_steps(scope);
    }
    if changed || fresh_io || activity {
        activity |= crate::core::intersection_observer::intersection_observer::run_intersection_steps(scope);
    }
    // Keep going while something is moving OR an rAF callback is still pending; stop the moment a frame
    // finds nothing to do and no animation frame is waiting.
    if activity || changed || ran_raf || crate::core::html::animation_frame::has_pending_frames(scope) {
        request_frame(scope);
    }
    paint_for_plugins(scope);
}

/// The frame's paint, for a plugin that wants frames (plugin ABI 3, C1): HTML's "update the rendering" paints after
/// the animation-frame callbacks and the observers, so a rAF animation reaches the plugin at the page's own frame
/// rate, each frame as its callbacks left it ([`crate::core::css::cssom::deliver_frame`]). Nothing with no such
/// plugin: this frame paints nothing then, as before (I9/J2).
fn paint_for_plugins(scope: &mut v8::PinScope) {
    if plugin_host::has_frames() && crate::core::css::cssom::deliver_frame(scope) {
        // too soon after the last one: the next vsync's frame paints it
        request_frame(scope);
    }
}

// ── geometry ─────────────────────────────────────────────────────────────────────────────────────

/// Everything an observation's answer depends on, in one number.
fn geometry_key(scope: &mut v8::PinScope) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let fp = (|| {
            let ctx = st.try_borrow::<spec::kernel::Context>()?;
            let handle = ctx.get::<page::PageHandle>()?;
            let dom = st.try_borrow::<HostDom>()?;
            let media = crate::core::dom::geometry_queries::media_context(&st);
            Some(handle.0.layout_fingerprint(&dom.0, &media))
        })();
        fp.hash(&mut h);
        let (x, y) = crate::core::html::window_geometry::scroll_position(&st);
        x.to_bits().hash(&mut h);
        y.to_bits().hash(&mut h);
    }
    // Each scroller's offset as `Element.scrollTop` then `scrollLeft` read it: each runs its style flush
    // first, so the flushes stay two per scroller, in scroller order. The offsets are read once they have
    // all run -- a tick writes neither an element's scroll nor the window's (see [`scroll_offset_now`]), so
    // they are the numbers the getters read between them.
    // Once one flush is skipped as changing nothing, every flush after it would be (nothing runs between them).
    let scrollers = with_state(scope, |s| s.scrollers.clone());
    'flush: for &n in &scrollers {
        for _ in 0..2 {
            if crate::core::animation::css::flush_style_for(scope, n) == crate::core::animation::css::Flush::Quiet {
                break 'flush;
            }
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    for n in scrollers {
        let (x, y) = scroll_offset_now(&st, n);
        n.hash(&mut h);
        x.to_bits().hash(&mut h);
        y.to_bits().hash(&mut h);
    }
    h.finish()
}

/// An element's own scroll offset, `(scrollLeft, scrollTop)`, as those members answer once their style flush
/// has run (`element::scroll_read`'s body after it, over the same stores in the same order): zero for an
/// element with no scrolling box, the viewport's for the document element, the element's own otherwise.
///
/// This was a call of the members' own getter functions, two per scroller per frame and again per scroller
/// an observer pass found: a V8 call, a wrapper lookup and an unwrap each, for a number this store holds.
/// The getters were engine-built functions no page accessor ever saw, and could not throw.
fn scroll_offset_now(st: &OpState, id: u32) -> (f64, f64) {
    let Some(h) = st.try_borrow::<HostDom>() else { return (0.0, 0.0) };
    let dom = &h.0;
    let has_box = dom.is_in_document(id) || {
        let root = dom.shadow_including_root(id);
        root != id && dom.node_type(root) == 9
    };
    if !has_box {
        return (0.0, 0.0);
    }
    if crate::core::dom::document::doc_element_child(dom, dom.root()) == Some(id) {
        return crate::core::html::window_geometry::scroll_position(st);
    }
    crate::core::dom::element::element_scroll_offset(st, id)
}

/// The flushes `Element.scrollTop` and `scrollLeft` run, in that order, then the offset they answer -- and
/// whether either flush ran a tick (a skipped one changed nothing).
fn element_scroll(scope: &mut v8::PinScope, node: u32) -> ((f64, f64), bool) {
    use crate::core::animation::css::{flush_style_for, Flush};
    let ticked = flush_style_for(scope, node) == Flush::Ticked;
    let ticked = (flush_style_for(scope, node) == Flush::Ticked) || ticked;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    (scroll_offset_now(&st, node), ticked)
}

/// One frame's view of the laid-out document, with the per-node answers it has already worked out.
///
/// ONE LAYOUT REFRESH PER PASS. Every layout read used to bring the cache up to date first: a media context,
/// two or three locks, the geometry fingerprint's hash and a map of every scrolled element, three to five
/// times per intersection observation and two or three per resize observation -- some 2,500 per frame on 600
/// observed targets, each finding the layout it was asked for already current. Nothing a pass does between
/// two reads moves a refresh's inputs except a style flush (the walk's, [`Geometry::shift_for_children`]): no
/// page script runs in a pass, and its computed-style reads resolve without the layout. So the cache is
/// refreshed at the pass's first read and again after each flush, and the boxes read from one refresh are
/// kept until the next flush ([`Geometry::stale`]).
pub(crate) struct Geometry {
    fixed: HashMap<u32, bool>,
    child_shift: HashMap<u32, (f64, f64)>,
    clips: HashMap<u32, bool>,
    doc_scroll: (f64, f64),
    pub scrollers: Vec<u32>,
    document_element: Option<u32>,
    body: Option<u32>,
    /// The layout cache was brought up to date in this pass and no style flush has run since.
    fresh: bool,
    /// Moved by every style flush: a box read before one and kept after it would be the pre-flush layout's
    /// answer, so a memo entry is stored only when no flush happened while it was worked out.
    epoch: u64,
    /// The answers of [`Geometry::border_box`], [`Geometry::client_box`] and [`Geometry::viewport`] since the
    /// last flush.
    boxes: HashMap<u32, Option<R>>,
    clients: HashMap<u32, Option<R>>,
    view: Option<R>,
    /// Taken from [`RenderingState::style_memo`] for the pass, put back by [`Geometry::finish`].
    memo: StyleMemo,
}

impl Geometry {
    pub fn new(scope: &mut v8::PinScope) -> Geometry {
        let memo = with_state(scope, |s| std::mem::take(&mut s.style_memo));
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let doc_scroll = crate::core::html::window_geometry::scroll_position(&st);
        let (document_element, body) = match st.try_borrow::<HostDom>() {
            Some(d) => {
                let dom = &d.0;
                let de = crate::core::dom::document::doc_element_child(dom, dom.root());
                let body = de.and_then(|h| {
                    let mut c = dom.first_child(h);
                    while let Some(n) = c {
                        if dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("body")) {
                            return Some(n);
                        }
                        c = dom.next_sibling(n);
                    }
                    None
                });
                (de, body)
            }
            None => (None, None),
        };
        Geometry {
            fixed: HashMap::new(),
            child_shift: HashMap::new(),
            clips: HashMap::new(),
            doc_scroll,
            scrollers: Vec::new(),
            document_element,
            body,
            fresh: false,
            epoch: 0,
            boxes: HashMap::new(),
            clients: HashMap::new(),
            view: None,
            memo,
        }
    }

    /// The pass is over: its computed answers are kept for the next one. A pass that ends without this only
    /// loses them.
    pub fn finish(&mut self, scope: &mut v8::PinScope) {
        let memo = std::mem::take(&mut self.memo);
        with_state(scope, |s| s.style_memo = memo);
    }

    /// Borrow the layout for the duration of `f`: brought up to date at the pass's first read and after each
    /// style flush ([`Geometry`]), read as it is otherwise. `None` when there is no page pipeline -- and then
    /// every read tries the refresh again, as each one did.
    pub(crate) fn layout<T>(&mut self, scope: &mut v8::PinScope, f: impl FnOnce(&page::LaidOut, &korvus_dom::Dom) -> T) -> Option<T> {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !self.fresh {
            if !refresh_layout_cache(&mut st) {
                return None;
            }
            self.fresh = true;
            let key = st.try_borrow::<HostDom>().map(|d| d.0.instance()).zip(st.try_borrow::<LayoutCache>().and_then(|c| c.0.as_ref().map(|(fp, _)| *fp)));
            if key != self.memo.key {
                self.memo = StyleMemo { key, ..StyleMemo::default() };
            }
        }
        let out = match st.try_borrow::<LayoutCache>() {
            Some(LayoutCache(Some((_, out)))) => out,
            _ => return None,
        };
        let dom = &st.try_borrow::<HostDom>()?.0;
        Some(f(out, dom))
    }

    /// A style flush ran: the next layout read refreshes where every read used to, and nothing read before it is
    /// kept. The per-pass maps stay -- the first answer in a pass was always the one it kept.
    fn stale(&mut self) {
        self.fresh = false;
        self.epoch += 1;
        self.boxes.clear();
        self.clients.clear();
        self.view = None;
    }

    /// May an answer worked out since `epoch` be kept? Only when it was read from one refresh with no flush since.
    fn keeps(&self, epoch: u64) -> bool {
        self.fresh && self.epoch == epoch
    }

    fn computed(scope: &mut v8::PinScope, node: u32, name: &str) -> String {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        crate::core::css::cssom::css_computed_one(&mut st, node, name)
    }

    pub fn is_element(scope: &mut v8::PinScope, node: u32) -> bool {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|d| d.0.node_type(node) == 1)
    }

    pub fn parent(scope: &mut v8::PinScope, node: u32) -> Option<u32> {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|d| d.0.parent(node))
    }

    pub fn is_connected(scope: &mut v8::PinScope, node: u32) -> bool {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|d| d.0.is_connected(node))
    }

    pub fn contains(scope: &mut v8::PinScope, ancestor: u32, node: u32) -> bool {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|d| d.0.contains(ancestor, node))
    }

    fn is_fixed(&mut self, scope: &mut v8::PinScope, node: u32) -> bool {
        if let Some(f) = self.fixed.get(&node) {
            return *f;
        }
        let f = Geometry::is_element(scope, node) && self.memoised(scope, node, |m| &mut m.fixed, |scope, node| Geometry::computed(scope, node, "position") == "fixed");
        self.fixed.insert(node, f);
        f
    }

    /// `node`'s computed `display` is `inline` (the resize observer's no-box case).
    pub fn is_inline(&mut self, scope: &mut v8::PinScope, node: u32) -> bool {
        self.memoised(scope, node, |m| &mut m.inline, |scope, node| Geometry::computed(scope, node, "display") == "inline")
    }

    /// A computed answer from the cross-pass memo while the pass's layout is fresh ([`StyleMemo`]), else read now --
    /// and kept, when it was read under a fresh layout.
    fn memoised(&mut self, scope: &mut v8::PinScope, node: u32, map: fn(&mut StyleMemo) -> &mut HashMap<u32, bool>, read: impl FnOnce(&mut v8::PinScope, u32) -> bool) -> bool {
        if self.fresh {
            if let Some(v) = map(&mut self.memo).get(&node) {
                return *v;
            }
        }
        let v = read(scope, node);
        if self.fresh {
            map(&mut self.memo).insert(node, v);
        }
        v
    }

    /// Does this element clip its descendants' boxes (any `overflow` other than `visible`)? The root
    /// and `<body>` never do here: their overflow is the VIEWPORT's.
    pub fn clips(&mut self, scope: &mut v8::PinScope, node: u32) -> bool {
        if let Some(c) = self.clips.get(&node) {
            return *c;
        }
        let c = Some(node) != self.document_element
            && Some(node) != self.body
            && Geometry::is_element(scope, node)
            && self.memoised(scope, node, |m| &mut m.overflows, |scope, node| {
                Geometry::computed(scope, node, "overflow-x") != "visible" || Geometry::computed(scope, node, "overflow-y") != "visible"
            });
        self.clips.insert(node, c);
        c
    }

    /// Which axes an element that [`Geometry::clips`] clips: `(overflow-x, overflow-y)` not `visible`.
    pub fn clip_axes(&mut self, scope: &mut v8::PinScope, node: u32) -> (bool, bool) {
        (Geometry::computed(scope, node, "overflow-x") != "visible", Geometry::computed(scope, node, "overflow-y") != "visible")
    }

    /// The translation (to SUBTRACT) that scrolling applies to the boxes of `node`'s children.
    fn shift_for_children(&mut self, scope: &mut v8::PinScope, node: u32) -> (f64, f64) {
        if let Some(s) = self.child_shift.get(&node) {
            return *s;
        }
        let s = if !Geometry::is_element(scope, node) {
            // The document (or a fragment): the viewport's scroll.
            self.doc_scroll
        } else {
            let base = if self.is_fixed(scope, node) {
                (0.0, 0.0)
            } else {
                match Geometry::parent(scope, node) {
                    Some(p) => self.shift_for_children(scope, p),
                    None => (0.0, 0.0),
                }
            };
            let own = if Some(node) == self.document_element || !self.clips(scope, node) {
                (0.0, 0.0)
            } else {
                let extent = self.layout(scope, |out, _| out.scroll_extent(node)).unwrap_or((0, 0));
                if extent == (0, 0) {
                    (0.0, 0.0)
                } else {
                    if !self.scrollers.contains(&node) {
                        self.scrollers.push(node);
                    }
                    let (offset, ticked) = element_scroll(scope, node);
                    // a skipped flush changed nothing a refresh reads
                    if ticked {
                        self.stale();
                    }
                    offset
                }
            };
            (base.0 + own.0, base.1 + own.1)
        };
        self.child_shift.insert(node, s);
        s
    }

    fn shift_of(&mut self, scope: &mut v8::PinScope, node: u32) -> (f64, f64) {
        if self.is_fixed(scope, node) {
            return (0.0, 0.0);
        }
        match Geometry::parent(scope, node) {
            Some(p) => self.shift_for_children(scope, p),
            None => (0.0, 0.0),
        }
    }

    /// The border box in client coordinates, or `None` for an element that generated no box.
    ///
    /// The client coordinates are the LAYOUT's answer (`LaidOut::viewport_bounding_rect`),
    /// the same one `getBoundingClientRect` gives, so an entry's `boundingClientRect` and the member
    /// cannot disagree. The walk over the DOM ancestors stays for what only it knows: which scroll
    /// containers this observation read an offset from, so the frame loop notices when one moves.
    pub fn border_box(&mut self, scope: &mut v8::PinScope, node: u32) -> Option<R> {
        // A kept box skips the walk too: the walk's second time over a node was all per-pass answers.
        if let Some(b) = self.boxes.get(&node) {
            return *b;
        }
        let epoch = self.epoch;
        if !Geometry::is_connected(scope, node) {
            return None;
        }
        // Read BEFORE the walk, whose flushes may move the layout: a box read here and a flush there is not kept.
        let r = self
            .layout(scope, |out, _| {
                if out.client_rects(node).is_empty() {
                    None
                } else {
                    let r = out.viewport_bounding_rect(node);
                    Some(R { x: r.x as f64, y: r.y as f64, w: r.width as f64, h: r.height as f64 })
                }
            })
            .flatten();
        if r.is_some() {
            let _ = self.shift_of(scope, node);
        }
        if self.keeps(epoch) {
            self.boxes.insert(node, r);
        }
        r
    }

    /// The padding box less its scrollbars -- the area a scroll container shows its content through --
    /// in client coordinates (`LaidOut::viewport_padding_box`).
    ///
    /// The document element's client area is the VIEWPORT's (`clientWidth`/`clientHeight` answer the
    /// viewport there), so it keeps being read that way.
    pub fn client_box(&mut self, scope: &mut v8::PinScope, node: u32) -> Option<R> {
        if let Some(c) = self.clients.get(&node) {
            return *c;
        }
        let epoch = self.epoch;
        let c = self.client_box_now(scope, node);
        if self.keeps(epoch) {
            self.clients.insert(node, c);
        }
        c
    }

    fn client_box_now(&mut self, scope: &mut v8::PinScope, node: u32) -> Option<R> {
        let b = self.border_box(scope, node)?;
        if Some(node) == self.document_element {
            let ((cw, ch), (bt, bl)) = self.layout(scope, |out, _| (out.client_size(node), out.border_widths(node)))?;
            return Some(R { x: b.x + bl as f64, y: b.y + bt as f64, w: cw as f64, h: ch as f64 });
        }
        let r = self.layout(scope, |out, _| out.viewport_padding_box(node)).flatten()?;
        Some(R { x: r.x as f64, y: r.y as f64, w: r.width as f64, h: r.height as f64 })
    }

    /// The viewport: the document element's client area.
    pub fn viewport(&mut self, scope: &mut v8::PinScope) -> R {
        if let Some(v) = self.view {
            return v;
        }
        let epoch = self.epoch;
        let media = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            crate::core::dom::geometry_queries::media_context(&st)
        };
        let fallback = R { x: 0.0, y: 0.0, w: media.viewport_width as f64, h: media.viewport_height as f64 };
        let v = match self.document_element {
            Some(de) => self
                .layout(scope, |out, _| {
                    let (w, h) = out.client_size(de);
                    R { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }
                })
                .unwrap_or(fallback),
            None => fallback,
        };
        if self.keeps(epoch) {
            self.view = Some(v);
        }
        v
    }
}

/// The interface prototype a Rust-minted instance wears: off the realm's interface object as its install left
/// it (`protos::iface_object`), never whatever a page assigned to the global name
/// (`caretPositionFromPoint` read `window.CaretPosition`).
pub(crate) fn interface_proto<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    crate::binding::protos::iface_object(scope, name)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// The interface objects one batch of observer entries is minted from: an intersection run's, or one resize
/// observer's batch -- no page script runs inside either.
///
/// Each entry looked its interfaces up by name again (`protos::iface_object`: a private-map read and a string
/// per name), four times per `IntersectionObserverEntry` and five per `ResizeObserverEntry`. The FIRST use of a
/// name is still that lookup, at the moment and in the order the old code made it, so a lazy realm makes its
/// interface objects when it did. The answer is reused only when `protos::page_eager_interface` names the same
/// object -- a lookup the sealed map (null-prototyped) would answer again, never a page accessor -- and never in
/// a realm or for a name made lazily, whose descriptor read would make it. Otherwise every use repeats the old
/// lookup. `.prototype` is still read per entry, as [`interface_proto`] reads it.
#[derive(Default)]
pub(crate) struct Kit<'s> {
    ctors: Vec<(&'static str, v8::Local<'s, v8::Function>)>,
}

impl<'s> Kit<'s> {
    /// `protos::iface_object(name)`, from the batch's cache when it is provably the same answer.
    fn ctor(&mut self, scope: &mut v8::PinScope<'s, '_>, name: &'static str) -> Option<v8::Local<'s, v8::Value>> {
        if let Some((_, c)) = self.ctors.iter().find(|(n, _)| *n == name) {
            return Some((*c).into());
        }
        let found = crate::binding::protos::iface_object(scope, name);
        if let Some(f) = found.and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
            let global = scope.get_current_context().global(scope);
            let plain = !crate::install::templates::is_lazy_realm(scope) && crate::install::templates::lazy_global(scope, global, name).is_none();
            if plain && crate::binding::protos::page_eager_interface(scope, global, name).is_some_and(|c| c.strict_equals(f.into())) {
                self.ctors.push((name, f));
            }
        }
        found
    }

    /// [`interface_proto`] through the batch's interface objects.
    pub fn proto(&mut self, scope: &mut v8::PinScope<'s, '_>, name: &'static str) -> Option<v8::Local<'s, v8::Object>> {
        self.ctor(scope, name)
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .and_then(|c| get_prop(scope, c, "prototype"))
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    }

    /// A `DOMRectReadOnly` for `r`, constructed as `protos::new_global` constructs one (the realm's own interface
    /// object, else the global's).
    pub fn rect(&mut self, scope: &mut v8::PinScope<'s, '_>, r: R) -> v8::Local<'s, v8::Value> {
        let args: Vec<v8::Local<v8::Value>> = [r.x, r.y, r.w, r.h].iter().map(|n| v8::Number::new(scope, *n).into()).collect();
        let ctor = self.ctor(scope, "DOMRectReadOnly").or_else(|| global_get(scope, "DOMRectReadOnly"));
        match ctor.and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()).and_then(|c| c.new_instance(scope, &args)) {
            Some(o) => o.into(),
            None => v8::null(scope).into(),
        }
    }
}

/// Invoke an observer's callback as WebIDL "invoke a callback function": `this` and the second
/// argument are the observer, a throw is REPORTED rather than propagated.
pub(crate) fn invoke_callback(scope: &mut v8::PinScope, observer: v8::Local<v8::Object>, entries: v8::Local<v8::Value>) {
    let Some(cb) = crate::core::events::event::evt_slot_read(scope, observer, "observer_callback")
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    // A frame's observer runs as the frame's realm.
    crate::core::html::browsing_context::as_callback_realm(scope, cb, |scope| {
        let exc = {
            v8::tc_scope!(let tc, scope);
            let recv: v8::Local<v8::Value> = observer.into();
            #[cfg(feature = "analysis")]
            let _ = {
                let label = crate::analysis_hook::observer_callback_label(tc, observer);
                crate::analysis_hook::call_traced(tc, label, cb, recv, &[entries, observer.into()])
            };
            #[cfg(not(feature = "analysis"))]
            let _ = cb.call(tc, recv, &[entries, observer.into()]);
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
    });
}

/// A task that runs `cb` with a number as its data.
pub(crate) fn queue_task_with_id(scope: &mut v8::PinScope, cb: impl v8::MapFnTo<v8::FunctionCallback>, id: u64, ms: f64) {
    let d: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    let f = build_fn_data(scope, cb, "", 0, d);
    queue_task(scope, f, ms);
}

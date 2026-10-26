//! `core/timing/performance.rs` — the performance TIMELINE: `Performance`, `PerformanceEntry`,
//! `PerformanceMark`, `PerformanceMeasure`, `PerformanceTiming`, `PerformanceNavigation` and
//! `EventCounts`, each one `interface!` table with Rust bodies.
//!
//! WHAT THIS REPLACES. There were **two hand-written copies of one algorithm**, one per realm:
//! `core/korvus-runtime/src/js/host.js`'s `KB.__createPerformance()` (~300 lines) and
//! `core/korvus-runtime/src/js/worker-host.js`'s `makePerformance()` (~100), the second of which
//! carried a `TODO(DRY)` admitting it. Each built a plain OBJECT LITERAL with eighteen (window) or
//! thirteen (worker) own members, a JS `PerformanceEntry`/`PerformanceMark`/`PerformanceMeasure`
//! class trio, a JS `eventCounts` over a `Map`, a `performance.timing` / `performance.navigation`
//! pair minted onto the shells' prototypes, and a JS observer-notification loop reading the
//! observer list back off the `KB.__perfObs` bridge slot through `KB.__poTypes` / `KB.__poCb`.
//! `korvus-runtime/src/singletons.rs::rehome_performance` then RELOCATED those own members onto
//! `Performance.prototype` at context creation so the singleton could at least be an instance.
//!
//! All of that is the rows below. One implementation, two realms: the ACTIVE bundle's capture decides
//! which rows a realm's global gets (thirteen of the eighteen in a worker, and the five it leaves out
//! carry `window` on the row), and each
//! realm still gets its OWN timeline instance — `worker.performance !== page.performance` is spec,
//! and `tests/fixtures/an agent-browser reference/performance-worker.js` asserts it.
//!
//! ## TIME (the three hazards this file must not reintroduce)
//!
//! * **`timeOrigin` is read LAZILY.** The deleted JS said why in its own words: `host.js` is baked
//!   into a V8 startup snapshot, so a `timeOrigin` evaluated at load time would report the snapshot
//!   BUILD time, possibly hours old. Every row here reads [`spec::PerfClock`] out of `OpState` at
//!   CALL time; nothing is captured at install time.
//! * **The clock is 100 µs-quantized on purpose** (Chrome's Spectre clamp), not `Date.now()`'s 1 ms.
//!   That decision lives in `spec::PerfClock::now_ms` and is a fingerprinting decision, not a
//!   resolution bug. The worker used to run `Date.now() - origin`, a 1 ms clock; it shares this one
//!   now, which is the same fidelity the window always had.
//! * **And the quantum is only half of it: the ARITHMETIC has a shape**. Every row in
//!   this file, `Event.timeStamp`, `chrome.loadTimes()`/`csi()`, `document.timeline` and resource
//!   timing read one clock, so they all inherit the same one — see `spec::PerfClock::dom_hrt` for the
//!   measurement and the derivation. Nothing here rounds, floors or re-quantizes what the clock
//!   returns; doing so would flatten the shape back out on that one surface and make it the tell.
//! * **Monotonicity.** `Event.timeStamp` (`host.js`) reads `performance.now()` and WPT asserts it is
//!   strictly positive; `now()` is a monotonic `Instant` elapsed, so it never goes backwards, and the
//!   `|| 0.001` fallback at that call site is untouched.
//!
//! ## ASKED OF CHROME, NOT OF THE OLD CODE
//!
//! Every behaviour below that differs from the deleted JS was measured against real Chrome
//! (150.0.7871.46, `--headless=new --dump-dom`, 2026-09-11) before it was written, because the JS
//! disagreed with the browser in four places:
//!
//! | expression | deleted JS | Chrome | here |
//! |---|---|---|---|
//! | `measure("d", { duration: 100.5 })` | `{startTime:0,duration:100.5}` | **TypeError** | TypeError |
//! | `measure("d", { detail: 41 })` | a measure | **TypeError** | TypeError (the bundle's oracle records it) |
//! | `measure("d", "nomark")` | `startTime` 0 | **SyntaxError** | SyntaxError |
//! | `mark("m", { startTime: -1 })` | a mark at -1 | **TypeError** | TypeError |
//! | `eventCounts.get("nope")` | `0` | **undefined** | undefined |
//!
//! The messages are Blink's, verbatim from the same probe.

use deno_core::v8;
use std::cell::{Cell, RefCell};

use crate::binding::protos::{
    build_fn_data, get_prop, global_get, member_key, set_prop, str_val, throw_dom, throw_type_error,
};
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;

// ── the realm's clock ────────────────────────────────────────────────────────────────────────────

/// This realm's [`spec::PerfClock`], created on first read if the kernel did not provide one.
///
/// `korvus-runtime` puts one into `OpState` at extension-init time in BOTH realms, which is what
/// makes `timeOrigin` the realm's start rather than the moment a page first asked. The lazy branch
/// exists for an embedder (and for the shape gates, which build a bare `JsRuntime`) that never ran
/// that init: a real clock whose origin is its first read is honest, where a fabricated 0 would be a
/// `timeOrigin` in 1970.
///
/// Read out of `OpState` ONCE and kept in an isolate slot (a copy: the clock is immutable once made),
/// because this runs on every `performance.now()` and every event's `timeStamp`, and an `OpState`
/// lookup costs whatever position `PerfClock`'s `TypeId` happens to take in deno_core's ordered map
/// (see `binding::protos::private_key`). Measured once (instructions per call,
/// `bench/percall_instr.py --p4`): 8434 against the pre-migration binary's 7732, with nothing on the
/// path changed but the types around it.
fn with_clock<R>(scope: &mut v8::PinScope, f: impl FnOnce(&spec::PerfClock) -> R) -> R {
    // a frame's realm keeps its document's own clock (`browsing_context::Ctx::clock`)
    if let Some(c) = crate::core::html::browsing_context::realm_frame_clock(scope) {
        return f(&c);
    }
    let c = page_clock(scope);
    f(&c)
}

/// Does the current realm's clock read at the cross-origin isolated grain (5 µs, else 100 µs)?
pub(crate) fn realm_clock_isolated(scope: &mut v8::PinScope) -> bool {
    with_clock(scope, |c| c.cross_origin_isolated())
}

/// The PAGE document's clock -- the isolate's, whichever realm asks.
pub(crate) fn page_clock(scope: &mut v8::PinScope) -> spec::PerfClock {
    if let Some(c) = scope.get_slot::<spec::PerfClock>().copied() {
        return c;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let c = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<spec::PerfClock>().is_none() {
            st.put(spec::PerfClock::default());
        }
        *st.borrow::<spec::PerfClock>()
    };
    scope.set_slot(c);
    c
}

/// A [`spec::process_ms`] reading -- a time the transport measured -- on the current realm's clock.
pub(crate) fn realm_ms(scope: &mut v8::PinScope, process: f64) -> f64 {
    with_clock(scope, |c| c.realm_ms(process))
}

/// Write milestone `name` (at `t`) into the navigation entry `e` a script already holds.
pub(crate) fn set_entry_milestone(scope: &mut v8::PinScope, e: v8::Local<v8::Object>, name: &str, t: f64) {
    def_num(scope, e, name, t);
    if name == "loadEventEnd" {
        def_num(scope, e, "duration", t);
    }
}

/// The navigation response that delivered this realm's document, for
/// `PerformanceNavigationTiming`. `None` for a document no navigation fetched (`about:blank`, a test's
/// `load_html`), which reports the zeros it always did.
#[derive(Default, Clone)]
pub(crate) struct NavTiming(pub Option<spec::FetchTiming>);

/// A new document's clock and navigation timing, at install, before its scripts.
///
/// HR Time: a document's `timeOrigin` is the START of the navigation that produced it. korvus took the
/// engine's construction instead -- for a page's later navigation (a bot challenge reload, a form
/// POST) that is the FIRST document's, so `performance.now()` kept counting across documents -- and the
/// navigation entry carried zeros where Chrome reports the response's phases, `nextHopProtocol` `h2` and
/// its sizes. The transport keeps the newest navigation response's timing; this takes it for `url`.
pub fn begin_document_timing(scope: &mut v8::PinScope, url: &str) {
    // A remote frame's document: its parent fetched it and handed the timing over.
    let pending = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<crate::core::html::remote_frame::PendingNavTiming>().and_then(|p| p.0.take())
    };
    let t = pending.or_else(|| transport::take_navigation_timing(url));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(t) = &t {
        let c = spec::PerfClock::since_process_ms(t.start);
        op_state.borrow_mut().put(c);
        scope.set_slot(c);
    }
    op_state.borrow_mut().put(NavTiming(t));
    op_state.borrow_mut().put(DocMilestones::default());
    // a new document's timeline waits for nothing an earlier one recorded
    let _ = op_state.borrow_mut().try_take::<PendingTimeline>();
}

/// The document's lifecycle milestones as they happen: `domInteractive`, the
/// `DOMContentLoaded` and `load` event bounds, `domComplete` -- each 0 until it has happened, as Chrome
/// reports them. They were all the moment the entry was first READ, so a page reading the entry at
/// `DOMContentLoaded` saw `loadEventEnd` already set, and `domInteractive` 1 s after a 2 ms parse.
#[derive(Default)]
pub(crate) struct DocMilestones {
    at: Vec<(&'static str, f64)>,
    entry: Option<v8::Global<v8::Object>>,
    /// The navigation's randomized confidence (`randomizedTriggerRate`, `value`), finalized when the
    /// document becomes interactive -- see [`navigation_confidence`].
    confidence: Option<(f64, &'static str)>,
}

/// Navigation Timing's `confidence`, as Chromium finalizes it (`RenderFrameHostImpl::
/// NotifyDocumentInteractive`): for the page's own document only, once it is interactive. The true level
/// is `low` for a navigation made while the browser is still starting up (`NavigationRequest`: not
/// `IsBrowserStartupComplete`) -- this process's first -- and `high` after; it is then reported as a
/// randomized response (`GenerateRandomizedConfidenceLevel`): with probability `randomizedTriggerRate`
/// a fair coin answers instead. The rate is 2 / (1 + e^epsilon) at Chrome's default epsilon 1.1,
/// rounded to 7 places: 0.4994798 (Chrome 150, measured on three fresh launches: the rate every time,
/// the value `low`, `low`, `high`).
fn navigation_confidence() -> (f64, &'static str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STARTED_UP: AtomicBool = AtomicBool::new(false);
    let rate = (2.0 / (1.0 + 1.1f64.exp()) * 1e7).round() / 1e7;
    let honest = if STARTED_UP.swap(true, Ordering::Relaxed) { "high" } else { "low" };
    let mut b = [0u8; 8];
    let _ = getrandom::getrandom(&mut b);
    let coin = (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64) / (u32::MAX as f64 + 1.0);
    let value = if coin < rate { if b[4] & 1 == 1 { "high" } else { "low" } } else { honest };
    (rate, value)
}

const MILESTONES: [&str; 6] = ["domInteractive", "domContentLoadedEventStart", "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd"];

/// Record that the document reached milestone `name` (one of [`MILESTONES`], or `domLoading`) now, and update a
/// navigation entry the page already holds.
pub fn note_milestone(scope: &mut v8::PinScope, name: &'static str) {
    let t = now_ms(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let entry = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DocMilestones>().is_none() {
            st.put(DocMilestones::default());
        }
        let m = st.borrow_mut::<DocMilestones>();
        if m.at.iter().any(|(n, _)| *n == name) {
            return;
        }
        m.at.push((name, t));
        if name == "domInteractive" {
            m.confidence = Some(navigation_confidence());
        }
        // `domLoading` is `performance.timing`'s alone: the navigation entry has no such field
        if name == "domLoading" {
            return;
        }
        m.entry.clone()
    };
    if let Some(e) = entry {
        let e = v8::Local::new(scope, &e);
        def_num(scope, e, name, t);
        if name == "loadEventEnd" {
            def_num(scope, e, "duration", t);
        }
    }
}

/// Milestone `name` of the realm's document if it has happened (at 0.0 when that was in the time origin's first
/// quantum), `None` until it has.
fn milestone_at(scope: &mut v8::PinScope, name: &str) -> Option<f64> {
    if let Some(t) = crate::core::html::browsing_context::realm_frame_milestone_at(scope, name) {
        return t;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<DocMilestones>().and_then(|m| m.at.iter().find(|(n, _)| *n == name).map(|(_, t)| *t))
}

fn milestone(scope: &mut v8::PinScope, name: &str) -> f64 {
    if let Some(t) = crate::core::html::browsing_context::realm_frame_milestone(scope, name) {
        return t;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<DocMilestones>().and_then(|m| m.at.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)).unwrap_or(0.0)
}

/// The realm's document (or a worker's creator) decided its cross-origin isolation: the clock's clamp
/// follows, 5 µs when isolated and 100 µs otherwise. Both copies move -- the one in
/// `OpState` every other reader takes, and the isolate-slot copy [`with_clock`] serves.
pub(crate) fn set_clock_isolated(scope: &mut v8::PinScope, isolated: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let c = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<spec::PerfClock>().is_none() {
            st.put(spec::PerfClock::default());
        }
        let c = st.borrow_mut::<spec::PerfClock>();
        c.set_cross_origin_isolated(isolated);
        *c
    };
    scope.set_slot(c);
}

/// `performance.now()` — monotonic ms since this realm's clock origin, clamped to 100 µs (5 µs in a
/// cross-origin-isolated realm).
///
/// F5 (plugin virtual time): while a plugin has pinned the clock ([`plugin_host::virtual_time`] is
/// `Some`), the realm reads the PINNED virtual now on its own timeline, through the same Blink clamp
/// [`instant_to_ms`] applies to any monotonic instant -- never the OS clock. `freeze` holds this value
/// steady and `advance(ms)` jumps it forward. The gate is one `Option` read: with no plugin -- the
/// ~10,000 parity cases -- it is `None`, and the reading is byte-for-byte the real clock (invariant I9).
/// This one function is the whole reading clock; `clock_now_ms`, `Event.timeStamp`, `document.timeline`
/// and the `requestAnimationFrame` timestamp all read through it, so they follow virtual time too.
//
// TODO(1021-verify): the V8 `Date` object (`Date.now`, `new Date`) is seeded from the OS clock inside
// V8 and is NOT virtualized here -- it needs a V8 date-time override (not exposed through deno_core
// today), so only the monotonic clock this function serves is under virtual time. `origin_ms`
// (`performance.timeOrigin`), `legacy_page_ms` (`chrome.csi().pageT`) and transport-measured resource
// timings stay on the real clock as well: the origin is a fixed anchor, the other two are wall-clock
// surfaces.
pub(crate) fn now_ms(scope: &mut v8::PinScope) -> f64 {
    if let Some(vt) = plugin_host::virtual_time() {
        return instant_to_ms(scope, vt.now_instant());
    }
    with_clock(scope, |c| c.now_ms())
}

/// The same reading, for the one other table that needs this realm's clock: `Event.timeStamp`
/// (`core/events/ctor.rs`). It is a Rust call on the SAME `spec::PerfClock` out of `OpState`, not a
/// JS `performance.now()` round trip, so a page that overwrites `performance` cannot change what an
/// event's `timeStamp` reports and the two readings cannot drift apart.
pub fn clock_now_ms(scope: &mut v8::PinScope) -> f64 {
    now_ms(scope)
}

/// `performance.timeOrigin` — the wall-clock epoch ms at which this realm's clock started.
fn origin_ms(scope: &mut v8::PinScope) -> f64 {
    with_clock(scope, |c| c.origin_ms())
}

/// `chrome.csi().pageT`, off the SAME `spec::PerfClock` — but not off the same clock READING.
///
/// It is the one number on these surfaces that Chromium does not take from the monotonic timeline:
/// see `spec::PerfClock::legacy_page_ms` for what it is and what was measured. It lives here rather
/// than in `modules/chrome_object.rs` for the reason every other row does — one clock, read out of
/// `OpState` in Rust, so a page that overwrites `performance` cannot move it.
pub(crate) fn clock_legacy_page_ms(scope: &mut v8::PinScope) -> f64 {
    with_clock(scope, |c| c.legacy_page_ms())
}

// ── the three cppgc values ───────────────────────────────────────────────────────────────────────

/// One realm's timeline. The entry buffer, the observer list and the three `[SameObject]` partials
/// the window exposes, all of which the JS held in a closure over an object literal.
///
/// The observer list is HERE, not on the `KB.__perfObs` bridge slot it used to live on:
/// `PerformanceObserver`'s `perf` field already holds the timeline the observer was constructed
/// against, so `observe()` reaches this vector through its own receiver and a Worker's observers stay
/// that Worker's by construction rather than by `bridge_get` resolving per realm.
#[repr(C)]
pub struct Performance {
    /// The buffer, in insertion order — marks, measures, and the one navigation entry at the front.
    entries: RefCell<Vec<v8::Global<v8::Object>>>,
    /// The `PerformanceObserver`s observing this timeline (the old `KB.__perfObs`).
    observers: RefCell<Vec<v8::Global<v8::Value>>>,
    /// The one `[SameObject]` `PerformanceTiming` — `performance.timing === performance.timing`.
    timing: RefCell<Option<v8::Global<v8::Value>>>,
    /// The one `[SameObject]` `PerformanceNavigation`.
    navigation: RefCell<Option<v8::Global<v8::Value>>>,
    /// The one `[SameObject]` `EventCounts`.
    event_counts: RefCell<Option<v8::Global<v8::Value>>>,
    /// The document's single `PerformanceNavigationTiming`, built on first read (see [`ensure_nav`]).
    nav_entry: RefCell<Option<v8::Global<v8::Object>>>,
    /// `setResourceTimingBufferSize`'s value: how many `resource` entries the buffer keeps (250 until a
    /// page says otherwise, Resource Timing's default).
    resource_buffer_size: Cell<u32>,
    /// Resource Timing's secondary buffer: entries that arrived while the buffer was full, waiting for
    /// the `resourcetimingbufferfull` task to see whether a handler made room.
    resource_secondary: RefCell<Vec<v8::Global<v8::Object>>>,
    /// Is a `resourcetimingbufferfull` task already queued?
    bufferfull_pending: Cell<bool>,
    /// The document's navigation id, every entry's `navigationId` ([`initial_navigation_id`]); 0 in a worker.
    navigation_id: Cell<f64>,
}

/// Chromium 152's `PerformanceTimelineEntryIdGenerator::ResetId`, which `WindowPerformance`'s navigation id
/// generator runs when the document's performance is made: `base::RandIntInclusive(kMinId, kMaxIdForReset)`,
/// 100..=10000 (`core/timing/performance_timeline_entry_id_generator.cc`). A soft navigation or a back/forward
/// cache restore adds `kIdIncrement` (7); this engine reports neither, so a document keeps its first id. A
/// worker's `Performance` has no generator: `Performance::NavigationId()` is `kNoId`, 0 (measured, Chrome 152:
/// a worker's mark reads 0, a page's entries one number from 100 to 10000 that changes per load).
fn initial_navigation_id() -> f64 {
    let mut b = [0u8; 8];
    let r = getrandom::getrandom(&mut b).map(|()| u64::from_le_bytes(b)).unwrap_or(0);
    (100 + r % 9901) as f64
}

/// The current realm's navigation id: its window's `Performance`'s, 0 in a realm with none (a worker).
pub(crate) fn realm_navigation_id(scope: &mut v8::PinScope) -> f64 {
    crate::core::html::window_globals::window_slot_get(scope, "performance")
        .and_then(|v| deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, v))
        .map(|p| unsafe { p.as_ref() }.navigation_id.get())
        .unwrap_or(0.0)
}

/// Does this realm's `PerformanceEntry` have `navigationId` (the worn capture records it: Chrome 152, Brave 151;
/// Chrome 150 had none)? Blink's `PerformanceEntry::BuildJSONValue` writes the key exactly when the
/// `NavigationId` feature that exposes the attribute is enabled, after `duration`.
pub(crate) fn navigation_id_exposed(scope: &mut v8::PinScope) -> bool {
    let Some(p) = crate::binding::protos::shell_proto(scope, "PerformanceEntry") else { return false };
    let Some(k) = v8::String::new(scope, "navigationId") else { return false };
    p.has_own_property(scope, k.into()).unwrap_or(false)
}

/// This realm's timeline, for Rust that adds entries to it without a JS receiver in hand (a finished
/// load). Put by [`mint_performance`] in the page realm; read nowhere a page can reach.
pub(crate) struct RealmTimeline(v8::Global<v8::Object>);

/// The context snapshot (slice 3): the page's `Performance`, minted by the install, is unseated before the
/// page realm is serialised, and minted again -- with its `[SameObject]` timing, navigation and eventCounts -- on its
/// first read after a load, from that run's clock; what the run records before then waits in Rust ([`Pending`]). The
/// Globals its Rust struct owns must go before V8 serialises (it refuses a live Global it was not handed, and the
/// cppgc object is swept only later): taken here, so nothing keeps the maker's sub-objects, and no restored realm can
/// answer with the maker's numbers.
#[cfg(not(feature = "analysis"))]
pub(crate) fn release_for_snapshot(scope: &mut v8::PinScope) {
    let Some(v) = crate::core::html::window_globals::window_slot_get(scope, "performance") else { return };
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, v) else { return };
    p.timing.borrow_mut().take();
    p.navigation.borrow_mut().take();
    p.event_counts.borrow_mut().take();
    p.nav_entry.borrow_mut().take();
    p.entries.borrow_mut().clear();
    p.observers.borrow_mut().clear();
    p.resource_secondary.borrow_mut().clear();
}

unsafe impl deno_core::GarbageCollected for Performance {
    fn trace(&self, _v: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"Performance"
    }
}

/// Navigation Timing L1 (`performance.timing`). The twenty-one fields are EPOCH MILLISECONDS — the
/// one performance clock in the platform that is not relative to `timeOrigin` — captured when the
/// timeline is built, exactly as the deleted JS captured them.
#[repr(C)]
pub struct PerformanceTiming;
unsafe impl deno_core::GarbageCollected for PerformanceTiming {
    fn trace(&self, _v: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"PerformanceTiming"
    }
}

/// Navigation Timing L1's `performance.navigation`: `type` is `TYPE_NAVIGATE` and `redirectCount` 0,
/// because this engine loads a document, it does not re-enter one.
#[repr(C)]
pub struct PerformanceNavigation;
unsafe impl deno_core::GarbageCollected for PerformanceNavigation {
    fn trace(&self, _v: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"PerformanceNavigation"
    }
}

/// The Event Timing read-only map of tracked event types → count.
#[repr(C)]
pub struct EventCounts {
    counts: RefCell<Vec<(&'static str, f64)>>,
}
unsafe impl deno_core::GarbageCollected for EventCounts {
    fn trace(&self, _v: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"EventCounts"
    }
}

// The four are cppgc-backed page objects of their tables' interfaces, minted by `binding::class::mint` on the
// calling realm's prototype (deno_core's generic class template until then).
impl crate::binding::class::OwnClass for Performance {
    const IFACE: &'static str = "Performance";
}
impl crate::binding::class::OwnClass for PerformanceTiming {
    const IFACE: &'static str = "PerformanceTiming";
}
impl crate::binding::class::OwnClass for PerformanceNavigation {
    const IFACE: &'static str = "PerformanceNavigation";
}
impl crate::binding::class::OwnClass for EventCounts {
    const IFACE: &'static str = "EventCounts";
}

/// The thirty-six event types Blink's Event Timing counts, in Chrome's own enumeration order.
///
/// WHERE THIS COMES FROM, AND WHY IT IS NOT GENERATED YET. The list is a fact about the browser, so
/// goal §W4.d asks for it as a const the sync emits from the persona bundle. The committed bundle
/// (`brave-151-linux-20260907T1006`) does not record it — `surface.json` carries `Performance`'s
/// member SHAPES but never reads `performance.eventCounts`'s keys — and writing capture data by hand
/// into a bundle is forbidden. So the rig learns to record it (`plugins/korvus-persona/capture/
/// collect.js`, `timing.eventCountKeys`) and the const lives here, in the interface's own file, until
/// a human runs the rig again; that is one definition site instead of the two JS hand-lists it
/// replaces, and the follow-up is written down rather than hidden.
///
/// ORDER IS MEASURED, NOT SORTED: Chrome 150.0.7871.46, loopback, 2026-09-11 —
/// `Array.from(performance.eventCounts.keys())`. It is Blink's hash order, and the JS this replaces
/// listed the same names in a different (hand-typed) one.
pub(crate) const EVENT_COUNT_TYPES: &[&str] = &[
    "pointerdown", "touchend", "input", "keydown", "mouseleave", "mouseenter", "drop", "beforeinput",
    "pointerenter", "dragend", "pointercancel", "compositionupdate", "mousedown", "dragleave",
    "dragover", "mouseup", "pointerover", "lostpointercapture", "mouseover", "gotpointercapture",
    "dblclick", "keyup", "keypress", "pointerup", "compositionstart", "auxclick", "dragstart",
    "touchstart", "compositionend", "pointerout", "dragenter", "touchcancel", "click", "contextmenu",
    "mouseout", "pointerleave",
];

/// Navigation Timing L1's twenty-one fields, in the interface's order (Blink's `GetAttributeMapping` table).
const TIMING_NAMES: [&str; 21] = [
    "navigationStart", "unloadEventStart", "unloadEventEnd", "redirectStart", "redirectEnd", "fetchStart",
    "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd", "secureConnectionStart", "requestStart",
    "responseStart", "responseEnd", "domLoading", "domInteractive", "domContentLoadedEventStart",
    "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd",
];

/// `performance.timing.toJSON()`'s key order in Chrome 150: Blink's `toJSONForBinding` walks
/// `GetAttributeMapping()`, a `HashMap` of the names above, so the keys come in its HASH order -- measured,
/// the same in a page, a same-origin and a cross-origin frame (`tests/fixtures/performance-timing/`).
const TIMING_TO_JSON: [&str; 21] = [
    "connectStart", "secureConnectionStart", "unloadEventEnd", "domainLookupStart", "domainLookupEnd", "responseStart",
    "connectEnd", "responseEnd", "requestStart", "domLoading", "redirectStart", "loadEventEnd", "domComplete",
    "navigationStart", "loadEventStart", "domContentLoadedEventEnd", "unloadEventStart", "redirectEnd",
    "domInteractive", "fetchStart", "domContentLoadedEventStart",
];

/// Navigation Timing L1 (`performance.timing`) for the current realm's document, field by field, read at CALL
/// time as Blink's `PerformanceTiming` reads each from the document's loader (`performance_timing.cc`). The
/// fields used to be one value -- the realm's time origin at install, floored -- written into all sixteen
/// "happened" fields when the timeline was built: `domInteractive` and `loadEventEnd` answered before either
/// had happened, `responseStart` equalled `fetchStart`, and a page whose clock then moved to its navigation's
/// start read `navigationStart` up to a millisecond AFTER `timeOrigin` (Chrome: in (-1, 0]).
///
/// Each value is epoch ms, the field's moment on the document's clock floored to an integer (Blink's
/// `ToIntegerMilliseconds` of the clamped pseudo-wall time), worked in whole microseconds so a sum like
/// `origin + 31.2` cannot floor a millisecond short. As Blink:
///  * `navigationStart` is the navigation's start, the time origin;
///  * the network fields are the navigation response's phases -- the connection's as the transport measured the
///    one the navigation opened ([`conn_phases`]) -- with Blink's backfill (no lookup or connect timing reads as
///    `fetchStart`), and `secureConnectionStart` is 0 unless the navigation opened a TLS connection
///    (`SslStart()` is null on a reused one, where the L2 entry answers `fetchStart`);
///  * a document no navigation fetched (`about:blank`, `srcdoc`) has no response: those fields are 0;
///  * `domLoading` and the DOM and load milestones are 0 until they have happened;
///  * `unloadEvent*` are 0 (this engine reports no previous document's unload), and `redirect*` are 0 unless
///    every hop was same-origin, as in the navigation entry.
pub(crate) fn timing_fields(scope: &mut v8::PinScope) -> [(&'static str, f64); 21] {
    let origin_us = (origin_ms(scope) * 1000.0).round() as i64;
    let at = |rel_ms: f64| ((origin_us + (rel_ms * 1000.0).round() as i64).div_euclid(1000)) as f64;
    let mut out = TIMING_NAMES.map(|k| (k, 0.0));
    let mut put = |k: &str, v: f64| {
        if let Some(slot) = out.iter_mut().find(|(n, _)| *n == k) {
            slot.1 = v;
        }
    };
    put("navigationStart", at(0.0));
    if let Some(n) = nav_phases(scope) {
        if let Some((rs, re)) = n.redirect {
            put("redirectStart", at(rs));
            put("redirectEnd", at(re));
        }
        put("fetchStart", at(n.fetch_start));
        // the connection the navigation opened, with Blink's backfill (`performance_timing.cc`): no lookup reads
        // `fetchStart`, no connect `domainLookupEnd`; L1's `secureConnectionStart` is the handshake's start or 0
        put("domainLookupStart", at(n.conn.domain_lookup_start));
        put("domainLookupEnd", at(n.conn.domain_lookup_end));
        put("connectStart", at(n.conn.connect_start));
        put("connectEnd", at(n.conn.connect_end));
        if let Some(ssl) = n.conn.ssl_start {
            put("secureConnectionStart", at(ssl));
        }
        put("requestStart", at(n.request_start));
        put("responseStart", at(n.response_start));
        put("responseEnd", at(n.response_end));
    } else if crate::core::html::browsing_context::realm_frame_nav_timing(scope).is_none() {
        // the PAGE's document with no response timing -- a document an embedder handed the engine (a test harness,
        // a replay), not one the transport fetched: a fetch at its navigation's start, as these fields read before
        // they were read per field. A page's `fetchStart` is never 0 in Chrome, and `performance.measure(n,
        // 'fetchStart')` would throw "'fetchStart' is empty" on one that was (an agent-browser reference conformance fixture).
        for k in ["fetchStart", "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd", "requestStart", "responseStart", "responseEnd"] {
            put(k, at(0.0));
        }
    } else if let Some(t) = milestone_at(scope, "domLoading") {
        // no response to report: the document's own body "ends" as it is begun (Chrome 150: an about:blank
        // or srcdoc frame's `responseEnd` is set, its network fields 0)
        put("responseEnd", at(t));
    }
    for k in ["domLoading", "domInteractive", "domContentLoadedEventStart", "domContentLoadedEventEnd", "domComplete",
              "loadEventStart", "loadEventEnd"] {
        if let Some(t) = milestone_at(scope, k) {
            put(k, at(t));
        }
    }
    out
}

/// One navigation response's phases on the current realm's clock (ms from its time origin), shared by the
/// navigation entry (L2) and `performance.timing` (L1) so the two cannot disagree. `None` for a document no
/// navigation fetched.
struct NavPhases {
    /// `(redirectStart, redirectEnd)` when every hop was same-origin with the document (see [`ensure_nav`]).
    redirect: Option<(f64, f64)>,
    redirect_count: u32,
    fetch_start: f64,
    /// The connection's phases: those of the one the navigation opened, `fetch_start` on one it reused.
    conn: ConnPhases,
    request_start: f64,
    response_start: f64,
    response_end: f64,
    critical_ch_restart: f64,
}

/// The realm's navigation response -- a frame's own document's, or the page's.
fn realm_nav_timing(scope: &mut v8::PinScope) -> Option<spec::FetchTiming> {
    match crate::core::html::browsing_context::realm_frame_nav_timing(scope) {
        Some(frame) => frame,
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<NavTiming>().and_then(|n| n.0.clone())
        }
    }
}

/// The HTTP status of the running realm's document response (a CSP violation's `statusCode`, Blink
/// `ExecutionContextCSPDelegate::GetStatusCode`); 0 for a document no HTTP response delivered.
pub(crate) fn document_status(scope: &mut v8::PinScope) -> u16 {
    realm_nav_timing(scope).map_or(0, |t| t.status)
}

fn nav_phases(scope: &mut v8::PinScope) -> Option<NavPhases> {
    let t = realm_nav_timing(scope)?;
    Some(phases_of(scope, &t))
}

fn phases_of(scope: &mut v8::PinScope, t: &spec::FetchTiming) -> NavPhases {
    let realm = |scope: &mut v8::PinScope, p: f64| realm_ms(scope, p).max(0.0);
    // Navigation Timing's redirect fields are the document's only when EVERY hop of the chain was
    // same-origin with it (a `Critical-CH` restart is one of those hops); otherwise all three are 0.
    // Chrome 150, measured: one and two same-origin 302s give 1 and 2, a 302 from 127.0.0.1 to localhost
    // gives 0, 0, 0, and a Critical-CH restart gives 1 with `criticalCHRestart` at the restart.
    let last = t.url_list.last().and_then(|u| url::Url::parse(u).ok()).map(|u| u.origin());
    let redirected = t.redirect_count > 0
        && last.is_some_and(|o| t.url_list.iter().all(|u| url::Url::parse(u).is_ok_and(|u| u.origin() == o)));
    let fetch_start = realm(scope, t.fetch_start);
    let redirect = if redirected { Some((realm(scope, t.redirect_start), realm(scope, t.redirect_end))) } else { None };
    let secure = t.url_list.last().is_some_and(|u| u.starts_with("https:"));
    let conn = conn_phases(t, fetch_start, secure, |p| realm(scope, p));
    let critical_ch_restart = if t.critical_ch_restart > 0.0 { realm(scope, t.critical_ch_restart) } else { 0.0 };
    NavPhases {
        redirect,
        redirect_count: if redirected { t.redirect_count } else { 0 },
        fetch_start,
        conn,
        request_start: realm(scope, t.request_start),
        response_start: realm(scope, t.response_start),
        response_end: realm(scope, t.response_end),
        critical_ch_restart,
    }
}

/// A response's connection phases on a realm's clock, as Blink's `PerformanceResourceTiming` getters read them
/// off the network stack's `LoadTimingInfo` (`performance_resource_timing.cc:337-430`):
///  * `domainLookupStart` / `domainLookupEnd`: the host's lookup; none (a proxied connection, a reused one)
///    reads `fetchStart`, and a missing end its start;
///  * `connectStart`: the lookup's end when there was one ("connectStart includes any DNS time, so we may need
///    to trim that off"), else the connect's own start (behind a proxy: the proxy's lookup, TCP and tunnel);
///    `domainLookupEnd` on a reused connection;
///  * `connectEnd`: when the connection became usable, `connectStart` on a reused one;
///  * `secureConnectionStart`: 0 over a scheme that is not secure, `fetchStart` on a reused connection (Resource
///    Timing's step 2) or with no handshake stamped, else the handshake's start.
///
/// The transport reports every phase 0 for a connection the request did not open (`FetchTiming`), Chromium's
/// `socket_reused`. korvus used to answer that for EVERY connection: a device-check script's device check reads TCP 0 and
/// `(tcp - tls) / tls` -1 on a fresh TLS connection as a bot (allegro.pl, rule 579924).
pub(crate) struct ConnPhases {
    pub domain_lookup_start: f64,
    pub domain_lookup_end: f64,
    pub connect_start: f64,
    pub connect_end: f64,
    pub secure_connection_start: f64,
    /// The handshake's start alone, for L1's `secureConnectionStart` (0 when null, whatever the scheme).
    pub ssl_start: Option<f64>,
}

/// [`ConnPhases`] of the transport's measurement `t`, `fetch_start` already on the realm's clock and `clock`
/// converting a process reading to it.
pub(crate) fn conn_phases(t: &spec::FetchTiming, fetch_start: f64, secure: bool, mut clock: impl FnMut(f64) -> f64) -> ConnPhases {
    let opened = t.connect_end > 0.0;
    let mut at = |p: f64| if opened && p > 0.0 { Some(clock(p)) } else { None };
    let lookup_start = at(t.domain_lookup_start);
    let lookup_end = at(t.domain_lookup_end);
    let connect = at(t.connect_start);
    let ssl_start = at(t.ssl_start);
    let end = at(t.connect_end);
    let domain_lookup_start = lookup_start.unwrap_or(fetch_start);
    let domain_lookup_end = lookup_end.unwrap_or(domain_lookup_start);
    let connect_start = match connect {
        Some(c) => lookup_end.unwrap_or(c),
        None => domain_lookup_end,
    };
    let connect_end = end.unwrap_or(connect_start);
    let secure_connection_start = if !secure { 0.0 } else { ssl_start.unwrap_or(fetch_start) };
    ConnPhases { domain_lookup_start, domain_lookup_end, connect_start, connect_end, secure_connection_start, ssl_start }
}

/// Whether the current realm is a window's (`PerformanceTiming` is `[Exposed=Window]`).
fn window_realm(scope: &mut v8::PinScope) -> bool {
    crate::binding::protos::iface_object(scope, "PerformanceTiming").is_some()
}

/// User Timing's "a name that is a read only attribute in the PerformanceTiming interface" may not name a
/// mark in a window (Blink `PerformanceMark::Create`): `SyntaxError`, after the start time was checked.
fn refuse_timing_mark_name(scope: &mut v8::PinScope, name: &str, prefix: &str) -> bool {
    if TIMING_NAMES.contains(&name) && window_realm(scope) {
        throw_dom(scope, &format!("{prefix}'{name}' is part of the PerformanceTiming interface, and cannot be used as a mark name."), "SyntaxError");
        return true;
    }
    false
}

// ── receivers ────────────────────────────────────────────────────────────────────────────────────

/// `args.this()` as this realm's timeline, or `TypeError: Illegal invocation`.
///
/// This is failure mode 2 of a table cutover, and the reason the timeline is a cppgc value rather
/// than the plain object literal it used to be: `Performance.prototype.mark.call({}, "x")` threw
/// nothing before (the body closed over the ONE entries array, so a foreign receiver silently marked
/// the real timeline), and `Performance.prototype` itself answered as a live timeline with its own
/// entries once `rehome_performance` moved the bodies onto it.
fn this_perf(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<deno_core::cppgc::UnsafePtr<Performance>> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, args.this().into()) {
        Some(p) => Some(p),
        None => {
            throw_type_error(scope, &format!("Failed to execute '{member}' on 'Performance': Illegal invocation"));
            None
        }
    }
}

/// The accessor form of [`this_perf`]: Blink's message for an attribute is the bare "Illegal
/// invocation" (measured: `Object.getOwnPropertyDescriptor(Performance.prototype,"timing").get
/// .call({})`).
fn this_perf_attr(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<deno_core::cppgc::UnsafePtr<Performance>> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, args.this().into()) {
        Some(p) => Some(p),
        None => {
            crate::binding::protos::illegal_invocation(scope);
            None
        }
    }
}

// ── WebIDL argument conversion ───────────────────────────────────────────────────────────────────

/// WebIDL `DOMString`: `ToString`, which THROWS on a Symbol. `String()` would describe it instead and
/// `performance.mark(Symbol("a")).name` would read "Symbol(a)"; Chrome answers
/// `TypeError: ... Cannot convert a Symbol value to a string`, measured.
/// One dictionary member: present means "not absent and not `undefined`", which is what WebIDL's
/// `has a member` test means and what the three `measure()` TypeErrors below are decided by.
fn dict_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    get_prop(scope, o, key).filter(|v| !v.is_undefined())
}

fn idl_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, member: &str) -> Option<String> {
    if v.is_symbol() {
        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': Cannot convert a Symbol value to a string"));
        return None;
    }
    Some(v.to_rust_string_lossy(scope))
}

// ── the entry buffer ─────────────────────────────────────────────────────────────────────────────

/// Mint one `PerformanceEntry`-family object: a plain object wearing `iface`'s prototype, with the
/// interface's readonly attributes in PRIVATE SLOTS.
///
/// The slots are the ones `install::kits::slot_get` reads, so the `PerformanceEntry` rows
/// (`name` / `entryType` / `startTime` / `duration` / `navigationId`) and the
/// `PerformanceMark`/`PerformanceMeasure` `detail` row are the table's — an entry has NO own
/// property, which is what `Object.getOwnPropertyNames(performance.mark("x"))` answers in Chrome
/// (measured: `[]`) and what the JS classes this replaces already achieved with the same slots.
fn mint_entry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    name: &str,
    entry_type: &str,
    start_time: f64,
    duration: f64,
    detail: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Object> {
    use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
    let e = crate::install::live::mint_table(scope, iface);
    let n = str_val(scope, name);
    evt_slot_write(scope, e, "name", n);
    let t = str_val(scope, entry_type);
    evt_slot_write(scope, e, "entryType", t);
    evt_slot_write_num(scope, e, "startTime", start_time);
    evt_slot_write_num(scope, e, "duration", duration);
    // the realm's navigation id (Chrome 152's `PerformanceEntry.navigationId`, an unsigned long long)
    let id = realm_navigation_id(scope);
    evt_slot_write_num(scope, e, "navigationId", id);
    evt_slot_write(scope, e, "detail", detail);
    e
}

/// Read an entry's `entryType` / `name` back (the filters below, and `notify`).
///
/// The private SLOT first, which is where [`mint_entry`] writes and where the `PerformanceEntry`
/// rows read -- then the property, because the navigation entry is the one entry in the buffer whose
/// fields are OWN data properties (`PerformanceNavigationTiming` is still a registry interface with
/// D2 stub rows, so an own property is the only thing that answers on it). Slot-only was the first
/// version of this function, and it made `getEntriesByType("navigation")` come back empty -- exactly
/// the defect the deleted JS had, for a different reason.
fn entry_slot(scope: &mut v8::PinScope, e: v8::Local<v8::Object>, key: &str) -> String {
    if let Some(v) = crate::core::events::event::evt_slot_read(scope, e, key) {
        return v.to_rust_string_lossy(scope);
    }
    get_prop(scope, e, key).filter(|v| !v.is_undefined()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// This timeline's entries as live handles.
fn entries_of<'s>(scope: &mut v8::PinScope<'s, '_>, p: &deno_core::cppgc::UnsafePtr<Performance>) -> Vec<v8::Local<'s, v8::Object>> {
    // the module fetches the loader finished since the last turn are on the timeline before it is read
    crate::core::timing::resource_timing::drain_module_fetches(scope);
    let held = unsafe { p.as_ref() }.entries.borrow().clone();
    held.iter().map(|g| v8::Local::new(scope, g)).collect()
}

fn set_array<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[v8::Local<'s, v8::Object>], rv: &mut v8::ReturnValue) {
    let arr = v8::Array::new(scope, items.len() as i32);
    for (i, v) in items.iter().enumerate() {
        let vv: v8::Local<v8::Value> = (*v).into();
        arr.set_index(scope, i as u32, vv);
    }
    rv.set(arr.into());
}

/// Push an entry and tell every observer that wants its type.
fn push_and_notify(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>, entry: v8::Local<v8::Object>, entry_type: &str) {
    unsafe { p.as_ref() }.entries.borrow_mut().push(v8::Global::new(scope, entry));
    notify(scope, p, entry, entry_type);
}

// ── paint timing as Blink keeps it ───────────────────────────────────────
//
// A rendering update that paints records `first-paint` / `first-contentful-paint` (and, as content grows,
// `largest-contentful-paint`) with Blink's two times (`PaintTiming`, paint_timing.cc 12.2-12.4): `paintTime`, the
// update's end, and `presentationTime`, when the compositor showed the frame -- coarsened UP to a 4 ms multiple
// unless the document is cross-origin isolated -- which is also the entry's `startTime` (an LCP's `renderTime`). The
// entries reach the timeline only AT that presentation time, by a task. Measured on stock Chrome 150: paintTime 216.7,
// presentationTime and startTime 232, nothing in `getEntries()` before then (after the first rAF, before the
// fourth), and the paint entries sorted after a resource that started at 218. These were minted at the update with
// the update's time and pushed at once: `getEntries()` listed them before resources Chrome lists them after, and
// `paintTime` read 0, `presentationTime` null.

/// When a rendering update ending at `paint_time` is presented: the next vsync on the persona's refresh grid
/// (`rendering::next_frame_ms`), coarsened up to 4 ms unless the document is cross-origin isolated.
fn presentation_time(scope: &mut v8::PinScope, paint_time: f64) -> f64 {
    let mut raw = paint_time + crate::core::intersection_observer::rendering::next_frame_ms(scope);
    if let Some(until) = presentation_held_until(scope, paint_time) {
        // commits resume at `until`: the frame is presented with the first one after it
        raw = raw.max(until + crate::core::intersection_observer::rendering::next_frame_ms(scope));
    }
    if crate::core::html::isolation_policy::cross_origin_isolated(scope) {
        // the vsync's own time, clamped as any timestamp is (it was the clamped paint time plus a raw distance)
        with_clock(scope, |c| c.clamped_ms(raw))
    } else {
        (raw / 4.0).ceil() * 4.0
    }
}

/// Chrome's PAINT HOLDING (`LocalFrameView::BeginLifecycleUpdates`, `kCommitDelayDefaultInMs`): once an outermost
/// main frame's document begins its lifecycle updates, its commits to the compositor are deferred until it paints
/// contentful content or 500 ms pass -- its rendering updates run, nothing is PRESENTED. So nothing that waits for
/// presentation -- a paint entry, a long animation frame that rendered -- reaches the timeline meanwhile. Chrome
/// 150, measured (`fixtures/an interactive-widget challenge_live/paint-hold-*`): a page whose body draws nothing has neither 400 ms in,
/// and both by 700; a bot challenge page's timeline, when its challenge first reads it, holds its first
/// long animation frame and no paint (2026-10-09). A frame's document never holds (`WillDoPaintHoldingForFCP`).
#[derive(Clone, Copy)]
struct PaintHold {
    /// the page timeline's time commits resume at if nothing contentful painted first
    until: f64,
    released: bool,
}

const PAINT_HOLD_MS: f64 = 500.0;

/// The page document's lifecycle updates began at `now_ms` (`cssom`, its first rendering update): commits held.
pub(crate) fn begin_paint_hold(scope: &mut v8::PinScope, now_ms: f64) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.has::<PaintHold>() {
        st.put(PaintHold { until: now_ms + PAINT_HOLD_MS, released: false });
    }
}

/// The page painted contentful content: commits resume, and what waited for a held presentation is presented
/// with the next frame.
pub(crate) fn release_paint_hold(scope: &mut v8::PinScope) {
    let held: Vec<u64> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<PaintHold>() {
            Some(h) if !h.released => h.released = true,
            _ => return,
        }
        st.try_borrow::<PresentationWaits>().map(|w| w.held.iter().copied().collect()).unwrap_or_default()
    };
    let next = crate::core::intersection_observer::rendering::next_frame_ms(scope);
    for id in held {
        // the entry's own, later task finds it gone
        crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_presented, id, next);
    }
}

/// When the page's presentation resumes, for a paint at `t` of the current realm -- `None` unless the page's
/// commits are held then. A frame's document holds nothing of its own, but what it draws reaches the screen through
/// the page's commits: Chrome 150 records no `first-paint` for a frame drawing text 400 ms into a page whose body
/// draws nothing, and both once the hold has ended. A frame's paint was presented at once.
fn presentation_held_until(scope: &mut v8::PinScope, t: f64) -> Option<f64> {
    // `t` and the answer are on this realm's timeline; a frame realm's clock is its own document's, the hold's the page's
    let frame = crate::install::templates::is_lazy_realm(scope) || crate::core::html::browsing_context::current_realm_ctx(scope).is_some();
    let shift = if frame { page_clock(scope).origin_ms() - with_clock(scope, |c| c.origin_ms()) } else { 0.0 };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let h = *st.try_borrow::<PaintHold>()?;
    let until = h.until + shift;
    (!h.released && t < until).then_some(until)
}

/// The realm's own timeline object: a lazy (frame) realm's `performance`, else the page's.
fn timeline_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    if crate::install::templates::is_lazy_realm(scope) {
        return crate::core::html::window_globals::window_singleton(scope, "performance").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    }
    held_timeline(scope).map(|t| v8::Local::new(scope, &t))
}

/// THE PAGE'S TIMELINE BEFORE IT EXISTS. A page realm restored from the snapshot has no
/// `performance` until its first read mints it (or a `PerformanceObserver`'s, whose constructor reads it). What the
/// run records before then waits here, as Rust data where it can -- a load, a paint, a largest paint -- and is put on
/// the timeline when [`mint_performance`] makes it ([`drain_pending`]), each entry made then.
/// - Before: dropped. A restored page that first read `performance` after its load found no resource and no paint
///   entry, where an installed one found them all (resource-timing, paint-timing and preload losses
///   in WPT, and a challenge provider challenge payload tell).
/// - R17 v1 minted the timeline at the first entry instead: +8 M instructions and ~13 ms on min.html, which never
///   reads it. Nothing is made here: no interface decoded, no entry object, no presentation task keeping the loop
///   alive.
#[derive(Default)]
struct PendingTimeline(Vec<Pending>);

enum Pending {
    /// A finished load, as `resource_timing::add` takes it.
    Resource(crate::core::timing::resource_timing::ResourceLoad),
    /// A rendering update's `paint` entries: their names, the update's paint time and its presentation time.
    Paint { names: Vec<&'static str>, paint_time: f64, presented: f64 },
    /// A `largest-contentful-paint` candidate (`add_lcp_entry`'s arguments and its two times).
    Lcp { node: u32, area: f64, url: String, paint_time: f64, presented: f64 },
    /// An entry already made (a long animation frame), now or at its presentation time.
    Made(Vec<(v8::Global<v8::Object>, &'static str)>, Option<f64>),
}

/// Whether this realm's entries wait for a timeline that is not made yet ([`PendingTimeline`]): the page realm with no
/// [`RealmTimeline`]. A frame's realm makes its own on first use.
pub(crate) fn timeline_deferred(scope: &mut v8::PinScope) -> bool {
    !crate::install::templates::is_lazy_realm(scope) && held_timeline(scope).is_none()
}

fn defer(scope: &mut v8::PinScope, item: Pending) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.has::<PendingTimeline>() {
        st.put(PendingTimeline::default());
    }
    st.borrow_mut::<PendingTimeline>().0.push(item);
}

/// A load that finished before the page's timeline was made ([`PendingTimeline`]).
pub(crate) fn defer_resource(scope: &mut v8::PinScope, load: crate::core::timing::resource_timing::ResourceLoad) {
    defer(scope, Pending::Resource(load));
}

/// Put what waited ([`PendingTimeline`]) on the timeline [`mint_performance`] just made, in the order it came. An entry
/// whose presentation has passed (and is not held by the paint holding) is on it at once -- the read that minted it
/// sees it, as an installed realm's read would -- and one still to be presented waits for its moment as ever.
fn drain_pending(scope: &mut v8::PinScope, perf: v8::Local<v8::Object>) {
    let items = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_take::<PendingTimeline>() {
            Some(p) => p.0,
            None => return,
        }
    };
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf.into()) else { return };
    let now = with_clock(scope, |c| c.now_ms());
    for item in items {
        match item {
            Pending::Resource(load) => crate::core::timing::resource_timing::add(scope, load),
            Pending::Paint { names, paint_time, presented } => {
                let made = paint_entries(scope, &names, paint_time, presented);
                deliver(scope, &p, made, Some(presented), now);
            }
            Pending::Lcp { node, area, url, paint_time, presented } => {
                let e = lcp_entry(scope, node, area, &url, paint_time, presented);
                deliver(scope, &p, vec![(e, "largest-contentful-paint")], Some(presented), now);
            }
            Pending::Made(entries, at) => {
                let made = entries.iter().map(|(e, t)| (v8::Local::new(scope, e), *t)).collect();
                deliver(scope, &p, made, at, now);
            }
        }
    }
}

fn deliver(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>, entries: Vec<(v8::Local<v8::Object>, &'static str)>, at: Option<f64>, now: f64) {
    match at {
        Some(at) if at > now || presentation_held_until(scope, now).is_some() => at_presentation(scope, entries, at, now),
        _ => {
            for (e, ty) in entries {
                push_and_notify(scope, p, e, ty);
            }
        }
    }
}

fn held_timeline(scope: &mut v8::PinScope) -> Option<v8::Global<v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<RealmTimeline>().map(|t| t.0.clone())
}

/// Entries waiting for their frame's presentation, by the task that will put them on their timeline.
#[derive(Default)]
struct PresentationWaits {
    next: u64,
    waiting: std::collections::HashMap<u64, (v8::Global<v8::Object>, Vec<(v8::Global<v8::Object>, &'static str)>)>,
    /// the waits whose presentation the page's paint holding put off ([`release_paint_hold`] brings them forward)
    held: std::collections::HashSet<u64>,
}

/// Put `entries` on this realm's timeline (and tell its observers) at presentation time `at`, by a task.
fn at_presentation(scope: &mut v8::PinScope, entries: Vec<(v8::Local<v8::Object>, &'static str)>, at: f64, now: f64) {
    let held = presentation_held_until(scope, now).is_some();
    let Some(perf) = timeline_object(scope) else {
        if timeline_deferred(scope) {
            let made = entries.into_iter().map(|(e, t)| (v8::Global::new(scope, e), t)).collect();
            defer(scope, Pending::Made(made, Some(at)));
        }
        return;
    };
    let entries_held = (v8::Global::new(scope, perf), entries.into_iter().map(|(e, t)| (v8::Global::new(scope, e), t)).collect());
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<PresentationWaits>() {
            st.put(PresentationWaits::default());
        }
        let w = st.borrow_mut::<PresentationWaits>();
        w.next += 1;
        let id = w.next;
        w.waiting.insert(id, entries_held);
        if held {
            w.held.insert(id);
        }
        id
    };
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_presented, id, (at - now).max(0.0));
}

fn cb_presented(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(0.0) as u64;
    let taken = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<PresentationWaits>().and_then(|w| {
            w.held.remove(&id);
            w.waiting.remove(&id)
        })
    };
    let Some((perf, entries)) = taken else { return };
    let perf = v8::Local::new(scope, &perf);
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf.into()) else { return };
    for (e, ty) in entries {
        let e = v8::Local::new(scope, &e);
        push_and_notify(scope, &p, e, ty);
    }
}

/// The `paint` entries (`"first-paint"`, `"first-contentful-paint"`) a rendering update records on
/// this realm's timeline -- `PerformancePaintTiming`s with Blink's paint and presentation times (above).
pub(crate) fn add_paint_entries(scope: &mut v8::PinScope, names: &[&'static str]) {
    if names.is_empty() {
        return;
    }
    let paint_time = with_clock(scope, |c| c.now_ms());
    let presented = presentation_time(scope, paint_time);
    if timeline_deferred(scope) {
        defer(scope, Pending::Paint { names: names.to_vec(), paint_time, presented });
        return;
    }
    let minted = paint_entries(scope, names, paint_time, presented);
    at_presentation(scope, minted, presented, paint_time);
}

/// The `paint` entries of one rendering update ([`add_paint_entries`]).
fn paint_entries<'s>(scope: &mut v8::PinScope<'s, '_>, names: &[&'static str], paint_time: f64, presented: f64) -> Vec<(v8::Local<'s, v8::Object>, &'static str)> {
    use crate::core::events::event::evt_slot_write_num;
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut minted = Vec::with_capacity(names.len());
    for name in names {
        let e = mint_entry(scope, "PerformancePaintTiming", name, "paint", presented, 0.0, undef);
        evt_slot_write_num(scope, e, "paintTime", paint_time);
        evt_slot_write_num(scope, e, "presentationTime", presented);
        minted.push((e, "paint"));
    }
    minted
}

/// Record a `largest-contentful-paint` entry for the largest contentful element the
/// compositor has painted (`core/css/cssom.rs` selects it + clamps an image to intrinsic size + gates on
/// a high-water-mark). `size` is that area (px²), `element` the node; `renderTime` (and `startTime`) the frame's
/// presentation, `paintTime` the update's end, as a paint entry's; `loadTime`/`url`/`id` take the defaults a
/// non-image LCP reports. The own members are slot-backed (data/slot_backed.json), so writing the slots is what
/// makes the getters answer. getEntriesByType filters it out (observer_only_type), so it reaches
/// PerformanceObservers only, exactly as Brave exposes LCP.
pub(crate) fn add_lcp_entry(scope: &mut v8::PinScope, node: u32, area: f64, url: &str) {
    let paint_time = with_clock(scope, |c| c.now_ms());
    let presented = presentation_time(scope, paint_time);
    if timeline_deferred(scope) {
        defer(scope, Pending::Lcp { node, area, url: url.to_string(), paint_time, presented });
        return;
    }
    let entry = lcp_entry(scope, node, area, url, paint_time, presented);
    at_presentation(scope, vec![(entry, "largest-contentful-paint")], presented, paint_time);
}

/// A `largest-contentful-paint` entry ([`add_lcp_entry`]).
fn lcp_entry<'s>(scope: &mut v8::PinScope<'s, '_>, node: u32, area: f64, url: &str, paint_time: f64, presented: f64) -> v8::Local<'s, v8::Object> {
    use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let entry = mint_entry(scope, "LargestContentfulPaint", "", "largest-contentful-paint", presented, 0.0, undef);
    evt_slot_write_num(scope, entry, "renderTime", presented);
    evt_slot_write_num(scope, entry, "paintTime", paint_time);
    evt_slot_write_num(scope, entry, "presentationTime", presented);
    evt_slot_write_num(scope, entry, "loadTime", 0.0);
    evt_slot_write_num(scope, entry, "size", area.round());
    let el = crate::page::node_object(scope, node);
    evt_slot_write(scope, entry, "element", el);
    // RENDER #6: an image LCP reports the image's request URL (Brave), a text LCP the empty string.
    let url_v = str_val(scope, url);
    evt_slot_write(scope, entry, "url", url_v);
    let empty = str_val(scope, "");
    evt_slot_write(scope, entry, "id", empty);
    entry
}

/// An `Instant` on this realm's timeline (ms since its time origin), as Blink stamps one
/// (`Performance::MonotonicTimeToDOMHighResTimeStamp`): the absolute time clamped, less the clamped origin. It was
/// the clamped reading NOW less the raw distance back to `t` -- a value off the tick grid, finer than any Blink
/// timestamp.
pub fn instant_to_ms(scope: &mut v8::PinScope, t: std::time::Instant) -> f64 {
    realm_ms(scope, spec::process_ms(t))
}

/// The navigation fetch that delivered this realm's document, `None` with no navigation timing.
fn document_navigation(scope: &mut v8::PinScope) -> Option<spec::FetchTiming> {
    match crate::core::html::browsing_context::realm_frame_nav_timing(scope) {
        Some(frame) => frame,
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<NavTiming>().and_then(|n| n.0.clone())
        }
    }
}

/// When this realm's document's navigation fetch started (its navigation entry's `fetchStart`). `None` with no
/// navigation timing.
pub(crate) fn document_fetch_start(scope: &mut v8::PinScope) -> Option<f64> {
    let nav = document_navigation(scope)?;
    Some(realm_ms(scope, nav.fetch_start).max(0.0))
}

/// A `long-animation-frame` entry on this realm's timeline (`core/timing/loaf.rs` decides when): `start`..`end` the
/// frame, `render` its rendering update's start (`None` for a long task that rendered nothing). A frame that
/// rendered carries the update's paint and presentation times and reaches the timeline at its presentation, as a
/// paint entry does; a task alone carries neither and reaches it at once (Chrome 150, measured).
pub(crate) fn add_long_animation_frame(scope: &mut v8::PinScope, start: f64, end: f64, render: Option<f64>, blocking: f64) {
    use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let e = mint_entry(scope, "PerformanceLongAnimationFrameTiming", "long-animation-frame", "long-animation-frame", start, end - start, undef);
    evt_slot_write_num(scope, e, "renderStart", render.unwrap_or(0.0));
    evt_slot_write_num(scope, e, "styleAndLayoutStart", render.unwrap_or(0.0));
    evt_slot_write_num(scope, e, "firstUIEventTimestamp", 0.0);
    // a span through Blink's clamper, as Chrome's (`ClampTimeResolution(TotalBlockingDuration())`)
    let isolated = realm_clock_isolated(scope);
    let blocking = spec::clamp_span_ms((blocking.max(0.0) * 1000.0).round() as i64, isolated);
    evt_slot_write_num(scope, e, "blockingDuration", blocking);
    let scripts = v8::Array::new(scope, 0);
    let _ = scripts.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    evt_slot_write(scope, e, "scripts", scripts.into());
    match render {
        Some(_) => {
            let presented = presentation_time(scope, end);
            evt_slot_write_num(scope, e, "paintTime", end);
            evt_slot_write_num(scope, e, "presentationTime", presented);
            let now = with_clock(scope, |c| c.now_ms());
            at_presentation(scope, vec![(e, "long-animation-frame")], presented, now);
        }
        None => {
            evt_slot_write_num(scope, e, "paintTime", 0.0);
            let Some(perf) = timeline_object(scope) else {
                if timeline_deferred(scope) {
                    let made = vec![(v8::Global::new(scope, e), "long-animation-frame")];
                    defer(scope, Pending::Made(made, None));
                }
                return;
            };
            let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf.into()) else { return };
            push_and_notify(scope, &p, e, "long-animation-frame");
        }
    }
}

/// `PerformancePaintTiming.toJSON` / `LargestContentfulPaint.toJSON`: Blink's
/// `PerformanceEntry::BuildJSONValue` -- name, entryType, startTime, duration, then the paint-timing pair -- and the
/// subclass's own after it (an LCP's size, renderTime, loadTime, id, url), read off the entry's slots. Both rows were
/// stubs answering `undefined`.
pub fn install_paint_timing(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "toJSON" => crate::install::live::method_row(scope, proto, member, cb_paint_entry_to_json),
        "getLargestInteractionContentfulPaint" => crate::install::live::method_row(scope, proto, member, cb_soft_nav_largest_icp),
        other => debug_assert!(false, "install_paint_timing: unexpected member {other}"),
    }
}

/// `PerformanceSoftNavigation.prototype.getLargestInteractionContentfulPaint()` (Chrome 152): the soft navigation's
/// largest interaction-contentful paint so far, or null.
fn cb_soft_nav_largest_icp(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ty = crate::core::events::event::evt_slot_read(scope, this, "entryType").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    if ty != "soft-navigation" {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    match crate::core::events::event::evt_slot_read(scope, this, "largestInteractionContentfulPaint") {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

fn cb_paint_entry_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const ENTRY: &[&str] = &["name", "entryType", "startTime", "duration"];
    const PAINT: &[&str] = &["paintTime", "presentationTime"];
    const LCP: &[&str] = &["size", "renderTime", "loadTime", "id", "url"];
    const LOAF: &[&str] = &["renderStart", "styleAndLayoutStart", "firstUIEventTimestamp", "blockingDuration", "scripts"];
    // `InteractionContentfulPaint::BuildJSONValue` (Chrome 152): the entry's, then these two.
    const ICP: &[&str] = &["interactionId", "largestContentfulPaint"];
    let this = args.this();
    // the receiver is an entry this engine minted when its slots name its type (`mint_entry`)
    let ty = crate::core::events::event::evt_slot_read(scope, this, "entryType").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let own: &[&str] = match ty.as_str() {
        "paint" => &[],
        "largest-contentful-paint" => LCP,
        "long-animation-frame" => LOAF,
        "interaction-contentful-paint" => ICP,
        _ => {
            crate::binding::protos::throw_type_error(scope, "Illegal invocation");
            return;
        }
    };
    // the paint-timing pair only for an entry that has one (a long task that rendered nothing has none)
    let painted = crate::core::events::event::evt_slot_read(scope, this, "presentationTime").is_some_and(|v| !v.is_null_or_undefined());
    let paint: &[&str] = if painted { PAINT } else { &[] };
    let nav: &[&str] = if navigation_id_exposed(scope) { &["navigationId"] } else { &[] };
    let o = v8::Object::new(scope);
    for k in ENTRY.iter().chain(nav).chain(paint).chain(own) {
        let v = crate::core::events::event::evt_slot_read(scope, this, k).unwrap_or_else(|| v8::undefined(scope).into());
        let Some(key) = v8::String::new(scope, k) else { continue };
        o.create_data_property(scope, key.into(), v);
    }
    rv.set(o.into());
}

/// Queue `entry` to every observer of its type, and schedule the delivery task.
///
/// The JS did this with a `Map` of pending buffers and a `setTimeout(..., 0)` per observer; this is
/// the same shape with the buffer on the observer itself (`core/timing/performance_observer.rs`) and
/// the task posted through the realm's own microtask/timer queue — so a Worker's callback still fires
/// on the Worker's scheduler, which is what `performance-worker.js`'s `observer` case asserts.
fn notify(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>, entry: v8::Local<v8::Object>, entry_type: &str) {
    let observers = unsafe { p.as_ref() }.observers.borrow().clone();
    for g in observers {
        let obs = v8::Local::new(scope, &g);
        crate::core::timing::performance_observer::queue_entry(scope, obs, entry, entry_type);
    }
}

/// Add a `resource` entry to this realm's timeline: Resource Timing's "add a PerformanceResourceTiming
/// entry". Observers are told first, whatever the buffer does; then the entry is buffered if the
/// buffer has room and nothing is waiting, and otherwise waits in the secondary buffer for the
/// `resourcetimingbufferfull` task (measured: size 2 and four reads keep two and fire twice; a handler
/// that grows the buffer keeps them all and fires once).
pub(crate) fn add_resource_entry(scope: &mut v8::PinScope, entry: v8::Local<v8::Object>) {
    // A FRAME's load goes on the frame's timeline (its realm has its own `performance`, made
    // on first read), where the frame's `PerformanceObserver`s are -- the page's is the page's loads'.
    let perf_obj: v8::Local<v8::Object> = if crate::install::templates::is_lazy_realm(scope) {
        let Some(p) = crate::core::html::window_globals::window_singleton(scope, "performance").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
        p
    } else {
        let Some(t) = held_timeline(scope) else { return };
        v8::Local::new(scope, &t)
    };
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf_obj.into()) else { return };
    notify(scope, &p, entry, "resource");
    let perf = unsafe { p.as_ref() };
    let count = resource_count(scope, &p);
    if count < perf.resource_buffer_size.get() as usize && perf.resource_secondary.borrow().is_empty() {
        perf.entries.borrow_mut().push(v8::Global::new(scope, entry));
        return;
    }
    perf.resource_secondary.borrow_mut().push(v8::Global::new(scope, entry));
    if perf.bufferfull_pending.replace(true) {
        return;
    }
    let data: v8::Local<v8::Value> = perf_obj.into();
    let task = build_fn_data(scope, cb_bufferfull_task, "", 0, data);
    // The engine's task, not the page's `setTimeout`.
    crate::core::intersection_observer::rendering::engine_task(scope, task, 0.0);
}

fn resource_count(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>) -> usize {
    entries_of(scope, p).into_iter().filter(|e| entry_slot(scope, *e, "entryType") == "resource").count()
}

/// The `resourcetimingbufferfull` task: fire the event at the timeline, then move what now fits from the
/// secondary buffer and drop the rest.
fn cb_bufferfull_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let perf_v = crate::binding::protos::body_data(scope, &args);
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf_v) else { return };
    let ty = str_val(scope, "resourcetimingbufferfull");
    if let Some(ev) = crate::binding::protos::new_global(scope, "Event", &[ty]) {
        let evv: v8::Local<v8::Value> = ev.into();
        crate::binding::protos::engine_call(scope, perf_v, "dispatchEvent", &[evv]);
    }
    let waiting = std::mem::take(&mut *unsafe { p.as_ref() }.resource_secondary.borrow_mut());
    for g in waiting {
        let count = resource_count(scope, &p);
        if count < unsafe { p.as_ref() }.resource_buffer_size.get() as usize {
            unsafe { p.as_ref() }.entries.borrow_mut().push(g);
        }
    }
    unsafe { p.as_ref() }.bufferfull_pending.set(false);
}

/// The entries of `entry_type` already in `perf`'s buffer, for an observer that asked for
/// `buffered: true` (measured: a `resource` observer registered after a fetch finished receives it).
pub(crate) fn buffered_entries<'s>(scope: &mut v8::PinScope<'s, '_>, perf: v8::Local<v8::Value>, entry_type: &str) -> Vec<v8::Local<'s, v8::Object>> {
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf) else { return Vec::new() };
    ensure_nav(scope, &p);
    let all = entries_of(scope, &p);
    sorted(scope, all).into_iter().filter(|e| entry_slot(scope, *e, "entryType") == entry_type).collect()
}

/// Entries in chronological order: by `startTime`, stable (Chrome's `getEntries*` answer the resource
/// entries of a page by start time, not by when each finished -- measured).
fn sorted<'s>(scope: &mut v8::PinScope<'s, '_>, items: Vec<v8::Local<'s, v8::Object>>) -> Vec<v8::Local<'s, v8::Object>> {
    let mut keyed: Vec<(f64, f64, v8::Local<'s, v8::Object>)> = items
        .into_iter()
        .map(|e| {
            let t = crate::core::events::event::evt_slot_read(scope, e, "startTime")
                .or_else(|| get_prop(scope, e, "startTime"))
                .and_then(|v| v.number_value(scope))
                .unwrap_or(0.0);
            // equal coarsened times: the order the requests really left in (resource entries only)
            let fine = crate::core::events::event::evt_slot_read(scope, e, crate::core::timing::resource_timing::ORDER_SLOT)
                .and_then(|v| v.number_value(scope))
                .unwrap_or(f64::NEG_INFINITY);
            (t, fine, e)
        })
        .collect();
    keyed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    keyed.into_iter().map(|(_, _, e)| e).collect()
}

/// Register / unregister an observer with this timeline (`PerformanceObserver.observe/disconnect`).
pub(crate) fn add_observer(scope: &mut v8::PinScope, perf: v8::Local<v8::Value>, obs: v8::Local<v8::Value>) {
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf) else { return };
    let mut list = unsafe { p.as_ref() }.observers.borrow_mut();
    if !list.iter().any(|g| v8::Local::new(scope, g) == obs) {
        list.push(v8::Global::new(scope, obs));
    }
}
pub(crate) fn remove_observer(scope: &mut v8::PinScope, perf: v8::Local<v8::Value>, obs: v8::Local<v8::Value>) {
    let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf) else { return };
    unsafe { p.as_ref() }.observers.borrow_mut().retain(|g| v8::Local::new(scope, g) != obs);
}

// ── Performance rows ─────────────────────────────────────────────────────────────────────────────

fn cb_perf_now(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if this_perf(scope, &args, "now").is_none() {
        return;
    }
    let n = now_ms(scope);
    rv.set_double(n);
}

fn cb_perf_time_origin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if this_perf_attr(scope, &args).is_none() {
        return;
    }
    let o = origin_ms(scope);
    rv.set_double(o);
}

/// `performance.interactionCount` — 0. This engine dispatches no user interactions through the Event
/// Timing pipeline, so the count of them is zero, which is also what the bundle's oracle records.
fn cb_perf_interaction_count(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if this_perf_attr(scope, &args).is_none() {
        return;
    }
    rv.set_double(0.0);
}

/// `performance.mark(name, options)`.
fn cb_perf_mark(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "mark") else { return };
    let Some(name) = idl_string(scope, args.get(0), "Performance", "mark") else { return };
    let mut start = now_ms(scope);
    let mut detail: v8::Local<v8::Value> = v8::null(scope).into();
    // `PerformanceMarkOptions`, converted as WebIDL converts it (Chrome 150): `detail`, then
    // `startTime` (a restricted double: NaN is a TypeError), each read once; a non-object is no dictionary. korvus
    // read `startTime` first and took NaN or a number for options.
    let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "PerformanceMarkOptions", "Failed to execute 'mark' on 'Performance': ") else { return };
    if let Some(n) = get_prop(scope, o, "startTime").filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) {
        // Chrome: "Failed to execute 'mark' on 'Performance': 'neg' cannot have a negative start
        // time." The JS accepted it and produced an entry before time began.
        if n < 0.0 {
            throw_type_error(scope, &format!("Failed to execute 'mark' on 'Performance': '{name}' cannot have a negative start time."));
            return;
        }
        start = n;
    }
    if let Some(d) = get_prop(scope, o, "detail").filter(|v| !v.is_undefined()) {
        detail = d;
    }
    if refuse_timing_mark_name(scope, &name, "Failed to execute 'mark' on 'Performance': ") {
        return;
    }
    let e = mint_entry(scope, "PerformanceMark", &name, "mark", start, 0.0, detail);
    push_and_notify(scope, &p, e, "mark");
    rv.set(e.into());
}

/// `new PerformanceMark(name, options)` — the one constructible entry type.
///
/// Chrome's `PerformanceMark` HAS a constructor (`new PerformanceMark("x")` is `[object
/// PerformanceMark]`, the bundle's oracle records it) and `new PerformanceMark()` throws for the
/// missing required argument. A constructed mark is NOT added to the timeline: only `performance
/// .mark()` buffers one, which is why this is a separate body from the row above.
pub(crate) fn cb_performance_mark_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("PerformanceMark"));
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'PerformanceMark': 1 argument required, but only 0 present.");
        return;
    }
    let Some(name) = idl_string(scope, args.get(0), "PerformanceMark", "constructor") else { return };
    let mut start = now_ms(scope);
    let mut detail: v8::Local<v8::Value> = v8::null(scope).into();
    // `PerformanceMarkOptions`, as `performance.mark` converts it
    let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "PerformanceMarkOptions", "Failed to construct 'PerformanceMark': ") else { return };
    if let Some(n) = get_prop(scope, o, "startTime").filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) {
        if n < 0.0 {
            throw_type_error(scope, &format!("Failed to construct 'PerformanceMark': '{name}' cannot have a negative start time."));
            return;
        }
        start = n;
    }
    if let Some(d) = get_prop(scope, o, "detail").filter(|v| !v.is_undefined()) {
        detail = d;
    }
    if refuse_timing_mark_name(scope, &name, "Failed to construct 'PerformanceMark': ") {
        return;
    }
    let e = mint_entry(scope, "PerformanceMark", &name, "mark", start, 0.0, detail);
    rv.set(e.into());
}

/// Resolve one end of a measure: a number is a timestamp, a string is the START TIME of the most
/// recent mark of that name (or a Navigation Timing L1 attribute name, the legacy spelling).
///
/// Chrome throws `SyntaxError: ... The mark 'nope' does not exist.` for a name that is neither; the
/// JS answered 0 and a site measuring against a mark it had not set got a plausible wrong number
/// instead of the error the spec promises.
fn resolve_mark(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>, v: v8::Local<v8::Value>) -> Option<f64> {
    if v.is_number() {
        return v.number_value(scope);
    }
    let Some(name) = idl_string(scope, v, "Performance", "measure") else { return None };
    for e in entries_of(scope, p).into_iter().rev() {
        if entry_slot(scope, e, "entryType") == "mark" && entry_slot(scope, e, "name") == name {
            return crate::core::events::event::evt_slot_read(scope, e, "startTime").and_then(|x| x.number_value(scope));
        }
    }
    // The Navigation Timing L1 legacy spelling (User Timing "convert a name to a timestamp", Blink
    // `UserTiming::FindExistingMarkStartTime`): `measure("x", "responseEnd")` starts at that field's offset from
    // `navigationStart` -- the fields are epoch ms -- and a field still 0 has not happened (or is not the page's
    // to see). It answered 0 for every name, so a measure from `responseEnd` ran from the time origin.
    if TIMING_NAMES.contains(&name.as_str()) {
        if !window_realm(scope) {
            throw_type_error(scope, &format!("Failed to execute 'measure' on 'Performance': When converting a mark name ('{name}') to a timestamp given a name that is a read only attribute in the PerformanceTiming interface, the global object has to be a Window object."));
            return None;
        }
        let f = timing_fields(scope);
        let v = f.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(0.0);
        if v == 0.0 {
            throw_dom(scope, &format!("Failed to execute 'measure' on 'Performance': '{name}' is empty: either the event hasn't happened yet, or it would provide cross-origin timing information."), "InvalidAccessError");
            return None;
        }
        return Some(v - f[0].1);
    }
    throw_dom(scope, &format!("Failed to execute 'measure' on 'Performance': The mark '{name}' does not exist."), "SyntaxError");
    None
}

/// `performance.measure(name, startOrMeasureOptions, endMark)`.
///
/// The three TypeErrors are User Timing L3 step 1, and all three are Chrome's (measured; the second
/// is also in the bundle's oracle as `measure("pm-b", { detail: 41 })`):
///  * a non-empty options dict AND an `endMark`,
///  * a non-empty options dict with neither `start` nor `end`,
///  * an options dict with all three of `start`, `duration`, `end`.
fn cb_perf_measure(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "measure") else { return };
    let Some(name) = idl_string(scope, args.get(0), "Performance", "measure") else { return };
    let start_or_opts = args.get(1);
    // The three arguments as WebIDL converts them, before any step runs (Chrome 150):
    // `(DOMString or PerformanceMeasureOptions)` is the dictionary for `undefined`, `null` and every object --
    // `detail`, `duration`, `end`, `start`, each read once -- and a string otherwise (a number is a mark NAME here,
    // not a time); `endMark` is a string. korvus read the dictionary in its own order, took `null` for a mark named
    // "null", a number for a timestamp, and converted `endMark` only when it came to use it.
    let opts = if start_or_opts.is_null_or_undefined() || start_or_opts.is_object() {
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, start_or_opts, "PerformanceMeasureOptions", "Failed to execute 'measure' on 'Performance': ") else { return };
        Some(o)
    } else {
        None
    };
    let start_mark = match opts {
        Some(_) => None,
        None => {
            let Some(m) = idl_string(scope, start_or_opts, "Performance", "measure") else { return };
            Some(m)
        }
    };
    let end_mark = match args.get(2) {
        e if e.is_undefined() => None,
        e => {
            let Some(m) = idl_string(scope, e, "Performance", "measure") else { return };
            Some(m)
        }
    };
    let mut detail: v8::Local<v8::Value> = v8::null(scope).into();
    let start_time;
    let end_time;

    let non_empty_opts = opts.filter(|o| ["start", "end", "duration", "detail"].iter().any(|k| dict_get(scope, *o, k).is_some()));
    if let Some(o) = non_empty_opts {
        let start_v = dict_get(scope, o, "start");
        let end_v = dict_get(scope, o, "end");
        let duration_v = dict_get(scope, o, "duration");
        let detail_v = dict_get(scope, o, "detail");
        let has_start = start_v.is_some();
        let has_end = end_v.is_some();
        let has_duration = duration_v.is_some();
        let non_empty = has_start || has_end || has_duration || detail_v.is_some();
        if non_empty {
            if end_mark.is_some() {
                throw_type_error(scope, "Failed to execute 'measure' on 'Performance': If a non-empty PerformanceMeasureOptions object was passed, |end_mark| must not be passed.");
                return;
            }
            if !has_start && !has_end {
                throw_type_error(scope, "Failed to execute 'measure' on 'Performance': If a non-empty PerformanceMeasureOptions object was passed, at least one of its 'start' or 'end' properties must be present.");
                return;
            }
            if has_start && has_end && has_duration {
                throw_type_error(scope, "Failed to execute 'measure' on 'Performance': If a non-empty PerformanceMeasureOptions object was passed, it must not have all of its 'start', 'duration', and 'end' properties defined");
                return;
            }
        }
        if let Some(d) = detail_v {
            detail = d;
        }
        let duration = match duration_v {
            Some(d) => d.number_value(scope),
            None => None,
        };
        let resolved_start = match start_v {
            Some(v) => match resolve_mark(scope, &p, v) {
                Some(n) => Some(n),
                None => return,
            },
            None => None,
        };
        let resolved_end = match end_v {
            Some(v) => match resolve_mark(scope, &p, v) {
                Some(n) => Some(n),
                None => return,
            },
            None => None,
        };
        match (resolved_start, resolved_end, duration) {
            (Some(s), Some(e), _) => {
                start_time = s;
                end_time = e;
            }
            (Some(s), None, Some(d)) => {
                start_time = s;
                end_time = s + d;
            }
            (Some(s), None, None) => {
                start_time = s;
                end_time = now_ms(scope);
            }
            (None, Some(e), Some(d)) => {
                start_time = e - d;
                end_time = e;
            }
            (None, Some(e), None) => {
                start_time = 0.0;
                end_time = e;
            }
            // The empty dictionary: `measure("f", {})` is `measure("f")`, which Chrome answers with
            // a measure from the time origin to now.
            (None, None, _) => {
                start_time = 0.0;
                end_time = now_ms(scope);
            }
        }
    } else {
        // an empty dictionary (or none) starts at the time origin; a string names the start mark
        start_time = match start_mark {
            None => 0.0,
            Some(m) => {
                let mv = str_val(scope, &m);
                match resolve_mark(scope, &p, mv) {
                    Some(n) => n,
                    None => return,
                }
            }
        };
        end_time = match end_mark {
            None => now_ms(scope),
            Some(m) => {
                let mv = str_val(scope, &m);
                match resolve_mark(scope, &p, mv) {
                    Some(n) => n,
                    None => return,
                }
            }
        };
    }

    // `duration` is end time MINUS start time, unclamped (User Timing L3 step 8): a start mark placed after
    // "now" gives a NEGATIVE duration whose end is now -- measured on Chrome 150 (`measure("A", far)` with
    // `far` 5 s ahead: `duration < 0`, `startTime + duration` = now). The clamp to 0 that stood here put the
    // end at the start mark; WPT `user-timing/measure-l3.any.js` caught it once a page reached the test in
    // under 123 ms.
    let e = mint_entry(scope, "PerformanceMeasure", &name, "measure", start_time, end_time - start_time, detail);
    push_and_notify(scope, &p, e, "measure");
    rv.set(e.into());
}

/// `clearMarks(name)` / `clearMeasures(name)` / `clearResourceTimings()`, by the entry type bound as
/// this function's `data` at install time.
///
/// `clearMarks(undefined)` clears every mark and `clearMarks("undefined")` clears the one named
/// "undefined" — measured in Chrome, and the distinction the JS's `name === undefined` test already
/// drew.
fn cb_perf_clear(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let want_type = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let member = match want_type.as_str() {
        "mark" => "clearMarks",
        "measure" => "clearMeasures",
        _ => "clearResourceTimings",
    };
    let Some(p) = this_perf(scope, &args, member) else { return };
    let filter_name = match args.get(0) {
        v if v.is_undefined() => None,
        v => match idl_string(scope, v, "Performance", member) {
            Some(s) => Some(s),
            None => return,
        },
    };
    let mut keep: Vec<v8::Global<v8::Object>> = Vec::new();
    for e in entries_of(scope, &p) {
        let drop_it = entry_slot(scope, e, "entryType") == want_type
            && filter_name.as_ref().is_none_or(|n| entry_slot(scope, e, "name") == *n);
        if !drop_it {
            keep.push(v8::Global::new(scope, e));
        }
    }
    *unsafe { p.as_ref() }.entries.borrow_mut() = keep;
    if want_type == "resource" && filter_name.is_none() {
        unsafe { p.as_ref() }.resource_secondary.borrow_mut().clear();
    }
}

/// `setResourceTimingBufferSize(n)`.
fn cb_perf_set_buffer_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "setResourceTimingBufferSize") else { return };
    let n = args.get(0).number_value(scope).unwrap_or(0.0);
    let n = if n.is_finite() && n > 0.0 { n as u32 } else { 0 };
    unsafe { p.as_ref() }.resource_buffer_size.set(n);
}

/// Entry types delivered to PerformanceObservers but NOT added to the performance timeline that
/// getEntries/getEntriesByType/getEntriesByName read (Chrome: `availableFromTimeline` is false for
/// these). A page sees them ONLY through a PerformanceObserver (`buffered: true` replays past ones).
/// Measured on Brave 151 for `largest-contentful-paint`: a buffered observer gets the entries while
/// `getEntriesByType('largest-contentful-paint')` is `[]`.
fn observer_only_type(t: &str) -> bool {
    matches!(
        t,
        "largest-contentful-paint" | "layout-shift" | "element" | "event" | "first-input"
    )
}

fn cb_perf_get_entries(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "getEntries") else { return };
    ensure_nav(scope, &p);
    let all = entries_of(scope, &p);
    let items: Vec<_> = sorted(scope, all)
        .into_iter()
        .filter(|e| !observer_only_type(&entry_slot(scope, *e, "entryType")))
        .collect();
    set_array(scope, &items, &mut rv);
}

fn cb_perf_get_entries_by_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "getEntriesByType") else { return };
    let Some(want) = idl_string(scope, args.get(0), "Performance", "getEntriesByType") else { return };
    // Observer-only types are not on the timeline (measured vs Brave): getEntriesByType returns [].
    if observer_only_type(&want) {
        set_array(scope, &[], &mut rv);
        return;
    }
    ensure_nav(scope, &p);
    let all = entries_of(scope, &p);
    let items: Vec<_> = sorted(scope, all).into_iter().filter(|e| entry_slot(scope, *e, "entryType") == want).collect();
    set_array(scope, &items, &mut rv);
}

fn cb_perf_get_entries_by_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "getEntriesByName") else { return };
    let Some(want) = idl_string(scope, args.get(0), "Performance", "getEntriesByName") else { return };
    // `getEntriesByName("x", undefined)` filters by name only — measured.
    let want_type = match args.get(1) {
        v if v.is_undefined() => None,
        v => match idl_string(scope, v, "Performance", "getEntriesByName") {
            Some(s) => Some(s),
            None => return,
        },
    };
    ensure_nav(scope, &p);
    let all = entries_of(scope, &p);
    let items: Vec<_> = sorted(scope, all)
        .into_iter()
        .filter(|e| {
            let ty = entry_slot(scope, *e, "entryType");
            !observer_only_type(&ty)
                && entry_slot(scope, *e, "name") == want
                && want_type.as_ref().is_none_or(|t| ty == *t)
        })
        .collect();
    set_array(scope, &items, &mut rv);
}

/// `performance.toJSON()` — `{ timeOrigin, timing, navigation }` in a Window, `{ timeOrigin }` in a
/// Worker, because the two partials are `[Exposed=Window]`. Measured in Chrome
/// (`Object.keys(performance.toJSON())`), and the JS had no `toJSON` of its own at all: the member
/// came from the shell's D2 stub and answered `undefined`.
fn cb_perf_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf(scope, &args, "toJSON") else { return };
    let o = v8::Object::new(scope);
    let origin = origin_ms(scope);
    let ov: v8::Local<v8::Value> = v8::Number::new(scope, origin).into();
    set_prop(scope, o, "timeOrigin", ov);
    if let Some(t) = unsafe { p.as_ref() }.timing.borrow().clone() {
        let tv = v8::Local::new(scope, &t);
        set_prop(scope, o, "timing", tv);
    }
    if let Some(n) = unsafe { p.as_ref() }.navigation.borrow().clone() {
        let nv = v8::Local::new(scope, &n);
        set_prop(scope, o, "navigation", nv);
    }
    rv.set(o.into());
}

/// A `[SameObject]` partial: `timing`, `navigation` or `eventCounts`, by the slot bound as `data`.
///
/// `performance.timing === performance.timing` in Chrome (measured), and each is built once when the
/// timeline is minted, not on first read — so the three cannot be seen to appear.
fn cb_perf_same_object(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = this_perf_attr(scope, &args) else { return };
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let held = match which.as_str() {
        "timing" => unsafe { p.as_ref() }.timing.borrow().clone(),
        "navigation" => unsafe { p.as_ref() }.navigation.borrow().clone(),
        _ => unsafe { p.as_ref() }.event_counts.borrow().clone(),
    };
    match held {
        Some(g) => {
            let v = v8::Local::new(scope, &g);
            rv.set(v);
        }
        None => rv.set_undefined(),
    }
}

/// `performance.memory` — Chrome's non-standard heap window.
///
/// A FRESH `MemoryInfo` per read (`performance.memory !== performance.memory`, measured) whose three
/// numbers sit on a prototype Chrome does not expose as a global, in Chrome's enumeration order
/// `totalJSHeapSize, usedJSHeapSize, jsHeapSizeLimit`. This is `singletons.rs::install_memory_info`
/// moved across the crate boundary — the runtime had to own it only because it read `HostPersona`,
/// and `spec::PersonaHandle` on the kernel `Context` is the same value from here (invariant I9).
///
/// WHAT THE THREE NUMBERS NOW ARE, and what they were. They were the PERSONA's, read
/// out of `values.json` and returned unchanged: three frozen byte counts, the same on every read,
/// unmoved by a page that allocates 300 MB, unmoved across a navigation. That is not what any
/// Chromium does with any of them, and it is a stronger tell than the magnitude the brief opened
/// with — a page allocates and watches the number not move. See [`memory_info`] for the rule that
/// replaced it, measured against all four installed browsers.
/// This realm's `MemoryInfo` prototype -- built on first use and parked on the realm's global, so every
/// `performance.memory` of one realm shares one prototype.
fn realm_memory_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let global = scope.get_current_context().global(scope);
    let k = memory_proto_key(scope);
    if let Some(o) = global.get_private(scope, k).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return o;
    }
    let mp = v8::Object::new(scope);
    // the interface the prototype is (a private stamp): what the recorder names its getters by -- they were recorded
    // as `Object.get totalJSHeapSize`, so the side-by-side never paired them with Chrome's `MemoryInfo` getters
    let iname = str_val(scope, "MemoryInfo");
    crate::binding::protos::stamp_iface_name(scope, mp.into(), iname);
    // `Object.prototype.toString.call(performance.memory)` is `[object MemoryInfo]`. There is
    // no `MemoryInfo` in the capture's globals, so the prototype is built here and stays
    // unreachable by name, exactly as Chrome's is.
    {
        let tag = v8::Symbol::get_to_string_tag(scope);
        let name = str_val(scope, "MemoryInfo");
        let mut td = v8::PropertyDescriptor::new_from_value_writable(name, false);
        td.set_enumerable(false);
        td.set_configurable(true);
        mp.define_property(scope, tag.into(), &td);
    }
    // Chrome's enumeration order, measured 2026-09-11.
    for (i, k) in ["totalJSHeapSize", "usedJSHeapSize", "jsHeapSizeLimit"].iter().enumerate() {
        let idx: v8::Local<v8::Value> = v8::Integer::new(scope, i as i32).into();
        let g = build_fn_data(scope, cb_heap_number, &format!("get {k}"), 0, idx);
        let mut pd = v8::PropertyDescriptor::new_from_get_set(g.into(), v8::undefined(scope).into());
        pd.set_enumerable(true);
        pd.set_configurable(true);
        if let Some(kk) = v8::String::new(scope, k) {
            mp.define_property(scope, kk.into(), &pd);
        }
    }
    global.set_private(scope, k, mp.into());
    mp
}

fn memory_proto_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus_perf_memory_proto").unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn cb_perf_memory(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if this_perf_attr(scope, &args).is_none() {
        return;
    }
    let mp = realm_memory_proto(scope);
    let info = crate::binding::protos::mint_on(scope, mp.into());
    let k = memory_brand_key(scope);
    let t = v8::Boolean::new(scope, true);
    info.set_private(scope, k, t.into());
    rv.set(info.into());
}

/// The brand a `MemoryInfo` carries -- the object `performance.memory` minted, and nothing else. Its three
/// getters are a `[LegacyNoInterfaceObject]` interface's attributes, and Chrome 150 refuses every other
/// receiver -- `{}`, `Object.create(proto)`, the prototype itself -- with `TypeError: Illegal invocation`
/// (measured 2026-10-09, a page and a cross-origin frame); these answered a heap number for any object.
fn memory_brand_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus_memory_info").unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// One heap number. `data` is the member's position in Chrome's enumeration order, which is also
/// [`memory_info`]'s order: `totalJSHeapSize, usedJSHeapSize, jsHeapSizeLimit`.
fn cb_heap_number(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let k = memory_brand_key(scope);
    if !args.this().get_private(scope, k).is_some_and(|v| v.is_true()) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let slot = crate::binding::protos::body_data(scope, &args).uint32_value(scope).unwrap_or(0) as usize;
    match memory_info(scope) {
        Some(m) => rv.set_double(m[slot.min(2)]),
        None => rv.set_undefined(),
    }
}

// ── Chromium's `MemoryInfo`, measured ────────────────────────────────────────────────────────────
//
// MEASURED 2026-09-18, this box, four fresh profiles, `--remote-debugging-port=<a port I picked>`
// (never `=0`, L18.1) and a loopback fixture server:
//
//   Brave 151.1.93.138 (Chrome/151.0.7922.173)   Chrome/150.0.7871.46
//   Edg/151.0.4129.93                            Opera (Chrome/151.0.7922.176)
//
// ALL FOUR ANSWER IDENTICALLY, row for row. There is no vendor variation in this surface to carry.
//
// 1. THERE ARE TWO MODES, and the document picks one.  On `http://127.0.0.1:<port>/` the three
//    numbers are the renderer's RAW V8 heap: unrounded, and different in every browser and on every
//    read (`1255591 / 1866151`, `518472 / 1052080`, ...).  On `about:blank`, on `data:text/html,…`
//    and on `file:///…` all three are QUANTIZED, and all four browsers print the same triple for an
//    idle page: `10000000 / 10000000 / 3760000000`.  The split is Chromium's `Precision::kPrecise`
//    vs `kBucketized`, chosen by whether the renderer process is locked to a site — which is exactly
//    "does this document have a tuple origin", the predicate `feature_policy::document_origin`
//    already computes for this engine (it answers `"null"` for all three quantized cases).
//
// 2. THE QUANTIZER IS A LADDER OF 100 BUCKETS and the value is the SMALLEST bucket >= the size,
//    clamped to the last.  [`HEAP_BUCKETS`] builds it.  Every one of the 16 distinct values read out
//    of the four browsers is a member of that ladder, at these indices:
//
//      10000000 [0]   13400000 [5]   14300000 [6]    21700000 [13]   23100000 [14]   33100000 [20]
//      50400000 [27]  64000000 [31]  97400000 [38]  139000000 [44]  212000000 [51]  322000000 [58]
//     553000000 [67] 842000000 [74] 1280000000 [81] 3760000000 [99]
//
//    `jsHeapSizeLimit` is why the last one matters: the real limit (4395630592) is ABOVE the top of
//    the ladder, so it clamps, and every quantized page in every browser reports 3760000000 for it.
//
// 3. A READING IS RATE-LIMITED, and that is the half the old code got most wrong.  Two reads of
//    `usedJSHeapSize` in one expression are EQUAL; allocating 8 MB between them changes nothing
//    until enough time has passed.  Bracketed at 1 ms on the precise path, measuring the page's own
//    elapsed time BETWEEN the two reads rather than the busy-wait: 50 ms apart still reports the old
//    number, 52 ms apart reports the new one, and every step from 37 to 50 ms reports the old one.
//    So the window is 50 ms and the comparison is `<`, which is what [`memory_info`] writes.  On the
//    quantized path the same latch is TWENTY MINUTES, and that is measured rather than assumed: a
//    page that read once, then allocated 300 MB, was polled every 10 s for 25 minutes and reported
//    bucket [0] at 1150.1 s and bucket [58] at 1200.1 s — it moved at 20 min to the sample, and the
//    bucket it moved to is the one that covers the 300 MB.  A page that allocated 200 MB BEFORE its
//    first read reported bucket [51] straight away, so it is a latch on the READING, not a refusal.
//
// WHAT IS NOT REPRODUCED HERE, deliberately: `jsHeapSizeLimit` stays the PERSONA's number. It is the
// browser's `--max-old-space-size` configuration, not a fact about this engine's heap, and it is the
// one row of the three that already matched the reference browser byte for byte.

/// How many buckets Chromium's quantizer has, and the two ends of the EXPONENTIAL it walks. The
/// last BUCKET is not `HEAP_LADDER_TO`: it is that step rounded down to three significant digits,
/// which is why the top of the ladder reads 3760000000 and not 4000000000.
const HEAP_BUCKETS: usize = 100;
const HEAP_LADDER_FROM: f64 = 10_000_000.0;
const HEAP_LADDER_TO: f64 = 4_000_000_000.0;

/// The rate limit on one reading, in ms: 50 on the precise path, 20 minutes on the quantized one.
const HEAP_REFRESH_PRECISE_MS: f64 = 50.0;
const HEAP_REFRESH_BUCKETED_MS: f64 = 20.0 * 60.0 * 1000.0;

/// `v` rounded DOWN to three significant decimal digits — the shape every bucket has.
///
/// It is why the ladder reads `139000000` where the exponential itself is `139604200`, and why
/// `97400000` keeps five digits of zeros and `1280000000` seven. Every result is a small integer
/// times a power of ten, so it is exact in an `f64` over the whole range the ladder covers.
fn three_significant_digits(v: f64) -> f64 {
    if !(v > 0.0) {
        return 0.0;
    }
    let step = 10f64.powi(v.log10().floor() as i32 - 2);
    (v / step).floor() * step
}

/// Chromium's `QuantizeMemorySize`: the smallest bucket that is not smaller than `size`, clamped to
/// the top of the ladder. A size below the first bucket therefore reports the first bucket, and a
/// size above the last — `jsHeapSizeLimit` always is — reports the last.
fn quantize_heap_size(size: f64) -> f64 {
    let ratio = (HEAP_LADDER_TO / HEAP_LADDER_FROM).powf(1.0 / HEAP_BUCKETS as f64);
    let mut bucket = HEAP_LADDER_FROM;
    for i in 0..HEAP_BUCKETS {
        bucket = three_significant_digits(HEAP_LADDER_FROM * ratio.powi(i as i32));
        if size <= bucket {
            break;
        }
    }
    bucket
}

/// ONE RENDERER PROCESS'S `performance.memory` STATE, as Chrome keeps it per process: the
/// engine's heap when the process's first realm was ready (what [`memory_info`] takes away), and Chromium's per-thread
/// `HeapSizeCache` -- the last reading and when it was taken. The latch is the point: a page may not watch its own
/// allocations land by polling `performance.memory`, so a reading younger than the mode's window is handed back
/// unchanged, to every realm of the process.
#[derive(Default, Clone, Copy)]
struct ProcessHeap {
    /// `(total, used)`, external memory included.
    baseline: Option<(f64, f64)>,
    /// `(when, [total, used, limit])`, `when` on the wall clock every realm shares (a realm's own clock starts at its
    /// own time origin).
    cache: Option<(f64, [f64; 3])>,
}

/// The processes Chrome would run this engine's realms in, under site isolation: the PAGE's, which every realm of
/// the page's site shares -- a same-site frame reads its page's heap, as it is the same renderer's -- and one per
/// other site, a cross-site frame's.
#[derive(Default)]
struct HeapProcesses {
    page: ProcessHeap,
    sites: std::collections::HashMap<String, ProcessHeap>,
}

fn heap_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// The isolate's heap now, `(total, used)`, each with V8's external memory (see [`memory_info`]).
fn heap_now(scope: &mut v8::PinScope) -> (f64, f64) {
    let stats = scope.get_heap_statistics();
    let external = stats.external_memory() as f64;
    (stats.total_heap_size() as f64 + external, stats.used_heap_size() as f64 + external)
}

/// THE ENGINE'S OWN HEAP, read when the page's realm is ready -- the end of the engine's install, before any page
/// script runs: what [`memory_info`] takes away. This engine keeps its whole Web API in the heap
/// from the start (measured on an empty page: 10,035,014 used where stock Chrome 150 reads 515,778), which Chrome's
/// snapshot and lazily made interface objects keep out. Nothing to do for a persona that measured no empty document
/// (the raw heap is reported).
///
/// ITS LIVE HEAP: the install's garbage is collected first. Read raw, the baseline held the install's temporaries
/// (1.25 MB of 9.6 MB in a release build, ~9 MB in a debug one), and the first collection after it took them out of
/// the PAGE's count instead -- a same-site frame of a page holding 4 MiB read 3.1 MB after a scavenge (6.1 MB before
/// it; Chrome 5.1 MB) and Chrome's empty document after a mark-compact, so the reading swung with the collector's
/// timing. V8 has no public request for one collection that does not also hand every page a `gc` function
/// (`RequestGarbageCollectionForTesting` needs `--expose-gc`); `LowMemoryNotification` collects all it can and keeps
/// no state -- measured 4-7 ms with the template cache warm, 20-27 ms cold. It also empties V8's compilation cache,
/// which holds nothing of the page's yet.
pub fn mark_heap_baseline(scope: &mut v8::PinScope) {
    if persona_empty_document(scope).is_none() {
        return;
    }
    scope.low_memory_notification();
    let now = heap_now(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<HeapProcesses>().is_none() {
        st.put(HeapProcesses::default());
    }
    st.borrow_mut::<HeapProcesses>().page.baseline = Some(now);
}

/// A frame's realm is ready: its heap then, kept on its global. Read only if the frame turns out to be ANOTHER
/// site's -- another process in Chrome, whose heap started then -- and only by the first realm of that site to ask
/// ([`process_of`]).
pub(crate) fn mark_frame_heap_baseline(scope: &mut v8::PinScope) {
    let (t, u) = heap_now(scope);
    let global = scope.get_current_context().global(scope);
    let k = heap_key(scope, "korvus_heap_baseline");
    let a = v8::Array::new(scope, 2);
    let t = v8::Number::new(scope, t);
    let u = v8::Number::new(scope, u);
    a.set_index(scope, 0, t.into());
    a.set_index(scope, 1, u.into());
    global.set_private(scope, k, a.into());
}

/// The running frame realm's own baseline ([`mark_frame_heap_baseline`]).
fn frame_heap_baseline(scope: &mut v8::PinScope) -> Option<(f64, f64)> {
    let global = scope.get_current_context().global(scope);
    let k = heap_key(scope, "korvus_heap_baseline");
    let a = global.get_private(scope, k).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
    let t = a.get_index(scope, 0)?.number_value(scope)?;
    let u = a.get_index(scope, 1)?.number_value(scope)?;
    Some((t, u))
}

/// Which process the running realm is in: `None` for the page's (its own realm, and any of the page's site), else
/// the other site's -- the site naming the process its realm runs in (`realm_process_urls`: an `about:` frame's is
/// its creator's).
fn process_of(scope: &mut v8::PinScope) -> Option<String> {
    let (_, site_url) = crate::core::html::browsing_context::realm_process_urls(scope);
    let page = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
    };
    let site = |u: &str| url::Url::parse(u).ok().and_then(|u| transport::schemeful_site(&u)).unwrap_or_else(|| u.to_string());
    let (realm, page) = (site(&site_url), site(&page));
    (realm != page).then_some(realm)
}

/// The persona's empty-document heap (`memory.emptyDocument`), `(total, used)`.
fn persona_empty_document(scope: &mut v8::PinScope) -> Option<(f64, f64)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)?.0.memory().empty_document
}

/// The persona's `jsHeapSizeLimit`, or `None` when the profile captured no heap window at all — in
/// which case `performance.memory` is not installed, which is the honest answer for a profile that
/// does not carry the numbers (`install_performance`'s `"memory"` arm).
fn persona_heap_limit(scope: &mut v8::PinScope) -> Option<f64> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let persona = crate::binding::protos::persona_handle(&st)?;
    let limit = persona.0.memory().js_heap_size_limit;
    (limit > 0.0).then_some(limit)
}

/// The three numbers, in Chrome's enumeration order `[total, used, limit]`, by the rule measured in
/// the block above: THIS engine's real V8 heap, quantized exactly when the browser would quantize
/// it, latched for exactly as long as the browser latches it.
fn memory_info(scope: &mut v8::PinScope) -> Option<[f64; 3]> {
    let limit = persona_heap_limit(scope)?;
    // A tuple origin is a site the renderer process can be locked to, and a locked process is the
    // one Chromium answers precisely from. `about:blank`, `data:` and `file:` answer `"null"` here
    // and are the three the measurement found quantized.
    let precise = crate::core::feature_policy::document_origin(scope) != "null";
    // THIS REALM'S CLOCK, off `OpState` in Rust (FP4) — the same reading `performance.now()` is a
    // difference of, so a page cannot move the latch by overwriting `performance`.
    let now = origin_ms(scope) + now_ms(scope);
    let window = if precise { HEAP_REFRESH_PRECISE_MS } else { HEAP_REFRESH_BUCKETED_MS };
    let process = process_of(scope);
    let own = if process.is_some() { frame_heap_baseline(scope) } else { None };
    let state = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<HeapProcesses>().is_none() {
            st.put(HeapProcesses::default());
        }
        let all = st.borrow_mut::<HeapProcesses>();
        match &process {
            None => all.page,
            Some(site) => *all.sites.entry(site.clone()).or_insert(ProcessHeap { baseline: own, cache: None }),
        }
    };
    if let Some((at, v)) = state.cache {
        if now - at < window {
            return Some(v);
        }
    }
    // BOTH rows carry V8's EXTERNAL memory, which is where a `Uint8Array`'s backing store lives.
    // Measured on the reference browser: 64 MiB of `Uint8Array` moved Brave's `totalJSHeapSize` by
    // 67108867 and its `usedJSHeapSize` by 67119759 -- 67108864 is 64 MiB to the byte, and the
    // remainder is the JS that wrapped it. V8's own `used_heap_size` moves by neither (measured in
    // this engine: 9839400 -> 13509736 for the same allocation, while `external_memory` went
    // 98989 -> 67207853). Reading the JS heap alone would have reported a page's 64 MiB as 3 MB.
    let (total, used) = heap_now(scope);
    let mut v = [total, used, limit];
    // CHROME'S EMPTY DOCUMENT, PLUS WHAT HAS BEEN ALLOCATED SINCE THE PROCESS'S FIRST REALM WAS READY.
    // The raw heap of this engine is its Web API's as much as the page's: an empty page read 10 MB here,
    // 0.5 MB in Chrome 150, and a bot-challenge frame -- which reads the three numbers into its payload -- 65 MB
    // where Chrome's read 20 MB. What the page allocates is V8's; what the engine built before the page ran is not
    // Chrome's and is taken away, Chrome's own empty document put in its place (the persona's measurement). A persona
    // that measured none keeps the raw heap. Not exact, and not meant to be: Chrome's own reading moves with its
    // collector's timing, and this engine's isolate is one for all of its realms -- a cross-site frame's reading
    // counts what the page allocated beside it (an out-of-process frame, `KORVUS_OOPIF`, has an isolate of its own).
    // Committed memory is never below what is used plus the slack Chrome's empty document already had: this engine's
    // young generation stays committed at the size its install grew it to, so a page's first allocations commit
    // nothing here that Chrome's fresh renderer would.
    // DIAGNOSTIC, analysis builds only: KORVUS_DIAG_HEAP_SCALE=<f> scales what was allocated
    // since the process's first realm, to tell whether a verdict follows performance.memory's size (the pinned-session
    // replay read 66 MB used in the challenge frame where Chrome read 30 MB)
    #[cfg(feature = "analysis")]
    let scale = std::env::var("KORVUS_DIAG_HEAP_SCALE").ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.0);
    #[cfg(not(feature = "analysis"))]
    let scale = 1.0;
    if let (Some((et, eu)), Some((bt, bu))) = (persona_empty_document(scope), state.baseline) {
        let used = eu + ((v[1] - bu).max(0.0) * scale).floor();
        v[0] = (et + ((v[0] - bt).max(0.0) * scale).floor()).max(used + (et - eu).max(0.0));
        v[1] = used;
    }
    if !precise {
        for n in v.iter_mut() {
            *n = quantize_heap_size(*n);
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let all = st.borrow_mut::<HeapProcesses>();
    let slot = match &process {
        None => &mut all.page,
        Some(site) => all.sites.entry(site.clone()).or_default(),
    };
    slot.cache = Some((now, v));
    Some(v)
}

// ── PerformanceTiming / PerformanceNavigation rows ───────────────────────────────────────────────

/// The receiver's realm entered, then `f`: a `PerformanceTiming` answers for the document of the window that
/// made it, whichever realm's getter is called on it (Blink reads its own `DomWindow()`).
fn in_timing_realm<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, f: impl FnOnce(&mut v8::PinScope) -> R) -> R {
    match this.get_creation_context(scope) {
        Some(ctx) if ctx != scope.get_current_context() => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
            f(scope)
        }
        _ => f(scope),
    }
}

fn cb_pt_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceTiming>(scope, args.this().into()).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let fields = in_timing_realm(scope, args.this(), timing_fields);
    match fields.iter().find(|(k, _)| *k == name) {
        Some((_, n)) => rv.set_double(*n),
        None => rv.set_undefined(),
    }
}

/// `performance.timing.toJSON()` -- a plain object of the twenty-one fields, each read now, in Chrome 150's
/// order ([`TIMING_TO_JSON`]). `timing.toJSON().navigationStart === timing.navigationStart` is in the
/// bundle's oracle.
fn cb_pt_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceTiming>(scope, args.this().into()).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let fields = in_timing_realm(scope, args.this(), timing_fields);
    let o = v8::Object::new(scope);
    for k in TIMING_TO_JSON {
        let v = fields.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap_or(0.0);
        let n: v8::Local<v8::Value> = v8::Number::new(scope, v).into();
        set_prop(scope, o, k, n);
    }
    rv.set(o.into());
}

fn cb_pn_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceNavigation>(scope, args.this().into()).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    // `redirectCount` is 0: this engine follows no redirect it then reports.
    rv.set_double(0.0);
}

/// `performance.navigation.type`: `TYPE_RELOAD` (1) for a reloaded document, `TYPE_BACK_FORWARD` (2) for one a
/// session-history traversal reached, else `TYPE_NAVIGATE`.
fn pn_type(scope: &mut v8::PinScope) -> f64 {
    let a = crate::page::arrival(scope);
    if a.reload {
        1.0
    } else if matches!(a.history, crate::core::html::session_history::HistoryCommit::Traverse(_)) {
        2.0
    } else {
        0.0
    }
}

fn cb_pn_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceNavigation>(scope, args.this().into()).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let t = pn_type(scope);
    rv.set_double(t);
}

fn cb_pn_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if deno_core::cppgc::try_unwrap_cppgc_object::<PerformanceNavigation>(scope, args.this().into()).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let o = v8::Object::new(scope);
    let t = pn_type(scope);
    let t: v8::Local<v8::Value> = v8::Number::new(scope, t).into();
    let z: v8::Local<v8::Value> = v8::Number::new(scope, 0.0).into();
    set_prop(scope, o, "type", t);
    set_prop(scope, o, "redirectCount", z);
    rv.set(o.into());
}

// ── EventCounts rows ─────────────────────────────────────────────────────────────────────────────

fn this_counts(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<Vec<(&'static str, f64)>> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<EventCounts>(scope, args.this().into()) {
        Some(c) => Some(unsafe { c.as_ref() }.counts.borrow().clone()),
        None => {
            crate::binding::protos::illegal_invocation(scope);
            None
        }
    }
}

fn cb_ec_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(c) = this_counts(scope, &args) else { return };
    rv.set_double(c.len() as f64);
}

/// `get(key)` — the count, or `undefined` for a type this build does not track. Measured: Chrome
/// answers `undefined`, where the JS answered `0` and so claimed every event type existed.
fn cb_ec_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(c) = this_counts(scope, &args) else { return };
    let Some(k) = idl_string(scope, args.get(0), "EventCounts", "get") else { return };
    match c.iter().find(|(n, _)| *n == k) {
        Some((_, v)) => rv.set_double(*v),
        None => rv.set_undefined(),
    }
}

fn cb_ec_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(c) = this_counts(scope, &args) else { return };
    let Some(k) = idl_string(scope, args.get(0), "EventCounts", "has") else { return };
    rv.set_bool(c.iter().any(|(n, _)| *n == k));
}

/// `keys()` / `values()` / `entries()` — a fresh iterator over the map, by the shape bound as `data`.
fn cb_ec_iter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(c) = this_counts(scope, &args) else { return };
    let shape = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let arr = v8::Array::new(scope, c.len() as i32);
    for (i, (k, v)) in c.iter().enumerate() {
        let item: v8::Local<v8::Value> = match shape.as_str() {
            "keys" => str_val(scope, k),
            "values" => v8::Number::new(scope, *v).into(),
            _ => {
                let pair = v8::Array::new(scope, 2);
                let ks = str_val(scope, k);
                let vn: v8::Local<v8::Value> = v8::Number::new(scope, *v).into();
                pair.set_index(scope, 0, ks);
                pair.set_index(scope, 1, vn);
                pair.into()
            }
        };
        arr.set_index(scope, i as u32, item);
    }
    rv.set(crate::core::dom::webidl_iter::array_as_iter(scope, "EventCounts", crate::core::dom::webidl_iter::IterKind::Map, arr));
}

/// `forEach(callback, thisArg)` — `callback(value, key, map)`, the maplike order (measured:
/// `[0, "pointerdown", true]`).
fn cb_ec_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(c) = this_counts(scope, &args) else { return };
    let entries: Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)> =
        c.into_iter().map(|(k, v)| (v8::Number::new(scope, v).into(), str_val(scope, k))).collect();
    crate::core::dom::webidl_iter::for_each(scope, &args, "EventCounts", |_, i| entries.get(i).copied());
}

// ── the navigation entry ─────────────────────────────────────────────────────────────────────────

/// The one `PerformanceNavigationTiming` of this document, built on first read.
///
/// WHY ON FIRST READ AND NOT AT MINT TIME: `PerformanceNavigationTiming` is still a registry
/// interface (its own table is not in this unit's scope), so at the moment the timeline is built the
/// interface object may not exist yet and the entry would be a plain object with no prototype — which
/// is exactly what happened to the JS version, and `getEntriesByType("navigation")` then filtered it
/// straight back out.
///
/// The values are `DOMHighResTimeStamp`s relative to `timeOrigin` (unlike `performance.timing`, whose
/// L1 fields are epoch ms), and the phases this engine does not run — redirect, worker,
/// secureConnection — are 0, which is what a browser reports for a navigation that had none. The
/// fields live in the private slots the interfaces' slot getters read ([`define_own`]): the entry has
/// no own property, as in Chrome.
fn ensure_nav(scope: &mut v8::PinScope, p: &deno_core::cppgc::UnsafePtr<Performance>) {
    if unsafe { p.as_ref() }.nav_entry.borrow().is_some() {
        return;
    }
    // A realm with no `PerformanceNavigationTiming` (a Worker: the interface is `[Exposed=Window]`,
    // and `worker.surface.json` does not list it) has no navigation to report.
    let Some(proto) = crate::binding::protos::iface_object(scope, "PerformanceNavigationTiming")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    let e = crate::binding::protos::mint_on(scope, proto.into());
    let dur = now_ms(scope).max(0.0);
    let href = global_get(scope, "location")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|l| get_prop(scope, l, "href"))
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();

    def_str(scope, e, "name", &href);
    def_str(scope, e, "entryType", "navigation");
    def_num(scope, e, "startTime", 0.0);
    def_num(scope, e, "duration", dur);
    let id = realm_navigation_id(scope);
    def_num(scope, e, "navigationId", id);
    // The PerformanceResourceTiming half: the navigation response's own phases when the transport measured
    // them, zeros for a document no navigation fetched.
    def_str(scope, e, "initiatorType", "navigation");
    // a frame's own document's response, or the page's -- read through the phases `performance.timing` reads
    let nav = realm_nav_timing(scope);
    let phases = nav.as_ref().map(|t| phases_of(scope, t));
    def_str(scope, e, "nextHopProtocol", nav.as_ref().map(|t| t.protocol.as_str()).unwrap_or(""));
    def_str(scope, e, "deliveryType", "");
    for k in ["workerStart", "firstInterimResponseStart", "workerRouterEvaluationStart", "workerCacheLookupStart"] {
        def_num(scope, e, k, 0.0);
    }
    let redirect_count = phases.as_ref().map(|n| n.redirect_count).unwrap_or(0);
    match (&nav, &phases) {
        (Some(t), Some(n)) => {
            let (rs, re) = n.redirect.unwrap_or((0.0, 0.0));
            def_num(scope, e, "redirectStart", rs);
            def_num(scope, e, "redirectEnd", re);
            def_num(scope, e, "fetchStart", n.fetch_start);
            // the connection the navigation opened, phase by phase as the transport measured it; `fetchStart` for
            // one it reused (`conn_phases`)
            def_num(scope, e, "domainLookupStart", n.conn.domain_lookup_start);
            def_num(scope, e, "domainLookupEnd", n.conn.domain_lookup_end);
            def_num(scope, e, "connectStart", n.conn.connect_start);
            def_num(scope, e, "connectEnd", n.conn.connect_end);
            def_num(scope, e, "secureConnectionStart", n.conn.secure_connection_start);
            def_num(scope, e, "requestStart", n.request_start);
            def_num(scope, e, "responseStart", n.response_start);
            def_num(scope, e, "finalResponseHeadersStart", n.response_start);
            let encoded = t.encoded_size as f64;
            // a document the HTTP cache served moved nothing; one a `304` confirmed, its headers (Blink's
            // `GetTransferSize`, see `resource_timing`)
            let transfer = match t.cache.as_str() {
                "local" => 0.0,
                "validated" => 300.0,
                _ => encoded + 300.0,
            };
            def_num(scope, e, "transferSize", transfer);
            def_num(scope, e, "encodedBodySize", encoded);
            def_num(scope, e, "decodedBodySize", t.body_size as f64);
        }
        _ => {
            for k in ["redirectStart", "redirectEnd", "fetchStart", "domainLookupStart", "domainLookupEnd", "connectStart",
                      "connectEnd", "secureConnectionStart", "requestStart", "responseStart", "transferSize",
                      "encodedBodySize", "decodedBodySize", "finalResponseHeadersStart"] {
                def_num(scope, e, k, 0.0);
            }
        }
    }
    let response_end = phases.as_ref().map(|n| n.response_end).unwrap_or(dur);
    def_num(scope, e, "responseEnd", response_end);
    {
        // the document's own response's metrics (a navigation's are always the page's to see)
        let metrics = nav.as_ref().map(|t| crate::core::timing::server_timing::parse(&t.server_timing)).unwrap_or_default();
        let arr = crate::core::timing::server_timing::mint(scope, &metrics);
        define_own(scope, e, "serverTiming", arr.into());
    }
    def_str(scope, e, "renderBlockingStatus", "non-blocking");
    def_num(scope, e, "responseStatus", nav.as_ref().map(|t| t.status as f64).unwrap_or(200.0));
    // Blink's minimized type, as every resource entry's (Chrome 150: `text/html` stays, `application/xhtml+xml` reads
    // "application/xml")
    let content_type = nav.as_ref().map(|t| crate::core::timing::resource_timing::minimized_mime_type(scope, &t.content_type)).unwrap_or_default();
    def_str(scope, e, "contentType", &content_type);
    def_str(scope, e, "contentEncoding", "");
    // `""` with no service worker in the way (Chrome 150, measured), not null
    for k in ["workerMatchedSourceType", "workerFinalSourceType"] {
        def_str(scope, e, k, "");
    }
    // The PerformanceNavigationTiming half.
    def_num(scope, e, "unloadEventStart", 0.0);
    def_num(scope, e, "unloadEventEnd", 0.0);
    for k in MILESTONES {
        let v = milestone(scope, k);
        def_num(scope, e, k, v);
    }
    // `duration` is `loadEventEnd - startTime`: 0 until the load event has ended.
    let load_end = milestone(scope, "loadEventEnd");
    def_num(scope, e, "duration", load_end);
    // a frame's entry is its own document's to update; the page's stays the page's
    if !crate::core::html::browsing_context::keep_realm_frame_nav_entry(scope, e) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DocMilestones>().is_none() {
            st.put(DocMilestones::default());
        }
        st.borrow_mut::<DocMilestones>().entry = Some(v8::Global::new(scope, e));
    }
    let a = crate::page::arrival(scope);
    let ty = if a.reload {
        "reload"
    } else if matches!(a.history, crate::core::html::session_history::HistoryCommit::Traverse(_)) {
        "back_forward"
    } else {
        "navigate"
    };
    def_str(scope, e, "type", ty);
    def_num(scope, e, "redirectCount", redirect_count as f64);
    def_num(scope, e, "criticalCHRestart", phases.as_ref().map(|n| n.critical_ch_restart).unwrap_or(0.0));
    def_num(scope, e, "activationStart", 0.0);
    def_null(scope, e, "notRestoredReasons");

    *unsafe { p.as_ref() }.nav_entry.borrow_mut() = Some(v8::Global::new(scope, e));
    unsafe { p.as_ref() }.entries.borrow_mut().insert(0, v8::Global::new(scope, e));

    // The document's INITIAL visibility, a `VisibilityStateEntry` at 0 right behind the navigation
    // entry (Page Visibility "the initial visibility state" -> "report the visibility state"). Chrome
    // 150, measured on a page and in its frame: `getEntries()` answers [navigation, visibility-state
    // "visible"] before anything has loaded; korvus had no such entry (a bot-challenge frame
    // reads every entry's `entryType`). A realm with no `VisibilityStateEntry` (a worker) has none.
    if crate::install::surface::publishes_interface(scope, "VisibilityStateEntry") {
        let state = if crate::core::dom::document::page_hidden(scope) { "hidden" } else { "visible" };
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let v = mint_entry(scope, "VisibilityStateEntry", state, "visibility-state", 0.0, 0.0, undef);
        unsafe { p.as_ref() }.entries.borrow_mut().insert(1, v8::Global::new(scope, v));
    }
}

/// [`define_own`] for the three value shapes the navigation entry's fields take.
fn def_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: &str) {
    let s = str_val(scope, v);
    define_own(scope, o, key, s);
}
fn def_num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: f64) {
    let n: v8::Local<v8::Value> = v8::Number::new(scope, v).into();
    define_own(scope, o, key, n);
}
fn def_null(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) {
    let n: v8::Local<v8::Value> = v8::null(scope).into();
    define_own(scope, o, key, n);
}

/// One navigation-entry field, in the private slot its prototype getter reads (the registry's
/// `PerformanceNavigationTiming` / `PerformanceResourceTiming` / `PerformanceEntry` rows are slot getters):
/// the entry has NO own property, as Chrome's has none (`Object.getOwnPropertyNames(entry)` is `[]`,
/// measured on Chrome 150). The fields used to be own data properties, so a page reading
/// `entry.entryType` read a property where Chrome calls a getter (korvus-analysis U-3, an interactive-widget challenge
/// challenge frame).
fn define_own(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: v8::Local<v8::Value>) {
    crate::core::events::event::evt_slot_write(scope, o, key, v);
}

/// `PerformanceNavigationTiming.prototype.toJSON` -- every attribute, read through the receiver, in
/// Chrome 150's order (measured: the resource entry's keys, then the navigation half).
fn cb_nav_entry_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const NAV_TO_JSON: &[&str] = &[
        "unloadEventStart", "unloadEventEnd", "domInteractive", "domContentLoadedEventStart",
        "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd", "type", "redirectCount",
        "activationStart", "criticalCHRestart", "notRestoredReasons", "confidence",
    ];
    let this = args.this();
    let o = v8::Object::new(scope);
    let nav = navigation_id_exposed(scope);
    for k in crate::core::timing::resource_timing::TO_JSON.iter().chain(NAV_TO_JSON) {
        if *k == "navigationId" && !nav {
            continue;
        }
        if *k == "serverTiming" {
            let l = crate::core::timing::server_timing::json_list(scope, this);
            set_prop(scope, o, k, l.into());
            continue;
        }
        if let Some(v) = get_prop(scope, this, k) {
            set_prop(scope, o, k, v);
        }
    }
    rv.set(o.into());
}

/// The body the registry's `PerformanceNavigationTiming.toJSON` row names (`tools/sync/gen_shells.py`).
pub fn install_nav_timing(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "toJSON" => crate::install::live::method_row(scope, proto, member, cb_nav_entry_to_json),
        "confidence" => crate::install::live::getter_row(scope, proto, member, cb_nav_confidence),
        other => debug_assert!(false, "install_nav_timing: unexpected member {other}"),
    }
}

/// `navigationEntry.confidence`: a NEW `PerformanceTimingConfidence` every read (Chrome 150, measured:
/// `n.confidence === n.confidence` is false) carrying the navigation's finalized confidence, or `null`
/// -- in a frame (only the primary main frame's document gets one), and before the document is
/// interactive ([`navigation_confidence`]).
fn cb_nav_confidence(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if crate::core::html::browsing_context::realm_frame_nav_timing(scope).is_some() {
        rv.set_null();
        return;
    }
    let c = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<DocMilestones>().and_then(|m| m.confidence)
    };
    let Some((rate, value)) = c else {
        rv.set_null();
        return;
    };
    use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
    let o = crate::install::live::mint_table(scope, "PerformanceTimingConfidence");
    evt_slot_write_num(scope, o, "randomizedTriggerRate", rate);
    let v = str_val(scope, value);
    evt_slot_write(scope, o, "value", v);
    rv.set(o.into());
}

/// The body the registry's `PerformanceTimingConfidence.toJSON` row names (`tools/sync/gen_shells.py`).
pub fn install_confidence(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "toJSON" => crate::install::live::method_row(scope, proto, member, cb_confidence_to_json),
        other => debug_assert!(false, "install_confidence: unexpected member {other}"),
    }
}

/// `{randomizedTriggerRate, value}`, read through the receiver, in Chrome's order.
fn cb_confidence_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let o = v8::Object::new(scope);
    for k in ["randomizedTriggerRate", "value"] {
        if let Some(v) = get_prop(scope, this, k) {
            set_prop(scope, o, k, v);
        }
    }
    rv.set(o.into());
}

// ── minting a realm's timeline ───────────────────────────────────────────────────────────────────

/// Build this realm's `performance`: a cppgc `Performance` wearing the table's prototype, with the
/// window's three `[SameObject]` partials when `window_realm`.
///
/// Each realm gets its OWN instance — `worker.performance !== page.performance` is spec, and the
/// `performance-worker` fixture asserts it. One implementation, many instances.
pub fn mint_performance<'s>(scope: &mut v8::PinScope<'s, '_>, window_realm: bool) -> v8::Local<'s, v8::Object> {
    let perf = crate::binding::class::mint(
        scope,
        Performance {
            entries: RefCell::new(Vec::new()),
            observers: RefCell::new(Vec::new()),
            timing: RefCell::new(None),
            navigation: RefCell::new(None),
            event_counts: RefCell::new(None),
            nav_entry: RefCell::new(None),
            resource_buffer_size: Cell::new(250),
            resource_secondary: RefCell::new(Vec::new()),
            bufferfull_pending: Cell::new(false),
            navigation_id: Cell::new(if window_realm { initial_navigation_id() } else { 0.0 }),
        },
    );
    set_table_proto(scope, perf, "Performance");
    // The page's (or the worker's), never a realm built from templates: a frame's or an isolated world's `performance`
    // is its own and must not take the page's resource timeline over. "The first realm's" was the rule
    // while the page's install always minted first; a page realm restored from the snapshot mints on its first read,
    // and a frame that read its own `performance` before then took the page's timeline.
    let mut owns = false;
    if !crate::install::templates::is_lazy_realm(scope) {
        let g = v8::Global::new(scope, perf);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<RealmTimeline>() {
            owns = true;
            st.put(RealmTimeline(g));
        }
    }
    if window_realm {
        let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<Performance>(scope, perf.into()) else { return perf };
        let timing = crate::binding::class::mint(scope, PerformanceTiming);
        set_table_proto(scope, timing, "PerformanceTiming");
        *unsafe { p.as_ref() }.timing.borrow_mut() = Some(v8::Global::new(scope, v8::Local::<v8::Value>::from(timing)));

        let nav = crate::binding::class::mint(scope, PerformanceNavigation);
        set_table_proto(scope, nav, "PerformanceNavigation");
        *unsafe { p.as_ref() }.navigation.borrow_mut() = Some(v8::Global::new(scope, v8::Local::<v8::Value>::from(nav)));

        let counts = crate::binding::class::mint(
            scope,
            EventCounts { counts: RefCell::new(EVENT_COUNT_TYPES.iter().map(|t| (*t, 0.0)).collect()) },
        );
        set_table_proto(scope, counts, "EventCounts");
        *unsafe { p.as_ref() }.event_counts.borrow_mut() = Some(v8::Global::new(scope, v8::Local::<v8::Value>::from(counts)));
    }
    // the page's timeline is made now: what the run recorded before it goes on it
    if owns {
        drain_pending(scope, perf);
    }
    perf
}

// ── the row mounts ───────────────────────────────────────────────────────────────────────────────

/// [`super::super::super::install::live::getter_row`] with a value bound into the getter's `data`.
fn getter_row_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<'s, v8::Value>,
) {
    let g = build_fn_data(scope, cb, &format!("get {}", member.name), 0, data);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

/// [`crate::install::live::method_row`] with a value bound into the method's `data`, arity guard and
/// all.
fn method_row_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<'s, v8::Value>,
) {
    use crate::binding::protos::make_arity_guard;
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn_data(scope, cb, member.name, member.length as i32, data);
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, target, member, v, true);
}

/// The body every implemented row of [`PERFORMANCE`] names.
pub fn install_performance(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "timeOrigin" => getter_row(scope, proto, member, cb_perf_time_origin),
        "interactionCount" => getter_row(scope, proto, member, cb_perf_interaction_count),
        "now" => method_row(scope, proto, member, cb_perf_now),
        "mark" => method_row(scope, proto, member, cb_perf_mark),
        "measure" => method_row(scope, proto, member, cb_perf_measure),
        "getEntries" => method_row(scope, proto, member, cb_perf_get_entries),
        "getEntriesByType" => method_row(scope, proto, member, cb_perf_get_entries_by_type),
        "getEntriesByName" => method_row(scope, proto, member, cb_perf_get_entries_by_name),
        "setResourceTimingBufferSize" => method_row(scope, proto, member, cb_perf_set_buffer_size),
        "toJSON" => method_row(scope, proto, member, cb_perf_to_json),
        "clearMarks" | "clearMeasures" | "clearResourceTimings" => {
            let ty = match member.name {
                "clearMarks" => "mark",
                "clearMeasures" => "measure",
                _ => "resource",
            };
            let d = str_val(scope, ty);
            method_row_data(scope, proto, member, cb_perf_clear, d);
        }
        "timing" | "navigation" | "eventCounts" => {
            let d = str_val(scope, member.name);
            getter_row_data(scope, proto, member, cb_perf_same_object, d);
        }
        // `memory` is left OFF ENTIRELY when the profile captured no heap numbers: absent is honest,
        // invented is not. That was `op_perf_memory`'s own rule, kept after that op was removed.
        "memory" => {
            if persona_heap_limit(scope).is_none() {
                return;
            }
            // the getter builds its realm's MemoryInfo prototype on first use ([`realm_memory_proto`]); it
            // used to be built here and handed over as the getter's `data`, which a V8 template cannot
            // hold
            crate::install::live::getter_row(scope, proto, member, cb_perf_memory);
        }
        other => debug_assert!(false, "install_performance: unexpected member {other}"),
    }
}

/// The body every row of [`PERFORMANCETIMING`] names.
pub fn install_performance_timing(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_pt_to_json),
        name => {
            let d = str_val(scope, name);
            getter_row_data(scope, proto, member, cb_pt_field, d);
        }
    }
}

/// The body every non-constant row of [`PERFORMANCENAVIGATION`] names.
pub fn install_performance_navigation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_pn_to_json),
        "type" => getter_row(scope, proto, member, cb_pn_type),
        "redirectCount" => getter_row(scope, proto, member, cb_pn_field),
        other => debug_assert!(false, "install_performance_navigation: unexpected member {other}"),
    }
}

/// The body every row of [`EVENTCOUNTS`] names.
pub fn install_event_counts(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => getter_row(scope, proto, member, cb_ec_size),
        "get" => method_row(scope, proto, member, cb_ec_get),
        "has" => method_row(scope, proto, member, cb_ec_has),
        "forEach" => method_row(scope, proto, member, cb_ec_for_each),
        "keys" | "values" | "entries" => {
            let d = str_val(scope, member.name);
            method_row_data(scope, proto, member, cb_ec_iter, d);
        }
        // A maplike's `@@iterator` IS its `entries` -- the SAME function object, non-enumerable,
        // writable, configurable (Chrome 150, measured:
        // `EventCounts.prototype[Symbol.iterator] === EventCounts.prototype.entries`). The row is
        // last so the `entries` row above has already been mounted when it reads it back.
        "@@iterator" => {
            let k: v8::Local<v8::Value> = member_key(scope, "entries").into();
            if let Some(f) = proto.get(scope, k) {
                crate::install::kits::define_value(scope, proto, member, f, true);
            }
        }
        other => debug_assert!(false, "install_event_counts: unexpected member {other}"),
    }
}

// ── the tables ───────────────────────────────────────────────────────────────────────────────────

// Rows VERBATIM from the `Performance` block of `install/generated_registry.rs`, in the capture's
// order, plus the ONE `@@iterator` row `EventCounts` needs below (a symbol key the surface dump
// records as `iterable: true` but cannot list among the string members).
//
// `[interface, both]`: the capture records `newable: false` for the interface in BOTH realms, so the
// installer's "Illegal constructor" interface object is the right one -- only the installer mints a
// timeline. `parent "EventTarget"`: `Object.getPrototypeOf(Performance) === EventTarget`, measured.
//
// The window's eighteen; the worker capture lists the first THIRTEEN -- `timing`, `navigation`,
// `memory`, `eventCounts` and `interactionCount` are `[Exposed=Window]` partials and say so on their
// own rows, and the `performance-worker` fixture asserts the first three are absent from a worker's
// timeline.
webapi::interface! {
    "Performance" : "EventTarget" [interface, both] {
        ctor        "Performance"                  len 0 = stub,
        attr        "timeOrigin"                 : Double readonly            = install_performance,
        handler     "onresourcetimingbufferfull"           nullable           = event_handler,
        method      "clearMarks"                 : Undefined len 0            = install_performance,
        method      "clearMeasures"              : Undefined len 0            = install_performance,
        method      "clearResourceTimings"       : Undefined len 0            = install_performance,
        method      "getEntries"                 : Sequence len 0             = install_performance,
        method      "getEntriesByName"           : Sequence len 1 required 1  = install_performance,
        method      "getEntriesByType"           : Sequence len 1 required 1  = install_performance,
        method      "mark"                       : Object len 1 required 1    = install_performance,
        method      "measure"                    : Object len 1 required 1    = install_performance,
        method      "setResourceTimingBufferSize": Undefined len 1 required 1 = install_performance,
        method      "toJSON"                     : Object len 0               = install_performance,
        method      "now"                        : Double len 0               = install_performance,
        // THE FIVE `[Exposed=Window]` PARTIALS, marked as such on the ROW. Both captures
        // agree, in both realms: `surface.json` lists eighteen `Performance` members and
        // `worker.surface.json` thirteen, and the five missing ones are exactly these. The fact lived
        // in `WORKER_PERFORMANCE_MEMBERS`, a hand-written copy of the worker capture's thirteen names
        // handed to the installer as a selector; the selector is the ACTIVE capture's now, so the fact
        // belongs where WebIDL puts it -- on the row. It also fixes the realm this file could not
        // reach that way: a worker with NO persona (`worker.rs`'s own unit tests) has no capture to
        // select from, and before this word it grew an `eventCounts` no real worker has.
        attr        "timing"                     : Object window readonly     = install_performance,
        attr        "navigation"                 : Object window readonly     = install_performance,
        attr        "memory"                     : Object window readonly     = install_performance,
        attr        "eventCounts"                : Object window readonly     = install_performance,
        attr        "interactionCount"           : Integer window readonly    = install_performance
    }
}

// `WORKER_PERFORMANCE_MEMBERS` WAS HERE: the worker capture's thirteen names, written
// down and handed to the installer as this realm's selector. Both facts it carried are now where
// they belong -- WHICH members and IN WHAT ORDER comes from the ACTIVE capture
// (`install::installer::join`), and WHY the five are absent is the `window` word on their rows
// above, which is WebIDL's own answer and holds even in a realm with no capture to read.

// Rows VERBATIM from the `PerformanceTiming` block of `install/generated_registry.rs`, in the
// capture's order. `[interface, window]`: Navigation Timing L1 is `[Exposed=Window]` and the worker
// capture does not list it; the capture records `newable: false` and the bundle's oracle records
// `new PerformanceTiming()` as a TypeError.
webapi::interface! {
    "PerformanceTiming" [interface, window] {
        ctor   "PerformanceTiming"                        len 0 = stub,
        attr   "navigationStart"            : Integer readonly  = install_performance_timing,
        attr   "unloadEventStart"           : Integer readonly  = install_performance_timing,
        attr   "unloadEventEnd"             : Integer readonly  = install_performance_timing,
        attr   "redirectStart"              : Integer readonly  = install_performance_timing,
        attr   "redirectEnd"                : Integer readonly  = install_performance_timing,
        attr   "fetchStart"                 : Integer readonly  = install_performance_timing,
        attr   "domainLookupStart"          : Integer readonly  = install_performance_timing,
        attr   "domainLookupEnd"            : Integer readonly  = install_performance_timing,
        attr   "connectStart"               : Integer readonly  = install_performance_timing,
        attr   "connectEnd"                 : Integer readonly  = install_performance_timing,
        attr   "secureConnectionStart"      : Integer readonly  = install_performance_timing,
        attr   "requestStart"               : Integer readonly  = install_performance_timing,
        attr   "responseStart"              : Integer readonly  = install_performance_timing,
        attr   "responseEnd"                : Integer readonly  = install_performance_timing,
        attr   "domLoading"                 : Integer readonly  = install_performance_timing,
        attr   "domInteractive"             : Integer readonly  = install_performance_timing,
        attr   "domContentLoadedEventStart" : Integer readonly  = install_performance_timing,
        attr   "domContentLoadedEventEnd"   : Integer readonly  = install_performance_timing,
        attr   "domComplete"                : Integer readonly  = install_performance_timing,
        attr   "loadEventStart"             : Integer readonly  = install_performance_timing,
        attr   "loadEventEnd"               : Integer readonly  = install_performance_timing,
        method "toJSON"                     : Object len 0      = install_performance_timing
    }
}

// Rows VERBATIM from the `PerformanceNavigation` block of `install/generated_registry.rs`, in the
// capture's order: the two attributes, the four constants on the PROTOTYPE, `toJSON`, then the same
// four as STATICS on the interface object (the registry carries every WebIDL constant twice, one row
// per holder, and the installer mounts each on its own).
webapi::interface! {
    "PerformanceNavigation" [interface, window] {
        ctor            "PerformanceNavigation"       len 0 = stub,
        attr            "type"              : Integer readonly = install_performance_navigation,
        attr            "redirectCount"     : Integer readonly = install_performance_navigation,
        constant        "TYPE_NAVIGATE"     : Integer          = 0,
        constant        "TYPE_RELOAD"       : Integer          = 1,
        constant        "TYPE_BACK_FORWARD" : Integer          = 2,
        constant        "TYPE_RESERVED"     : Integer          = 255,
        method          "toJSON"            : Object len 0     = install_performance_navigation,
        static_constant "TYPE_NAVIGATE"     : Integer          = 0,
        static_constant "TYPE_RELOAD"       : Integer          = 1,
        static_constant "TYPE_BACK_FORWARD" : Integer          = 2,
        static_constant "TYPE_RESERVED"     : Integer          = 255
    }
}

// Rows VERBATIM from the `EventCounts` block of `install/generated_registry.rs`, in the capture's
// order, plus the `@@iterator` row. `[interface, window]`: Event Timing is `[Exposed=Window]`.
webapi::interface! {
    "EventCounts" [interface, window] {
        ctor   "EventCounts"              len 0 = stub,
        attr   "size"       : Integer readonly  = install_event_counts,
        method "entries"    : Object len 0      = install_event_counts,
        method "forEach"    : Undefined len 1   = install_event_counts,
        method "get"        : Any len 1         = install_event_counts,
        method "has"        : Boolean len 1     = install_event_counts,
        method "keys"       : Object len 0      = install_event_counts,
        method "values"     : Object len 0      = install_event_counts,
        method "@@iterator" : Object len 0 hidden = install_event_counts
    }
}

// Rows VERBATIM from the `PerformanceMark` / `PerformanceMeasure` blocks of
// `install/generated_registry.rs`. `PerformanceMark` is the one CONSTRUCTIBLE entry type (the
// capture records `length: 1`, and `new PerformanceMark("x")` is in the bundle's oracle);
// `PerformanceMeasure` is not (`new PerformanceMeasure()` is a recorded TypeError). Both are
// `slot_backed`: `detail` reads the private slot [`mint_entry`] writes, the same slot the
// `PerformanceEntry` rows read, so an entry has no own property.
webapi::interface! {
    "PerformanceMark" : "PerformanceEntry" [value, both] {
        ctor "PerformanceMark"   len 1 = install_performance_mark_ctor,
        attr "detail" : Any readonly   = slot_get
    }
}
webapi::interface! {
    "PerformanceMeasure" : "PerformanceEntry" [interface, both] {
        ctor "PerformanceMeasure" len 0 = stub,
        attr "detail" : Any readonly    = slot_get
    }
}

/// `CtorDef::install` is not read by the installer (a table row cannot yet name a constructor body),
/// so `install::live::performance` passes [`cb_performance_mark_ctor`] to `build_newable` directly;
/// this exists so the row can NAME its body rather than claim to be a stub.
pub fn install_performance_mark_ctor(_scope: &mut v8::PinScope, _proto: v8::Local<v8::Object>, _member: &'static MemberDef) {}

// ── PerformanceEntry ─────────────────────────────────────────────────────────────────────────────

/// `entry.toJSON()` — `{name, entryType, startTime, duration}`, the four the base interface declares, and
/// `navigationId` where the realm has it ([`navigation_id_exposed`]; Chrome 152 measured) -- a mark's `detail` is
/// NOT in it. A Chrome 152 soft navigation inherits this `toJSON` and its `BuildJSONValue` adds the paint pair
/// (when painted), `navigationType` and `interactionId`.
pub(crate) fn cb_perf_entry_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let o = v8::Object::new(scope);
    let nav: &[&str] = if navigation_id_exposed(scope) { &["navigationId"] } else { &[] };
    for k in ["name", "entryType", "startTime", "duration"].iter().chain(nav) {
        if let Some(v) = get_prop(scope, this, k) {
            set_prop(scope, o, k, v);
        }
    }
    let soft = crate::core::events::event::evt_slot_read(scope, this, "entryType").is_some_and(|v| v.to_rust_string_lossy(scope) == "soft-navigation");
    if soft {
        let painted = crate::core::events::event::evt_slot_read(scope, this, "presentationTime").is_some_and(|v| !v.is_null_or_undefined());
        let paint: &[&str] = if painted { &["paintTime", "presentationTime"] } else { &[] };
        for k in paint.iter().chain(["navigationType", "interactionId"].iter()) {
            let v = crate::core::events::event::evt_slot_read(scope, this, k).unwrap_or_else(|| v8::undefined(scope).into());
            set_prop(scope, o, k, v);
        }
    }
    rv.set(o.into());
}

/// The body the one implemented row of [`PERFORMANCEENTRY`] names.
pub fn install_performance_entry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_perf_entry_to_json),
        other => debug_assert!(false, "install_performance_entry: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `PerformanceEntry` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: four slot-backed readonly attributes,
// `toJSON`, then `navigationId`. `[interface, both]`: the capture records `newable: false` -- the
// interface is abstract, only a timeline mints entries, and `new PerformanceEntry()` is
// `TypeError: Illegal constructor` (the installer's interface object, where it used to be an
// `op_nativize_ctor` mask over a JS class in `host.js`).
//
// The four attributes stay `slot_get`: `mint_entry` writes those private slots, and so do the
// `PerformanceMark` / `PerformanceMeasure` `detail` rows. The table moved here from
// `performance_observer.rs` with the rest of the timeline; the rows are unchanged.
webapi::interface! {
    "PerformanceEntry" [interface, both] {
        ctor   "PerformanceEntry"               len 0 = stub,
        attr   "name"         : Object readonly       = slot_get,
        attr   "entryType"    : Object readonly       = slot_get,
        attr   "startTime"    : Object readonly       = slot_get,
        attr   "duration"     : Object readonly       = slot_get,
        method "toJSON"       : Object len 0          = install_performance_entry,
        attr   "navigationId" : Object readonly       = slot_get
    }
}

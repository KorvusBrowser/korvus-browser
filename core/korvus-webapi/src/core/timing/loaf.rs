//! Long animation frames -- `long-animation-frame` entries, `PerformanceLongAnimationFrameTiming` -- as Blink's
//! `AnimationFrameTimingMonitor` reports them (core/frame/animation_frame_timing_monitor.cc).
//!
//! A frame is the time from the first task that ran while a rendering update was pending to that update's end;
//! one of 50 ms or more is a long animation frame. A task of 50 ms or more with no rendering pending is one on its
//! own. Measured on stock Chrome 150: an 80 ms timer that touches nothing is `{duration: 80, renderStart: 0,
//! blockingDuration: 30}` with no paint times; the same timer appending to the body renders right after it
//! (`renderStart` its end, `blockingDuration` 37 = 80 + the 7 ms render - 50) and carries the frame's
//! `paintTime` / `presentationTime`; three 30 ms timers that append make ONE 60 ms frame of the two that ran
//! before the vsync (`blockingDuration` 0); and a page's first frame -- 11.6 ms to the first render at 216.6 -- is
//! one too. The challenge frame of a live an interactive-widget challenge reads `entryType` of eight of them; this engine had none, and
//! `getEntries()` dropped the type as observer-only where Chrome 150 lists it.
//!
//! The tasks are V8's outermost calls into script (`Isolate::AddBeforeCallEnteredCallback` /
//! `AddCallCompletedCallback`); a task asks for a rendering update when it changed what the document renders
//! (`Dom::render_revision`), left an animation frame pending, or ran before the document's first rendering update. The update
//! comes at the next vsync on the persona's refresh grid, or when the task running then ends.
//!
//! A frame tree renders as Chrome's site isolation splits it (measured on stock Chrome 150 with
//! a cross-site frame whose document answers after 600 ms): each LOCAL ROOT -- the page, and every frame cross-site to
//! its parent -- keeps its own animation frame, and a frame's rendering is its local root's
//! (`browsing_context::realm_local_root`); a long task that rendered nothing is the long animation frame of the realm
//! whose task it was, local root or not. A task's realm is the realm whose callback the engine ran for it
//! ([`begin_task`]), not the realm its outermost call entered: every realm's timers and engine tasks (a
//! message's delivery) run from one pump, in the realm that started it. A cross-site frame's first frame begins with
//! its document's first task after the commit -- its widget is made then -- where the page's begins with its
//! navigation's fetch.
//!
//! What is not here: the frame's `scripts` (`PerformanceScriptTiming` attribution -- an empty list), and
//! `firstUIEventTimestamp` (no input reaches it).
//!
//! The monitor lives in an isolate SLOT, not a thread-local: one thread runs one isolate after another (a test
//! harness's engines, a navigation's successor), and a realm handle kept past its isolate's disposal crashed the
//! next one (`attempt to access Handle hosted by disposed Isolate`, gate-47). It names realms WEAKLY: a pending
//! frame or a finished entry does not keep its realm -- a removed frame's whole DOM and heap -- alive; one whose
//! realm is gone is dropped. And its work runs outside any template capture (`templates::outside_capture`): the
//! calls it watches include every API call an install makes, and its persona read (the refresh grid), counted
//! as the install's, kept each blueprint per persona -- the runtime suite's memory climbed ~50 MB a test to
//! 35 GB (2026-10-09; 2.7 GB without the read).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use deno_core::v8;

const LONG_MS: f64 = 50.0;

/// A rendering update waiting to run, and what the tasks before it cost.
struct PendingFrame {
    start: Instant,
    /// the vsync the update runs at if the thread is free then
    vsync: Instant,
    longest_ms: f64,
    blocking_excl_longest_ms: f64,
    /// the realm's document has had no rendering update yet: its first frame, whose start is the document's own
    first_render: bool,
}

/// A long animation frame, finished, waiting to be put on its realm's timeline.
struct Long {
    realm: v8::Weak<v8::Context>,
    start: Instant,
    end: Instant,
    /// `Some(render start)` for a frame that rendered; `None` for a task alone
    render_start: Option<Instant>,
    blocking_ms: f64,
}

#[derive(Default)]
struct Monitor {
    /// the task running now: its start, its realm, the document's render revision then, and whether its realm is
    /// the one [`begin_task`] named (or still the context its outermost call entered)
    task: Option<(Instant, Option<v8::Weak<v8::Context>>, u64, bool)>,
    frames: Vec<(i32, v8::Weak<v8::Context>, PendingFrame)>,
    /// the realm [`begin_task`] named while no task was open: the realm of the call about to open one (a timer's
    /// callback, called from inside the pump's microtask, is an outermost call of its own)
    next_realm: Option<v8::Weak<v8::Context>>,
    ready: Vec<Long>,
    /// the realms whose documents have had their first rendering update, as [`first_render`] saw them -- before the paint step's own
    /// bookkeeping says so (the step makes calls into script, and a task it opens is not a first frame's)
    rendered: std::collections::HashSet<i32>,
    /// Every finished task's `(start, end)`, newest last, at most [`TASKS_KEPT`] -- Blink's
    /// `IdlenessDetector::DidProcessTask` shifts the network-quiet window by each one ([`task_intervals`])
    tasks: std::collections::VecDeque<(Instant, Instant)>,
    /// How many task brackets the event loop's runners hold open ([`task_start`]): while one is, the
    /// task is the runner's -- its call, its microtask checkpoint and the rest it left -- and a call returning to an
    /// empty stack does not end it.
    explicit: u32,
}

/// How many finished tasks [`Monitor::tasks`] keeps; the oldest goes first. The network-quiet replay reads only the
/// tasks since `DOMContentLoaded`, and a page that runs more than this many in the window it waits on is busy anyway.
const TASKS_KEPT: usize = 4096;

/// The page isolate's finished tasks, `(start, end)`, oldest first -- every outermost script call the
/// monitor saw end, a frame's included, whether or not anything observes long animation frames. A task here is a
/// script call: work that runs no script (parsing, style, layout) is not one, where Blink's task observer sees every
/// task of the thread. Empty for an isolate without the monitor (a worker's).
pub fn task_intervals(isolate: &v8::Isolate) -> Vec<(Instant, Instant)> {
    monitor(isolate).and_then(|m| m.try_borrow().ok().map(|m| m.tasks.iter().copied().collect())).unwrap_or_default()
}

/// The isolate's [`Monitor`], in its slot ([`install_loaf_monitor`]); shared out so a caller can borrow it while
/// it uses the scope.
#[derive(Clone, Default)]
struct MonitorSlot(Rc<RefCell<Monitor>>);

fn monitor(isolate: &v8::Isolate) -> Option<Rc<RefCell<Monitor>>> {
    isolate.get_slot::<MonitorSlot>().map(|s| s.0.clone())
}

unsafe extern "C" {
    // `v8::Isolate::AddBeforeCallEnteredCallback(void (*)(v8::Isolate*))` and `AddCallCompletedCallback`: public V8
    // API the v8 crate does not bind, present in its archive (a member function: `this` first).
    // Mangled per the C++ compiler that built librusty_v8: Itanium (clang) off Windows, MSVC on it.
    #[cfg_attr(not(windows), link_name = "_ZN2v87Isolate28AddBeforeCallEnteredCallbackEPFvPS0_E")]
    #[cfg_attr(windows, link_name = "?AddBeforeCallEnteredCallback@Isolate@v8@@QEAAXP6AXPEAV12@@Z@Z")]
    fn v8_add_before_call_entered(isolate: *mut v8::RealIsolate, cb: unsafe extern "C" fn(v8::UnsafeRawIsolatePtr));
    #[cfg_attr(not(windows), link_name = "_ZN2v87Isolate24AddCallCompletedCallbackEPFvPS0_E")]
    #[cfg_attr(windows, link_name = "?AddCallCompletedCallback@Isolate@v8@@QEAAXP6AXPEAV12@@Z@Z")]
    fn v8_add_call_completed(isolate: *mut v8::RealIsolate, cb: unsafe extern "C" fn(v8::UnsafeRawIsolatePtr));
    // `v8::Isolate::AddMicrotasksCompletedCallback(void (*)(v8::Isolate*, void*), void*)`
    #[cfg_attr(not(windows), link_name = "_ZN2v87Isolate30AddMicrotasksCompletedCallbackEPFvPS0_PvES2_")]
    #[cfg_attr(windows, link_name = "?AddMicrotasksCompletedCallback@Isolate@v8@@QEAAXP6AXPEAV12@PEAX@Z1@Z")]
    fn v8_add_microtasks_completed(isolate: *mut v8::RealIsolate, cb: unsafe extern "C" fn(v8::UnsafeRawIsolatePtr, *mut std::ffi::c_void), data: *mut std::ffi::c_void);
    // rusty_v8's own C glue (binding.cc): the isolate's current context as a handle in the current HandleScope
    fn v8__Isolate__GetCurrentContext(isolate: *mut v8::RealIsolate) -> *const v8::Context;
}

/// Watch the page isolate's tasks (not a worker's: a worker has no frames).
/// The context snapshot (slice 3): a fresh, empty monitor in place of the one the install's JS calls filled.
/// Its `v8::Weak<v8::Context>` handles are global-handle nodes V8 refuses to serialise with
/// (`CheckGlobalAndEternalHandles` walks weak ones too). The hooks keep finding a slot; a process made from the snapshot
/// installs its own monitor at engine start (`install_loaf_monitor`).
#[cfg(not(feature = "analysis"))]
pub(crate) fn reset_for_snapshot(isolate: &mut v8::Isolate) {
    if isolate.get_slot::<MonitorSlot>().is_some() {
        isolate.set_slot(MonitorSlot::default());
    }
}

pub fn install_loaf_monitor(isolate: &mut v8::Isolate) {
    isolate.set_slot(MonitorSlot::default());
    // SAFETY: rusty_v8's `Isolate` wraps exactly the C++ isolate pointer (`repr(transparent)`).
    let raw = unsafe { *(isolate as *mut v8::Isolate as *const *mut v8::RealIsolate) };
    // SAFETY: the callbacks are `extern "C"` and live for the process; V8 calls them on this isolate's thread.
    unsafe {
        v8_add_before_call_entered(raw, cb_entered);
        v8_add_call_completed(raw, cb_completed);
        v8_add_microtasks_completed(raw, cb_microtasks_completed, std::ptr::null_mut());
    }
}

/// A microtask checkpoint ended: a call made from inside one (a native callback running as a promise job --
/// the timer pump, an XHR's events) entered script at a call depth V8 never sees return to zero, so its
/// `CallCompleted` never fires; the task ends with the checkpoint.
unsafe extern "C" fn cb_microtasks_completed(raw: v8::UnsafeRawIsolatePtr, _data: *mut std::ffi::c_void) {
    // SAFETY: the same callback contract as `CallCompleted`'s.
    unsafe { cb_completed(raw) }
}

/// The document's render revision, read without failing inside a V8 callback.
fn render_revision(isolate: &mut v8::Isolate) -> u64 {
    let op_state = deno_core::JsRuntime::op_state_from(isolate);
    let Ok(st) = op_state.try_borrow() else { return 0 };
    st.try_borrow::<crate::binding::HostDom>().map(|d| d.0.render_revision()).unwrap_or(0)
}

/// The next vsync after `t` on the refresh grid `rendering::next_frame_ms` keeps (read now, projected to `t`).
fn vsync_after(scope: &mut v8::PinScope, t: Instant) -> Instant {
    let now = Instant::now();
    let next = now + std::time::Duration::from_secs_f64(crate::core::intersection_observer::rendering::next_frame_ms(scope) / 1000.0);
    if t <= next {
        return next;
    }
    let hz = crate::core::navigator::navigator::persona(scope).map(|p| p.screen().refresh_rate).filter(|r| r.is_finite() && *r > 0.0).unwrap_or(60.0);
    let period = std::time::Duration::from_secs_f64(1.0 / hz);
    let mut v = next;
    while v < t {
        v += period;
    }
    v
}

unsafe extern "C" fn cb_entered(mut raw: v8::UnsafeRawIsolatePtr) {
    if raw.is_null() {
        return;
    }
    let now = Instant::now();
    // SAFETY: `UnsafeRawIsolatePtr` is the C++ isolate pointer (`repr(transparent)`).
    let real: *mut v8::RealIsolate = unsafe { std::mem::transmute_copy(&raw) };
    // SAFETY: V8 calls this on the isolate's own thread with the isolate entered.
    let isolate = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut raw) };
    let Some(mon) = monitor(isolate) else { return };
    if mon.try_borrow().map(|m| m.task.is_some()).unwrap_or(true) {
        return;
    }
    let rev = render_revision(isolate);
    let realm = {
        v8::scope!(let scope, isolate);
        // SAFETY: V8 sets the call's context before firing this callback (`CallDepthScope`); the handle lives in
        // the HandleScope just opened, and `Local` is the handle pointer (`repr(transparent)`).
        let ptr = unsafe { v8__Isolate__GetCurrentContext(real) };
        if ptr.is_null() {
            None
        } else {
            let ctx: v8::Local<v8::Context> = unsafe { std::mem::transmute(ptr) };
            Some(v8::Weak::new(&mut **scope, ctx))
        }
    };
    if let Ok(mut m) = mon.try_borrow_mut() {
        m.task = Some(match m.next_realm.take() {
            Some(named) => (now, Some(named), rev, true),
            None => (now, realm, rev, false),
        });
    };
}

/// A TASK OF ITS OWN BEGINS, the realm `of` belongs to's: the engine is about to run what Chrome runs as one task --
/// a timer's callback or an engine task's (the runtime's `run_timer`; a frame's message delivery is made in the
/// frame's realm), an XMLHttpRequest's network event -- though here it runs inside whatever pump or microtask
/// checkpoint delivered it. With no task open, the call about to be made opens it, as `of`'s realm's. A task open
/// but not yet named (the pump's own call, entered in the realm that started it) is named `of`'s realm's. A task an
/// earlier callback named ends here, and the new one is `of`'s realm's: every realm's timers run from one pump and
/// every finished request's reactions in one microtask checkpoint, so the page's timer and a frame's XMLHttpRequest
/// `load` that came due together were ONE task of the page's -- the frame's 80 ms `load` handler was a long animation
/// frame on the PAGE's timeline (`frame_tasks_live`, whenever a loaded machine let the two meet; Chrome
/// 150 lists it on the frame's).
pub fn begin_task(scope: &mut v8::PinScope, of: v8::Local<v8::Object>) {
    let Some(mon) = monitor(scope) else { return };
    let Some(ctx) = of.get_creation_context(scope) else { return };
    let w = v8::Weak::new(scope, ctx);
    let named = match mon.try_borrow().map(|m| m.task.as_ref().map(|t| t.3)) {
        Ok(n) => n,
        Err(_) => return,
    };
    match named {
        None => {
            if let Ok(mut m) = mon.try_borrow_mut() {
                m.next_realm = Some(w);
            };
        }
        Some(false) => {
            if let Ok(mut m) = mon.try_borrow_mut() {
                if let Some(t) = m.task.as_mut() {
                    t.1 = Some(w);
                    t.3 = true;
                }
            };
        }
        Some(true) => {
            finish_task(scope, &mon);
            let rev = render_revision(scope);
            if let Ok(mut m) = mon.try_borrow_mut() {
                m.task = Some((Instant::now(), Some(w), rev, true));
            };
        }
    }
}

/// End the task open now: [`task_done`] for its realm, at this moment.
fn finish_task(isolate: &mut v8::Isolate, mon: &Rc<RefCell<Monitor>>) {
    let Some((start, realm, rev0, _)) = mon.try_borrow_mut().ok().and_then(|mut m| {
        m.next_realm = None;
        m.task.take()
    }) else {
        return;
    };
    let end = Instant::now();
    // every task, before anything below can return early (no realm, a realm gone): the busy time is the thread's
    if let Ok(mut m) = mon.try_borrow_mut() {
        if m.tasks.len() >= TASKS_KEPT {
            m.tasks.pop_front();
        }
        m.tasks.push_back((start, end));
    }
    let Some(ctx) = realm else { return };
    let rev1 = render_revision(isolate);
    v8::scope!(let scope, isolate);
    let Some(context) = ctx.to_local(scope) else { return };
    let scope = &mut v8::ContextScope::new(scope, context);
    let key = context.global(scope).get_identity_hash().get();
    // the call may be one an install made: what the monitor reads is not the install's
    crate::install::templates::outside_capture(|| task_done(scope, mon, key, ctx, start, end, rev1 != rev0));
}

unsafe extern "C" fn cb_completed(mut raw: v8::UnsafeRawIsolatePtr) {
    if raw.is_null() {
        return;
    }
    // SAFETY: as above.
    let isolate = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut raw) };
    let Some(mon) = monitor(isolate) else { return };
    // a runner's task goes on through its checkpoint ([`task_start`])
    if mon.try_borrow().map(|m| m.explicit > 0).unwrap_or(true) {
        return;
    }
    if mon.try_borrow().map(|m| m.task.is_none()).unwrap_or(true) {
        if let Ok(mut m) = mon.try_borrow_mut() {
            m.next_realm = None;
        };
        return;
    }
    finish_task(isolate, &mon);
}

/// A TASK OF THE EVENT LOOP BEGINS: a runner is about to run one -- a timer's, a fetch's end, an
/// engine task -- from the event loop, with its microtask checkpoint and the rest it leaves inside it, as Chrome's
/// task is. Opened in the realm entered now; the callback it runs names it ([`begin_task`]). Until [`task_end`] a call
/// that returns to an empty stack is part of it, not its end: the checkpoint's script runs after that return.
pub fn task_start(scope: &mut v8::PinScope) {
    // the moment the loop took the task, before any of its bookkeeping (Blink: the sequence manager's task start time)
    let at = Instant::now();
    let Some(mon) = monitor(scope) else { return };
    let open = mon.try_borrow().map(|m| m.task.is_some()).unwrap_or(true);
    let rev = render_revision(scope);
    let ctx = scope.get_current_context();
    let w = v8::Weak::new(scope, ctx);
    if let Ok(mut m) = mon.try_borrow_mut() {
        m.explicit += 1;
        if !open {
            m.task = Some(match m.next_realm.take() {
                Some(named) => (at, Some(named), rev, true),
                None => (at, Some(w), rev, false),
            });
        }
    };
}

/// The runner's task is over ([`task_start`]).
pub fn task_end(scope: &mut v8::PinScope) {
    let Some(mon) = monitor(scope) else { return };
    let last = match mon.try_borrow_mut() {
        Ok(mut m) => {
            m.explicit = m.explicit.saturating_sub(1);
            m.explicit == 0
        }
        Err(_) => return,
    };
    if last {
        finish_task(scope, &mon);
    }
}

/// `OnTaskCompleted`, for one realm: the task joins its local root's pending frame, opens one, or is long on its own
/// -- then its own realm's.
fn task_done(scope: &mut v8::PinScope, mon: &RefCell<Monitor>, task_key: i32, task_ctx: v8::Weak<v8::Context>, start: Instant, end: Instant, changed: bool) {
    let dur = (end - start).as_secs_f64() * 1000.0;
    let pending_raf = crate::core::html::animation_frame::has_pending_frames(scope);
    // the frame is the local root's: its key and realm, and whether its document has rendered
    let root = crate::core::html::browsing_context::realm_local_root(scope);
    let key = root.global(scope).get_identity_hash().get();
    let ctx = if key == task_key { task_ctx.clone() } else { v8::Weak::new(scope, root) };
    let scope = &mut v8::ContextScope::new(scope, root);
    let rendered = mon.try_borrow().map(|m| m.rendered.contains(&key)).unwrap_or(false) || crate::core::css::cssom::realm_has_rendered(scope);
    let wants_frame = changed || !rendered || pending_raf || crate::core::html::animation_frame::has_pending_frames(scope);
    let vsync = vsync_after(scope, end);
    {
        let Ok(mut m) = mon.try_borrow_mut() else { return };
        // a frame of this realm the thread was free for at its vsync rendered then, before this task
        if let Some(i) = m.frames.iter().position(|f| f.0 == key && f.2.vsync <= start && !f.2.first_render) {
            let (_, c, f) = m.frames.remove(i);
            let at = f.vsync;
            finish(&mut m.ready, c, f, at);
        }
        match m.frames.iter().position(|f| f.0 == key) {
            Some(i) => {
                let f = &mut m.frames[i].2;
                apply(f, dur);
                // a first frame waits for the document's first rendering update ([`first_render`]), not for a vsync
                if end >= f.vsync && !f.first_render {
                    // the vsync came while this task ran: the update runs at its end
                    let (_, c, f) = m.frames.remove(i);
                    finish(&mut m.ready, c, f, end);
                }
            }
            None if wants_frame => {
                let mut f = PendingFrame { start, vsync, longest_ms: 0.0, blocking_excl_longest_ms: 0.0, first_render: !rendered };
                apply(&mut f, dur);
                if end >= vsync && rendered {
                    finish(&mut m.ready, ctx, f, end);
                } else {
                    m.frames.push((key, ctx, f));
                }
            }
            None => {
                // a long task that rendered nothing: its own realm's
                if dur >= LONG_MS {
                    m.ready.push(Long { realm: task_ctx, start, end, render_start: None, blocking_ms: dur - LONG_MS });
                }
            }
        }
    }
}

/// `ApplyTaskDuration`: the longest task apart, the others' excess over 50 ms summed.
fn apply(f: &mut PendingFrame, mut dur: f64) {
    if dur > f.longest_ms {
        std::mem::swap(&mut dur, &mut f.longest_ms);
    }
    if dur > LONG_MS {
        f.blocking_excl_longest_ms += dur - LONG_MS;
    }
}

/// `RecordRenderingUpdateEndTime`: the update ran at `render`; a frame of 50 ms or more is long.
fn finish(ready: &mut Vec<Long>, realm: v8::Weak<v8::Context>, f: PendingFrame, render: Instant) {
    let render = render.max(f.start);
    let dur = (render - f.start).as_secs_f64() * 1000.0;
    if dur < LONG_MS || f.first_render {
        // a first frame is finished by the document's first rendering update ([`first_render`]), with its own start
        return;
    }
    let mut blocking = f.blocking_excl_longest_ms;
    if f.longest_ms > LONG_MS {
        blocking += f.longest_ms - LONG_MS;
    }
    ready.push(Long { realm, start: f.start, end: render, render_start: Some(render), blocking_ms: blocking });
}

/// The realm's document had its FIRST RENDERING UPDATE at `render_ms` (`cssom`, whether or not it drew anything --
/// Blink's first frame ends at its first `BeginMainFrame`): its first frame ran from the navigation's first task --
/// Blink's monitor opens it on the first task that completes with a main frame requested, before the commit, near
/// the fetch's start (Chrome 150: 24 and 40 ms against a `fetchStart` of 7 and 15 and a `responseStart` of 500) --
/// to this update, its blocking the tasks before it. A page's first frame waits out its network fetch, so it is
/// long: a bot challenge page's timeline holds it when the challenge first reads it, where this engine,
/// which started the frame at the response and ended it at the first PAINT, had none (2026-10-09).
///
/// Only a LOCAL ROOT's document has a first frame of its own (a same-site frame's first rendering update is its
/// local root's frame), and a cross-site frame's -- in this process, or the one an out-of-process frame runs in --
/// begins with its document's first task after the commit: Chrome makes its widget at the commit, and its timeline
/// holds no frame for the fetch, nor for the process it waited for (a frame answering after
/// 600 ms has none; this engine reported one from `fetchStart`, twice in-process -- once for the frame's initial
/// `about:blank` -- where the challenge frame had none).
pub(crate) fn first_render(scope: &mut v8::PinScope, render_ms: f64) {
    let ctx = scope.get_current_context();
    let key = ctx.global(scope).get_identity_hash().get();
    let root = crate::core::html::browsing_context::realm_local_root(scope);
    if root.global(scope).get_identity_hash().get() != key {
        return;
    }
    let embedded = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        crate::core::html::remote_frame::embedding(&st).is_some()
    };
    let frame = embedded || crate::core::html::browsing_context::current_realm_ctx(scope).is_some();
    let pending = monitor(scope).and_then(|mon| {
        let Ok(mut m) = mon.try_borrow_mut() else { return None };
        m.rendered.insert(key);
        let i = m.frames.iter().position(|f| f.0 == key)?;
        Some(m.frames.remove(i).2)
    });
    let start_ms = if frame {
        // a frame's first frame opens with its document's first task after the commit (Blink's monitor, on the
        // widget the commit made): none before its first rendering update, no first frame
        match pending.as_ref() {
            Some(f) => crate::core::timing::performance::instant_to_ms(scope, f.start).max(0.0),
            None => return,
        }
    } else {
        crate::core::timing::performance::document_fetch_start(scope).unwrap_or(0.0)
    };
    let dur = render_ms - start_ms;
    if dur < LONG_MS {
        return;
    }
    let (longest, excl) = pending.map(|f| (f.longest_ms, f.blocking_excl_longest_ms)).unwrap_or((0.0, 0.0));
    let mut blocking = excl;
    if longest > LONG_MS {
        blocking += longest - LONG_MS;
    }
    crate::core::timing::performance::add_long_animation_frame(scope, start_ms, render_ms, Some(render_ms), blocking);
}

/// At each rendering opportunity: frames whose vsync passed while the thread was idle rendered at their vsync,
/// and every finished long frame goes on its realm's timeline.
pub fn loaf_rendering_opportunity(scope: &mut v8::PinScope) {
    let now = Instant::now();
    let Some(mon) = monitor(scope) else { return };
    let ready = {
        let Ok(mut m) = mon.try_borrow_mut() else { return };
        let mut i = 0;
        while i < m.frames.len() {
            if m.frames[i].2.vsync <= now && !m.frames[i].2.first_render {
                let (_, c, f) = m.frames.remove(i);
                let at = f.vsync;
                finish(&mut m.ready, c, f, at);
            } else {
                i += 1;
            }
        }
        std::mem::take(&mut m.ready)
    };
    for l in ready {
        let Some(ctx) = l.realm.to_local(scope) else { continue };
        let scope = &mut v8::ContextScope::new(scope, ctx);
        let to_ms = |scope: &mut v8::PinScope, t: Instant| crate::core::timing::performance::instant_to_ms(scope, t);
        let start = to_ms(scope, l.start);
        if start < 0.0 {
            // before this document's time origin: the engine's own set-up, no frame of the page's
            continue;
        }
        let end = to_ms(scope, l.end);
        let render = l.render_start.map(|r| to_ms(scope, r));
        crate::core::timing::performance::add_long_animation_frame(scope, start, end, render, l.blocking_ms);
    }
}

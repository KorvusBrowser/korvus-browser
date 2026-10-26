//! The TIMER TASK SOURCE's five members, as ONE set of native bodies serving BOTH realms
//! (invariants I1/I3/I9).
//!
//! `setTimeout`, `setInterval`, `clearTimeout`, `clearInterval` and `queueMicrotask` are
//! `WindowOrWorkerGlobalScope` members. The worker got them as native bodies on
//! `WorkerGlobalScope` rows at W6.2; the window kept five JS arrows in `engine.rs`'s `BOOTSTRAP`
//! string behind the native-mask op, which reported `[native code]` for a body that was JavaScript —
//! a member that was not a table row (I1), whose body was JS (I3), defined by the kernel (I9).
//!
//! This file is what closed that, and it is deliberately ONE file rather than a second copy:
//! `worker_scope.rs` held these bodies alone until W9.23, and the only thing that differs between
//! the two realms is the HOLDER NAME WebIDL puts in the error messages — `'Window'` in a document,
//! `'WorkerGlobalScope'` in a worker. So the holder TRAVELS, as the function's call data, and both
//! realms mount the same five callbacks over the same [`crate::scheduler`] queue.
//!
//! # The three properties `BOOTSTRAP`'s comments recorded, which a naive port loses
//!
//! 1. **One callback per turn of the event loop.** `op_timer_next_due` is an ASYNC op, so its
//!    resolution is a fresh turn, and running exactly ONE callback per turn is what puts a microtask
//!    checkpoint between two timers due at the same instant. Chrome answers `t1, mt1, t2`; this
//!    engine used to answer `t1, t2, mt1`. [`start_pump`] + [`cb_pump_step`] are
//!    `do { id = await next(); if (!id) break; run(id); } while (claim())` with the `await` written
//!    as a `.then`, and `core/korvus-runtime/tests/timers.rs` is the gate on it.
//! 2. **The pump stops when the queue drains** (`op_timer_next_due` answers 0). A perpetually
//!    pending op would keep `run_event_loop` from ever resolving and cost every page a full pump
//!    budget.
//! 3. **"Is there already a pump?" is answered by the KERNEL** ([`Scheduler::claim_pump`]), never by
//!    a flag in scope: a flag would be stranded true if V8 terminated the pump's frame inside a
//!    callback — exactly what the WPT runner's watchdog does — and that realm would then silently
//!    lose every later timer. Rust can tell a live pump from a destroyed one; a boolean cannot.

use std::collections::HashMap;
use deno_core::{op2, v8, JsRuntime};

use crate::scheduler::Scheduler;

/// A native function carrying `name`/`length` and the holder name as its call data.
///
/// `ConstructorBehavior::Throw` for the reason `worker.rs::build_fn` gives: a real DOM method carries
/// no own `.prototype`, and `new setTimeout()` is Chrome's `TypeError: setTimeout is not a
/// constructor` (measured, 150.0.7871.46).
fn build_fn_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    name: &str,
    length: i32,
    data: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Function> {
    let f = v8::Function::builder(webapi::binding::refs::f(cb))
        .data(data)
        .length(length)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .unwrap();
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    f
}

/// The row's HOLDER, read back off the function's call data — `'Window'` or `'WorkerGlobalScope'`.
/// It is what WebIDL names in every message these bodies throw, and the ONLY thing the two realms
/// disagree about.
fn holder(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> String {
    webapi::binding::protos::body_data(scope, args).to_rust_string_lossy(scope)
}

/// One TypeError with Chrome's wording, for a row whose WebIDL requires an argument. Measured with
/// `google-chrome --headless=new` 150.0.7871.46 on 2026-09-12:
///
/// ```text
/// setTimeout()      Failed to execute 'setTimeout' on 'Window': 1 argument required, but only 0 present.
/// setInterval()     Failed to execute 'setInterval' on 'Window': 1 argument required, but only 0 present.
/// queueMicrotask()  Failed to execute 'queueMicrotask' on 'Window': 1 argument required, but only 0 present.
/// clearTimeout()    undefined          clearInterval()   undefined
/// ```
///
/// …and the same three sentences with `'WorkerGlobalScope'` in a worker, which is why the interface
/// name travels as an argument rather than being hardcoded once.
pub(crate) fn require_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, need: usize, iface: &str, member: &str) -> bool {
    if args.length() as usize >= need {
        return true;
    }
    let msg = format!(
        "Failed to execute '{member}' on '{iface}': {need} argument{} required, but only {} present.",
        if need == 1 { "" } else { "s" },
        args.length()
    );
    // Blink's context applied after the error is made (`deno_core::error::native_error`)
    let e = deno_core::error::native_error(scope, "TypeError", &msg);
    scope.throw_exception(e);
    false
}

// ── the pump ────────────────────────────────────────────────────────────────────────────────────

/// One timer callback per turn of the event loop — property 1 of this file's header.
pub(crate) fn start_pump(scope: &mut v8::PinScope) {
    let Some(next_due) = webapi::stashed_op(scope, "op_timer_next_due") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    let Some(p) = next_due.call(tc, undef, &[]) else {
        tc.reset();
        return;
    };
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(p) else { return };
    let step = build_fn(tc, cb_pump_step, "", 1);
    // A TASK, NOT A PROMISE JOB (`webapi::then_task`): the step ran inside V8's microtask
    // checkpoint, where no checkpoint of its own can run -- a timer's or an engine task's listeners all ran
    // before the first one's microtasks (Chrome runs them after each listener).
    webapi::then_task(tc, promise, step, None);
}

/// A plain native function with no call data — the pump's own continuation, which is not a Web API
/// member and therefore has no holder.
fn build_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    let f = v8::Function::builder(webapi::binding::refs::f(cb))
        .length(length)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .unwrap();
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    f
}

/// One turn of the pump: run the due timer, then take the next if the queue is not empty.
fn cb_pump_step(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = args.get(0).uint32_value(scope).unwrap_or(0);
    if id == 0 {
        return; // the queue drained; `op_timer_next_due` has already parked the pump (property 2).
    }
    // A THROWING CALLBACK IS REPORTED, NOT CARRIED INTO THE PUMP. `scheduler::run_timer` does the
    // reporting itself (HTML "report an exception" -> an ErrorEvent at the global), which is what the
    // deleted JS pump's `catch (e) { KB.__recordUncaught(e) }` reached for through the bridge.
    crate::scheduler::run_timer(scope, id);
    let again = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<Scheduler>() {
            Some(s) => s.claim_pump(),
            None => false,
        }
    };
    if again {
        start_pump(scope);
    }
}

// ── the five bodies ─────────────────────────────────────────────────────────────────────────────

/// Arm a timer from a scope row. A non-callable handler is a legal call that simply never does
/// anything — Chrome, measured: `setTimeout(1)` answers a handle rather than throwing — which is what
/// the promise chain this replaced did and what `BOOTSTRAP`'s `kArm` did after it.
fn arm(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, repeat: bool) -> u32 {
    // the window whose timer this is ([`RealmHandles::realm`])
    let realm = webapi::incumbent_realm(scope);
    // A handler that is not a function is SOURCE TEXT (HTML "timer initialization steps"): stringified
    // now -- a `TrustedScript` stringifies to its script -- and run as a classic script in the global
    // scope each time the timer fires, its errors reported like any script's. It used to be replaced by
    // a no-op, so `setTimeout("x=1")` and `setTimeout(policy.createScript(...))` never ran
    // (measured on Chrome 150).
    fn run_source(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _: v8::ReturnValue) {
        let src = webapi::binding::protos::body_data(scope, &args);
        let Ok(src) = v8::Local::<v8::String>::try_from(src) else { return };
        v8::tc_scope!(let tc, scope);
        let _ = v8::Script::compile(tc, src, None).and_then(|s| s.run(tc));
        if tc.has_caught() {
            // reported where the `throw` ran, read off the catch before it is reset (`webapi::caught_site`)
            let thrown = tc.exception().map(|err| {
                let m = tc.message();
                (v8::Global::new(tc, err), webapi::caught_site(tc, err, m))
            });
            tc.reset();
            if let Some((err, site)) = thrown {
                let err = v8::Local::new(tc, &err);
                webapi::report_exception_at(tc, err, site);
            }
        }
    }
    let cb = match v8::Local::<v8::Function>::try_from(args.get(0)) {
        Ok(f) => f,
        Err(_) => {
            // Blink `DOMTimer::setTimeout(String)`, in its order: the Trusted Types check (sink
            // "Window setTimeout", may throw), then CSP `'unsafe-eval'`, then an empty script -- the last
            // two answer 0 and make no timer (Chrome 150, measured: `setTimeout("x=1")` under
            // `script-src 'unsafe-inline'` answers 0 and never runs).
            let iface = holder(scope, args);
            let member = if repeat { "setInterval" } else { "setTimeout" };
            let Some(text) = webapi::compliant_script(scope, args.get(0), &iface, member) else { return 0 };
            if !webapi::string_compilation_allowed(scope) || text.length() == 0 {
                return 0;
            }
            build_fn_data(scope, run_source, "", 0, text.into())
        }
    };
    let ms = args.get(1).number_value(scope).unwrap_or(0.0);
    let ms = if ms.is_finite() { ms } else { 0.0 };
    let extra: Vec<v8::Local<v8::Value>> = (2..args.length()).map(|i| args.get(i)).collect();
    let cbg = v8::Global::new(scope, cb);
    let argsg: Vec<v8::Global<v8::Value>> = extra.iter().map(|v| v8::Global::new(scope, *v)).collect();
    let in_checkpoint = crate::scheduler::in_microtask_checkpoint(scope);
    // the window the timer is set on -- the realm this `setTimeout` belongs to, which V8 entered to call it -- as
    // its WindowProxy: the callback's `this` (the receiver V8 hands a bare call is the global OBJECT behind it)
    let this = {
        let t: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
        Some(v8::Global::new(scope, t))
    };
    // a single-shot 0 ms timer is an immediate task (`Scheduler::set_immediate_timer`), queued on the event loop now
    let has_loop = JsRuntime::op_state_from(scope).borrow().try_borrow::<deno_core::V8TaskSpawner>().is_some();
    if has_loop {
        let immediate = {
            let op_state = JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return 0 };
            sched.set_immediate_timer(&cbg, &argsg, &this, ms, repeat, in_checkpoint)
        };
        if let Some(id) = immediate {
            spawn_ready(scope, id);
            return page_handle(scope, id, realm);
        }
    }
    // THE PUMP FIRST, THEN THE TIMER: its deadline is taken once the call's own work is done, as Blink posts the delayed
    // task at the end of `DOMTimer`'s constructor. Starting a pump (its op, its promise, its step) took a page's first
    // `setTimeout` 50-90 us AFTER its deadline was taken, so two `setTimeout(f, 10)`s in a row came due that far apart,
    // across the window in which Chrome moves timers due together at one selection ([`crate::scheduler`]'s
    // `SAME_WAKE`); in Chrome 152 a whole first `setTimeout` takes 25-35 us (2026-10-23, a 5 us clock).
    let claim = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return 0 };
        sched.claim_pump()
    };
    if claim {
        start_pump(scope);
    }
    let id = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return 0 };
        sched.set_timer(cbg, argsg, this, ms, repeat, in_checkpoint)
    };
    page_handle(scope, id, realm)
}

/// The handles `setTimeout` / `setInterval` answer, PER REALM: HTML keeps a map of active timers per
/// global object, so a frame's first `setTimeout` answers 1 whatever its parent has armed. The
/// scheduler's own ids are one counter for the isolate -- every realm's timers AND the engine's tasks
/// (`queue_task`: message deliveries, observer notifications) -- so a page handed those saw them jump
/// and skip. Chrome 150, measured 2026-10-07: a frame's first two `setTimeout`s answer [1, 2] beside a
/// parent's [1, 2]; korvus answered [3, 4] (korvus-analysis U-2, a bot-challenge frame read
/// [9, 12, 14, 15] where Chrome read [1, 2, 3, 4]).
#[derive(Default)]
pub(crate) struct RealmTimerHandles {
    realms: Vec<RealmHandles>,
}

struct RealmHandles {
    /// the realm: the browsing context of the INCUMBENT realm (`webapi::incumbent_realm`: the realm of the script
    /// function that called the timer method), `None` for the page's and a worker's -- the window whose `setTimeout` a
    /// script calls, its "map of active timers". It was the realm whose script was running (the entered context):
    /// a bot-detection script's press & hold arms its progress interval from a PAGE function a listener of the challenge FRAME's
    /// event called, and clears it from the interval's own callback -- two maps, so the clear missed and the interval
    /// ran on, every 32 ms, into the captcha's reset (Chrome 150: one map, the page's).
    /// The window's own method would be exact, but korvus builds every window's timer functions in the page's context
    /// (a deno op makes them), so neither their context nor their `this` names a frame.
    realm: Option<u32>,
    next: u32,
    /// page handle -> scheduler id, while the timer may still run
    live: HashMap<u32, u32>,
}

/// This realm's handle for the scheduler's timer `id`.
fn page_handle(scope: &mut v8::PinScope, id: u32, realm: Option<u32>) -> u32 {
    if id == 0 {
        return 0;
    }
    let op_state = JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<RealmTimerHandles>().is_none() {
        st.put(RealmTimerHandles::default());
    }
    let at = {
        let h = st.borrow::<RealmTimerHandles>();
        h.realms.iter().position(|r| r.realm == realm)
    };
    let at = match at {
        Some(i) => i,
        None => {
            let h = st.borrow_mut::<RealmTimerHandles>();
            h.realms.push(RealmHandles { realm, next: 1, live: HashMap::new() });
            h.realms.len() - 1
        }
    };
    // a finished timer's entry goes once there are many (a page arming timers forever never grows it)
    let prune: Option<Vec<u32>> = {
        let h = st.borrow::<RealmTimerHandles>();
        (h.realms[at].live.len() >= 256).then(|| h.realms[at].live.iter().map(|(&k, &v)| (k, v)).filter(|&(_, v)| !st.borrow::<Scheduler>().is_live(v)).map(|(k, _)| k).collect())
    };
    let h = st.borrow_mut::<RealmTimerHandles>();
    let r = &mut h.realms[at];
    if let Some(dead) = prune {
        for k in dead {
            r.live.remove(&k);
        }
    }
    let handle = r.next;
    r.next = r.next.wrapping_add(1).max(1);
    r.live.insert(handle, id);
    handle
}

fn cb_set_timeout(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let iface = holder(scope, &args);
    if !require_args(scope, &args, 1, &iface, "setTimeout") {
        return;
    }
    let id = arm(scope, &args, false);
    rv.set_uint32(id);
}

/// `queueEngineTask(f, ms)` -- see [`Scheduler::queue_task`].
fn cb_queue_engine_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    queue_kind(scope, args, rv, Scheduler::queue_task, true);
}

/// `queueFrameTask(f, ms)` -- the rendering frame, see [`Scheduler::queue_frame_task`].
fn cb_queue_frame_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    queue_kind(scope, args, rv, Scheduler::queue_frame_task, false);
}

/// `queueIdleTask(f, notBeforeMs)` -- an idle task, see [`Scheduler::queue_idle_task`].
fn cb_queue_idle_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    queue_kind(scope, args, rv, Scheduler::queue_idle_task, false);
}

/// The three engine entries' one body: queue `f` with `ms`, start the pump if none runs, answer the id.
/// [`Scheduler::queue_task`] from Rust: `f` runs as an engine task `ms` from now.
pub(crate) fn queue_engine_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ms: f64) {
    if !(ms > 0.0) && queue_ready_task(scope, f).is_some() {
        return;
    }
    let cbg = v8::Global::new(scope, f);
    let claim = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return };
        sched.queue_task(cbg, ms);
        sched.claim_pump()
    };
    if claim {
        start_pump(scope);
    }
}

/// An engine task that is ready now, queued on the event loop at once -- as a fetch's end is (`webapi::then_task`) --
/// so every ready task runs in the order it was queued ([`Scheduler::record_ready_task`]). `None` with no event loop
/// to queue on (the caller then uses the pump).
fn queue_ready_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<u32> {
    let op_state = JsRuntime::op_state_from(scope);
    op_state.borrow().try_borrow::<deno_core::V8TaskSpawner>()?;
    let cbg = v8::Global::new(scope, f);
    let id = op_state.borrow_mut().try_borrow_mut::<Scheduler>()?.record_ready_task(cbg);
    spawn_ready(scope, id);
    Some(id)
}

/// Queue the run of ready task `id` on the event loop now, in this realm, its microtask checkpoint and the rest of the
/// task ([`webapi::run_next_tasks`]) after it.
fn spawn_ready(scope: &mut v8::PinScope, id: u32) {
    let Some(spawner) = JsRuntime::op_state_from(scope).borrow().try_borrow::<deno_core::V8TaskSpawner>().cloned() else { return };
    let ctx = v8::Global::new(scope, scope.get_current_context());
    spawner.spawn(move |scope| {
        v8::scope!(let hs, scope);
        let ctx = v8::Local::new(hs, &ctx);
        let scope = &mut v8::ContextScope::new(hs, ctx);
        webapi::run_as_task(scope, |scope| crate::scheduler::run_ready_task(scope, id));
        // an interval's next tick: immediate again, or the pump's once the clamp delays it
        let next = JsRuntime::op_state_from(scope).borrow_mut().try_borrow_mut::<Scheduler>().and_then(|s| s.interval_next_immediate(id));
        match next {
            Some(true) => spawn_ready(scope, id),
            Some(false) => {
                let claim = JsRuntime::op_state_from(scope).borrow_mut().try_borrow_mut::<Scheduler>().is_some_and(|s| s.claim_pump());
                if claim {
                    start_pump(scope);
                }
            }
            None => {}
        }
    });
}

/// [`Scheduler::queue_next_task`] from Rust: `f` runs as the very next task.
pub(crate) fn queue_next_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    // the rest of the running task, run by its runner right after its checkpoint (`webapi::queue_next_task`)
    if webapi::queue_next_task(scope, f) {
        return;
    }
    let cbg = v8::Global::new(scope, f);
    let claim = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return };
        sched.queue_next_task(cbg);
        sched.claim_pump()
    };
    if claim {
        start_pump(scope);
    }
}

/// `plain`: an engine task, which runs as soon as it is ready (not the rendering frame or an idle task, which wait for
/// their time).
fn queue_kind(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, queue: fn(&mut Scheduler, v8::Global<v8::Function>, f64) -> u32, plain: bool) {
    let Ok(cb) = v8::Local::<v8::Function>::try_from(args.get(0)) else { return };
    let ms = args.get(1).number_value(scope).unwrap_or(0.0);
    // a plain engine task ready now goes on the event loop at once
    if plain && !(ms > 0.0) {
        if let Some(id) = queue_ready_task(scope, cb) {
            rv.set_uint32(id);
            return;
        }
    }
    let cbg = v8::Global::new(scope, cb);
    let (id, claim) = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return };
        let id = queue(sched, cbg, ms);
        (id, sched.claim_pump())
    };
    if claim {
        start_pump(scope);
    }
    rv.set_uint32(id);
}

fn cb_set_interval(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let iface = holder(scope, &args);
    if !require_args(scope, &args, 1, &iface, "setInterval") {
        return;
    }
    let id = arm(scope, &args, true);
    rv.set_uint32(id);
}

/// `clearTimeout(optional long handle = 0)` / `clearInterval` — anything that is not one of our ids
/// is a no-op, and id 0 is never handed out, so it is the safe "nothing" to pass. The handle is
/// OPTIONAL, so both correctly accept a bare call and both are `length` 0 (Chrome, measured).
pub(crate) fn clear_timer(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) {
    let n = args.get(0).number_value(scope).unwrap_or(0.0);
    let handle = if (1.0..=4_294_967_295.0).contains(&n) { n as u32 } else { 0 };
    // this window's handle -> the scheduler's id (a handle another window armed is not this window's)
    let realm = webapi::incumbent_realm(scope);
    let op_state = JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let id = st.try_borrow_mut::<RealmTimerHandles>().and_then(|h| {
        let r = h.realms.iter_mut().find(|r| r.realm == realm)?;
        r.live.remove(&handle)
    });
    if let (Some(id), Some(s)) = (id, st.try_borrow_mut::<Scheduler>()) {
        s.clear_timer(id);
    }
}

/// Clear the scheduler's timer `id` itself (not a page handle): a worker's `cancelAnimationFrame`,
/// whose ids are the scheduler's.
pub(crate) fn clear_scheduler_timer(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) {
    let n = args.get(0).number_value(scope).unwrap_or(0.0);
    let id = if (1.0..=4_294_967_295.0).contains(&n) { n as u32 } else { 0 };
    let op_state = JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(s) = st.try_borrow_mut::<Scheduler>() {
        s.clear_timer(id);
    }
}

fn cb_clear_timer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    clear_timer(scope, &args);
}

/// `queueMicrotask(callback)` — a REAL microtask on V8's queue.
///
/// The window's JS body kept "whatever the host realm already provides" and fell back to
/// `Promise.resolve().then(cb)`; both are one extra promise job, and the fallback has a rejection path
/// no browser has. `enqueue_microtask` is the thing itself.
///
/// A non-callable is Chrome's `parameter 1 is not of type 'Function'.` (measured for `1` and for
/// `null`), with the holder's name in front of it exactly as the arity message carries it.
fn cb_queue_microtask(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        // The holder's name is read for the message only: it is a string made from the row's data on every call
        // that reads it.
        let iface = holder(scope, &args);
        if !require_args(scope, &args, 1, &iface, "queueMicrotask") {
            return;
        }
        let msg = format!("Failed to execute 'queueMicrotask' on '{iface}': parameter 1 is not of type 'Function'.");
        let e = deno_core::error::native_error(scope, "TypeError", &msg);
        scope.throw_exception(e);
        return;
    };
    // The job is a native trampoline ([`cb_microtask_job`]); a native function adds no JS frame, so the
    // callback runs as the bare function would. NOT A CONSTRUCTOR: a function V8 may construct gets a `prototype`
    // object and a map of its own, made for every call and garbage after one -- most of what a `queueMicrotask` cost:
    // 13,800 user instructions a call with its job, 5,240 without them (perf stat, 200,000 calls, 2026-10-23); Chrome
    // 152 takes 200-214 ns a call, this took 830-1,950 and takes 360-400.
    match v8::Function::builder(webapi::binding::refs::f(cb_microtask_job))
        .data(f.into())
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
    {
        Some(t) => scope.enqueue_microtask(t),
        None => scope.enqueue_microtask(f),
    }
}

/// A `queueMicrotask` job as Blink runs one (`V8VoidFunction::InvokeAndReportException`): the callback, called
/// with no receiver, and a throw REPORTED there -- where it was thrown, as that verbose `TryCatch` has V8
/// report it. Put on V8's queue as the bare function, a throw went to V8's microtask runner, which hands it to the
/// isolate's message listeners -- none is registered, so no `onerror` ever saw it -- and would place a thrown
/// Error where it was CONSTRUCTED (stock Chrome 150, 2026-10-10: `var e = new Error();\n throw e;` in a
/// microtask reports the throw's line). With the recorder on, the call is recorded
/// (`VoidFunction`, the type Chromium's queueMicrotask invokes).
fn cb_microtask_job(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.data()) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    #[cfg(feature = "analysis")]
    let r = if spec::analysis::enabled() { webapi::call_traced(tc, "VoidFunction", f, undef, &[]) } else { f.call(tc, undef, &[]) };
    #[cfg(not(feature = "analysis"))]
    let r = f.call(tc, undef, &[]);
    if r.is_some() || !tc.has_caught() || !tc.can_continue() {
        return;
    }
    let thrown = tc.exception().map(|e| {
        let m = tc.message();
        (v8::Global::new(tc, e), webapi::caught_site(tc, e, m))
    });
    tc.reset();
    if let Some((e, site)) = thrown {
        let e = v8::Local::new(tc, &e);
        webapi::report_exception_at(tc, e, site);
    }
}

// ── the factories the two tables call ───────────────────────────────────────────────────────────

/// The five rows, built for `iface`'s holder. `None` for a name this file does not serve, at which
/// point the caller decides what a missing kernel service means for its realm.
pub(crate) fn timer_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    length: i32,
    iface: &str,
) -> Option<v8::Local<'s, v8::Function>> {
    let d: v8::Local<v8::Value> = v8::String::new(scope, iface)?.into();
    // webapi's kernel builder, not this file's copy: it records while a template capture
    // runs.
    let f = match name {
        "setTimeout" => webapi::binding::protos::build_kernel_fn_data(scope, cb_set_timeout, name, length, d),
        "setInterval" => webapi::binding::protos::build_kernel_fn_data(scope, cb_set_interval, name, length, d),
        "clearTimeout" | "clearInterval" => webapi::binding::protos::build_kernel_fn_data(scope, cb_clear_timer, name, length, d),
        "queueMicrotask" => webapi::binding::protos::build_kernel_fn_data(scope, cb_queue_microtask, name, length, d),
        // The ENGINE's task entry (never on a page's surface: only webapi's `queue_task` asks for it).
        "queueEngineTask" => webapi::binding::protos::build_kernel_fn_data(scope, cb_queue_engine_task, name, length, d),
        // ...and the rendering frame's and the idle tasks' (webapi's `rendering` and `animation_frame`)
        "queueFrameTask" => webapi::binding::protos::build_kernel_fn_data(scope, cb_queue_frame_task, name, length, d),
        "queueIdleTask" => webapi::binding::protos::build_kernel_fn_data(scope, cb_queue_idle_task, name, length, d),
        _ => return None,
    };
    Some(f)
}

/// THE FACTORY the `Window` table calls for its five timer rows (`core/html/window.rs`), the same
/// cross-crate shape `op_worker_scope_fn` has served the two worker scope tables since W6.2: the
/// kernel owns the task source, the TABLE owns the shape, and the function that comes back carries
/// the ROW's own `name` and `length`.
///
/// A realm that does not register this op gets the D2 stub for each row — the honest
/// not-implemented answer, counted (invariant I7) — rather than a silently missing global.
#[op2]
pub fn op_window_scope_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Value> {
    match timer_row(scope, name, length, "Window") {
        Some(f) => f.into(),
        None => v8::undefined(scope).into(),
    }
}

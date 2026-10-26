//! An engine promise's continuation run as a TASK of the event loop, not as a promise job.
//!
//! The engine's pumps -- the timer task source, a script's or a sheet's fetch, a frame's load, a worker's or a
//! socket's message -- wait on an async op and chained their next step with `then`. So every step ran as a
//! PROMISE JOB, inside V8's microtask checkpoint, and V8 makes a checkpoint nested in another a no-op: nothing a
//! step did could have a checkpoint of its own. A listener's microtasks waited for every later listener, and a
//! task's for every later step of the same drain, where HTML runs them after each callback when no script is on
//! the stack ("clean up after running a callback", `call_listener`) and after each task.
//!
//! [`then_task`] chains a trampoline instead. When the promise settles, the trampoline hands the step to
//! deno_core's `V8TaskSpawner`, which runs it from the event loop with no script and no microtask on the stack
//! (a task), in the realm the job would have run in, and the task's microtask checkpoint follows it.
//!
//! Only a step whose result nothing reads goes through here: the derived promise settles with `undefined` when
//! the job hands the step over, not with what the step returns.

use deno_core::{v8, JsRuntime, V8TaskSpawner};

/// `p.then(ok, err)` with each step run as a task (see the module).
pub fn then_task<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<'s, v8::Promise>, ok: v8::Local<'s, v8::Function>, err: Option<v8::Local<'s, v8::Function>>) -> Option<v8::Local<'s, v8::Promise>> {
    // Only a promise still PENDING here can be settled by an op's resolution, and only then is the step that op's
    // continuation (see `cb_trampoline`). One that has settled already -- `start_shared_fetch` handing back a kept image
    // or font, a preloaded or staged response -- queues its step when the page's code chains it: a new task, at the
    // back. Run as the continuation of whatever arrival was being resolved then, an `<img>` of a loaded image, started
    // in a `fetch()`'s reaction, loaded ahead of a message posted before it: 'f img m' where Chrome 152 runs 'f m img'
    // (5 of 5 trials, three runs, 2026-10-23).
    let cont = p.state() == v8::PromiseState::Pending;
    then_task_as(scope, p, ok, err, cont)
}

/// [`then_task`] for a step Chrome runs in a task it POSTS once the response has been processed -- never inside the
/// response's own task: an `<img>`'s and a `<link rel=stylesheet>`'s `load` or `error` (Blink's `ImageLoader` and the
/// link's pending load event post theirs), an `@import`'s end. The step is a new task, queued when the promise settles,
/// behind what was queued before: beside a `MessageChannel` loop of 5 ms tasks Chrome 152 fires an image's and a
/// stylesheet's `load` after ONE more task has started past the response's end (35 of 35 each, seven runs, 2026-10-23);
/// as the response's continuation they fired after none, an order Chrome never produced.
pub fn then_new_task<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<'s, v8::Promise>, ok: v8::Local<'s, v8::Function>, err: Option<v8::Local<'s, v8::Function>>) -> Option<v8::Local<'s, v8::Promise>> {
    then_task_as(scope, p, ok, err, false)
}

fn then_task_as<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<'s, v8::Promise>, ok: v8::Local<'s, v8::Function>, err: Option<v8::Local<'s, v8::Function>>, cont: bool) -> Option<v8::Local<'s, v8::Promise>> {
    let ok = trampoline(scope, ok, cont)?;
    match err {
        Some(e) => {
            let e = trampoline(scope, e, cont)?;
            p.then2(scope, ok, e)
        }
        None => p.then(scope, ok),
    }
}

fn trampoline<'s>(scope: &mut v8::PinScope<'s, '_>, step: v8::Local<'s, v8::Function>, cont: bool) -> Option<v8::Local<'s, v8::Function>> {
    if cont {
        v8::Function::builder(crate::binding::refs::f(cb_trampoline_continuation)).data(step.into()).build(scope)
    } else {
        v8::Function::builder(crate::binding::refs::f(cb_trampoline)).data(step.into()).build(scope)
    }
}

/// The job: queue the step as a task with the value the promise settled with.
fn cb_trampoline(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    queue_step(scope, args, false);
}

/// [`cb_trampoline`] for a promise that was pending when the step was chained: the step may continue an arrival.
fn cb_trampoline_continuation(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    queue_step(scope, args, true);
}

fn queue_step(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, cont: bool) {
    let Ok(step) = v8::Local::<v8::Function>::try_from(args.data()) else { return };
    let value = args.get(0);
    let spawner = JsRuntime::op_state_from(scope).borrow().try_borrow::<V8TaskSpawner>().cloned();
    let Some(spawner) = spawner else {
        // no event loop to queue on: the step runs as the job it was
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let _ = step.call(scope, undef, &[value]);
        return;
    };
    let ctx = v8::Global::new(scope, scope.get_current_context());
    let step = v8::Global::new(scope, step);
    let value = v8::Global::new(scope, value);
    // A CONTINUATION: when the promise is an async op's, settled as the event loop resolves the op that completed,
    // the step is that arrival's own task and runs right after it -- not behind every task queued while the op's
    // result waited to be noticed (`deno_core::V8TaskSpawner::spawn_continuation`). A timer's callback is such a step
    // (the pump's `op_timer_next_due`), and so are an XMLHttpRequest's events: under a `MessageChannel` loop of 5 ms
    // tasks Chrome 152 runs a `setTimeout(50)` after ONE more task has started past its deadline, and an
    // XMLHttpRequest's `load` right after the task its response came in during (2026-10-23, three runs alike). Queued
    // behind the message that next task posts, the step would run a task later still.
    let task = move |scope: &mut v8::PinScope| {
        v8::scope!(let hs, scope);
        let ctx = v8::Local::new(hs, &ctx);
        let scope = &mut v8::ContextScope::new(hs, ctx);
        let step = v8::Local::new(scope, &step);
        let value = v8::Local::new(scope, &value);
        run_as_task(scope, |scope| {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            v8::tc_scope!(let tc, scope);
            let _ = step.call(tc, undef, &[value]);
            // A step reports what page code throws inside it. What the step itself throws is the engine's and
            // stops here, as it did in the derived promise of the job it was. A termination stays pending.
            if tc.has_caught() && !tc.has_terminated() {
                tc.reset();
            }
        });
    };
    if cont {
        spawner.spawn_continuation(task);
    } else {
        spawner.spawn(task);
    }
}

/// Resolve `resolver` with `value` in a TASK queued now, not in the running one: Fetch's "fully read body" queues a fetch
/// task to run processBody, and Blink resolves a body consumer's promise from a task it posts (`BodyConsumerBase::
/// ResolveLater`). Chrome 152 (2026-10-23, two runs alike): `postMessage(m); new Response('{}').json().then(ok);
/// Promise.resolve().then(mt)` logs `mt m ok`, and a `fetch()` body's `json()` resolves after ONE more task of a
/// `MessageChannel` loop has started past the body's arrival (35 of 35, seven runs) -- where a body reader's `done`
/// comes after none. A rejection is not queued: a body already used, or `json()` of bad JSON, rejects before `mt` runs
/// in Chrome too.
pub(crate) fn resolve_in_task(scope: &mut v8::PinScope, resolver: v8::Local<v8::PromiseResolver>, value: v8::Local<v8::Value>) {
    let spawner = JsRuntime::op_state_from(scope).borrow().try_borrow::<V8TaskSpawner>().cloned();
    let Some(spawner) = spawner else {
        // no event loop to queue on: resolved now
        resolver.resolve(scope, value);
        return;
    };
    let ctx = v8::Global::new(scope, scope.get_current_context());
    let resolver = v8::Global::new(scope, resolver);
    let value = v8::Global::new(scope, value);
    spawner.spawn(move |scope| {
        v8::scope!(let hs, scope);
        let ctx = v8::Local::new(hs, &ctx);
        let scope = &mut v8::ContextScope::new(hs, ctx);
        let resolver = v8::Local::new(scope, &resolver);
        let value = v8::Local::new(scope, &value);
        run_as_task(scope, |scope| {
            resolver.resolve(scope, value);
        });
    });
}

/// HTML "clean up after running a callback": the microtask checkpoint, when no script is on the stack -- after
/// each listener, animation-frame callback or observer callback the engine calls from a task. With a script on the
/// stack (`dispatchEvent` or `click()` from script) the script's own checkpoint runs them; inside a promise job V8
/// makes it a no-op.
pub(crate) fn clean_up_after_running_callback(scope: &mut v8::PinScope) {
    // inside an algorithm Blink runs in one outer MicrotasksScope ([`hold_checkpoints`]), the scope's end runs them
    if JsRuntime::op_state_from(scope).borrow().try_borrow::<CheckpointHold>().is_some_and(|h| h.0 > 0) {
        return;
    }
    // inside an error's report its listeners' microtasks wait for the report to end (`report::is_reporting`)
    if crate::core::events::report::is_reporting(scope) {
        return;
    }
    // a callback the engine invoked, or a `dispatchEvent` call, is below ([`ScriptBelow`])
    if SCRIPT_BELOW.with(|d| d.get()) > 0 {
        return;
    }
    if v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0 {
        scope.perform_microtask_checkpoint();
        crate::core::events::report::notify_rejected_promises(scope);
    }
}

thread_local! {
    /// How many callbacks the engine invoked, and `dispatchEvent` calls, are running ([`ScriptBelow`]).
    static SCRIPT_BELOW: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Script is below whatever runs now: a callback the engine invoked (a listener, a timer's handler, a frame
/// callback, an observer's) or a `dispatchEvent` call (`EventTarget.prototype.dispatchEvent`, which only script or
/// such a callback can make) is running. A callback that returns into one returns to no empty stack, so its
/// microtasks wait: Blink runs each of them inside a `v8::MicrotasksScope` (`V8ScriptRunner::CallFunction`,
/// `RunCompiledScript`), and only the outermost one's end is a checkpoint. Chrome 152, measured (two listeners that
/// each queue a microtask): a bound `dispatchEvent` or `click` run as a timer's handler, a frame callback or another
/// event's listener runs both listeners and then both microtasks -- korvus ran each listener's microtask after it,
/// because the stack walk [`clean_up_after_running_callback`] makes otherwise finds no script frame under a native
/// function the engine called. Asked before that walk, it also spares it: one detailed stack trace per listener was
/// 4.4% of a `dispatchEvent` loop's main thread (frame-pointer profile).
pub struct ScriptBelow;

impl ScriptBelow {
    pub fn enter() -> Self {
        SCRIPT_BELOW.with(|d| d.set(d.get() + 1));
        ScriptBelow
    }
}

impl Drop for ScriptBelow {
    fn drop(&mut self) {
        SCRIPT_BELOW.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// How many algorithms Blink runs inside ONE outer `v8::MicrotasksScope` are running ([`hold_checkpoints`]).
#[derive(Default)]
struct CheckpointHold(u32);

/// Run `f` as Blink runs an algorithm inside one outer `MicrotasksScope`: the callbacks it calls do not end the
/// scope, so their microtasks wait for the algorithm's own checkpoint -- `NavigationApi::UpdateForNavigation`'s
/// `currententrychange` listeners, its intercept handlers and its `dispose` events run one after another, and the
/// `committed` promise's reactions after them (WPT navigation-api/ordering-and-transition).
pub(crate) fn hold_checkpoints<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut v8::PinScope) -> R) -> R {
    {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<CheckpointHold>().is_none() {
            st.put(CheckpointHold::default());
        }
        st.borrow_mut::<CheckpointHold>().0 += 1;
    }
    let r = f(scope);
    if let Some(h) = JsRuntime::op_state_from(scope).borrow_mut().try_borrow_mut::<CheckpointHold>() {
        h.0 = h.0.saturating_sub(1);
    }
    r
}

/// Run `f` as a task of its own, from Rust, in the page's realm, with no script on the stack and its microtask
/// checkpoint after it -- the shape the host's own load-task drain gives a step, on whatever drives the event loop.
pub(crate) fn queue_host_step(scope: &mut v8::PinScope, f: fn(&mut v8::PinScope)) {
    let spawner = JsRuntime::op_state_from(scope).borrow().try_borrow::<V8TaskSpawner>().cloned();
    let Some(spawner) = spawner else { return };
    spawner.spawn(move |scope| {
        v8::scope!(let hs, scope);
        let ctx = hs.get_current_context();
        let scope = &mut v8::ContextScope::new(hs, ctx);
        run_as_task(scope, f);
    });
}

/// Run `f` as ONE task of the event loop, from the loop, in the realm entered now: its microtask checkpoint and the rest
/// it leaves ([`NextTasks`]) are part of it -- a long animation frame and the page's busy time see one task
/// (`loaf::task_start`) -- and the loop's rendering opportunity follows it ([`AfterTaskHook`]).
pub fn run_as_task(scope: &mut v8::PinScope, f: impl FnOnce(&mut v8::PinScope)) {
    crate::core::timing::loaf::task_start(scope);
    f(scope);
    scope.perform_microtask_checkpoint();
    crate::core::events::report::notify_rejected_promises(scope);
    run_rest(scope);
    crate::core::timing::loaf::task_end(scope);
    let hook = JsRuntime::op_state_from(scope).borrow().try_borrow::<AfterTaskHook>().map(|h| h.0);
    if let Some(hook) = hook {
        hook(scope);
    }
}

/// The rest of the running task, for an algorithm Chrome runs in ONE task with a microtask checkpoint in its middle --
/// a traversal's Navigation API commit, then its `popstate` (Blink's `NavigationApi::UpdateForNavigation` runs the
/// microtasks between): run by the task's runner right after the task's checkpoint, before any other task.
#[derive(Default)]
pub struct NextTasks(std::collections::VecDeque<v8::Global<v8::Function>>);

/// Queue `f` as the rest of the running task ([`NextTasks`]); `false` with no event loop to run it on.
pub fn queue_next_task(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> bool {
    let op_state = JsRuntime::op_state_from(scope);
    let Some(spawner) = op_state.borrow().try_borrow::<V8TaskSpawner>().cloned() else { return false };
    let g = v8::Global::new(scope, f);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<NextTasks>().is_none() {
            st.put(NextTasks::default());
        }
        st.borrow_mut::<NextTasks>().0.push_back(g);
    }
    // queued outside any task (a host's script, a CDP evaluation): it is the next task of its own
    spawner.spawn(move |scope| {
        v8::scope!(let hs, scope);
        let ctx = hs.get_current_context();
        let scope = &mut v8::ContextScope::new(hs, ctx);
        run_as_task(scope, |_| {});
    });
    true
}

/// What the event loop does after every task, from the realm's task source (`korvus-runtime`'s scheduler): its
/// rendering opportunity -- a rendering frame that is due runs before the next task, as Chrome's compositor-driven
/// frame does (it outranks the page's ordinary tasks once due).
pub struct AfterTaskHook(pub fn(&mut v8::PinScope));

/// Run what the finished task left for its own rest ([`NextTasks`]), each with its microtask checkpoint.
fn run_rest(scope: &mut v8::PinScope) {
    loop {
        let next = {
            let op_state = JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            st.try_borrow_mut::<NextTasks>().and_then(|q| q.0.pop_front())
        };
        let Some(f) = next else { return };
        {
            v8::tc_scope!(let tc, scope);
            let f = v8::Local::new(tc, &f);
            let undef: v8::Local<v8::Value> = v8::undefined(tc).into();
            let _ = f.call(tc, undef, &[]);
            if let Some(e) = tc.exception().filter(|_| tc.has_caught() && !tc.has_terminated()) {
                let m = tc.message();
                let site = crate::core::events::report::caught_site(tc, e, m);
                tc.reset();
                crate::core::events::report::report_exception_at(tc, e, site);
            }
        }
        scope.perform_microtask_checkpoint();
        crate::core::events::report::notify_rejected_promises(scope);
    }
}

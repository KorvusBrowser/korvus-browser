//! `core/html/scheduler.rs` — `Scheduler` and the `scheduler` singleton.
//!
//! FP5 deferred this name with one sentence: **`postTask` must RUN the task and resolve with its
//! value.** A `postTask` that answers the D2 stub's `undefined` hands a page something that is not a
//! promise and every `await scheduler.postTask(...)` on it becomes a silent no-op, so the name was
//! correctly absent until there was a queue behind it. This is that queue.
//!
//! # WHAT THE DEVICE ANSWERS — Chrome 150.0.7871.46 and Brave 151.1.93.138, headed over CDP under
//! Xvfb, two origins, 2026-09-19/20. Identical, reading for reading.
//!
//! ```text
//! scheduler                    own accessor, `get scheduler` AND `set scheduler`, e=1 c=1
//!                              ([Replaceable]); one object per realm
//! postTask(cb)                 a Promise that resolves with cb's return; a thenable is FLATTENED
//! the callback                 gets NO arguments (measured: `(...a) => a.length` is 0) and an
//!                              UNDEFINED receiver, so a sloppy callback's `this` reads as the global
//! a callback that throws       the promise rejects with what it threw, unchanged
//! priority                     'user-blocking' < 'user-visible' (the default) < 'background':
//!                              posting bg, ub, uv runs ub, uv, bg
//! a task vs a microtask        the microtask first ("micro,task")
//! a task vs setTimeout(…, 0)   the setTimeout first when it was registered first ("to,pt")
//! postTask inside a task       the inner task runs AFTER the outer one finishes ("outer,inner")
//! delay                        `{delay: 10}` posted BEFORE `{delay: 0}` runs after it
//! yield()                      a Promise. It is a CONTINUATION, not a task: it resumes BEFORE any
//!                              pending task, inside a task ("task-start,after-yield,uv,bg") and
//!                              outside one ("yielded,uv") alike
//! signal                       an aborted AbortSignal rejects the promise with the signal's reason,
//!                              or `AbortError: signal is aborted without reason`; aborting INSIDE
//!                              the running callback still rejects; two tasks on one signal both do
//!
//! every failure is a REJECTION, never a throw -- these are Promise-returning operations:
//!   no callback         TypeError "1 argument required, but only 0 present."
//!   a non-function      TypeError "parameter 1 is not of type 'Function'."
//!   options not object  TypeError "The provided value is not of type 'SchedulerPostTaskOptions'."
//!                       (`null` is fine, `5` is not)
//!   a bad priority      TypeError "Failed to read the 'priority' property from
//!                       'SchedulerPostTaskOptions': The provided value 'bogus' is not a valid enum
//!                       value of type TaskPriority."
//!   a negative delay    TypeError "Failed to read the 'delay' property from
//!                       'SchedulerPostTaskOptions': Value is outside the 'unsigned long long'
//!                       value range."  (so it is [EnforceRange]; '5' and 1.9 are both fine)
//!   a foreign receiver  TypeError "Failed to execute 'postTask' on 'Scheduler': Illegal invocation"
//! ```
//!
//! # HOW THE QUEUE IS BUILT, and why it is not a second event loop
//!
//! One realm-private queue plus ONE pump, and the pump is scheduled through the realm's own
//! `setTimeout` — the same hook `core/dom/abort_controller.rs` and `core/html/animation_frame.rs`
//! already reach the macrotask queue through. So a scheduler task IS a task of this engine's event
//! loop, which is what makes every ordering above come out right without any of them being coded
//! for: the microtask checkpoint happens before the pump because the pump is a macrotask, and an
//! earlier `setTimeout(…, 0)` runs first because it was queued first.
//!
//! **One task per pump**, re-scheduling while work remains, rather than draining the queue in one
//! go: that is what puts a task boundary between two scheduler tasks, and it is what
//! `postTask inside a task` measures.
//!
//! **Continuations before tasks.** `yield()` parks a resolver in a separate list that the pump
//! empties first, which is the whole of the measured difference between a yield and a
//! `postTask(…, {priority: 'user-blocking'})`.
//!
//! **Abort is read at the task's turn**, not through a listener on the signal. Every measured row
//! comes out the same — a task whose signal is aborted never runs and its promise rejects — and the
//! one thing nobody measured is WHEN the rejection is delivered: here it is when the task's turn
//! comes rather than at the `abort()` call. Named rather than assumed.
//!
//! # Exposure
//!
//! **Unconditional**, measured (FP5.7/FP5.9): `scheduler` is an own property of `window` on
//! `http://web-platform.test:<p>/` — `isSecureContext` false — on all four browsers, and so is the
//! `Scheduler` interface object. Both are absent from `tools/sync/data/secure_context.json`, and
//! that file's silence IS the reading.
//!
//! # Realms
//!
//! `Scheduler` is `[Exposed=(Window,Worker)]` and every `worker.surface.json` lists it with the
//! window's two members, so the table is installed in BOTH realms (invariant I8). The `scheduler`
//! SINGLETON is window-only: no worker capture records it as an own global, because there it is a
//! `WorkerGlobalScope` prototype attribute — a row this lane does not own.
//!
//! # WHAT IS NOT HERE, and it is reported rather than faked
//!
//! `TaskController`, `TaskSignal` and `TaskPriorityChangeEvent` are still the generated registry's
//! stubs: `new TaskController()` is `Illegal constructor` here where the device constructs one. That
//! is a PRE-EXISTING absence this lane did not widen — `postTask` takes a plain `AbortSignal`, which
//! is what the measured abort rows use — and it is the residual named in FP5.5's report.

use std::cell::{Cell, RefCell};

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, make_dom_exc};
use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::branded;

/// The three real priorities, best first. The order IS the enum's order, and the index is the sort
/// key, so "user-blocking runs before background" is not a separate rule anywhere below.
const PRIORITIES: [&str; 3] = ["user-blocking", "user-visible", "background"];
/// `SchedulerPostTaskOptions.priority` defaults to `user-visible` (measured: an option-less task runs
/// between a `user-blocking` and a `background` one).
const DEFAULT_PRIORITY: u8 = 1;

struct Task {
    id: u64,
    priority: u8,
    ready: bool,
    callback: v8::Global<v8::Function>,
    resolver: v8::Global<v8::PromiseResolver>,
    signal: Option<v8::Global<v8::Object>>,
}

/// This realm's scheduler queue. Private to the realm — a page can reach it only through the two
/// rows below.
#[derive(Default)]
pub(crate) struct SchedulerStore {
    next_id: Cell<u64>,
    tasks: RefCell<Vec<Task>>,
    /// `yield()`'s parked resolvers, in order. Emptied BEFORE any task.
    continuations: RefCell<Vec<v8::Global<v8::PromiseResolver>>>,
    pump_scheduled: Cell<bool>,
}

fn with_store<R>(scope: &mut v8::PinScope, f: impl FnOnce(&SchedulerStore) -> R) -> R {
    let rc = deno_core::JsRuntime::op_state_from(scope);
    if rc.borrow().try_borrow::<SchedulerStore>().is_none() {
        rc.borrow_mut().put(SchedulerStore::default());
    }
    let st = rc.borrow();
    f(st.borrow::<SchedulerStore>())
}

// ── the pump ────────────────────────────────────────────────────────────────────────────────────

/// Is there anything for a pump to do?
fn has_work(scope: &mut v8::PinScope) -> bool {
    with_store(scope, |s| !s.continuations.borrow().is_empty() || s.tasks.borrow().iter().any(|t| t.ready))
}

/// Put ONE pump on the realm's macrotask queue, unless one is already there.
///
/// Through the realm's own `setTimeout`, which is how `core/dom/abort_controller.rs` and
/// `core/html/animation_frame.rs` reach the same queue: a scheduler task is a task of THIS event
/// loop, not of a second one, and that is what makes the microtask and `setTimeout` orderings above
/// come out right without being coded for.
fn schedule_pump(scope: &mut v8::PinScope) {
    if with_store(scope, |s| s.pump_scheduled.replace(true)) {
        return;
    }
    let f = build_fn(scope, cb_pump, "", 0);
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[f.into(), zero]);
}

/// Has this task's signal been aborted? `None` for a task with no signal.
fn aborted(scope: &mut v8::PinScope, signal: &Option<v8::Global<v8::Object>>) -> bool {
    let Some(g) = signal else { return false };
    let s = v8::Local::new(scope, g);
    get_prop(scope, s, "aborted").map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// Reject a task's promise the way an aborted signal does: with the signal's own `reason`, which the
/// platform fills with `AbortError: signal is aborted without reason` when `abort()` is given none.
fn reject_aborted(scope: &mut v8::PinScope, resolver: v8::Local<v8::PromiseResolver>, signal: &Option<v8::Global<v8::Object>>) {
    let reason = signal
        .as_ref()
        .map(|g| v8::Local::new(scope, g))
        .and_then(|s| get_prop(scope, s, "reason"))
        .filter(|v| !v.is_undefined());
    let e = match reason {
        Some(r) => r,
        None => match make_dom_exc(scope, "signal is aborted without reason", "AbortError") {
            Some(e) => e,
            None => return,
        },
    };
    resolver.reject(scope, e);
}

/// The index of the READY task that runs next: best priority, then oldest.
fn next_task_index(tasks: &[Task]) -> Option<usize> {
    tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.ready)
        .min_by_key(|(_, t)| (t.priority, t.id))
        .map(|(i, _)| i)
}

/// ONE unit of scheduler work, then re-schedule if any is left.
fn cb_pump(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    with_store(scope, |s| s.pump_scheduled.set(false));

    // Continuations first: that is the whole of what makes `yield()` a continuation rather than a
    // user-blocking task.
    let cont = with_store(scope, |s| {
        let mut c = s.continuations.borrow_mut();
        if c.is_empty() {
            None
        } else {
            Some(c.remove(0))
        }
    });
    if let Some(g) = cont {
        let r = v8::Local::new(scope, &g);
        let u = v8::undefined(scope);
        r.resolve(scope, u.into());
        if has_work(scope) {
            schedule_pump(scope);
        }
        return;
    }

    let task = with_store(scope, |s| {
        let mut t = s.tasks.borrow_mut();
        next_task_index(&t).map(|i| t.remove(i))
    });
    let Some(task) = task else { return };
    let resolver = v8::Local::new(scope, &task.resolver);

    if aborted(scope, &task.signal) {
        reject_aborted(scope, resolver, &task.signal);
    } else {
        let cb = v8::Local::new(scope, &task.callback);
        // NO arguments and an UNDEFINED receiver, both measured.
        let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
        let thrown = {
            v8::tc_scope!(let tc, scope);
            match cb.call(tc, recv, &[]) {
                Some(out) => Ok(v8::Global::new(tc, out)),
                None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
            }
        };
        match thrown {
            Err(e) => {
                let e = match e {
                    Some(g) => v8::Local::new(scope, &g),
                    None => v8::undefined(scope).into(),
                };
                resolver.reject(scope, e);
            }
            // A callback that aborts its OWN signal still rejects (measured), so the check is after
            // the call as well as before it.
            Ok(_) if aborted(scope, &task.signal) => reject_aborted(scope, resolver, &task.signal),
            Ok(g) => {
                let out = v8::Local::new(scope, &g);
                // A thenable return is FLATTENED, which `resolve` does for free.
                resolver.resolve(scope, out);
            }
        }
    }
    if has_work(scope) {
        schedule_pump(scope);
    }
}

/// A delayed task's `setTimeout` callback: make it ready and wake the pump. `data` is the task id.
fn cb_release(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(-1.0) as u64;
    with_store(scope, |s| {
        if let Some(t) = s.tasks.borrow_mut().iter_mut().find(|t| t.id == id) {
            t.ready = true;
        }
    });
    if has_work(scope) {
        schedule_pump(scope);
    }
}

// ── the two rows ────────────────────────────────────────────────────────────────────────────────

/// Blink's context applied after the error is made (`deno_core::error::native_error`).
fn reject_type_error(scope: &mut v8::PinScope, r: v8::Local<v8::PromiseResolver>, msg: &str) {
    let e = deno_core::error::native_error(scope, "TypeError", msg);
    r.reject(scope, e);
}


/// `postTask(callback, options?)`.
fn cb_post_task<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s>) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    rv.set(resolver.get_promise(scope).into());
    if !branded(scope, &args) {
        reject_type_error(scope, resolver, "Failed to execute 'postTask' on 'Scheduler': Illegal invocation");
        return;
    }
    if args.length() < 1 {
        reject_type_error(
            scope,
            resolver,
            "Failed to execute 'postTask' on 'Scheduler': 1 argument required, but only 0 present.",
        );
        return;
    }
    let Ok(callback) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        reject_type_error(
            scope,
            resolver,
            "Failed to execute 'postTask' on 'Scheduler': parameter 1 is not of type 'Function'.",
        );
        return;
    };

    // `SchedulerPostTaskOptions`, converted as WebIDL converts it (Chrome 150): absent,
    // `undefined` and `null` are the empty dictionary, anything else that is not an object is refused (measured:
    // `null` is fine, `5` is not); its members are read once each in WebIDL's order -- `delay`
    // ([EnforceRange] unsigned long long), `priority` (an enum), `signal` (an AbortSignal) -- and a refusal REJECTS
    // the promise. korvus read `priority` first.
    let options = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(1), "SchedulerPostTaskOptions", "Failed to execute 'postTask' on 'Scheduler': ") {
        Ok(o) => Some(o),
        Err(e) => {
            resolver.reject(scope, e);
            return;
        }
    };

    // the members as converted: `priority` a TaskPriority, `delay` in range, `signal` an AbortSignal
    let mut priority = DEFAULT_PRIORITY;
    let mut delay: u64 = 0;
    let mut signal: Option<v8::Global<v8::Object>> = None;
    if let Some(o) = options {
        if let Some(v) = get_prop(scope, o, "priority").filter(|v| v.is_string()) {
            let want = v.to_rust_string_lossy(scope);
            if let Some(i) = PRIORITIES.iter().position(|p| *p == want) {
                priority = i as u8;
            }
        }
        if let Some(n) = get_prop(scope, o, "delay").filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) {
            delay = n as u64;
        }
        if let Some(v) = get_prop(scope, o, "signal").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            signal = Some(v8::Global::new(scope, v));
        }
    }

    let id = with_store(scope, |s| {
        let id = s.next_id.get();
        s.next_id.set(id + 1);
        id
    });
    let task = Task {
        id,
        priority,
        ready: delay == 0,
        callback: v8::Global::new(scope, callback),
        resolver: v8::Global::new(scope, resolver),
        signal,
    };
    with_store(scope, |s| s.tasks.borrow_mut().push(task));
    if delay == 0 {
        schedule_pump(scope);
        return;
    }
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    let f = build_fn_data(scope, cb_release, "", 0, idv);
    let global = scope.get_current_context().global(scope);
    let gv: v8::Local<v8::Value> = global.into();
    let d: v8::Local<v8::Value> = v8::Number::new(scope, delay as f64).into();
    crate::binding::protos::engine_call(scope, gv, "setTimeout", &[f.into(), d]);
}

/// `yield()` — a CONTINUATION. Measured: it resumes before every pending task, inside a task and
/// outside one alike, which is exactly what the pump's continuation list expresses.
fn cb_yield<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s>) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    rv.set(resolver.get_promise(scope).into());
    if !branded(scope, &args) {
        reject_type_error(scope, resolver, "Failed to execute 'yield' on 'Scheduler': Illegal invocation");
        return;
    }
    let g = v8::Global::new(scope, resolver);
    with_store(scope, |s| s.continuations.borrow_mut().push(g));
    schedule_pump(scope);
}

/// Mount one `Scheduler` row. Both are promise-returning, so both do their own receiver check and
/// both must SAY so: `install::receiver`'s derived pass would otherwise wrap them with a guard that
/// THROWS where the device rejects (the defect `install::webidl`'s promise rows record from W9.17).
pub fn install_scheduler(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let f = match member.name {
        "postTask" => crate::install::kits::proto_data_fn(scope, proto, cb_post_task, member.name, member.length as i32),
        "yield" => crate::install::kits::proto_data_fn(scope, proto, cb_yield, member.name, member.length as i32),
        other => {
            debug_assert!(false, "install_scheduler: unexpected member {other}");
            return;
        }
    };
    crate::binding::protos::stamp_receiver_guarded(scope, f);
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}

/// The interface from its table. Realm-agnostic: every `worker.surface.json` lists `Scheduler` with
/// the window's two members, and `create_interface` publishes it only if the ACTIVE capture records
/// it (`Publish::IfCaptured`, through `exposure::gate`).
fn install_interface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    let built =
        installer::create_interface(scope, global, &SCHEDULER, SCHEDULER.name, None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] scheduler install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

/// The interface and the `scheduler` singleton, in the WINDOW.
pub fn install_window_scheduler<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    install_interface(scope, global);
    if !gate(scope, Publishes::Global, "scheduler", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_shell(scope, "Scheduler") else { return };
    // `get scheduler` AND `set scheduler`, e=1 c=1 on all four browsers — `[Replaceable]`, which is
    // where it differs from `trustedTypes` and `crashReport` next door.
    define_window_singleton(scope, global, "scheduler", o.into(), WindowSetter::Replaceable, true, Mint::Shell("Scheduler"));
}

/// The SAME table in the WORKER realm (invariant I8), called from `core/korvus-runtime/src/worker.rs`.
/// No singleton: no `worker.surface.json` records `scheduler` as an own global, because there it is a
/// `WorkerGlobalScope` prototype attribute. Without this call the worker would lose the `Scheduler`
/// interface outright, because the table takes it out of the registry in BOTH realms.
/// The realm's one `Scheduler`, minted on first use -- a worker's `WorkerGlobalScope.prototype
/// .scheduler` getter answers it (that row was a stub answering `null`, so
/// `scheduler.postTask` threw in every worker; Brave has it).
pub(crate) fn worker_scheduler_singleton<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    if let Some(v) = crate::core::css::css_style_declaration::priv_get(scope, global, "korvus_worker_scheduler") {
        return v8::Local::<v8::Object>::try_from(v).ok();
    }
    let proto = crate::binding::protos::shell_proto(scope, "Scheduler")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::core::dom::element::priv_set(scope, global, "korvus_worker_scheduler", o.into());
    Some(o)
}

pub fn install_worker_scheduler<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_interface(scope, global);
}

// The table. All four captures record `postTask` (length 1) and `yield` (length 0) in that order,
// both enumerable, configurable and writable, with `ctorIndex` 2. `new Scheduler()` is
// `Illegal constructor` on all four, which `Model::Interface` plus a `stub` ctor row spells.
webapi::interface! {
    "Scheduler" [interface, both] {
    ctor   "Scheduler"                 len 0                   = stub,
    method "postTask" : Promise len 1 required 1               = install_scheduler,
    method "yield"    : Promise len 0 required 0               = install_scheduler
    }
}

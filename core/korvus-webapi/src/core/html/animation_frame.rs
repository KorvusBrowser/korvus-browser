//! `core/html/animation_frame.rs` -- `requestAnimationFrame` / `cancelAnimationFrame` /
//! `requestIdleCallback` / `cancelIdleCallback`.
//!
//! WHAT THIS REPLACES: the `nat(...)` block of `core/korvus-runtime/src/js/host.js` -- four
//! `Object.defineProperty(globalThis, …)` calls through one helper, each behind an `op_nativize`
//! mask, over a JS `Map` of pending callbacks and a `seq` counter. Four rows of the `Window` table
//! now (invariants I2, I3, I9).
//!
//! # The shape is the JS's; two things were not Chrome's
//!
//! Everything the JS had right is kept. Both `request*` take a REQUIRED callback, so a bare call is a
//! WebIDL arity TypeError; both `cancel*` take a REQUIRED handle, so `cancelAnimationFrame()` throws
//! while `cancelAnimationFrame(12345)` on an unknown handle is a SILENT NO-OP -- which is what the
//! recordings show, what Chrome answers (measured), and what the pair used to have backwards: a
//! no-op cancel that accepted anything, and a constant `0` handle that meant a page could never
//! cancel a specific frame. The handles are real: each request gets its own id and a cancel drops
//! that callback and no other.
//!
//! WHAT WAS WRONG, measured on `google-chrome --headless=new` 150.0.7871.46 over a `file:` URL:
//!
//!   * a non-callable first argument threw "Failed to execute 'requestAnimationFrame' on 'Window':
//!     The callback provided as parameter 1 is not a function." Chrome's is
//!     "…: parameter 1 is not of type 'Function'." -- the WebIDL conversion sentence rather than a
//!     hand-written one. Same for `requestIdleCallback`, and `null` takes the same path as `1`.
//!   * all four answered for a FOREIGN RECEIVER. Chrome throws `Illegal invocation` from every one,
//!     which the rows' `guarded` wrapper supplies; the JS bodies ignored `this`.
//!
//! There is no vsync here, so a frame is a TIMER: the timing is a stub, the bookkeeping is not. The
//! `requestAnimationFrame` callback's argument is the frame timestamp, which is `performance.now()`
//! -- a real clock, not 0 -- and `requestIdleCallback`'s is the `{didTimeout, timeRemaining}` shape,
//! with an honest 0 remaining because there is no idle period to measure.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, int_val, throw_type_error};

/// Which of the four a row is, bound as its body's `data`.
const RAF: f64 = 0.0;
const CAF: f64 = 1.0;
const RIC: f64 = 2.0;
const CIC: f64 = 3.0;

fn kind_name(k: f64) -> &'static str {
    match k {
        RAF => "requestAnimationFrame",
        CAF => "cancelAnimationFrame",
        RIC => "requestIdleCallback",
        _ => "cancelIdleCallback",
    }
}

/// The pending frame and idle callbacks of every realm.
///
/// THE HANDLES ARE EACH DOCUMENT'S, EACH KIND'S (a bot-detection service D6). Blink numbers a document's
/// animation-frame callbacks (`FrameRequestCallbackCollection::next_callback_id_`) and its idle callbacks
/// (`ScriptedIdleTaskController::next_callback_id_`) apart, each from 1. korvus had one counter for both kinds and
/// every frame: a page's first `requestIdleCallback` after a `requestAnimationFrame` answered 2, and a frame's
/// first `requestAnimationFrame` counted on from its parent's. The runners take a callback by the engine's own key.
#[derive(Default)]
pub(crate) struct FrameCallbacks {
    /// the engine's key for the next callback, unique across realms and kinds
    next_key: u32,
    /// the handle each (realm, is_idle) answers next
    next_handle: Vec<((Option<u32>, bool), u32)>,
    pending: Vec<Pending>,
    /// When the current idle period ends, on the realm's clock (0: none has begun). The idle callbacks that run in
    /// one period share its deadline, and one requested during it waits for the next (Blink's idle queue).
    idle_end: f64,
}

/// One requested callback. The kind rides along so the runner knows which ARGUMENT to build.
pub(crate) struct Pending {
    key: u32,
    handle: u32,
    /// the realm that asked (`browsing_context::realm_ctx`; `None` for the page's): its handles are its own
    realm: Option<u32>,
    idle: bool,
    cb: v8::Global<v8::Function>,
}

fn with_frames<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut FrameCallbacks) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<FrameCallbacks>().is_none() {
        st.put(FrameCallbacks::default());
    }
    f(st.borrow_mut::<FrameCallbacks>())
}

/// `requestAnimationFrame(cb)` / `requestIdleCallback(cb)`: schedule, and answer the handle.
///
/// The frame is a task on the realm's timer task source, queued through the ENGINE's `setTimeout`
/// (`rendering::engine_task`), never the page's: a page that replaces `setTimeout` does not cancel its
/// own frames. This used to call `window.setTimeout` by name, on the theory that a harness stubbing
/// timers expects rAF to follow -- Chrome 150 says otherwise (`H-engine-tasks-stub`: with `setTimeout`
/// a stub that never calls back, rAF and rIC still run), and instagram.com's MessageChannel-built
/// timers deadlocked it.
fn request(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, kind: f64) -> Option<u32> {
    let name = kind_name(kind);
    // NO ARITY CHECK HERE: each row carries `required 1` and the installer's guard mounts it, with
    // the same sentence. The JS enforced it by hand (`need(name)`) because there was no row.
    // WebIDL `FrameRequestCallback callback` / `IdleRequestCallback callback`: a non-callable is a
    // TypeError at the CONVERSION, before anything is scheduled. Accepting one and silently never
    // running it -- which is what `requestAnimationFrame(1)` used to do, handle and all -- tells a
    // page its callback was queued when nothing was.
    let Ok(cb) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        throw_type_error(scope, &format!("Failed to execute '{name}' on 'Window': parameter 1 is not of type 'Function'."));
        return None;
    };
    let g = v8::Global::new(scope, cb);
    let idle = kind == RIC;
    let realm = crate::core::html::browsing_context::realm_ctx(scope);
    let (id, handle) = with_frames(scope, |f| {
        f.next_key += 1;
        let key = f.next_key;
        let handle = match f.next_handle.iter_mut().find(|(k, _)| *k == (realm, idle)) {
            Some((_, n)) => {
                *n = n.wrapping_add(1).max(1);
                *n
            }
            None => {
                f.next_handle.push(((realm, idle), 1));
                1
            }
        };
        f.pending.push(Pending { key, handle, realm, idle, cb: g });
        (key, handle)
    });
    if idle {
        // rIC is an IDLE task, NOT part of the animation frame: it runs in an idle period, which the
        // task source begins only when no other task is ready and no frame is expected (the runtime's
        // `Scheduler::idle`) -- and, requested during an idle period, from an idle callback, not before that period has
        // ended. Blink defers such a task to the next period, so a 1 ms `timeout` set inside a 30 ms callback runs it
        // first (Chrome 150: `didTimeout` true).
        let now = crate::core::timing::performance::clock_now_ms(scope);
        let end = with_frames(scope, |f| f.idle_end);
        let idv = int_val(scope, id as i64);
        let runner = build_fn_data(scope, cb_run_frame, "", 1, idv);
        crate::core::intersection_observer::rendering::queue_idle(scope, runner, (end - now).max(0.0));
        // `IdleRequestOptions.timeout` (an `unsigned long`; 0 is none): the callback runs then if no idle period
        // came first, with `didTimeout` and no time left (Blink `ScriptedIdleTaskController`). It was ignored.
        let timeout = v8::Local::<v8::Object>::try_from(args.get(1))
            .ok()
            .and_then(|o| crate::binding::protos::get_prop(scope, o, "timeout"))
            .filter(|v| !v.is_undefined())
            .and_then(|v| v.number_value(scope))
            .filter(|n| n.is_finite())
            .map(|n| (n.trunc().rem_euclid(4_294_967_296.0)) as u32)
            .unwrap_or(0);
        if timeout > 0 {
            let idv = int_val(scope, id as i64);
            let runner = build_fn_data(scope, cb_idle_timeout, "", 0, idv);
            crate::core::intersection_observer::rendering::engine_task(scope, runner, f64::from(timeout));
        }
    } else {
        // rAF no longer gets a private 16 ms timer — it runs in the one "update the
        // rendering" frame the observer loop already drives ([`rendering::cb_frame`], HTML's "run the
        // animation frame callbacks" step). `request_frame` is idempotent, so every rAF requested before
        // the frame shares it and the whole set runs in ONE batch at the frame's cadence — one callback
        // batch per produced frame, as a browser runs rAF at vsync before paint.
        crate::core::intersection_observer::rendering::request_frame(scope);
    }
    Some(handle)
}

/// The scheduled runner: TAKE the callback if it is still pending, then call it.
///
/// Take-then-call is the JS's order and it matters twice: a callback that requests another frame must
/// not be able to see its own entry, and one cancelled between the schedule and the timer must not
/// run at all.
fn cb_run_frame(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).integer_value(scope) else { return };
    let Some((cb, idle)) = take(scope, id) else { return };
    let arg = if idle {
        // the idle period it runs in: the current one while it lasts, else the one the task source began for it (its
        // end, on the process clock, is the argument: until the next pending task, at most 50 ms) -- cut at the next
        // vsync when a frame has just run (Blink's short idle period: Chrome 150 hands an idle callback after a frame
        // 5-16 ms)
        let now = crate::core::timing::performance::clock_now_ms(scope);
        let end = with_frames(scope, |f| f.idle_end);
        let end = if now < end {
            end
        } else {
            let given = args.get(0).number_value(scope).filter(|n| n.is_finite()).map(|p| crate::core::timing::performance::realm_ms(scope, p));
            let mut end = given.unwrap_or_else(|| now + MAX_IDLE_MS).min(now + MAX_IDLE_MS);
            let after_frame = crate::core::intersection_observer::rendering::last_frame_at(scope)
                .is_some_and(|at| now - at < crate::core::intersection_observer::rendering::frame_interval_ms(scope));
            if after_frame || has_pending_frames(scope) {
                end = end.min(now + crate::core::intersection_observer::rendering::next_frame_ms(scope));
            }
            with_frames(scope, |f| f.idle_end = end);
            end
        };
        idle_deadline(scope, false, end)
    } else {
        frame_timestamp(scope)
    };
    invoke(scope, cb, idle, arg);
}

/// An idle callback's `timeout` ran out before an idle period ran it: it runs now, `didTimeout`, no time left.
fn cb_idle_timeout(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).integer_value(scope) else { return };
    let Some((cb, _)) = take(scope, id) else { return };
    let now = crate::core::timing::performance::clock_now_ms(scope);
    let arg = idle_deadline(scope, true, now);
    invoke(scope, cb, true, arg);
}

/// Take callback `id` if it is still pending.
fn take(scope: &mut v8::PinScope, id: i64) -> Option<(v8::Global<v8::Function>, bool)> {
    with_frames(scope, |f| {
        let i = f.pending.iter().position(|p| p.key as i64 == id)?;
        let p = f.pending.remove(i);
        Some((p.cb, p.idle))
    })
}

/// Call a taken frame or idle callback with its argument.
fn invoke(scope: &mut v8::PinScope, cb: v8::Global<v8::Function>, idle: bool, arg: v8::Local<v8::Value>) {
    // A FRAME IS A RENDERING OPPORTUNITY, and it is the only one this engine has that a
    // page can cause. The animation timeline is sampled once per opportunity and held for the whole
    // task, so this is where it moves -- an idle callback is not a frame and does not move it.
    if !idle {
        crate::core::animation::advance_frame_time(scope);
    }
    let cb = v8::Local::new(scope, &cb);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    // A throwing callback is REPORTED, as the JS's `catch (e) { KB.__recordUncaught(e) }` was. A
    // frame's callback runs as the frame's realm.
    crate::core::html::browsing_context::as_callback_realm(scope, cb, |scope| {
        v8::tc_scope!(let tc, scope);
        let below = crate::core::html::task_queue::ScriptBelow::enter();
        #[cfg(feature = "analysis")]
        crate::analysis_hook::call_traced(tc, if idle { "IdleRequestCallback" } else { "FrameRequestCallback" }, cb, undef, &[arg]);
        #[cfg(not(feature = "analysis"))]
        cb.call(tc, undef, &[arg]);
        drop(below);
        if tc.has_caught() {
            match tc.exception() {
                Some(err) => {
                    // reported where it was thrown (`report::caught_site`)
                    let m = tc.message();
                    let site = crate::core::events::report::caught_site(tc, err, m);
                    tc.reset();
                    crate::core::events::report::report_exception_at(tc, err, site);
                }
                None => tc.reset(),
            }
        }
    });
    // its microtasks before anything else runs (HTML "clean up after running a callback")
    crate::core::html::task_queue::clean_up_after_running_callback(scope);
}

/// Run every pending ANIMATION-frame (rAF) callback once — the "run the animation frame
/// callbacks" step of HTML's "update the rendering", called from the one frame the observer loop drives
/// ([`rendering::cb_frame`]). Idle (rIC) callbacks are not frames and are left alone. The rAF set is
/// SNAPSHOTTED first, so a callback that requests another frame is deferred to the next one
/// (take-then-call, exactly as the single-callback [`cb_run_frame`] does). The animation timeline is
/// advanced ONCE, before the batch, so every callback in this frame reads the same timestamp.
/// Returns whether any rAF callback ran.
pub(crate) fn run_frame_callbacks(scope: &mut v8::PinScope) -> bool {
    let ids: Vec<u32> =
        with_frames(scope, |f| f.pending.iter().filter(|p| !p.idle).map(|p| p.key).collect());
    if ids.is_empty() {
        return false;
    }
    crate::core::animation::advance_frame_time(scope);
    let arg = frame_timestamp(scope);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    for id in ids {
        let taken = with_frames(scope, |f| {
            let i = f.pending.iter().position(|p| p.key == id && !p.idle)?;
            Some(f.pending.remove(i).cb)
        });
        let Some(cb) = taken else { continue };
        let cb = v8::Local::new(scope, &cb);
        // A frame's callback runs as the frame's realm; a throw is REPORTED, as cb_run_frame.
        crate::core::html::browsing_context::as_callback_realm(scope, cb, |scope| {
            v8::tc_scope!(let tc, scope);
            let below = crate::core::html::task_queue::ScriptBelow::enter();
            #[cfg(feature = "analysis")]
            crate::analysis_hook::call_traced(tc, "FrameRequestCallback", cb, undef, &[arg]);
            #[cfg(not(feature = "analysis"))]
            cb.call(tc, undef, &[arg]);
            drop(below);
            if tc.has_caught() {
                match tc.exception() {
                    Some(err) => {
                        // reported where it was thrown (`report::caught_site`)
                        let m = tc.message();
                        let site = crate::core::events::report::caught_site(tc, err, m);
                        tc.reset();
                        crate::core::events::report::report_exception_at(tc, err, site);
                    }
                    None => tc.reset(),
                }
            }
        });
        // its microtasks before the next callback (HTML "clean up after running a callback"; Chrome 150, measured)
        crate::core::html::task_queue::clean_up_after_running_callback(scope);
    }
    true
}

/// Whether any ANIMATION-frame (rAF) callback is still pending — the frame loop re-arms while true,
/// and the compositor paints while true (a rAF animation is producing frames).
pub fn has_pending_frames(scope: &mut v8::PinScope) -> bool {
    with_frames(scope, |f| f.pending.iter().any(|p| !p.idle))
}

/// The frame timestamp: `performance.now()`, the realm's real clock.
fn frame_timestamp<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let now = crate::core::timing::performance::clock_now_ms(scope);
    v8::Number::new(scope, now).into()
}

/// The longest idle period (Blink `IdleHelper::kMaximumIdlePeriodDuration`).
const MAX_IDLE_MS: f64 = 50.0;

fn idle_slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus.idle.{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// The `IdleDeadline` an idle callback is called with (Blink `idle_deadline.cc`): an instance of the interface --
/// `didTimeout` a getter and `timeRemaining()` a method of its prototype, no own property -- over `deadline` on the
/// realm's clock. It was `{didTimeout: false, timeRemaining: () => 0}`, an `[object Object]` with two own properties
/// whose time was always gone (Chrome 150: `[object IdleDeadline]`, up to 50 ms left in an idle period).
fn idle_deadline<'s>(scope: &mut v8::PinScope<'s, '_>, did_timeout: bool, deadline: f64) -> v8::Local<'s, v8::Value> {
    let Some(proto) = crate::binding::protos::shell_proto(scope, "IdleDeadline") else { return v8::undefined(scope).into() };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    let (k, d) = (idle_slot(scope, "deadline"), v8::Number::new(scope, deadline));
    o.set_private(scope, k, d.into());
    let (k, t) = (idle_slot(scope, "didTimeout"), v8::Boolean::new(scope, did_timeout));
    o.set_private(scope, k, t.into());
    o.into()
}

/// The body `IdleDeadline`'s two registry rows name (`tools/sync/gen_shells.py` claims them).
pub fn install_idle_deadline(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "didTimeout" => crate::install::live::getter_row(scope, proto, member, cb_idle_did_timeout),
        "timeRemaining" => crate::install::live::method_row(scope, proto, member, cb_idle_time_remaining),
        other => debug_assert!(false, "install_idle_deadline: unexpected member {other}"),
    }
}

/// The receiver's deadline, or `Illegal invocation` for an object that is not an `IdleDeadline` this engine made.
fn deadline_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<(f64, bool)> {
    let o = args.this();
    let k = idle_slot(scope, "deadline");
    let d = o.get_private(scope, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope));
    let Some(d) = d else {
        crate::binding::protos::illegal_invocation(scope);
        return None;
    };
    let k = idle_slot(scope, "didTimeout");
    let t = o.get_private(scope, k).is_some_and(|v| v.is_true());
    Some((d, t))
}

fn cb_idle_did_timeout(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((_, t)) = deadline_of(scope, &args) else { return };
    rv.set_bool(t);
}

/// `timeRemaining()`: the time to the deadline, 0 once it has passed, clamped as the clock is (Blink:
/// `Performance::ClampTimeResolution` of the remaining time -- 100 µs, 5 µs when cross-origin isolated).
fn cb_idle_time_remaining(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((deadline, _)) = deadline_of(scope, &args) else { return };
    let now = crate::core::timing::performance::clock_now_ms(scope);
    let us = ((deadline - now) * 1000.0).round() as i64;
    if us <= 0 {
        rv.set_double(0.0);
        return;
    }
    // Blink's clamper, as every timestamp's: the tick or the next, unbiased -- it was a floor
    let isolated = crate::core::timing::performance::realm_clock_isolated(scope);
    rv.set_double(spec::clamp_span_ms(us, isolated));
}

/// `cancelAnimationFrame(handle)` / `cancelIdleCallback(handle)`.
///
/// The handle is REQUIRED (`cancelAnimationFrame(unsigned long handle)`), so a bare call throws; an
/// unknown handle is a silent no-op, which is what the recordings show and what Chrome answers
/// (`cancelAnimationFrame(12345)` is `undefined`, and so is `cancelAnimationFrame("x")`).
fn cancel(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, kind: f64) {
    let _ = kind_name(kind);
    // The handle is REQUIRED and the row's guard says so; see `request`.
    let Some(h) = args.get(0).number_value(scope) else { return };
    if !h.is_finite() {
        return;
    }
    let id = h as i64;
    // this realm's callback of this kind with that handle (a frame's handles, an idle callback's, are their own)
    let idle = kind == CIC;
    let realm = crate::core::html::browsing_context::realm_ctx(scope);
    with_frames(scope, |f| {
        if let Some(i) = f.pending.iter().position(|p| p.handle as i64 == id && p.idle == idle && p.realm == realm) {
            f.pending.remove(i);
        }
    });
}

fn cb_frame_row(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(kind) = crate::binding::protos::body_data(scope, &args).number_value(scope) else { return };
    if kind == RAF || kind == RIC {
        if let Some(id) = request(scope, &args, kind) {
            rv.set_double(id as f64);
        }
    } else {
        cancel(scope, &args, kind);
    }
}

/// Is `name` one of this file's four rows?
pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "requestAnimationFrame" | "cancelAnimationFrame" | "requestIdleCallback" | "cancelIdleCallback")
}

/// The body of one of the four rows. `length` 1 for all four, which is the capture's.
pub(crate) fn body<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Function> {
    let kind = match name {
        "requestAnimationFrame" => RAF,
        "cancelAnimationFrame" => CAF,
        "requestIdleCallback" => RIC,
        _ => CIC,
    };
    let d = v8::Number::new(scope, kind);
    build_fn_data(scope, cb_frame_row, name, 1, d.into())
}

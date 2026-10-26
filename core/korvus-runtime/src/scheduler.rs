//! The kernel's **task source** — HTML's timer task source, owned by Rust.
//!
//! # The defect this file exists to remove
//!
//! `setTimeout` used to be a promise chain:
//!
//! ```js
//! Promise.resolve(sleep(Number(ms) || 0)).then(() => { … })
//! ```
//!
//! A timer callback was therefore a **promise job**, not a task. Two timers due at the same moment
//! resolved in the same event-loop poll, so both `.then` jobs landed in **one** microtask checkpoint
//! and ran back-to-back; anything the first timer queued as a microtask was appended AFTER the second
//! timer's job. Measured against `google-chrome --headless=new` on 2026-09-11:
//!
//! ```js
//! setTimeout(() => { log('t1'); queueMicrotask(() => log('mt1')); }, 0);
//! setTimeout(() => log('t2'), 0);
//! ```
//!
//! * Chrome: `t1, mt1, t2` — each timer is its own task, with a microtask checkpoint between them.
//! * Korvus, before this file: `t1, t2, mt1`.
//!
//! The blast radius was everything built on `globalThis.setTimeout`: `requestAnimationFrame` and
//! `requestIdleCallback` (`host.js`'s `schedule` helper), `AbortSignal.timeout`, and the worker realm's
//! own copy in `worker-host.js`.
//!
//! # What replaces it
//!
//! One queue, in Rust, holding the callback handles, the deadlines, the insertion order and HTML's
//! timer nesting level. **Exactly one timer callback runs per event-loop turn**, because the only way a
//! callback is reached is:
//!
//! ```js
//! do { const id = await op_timer_next_due(); if (id === 0) break; op_timer_run(id); }
//! while (op_timer_pump_claim());
//! ```
//!
//! `op_timer_next_due` is an async op, so its resolution is a fresh turn of the loop and the pump's
//! continuation is a job in the microtask checkpoint that follows it. The pump runs ONE callback and
//! then awaits again, so the checkpoint that ran timer *n* drains to exhaustion — including anything
//! that callback queued — before timer *n+1* can be reached. That is the task boundary HTML requires.
//!
//! The pump is started by `setTimeout`/`setInterval` when it is not already running, and STOPS as soon
//! as the queue drains (`op_timer_next_due` answers `0`). That matters as much as the ordering: a
//! perpetual pending op would keep `run_event_loop` from ever resolving, and [`crate::Engine::pump`]'s
//! whole "a page that leaves nothing scheduled settles instantly" contract would be gone.
//!
//! # The four ordering properties, and what Chrome answers
//!
//! Asked of `google-chrome --headless=new --dump-dom` (truth source 2), 2026-09-11:
//!
//! 1. `t1 mt1 t2` — two timers due together are two tasks.
//! 2. `T m1 m2 m3 AFTER` — a microtask chain queued inside a timer drains to exhaustion inside that
//!    one checkpoint, before the next timer task.
//! 3. `a b c` for three timers armed at the same deadline — equal deadlines run in insertion order.
//! 4. `gaps=1,0,0,0,0,0,4,4,4,4,4,4` for a self-re-arming `setTimeout(…, 0)` chain, and
//!    `interval gaps=0,0,0,0,0,0,4,4,4,4,4,4` for `setInterval(…, 0)` — HTML's nesting clamp. A 0 ms
//!    timer chain runs flat out for five levels and is then clamped to 4 ms, which is what stops a
//!    re-arming chain from starving the rest of the loop.
//!
//! Property 4 is [`MAX_NESTING`]/[`CLAMP_MS`] below; 1–3 are the queue's shape.

use deno_core::{op2, v8, OpState};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::rc::Rc;
use tokio::time::{Duration, Instant};

/// HTML: "If nesting level is greater than 5, and timeout is less than 4, then set timeout to 4."
const MAX_NESTING: u32 = 5;
/// …the 4 in that sentence, in milliseconds.
const CLAMP_MS: f64 = 4.0;

/// How long the timer pump parks while virtual time (F5) is pinned and nothing is due. It is a bounded
/// real-time wait, NOT a sleep to the virtual deadline (which never arrives on its own): on waking, the
/// loop re-reads the advanced virtual now through [`Scheduler::next`] and fires what has come due. The
/// ceiling is what picks up an advance made on another thread, which cannot notify the Rc-local
/// [`Scheduler::wake`]; an arm still wakes the pump at once.
const VIRTUAL_PARK: Duration = Duration::from_millis(1);

/// The current moment on the scheduler's timeline. With no virtual-time policy set -- the default, and
/// every no-plugin run -- this is real [`Instant::now`], byte-for-byte the old behaviour (invariant
/// I9). While a plugin has pinned the clock (F5), it is the PINNED virtual now: the clock does not
/// advance on its own, and only `host->virtual_time_advance` moves it, so a timer armed here is due
/// exactly its delay of VIRTUAL time later and [`Scheduler::next`] treats a deadline at or before the
/// virtual now as ready. A cheap `Option` read on the arming/due hot path.
fn sched_now() -> Instant {
    match plugin_host::virtual_time() {
        Some(vt) => Instant::from_std(vt.now_instant()),
        None => Instant::now(),
    }
}
// TODO(1021-verify): what virtual time reaches here. Virtualized: `setTimeout`/`setInterval` deadlines
// (`set_timer`, `rearm`), engine + rendering-frame tasks (`queue_task` -> `queue_frame_task`, so
// `requestAnimationFrame` fires on `advance`) and the timer pump's due check (`op_timer_next_due`,
// `run_due_frame`). Left on the REAL clock: `requestIdleCallback`'s idle-period bounds (`next`'s idle
// branch, `queue_idle_task`) and 0 ms immediate tasks (`set_immediate_timer`, `record_ready_task`,
// `queue_next_task` -- they run at once regardless). The WORKER realm runs its own pump in
// `worker_scope.rs` (not a file this task edits): its shared `Scheduler` arms on the virtual timeline
// now, but the worker pump's due check still reads real now, so worker-realm timers under virtual time
// need `worker_scope.rs` switched to `sched_now` to match.

/// One armed timer. The callback and its arguments are V8 globals held by the kernel for exactly as
/// long as the timer is live — a one-shot drops them when it runs, an interval when it is cleared.
struct Timer {
    cb: v8::Global<v8::Function>,
    args: Vec<v8::Global<v8::Value>>,
    /// The global `setTimeout`/`setInterval` was called on: HTML's callback this value. `None`
    /// for an engine task, which is called on the global of the realm the pump runs in.
    this: Option<v8::Global<v8::Value>>,
    /// `setInterval` re-arms itself; `setTimeout` does not.
    repeat: bool,
    /// The requested period, BEFORE the nesting clamp — an interval re-clamps per tick, because its
    /// nesting level grows by one each time it fires (measured in Chrome: ticks 1-6 at 0 ms, 7+ at 4).
    interval_ms: f64,
    /// This timer's task's timer nesting level (HTML "timer initialization steps" step 11).
    nesting: u32,
    /// When this timer is scheduled to fire -- the grid an interval's next tick snaps to.
    due: Instant,
    /// Which queue entry is the live one for this id. An entry whose `seq` no longer matches was
    /// superseded (an interval re-armed) and is skipped — the cheap alternative to removing from the
    /// middle of a heap.
    seq: u64,
    /// An IDLE task ([`Scheduler::queue_idle_task`]): it waits in the idle queue, not the timer queue, and its
    /// callback is handed the end of the idle period it runs in.
    idle: bool,
}

/// A queue entry: `(deadline, insertion order, id)`.
#[derive(PartialEq, Eq)]
struct Due {
    at: Instant,
    seq: u64,
    id: u32,
}
// `BinaryHeap` is a MAX-heap, so the ordering is reversed: earliest deadline first and, among equal
// deadlines, smallest sequence number first. That second half is ordering property 3 — equal
// deadlines fire in insertion order — and it is why the entry carries a sequence at all.
impl Ord for Due {
    fn cmp(&self, other: &Self) -> Ordering {
        other.at.cmp(&self.at).then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What [`Scheduler::next`] found.
enum Next {
    /// This id is due now; run it.
    Ready(u32),
    /// Nothing is due yet; the earliest deadline is this.
    Wait(Instant),
    /// Nothing is due and no deadline is pending, but an idle task waits for the event loop to have nothing ready: park
    /// until woken ([`wake_due_timers`]).
    Park,
    /// The queue is empty — the pump may stop.
    Idle,
}

/// A SELECTION: the moment [`Scheduler::next`] found a timer due -- when, the last queue entry there was then, and the
/// point of the event loop's task sequence it was made at ([`deno_core::task_seq_now`]).
#[derive(Clone, Copy)]
struct Selection {
    now: Instant,
    last_seq: u64,
    point: u64,
}

/// Where the one pump is, which is what makes "exactly one timer callback per event-loop turn" an
/// enforced property rather than a convention: a second pump would run a second callback in the same
/// turn and the ordering this unit exists for would be gone. The phase lives in RUST, not in a JS
/// boolean, for the reason [`Pump::Stranded`] records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pump {
    /// No pump: the queue drained and the last one exited. The next `setTimeout` starts one.
    Stopped,
    /// A pump is parked in [`op_timer_next_due`], or holds an id it has not run yet.
    Waiting,
    /// A pump is inside a timer callback. A `setTimeout` from that callback sees this and does NOT
    /// start a second pump.
    Dispatching,
    /// [`op_timer_run`] returned and the pump has not asked for the next task yet. Zero JS statements
    /// wide in a live pump — the loop's `while (claim())` is the very next thing that runs — so
    /// observing it from anywhere else means the pump's JS frame was destroyed mid-callback (V8
    /// termination: the WPT runner's watchdog does exactly this) and a new pump must take over.
    /// Without this state a terminated file would silently lose every later timer in its realm.
    Stranded,
}

/// The realm's timer task source. One per isolate, in `OpState`; both the window extension and the
/// worker extension put one, so both realms run the same algorithm (goal invariant I8).
pub struct Scheduler {
    timers: HashMap<u32, Timer>,
    queue: BinaryHeap<Due>,
    pump: Pump,
    next_id: u32,
    next_seq: u64,
    /// The timer nesting level of the task currently running, or 0 when no timer task is running.
    /// HTML reads this when a timer is armed and writes it while a timer task runs.
    nesting: u32,
    /// The level to fall back to once the timer task that set [`Self::nesting`] is over.
    ///
    /// CHROME'S RULE, NOT HTML'S (decided 2026-10-08 by the project owner: the goal is Chrome's exact
    /// behaviour). Chrome 150 keeps a timer task's nesting level through the microtasks that run after
    /// its callback, so a timer armed from a promise continuation of the callback -- an `await` loop of
    /// `setTimeout(r, 0)` -- is clamped to 4 ms from its sixth level (measured 2026-10-08: 4.0-4.1 ms a
    /// turn; this engine ran it at 0.0). HTML says such a timer does not inherit the level, and WPT
    /// `html/webappapis/timers/timer-nesting-not-inherited-in-microtask.html` checks it; Chrome fails
    /// that file and so does this engine now. The checkpoint's end is not observable from here, so the
    /// level is kept while the isolate is still running microtasks and dropped at the next task
    /// boundary (next turn, next timer task, or the first timer armed outside a checkpoint).
    after_task: Option<u32>,
    /// Wakes the parked task source when a timer is armed with an EARLIER deadline than the one it is
    /// sleeping to. Without it, `setTimeout(f, 1000)` followed by `setTimeout(g, 0)` would hold `g`
    /// for a second.
    wake: Rc<tokio::sync::Notify>,
    /// THE IDLE QUEUE: `requestIdleCallback`'s tasks, in posting order, each with the moment
    /// before which it may not run (one posted inside an idle period waits for the next). Blink's idle tasks are the
    /// lowest priority there is: one runs only in an IDLE PERIOD, which begins when no other task is ready, lasts until
    /// the next pending task -- at most 50 ms -- and does not begin at all when that is under 1 ms
    /// (`IdleHelper::ComputeNewLongIdlePeriodState`). Chrome 150, five runs alike: `requestIdleCallback`, then a 0 ms
    /// timer, a MessageChannel message and a 1 ms timer run `t0, msg, t1, ric`; korvus ran the idle callback first, as
    /// the 0 ms task it was queued as.
    idle: std::collections::VecDeque<(u32, Instant, u64)>,
    /// The rendering frame queued and not yet run ([`Self::queue_frame_task`]): an idle task queued while a frame is
    /// expected waits for THAT frame, and runs in the short idle period it leaves -- Chrome 150 runs
    /// `requestIdleCallback` + `setTimeout(0)` + `requestAnimationFrame` as `t0, raf, ric` (five runs alike); korvus ran
    /// `ric, t0, raf`. One frame, not every one: under a page's endless `requestAnimationFrame` loop a frame is always
    /// queued, and the idle periods between frames still come (WPT `requestidlecallback/callback-idle-periods`,
    /// `deadline-max-rAF`, `callback-multiple-calls`: Chrome 150 passes all three).
    frame_task: Option<u32>,
    /// How many frames have run: an idle entry records the count it was queued at ([`Self::idle`]'s third field).
    frames_run: u64,
    /// The frame task is running: an idle task it queues -- a `requestIdleCallback` from an animation frame callback --
    /// belongs to the idle period THIS frame leaves, before the next frame (Chrome 150, five runs alike: rIC, a 0 ms
    /// timer and rAF from inside a rAF callback run `raf1, t0, ric, raf2`; from a 0 ms timer after the frame, `raf1, t0,
    /// raf2, ric`).
    in_frame: bool,
    /// The end of the idle period the idle task being handed out runs in.
    idle_deadline: Option<Instant>,
    /// Tasks that run before anything else that is due ([`Self::queue_next_task`]), in their order.
    next_tasks: std::collections::VecDeque<u32>,
    /// The last selection that found a timer due: every entry already due then was moved at it ([`Self::next`]).
    selection: Option<Selection>,
    /// Whether the event loop has a task ready, once the pump has looked it up ([`op_timer_next_due`]).
    readiness: Option<deno_core::EventLoopReadiness>,
}

/// How long after a deadline a timer still counts as due at the selection a pump that waited for that deadline makes
/// ([`Scheduler::next`]): Chrome 152's wake-up latency on this machine, 2026-10-23, a 5 us clock -- timers armed 0-60 us
/// apart were moved at one selection 58 times in 60, 80-120 us apart 11 times in 60 (and the first ran 0.11-0.14 ms
/// after its deadline).
const SAME_WAKE: Duration = Duration::from_micros(70);

/// Blink's idle period bounds (`IdleHelper::kMaximumIdlePeriodDuration`, `kMinimumIdlePeriodDuration`).
const MAX_IDLE_PERIOD: Duration = Duration::from_millis(50);
const MIN_IDLE_PERIOD: Duration = Duration::from_millis(1);

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            timers: HashMap::new(),
            queue: BinaryHeap::new(),
            pump: Pump::Stopped,
            next_id: 1, // 0 is never a timer id: the pump reads it as "queue empty, stop".
            next_seq: 0,
            nesting: 0,
            after_task: None,
            wake: Rc::new(tokio::sync::Notify::new()),
            idle: std::collections::VecDeque::new(),
            frame_task: None,
            frames_run: 0,
            in_frame: false,
            idle_deadline: None,
            next_tasks: std::collections::VecDeque::new(),
            selection: None,
            readiness: None,
        }
    }
}

impl Scheduler {
    /// HTML's timer initialization steps, the part that is arithmetic: clamp a negative delay to zero,
    /// then apply the nesting clamp. `nesting` is the level the timer is being armed AT (the running
    /// task's level, or 0 at top level) — not the level the new task gets, which is one higher.
    fn clamp(ms: f64, nesting: u32) -> f64 {
        // WebIDL types the argument `long`, so a delay outside int32 wraps exactly as it does in a
        // browser (`setTimeout(f, 2**32)` fires immediately in Chrome). Doing this in Rust also keeps
        // a nonsense delay from reaching `Duration`, which panics on an out-of-range float.
        let ms = idl_long(ms) as f64;
        let ms = if ms > 0.0 { ms } else { 0.0 };
        if nesting > MAX_NESTING && ms < CLAMP_MS {
            CLAMP_MS
        } else {
            ms
        }
    }

    /// Arm a timer, answering its id. `nesting` is taken from the running task, so a timer armed
    /// inside a timer callback is one level deeper.
    ///
    /// `in_checkpoint`: the isolate is running microtasks -- still inside the task that set the level
    /// (see [`Self::after_task`]); outside one, a finished timer task's level no longer applies.
    pub(crate) fn set_timer(&mut self, cb: v8::Global<v8::Function>, args: Vec<v8::Global<v8::Value>>, this: Option<v8::Global<v8::Value>>, ms: f64, repeat: bool, in_checkpoint: bool) -> u32 {
        if !in_checkpoint {
            self.end_task();
        }
        let at_level = self.nesting;
        let delay = Self::clamp(ms, at_level);
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        // `sched_now` not `Instant::now`: under virtual time (F5) the deadline is `delay` of VIRTUAL
        // time from the pinned now, so an advance of `delay` makes it due. With no plugin it IS
        // `Instant::now()` (invariant I9).
        let at = sched_now() + Duration::from_secs_f64(delay / 1000.0);
        self.timers.insert(
            id,
            Timer { cb, args, this, repeat, interval_ms: ms, nesting: at_level + 1, seq, due: at, idle: false },
        );
        self.queue.push(Due { at, seq, id });
        self.wake.notify_waiters();
        id
    }

    /// A `setTimeout` or `setInterval` whose delay is 0 after the nesting clamp: `Some(id)`, recorded but NOT put in the
    /// pump's queue -- the caller queues it on the event loop at once ([`crate::scope_timers`]). Blink posts it as an
    /// immediate task (`DOMTimer` -> `kJavascriptTimerImmediate`, `TimerBase::Start` with a null fire time; an
    /// interval too, `kSetIntervalWithoutClamp` being on in Chrome 150), so it runs in the order it was posted among
    /// every other ready task, not after the next wake-up. `None`: an ordinary timer.
    pub(crate) fn set_immediate_timer(&mut self, cb: &v8::Global<v8::Function>, args: &[v8::Global<v8::Value>], this: &Option<v8::Global<v8::Value>>, ms: f64, repeat: bool, in_checkpoint: bool) -> Option<u32> {
        if !in_checkpoint {
            self.end_task();
        }
        let at_level = self.nesting;
        if Self::clamp(ms, at_level) > 0.0 {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        self.timers.insert(
            id,
            Timer { cb: cb.clone(), args: args.to_vec(), this: this.clone(), repeat, interval_ms: ms, nesting: at_level + 1, seq, due: Instant::now(), idle: false },
        );
        Some(id)
    }

    /// After an immediate interval's tick ([`run_ready_task`]): `Some(true)` when its next tick is immediate too -- its
    /// pump entry is made stale, the caller queues it on the event loop -- `Some(false)` when the clamp delays it (the
    /// pump's), `None` when it is gone.
    pub(crate) fn interval_next_immediate(&mut self, id: u32) -> Option<bool> {
        let t = self.timers.get(&id).filter(|t| t.repeat)?;
        if Self::clamp(t.interval_ms, t.nesting - 1) > 0.0 {
            return Some(false);
        }
        let seq = self.bump_seq();
        if let Some(t) = self.timers.get_mut(&id) {
            t.seq = seq;
        }
        Some(true)
    }

    /// Queue an ENGINE task -- a message's delivery, an observer's notification, `voiceschanged` -- on
    /// this task source, `ms` from now. It is not a timer: it is never nesting-clamped,
    /// and it runs at nesting level 0, so a `setTimeout` its callback arms starts a fresh chain.
    ///
    /// They went through the page's `setTimeout` and inherited its nesting level, so a `postMessage`
    /// ping-pong was clamped to 4 ms per hop after five: 220 deliveries/s where Chrome 150 runs a
    /// MessageChannel at ~410,000/s and `window.postMessage` at ~314,000/s (measured headed, same box).
    pub(crate) fn queue_task(&mut self, cb: v8::Global<v8::Function>, ms: f64) -> u32 {
        let ms = if ms.is_finite() && ms > 0.0 { ms } else { 0.0 };
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        // On the virtual timeline under F5 (so a queued rendering frame fires on `advance`), real now
        // otherwise -- see [`sched_now`].
        let at = sched_now() + Duration::from_secs_f64(ms / 1000.0);
        self.timers.insert(id, Timer { cb, args: Vec::new(), this: None, repeat: false, interval_ms: ms, nesting: 0, seq, due: at, idle: false });
        self.queue.push(Due { at, seq, id });
        self.wake.notify_waiters();
        id
    }

    /// The rendering frame, when it is due now ([`Self::queue_frame_task`]).
    pub(crate) fn due_frame(&self, now: Instant) -> Option<u32> {
        let id = self.frame_task?;
        self.timers.get(&id).filter(|t| t.due <= now).map(|_| id)
    }

    /// Record an engine task that is READY NOW without putting it in the pump's queue: the caller
    /// queues its run on the event loop at once ([`crate::scope_timers::queue_engine_task`]), in the order every
    /// other ready task was -- Chromium's sequence manager orders ready tasks by when they were queued, a delayed one
    /// by when it woke. Through the pump it ran a turn or two after a fetch's end that came later.
    pub(crate) fn record_ready_task(&mut self, cb: v8::Global<v8::Function>) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        self.timers.insert(id, Timer { cb, args: Vec::new(), this: None, repeat: false, interval_ms: 0.0, nesting: 0, seq, due: Instant::now(), idle: false });
        id
    }

    /// Queue the REST OF THE RUNNING TASK as the very next task, ahead of everything else that is due -- for an
    /// algorithm Chrome runs in one task with a microtask checkpoint in its middle (a same-document traversal's
    /// Navigation API commit, then its `popstate`). A task runs inside the pump's own microtask checkpoint here, where
    /// V8 makes a nested checkpoint a no-op; ending the task and going on in the next one drains it in between.
    pub(crate) fn queue_next_task(&mut self, cb: v8::Global<v8::Function>) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        let at = Instant::now();
        self.timers.insert(id, Timer { cb, args: Vec::new(), this: None, repeat: false, interval_ms: 0.0, nesting: 0, seq, due: at, idle: false });
        self.next_tasks.push_back(id);
        self.wake.notify_waiters();
        id
    }

    /// Queue the RENDERING FRAME as an engine task `ms` from now: [`Self::queue_task`], remembered as the frame an
    /// idle period does not begin ahead of ([`Self::frame_task`]).
    pub(crate) fn queue_frame_task(&mut self, cb: v8::Global<v8::Function>, ms: f64) -> u32 {
        let id = self.queue_task(cb, ms);
        self.frame_task = Some(id);
        id
    }

    /// Queue an IDLE task (`requestIdleCallback`'s runner), to run in an idle period no sooner than `not_before_ms`
    /// from now -- see [`Self::idle`]. Its callback is called with the end of that idle period
    /// ([`spec::process_ms`]).
    pub(crate) fn queue_idle_task(&mut self, cb: v8::Global<v8::Function>, not_before_ms: f64) -> u32 {
        let ms = if not_before_ms.is_finite() && not_before_ms > 0.0 { not_before_ms } else { 0.0 };
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let seq = self.bump_seq();
        let at = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
        self.timers.insert(id, Timer { cb, args: Vec::new(), this: None, repeat: false, interval_ms: ms, nesting: 0, seq, due: at, idle: true });
        // queued by the running frame: counted as queued before it, so it may run once this frame is done
        let frames_then = if self.in_frame { self.frames_run.saturating_sub(1) } else { self.frames_run };
        self.idle.push_back((id, at, frames_then));
        self.wake.notify_waiters();
        id
    }

    /// The timer task that set [`Self::nesting`] is over: fall back to the level outside it.
    pub(crate) fn end_task(&mut self) {
        if let Some(outer) = self.after_task.take() {
            self.nesting = outer;
        }
    }

    fn bump_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// `clearTimeout` / `clearInterval`. Dropping the record is enough: its queue entry is skipped
    /// when it surfaces, so a timer cleared from a microtask queued by an EARLIER timer never runs —
    /// which is what Chrome does (probe 3, 2026-09-11: `p1 | cleared | p3`, never `SHOULD-NOT-RUN`).
    pub(crate) fn clear_timer(&mut self, id: u32) {
        self.timers.remove(&id);
    }

    /// Whether `id` is still armed (not cleared, not a finished one-shot).
    pub(crate) fn is_live(&self, id: u32) -> bool {
        self.timers.contains_key(&id)
    }

    /// The earliest live entry, popped if it is already due -- else, when no task is due, an idle task if an idle
    /// period may begin now ([`Self::idle`]). `waited`: the pump waited for a deadline and woke for it itself.
    fn next(&mut self, now: Instant, waited: bool) -> Next {
        while let Some(id) = self.next_tasks.pop_front() {
            if self.timers.contains_key(&id) {
                return Next::Ready(id);
            }
        }
        let pending = loop {
            let Some((at, seq, id)) = self.queue.peek().map(|d| (d.at, d.seq, d.id)) else {
                break None;
            };
            // Cleared, or superseded by an interval's re-arm: drop the entry and look again.
            if !self.timers.get(&id).is_some_and(|t| t.seq == seq) {
                self.queue.pop();
                continue;
            }
            if at <= now {
                let moved = self.selection.filter(|s| at <= s.now && seq <= s.last_seq);
                // A NEW SELECTION, made by a pump that waited for this deadline with nothing else to run: Chrome's
                // thread, asleep until it, wakes a little after it, and a timer due in between is moved at the same
                // selection. Two `setTimeout(10)`s armed 0-60 us apart, the first posting a message, run `A B M` in Chrome
                // 152 (58 of 60, 2026-10-23, a 5 us clock); 80-120 us apart `A M B` (49 of 60). This pump wakes ON the
                // deadline, so it waits out the timers due within [`SAME_WAKE`] after it before it selects -- never
                // running one early.
                if moved.is_none() && waited && !self.loop_busy() {
                    if let Some(last) = self.due_within(at + SAME_WAKE).filter(|&last| last > now) {
                        return Next::Wait(last);
                    }
                }
                // Popped, but the record STAYS: `op_timer_run` re-checks it, so a `clearTimeout` that
                // lands between this pop and the callback still cancels.
                self.queue.pop();
                // EVERY TIMER DUE AT A SELECTION IS MOVED AT IT. Chromium's sequence manager moves every delayed task
                // that is due to its work queue when it selects the next task, so timers due together run back to
                // back, each a task of its own, AHEAD of what the first one posts. The pump hands out one id per
                // resolution of its op, and an unnamed resolution is queued where the loop noticed it -- behind
                // whatever the timer before it posted: four `setTimeout(50)`s beside a `MessageChannel` loop of 5 ms
                // tasks ran after 1, 2, 3 and 4 more of its tasks, and three `setTimeout(10)`s, the first posting a
                // message, `A M B C`. Chrome 152 (2026-10-23, four runs): 1, 1, 1, 1 and `A amt B C M W T0` (5/5). So
                // the op names the selection it was moved at ([`deno_core::op_ready_at`]) -- the first selection that
                // found it due, an entry armed after it being no part of it -- and the event loop queues it there.
                // `clearTimeout` from an earlier one still cancels: each is popped, and re-checked, in its turn.
                let point = match moved {
                    Some(s) => s.point,
                    None => {
                        let s = Selection { now, last_seq: self.next_seq, point: deno_core::task_seq_now() };
                        self.selection = Some(s);
                        s.point
                    }
                };
                deno_core::op_ready_at(point);
                return Next::Ready(id);
            }
            break Some(at);
        };
        // No task is ready: an idle period may begin -- for the idle task at the head of the queue (a cancelled one
        // is dropped), if its moment has come and no frame is expected first.
        while let Some(&(id, _, _)) = self.idle.front() {
            if self.timers.contains_key(&id) {
                break;
            }
            self.idle.pop_front();
        }
        let Some(&(id, not_before, frames_then)) = self.idle.front() else {
            return pending.map_or(Next::Idle, Next::Wait);
        };
        // ...and only when the EVENT LOOP has nothing ready either: Blink's idle queue is the lowest priority there is,
        // so an idle task waits behind every task of every other source. This looked at the timer queue alone and
        // began idle periods while `MessageChannel` tasks were queued: `requestIdleCallback` beside a loop of 5 ms of
        // them ran after 1-6 of them, mid-loop, with 45-50 ms "remaining", and one with `{timeout: 400}` ran as an idle
        // callback at 16-24 ms; Chrome 152 (2026-10-23) runs the first after the loop's last task (60 of 60, three runs)
        // and the second at its timeout, `didTimeout` true (406-412 ms, four runs). Parked, the pump is woken by the
        // event loop's look between two tasks once nothing is ready ([`wake_due_timers`]).
        if self.loop_busy() {
            return pending.map_or(Next::Park, Next::Wait);
        }
        let frame_expected = self.frame_task.is_some_and(|f| self.timers.contains_key(&f));
        if frame_expected && self.frames_run == frames_then {
            // the frame expected when this task was queued is a pending task: wait for it, and look again once it has run
            return pending.map_or(Next::Idle, Next::Wait);
        }
        if not_before > now {
            return Next::Wait(pending.map_or(not_before, |p| p.min(not_before)));
        }
        // The period lasts until the next pending task, at most 50 ms; under 1 ms it does not begin -- that task runs
        // first, and the idle queue is looked at again after it.
        let period = pending.map_or(MAX_IDLE_PERIOD, |p| (p - now).min(MAX_IDLE_PERIOD));
        if period < MIN_IDLE_PERIOD {
            return pending.map_or(Next::Idle, Next::Wait);
        }
        self.idle.pop_front();
        self.idle_deadline = Some(now + period);
        Next::Ready(id)
    }

    /// Is a task of the timer queue due at `now` -- the rest of a task ([`Self::next_tasks`]) or a timer whose deadline
    /// has passed? Nothing is popped: the pump takes it ([`Self::next`]). Superseded and cleared entries at the head are
    /// dropped on the way, as `next` drops them. Idle tasks are not asked about: an idle period begins only when the
    /// loop has nothing else, and the pump finds that out for itself.
    fn has_due(&mut self, now: Instant) -> bool {
        if self.next_tasks.iter().any(|id| self.timers.contains_key(id)) {
            return true;
        }
        while let Some((at, seq, id)) = self.queue.peek().map(|d| (d.at, d.seq, d.id)) {
            if !self.timers.get(&id).is_some_and(|t| t.seq == seq) {
                self.queue.pop();
                continue;
            }
            return at <= now;
        }
        false
    }

    /// The latest deadline of the live entries at the head of the timer queue that are due by `by` (the head among
    /// them), the queue left as it was. Superseded and cleared entries met on the way are dropped, as `next` drops them.
    fn due_within(&mut self, by: Instant) -> Option<Instant> {
        let mut held = Vec::new();
        let mut last = None;
        while let Some((at, seq, id)) = self.queue.peek().map(|d| (d.at, d.seq, d.id)) {
            if !self.timers.get(&id).is_some_and(|t| t.seq == seq) {
                self.queue.pop();
                continue;
            }
            if at > by {
                break;
            }
            last = Some(at);
            held.extend(self.queue.pop());
        }
        self.queue.extend(held);
        last
    }

    /// Has the event loop a task ready to run ([`deno_core::EventLoopReadiness`])?
    fn loop_busy(&self) -> bool {
        self.readiness.as_ref().is_some_and(|r| r.has_ready_task())
    }

    /// "Should the caller be the pump?" — true exactly once per pump lifetime, to whoever asks while
    /// there is no live pump. Both arming functions ask (so a page that arms a timer with the queue
    /// idle starts one) and the pump itself asks at the top of every iteration (so it keeps the claim,
    /// and so the [`Pump::Stranded`] window closes in the same JS statement it opened).
    pub(crate) fn claim_pump(&mut self) -> bool {
        match self.pump {
            Pump::Waiting | Pump::Dispatching => false,
            Pump::Stopped | Pump::Stranded => {
                self.pump = Pump::Waiting;
                true
            }
        }
    }

    /// Re-arm an interval for its next tick, at the nesting level its task runs at.
    ///
    /// THE NEXT TICK IS ON THE INTERVAL'S GRID, as Blink's `TimerBase::RunInternal` puts it: "the next
    /// tick is next_fire_time_ + repeat_interval_, but if late wakeup happens we could miss ticks ... the
    /// next task targets the tick following a minimum interval of repeat_interval_ / 20" -- the first
    /// `due + k * interval` at least `now + interval / 20`. Chrome 150's `setInterval(f, 10)` ticks at
    /// 10.1, 20.0, 30.0, 40.4, 50.4 ms (measured 2026-10-08); re-arming from `now` drifted by the
    /// lateness every tick (11.1, 22.2, 33.4 ... 112.3).
    fn rearm(&mut self, id: u32) {
        let seq = self.bump_seq();
        // Virtual now under F5, so an interval's next tick lands on the virtual timeline and fires on
        // `advance`; real now with no plugin -- see [`sched_now`].
        let now = sched_now();
        let at = {
            let Some(t) = self.timers.get_mut(&id) else { return };
            // The re-init runs at the level of the task that is firing (`t.nesting`), and the NEXT tick's
            // task is one deeper -- which is why a 0 ms interval is clamped from its seventh tick.
            let delay = Self::clamp(t.interval_ms, t.nesting);
            t.seq = seq;
            t.nesting += 1;
            let at = if delay <= 0.0 {
                now
            } else {
                let period = Duration::from_secs_f64(delay / 1000.0);
                let target = now + period / 20;
                let mut tick = t.due + period;
                if tick < target {
                    let behind = (target - tick).as_secs_f64();
                    let k = (behind / period.as_secs_f64()).ceil() as u32;
                    tick += period * k;
                }
                tick
            };
            t.due = at;
            at
        };
        self.queue.push(Due { at, seq, id });
    }
}

/// THE EVENT LOOP'S QUESTION BETWEEN TWO TASKS ([`deno_core::JsRuntime::set_task_source_hook`]): has the pump's next
/// timer come due? Then the parked pump is woken, the loop polls its [`op_timer_next_due`] at once, and the timer's task
/// is queued behind what was ready before it -- as Chromium's sequence manager moves a delayed task to its work queue
/// when it selects the next task. The pump's own wake-up is a tokio sleep, and tokio runs only between two turns of the
/// loop: beside a `MessageChannel` loop of 5 ms tasks a `setTimeout(50)` or `(200)` ran after 4-12 more of them had
/// started past its deadline, where Chrome 152 (2026-10-23, three runs alike) runs it after ONE. And has the loop nothing
/// ready, with an idle task waiting? Then an idle period may begin ([`Scheduler::next`]). Installed on every realm's
/// runtime, the page's and each worker's.
pub(crate) fn wake_due_timers(scope: &mut v8::PinScope) {
    let state = deno_core::JsRuntime::op_state_from(scope);
    let Ok(mut st) = state.try_borrow_mut() else { return };
    let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return };
    // Only a pump parked in `op_timer_next_due` has anything to be woken from: a stopped one has no timer to run, and a
    // dispatching one is inside the task that is running.
    if sched.pump != Pump::Waiting {
        return;
    }
    if sched.has_due(sched_now()) {
        sched.wake.notify_waiters();
        return;
    }
    // An idle task waits for the loop to have nothing ready ([`Scheduler::next`]): this look is the moment it has none.
    if sched.idle.is_empty() {
        return;
    }
    let busy = st.try_borrow::<deno_core::EventLoopReadiness>().is_some_and(|r| r.has_ready_task());
    if !busy {
        st.borrow::<Scheduler>().wake.notify_waiters();
    }
}

/// Is the isolate running microtasks right now -- inside a task's checkpoint (see `after_task`)?
pub(crate) fn in_microtask_checkpoint(scope: &mut v8::PinScope) -> bool {
    scope.get_current_context().get_microtask_queue().is_some_and(|q| q.is_running_microtasks())
}

/// ECMAScript `ToInt32`, which is what WebIDL's `long` performs on the `timeout` argument.
fn idl_long(v: f64) -> i32 {
    if !v.is_finite() {
        return 0;
    }
    let m = v.trunc().rem_euclid(4_294_967_296.0);
    if m >= 2_147_483_648.0 {
        (m - 4_294_967_296.0) as i32
    } else {
        m as i32
    }
}

// ── the ops ────────────────────────────────────────────────────────────────────────────────────────

/// "Should I be the pump?" — see [`Scheduler::claim_pump`]. Sync on purpose: the claim has to land in
/// the SAME JS statement that decides to start a pump, or two timers armed back to back would each
/// start one (an async op's body does not run until the event loop polls it).
#[op2(fast)]
pub fn op_timer_pump_claim(state: &mut OpState) -> bool {
    state.borrow_mut::<Scheduler>().claim_pump()
}

/// `setTimeout` / `setInterval`: arm a timer, answer its id. The callback is held by the kernel, not
/// by a promise chain.
#[op2(nofast, reentrant)]
#[smi]
pub fn op_timer_set(
    scope: &mut v8::PinScope,
    cb: v8::Local<v8::Function>,
    delay_ms: f64,
    repeat: bool,
    args: v8::Local<v8::Value>,
) -> u32 {
    let cb = v8::Global::new(scope, cb);
    let mut extra = Vec::new();
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(args) {
        for i in 0..arr.length() {
            if let Some(v) = arr.get_index(scope, i) {
                extra.push(v8::Global::new(scope, v));
            }
        }
    }
    let in_checkpoint = in_microtask_checkpoint(scope);
    let state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = state.borrow_mut();
    st.borrow_mut::<Scheduler>().set_timer(cb, extra, None, delay_ms, repeat, in_checkpoint)
}

/// `clearTimeout` / `clearInterval`.
#[op2(fast)]
pub fn op_timer_clear(state: &mut OpState, #[smi] id: u32) {
    state.borrow_mut::<Scheduler>().clear_timer(id);
}

/// **The task source.** Resolves with the id of the one timer whose task is due, or `0` when the queue
/// is empty and the pump should stop.
///
/// Async on purpose even when a timer is already overdue: the resolution of an async op is what makes
/// the callback a new turn of the event loop rather than one more job in the checkpoint that is
/// already running. That is the whole fix.
#[op2(async(lazy), fast)]
#[smi]
pub async fn op_timer_next_due(state: Rc<RefCell<OpState>>) -> u32 {
    // the pump slept or yielded to a deadline and woke for it itself (not woken by an arm or the event loop)
    let mut waited = false;
    loop {
        let wake = { state.borrow().borrow::<Scheduler>().wake.clone() };
        let notified = wake.notified();
        tokio::pin!(notified);
        // Register BEFORE looking at the queue: `notify_waiters` only reaches waiters that are already
        // registered, so arming a timer between the look and the park must not be able to slip through.
        notified.as_mut().enable();
        let next = {
            let mut st = state.borrow_mut();
            if st.borrow::<Scheduler>().readiness.is_none() {
                let r = st.try_borrow::<deno_core::EventLoopReadiness>().cloned();
                st.borrow_mut::<Scheduler>().readiness = r;
            }
            let sched = st.borrow_mut::<Scheduler>();
            // A new turn of the event loop: the last timer task's checkpoint has drained.
            sched.end_task();
            // Due against the virtual now under F5 (so an advance makes deadlines ready), real now
            // otherwise -- see [`sched_now`].
            sched.next(sched_now(), waited)
        };
        match next {
            Next::Idle => {
                // The queue drained: this pump is finished, and the next `setTimeout` starts a new
                // one. Stopping matters as much as the ordering — a perpetually pending op would keep
                // `run_event_loop` from ever resolving, so every page would cost a full pump budget.
                state.borrow_mut().borrow_mut::<Scheduler>().pump = Pump::Stopped;
                return 0;
            }
            Next::Ready(id) => return id,
            Next::Park => {
                notified.as_mut().await;
                waited = false;
            }
            Next::Wait(at) => {
                if plugin_host::virtual_time().is_some() {
                    // F5 virtual time: the clock advances ONLY by `host->virtual_time_advance`, so the
                    // virtual deadline `at` never arrives by waiting. Do not sleep to it -- park for a
                    // bounded real interval (so a cross-thread advance is picked up) or until an arm
                    // wakes us, then re-read the advanced virtual now through `next()` above. A plugin's
                    // `setTimeout(f, 10000)` fires on the first turn after 10000 ms are advanced.
                    tokio::select! {
                        _ = tokio::time::sleep(VIRTUAL_PARK) => {}
                        _ = &mut notified => {}
                    }
                    waited = false;
                } else {
                    // ON TIME, NOT A MILLISECOND LATE. tokio's timer wheel works in whole milliseconds and
                    // rounds a deadline UP, so every timer fired ~1.1 ms late (`setTimeout(f, 10)` at
                    // 11.3 ms where Chrome 150 fires at 10.1, measured 2026-10-08 -- a timing a page can
                    // read). Sleep to a millisecond short of the deadline, then yield until it.
                    let now = Instant::now();
                    if at > now + Duration::from_micros(2500) {
                        waited = tokio::select! {
                            _ = tokio::time::sleep_until(at - Duration::from_millis(2)) => true,
                            _ = &mut notified => false,
                        };
                    } else {
                        // ...and woken by the event loop's look between two tasks ([`wake_due_timers`]) when the
                        // deadline passes inside a task: a yield is only seen to once tokio next has a turn.
                        waited = tokio::select! {
                            _ = tokio::task::yield_now() => true,
                            _ = &mut notified => false,
                        };
                    }
                }
            }
        }
    }
}

/// Run one timer's task: the callback, at its own nesting level, with a throwing callback REPORTED to
/// the page (HTML "report an exception") rather than swallowed or propagated into the pump.
#[op2(nofast, reentrant)]
pub fn op_timer_run(scope: &mut v8::PinScope, #[smi] id: u32) {
    run_timer(scope, id);
}

/// The op's body, callable from Rust. The WORKER realm's pump is Rust (`worker_scope.rs`), so it runs
/// a due task by calling this directly instead of going out to JS and back in through the op.
/// Run engine task `id` ([`Scheduler::record_ready_task`]) as a task of its own, from the event loop and not from the
/// pump: no pump is inside it, so a `setTimeout` its callback makes starts or wakes one as any other would.
pub(crate) fn run_ready_task(scope: &mut v8::PinScope, id: u32) {
    run_task(scope, id, false);
}

/// At most one rendering frame per poll of the page's loop, while a host driver that hands the thread back between
/// polls is polling it ([`arm_frame_cap`]). deno_core runs up to three batches of queued tasks in one poll, and a frame
/// was due after every one of them: a frame that costs more than a refresh (minea.com's, 50 ms) is due again after
/// the next task, so one poll ran task, frame, task, frame ... for 10 s, and a DevTools command waited all of it. HTML
/// gives the rendering its opportunity after a task; the loop it describes also returns to its host between them.
#[derive(Default)]
pub(crate) struct FrameCap {
    armed: bool,
    ran: bool,
}

/// Arm the cap for the poll about to run (`true`), or lift it after (`false`).
pub(crate) fn arm_frame_cap(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, armed: bool) {
    op_state.borrow_mut().put(FrameCap { armed, ran: false });
}

/// The event loop's rendering opportunity after a task ([`webapi::AfterTaskHook`]): the rendering frame, if it is due,
/// runs now -- before the next ready task -- with its microtask checkpoint. The pump's entry for it then finds it run.
pub(crate) fn run_due_frame(scope: &mut v8::PinScope) {
    let due = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if st.try_borrow::<FrameCap>().is_some_and(|c| c.armed && c.ran) {
            return;
        }
        // Virtual now under F5 so the rendering frame fires on `advance`; real now otherwise ([`sched_now`]).
        st.try_borrow::<Scheduler>().and_then(|s| s.due_frame(sched_now()))
    };
    if let Some(id) = due {
        if let Some(c) = deno_core::JsRuntime::op_state_from(scope).borrow_mut().try_borrow_mut::<FrameCap>() {
            c.ran = true;
        }
        // a task of its own -- the rendering's, not the one before it
        webapi::loaf_task_start(scope);
        run_task(scope, id, false);
        scope.perform_microtask_checkpoint();
        webapi::notify_rejected_promises(scope);
        webapi::loaf_task_end(scope);
    }
}

pub(crate) fn run_timer(scope: &mut v8::PinScope, id: u32) {
    run_task(scope, id, true);
}

/// One task of this source: `from_pump` when the pump dequeued it (and is inside it until it returns).
fn run_task(scope: &mut v8::PinScope, id: u32, from_pump: bool) {
    let state = deno_core::JsRuntime::op_state_from(scope);
    let Some((cb, args, this, outer, idle_end)) = ({
        let mut st = state.borrow_mut();
        let sched = st.borrow_mut::<Scheduler>();
        // A new task: the previous timer task (and its microtasks) is over.
        sched.end_task();
        // the frame runs: the idle tasks that waited for it may run in the idle period it leaves
        sched.in_frame = sched.frame_task == Some(id);
        if sched.in_frame {
            sched.frame_task = None;
            sched.frames_run += 1;
        }
        // The pump is inside a callback from here until this op returns. A `setTimeout` from that
        // callback must NOT start a second pump, and a pump whose frame never comes back from the
        // callback must be replaceable -- which is what the `Stranded` this leaves behind is for.
        if from_pump {
            sched.pump = Pump::Dispatching;
        }
        match sched.timers.get(&id) {
            // Cleared after it was dequeued — HTML's "if id does not exist in global's map of active
            // timers, then return".
            None => None,
            Some(t) => {
                let level = t.nesting;
                let repeat = t.repeat;
                // an idle task is handed the end of its idle period
                let idle_end = if t.idle { sched.idle_deadline.take() } else { None };
                // HTML runs the repeat's initialization steps INSIDE the task, before the callback, so
                // a `clearInterval` from the callback still stops the next tick (it drops the record,
                // which strands the entry this just pushed). A one-shot's record is taken whole: its handles
                // move out rather than being copied and dropped (a message's delivery is such a task).
                let held = if repeat {
                    let held = sched.timers.get(&id).map(|t| (t.cb.clone(), t.args.clone(), t.this.clone()));
                    sched.rearm(id);
                    held
                } else {
                    sched.timers.remove(&id).map(|t| (t.cb, t.args, t.this))
                };
                let outer = sched.nesting;
                held.map(|(cb, args, this)| {
                    sched.nesting = level;
                    (cb, args, this, outer, idle_end)
                })
            }
        }
    }) else {
        // Cleared between the dequeue and here (a microtask queued by an earlier timer can do exactly
        // that, and Chrome agrees the timer must not run) -- but the pump still asked for this task, so
        // it still has to be handed back its claim.
        if from_pump {
            state.borrow_mut().borrow_mut::<Scheduler>().pump = Pump::Stranded;
        }
        return;
    };

    let exc = {
        v8::tc_scope!(let tc, scope);
        let f = v8::Local::new(tc, &cb);
        // `this` IS THE WINDOWPROXY. HTML's "invoke the callback" passes the global as the
        // callback this value, so a timer callback sees `window` whatever its strictness. This used to
        // pass `undefined` -- which a SLOPPY callback never noticed, because V8 coerces an undefined
        // receiver to the global proxy for it, and which a STRICT one saw exactly. The note this
        // replaces named W4.f as the unit that closes it.
        //
        // Measured on `google-chrome --headless=new` 141, 2026-09-12, with four callbacks that all
        // declare `"use strict"`:
        //
        //   setTimeout   -> window        setInterval       -> window
        //   queueMicrotask -> undefined   requestAnimationFrame -> undefined
        //
        // So this is the TIMER task source's rule and not a general one: the other two are `invoke`d
        // with no callback this value, and they are not this file's callers.
        //
        // And the window the timer was set ON: every realm's timers run from one pump, in
        // whichever realm started it, so a frame's callback was called on the PAGE's window when the page's script
        // had armed the first timer -- a strict callback's `this.document` was the page's document.
        let recv: v8::Local<v8::Value> = match &this {
            Some(t) => v8::Local::new(tc, t),
            None => {
                let ctx = tc.get_current_context();
                ctx.global(tc).into()
            }
        };
        // a task of its own, the realm of the callback it runs: a frame's, though the pump runs in the page's realm
        webapi::loaf_begin_task(tc, f.into());
        let mut locals: Vec<v8::Local<v8::Value>> = args.iter().map(|a| v8::Local::new(tc, a)).collect();
        if let Some(end) = idle_end {
            locals.push(v8::Number::new(tc, spec::process_ms(end.into_std())).into());
        }
        // A timer's handler, as Chromium's ScheduledAction invokes it (`Function`). A page's handler is a callback
        // the engine invokes (`webapi::ScriptBelow`); an engine task (`this` None) is not -- a message's delivery or a
        // rendering frame runs its listeners and callbacks each with its own checkpoint.
        let below = this.is_some().then(webapi::ScriptBelow::enter);
        #[cfg(feature = "analysis")]
        let _ = webapi::call_traced(tc, "Function", f, recv, &locals);
        #[cfg(not(feature = "analysis"))]
        let _ = f.call(tc, recv, &locals);
        drop(below);
        let e = if tc.has_caught() {
            tc.exception().map(|e| {
                let m = tc.message();
                (v8::Global::new(tc, e), webapi::caught_site(tc, e, m))
            })
        } else {
            None
        };
        tc.reset();
        e
    };

    {
        let mut st = state.borrow_mut();
        let sched = st.borrow_mut::<Scheduler>();
        // The level stays while this task's microtask checkpoint runs (`after_task`).
        sched.after_task = Some(outer);
        if from_pump {
            sched.pump = Pump::Stranded;
        }
        sched.in_frame = false;
    }
    if let Some((e, site)) = exc {
        let e = v8::Local::new(scope, &e);
        webapi::report_exception_at(scope, e, site);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negative_or_nonsense_delay_is_zero_not_a_panic() {
        assert_eq!(Scheduler::clamp(-5.0, 0), 0.0);
        assert_eq!(Scheduler::clamp(f64::NAN, 0), 0.0);
        assert_eq!(Scheduler::clamp(f64::INFINITY, 0), 0.0);
        // WebIDL `long`: 2**32 wraps to 0, so the timer is due immediately (as in Chrome).
        assert_eq!(Scheduler::clamp(4_294_967_296.0, 0), 0.0);
        assert_eq!(Scheduler::clamp(1e300, 0), 0.0);
    }

    #[test]
    fn the_nesting_clamp_starts_where_chrome_starts_it() {
        // Measured in headless Chrome: a self-re-arming `setTimeout(…, 0)` chain runs flat out for
        // six calls and is clamped to 4 ms from the seventh. The seventh is armed by the sixth task,
        // whose nesting level is 6 — the first level greater than 5.
        for level in 0..=MAX_NESTING {
            assert_eq!(Scheduler::clamp(0.0, level), 0.0, "level {level} must not be clamped");
        }
        assert_eq!(Scheduler::clamp(0.0, MAX_NESTING + 1), CLAMP_MS);
        // Only a delay BELOW the clamp is raised; a longer one is left alone.
        assert_eq!(Scheduler::clamp(50.0, MAX_NESTING + 1), 50.0);
    }

    #[test]
    fn equal_deadlines_come_out_in_insertion_order() {
        let at = Instant::now();
        let mut q = BinaryHeap::new();
        q.push(Due { at, seq: 3, id: 30 });
        q.push(Due { at, seq: 1, id: 10 });
        q.push(Due { at, seq: 2, id: 20 });
        // …and an earlier deadline still wins over insertion order.
        q.push(Due { at: at - Duration::from_millis(1), seq: 9, id: 90 });
        let order: Vec<u32> = std::iter::from_fn(|| q.pop()).map(|d| d.id).collect();
        assert_eq!(order, vec![90, 10, 20, 30]);
    }
}

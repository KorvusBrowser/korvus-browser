//! A timer callback is a TASK, not a promise job.
//!
//! `setTimeout` used to be `Promise.resolve(sleep(ms)).then(cb)`. Two timers due at the same moment
//! therefore resolved in one event-loop poll, both `.then` jobs landed in ONE microtask checkpoint, and
//! a microtask the first timer queued was appended after the second timer's job. The queue, the
//! deadlines, the insertion order and HTML's nesting clamp are Rust now
//! (`core/korvus-runtime/src/scheduler.rs`), and the pump runs exactly one callback per turn of the
//! event loop.
//!
//! THE ORACLE. Every expectation below was measured against `google-chrome --headless=new --dump-dom`
//! on 2026-09-11, on the same page bodies these tests eval:
//!
//! ```text
//! t1 mt1 t2 a b c T m1 m2 m3 AFTER            gaps=1,0,0,0,0,0,4,4,4,4,4,4
//! interval gaps=0,0,0,0,0,0,4,4,4,4,4,4       p1 | cleared | p3
//! ```
//!
//! The four ordering properties, and the test that holds each:
//!
//! 1. two timers due together are two tasks → [`two_timers_due_together_are_two_tasks`]
//! 2. `queueMicrotask` drains to exhaustion in one checkpoint →
//!    [`a_microtask_chain_drains_before_the_next_timer_runs`]
//! 3. equal deadlines fire in insertion order → [`equal_deadlines_fire_in_insertion_order`]
//! 4. a 0 ms timer does not starve the loop → [`a_self_rearming_zero_ms_chain_does_not_starve_other_tasks`]
//!    and [`the_nesting_clamp_slows_a_runaway_chain_exactly_where_chrome_slows_it`]
use engine::Engine;

/// Boot an engine with a document, eval `src`, drain the event loop, and read `globalThis.__log`.
async fn log_after(src: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("document");
    e.eval("timers-driver", format!("globalThis.__log = [];\nglobalThis.L = (s) => __log.push(String(s));\n{src}"))
        .expect("eval");
    // `run()` resolves when NOTHING is scheduled -- which, with the task source, is exactly when the
    // timer queue has drained. A test that hangs here is a pump that never stops, and that is a bug
    // worth failing on rather than papering over with a budget.
    tokio::time::timeout(std::time::Duration::from_secs(20), e.run())
        .await
        .expect("the timer queue drained (a hang here means the pump never stops)")
        .expect("run");
    let v = e.eval_value("timers-log", "globalThis.__log".to_string()).expect("read log");
    v.as_array()
        .expect("__log is an array")
        .iter()
        .map(|x| x.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Read a number out of the page after the same drain.
async fn number_after(src: &str, expr: &str) -> f64 {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("document");
    e.eval("timers-driver", src.to_string()).expect("eval");
    tokio::time::timeout(std::time::Duration::from_secs(20), e.run())
        .await
        .expect("the timer queue drained")
        .expect("run");
    e.eval_value("timers-num", expr.to_string())
        .expect("read")
        .as_f64()
        .expect("a number")
}

// ── property 1: two timers due together are two tasks ───────────────────────────────────────────────

/// THE REDUCTION the unit was opened on. Chrome answers `t1 mt1 t2`; this engine answered `t1 t2 mt1`,
/// because both callbacks were promise jobs inside one microtask checkpoint.
#[tokio::test]
async fn two_timers_due_together_are_two_tasks() {
    let log = log_after(
        "setTimeout(() => { L('t1'); queueMicrotask(() => L('mt1')); }, 0);\
         setTimeout(() => L('t2'), 0);",
    )
    .await;
    assert_eq!(log, vec!["t1", "mt1", "t2"], "a microtask queued by the first timer must run before the second timer");
}

/// The same property one layer up: `requestAnimationFrame` and `requestIdleCallback` run as tasks of
/// the engine's task source, each with its own microtask checkpoint. Asserting it here is what stops a
/// future "optimisation" putting either back on a promise chain of its own.
///
/// And in Chrome's ORDER (Chrome 150, three and five runs alike, with the last timer at 100
/// and at 30 ms: `t0, raf, mt-raf, ric, mt-ric, t100`): the 0 ms timer first, the frame at its vsync, and the idle
/// callback only in the idle period the frame leaves -- an idle period never begins ahead of a frame that is
/// expected. This engine ran the idle callback first, as a 0 ms task. (The last timer is 100 ms out so that a
/// loaded machine, which can hold the frame past 30 ms, leaves the order to the rule under test.)
#[tokio::test]
async fn request_animation_frame_is_a_task_too() {
    let log = log_after(
        "requestIdleCallback(() => { L('ric'); queueMicrotask(() => L('mt-ric')); });\
         setTimeout(() => L('t0'), 0);\
         requestAnimationFrame(() => { L('raf'); queueMicrotask(() => L('mt-raf')); });\
         setTimeout(() => L('t100'), 100);",
    )
    .await;
    assert_eq!(log, vec!["t0", "raf", "mt-raf", "ric", "mt-ric", "t100"], "rAF/rIC are tasks too: {log:?}");
}

/// AN IDLE CALLBACK POSTED IN A FRAME runs in the idle period that frame leaves, before the next frame; one posted
/// after the frame, with another frame requested, waits for that frame. Chrome 150, five runs alike each: from inside a
/// rAF callback, rIC + a 0 ms timer + rAF run `raf1, t0, ric, mt-ric, raf2, mt-raf2, t100`; from a 0 ms timer after
/// the frame, `raf1, t0, raf2, mt-raf2, ric, mt-ric, t100`.
#[tokio::test]
async fn an_idle_callback_posted_in_a_frame_runs_before_the_next() {
    let log = log_after(
        "requestAnimationFrame(() => {\
           L('raf1');\
           requestIdleCallback(() => { L('ric'); queueMicrotask(() => L('mt-ric')); });\
           setTimeout(() => L('t0'), 0);\
           requestAnimationFrame(() => { L('raf2'); queueMicrotask(() => L('mt-raf2')); });\
           setTimeout(() => L('t100'), 100);\
         });",
    )
    .await;
    assert_eq!(log, vec!["raf1", "t0", "ric", "mt-ric", "raf2", "mt-raf2", "t100"], "an idle callback from a frame: {log:?}");
}

#[tokio::test]
async fn an_idle_callback_posted_after_a_frame_waits_for_the_next() {
    let log = log_after(
        "requestAnimationFrame(() => {\
           L('raf1');\
           setTimeout(() => {\
             L('t0');\
             requestIdleCallback(() => { L('ric'); queueMicrotask(() => L('mt-ric')); });\
             requestAnimationFrame(() => { L('raf2'); queueMicrotask(() => L('mt-raf2')); });\
             setTimeout(() => L('t100'), 100);\
           }, 0);\
         });",
    )
    .await;
    assert_eq!(log, vec!["raf1", "t0", "raf2", "mt-raf2", "ric", "mt-ric", "t100"], "an idle callback after a frame: {log:?}");
}

/// AN IDLE TASK RUNS WHEN NOTHING ELSE IS READY: Blink's idle tasks are the lowest priority, and an idle
/// period does not begin while the next task is less than 1 ms away (`IdleHelper`). Chrome 150, alike over five
/// runs with the last timer at 30 ms and three at 100: a `requestIdleCallback`, then a 0 ms timer, a
/// MessageChannel message and a 1 ms timer run `t0, msg, t1, ric, mt-ric, t100`; this
/// engine ran `ric` first.
#[tokio::test]
async fn an_idle_task_waits_for_every_ready_task() {
    let log = log_after(
        "requestIdleCallback(() => { L('ric'); queueMicrotask(() => L('mt-ric')); });\
         setTimeout(() => L('t0'), 0);\
         setTimeout(() => L('t1'), 1);\
         var mc = new MessageChannel(); mc.port1.onmessage = () => L('msg'); mc.port2.postMessage(0);\
         setTimeout(() => L('t100'), 100);",
    )
    .await;
    assert_eq!(log, vec!["t0", "msg", "t1", "ric", "mt-ric", "t100"], "idle tasks run last: {log:?}");
}

// ── property 2: microtasks drain to exhaustion inside one checkpoint ────────────────────────────────

/// Chrome: `T m1 m2 m3 AFTER`. A microtask that queues another microtask keeps the SAME checkpoint
/// going; the next timer task waits for all of them. This is the property that a task source must not
/// break in the other direction — one task per turn must not become one MICROTASK per turn.
#[tokio::test]
async fn a_microtask_chain_drains_before_the_next_timer_runs() {
    let log = log_after(
        "setTimeout(() => { L('T'); let n = 0; const q = () => { L('m' + (++n)); if (n < 3) queueMicrotask(q); }; queueMicrotask(q); }, 0);\
         setTimeout(() => L('AFTER'), 0);",
    )
    .await;
    assert_eq!(log, vec!["T", "m1", "m2", "m3", "AFTER"], "the microtask chain must drain before the next timer");
}

// ── property 3: equal deadlines fire in insertion order ─────────────────────────────────────────────

/// Chrome: `a b c`. And an EARLIER deadline armed later still wins — the queue is ordered by deadline
/// first, insertion second, which is the pair of keys `scheduler.rs`'s heap entry carries.
///
/// THE GAP IS 50 ms AND IT USED TO BE 5, WHICH MADE THIS TEST MACHINE-SPEED-DEPENDENT. `scheduler.rs`
/// stores an ABSOLUTE deadline (`at: Instant::now() + delay`, ordered `at` then `seq`), so the four
/// `setTimeout` calls race the clock they are arming against: if more than the gap elapses between the
/// first arming and the last — routine while an 8-way workspace run is booting V8 isolates — then
/// `first`'s `now + 0` really is later than `a`'s `t0 + 5`, and `["a","b","c","first"]` is the CORRECT
/// deadline-ordered answer. The engine was right and the test's unstated premise was what failed; it
/// went red once in a gate run and green at the same commit immediately after.
///
/// So the premise is now explicit rather than assumed: the assertion is unchanged, and the gap is wide
/// enough that arming four timers would have to take 50 ms to cross it. This is NOT a widened
/// tolerance — no other ordering is accepted — it removes a dependency on how fast the box is.
#[tokio::test]
async fn equal_deadlines_fire_in_insertion_order() {
    let log = log_after(
        "setTimeout(() => L('a'), 50); setTimeout(() => L('b'), 50); setTimeout(() => L('c'), 50);\
         setTimeout(() => L('first'), 0);",
    )
    .await;
    assert_eq!(log, vec!["first", "a", "b", "c"], "deadline first, insertion order second: {log:?}");
}

/// Insertion order also survives a timer armed from INSIDE another timer at the same deadline.
#[tokio::test]
async fn a_timer_armed_inside_a_timer_queues_behind_what_is_already_due() {
    let log = log_after(
        "setTimeout(() => { L('one'); setTimeout(() => L('three'), 0); }, 0);\
         setTimeout(() => L('two'), 0);",
    )
    .await;
    assert_eq!(log, vec!["one", "two", "three"]);
}

// ── property 4: a 0 ms timer does not starve ────────────────────────────────────────────────────────

/// A self-re-arming `setTimeout(…, 0)` chain must not lock the loop out. Measured before the task
/// source: the WPT file that made the W9.18 unit's harness allocate forever was exactly this shape.
///
/// The assertion is the interleaving, not the wall clock: a plain timer armed at the same moment as the
/// chain's first link must run near the START of the chain, not after all 200 of its links.
#[tokio::test]
async fn a_self_rearming_zero_ms_chain_does_not_starve_other_tasks() {
    let log = log_after(
        "let n = 0; const chain = () => { L('c' + (++n)); if (n < 200) setTimeout(chain, 0); };\
         setTimeout(chain, 0); setTimeout(() => L('other'), 0);",
    )
    .await;
    assert_eq!(log.len(), 201, "every link and the other task ran: {}", log.len());
    let other = log.iter().position(|s| s == "other").expect("the other task ran at all");
    assert!(other <= 2, "the other task must not wait for the whole chain (it ran at {other}): {:?}", &log[..5.min(log.len())]);
}

/// HTML's nesting clamp, the mechanism that makes "does not starve" true of the CLOCK and not only of
/// the queue: "if nesting level is greater than 5, and timeout is less than 4, then set timeout to 4."
///
/// Chrome, measured: `gaps=1,0,0,0,0,0,4,4,4,4,4,4` — a chained `setTimeout(…, 0)` runs flat out for
/// six links and is clamped to 4 ms from the seventh, because the seventh is armed by the sixth task,
/// whose nesting level (6) is the first one greater than 5.
///
/// The assertion is one-sided on purpose: the clamp GUARANTEES at least 6 × 4 ms across links 7-12, so
/// `>= 20 ms` cannot flake high. The other half only has to show the first six were not clamped, and a
/// generous ceiling is enough for that.
#[tokio::test]
async fn the_nesting_clamp_slows_a_runaway_chain_exactly_where_chrome_slows_it() {
    let src = "globalThis.__ts = []; let n = 0;\
               const chain = () => { __ts.push(performance.now()); if (++n < 12) setTimeout(chain, 0); };\
               setTimeout(chain, 0);";
    let early = number_after(src, "__ts[5] - __ts[0]").await;
    let late = number_after(src, "__ts[11] - __ts[5]").await;
    assert!(late >= 20.0, "links 7-12 must each be clamped to 4 ms (measured {late} ms across six)");
    assert!(early < 20.0, "links 1-6 must not be clamped (measured {early} ms across six)");
}

/// `setInterval` carries the same nesting level forward per tick, so a 0 ms interval is clamped from
/// its seventh tick exactly as the chain is. Chrome: `interval gaps=0,0,0,0,0,0,4,4,4,4,4,4`.
#[tokio::test]
async fn a_zero_ms_interval_is_clamped_from_its_seventh_tick() {
    let src = "globalThis.__ts = []; let n = 0;\
               const iv = setInterval(() => { __ts.push(performance.now()); if (++n === 12) clearInterval(iv); }, 0);";
    let late = number_after(src, "__ts[11] - __ts[5]").await;
    assert!(late >= 20.0, "ticks 7-12 of a 0 ms interval must be clamped to 4 ms (measured {late} ms)");
}

// ── the timer registry itself ───────────────────────────────────────────────────────────────────────

/// A timer cleared from a microtask queued by an EARLIER timer must never run — Chrome probe,
/// 2026-09-11: `p1 | cleared | p3`, never `SHOULD-NOT-RUN`. This is why the task source re-checks the
/// registry when it RUNS a timer rather than when it dequeues it.
#[tokio::test]
async fn a_timer_cleared_from_a_microtask_never_runs() {
    let log = log_after(
        "setTimeout(() => { L('p1'); queueMicrotask(() => { clearTimeout(h); L('cleared'); }); }, 0);\
         const h = setTimeout(() => L('SHOULD-NOT-RUN'), 0);\
         setTimeout(() => L('p3'), 0);",
    )
    .await;
    assert_eq!(log, vec!["p1", "cleared", "p3"]);
}

/// `setInterval` repeats and `clearInterval` stops it; the handle is the same id space as
/// `setTimeout`'s, and every id is distinct and non-zero (0 is the pump's "queue empty" answer, so it
/// must never be handed to a page).
#[tokio::test]
async fn set_interval_repeats_until_cleared_and_ids_are_distinct_and_nonzero() {
    let log = log_after(
        "const a = setTimeout(() => {}, 1000); const b = setInterval(() => {}, 1000);\
         L(a !== b && a > 0 && b > 0); clearTimeout(a); clearInterval(b);\
         let n = 0; const iv = setInterval(() => { L('tick' + (++n)); if (n === 3) clearInterval(iv); }, 1);",
    )
    .await;
    assert_eq!(log, vec!["true", "tick1", "tick2", "tick3"], "interval must repeat exactly until cleared");
}

/// A throwing timer callback is REPORTED to the page (HTML "report an exception" → an `ErrorEvent` at
/// the global), not swallowed and not propagated into the pump: the timers after it still run.
#[tokio::test]
async fn a_throwing_timer_callback_is_reported_and_the_next_timer_still_runs() {
    let log = log_after(
        "window.addEventListener('error', (e) => L('onerror:' + e.message));\
         setTimeout(() => { throw new Error('boom'); }, 0);\
         setTimeout(() => L('after'), 0);",
    )
    .await;
    // `Uncaught Error: boom` is Chrome 150's ErrorEvent.message for a timer callback's throw (measured
    // over loopback: `Uncaught Error: in timeout`); the bare `boom` was this engine's.
    assert_eq!(log, vec!["onerror:Uncaught Error: boom", "after"], "a throwing timer must not take the pump with it: {log:?}");
}

/// Nonsense delays are the WebIDL `long` conversion, not a panic: negative fires immediately, and a
/// value outside int32 wraps (Chrome fires `setTimeout(f, 2**32)` immediately for the same reason).
#[tokio::test]
async fn nonsense_delays_are_the_webidl_long_conversion() {
    let log = log_after(
        "setTimeout(() => L('neg'), -1); setTimeout(() => L('nan'), NaN);\
         setTimeout(() => L('wrap'), 4294967296); setTimeout(() => L('undef'));",
    )
    .await;
    assert_eq!(log, vec!["neg", "nan", "wrap", "undef"], "all four are due immediately, in insertion order");
}

/// The extra arguments `setTimeout(cb, ms, …args)` carries reach the callback.
#[tokio::test]
async fn timer_arguments_reach_the_callback() {
    let log = log_after("setTimeout((a, b) => L(a + ':' + b), 0, 'x', 7);").await;
    assert_eq!(log, vec!["x:7"]);
}

/// A TIMER CALLBACK'S `this` IS THE WINDOWPROXY. HTML's "invoke the callback" passes the
/// global as the callback this value, so a callback sees `window` whatever its strictness. This task
/// source passed `undefined`, which a SLOPPY callback never noticed -- V8 coerces an undefined
/// receiver to the global proxy for it -- and which a STRICT one saw exactly.
///
/// Measured on `google-chrome --headless=new` 141, 2026-09-12, with callbacks that all declare
/// `"use strict"`:
///
/// ```text
/// setTimeout -> window   setInterval -> window   queueMicrotask -> undefined   rAF -> undefined
/// ```
///
/// The last two are in the reduction because they are the BOUNDARY: they are `invoke`d with no
/// callback this value, so this is the timer task source's rule and not a rule about every callback.
#[tokio::test]
async fn a_timer_callbacks_this_is_the_window() {
    let log = log_after(
        "const show = (v) => v === globalThis ? 'window' : (v === undefined ? 'undefined' : typeof v);\
         setTimeout(function () { 'use strict'; L('timeoutStrict=' + show(this)); }, 0);\
         setTimeout(function () { L('timeoutSloppy=' + show(this)); }, 0);\
         let iv; iv = setInterval(function () { 'use strict'; L('intervalStrict=' + show(this)); clearInterval(iv); }, 0);\
         queueMicrotask(function () { 'use strict'; L('microtask=' + show(this)); });\
         requestAnimationFrame(function () { 'use strict'; L('raf=' + show(this)); });",
    )
    .await;
    let mut log = log;
    log.sort();
    assert_eq!(
        log,
        vec![
            "intervalStrict=window",
            "microtask=undefined",
            "raf=undefined",
            "timeoutSloppy=window",
            "timeoutStrict=window",
        ]
    );
}

// ── the five members' own SHAPE ────────────────────────────────────────────────────────
//
// The four ordering properties above were true before this unit and are true after it. What the unit
// changed is WHERE the five members come from: they were JS arrows in `engine.rs`'s `BOOTSTRAP` string
// behind `op_nativize`, and they are five rows of the `Window` table with native bodies in
// `core/korvus-runtime/src/scope_timers.rs` now. Nothing above would have noticed the difference —
// the mask made `String(setTimeout)` read `[native code]` either way — so the shape is pinned here.
//
// THE ORACLE for this block is `google-chrome --headless=new` 150.0.7871.46, 2026-09-12, on a page
// that read every descriptor and caught every throw:
//
// ```text
// setTimeout     own len=1 {w:true,e:true,c:true}   Window.prototype owns none of the five
// clearTimeout   own len=0 {w:true,e:true,c:true}   (clearTimeout(optional long handle = 0))
// setTimeout()   TypeError: Failed to execute 'setTimeout' on 'Window': 1 argument required, but only 0 present.
// queueMicrotask(1)        …on 'Window': parameter 1 is not of type 'Function'.
// clearTimeout()           undefined          setTimeout.call({}, f, 0)  TypeError: Illegal invocation
// setTimeout.call(null, f, 0)  a handle       new setTimeout(f, 0)       TypeError: setTimeout is not a constructor
// ```

/// Evaluate one expression in a booted page realm and read the string it answers.
async fn expr(src: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("document");
    let v = e.eval_value("timers-shape", src.to_string()).expect("eval");
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

/// Every column Chrome reports for the five, in one line each: WebIDL `length`, the function's `name`,
/// the own-property descriptor on `window`, whether the body is native, whether `Window.prototype`
/// carries a copy (a `[Global]` interface's members are own properties of the GLOBAL and of nothing
/// else), and the function's OWN property names — a JS-shim carries `prototype`, a real DOM method
/// does not.
///
/// `queueMicrotask`'s `enumerable` is the column this unit moved: deno_core installs one, `BOOTSTRAP`
/// reached the slot by ASSIGNMENT, and assigning to an existing property keeps its attributes — so the
/// engine's was `enumerable: false` where Chrome's is true, and `for (const k in window)` skipped it.
#[tokio::test]
async fn the_five_timer_members_have_chromes_shape() {
    let got = expr(
        "['setTimeout','clearTimeout','setInterval','clearInterval','queueMicrotask'].map(n => {\
           const d = Object.getOwnPropertyDescriptor(globalThis, n); const f = globalThis[n];\
           return n + ' len=' + f.length + ' name=' + f.name\
             + ' ' + (d.writable?'w':'-') + (d.enumerable?'e':'-') + (d.configurable?'c':'-')\
             + ' ' + (String(f).indexOf('[native code]') >= 0 ? 'native' : 'JS-SOURCE')\
             + ' protoOwn=' + Window.prototype.hasOwnProperty(n)\
             + ' ownProps=' + Object.getOwnPropertyNames(f).sort().join('|');\
         }).join('\\n')",
    )
    .await;
    assert_eq!(
        got,
        "setTimeout len=1 name=setTimeout wec native protoOwn=false ownProps=length|name\n\
         clearTimeout len=0 name=clearTimeout wec native protoOwn=false ownProps=length|name\n\
         setInterval len=1 name=setInterval wec native protoOwn=false ownProps=length|name\n\
         clearInterval len=0 name=clearInterval wec native protoOwn=false ownProps=length|name\n\
         queueMicrotask len=1 name=queueMicrotask wec native protoOwn=false ownProps=length|name"
    );
}

/// The WebIDL argument checks, word for word. `clearTimeout`/`clearInterval` take an OPTIONAL handle,
/// so a bare call is legal and answers undefined; the other three require one argument, and
/// `queueMicrotask` additionally requires it to be callable — a different sentence, which is why the
/// bodies throw these themselves rather than leaning on the generic arity guard.
#[tokio::test]
async fn the_five_timer_members_throw_chromes_webidl_sentences() {
    let got = expr(
        "const t=(f)=>{try{const r=f();return 'OK:'+String(r);}catch(e){return e.constructor.name+': '+e.message;}};\
         [t(()=>setTimeout()), t(()=>setInterval()), t(()=>queueMicrotask()),\
          t(()=>queueMicrotask(1)), t(()=>queueMicrotask(null)),\
          t(()=>clearTimeout()), t(()=>clearInterval())].join('\\n')",
    )
    .await;
    assert_eq!(
        got,
        "TypeError: Failed to execute 'setTimeout' on 'Window': 1 argument required, but only 0 present.\n\
         TypeError: Failed to execute 'setInterval' on 'Window': 1 argument required, but only 0 present.\n\
         TypeError: Failed to execute 'queueMicrotask' on 'Window': 1 argument required, but only 0 present.\n\
         TypeError: Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'.\n\
         TypeError: Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'.\n\
         OK:undefined\n\
         OK:undefined"
    );
}

/// The receiver rule, which the JS bodies had no way to enforce: a FOREIGN receiver is Chrome's
/// `Illegal invocation` for all five, a null/undefined receiver is the global proxy and therefore
/// legal, a non-callable handler is a legal call that never does anything (`setTimeout(1)` answers a
/// handle), and none of the five is a constructor.
#[tokio::test]
async fn the_five_timer_members_check_their_receiver_the_way_chrome_does() {
    let got = expr(
        "const t=(f)=>{try{const r=f();return 'OK:'+String(r);}catch(e){return e.constructor.name+': '+e.message;}};\
         const h=(v)=>{clearTimeout(v);return typeof v==='number'&&v>0?'handle':'BAD:'+String(v);};\
         [t(()=>h(setTimeout.call({}, ()=>{}, 0))), t(()=>h(setInterval.call({}, ()=>{}, 0))),\
          t(()=>clearTimeout.call({}, 1)), t(()=>clearInterval.call({}, 1)),\
          t(()=>queueMicrotask.call({}, ()=>{})),\
          t(()=>h(setTimeout.call(null, ()=>{}, 0))), t(()=>h(setTimeout(1))),\
          t(()=>new setTimeout(()=>{}, 0))].join('\\n')",
    )
    .await;
    assert_eq!(
        got,
        "TypeError: Illegal invocation\n\
         TypeError: Illegal invocation\n\
         TypeError: Illegal invocation\n\
         TypeError: Illegal invocation\n\
         TypeError: Illegal invocation\n\
         OK:handle\n\
         OK:handle\n\
         TypeError: setTimeout is not a constructor"
    );
}

// ── the worker realm runs the SAME task source (invariant I8) ───────────────────────────────────────

/// The worker had its own `setTimeout` with the same promise-chain defect and a `setInterval` that
/// answered 0 and never fired. Both realms are the one kernel queue now, so the reduction answers the
/// same thing here — which is what makes a WPT `.any.js` file's `.any.worker.html` scope agree with its
/// window scope.
#[tokio::test]
async fn the_worker_realm_orders_timers_the_same_way() {
    const W: &str = "self.onmessage = () => { const r = [];\
        setTimeout(() => { r.push('t1'); queueMicrotask(() => r.push('mt1')); }, 0);\
        setTimeout(() => { r.push('t2'); setTimeout(() => postMessage(r.join(',')), 0); }, 0); };";
    assert_eq!(worker_says(W).await, "t1,mt1,t2");
}

/// …and the two realms run the same five BODIES, differing only in the HOLDER name WebIDL puts in the
/// message. This is the assertion that makes `scope_timers.rs` one file rather than two:
/// if a realm's copy ever drifted, the sentence is where it would show.
///
/// Measured in `google-chrome --headless=new` 150.0.7871.46, 2026-09-12, inside a Blob worker:
///
/// ```text
/// setTimeout()       Failed to execute 'setTimeout' on 'WorkerGlobalScope': 1 argument required, but only 0 present.
/// queueMicrotask(1)  Failed to execute 'queueMicrotask' on 'WorkerGlobalScope': parameter 1 is not of type 'Function'.
/// clearTimeout()     undefined      setTimeout.length 1     clearTimeout.length 0
/// ```
#[tokio::test]
async fn the_worker_realm_names_its_own_holder_in_the_same_sentences() {
    const W: &str = "self.onmessage = () => {\
        const t=(f)=>{try{return 'OK:'+String(f());}catch(e){return e.message;}};\
        postMessage([t(()=>setTimeout()), t(()=>queueMicrotask(1)), t(()=>clearTimeout()),\
                     'len ' + setTimeout.length + clearTimeout.length + setInterval.length\
                            + clearInterval.length + queueMicrotask.length].join(' | ')); };";
    assert_eq!(
        worker_says(W).await,
        "Failed to execute 'setTimeout' on 'WorkerGlobalScope': 1 argument required, but only 0 present. | \
         Failed to execute 'queueMicrotask' on 'WorkerGlobalScope': parameter 1 is not of type 'Function'. | \
         OK:undefined | len 10101"
    );
}

/// …and `setInterval` in a worker actually repeats now.
#[tokio::test]
async fn the_worker_realm_has_a_real_set_interval() {
    const W: &str = "self.onmessage = () => { let n = 0;\
        const iv = setInterval(() => { if (++n === 3) { clearInterval(iv); postMessage('ticks:' + n); } }, 1); };";
    assert_eq!(worker_says(W).await, "ticks:3");
}

/// Spawn a real worker isolate running `src`, poke it once, and return the single string it posts back.
async fn worker_says(src: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("document");
    e.eval(
        "worker-scripts",
        format!("globalThis.__workerScripts = {{ \"./t.js\": {} }};", serde_json::to_string(src).unwrap()),
    )
    .expect("scripts");
    e.run().await.expect("run");
    e.eval(
        "worker-driver",
        "const w = new Worker('./t.js'); w.onmessage = (e) => print('RESULT:' + e.data); w.postMessage(1);"
            .to_string(),
    )
    .expect("driver");
    let mut out: Vec<String> = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out.iter()
        .find_map(|l| l.strip_prefix("RESULT:").map(str::to_string))
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

/// Timer handles are PER REALM, and only `setTimeout` / `setInterval` take one. Chrome 150, measured
/// 2026-10-07 (`--dump-dom`): the engine's own tasks (a `postMessage` delivery) take no handle, and a
/// frame's first two `setTimeout`s answer [1, 2] beside the page's [1, 2]; the page's `clearTimeout(1)`
/// does not cancel the frame's timer 1. korvus answered the frame [3, 4] (korvus-analysis U-2).
#[tokio::test]
async fn timer_handles_are_per_realm_and_engine_tasks_take_none() {
    let log = log_after(
        r#"
        window.postMessage("x", "*");
        L("page " + setTimeout(() => {}, 0) + "," + setTimeout(() => {}, 0));
        const f = document.createElement("iframe");
        f.srcdoc = "<script>parent.__frame = [setTimeout(() => { parent.__fired = 1; }, 30), setTimeout(() => {}, 0)];<\/script>";
        f.onload = () => {
            L("frame " + window.__frame.join(","));
            clearTimeout(1);
            L("page next " + setTimeout(() => {}, 0));
            setTimeout(() => L("frame timer 1 fired " + !!window.__fired), 100);
        };
        document.body.appendChild(f);
        "#,
    )
    .await;
    assert_eq!(log, vec!["page 1,2", "frame 1,2", "page next 3", "frame timer 1 fired true"]);
}

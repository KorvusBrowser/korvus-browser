//! `window.scheduler` on the LIVE surface, against the browsers' own answers.
//!
//! FP5 deferred this name with one sentence — **`postTask` must RUN the task and resolve with its
//! value** — and every assertion here is about ordering, because ordering is the whole of what a
//! scheduler is. The orderings were read off Chrome 150.0.7871.46 and Brave 151.1.93.138, headed over
//! CDP under Xvfb, at two origins, 2026-09-19/20, and both browsers agreed on every one.
//!
//! The engine's queue is one realm-private list plus one pump scheduled through the realm's own
//! `setTimeout`, so a scheduler task IS a task of this event loop; these tests are what says that
//! choice reproduces the device rather than merely resembling it.

use engine::Engine;
use std::time::Duration;

const URL: &str = "http://127.0.0.1:8123/page.html";
const DOC: &str = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";

/// Evaluate `js` and WAIT for its promise: every question here is about when something runs.
async fn page(js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(DOC, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value_settled("sch", js.to_string(), Duration::from_secs(10)).await.expect("eval")
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().expect("a string").to_string()).collect()
}

/// The singleton and the interface object, exactly as the captures record them.
#[tokio::test]
async fn the_singleton_and_the_interface_are_the_browsers() {
    let v = page(
        r#"(async () => ({
            own: Object.prototype.hasOwnProperty.call(window, 'scheduler'),
            tag: Object.prototype.toString.call(scheduler),
            same: window.scheduler === window.scheduler,
            // [Replaceable]: get AND set, which is where it differs from `trustedTypes` next door.
            desc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(window, 'scheduler')),
            proto: Object.getOwnPropertyNames(Scheduler.prototype),
            lengths: [Scheduler.prototype.postTask.length, Scheduler.prototype.yield.length],
            newThrows: (() => { try { new Scheduler(); return 'no throw'; }
                                catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            chain: (() => { const a = []; let p = scheduler;
                            while ((p = Object.getPrototypeOf(p))) a.push(p.constructor.name); return a; })(),
        }))()"#,
    )
    .await;
    assert_eq!(v["own"], true);
    assert_eq!(v["tag"], "[object Scheduler]");
    assert_eq!(v["same"], true);
    assert_eq!(v["desc"], serde_json::json!([true, true, true, true]));
    assert_eq!(strings(&v["proto"]), ["postTask", "yield", "constructor"]);
    assert_eq!(v["lengths"], serde_json::json!([1, 0]));
    assert_eq!(v["newThrows"], "TypeError: Failed to construct 'Scheduler': Illegal constructor");
    assert_eq!(strings(&v["chain"]), ["Scheduler", "Object"]);
}

/// THE ROW FP5 NAMED: `postTask` runs the task and resolves with its value.
#[tokio::test]
async fn post_task_runs_the_task_and_resolves_with_its_value() {
    let v = page(
        r#"(async () => {
            const caught = async (p) => { try { return 'ok:' + JSON.stringify(await p); }
                                          catch (e) { return e.name + ': ' + e.message; } };
            return {
                isPromise: Object.prototype.toString.call(scheduler.postTask(() => 1)),
                value: await scheduler.postTask(() => 42),
                object: await caught(scheduler.postTask(() => ({ a: 1 }))),
                asyncCb: await scheduler.postTask(async () => { await 0; return 'async-done'; }),
                thenable: await scheduler.postTask(() => ({ then: (r) => r('flattened') })),
                threw: await caught(scheduler.postTask(() => { throw new TypeError('inner'); })),
                args: await scheduler.postTask((...a) => a.length),
                self: await scheduler.postTask(function () {
                    return this === undefined ? 'undefined' : Object.prototype.toString.call(this); }),
                priorities: await Promise.all(['user-blocking', 'user-visible', 'background']
                    .map(p => scheduler.postTask(() => p, { priority: p }))),
                delay: await scheduler.postTask(() => 'delayed', { delay: 5 }),
                yieldIsPromise: Object.prototype.toString.call(scheduler.yield()),
                yielded: await scheduler.yield().then(() => 'yielded'),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["isPromise"], "[object Promise]");
    assert_eq!(v["value"], 42, "the task RAN and the promise carries its return");
    assert_eq!(v["object"], r#"ok:{"a":1}"#);
    assert_eq!(v["asyncCb"], "async-done");
    assert_eq!(v["thenable"], "flattened", "a thenable return is flattened");
    assert_eq!(v["threw"], "TypeError: inner", "a throw becomes the rejection, unchanged");
    assert_eq!(v["args"], 0, "the callback gets NO arguments");
    assert_eq!(v["self"], "[object Window]", "an undefined receiver, so a sloppy callback sees the global");
    assert_eq!(strings(&v["priorities"]), ["user-blocking", "user-visible", "background"]);
    assert_eq!(v["delay"], "delayed");
    assert_eq!(v["yieldIsPromise"], "[object Promise]");
    assert_eq!(v["yielded"], "yielded");
}

/// THE ORDERINGS, which are the whole of what a scheduler is.
#[tokio::test]
async fn the_orderings_are_the_browsers() {
    let v = page(
        r#"(async () => {
            const settle = () => new Promise(r => setTimeout(r, 60));
            const out = {};
            {   // priority: posted bg, ub, uv -> runs ub, uv, bg
                const log = [];
                scheduler.postTask(() => log.push('bg'), { priority: 'background' });
                scheduler.postTask(() => log.push('ub'), { priority: 'user-blocking' });
                scheduler.postTask(() => log.push('uv'), { priority: 'user-visible' });
                await settle(); out.priority = log.join(',');
            }
            {   // the DEFAULT priority sits between the other two
                const log = [];
                scheduler.postTask(() => log.push('bg'), { priority: 'background' });
                scheduler.postTask(() => log.push('default'));
                scheduler.postTask(() => log.push('ub'), { priority: 'user-blocking' });
                await settle(); out.defaultPriority = log.join(',');
            }
            {   // a microtask runs before a task
                const log = [];
                Promise.resolve().then(() => log.push('micro'));
                scheduler.postTask(() => log.push('task'));
                await settle(); out.vsMicrotask = log.join(',');
            }
            {   // a setTimeout registered FIRST runs first
                const log = [];
                setTimeout(() => log.push('to'), 0);
                scheduler.postTask(() => log.push('pt'));
                await settle(); out.vsSetTimeout = log.join(',');
            }
            {   // a task posted INSIDE a task runs after it
                const log = [];
                await scheduler.postTask(() => {
                    scheduler.postTask(() => log.push('inner'));
                    log.push('outer');
                });
                await settle(); out.nested = log.join(',');
            }
            {   // a delayed task posted FIRST still runs second
                const log = [];
                scheduler.postTask(() => log.push('d10'), { delay: 10 });
                scheduler.postTask(() => log.push('d0'));
                await settle(); out.delay = log.join(',');
            }
            {   // yield() is a CONTINUATION: it resumes before every pending task, inside a task
                const log = [];
                scheduler.postTask(() => log.push('bg'), { priority: 'background' });
                scheduler.postTask(() => log.push('uv'));
                await scheduler.postTask(async () => {
                    log.push('task-start');
                    await scheduler.yield();
                    log.push('after-yield');
                }, { priority: 'user-blocking' });
                await settle(); out.yieldInTask = log.join(',');
            }
            {   // ... and outside one
                const log = [];
                scheduler.postTask(() => log.push('uv'));
                await scheduler.yield();
                log.push('yielded');
                await settle(); out.yieldOutside = log.join(',');
            }
            return out;
        })()"#,
    )
    .await;
    assert_eq!(v["priority"], "ub,uv,bg");
    assert_eq!(v["defaultPriority"], "ub,default,bg", "the default is `user-visible`");
    assert_eq!(v["vsMicrotask"], "micro,task");
    assert_eq!(v["vsSetTimeout"], "to,pt");
    assert_eq!(v["nested"], "outer,inner");
    assert_eq!(v["delay"], "d0,d10");
    assert_eq!(v["yieldInTask"], "task-start,after-yield,uv,bg", "a yield resumes before a pending task");
    assert_eq!(v["yieldOutside"], "yielded,uv");
}

/// `signal`, and every failure as a REJECTION rather than a throw.
#[tokio::test]
async fn a_signal_aborts_and_every_failure_rejects() {
    let v = page(
        r#"(async () => {
            const caught = async (p) => { try { return 'resolved:' + await p; }
                                          catch (e) { return e.name + ': ' + e.message; } };
            const sync = (f) => { try { f(); return 'no throw'; } catch (e) { return 'THREW ' + e.name; } };
            const before = new AbortController(); before.abort();
            const after = new AbortController();
            const afterP = scheduler.postTask(() => 'ran', { signal: after.signal });
            after.abort();
            const reasoned = new AbortController();
            const reasonedP = scheduler.postTask(() => 'ran', { signal: reasoned.signal });
            reasoned.abort(new TypeError('why'));
            const shared = new AbortController();
            const sharedA = scheduler.postTask(() => 'a', { signal: shared.signal });
            const sharedB = scheduler.postTask(() => 'b', { signal: shared.signal });
            shared.abort();
            const selfAbort = new AbortController();
            const selfP = scheduler.postTask(() => { selfAbort.abort(); return 'ran'; }, { signal: selfAbort.signal });
            return {
                abortedBefore: await caught(scheduler.postTask(() => 'ran', { signal: before.signal })),
                abortedAfter: await caught(afterP),
                withReason: await caught(reasonedP),
                sharedA: await caught(sharedA),
                sharedB: await caught(sharedB),
                abortDuringRun: await caught(selfP),
                noCallback: await caught(scheduler.postTask()),
                nonCallable: await caught(scheduler.postTask(1)),
                badPriority: await caught(scheduler.postTask(() => 1, { priority: 'bogus' })),
                negativeDelay: await caught(scheduler.postTask(() => 1, { delay: -1 })),
                optionsNotObject: await caught(scheduler.postTask(() => 1, 5)),
                optionsNull: await caught(scheduler.postTask(() => 'ok', null)),
                priorityUndefined: await caught(scheduler.postTask(() => 'ok', { priority: undefined })),
                delayString: await caught(scheduler.postTask(() => 'ok', { delay: '5' })),
                delayFloat: await caught(scheduler.postTask(() => 'ok', { delay: 1.9 })),
                badSignal: await caught(scheduler.postTask(() => 1, { signal: {} })),
                foreignPost: await caught(Scheduler.prototype.postTask.call({}, () => 1)),
                foreignYield: await caught(Scheduler.prototype.yield.call({})),
                // Both rows RETURN a promise even for a foreign receiver -- they never throw.
                foreignIsPromise: sync(() => Scheduler.prototype.postTask.call({}, () => 1).catch(() => {})),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["abortedBefore"], "AbortError: signal is aborted without reason");
    assert_eq!(v["abortedAfter"], "AbortError: signal is aborted without reason");
    assert_eq!(v["withReason"], "TypeError: why", "the signal's own reason is the rejection");
    assert_eq!(v["sharedA"], "AbortError: signal is aborted without reason");
    assert_eq!(v["sharedB"], "AbortError: signal is aborted without reason");
    assert_eq!(v["abortDuringRun"], "AbortError: signal is aborted without reason",
        "a callback that aborts its own signal still rejects");
    assert_eq!(
        v["noCallback"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': 1 argument required, but only 0 present."
    );
    assert_eq!(
        v["nonCallable"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': parameter 1 is not of type 'Function'."
    );
    assert_eq!(
        v["badPriority"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': Failed to read the 'priority' property \
         from 'SchedulerPostTaskOptions': The provided value 'bogus' is not a valid enum value of type \
         TaskPriority."
    );
    assert_eq!(
        v["negativeDelay"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': Failed to read the 'delay' property from \
         'SchedulerPostTaskOptions': Value is outside the 'unsigned long long' value range."
    );
    assert_eq!(
        v["optionsNotObject"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': The provided value is not of type \
         'SchedulerPostTaskOptions'."
    );
    assert_eq!(v["optionsNull"], "resolved:ok", "`null` options are no options");
    assert_eq!(v["priorityUndefined"], "resolved:ok");
    assert_eq!(v["delayString"], "resolved:ok");
    assert_eq!(v["delayFloat"], "resolved:ok");
    assert_eq!(
        v["badSignal"],
        "TypeError: Failed to execute 'postTask' on 'Scheduler': Failed to read the 'signal' property from \
         'SchedulerPostTaskOptions': Failed to convert value to 'AbortSignal'."
    );
    assert_eq!(v["foreignPost"], "TypeError: Failed to execute 'postTask' on 'Scheduler': Illegal invocation");
    assert_eq!(v["foreignYield"], "TypeError: Failed to execute 'yield' on 'Scheduler': Illegal invocation");
    assert_eq!(v["foreignIsPromise"], "no throw");
}

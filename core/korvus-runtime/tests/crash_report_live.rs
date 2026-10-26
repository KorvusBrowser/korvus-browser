//! `window.crashReport` and `window.documentPictureInPicture` on the LIVE surface, against the four
//! browsers' own answers.
//!
//! Both were globals FP5 measured missing and deliberately left out, for one reason each:
//! `crashReport.set()` must throw the device's `InvalidStateError` until `initialize()` has settled,
//! and `documentPictureInPicture.window` must be **`null`** rather than a D2 stub's `undefined`.
//!
//! Every expectation below was read off Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93
//! and Opera 135.0.5973.133 — headed over CDP under Xvfb, 2026-09-19, at a loopback and a
//! non-loopback origin — and all four agreed on every one.
//!
//! The budget sweep is the part that is a RULE rather than a number. One `initialize` is allowed per
//! DOCUMENT, so the browsers were swept with one same-origin iframe per buffer size (sixteen sizes,
//! four operations each) and the boundaries pin `2 + SUM(key.len + value.len + 5)`. The engine gets
//! one `Engine` per size here for the same reason.

use engine::Engine;
use std::time::Duration;

const URL: &str = "http://127.0.0.1:8123/page.html";
const DOC: &str = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";

/// A fresh realm — so a fresh `crashReport`, whose `initialize` may be called once — and `js` in it.
///
/// `eval_value_settled`, not `eval_value`: every question here is about when a promise settles, so
/// the harness has to pump the loop and wait for one. `eval_value` hands back `null` for a promise,
/// which is what four of these tests read on their first run.
async fn fresh(js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(DOC, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value_settled("cr", js.to_string(), Duration::from_secs(10)).await.expect("eval")
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().expect("a string").to_string()).collect()
}

/// The shape of both singletons and of the two interface objects, exactly as the captures record them.
#[tokio::test]
async fn the_two_singletons_have_the_browsers_shape() {
    let v = fresh(
        r#"({
            crOwn: Object.prototype.hasOwnProperty.call(window, 'crashReport'),
            crTag: Object.prototype.toString.call(crashReport),
            crSame: window.crashReport === window.crashReport,
            crDesc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(window, 'crashReport')),
            crChain: (() => { const a = []; let p = crashReport;
                              while ((p = Object.getPrototypeOf(p))) a.push(p.constructor.name); return a; })(),
            crProto: Object.getOwnPropertyNames(CrashReportContext.prototype),
            crLens: ['delete', 'initialize', 'set'].map(n => CrashReportContext.prototype[n].length),
            crNew: (() => { try { new CrashReportContext(); return 'no throw'; }
                            catch (e) { return e.constructor.name + ': ' + e.message; } })(),

            dpOwn: Object.prototype.hasOwnProperty.call(window, 'documentPictureInPicture'),
            dpTag: Object.prototype.toString.call(documentPictureInPicture),
            dpSame: window.documentPictureInPicture === window.documentPictureInPicture,
            dpDesc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(window, 'documentPictureInPicture')),
            dpChain: (() => { const a = []; let p = documentPictureInPicture;
                              while ((p = Object.getPrototypeOf(p))) a.push(p.constructor.name); return a; })(),
            dpProto: Object.getOwnPropertyNames(DocumentPictureInPicture.prototype),
            dpNew: (() => { try { new DocumentPictureInPicture(); return 'no throw'; }
                            catch (e) { return e.constructor.name + ': ' + e.message; } })(),
        })"#,
    )
    .await;
    assert_eq!(v["crOwn"], true);
    assert_eq!(v["crTag"], "[object CrashReportContext]");
    assert_eq!(v["crSame"], true);
    // A readonly attribute: get, no set, e=1 c=1 — measured on all four.
    assert_eq!(v["crDesc"], serde_json::json!([true, false, true, true]));
    // NOT an EventTarget, which is the one place it differs from `documentPictureInPicture`.
    assert_eq!(strings(&v["crChain"]), ["CrashReportContext", "Object"]);
    // Alphabetical, which is not the IDL's order, with `constructor` at the capture's index 3.
    assert_eq!(strings(&v["crProto"]), ["delete", "initialize", "set", "constructor"]);
    assert_eq!(v["crLens"], serde_json::json!([1, 1, 2]));
    assert_eq!(v["crNew"], "TypeError: Failed to construct 'CrashReportContext': Illegal constructor");

    assert_eq!(v["dpOwn"], true);
    assert_eq!(v["dpTag"], "[object DocumentPictureInPicture]");
    assert_eq!(v["dpSame"], true);
    assert_eq!(v["dpDesc"], serde_json::json!([true, false, true, true]));
    assert_eq!(strings(&v["dpChain"]), ["DocumentPictureInPicture", "EventTarget", "Object"]);
    assert_eq!(strings(&v["dpProto"]), ["window", "onenter", "requestWindow", "constructor"]);
    assert_eq!(v["dpNew"], "TypeError: Failed to construct 'DocumentPictureInPicture': Illegal constructor");
}

/// `documentPictureInPicture.window` is **`null`** — the one value FP5 deferred the whole name for —
/// and `requestWindow()` refuses the way the device refuses.
#[tokio::test]
async fn document_pip_answers_null_and_refuses_for_the_devices_reason() {
    let v = fresh(
        r#"(async () => {
            const dp = documentPictureInPicture;
            const caught = async (p) => { try { await p; return 'resolved'; }
                                          catch (e) { return e.name + ': ' + e.message; } };
            const log = [];
            dp.addEventListener('enter', () => log.push('listener'));
            dp.onenter = () => log.push('handler');
            dp.dispatchEvent(new Event('enter'));
            return {
                window: dp.window, windowType: typeof dp.window, isNull: dp.window === null,
                onenterAfter: typeof dp.onenter,
                cleared: (dp.onenter = null, dp.onenter),
                log,
                request: await caught(dp.requestWindow()),
                requestTag: Object.prototype.toString.call(dp.requestWindow().catch(() => {})),
                foreign: await caught(DocumentPictureInPicture.prototype.requestWindow.call({})),
                foreignGetter: (() => { try {
                        Object.getOwnPropertyDescriptor(DocumentPictureInPicture.prototype, 'window').get.call({});
                        return 'no throw';
                    } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["window"], serde_json::Value::Null, "`.window` must be null, NOT undefined");
    assert_eq!(v["windowType"], "object");
    assert_eq!(v["isNull"], true);
    assert_eq!(v["onenterAfter"], "function");
    assert_eq!(v["cleared"], serde_json::Value::Null);
    assert_eq!(strings(&v["log"]), ["listener", "handler"], "it is a real EventTarget");
    assert_eq!(
        v["request"],
        "NotAllowedError: Failed to execute 'requestWindow' on 'DocumentPictureInPicture': \
         Document PiP requires user activation"
    );
    // It REJECTS rather than throwing, including for a foreign receiver — a `Promise`-returning
    // operation converts its own receiver error, which is why the device hands back a Promise here.
    assert_eq!(v["requestTag"], "[object Promise]");
    assert_eq!(
        v["foreign"],
        "TypeError: Failed to execute 'requestWindow' on 'DocumentPictureInPicture': Illegal invocation"
    );
    // The plain attribute getter does throw.
    assert_eq!(v["foreignGetter"], "TypeError: Illegal invocation");
}

/// `set`/`delete` before `initialize` has SETTLED, and the arity and receiver errors around them.
#[tokio::test]
async fn crash_report_is_not_initialized_until_the_promise_settles() {
    let v = fresh(
        r#"(async () => {
            const cr = crashReport;
            const sync = (f) => { try { f(); return 'ok'; } catch (e) { return e.name + ': ' + e.message; } };
            const caught = async (p) => { try { await p; return 'resolved'; }
                                          catch (e) { return e.name + ': ' + e.message; } };
            const before = sync(() => cr.set('a', 'b'));
            const beforeDel = sync(() => cr.delete('a'));
            const noArgs = await caught(cr.initialize());
            const foreignInit = await caught(CrashReportContext.prototype.initialize.call({}, 1));
            const p = cr.initialize(4096);
            const tag = Object.prototype.toString.call(p);
            // NOT awaited yet: the device says "wait for it to resolve", and means it.
            const during = sync(() => cr.set('a', 'b'));
            await p;
            return {
                before, beforeDel, noArgs, foreignInit, tag, during,
                after: sync(() => cr.set('a', 'b')),
                afterDelete: sync(() => cr.delete('a')),
                deleteMissing: sync(() => cr.delete('not-there')),
                coerced: sync(() => cr.set(1, 2)),
                nullArgs: sync(() => cr.set(null, null)),
                setNoArgs: sync(() => cr.set()),
                setOneArg: sync(() => cr.set('a')),
                delNoArgs: sync(() => cr.delete()),
                foreignSet: sync(() => CrashReportContext.prototype.set.call({}, 'a', 'b')),
                again: await caught(cr.initialize(8192)),
            };
        })()"#,
    )
    .await;
    const NOT_INIT_SET: &str = "InvalidStateError: Failed to execute 'set' on 'CrashReportContext': \
                                CrashReportContext is not initialized. Call initialize() and wait for it to resolve.";
    const NOT_INIT_DEL: &str = "InvalidStateError: Failed to execute 'delete' on 'CrashReportContext': \
                                CrashReportContext is not initialized. Call initialize() and wait for it to resolve.";
    assert_eq!(v["before"], NOT_INIT_SET);
    assert_eq!(v["beforeDel"], NOT_INIT_DEL);
    assert_eq!(
        v["noArgs"],
        "TypeError: Failed to execute 'initialize' on 'CrashReportContext': 1 argument required, but only 0 present."
    );
    assert_eq!(
        v["foreignInit"],
        "TypeError: Failed to execute 'initialize' on 'CrashReportContext': Illegal invocation"
    );
    assert_eq!(v["tag"], "[object Promise]");
    // THE ORDERING THE DEVICE'S OWN SENTENCE DESCRIBES: the call is not the settling.
    assert_eq!(v["during"], NOT_INIT_SET, "the state must flip when the promise settles, not when the call returns");
    assert_eq!(v["after"], "ok");
    assert_eq!(v["afterDelete"], "ok");
    assert_eq!(v["deleteMissing"], "ok", "deleting a key that is not there is not an error");
    assert_eq!(v["coerced"], "ok", "set(1, 2) stores \"1\"/\"2\"");
    assert_eq!(v["nullArgs"], "ok");
    assert_eq!(
        v["setNoArgs"],
        "TypeError: Failed to execute 'set' on 'CrashReportContext': 2 arguments required, but only 0 present."
    );
    assert_eq!(
        v["setOneArg"],
        "TypeError: Failed to execute 'set' on 'CrashReportContext': 2 arguments required, but only 1 present."
    );
    assert_eq!(
        v["delNoArgs"],
        "TypeError: Failed to execute 'delete' on 'CrashReportContext': 1 argument required, but only 0 present."
    );
    assert_eq!(v["foreignSet"], "TypeError: Illegal invocation", "the bare sentence, not `Failed to execute`");
    assert_eq!(
        v["again"],
        "InvalidStateError: Failed to execute 'initialize' on 'CrashReportContext': The initialize() method has already been called."
    );
}

/// THE BUDGET, as the six boundaries that pin it rather than as a number.
///
/// One `initialize` per document, so one realm per size — which is also how the browsers were swept
/// (one same-origin iframe each). The four browsers' answers are in the right-hand column.
#[tokio::test]
async fn the_budget_is_two_bytes_of_framing_and_five_per_entry() {
    // size, then the three operations in order, as the sweep ran them in each frame.
    let cases: [(u64, [&str; 4]); 6] = [
        //        delete('zz')  set('a','b')  set('c','de')  set('','')
        (1, ["NotAllowedError", "NotAllowedError", "NotAllowedError", "NotAllowedError"]),
        (2, ["ok", "NotAllowedError", "NotAllowedError", "NotAllowedError"]),
        (6, ["ok", "NotAllowedError", "NotAllowedError", "NotAllowedError"]),
        (7, ["ok", "NotAllowedError", "NotAllowedError", "ok"]),
        (9, ["ok", "ok", "NotAllowedError", "NotAllowedError"]),
        (16, ["ok", "ok", "NotAllowedError", "ok"]),
    ];
    for (size, want) in cases {
        let v = fresh(&format!(
            r#"(async () => {{
                const cr = crashReport;
                const sync = (f) => {{ try {{ f(); return 'ok'; }} catch (e) {{ return e.name; }} }};
                await cr.initialize({size});
                return [sync(() => cr.delete('zz')), sync(() => cr.set('a', 'b')),
                        sync(() => cr.set('c', 'de')), sync(() => cr.set('', ''))];
            }})()"#
        ))
        .await;
        assert_eq!(strings(&v), want, "buffer of {size} bytes");
    }
}

/// A large buffer behaves like a large buffer: the boundaries that pin the per-entry cost at the top
/// end, where the value rather than the framing is what does not fit.
#[tokio::test]
async fn a_large_buffer_takes_a_large_value_and_stops_where_it_should() {
    let v = fresh(
        r#"(async () => {
            const cr = crashReport;
            const sync = (f) => { try { f(); return 'ok'; } catch (e) { return e.name; } };
            await cr.initialize(4096);
            const out = {};
            // 2 framing + 1 key + N value + 5 per entry <= 4096  =>  N <= 4088
            for (const n of [1, 1000, 4000, 4088, 4089, 5000]) {
                out['v' + n] = sync(() => cr.set('k', 'v'.repeat(n)));
                sync(() => cr.delete('k'));
            }
            for (const n of [1, 4000, 5000]) {
                out['k' + n] = sync(() => cr.set('k'.repeat(n), 'v'));
                sync(() => cr.delete('k'.repeat(n)));
            }
            return out;
        })()"#,
    )
    .await;
    assert_eq!(v["v1"], "ok");
    assert_eq!(v["v1000"], "ok");
    assert_eq!(v["v4000"], "ok");
    // The exact boundary the rule predicts, asserted on both sides so the constant cannot drift.
    assert_eq!(v["v4088"], "ok");
    assert_eq!(v["v4089"], "NotAllowedError");
    assert_eq!(v["v5000"], "NotAllowedError");
    assert_eq!(v["k1"], "ok");
    assert_eq!(v["k4000"], "ok", "a 4000-byte key fits — measured `ok` on Chrome 150 and Brave 151");
    assert_eq!(v["k5000"], "NotAllowedError", "a 5000-byte key does not — measured");
}

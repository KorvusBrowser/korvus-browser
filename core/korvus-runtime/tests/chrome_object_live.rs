//! `window.chrome`, read back off the LIVE surface.
//!
//! `chrome.loadTimes()` and `chrome.csi()` are a fingerprint surface: bot detectors read
//! `!!window.chrome`, then `Object.keys(chrome)`, then the shape of what those two return. So this
//! gate asserts the SHAPE — key order, function names and lengths, descriptor flags, and the
//! coherence of the values with `performance.timing` — not merely that the members are present.
//!
//! EVERY EXPECTATION HERE WAS MEASURED on `google-chrome --headless=new` 150.0.7871.46, over a
//! loopback HTTP server, on 2026-09-12. Where this engine cannot answer what Chrome answers the
//! difference is stated in the assertion's own comment, never quietly widened.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The three keys, in the recorded order, and the descriptor `window.chrome` itself carries.
///
/// `configurable` is Chrome 152's `true`: `delete window.chrome` answers `true` and removes the object
/// (Chrome 150's was `false`, and the object stayed).
#[tokio::test]
async fn the_chrome_object_has_the_recorded_shape() {
    let out = probe(
        r#"
        print("keys=" + JSON.stringify(Object.keys(chrome)));
        const d = Object.getOwnPropertyDescriptor(globalThis, "chrome");
        print("desc=" + d.writable + " " + d.enumerable + " " + d.configurable);
        print("proto=" + (Object.getPrototypeOf(chrome) === Object.prototype));
        print("tag=" + Object.prototype.toString.call(chrome));
        // `runtime` is an EXTENSION surface; a page with no extension talking to it sees none.
        print("runtime=" + ("runtime" in chrome));
        // last: Chrome 152's `chrome` is configurable, so the delete takes it away
        print("deleted=" + (delete globalThis.chrome) + " " + typeof globalThis.chrome);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"keys=["loadTimes","csi","app"]"#,
            "desc=true true true",
            "proto=true",
            "tag=[object Object]",
            "runtime=false",
            "deleted=true undefined",
        ]
    );
}

/// `chrome.loadTimes` and `chrome.csi` are ANONYMOUS in Chrome — `.name` is `""`, so
/// `String(chrome.loadTimes)` reads `function () { [native code] }`. The JS used named function
/// expressions and the masker printed the name.
///
/// And they are CONSTRUCTORS with a `prototype` of their own: the `v8/LoadTimes` extension defines them as
/// function expressions. This test asserted the opposite ("a native function owns no `prototype`"), which was
/// reasoning, not a measurement; Chrome 150 headed, no DevTools, three runs alike
/// (`fixtures/chrome-object/fns-chrome-150.json`) answers `length,name,prototype`, and none of `arguments` and
/// `caller` is an own property.
#[tokio::test]
async fn load_times_and_csi_are_anonymous_native_functions() {
    let out = probe(
        r#"
        for (const k of ["loadTimes", "csi"]) {
          const f = chrome[k];
          print(k + "=" + JSON.stringify(f.name) + "/" + f.length + " " + String(f));
        }
        print("own=" + Object.getOwnPropertyNames(chrome.loadTimes).join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"loadTimes=""/0 function () { [native code] }"#,
            r#"csi=""/0 function () { [native code] }"#,
            "own=length,name,prototype",
        ]
    );
}

/// The thirteen `loadTimes()` keys in the recorded order, and the property the JS never had: the
/// answer is STABLE and agrees with this realm's own `performance.timing`.
///
/// Chrome's `loadTimes()` is Navigation Timing L1 in seconds (`requestTime*1000 ===
/// timing.navigationStart`, `commitLoadTime*1000 === timing.responseStart`, `finishLoadTime*1000 ===
/// timing.loadEventEnd`). Both read the same L1 fields (`performance::timing_fields`), so they agree
/// whatever the fields are -- and the two "finish" fields are 0 until their events have happened.
#[tokio::test]
async fn load_times_is_stable_and_agrees_with_performance_timing() {
    let out = probe(
        r#"
        const a = chrome.loadTimes();
        print("keys=" + JSON.stringify(Object.keys(a)));
        // A fresh object each call (Chrome: `chrome.loadTimes() === chrome.loadTimes()` is false),
        // with equal contents (the JS answered a different number on every call).
        const b = chrome.loadTimes();
        print("identity=" + (a === b) + " stable=" + (JSON.stringify(a) === JSON.stringify(b)));
        const t = performance.timing;
        print("navigationStart=" + (a.requestTime * 1000 === t.navigationStart));
        print("startLoad=" + (a.startLoadTime * 1000 === t.navigationStart));
        print("responseStart=" + (a.commitLoadTime * 1000 === t.responseStart));
        print("dcle=" + (a.finishDocumentLoadTime * 1000 === t.domContentLoadedEventEnd));
        print("loadEventEnd=" + (a.finishLoadTime * 1000 === t.loadEventEnd));
        // Headless Chrome paints nothing and reports both as 0.
        print("paint=" + a.firstPaintTime + "/" + a.firstPaintAfterLoadTime);
        print("nav=" + a.navigationType + " conn=" + a.connectionInfo + "/" + a.npnNegotiatedProtocol);
        print("spdy=" + a.wasFetchedViaSpdy + " npn=" + a.wasNpnNegotiated + " alt=" + a.wasAlternateProtocolAvailable);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"keys=["requestTime","startLoadTime","commitLoadTime","finishDocumentLoadTime","finishLoadTime","firstPaintTime","firstPaintAfterLoadTime","navigationType","wasFetchedViaSpdy","wasNpnNegotiated","npnNegotiatedProtocol","wasAlternateProtocolAvailable","connectionInfo"]"#,
            "identity=false stable=true",
            "navigationStart=true",
            "startLoad=true",
            "responseStart=true",
            "dcle=true",
            "loadEventEnd=true",
            "paint=0/0",
            "nav=Other conn=h2/h2",
            "spdy=true npn=true alt=false",
        ]
    );
}

/// `csi()`'s four keys, `startE` FIRST (the JS had it third), and the same coherence with
/// `performance.timing`.
///
/// `pageT` used to be `performance.now()` here, and a measurement found that that is the one number
/// on this surface Chromium does NOT take from the monotonic timeline. Its binding predates the
/// Spectre clamp: `base::Time::Now() - navigationStart` through `InMillisecondsF()`, i.e. the wall
/// clock at microsecond resolution, measured from the FLOORED navigationStart. 2026-09-18, twenty
/// readings per engine: Chrome `now 1041.0999999940395 / pageT 1041.847`, Brave
/// `now 1035.9000000059605 / pageT 1036.562` — three decimals where a clamped reading has one, and
/// ahead of `now()` by the origin's own sub-millisecond fraction.
#[tokio::test]
async fn csi_has_the_recorded_key_order_and_fields() {
    let out = probe(
        r#"
        const c = chrome.csi();
        print("keys=" + JSON.stringify(Object.keys(c)));
        const t = performance.timing;
        print("startE=" + (c.startE === t.navigationStart));
        print("onloadT=" + (c.onloadT === t.domContentLoadedEventEnd));
        print("tran=" + c.tran);
        print("pageT=" + (typeof c.pageT === "number" && c.pageT >= 0));
        // It is the WALL clock, not the clamped monotonic one: `pageT` runs AHEAD of `now()` by the
        // fraction of a millisecond `startE` floored away, and it is finer than the 100 us tick.
        // Ten readings, because one could be a coincidence of a clock that happened to land there.
        const rows = [];
        for (let i = 0; i < 40; i++) {
            // BRACKET the read: `n2 - n` is how long `now() -> csi()` took, so a read the scheduler
            // preempted between the two is visible as a wide bracket. "CLOSE" is judged ONLY on a read
            // that was NOT preempted (bracket under 0.3 ms); a preempted read is late by the pause, which
            // inflates `p - n` and says nothing about the clock. "AHEAD" holds on EVERY read -- a preempted
            // read is more ahead, never less -- so it needs no bracket. AHEAD within Blink's clamp:
            // `startE` is the integer part of the CLAMPED origin (performance_timing.cc:29-36), so a raw origin just
            // under a whole millisecond can round up into it, and `now()` is two jittered clamps
            // (performance.cc:1277). So `pageT - now()` is the origin's floored fraction, as low as -0.1, give or take
            // < 0.2: above -0.3 on every read, in Chrome as here. (`p > n` held only for the old FLOORED clock.)
            const n = performance.now(), p = chrome.csi().pageT, n2 = performance.now();
            rows.push([p > n - 0.3, (n2 - n) < 0.3 && (p - n) < 2]);
            const t0 = performance.now(); while (performance.now() - t0 < 1) {}
        }
        // AHEAD on every reading; CLOSE (under 2 ms) on at least one UN-PREEMPTED reading. Over ten reads
        // with no bracket this went `wall=true,false` when all ten were preempted under the full suite's
        // load (the 2026-09-28 run, and a gate twice); 40 bracketed reads make an un-preempted
        // one certain -- a read is microseconds of work -- so it is robust while asserting the same thing.
        print("wall=" + [rows.every((r) => r[0]), rows.some((r) => r[1])].join(","));
        // Finer than the clamp: a 100 us-quantized number has `x * 10` whole, and over ten readings a
        // microsecond one essentially never does.
        const fine = [];
        for (let i = 0; i < 10; i++) {
            fine.push(chrome.csi().pageT);
            const t0 = performance.now(); while (performance.now() - t0 < 1) {}
        }
        print("fine=" + (fine.filter((v) => v * 10 !== Math.round(v * 10)).length >= 8));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"keys=["startE","onloadT","pageT","tran"]"#,
            "startE=true",
            "onloadT=true",
            "tran=15",
            "pageT=true",
            "wall=true,true",
            "fine=true",
        ]
    );
}

/// `chrome.app`: the seven keys in the recorded order (the two enum objects LAST — the JS put them
/// second and third), each method's name and length, and the four extension-binding arity errors.
#[tokio::test]
async fn chrome_app_has_the_recorded_shape_and_binding_errors() {
    let out = probe(
        r#"
        const app = chrome.app;
        print("keys=" + JSON.stringify(Object.keys(app)));
        print("isInstalled=" + app.isInstalled);
        for (const k of ["getDetails", "getIsInstalled", "installState", "runningState"]) {
          print(k + "=" + JSON.stringify(app[k].name) + "/" + app[k].length + " " + String(app[k]));
        }
        print("values=" + JSON.stringify(app.getDetails()) + " " + app.getIsInstalled() + " " + app.runningState());
        print("InstallState=" + JSON.stringify(app.InstallState));
        print("RunningState=" + JSON.stringify(app.RunningState));
        // Chrome's extension bindings reject a call that does not match the signature, with this
        // message verbatim -- including the trailing colon-space.
        const err = (f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name + ": " + e.message; } };
        print("getDetails!=" + err(() => app.getDetails("x")));
        print("getIsInstalled!=" + err(() => app.getIsInstalled("x")));
        print("runningState!=" + err(() => app.runningState("x")));
        print("installState!=" + err(() => app.installState(123)));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"keys=["isInstalled","getDetails","getIsInstalled","installState","runningState","InstallState","RunningState"]"#,
            "isInstalled=false",
            r#"getDetails="getDetails"/0 function getDetails() { [native code] }"#,
            r#"getIsInstalled="getIsInstalled"/0 function getIsInstalled() { [native code] }"#,
            r#"installState="installState"/0 function installState() { [native code] }"#,
            r#"runningState="runningState"/0 function runningState() { [native code] }"#,
            "values=null false cannot_run",
            r#"InstallState={"DISABLED":"disabled","INSTALLED":"installed","NOT_INSTALLED":"not_installed"}"#,
            r#"RunningState={"CANNOT_RUN":"cannot_run","READY_TO_RUN":"ready_to_run","RUNNING":"running"}"#,
            "getDetails!=TypeError: Error in invocation of app.getDetails(): ",
            "getIsInstalled!=TypeError: Error in invocation of app.getIsInstalled(): ",
            "runningState!=TypeError: Error in invocation of app.runningState(): ",
            "installState!=TypeError: Error in invocation of app.installState(optional function callback): ",
        ]
    );
}

/// `installState(cb)` returns `undefined` and delivers in a LATER TASK — measured as ordering after a
/// `setTimeout(…, 0)` queued first, which a microtask never would. The JS called it synchronously.
#[tokio::test]
async fn install_state_delivers_in_a_later_task() {
    let out = probe(
        r#"
        const seq = [];
        Promise.resolve().then(() => seq.push("microtask"));
        print("returns=" + String(chrome.app.installState((s) => seq.push("installState:" + s))));
        setTimeout(() => seq.push("timeout"), 0);
        // `installState` was queued BEFORE this timeout, so a synchronous or microtask delivery would
        // put it first; a task delivery puts it after the microtask and before/after the timeout by
        // queue order. What must never happen is it running before the microtask.
        setTimeout(() => print("seq=" + JSON.stringify(seq)), 20);
        "#,
    )
    .await;
    assert_eq!(out, vec!["returns=undefined", r#"seq=["microtask","installState:not_installed","timeout"]"#]);
}

//! LIVE one-table gate for the performance TIMELINE, in BOTH realms.
//!
//! The seven interfaces — `Performance`, `PerformanceEntry`, `PerformanceMark`,
//! `PerformanceMeasure`, `PerformanceTiming`, `PerformanceNavigation`, `EventCounts` — left
//! `host.js` / `worker-host.js` for `core/korvus-webapi/src/core/timing/performance.rs`. This file is
//! the proof, and it is organised around **the four failure modes of a table cutover**, because those
//! are what a cutover silently gets wrong:
//!
//! 1. **An instance on the class template's prototype.** `Performance.prototype` must not be a live
//!    timeline with its own entries — which is exactly what it BECAME under the deleted
//!    `rehome_performance`, since that function moved an object literal's own members (closures over
//!    ONE entries array) onto the prototype.
//! 2. **A row accepting a foreign receiver.** `Performance.prototype.mark.call({}, "x")` must throw,
//!    and a second realm's `now`/`timeOrigin` must read the OTHER realm's clock.
//! 3. **A body reachable where it was a silent no-op, and the reverse.**
//! 4. **An install ORDER change discarding a decorator.** `PerformanceObserver` is a table of its own
//!    and the timeline drives its callback; if the order flips, observers stop firing silently. And
//!    `new PerformanceEntry()` must still throw `TypeError: Illegal constructor`, which used to come
//!    from `op_nativize_ctor` over a JS class.
//!
//! Every expectation below was measured against Chrome 150.0.7871.46 (`--headless=new --dump-dom`,
//! loopback, 2026-09-11) or is recorded in the persona bundle's oracle; the ones that CHANGED
//! behaviour with this cutover say so at the assertion.

use engine::Engine;

async fn eval(src: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.eval("probe", src.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

// ── failure mode 1: the prototype is not an instance ─────────────────────────────────────────────

#[tokio::test]
async fn the_prototype_is_not_a_timeline_and_owns_only_its_rows() {
    let out = eval(r#"
        const P = Performance.prototype;
        // A prototype that is an INSTANCE answers its own members; a prototype that is a TABLE throws
        // Illegal invocation on every one of them. Chrome: `Performance.prototype.now()` ->
        // "TypeError: Illegal invocation".
        const call = (f, ...a) => { try { return String(f.apply(P, a)); } catch (e) { return e.constructor.name; } };
        print("proto-now=" + call(P.now));
        print("proto-getEntries=" + call(P.getEntries));
        print("proto-mark=" + call(P.mark, "x"));
        const getter = (n) => { try { return String(Object.getOwnPropertyDescriptor(P, n).get.call(P)); } catch (e) { return e.constructor.name; } };
        print("proto-timeOrigin=" + getter("timeOrigin"));
        print("proto-timing=" + getter("timing"));
        // The singleton owns NOTHING: every member is on the prototype. Chrome:
        // `Object.getOwnPropertyNames(performance)` is `[]`.
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(performance)));
        print("chain=" + [Object.getPrototypeOf(performance) === P,
                          Object.getPrototypeOf(P) === EventTarget.prototype].join(","));
        print("tag=" + Object.prototype.toString.call(performance));
        print("instance=" + (performance instanceof Performance));
    "#).await;
    assert_eq!(out, vec![
        "proto-now=TypeError",
        "proto-getEntries=TypeError",
        "proto-mark=TypeError",
        "proto-timeOrigin=TypeError",
        "proto-timing=TypeError",
        "own=[]",
        "chain=true,true",
        "tag=[object Performance]",
        "instance=true",
    ]);
}

// ── failure mode 2: foreign receivers ────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_row_rejects_a_foreign_receiver() {
    let out = eval(r#"
        const P = Performance.prototype;
        const m = (n, ...a) => { try { P[n].call({}, ...a); return "no-throw"; } catch (e) { return e.constructor.name; } };
        const g = (proto, n) => { try { Object.getOwnPropertyDescriptor(proto, n).get.call({}); return "no-throw"; } catch (e) { return e.constructor.name; } };
        print("methods=" + ["now","mark","measure","clearMarks","clearMeasures","clearResourceTimings",
                            "getEntries","getEntriesByType","getEntriesByName","toJSON",
                            "setResourceTimingBufferSize"]
              .map((n) => m(n, "x")).join(","));
        print("attrs=" + ["timeOrigin","timing","navigation","eventCounts","interactionCount","memory"]
              .map((n) => g(P, n)).join(","));
        print("timing=" + ["navigationStart","loadEventEnd"].map((n) => g(PerformanceTiming.prototype, n)).join(",")
              + "," + (() => { try { PerformanceTiming.prototype.toJSON.call({}); return "no-throw"; } catch (e) { return e.constructor.name; } })());
        print("navigation=" + ["type","redirectCount"].map((n) => g(PerformanceNavigation.prototype, n)).join(","));
        print("eventCounts=" + ["size"].map((n) => g(EventCounts.prototype, n)).join(",")
              + "," + ["get","has","keys","values","entries","forEach"]
                      .map((n) => { try { EventCounts.prototype[n].call({}, "click"); return "no-throw"; } catch (e) { return e.constructor.name; } }).join(","));
    "#).await;
    assert_eq!(out, vec![
        "methods=TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
        "attrs=TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
        "timing=TypeError,TypeError,TypeError",
        "navigation=TypeError,TypeError",
        "eventCounts=TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
    ]);
}

// ── failure mode 3: the bodies answer, and answer what Chrome answers ────────────────────────────

#[tokio::test]
async fn the_rows_answer_what_chrome_answers() {
    let out = eval(r#"
        performance.clearMarks(); performance.clearMeasures();
        // mark(): an entry with NO own property, wearing PerformanceMark.prototype, buffered.
        const mk = performance.mark("a", { detail: { n: 7 } });
        print("mark=" + [mk instanceof PerformanceMark, mk instanceof PerformanceEntry,
                         Object.prototype.toString.call(mk), JSON.stringify(Object.getOwnPropertyNames(mk)),
                         mk.name, mk.entryType, mk.duration, mk.detail.n].join(","));
        print("markJSON=" + JSON.stringify(Object.keys(mk.toJSON())));
        // A negative startTime is a TypeError (Chrome: "'neg' cannot have a negative start time.").
        // The deleted JS produced an entry before time began.
        let neg = "no-throw"; try { performance.mark("neg", { startTime: -1 }); } catch (e) { neg = e.constructor.name; }
        print("negStart=" + neg);
        // A Symbol name is a TypeError, not a described string (WebIDL DOMString is ToString).
        let sym = "no-throw"; try { performance.mark(Symbol("s")); } catch (e) { sym = e.constructor.name; }
        print("symbol=" + sym);
        // measure(): the three User Timing L3 TypeErrors, then the shapes that work.
        // `e.name`, not `e.constructor.name`: a DOMException's constructor IS DOMException and its
        // NAME is what identifies it ("SyntaxError"). Chrome answers a DOMException here too.
        const thrown = (f) => { try { f(); return "no-throw"; } catch (e) { return e.name; } };
        print("measureThrows=" + [
            thrown(() => performance.measure("m", { duration: 5 })),          // neither start nor end
            thrown(() => performance.measure("m", { detail: 41 })),           // the bundle's oracle
            thrown(() => performance.measure("m", { start: 1, end: 2, duration: 1 })),
            thrown(() => performance.measure("m", { start: 0 }, "endmark")),  // dict AND endMark
            thrown(() => performance.measure("m", "no-such-mark")),           // SyntaxError, not 0
        ].join(","));
        performance.clearMeasures();
        print("measure=" + [
            (() => { const x = performance.measure("s", { start: 5, duration: 10 }); return x.startTime + ":" + x.duration; })(),
            (() => { const x = performance.measure("e", { end: 50, duration: 10 }); return x.startTime + ":" + x.duration; })(),
            (() => { const x = performance.measure("n", { start: 1, end: 4 }); return x.startTime + ":" + x.duration; })(),
            (() => { const x = performance.measure("a2", "a"); return (x.startTime >= 0) + ":" + (x.duration >= 0); })(),
        ].join(","));
        // clearMarks(undefined) clears ALL; clearMarks("undefined") clears the one so named.
        performance.clearMarks();
        performance.mark("k1"); performance.mark("undefined");
        performance.clearMarks("undefined");
        print("clearName=" + performance.getEntriesByType("mark").map((e) => e.name).join("|"));
        performance.clearMarks(undefined);
        print("clearAll=" + performance.getEntriesByType("mark").length);
        // The three [SameObject] window partials, and the fresh-per-read MemoryInfo.
        print("same=" + [performance.timing === performance.timing,
                         performance.navigation === performance.navigation,
                         performance.eventCounts === performance.eventCounts,
                         performance.memory === performance.memory].join(","));
        print("timingTags=" + [Object.prototype.toString.call(performance.timing),
                               Object.prototype.toString.call(performance.navigation),
                               Object.prototype.toString.call(performance.eventCounts),
                               Object.prototype.toString.call(performance.memory)].join(","));
        print("timingOwn=" + [Object.getOwnPropertyNames(performance.timing).length,
                              Object.getOwnPropertyNames(performance.navigation).length,
                              Object.getOwnPropertyNames(performance.eventCounts).length].join(","));
        // Navigation Timing L1 is EPOCH MILLISECONDS, not timeOrigin-relative -- the bundle's oracle
        // pins `navigationStart > 1500000000000` and `<= Date.now()`.
        const t = performance.timing;
        print("L1=" + [typeof t.navigationStart, t.navigationStart > 1500000000000,
                       t.navigationStart <= Date.now(), t.toJSON().navigationStart === t.navigationStart,
                       Object.keys(t.toJSON()).length, t.unloadEventStart, t.redirectEnd].join(","));
        print("L1nav=" + [performance.navigation.type, performance.navigation.redirectCount,
                          PerformanceNavigation.TYPE_RESERVED, PerformanceNavigation.prototype.TYPE_BACK_FORWARD,
                          JSON.stringify(Object.keys(performance.navigation.toJSON()))].join(","));
        // toJSON: the window's three keys (a worker's is `timeOrigin` alone).
        print("toJSON=" + JSON.stringify(Object.keys(performance.toJSON())));
        // EventCounts is a real maplike: 36 tracked types, `get` undefined off the map, and
        // `@@iterator === entries` (the SAME function object).
        const ec = performance.eventCounts;
        print("ec=" + [ec.size, Array.from(ec.keys()).length, ec.has("click"), ec.has("load"),
                       ec.get("click"), typeof ec.get("load"),
                       ec[Symbol.iterator] === ec.entries, [...ec].length,
                       JSON.stringify(Array.from(ec.entries())[0])].join(","));
        let forEachArgs = null;
        ec.forEach((v, k, m) => { if (!forEachArgs) forEachArgs = [v, typeof k, m === ec]; });
        print("ecForEach=" + JSON.stringify(forEachArgs));
        // The navigation entry exists from the moment the document does, and getEntriesByType finds
        // it (it used to be built by a bridge name that was never defined, so this was always empty).
        const nav = performance.getEntriesByType("navigation");
        print("navEntry=" + [nav.length, nav[0] && nav[0].entryType, nav[0] && nav[0].type,
                             Object.prototype.toString.call(nav[0])].join(","));
        print("resourceBuffer=" + [String(performance.setResourceTimingBufferSize(5)),
                                   String(performance.clearResourceTimings()),
                                   performance.interactionCount,
                                   String(performance.onresourcetimingbufferfull)].join(","));
    "#).await;
    assert_eq!(out, vec![
        "mark=true,true,[object PerformanceMark],[],a,mark,0,7",
        r#"markJSON=["name","entryType","startTime","duration","navigationId"]"#,
        "negStart=TypeError",
        "symbol=TypeError",
        "measureThrows=TypeError,TypeError,TypeError,TypeError,SyntaxError",
        "measure=5:10,40:10,1:3,true:true",
        "clearName=k1",
        "clearAll=0",
        "same=true,true,true,false",
        "timingTags=[object PerformanceTiming],[object PerformanceNavigation],[object EventCounts],[object MemoryInfo]",
        "timingOwn=0,0,0",
        "L1=number,true,true,true,21,0,0",
        r#"L1nav=0,0,255,2,["type","redirectCount"]"#,
        r#"toJSON=["timeOrigin","timing","navigation"]"#,
        r#"ec=36,36,true,false,0,undefined,true,36,["pointerdown",0]"#,
        r#"ecForEach=[0,"string",true]"#,
        "navEntry=1,navigation,navigate,[object PerformanceNavigationTiming]",
        "resourceBuffer=undefined,undefined,0,null",
    ]);
}

// ── the clock: the three hazards the brief names ─────────────────────────────────────────────────

#[tokio::test]
async fn the_clock_is_lazy_monotonic_and_quantized() {
    let out = eval(r#"
        // timeOrigin is THIS process's start, read lazily -- not the (possibly hours-old) V8 startup
        // snapshot's build time. Bounded rather than pinned, which is all a wall clock allows: it is
        // an epoch millisecond count no older than a minute.
        const o = performance.timeOrigin;
        print("origin=" + [typeof o, o > 1500000000000, o <= Date.now(), (Date.now() - o) < 60000].join(","));
        // now() is monotonic, starts at 0, and is 100us-quantized (Chrome's Spectre clamp) -- NOT
        // Date.now()'s 1ms. A 1ms clock makes a tight loop return the same value over and over, which
        // is a one-line detect; the worker realm used to have exactly that.
        const s = [];
        for (let i = 0; i < 2000; i++) s.push(performance.now());
        let monotonic = true;
        for (let i = 1; i < s.length; i++) if (s[i] < s[i-1]) monotonic = false;
        const deltas = [];
        for (let i = 1; i < s.length; i++) { const d = s[i] - s[i-1]; if (d > 0) deltas.push(d); }
        deltas.sort((a, b) => a - b);
        print("now=" + [typeof s[0], s[0] >= 0, monotonic,
                        // every positive delta is a multiple of 0.1ms, to float tolerance
                        deltas.every((d) => Math.abs(Math.round(d * 10) - d * 10) < 1e-6),
                        deltas.length > 0].join(","));
        // Event.timeStamp reads performance.now() and WPT asserts it is strictly positive.
        print("timeStamp=" + (new Event("x").timeStamp > 0));
        // The animation timeline follows the SAME clock (a second Instant would drift, and
        // `document.timeline.currentTime - performance.now()` is a two-line probe).
        print("timeline=" + (Math.abs(document.timeline.currentTime - performance.now()) < 50));
    "#).await;
    assert_eq!(out, vec![
        "origin=number,true,true,true",
        "now=number,true,true,true,true",
        "timeStamp=true",
        "timeline=true",
    ]);
}

/// The clock's ARITHMETIC, not just its quantum.
///
/// The 100 µs clamp above was right and the arithmetic under it was not: korvus divided an integer
/// tick and handed back an exact decimal multiple of 0.1, so `String(performance.now())` was always
/// one decimal place. Sampled 2026-09-18 on this box, 600 spread readings per engine:
///
/// ```text
/// chrome 150.0.7871.46   1037.9000000059605  1038.300000011921   1038.7000000029802
/// brave  151.1.93.138    1021.9000000059605  1022.2999999970198  1022.7000000029802
/// korvus (before)        118.9               119.3               119.7
/// ```
///
/// A browser's reading is the difference of two doubles taken on the monotonic clock's own
/// boot-relative millisecond scale, so it sits on THAT scale's grid; the 100 µs tick survives as the
/// nominal value with a residue around it. `spec::PerfClock::dom_hrt` does the same arithmetic, and
/// this is the in-realm proof of it. **Nothing here pins a timestamp or a grid**: the assertions are
/// the relationships a sensor can read off a sample of its own.
#[tokio::test]
async fn the_clock_carries_chromiums_float_artifacts() {
    let out = eval(r#"
        // >= 250 DISTINCT readings, walked a tick at a time so every residue class is sampled.
        const s = [];
        let guard = 0;
        while (s.length < 260 && guard++ < 4000000) {
            const v = performance.now();
            if (s.length === 0 || v !== s[s.length - 1]) s.push(v);
        }
        const tick = (v) => Math.round(v * 10);
        // 1. The QUANTUM still holds: every reading is a 100 us tick, to well under a tick.
        const onTick = s.every((v) => Math.abs(v * 10 - tick(v)) < 1e-4);
        // 2. The SHAPE: a reading is an exact decimal tenth exactly when its tick lands on a multiple
        //    of 0.5 ms (k % 5 === 0) -- the only ticks a double can hold exactly. Everywhere else the
        //    origin's own rounding shows, which is what a browser does and korvus did not.
        const isExact = (v) => v * 10 === tick(v);
        const classified = s.every((v) => isExact(v) === (tick(v) % 5 === 0));
        const exact = s.filter(isExact).length;
        // 3. ... so roughly one reading in five is clean, not all of them (korvus: 600 of 600) and not
        //    none of them.
        const share = exact / s.length;
        print("shape=" + [s.length >= 250, onTick, classified, exact > 0,
                          share > 0.1 && share < 0.3].join(","));
        // 4. Still a clock. Monotonic, from the realm's origin, and advancing by whole ticks.
        let mono = true, fromOrigin = s[0] >= 0;
        const steps = new Set();
        for (let i = 1; i < s.length; i++) {
            if (s[i] <= s[i - 1]) mono = false;
            steps.add(tick(s[i]) - tick(s[i - 1]));
        }
        print("clock=" + [mono, fromOrigin, [...steps].every((d) => d >= 1)].join(","));
        // 5. The SAME shape on every surface fed by the same clock -- one clock, one shape. A residue
        //    that appeared on `now()` and not on `Event.timeStamp` would be the tell instead.
        const same = (v) => Number.isFinite(v) && isExact(v) === (tick(v) % 5 === 0)
                            && Math.abs(v * 10 - tick(v)) < 1e-4;
        const ev = new Event("x").timeStamp;
        const nav = performance.getEntriesByType("navigation")[0];
        print("surfaces=" + [same(ev), same(nav.responseEnd), same(nav.duration),
                             same(performance.now()), same(document.timeline.currentTime)].join(","));
        // 6. `timeOrigin` is on the same 100 us quantum -- one decimal place, like Chrome's
        //    1789718996556.8 and Brave's 1789718997834.7. korvus printed 1789718898281.8726, four
        //    digits of real sub-quantum precision (a double resolves ~244 ns at that magnitude, so
        //    they were not float noise -- they were the wall clock, readable with one String()).
        const o = performance.timeOrigin;
        print("origin=" + [o * 10 === Math.round(o * 10),
                           performance.timing.navigationStart === Math.floor(o)].join(","));
    "#).await;
    assert_eq!(out, vec![
        "shape=true,true,true,true,true",
        "clock=true,true,true",
        "surfaces=true,true,true,true,true",
        "origin=true,true",
    ]);
}

// ── failure mode 4: the interface objects, and the observer the timeline drives ──────────────────

#[tokio::test]
async fn the_interface_objects_are_what_the_capture_records() {
    let out = eval(r#"
        const ctor = (f, ...a) => { try { new f(...a); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
        // `new PerformanceEntry()` came from an `op_nativize_ctor` mask over a JS class; it is the
        // installer's interface object now, and must still refuse.
        print("entry=" + ctor(PerformanceEntry));
        print("perf=" + ctor(Performance));
        print("measure=" + ctor(PerformanceMeasure));
        print("timing=" + ctor(PerformanceTiming));
        print("navigation=" + ctor(PerformanceNavigation));
        print("counts=" + ctor(EventCounts));
        // PerformanceMark is the ONE constructible entry type (the bundle's oracle records
        // `new PerformanceMark("x")` as `[object PerformanceMark]`), and a constructed mark is NOT
        // buffered -- only `performance.mark()` buffers one.
        performance.clearMarks();
        const m = new PerformanceMark("q", { detail: { a: 1 }, startTime: 3 });
        print("mark=" + [Object.prototype.toString.call(m), m.startTime, m.entryType, m.detail.a,
                         performance.getEntriesByType("mark").length,
                         m instanceof PerformanceEntry].join(","));
        print("markThrows=" + ctor(PerformanceMark).split(":")[0] + "," + ctor(PerformanceMark, "q2", { startTime: -5 }).split(":")[0]);
        // The WebIDL constructor-inheritance splice, and the lengths the capture records.
        print("chain=" + [Object.getPrototypeOf(Performance) === EventTarget,
                          Object.getPrototypeOf(PerformanceMark) === PerformanceEntry,
                          Object.getPrototypeOf(PerformanceMeasure) === PerformanceEntry,
                          Object.getPrototypeOf(PerformanceMark.prototype) === PerformanceEntry.prototype].join(","));
        print("lengths=" + [Performance.length, PerformanceEntry.length, PerformanceMark.length,
                            PerformanceMeasure.length, PerformanceTiming.length,
                            PerformanceNavigation.length, EventCounts.length].join(","));
        print("fnLengths=" + ["mark","measure","clearMarks","clearMeasures","getEntries",
                              "getEntriesByName","getEntriesByType","now","toJSON",
                              "setResourceTimingBufferSize","clearResourceTimings"]
              .map((n) => Performance.prototype[n].length).join(","));
        // Every body is native by construction (no `[native code]` mask anywhere in this family).
        const native = (f) => /\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(f));
        print("native=" + [native(Performance.prototype.mark),
                           native(Object.getOwnPropertyDescriptor(Performance.prototype, "timeOrigin").get),
                           native(EventCounts.prototype.get),
                           native(Object.getOwnPropertyDescriptor(PerformanceTiming.prototype, "navigationStart").get),
                           native(PerformanceMark)].join(","));
    "#).await;
    assert_eq!(out, vec![
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        "entry=TypeError:Failed to construct 'PerformanceEntry': Illegal constructor",
        "perf=TypeError:Failed to construct 'Performance': Illegal constructor",
        "measure=TypeError:Failed to construct 'PerformanceMeasure': Illegal constructor",
        "timing=TypeError:Failed to construct 'PerformanceTiming': Illegal constructor",
        "navigation=TypeError:Failed to construct 'PerformanceNavigation': Illegal constructor",
        "counts=TypeError:Failed to construct 'EventCounts': Illegal constructor",
        "mark=[object PerformanceMark],3,mark,1,0,true",
        "markThrows=TypeError,TypeError",
        "chain=true,true,true,true",
        "lengths=0,0,1,0,0,0,0",
        "fnLengths=1,1,0,0,0,1,1,0,0,1,0",
        "native=true,true,true,true,true",
    ]);
}

#[tokio::test]
async fn the_timeline_still_drives_the_observer() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    // The observer is a table of its own (`core/timing/performance_observer.rs`) and the TIMELINE is
    // what notifies it. This is failure mode 4: if the install order flips, or the timeline stops
    // reaching the observer list, the callback silently never fires and nothing else notices.
    e.eval("probe", r#"
        performance.clearMarks();
        const seen = [];
        const obs = new PerformanceObserver((list, o) => {
            seen.push([list.getEntries().map((x) => x.name).join("|"),
                       list.getEntriesByType("mark").length,
                       list.getEntriesByName("o2").length,
                       o === obs,
                       Object.prototype.toString.call(list)].join(","));
        });
        obs.observe({ entryTypes: ["mark"] });
        performance.mark("o1");
        performance.mark("o2");
        // A measure is NOT observed by an observer that asked only for marks.
        performance.measure("ignored", { start: 0 });
        globalThis.__done = () => { print("seen=" + seen.length); print("batch=" + seen[0]); };
    "#.into()).unwrap();
    e.run().await.unwrap();
    e.eval("done", "setTimeout(() => __done(), 0);".into()).unwrap();
    let mut out = Vec::new();
    for _ in 0..20 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("batch=")) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        out,
        vec![
            // ONE delivery, carrying BOTH marks: entries queued in the same synchronous block are
            // batched into a single callback, which is what the `performance-worker` fixture's
            // `entry_count: 2` also asserts.
            "seen=1",
            "batch=o1|o2,2,1,true,[object PerformanceObserverEntryList]",
        ]
    );
}

// ── the OTHER realm: one implementation, one instance per realm ──────────────────────────────────

/// Spawn a worker that probes its own timeline and posts a pipe-joined summary back.
async fn worker_probe(worker_body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(worker_body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
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
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

/// The worker realm gets THE SAME table with the WORKER capture's selector — and its own instance,
/// its own entry buffer and its own clock.
///
/// The page-realm surface board cannot see in here (UNIT-CONTRACT: "the page-realm board does not see
/// inside workers"), and `worker-host.js`'s deleted copy of the timeline is exactly the kind of second
/// implementation that only a realm-specific gate catches.
#[tokio::test]
async fn the_worker_realm_gets_the_same_table_with_the_workers_thirteen_rows() {
    let body = r#"
        const P = Performance.prototype;
        const names = Object.getOwnPropertyNames(P).filter((n) => n !== "constructor");
        const call = (f, ...a) => { try { f.apply({}, a); return "no-throw"; } catch (e) { return e.name; } };
        const parts = [
            // The worker capture's THIRTEEN rows, in its order -- the window's eighteen minus the five
            // [Exposed=Window] partials. `Exposed` filtering is what makes this one table.
            names.join("|"),
            // ... so the three the `performance-worker` fixture names are not even `in` the object.
            ["eventCounts", "timing", "navigation", "memory", "interactionCount"].map((n) => n in performance).join(","),
            // An instance of its own, with no own property, on the table's prototype.
            [performance instanceof Performance,
             Object.getPrototypeOf(performance) === P,
             Object.getOwnPropertyNames(performance).length,
             Object.prototype.toString.call(performance)].join(","),
            // The global does NOT own it -- a real worker global has no own `performance`; it is an
            // accessor on WorkerGlobalScope.prototype (this is what worker-finalize.js used to do in
            // JS over an `op_nativize` mask, and what the Rust install does now).
            [Object.getOwnPropertyDescriptor(globalThis, "performance") === undefined,
             typeof Object.getOwnPropertyDescriptor(WorkerGlobalScope.prototype, "performance").get,
             performance === self.performance].join(","),
            // The prototype is not a timeline, and every row rejects a foreign receiver.
            [call(P.now), call(P.mark, "x"), call(P.getEntries),
             (() => { try { Object.getOwnPropertyDescriptor(P, "timeOrigin").get.call({}); return "no-throw"; } catch (e) { return e.name; } })()].join(","),
            // THIS realm's clock: an epoch timeOrigin, a monotonic now(), and 100us quantization --
            // where `worker-host.js` ran `Date.now() - origin`, a 1ms clock the window never had.
            (() => {
                const o = performance.timeOrigin;
                const s = []; for (let i = 0; i < 500; i++) s.push(performance.now());
                const d = []; for (let i = 1; i < s.length; i++) { const x = s[i] - s[i-1]; if (x > 0) d.push(x); }
                let mono = true; for (let i = 1; i < s.length; i++) if (s[i] < s[i-1]) mono = false;
                return [typeof o, o > 1500000000000, o <= Date.now(), mono,
                        d.every((x) => Math.abs(Math.round(x * 10) - x * 10) < 1e-6)].join(",");
            })(),
            // Its own entry buffer, its own `PerformanceMark` / `PerformanceMeasure` -- and a worker
            // has no navigation entry (PerformanceNavigationTiming is [Exposed=Window]).
            (() => {
                performance.clearMarks(); performance.clearMeasures();
                const m = performance.mark("w", { detail: 5 });
                const me = performance.measure("wm", { start: 0 });
                return [m instanceof PerformanceMark, m instanceof PerformanceEntry, m.detail,
                        me instanceof PerformanceMeasure,
                        Object.getOwnPropertyNames(m).length,
                        performance.getEntries().length,
                        performance.getEntriesByType("navigation").length,
                        JSON.stringify(Object.keys(performance.toJSON()))].join(",");
            })(),
            // The same three `measure` / `mark` throws the window enforces -- one implementation.
            [(() => { try { performance.measure("x", { duration: 1 }); return "no-throw"; } catch (e) { return e.name; } })(),
             (() => { try { performance.measure("x", "nope"); return "no-throw"; } catch (e) { return e.name; } })(),
             (() => { try { performance.mark("x", { startTime: -1 }); return "no-throw"; } catch (e) { return e.name; } })()].join(","),
            // `new PerformanceEntry()` still refuses; `new PerformanceMark("x")` still works.
            [(() => { try { new PerformanceEntry(); return "no-throw"; } catch (e) { return e.name; } })(),
             Object.prototype.toString.call(new PerformanceMark("x"))].join(","),
        ];
        postMessage(parts.join(" ;; "));
    "#;
    let got = worker_probe(body).await;
    assert_eq!(
        got,
        [
            "timeOrigin|onresourcetimingbufferfull|clearMarks|clearMeasures|clearResourceTimings|getEntries|getEntriesByName|getEntriesByType|mark|measure|setResourceTimingBufferSize|toJSON|now",
            "false,false,false,false,false",
            "true,true,0,[object Performance]",
            "true,function,true",
            "TypeError,TypeError,TypeError,TypeError",
            "number,true,true,true,true",
            "true,true,5,true,0,2,0,[\"timeOrigin\"]",
            "TypeError,SyntaxError,TypeError",
            "TypeError,[object PerformanceMark]",
        ]
        .join(" ;; ")
    );
}

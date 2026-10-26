//! Performance Timeline: mark/measure/getEntries + eventCounts + timing/navigation.
use engine::Engine;
#[tokio::test]
async fn performance_timeline() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        performance.clearMarks(); performance.clearMeasures();
        const m = performance.mark("start", { detail: { x: 1 } });
        print("mark=" + (m instanceof PerformanceMark) + "," + m.name + "," + m.entryType + "," + m.duration + "," + m.detail.x);
        performance.mark("end", { startTime: 100 });
        const meas = performance.measure("dur", { start: "start", end: "end" });
        // `duration` is the end mark's time MINUS the start mark's, unclamped (User Timing L3; Chrome 150
        // answers a negative one when the start is later). It read `>= 0` here, true only because the
        // engine clamped: "start" is marked at NOW, past 100 ms in a debug build.
        print("measure=" + (meas instanceof PerformanceMeasure) + "," + meas.name + "," + (meas.duration === 100 - m.startTime));
        // `measure(name, { duration })` ALONE is a TypeError, not a measure from the time origin.
        // User Timing L3 step 1.2: "If startOrMeasureOptions's start and end members are both
        // omitted, throw a TypeError." Asked of Chrome 150.0.7871.46 (--headless=new, loopback,
        // 2026-09-11) because this line used to assert `duronly=0,100.5` and so pinned the opposite:
        //   performance.measure("d", { duration: 100.5 })
        //     -> TypeError: Failed to execute 'measure' on 'Performance': If a non-empty
        //        PerformanceMeasureOptions object was passed, at least one of its 'start' or 'end'
        //        properties must be present.
        // The bundle's own oracle records the same throw for `measure("pm-b", { detail: 41 })`.
        let threw = "";
        try { performance.measure("fixed", { duration: 100.5 }); } catch (e) { threw = e.constructor.name; }
        print("duronly=" + threw);
        // With a `start`, `duration` is honoured -- Chrome: `measure("g", {start:5,duration:10})`
        // is `[5, 10]`.
        const d = performance.measure("fixed", { start: 0, duration: 100.5 });
        print("startdur=" + d.startTime + "," + d.duration);
        print("entries=" + performance.getEntriesByType("mark").length + "," + performance.getEntriesByType("measure").length);
        print("byName=" + performance.getEntriesByName("start", "mark").length);
        performance.clearMarks("start");
        print("afterClear=" + performance.getEntriesByType("mark").length);
        // Navigation Timing L1 is a `Date.now()`-comparable epoch millisecond count, not a
        // timeOrigin-relative one, so the VALUE cannot be pinned in an expectation -- only its shape
        // and its bounds. Asked of Brave 151 on this machine: navigationStart=1788181912811, which
        // is > 1.5e12 and <= Date.now(). This line used to assert 0 and pinned the wrong behaviour.
        print("timing=" + (typeof performance.timing.navigationStart) + "," +
              (performance.timing.navigationStart > 1500000000000) + "," +
              (performance.timing.toJSON().navigationStart === performance.timing.navigationStart));
        print("nav=" + performance.navigation.type + "," + performance.navigation.redirectCount);
        const ec = performance.eventCounts;
        // `get` on a type this build does not track is `undefined`, not 0 -- a maplike `get()` has
        // no default. Chrome 150 (same probe): `typeof performance.eventCounts.get("nope")` is
        // "undefined"; `get("click")` is the number 0. This line used to assert `0` for both.
        print("ec=" + ec.size + "," + ec.has("click") + "," + ec.has("scroll") + "," + ec.get("click") + "," + ec.get("nope"));
        print("ecKeys=" + Array.from(ec.keys()).length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "mark=true,start,mark,0,1",
        "measure=true,dur,true",
        "duronly=TypeError",
        "startdur=0,100.5",
        "entries=2,2",
        "byName=1",
        "afterClear=1",
        "timing=number,true,true",
        "nav=0,0",
        "ec=36,true,false,0,undefined",
        "ecKeys=36",
    ]);
}

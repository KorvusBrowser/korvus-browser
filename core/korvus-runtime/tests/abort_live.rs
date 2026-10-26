//! `AbortController` and `AbortSignal`'s live gate, in BOTH realms.
//!
//! Both were JS classes in `host.js`, and `class AbortSignal extends globalThis.EventTarget` was one
//! of the THREE ESM-time `extends EventTarget` declarations W4.b named as the reason `events.js`
//! still exists. They are `interface!` tables with a native constructor and a Rust mint now
//! (`core/korvus-webapi/src/core/dom/abort_controller.rs`).
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/abort-probe.js` UNMODIFIED — the same file this test evaluates in the window and inside
//! a real Worker isolate. `KORVUS_ABORT_OUT=<dir>` writes `window.json` / `worker.json`.
//!
//! THE ONE EXPECTATION THAT WAS THE TREE'S AND NOT CHROME'S IS CLOSED. `new
//! AbortSignal()` answered `TypeError: Illegal constructor` where Chrome answers `TypeError: Failed
//! to construct 'AbortSignal': Illegal constructor`; the missing prefix was
//! `binding::protos::cb_illegal_ctor`'s, shared by every non-newable interface object in the tree,
//! and this file's two weakened assertions (`starts_with("TypeError:")` + `contains("Illegal
//! constructor")`) were the shape that let it hide. They are one exact `assert_eq!` on the full
//! string now, in both realms.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/abort-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_ABORT_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the abort dump");
    }
}

/// The probe settles a `setTimeout` before it serialises (`AbortSignal.timeout` has to actually
/// fire), so both realms poll `print` output rather than reading an eval result.
async fn pump_for(e: &mut Engine, tag: &str) -> String {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with(tag)) {
            return line[tag.len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("{tag} never arrived within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("abort_probe", format!("({PROBE}).then((s) => print('ABRT:' + s));")).unwrap();
    let s = pump_for(&mut e, "ABRT:").await;
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("({PROBE}).then((s) => self.postMessage(s));");
    let reg = format!("globalThis.__workerScripts = {{ './abort.js': {} }};", serde_json::to_string(&worker_src).unwrap());
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("abort-driver", "const w = new Worker('./abort.js'); w.onmessage = (ev) => print('ABRT:' + ev.data);".into()).unwrap();
    let s = pump_for(&mut e, "ABRT:").await;
    save("worker", &s);
    serde_json::from_str(&s).expect("the worker probe's JSON parses")
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

#[tokio::test]
async fn the_two_interfaces_match_the_capture_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let c = &d["iface"]["AbortController"];
        assert!(c.get("absent").is_none(), "{realm}: AbortController is not exposed");
        assert_eq!(c["name"], serde_json::json!("AbortController"), "{realm}");
        assert_eq!(c["length"].as_u64(), Some(0), "{realm}: AbortController.length");
        assert_eq!(c["nat"], serde_json::json!(true), "{realm}: the interface object is [native code]");
        assert_eq!(c["newable"], serde_json::json!("OK:[object AbortController]"), "{realm}: `new AbortController()`");
        assert_eq!(strs(&c["protoOrder"]), vec!["signal", "abort", "constructor"], "{realm}: the capture's order");
        assert_eq!(strs(&c["ctorOrder"]), vec!["length", "name", "prototype"], "{realm}: no statics");
        assert_eq!(strs(&c["protoChain"]), vec!["Object"], "{realm}: AbortController inherits nothing");
        assert_eq!(c["ctorProtoName"], serde_json::json!(""), "{realm}: its [[Prototype]] is Function.prototype");
        assert_eq!(c["tag"], serde_json::json!({"v": "AbortController", "e": false, "c": true, "w": false}), "{realm}");

        let s = &d["iface"]["AbortSignal"];
        assert!(s.get("absent").is_none(), "{realm}: AbortSignal is not exposed");
        assert_eq!(s["length"].as_u64(), Some(0), "{realm}: AbortSignal.length");
        assert_eq!(s["nat"], serde_json::json!(true), "{realm}: the interface object is [native code]");
        // EXACT, both realms. Measured on `google-chrome --headless=new`
        // 150.0.7871.46 over a loopback server, 2026-09-12, in a page AND in a real worker.
        assert_eq!(
            s["newable"],
            serde_json::json!("TypeError: Failed to construct 'AbortSignal': Illegal constructor"),
            "{realm}: `new AbortSignal()`"
        );
        assert_eq!(strs(&s["protoOrder"]), vec!["aborted", "reason", "onabort", "throwIfAborted", "constructor"], "{realm}: the capture's order");
        assert_eq!(strs(&s["ctorOrder"]), vec!["length", "name", "prototype", "abort", "any", "timeout"], "{realm}: the three statics, in the capture's order");
        assert_eq!(strs(&s["protoChain"]), vec!["EventTarget", "Object"], "{realm}: AbortSignal.prototype -> EventTarget.prototype");
        assert_eq!(s["ctorProtoName"], serde_json::json!("EventTarget"), "{realm}: Object.getPrototypeOf(AbortSignal) === EventTarget");
        assert_eq!(s["tag"], serde_json::json!({"v": "AbortSignal", "e": false, "c": true, "w": false}), "{realm}");

        // Every row native, with the WebIDL name/length, no own-tell, and the capture's flags.
        for (iface, rows) in [("AbortController", &["signal", "abort"][..]), ("AbortSignal", &["aborted", "reason", "onabort", "throwIfAborted"][..])] {
            for m in rows {
                let r = &d["rows"][iface][*m];
                assert_eq!(r["e"], serde_json::json!(true), "{realm}: {iface}.{m} enumerable");
                assert_eq!(r["c"], serde_json::json!(true), "{realm}: {iface}.{m} configurable");
                assert_eq!(r["nat"], serde_json::json!(true), "{realm}: {iface}.{m} must be [native code]");
                if r["k"] == serde_json::json!("a") {
                    assert_eq!(r["gn"], serde_json::json!(format!("get {m}")), "{realm}: {iface}.{m} getter name");
                    assert_eq!(r["gt"], serde_json::json!(false), "{realm}: {iface}.{m} getter own-tell");
                    // `onabort` is the ONE read/write row; the other three are readonly attributes.
                    assert_eq!(r["s"], serde_json::json!(*m == "onabort"), "{realm}: {iface}.{m} setter presence");
                } else {
                    assert_eq!(r["fn"], serde_json::json!(*m), "{realm}: {iface}.{m} function name");
                    assert_eq!(r["fl"].as_u64(), Some(0), "{realm}: {iface}.{m} length");
                    assert_eq!(r["ft"], serde_json::json!(false), "{realm}: {iface}.{m} own-tell");
                }
            }
        }
        for (m, len) in [("abort", 0u64), ("any", 1), ("timeout", 1)] {
            let r = &d["statics"]["AbortSignal"][m];
            assert_eq!(r["nat"], serde_json::json!(true), "{realm}: AbortSignal.{m} must be [native code]");
            assert_eq!(r["fn"], serde_json::json!(m), "{realm}: AbortSignal.{m} name");
            assert_eq!(r["fl"].as_u64(), Some(len), "{realm}: AbortSignal.{m} length");
            assert_eq!(r["ft"], serde_json::json!(false), "{realm}: AbortSignal.{m} own-tell");
        }
        assert!(d["statics"]["AbortController"].as_object().unwrap().is_empty(), "{realm}: AbortController has no statics");
    }
}

/// FAILURE MODE 2. All eight receiver/row pairs throw `Illegal invocation`, as Chrome does. None of
/// these rows returns a promise, so all eight are synchronous throws.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        for iface in ["AbortController", "AbortSignal"] {
            let f = d["foreign"][iface].as_object().unwrap_or_else(|| panic!("{realm}: no foreign map for {iface}"));
            assert!(!f.is_empty(), "{realm}: {iface} has no rows to check");
            for (k, v) in f {
                assert_eq!(v.as_str().unwrap_or(""), "TypeError: Illegal invocation", "{realm}: {iface}.prototype.{k} accepted a foreign receiver");
            }
        }
    }
}

/// FAILURE MODE 1 and 3. Nothing own on either object, and the whole abort ALGORITHM runs: the flag,
/// the default `DOMException` reason, exactly one `abort` event through both listener kinds,
/// idempotence, `throwIfAborted`, and the three statics including propagation and the timer.
#[tokio::test]
async fn the_abort_algorithm_runs_end_to_end_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let v = &d["vals"];
        assert_eq!(v["ctlOwn"], serde_json::json!([]), "{realm}: an AbortController owns nothing (Chrome: [])");
        assert_eq!(v["sigOwn"], serde_json::json!([]), "{realm}: an AbortSignal owns nothing (Chrome: [])");
        assert_eq!(v["sigTag"], serde_json::json!("[object AbortSignal]"), "{realm}");
        assert_eq!(v["sameObject"], serde_json::json!(true), "{realm}: `signal` is [SameObject]");
        assert_eq!(v["isEventTarget"], serde_json::json!(true), "{realm}: a signal IS an EventTarget");
        assert_eq!(v["isSignal"], serde_json::json!(true), "{realm}: ...and an AbortSignal");
        assert_eq!(v["before"], serde_json::json!([false, null, null]), "{realm}: aborted/reason/onabort before an abort");
        assert_eq!(v["throwIfNotAborted"], serde_json::json!("OK:undefined"), "{realm}: throwIfAborted is a no-op until it aborts");
        // ONE event, reaching the addEventListener listener and `onabort`, in registration order, and
        // a second abort() does nothing at all.
        assert_eq!(v["events"], serde_json::json!(["listener:abort:true", "onabort:abort"]), "{realm}: one abort event, both listener kinds, in order");
        assert_eq!(
            v["after"], serde_json::json!([true, true, "AbortError", "signal is aborted without reason"]),
            "{realm}: the default reason is a DOMException with Chrome's exact message"
        );
        assert_eq!(v["throwIfAborted"], serde_json::json!("AbortError: signal is aborted without reason"), "{realm}: throwIfAborted throws the reason");
        assert_eq!(v["abortWithReason"], serde_json::json!([true, "boom"]), "{realm}: an explicit reason is kept as-is");
        assert_eq!(v["abortUndefined"], serde_json::json!([true, "AbortError"]), "{realm}: an explicit undefined takes the default reason");
        assert_eq!(v["staticAbort"], serde_json::json!([true, true, "AbortError", true]), "{realm}: AbortSignal.abort()");
        assert_eq!(v["staticAbortReason"], serde_json::json!([true, 42]), "{realm}: AbortSignal.abort(42)");
        // Chrome's three `any` messages and two `timeout` messages, verbatim.
        assert_eq!(
            v["anyErrs"],
            serde_json::json!({
                "noArg": "TypeError: Failed to execute 'any' on 'AbortSignal': 1 argument required, but only 0 present.",
                "notIterable": "TypeError: Failed to execute 'any' on 'AbortSignal': The provided value cannot be converted to a sequence.",
                "badElement": "TypeError: Failed to execute 'any' on 'AbortSignal': Failed to convert value to 'AbortSignal'.",
            }),
            "{realm}: AbortSignal.any's WebIDL failures"
        );
        assert_eq!(
            v["timeoutErrs"],
            serde_json::json!({
                "noArg": "TypeError: Failed to execute 'timeout' on 'AbortSignal': 1 argument required, but only 0 present.",
                "negative": "TypeError: Failed to execute 'timeout' on 'AbortSignal': Value is outside the 'unsigned long long' value range.",
                // [EnforceRange] TRUNCATES a fractional value; it is not an error. The JS class threw.
                "fractional": "OK:[object AbortSignal]",
            }),
            "{realm}: AbortSignal.timeout's WebIDL failures"
        );
        assert_eq!(v["anyAlreadyAborted"], serde_json::json!([true, "x"]), "{realm}: any() over an already-aborted source is born aborted with its reason");
        assert_eq!(
            v["anyPropagates"], serde_json::json!([true, "propagated", ["any:propagated"]]),
            "{realm}: aborting a source aborts the composite, with the source's reason, and fires once"
        );
        assert_eq!(
            v["timeout"], serde_json::json!([true, "TimeoutError", true, ["timeout:TimeoutError"]]),
            "{realm}: AbortSignal.timeout fires through this realm's setTimeout with a TimeoutError DOMException"
        );
    }
}

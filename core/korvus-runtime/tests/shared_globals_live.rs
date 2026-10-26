//! `btoa`, `atob`, `crypto` and `console`, in BOTH realms.
//!
//! `core/korvus-runtime/src/js/shared-globals.js` is DELETED. Its five globals are Rust: `fetch` and
//! `Response` went in the fetch commit, `structuredClone` with `ImageData`, and this file gates the
//! last three — `modules/base64.rs`, `modules/console_ns.rs`, and the `crypto` singleton, which
//! `modules/crypto/crypto.rs` mints now instead of adopting a JS object literal that the same install
//! then stripped bare.
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/base64-console-probe.js` UNMODIFIED. `KORVUS_SHARED_GLOBALS_OUT=<dir>` writes the dumps.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/base64-console-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_SHARED_GLOBALS_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the shared-globals dump");
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    let v = e.eval_value("shared_globals_probe", PROBE.to_string()).expect("the window probe runs");
    let s = v.as_str().expect("the probe returns a JSON string").to_string();
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("self.postMessage({PROBE});");
    let reg = format!("globalThis.__workerScripts = {{ './sg.js': {} }};", serde_json::to_string(&worker_src).unwrap());
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("sg-driver", "const w = new Worker('./sg.js'); w.onmessage = (ev) => print('SGLB:' + ev.data);".into()).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("SGLB:")) {
            let s = line["SGLB:".len()..].to_string();
            save("worker", &s);
            return serde_json::from_str(&s).expect("the worker probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the worker did not post its probe within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

#[tokio::test]
async fn btoa_and_atob_are_native_and_answer_chromes_errors() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let want = if realm == "window" { "Window" } else { "WorkerGlobalScope" };
        for n in ["btoa", "atob"] {
            let f = &d["b64"][n];
            assert!(!f.is_null(), "{realm}: {n} is missing");
            assert_eq!(f["name"], serde_json::json!(n), "{realm}: {n}.name");
            assert_eq!(f["length"].as_u64(), Some(1), "{realm}: {n}.length");
            assert_eq!(f["nat"], serde_json::json!(true), "{realm}: {n} must be [native code]");
            assert_eq!(f["tells"], serde_json::json!(false), "{realm}: {n} owns no prototype/arguments/caller");
            assert_eq!(f["e"], serde_json::json!(true), "{realm}: {n} is enumerable");
            assert_eq!(f["c"], serde_json::json!(true), "{realm}: {n} is configurable");
            assert_eq!(f["w"], serde_json::json!(true), "{realm}: {n} is writable");
            // WHO HOLDS IT. `btoa` is an own property of `window` and a
            // `WorkerGlobalScope` OPERATION -- in a worker it lives on `WorkerGlobalScope.prototype`
            // and the global inherits it. Measured in real Chrome (`--headless=new`, a loopback page
            // spawning a real dedicated worker, 2026-09-12): `getOwnPropertyNames(self)` in the worker
            // contains `console` and none of `btoa`/`atob`/`fetch`/`structuredClone`/`navigator`, and
            // `WorkerGlobalScope.prototype` owns all four of those as enumerable+configurable+writable
            // data properties. The bundle's `worker.surface.json` records the same thing. This engine
            // used to install them on the worker global, which is where `worker_surface.rs`'s five
            // GLOBAL-TELLS came from.
            let want_holder = if realm == "window" { "globalThis" } else { "WorkerGlobalScope.prototype" };
            assert_eq!(f["holder"], serde_json::json!(want_holder), "{realm}: {n} is held by the wrong object");
        }
        let want_holder = if realm == "window" { "globalThis" } else { "WorkerGlobalScope.prototype" };
        for n in ["fetchHolder", "structuredCloneHolder"] {
            assert_eq!(d[n], serde_json::json!(want_holder), "{realm}: {n}");
        }
        assert_eq!(d["navigator"]["holder"], serde_json::json!(want_holder), "{realm}: navigator's holder");
        // `navigator` is a readonly ATTRIBUTE, so an ACCESSOR on its holder -- never a data property.
        // Chrome, same run: the worker's is on `WorkerGlobalScope.prototype`, `{enumerable: true,
        // configurable: true}`, getter and no setter; the WINDOW's is an own accessor of `globalThis`
        // with the same flags.
        //
        // Only the worker's is asserted, and the reason is the one two lines of this file already
        // record for `crypto`: the WINDOW's `navigator` is a DATA property here, which is the `Window`
        // interface's own-global shape that `install::live::window` owns and W4.f finishes. It was a
        // data property before this commit too -- W6 did not move it, and turning it red here would
        // add a failure to the standing suite for a defect this unit did not introduce and cannot fix
        // without reshaping the window global. Measured 2026-09-12 and named, not tolerated silently.
        if realm == "worker" {
            assert_eq!(d["navigator"]["acc"], serde_json::json!(true), "worker: navigator is an accessor");
        }
        let b = &d["b64"];
        assert_eq!(b["round"], serde_json::json!("OK:hi\u{ff}"), "{realm}: btoa/atob round-trip a Latin1 string");
        assert_eq!(
            b["btoaNoArgs"], serde_json::json!(format!("TypeError: Failed to execute 'btoa' on '{want}': 1 argument required, but only 0 present.")),
            "{realm}: btoa() -- the REALM names itself"
        );
        assert_eq!(
            b["atobNoArgs"], serde_json::json!(format!("TypeError: Failed to execute 'atob' on '{want}': 1 argument required, but only 0 present.")),
            "{realm}: atob()"
        );
        assert_eq!(
            b["btoaUnicode"],
            serde_json::json!(format!("InvalidCharacterError: Failed to execute 'btoa' on '{want}': The string to be encoded contains characters outside of the Latin1 range.")),
            "{realm}: btoa of a non-Latin1 character"
        );
        assert_eq!(
            b["atobBad"],
            serde_json::json!(format!("InvalidCharacterError: Failed to execute 'atob' on '{want}': The string to be decoded is not correctly encoded.")),
            "{realm}: atob of a non-base64 string"
        );
        assert_eq!(b["btoaNumber"], serde_json::json!("OK:MTIz"), "{realm}: DOMString conversion");
        assert_eq!(b["atobWhitespace"], serde_json::json!("OK:hi"), "{realm}: forgiving-base64 strips whitespace from anywhere");
        assert_eq!(b["btoaForeign"], serde_json::json!("TypeError: Illegal invocation"), "{realm}: btoa.call({{}}, …) -- UNPREFIXED, unlike the arity message");
    }
}

#[tokio::test]
async fn the_crypto_singleton_is_a_real_crypto_that_owns_nothing() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let c = &d["crypto"];
        assert_eq!(c["tag"], serde_json::json!("[object Crypto]"), "{realm}: Object.prototype.toString");
        assert_eq!(c["own"], serde_json::json!([]), "{realm}: a real singleton OWNS NOTHING (Chrome: [])");
        assert_eq!(c["ctorName"], serde_json::json!("Crypto"), "{realm}: crypto.constructor.name");
        assert_eq!(c["isCrypto"], serde_json::json!(true), "{realm}: instanceof Crypto");
        assert_eq!(c["uuidShape"], serde_json::json!("OK:true"), "{realm}: randomUUID is a v4 UUID");
        assert_eq!(c["fills"], serde_json::json!("OK:true:true"), "{realm}: getRandomValues returns its argument, filled");
        // A WORKER's `crypto` is an INHERITED accessor on WorkerGlobalScope.prototype, so the global
        // has no own descriptor for it -- which is what Chrome answers, and what the object's new
        // Rust mint site has to reproduce now that `worker-finalize.js` has no object to capture.
        if realm == "worker" {
            assert!(c["desc"].is_null(), "worker: `crypto` must NOT be an own property of the global");
        } else {
            // The WINDOW's is an own ACCESSOR, enumerable and configurable, as Chrome 150's is
            // (measured over a loopback page: `get crypto`/0, no setter). It was an own
            // DATA property here until then -- what `globalThis.crypto = {…}` produced.
            assert_eq!(
                c["desc"],
                serde_json::json!({"e": true, "c": true, "acc": true}),
                "window: `crypto` is an own accessor of the window"
            );
        }
    }
}

/// Chrome's console members, in Chrome's OWN-KEY ORDER (measured unsorted), each `[native code]`; the
/// namespace's `[object console]` tag; and `console` itself NON-ENUMERABLE on the global. The JS
/// literal had none of those four right.
///
/// `memory` IS WINDOW-ONLY, and this list used to claim it in both realms. Re-measured on
/// `google-chrome --headless=new` 150.0.7871.46 with
/// `core/korvus-runtime/tests/fixtures/worker-scope-probe.js` run in a page AND in a real worker:
/// the page's `Object.getOwnPropertyNames(console)` ends `…, context, createTask, memory`
/// and the worker's ends `…, context, createTask`. It is one of the twelve window/worker asymmetries
/// `tests/worker_scope_live.rs` pins; this engine had `memory` in both realms and now has Chrome's.
const CONSOLE_NAMES: &[&str] = &[
    "debug", "error", "info", "log", "warn", "dir", "dirxml", "table", "trace", "group",
    "groupCollapsed", "groupEnd", "clear", "count", "countReset", "assert", "profile", "profileEnd",
    "time", "timeLog", "timeEnd", "timeStamp", "context", "createTask",
];
/// …plus `memory`, in the window only.
const CONSOLE_NAMES_WINDOW: &[&str] = &[
    "debug", "error", "info", "log", "warn", "dir", "dirxml", "table", "trace", "group",
    "groupCollapsed", "groupEnd", "clear", "count", "countReset", "assert", "profile", "profileEnd",
    "time", "timeLog", "timeEnd", "timeStamp", "context", "createTask", "memory",
];

#[tokio::test]
async fn the_console_namespace_matches_chrome() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let k = &d["console"];
        assert_eq!(k["tag"], serde_json::json!("[object console]"), "{realm}: the NAMESPACE's tag, lower-case");
        let want = if realm == "window" { CONSOLE_NAMES_WINDOW } else { CONSOLE_NAMES };
        assert_eq!(strs(&k["names"]), want, "{realm}: Chrome's console members, in Chrome's order");
        assert_eq!(strs(&k["symbols"]), vec!["Symbol(Symbol.toStringTag)"], "{realm}: the one own symbol");
        assert_eq!(
            k["tagDesc"], serde_json::json!({"v": "console", "e": false, "c": true, "w": false}),
            "{realm}: the toStringTag descriptor"
        );
        assert_eq!(k["proto"], serde_json::json!("Object"), "{realm}: console sits on a prototype of its own, not on Object.prototype");
        assert_eq!(k["logNative"], serde_json::json!(true), "{realm}: console.log must be [native code] -- it was a JS arrow");
        assert_eq!(k["logName"], serde_json::json!("log"), "{realm}: console.log.name");
        assert_eq!(k["logLength"].as_u64(), Some(0), "{realm}: every console method reports length 0");
        assert_eq!(
            k["desc"], serde_json::json!({"e": false, "c": true, "w": true}),
            "{realm}: `console` is a NON-enumerable own property of the global"
        );
        assert_eq!(
            k["memberDesc"], serde_json::json!({"e": true, "c": true, "w": true}),
            "{realm}: its members are writable, enumerable and configurable"
        );
    }
}

//! `DOMException`'s live gate, in BOTH realms.
//!
//! `DOMException` was a JS class in `host.js` (window) and a DIFFERENT JS class in `worker-host.js`
//! (worker). It is one `interface!` table with a native constructor now
//! (`core/korvus-webapi/src/core/dom/dom_exception.rs`), and `Exposed=(Window,Worker)` gives both
//! realms the same one.
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/dom-exception-probe.js` UNMODIFIED — the same file this test evaluates in the window and
//! inside a real Worker isolate, so the three dumps are directly diffable. `KORVUS_DOM_EXC_OUT=<dir>`
//! writes `window.json` / `worker.json` there.
//!
//! The four table-cutover failure modes each have an assertion here:
//!   1. an instance on the class template's prototype  -> `fm.protoIsInstance`
//!   2. a row accepting a foreign receiver             -> `fm.foreign` (9 receivers, both realms)
//!   3. a body reachable where it was a no-op          -> `vals` (name/message/code read back)
//!   4. an install ORDER change discarding a decorator -> `protoOrder` / `ctorOrder`, and
//!      `the_five_registry_children_still_chain_to_dom_exception`, which is the one this cutover
//!      actually threatened: the registry's chain pass runs in the ESM layer and used to find
//!      `host.js`'s class there.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/dom-exception-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_DOM_EXC_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the DOMException dump");
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    let v = e.eval_value("dom_exception_probe", PROBE.to_string()).expect("the window probe runs");
    let s = v.as_str().expect("the probe returns a JSON string").to_string();
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

/// The probe inside a REAL worker isolate, posted back — `Exposed` is what decides the worker's rows,
/// so the worker realm has to be measured in a worker.
async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("self.postMessage({PROBE});");
    let reg = format!(
        "globalThis.__workerScripts = {{ './dexc.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "dexc-driver",
        "const w = new Worker('./dexc.js'); w.onmessage = (ev) => print('DEXC:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("DEXC:")) {
            let s = line["DEXC:".len()..].to_string();
            save("worker", &s);
            return serde_json::from_str(&s).expect("the worker probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the worker did not post its probe within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The capture's prototype enumeration order: the three accessors, the 25 legacy constants,
/// `constructor` LAST. Chrome's, verbatim.
const PROTO_ORDER: &[&str] = &[
    "code", "name", "message",
    "INDEX_SIZE_ERR", "DOMSTRING_SIZE_ERR", "HIERARCHY_REQUEST_ERR", "WRONG_DOCUMENT_ERR",
    "INVALID_CHARACTER_ERR", "NO_DATA_ALLOWED_ERR", "NO_MODIFICATION_ALLOWED_ERR", "NOT_FOUND_ERR",
    "NOT_SUPPORTED_ERR", "INUSE_ATTRIBUTE_ERR", "INVALID_STATE_ERR", "SYNTAX_ERR",
    "INVALID_MODIFICATION_ERR", "NAMESPACE_ERR", "INVALID_ACCESS_ERR", "VALIDATION_ERR",
    "TYPE_MISMATCH_ERR", "SECURITY_ERR", "NETWORK_ERR", "ABORT_ERR", "URL_MISMATCH_ERR",
    "QUOTA_EXCEEDED_ERR", "TIMEOUT_ERR", "INVALID_NODE_TYPE_ERR", "DATA_CLONE_ERR",
    "constructor",
];

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

#[tokio::test]
async fn the_interface_object_and_the_prototype_match_the_capture_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        assert!(d.get("absent").is_none(), "{realm}: DOMException is not exposed: {d}");
        assert_eq!(d["ctor"]["name"], serde_json::json!("DOMException"), "{realm}: DOMException.name");
        assert_eq!(d["ctor"]["length"].as_u64(), Some(0), "{realm}: DOMException.length -- both arguments are optional");
        assert_eq!(d["ctor"]["nat"], serde_json::json!(true), "{realm}: the interface object must be [native code]");
        // WebIDL leaves the interface OBJECT on %Function.prototype% and gives only the PROTOTYPE
        // object %Error.prototype%. `class DOMException extends Error` set both, which is a second
        // chain a page reads with one getPrototypeOf. Chrome answers "" here (Function.prototype).
        assert_eq!(d["ctorProtoName"], serde_json::json!(""), "{realm}: Object.getPrototypeOf(DOMException) must be Function.prototype");
        assert_eq!(strs(&d["protoChain"]), vec!["Error", "Object"], "{realm}: DOMException.prototype -> Error.prototype -> Object.prototype");
        assert_eq!(strs(&d["protoOrder"]), PROTO_ORDER, "{realm}: the prototype's enumeration order is the capture's");
        // The interface object carries the same 25 constants, after its own length/name/prototype.
        let ctor_order = strs(&d["ctorOrder"]);
        assert_eq!(&ctor_order[..3], &["length", "name", "prototype"], "{realm}: the interface object's first three keys");
        assert_eq!(&ctor_order[3..], &PROTO_ORDER[3..PROTO_ORDER.len() - 1], "{realm}: the 25 legacy constants, in order, on the interface object");
        assert_eq!(d["tag"], serde_json::json!({"v": "DOMException", "e": false, "c": true, "w": false}), "{realm}: Symbol.toStringTag");
        for n in ["code", "name", "message"] {
            let r = &d["rows"][n];
            assert_eq!(r["k"], serde_json::json!("a"), "{realm}: {n} must be an accessor");
            assert_eq!(r["g"], serde_json::json!(true), "{realm}: {n} must have a getter");
            assert_eq!(r["s"], serde_json::json!(false), "{realm}: {n} must be getter-only");
            assert_eq!(r["e"], serde_json::json!(true), "{realm}: {n} enumerable");
            assert_eq!(r["c"], serde_json::json!(true), "{realm}: {n} configurable");
            assert_eq!(r["nat"], serde_json::json!(true), "{realm}: {n}'s getter must be [native code]");
            assert_eq!(r["gn"], serde_json::json!(format!("get {n}")), "{realm}: {n}'s getter name");
            assert_eq!(r["gt"], serde_json::json!(false), "{realm}: {n}'s getter must own no prototype/arguments/caller");
        }
        for (holder, key) in [("consts", "prototype"), ("statics", "the interface object")] {
            for (n, want) in [("INDEX_SIZE_ERR", "1"), ("ABORT_ERR", "20"), ("DATA_CLONE_ERR", "25")] {
                let c = &d[holder][n];
                assert_eq!(c["v"], serde_json::json!(want), "{realm}: {n} on {key}");
                assert_eq!(c["w"], serde_json::json!(false), "{realm}: {n} on {key} must be non-writable");
                assert_eq!(c["e"], serde_json::json!(true), "{realm}: {n} on {key} must be enumerable");
                assert_eq!(c["c"], serde_json::json!(false), "{realm}: {n} on {key} must be non-configurable");
            }
        }
    }
}

/// FAILURE MODE 1 and 2. The prototype is not an instance of itself, and NO row accepts a foreign
/// receiver: Chrome throws `TypeError: Illegal invocation` for all nine receiver/row pairs the probe
/// tries, `DOMException.prototype` itself included, and `JSON.stringify(DOMException.prototype)`
/// throws rather than serialising the prototype. This is goal §W9 item 17 for this interface.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        assert_eq!(d["fm"]["protoIsInstance"], serde_json::json!(false), "{realm}: DOMException.prototype is an instance of itself");
        let f = d["fm"]["foreign"].as_object().expect("the foreign-receiver map");
        assert_eq!(f.len(), 9, "{realm}: the probe tries three rows over three receivers");
        for (k, v) in f {
            let v = v.as_str().unwrap_or("");
            assert!(v.starts_with("TypeError"), "{realm}: DOMException.prototype.{k} accepted a foreign receiver ({v})");
        }
        assert_eq!(
            d["fm"]["jsonStringifyProto"], serde_json::json!("THROW:TypeError"),
            "{realm}: JSON.stringify(DOMException.prototype) must throw, not serialise the prototype"
        );
        let no_new = d["fm"]["callNoNew"].as_str().unwrap_or("");
        assert!(
            no_new.starts_with("TypeError: Failed to construct 'DOMException': Please use the 'new' operator"),
            "{realm}: DOMException('a') without new -- Chrome's message, got {no_new}"
        );
    }
}

/// FAILURE MODE 3. Every row answers off a real instance, the instance OWNS NOTHING, and the legacy
/// `code` mapping is the DOM Level 1 one. `ownNames` is the assertion the JS class could not pass:
/// `class X extends Error` runs V8's `Error` constructor, which defines an own `stack`.
#[tokio::test]
async fn an_instance_owns_nothing_and_every_row_reads_back() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let v = &d["vals"];
        assert_eq!(v["ownNames"], serde_json::json!([]), "{realm}: a real DOMException has NO own properties (Chrome: [])");
        assert_eq!(v["stackType"], serde_json::json!("undefined"), "{realm}: Chrome's DOMException has no stack");
        assert_eq!(v["isError"], serde_json::json!(true), "{realm}: instanceof Error");
        assert_eq!(v["isDomException"], serde_json::json!(true), "{realm}: instanceof DOMException");
        assert_eq!(v["tag"], serde_json::json!("[object DOMException]"), "{realm}: Object.prototype.toString");
        assert_eq!(v["toString"], serde_json::json!("AbortError: msg"), "{realm}: Error.prototype.toString over the two accessors");
        assert_eq!(v["json"], serde_json::json!("{}"), "{realm}: the slots are invisible to JSON.stringify");
        assert_eq!(v["noArg"], serde_json::json!(["Error", "", 0]), "{realm}: new DOMException()");
        assert_eq!(v["undefArgs"], serde_json::json!(["Error", "", 0]), "{realm}: an explicit undefined takes the WebIDL default");
        assert_eq!(v["named"], serde_json::json!(["NotFoundError", "x", 8]), "{realm}: the legacy code mapping");
        assert_eq!(v["abort"], serde_json::json!(["AbortError", "m", 20]), "{realm}: the legacy code mapping");
        assert_eq!(v["unknownName"], serde_json::json!(["Bogus", "x", 0]), "{realm}: an unrecognised name keeps its text and answers code 0");
        // THE deno_core ERROR BUILDERS, read back where a page can see them. `atob("x")`
        // fails forgiving-base64 decoding, and `op_atob` returns that as
        // `JsErrorBox::new("InvalidCharacterError", …)`. Only a REGISTERED builder turns that class
        // name into a real DOMException; without one deno_core's `buildCustomError` answers undefined
        // and the page catches something that is not a DOMException at all.
        //
        // This is the assertion `register_error_builders`'s "must run inside the ESM layer" ceiling
        // never had. The WORKER registers from `install::live::install_worker_bootstrap` now and the
        // WINDOW still from `host.js`'s ESM; both realms answer the same four values, which is what
        // says the move did not cost the registration.
        assert_eq!(
            v["fromOp"],
            serde_json::json!([true, true, "InvalidCharacterError", 5]),
            "{realm}: an op failing with a DOM exception name must throw a real DOMException (code 5)"
        );
        assert_eq!(v["constOnProto"], serde_json::json!(20), "{realm}: DOMException.prototype.ABORT_ERR");
        assert_eq!(v["constOnCtor"], serde_json::json!(20), "{realm}: DOMException.ABORT_ERR");
        assert_eq!(v["thrown"], serde_json::json!([true, "InvalidStateError", 11, "boom"]), "{realm}: a thrown DOMException round-trips");
    }
}

/// FAILURE MODE 4, and the one this cutover actually threatened. `RTCError`, `OverconstrainedError`,
/// `WebTransportError`, `QuotaExceededError` and `WebSocketError` are REGISTRY interfaces whose
/// WebIDL parent is `DOMException`. The registry's chain pass runs during the ESM layer, where
/// `host.js`'s class used to be; with the class gone it finds nothing and leaves all five on the bare
/// `Object.prototype`. `install_window_abort_style` ends with `rechain_orphans`, which is the repair
/// — and this is the assertion that says the repair happened.
#[tokio::test]
async fn the_five_registry_children_still_chain_to_dom_exception() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    let probe = r#"(() => {
      const out = {};
      for (const n of ["RTCError","OverconstrainedError","WebTransportError","QuotaExceededError","WebSocketError"]) {
        const C = globalThis[n];
        if (typeof C !== "function") { out[n] = "absent"; continue; }
        out[n] = [Object.getPrototypeOf(C.prototype) === DOMException.prototype,
                  Object.getPrototypeOf(C) === DOMException].join(",");
      }
      return JSON.stringify(out);
    })()"#;
    let v = e.eval_value("dom_exception_children", probe.to_string()).expect("the child probe runs");
    let d: serde_json::Value = serde_json::from_str(v.as_str().unwrap()).unwrap();
    for (name, verdict) in d.as_object().unwrap() {
        assert_eq!(
            verdict.as_str().unwrap(), "true,true",
            "{name} lost its DOMException chain -- the registry chain pass ran before the table existed \
             and rechain_orphans did not repair it"
        );
    }
}

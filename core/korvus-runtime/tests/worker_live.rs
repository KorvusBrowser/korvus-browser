//! `Worker`'s live gate.
//!
//! `host.js`'s `class Worker extends EventTarget` (the constructor, the four `UNREF`'d receive pumps
//! and the two `Object.defineProperty` mounts) is an `interface!` table with a native constructor
//! (`core/korvus-webapi/src/core/workers/worker.rs`). It was the SECOND of the three ESM-time
//! `extends EventTarget` declarations W4.b named as the reason `events.js` still exists.
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/worker-iface-probe.js` UNMODIFIED — the same file this test evaluates in the window,
//! where it reaches a real worker isolate through a Blob URL exactly as it does in Chrome.
//! `KORVUS_WORKER_OUT=<dir>` writes `window.json` there.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/worker-iface-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_WORKER_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the worker dump");
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("worker_iface_probe", format!("({PROBE}).then((s) => print('WIFACE:' + s));")).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("WIFACE:")) {
            let s = line["WIFACE:".len()..].to_string();
            save("window", &s);
            return serde_json::from_str(&s).expect("the window probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("the Worker probe never reported within 30s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

#[tokio::test]
async fn the_interface_object_and_prototype_match_the_capture() {
    let d = dump_window().await;
    let i = &d["iface"];
    assert!(i.get("absent").is_none(), "Worker is not exposed");
    assert_eq!(i["name"], serde_json::json!("Worker"), "Worker.name");
    assert_eq!(i["length"].as_u64(), Some(1), "Worker.length -- scriptURL is required");
    assert_eq!(i["nat"], serde_json::json!(true), "the interface object must be [native code]");
    assert_eq!(strs(&i["protoChain"]), vec!["EventTarget", "Object"], "Worker.prototype -> EventTarget.prototype");
    assert_eq!(i["ctorProtoName"], serde_json::json!("EventTarget"), "Object.getPrototypeOf(Worker) === EventTarget");
    assert_eq!(strs(&i["ctorOrder"]), vec!["length", "name", "prototype"], "Worker has no statics");
    assert_eq!(i["tag"], serde_json::json!({"v": "Worker", "e": false, "c": true, "w": false}), "Worker's toStringTag");
    assert_eq!(
        i["noArgs"], serde_json::json!("TypeError: Failed to construct 'Worker': 1 argument required, but only 0 present."),
        "`new Worker()` -- Chrome's message"
    );
    assert!(
        i["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'Worker': Please use the 'new' operator"),
        "`Worker('x')` without new"
    );
    // The capture's member order (`generated_order.rs` recorded exactly these four, in this order).
    // `constructor` is at a DIFFERENT position in Chrome -- see `constructor_sits_where_this_installer_puts_it`.
    let proto: Vec<String> = strs(&i["protoOrder"]).into_iter().filter(|n| n != "constructor").collect();
    assert_eq!(proto, vec!["onmessage", "postMessage", "terminate", "onerror"], "the capture's member order");
    for (m, kind, len) in [("onmessage", "a", 0u64), ("postMessage", "d", 1), ("terminate", "d", 0), ("onerror", "a", 0)] {
        let r = &d["rows"][m];
        assert_eq!(r["e"], serde_json::json!(true), "Worker.{m} enumerable");
        assert_eq!(r["c"], serde_json::json!(true), "Worker.{m} configurable");
        assert_eq!(r["nat"], serde_json::json!(true), "Worker.{m} must be [native code]");
        assert_eq!(r["k"], serde_json::json!(kind), "Worker.{m} kind");
        if kind == "a" {
            assert_eq!(r["s"], serde_json::json!(true), "Worker.{m} is an event-handler pair");
            assert_eq!(r["gn"], serde_json::json!(format!("get {m}")), "Worker.{m} getter name");
            assert_eq!(r["gt"], serde_json::json!(false), "Worker.{m} getter own-tell");
        } else {
            assert_eq!(r["fn"], serde_json::json!(m), "Worker.{m} function name");
            assert_eq!(r["fl"].as_u64(), Some(len), "Worker.{m} length");
            assert_eq!(r["ft"], serde_json::json!(false), "Worker.{m} own-tell");
        }
    }
}

/// FAILURE MODE 2. All four rows, over `{}` and over the prototype itself, throw `Illegal
/// invocation` — which is what Chrome answers for all eight.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    let d = dump_window().await;
    let f = d["foreign"].as_object().expect("the foreign-receiver map");
    assert_eq!(f.len(), 8, "four rows over two receivers");
    for (k, v) in f {
        assert_eq!(v.as_str().unwrap_or(""), "TypeError: Illegal invocation", "Worker.prototype.{k} accepted a foreign receiver");
    }
}

/// FAILURE MODE 1 and 3. A real `Worker` owns NOTHING (the `__id` the JS class defined is a v8
/// private now), and the whole round trip runs: three `postMessage`s reach the isolate, two echoes
/// come back through BOTH `onmessage` and `addEventListener` with `target` set and a real
/// `MessageEvent`, and the worker's uncaught error arrives as an `error` event on the parent.
#[tokio::test]
async fn a_worker_round_trips_messages_and_reports_its_errors() {
    let d = dump_window().await;
    let v = &d["vals"];
    assert!(v.get("skipped").is_none(), "the window realm must be able to spawn a worker: {v}");
    assert_eq!(v["ownNames"], serde_json::json!([]), "a real Worker has NO own properties (Chrome: [])");
    assert_eq!(v["isEventTarget"], serde_json::json!(true), "a Worker IS an EventTarget");
    assert_eq!(v["isWorker"], serde_json::json!(true), "...and a Worker");
    assert_eq!(v["tag"], serde_json::json!("[object Worker]"), "Object.prototype.toString");
    assert_eq!(v["handlersNull"], serde_json::json!([null, null]), "onmessage/onerror read null before they are set");
    // Chrome's order for this script: echo one (both listener kinds), the error, echo two.
    assert_eq!(
        strs(&v["seen"]),
        vec![
            "onmessage:echo:one:true:true",
            "listener:echo:one",
            "onerror:true",
            "onmessage:echo:two:true:true",
            "listener:echo:two",
        ],
        "the message/error round trip, in Chrome's order"
    );
}

//! `SharedWorker`'s live gate.
//!
//! `host.js`'s `class SharedWorker extends EventTarget` — the LAST of the three ESM-time
//! `extends EventTarget` declarations W4.b named as the reason `events.js` still exists — is an
//! `interface!` table with a native constructor (`core/korvus-webapi/src/core/workers/shared_worker.rs`).
//! Its two `op_nativize` masks went with it.
//!
//! Every SHAPE expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/shared-worker-probe.js` UNMODIFIED, the same file this test evaluates in the window.
//! `KORVUS_SHARED_WORKER_OUT=<dir>` writes `window.json` there.
//!
//! THE ROUND TRIP IS KORVUS'S OWN, and this file says so rather than implying Chrome confirmed it:
//! Chrome's dump under `--virtual-time-budget` reported `seen: []` — its shared worker had not
//! answered by the time the page was dumped — so the echo assertion below is a regression gate on the
//! two main-side pumps this commit moved into Rust, not a parity claim.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/shared-worker-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_SHARED_WORKER_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the shared-worker dump");
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("shared_worker_probe", format!("({PROBE}).then((s) => print('SWIFACE:' + s));")).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("SWIFACE:")) {
            let s = line["SWIFACE:".len()..].to_string();
            save("window", &s);
            return serde_json::from_str(&s).expect("the window probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("the SharedWorker probe never reported within 30s; output: {out:?}");
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
    assert!(i.get("absent").is_none(), "SharedWorker is not exposed");
    assert_eq!(i["name"], serde_json::json!("SharedWorker"), "SharedWorker.name");
    assert_eq!(i["length"].as_u64(), Some(1), "SharedWorker.length");
    assert_eq!(i["nat"], serde_json::json!(true), "the interface object must be [native code]");
    assert_eq!(strs(&i["protoChain"]), vec!["EventTarget", "Object"], "SharedWorker.prototype -> EventTarget.prototype");
    assert_eq!(i["ctorProtoName"], serde_json::json!("EventTarget"), "Object.getPrototypeOf(SharedWorker) === EventTarget");
    assert_eq!(strs(&i["ctorOrder"]), vec!["length", "name", "prototype"], "no statics");
    assert_eq!(i["tag"], serde_json::json!({"v": "SharedWorker", "e": false, "c": true, "w": false}), "toStringTag is a DATA property");
    assert_eq!(
        i["noArgs"], serde_json::json!("TypeError: Failed to construct 'SharedWorker': 1 argument required, but only 0 present."),
        "`new SharedWorker()` -- Chrome's message"
    );
    assert!(
        i["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'SharedWorker': Please use the 'new' operator"),
        "`SharedWorker('x')` without new"
    );
    // `constructor` sits between the two in Chrome (`onerror` comes from the AbstractWorker mixin);
    // this installer puts it last for every table, and the capture records no position for it.
    let proto: Vec<String> = strs(&i["protoOrder"]).into_iter().filter(|n| n != "constructor").collect();
    assert_eq!(proto, vec!["port", "onerror"], "the capture's member order");
    let port = &d["rows"]["port"];
    assert_eq!(port["k"], serde_json::json!("a"), "port is an accessor");
    assert_eq!(port["s"], serde_json::json!(false), "port is readonly ([SameObject])");
    assert_eq!(port["nat"], serde_json::json!(true), "port's getter must be [native code]");
    assert_eq!(port["gn"], serde_json::json!("get port"), "port's getter name");
    assert_eq!(port["gt"], serde_json::json!(false), "port's getter own-tell");
    let onerror = &d["rows"]["onerror"];
    assert_eq!(onerror["k"], serde_json::json!("a"), "onerror is an accessor");
    assert_eq!(onerror["s"], serde_json::json!(true), "onerror is an event-handler pair");
    assert_eq!(onerror["nat"], serde_json::json!(true), "onerror's accessors must be [native code]");
    assert_eq!(onerror["gn"], serde_json::json!("get onerror"), "onerror's getter name");
    assert_eq!(onerror["sn"], serde_json::json!("set onerror"), "onerror's setter name");
}

/// FAILURE MODE 2. Both rows, over `{}` and over the prototype, throw `Illegal invocation`.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    let d = dump_window().await;
    let f = d["foreign"].as_object().expect("the foreign-receiver map");
    assert_eq!(f.len(), 4, "two rows over two receivers");
    for (k, v) in f {
        assert_eq!(v.as_str().unwrap_or(""), "TypeError: Illegal invocation", "SharedWorker.prototype.{k} accepted a foreign receiver");
    }
}

/// FAILURE MODE 1 and 3. The instance owns nothing, `port` is the `[SameObject]` MessagePort, the
/// `onerror` row is the realm's one event-handler store (so a non-callable clears it, as Chrome does),
/// and the two main-side pumps this commit moved into Rust still carry a message both ways.
#[tokio::test]
async fn a_shared_worker_round_trips_through_its_port() {
    let d = dump_window().await;
    let v = &d["vals"];
    assert!(v.get("skipped").is_none(), "the window realm must be able to open a shared worker: {v}");
    assert_eq!(v["ownNames"], serde_json::json!([]), "a real SharedWorker has NO own properties (Chrome: [])");
    assert_eq!(v["isEventTarget"], serde_json::json!(true), "a SharedWorker IS an EventTarget");
    assert_eq!(v["isShared"], serde_json::json!(true), "...and a SharedWorker");
    assert_eq!(v["tag"], serde_json::json!("[object SharedWorker]"), "Object.prototype.toString");
    assert_eq!(v["portIsMessagePort"], serde_json::json!(true), "port is a real MessagePort");
    assert_eq!(v["portSameObject"], serde_json::json!(true), "port is [SameObject]");
    assert_eq!(v["onerrorNull"], serde_json::json!(null), "onerror reads null before it is set");
    assert_eq!(v["onerrorRoundTrip"], serde_json::json!([true, "null-ish"]), "onerror round-trips a function and clears on a non-callable");
    assert_eq!(strs(&v["seen"]), vec!["port:shared:ping"], "the port round trip through the Rust main-side pumps");
}

/// THE SHARED WORKER'S GLOBAL SCOPE IS `SharedWorkerGlobalScope`, AND ITS NAME IS ON ITS GLOBAL.
///
/// This is not a shape nicety. `resources/testharness.js::create_test_environment` chooses a worker
/// environment by NAME first and `instanceof` second:
///
/// ```text
/// if ('DedicatedWorkerGlobalScope' in global_scope && global_scope instanceof DedicatedWorkerGlobalScope)
/// if ('SharedWorkerGlobalScope'    in global_scope && global_scope instanceof SharedWorkerGlobalScope)
/// ...
/// if ('WorkerGlobalScope'          in global_scope && global_scope instanceof WorkerGlobalScope)  -> DEDICATED
/// ```
///
/// so a shared worker whose global does not carry the name falls through to the DEDICATED
/// environment, which publishes its results down the dedicated `postMessage` plane — a plane a page
/// holding a `SharedWorker` has no end of. The file then reports nothing at all and leaves the WPT
/// denominator entirely: `html/infrastructure` 89 -> 88 and `html/webappapis` 248 -> 247 verdicts,
/// which is exactly the coverage regression `scripts/wpt-tiers.sh` caught at `064721c8`.
///
/// The capture cannot answer this question and never will: the rig enters a window and a DEDICATED
/// worker, so neither `SharedWorkerGlobalScope` nor `ServiceWorkerGlobalScope` appears in any
/// bundle's `worker.surface.json`, however many devices are captured. The realm's own global-scope
/// interface is the realm's fact, not the device's — which is why `install_worker_global_scope`
/// publishes it through [`installer::create_global_scope`] rather than through the capture-gated
/// create.
#[tokio::test]
async fn the_shared_worker_global_scope_is_reachable_by_name() {
    let d = dump_window().await;
    let s = &d["scope"];
    assert!(!s.is_null(), "the shared worker never reported its own global scope: {}", d["vals"]);
    assert_eq!(
        s["protoName"], serde_json::json!("SharedWorkerGlobalScope"),
        "Object.getPrototypeOf(self).constructor.name inside the shared worker"
    );
    assert_eq!(s["tag"], serde_json::json!("[object SharedWorkerGlobalScope]"), "Object.prototype.toString.call(self)");
    assert_eq!(
        s["isShared"], serde_json::json!(true),
        "`self instanceof SharedWorkerGlobalScope` — the second half of testharness.js's test"
    );
    // The names, exactly: a shared worker has its own scope and the base one, and is NOT a dedicated
    // or service worker. `DedicatedWorkerGlobalScope` being absent is what makes the NAME test above
    // load-bearing — testharness.js reaches the shared branch only because the dedicated one missed.
    assert_eq!(
        strs(&s["names"]), vec!["SharedWorkerGlobalScope", "WorkerGlobalScope"],
        "the global-scope names reachable inside a shared worker"
    );
}

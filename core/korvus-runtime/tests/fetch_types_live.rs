//! `Request` and `Response`'s live gate, in BOTH realms.
//!
//! `host.js`'s `class Request` and `shared-globals.js`'s `class Response` are native constructors on
//! the tables that already owned their prototypes
//! (`core/korvus-webapi/src/modules/fetch/{request,response}.rs`).
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/fetch-types-probe.js` UNMODIFIED — the same file this test evaluates in the window and
//! inside a real Worker isolate. `KORVUS_FETCH_TYPES_OUT=<dir>` writes `window.json` / `worker.json`.
//!
//! THE PROBE NEVER TOUCHES THE NETWORK. `fetch`'s live path — including the Service-Worker
//! interception — is `tests/worker.rs::service_worker_fetch_interception`, which drives a real SW
//! isolate through `respondWith` and asserts the page's `fetch("/data")` resolves with the SW's body.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/fetch-types-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_FETCH_TYPES_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the fetch-types dump");
    }
}

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
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("fetch_types_probe", format!("({PROBE}).then((s) => print('FTYP:' + s));")).unwrap();
    let s = pump_for(&mut e, "FTYP:").await;
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("({PROBE}).then((s) => self.postMessage(s));");
    let reg = format!("globalThis.__workerScripts = {{ './ftyp.js': {} }};", serde_json::to_string(&worker_src).unwrap());
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("ftyp-driver", "const w = new Worker('./ftyp.js'); w.onmessage = (ev) => print('FTYP:' + ev.data);".into()).unwrap();
    let s = pump_for(&mut e, "FTYP:").await;
    save("worker", &s);
    serde_json::from_str(&s).expect("the worker probe's JSON parses")
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

const REQUEST_ROWS: &[&str] = &[
    "method", "url", "headers", "destination", "referrer", "referrerPolicy", "mode", "credentials",
    "cache", "redirect", "integrity", "keepalive", "signal", "duplex", "isHistoryNavigation",
    "bodyUsed", "arrayBuffer", "blob", "clone", "formData", "json", "text", "targetAddressSpace",
    "isReloadNavigation", "body", "bytes",
];
const RESPONSE_ROWS: &[&str] = &[
    "type", "url", "redirected", "status", "ok", "statusText", "headers", "body", "bodyUsed",
    "arrayBuffer", "blob", "clone", "formData", "json", "text", "bytes",
];

#[tokio::test]
async fn the_two_interfaces_match_the_capture_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let q = &d["iface"]["Request"];
        assert!(q.get("absent").is_none(), "{realm}: Request is not exposed");
        assert_eq!(q["length"].as_u64(), Some(1), "{realm}: Request.length");
        assert_eq!(q["nat"], serde_json::json!(true), "{realm}: Request must be [native code]");
        assert_eq!(q["ctorProtoName"], serde_json::json!(""), "{realm}: Request inherits nothing");
        assert_eq!(strs(&q["ctorOrder"]), vec!["length", "name", "prototype"], "{realm}: Request has no statics");
        assert_eq!(q["tag"], serde_json::json!({"v": "Request", "e": false, "c": true, "w": false}), "{realm}");
        assert_eq!(
            q["noArgs"], serde_json::json!("TypeError: Failed to construct 'Request': 1 argument required, but only 0 present."),
            "{realm}: `new Request()` -- Chrome's message"
        );
        assert!(
            q["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'Request': Please use the 'new' operator"),
            "{realm}: `Request('/x')` without new"
        );
        // The capture's order. `textStream` is the bundle's extra row (Brave 151 has it, Chrome 150
        // does not); everything before it is Chrome's list exactly.
        let order: Vec<String> = strs(&q["protoOrder"]).into_iter().filter(|n| n != "constructor" && n != "textStream").collect();
        assert_eq!(order, REQUEST_ROWS, "{realm}: Request.prototype's enumeration order");

        let r = &d["iface"]["Response"];
        assert!(r.get("absent").is_none(), "{realm}: Response is not exposed");
        assert_eq!(r["length"].as_u64(), Some(0), "{realm}: Response.length -- both arguments are optional");
        assert_eq!(r["nat"], serde_json::json!(true), "{realm}: Response must be [native code]");
        assert_eq!(r["ctorProtoName"], serde_json::json!(""), "{realm}: Response inherits nothing");
        assert_eq!(strs(&r["ctorOrder"]), vec!["length", "name", "prototype", "error", "json", "redirect"], "{realm}: the three statics");
        assert_eq!(r["tag"], serde_json::json!({"v": "Response", "e": false, "c": true, "w": false}), "{realm}");
        assert_eq!(r["noArgs"], serde_json::json!("OK:[object Response]"), "{realm}: `new Response()` is a real 200 response");
        assert!(
            r["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'Response': Please use the 'new' operator"),
            "{realm}: `Response('/x')` without new"
        );
        let order: Vec<String> = strs(&r["protoOrder"]).into_iter().filter(|n| n != "constructor" && n != "textStream").collect();
        assert_eq!(order, RESPONSE_ROWS, "{realm}: Response.prototype's enumeration order");
    }
}

/// FAILURE MODE 2, in Chrome's TWO shapes: an accessor throws `Illegal invocation`; a
/// PROMISE-returning method hands back a promise that rejects with "Failed to execute '<m>' on
/// '<Iface>': Illegal invocation". `clone` returns a value, so it throws. All three measured.
///
/// `Request`'s five body-reading methods used to be D2 stubs, whose receiver check THREW where Chrome
/// rejects; this test carried them as a recorded exception "until the rows get bodies". A later change
/// gave them bodies (the Body mixin, `modules/fetch/body.rs`, shared with `Response`), so they answer
/// Chrome's shape -- the rejection -- and the exception is gone.

#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        for iface in ["Request", "Response"] {
            let f = d["foreign"][iface].as_object().unwrap_or_else(|| panic!("{realm}: no foreign map for {iface}"));
            assert!(f.len() >= 8, "{realm}: {iface} has too few rows to check ({})", f.len());
            for (k, v) in f {
                let v = v.as_str().unwrap_or("");
                let Some(m) = k.strip_prefix("m:") else {
                    assert_eq!(v, "TypeError: Illegal invocation", "{realm}: {iface}.prototype.{k} accepted a foreign receiver");
                    continue;
                };
                if m == "clone" {
                    assert_eq!(
                        v, "SYNC-THROW:TypeError: Illegal invocation",
                        "{realm}: {iface}.prototype.{m} must reject the receiver (synchronously: it is a value-returning row)"
                    );
                } else {
                    assert_eq!(
                        v, format!("TypeError: Failed to execute '{m}' on '{iface}': Illegal invocation"),
                        "{realm}: {iface}.prototype.{m} must REJECT for a foreign receiver"
                    );
                }
            }
        }
    }
}

/// FAILURE MODE 1 and 3. Neither instance owns anything, and every row reads back what the
/// constructor put in the slot.
#[tokio::test]
async fn an_instance_owns_nothing_and_every_row_reads_back() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let v = &d["vals"];
        assert_eq!(v["respOwn"], serde_json::json!([]), "{realm}: a real Response has NO own properties (Chrome: [])");
        assert_eq!(v["reqOwn"], serde_json::json!([]), "{realm}: a real Request has NO own properties (Chrome: [])");
        assert_eq!(v["respTag"], serde_json::json!("[object Response]"), "{realm}");
        assert_eq!(v["reqTag"], serde_json::json!("[object Request]"), "{realm}");
        assert_eq!(v["respIs"], serde_json::json!(true), "{realm}: instanceof Response");
        assert_eq!(v["reqIs"], serde_json::json!(true), "{realm}: instanceof Request");
        // `type` is "default" for a CONSTRUCTED response; "basic" is what a same-origin fetch produces.
        assert_eq!(
            v["resp"], serde_json::json!([201, true, "Created", "default", "", false, false]),
            "{realm}: every Response row off `new Response(\"hello\", {{status:201, statusText:\"Created\"}})`"
        );
        assert_eq!(v["respDefault"], serde_json::json!([200, true, "", "default"]), "{realm}: `new Response()`");
        assert_eq!(
            v["req"],
            serde_json::json!(["http://example.test/x", "POST", "", "about:client", "", "cors", "same-origin", "default", "follow", "", false, false, false, false]),
            "{realm}: every Request row off `new Request(url, {{method:\"POST\"}})`"
        );
        assert_eq!(v["reqHeaders"], serde_json::json!(true), "{realm}: Request.headers is a real Headers, not a Map");
        assert_eq!(v["reqClone"], serde_json::json!([true, "http://example.test/x", "POST"]), "{realm}: clone()");
    }
}

/// `fetch` itself, as a function a page reads in three lines. Chrome's, measured:
/// `{name: "fetch", length: 1, [native code], no own-tell}`, a writable/enumerable/configurable data
/// property, and `fetch()` REJECTS rather than throwing — as does every other input failure, because
/// it is a promise-returning operation.
///
/// The realm names ITSELF in those messages: `Window` in the window, `WorkerGlobalScope` in a worker.
#[tokio::test]
async fn the_fetch_global_is_a_native_function_of_arity_one() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let f = &d["fetchFn"];
        assert_eq!(f["type"], serde_json::json!("function"), "{realm}: fetch is a function");
        assert_eq!(f["name"], serde_json::json!("fetch"), "{realm}: fetch.name");
        assert_eq!(f["length"].as_u64(), Some(1), "{realm}: fetch.length (Chrome: 1; the JS arrow reported 2)");
        assert_eq!(f["nat"], serde_json::json!(true), "{realm}: fetch must be [native code]");
        assert_eq!(f["tells"], serde_json::json!(false), "{realm}: fetch owns no prototype/arguments/caller");
        if realm == "window" {
            // Chrome's WINDOW has `fetch` as an own data property of the global with these flags.
            // In a Chrome WORKER `Object.getOwnPropertyDescriptor(globalThis, "fetch")` is NULL --
            // the member lives on `WorkerGlobalScope.prototype` — and korvus's worker global owns it.
            // That difference is the WORKER GLOBAL's shape, which `tests/worker_surface.rs` measures
            // against the bundle and `worker-finalize.js` owns; it is not this row's, and moving it
            // is not this unit's. Measured over a loopback page that posts this probe into a real
            // dedicated worker.
            assert_eq!(f["desc"]["e"], serde_json::json!(true), "{realm}: the fetch global is enumerable");
            assert_eq!(f["desc"]["c"], serde_json::json!(true), "{realm}: ...and configurable");
            assert_eq!(f["desc"]["w"], serde_json::json!(true), "{realm}: ...and writable");
            assert_eq!(f["desc"]["fl"].as_u64(), Some(1), "{realm}: ...holding a function of length 1");
        }
        assert_eq!(f["newFetch"].as_str().map(|s| s.starts_with("TypeError")), Some(true), "{realm}: `new fetch(...)` is a TypeError");
        // A REJECTION, not a throw, and with the realm's own name in it.
        let want = if realm == "window" { "Window" } else { "WorkerGlobalScope" };
        assert_eq!(
            f["noArgs"], serde_json::json!(format!("TypeError: Failed to execute 'fetch' on '{want}': 1 argument required, but only 0 present.")),
            "{realm}: `fetch()` must REJECT with Chrome's sentence"
        );
    }
}

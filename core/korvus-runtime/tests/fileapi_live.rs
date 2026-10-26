//! `Blob` and `File`'s live gate, in BOTH realms.
//!
//! Both were JS classes in `host.js` with a constructor-only body, keeping their state in OWN
//! properties (`_bytes`, `_type`, `_name`, `_lastModified`, `_webkitRelativePath`) that
//! `Object.getOwnPropertyNames` showed to any page that looked. They are `interface!` tables with
//! native constructors now (`core/korvus-webapi/src/core/fileapi/{blob,file}.rs`), their state is in
//! v8 privates, and `Exposed=(Window,Worker)` builds both realms from the one table.
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/fileapi-probe.js` UNMODIFIED — the same file this test evaluates in the window and
//! inside a real Worker isolate. `KORVUS_FILEAPI_OUT=<dir>` writes `window.json` / `worker.json`.
//!
//! `Blob.prototype.textStream` was the one member that was not Chrome 150's while the persona was
//! Brave 151, which records it. The persona is Chrome 150 since 2026-10-16, so the prototype is Chrome's
//! seven rows.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/fileapi-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_FILEAPI_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the fileapi dump");
    }
}

/// The probe resolves ASYNCHRONOUSLY: three of the rows it measures are promise-returning, and their
/// foreign-receiver verdict is a rejection reason rather than a thrown exception, so it settles them
/// before serialising. Both realms therefore poll `print` output instead of reading an eval result.
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
    e.eval("fileapi_probe", format!("({PROBE}).then((s) => print('FAPI:' + s));")).unwrap();
    let s = pump_for(&mut e, "FAPI:").await;
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("({PROBE}).then((s) => self.postMessage(s));");
    let reg = format!("globalThis.__workerScripts = {{ './fileapi.js': {} }};", serde_json::to_string(&worker_src).unwrap());
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("fileapi-driver", "const w = new Worker('./fileapi.js'); w.onmessage = (ev) => print('FAPI:' + ev.data);".into()).unwrap();
    let s = pump_for(&mut e, "FAPI:").await;
    save("worker", &s);
    serde_json::from_str(&s).expect("the worker probe's JSON parses")
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()
}

/// The bundle's prototype order (Chrome 152's, measured in both realms: `textStream` after `bytes`, which Chrome 150
/// did not have).
const BLOB_ROWS: &[&str] = &["size", "type", "arrayBuffer", "slice", "stream", "text", "bytes", "textStream", "constructor"];
const FILE_ROWS: &[&str] = &["name", "lastModified", "lastModifiedDate", "webkitRelativePath", "constructor"];

#[tokio::test]
async fn the_interface_objects_and_prototypes_match_the_capture_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let b = &d["iface"]["Blob"];
        assert!(b.get("absent").is_none(), "{realm}: Blob is not exposed");
        assert_eq!(b["name"], serde_json::json!("Blob"), "{realm}: Blob.name");
        assert_eq!(b["length"].as_u64(), Some(0), "{realm}: Blob.length -- both arguments are optional");
        assert_eq!(b["nat"], serde_json::json!(true), "{realm}: Blob must be [native code]");
        assert_eq!(strs(&b["protoOrder"]), BLOB_ROWS, "{realm}: Blob.prototype's enumeration order");
        assert_eq!(strs(&b["protoChain"]), vec!["Object"], "{realm}: Blob inherits nothing");
        assert_eq!(b["ctorProtoName"], serde_json::json!(""), "{realm}: Object.getPrototypeOf(Blob) is Function.prototype");
        assert_eq!(b["tag"], serde_json::json!({"v": "Blob", "e": false, "c": true, "w": false}), "{realm}: Blob's toStringTag");

        let f = &d["iface"]["File"];
        assert!(f.get("absent").is_none(), "{realm}: File is not exposed");
        assert_eq!(f["length"].as_u64(), Some(2), "{realm}: File.length -- fileBits and fileName are required");
        assert_eq!(f["nat"], serde_json::json!(true), "{realm}: File must be [native code]");
        assert_eq!(strs(&f["protoOrder"]), FILE_ROWS, "{realm}: File.prototype's enumeration order");
        assert_eq!(strs(&f["protoChain"]), vec!["Blob", "Object"], "{realm}: File.prototype -> Blob.prototype");
        assert_eq!(f["ctorProtoName"], serde_json::json!("Blob"), "{realm}: Object.getPrototypeOf(File) === Blob");
        assert_eq!(f["tag"], serde_json::json!({"v": "File", "e": false, "c": true, "w": false}), "{realm}: File's toStringTag");

        // Every row is a native accessor/method with the WebIDL name, length and no own-tell.
        for (iface, rows) in [("Blob", BLOB_ROWS), ("File", FILE_ROWS)] {
            for m in rows.iter().filter(|m| **m != "constructor") {
                let r = &d["rows"][iface][*m];
                assert_eq!(r["e"], serde_json::json!(true), "{realm}: {iface}.{m} must be enumerable");
                assert_eq!(r["c"], serde_json::json!(true), "{realm}: {iface}.{m} must be configurable");
                assert_eq!(r["nat"], serde_json::json!(true), "{realm}: {iface}.{m} must be [native code]");
                if r["k"] == serde_json::json!("a") {
                    assert_eq!(r["s"], serde_json::json!(false), "{realm}: {iface}.{m} is a readonly attribute");
                    assert_eq!(r["gn"], serde_json::json!(format!("get {m}")), "{realm}: {iface}.{m}'s getter name");
                    assert_eq!(r["gt"], serde_json::json!(false), "{realm}: {iface}.{m}'s getter owns a tell");
                } else {
                    assert_eq!(r["fn"], serde_json::json!(*m), "{realm}: {iface}.{m}'s function name");
                    assert_eq!(r["ft"], serde_json::json!(false), "{realm}: {iface}.{m} owns a tell");
                }
            }
        }
    }
}

/// FAILURE MODE 2, in Chrome's TWO shapes. A value-returning row throws `TypeError: Illegal
/// invocation` on the spot; a PROMISE-returning row (`arrayBuffer`, `text`, `bytes`) never throws
/// synchronously — it hands back a promise that rejects with `TypeError: Failed to execute '<m>' on
/// 'Blob': Illegal invocation`. Both measured; a page's `.catch` is written for the second.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    const PROMISE_ROWS: &[&str] = &["arrayBuffer", "text", "bytes"];
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        for iface in ["Blob", "File"] {
            let f = d["foreign"][iface].as_object().unwrap_or_else(|| panic!("{realm}: no foreign map for {iface}"));
            assert!(!f.is_empty(), "{realm}: {iface} has no rows to check");
            for (k, v) in f {
                let v = v.as_str().unwrap_or("");
                let row = k.split('@').next().unwrap_or(k);
                if PROMISE_ROWS.contains(&row) {
                    assert_eq!(
                        v, format!("TypeError: Failed to execute '{row}' on 'Blob': Illegal invocation"),
                        "{realm}: {iface}.prototype.{k} must REJECT (a promise-returning operation never throws synchronously)"
                    );
                } else {
                    assert_eq!(v, "TypeError: Illegal invocation", "{realm}: {iface}.prototype.{k} accepted a foreign receiver");
                }
            }
        }
    }
}

/// FAILURE MODE 1 and 3. A real `Blob` / `File` OWNS NOTHING, and every row reads back off it.
/// `ownNames` is the assertion the JS classes could not pass: they kept `_bytes` / `_type` /
/// `_name` / `_lastModified` / `_webkitRelativePath` as own properties on every instance.
#[tokio::test]
async fn an_instance_owns_nothing_and_every_row_reads_back() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let v = &d["vals"];
        assert_eq!(v["ownNames"], serde_json::json!([]), "{realm}: a real Blob has NO own properties");
        assert_eq!(v["noArgOwn"], serde_json::json!([]), "{realm}: `new Blob()` owns nothing either");
        assert_eq!(v["fileOwnNames"], serde_json::json!([]), "{realm}: a real File has NO own properties");
        assert_eq!(v["size"].as_u64(), Some(4), "{realm}: Blob.size is the flattened byte length");
        assert_eq!(v["type"], serde_json::json!("text/plain"), "{realm}: Blob.type is the ASCII-lowercased string");
        assert_eq!(v["tag"], serde_json::json!("[object Blob]"), "{realm}: Object.prototype.toString");
        assert_eq!(v["isBlob"], serde_json::json!(true), "{realm}: instanceof Blob");
        assert_eq!(v["empty"], serde_json::json!([0, "", 0]), "{realm}: `new Blob()`");
        // FileAPI's type rule, not mimesniff's: outside U+0020..U+007E -> "", else lowercased whole.
        assert_eq!(
            v["types"],
            serde_json::json!(["text/plain; charset=utf-8", "bogus", "  text/plain  ", "text/plain;charset=\"utf-8\"", ""]),
            "{realm}: the FileAPI type normalisation"
        );
        // strings as UTF-8; Uint8Array; ArrayBuffer; a view's own window; a Blob part; ToString of
        // a non-object part ("1" + "null" + "undefined" = 14 bytes).
        assert_eq!(v["parts"], serde_json::json!([2, 3, 4, 2, 4, 14]), "{realm}: the part flattening");
        assert_eq!(v["endings"], serde_json::json!([5, 6]), "{realm}: endings:\"native\" normalises CRLF/CR to LF");
        assert_eq!(v["slice"], serde_json::json!([2, "x/y", true]), "{realm}: slice(start, end, contentType)");
        assert_eq!(v["sliceNeg"], serde_json::json!([2, ""]), "{realm}: a negative start counts from the end");
        assert_eq!(v["sliceNone"], serde_json::json!([4, ""]), "{realm}: slice() is the whole blob, typeless");
        // WebIDL `sequence<BlobPart>`: a non-iterable, null and a bare string are all TypeErrors at
        // the CONVERSION. The JS class accepted all three -- `new Blob("abc").size` was 3.
        let errs = &v["ctorErrs"];
        for k in ["noIterable", "nullParts", "stringParts"] {
            let got = errs[k].as_str().unwrap_or("");
            assert_eq!(
                got, "TypeError: Failed to construct 'Blob': The provided value cannot be converted to a sequence.",
                "{realm}: new Blob(<{k}>) -- Chrome's message"
            );
        }
        assert!(
            errs["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'Blob': Please use the 'new' operator"),
            "{realm}: Blob([]) without new"
        );

        assert_eq!(v["file"], serde_json::json!(["n.txt", 1700000000000i64, "", 2, "text/plain", true, true]), "{realm}: every File row");
        assert_eq!(v["fileTag"], serde_json::json!("[object File]"), "{realm}: Object.prototype.toString on a File");
        assert_eq!(v["fileLmDate"].as_i64(), Some(1700000000000), "{realm}: lastModifiedDate is a Date at lastModified");
        // Chrome answers `new Date(0)`, NOT null, for lastModified 0 (measured; the JS class read
        // MDN's "null if no modification date is known" and returned null).
        assert_eq!(v["fileLmZero"].as_i64(), Some(0), "{realm}: lastModifiedDate at lastModified 0 is Date(0)");
        assert_eq!(v["fileCoerce"], serde_json::json!(["5", 12]), "{realm}: fileName is ToString'd, lastModified truncates toward zero");
        assert_eq!(v["fileLmNaN"].as_i64(), Some(0), "{realm}: WebIDL `long long`: NaN converts to 0");
        assert_eq!(v["fileDefaultLmIsNow"], serde_json::json!(true), "{realm}: the default lastModified is the construction time");
        let fe = &v["fileErrs"];
        assert_eq!(fe["noArgs"], serde_json::json!("TypeError: Failed to construct 'File': 2 arguments required, but only 0 present."), "{realm}");
        assert_eq!(fe["oneArg"], serde_json::json!("TypeError: Failed to construct 'File': 2 arguments required, but only 1 present."), "{realm}");
        assert!(fe["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'File': Please use the 'new' operator"), "{realm}");
    }
}

//! `ImageData`'s live gate, in BOTH realms.
//!
//! `host.js`'s `globalThis.ImageData = class ImageData {…}` — the last class in that file to write its
//! members through the `KB.__evtSlotSet` bridge — is an `interface!` table with a native constructor
//! (`core/korvus-webapi/src/core/html/image_data.rs`).
//!
//! Every expectation below was READ OFF `google-chrome --headless=new` 150.0.7871.46 running
//! `fixtures/image-data-probe.js` UNMODIFIED. `KORVUS_IMAGE_DATA_OUT=<dir>` writes the dumps.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/image-data-probe.js");

fn save(name: &str, json: &str) {
    if let Ok(d) = std::env::var("KORVUS_IMAGE_DATA_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the ImageData dump");
    }
}

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    let v = e.eval_value("image_data_probe", PROBE.to_string()).expect("the window probe runs");
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
    let reg = format!("globalThis.__workerScripts = {{ './imgd.js': {} }};", serde_json::to_string(&worker_src).unwrap());
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("imgd-driver", "const w = new Worker('./imgd.js'); w.onmessage = (ev) => print('IMGD:' + ev.data);".into()).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("IMGD:")) {
            let s = line["IMGD:".len()..].to_string();
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
async fn the_interface_object_and_prototype_match_the_capture_in_both_realms() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let i = &d["iface"];
        assert!(i.get("absent").is_none(), "{realm}: ImageData is not exposed");
        assert_eq!(i["name"], serde_json::json!("ImageData"), "{realm}: ImageData.name");
        assert_eq!(i["length"].as_u64(), Some(2), "{realm}: ImageData.length");
        assert_eq!(i["nat"], serde_json::json!(true), "{realm}: the interface object must be [native code]");
        assert_eq!(i["ctorProtoName"], serde_json::json!(""), "{realm}: ImageData inherits nothing");
        assert_eq!(strs(&i["ctorOrder"]), vec!["length", "name", "prototype"], "{realm}: no statics");
        assert_eq!(i["tag"], serde_json::json!({"v": "ImageData", "e": false, "c": true, "w": false}), "{realm}: toStringTag");
        let order: Vec<String> = strs(&i["protoOrder"]).into_iter().filter(|n| n != "constructor").collect();
        assert_eq!(order, vec!["width", "height", "colorSpace", "data", "pixelFormat"], "{realm}: the capture's member order");
        for m in ["width", "height", "colorSpace", "data", "pixelFormat"] {
            let r = &d["rows"][m];
            assert_eq!(r["k"], serde_json::json!("a"), "{realm}: {m} is an accessor");
            assert_eq!(r["s"], serde_json::json!(false), "{realm}: {m} is readonly");
            assert_eq!(r["e"], serde_json::json!(true), "{realm}: {m} enumerable");
            assert_eq!(r["c"], serde_json::json!(true), "{realm}: {m} configurable");
            assert_eq!(r["nat"], serde_json::json!(true), "{realm}: {m}'s getter must be [native code]");
            assert_eq!(r["gn"], serde_json::json!(format!("get {m}")), "{realm}: {m}'s getter name");
            assert_eq!(r["gt"], serde_json::json!(false), "{realm}: {m}'s getter own-tell");
        }
    }
}

/// FAILURE MODE 2. All five rows, over `{}` and over the prototype, throw `Illegal invocation` — which
/// is Chrome's answer for all ten, `data` included even though an instance shadows it with an own
/// property.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let f = d["foreign"].as_object().expect("the foreign-receiver map");
        assert_eq!(f.len(), 10, "{realm}: five rows over two receivers");
        for (k, v) in f {
            assert_eq!(v.as_str().unwrap_or(""), "TypeError: Illegal invocation", "{realm}: ImageData.prototype.{k} accepted a foreign receiver");
        }
    }
}

/// FAILURE MODE 1 and 3. `data` is the ONE own property (Chrome: `["data"]`), every row reads back,
/// and each of Chrome's SEVEN construction failures answers with its own name and sentence.
#[tokio::test]
async fn both_overloads_construct_and_every_failure_is_chromes() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        let v = &d["vals"];
        assert_eq!(v["own"], serde_json::json!(["data"]), "{realm}: `data` is the one own property");
        assert_eq!(v["tag"], serde_json::json!("[object ImageData]"), "{realm}: Object.prototype.toString");
        assert_eq!(v["isImageData"], serde_json::json!(true), "{realm}: instanceof ImageData");
        assert_eq!(v["dims"], serde_json::json!([3, 2, "srgb", 24, "Uint8ClampedArray", 0]), "{realm}: `new ImageData(3, 2)`");
        assert_eq!(v["pixelFormat"], serde_json::json!("rgba-unorm8"), "{realm}: pixelFormat");
        assert_eq!(v["fromArray"], serde_json::json!([3, 2, true]), "{realm}: the data overload keeps the SAME array and derives the height");
        assert_eq!(v["fromArrayWithHeight"], serde_json::json!([3, 2]), "{realm}: ...and takes an explicit height");
        assert_eq!(v["settings"], serde_json::json!("display-p3"), "{realm}: ImageDataSettings.colorSpace");
        let e = &d["errs"];
        assert_eq!(e["noArgs"], serde_json::json!("TypeError: Failed to construct 'ImageData': 2 arguments required, but only 0 present."), "{realm}");
        assert_eq!(e["oneArg"], serde_json::json!("TypeError: Failed to construct 'ImageData': 2 arguments required, but only 1 present."), "{realm}");
        assert_eq!(e["zeroWidth"], serde_json::json!("IndexSizeError: Failed to construct 'ImageData': The source width is zero or not a number."), "{realm}");
        assert_eq!(e["zeroHeight"], serde_json::json!("IndexSizeError: Failed to construct 'ImageData': The source height is zero or not a number."), "{realm}");
        assert_eq!(e["negative"], serde_json::json!("IndexSizeError: Failed to construct 'ImageData': The requested image size exceeds the supported range."), "{realm}");
        assert_eq!(e["huge"], serde_json::json!("IndexSizeError: Failed to construct 'ImageData': The requested image size exceeds the supported range."), "{realm}");
        assert_eq!(
            e["badArrayLength"], serde_json::json!("InvalidStateError: Failed to construct 'ImageData': The input data length is not a multiple of 4."),
            "{realm}: a data length that is not a multiple of 4"
        );
        assert_eq!(
            e["mismatchHeight"], serde_json::json!("IndexSizeError: Failed to construct 'ImageData': The input data length is not equal to (4 * width * height)."),
            "{realm}: a data length that does not match width x height"
        );
        assert!(
            e["callNoNew"].as_str().unwrap_or("").starts_with("TypeError: Failed to construct 'ImageData': Please use the 'new' operator"),
            "{realm}: `ImageData(1, 1)` without new"
        );
        // `[EnforceRange] unsigned long` truncates: 2.9 x 1.9 is 2 x 1.
        assert_eq!(e["fractional"], serde_json::json!("OK:2,1"), "{realm}: fractional dimensions truncate");
    }
}

//! LIVE one-table gate for `DOMQuad`, in BOTH realms.
//!
//! `DOMQuad` was the third cppgc value family and the last one with no table. It is one now
//! (`core/geometry/dom_quad.rs`), which took it out of both generated registries AND out of
//! `generated_order::ORDER` -- so the enumeration order is no longer restored by a later pass, it is
//! the op2 class template's own declaration order, verified against the capture here.
//!
//! And it carries the first `Install` row a cppgc table has: `toJSON`. The op2 method returns
//! `serde_json::Value`, which renders as plain nested objects, where Chrome hands back real
//! `DOMPoint` instances (`Object.keys(q.toJSON().p1)` is `[]`, `instanceof DOMPoint` holds).
//! The row's body is the table's in BOTH realms (`installer::verify_native` mounted it over the op2
//! template's until both were deleted) -- the ESM
//! layer that used to do it (`dom/08c-collections.js`) never ran in a worker, so a worker's
//! `q.toJSON().p1` was a plain object with four own keys. Measured on headless Chrome 151:
//! `[["p1","p2","p3","p4"], "[object DOMPoint]", [], true]`.

use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// The capture's own-prototype describe lines for an interface, plus the trailing `constructor`.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| {
        let n = m["name"].as_str().unwrap();
        match m["kind"].as_str().unwrap() {
            "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
            "method" => format!("{n}:data:e=1:c=1:w=1"),
            k => panic!("unexpected kind {k}"),
        }
    }).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

#[tokio::test]
async fn live_dom_quad_is_byte_identical_to_the_capture_and_to_json_is_the_tables() {
    let mut e = page().await;

    let src = format!(r#"(() => {{ {DESCRIBE} return desc(DOMQuad.prototype); }})()"#);
    let got = e.eval_value("dq-desc", src).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        expected_proto("DOMQuad"),
        "DOMQuad.prototype != capture (no ORDER pass restores this any more -- the table is the order)",
    );

    let src = r#"(() => {
        const q = new DOMQuad({x:1,y:2}, {x:3,y:4}, {x:5,y:6}, {x:7,y:8});
        const j = q.toJSON();
        return [
            Object.prototype.toString.call(q),            // [object DOMQuad]
            Object.keys(j).join(","),                      // p1,p2,p3,p4
            Object.prototype.toString.call(j.p1),          // [object DOMPoint] -- the Install row's work
            Object.keys(j.p1).length,                      // 0 -- x/y/z/w live on the prototype
            j.p1 instanceof DOMPoint,                      // true
            j.p3.x,                                        // 5
            DOMQuad.prototype.toJSON.length,               // 0 (WebIDL, from the row)
            DOMQuad.fromRect.length, DOMQuad.fromQuad.length,   // 0, 0
            DOMQuad.prototype.toJSON.toString().includes("[native code]")
        ].join("|");
    })()"#;
    let got = e.eval_value("dq-json", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "[object DOMQuad]|p1,p2,p3,p4|[object DOMPoint]|0|true|5|0|0|0|true",
        "DOMQuad.toJSON's object graph != Chrome's (the table's Install row must beat the op2 method)",
    );
}

/// The same two facts inside a worker isolate. `DOMQuad` is `[Exposed=(Window,Worker)]`, the worker
/// global takes the same native class, and the same table's reconcile runs there -- so `toJSON`'s
/// real body is in this realm for the first time.
#[tokio::test]
async fn worker_dom_quad_is_byte_identical_and_to_json_is_the_tables() {
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const q = new DOMQuad({x:1,y:2}, {x:3,y:4}, {x:5,y:6}, {x:7,y:8});
        const j = q.toJSON();
        self.postMessage([
            desc(DOMQuad.prototype),
            Object.prototype.toString.call(q),
            Object.keys(j).join(","),
            Object.prototype.toString.call(j.p1),
            Object.keys(j.p1).length,
            j.p1 instanceof DOMPoint,
            j.p3.x,
            DOMQuad.prototype.toJSON.length
        ].join("|"));
    "#;
    let got = worker_probe(body).await;
    let proto = "p1:a:s=0,p2:a:s=0,p3:a:s=0,p4:a:s=0,getBounds:d:w=1,toJSON:d:w=1,constructor:d:w=1";
    assert_eq!(
        got,
        format!("{proto}|[object DOMQuad]|p1,p2,p3,p4|[object DOMPoint]|0|true|5|0"),
        "worker DOMQuad != capture / toJSON is not the table's",
    );
}

/// Spawn a worker that posts a pipe-joined summary back; pump until RESULT:. (The same driver
/// `dom_rect_worker_live.rs` uses -- one binary, so it cannot be shared without a module.)
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

//! LIVE one-table gate for `Headers` and `FormData`, in BOTH realms.
//!
//! These were the last two interfaces the deleted `dom/08c-collections.js` finished by hand, and the
//! pair that showed what "one member, two realms, two answers" costs. Measured at `23428558`, inside
//! a worker: `new Headers(); h.append("a","1"); h.get("a")` answered **null**,
//! `[...new Headers([["a","1"]])]` threw "not iterable", `Headers.prototype[Symbol.iterator] ===
//! entries` was **false**, and `constructor` enumerated FIRST. The page realm answered correctly on
//! all four, because an ESM pass that only runs in a page fixed it there. Both realms read the same
//! `interface!` table now.
//!
//! Every expectation below was measured on headless Chrome 151 (`--dump-dom`), not derived:
//!
//! ```text
//! Object.getOwnPropertyNames(Headers.prototype)
//!   -> ["append","delete","get","getSetCookie","has","set","entries","forEach","keys","values","constructor"]
//! Object.getOwnPropertyNames(FormData.prototype)
//!   -> ["append","delete","get","getAll","has","set","entries","forEach","keys","values","constructor"]
//! new Headers().get()       -> TypeError: Failed to execute 'get' on 'Headers': 1 argument required, but only 0 present.
//! new FormData().getAll()   -> TypeError: Failed to execute 'getAll' on 'FormData': 1 argument required, but only 0 present.
//! typeof h.append("a","1")  -> "undefined"   (op2 renders Rust `()` as null)
//! Headers.prototype[Symbol.iterator] === Headers.prototype.entries -> true
//! getOwnPropertyDescriptor(Headers.prototype, Symbol.iterator) -> enumerable false, configurable true, writable true
//! ```

use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// The capture's own-prototype describe lines, plus the trailing non-enumerable `constructor`.
fn expected_proto(iface: &str, artifact: &str) -> Vec<String> {
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact(artifact)).unwrap()).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| {
        let n = m["name"].as_str().unwrap();
        format!("{n}:data:e=1:c=1:w=1:len={}", m["length"].as_u64().unwrap())
    }).collect();
    out.push("constructor:data:e=0:c=1:w=1:len=0".to_string());
    out
}

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}:len=${d.value.length}`;
}).join("\n");"#;

#[tokio::test]
async fn live_headers_and_form_data_are_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in ["Headers", "FormData"] {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = e.eval_value("fv-desc", src).unwrap().as_str().unwrap().to_string();
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            expected_proto(iface, "surface"),
            "{iface}.prototype != capture (members, order, descriptors and WebIDL lengths)",
        );
    }
}

/// The three WebIDL conversions the table's rows now carry, and the iterable surface.
#[tokio::test]
async fn the_rows_carry_the_arity_guard_the_undefined_return_and_the_iterator_alias() {
    let mut e = page().await;
    let src = r#"(() => {
        const T = (f) => { try { return JSON.stringify(f()); } catch (e) { return e.constructor.name + "!" + e.message; } };
        const h = new Headers(); h.append("a", "1");
        return [
            // required > 0 -> the WebIDL arity guard, from the row
            T(() => new Headers().get()),
            T(() => new FormData().getAll()),
            // an Undefined IDL return is undefined, not op2's null
            typeof h.append("b", "2"),
            // and the body still works
            h.get("a"), h.get("b"),
            // the iterable surface: four Install rows + @@iterator === entries
            Headers.prototype[Symbol.iterator] === Headers.prototype.entries,
            FormData.prototype[Symbol.iterator] === FormData.prototype.entries,
            [...new Headers([["x", "9"]])].join(":"),
            (() => { const d = Object.getOwnPropertyDescriptor(Headers.prototype, Symbol.iterator);
                     return [+d.enumerable, +d.configurable, +d.writable].join(""); })(),
            // every one of them is still a native function
            [Headers.prototype.get, Headers.prototype.entries, FormData.prototype.values]
                .every(f => f.toString().includes("[native code]"))
        ].join("|");
    })()"#;
    let got = e.eval_value("fv-behaviour", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        [
            "TypeError!Failed to execute 'get' on 'Headers': 1 argument required, but only 0 present.",
            "TypeError!Failed to execute 'getAll' on 'FormData': 1 argument required, but only 0 present.",
            "undefined", "1", "2", "true", "true", "x,9", "011", "true",
        ].join("|"),
        "the Headers/FormData rows do not carry the WebIDL conversions Chrome 151 was measured to have",
    );
}

/// The same interface, in a worker, where it was a registry SHELL until this lane.
#[tokio::test]
async fn worker_headers_and_form_data_are_the_same_table_and_actually_work() {
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return `${n}:${+d.enumerable}:${d.value.length}`;
        }).join(",");
        const T = (f) => { try { return JSON.stringify(f()); } catch (e) { return e.constructor.name + "!"; } };
        const h = new Headers(); h.append("a", "1");
        const f = new FormData(); f.append("k", "v");
        self.postMessage([
            desc(Headers.prototype),
            desc(FormData.prototype),
            h.get("a"),                                                  // "1", not the shell's null
            f.get("k"),                                                  // "v"
            [...new Headers([["x", "9"]])].join(":"),                    // x,9 (one [k,v] pair), not "not iterable"
            Headers.prototype[Symbol.iterator] === Headers.prototype.entries,
            T(() => new Headers().get()),                                // TypeError!
            typeof h.append("b", "2")                                    // undefined
        ].join("|"));
    "#;
    let got = worker_probe(body).await;
    let hp = "append:1:2,delete:1:1,get:1:1,getSetCookie:1:0,has:1:1,set:1:2,entries:1:0,forEach:1:1,keys:1:0,values:1:0,constructor:0:0";
    let fp = "append:1:2,delete:1:1,get:1:1,getAll:1:1,has:1:1,set:1:2,entries:1:0,forEach:1:1,keys:1:0,values:1:0,constructor:0:0";
    assert_eq!(
        got,
        format!("{hp}|{fp}|1|v|x,9|true|TypeError!|undefined"),
        "the worker's Headers/FormData are not the table's (they were registry shells answering null)",
    );
}

/// Spawn a worker that posts a pipe-joined summary back; pump until RESULT:.
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

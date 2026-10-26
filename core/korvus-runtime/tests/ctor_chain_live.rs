//! Every interface object chains to its parent interface's, in a window, a frame and a worker: WebIDL's "create an
//! interface object" makes an inherited interface object's [[Prototype]] the parent's interface object.
//!
//! The registry pass linked only the PROTOTYPES, so `Object.getPrototypeOf(CSSMediaRule)` was `Function.prototype` --
//! 206 interfaces in a window and in a frame, 33 in a worker (two of them, CSSSkewX/Y, under a parent the worker does
//! not expose) -- where Chrome answers the parent (`CSSConditionRule`). One line of script tells the two apart.
//!
//! `fixtures/ctor-chain.chrome-152.json`: for every capitalised global function with an object `prototype`, the name of
//! its [[Prototype]] and of its prototype's [[Prototype]]'s constructor, in each realm -- Chrome 152.0.7977.82 on this
//! machine, headed on the Wayland desktop, no DevTools, three runs byte-identical. Run on Chrome's persona, so
//! the two global sets are the same 952 / 293 names.
use engine::Engine;
use std::time::Duration;

const PROBE: &str = r#"<!doctype html><html><body><iframe srcdoc="<p>x</p>"></iframe><script>
function sweep(G) {
  var out = {};
  Object.getOwnPropertyNames(G).sort().forEach(function (n) {
    var d = Object.getOwnPropertyDescriptor(G, n); if (!d || typeof d.value !== 'function') return;
    var f = d.value, p; try { p = f.prototype; } catch (e) { return; }
    if (!p || typeof p !== 'object' || !/^[A-Z]/.test(n)) return;
    var cp = Object.getPrototypeOf(f), pp = Object.getPrototypeOf(p);
    var cname = cp === G.Function.prototype ? 'Function.prototype' : (cp && cp.name) || '?';
    var pname = pp === G.Object.prototype ? 'Object.prototype' : (pp && pp.constructor && pp.constructor.name) || '?';
    out[n] = cname + ' | ' + pname;
  });
  return out;
}
onload = function () {
  var R = {page: sweep(window), frame: sweep(frames[0])};
  var w = new Worker(URL.createObjectURL(new Blob(["function sweep(G) {\n  var out = {};\n  Object.getOwnPropertyNames(G).sort().forEach(function (n) {\n    var d = Object.getOwnPropertyDescriptor(G, n); if (!d || typeof d.value !== 'function') return;\n    var f = d.value, p; try { p = f.prototype; } catch (e) { return; }\n    if (!p || typeof p !== 'object' || !/^[A-Z]/.test(n)) return;\n    var cp = Object.getPrototypeOf(f), pp = Object.getPrototypeOf(p);\n    var cname = cp === G.Function.prototype ? 'Function.prototype' : (cp && cp.name) || '?';\n    var pname = pp === G.Object.prototype ? 'Object.prototype' : (pp && pp.constructor && pp.constructor.name) || '?';\n    out[n] = cname + ' | ' + pname;\n  });\n  return out;\n}" + ';postMessage(sweep(self))'], {type: 'text/javascript'})));
  w.onmessage = function (e) { R.worker = e.data; window.RESULT = JSON.stringify(R); };
};
</script></body></html>"#;
const CHROME: &str = include_str!("fixtures/ctor-chain.chrome-152.json");

#[tokio::test]
async fn an_interface_object_chains_to_its_parents_in_every_realm() {
    let chrome = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("chrome bundle");
    let mut e = Engine::new(Box::new(chrome));
    e.run().await.expect("boot");
    e.load_page(PROBE, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("ctor-chain", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the sweep never finished");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    let want: serde_json::Value = serde_json::from_str(CHROME).expect("fixture");
    let mut diffs = Vec::new();
    for realm in ["page", "frame", "worker"] {
        let (g, w) = (got[realm].as_object().expect("realm"), want[realm].as_object().expect("realm"));
        for (name, row) in w {
            if g.get(name) != Some(row) {
                diffs.push(format!("{realm} {name}: korvus {:?} chrome {row}", g.get(name)));
            }
        }
        for name in g.keys().filter(|n| !w.contains_key(*n)) {
            diffs.push(format!("{realm} {name}: korvus only"));
        }
    }
    assert!(diffs.is_empty(), "{} rows differ from Chrome 152:\n{}", diffs.len(), diffs.join("\n"));
}

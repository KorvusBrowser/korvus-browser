//! A message delivered through a MessagePort is the RECEIVER's: its `MessageEvent`, its data and the ports it carries
//! are made in the realm of the port it arrives on, as HTML deserializes it there.
//!
//! korvus minted a transferred port, cloned a port message and built its event in the SENDER's realm, so a sandboxed
//! frame that the page handed a port read `e.ports[0] instanceof MessagePort` false, and every port message between the
//! page and a frame failed `instanceof MessageEvent` / `instanceof Object` on the receiving side. a bot-detection script's ri frame
//! (`about:srcdoc`, `sandbox=allow-scripts`) talks to the page and to its own nested frame only through such ports
//! (a traced Chromium beside the analysis build).
//!
//! Chrome 150.0.7871.46 on this machine, headed, no CDP, three runs alike; the page
//! here is that probe with its report replaced by a flag, at 127.0.0.1:9.

use engine::Engine;
use std::time::Duration;

const PROBE: &str = r#"<!doctype html><html><body><script>
var R = {up: []}, CHILD = "<script>\naddEventListener('message', function (e) {\n  var p = e.ports[0], r = {};\n  try {\n    r.portsLen = e.ports.length; r.portIsMP = p instanceof MessagePort;\n    r.protoEq = Object.getPrototypeOf(p) === MessagePort.prototype;\n    r.pmEq = p.postMessage === MessagePort.prototype.postMessage;\n    r.portsIsArray = e.ports instanceof Array; r.portsFrozen = Object.isFrozen(e.ports);\n    r.evIsME = e instanceof MessageEvent; r.origin = e.origin; r.srcIsParent = e.source === parent;\n  } catch (x) { r.err = String(x); }\n  p.onmessage = function (m) {\n    r.inner = {evIsME: m instanceof MessageEvent, dataIsObj: m.data instanceof Object, arrIsArray: m.data.a instanceof Array,\n               targetIsP: m.target === p, origin: m.origin, portsLen: m.ports.length,\n               sub: m.ports.length ? (m.ports[0] instanceof MessagePort) : 'none'};\n    var back = new MessageChannel();\n    parent.postMessage({stage: 3, o: {x: [1]}}, '*', [back.port2]);\n    p.postMessage({stage: 2, r: r});\n  };\n  p.postMessage({stage: 1});\n});\n<\/script>", cases = [['same', null], ['sbx', 'allow-scripts']], i = 0;
function next() {
  if (i >= cases.length) { (function done() { if (R.up.length < 2) return setTimeout(done, 50); window.DONE = true; })(); return; }
  var c = cases[i++], name = c[0], f = document.createElement('iframe');
  if (c[1]) f.setAttribute('sandbox', c[1]);
  f.srcdoc = CHILD;
  f.onload = function () {
    var ch = new MessageChannel();
    ch.port1.onmessage = function (e) {
      if (e.data.stage === 1) { var sub = new MessageChannel(); ch.port1.postMessage({a: [1]}, [sub.port2]); return; }
      R[name] = {child: e.data.r, parentEvIsME: e instanceof MessageEvent, parentDataIsObj: e.data instanceof Object, parentRIsObj: e.data.r instanceof Object};
      next();
    };
    f.contentWindow.postMessage('init', '*', [ch.port2]);
  };
  document.body.appendChild(f);
}
addEventListener('message', function (e) {
  if (e.data && e.data.stage === 3) R.up.push({evIsME: e instanceof MessageEvent, dataIsObj: e.data instanceof Object, oIsObj: e.data.o instanceof Object,
    xIsArray: e.data.o.x instanceof Array, portIsMP: e.ports[0] instanceof MessagePort, protoEq: Object.getPrototypeOf(e.ports[0]) === MessagePort.prototype,
    origin: e.origin, srcOk: e.source === document.querySelectorAll('iframe')[R.up.length].contentWindow});
});
next();
setTimeout(function () { R.timeout = true; window.DONE = true; }, 4000);
</script></body></html>"#;

const CHROME: &str = r#"{"same": {"child": {"evIsME": true, "inner": {"arrIsArray": true, "dataIsObj": true, "evIsME": true, "origin": "", "portsLen": 1, "sub": true, "targetIsP": true}, "origin": "http://127.0.0.1:9", "pmEq": true, "portIsMP": true, "portsFrozen": true, "portsIsArray": true, "portsLen": 1, "protoEq": true, "srcIsParent": true}, "parentDataIsObj": true, "parentEvIsME": true, "parentRIsObj": true}, "sbx": {"child": {"evIsME": true, "inner": {"arrIsArray": true, "dataIsObj": true, "evIsME": true, "origin": "", "portsLen": 1, "sub": true, "targetIsP": true}, "origin": "http://127.0.0.1:9", "pmEq": true, "portIsMP": true, "portsFrozen": true, "portsIsArray": true, "portsLen": 1, "protoEq": true, "srcIsParent": true}, "parentDataIsObj": true, "parentEvIsME": true, "parentRIsObj": true}, "up": [{"dataIsObj": true, "evIsME": true, "oIsObj": true, "origin": "http://127.0.0.1:9", "portIsMP": true, "protoEq": true, "srcOk": true, "xIsArray": true}, {"dataIsObj": true, "evIsME": true, "oIsObj": true, "origin": "null", "portIsMP": true, "protoEq": true, "srcOk": true, "xIsArray": true}]}"#;

#[tokio::test]
async fn a_port_message_is_made_in_the_receivers_realm() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(PROBE, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    for _ in 0..80 {
        let _ = e.pump(Duration::from_millis(50)).await;
        if e.eval_value("port-realm", "window.DONE === true".to_string()).ok().and_then(|v| v.as_bool()) == Some(true) {
            break;
        }
    }
    let got = e.eval_value("port-realm", "JSON.stringify(R)".to_string()).expect("eval");
    let got: serde_json::Value = serde_json::from_str(got.as_str().unwrap_or("null")).expect("json");
    let want: serde_json::Value = serde_json::from_str(CHROME).expect("fixture");
    assert_eq!(got, want);
}

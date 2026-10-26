//! `requestIdleCallback`'s callback and its `IdleDeadline`, as Chrome 150 calls it.
//!
//! The deadline was an object literal -- `[object Object]`, two own properties, `timeRemaining()` always 0 -- the
//! `timeout` option was ignored, and an idle callback requested from an idle callback ran in the same period. Chrome:
//! an `IdleDeadline` (its getter and method on the prototype), time left in an idle period (up to Blink's 50 ms), and
//! a request made during a period waits for the next, so a 1 ms `timeout` set in a 30 ms callback runs it first, with
//! `didTimeout` and no time left. `CHROME` is `/usr/bin/google-chrome` 150.0.7871.46's answer, headed under Xvfb,
//! twice alike.
use std::time::{Duration, Instant};

const PROBE: &str = "\naddEventListener('load', function () {\n  var out = [];\n  requestIdleCallback(function (d) {\n    var p = Object.getPrototypeOf(d);\n    out.push({\n      tag: Object.prototype.toString.call(d), ctor: d.constructor && d.constructor.name, own: Object.getOwnPropertyNames(d).join(','),\n      protoKeys: Object.getOwnPropertyNames(p).join(','), didTimeout: d.didTimeout, trType: typeof d.timeRemaining,\n      tr0: d.timeRemaining() > 0, trMax: d.timeRemaining() <= 50, isIdleDeadline: typeof IdleDeadline === 'function' && d instanceof IdleDeadline,\n      args: arguments.length\n    });\n    requestIdleCallback(function (d2) { out.push({didTimeout: d2.didTimeout, tr: d2.timeRemaining() === 0 ? 'zero' : 'positive'}); window.__ric = JSON.stringify(out); }, {timeout: 1});\n    var t = performance.now(); while (performance.now() - t < 30) {}\n  });\n});\n";
const CHROME: &str = "[{\"tag\":\"[object IdleDeadline]\",\"ctor\":\"IdleDeadline\",\"own\":\"\",\"protoKeys\":\"didTimeout,timeRemaining,constructor\",\"didTimeout\":false,\"trType\":\"function\",\"tr0\":true,\"trMax\":true,\"isIdleDeadline\":true,\"args\":1},{\"didTimeout\":true,\"tr\":\"zero\"}]";

#[tokio::test]
async fn an_idle_callback_gets_an_idle_deadline_as_in_chrome_150() {
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&format!("<!doctype html><title>ric</title><script>{PROBE}</script>"), "http://127.0.0.1:8125/ric.html", &[]).await.expect("load_page");
    let start = Instant::now();
    let got = loop {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        let v = e.eval_value("r", "window.__ric || ''".to_string()).expect("eval");
        if let Some(l) = v.as_str().filter(|s| !s.is_empty()) {
            break l.to_string();
        }
        assert!(start.elapsed() < Duration::from_secs(10), "the probe never answered");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let (g, w): (serde_json::Value, serde_json::Value) = (serde_json::from_str(&got).expect("korvus's answer"), serde_json::from_str(CHROME).unwrap());
    assert_eq!(g, w, "korvus vs Chrome 150");
}

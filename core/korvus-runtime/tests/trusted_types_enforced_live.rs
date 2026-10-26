//! Trusted Types ENFORCED at the sinks, against stock Chrome 150's own answers.
//!
//! a bot-challenge frame is delivered under `trusted-types nAZtB7 default; require-trusted-types-for
//! 'script'`, and two of its verdict's values come from how documents NESTED in it enforce that:
//!
//! * an `about:blank` frame it makes inherits the policy, so the frame's `div.innerHTML = '<b>t</b>'`,
//!   `eval('0')` and `script.src = 'data:,0'` each go through the frame's default policy -- Chrome's bitmask of
//!   the three is 7, korvus's was 2 (only `eval` asked a policy: no DOM sink did, and nothing was inherited);
//! * a `srcdoc` frame with its own `<meta>` policy (`script-src 'nonce-…'`, no `'unsafe-eval'`) refuses
//!   `frame.contentWindow.eval('this')` with an EvalError after asking its default policy -- korvus read no
//!   frame's metas and asked the running realm's policies, so the eval ran.
//!
//! Spliced into a live Chrome session, korvus's verdict payload with only those two values made Chrome's passed
//! 3/3; korvus's own failed 4/4.
//!
//! Every row of `fixtures/trusted-types-enforced/chrome-150.json` was recorded from `/usr/bin/google-chrome`
//! 150.0.7871.46 on this machine by `record.py` (`--headless=new --dump-dom`, a fresh profile per page, three
//! runs that must agree), with the probe beside it; each pair names its delivery -- a `Content-Security-Policy`
//! or `-Report-Only` header, or a `<head>` meta -- and the probe's case, and is replayed here as delivered.

use std::time::Duration;

const PROBE: &str = include_str!("fixtures/trusted-types-enforced/probe.js");
const CHROME: &str = include_str!("fixtures/trusted-types-enforced/chrome-150.json");

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// The probe's rows for one pair: the page delivered as Chrome's was, the probe run after `load`.
async fn rows(pair: &serde_json::Value) -> serde_json::Value {
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let meta = pair["meta"].as_str().map(|m| format!(r#"<meta http-equiv="Content-Security-Policy" content="{}">"#, escape_attr(m))).unwrap_or_default();
    let doc = format!(r#"<!doctype html><html><head>{meta}<title>t</title></head><body>x<pre id="out"></pre></body></html>"#);
    let mut headers = vec![("content-type".to_string(), "text/html; charset=utf-8".to_string())];
    if let Some(h) = pair["header"].as_array() {
        headers.push((h[0].as_str().unwrap().to_ascii_lowercase(), h[1].as_str().unwrap().to_string()));
    }
    e.load_page_delivered(&doc, "http://127.0.0.1:8123/page.html", &[], &headers).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let case = pair["case"].as_str().unwrap();
    let js = format!("globalThis.__case = {}; Promise.resolve({PROBE}).then(r => JSON.stringify(r))", serde_json::to_string(case).unwrap());
    let got = e.eval_value_settled("tt", js, Duration::from_secs(10)).await.expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("null")).expect("the probe's rows")
}

#[tokio::test]
async fn every_trusted_types_sink_answers_as_chrome_150s_does() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).expect("the recorded Chrome answers");
    let pairs = chrome["pairs"].as_object().expect("pairs");
    let mut bad = Vec::new();
    for (key, pair) in pairs {
        let want = &chrome["answers"][key];
        let got = rows(pair).await;
        let (Some(g), Some(w)) = (got.as_array(), want.as_array()) else {
            bad.push(format!("{key}: korvus {got}"));
            continue;
        };
        if g.len() != w.len() {
            bad.push(format!("{key}: {} rows, Chrome {}", g.len(), w.len()));
        }
        for (a, b) in g.iter().zip(w) {
            if a != b {
                bad.push(format!("{key}:\n    korvus {a}\n    Chrome {b}"));
            }
        }
    }
    assert!(bad.is_empty(), "{} answers differ from Chrome 150:\n{}", bad.len(), bad.join("\n"));
}

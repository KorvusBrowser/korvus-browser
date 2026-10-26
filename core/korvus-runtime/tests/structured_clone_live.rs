//! `structuredClone` of every kind of value, against stock Chrome 150's own answers.
//!
//! a bot-challenge frame clones a BigInt OBJECT (`structuredClone(Object(611n))`) and concatenates the
//! clone into its payload: Chrome's clone is a BigInt wrapper again (`"TNMKX5" + c` is `"TNMKX5611"`), korvus's
//! was an empty plain object (`"TNMKX5[object Object]"`) -- the primitive wrappers (Boolean, Number, BigInt,
//! String) were walked as ordinary objects, and a Symbol wrapper cloned instead of refusing. They now go through
//! V8's own serializer, as Blink's do.
//!
//! `fixtures/structured-clone/chrome-152.json` was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this
//! machine by `record.py` beside it (`--headless=new --dump-dom`, a fresh profile per run, three runs that agree).

use std::time::Duration;

#[tokio::test]
async fn structured_clone_answers_as_chrome_150s_does() {
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page("<!doctype html><html><head><title>t</title></head><body></body></html>", "http://127.0.0.1:8123/page.html", &[])
        .await
        .expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let probe = include_str!("fixtures/structured-clone/probe.js");
    let got = e
        .eval_value_settled("sc", format!("Promise.resolve({probe}).then(r => JSON.stringify(r))"), Duration::from_secs(10))
        .await
        .expect("eval");
    let got: serde_json::Value = serde_json::from_str(got.as_str().unwrap_or("null")).expect("the probe's answers");
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/structured-clone/chrome-152.json")).expect("Chrome's answers");
    let (g, w) = (got.as_object().expect("an object"), want.as_object().expect("an object"));
    let mut bad: Vec<String> = w
        .iter()
        .filter(|(k, v)| g.get(*k) != Some(v))
        .map(|(k, v)| format!("{k}:\n    korvus {}\n    Chrome {v}", g.get(k).map_or("(none)".to_string(), |x| x.to_string())))
        .collect();
    bad.extend(g.keys().filter(|k| !w.contains_key(*k)).map(|k| format!("{k}: korvus only")));
    assert!(bad.is_empty(), "{} answers differ from Chrome 150:\n{}", bad.len(), bad.join("\n"));
}

//! A WebGL context's state as Chrome 150 reads it back: `getParameter` over the
//! state the page set, in Blink's types, on exactly Blink's set of names; setters that Chrome refuses
//! raising its error and changing nothing; bindings answering the objects the page bound; `getError`
//! as Blink's de-duplicated queue; `drawingBufferColorSpace` / `unpackColorSpace`;
//! `getInternalformatParameter`'s validation. a managed bot challenge reads 140 `getParameter`
//! answers in its second stage after enabling extensions; before this change `getParameter` replayed
//! one recorded fresh context for every read (`enable(BLEND)` read back `false`, a bound buffer
//! `null`), accepted all 65,536 names without an error, and both colour spaces were `undefined`.
//!
//! `fixtures/webgl-state/webgl-state.js` runs in a page and its answers are compared with
//! `webgl-state.chrome-150.json`, google-chrome 150.0.7871.46's own (headed, this machine's GPU).
//! Every probe in it is device-independent -- Blink's and GL's rules, never a GPU limit -- so the one
//! recording is the answer under any persona. Before this change 197 of its 384 answers differed.
use engine::Engine;

const SCRIPT: &str = include_str!("fixtures/webgl-state/webgl-state.js");
const CHROME: &str = include_str!("fixtures/webgl-state/webgl-state.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

#[tokio::test]
async fn webgl_state_reads_back_as_chrome_150_reads_it() {
    let want: Vec<serde_json::Value> = serde_json::from_str(CHROME).expect("the recorded Chrome list");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/webgl-state";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><meta charset=utf-8><body></body>", url, &[], &[]).await.expect("load");
    let got = e.eval_value("r", format!("JSON.stringify({SCRIPT})")).expect("eval");
    let got: Vec<serde_json::Value> = serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list");
    assert_eq!(got.len(), want.len(), "answer count");
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| {
            let cut = |v: &serde_json::Value| v.to_string().chars().take(240).collect::<String>();
            format!("korvus {}\n    Chrome {}", cut(g), cut(w))
        })
        .collect();
    assert!(differ.is_empty(), "{} answer(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

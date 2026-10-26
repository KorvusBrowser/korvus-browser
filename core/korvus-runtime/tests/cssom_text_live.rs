//! A style sheet's `cssText` as Chrome 150 writes it: every value in Blink's
//! spelling and every shorthand whose longhands are all present written back as the shorthand.
//! a managed bot challenge reads the `cssText` of every rule of its frame's sheet; korvus echoed
//! each value as written and each shorthand as its longhands (127 of that sheet's 204 rules apart).
//!
//! `fixtures/cssom-text/cssom-text.css` is put in a `<style>` and each `cssRules[i].cssText` compared with
//! `cssom-text.chrome-150.json`, google-chrome 150.0.7871.46's own reading of the same sheet (headed under
//! Xvfb). Before this change 25 of its 26 rules differed.
use engine::Engine;

const CSS: &str = include_str!("fixtures/cssom-text/cssom-text.css");
const CHROME: &str = include_str!("fixtures/cssom-text/cssom-text.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

#[tokio::test]
async fn every_rule_reads_back_as_chrome_150_writes_it() {
    let want: Vec<String> = serde_json::from_str(CHROME).expect("the recorded Chrome list");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/cssom-text";
    e.set_url(url);
    let html = format!("<!doctype html><meta charset=utf-8><style>{CSS}</style>");
    e.load_page_response(html.as_bytes(), url, &[], &[]).await.expect("load");
    let got = e
        .eval_value("r", "JSON.stringify(Array.from(document.styleSheets[0].cssRules, r => r.cssText))".to_string())
        .expect("eval");
    let got: Vec<String> = serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list");
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .enumerate()
        .filter(|(_, (g, w))| g != w)
        .map(|(i, (g, w))| format!("#{i}\n    korvus {g}\n    Chrome {w}"))
        .collect();
    assert_eq!(got.len(), want.len(), "rule count");
    assert!(differ.is_empty(), "{} rule(s) differ from Chrome 150's cssText:\n  {}", differ.len(), differ.join("\n  "));
}

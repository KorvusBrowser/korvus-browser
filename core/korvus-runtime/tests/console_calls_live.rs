//! What a `console` call does with its arguments -- the conversions a page can watch -- as Chrome 150's
//! (the live a challenge provider challenge logs objects whose `toString`s and getters record being called).
//!
//! `fixtures/console-calls/console-calls.html` is loaded UNMODIFIED here and in `google-chrome --headless=new
//! --dump-dom` 150.0.7871.46 with NO DevTools client (a client adds reads of its own: measured, a session
//! reads an Error's `name` a second time when its conversion threw); `console-calls.chrome-150.txt` is that
//! Chrome's `window.__r`, two runs identical. Each line is one call: what it threw, and which page functions
//! ran (`toString`s, getters, `Symbol.toPrimitive`, a Proxy's traps). A line that differs is a finding about
//! the engine, never a reason to edit the file.
//!
//! Re-record: serve the directory, run `google-chrome --headless=new --dump-dom <url>/console-calls.html`,
//! and keep the JSON list in `<pre id="out">` after its `RES:`.
use engine::Engine;
use std::time::Duration;

const PAGE: &str = include_str!("fixtures/console-calls/console-calls.html");
const CHROME: &str = include_str!("fixtures/console-calls/console-calls.chrome-150.txt");

#[tokio::test]
async fn a_console_call_converts_what_chromes_does_and_nothing_else() {
    let want: Vec<String> = serde_json::from_str(CHROME).expect("the recorded Chrome output is a JSON list");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(PAGE, "http://127.0.0.1:8123/console-calls.html", &[]).await.expect("load_page");
    let _ = tokio::time::timeout(Duration::from_millis(200), e.run()).await;
    e.eval("read", "print('R:' + JSON.stringify(window.__r))".to_string()).unwrap();
    let line = e.take_output().into_iter().rev().find(|l| l.starts_with("R:")).expect("the page's list");
    let got: Vec<String> = serde_json::from_str(&line[2..]).expect("a JSON list");
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(g, w, "call #{i}: korvus vs Chrome 150 (fixtures/console-calls/console-calls.chrome-150.txt)");
    }
    assert_eq!(got.len(), want.len(), "the number of calls the page made");
}

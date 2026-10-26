//! An src-less `<iframe>`'s initial `load`, and a `document.open()` / `write()` / `close()` of its document from that
//! very handler: the handler runs ONCE (HTML "iframe load in progress" sets the document's "mute iframe load" flag).
//!
//! korvus fired the initial about:blank `load` at insertion without marking the frame's load event as running, so the
//! handler's `close()` ended the reopened document with a second `load` -- re-entrantly, the frame's URL by then the
//! page's. a bot-detection script's press & hold captcha builds its challenge frames exactly so (onload: `open("text/html",
//! "replace")`, `write`, `close`, then it records the frame's document), and a press & hold in korvus ended in a
//! `TypeError` PX reported as a clientError (11 per replayed session, 0 in Chrome).
//!
//! Chrome 150.0.7871.46 on this machine, three runs alike: `onload` once, about:blank, the write in
//! place -- in the light tree and in a closed shadow root alike.

use engine::Engine;
use std::time::Duration;

const PROBE: &str = r#"<!doctype html><html><body><div id=c></div><script>
var R = [];
function frame(into) {
  var f = document.createElement('iframe');
  f.setAttribute('style', 'display: none; width: 100%; height: 100px; border: 0');
  var t = false;
  f.onload = function () {
    R.push('onload ' + (t ? 'again' : 'first') + ' ' + (f.contentDocument && f.contentDocument.URL));
    if (t) return; t = true;
    f.contentDocument.open('text/html', 'replace');
    f.contentDocument.write('<div id=x>hold</div>');
    try { f.contentDocument.close(); } catch (e) {}
    R.push('wrote ' + !!f.contentDocument.getElementById('x'));
  };
  into.appendChild(f);
  R.push('appended');
}
frame(document.getElementById('c'));
var host = document.createElement('div'); document.body.appendChild(host);
frame(host.attachShadow({mode: 'closed'}));
</script></body></html>"#;

#[tokio::test]
async fn an_iframe_opened_and_closed_from_its_first_load_handler_loads_once() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(PROBE, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    let _ = e.pump(Duration::from_millis(200)).await;
    let v = e.eval_value("mute-load", "R.slice()".to_string()).expect("eval");
    let got: Vec<String> = v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
    let want = ["onload first about:blank", "wrote true", "appended"];
    let want: Vec<String> = want.iter().chain(want.iter()).map(|s| s.to_string()).collect();
    assert_eq!(got, want);
}

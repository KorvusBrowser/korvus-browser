//! A FRAME'S DOCUMENT IS ITS OWN: `compatMode`, `referrer`, the fullscreen / picture-in-picture flags and
//! `customElementRegistry` of the document a frame shows, against Chrome's own answers.
//!
//! # What was wrong, and who reads it
//!
//! a challenge provider's JSD, an interactive-widget challenge and `a clearance cookie` collectors (and a behavioural sensor) take their surface
//! fingerprint INSIDE A FRESH `about:blank` IFRAME: they append a hidden `<iframe>`, walk its
//! `contentWindow` / `navigator` / `contentDocument`, and file every value. In that frame korvus answered
//! four things no browser does (a surface-bucket probe, 2026-10-05):
//!
//! * `compatMode` read `CSS1Compat`: the getter unwrapped `this` with a helper that knows no Document, so
//!   EVERY document answered the PAGE's mode -- an initial `about:blank` (quirks: it has no doctype) and
//!   a doctype-less `DOMParser` result alike;
//! * `referrer` read `""`: an `about:blank` document's referrer is its creator's URL;
//! * `fullscreenEnabled`, `webkitFullscreenEnabled` and `pictureInPictureEnabled` were constant `false`
//!   on every document, the page's included;
//! * `customElementRegistry` was `null` for every frame's document.
//!
//! # The oracle: stock Chrome 150.0.7871.46 and Brave 151.1.93.138 (identical), 2026-10-05
//!
//! Both launched on one loopback page. `ref` is the page's URL
//! where it says `PAGE`:
//!
//! ```text
//!                               compatMode  referrer            fs/wfs/pip   registry
//!   the page                    CSS1Compat  ""                  T  T  T      own
//!   initial about:blank         BackCompat  PAGE (fragment too) T  T  T      own
//!     referrerpolicy=no-referrer BackCompat PAGE                T  T  T      own
//!     allow="fullscreen 'none'; picture-in-picture 'none'"      F  F  F
//!     nested in a blank frame   BackCompat  "about:blank"       T  T  T      own
//!   open(); write("<!DOCTYPE html>…")  CSS1Compat  PAGE
//!   open(); write("<p>x")       BackCompat  PAGE
//!   src="about:blank"           BackCompat  PAGE
//!   srcdoc="<p>x"               CSS1Compat  the page's ORIGIN   T  T  T      own
//!   cross-origin frame                                          F  F  T      own
//!     allow="fullscreen" / allowfullscreen                      T  T  T
//!   new Document() / createHTMLDocument   CSS1Compat ""          F  F  F      null
//!   DOMParser("<p>x")           BackCompat  ""                  F  F  F      null
//! ```
//!
//! The cross-origin rows need a second origin and a remote frame; they were checked live on the same
//! page (16/16 rows match) and are not repeated here.
use engine::Engine;
use std::time::Duration;

const PAGE_URL: &str = "http://127.0.0.1/page.html?q=1#frag";

async fn run(html: &str, read: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, PAGE_URL, &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    e.pump(Duration::from_millis(300)).await.expect("pump 2");
    e.eval_value("read", read.to_string()).expect("eval")
}

const CASES: &str = r#"<!doctype html><html><body><script>
function kv(w, d) {
  var reg = d.customElementRegistry;
  return [d.compatMode, d.referrer, d.fullscreenEnabled, d.webkitFullscreenEnabled, d.pictureInPictureEnabled,
          reg === null ? 'null' : (w && reg === w.customElements ? 'own' : String(reg))].join('|');
}
window.R = {};
function mk(attrs, cb) {
  var f = document.createElement('iframe');
  for (var k in attrs) f.setAttribute(k, attrs[k]);
  if (cb) f.onload = function () { cb(f); };
  document.body.appendChild(f);
  return f;
}
R.top = kv(window, document);
var b = mk({}); R.blank_initial = kv(b.contentWindow, b.contentDocument);
var bn = mk({referrerpolicy: 'no-referrer'}); R.blank_noref = kv(bn.contentWindow, bn.contentDocument);
var bfs = mk({allow: "fullscreen 'none'; picture-in-picture 'none'"}); R.blank_allow_none = kv(bfs.contentWindow, bfs.contentDocument);
var nest = b.contentDocument.createElement('iframe'); b.contentDocument.body.appendChild(nest);
R.blank_nested = kv(nest.contentWindow, nest.contentDocument);
var w1 = mk({}); var d1 = w1.contentDocument; d1.open(); d1.write('<!DOCTYPE html><p>x'); d1.close(); R.write_doctype = kv(w1.contentWindow, d1);
var w2 = mk({}); var d2 = w2.contentDocument; d2.open(); d2.write('<p>x'); d2.close(); R.write_nodoctype = kv(w2.contentWindow, d2);
mk({src: 'about:blank'}, function (f) { R.blank_src = kv(f.contentWindow, f.contentDocument); });
mk({srcdoc: '<p>x'}, function (f) { R.srcdoc = kv(f.contentWindow, f.contentDocument); });
R.new_document = kv(null, new Document());
R.create_html = kv(null, document.implementation.createHTMLDocument('t'));
R.domparser = kv(null, new DOMParser().parseFromString('<p>x', 'text/html'));
R.domparser_doctype = kv(null, new DOMParser().parseFromString('<!doctype html><p>x', 'text/html'));
</script></body></html>"#;

/// EVERY ROW IS CHROME'S, page-relative URLs and all.
#[tokio::test]
async fn a_frames_document_answers_chromes_compat_mode_referrer_policy_flags_and_registry() {
    let out = run(CASES, "JSON.stringify(window.R)").await;
    let r: serde_json::Value = serde_json::from_str(out.as_str().expect("a string")).expect("json");
    let want = [
        ("top", "CSS1Compat||true|true|true|own".to_string()),
        ("blank_initial", format!("BackCompat|{PAGE_URL}|true|true|true|own")),
        ("blank_noref", format!("BackCompat|{PAGE_URL}|true|true|true|own")),
        ("blank_allow_none", format!("BackCompat|{PAGE_URL}|false|false|false|own")),
        ("blank_nested", "BackCompat|about:blank|true|true|true|own".to_string()),
        ("write_doctype", format!("CSS1Compat|{PAGE_URL}|true|true|true|own")),
        ("write_nodoctype", format!("BackCompat|{PAGE_URL}|true|true|true|own")),
        ("blank_src", format!("BackCompat|{PAGE_URL}|true|true|true|own")),
        ("srcdoc", "CSS1Compat|http://127.0.0.1/|true|true|true|own".to_string()),
        ("new_document", "CSS1Compat||false|false|false|null".to_string()),
        ("create_html", "CSS1Compat||false|false|false|null".to_string()),
        ("domparser", "BackCompat||false|false|false|null".to_string()),
        ("domparser_doctype", "CSS1Compat||false|false|false|null".to_string()),
    ];
    let mut bad = Vec::new();
    for (k, v) in &want {
        let got = r.get(*k).and_then(|x| x.as_str()).unwrap_or("<missing>");
        if got != v {
            bad.push(format!("{k}: got {got:?}, Chrome {v:?}"));
        }
    }
    assert!(bad.is_empty(), "rows that differ from Chrome 150:\n  {}", bad.join("\n  "));
}

/// `compatMode` is a `Document` getter: called on anything else it is an Illegal invocation, as in
/// Chrome (measured: `TypeError: Illegal invocation`). The table's receiver check already threw before
/// the getter changed (the control run passes this one too); it pins that the getter's own `doc_this`
/// never answers the page's mode for a foreign receiver.
#[tokio::test]
async fn compat_mode_on_a_non_document_throws() {
    let out = run(
        "<!doctype html><p>x",
        r#"(() => { const g = Object.getOwnPropertyDescriptor(Document.prototype, 'compatMode').get;
            try { g.call(document.body); return 'no throw'; } catch (e) { return e.constructor.name; } })()"#,
    )
    .await;
    assert_eq!(out, "TypeError");
}

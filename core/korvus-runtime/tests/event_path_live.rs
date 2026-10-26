//! An event's path ends in ITS OWN tree's document and window, not the page's.
//!
//! # What was wrong, and how a page felt it
//!
//! `core/events/event_target.rs::et_path` built `[target, ...ancestors]` and then appended the PAGE's
//! `document` and `window` whatever tree the target was in. DOM's "get the parent" gives a document's
//! parent as its relevant global only when the document HAS a browsing context, and a node whose root
//! is not a document has no parent at all.
//!
//! Nothing showed it while frames fired no events of their own. Then a frame document fired
//! `DOMContentLoaded`, and the frame's DCL then bubbled into the page's `document` listeners. dev.to
//! registers `document.addEventListener('DOMContentLoaded', initAuth)`, and `initAuth` appends a hidden
//! `forem.com/auth_pass` iframe, so every frame load ran it again (parity replay, 2026-09-25):
//!
//! ```text
//!                                   auth_pass navigations   readyState at the deadline
//!   stock Chrome 150 (the bank)     1                       complete
//!   korvus after F1                 1,715 in 30 s           interactive
//! ```
//!
//! # The oracle
//!
//! The frame row is the Chrome recording above: `tools/parity/recordings/dev.to/flow`, recorded by
//! `stock-chrome-150.0.7871.46`, holds ONE `auth_pass` request. The created-document rows are
//! `dom/events/Event-dispatch-bubbles-true.html` (`new Document()`, `createHTMLDocument()`,
//! `document.cloneNode(true)`: the chain is the document's own, with no window), a WPT file Chrome
//! passes. The detached row is the same algorithm with no document at the root.
use engine::Engine;
use std::time::Duration;

async fn run(html: &str, read: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, "http://127.0.0.1/page.html", &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    e.pump(Duration::from_millis(300)).await.expect("pump 2");
    let v = e.eval_value("read", read.to_string()).expect("eval");
    serde_json::from_str(v.as_str().expect("a JSON string")).expect("JSON")
}

/// A FRAME DOCUMENT'S `DOMContentLoaded` STAYS IN THE FRAME: the page's `document` and `window`
/// listeners hear the page's own DCL once, however many frames load after it. The listener is
/// capped at three runs so that the pre-fix engine answers `3` instead of recursing: a script-less
/// `srcdoc` frame fires its DCL inside `appendChild` here.
#[tokio::test]
async fn a_frame_documents_dom_content_loaded_does_not_reach_the_page() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { doc: 0, win: 0 };
        document.addEventListener("DOMContentLoaded", function () {
          if (++C.doc > 3) return;
          var f = document.createElement("iframe");
          f.style.display = "none";
          f.srcdoc = "<p>frame</p>";
          document.body.appendChild(f);
        });
        window.addEventListener("DOMContentLoaded", function () { C.win++; });
        </script></body></html>"#,
        "JSON.stringify({ doc: C.doc, win: C.win, frames: document.querySelectorAll('iframe').length })",
    )
    .await;
    assert_eq!(out, serde_json::json!({ "doc": 1, "win": 1, "frames": 1 }), "the page's DCL listeners run once");
}

/// A DOCUMENT WITHOUT A BROWSING CONTEXT ENDS THE PATH: an event bubbling in a
/// `createHTMLDocument()` document reaches that document and neither the page's document nor the
/// window -- capturing listeners included.
#[tokio::test]
async fn a_created_documents_path_ends_at_that_document() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { created: 0, page: 0, win: 0, winCapture: 0 };
        var d = document.implementation.createHTMLDocument("x");
        var el = d.body.appendChild(d.createElement("div"));
        d.addEventListener("ping", function () { C.created++; });
        document.addEventListener("ping", function () { C.page++; });
        window.addEventListener("ping", function () { C.win++; });
        window.addEventListener("ping", function () { C.winCapture++; }, true);
        el.dispatchEvent(new Event("ping", { bubbles: true }));
        </script></body></html>"#,
        "JSON.stringify(C)",
    )
    .await;
    assert_eq!(out, serde_json::json!({ "created": 1, "page": 0, "win": 0, "winCapture": 0 }));
}

/// A DETACHED SUBTREE ENDS AT ITS ROOT: its root has no parent, so no document and no window.
#[tokio::test]
async fn a_detached_subtrees_path_ends_at_its_root() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { root: 0, page: 0, win: 0 };
        var root = document.createElement("div");
        var leaf = root.appendChild(document.createElement("span"));
        root.addEventListener("ping", function () { C.root++; });
        document.addEventListener("ping", function () { C.page++; });
        window.addEventListener("ping", function () { C.win++; });
        leaf.dispatchEvent(new Event("ping", { bubbles: true }));
        </script></body></html>"#,
        "JSON.stringify(C)",
    )
    .await;
    assert_eq!(out, serde_json::json!({ "root": 1, "page": 0, "win": 0 }));
}

/// THE CONTROL: a connected element's bubbling event still reaches the page's document and window,
/// in that order, and a `load` still stops at the document.
#[tokio::test]
async fn a_connected_elements_path_still_ends_in_the_page_window() {
    let out = run(
        r#"<!doctype html><html><body><div id=t></div><script>
        window.SEQ = [];
        var t = document.getElementById("t");
        document.addEventListener("ping", function () { SEQ.push("doc"); });
        window.addEventListener("ping", function () { SEQ.push("win"); });
        window.addEventListener("load", function (e) { if (e.target !== document) SEQ.push("win-load-from-element"); }, true);
        t.dispatchEvent(new Event("ping", { bubbles: true }));
        t.dispatchEvent(new Event("load"));
        </script></body></html>"#,
        "JSON.stringify(SEQ)",
    )
    .await;
    assert_eq!(out, serde_json::json!(["doc", "win"]));
}

/// A FRAME'S SECOND DOCUMENT GETS A NEW WINDOW, so the first document's listeners do not hear the
/// second document's events. HTML makes a Window per Document (reusing it only when a context leaves
/// its initial `about:blank`), and this engine keeps one window object per frame: YouTube's embed adds a
/// `DOMContentLoaded` listener that calls and deletes `window.ytAtN`; the IFrame API re-navigates each
/// embed, both documents' listeners ran on the second DCL, and un.org threw "window.ytAtN is not a
/// function" once per embed where Chrome's replay reference throws nothing. Each document here adds one
/// window listener: one run per document is 2; the stale listener made it 3.
///
/// The second `srcdoc` is set once the first document LOADED: a `srcdoc` navigates a task after it is set,
/// so one set again in the same task supersedes the first before it commits and
/// only the second document ever runs -- Chrome 150 counts 1 there, and 2 here.
#[tokio::test]
async fn a_frames_second_document_does_not_run_the_first_documents_window_listeners() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { runs: 0, superseded: 0 };
        var doc = "<script>window.addEventListener('DOMContentLoaded', function () { parent.C.runs++; });<\/script><p>x</p>";
        var f = document.createElement("iframe");
        f.srcdoc = doc;
        f.onload = function () { f.onload = null; f.srcdoc = doc + "<p>second</p>"; };
        document.body.appendChild(f);
        var once = "<script>window.addEventListener('DOMContentLoaded', function () { parent.C.superseded++; });<\/script>";
        var g = document.createElement("iframe");
        g.srcdoc = once;
        document.body.appendChild(g);
        g.srcdoc = once + "<p>second</p>";
        </script></body></html>"#,
        "JSON.stringify(C)",
    )
    .await;
    assert_eq!(out, serde_json::json!({ "runs": 2, "superseded": 1 }));
}

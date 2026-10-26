//! `document.readyState` is HTML's "current document readiness", not a constant.
//!
//! Until this landed, `document.readyState` answered the string `"complete"` from the first line of
//! the first inline script, and `readystatechange` was fired once with a value that had never
//! changed. jQuery 1.4.2 runs its `ready` callbacks SYNCHRONOUSLY on that answer, which is how
//! `rakuten.co.jp` threw `Cannot read properties of undefined (reading 'split')` out of `live`, and
//! the ubiquitous `if (document.readyState === "loading") … else init()` idiom took the wrong branch
//! on every page.
//!
//! THE ORACLE. Every expectation here was measured against headless Chrome 150 over a loopback
//! server on 2026-09-11 (`file://` is refused by the fetcher), with a page recording
//! `document.readyState` at each lifecycle moment:
//!
//! ```text
//! head-inline:loading           body-inline:loading           ext-body:loading
//! readystatechange:interactive  defer:interactive             module:interactive
//! DOMContentLoaded:interactive  readystatechange:complete     load:complete
//! setTimeout-after-load:complete
//! ```
//!
//! KNOWN REMAINING DIVERGENCE, not asserted here and not this unit's: a classic `<script defer>`
//! still runs in document order with the other classic scripts, so it reads `loading` where Chrome
//! reads `interactive`. That is script SCHEDULING, not readiness — the readiness is right on both
//! sides of it.
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The recorder every page below installs first: a global log plus listeners for the three events
/// whose readiness Chrome pins above.
const RECORDER: &str = r#"
  globalThis.__log = [];
  function L(s) { globalThis.__log.push(s); }
  L("head-inline:" + document.readyState);
  document.addEventListener("readystatechange", function () { L("readystatechange:" + document.readyState); });
  document.addEventListener("DOMContentLoaded", function () { L("DOMContentLoaded:" + document.readyState); });
  window.addEventListener("load", function () { L("load:" + document.readyState); });
"#;

async fn log_of(e: &mut Engine) -> Vec<String> {
    e.run().await.expect("run");
    let v = e.eval_value("chk-log", "globalThis.__log".to_string()).expect("eval");
    v.as_array()
        .expect("__log is an array")
        .iter()
        .map(|x| x.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The five moments the unit exists for, in one page, plus the event ordering around them.
#[tokio::test]
async fn readiness_progresses_loading_interactive_complete() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = format!(
        "<!doctype html><html><head><script>{RECORDER}</script></head>\
         <body><script>L(\"body-inline:\" + document.readyState);</script></body></html>"
    );
    e.load_page(&html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    let log = log_of(&mut e).await;

    assert_eq!(
        log,
        vec![
            "head-inline:loading",
            "body-inline:loading",
            "readystatechange:interactive",
            "DOMContentLoaded:interactive",
            "readystatechange:complete",
            "load:complete",
        ],
        "the readiness progression and the readystatechange ordering Chrome 150 measures"
    );
}

/// After the load event -- and on a task after it -- the document stays `complete`.
#[tokio::test]
async fn readiness_is_complete_after_load() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = "<!doctype html><html><body><script>\
        window.addEventListener('load', function () { setTimeout(function () { globalThis.__after = document.readyState; }, 0); });\
        </script></body></html>";
    e.load_page(html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    e.run().await.expect("run");
    let after = e.eval_value("chk-after", "globalThis.__after".to_string()).expect("eval");
    assert_eq!(after.as_str(), Some("complete"), "a setTimeout after load reads complete");
    let now = e.eval_value("chk-now", "document.readyState".to_string()).expect("eval");
    assert_eq!(now.as_str(), Some("complete"), "and the state stays complete afterwards");
}

/// The idiom the constant broke in the wrong direction: during the parse a page must take the
/// `addEventListener` branch, not the `else init()` branch.
#[tokio::test]
async fn loading_branch_is_taken_during_the_parse() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = "<!doctype html><html><head><script>\
        globalThis.__branch = 'none'; globalThis.__initAt = 'never';\
        function init() { globalThis.__initAt = document.readyState; }\
        if (document.readyState === 'loading') { globalThis.__branch = 'listener'; document.addEventListener('DOMContentLoaded', init); }\
        else { globalThis.__branch = 'immediate'; init(); }\
        </script></head><body></body></html>";
    e.load_page(html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    e.run().await.expect("run");
    let branch = e.eval_value("chk-branch", "globalThis.__branch".to_string()).expect("eval");
    let at = e.eval_value("chk-at", "globalThis.__initAt".to_string()).expect("eval");
    assert_eq!(branch.as_str(), Some("listener"), "an inline script during the parse must see `loading`");
    assert_eq!(at.as_str(), Some("interactive"), "and its DOMContentLoaded handler must see `interactive`");
}

/// jQuery 1.4.2's own test -- the one `rakuten.co.jp` fails. Its `ready` runs callbacks
/// synchronously from the first script when `readyState` is already `complete`, so a callback that
/// reads a symbol the SAME script assigns later gets `undefined`. With a real readiness the callback
/// is deferred to DOMContentLoaded and the symbol is there.
#[tokio::test]
async fn a_ready_callback_deferred_like_jquery_sees_later_assignments() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = "<!doctype html><html><head><script>\
        var jqReady = function (fn) { if (document.readyState === 'complete') { fn(); } else { document.addEventListener('DOMContentLoaded', fn); } };\
        var mod = {};\
        jqReady(function () { globalThis.__handler = typeof mod.prepareDomain; });\
        mod.prepareDomain = function () { return 1; };\
        </script></head><body></body></html>";
    e.load_page(html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    e.run().await.expect("run");
    let h = e.eval_value("chk-handler", "globalThis.__handler".to_string()).expect("eval");
    assert_eq!(h.as_str(), Some("function"), "the ready callback must run after the whole script did");
}

/// `onreadystatechange`, the content-attribute-style handler, sees the same two transitions as a
/// listener -- and each exactly once (the lifecycle used to dispatch the event AND call the handler).
#[tokio::test]
async fn onreadystatechange_handler_fires_once_per_transition() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = "<!doctype html><html><head><script>\
        globalThis.__states = [document.readyState];\
        document.onreadystatechange = function () { globalThis.__states.push(document.readyState); };\
        </script></head><body></body></html>";
    e.load_page(html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    e.run().await.expect("run");
    let v = e.eval_value("chk-states", "globalThis.__states.join(',')".to_string()).expect("eval");
    assert_eq!(
        v.as_str(),
        Some("loading,interactive,complete"),
        "WPT html/dom/documents/resource-metadata-management/document-readyState.html asserts exactly this array"
    );
}

/// Non-page documents' readiness, measured on Chrome 152 (2026-10-16): `new Document()`, `createDocument`,
/// `createHTMLDocument` and a `DOMParser` result are `complete` the moment script sees them. (Chrome 150's
/// `createHTMLDocument` was `loading` and stayed there: Chrome 152 builds its tree instead of writing it.)
#[tokio::test]
async fn non_page_documents_readiness_matches_chrome() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let html = "<!doctype html><html><body><script>\
        globalThis.__others = [\
          new Document().readyState,\
          document.implementation.createDocument('http://www.w3.org/1999/xhtml', 'html', null).readyState,\
          document.implementation.createHTMLDocument('t').readyState,\
          new DOMParser().parseFromString('<html></html>', 'text/xml').readyState\
        ].join(',');\
        </script></body></html>";
    e.load_page(html, "http://127.0.0.1/rs.html", &[]).await.expect("load_page");
    e.run().await.expect("run");
    let v = e.eval_value("chk-others", "globalThis.__others".to_string()).expect("eval");
    assert_eq!(v.as_str(), Some("complete,complete,complete,complete"));
}

async fn serve(listener: TcpListener, content_type: &'static str, body: &'static str) {
    loop {
        let (mut s, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    }
}

/// An EXTERNAL classic script -- fetched over transport, not inlined -- is still part of the parse,
/// so it reads `loading` like the inline ones around it. (`ext-body:loading` in the Chrome trace.)
#[tokio::test]
async fn an_external_script_reads_loading() {
    let js = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = js.local_addr().unwrap();
    tokio::spawn(serve(js, "application/javascript", "globalThis.__ext = document.readyState;"));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let base = format!("http://{addr}/");
    let html = "<!doctype html><html><body><script src=\"/ext.js\"></script></body></html>";
    e.load_page(html, &base, &[]).await.expect("load_page");
    e.run().await.expect("run");
    let v = e.eval_value("chk-ext", "globalThis.__ext".to_string()).expect("eval");
    assert_eq!(v.as_str(), Some("loading"), "an external <script src> runs during the parse");
}

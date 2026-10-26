//! Inside an iframe, the window's global functions answer the frame's own calls, refuse a
//! foreign receiver, and a frame's `fetch` is the FRAME document's request. Live, over loopback.
//!
//! # The defect, measured before the fix (master `6decb505`, release binary)
//!
//! ```text
//! <iframe srcdoc="<script>fetch(parent.location.href)</script>">
//!     korvus  rejects "Failed to execute 'fetch' on 'Window': Illegal invocation"     chrome  a Response
//! ```
//!
//! A frame's scripts reach their window through `with`, so a frame's bare `fetch(u)` is a call ON the
//! frame's window, which is not the realm's global in this engine; `fetch`, `atob`, `btoa` and
//! `createImageBitmap` compared `this` with the global by hand. The sweep below found the other half:
//! members that answered for ANY receiver -- `structuredClone`, `matchMedia`, and the frame's own
//! `setTimeout`, `setInterval`, `queueMicrotask`, `postMessage`, `focus`, `blur`.
//!
//! # The oracle
//!
//! `tests/fixtures/p12g-frame-sweep.chrome-150.json` is what stock `google-chrome` 150.0.7871.46
//! (`--headless=new`, attached over CDP) answered for `tests/fixtures/p12g-frame-sweep.js` run inside
//! an `<iframe srcdoc>` and inside an `<iframe src>` over 127.0.0.1 on 2026-09-18 -- the two answered
//! identically. Every function is called six ways from the frame's script:
//!
//! ```text
//! direct            fetch(u)                     (resolved through the frame's scope)
//! bare              fetch.call(undefined, u)
//! self              fetch.call(window, u)        (the frame's window)
//! foreign           fetch.call({}, u)
//! parent            fetch.call(parent, u)
//! pageFnFrameRecv   parent.fetch.call(window, u)
//! ```
//!
//! # Not asserted, and why (the sweep's rows where this engine is still not Chrome)
//!
//! None is a receiver question; each answers the same for every receiver Chrome accepts.
//!
//! ```text
//! prompt                  chrome ""        korvus null     the oracle's CDP driver ACCEPTED the dialog;
//!                                                          this engine has no UI and dismisses
//! open                    chrome null      korvus Window   Chrome blocks a popup without user activation
//! scroll/scrollTo/scrollBy chrome Promise  korvus undefined Chrome 150 returns a promise, and rejects
//!                                                          a foreign receiver rather than throwing
//! ```
use engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SWEEP_JS: &str = include_str!("fixtures/p12g-frame-sweep.js");
const CHROME_SWEEP: &str = include_str!("fixtures/p12g-frame-sweep.chrome-150.json");
const NOT_ASSERTED: [&str; 5] = ["prompt", "open", "scroll", "scrollTo", "scrollBy"];

type Routes = Arc<HashMap<String, (&'static str, String)>>;
/// One line per request: `METHOD host path | referer | origin | sec-fetch-site`.
type Log = Arc<Mutex<Vec<String>>>;

async fn serve(listener: TcpListener, routes: Routes, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let routes = routes.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 16384];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let mut words = req.split_whitespace();
            let method = words.next().unwrap_or("").to_string();
            let target = words.next().unwrap_or("/").to_string();
            let header = |name: &str| {
                req.lines()
                    .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_else(|| "-".to_string())
            };
            let path = target.split('?').next().unwrap_or("/").to_string();
            let host = s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            if path.ends_with("/echo") {
                log.lock().unwrap().push(format!(
                    "{method} {host} {target} | {} | {} | {}",
                    header("referer"),
                    header("origin"),
                    header("sec-fetch-site")
                ));
            }
            let (ctype, body) = match routes.get(&path) {
                Some((ct, b)) => (*ct, b.clone()),
                None => ("text/plain", "ok".to_string()),
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

/// Serve `routes` on 127.0.0.1 (the page) and 127.0.0.2 (another site); `build` gets both origins and
/// returns the routes. Load `/page.html`, wait until `expr` is a string, and return it with the log.
async fn run(build: impl Fn(&str, &str) -> HashMap<String, (&'static str, String)>, expr: &str) -> (String, Vec<String>) {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (oa, ob) = (format!("http://{}", a.local_addr().unwrap()), format!("http://{}", b.local_addr().unwrap()));
    let routes: Routes = Arc::new(build(&oa, &ob));
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(a, routes.clone(), log.clone()));
    tokio::spawn(serve(b, routes.clone(), log.clone()));
    let doc = routes["/page.html"].1.clone();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("{oa}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        e.take_output();
        e.eval("p12g", format!("print('P12G:' + (typeof ({expr}) === 'string' ? ({expr}) : ''))")).expect("eval");
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        if let Some(line) = e.take_output().iter().find_map(|l| l.strip_prefix("P12G:").filter(|v| !v.is_empty()).map(str::to_string)) {
            // Let the requests the page started land in the log.
            let until = tokio::time::Instant::now() + Duration::from_millis(400);
            while tokio::time::Instant::now() < until {
                let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let mut l = log.lock().unwrap().clone();
            l.sort();
            return (line, l.into_iter().map(|r| r.replace(&oa, "A").replace(&ob, "B")).collect());
        }
        assert!(start.elapsed() < Duration::from_secs(40), "{expr} never became a string");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn sweep_routes(frame: &str) -> HashMap<String, (&'static str, String)> {
    let mut r = HashMap::new();
    r.insert("/page.html".to_string(), ("text/html", format!("<!doctype html><html><body>{frame}</body></html>")));
    r.insert("/frame.html".to_string(), ("text/html", "<!doctype html><html><body><script src=\"/sweep.js\"></script></body></html>".to_string()));
    r.insert("/sweep.js".to_string(), ("text/javascript", SWEEP_JS.to_string()));
    r
}

fn assert_sweep(which: &str, got: &str) {
    let got: serde_json::Value = serde_json::from_str(got).expect("sweep JSON");
    let chrome: serde_json::Value = serde_json::from_str(CHROME_SWEEP).unwrap();
    let mut diffs = Vec::new();
    for (fname, row) in chrome.as_object().unwrap() {
        if NOT_ASSERTED.contains(&fname.as_str()) {
            continue;
        }
        for (recv, want) in row.as_object().unwrap() {
            let have = &got[fname][recv];
            if have != want {
                diffs.push(format!("{fname} {recv}: korvus {have} chrome {want}"));
            }
        }
    }
    assert!(diffs.is_empty(), "{which}: {} cells differ from Chrome 150:\n{}", diffs.len(), diffs.join("\n"));
}

/// Every window-global function, called from inside an `<iframe srcdoc>`, answers each receiver the
/// way Chrome 150 does.
#[tokio::test]
async fn a_srcdoc_frames_globals_answer_its_own_calls_and_refuse_a_foreign_receiver() {
    let (got, _) = run(|_, _| sweep_routes("<iframe srcdoc=\"<script src='/sweep.js'></script>\"></iframe>"), "window.RESULT").await;
    assert_sweep("srcdoc", &got);
}

/// The same sweep inside an `<iframe src>` -- a frame with its own URL answers identically in Chrome.
#[tokio::test]
async fn a_src_frames_globals_answer_its_own_calls_and_refuse_a_foreign_receiver() {
    let (got, _) = run(|_, _| sweep_routes("<iframe src=\"/frame.html\"></iframe>"), "window.RESULT").await;
    assert_sweep("src", &got);
}

/// The receivers the sweep does not spell: `null`, a bare `Window.prototype` object, and a frame's own
/// members called on a foreign receiver with no arguments -- the receiver is checked before the count.
#[tokio::test]
async fn null_is_the_global_and_a_prototype_object_is_not_a_window() {
    let page = r#"<!doctype html><html><body><iframe srcdoc="<p>x</p>"></iframe><script>
function t(f) { try { var v = f(); if (v && typeof v.then === "function") return v.then(function () { return "res"; }, function (e) { return "rej:" + e.message; }); return "ok:" + Object.prototype.toString.call(v); } catch (e) { return "throw:" + e.message; } }
onload = function () {
  var F = frames[0];
  var c = {
    structuredClone0: t(function () { return structuredClone.call({}); }),
    structuredCloneNull: t(function () { return structuredClone.call(null, 1); }),
    matchMedia0: t(function () { return matchMedia.call({}); }),
    matchMediaNull: t(function () { return matchMedia.call(null, "all"); }),
    frameSetTimeout0: t(function () { return F.setTimeout.call({}); }),
    frameQueueMicrotask0: t(function () { return F.queueMicrotask.call({}); }),
    framePostMessage0: t(function () { return F.postMessage.call({}); }),
    frameFocusProto: t(function () { return F.focus.call(Object.create(Window.prototype)); }),
    frameSetTimeoutProto: t(function () { return F.setTimeout.call(Object.create(Window.prototype), function () {}, 0); }),
    fetchProto: t(function () { return fetch.call(Object.create(Window.prototype), "/x"); }),
    atobNull: t(function () { return atob.call(null, "YQ=="); }),
    fetchNull: t(function () { return fetch.call(null, location.href); }),
    frameSetTimeoutWindow: t(function () { return F.setTimeout.call(window, function () {}, 0); })
  };
  var out = {};
  Promise.all(Object.keys(c).map(function (k) { return Promise.resolve(c[k]).then(function (v) { out[k] = v; }); })).then(function () { window.RESULT = JSON.stringify(out); });
};
</script></body></html>"#;
    let (got, _) = run(
        |_, _| {
            let mut r = HashMap::new();
            r.insert("/page.html".to_string(), ("text/html", page.to_string()));
            r
        },
        "window.RESULT",
    )
    .await;
    let got: serde_json::Value = serde_json::from_str(&got).unwrap();
    // Chrome 150.0.7871.46, the same markup over loopback.
    let chrome = serde_json::json!({
        "structuredClone0": "throw:Illegal invocation",
        "structuredCloneNull": "ok:[object Number]",
        "matchMedia0": "throw:Illegal invocation",
        "matchMediaNull": "ok:[object MediaQueryList]",
        "frameSetTimeout0": "throw:Illegal invocation",
        "frameQueueMicrotask0": "throw:Illegal invocation",
        "framePostMessage0": "throw:Illegal invocation",
        "frameFocusProto": "throw:Illegal invocation",
        "frameSetTimeoutProto": "throw:Illegal invocation",
        "fetchProto": "rej:Failed to execute 'fetch' on 'Window': Illegal invocation",
        "atobNull": "ok:[object String]",
        "fetchNull": "res",
        "frameSetTimeoutWindow": "ok:[object Number]"
    });
    assert_eq!(got, chrome);
}

/// A frame's `fetch` is the frame document's request: resolved against the frame's URL, with the frame
/// document as `Referer`, `Origin` and `Sec-Fetch-Site`'s initiator -- and an `about:srcdoc` frame's
/// client is its creator, the page. Chrome 150.0.7871.46, row for row, over the same two loopback
/// origins (a cross-origin `Referer` is trimmed to the origin by the default policy).
#[tokio::test]
async fn a_frames_fetch_is_the_frame_documents_request() {
    let (_, log) = run(
        |a, b| {
            let mut r = HashMap::new();
            r.insert(
                "/page.html".to_string(),
                (
                    "text/html",
                    format!(
                        "<!doctype html><html><body>\
                         <iframe src=\"/sub/f.html?same\"></iframe>\
                         <iframe src=\"{b}/sub/f.html?cross\"></iframe>\
                         <iframe srcdoc=\"<script>fetch('echo?rel-srcdoc');fetch('{b}/echo?xfetch-srcdoc');</script>\"></iframe>\
                         <script>fetch('echo?rel-page'); setTimeout(function () {{ window.DONE = 'yes'; }}, 600);</script>\
                         </body></html>"
                    ),
                ),
            );
            r.insert(
                "/sub/f.html".to_string(),
                (
                    "text/html",
                    format!(
                        "<!doctype html><html><body><script>\
                         var tag = location.search.slice(1);\
                         fetch('{a}/echo?fetch-' + tag);\
                         fetch('{b}/echo?xfetch-' + tag);\
                         fetch('echo?rel-' + tag);\
                         fetch('echo?post-' + tag, {{method: 'POST', body: 'x'}});\
                         var alias = fetch; alias('echo?alias-' + tag);\
                         </script></body></html>"
                    ),
                ),
            );
            r
        },
        "window.DONE",
    )
    .await;
    let want = [
        "GET 127.0.0.1 /echo?fetch-cross | B/ | B | cross-site",
        "GET 127.0.0.1 /echo?fetch-same | A/sub/f.html?same | - | same-origin",
        "GET 127.0.0.1 /echo?rel-page | A/page.html | - | same-origin",
        "GET 127.0.0.1 /echo?rel-srcdoc | A/page.html | - | same-origin",
        "GET 127.0.0.1 /sub/echo?alias-same | A/sub/f.html?same | - | same-origin",
        "GET 127.0.0.1 /sub/echo?rel-same | A/sub/f.html?same | - | same-origin",
        "GET 127.0.0.2 /echo?xfetch-cross | B/sub/f.html?cross | - | same-origin",
        "GET 127.0.0.2 /echo?xfetch-same | A/ | A | cross-site",
        "GET 127.0.0.2 /echo?xfetch-srcdoc | A/ | A | cross-site",
        "GET 127.0.0.2 /sub/echo?alias-cross | B/sub/f.html?cross | - | same-origin",
        "GET 127.0.0.2 /sub/echo?rel-cross | B/sub/f.html?cross | - | same-origin",
        "POST 127.0.0.1 /sub/echo?post-same | A/sub/f.html?same | A | same-origin",
        "POST 127.0.0.2 /sub/echo?post-cross | B/sub/f.html?cross | B | same-origin",
    ];
    assert_eq!(log, want, "the frames' requests, sorted");
}

/// A FRAME'S MARKUP RESOLVES AGAINST THE FRAME. `script.src`, `a.href` and
/// `node.baseURI` answer the element's NODE DOCUMENT's base URL -- they all answered the page's, and
/// YouTube's embed player, which derives every module URL from its `<script id=base-js>`'s `src`,
/// fetched its modules from the EMBEDDING site (`https://dev.to/s/_/ytembeds/...`, parity replay) and
/// threw. The frame is same-origin under `/sub/dir/`, so the page's base (`/lib.js`) and the frame's
/// (`/sub/dir/lib.js`) differ without a cross-origin barrier. Chrome resolves each against the
/// frame's document URL: HTML's "parse a URL" relative to the element's node document.
#[tokio::test]
async fn a_frames_reflected_urls_resolve_against_the_frames_own_base() {
    let (got, _) = run(
        |_, _| {
            let mut r = HashMap::new();
            r.insert("/page.html".to_string(), ("text/html", "<!doctype html><html><body><iframe src=\"/sub/dir/frame.html\"></iframe></body></html>".to_string()));
            r.insert("/sub/dir/lib.js".to_string(), ("text/javascript", "window.LIB = 1;".to_string()));
            r.insert(
                "/sub/dir/frame.html".to_string(),
                (
                    "text/html",
                    "<!doctype html><html><body><script id=\"base-js\" src=\"lib.js\"></script><a id=\"l\" href=\"x/y\"></a><script>\
                     var made = document.createElement('script'); made.setAttribute('src', 'c.js');\
                     parent.RESULT = JSON.stringify({\
                       src: document.getElementById('base-js').src,\
                       href: document.getElementById('l').href,\
                       baseURI: document.body.baseURI,\
                       created: made.src,\
                       lib: window.LIB\
                     });\
                     </script></body></html>"
                        .to_string(),
                ),
            );
            r
        },
        "window.RESULT",
    )
    .await;
    let got: serde_json::Value = serde_json::from_str(&got).expect("JSON");
    let path = |k: &str| got[k].as_str().map(|u| u.split_once("//").map(|(_, rest)| rest.split_once('/').map(|(_, p)| format!("/{p}")).unwrap_or_default()).unwrap_or_default());
    assert_eq!(path("src").as_deref(), Some("/sub/dir/lib.js"), "{got}");
    assert_eq!(path("href").as_deref(), Some("/sub/dir/x/y"), "{got}");
    assert_eq!(path("baseURI").as_deref(), Some("/sub/dir/frame.html"), "{got}");
    assert_eq!(path("created").as_deref(), Some("/sub/dir/c.js"), "{got}");
    assert_eq!(got["lib"], 1, "the frame's own script loaded from the frame's base: {got}");
}

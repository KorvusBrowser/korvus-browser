//! CDP-observable behaviors, driven end-to-end over the CDP WebSocket.
//!
//! These fixtures exercise behavior a client only sees THROUGH the protocol (script injection on a
//! new document, etc.), so they are checked here rather than in the in-process conformance runner.
//! A tiny HTTP server serves the fixture page; a WebSocket client drives the CDP server against a
//! real `engine::Engine`. testing.js is delivered as an injected "evaluate on new document" script,
//! the same mechanism the fixture's assertions rely on.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
const TESTING_JS: &str = include_str!("../../../core/korvus-runtime/tests/fixtures/conformance/testing.js");
const INJECT_SCRIPT_HTML: &str = include_str!("../../../core/korvus-runtime/tests/fixtures/conformance/inject_script.html");
const PAGE_JS_ERROR_HTML: &str = include_str!("../../../core/korvus-runtime/tests/fixtures/conformance/page_js_error.html");
const SELECTOR_PATH_HTML: &str = include_str!("../../../core/korvus-runtime/tests/fixtures/conformance/selector_path.html");
const DUMP_HTML: &str = include_str!("../../../core/korvus-runtime/tests/fixtures/conformance/dump.html");

// Serializes the whole document and checks the dump is complete and lossless: a full DOM dump
// includes every element regardless of CSS (the display:none <p>secret</p>, <noscript>), keeps void
// elements (<img>), preserves text, and re-parses to the same element count. Objective round-trip
// properties (the fixture ships no expected dump string).
const DUMP_JS: &str = r####"
(() => {
  const oh = document.documentElement.outerHTML;
  const before = document.querySelectorAll('*').length;
  const doc2 = new DOMParser().parseFromString(oh, 'text/html');
  const after = doc2 ? doc2.querySelectorAll('*').length : -1;
  return JSON.stringify({
    before, after,
    hasTitle: /<h1>Title<\/h1>/.test(oh),
    hasImg: /<img\s*\/?>/.test(oh),
    hasHidden: oh.indexOf('secret') >= 0,
    hasNoscript: oh.indexOf('nojs') >= 0,
    textPreserved: oh.indexOf('visible & well') >= 0 || oh.indexOf('visible &amp; well') >= 0,
  });
})()
"####;

// Computes a unique CSS selector for every element, then self-verifies it: the selector must
// resolve back to exactly that element. Returns a JSON string {total, failures}. This is the
// objective, ported-spec-free contract for "selector path" (the fixture ships no expected strings).
const SELECTOR_PATH_JS: &str = r####"
(() => {
  const q1 = (s) => { try { return document.querySelectorAll(s).length; } catch (e) { return -1; } };
  const uniq = (s) => q1(s) === 1;
  const esc = (s) => String(s).replace(/[^a-zA-Z0-9_-]/g, (c) => '\\' + c);
  function path(el) {
    if (el.id && uniq('#' + esc(el.id))) return '#' + esc(el.id);
    const parts = []; let cur = el;
    while (cur && cur.nodeType === 1) {
      if (cur.id && uniq('#' + esc(cur.id))) { parts.unshift('#' + esc(cur.id)); break; }
      let seg = cur.tagName.toLowerCase();
      const parent = cur.parentElement || cur.parentNode;
      if (parent && parent.children) {
        const same = Array.from(parent.children).filter((c) => c.tagName === cur.tagName);
        if (same.length > 1) seg += ':nth-of-type(' + (same.indexOf(cur) + 1) + ')';
      }
      parts.unshift(seg);
      if (uniq(parts.join(' > '))) break;
      if (!parent || parent.nodeType !== 1) break;
      cur = parent;
    }
    return parts.join(' > ');
  }
  const all = Array.from(document.querySelectorAll('*')).filter((el) => ['HTML', 'BODY', 'HEAD'].indexOf(el.tagName) === -1);
  const failures = [];
  for (const el of all) {
    const p = path(el);
    let ok = false;
    try { ok = document.querySelector(p) === el && document.querySelectorAll(p).length === 1; } catch (e) {}
    if (!ok) failures.push(el.tagName + ' -> ' + p);
  }
  return JSON.stringify({ total: all.length, failures });
})()
"####;

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open the browser-level CDP WebSocket on `addr`. The real path carries a per-process UUID that a
/// client discovers from `/json/version` (see `server.rs`'s
/// `cdp_identifiers_are_chrome_shaped_and_correlate`); this server upgrades on the `Upgrade` header
/// rather than the path, so a behaviour fixture connects directly and pins no path.
async fn connect_cdp(addr: std::net::SocketAddr) -> ClientWs {
    tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/browser/")).await.unwrap().0
}

async fn call(ws: &mut ClientWs, id: i64, method: &str, params: Value) -> Value {
    call_full(ws, id, method, params).await.get("result").cloned().unwrap_or(Value::Null)
}

/// The WHOLE reply, `error` included — what a test asserting a refusal has to read.
async fn call_full(ws: &mut ClientWs, id: i64, method: &str, params: Value) -> Value {
    let req = json!({ "id": id, "method": method, "params": params });
    ws.send(Message::Text(req.to_string())).await.unwrap();
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
        }
    }
}

/// Serve one HTTP GET with `body` (text/html), then close.
async fn serve_page(listener: TcpListener, body: &'static str) {
    let (mut s, _) = listener.accept().await.unwrap();
    let mut buf = [0u8; 4096];
    let _ = s.read(&mut buf).await;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    let _ = s.write_all(resp.as_bytes()).await;
    let _ = s.flush().await;
}

/// Page.addScriptToEvaluateOnNewDocument injects testing.js + a couple of globals BEFORE the page's
/// scripts run; the page then asserts it saw them, and Runtime.evaluate reads testing.assertOk().
#[tokio::test]
async fn cdp_inject_script() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    let page_server = serve_page(page, INJECT_SCRIPT_HTML);

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;

        call(&mut ws, 1, "Page.enable", json!({})).await;
        call(&mut ws, 2, "Runtime.enable", json!({})).await;

        // The environment the fixture's inline script depends on, injected as new-document scripts.
        let add = call(&mut ws, 3, "Page.addScriptToEvaluateOnNewDocument", json!({ "source": TESTING_JS })).await;
        assert!(add.get("identifier").is_some(), "addScript returns an identifier: {add}");
        call(&mut ws, 4, "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "window.__injected = true; window.__injectValue = 42;" })).await;

        call(&mut ws, 5, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;

        // The page's assertions ran during navigation; assertOk() is true iff they all passed,
        // which proves the injected globals were visible to the page script.
        let ok = call(&mut ws, 6, "Runtime.evaluate", json!({ "expression": "testing.assertOk()" })).await;
        assert_eq!(ok["result"]["value"], true, "inject_script assertions passed: {ok}");

        ws.send(Message::Close(None)).await.unwrap();
    };

    let (server_res, _, _) = tokio::join!(server, page_server, client);
    server_res.unwrap();
}

/// The page throws twice uncaught (one top-level, one from a timer callback), and the tally is 2.
///
/// W9.22 MOVED WHERE THAT IS READ, and this test is the evidence for both halves of the move.
/// `Page.js_error_count` was an engine-invented method sitting on a channel that is supposed to be
/// Chrome's — real Chrome answers `-32601 "'Page.js_error_count' wasn't found"` (measured), so this
/// server now does too, which the client half asserts. The capability did not go anywhere: it is
/// `Engine::uncaught_error_count`, which is where its real callers
/// (`tools/corpus/run.py` through `korvus fetch --errors`) have always read it. So the navigation is
/// still driven over CDP and the count is still checked — off the engine the server hands back.
#[tokio::test]
async fn cdp_page_js_error() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    let page_server = serve_page(page, PAGE_JS_ERROR_HTML);

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        call(&mut ws, 1, "Page.enable", json!({})).await;
        // navigate() drains the event loop, so the 1ms timer callback has thrown by the time it
        // returns and both uncaught errors are counted.
        call(&mut ws, 2, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;
        // The method this engine used to answer here is now refused exactly as Chrome refuses it.
        let refused = call_full(&mut ws, 3, "Page.js_error_count", json!({})).await;
        assert!(refused.get("result").is_none(), "Page.js_error_count must not answer: {refused}");
        assert_eq!(refused["error"]["code"], -32601, "Chrome's code for it: {refused}");
        assert_eq!(
            refused["error"]["message"], "'Page.js_error_count' wasn't found",
            "Chrome's exact message for it: {refused}"
        );
        ws.send(Message::Close(None)).await.unwrap();
    };

    let (server_res, _, _) = tokio::join!(server, page_server, client);
    // The tally itself, read where it lives. The engine comes back from the connection it served.
    let mut engine = server_res.unwrap();
    assert_eq!(engine.uncaught_error_count(), 2, "two uncaught page errors counted");
}

/// A CDP client computes a unique selector for every element and verifies each round-trips to its
/// element (querySelector(path) === el, uniquely). The page has duplicate ids and repeated
/// tags/forms, so this exercises id / :nth-of-type / descendant-path disambiguation.
#[tokio::test]
async fn cdp_selector_path() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    let page_server = serve_page(page, SELECTOR_PATH_HTML);

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        call(&mut ws, 1, "Page.enable", json!({})).await;
        call(&mut ws, 2, "Runtime.enable", json!({})).await;
        call(&mut ws, 3, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;
        let ev = call(&mut ws, 4, "Runtime.evaluate", json!({ "expression": SELECTOR_PATH_JS })).await;
        let report: Value = serde_json::from_str(ev["result"]["value"].as_str().unwrap_or("{}")).unwrap();
        assert_eq!(report["failures"], json!([]), "every element's selector round-trips: {report}");
        assert!(report["total"].as_i64().unwrap_or(0) >= 15, "all elements covered: {report}");
        ws.send(Message::Close(None)).await.unwrap();
    };

    let (server_res, _, _) = tokio::join!(server, page_server, client);
    server_res.unwrap();
}

/// A CDP client dumps the whole document and checks the serialization is complete and lossless:
/// hidden and <noscript> content present, void <img> kept, text preserved, and re-parsing yields the
/// same element count.
#[tokio::test]
async fn cdp_dump() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    let page_server = serve_page(page, DUMP_HTML);

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        call(&mut ws, 1, "Page.enable", json!({})).await;
        call(&mut ws, 2, "Runtime.enable", json!({})).await;
        call(&mut ws, 3, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;
        let ev = call(&mut ws, 4, "Runtime.evaluate", json!({ "expression": DUMP_JS })).await;
        let r: Value = serde_json::from_str(ev["result"]["value"].as_str().unwrap_or("{}")).unwrap();
        assert_eq!(r["before"], r["after"], "dump re-parses to the same element count: {r}");
        for k in ["hasTitle", "hasImg", "hasHidden", "hasNoscript", "textPreserved"] {
            assert_eq!(r[k], json!(true), "dump {k}: {r}");
        }
        ws.send(Message::Close(None)).await.unwrap();
    };

    let (server_res, _, _) = tokio::join!(server, page_server, client);
    server_res.unwrap();
}

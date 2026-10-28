//! Isolated worlds over CDP, as Chrome 150 makes and numbers them.
//!
//! `Page.createIsolatedWorld` answered context 2 and every evaluation in it ran in the page's context, and a
//! new-document script naming a world ran in the page's too -- so Playwright's utility world never existed (its
//! selector APIs hung on `korvus serve`) and a page's own overrides were the driver's
//! to trip over.
//!
//! `fixtures/isolated-worlds.chrome-150.json` is stock Chrome 150.0.7871.46's answer (headed, Xvfb, a fresh profile,
//! one raw CDP client, three runs identical) to: `Page.enable`, `Runtime.enable`, a
//! new-document script for world `util` and one for the page, `Page.createIsolatedWorld("w2")` on `about:blank`,
//! a navigation to `fixtures/isolated-worlds.html` -- whose script sets `window.pageVar`,
//! `HTMLElement.prototype.pageProto`, `document.body.custom`, an own `document.title` getter and an own
//! `document.querySelector` -- the probe below in every context id announced so far, `createIsolatedWorld("w3")`, a
//! second navigation, the probe again. Asserted: the commands' answers, every context event from `w2`'s on (ids,
//! names, origins, types), and every context's probe -- the page's sees the page's changes, each world sees none of
//! them and the same DOM, and a context a navigation left behind is gone.
//!
//! KNOWN, not graded: the probe's `attr`. `util`'s script sets an attribute on `document.documentElement` when there
//! is one; Chrome runs it at document start, before the parser has made the element, and reads `null`; this engine
//! parses the whole document before any script runs (`parse-ahead`, an owner decision) and reads `"1"`.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PAGE: &str = include_str!("fixtures/isolated-worlds.html");
const CHROME: &str = include_str!("fixtures/isolated-worlds.chrome-150.json");
const UTIL: &str = "window.__iso = (window.__iso || 0) + 1; document.documentElement && document.documentElement.setAttribute('data-iso', String(window.__iso));";
const PROBE: &str = "JSON.stringify({pageVar: typeof window.pageVar, iso: typeof window.__iso === 'number' ? window.__iso : null,
proto: typeof HTMLElement.prototype.pageProto, custom: typeof document.body.custom, title: document.title,
qs: String(document.querySelector('#p') && document.querySelector('#p').id), same: document.body === document.body,
attr: document.documentElement.getAttribute('data-iso'), href: location.pathname, text: document.getElementById('p').textContent,
ctor: document.body.constructor === HTMLBodyElement, win: typeof window.HTMLElement})";

async fn fixture_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}", PAGE.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    port
}

struct Client {
    ws: ClientWs,
    next: i64,
    events: Vec<Value>,
}

impl Client {
    /// One command: its whole answer (`result` or `error`).
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next()).await.expect("an answer within 30 s").unwrap().unwrap();
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    return v;
                }
                if v.get("method").is_some() {
                    self.events.push(v);
                }
            }
        }
    }

    /// Read events until `method` has come `n` times in all, or 15 s.
    async fn wait_for(&mut self, method: &str, n: usize) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && self.events.iter().filter(|e| e["method"] == method).count() < n {
            if let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), self.ws.next()).await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("method").is_some() {
                    self.events.push(v);
                }
            }
        }
    }

    /// The probe in every context id `ids` names: its answer, or the error.
    async fn probe(&mut self, ids: &serde_json::Map<String, Value>) -> Value {
        let mut out = serde_json::Map::new();
        for k in ids.keys() {
            let r = self.call("Runtime.evaluate", json!({ "expression": PROBE, "contextId": k.parse::<i64>().unwrap(), "returnByValue": true })).await;
            let row = match r.get("error") {
                Some(e) => json!({ "error": { "code": e["code"], "message": e["message"] } }),
                None => {
                    let mut v: Value = serde_json::from_str(r["result"]["result"]["value"].as_str().expect("the probe's JSON")).unwrap();
                    v.as_object_mut().unwrap().remove("attr");
                    v
                }
            };
            out.insert(k.clone(), row);
        }
        Value::Object(out)
    }
}

#[tokio::test]
async fn isolated_worlds_are_made_numbered_and_kept_apart_as_chromes() {
    // The request ledger is process-global: a navigating test takes its turn (see `navigation::ONE_AT_A_TIME`).
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    for k in ["answers1", "answers2"] {
        for (_, row) in chrome["answers"][k].as_object_mut().unwrap().iter_mut() {
            if let Some(o) = row.as_object_mut() {
                o.remove("attr");
            }
        }
    }
    let port = fixture_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let chrome_ref = &chrome;
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        let mut commands = Vec::new();
        let answer = |v: &Value| v.get("result").cloned().unwrap_or_else(|| v.get("error").cloned().unwrap_or_default());
        let r = c.call("Page.enable", json!({})).await;
        commands.push(json!(["Page.enable", answer(&r)]));
        let r = c.call("Runtime.enable", json!({})).await;
        commands.push(json!(["Runtime.enable", answer(&r)]));
        let tree = c.call("Page.getFrameTree", json!({})).await;
        let frame = tree["result"]["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
        let r = c.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": UTIL, "worldName": "util" })).await;
        commands.push(json!(["add util", answer(&r)]));
        let r = c.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": "window.__mainran = 1;" })).await;
        commands.push(json!(["add main", answer(&r)]));
        let mark = c.events.len();
        let r = c.call("Page.createIsolatedWorld", json!({ "frameId": frame, "worldName": "w2" })).await;
        commands.push(json!(["create w2 (blank)", answer(&r)]));
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/world") })).await;
        c.wait_for("Page.loadEventFired", 2).await;
        let answers1 = c.probe(chrome_ref["answers"]["answers1"].as_object().unwrap()).await;
        let r = c.call("Page.createIsolatedWorld", json!({ "frameId": frame, "worldName": "w3" })).await;
        commands.push(json!(["create w3 (loaded)", answer(&r)]));
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/world2") })).await;
        c.wait_for("Page.loadEventFired", 3).await;
        let answers2 = c.probe(chrome_ref["answers"]["answers2"].as_object().unwrap()).await;
        c.ws.send(Message::Close(None)).await.unwrap();
        let mut events = Vec::new();
        for e in &c.events[mark..] {
            let p = &e["params"];
            match e["method"].as_str() {
                Some("Runtime.executionContextCreated") => {
                    let x = &p["context"];
                    let origin = x["origin"].as_str().unwrap().replace(&format!(":{port}"), ":PORT");
                    events.push(json!(["CTX", x["id"], x["name"], origin, x["auxData"]["type"], x["auxData"]["isDefault"]]));
                }
                Some("Runtime.executionContextsCleared") => events.push(json!(["CLEARED"])),
                Some("Page.frameNavigated") => {
                    let url = p["frame"]["url"].as_str().unwrap().replace(&format!("http://127.0.0.1:{port}"), "");
                    events.push(json!(["FN", url]));
                }
                _ => {}
            }
        }
        (commands, events, answers1, answers2)
    };
    let (server, (commands, events, answers1, answers2)) = tokio::join!(server, client);
    server.unwrap();
    assert_eq!(Value::Array(commands), chrome["commands"], "the commands' answers");
    assert_eq!(Value::Array(events), chrome["events"], "the context events from w2's on");
    assert_eq!(answers1, chrome["answers"]["answers1"], "every context's probe on the first document");
    assert_eq!(answers2, chrome["answers"]["answers2"], "every context's probe on the second");
}

/// An observer a WORLD makes is told while the page's own frame is pending, and so is the page's.
///
/// The observers' registry is the page's, shared by its worlds, and it was dropped whenever the running realm's
/// `document` was not the one it had noted. A world's `document` is its own wrapper of the same document, so a world's
/// `observe()` looked like a navigation: it forgot the page's observers and its pending frame, and that frame, when it
/// ran, forgot the world's. Puppeteer's `locator.click()` waits on an `IntersectionObserver` in its utility world
/// while it waits on `requestAnimationFrame` in the page's: it timed out ("The element did not become interactive",
/// chrome-devtools-mcp's click and fill). Chrome 152 tells all three observers here.
#[tokio::test]
async fn an_observer_a_world_makes_is_told_while_the_pages_frame_is_pending() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let port = fixture_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/world") })).await;
        c.wait_for("Page.loadEventFired", 1).await;
        let tree = c.call("Page.getFrameTree", json!({})).await;
        let frame = tree["result"]["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
        let world = c.call("Page.createIsolatedWorld", json!({ "frameId": frame, "worldName": "w" })).await;
        let world = world["result"]["executionContextId"].as_i64().expect("the world's context");
        // the page: an observer of its own, and animation frames for 300 ms, so its frame is pending throughout
        let page = "window.__told = 'not told'; new IntersectionObserver(() => { window.__told = 'told'; }).observe(document.getElementById('p'));
            const t = performance.now(); const tick = () => { if (performance.now() - t < 300) requestAnimationFrame(tick); }; requestAnimationFrame(tick); 1";
        c.call("Runtime.evaluate", json!({ "expression": page })).await;
        let observe = "Promise.race([
            Promise.all([
                new Promise((r) => { const o = new IntersectionObserver(() => { o.disconnect(); r('io'); }); o.observe(document.getElementById('p')); }),
                new Promise((r) => { const o = new ResizeObserver(() => { o.disconnect(); r('ro'); }); o.observe(document.getElementById('p')); }),
            ]).then((a) => a.join(' ')),
            new Promise((r) => setTimeout(() => r('not told in 2 s'), 2000)),
        ])";
        let r = c.call("Runtime.evaluate", json!({ "expression": observe, "contextId": world, "awaitPromise": true, "returnByValue": true })).await;
        let in_world = r["result"]["result"]["value"].clone();
        let r = c.call("Runtime.evaluate", json!({ "expression": "window.__told", "returnByValue": true })).await;
        let in_page = r["result"]["result"]["value"].clone();
        c.ws.send(Message::Close(None)).await.unwrap();
        (in_world, in_page)
    };
    let (server, (in_world, in_page)) = tokio::join!(server, client);
    server.unwrap();
    assert_eq!(in_world, json!("io ro"), "the world's observers");
    assert_eq!(in_page, json!("told"), "the page's observer, made before the world's");
}

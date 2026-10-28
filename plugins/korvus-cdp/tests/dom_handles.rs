//! Element handles over CDP, as Chrome 150 names and describes them.
//!
//! Every object this server handed a client was `{"type": "object", "className": "Object"}`. Puppeteer and Playwright
//! make an ELEMENT handle only of `subtype: "node"`, so `page.$()`, `click`, `type` and `fill` could not work at all,
//! and `DOM.describeNode` / `resolveNode` / `getContentQuads` / `getDocument` were `-32601`.
//!
//! `fixtures/dom-handles.chrome-150.json` is stock Chrome 150.0.7871.46's answer: headless, a fresh profile, one raw
//! CDP client (`~/bench-lean/cdp/probe/page_shapes.mjs`, 2026-10-12), on `fixtures/dom-handles.html`, the
//! Puppeteer/Playwright compat suite's form page. Asserted, with the per-session ids left out:
//! * `Runtime.evaluate("document.querySelector('#q')")`: the remote object's type, subtype, className, description;
//! * `DOM.describeNode` by that object, and for `#go`: nodeType, nodeName, localName, nodeValue, childNodeCount,
//!   attributes;
//! * `DOM.resolveNode` by the `backendNodeId` describeNode gave: the object again;
//! * `DOM.getDocument`: the root's shape.
//!
//! KNOWN, not graded:
//! * the quad's numbers past its left edge -- Chrome's are `[8,108,193,108,193,129,8,129]`, and the box's height and
//!   width follow the fonts the page is laid out with, which a headless Chrome and the persona's do not share. What is
//!   graded is the shape: one axis-aligned quad, clockwise from the top left, at the body's 8 px margin.
//! * `shadowRoots` on `<input>` -- Chrome reports its user-agent shadow root; this engine has none.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PAGE: &str = include_str!("fixtures/dom-handles.html");
const CHROME: &str = include_str!("fixtures/dom-handles.chrome-150.json");

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

    async fn wait_for(&mut self, method: &str) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && !self.events.iter().any(|e| e["method"] == method) {
            if let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), self.ws.next()).await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("method").is_some() {
                    self.events.push(v);
                }
            }
        }
    }
}

/// `v` without the keys named -- the ids that are a session's own.
fn without(v: &Value, keys: &[&str]) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        for k in keys {
            o.remove(*k);
        }
    }
    v
}

#[tokio::test]
async fn element_handles_are_named_described_and_resolved_as_chromes() {
    // The request ledger is process-global: a navigating test takes its turn (see `navigation::ONE_AT_A_TIME`).
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    let port = fixture_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/dom-handles.html") })).await;
        c.wait_for("Page.loadEventFired").await;

        let q = c.call("Runtime.evaluate", json!({ "expression": "document.querySelector('#q')" })).await;
        let q_obj = &q["result"]["result"];
        assert_eq!(without(q_obj, &["objectId"]), chrome["evaluate_q"], "the element's remote object");
        let q_id = q_obj["objectId"].as_str().expect("an objectId").to_string();

        let d = c.call("DOM.describeNode", json!({ "objectId": q_id })).await;
        assert_eq!(without(&d["result"]["node"], &["nodeId", "backendNodeId"]), chrome["describe_q_by_object"], "describeNode #q");
        let backend = d["result"]["node"]["backendNodeId"].as_u64().expect("a backendNodeId");

        let go = c.call("Runtime.evaluate", json!({ "expression": "document.querySelector('#go')" })).await;
        let go_id = go["result"]["result"]["objectId"].as_str().expect("an objectId").to_string();
        let d = c.call("DOM.describeNode", json!({ "objectId": go_id })).await;
        assert_eq!(without(&d["result"]["node"], &["nodeId", "backendNodeId"]), chrome["describe_go_by_node_id"], "describeNode #go");

        let r = c.call("DOM.resolveNode", json!({ "backendNodeId": backend })).await;
        assert_eq!(without(&r["result"]["object"], &["objectId"]), chrome["resolve_q"], "resolveNode by backendNodeId");

        let quads = c.call("DOM.getContentQuads", json!({ "objectId": q_id })).await;
        let quads = quads["result"]["quads"].as_array().expect("quads").clone();
        assert_eq!(quads.len(), 1, "one quad for a one-box element");
        let q: Vec<f64> = quads[0].as_array().expect("a quad").iter().map(|n| n.as_f64().unwrap()).collect();
        assert_eq!(q.len(), 8);
        let chrome_x = chrome["quads_q"][0][0].as_f64().unwrap();
        assert_eq!(q[0], chrome_x, "the quad starts at the body's margin, as Chrome's: {q:?}");
        assert!(q[1] == q[3] && q[5] == q[7] && q[0] == q[6] && q[2] == q[4], "axis-aligned, clockwise from the top left: {q:?}");
        assert!(q[2] > q[0] && q[5] > q[1], "a box with an area: {q:?}");

        let doc = c.call("DOM.getDocument", json!({ "depth": 0 })).await;
        assert_eq!(
            without(&doc["result"]["root"], &["nodeId", "backendNodeId", "documentURL", "baseURL"]),
            chrome["document_root"],
            "getDocument's root"
        );

        // A released handle is gone: releasing it again is V8 inspector's "not found", and a malformed id is its
        // "invalid" -- the two errors `V8RuntimeAgentImpl::releaseObject` answers.
        let released = c.call("Runtime.releaseObject", json!({ "objectId": q_id })).await;
        assert_eq!(released["result"], json!({}), "releaseObject answers {{}}: {released}");
        let again = c.call("Runtime.releaseObject", json!({ "objectId": q_id })).await;
        assert_eq!(again["error"]["message"], "Could not find object with given id", "{again}");
        let bad = c.call("Runtime.releaseObject", json!({ "objectId": "nonsense" })).await;
        assert_eq!(bad["error"]["message"], "Invalid remote object id", "{bad}");
        let still = c.call("DOM.describeNode", json!({ "objectId": go_id })).await;
        assert!(still.get("result").is_some(), "releasing one handle leaves the others: {still}");
        let _ = c.ws.close(None).await;
    };
    let (served, ()) = tokio::join!(server, client);
    served.expect("the connection is served");
}

/// A text node is resolved, described and measured, as Chrome's is.
///
/// chrome-devtools-mcp's `hover` on a snapshot's `StaticText` resolves the text's backend node (`DOM.resolveNode`),
/// which this engine refused for anything but an element or a document ("No node with given id found"), so the tool
/// said the element no longer existed; and a text node had no quads to point at. Chrome 152 (headless, 2026-10-28):
/// `className` `Text`, `description` `#text`, one quad over the text's line, the same in an isolated world.
#[tokio::test]
async fn a_text_node_is_resolved_and_measured_as_chromes_is() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let port = fixture_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/dom-handles.html") })).await;
        c.wait_for("Page.loadEventFired").await;
        let tree = c.call("Accessibility.getFullAXTree", json!({})).await;
        let text = tree["result"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["role"]["value"] == "StaticText" && n["name"]["value"].as_str().is_some_and(|s| !s.trim().is_empty()) && n["backendDOMNodeId"].is_number())
            .cloned()
            .expect("a StaticText with its text node");
        let backend = text["backendDOMNodeId"].clone();

        let resolved = c.call("DOM.resolveNode", json!({ "backendNodeId": backend })).await;
        let obj = &resolved["result"]["object"];
        assert_eq!((&obj["subtype"], &obj["className"], &obj["description"]), (&json!("node"), &json!("Text"), &json!("#text")), "{resolved}");
        let oid = obj["objectId"].as_str().unwrap().to_string();
        let what = c.call("Runtime.callFunctionOn", json!({ "objectId": oid, "functionDeclaration": "function(){ return [this.nodeType, this.isConnected] }", "returnByValue": true })).await;
        assert_eq!(what["result"]["result"]["value"], json!([3, true]), "{what}");
        let described = c.call("DOM.describeNode", json!({ "objectId": oid })).await;
        assert_eq!((&described["result"]["node"]["nodeType"], &described["result"]["node"]["nodeName"]), (&json!(3), &json!("#text")), "{described}");

        let quads = c.call("DOM.getContentQuads", json!({ "objectId": oid })).await;
        let q: Vec<f64> = quads["result"]["quads"][0].as_array().unwrap_or_else(|| panic!("a quad: {quads}")).iter().map(|n| n.as_f64().unwrap()).collect();
        assert!(q[2] > q[0] && q[5] > q[1], "the text's line has an area: {q:?}");

        let frame = c.call("Page.getFrameTree", json!({})).await["result"]["frameTree"]["frame"]["id"].clone();
        let world = c.call("Page.createIsolatedWorld", json!({ "frameId": frame, "worldName": "w" })).await;
        let ctx = world["result"]["executionContextId"].clone();
        let in_world = c.call("DOM.resolveNode", json!({ "backendNodeId": backend, "executionContextId": ctx })).await;
        assert_eq!(in_world["result"]["object"]["className"], "Text", "{in_world}");
        let _ = c.ws.close(None).await;
    };
    let (served, ()) = tokio::join!(server, client);
    served.expect("the connection is served");
}

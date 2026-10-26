//! An isolated world wraps the page's parsed elements as the page's own interfaces, even when the world
//! is the FIRST to touch one whose interface the page put lazily (the registry leaves).
//!
//! Playwright's `page.fill` runs in its utility world, a world named by a new-document script. On a parsed
//! `<input>` that no page script had touched, `protos::lazy_tag_proto` made `HTMLInputElement` under the WORLD's
//! context and kept the world's prototype in the page's tag map. Two bugs followed:
//! - the world minted the `<input>` as an `HTMLElement`, so `input.type` was undefined and Playwright's
//!   `getImplicitAriaRole` threw (`e.type.toLowerCase()`): the compat row, red until this fix;
//! - the page's own `<input>` wrappers took the world's prototype, so the main world's `input` was not an
//!   instance of its own `HTMLInputElement`.
//!
//! The process is WARM first, as the CDP server's second page target is: a page realm puts an interface lazily
//! only on a blueprint hit. The world is probed before the page touches any of the elements, then the page.
//!
//! And the world has the page's `SharedArrayBuffer` gate. A page that is not cross-origin isolated has no
//! `SharedArrayBuffer` in any of its worlds (Blink asks per context, of the page's window); the world kept V8's.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const PAGE: &str = "<!doctype html><title>t</title><body><form><input id=i><select id=s><option>a</option></select>\
<textarea id=t></textarea><button id=b>go</button></form><video id=v></video><canvas id=c></canvas><progress id=p></progress>\
<svg id=g><rect id=r width=1 height=1></rect></svg></body>";

/// Per element: its class, the prototype's own member count, whether its prototype is THIS realm's interface of
/// that name, and the `<input>`'s `type` (what Playwright reads).
const PROBE: &str = "JSON.stringify(['i','s','t','b','v','c','p','r'].map((id) => {
  const e = document.getElementById(id); const tag = Object.prototype.toString.call(e).slice(8, -1);
  const own = window[tag]; const p = Object.getPrototypeOf(e);
  return [id, tag, Object.getOwnPropertyNames(p).length, !!own && p === own.prototype, id === 'i' ? String(e.type) : null];
}))";

async fn page_server() -> u16 {
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

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn call(ws: &mut ClientWs, next: &mut i64, events: &mut Vec<Value>, method: &str, params: Value) -> Value {
    *next += 1;
    let id = *next;
    ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), ws.next()).await.expect("an answer within 30 s").unwrap().unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
            if v.get("method").is_some() {
                events.push(v);
            }
        }
    }
}

#[tokio::test]
async fn an_isolated_world_first_to_wrap_a_lazy_tag_sees_the_pages_interface() {
    // The request ledger is process-global: a navigating test takes its turn (see `navigation::ONE_AT_A_TIME`).
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // a warm process: the first engine's install records the blueprints the second one's puts lazily on
    drop(Engine::new(Box::new(persona::CapturedPersona::default())));
    let port = page_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let mut ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let (mut next, mut events) = (0i64, Vec::new());
        call(&mut ws, &mut next, &mut events, "Page.enable", json!({})).await;
        call(&mut ws, &mut next, &mut events, "Runtime.enable", json!({})).await;
        call(&mut ws, &mut next, &mut events, "Page.addScriptToEvaluateOnNewDocument", json!({ "source": "", "worldName": "util" })).await;
        let mark = events.len();
        call(&mut ws, &mut next, &mut events, "Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/") })).await;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && !events[mark..].iter().any(|e| e["method"] == "Page.loadEventFired") {
            if let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("method").is_some() {
                    events.push(v);
                }
            }
        }
        // the new document's contexts: its main world and the `util` world
        let ctx = |name: Option<&str>| -> i64 {
            events[mark..]
                .iter()
                .filter(|e| e["method"] == "Runtime.executionContextCreated")
                .map(|e| &e["params"]["context"])
                .filter(|c| match name {
                    Some(n) => c["name"] == n,
                    None => c["auxData"]["isDefault"] == true,
                })
                .last()
                .and_then(|c| c["id"].as_i64())
                .expect("the context was announced")
        };
        let (util, main) = (ctx(Some("util")), ctx(None));
        let mut answers = Vec::new();
        // the WORLD first: nothing in the page has touched these elements yet
        for id in [util, main] {
            let r = call(&mut ws, &mut next, &mut events, "Runtime.evaluate", json!({ "expression": PROBE, "contextId": id, "returnByValue": true })).await;
            let v: Value = serde_json::from_str(r["result"]["result"]["value"].as_str().expect("the probe's JSON")).unwrap();
            answers.push(v);
        }
        // the page's SharedArrayBuffer gate, in both
        for id in [util, main] {
            let expr = "typeof SharedArrayBuffer + ' ' + crossOriginIsolated";
            let r = call(&mut ws, &mut next, &mut events, "Runtime.evaluate", json!({ "expression": expr, "contextId": id, "returnByValue": true })).await;
            answers.push(r["result"]["result"]["value"].clone());
        }
        ws.send(Message::Close(None)).await.unwrap();
        answers
    };
    let (server, answers) = tokio::join!(server, client);
    server.unwrap();
    let (world, page) = (&answers[0], &answers[1]);
    for (i, want) in ["HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLVideoElement", "HTMLCanvasElement", "HTMLProgressElement", "SVGRectElement"].iter().enumerate() {
        assert_eq!(world[i][1], *want, "the world's {} is its own {want}: {world}", world[i][0]);
        assert_eq!(page[i][1], *want, "the page's {} is its own {want}: {page}", page[i][0]);
        assert_eq!(world[i][3], true, "the world's {} has the WORLD's {want}.prototype: {world}", world[i][0]);
        assert_eq!(page[i][3], true, "the page's {} has the PAGE's {want}.prototype, not a world's: {page}", page[i][0]);
        assert_eq!(world[i][2], page[i][2], "{want}.prototype has the same members in both realms");
    }
    assert_eq!(world[0][4], "text", "the world reads the <input>'s type (Playwright's getImplicitAriaRole)");
    assert_eq!(page[0][4], "text");
    assert_eq!(answers[3], "undefined false", "the page is not cross-origin isolated: no SharedArrayBuffer");
    assert_eq!(answers[2], answers[3], "the world has the page's SharedArrayBuffer gate");
}

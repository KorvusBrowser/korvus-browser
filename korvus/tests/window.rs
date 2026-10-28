//! A page's window over `korvus serve`'s browser endpoint, from the page's own session, as Puppeteer sizes it.
//!
//! `page.resize()` (chrome-devtools-mcp's `resize_page`) sends `Browser.getWindowForTarget` with no target, then
//! `Browser.setContentsSize`, on the PAGE's session. The router answered the window commands on the browser's session
//! only, the page's process had none (`'Browser.getWindowForTarget' wasn't found`), and `setContentsSize` and
//! `getWindowBounds` existed nowhere. Stock Chrome 152 (headless, a 1534 x 910 window whose viewport was 1534 x 823)
//! answers them on a page's session: 1000 x 700 made the viewport 1000 x 700 and the window 1000 x 787, and its
//! refusals are the ones asserted below. And its page session takes any CPU throttling rate; this engine throttles
//! nothing, so it takes the rates that throttle nothing (1 or less) and refuses the rest.
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use tokio_tungstenite::tungstenite::Message;

/// The endpoint's process, ended with the test however it ends.
struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `korvus serve` on an ephemeral port: the process and its address, from the line it prints when it listens.
fn serve() -> (Serve, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_korvus"))
        .args(["serve", "--port", "0"])
        .env("KORVUS_UPDATE", "off")
        .env("KORVUS_HOST", "ignore")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("korvus serve runs");
    let mut line = String::new();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    out.read_line(&mut line).unwrap();
    // the rest is read and dropped: a closed pipe would end the process at its next line
    std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
    let addr = line.split("http://").nth(1).and_then(|s| s.split_whitespace().next()).unwrap_or_else(|| panic!("the ready line: {line}")).to_string();
    (Serve(child), addr)
}

fn ws_url(addr: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET /json/version HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).unwrap();
    let body: Value = serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    body["webSocketDebuggerUrl"].as_str().unwrap().to_string()
}

struct Cdp {
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    next: i64,
}

impl Cdp {
    /// One command, on `session` or the browser's: its whole answer.
    async fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        self.ws.send(Message::Text(m.to_string())).await.unwrap();
        loop {
            if let Message::Text(t) = self.ws.next().await.unwrap().unwrap() {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["id"].as_i64() == Some(id) {
                    return v;
                }
            }
        }
    }
}

#[tokio::test]
async fn a_page_session_sizes_its_window_as_chromes_does() {
    let (_serve, addr) = serve();
    let (ws, _) = tokio_tungstenite::connect_async(ws_url(&addr)).await.unwrap();
    let mut c = Cdp { ws, next: 0 };
    let target = c.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await;
    let target = target["result"]["targetId"].as_str().unwrap().to_string();
    let attached = c.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true })).await;
    let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
    let page = Some(session.as_str());
    let dims = "JSON.stringify([innerWidth, innerHeight, outerWidth, outerHeight])";

    // the page's own window, named by no target, as the browser's session names it by the page's
    let mine = c.call(page, "Browser.getWindowForTarget", json!({})).await;
    let theirs = c.call(None, "Browser.getWindowForTarget", json!({ "targetId": target })).await;
    assert_eq!(mine["sessionId"], json!(session), "answered on the page's session: {mine}");
    assert_eq!(mine["result"], theirs["result"], "the page's window");
    let window = mine["result"]["windowId"].as_u64().expect("a window id");
    let at_start = c.call(page, "Runtime.evaluate", json!({ "expression": dims, "returnByValue": true })).await;
    let at_start: Vec<i64> = serde_json::from_str(at_start["result"]["result"]["value"].as_str().unwrap()).unwrap();
    let frame = at_start[3] - at_start[1];
    let bounds = c.call(page, "Browser.getWindowBounds", json!({ "windowId": window })).await;
    assert_eq!(bounds["result"]["bounds"], mine["result"]["bounds"], "getWindowBounds");

    // the viewport made 1000 x 700, the window's frame kept around it
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window, "width": 1000, "height": 700 })).await;
    assert_eq!(r["result"], json!({}), "{r}");
    let after = c.call(page, "Runtime.evaluate", json!({ "expression": dims, "returnByValue": true })).await;
    assert_eq!(after["result"]["result"]["value"], json!(format!("[1000,700,1000,{}]", 700 + frame)), "the page's window after it");
    let bounds = c.call(page, "Browser.getWindowBounds", json!({ "windowId": window })).await;
    assert_eq!((&bounds["result"]["bounds"]["width"], &bounds["result"]["bounds"]["height"]), (&json!(1000), &json!(700 + frame)));
    let r = c.call(None, "Browser.setContentsSize", json!({ "windowId": window, "width": 900 })).await;
    assert_eq!(r["result"], json!({}), "{r}");
    let after = c.call(page, "Runtime.evaluate", json!({ "expression": dims, "returnByValue": true })).await;
    assert_eq!(after["result"]["result"]["value"], json!(format!("[900,700,900,{}]", 700 + frame)), "the width alone");

    // Chrome 152's refusals
    let error = |v: &Value| (v["error"]["code"].clone(), v["error"]["message"].clone());
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window })).await;
    assert_eq!(error(&r), (json!(-32602), json!("At least one of 'width' or 'height' must be specified")));
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window, "width": 0, "height": 0 })).await;
    assert_eq!(error(&r), (json!(-32602), json!("Contents 'width' must be a positive value")));
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window, "height": -3 })).await;
    assert_eq!(error(&r), (json!(-32602), json!("Contents 'height' must be a positive value")));
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window, "width": 801.5 })).await;
    assert_eq!(error(&r).0, json!(-32602), "a width that is no integer: {r}");
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": 5, "width": 800, "height": 600 })).await;
    assert_eq!(error(&r), (json!(-32000), json!("Browser window not found")));
    let r = c.call(page, "Browser.getWindowBounds", json!({ "windowId": 5 })).await;
    assert_eq!(error(&r), (json!(-32000), json!("Browser window not found")));
    let r = c.call(page, "Browser.setWindowBounds", json!({ "windowId": window, "bounds": { "windowState": "maximized" } })).await;
    assert_eq!(r["result"], json!({}), "{r}");
    let r = c.call(page, "Browser.setContentsSize", json!({ "windowId": window, "width": 800, "height": 600 })).await;
    assert_eq!(error(&r), (json!(-32000), json!("Restore window to normal state before setting content size")));

    // CPU throttling: what throttles nothing is taken, the rest refused
    for rate in [1.0, 0.5] {
        let r = c.call(page, "Emulation.setCPUThrottlingRate", json!({ "rate": rate })).await;
        assert_eq!(r["result"], json!({}), "rate {rate}: {r}");
    }
    let r = c.call(page, "Emulation.setCPUThrottlingRate", json!({ "rate": 4 })).await;
    assert_eq!(error(&r).0, json!(-32000), "rate 4: {r}");
    let r = c.call(page, "Emulation.setCPUThrottlingRate", json!({})).await;
    assert_eq!(error(&r).0, json!(-32602), "no rate: {r}");
}

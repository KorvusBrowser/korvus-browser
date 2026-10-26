//! A SECOND CDP client on one page target reads the page, not another world.
//!
//! # What this is a test of
//!
//! Unit S1, censusing the structural cluster, found that its own first measurement was wrong
//! because the probe it attached alongside the driver was reading a different realm: `document`
//! came back `{"readyState":"complete"}` with prototype `Object` and no `documentElement`, on
//! `example.com` as well as on a loopback fixture. The cause was not how contexts were announced.
//! `cdp::serve` built an `Engine` — a V8 isolate, a realm, an `about:blank` — **per WebSocket
//! connection**, while advertising the same `targetId` throughout, so one target had as many
//! realms as it had ever had clients and only the client that navigated could see the page.
//!
//! One protocol, both engines, no engine-specific step. A probe attached beside the
//! driver reading a world the page does not live in breaks that at the root, and it produces wrong
//! numbers that look plausible, which is the most expensive thing this project makes.
//!
//! # The oracle
//!
//! `fixtures/second-client.chrome-150.txt` is what **Chrome 150.0.7871.46** answers to exactly this
//! experiment, recorded twice, byte-identical, with Chrome launched by us and not by Playwright
//! (M1). Its header carries the flags, the fixture page, and the rule for the two `!` lines that are
//! recorded and deliberately not asserted.
//!
//! **This engine's own column is Chrome's, not a tautology.** A rows are asserted too: if the
//! page this engine loads were not the page Chrome loaded, A column would differ first and the
//! B column's agreement with it would prove nothing.
//!
//! # Re-record
//!
//! Drive the same sequence against `google-chrome --headless=new --remote-debugging-port=0` over
//! two sequential page-socket connections and substitute the loopback origin with `<BASE>`.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const CHROME: &str = include_str!("fixtures/second-client.chrome-150.txt");

const PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>CDP1 loopback fixture</title></head><body>\
<h1 id=\"h\">CDP1</h1><p class=\"p\">one page, two clients</p>\
<div id=\"d\"><span>a</span><span>b</span></div>\
<script>window.__fixture_ran = true;</script></body></html>";

/// The rows both clients are asked, in the order the fixture records them.
const ROWS: &[(&str, &str)] = &[
    ("document.documentElement.tagName", "document.documentElement ? document.documentElement.tagName : null"),
    ("proto-of-document ctor name", "(function(){var p=Object.getPrototypeOf(document);return p&&p.constructor?p.constructor.name:String(p)})()"),
    ("String(document)", "String(document)"),
    ("document.readyState", "document.readyState"),
    ("document.title", "document.title"),
    ("document.querySelectorAll('*').length", "document.querySelectorAll ? document.querySelectorAll('*').length : 'no querySelectorAll'"),
    ("location.href", "location.href"),
    ("window === self", "window === self"),
    ("globalThis === window", "globalThis === window"),
    ("document === window.document", "document === window.document"),
    ("marker the FIRST client set", "typeof globalThis.__cdp1_marker === 'undefined' ? '<<absent>>' : globalThis.__cdp1_marker"),
];

/// A CDP client that keeps the events it was sent, because which contexts a socket was told about
/// is half of what this test measures.
struct Cdp {
    ws: ClientWs,
    next_id: i64,
    events: Vec<Value>,
}

impl Cdp {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        // This server upgrades on the `Upgrade` header rather than the path (see `tests/server.rs`),
        // so no `/json/list` round trip is needed -- and none is made, so the accept loop sees
        // exactly the two connections this test is about.
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/browser/")).await.unwrap().0;
        Cdp { ws, next_id: 0, events: Vec::new() }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        loop {
            let msg = self.ws.next().await.expect("the socket closed mid-command").unwrap();
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    return v.get("result").cloned().unwrap_or(Value::Null);
                }
                self.events.push(v);
            }
        }
    }

    /// Read events until `method` arrives. A navigation answers at commit and the document loads
    /// after, so a client that evaluates the moment `Page.navigate` returns reads the old document.
    async fn wait_for(&mut self, method: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let Ok(Some(Ok(msg))) =
                tokio::time::timeout(std::time::Duration::from_secs(30), self.ws.next()).await
            else {
                panic!("the socket closed before {method}");
            };
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                let hit = v.get("method").and_then(Value::as_str) == Some(method);
                self.events.push(v);
                if hit {
                    return;
                }
            }
        }
        panic!("{method} never arrived within 30s");
    }

    /// `Runtime.evaluate`, by value, rendered the way the fixture records it.
    async fn eval(&mut self, expr: &str) -> String {
        let r = self.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true })).await;
        if let Some(ed) = r.get("exceptionDetails") {
            return format!("THREW {ed}");
        }
        match r.pointer("/result/value") {
            Some(v) => serde_json::to_string(v).unwrap(),
            None => format!("<{}>", r.pointer("/result/type").and_then(Value::as_str).unwrap_or("?")),
        }
    }

    /// The `Runtime.executionContextCreated` this socket was given, in order.
    fn contexts(&self) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|e| e.get("method").and_then(Value::as_str) == Some("Runtime.executionContextCreated"))
            .filter_map(|e| e.pointer("/params/context"))
            .collect()
    }

    async fn attach(&mut self) {
        self.call("Page.enable", json!({})).await;
        self.call("Runtime.enable", json!({})).await;
        self.call("Runtime.runIfWaitingForDebugger", json!({})).await;
    }
}

/// A loopback HTTP server that answers `PAGE` to anything, for as long as the test runs.
async fn fixture_server() -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    PAGE.len(),
                    PAGE
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    addr
}

/// The lines this engine produces for the experiment the fixture records.
async fn readings(cdp_addr: std::net::SocketAddr, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let url = format!("{base}/fixture.html");

    // ── client A: attaches, navigates, reads, DETACHES ───────────────────────────────────────
    let mut a = Cdp::connect(cdp_addr).await;
    a.attach().await;
    a.call("Page.navigate", json!({ "url": url })).await;
    a.wait_for("Page.loadEventFired").await;
    a.eval("globalThis.__cdp1_marker = 'set-by-the-first-client'").await;
    for (name, expr) in ROWS {
        out.push(format!("A {name} = {}", a.eval(expr).await));
    }
    let a_ctx = a.contexts();
    let a_count = a_ctx.len();
    let a_last = (*a_ctx.last().expect("the first client is announced a context")).clone();
    out.push(format!("A contexts announced = {a_count}"));
    out.push(format!("A last context origin = {}", a_last["origin"]));
    out.push(format!("A last context isDefault = {}", a_last["auxData"]["isDefault"]));
    a.ws.send(Message::Close(None)).await.unwrap();
    drop(a);

    // ── client B: attaches to the same target afterwards, and never navigates ────────────────
    let mut b = Cdp::connect(cdp_addr).await;
    b.attach().await;
    for (name, expr) in ROWS {
        out.push(format!("B {name} = {}", b.eval(expr).await));
    }
    let b_ctx = b.contexts();
    let b_count = b_ctx.len();
    let b_last = (*b_ctx.last().expect("the second client is announced a context")).clone();
    out.push(format!("B contexts announced = {b_count}"));
    out.push(format!("B last context origin = {}", b_last["origin"]));
    out.push(format!("B last context isDefault = {}", b_last["auxData"]["isDefault"]));
    out.push(format!("B last context id == A last context id = {}", b_last["id"] == a_last["id"]));
    out.push(format!("B context uniqueId == A's = {}", b_last["uniqueId"] == a_last["uniqueId"]));
    let tree = b.call("Page.getFrameTree", json!({})).await;
    out.push(format!("B frameTree url = {}", tree.pointer("/frameTree/frame/url").cloned().unwrap_or(Value::Null)));
    b.ws.send(Message::Close(None)).await.unwrap();
    out
}

/// The fixture's asserted lines, with `<BASE>` resolved. Comments and `!` lines are not asserted;
/// `!` lines are counted, so softening this file is a visible edit.
fn expected(base: &str) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut unasserted = 0;
    for raw in CHROME.lines() {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix('!') {
            assert!(rest.contains('#'), "an unasserted fixture line must carry its reason: {raw}");
            unasserted += 1;
            continue;
        }
        lines.push(t.replace("<BASE>", base));
    }
    (lines, unasserted)
}

#[tokio::test]
async fn a_second_client_reads_the_page_the_first_one_left() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page_addr = fixture_server().await;
    let base = format!("http://{page_addr}");

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    // THE SERVER'S OWN ACCEPT LOOP, for exactly the two connections this test makes -- not a
    // second definition of it (UNIT-CONTRACT, "assert the rule, never a second definition").
    let server = cdp::serve_connections(
        cdp_listener,
        || Engine::new(Box::new(persona::CapturedPersona::default())),
        std::sync::Arc::new(persona::CapturedPersona::default()),
        2,
    );
    let client = readings(cdp_addr, &base);
    let (served, got) = tokio::join!(server, client);
    served.unwrap();

    let (want, unasserted) = expected(&base);
    assert_eq!(
        unasserted, 2,
        "the fixture has {unasserted} lines recorded-but-not-asserted; it had 2, and each one is a \
         row this lane deliberately does not own. Adding one is how a table stops being a test."
    );
    let mut diffs = Vec::new();
    for i in 0..want.len().max(got.len()) {
        let w = want.get(i).map(String::as_str).unwrap_or("<missing>");
        let g = got.get(i).map(String::as_str).unwrap_or("<missing>");
        if w != g {
            diffs.push(format!("  chrome: {w}\n  korvus: {g}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} of {} recorded Chrome 150 lines differ:\n{}",
        diffs.len(),
        want.len(),
        diffs.join("\n")
    );
}

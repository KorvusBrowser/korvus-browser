//! G1.8: a CDP WebSocket client drives the server end-to-end (evaluate + navigate + DOM).
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open the browser-level CDP WebSocket on `addr`.
///
/// The real path carries a per-process UUID (`/devtools/browser/<uuid>`) that a client discovers
/// from `/json/version`; `cdp_identifiers_are_chrome_shaped_and_correlate` connects that way and
/// proves it. This server upgrades on the `Upgrade` header rather than the path, so a test that is
/// not about discovery connects directly and pins no path.
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

#[tokio::test]
async fn cdp_drive() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // Page HTTP server for Page.navigate.
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = page.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        let body = "<!doctype html><html><head><title>PageTitle</title></head><body><div id=\"x\">hi there</div></body></html>";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    });

    // CDP server.
    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;

        let ver = call(&mut ws, 1, "Browser.getVersion", json!({})).await;
        // The full check lives in `cdp_version_answers_the_persona_not_the_engine_name`; here it is
        // enough that the handshake Playwright performs first still answers something Chrome-shaped.
        assert_eq!(ver["protocolVersion"], "1.3");
        assert!(
            ver["product"].as_str().unwrap().starts_with("Chrome/"),
            "Browser.getVersion product must be a Chrome product string, got {}",
            ver["product"]
        );

        let ev = call(&mut ws, 2, "Runtime.evaluate", json!({ "expression": "1 + 2 * 3" })).await;
        assert_eq!(ev["result"]["type"], "number");
        assert_eq!(ev["result"]["value"], 7);

        // W9.16: the frame id is READ OFF THE PROTOCOL, not pinned to a literal. It is minted per
        // server process now, so the only thing a client can assert is what a client actually
        // needs: `Page.navigate` answers the same frame the frame tree reports, and that id does
        // not name the engine. `cdp_identifiers_are_chrome_shaped_and_correlate` checks the shape
        // and every other correlation exhaustively.
        let tree = call(&mut ws, 3, "Page.getFrameTree", json!({})).await;
        let frame_id =
            tree["frameTree"]["frame"]["id"].as_str().expect("a frame id in the tree").to_string();
        assert!(
            !frame_id.to_lowercase().contains("korvus"),
            "the frame id names the engine: {frame_id}"
        );

        let nav = call(&mut ws, 4, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;
        assert_eq!(
            nav["frameId"].as_str(),
            Some(frame_id.as_str()),
            "Page.navigate must answer the frame the frame tree reports, got {nav}"
        );

        let title = call(&mut ws, 5, "Runtime.evaluate", json!({ "expression": "document.title" })).await;
        assert_eq!(title["result"]["value"], "PageTitle");

        let text = call(&mut ws, 6, "Runtime.evaluate", json!({ "expression": "document.getElementById('x').textContent" })).await;
        assert_eq!(text["result"]["value"], "hi there");

        ws.send(Message::Close(None)).await.unwrap();
    };

    tokio::join!(server, client).0.unwrap();
}

/// FIXER C6: the CDP Input domain dispatches EXACTLY what the client sends, as Chrome 150 does
/// (`content/browser/devtools/protocol/input_handler.cc`). This test used to require the opposite -- a
/// `mouseMoved` turned into a humanised trajectory of >= 10 mousemoves and a `mousePressed` into a whole
/// click -- which made korvus the browser that fires 696 events for 18 CDP moves and releases a held button
/// by itself. Now: one `mouseMoved` is one `mousemove`, at the ELEMENT under the point (not the document); a
/// `mousePressed` is a `mousedown` with the client's `clickCount` as `detail` and no click; the `click` comes
/// with the `mouseReleased`, once.
#[tokio::test]
async fn cdp_input_dispatches_exactly_what_the_client_sends() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = page.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        let body = "<!doctype html><html><body style='margin:0'><div id=box style='width:200px;height:200px'></div></body></html>";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    });

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        call(&mut ws, 1, "Browser.getVersion", json!({})).await;
        call(&mut ws, 2, "Page.navigate", json!({ "url": format!("http://{page_addr}/") })).await;
        call(&mut ws, 3, "Runtime.evaluate", json!({ "expression":
            "globalThis.__mm=0; globalThis.__mt=0; globalThis.__ck=0; globalThis.__md=''; globalThis.__tg=''; \
             document.addEventListener('mousemove', e => { globalThis.__mm++; if (e.isTrusted) globalThis.__mt++; globalThis.__tg = e.target.id + ',' + (e.toElement && e.toElement.id); }); \
             document.addEventListener('mousedown', e => { globalThis.__md = e.detail + ',' + e.buttons; }); \
             document.addEventListener('click', e => { globalThis.__ck++; globalThis.__cd = e.detail + ',' + e.isTrusted; }); 0" })).await;

        call(&mut ws, 4, "Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": 60.0, "y": 40.0 })).await;
        let mm = call(&mut ws, 5, "Runtime.evaluate", json!({ "expression": "globalThis.__mm + '|' + globalThis.__mt + '|' + globalThis.__tg" })).await;
        assert_eq!(mm["result"]["value"], "1|1|box,box", "one trusted mousemove at the element under the point, got {mm}");

        call(&mut ws, 6, "Input.dispatchMouseEvent", json!({ "type": "mousePressed", "x": 60.0, "y": 40.0, "button": "left", "clickCount": 1 })).await;
        let down = call(&mut ws, 7, "Runtime.evaluate", json!({ "expression": "globalThis.__md + '|' + globalThis.__ck" })).await;
        assert_eq!(down["result"]["value"], "1,1|0", "a press is a press: detail = clickCount, the button held, no click yet");

        call(&mut ws, 8, "Input.dispatchMouseEvent", json!({ "type": "mouseReleased", "x": 60.0, "y": 40.0, "button": "left", "clickCount": 1 })).await;
        let ck = call(&mut ws, 9, "Runtime.evaluate", json!({ "expression": "globalThis.__ck + '|' + globalThis.__cd" })).await;
        assert_eq!(ck["result"]["value"], "1|1,true", "the release clicks, once, trusted, detail 1");

        let bad = call_full(&mut ws, 10, "Input.dispatchMouseEvent", json!({ "type": "mousePressed", "x": 1.0, "y": 1.0, "button": "thumb" })).await;
        assert_eq!(bad["error"]["code"], -32602, "an unknown button is refused as Chrome refuses it: {bad}");
        assert_eq!(bad["error"]["message"], "Invalid mouse button");

        ws.send(Message::Close(None)).await.unwrap();
    };

    tokio::join!(server, client).0.unwrap();
}

/// W9.5 — the control channel wears the SAME identity as the page.
///
/// `Browser.getVersion` and `/json/version` used to answer `Korvus/0.0.0` / `Korvus`: the engine's own
/// name, hardcoded, handed to anything that attached over CDP — while `navigator.userAgent` in the
/// very same process claimed Chrome 151. Two answers to one question, and the one on the control
/// channel named the engine. Whatever drives Korvus over CDP is exactly what an automation-detection
/// harness drives, so this is a first-tier tell, not tidiness.
///
/// The shape asserted here was MEASURED from `google-chrome --headless=new --remote-debugging-port`
/// on 2026-09-11 (Chrome 150.0.7871.46), not guessed — including the part that looks wrong: `product`
/// carries the full version and always says `Chrome`, while `userAgent` is version-reduced and may say
/// `HeadlessChrome`. They are allowed to differ; neither may name this engine.
#[tokio::test]
async fn cdp_version_answers_the_persona_not_the_engine_name() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    use spec::Persona;
    let p = persona::CapturedPersona::default();
    let ua = p.navigator().user_agent;
    let full = p.client_hints().ua_full_version;
    assert!(!ua.is_empty() && !full.is_empty(), "the shipped bundle must carry a UA and a full version");

    // (a) the WebSocket answer.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut ws = connect_cdp(addr).await;
        let ver = call(&mut ws, 1, "Browser.getVersion", json!({})).await;
        assert_eq!(ver["userAgent"], ua, "CDP must report the UA the page reports");
        assert_eq!(ver["product"], format!("Chrome/{full}"), "product is Chrome + the identity's full version");
        assert_eq!(ver["protocolVersion"], "1.3");
        assert!(
            !ver["jsVersion"].as_str().unwrap().is_empty(),
            "jsVersion must name the V8 this engine really runs; Chrome answers e.g. 15.0.245.13"
        );
        for (k, v) in ver.as_object().unwrap() {
            assert!(!v.as_str().unwrap_or("").to_lowercase().contains("korvus"), "Browser.getVersion.{k} names the engine: {v}");
        }
        ws.send(Message::Close(None)).await.unwrap();
    };
    tokio::join!(server, client).0.unwrap();

    // (b) the HTTP discovery answer, which Playwright reads BEFORE it ever opens a socket. Same four
    // facts; a process that answered one of them differently over HTTP would be tellable by the
    // disagreement alone.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await.unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        let body = text.split("\r\n\r\n").nth(1).expect("an HTTP body").to_string();
        let v: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(v["User-Agent"], ua);
        assert_eq!(v["Browser"], format!("Chrome/{full}"));
        assert_eq!(v["Protocol-Version"], "1.3");
        assert!(!v["V8-Version"].as_str().unwrap().is_empty(), "V8-Version must not be empty");
        assert!(v["WebKit-Version"].as_str().unwrap().starts_with("537."), "WebKit-Version comes from the UA, got {}", v["WebKit-Version"]);
        for k in ["Browser", "User-Agent", "V8-Version", "WebKit-Version"] {
            assert!(!v[k].as_str().unwrap().to_lowercase().contains("korvus"), "/json/version {k} names the engine: {}", v[k]);
        }
    };
    tokio::join!(server, client).0.unwrap();
}

// ---------------------------------------------------------------------------------------------
// W9.22 — what this channel REFUSES. A server that succeeds at a method nobody has implemented
// tells a harness what it is in one round trip, and that is cheaper than any page-realm probe.
// ---------------------------------------------------------------------------------------------

/// Every unknown method is refused with Chrome's exact envelope, and the methods a real harness
/// actually sends still answer.
///
/// The envelopes are MEASURED, `google-chrome --headless=new --remote-debugging-port=0`, 2026-09-12,
/// Chrome 150.0.7871.46 — the table is in `plugins/korvus-cdp/src/lib.rs` on `RpcError`. The four
/// probe shapes below are four separate Chrome measurements that all came back byte-identical, which
/// is the finding: there is ONE unknown-method envelope, not one per kind of wrongness.
#[tokio::test]
async fn unknown_methods_are_refused_the_way_chrome_refuses_them() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut ws = connect_cdp(addr).await;

        // Chrome answers all four of these with the SAME code and the same message template.
        let probes = [
            "Foo.bar",                 // unknown domain
            "Page.definitelyNotReal",  // known domain, unknown method
            "bareMethodNoDot",         // no domain at all
            "page.enable",             // right method, wrong case
            "Page.js_error_count",     // this engine's own former extension
            "HeapProfiler.takeHeapSnapshot", // a REAL Chrome method this engine does not implement (`Fetch.enable`, `Runtime.addBinding` and `Page.printToPDF` were)
        ];
        let mut id = 0;
        for m in probes {
            id += 1;
            let r = call_full(&mut ws, id, m, json!({})).await;
            assert!(r.get("result").is_none(), "{m} must not answer a result: {r}");
            assert_eq!(r["error"]["code"], -32601, "{m}: Chrome's code is -32601, got {r}");
            assert_eq!(
                r["error"]["message"],
                format!("'{m}' wasn't found"),
                "{m}: Chrome's exact message, got {r}"
            );
            assert!(
                r["error"].get("data").is_none(),
                "Chrome's -32601 carries NO data field, got {r}"
            );
            assert!(
                !r.to_string().to_lowercase().contains("korvus"),
                "the refusal names the engine: {r}"
            );
        }

        // A message with no `method` is malformed, and Chrome uses its own code for that.
        id += 1;
        let req = json!({ "id": id, "params": {} });
        ws.send(Message::Text(req.to_string())).await.unwrap();
        let r = loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    break v;
                }
            }
        };
        assert_eq!(r["error"]["code"], -32600, "a message with no method: {r}");
        assert_eq!(r["error"]["message"], "Message must have string 'method' property", "{r}");

        // …and the settings calls a real harness sends while attaching still answer `{}`. This list
        // is the one Playwright 1.60 `connect_over_cdp` was MEASURED sending at this binary; a
        // blanket refusal would have broken attachment on the first burst.
        for m in [
            "Browser.setDownloadBehavior",
            "Log.enable",
            "Page.setLifecycleEventsEnabled",
            "Network.enable",
            "Emulation.setFocusEmulationEnabled",
            "Emulation.setEmulatedMedia",
            "DOM.enable",
            "Emulation.setDeviceMetricsOverride",
            "Network.setUserAgentOverride",
        ] {
            id += 1;
            let r = call_full(&mut ws, id, m, json!({})).await;
            assert!(r.get("error").is_none(), "{m} must still be accepted: {r}");
            assert_eq!(r["result"], json!({}), "{m} answers Chrome's empty result: {r}");
        }

        ws.send(Message::Close(None)).await.unwrap();
    };
    tokio::join!(server, client).0.unwrap();
}

// ---------------------------------------------------------------------------------------------
// W9.16 — the identifiers the control channel puts on the wire.
// ---------------------------------------------------------------------------------------------

/// Chrome's `targetId`, `sessionId`, `frameId`, `browserContextId` and `loaderId` are all the same
/// shape: 32 UPPERCASE hex characters, no dashes. Measured over real Chrome 150.0.7871.46
/// (`--headless=new --remote-debugging-port=0`), two runs, 2026-09-12 — the table is in
/// `plugins/korvus-cdp/src/ids.rs`.
fn hex32(v: &Value, what: &str) -> String {
    let s = v.as_str().unwrap_or_else(|| panic!("{what} must be a string, got {v}"));
    assert_eq!(s.len(), 32, "{what}: Chrome's is 32 chars, got {s:?}");
    assert!(
        s.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)),
        "{what}: Chrome's is UPPERCASE hex with no dashes, got {s:?}"
    );
    s.to_string()
}

/// An execution context's `uniqueId` is NOT the same shape as a target id: Chrome answers two
/// signed 64-bit decimals joined by a dot (`-6739544306189926202.7689520667313427499`), either half
/// possibly negative. A UUID here would be a tell, not a fix.
fn unique_id(v: &Value, what: &str) -> String {
    let s = v.as_str().unwrap_or_else(|| panic!("{what} must be a string, got {v}"));
    let halves: Vec<&str> = s.split('.').collect();
    assert_eq!(halves.len(), 2, "{what}: Chrome's is <i64>.<i64>, got {s:?}");
    for h in &halves {
        h.parse::<i64>().unwrap_or_else(|_| panic!("{what}: {h:?} is not an i64 in {s:?}"));
    }
    s.to_string()
}

/// The `/devtools/browser/<id>` path is the ONE place Chrome uses a UUID, and it is lowercase
/// RFC-4122 v4.
fn uuid_v4_lower(s: &str, what: &str) {
    let f: Vec<&str> = s.split('-').collect();
    assert_eq!(
        f.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12],
        "{what}: Chrome's is a dashed UUID, got {s:?}"
    );
    assert!(
        s.bytes().all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{what}: lowercase hex only, got {s:?}"
    );
    assert_eq!(&f[2][..1], "4", "{what}: UUID version nibble, got {s:?}");
    assert!("89ab".contains(&f[3][..1]), "{what}: RFC-4122 variant nibble, got {s:?}");
}

/// `call`, but keeping every frame the server sent — the ids in the EVENTS have to agree with the
/// ids in the RESULTS, and a helper that drops events cannot see that.
async fn call_log(
    ws: &mut ClientWs,
    id: i64,
    method: &str,
    params: Value,
    log: &mut Vec<Value>,
) -> Value {
    let req = json!({ "id": id, "method": method, "params": params });
    ws.send(Message::Text(req.to_string())).await.unwrap();
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            log.push(v.clone());
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }
}

/// Read frames into `log` until an event named `method` arrives.
async fn drain_until(ws: &mut ClientWs, method: &str, log: &mut Vec<Value>) {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            log.push(v.clone());
            if v.get("method").and_then(Value::as_str) == Some(method) {
                return;
            }
        }
    }
}

/// `Page.navigate` answers when the document COMMITS, and the page's scripts run after
/// the answer, not before it.
///
/// This server used to answer after the whole load, so a page whose scripts ran for 30 s left the
/// parity driver's `Page.navigate` unanswered for 30 s, on seven replayed sites whose Chrome oracle
/// answered all seven. Chrome's order, MEASURED (stock `google-chrome --headless=new` 150.0.7871.46,
/// a loopback page whose inline script busy-loops 40 s, one page-target WebSocket; seconds from
/// sending `Page.navigate`):
///
/// ```text
///   0.05  Page.navigate answered {frameId, loaderId}
///   0.05  Runtime.evaluate("document.title") sent
///   0.05  Page.frameNavigated
///  40.06  Runtime.evaluate answered "done"      <- the script's own last statement
///  40.07  Page.domContentEventFired, Page.loadEventFired
/// ```
///
/// The assertions are ORDER on the wire, never time, so they hold on any machine: the answer is
/// ahead of every event of the navigation it answers except the document request's own
/// `Network.*` (sent before commit, in Chrome and here); an evaluate sent right after the answer
/// sees the page AFTER its script ran, which is Chrome's row four (a long task is not preempted for
/// `Runtime.evaluate` there either); and the load events still arrive.
#[tokio::test]
async fn page_navigate_answers_at_commit_and_the_page_loads_after() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = page.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        // The script's LAST statement is what an evaluate after the answer must see. The loop gives
        // it real work to do before it gets there.
        let body = "<!doctype html><html><head><title>before</title></head><body><p>x</p>\
                    <script>let n = 0; for (let i = 0; i < 2e6; i++) { n += i } \
                    document.title = 'done';</script></body></html>";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    });

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        let mut log: Vec<Value> = Vec::new();
        call_log(&mut ws, 1, "Page.enable", json!({}), &mut log).await;
        call_log(&mut ws, 2, "Runtime.enable", json!({}), &mut log).await;
        call_log(&mut ws, 3, "Network.enable", json!({}), &mut log).await;

        // Both commands go out before anything comes back, as the measurement's did.
        let url = format!("http://{page_addr}/");
        ws.send(Message::Text(json!({ "id": 4, "method": "Page.navigate", "params": { "url": url } }).to_string()))
            .await
            .unwrap();
        ws.send(Message::Text(
            json!({ "id": 5, "method": "Runtime.evaluate",
                    "params": { "expression": "document.title", "returnByValue": true } })
            .to_string(),
        ))
        .await
        .unwrap();
        let before = log.len();
        let mut evaluated = None;
        while evaluated.is_none() || events(&log[before..], "Page.frameStoppedLoading").is_empty() {
            let msg = ws.next().await.unwrap().unwrap();
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(5) {
                    evaluated = Some(v.clone());
                }
                log.push(v);
            }
        }
        let wire = &log[before..];
        let position = |pred: &dyn Fn(&Value) -> bool| wire.iter().position(pred);
        let answered = position(&|v| v.get("id").and_then(Value::as_i64) == Some(4))
            .expect("Page.navigate was answered");
        let nav = &wire[answered]["result"];
        assert!(nav["frameId"].is_string() && nav["loaderId"].is_string(), "Chrome's answer shape: {nav}");

        // 1. The answer precedes the navigation's frame and lifecycle events.
        for method in ["Page.frameNavigated", "Page.lifecycleEvent", "Page.loadEventFired",
                       "Runtime.executionContextCreated", "Page.frameStoppedLoading"] {
            let at = position(&|v| v.get("method").and_then(Value::as_str) == Some(method))
                .unwrap_or_else(|| panic!("{method} still arrives: {wire:?}"));
            assert!(answered < at, "Page.navigate must be answered before {method}: {wire:?}");
        }
        // ...and the document request's own events precede it, because the request was sent before
        // the document committed; every other request's follow it.
        let document_request = position(&|v| {
            v.get("method").and_then(Value::as_str) == Some("Network.requestWillBeSent")
                && v["params"]["requestId"] == nav["loaderId"]
        })
        .expect("the document request is on the wire");
        assert!(document_request < answered, "the document's requestWillBeSent precedes the answer: {wire:?}");

        // 2. An evaluate sent behind the navigation waits for the page's script, as Chrome's does.
        let evaluated = evaluated.unwrap();
        assert_eq!(evaluated["result"]["result"]["value"], "done", "{evaluated}");

        ws.send(Message::Close(None)).await.unwrap();
    };

    tokio::join!(server, client).0.unwrap();
}

/// A request a loading document makes reaches the client WHILE the document loads, as Chrome's do.
///
/// Every subresource's `Network.*` events used to go out in one batch after the load, so a client waiting for network
/// quiet saw the load as silence (costco.com: 1.9 s per load step). Here the server holds `held.js` -- a parser-blocking
/// script, so the load cannot finish without it -- until the client has seen that request's `requestWillBeSent`, or
/// 10 s have passed. Batched, the event could only come after the load, which waits on the script: the server would
/// time out and say so.
#[tokio::test]
async fn a_request_the_load_makes_reaches_the_client_while_the_load_runs() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    let seen = std::sync::Arc::new(tokio::sync::Notify::new());
    let released_by_client = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (seen_srv, released_srv) = (seen.clone(), released_by_client.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = page.accept().await else { return };
            let (seen, released) = (seen_srv.clone(), released_srv.clone());
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (ctype, body) = match path.as_str() {
                    "/" => ("text/html", "<!doctype html><html><head><title>t</title></head><body>\
                                         <script src=\"/held.js\"></script><p>after</p></body></html>"),
                    "/held.js" => {
                        if tokio::time::timeout(std::time::Duration::from_secs(10), seen.notified()).await.is_ok() {
                            released.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        ("text/javascript", "document.title = 'held';")
                    }
                    _ => ("text/plain", ""),
                };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let engine = Engine::new(Box::new(persona::CapturedPersona::default()));
    let server = cdp::serve_connection(cdp_listener, engine);

    let client = async {
        let mut ws = connect_cdp(cdp_addr).await;
        let mut log: Vec<Value> = Vec::new();
        call_log(&mut ws, 1, "Page.enable", json!({}), &mut log).await;
        call_log(&mut ws, 2, "Network.enable", json!({}), &mut log).await;
        let url = format!("http://{page_addr}/");
        ws.send(Message::Text(json!({ "id": 3, "method": "Page.navigate", "params": { "url": url } }).to_string()))
            .await
            .unwrap();
        let before = log.len();
        loop {
            let msg = ws.next().await.unwrap().unwrap();
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("method").and_then(Value::as_str) == Some("Network.requestWillBeSent")
                && v["params"]["request"]["url"].as_str().is_some_and(|u| u.ends_with("/held.js"))
            {
                seen.notify_one();
            }
            let loaded = v.get("method").and_then(Value::as_str) == Some("Page.loadEventFired");
            log.push(v);
            if loaded {
                break;
            }
        }
        let wire = &log[before..];
        let at = |pred: &dyn Fn(&Value) -> bool| wire.iter().position(pred);
        let held = at(&|v| {
            v.get("method").and_then(Value::as_str) == Some("Network.requestWillBeSent")
                && v["params"]["request"]["url"].as_str().is_some_and(|u| u.ends_with("/held.js"))
        })
        .expect("held.js was requested");
        let loaded = at(&|v| v.get("method").and_then(Value::as_str) == Some("Page.loadEventFired")).unwrap();
        assert!(held < loaded, "{wire:?}");
        assert!(
            released_by_client.load(std::sync::atomic::Ordering::SeqCst),
            "held.js's requestWillBeSent did not reach the client while the load waited on it: {wire:?}"
        );
        ws.send(Message::Close(None)).await.unwrap();
    };

    tokio::join!(server, client).0.unwrap();
}

/// The `params` of every event named `method`, in the order the server sent them.
fn events<'a>(log: &'a [Value], method: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|v| v.get("method").and_then(Value::as_str) == Some(method))
        .filter_map(|v| v.get("params"))
        .collect()
}

/// One HTTP GET against the CDP server's discovery endpoints.
async fn http_get(addr: std::net::SocketAddr, path: &str) -> (Value, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let body = text.split("\r\n\r\n").nth(1).expect("an HTTP body").to_string();
    (serde_json::from_str(&body).expect("json body"), body)
}

/// W9.16 — the CDP channel used to spell the engine's name in every identifier it answered:
/// `targetId "korvus-page-1"`, `sessionId "korvus-session-1"`, `browserContextId "korvus-ctx"`,
/// execution contexts `"korvus-ctx-1/-2/-3"`, loaders `"korvus-loader-0/-1"`, injected scripts
/// `"korvus-script-N"`, object ids `"obj-N"`, and a debugger URL ending `/devtools/browser/korvus`.
/// Anything that attaches over CDP — which is what Playwright does, and what an automation-detection
/// harness does — read the engine's name off the first message while the page realm reported zero
/// tells.
///
/// THIS TEST REPLACES `assert_eq!(nav["frameId"], "korvus-page-1")`. A test pinned to a constant
/// cannot survive the constant's deletion, and "is a string" would not be worth keeping. What is
/// worth keeping are the three invariants a dynamic id has to satisfy, and they are strictly
/// stronger than the assertion they replace:
///
/// 1. **WELL-FORMED FOR ITS KIND.** Not "random-looking": Chrome's identifiers are deliberately
///    NOT all the same shape, and a plausible id of the wrong shape is a worse tell than a
///    hardcoded string because it looks deliberate. Shapes measured from Chrome 150.0.7871.46, two
///    runs (see `src/ids.rs` for the table).
/// 2. **CORRELATED.** This is what actually breaks when an id becomes dynamic. Minting a fresh id
///    per connection is not enough — it has to be the SAME id everywhere a message pair requires
///    it: discovery → attach, result → event, navigation → its own lifecycle, object id → the
///    object it is handed back for. Each pair below is one a real client depends on
///    (a CDP client resolves `getTargets` → `attachToTarget` exactly this way,
///    and Playwright looks the frame up by target id).
/// 3. **NAMES NO ENGINE.** Checked over the FULL TEXT of every frame the server sent, not over a
///    list of fields someone remembered to enumerate.
#[tokio::test]
async fn cdp_identifiers_are_chrome_shaped_and_correlate() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = page.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let body = "<!doctype html><html><head><title>T</title></head><body></body></html>";
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.flush().await;
        }
    });

    // `serve` (not `serve_connection`): discovery and the session are SEPARATE CONNECTIONS to one
    // server, which is how Playwright attaches, and the only way to test that a
    // target id survives the handoff.
    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let server = cdp::serve(
        cdp_listener,
        || Engine::new(Box::new(persona::CapturedPersona::default())),
        std::sync::Arc::new(persona::CapturedPersona::default()),
    );

    let client = async {
        let mut wire = String::new();

        // ---- discovery, over HTTP, before any socket is opened --------------------------------
        let (ver, ver_body) = http_get(cdp_addr, "/json/version").await;
        wire.push_str(&ver_body);
        let ws_url = ver["webSocketDebuggerUrl"].as_str().expect("a debugger url").to_string();
        let browser_uuid = ws_url
            .rsplit_once("/devtools/browser/")
            .unwrap_or_else(|| panic!("the browser endpoint is /devtools/browser/<id>, got {ws_url}"))
            .1
            .to_string();
        uuid_v4_lower(&browser_uuid, "the /devtools/browser/<id> path");

        let (list, list_body) = http_get(cdp_addr, "/json/list").await;
        wire.push_str(&list_body);
        let advertised = hex32(&list[0]["id"], "/json/list target id");
        assert_eq!(
            list[0]["webSocketDebuggerUrl"].as_str().unwrap(),
            format!("ws://{cdp_addr}/devtools/page/{advertised}"),
            "a page's debugger url must address the target it advertises"
        );

        // ---- the session, over the url discovery just handed us -------------------------------
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap_or_else(|e| panic!("the advertised debugger url must be connectable: {e}"));
        let mut log: Vec<Value> = Vec::new();

        let targets = call_log(&mut ws, 1, "Target.getTargets", json!({}), &mut log).await;
        let info = &targets["targetInfos"][0];
        let target_id = hex32(&info["targetId"], "Target.getTargets targetId");
        let context_id = hex32(&info["browserContextId"], "browserContextId");
        // CORRELATION: discovery -> attach. A per-connection target id would break this, and it is
        // the first thing Playwright does.
        assert_eq!(target_id, advertised, "the target /json/list advertised must be the one the session reports");
        assert_ne!(context_id, target_id, "a browser context is not its target");

        call_log(&mut ws, 2, "Target.setAutoAttach", json!({ "autoAttach": true, "flatten": true }), &mut log).await;
        let attached = events(&log, "Target.attachedToTarget");
        assert_eq!(attached.len(), 1, "the page target is announced once: {attached:?}");
        let session_id = hex32(&attached[0]["sessionId"], "attachedToTarget sessionId");
        assert_eq!(
            hex32(&attached[0]["targetInfo"]["targetId"], "attachedToTarget targetId"),
            target_id,
            "the auto-attach event must announce THIS target"
        );
        // CORRELATION: the event's session and the call's session are one session.
        let attach = call_log(&mut ws, 3, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), &mut log).await;
        assert_eq!(
            attach["sessionId"].as_str(),
            Some(session_id.as_str()),
            "attachToTarget must hand back the session the client is already on, got {attach}"
        );
        assert_ne!(session_id, target_id, "a session is not its target");

        call_log(&mut ws, 4, "Page.enable", json!({}), &mut log).await;
        call_log(&mut ws, 5, "Runtime.enable", json!({}), &mut log).await;

        let tree = call_log(&mut ws, 6, "Page.getFrameTree", json!({}), &mut log).await;
        let frame = &tree["frameTree"]["frame"];
        let frame_id = hex32(&frame["id"], "frameTree frame id");
        let loader_0 = hex32(&frame["loaderId"], "the initial document's loaderId");
        // MEASURED, not preserved by default: real Chrome answers frameId == targetId for a page's
        // main frame, in both probe runs. Playwright depends on it (`frame(this._targetId)`).
        assert_eq!(frame_id, target_id, "Chrome: a page's main frame id IS its target id");
        // CORRELATION: Page.enable's frameNavigated describes the same frame and the same document.
        let pre_nav = events(&log, "Page.frameNavigated");
        assert_eq!(pre_nav.len(), 1, "one initial frameNavigated: {pre_nav:?}");
        assert_eq!(pre_nav[0]["frame"]["id"].as_str(), Some(frame_id.as_str()));
        assert_eq!(
            pre_nav[0]["frame"]["loaderId"].as_str(),
            Some(loader_0.as_str()),
            "the frame tree and the frameNavigated event must agree on the current document"
        );

        call_log(&mut ws, 7, "Runtime.runIfWaitingForDebugger", json!({}), &mut log).await;
        let ctx = events(&log, "Runtime.executionContextCreated");
        assert_eq!(ctx.len(), 1, "one default context so far: {ctx:?}");
        let ctx_1 = unique_id(&ctx[0]["context"]["uniqueId"], "the default context uniqueId");
        assert_eq!(
            ctx[0]["context"]["auxData"]["frameId"].as_str(),
            Some(frame_id.as_str()),
            "an execution context must name the frame it belongs to"
        );

        let world = call_log(&mut ws, 8, "Page.createIsolatedWorld",
            json!({ "frameId": frame_id, "worldName": "probe" }), &mut log).await;
        assert!(world["executionContextId"].is_number(), "{world}");
        let ctx = events(&log, "Runtime.executionContextCreated");
        let ctx_2 = unique_id(&ctx[1]["context"]["uniqueId"], "the isolated world uniqueId");
        assert_ne!(ctx_2, ctx_1, "two contexts, two unique ids");
        assert_eq!(ctx[1]["context"]["auxData"]["frameId"].as_str(), Some(frame_id.as_str()));

        let script = call_log(&mut ws, 9, "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "globalThis.__w916 = 1;" }), &mut log).await;
        let ident = script["identifier"].as_str().expect("an identifier");
        // Chrome answers a plain decimal counter from "1" (measured), not a named handle.
        ident.parse::<u64>().unwrap_or_else(|_| panic!("script identifier is decimal in Chrome, got {ident:?}"));

        // ---- a navigation, which is a NEW document ---------------------------------------------
        let before = log.len();
        let nav = call_log(&mut ws, 10, "Page.navigate",
            json!({ "url": format!("http://{page_addr}/") }), &mut log).await;
        // The answer comes at COMMIT and the navigation's events after it, as in Chrome (P9b; see
        // `page_navigate_answers_at_commit_and_the_page_loads_after`), so read them off the wire.
        drain_until(&mut ws, "Page.frameStoppedLoading", &mut log).await;
        let loader_1 = hex32(&nav["loaderId"], "Page.navigate loaderId");
        assert_eq!(nav["frameId"].as_str(), Some(frame_id.as_str()), "navigation stays in this frame");
        assert_ne!(loader_1, loader_0, "Chrome mints a loaderId per navigation");

        // CORRELATION, and the one that matters most: every event of THIS navigation carries THIS
        // navigation's loader. A client that cannot pair an event with the navigation that caused
        // it cannot tell a stale lifecycle event from a current one.
        let during: Vec<Value> = log[before..].to_vec();
        let navigated = events(&during, "Page.frameNavigated");
        assert_eq!(navigated.len(), 1, "one frameNavigated for one navigation: {navigated:?}");
        assert_eq!(navigated[0]["frame"]["id"].as_str(), Some(frame_id.as_str()));
        assert_eq!(
            navigated[0]["frame"]["loaderId"].as_str(),
            Some(loader_1.as_str()),
            "frameNavigated must carry the loader Page.navigate answered"
        );
        let lifecycle = events(&during, "Page.lifecycleEvent");
        assert!(lifecycle.len() >= 3, "init/DOMContentLoaded/load at least: {lifecycle:?}");
        for ev in &lifecycle {
            assert_eq!(ev["frameId"].as_str(), Some(frame_id.as_str()), "lifecycle event frame: {ev}");
            assert_eq!(
                ev["loaderId"].as_str(),
                Some(loader_1.as_str()),
                "every lifecycle event of this navigation must carry its loader: {ev}"
            );
        }
        let stopped = events(&during, "Page.frameStoppedLoading");
        assert_eq!(stopped[0]["frameId"].as_str(), Some(frame_id.as_str()));
        let new_ctx = events(&during, "Runtime.executionContextCreated");
        let ctx_3 = unique_id(&new_ctx[0]["context"]["uniqueId"], "the post-navigation context uniqueId");
        assert!(ctx_3 != ctx_1 && ctx_3 != ctx_2, "the new document gets its own context id");
        assert_eq!(new_ctx[0]["context"]["auxData"]["frameId"].as_str(), Some(frame_id.as_str()));

        // The frame tree now reports the CURRENT document, not the one Page.enable announced.
        let tree = call_log(&mut ws, 11, "Page.getFrameTree", json!({}), &mut log).await;
        assert_eq!(
            tree["frameTree"]["frame"]["loaderId"].as_str(),
            Some(loader_1.as_str()),
            "getFrameTree must report the loader of the document actually in the frame, got {tree}"
        );

        // CORRELATION: an object id the server issues must be the object id it parses back. This is
        // the round trip that a reshaped `objectId` could silently break — the value proves the
        // handle resolved to the same object.
        let obj = call_log(&mut ws, 12, "Runtime.evaluate",
            json!({ "expression": "({ a: 41 + 1 })" }), &mut log).await;
        let object_id = obj["result"]["objectId"].as_str().expect("an objectId").to_string();
        let parts: Vec<&str> = object_id.split('.').collect();
        assert_eq!(parts.len(), 3, "Chrome's objectId is <isolate>.<context>.<serial>, got {object_id:?}");
        for p in &parts {
            p.parse::<i64>().unwrap_or_else(|_| panic!("objectId component {p:?} is decimal in Chrome: {object_id:?}"));
        }
        let back = call_log(&mut ws, 13, "Runtime.callFunctionOn",
            json!({ "objectId": object_id, "functionDeclaration": "function () { return this.a; }",
                    "returnByValue": true }), &mut log).await;
        assert_eq!(
            back["result"]["value"], 42,
            "the objectId the server issued must resolve back to the object it named: {back}"
        );

        for frame in &log {
            wire.push_str(&frame.to_string());
        }
        ws.send(Message::Close(None)).await.unwrap();
        drop(ws);

        // ---- a SECOND connection to the SAME server --------------------------------------------
        // The target and its browser context belong to the browser and must not move; the session
        // belongs to the attach and must. That asymmetry is Chrome's (measured: two attaches to one
        // target got two different session ids in one run), and it is what stops "make it dynamic"
        // from becoming "make it random everywhere".
        let (mut ws2, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        let mut log2: Vec<Value> = Vec::new();
        let targets2 = call_log(&mut ws2, 1, "Target.getTargets", json!({}), &mut log2).await;
        assert_eq!(
            targets2["targetInfos"][0]["targetId"].as_str(),
            Some(target_id.as_str()),
            "a target keeps its id across connections, or discovery cannot be followed by an attach"
        );
        assert_eq!(
            targets2["targetInfos"][0]["browserContextId"].as_str(),
            Some(context_id.as_str()),
            "the browser context belongs to the browser, not to the connection"
        );
        call_log(&mut ws2, 2, "Target.setAutoAttach", json!({ "autoAttach": true, "flatten": true }), &mut log2).await;
        let session_2 = hex32(&events(&log2, "Target.attachedToTarget")[0]["sessionId"], "second sessionId");
        assert_ne!(session_2, session_id, "Chrome mints a session per attach");
        for frame in &log2 {
            wire.push_str(&frame.to_string());
        }
        ws2.send(Message::Close(None)).await.unwrap();

        // ---- and nothing anywhere on that wire names the engine --------------------------------
        // Over the FULL TEXT of both discovery bodies and every frame either session received, so a
        // field nobody thought to enumerate cannot hide one.
        let lower = wire.to_lowercase();
        assert!(
            !lower.contains("korvus"),
            "the CDP wire names the engine; first hit at byte {:?}",
            lower.find("korvus").map(|i| wire[i.saturating_sub(120)..(i + 120).min(wire.len())].to_string())
        );
    };

    tokio::select! {
        r = server => { r.unwrap(); }
        _ = client => {}
    }
}

/// THE REALM A NAVIGATION LOADS INTO, in both directions.
///
/// `cdp::serve` builds a connection's `Engine` on first use, so when `Page.navigate` is the first
/// command to ask for one, the document loads in the realm that navigation just built instead of in
/// a successor to a realm nobody touched. That is only sound while "nobody touched it" is true, and
/// this asserts both halves against one server:
///
///   * a connection whose first engine-touching command is `Page.navigate` gets the page -- title,
///     content and `document.readyState` -- so the reused realm really does load a document; and
///   * a connection that evaluates FIRST gets a successor as before, so a global written into the
///     initial `about:blank` is gone by the time the page's own scripts run. A realm reused after a
///     script had run in it would carry that global into the document, which is the defect the
///     successor exists to prevent and the one this branch must not reintroduce.
///
/// `Page.enable` and `Runtime.enable` are sent before the navigation in the first half on purpose:
/// they are what every client sends, they answer from the connection's ids, and a build triggered
/// by either would silently take the first half back to the successor path and leave it asserting
/// nothing new.
#[tokio::test]
async fn a_navigation_that_builds_the_realm_loads_into_it_and_one_that_does_not_gets_a_successor() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let page = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page_addr = page.local_addr().unwrap();
    // Two navigations, so the fixture answers twice.
    tokio::spawn(async move {
        loop {
            let (mut s, _) = page.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let body = "<!doctype html><html><head><title>FirstDoc</title></head>\
<body><div id=\"x\">loaded here</div></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.flush().await;
        }
    });

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    // `serve`, not `serve_connection`: only the server builds its engines on demand. A test helper
    // is handed one already built, which is the successor path by construction.
    let server = cdp::serve(
        cdp_listener,
        || Engine::new(Box::new(persona::CapturedPersona::default())),
        std::sync::Arc::new(persona::CapturedPersona::default()),
    );
    let url = format!("http://{page_addr}/");

    let client = async {
        // ---- half one: the navigation builds the realm it loads into ---------------------------
        let mut ws = connect_cdp(cdp_addr).await;
        call(&mut ws, 1, "Page.enable", json!({})).await;
        call(&mut ws, 2, "Runtime.enable", json!({})).await;
        let nav = call(&mut ws, 3, "Page.navigate", json!({ "url": url })).await;
        assert!(nav["frameId"].is_string(), "Page.navigate must commit a frame, got {nav}");
        let title = call(&mut ws, 4, "Runtime.evaluate", json!({ "expression": "document.title" })).await;
        assert_eq!(title["result"]["value"], "FirstDoc", "the reused realm must hold the document");
        let text = call(&mut ws, 5, "Runtime.evaluate",
                        json!({ "expression": "document.getElementById('x').textContent" })).await;
        assert_eq!(text["result"]["value"], "loaded here");
        let ready = call(&mut ws, 6, "Runtime.evaluate", json!({ "expression": "document.readyState" })).await;
        assert_eq!(ready["result"]["value"], "complete", "the reused realm must reach the load event");
        ws.send(Message::Close(None)).await.unwrap();
        drop(ws);

        // ---- half two: a script ran first, so the document gets a new realm --------------------
        let mut ws2 = connect_cdp(cdp_addr).await;
        call(&mut ws2, 1, "Page.enable", json!({})).await;
        let set = call(&mut ws2, 2, "Runtime.evaluate",
                       json!({ "expression": "globalThis.__beforeNav = 'from about:blank'; 1" })).await;
        assert_eq!(set["result"]["value"], 1, "the pre-navigation evaluate must run, got {set}");
        call(&mut ws2, 3, "Page.navigate", json!({ "url": url })).await;
        let title2 = call(&mut ws2, 4, "Runtime.evaluate", json!({ "expression": "document.title" })).await;
        assert_eq!(title2["result"]["value"], "FirstDoc");
        let leaked = call(&mut ws2, 5, "Runtime.evaluate",
                          json!({ "expression": "typeof globalThis.__beforeNav" })).await;
        assert_eq!(
            leaked["result"]["value"], "undefined",
            "a cross-document navigation loads in a NEW realm: the global a script wrote into the \
             initial about:blank must not survive it, got {leaked}"
        );
        ws2.send(Message::Close(None)).await.unwrap();
    };

    tokio::select! {
        r = server => { r.unwrap(); }
        _ = client => {}
    }
}

/// A dynamic id must not be a new hardcoded constant: two server processes must not agree on one.
#[tokio::test]
async fn two_servers_do_not_share_an_identity() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    async fn identity_of() -> (String, String, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
        let client = async {
            let mut ws = connect_cdp(addr).await;
            let mut log = Vec::new();
            let t = call_log(&mut ws, 1, "Target.getTargets", json!({}), &mut log).await;
            call_log(&mut ws, 2, "Target.setAutoAttach", json!({ "autoAttach": true, "flatten": true }), &mut log).await;
            let s = events(&log, "Target.attachedToTarget")[0]["sessionId"].as_str().unwrap().to_string();
            let tree = call_log(&mut ws, 3, "Page.getFrameTree", json!({}), &mut log).await;
            let out = (
                t["targetInfos"][0]["targetId"].as_str().unwrap().to_string(),
                s,
                tree["frameTree"]["frame"]["loaderId"].as_str().unwrap().to_string(),
            );
            ws.send(Message::Close(None)).await.unwrap();
            out
        };
        tokio::join!(server, client).1
    }
    let a = identity_of().await;
    let b = identity_of().await;
    assert_ne!(a.0, b.0, "two servers share a targetId — it is a constant again");
    assert_ne!(a.1, b.1, "two servers share a sessionId — it is a constant again");
    assert_ne!(a.2, b.2, "two servers share a loaderId — it is a constant again");
}

// ---------------------------------------------------------------------------------------------
// W9.22 — a throwing Runtime.evaluate is a RESULT, not a protocol error.
// ---------------------------------------------------------------------------------------------

/// Chrome answers a throw with `{result, exceptionDetails}`; this server answered `-32000
/// "execute_script failed: Error: boom\n    at cdp:Runtime.evaluate:1:7"`. The assertions below are
/// Chrome's measured answers (`--headless=new --remote-debugging-port=0`, 2026-09-12, Chrome
/// 150.0.7871.46) — the full probe table is on `engine::EvalOutcome`.
#[tokio::test]
async fn a_throwing_evaluate_answers_chromes_exception_details() {
    // The request ledger is process-global; every navigating test in this binary takes turns.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut ws = connect_cdp(addr).await;
        // The call frames below are a client's with the Runtime domain enabled, as the measuring client was: Chrome
        // captures an exception's stack only for one (Chrome 152, measured: no `stackTrace` before `Runtime.enable`,
        // none after `Runtime.disable` -- checked at the end).
        call_full(&mut ws, 100, "Runtime.enable", json!({})).await;

        // ---- a synchronous throw of an Error ---------------------------------------------------
        let r = call_full(&mut ws, 1, "Runtime.evaluate", json!({ "expression": "throw new Error('boom')" })).await;
        assert!(r.get("error").is_none(), "a page throw is a RESULT in Chrome, not an error: {r}");
        let res = r["result"]["result"].clone();
        assert_eq!(res["type"], "object", "{r}");
        assert_eq!(res["subtype"], "error", "{r}");
        assert_eq!(res["className"], "Error", "{r}");
        let desc = res["description"].as_str().unwrap_or_default().to_string();
        assert!(desc.starts_with("Error: boom"), "description is the Error's own stack: {r}");
        assert!(res["objectId"].is_string(), "Chrome hands back an objectId for the error: {r}");

        // THE LEAK THIS CLOSES. The script this server evaluates has no name, so V8 renders it
        // `<anonymous>` exactly as Chrome does — not `cdp:Runtime.evaluate`.
        assert!(
            desc.contains("at <anonymous>:1:7"),
            "the stack must name the script the way Chrome's does: {desc:?}"
        );
        let whole = r.to_string();
        assert!(
            !whole.to_lowercase().contains("korvus") && !whole.contains("cdp:"),
            "the evaluate answer names the engine or an internal script: {r}"
        );

        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(d["exceptionId"], 1, "Chrome's exceptionId counts from 1 per session: {r}");
        assert_eq!(d["text"], "Uncaught", "Chrome's text for a synchronous throw: {r}");
        assert_eq!(d["lineNumber"], 0, "{r}");
        assert_eq!(d["columnNumber"], 0, "the THROW site, 0-based: {r}");
        assert!(d.get("url").is_none(), "Chrome's exceptionDetails carries NO url key: {r}");
        // The thrown value appears TWICE, under two different ids.
        assert_eq!(d["exception"]["subtype"], "error", "{r}");
        assert_eq!(d["exception"]["description"], json!(desc), "{r}");
        assert_ne!(
            d["exception"]["objectId"], res["objectId"],
            "Chrome issues a SECOND id for exceptionDetails.exception: {r}"
        );
        let frames = d["stackTrace"]["callFrames"].as_array().expect("call frames").clone();
        assert!(!frames.is_empty(), "{r}");
        assert_eq!(frames[0]["url"], "", "a call frame's url is the EMPTY STRING, not <anonymous>: {r}");
        assert_eq!(frames[0]["functionName"], "", "{r}");
        assert_eq!(frames[0]["lineNumber"], 0, "{r}");
        assert_eq!(frames[0]["columnNumber"], 6, "the `new Error` site, 0-based: {r}");
        assert_eq!(d["scriptId"], frames[0]["scriptId"], "Chrome's scriptId is the top frame's: {r}");

        // ---- returnByValue does NOT apply to a synchronous throw (measured) ---------------------
        let r = call_full(&mut ws, 2, "Runtime.evaluate",
            json!({ "expression": "throw new Error('boom')", "returnByValue": true })).await;
        assert!(
            r["result"]["result"]["objectId"].is_string(),
            "Chrome still answers the error OBJECT under returnByValue: {r}"
        );
        assert_eq!(r["result"]["exceptionDetails"]["exceptionId"], 2, "the counter advances: {r}");

        // ---- a thrown non-Error is a plain by-value RemoteObject --------------------------------
        let r = call_full(&mut ws, 3, "Runtime.evaluate", json!({ "expression": "throw 'plainstring'" })).await;
        assert_eq!(r["result"]["result"], json!({ "type": "string", "value": "plainstring" }), "{r}");
        assert_eq!(r["result"]["exceptionDetails"]["text"], "Uncaught", "still bare `Uncaught`: {r}");
        assert_eq!(
            r["result"]["exceptionDetails"]["exception"],
            json!({ "type": "string", "value": "plainstring" }),
            "{r}"
        );

        // ---- a nested throw: several frames, and details != the top frame -----------------------
        let expr = "(function outer(){ function inner(){ throw new TypeError('deep') } inner() })()";
        let r = call_full(&mut ws, 4, "Runtime.evaluate", json!({ "expression": expr })).await;
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(r["result"]["result"]["className"], "TypeError", "{r}");
        let frames = d["stackTrace"]["callFrames"].as_array().expect("call frames").clone();
        let names: Vec<&str> = frames.iter().map(|f| f["functionName"].as_str().unwrap_or("")).collect();
        assert_eq!(names, vec!["inner", "outer", ""], "Chrome's frames, innermost first: {r}");
        assert_eq!(d["columnNumber"], 37, "the `throw`, not the `new TypeError`: {r}");
        assert_eq!(frames[0]["columnNumber"], 43, "the top frame IS the `new TypeError`: {r}");

        // ---- a SyntaxError has no stackTrace at all (measured) ----------------------------------
        let r = call_full(&mut ws, 5, "Runtime.evaluate", json!({ "expression": "this is not ( valid js" })).await;
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(r["result"]["result"]["className"], "SyntaxError", "{r}");
        assert!(d.get("stackTrace").is_none(), "Chrome omits stackTrace for a SyntaxError: {r}");
        assert_eq!(d["columnNumber"], 5, "{r}");

        // ---- a rejected promise under awaitPromise ----------------------------------------------
        let r = call_full(&mut ws, 6, "Runtime.evaluate",
            json!({ "expression": "Promise.reject(new Error('rejected!'))", "awaitPromise": true })).await;
        assert!(r.get("error").is_none(), "a rejection is a RESULT in Chrome too: {r}");
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(
            d["text"], "Uncaught (in promise) Error: rejected!",
            "Chrome's rejection text carries the error: {r}"
        );
        assert_eq!(r["result"]["result"]["subtype"], "error", "{r}");
        let frames = d["stackTrace"]["callFrames"].as_array().expect("call frames").clone();
        assert_eq!(frames[0]["columnNumber"], 15, "the `new Error` inside Promise.reject: {r}");
        // The measured Chrome quirk: a rejection's details are the top frame PLUS ONE on both axes.
        assert_eq!(d["lineNumber"], 1, "{r}");
        assert_eq!(d["columnNumber"], 16, "{r}");

        // ---- a rejection of a non-Error: bare text, and no stack ---------------------------------
        let r = call_full(&mut ws, 7, "Runtime.evaluate",
            json!({ "expression": "Promise.reject('plain')", "awaitPromise": true })).await;
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(d["text"], "Uncaught (in promise)", "bare, with no error to name: {r}");
        assert!(d.get("stackTrace").is_none(), "no captured stack on a plain value: {r}");
        assert_eq!(r["result"]["result"], json!({ "type": "string", "value": "plain" }), "{r}");

        // ---- and none of this broke the ordinary paths ------------------------------------------
        let r = call_full(&mut ws, 8, "Runtime.evaluate", json!({ "expression": "1 + 2 * 3" })).await;
        assert_eq!(r["result"]["result"]["value"], 7, "{r}");
        assert!(r["result"].get("exceptionDetails").is_none(), "no exception on a clean evaluate: {r}");
        let r = call_full(&mut ws, 9, "Runtime.evaluate",
            json!({ "expression": "Promise.resolve(7)", "awaitPromise": true })).await;
        assert_eq!(r["result"]["result"]["value"], 7, "a resolved promise still settles: {r}");
        let r = call_full(&mut ws, 10, "Runtime.evaluate",
            json!({ "expression": "42", "awaitPromise": true })).await;
        assert_eq!(r["result"]["result"]["value"], 42, "awaitPromise on a non-promise: {r}");

        // ---- a //# sourceURL still overrides the empty name, as it does in Chrome ----------------
        //
        // NOT asserted here, because it is not this layer's: Chrome's `description` for this case
        // reads `"Error: boom\n    at custom-name.js:1:7"` and this engine's reads `<anonymous>`.
        // `description` IS the Error's own `stack`, and the difference is in how this build's V8
        // formats a stack for a script carrying a `//# sourceURL` — the PAGE REALM has it too, with
        // no CDP involved: `(0,eval)("throw new Error('e')\n//# sourceURL=zz.js")` gives Chrome
        // `at eval (zz.js:1:7)` and this engine `at eval (zz.js, <anonymous>:1:7)`. Measured both
        // ways, 2026-09-12. It belongs to the Error.stack work (W9.21), not to this crate.
        let r = call_full(&mut ws, 11, "Runtime.evaluate",
            json!({ "expression": "throw new Error('boom')\n//# sourceURL=custom-name.js" })).await;
        let frames = r["result"]["exceptionDetails"]["stackTrace"]["callFrames"].as_array().unwrap();
        assert_eq!(frames[0]["url"], "custom-name.js", "{r}");

        // ---- Runtime.callFunctionOn, which is what `page.evaluate` REALLY is ---------------------
        // It answered "function call returned nothing / threw" -- a protocol error naming nothing.
        let r = call_full(&mut ws, 12, "Runtime.callFunctionOn",
            json!({ "functionDeclaration": "function(){ throw new Error('cfo boom') }" })).await;
        assert!(r.get("error").is_none(), "a throwing callFunctionOn is a RESULT in Chrome: {r}");
        assert_eq!(r["result"]["result"]["subtype"], "error", "{r}");
        assert_eq!(r["result"]["result"]["className"], "Error", "{r}");
        assert!(
            r["result"]["result"]["description"].as_str().unwrap_or_default().starts_with("Error: cfo boom"),
            "{r}"
        );
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(d["text"], "Uncaught", "{r}");
        let frames = d["stackTrace"]["callFrames"].as_array().expect("call frames").clone();
        assert_eq!(frames[0]["url"], "", "an unnamed function's frame has an empty url: {r}");
        // …and a callFunctionOn that does NOT throw is untouched.
        let r = call_full(&mut ws, 13, "Runtime.callFunctionOn",
            json!({ "functionDeclaration": "function(){ return 5 }", "returnByValue": true })).await;
        assert_eq!(r["result"]["result"]["value"], 5, "{r}");
        assert!(r["result"].get("exceptionDetails").is_none(), "{r}");

        // ---- and with the Runtime domain disabled, a throw carries no stack, as in Chrome -----------------------
        call_full(&mut ws, 101, "Runtime.disable", json!({})).await;
        let r = call_full(&mut ws, 14, "Runtime.evaluate", json!({ "expression": "(function f(){ throw new Error('boom') })()" })).await;
        let d = r["result"]["exceptionDetails"].clone();
        assert_eq!(d["text"], "Uncaught", "{r}");
        assert!(d.get("stackTrace").is_none(), "no stack is captured without the Runtime domain: {r}");

        ws.send(Message::Close(None)).await.unwrap();
    };
    tokio::join!(server, client).0.unwrap();
}

/// `Runtime.evaluate`'s `timeout`, as stock Chrome 150.0.7871.46 keeps it (`--headless=new`, 2026-10-28): the script's
/// own run is terminated when it falls due -- `while(true){}` under `timeout: 300` is answered `-32000 "Execution was
/// terminated"` at 0.30 s, and the page answers the next evaluation as ever -- and an awaited promise is not cut by it:
/// one that settles at 1 s under `timeout: 300` is answered `done` at 1 s. It was ignored, and a script that never
/// yielded held the page forever.
#[tokio::test]
async fn an_evaluate_timeout_ends_the_script_and_not_its_await() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut ws = connect_cdp(addr).await;
        let t = std::time::Instant::now();
        let r = call_full(&mut ws, 1, "Runtime.evaluate", json!({ "expression": "while(true){}", "timeout": 300 })).await;
        assert_eq!(r["error"]["code"], -32000, "{r}");
        assert_eq!(r["error"]["message"], "Execution was terminated", "{r}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "terminated at its timeout, not later: {:?}", t.elapsed());
        let r = call(&mut ws, 2, "Runtime.evaluate", json!({ "expression": "1+1", "returnByValue": true })).await;
        assert_eq!(r["result"]["value"], 2, "the page runs on after a terminated evaluation: {r}");
        let r = call_full(&mut ws, 3, "Runtime.evaluate", json!({
            "expression": "new Promise(r=>setTimeout(()=>r('done'),1000))", "awaitPromise": true, "timeout": 300, "returnByValue": true,
        })).await;
        assert_eq!(r["result"]["result"]["value"], "done", "an await outlives a shorter timeout: {r}");
        let r = call_full(&mut ws, 4, "Runtime.evaluate", json!({ "expression": "'fast'", "timeout": 300, "returnByValue": true })).await;
        assert_eq!(r["result"]["result"]["value"], "fast", "a run within its timeout is answered as ever: {r}");
        ws.send(Message::Close(None)).await.unwrap();
    };
    tokio::join!(server, client).0.unwrap();
}

/// `Korvus.getIdentity`: what the page's requests wear, for a client that replays a minted session without the browser
/// -- the persona's headers as a navigation sends them, in their order, with a `Network.setUserAgentOverride` in them,
/// the high-entropy hints, the TLS preset, and the context's cookies.
#[tokio::test]
async fn the_identity_a_page_wears_is_given_to_its_client() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async {
        let mut ws = connect_cdp(addr).await;
        let ua = call(&mut ws, 1, "Runtime.evaluate", json!({ "expression": "navigator.userAgent", "returnByValue": true })).await;
        let ua = ua["result"]["value"].as_str().unwrap().to_string();
        call(&mut ws, 2, "Network.setCookie", json!({ "name": "minted", "value": "tok", "url": "https://example.com/" })).await;
        let id = call(&mut ws, 3, "Korvus.getIdentity", json!({})).await;
        let headers: Vec<(String, String)> =
            id["headers"].as_array().unwrap().iter().map(|h| (h["name"].as_str().unwrap().into(), h["value"].as_str().unwrap().into())).collect();
        let at = |n: &str| headers.iter().position(|(k, _)| k == n).unwrap_or_else(|| panic!("{n} in {headers:?}"));
        assert_eq!(headers[at("user-agent")].1, ua, "the UA the page reports");
        assert!(at("sec-ch-ua") < at("user-agent") && at("user-agent") < at("accept-language"), "a navigation's order: {headers:?}");
        assert!(id["clientHints"]["sec-ch-ua-full-version-list"].is_string(), "{id}");
        assert!(!id["tls"]["emulation"].as_str().unwrap().is_empty(), "{id}");
        assert!(id["cookies"].as_array().unwrap().iter().any(|c| c["name"] == "minted" && c["value"] == "tok"), "{id}");
        call(&mut ws, 4, "Network.setUserAgentOverride", json!({ "userAgent": "Replayed/1.0" })).await;
        let id = call(&mut ws, 5, "Korvus.getIdentity", json!({})).await;
        assert!(id["headers"].as_array().unwrap().iter().any(|h| h["name"] == "user-agent" && h["value"] == "Replayed/1.0"), "{id}");
        ws.send(Message::Close(None)).await.unwrap();
    };
    tokio::join!(server, client).0.unwrap();
}

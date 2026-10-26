//! N27: a redirect chain over CDP `Network`, as Chrome 150 reports it.
//!
//! The request log kept a redirect chain as ONE row -- the first url, the final response's headers -- and this server
//! reported it as one `requestWillBeSent` and one `responseReceived`, so no client ever saw a redirect response. The
//! a bot sensor-marriott investigation read that row's `url` for a whole day and scored a passing challenge as a failure.
//! And `responseReceived` kept the last of a repeated header (one `Set-Cookie` of two, `X-Dup: two`).
//!
//! `fixtures/redirect-hops.chrome-150.json` is stock Chrome 150.0.7871.46's answer (headed, Xvfb, fresh profile, one
//! raw CDP client: `Network.enable`, `Page.navigate /nav-r1`, three runs identical):
//! `/nav-r1` 302 -> `/nav-r2` 301 -> `/landing` 200, whose script `fetch()`es `/f-r1` (307 -> `/f-r2`) and `/dup`; every
//! response carries two `Set-Cookie` and two `X-Dup` lines. Per request: a `requestWillBeSent` per hop, each hop after
//! the first with the previous hop's `redirectResponse`; one `responseReceived`; `Set-Cookie` in none of them; `X-Dup`
//! joined with `"\n"` for the navigation and `", "` for a `fetch()`.
//!
//! KNOWN, not graded: a header NAME's case. Chrome reports an HTTP/1.1 response's names as the server wrote them
//! (`Cache-Control`); the transport's client hands back lowercase names and keeps no record of the wire's case, so the
//! names are compared case-insensitively. And `Date`, which is the clock's.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const CHROME: &str = include_str!("fixtures/redirect-hops.chrome-150.json");
const LANDING: &[u8] = include_bytes!("fixtures/redirect-hops-landing.html");

/// `rec_cdp.py`'s fixture server, line for line: Python's `BaseHTTPRequestHandler` writes `Server` and `Date` first,
/// then the handler's headers in order, then `Content-Type`, `Cache-Control`, `Content-Length`.
fn serve_fixture() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                    }
                    let tag = path.trim_matches('/').replace('-', "");
                    let dup = [
                        ("Set-Cookie", format!("a{tag}=1; Path=/")),
                        ("Set-Cookie", format!("b{tag}=2; Path=/")),
                        ("X-Dup", "one".to_string()),
                        ("X-Dup", "two".to_string()),
                    ];
                    let (status, location, body, ctype): (&str, Option<&str>, &[u8], &str) = match path.as_str() {
                        "/nav-r1" => ("302 Found", Some("/nav-r2"), b"", "text/plain"),
                        "/nav-r2" => ("301 Moved Permanently", Some("/landing"), b"", "text/plain"),
                        "/landing" => ("200 OK", None, LANDING, "text/html; charset=utf-8"),
                        "/f-r1" => ("307 Temporary Redirect", Some("/f-r2"), b"", "text/plain"),
                        "/f-r2" => ("200 OK", None, b"f-r2 body", "text/plain"),
                        "/dup" => ("200 OK", None, b"dup body", "text/plain"),
                        _ => ("404 Not Found", None, b"", "text/plain"),
                    };
                    let mut head = format!("HTTP/1.1 {status}\r\nServer: BaseHTTP/0.6 Python/3.12.3\r\nDate: Sun, 11 Oct 2026 12:18:57 GMT\r\n");
                    if let Some(l) = location {
                        head.push_str(&format!("Location: {l}\r\n"));
                    }
                    if !status.starts_with("404") {
                        for (k, v) in &dup {
                            head.push_str(&format!("{k}: {v}\r\n"));
                        }
                    }
                    head.push_str(&format!("Content-Type: {ctype}\r\nCache-Control: no-store\r\nContent-Length: {}\r\n\r\n", body.len()));
                    if w.write_all(head.as_bytes()).and_then(|_| w.write_all(body)).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// A header object with every name lowercased and `Date` dropped (see the module docs).
fn names_folded(h: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in h.as_object().cloned().unwrap_or_default() {
        if !k.eq_ignore_ascii_case("date") {
            m.insert(k.to_ascii_lowercase(), v);
        }
    }
    Value::Object(m)
}

/// One event as the fixture keeps it: the path, not the port.
fn row(ev: &Value, port: u16) -> Option<Value> {
    let p = &ev["params"];
    let path = |u: &Value| json!(u.as_str().unwrap_or("").replace(&format!("http://127.0.0.1:{port}"), ""));
    match ev["method"].as_str()? {
        "Network.requestWillBeSent" => {
            let mut r = json!({ "event": "requestWillBeSent", "url": path(&p["request"]["url"]), "method": p["request"]["method"],
                "type": p["type"], "documentURL": path(&p["documentURL"]) });
            if let Some(rr) = p.get("redirectResponse") {
                r["redirectResponse"] = json!({ "url": path(&rr["url"]), "status": rr["status"], "statusText": rr["statusText"],
                    "headers": names_folded(&rr["headers"]), "mimeType": rr["mimeType"] });
            }
            Some(r)
        }
        "Network.responseReceived" => {
            let r = &p["response"];
            Some(json!({ "event": "responseReceived", "url": path(&r["url"]), "status": r["status"], "statusText": r["statusText"],
                "headers": names_folded(&r["headers"]), "mimeType": r["mimeType"], "type": p["type"] }))
        }
        "Network.loadingFinished" => Some(json!({ "event": "loadingFinished" })),
        _ => None,
    }
}

#[tokio::test]
async fn a_redirect_chain_is_reported_hop_by_hop_as_chromes() {
    // The request ledger is process-global: a navigating test takes its turn (see `navigation::ONE_AT_A_TIME`).
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    let mut want = serde_json::Map::new();
    for (first, rows) in chrome["requests"].as_object().expect("requests") {
        let rows: Vec<Value> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if let Some(h) = r.get("headers").cloned() {
                    r["headers"] = names_folded(&h);
                }
                if let Some(h) = r.pointer("/redirectResponse/headers").cloned() {
                    r["redirectResponse"]["headers"] = names_folded(&h);
                }
                r
            })
            .collect();
        want.insert(first.clone(), json!(rows));
    }

    let port = serve_fixture();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap();
        let mut events: Vec<Value> = Vec::new();
        for (id, method, params) in [(1, "Network.enable", json!({})), (2, "Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/nav-r1") }))] {
            ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        }
        // Until every request of the recording has finished (three `loadingFinished`), or 15 s.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && events.iter().filter(|e| e["method"] == "Network.loadingFinished").count() < 3 {
            if let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("method").is_some() {
                    events.push(v);
                }
            }
        }
        ws.send(Message::Close(None)).await.unwrap();
        events
    };
    let (server, events) = tokio::join!(server, client);
    server.unwrap();

    // Group by requestId, keyed by the first url, as the fixture is.
    let mut first: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut got = serde_json::Map::new();
    for ev in &events {
        let Some(id) = ev["params"]["requestId"].as_str() else { continue };
        let Some(r) = row(ev, port) else { continue };
        if ev["method"] == "Network.requestWillBeSent" && !first.contains_key(id) {
            first.insert(id.to_string(), r["url"].as_str().unwrap().to_string());
        }
        let Some(key) = first.get(id) else { continue };
        got.entry(key.clone()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(r);
    }
    for (k, rows) in &want {
        assert_eq!(got.get(k), Some(rows), "request {k}: korvus's Network.* rows vs Chrome 150's");
    }
    assert_eq!(got.len(), want.len(), "requests Chrome 150 did not make: {:?}", got.keys().collect::<Vec<_>>());
}

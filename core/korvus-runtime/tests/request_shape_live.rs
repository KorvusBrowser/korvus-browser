//! The shape of every request a page's loads put on the wire, as Chrome 150 puts it:
//! the credentials a load carries, its mode and `Origin` and where `Origin` sits, and its HTTP/2 `priority`.
//! a bot challenge loads its scripts by inserting them (`chl_page`, a `crossorigin` `api.js` from
//! another origin) and the page it guards is full of the rest.
//!
//! `fixtures/request-shape/request-shape.html` is served by the server below and, for Chrome, by
//! `fixtures/request-shape/server.js` (HTTP/2 over TLS, the same answers), both on two ports of `localhost`
//! -- one site, two origins -- with `{{B}}` standing for the second origin. The navigation starts at
//! `/rs/set`, which sets `rs7=1` for `/rs` and redirects to the page; `/rs/f-setz` answers `Set-Cookie:
//! rsz=1`. Each server records every request's header names in wire order, its `Sec-Fetch-Mode`, whether it
//! carries an `Origin`, `rs7` and `rsz`, and (Chrome's) its `priority` field.
//! `request-shape.chrome-150.json` is google-chrome 150.0.7871.46's record, headed under Xvfb: five runs, three
//! complete and identical in every field; the other two lost requests (six, one) that never reached the server
//! before `/rs/done`, a fault of the h2 rig and not a choice the browser made.
//!
//! korvus is driven over HTTP/1.1, where nothing carries `priority` -- Chromium writes it on HTTP/2 only -- so
//! its priority is read off the transport's request ledger, which records the field each request carries over
//! h2 (`RequestRecord::priority`). A request that differs is a finding about the engine, never a reason to edit
//! the record.
//!
//! Before this change, against the same record: a module script `no-cors` with no `Origin`, a cross-origin one
//! with the cookie; `Origin` after `accept` on every CORS element load and missing on the same-origin ones;
//! the cookie on a cross-origin `fetch()`, an XHR without `withCredentials`, `credentials: 'omit'`, a
//! `crossorigin` image and script, and `rsz` stored off an uncredentialed response; a `<track>` `cors`; and
//! `u=1` on every script, `u=2, i` on every image, `u=0` on every style sheet.
use engine::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("fixtures/request-shape/request-shape.html");
const CHROME: &str = include_str!("fixtures/request-shape/request-shape.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

type Log = Arc<Mutex<Vec<Value>>>;

/// One connection, keep-alive: every request on it recorded and answered as `server.js` answers it.
fn serve(s: std::net::TcpStream, log: Log, tag: &'static str, b: u16) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (m, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let (mut names, mut len) = (Vec::new(), 0usize);
        let (mut mode, mut origin, mut cookie) = (Value::Null, None::<String>, String::new());
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).unwrap_or_default();
            match k.as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "sec-fetch-mode" => mode = json!(v),
                "origin" => origin = Some(v.clone()),
                "cookie" => cookie = v.clone(),
                _ => {}
            }
            names.push(json!(k));
        }
        let mut body = vec![0u8; len];
        let _ = r.read_exact(&mut body);
        let has = |c: &str| cookie.split("; ").any(|p| p == c);
        log.lock().unwrap().push(json!({"m": m, "port": tag, "path": path, "names": names, "mode": mode,
            "origin": origin.is_some(), "rs7": has("rs7=1"), "rsz": has("rsz=1")}));
        let page = PAGE.replace("{{B}}", &format!("http://localhost:{b}"));
        let (status, ctype, body, extra): (&str, &str, &str, &str) = match (m.as_str(), path.as_str()) {
            ("OPTIONS", _) => ("204 No Content", "text/plain", "", ""),
            (_, "/rs/set") => ("302 Found", "text/plain", "", "Location: /rs/\r\nSet-Cookie: rs7=1; Path=/rs\r\n"),
            (_, "/rs/") => ("200 OK", "text/html", page.as_str(), ""),
            (_, "/rs/f-setz") => ("200 OK", "text/plain", "ok", "Set-Cookie: rsz=1; Path=/rs\r\n"),
            (_, p) if p.ends_with(".js") => ("200 OK", "text/javascript", "1", ""),
            (_, p) if p.ends_with(".css") => ("200 OK", "text/css", "a{}", ""),
            _ => ("200 OK", "text/plain", "ok", ""),
        };
        // The request's own origin is echoed -- `*` answers no credentialed request -- as `server.js` does.
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
             Access-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Credentials: true\r\nAccess-Control-Allow-Headers: x-k\r\n{extra}\r\n",
            body.len(),
            origin.as_deref().unwrap_or("*"),
        );
        if w.write_all(format!("{head}{body}").as_bytes()).is_err() {
            return;
        }
    }
}

/// Two ports, each on both loopback families (`localhost` may resolve to `::1`): `A` serves the page, `B`
/// is the other origin of the same site.
fn servers() -> (u16, u16, Log) {
    let log: Log = Arc::default();
    let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (pa, pb) = (a.local_addr().unwrap().port(), b.local_addr().unwrap().port());
    for (l, tag) in [(a, "A"), (b, "B")] {
        let port = l.local_addr().unwrap().port();
        for l in std::iter::once(l).chain(std::net::TcpListener::bind(("::1", port)).ok()) {
            let log = log.clone();
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    let log = log.clone();
                    std::thread::spawn(move || serve(s, log, tag, pb));
                }
            });
        }
    }
    (pa, pb, log)
}

/// A request's identity across the two records: method, origin (`A`/`B`) and path.
fn key(m: &str, port: &str, path: &str) -> String {
    format!("{m} {port} {path}")
}

/// The header names both records can compare: HTTP/2's pseudo-headers and HTTP/1.1's `host` and
/// `connection` are the protocol's, and `priority` is compared off the ledger.
fn names(r: &Value) -> Vec<String> {
    r["names"]
        .as_array()
        .map(|a| a.iter().filter_map(|n| n.as_str()).filter(|n| !n.starts_with(':') && !matches!(*n, "host" | "connection" | "priority")).map(str::to_string).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn every_request_has_the_shape_chrome_150_gives_it() {
    let want: Vec<Value> = serde_json::from_str(CHROME).expect("the recorded Chrome log is a JSON list");
    let (a, b, log) = servers();
    // A ledger generation of this test's own (no other test in this binary reads the ledger).
    transport::ledger_reset();
    let url = format!("http://localhost:{a}/rs/set");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.set_url(&resp.url);
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) && !log.lock().unwrap().iter().any(|r| r["path"] == "/rs/done") {
        let _ = e.pump(Duration::from_millis(50)).await;
    }
    let got = log.lock().unwrap().clone();
    assert!(got.iter().any(|r| r["path"] == "/rs/done"), "the page never finished: {got:#?}");
    // The priority each request carries, by the URL it was asked for and the one it ended at (a redirect
    // chain is one ledger row; each hop carries the same field).
    let mut priority: std::collections::HashMap<String, Value> = Default::default();
    for row in transport::ledger().records() {
        for u in [&row.url, &row.final_url] {
            let Ok(u) = url::Url::parse(u) else { continue };
            let port = match u.port() {
                Some(p) if p == a => "A",
                Some(p) if p == b => "B",
                _ => continue,
            };
            priority.insert(key(&row.method, port, u.path()), json!(row.priority));
        }
    }
    let mut failures = Vec::new();
    let theirs: std::collections::HashMap<String, &Value> =
        want.iter().map(|w| (key(w["m"].as_str().unwrap_or(""), w["port"].as_str().unwrap_or(""), w["path"].as_str().unwrap_or("")), w)).collect();
    for g in &got {
        let k = key(g["m"].as_str().unwrap_or(""), g["port"].as_str().unwrap_or(""), g["path"].as_str().unwrap_or(""));
        let Some(w) = theirs.get(&k) else {
            failures.push(format!("{k}: korvus sent it, Chrome did not"));
            continue;
        };
        for f in ["mode", "origin", "rs7", "rsz"] {
            if g[f] != w[f] {
                failures.push(format!("{k}: {f} korvus {} Chrome {}", g[f], w[f]));
            }
        }
        if names(g) != names(w) {
            failures.push(format!("{k}: header names\n    korvus {:?}\n    Chrome {:?}", names(g), names(w)));
        }
        let p = priority.get(&k).cloned().unwrap_or(json!("(no ledger row)"));
        if p != w["priority"] {
            failures.push(format!("{k}: priority korvus {p} Chrome {}", w["priority"]));
        }
    }
    let ours: std::collections::HashSet<String> =
        got.iter().map(|g| key(g["m"].as_str().unwrap_or(""), g["port"].as_str().unwrap_or(""), g["path"].as_str().unwrap_or(""))).collect();
    for k in theirs.keys().filter(|k| !ours.contains(*k)) {
        failures.push(format!("{k}: Chrome sent it, korvus did not"));
    }
    failures.sort();
    assert!(failures.is_empty(), "{} request(s) differ from Chrome 150's:\n  {}", failures.len(), failures.join("\n  "));
}

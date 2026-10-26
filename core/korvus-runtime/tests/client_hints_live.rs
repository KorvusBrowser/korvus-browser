//! HTTP Client Hints as Chrome 150 sends them after `Accept-CH` and `Critical-CH` (a challenge provider's
//! challenge page answers both, naming the high-entropy UA hints, and Chrome restarts the navigation with
//! them -- korvus ran the challenge on a "Chrome" that ignored `Critical-CH`).
//!
//! `fixtures/client-hints/client-hints.html` is served UNMODIFIED, here by the server below and for Chrome by
//! `fixtures/client-hints/server.py` (same routes): its document answers `Accept-CH` (eight UA hints and an
//! unknown token), `Critical-CH` (two of them) and a cookie; it loads a same-origin script, a cross-site one
//! (`localhost` on the same port), an XHR POST with an author header, a `fetch()`, a same-origin and a
//! cross-site frame, then navigates to `/ch/next`, whose page (its icon declared, so no second favicon fetch
//! races the end) asks for `/ch/done`. The server records every
//! request's header NAMES in wire order and its client hints' values. `client-hints.chrome-152.json` is
//! Chrome 152.0.7977.82's record, headed under Xvfb, three runs alike. A request that
//! differs is a finding about the engine, never a reason to edit the file.
use engine::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("fixtures/client-hints/client-hints.html");
const CHROME: &str = include_str!("fixtures/client-hints/client-hints.chrome-152.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";
const ACCEPT_CH: &str = "Sec-CH-UA-Full-Version-List, Sec-CH-UA-Arch, Sec-CH-UA-Bitness, Sec-CH-UA-Model, \
Sec-CH-UA-Platform-Version, Sec-CH-UA-Full-Version, Sec-CH-UA-WoW64, Sec-CH-UA-Form-Factors, UA-Arch";
const CRITICAL_CH: &str = "Sec-CH-UA-Full-Version-List, Sec-CH-UA-Arch";

type Log = Arc<Mutex<Vec<Value>>>;

/// One connection, keep-alive: every request on it is recorded and answered as `server.py` answers it.
fn serve(s: std::net::TcpStream, log: Log) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (m, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut names = Vec::new();
        let mut hints = serde_json::Map::new();
        let (mut host, mut len) = (String::new(), 0usize);
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
            if k == "host" {
                host = v.rsplit_once(':').map(|(a, _)| a.to_string()).unwrap_or(v.clone());
            }
            if k == "content-length" {
                len = v.parse().unwrap_or(0);
            }
            if k.starts_with("sec-ch-") || k == "cookie" {
                hints.insert(k.clone(), json!(v));
            }
            names.push(json!(k));
        }
        let mut body = vec![0u8; len];
        let _ = r.read_exact(&mut body);
        log.lock().unwrap().push(json!({"m": m, "host": host, "path": path, "names": names, "hints": hints}));
        let p = path.split('?').next().unwrap_or("").to_string();
        let (ctype, body, extra): (&str, &str, Vec<(&str, &str)>) = match p.as_str() {
            "/ch/" if m == "GET" => ("text/html", PAGE, vec![("Accept-CH", ACCEPT_CH), ("Critical-CH", CRITICAL_CH), ("Set-Cookie", "k=1; Path=/")]),
            "/ch/next" => ("text/html", "<!doctype html><link rel=icon href=\"data:,\"><script>fetch('/ch/done')</script>", vec![]),
            "/ch/frame.html" => ("text/html", "<!doctype html><p>f</p>", vec![]),
            p if p.ends_with(".js") => ("text/javascript", "1", vec![]),
            _ => ("text/plain", "ok", vec![]),
        };
        let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if w.write_all(format!("{head}\r\n{body}").as_bytes()).is_err() {
            return;
        }
    }
}

/// The server, on one port of both loopback families (`localhost` may resolve to `::1`).
fn server() -> (u16, Log) {
    let log: Log = Arc::default();
    let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = v4.local_addr().unwrap().port();
    let listeners = std::iter::once(v4).chain(std::net::TcpListener::bind(("::1", port)).ok());
    for l in listeners {
        let log = log.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || serve(s, log));
            }
        });
    }
    (port, log)
}

fn key(r: &Value) -> String {
    format!("{} {} {}", r["m"].as_str().unwrap_or(""), r["host"].as_str().unwrap_or(""), r["path"].as_str().unwrap_or(""))
}

#[tokio::test]
async fn client_hints_follow_accept_ch_and_critical_ch_as_chromes_do() {
    let want: Vec<Value> = serde_json::from_str(CHROME).expect("the recorded Chrome log is a JSON list");
    let (port, log) = server();
    let url = format!("http://127.0.0.1:{port}/ch/");
    // the `korvus fetch` driver's loop: the document through the transport, the page's navigation followed
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) && !log.lock().unwrap().iter().any(|r| r["path"] == "/ch/done") {
        if let Some(nav) = e.take_due_navigation() {
            e.fire_beforeunload();
            let next = transport::fetch(engine::navigation_request(&nav, e.persona().as_ref())).await.expect("the page's navigation");
            e.fire_unload();
            e = e.successor();
            e.arrive(engine::arrival_for(&nav, &next.url));
            e.load_page_response(&next.bytes, &next.url, &[], &next.headers).await.expect("load next");
            continue;
        }
        let _ = e.pump(Duration::from_millis(50)).await;
    }
    let got = log.lock().unwrap().clone();
    // Request by request, in each side's order of arrival (the favicon's moment is the network's): the
    // n-th request for a (method, host, path) against Chrome's n-th.
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    let mut theirs: std::collections::HashMap<String, Vec<&Value>> = Default::default();
    for w in &want {
        theirs.entry(key(w)).or_default().push(w);
    }
    for g in &got {
        let k = key(g);
        let n = seen.entry(k.clone()).or_default();
        let w = theirs.get(&k).and_then(|v| v.get(*n)).unwrap_or_else(|| panic!("korvus sent {k} (#{n}), Chrome did not"));
        *n += 1;
        assert_eq!(g["names"], w["names"], "{k}: the header names in wire order, korvus vs Chrome 150");
        assert_eq!(g["hints"], w["hints"], "{k}: the client hints' values (and cookie), korvus vs Chrome 150");
    }
    let mut got_keys: Vec<String> = got.iter().map(key).collect();
    let mut want_keys: Vec<String> = want.iter().map(key).collect();
    got_keys.sort();
    want_keys.sort();
    assert_eq!(got_keys, want_keys, "the requests made");
}

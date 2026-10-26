//! A response's content coding, undone as Chrome 150 undoes it.
//!
//! united.com's `HEAD /en/us` answered `Content-Encoding: gzip` with no body, as a HEAD does; korvus failed it
//! `net::ERR_CONTENT_DECODING_FAILED` and Chrome answered 200. The transport
//! ended every coded body with its decoder's `finish`, which demands a complete stream; Chromium's filters fail only
//! on bytes they cannot decode (`transport/src/content_coding.rs`). The probe covers the rest of those rules: an
//! empty or truncated body, a bad gzip CRC, a second gzip member, `identity` or an empty token beside a coding,
//! `deflate` as zlib and raw, and over XHR.
//!
//! `fixtures/content-coding/`: `cases.json` (a method, the `Content-Encoding` lines, a body file), the bodies
//! (`make_bodies.py`), `page.html` (fetches every case and reports the bytes it got), `record.py` and
//! `chrome-150.json` -- stock Chrome 150.0.7871.46, headed under Xvfb, no DevTools client, three runs byte-identical.
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/content-coding");

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// `record.py`'s server: `/`, `/cases.json`, and `/c/<case>` with the case's codings and body (none for a HEAD).
fn serve(s: std::net::TcpStream, cases: &serde_json::Value) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
            if let Some(n) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = n.trim().parse().unwrap_or(0);
            }
        }
        let mut sink = vec![0u8; len];
        let _ = r.read_exact(&mut sink);
        let (body, ctype, codings): (Vec<u8>, &str, Vec<String>) = match path.as_str() {
            "/" => (read("page.html"), "text/html; charset=utf-8", Vec::new()),
            "/cases.json" => (read("cases.json"), "application/json", Vec::new()),
            p => match p.strip_prefix("/c/").and_then(|k| cases.get(k)) {
                Some(c) => {
                    let codings = c[1].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
                    let body = c[2].as_str().map(|b| read(&format!("{b}.bin"))).unwrap_or_default();
                    (body, "text/plain", codings)
                }
                None => (Vec::new(), "text/plain", Vec::new()),
            },
        };
        let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\n");
        for c in &codings {
            head.push_str(&format!("Content-Encoding: {c}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        let body: &[u8] = if method == "HEAD" { &[] } else { &body };
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(body)).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_coded_body_is_undone_as_chromes() {
    let chrome: serde_json::Value = serde_json::from_slice(&read("chrome-150.json")).expect("chrome-150.json");
    let cases: serde_json::Value = serde_json::from_slice(&read("cases.json")).expect("cases.json");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let cases = cases.clone();
            std::thread::spawn(move || serve(s, &cases));
        }
    });
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let mut out = serde_json::Value::Null;
    while start.elapsed() < Duration::from_secs(20) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "window.__out ? JSON.stringify(window.__out) : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            out = serde_json::from_str(s).expect("the page's JSON");
            break;
        }
    }
    assert!(out.is_object(), "the probe page never finished");
    let want = chrome["rows"].as_object().expect("rows");
    let mut diffs = Vec::new();
    for (k, row) in want {
        if out.get(k) != Some(row) {
            diffs.push(format!("{k}: korvus {} vs Chrome 150 {row}", out.get(k).cloned().unwrap_or_default()));
        }
    }
    assert!(diffs.is_empty(), "{} of {} cases differ:\n{}", diffs.len(), want.len(), diffs.join("\n"));
}

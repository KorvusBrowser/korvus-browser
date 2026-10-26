//! LIVE gate: a frame's navigation that answers an HTTP error status, an empty body, or fails outright, as Chrome 150
//! commits it (ledger N19).
//!
//! Chromium commits ANY HTTP response in a frame as its document -- a 403, 404, 418 or 500 body runs its scripts and
//! reads its status in Navigation Timing; an empty one is an empty document; one sent as `text/plain` a text document
//! -- because `HttpErrorNavigationThrottle`, which swaps an empty error body for Chrome's own error page, is added for
//! the primary main frame alone. A navigation that fails on the network commits Chrome's error page at
//! `chrome-error://chromewebdata/`, cross-origin to the page: `contentDocument` null, `location` a SecurityError, and
//! `load` at the element either way. An `<object>` answered 404 shows its fallback and fires `error`. This engine
//! built an empty document for every status outside 2xx -- a device-check script 403 leboncoin's auth frame answers never ran
//! -- and gave a same-origin frame whose connection closed an empty, readable document.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools client, three runs byte-identical): `fixtures/frame-status/
//! page.html` with `frame.html` behind it answered `fixtures/frame-status/chrome-150.json`. The server below answers
//! as that recording's did.
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/frame-status");

/// Rows korvus does not answer as Chrome does for a reason outside this fix: an `<embed>` is no frame owner in this
/// engine, so it adds no `window` to the page, runs no document and fires `error` where Chrome's frame fires `load`.
const KNOWN_GAPS: &[&str] = &["window.length", "embed messages", "e404 events"];

fn read(name: &str) -> String {
    std::fs::read_to_string(format!("{FIX}/{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn serve(s: std::net::TcpStream, page: &str, frame: &str) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
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
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        // a same-origin navigation that fails outright: the connection closes with no response
        if path == "/reset" {
            let _ = w.shutdown(std::net::Shutdown::Both);
            return;
        }
        let mut ty = "text/html; charset=utf-8";
        let (status, body) = if path == "/" {
            (200u16, page.to_string())
        } else if let Some(code) = path.strip_prefix("/f/").and_then(|c| c.parse::<u16>().ok()) {
            let tag = match query {
                "o" => "object",
                "e" => "embed",
                _ => "",
            };
            if query == "text" {
                ty = "text/plain; charset=utf-8";
            }
            (code, if query == "empty" { String::new() } else { frame.replace("{c}", &format!("{code}{tag}")) })
        } else {
            // the page's POST /report, and anything else
            (if path == "/report" { 200 } else { 404 }, String::new())
        };
        let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n", body.len());
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(body.as_bytes())).is_err() {
            return;
        }
    }
}

fn start_server() -> String {
    let page = read("page.html");
    let frame = read("frame.html").trim().to_string();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (page, frame) = (page.clone(), frame.clone());
            std::thread::spawn(move || serve(s, &page, &frame));
        }
    });
    base
}

#[tokio::test]
async fn a_frame_commits_any_http_response_and_an_error_page_for_a_network_failure_as_chrome_150_does() {
    let want: Vec<(String, serde_json::Value)> = serde_json::from_str(&read("chrome-150.json")).expect("the recorded Chrome rows");
    let base = start_server();
    let url = format!("{base}/");
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let mut got: Vec<(String, serde_json::Value)> = Vec::new();
    while start.elapsed() < Duration::from_secs(15) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "window.__out ? JSON.stringify(window.__out) : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            got = serde_json::from_str(s).expect("the page's rows");
            break;
        }
    }
    assert!(!got.is_empty(), "the page never finished");
    let names = |rows: &[(String, serde_json::Value)]| rows.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    assert_eq!(names(&got), names(&want), "the same rows, in the same order");
    let mut diffs = Vec::new();
    for ((k, have), (_, chrome)) in got.iter().zip(&want) {
        let gap = KNOWN_GAPS.contains(&k.as_str());
        if (have != chrome) != gap {
            diffs.push(format!("{k}: chrome {chrome} korvus {have}{}", if gap { " (listed as a known gap)" } else { "" }));
        }
    }
    assert!(diffs.is_empty(), "korvus vs Chrome 150:\n{}", diffs.join("\n"));
}

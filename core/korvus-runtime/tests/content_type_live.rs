//! A response's MIME type, read off its `Content-Type` as Chrome 150 reads it (A/B C5).
//!
//! allegro.pl's device-check page arrived on 2026-10-11 as ONE field joining two values,
//! `text/html,text/html;charset=utf-8`; Chrome ran it, korvus showed it as a text document (every reader cut the
//! field at its first `;` by hand). Chromium has three readings (`transport/src/mime.rs`): a DOCUMENT is typed by the
//! network stack (every value, the last valid one winning, with its own charset); a script's and a style sheet's
//! strict checks read Blink's `HttpContentType()` (the FIRST value); `XMLHttpRequest` its own first-value extraction.
//!
//! `fixtures/content-type/` holds the table (`variants.json`: each response's `Content-Type` LINES), the variant
//! document, and the driver page that loads every variant as a frame, a script and a sheet under `nosniff`, an XHR and
//! a `fetch()`. `chrome-150.json` is stock Chrome 150.0.7871.46's answer: headed under Xvfb, no DevTools client,
//! popups allowed (its `top` rows are popups; here the engine navigates to each document itself), three runs
//! byte-identical (2026-10-11).
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/content-type");

/// Rows korvus does not answer as Chrome does for a reason outside this fix, each named. None now: the `f`/`g`
/// frames' documents reported their page's `characterSet` until a frame's body was decoded in its own charset;
/// they are GBK, as Chrome's are.
const KNOWN_GAPS: &[(&str, &str, usize)] = &[];

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn serve(s: std::net::TcpStream, v: &serde_json::Value) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
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
        let lines = |kind: &str, k: &str| -> Vec<String> {
            v[kind][k].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
        };
        let p = path.split('?').next().unwrap_or("/").to_string();
        let parts: Vec<&str> = p.trim_matches('/').split('/').collect();
        let (cts, nosniff, body): (Vec<String>, bool, Vec<u8>) = match (p.as_str(), parts.as_slice()) {
            ("/", _) => (vec!["text/html".into()], false, read("driver.html")),
            ("/variants.json", _) => (vec!["application/json".into()], false, read("variants.json")),
            (_, ["doc", k]) => (lines("doc", k), false, read("doc.html")),
            (_, ["s", k]) => (lines("script", k), true, format!("(window.__s = window.__s || {{}})['{k}'] = 1;").into_bytes()),
            (_, ["c", k]) => (lines("style", k), true, format!("#c_{k} {{ width: 123px; }}").into_bytes()),
            (_, ["x", k]) => (lines("xhr", k), false, b"<a/>".to_vec()),
            (_, ["f", k]) => (lines("fetch", k), false, b"x".to_vec()),
            _ => (vec!["text/plain".into()], false, Vec::new()),
        };
        let mut head = String::from("HTTP/1.1 200 OK\r\n");
        for ct in &cts {
            head.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if nosniff {
            head.push_str("X-Content-Type-Options: nosniff\r\n");
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(&body)).is_err() {
            return;
        }
    }
}

fn start_server() -> String {
    let v: serde_json::Value = serde_json::from_slice(&read("variants.json")).expect("variants.json");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let v = v.clone();
            std::thread::spawn(move || serve(s, &v));
        }
    });
    base
}

fn engine(url: &str) -> engine::Engine {
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(url);
    e
}

async fn navigate(e: &mut engine::Engine, url: &str) {
    let resp = transport::fetch(transport::FetchRequest::navigation(url.to_string()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
}

#[tokio::test]
async fn every_reader_of_a_content_type_answers_as_chromes() {
    let chrome: serde_json::Value = serde_json::from_slice(&read("chrome-150.json")).expect("chrome-150.json");
    let base = start_server();
    let mut got = serde_json::Map::new();

    // The page: frames, scripts, sheets, XHR, fetch.
    let mut e = engine(&format!("{base}/"));
    navigate(&mut e, &format!("{base}/")).await;
    let start = Instant::now();
    let mut out = serde_json::Value::Null;
    while start.elapsed() < Duration::from_secs(10) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "window.__out ? JSON.stringify(window.__out) : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            out = serde_json::from_str(s).expect("the driver's JSON");
            break;
        }
    }
    assert!(out.is_object(), "the driver page never finished");
    for (k, v) in out.as_object().unwrap() {
        got.insert(k.clone(), v.clone());
    }

    // Each document as a TOP-LEVEL navigation (Chrome's rows are popups of the same documents).
    let mut top = serde_json::Map::new();
    for k in chrome["top"].as_object().expect("top rows").keys() {
        let url = format!("{base}/doc/{k}");
        let mut e = engine(&url);
        navigate(&mut e, &url).await;
        let _ = e.pump(Duration::from_millis(200)).await;
        let v = e
            .eval_value("r", "JSON.stringify([window.__ran === true, document.contentType, document.characterSet, document.compatMode])".to_string())
            .expect("eval");
        top.insert(k.clone(), serde_json::from_str(v.as_str().unwrap_or("null")).expect("a row"));
    }
    got.insert("top".into(), serde_json::Value::Object(top));

    // Row by row, every field; a known gap must still BE a gap (a fix elsewhere removes it from the list).
    let mut diffs = Vec::new();
    for (sec, rows) in chrome.as_object().unwrap() {
        for (k, want) in rows.as_object().unwrap() {
            let have = &got[sec.as_str()][k.as_str()];
            match (want.as_array(), have.as_array()) {
                (Some(w), Some(h)) if w.len() == h.len() => {
                    for (i, (a, b)) in w.iter().zip(h).enumerate() {
                        let gap = KNOWN_GAPS.contains(&(sec.as_str(), k.as_str(), i));
                        if (a != b) != gap {
                            diffs.push(format!("{sec}.{k}[{i}]: chrome {a} korvus {b}{}", if gap { " (listed as a known gap)" } else { "" }));
                        }
                    }
                }
                _ if want != have => diffs.push(format!("{sec}.{k}: chrome {want} korvus {have}")),
                _ => {}
            }
        }
    }
    assert!(diffs.is_empty(), "korvus vs Chrome 150:\n{}", diffs.join("\n"));
}

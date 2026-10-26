//! Navigation Timing L1 (`performance.timing`) as Chrome 150 answers it.
//!
//! Every L1 field was one value -- the realm's time origin at install, floored -- so `domInteractive` and
//! `loadEventEnd` answered before either had happened, `responseStart` equalled `fetchStart`, a frame reported
//! nothing of its own, and a page whose clock then moved to its navigation's start read `navigationStart` up to a
//! millisecond AFTER `timeOrigin` (Chrome: in (-1, 0]; korvus +0.7..1.1 on a bot challenge). Each field is
//! now read from the document at call time, as Blink's `PerformanceTiming` reads it.
//!
//! The probe (`fixtures/performance-timing/probe.html`, a same-origin frame at `/frame`) answers only what does not
//! race: which fields are 0 when, their order, `L1 - navigationStart` against the navigation entry, the key order of
//! `toJSON()` (a `HashMap`'s in Blink), User Timing's L1 names, `chrome.loadTimes()` / `csi()`, an `<iframe srcdoc>`
//! (no response: no network fields) and a page getter called on a frame's object. `chrome-150.json` was recorded from
//! `/usr/bin/google-chrome` 150.0.7871.46 on this machine by `record.py` beside it: headed under Xvfb, launched by hand
//! and attached over CDP, three runs that agree.
use engine::Engine;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const PROBE: &str = include_str!("fixtures/performance-timing/probe.html");
const FRAME: &str = include_str!("fixtures/performance-timing/frame.html");

fn serve(s: std::net::TcpStream) {
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
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
        }
        let (status, page) = match path.as_str() {
            "/" => (200, PROBE),
            "/frame" => (200, FRAME),
            _ => (404, ""),
        };
        let head = format!("HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n", page.len());
        if w.write_all(format!("{head}{page}").as_bytes()).is_err() {
            return;
        }
    }
}

fn server() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    port
}

#[tokio::test]
async fn performance_timing_answers_as_chrome_150s_does() {
    let url = format!("http://127.0.0.1:{}/", server());
    // navigated as `korvus fetch` navigates: the transport fetches the document, so it has a navigation response
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "window.__r || null".to_string()).expect("eval");
        if let Some(s) = v.as_str() {
            break serde_json::from_str::<serde_json::Value>(s).expect("the probe's answers");
        }
        assert!(start.elapsed() < Duration::from_secs(15), "the probe never answered");
    };
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/performance-timing/chrome-150.json")).expect("Chrome's answers");
    let mut bad = Vec::new();
    for (part, w) in want.as_object().expect("an object") {
        for (k, v) in w.as_object().expect("an object") {
            let g = &got[part][k];
            if g != v {
                bad.push(format!("{part}.{k}:\n    korvus {g}\n    Chrome {v}"));
            }
        }
        for k in got[part].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() {
            if !w.as_object().unwrap().contains_key(&k) {
                bad.push(format!("{part}.{k}: korvus only"));
            }
        }
    }
    assert!(bad.is_empty(), "{} answers differ from Chrome 150:\n{}", bad.len(), bad.join("\n"));
}

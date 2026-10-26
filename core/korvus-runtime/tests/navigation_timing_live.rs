//! Navigation Timing as Chrome 150.0.7871.46 reports it -- a document's redirects, a `Critical-CH` restart,
//! and a frame's own clock (a bot challenge page reads its navigation entry, and its
//! frame reads the frame's).
//!
//! Measured on google-chrome 150.0.7871.46, headed under Xvfb, against a loopback server with these routes
//! (each page reporting its own `performance.getEntriesByType('navigation')[0]`):
//!
//! ```text
//! /same  302 -> /final                   redirectCount 1, redirectStart/End > 0
//! /same2 302 -> /same -> /final          redirectCount 2, redirectStart/End > 0
//! /cross 302 -> http://localhost/final   redirectCount 0, redirectStart 0, redirectEnd 0 (a cross-origin hop)
//! /ch    Accept-CH + Critical-CH         criticalCHRestart 118.3, fetchStart 118.5, redirectCount 1,
//!                                        redirectStart 4.8 (the restart counts as a redirect)
//! a cross-origin iframe inserted 500 ms  fetchStart 2, requestStart 3, responseStart 53, responseEnd 54,
//!   after the page's load                performance.now() 58 at its first script -- its OWN clock
//! ```
//!
//! korvus reported `redirectCount` 0 for every one, the redirect window for a cross-origin chain,
//! `criticalCHRestart` 0, and a frame's times on the PAGE's clock (fetchStart 510, now 562).
use engine::Engine;
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

const REPORT: &str = "<!doctype html><script>var n = performance.getEntriesByType('navigation')[0]; \
window.__r = {redirectCount: n.redirectCount, redirectStart: n.redirectStart, redirectEnd: n.redirectEnd, \
fetchStart: n.fetchStart, criticalCHRestart: n.criticalCHRestart};</script>";

const HOST: &str = "<!doctype html><script>addEventListener('message', function (e) { window.__r = e.data; }); \
addEventListener('load', function () { setTimeout(function () { var f = document.createElement('iframe'); \
f.src = 'http://localhost:' + location.port + '/frame'; document.body.appendChild(f); }, 500); });</script><body>";

const FRAME: &str = "<!doctype html><script>var n = performance.getEntriesByType('navigation')[0]; \
parent.postMessage({fetchStart: n.fetchStart, responseStart: n.responseStart, domInteractive: n.domInteractive, \
now: performance.now()}, '*');</script>";

fn serve(s: std::net::TcpStream, port: u16) {
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
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        let _ = r.read_exact(&mut body);
        let (status, location, extra, page): (u16, Option<String>, &str, &str) = match path.as_str() {
            "/same" => (302, Some("/final".into()), "", ""),
            "/same2" => (302, Some("/same".into()), "", ""),
            "/cross" => (302, Some(format!("http://localhost:{port}/final")), "", ""),
            "/ch" => (200, None, "Accept-CH: Sec-CH-UA-Arch, Sec-CH-UA-Full-Version-List\r\nCritical-CH: Sec-CH-UA-Arch\r\n", REPORT),
            "/host" => (200, None, "", HOST),
            "/frame" => (200, None, "", FRAME),
            _ => (200, None, "", REPORT),
        };
        let loc = location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
        let head = format!("HTTP/1.1 {status} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{loc}{extra}\r\n", page.len());
        if w.write_all(format!("{head}{page}").as_bytes()).is_err() {
            return;
        }
    }
}

fn server() -> u16 {
    let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = v4.local_addr().unwrap().port();
    for l in std::iter::once(v4).chain(std::net::TcpListener::bind(("::1", port)).ok()) {
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                std::thread::spawn(move || serve(s, port));
            }
        });
    }
    port
}

/// Navigate to `url` as `korvus fetch` does and return the page's `__r`.
async fn report(url: &str) -> serde_json::Value {
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    e.set_url(url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.to_string()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "JSON.stringify(window.__r || null)".to_string()).expect("eval");
        let v: serde_json::Value = serde_json::from_str(v.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        if !v.is_null() {
            return v;
        }
    }
    panic!("{url}: the page never reported");
}

fn num(v: &serde_json::Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or_else(|| panic!("{k} missing in {v}"))
}

#[tokio::test]
async fn redirects_and_a_critical_ch_restart_on_the_navigation_entry_as_chrome_reports_them() {
    let port = server();
    let at = |p: &str| format!("http://127.0.0.1:{port}{p}");
    for (path, count) in [("/same", 1.0), ("/same2", 2.0)] {
        let r = report(&at(path)).await;
        assert_eq!(num(&r, "redirectCount"), count, "{path}: {r}");
        // korvus's navigation starts as its fetch is asked, so its first hop can begin inside the same
        // 0.1 ms quantum -- `redirectStart` 0 -- where Chrome's own navigation steps put a few ms first
        assert!(num(&r, "redirectEnd") > 0.0 && num(&r, "redirectEnd") >= num(&r, "redirectStart"), "{path}: {r}");
        assert!(num(&r, "fetchStart") >= num(&r, "redirectEnd"), "{path}: {r}");
        assert_eq!(num(&r, "criticalCHRestart"), 0.0, "{path}: {r}");
    }
    // a cross-origin hop hides the redirects
    let r = report(&at("/cross")).await;
    assert_eq!((num(&r, "redirectCount"), num(&r, "redirectStart"), num(&r, "redirectEnd")), (0.0, 0.0, 0.0), "/cross: {r}");
    // the restart is a redirect, stamped
    let r = report(&at("/ch")).await;
    assert_eq!(num(&r, "redirectCount"), 1.0, "/ch: {r}");
    assert!(num(&r, "criticalCHRestart") > 0.0, "/ch: {r}");
    assert!(num(&r, "redirectStart") <= num(&r, "criticalCHRestart"), "/ch: {r}");
    assert!(num(&r, "fetchStart") >= num(&r, "criticalCHRestart"), "/ch: {r}");
}

/// A frame's times are on ITS clock: Chrome reads `fetchStart` 2 and `performance.now()` 58 in a frame made
/// 500 ms after its page loaded; on the page's clock they were 510 and 562.
#[tokio::test]
async fn a_frame_reports_its_navigation_on_its_own_clock() {
    let port = server();
    let r = report(&format!("http://127.0.0.1:{port}/host")).await;
    assert!(num(&r, "fetchStart") < 100.0, "{r}");
    assert!(num(&r, "responseStart") >= num(&r, "fetchStart") && num(&r, "responseStart") < 300.0, "{r}");
    assert!(num(&r, "now") < 400.0 && num(&r, "now") >= num(&r, "responseStart"), "{r}");
    // its own milestones: not interactive yet while its first script runs (the page's were 4)
    assert_eq!(num(&r, "domInteractive"), 0.0, "{r}");
}

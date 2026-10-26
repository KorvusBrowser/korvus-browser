//! `document.fonts` as Chrome 150's `FontFaceSetDocument` keeps it.
//!
//! a device-check script's tag lists the families of `document.fonts`' faces on a page with `@font-face` rules: Chrome 150
//! answers the page's faces, `unloaded`; korvus answered an empty set (`a device-check script/probes/dd_signals.js`). A document's
//! set holds a CSS-CONNECTED `FontFace` per `@font-face` rule of its active sheets that names a family and a
//! `src` -- in sheet order, before the faces script added, not deletable, made anew when a sheet holding them
//! changes -- each `unloaded` until layout needs it for a character in its `unicode-range` or a `FontFaceSet.load()`
//! asks for it. The set is `loading` from the moment one of its faces starts loading to the task that fires
//! `loadingdone` (with the faces that loaded) and `loadingerror` (with those that failed); `ready` is pending until
//! the task after the load event, and a load after that replaces it. `check()` and `load()` ask the family's
//! best-matching faces whose `unicode-range` covers the text, and `check()` passes over a family the persona has a
//! font of. A source list falls through a failed `url()` to a `local()` after it.
//!
//! `fixtures/document-fonts/fonts-set.js` runs in `probe-page.html` (the probe inlined in a classic script, its
//! answer POSTed to `/report`), served as this file serves it: every other path 404s. Every row is compared with
//! `fonts-set.chrome-150.json`, stock Chrome 150.0.7871.46's (headed under Xvfb, no DevTools, three runs alike).
//! `local("DejaVu Sans")` is a font of this machine's, the persona's; the test says so where it is missing.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const PROBE_PAGE: &str = include_str!("fixtures/document-fonts/probe-page.html");
const FONTS_SET: &str = include_str!("fixtures/document-fonts/fonts-set.js");
const FONTS_SET_CHROME: &str = include_str!("fixtures/document-fonts/fonts-set.chrome-150.json");
const FONTS_READY: &str = include_str!("fixtures/document-fonts/fonts-ready.js");
const FONTS_READY_CHROME: &str = include_str!("fixtures/document-fonts/fonts-ready.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

fn serve(s: std::net::TcpStream, probe: &'static str) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        if length > 0 {
            let mut body = vec![0u8; length];
            if r.read_exact(&mut body).is_err() {
                return;
            }
        }
        let body = match (method.as_str(), target.as_str()) {
            ("POST", _) => String::new(),
            (_, "/") => PROBE_PAGE.replace("PROBE", probe),
            _ => String::new(),
        };
        let status = if method == "POST" || !body.is_empty() { "200 OK" } else { "404 Not Found" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// What the page reports.
async fn answers(probe: &'static str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, probe));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got = loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break s.to_string();
        }
    };
    serde_json::from_str(&got).expect("the page's report")
}

fn dejavu_missing() -> bool {
    let fonts = std::process::Command::new("fc-list").arg(":fullname").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    !fonts.contains("DejaVu Sans") || !fonts.contains("DejaVu Serif")
}

fn differences(got: &serde_json::Value, want: &serde_json::Value) -> Vec<String> {
    let (g, w) = (got.as_array().cloned().unwrap_or_default(), want.as_array().cloned().unwrap_or_default());
    let mut out: Vec<String> = g.iter().zip(&w).filter(|(a, b)| a != b).map(|(a, b)| format!("korvus {a}\n    Chrome {b}")).collect();
    if g.len() != w.len() {
        out.push(format!("{} rows, Chrome {}", g.len(), w.len()));
    }
    out
}

/// The page's `@font-face` rules are `document.fonts`' faces, and they load, settle and signal as Chrome 150's do.
#[tokio::test]
async fn document_fonts_holds_the_pages_font_faces_as_chrome_150() {
    if dejavu_missing() {
        eprintln!("SKIPPED: DejaVu Sans / DejaVu Serif are not installed here; the probe's local() sources name them");
        return;
    }
    let want: serde_json::Value = serde_json::from_str(FONTS_SET_CHROME).expect("the recorded Chrome rows");
    let got = answers(FONTS_SET).await;
    let differ = differences(&got, &want);
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

/// `ready` waits for the load event, a created document's set has none to wait for, and a face's load reaches the
/// set's events and its promise in Chrome 150's order.
#[tokio::test]
async fn document_fonts_ready_settles_as_chrome_150() {
    if dejavu_missing() {
        eprintln!("SKIPPED: DejaVu Sans is not installed here; the probe's local() sources name it");
        return;
    }
    let want: serde_json::Value = serde_json::from_str(FONTS_READY_CHROME).expect("the recorded Chrome rows");
    let got = answers(FONTS_READY).await;
    let differ = differences(&got, &want);
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

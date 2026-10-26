//! A frame's tasks, its window and its document as stock Chrome 150 runs them. an interactive-widget challenge
//! challenge frame -- cross-site to its page -- makes an `about:blank` child, reads `document.domain`,
//! `scrollingElement` and `window.event` there, sets `script.nonce`, and reads its own `long-animation-frame` entries
//! into its payload; each answer here was this engine's own until this batch.
//!
//! Every list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine, three runs alike, from the
//! fixtures in `fixtures/frame-tasks/` served as this file serves them: a page at `127.0.0.1` frames `/f` from
//! `localhost` (another site, so Chrome runs the frame in a process of its own) and reports what the frame's probe
//! posts it, with the number of its own long animation frames since it made the frame; ports read `PORT`.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const HOST_PAGE: &str = include_str!("fixtures/frame-tasks/host-page.html");
const FRAME_PAGE: &str = include_str!("fixtures/frame-tasks/frame-page.html");
const CSP_PAGE: &str = include_str!("fixtures/frame-tasks/csp-page.html");
const PROBE_PAGE: &str = include_str!("fixtures/frame-tasks/probe-page.html");

#[derive(Clone, Copy)]
enum Mode {
    /// `host-page.html` at `/`, the probe in `frame-page.html` at `/f` (from `localhost`), answering after `delay`
    Host { delay: Duration },
    /// the probe in `csp-page.html`, under a header-delivered `Content-Security-Policy`
    Csp,
    /// the probe in `probe-page.html`
    Page,
}

fn serve(s: std::net::TcpStream, mode: Mode, probe: &'static str) {
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
        let mut extra = String::new();
        let mut ct = "text/html; charset=utf-8";
        let body: String = match (method.as_str(), target.as_str(), mode) {
            ("POST", _, _) => String::new(),
            (_, "/", Mode::Host { .. }) => HOST_PAGE.to_string(),
            (_, "/f", Mode::Host { delay }) => {
                std::thread::sleep(delay);
                FRAME_PAGE.replace("PROBE", probe)
            }
            (_, "/", Mode::Csp) => {
                extra.push_str("Content-Security-Policy: script-src 'nonce-n0nce' 'unsafe-eval'; object-src 'none'\r\n");
                CSP_PAGE.replace("PROBE", probe)
            }
            (_, "/s.js", Mode::Csp) => {
                ct = "text/javascript";
                "window.__pe = 1;".to_string()
            }
            (_, "/", Mode::Page) => PROBE_PAGE.replace("PROBE", probe),
            // a response that takes 100 ms
            (_, "/slow", Mode::Page) => {
                std::thread::sleep(Duration::from_millis(100));
                "ok".to_string()
            }
            _ => String::new(),
        };
        let status = if method == "POST" { "200 OK" } else if body.is_empty() { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: {ct}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// What the page reports, its ports read `PORT`.
async fn answers(mode: Mode, probe: &'static str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, mode, probe));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
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
    serde_json::from_str(&ports_read_port(&got)).expect("the page's report")
}

/// `:` and two to five digits, where no digit follows, read `:PORT` (the recording's normalisation).
fn ports_read_port(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if (2..=5).contains(&n) && !b.get(i + 1 + n).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                out.push_str(":PORT");
                i += 1 + n;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn assert_as_chrome(got: serde_json::Value, chrome: &str) {
    let want: serde_json::Value = serde_json::from_str(chrome).expect("the recorded Chrome answers");
    assert_eq!(got, want, "\n  korvus {got}\n  Chrome {want}");
}

/// The `about:blank` child of a cross-site frame: its `document.domain` is its creator's host (the frame's, not the
/// page's), it is in quirks mode and its `scrollingElement` is its `<body>`, and `window.event` is each window's
/// own -- a listener of the frame sees the frame's event and `undefined` for its child's, and the other way round.
/// This engine answered the page's host, `<html>`, and one current event for every window.
#[tokio::test]
async fn a_frames_blank_child_answers_as_chrome_150s() {
    let got = answers(Mode::Host { delay: Duration::ZERO }, include_str!("fixtures/frame-tasks/blank-child-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/blank-child-chrome-150.json"));
}

/// A frame's timer callback is called on the frame's window -- `this.location` is the frame's -- though the page
/// armed the first timer (every realm's timers run from one pump, in the realm that started it); a frame's
/// `queueMicrotask` callback has no `this`. This engine called a frame's callbacks on the PAGE's window.
#[tokio::test]
async fn a_frames_timer_callbacks_are_called_on_its_own_window_as_in_chrome_150() {
    let got = answers(Mode::Host { delay: Duration::ZERO }, include_str!("fixtures/frame-tasks/timer-this-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/timer-this-chrome-150.json"));
}

/// Long animation frames in a cross-site frame whose document answers after 600 ms: none spans the fetch; the
/// frame's own 80 ms task, the one that renders, the 80 ms handler of a message the page sent it and of its own
/// XMLHttpRequest's `load` are each a `long-animation-frame` of the FRAME (a `task`, a `rendered`, a `task`, a
/// `task`); its same-origin child's 80 ms task is the
/// child's, and the child's task that renders is the frame's -- the cross-site frame is the child's local root; and
/// the page's timeline gets none of them. Durations under 70 ms are not compared (a loaded machine stretches a
/// 30 ms frame past Chrome's 50 ms line). This engine put a frame spanning the fetch on the frame, and every task of
/// the frame on the page.
#[tokio::test]
async fn long_animation_frames_are_their_frames_as_in_chrome_150() {
    let got = answers(Mode::Host { delay: Duration::from_millis(600) }, include_str!("fixtures/frame-tasks/long-animation-frames-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/long-animation-frames-chrome-150.json"));
}

/// A TIMER AND A REQUEST THAT COME DUE TOGETHER ARE TWO TASKS. A same-origin frame's XMLHttpRequest is answered while
/// the page is busy for 300 ms, and a page timer falls due meanwhile; when the page is done, Chrome 150 runs the timer
/// and the request's `load` -- each busy 80 ms -- as tasks of their own, a long animation frame each: the FRAME's and
/// the page's (three runs alike). This engine runs every realm's timers from one pump and a finished request's
/// reactions in the same microtask checkpoint: the two were one task, whichever realm's came first, and the other's
/// 80 ms went on its timeline (`frame_tasks_live`'s long-animation-frame test failed so whenever a loaded machine let a
/// page timer meet the frame's `load`).
#[tokio::test]
async fn a_timer_and_a_request_due_together_are_two_tasks_as_in_chrome_150() {
    let got = answers(Mode::Page, include_str!("fixtures/frame-tasks/merged-tasks-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/merged-tasks-chrome-150.json"));
}

/// HTML's `[[CryptographicNonce]]`, under a header-delivered CSP: the `nonce` IDL attribute reads and writes the slot
/// and never the content attribute; an element connected to the document has its `nonce` attribute emptied, the
/// slot keeping the value (parser-inserted scripts included, and the external one still runs); a clone keeps the
/// slot; an SVG element's `nonce` is the same slot. This engine reflected `nonce` as a plain attribute.
#[tokio::test]
async fn the_nonce_slot_and_its_hiding_as_in_chrome_150() {
    let got = answers(Mode::Csp, include_str!("fixtures/frame-tasks/nonce-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/nonce-chrome-150.json"));
}

/// An `about:blank` frame's clock starts when it is made: its `timeOrigin` 300 ms after its page's, its
/// `performance.now()` small. A page whose only frame is an `about:blank` -- no frame document ever committed a
/// navigation -- read the page's clock in every frame realm.
#[tokio::test]
async fn a_blank_frames_clock_starts_with_it_as_in_chrome_150() {
    let got = answers(Mode::Page, include_str!("fixtures/frame-tasks/child-clock-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/frame-tasks/child-clock-chrome-150.json"));
}

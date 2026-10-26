//! `devicemotion`, `deviceorientation` and `deviceorientationabsolute` as stock Chrome fires them on a desktop with
//! no motion sensor (investigator `testpages` F4: incolumitas' `hc2.js` waits for a `devicemotion` event,
//! which Chrome fires -- every reading null, `interval` 16 -- and this engine never did).
//!
//! A listener ADDED for one of the three types on a window starts that window's controller for the type, which fires
//! ONE trusted null event at every listener of the type and stops; the next listener added starts it again. Only in
//! a secure context, never in a detached frame's window, and only where permissions policy enables `accelerometer`
//! and `gyroscope` (and `magnetometer` for `deviceorientationabsolute`) -- a cross-origin frame needs `allow`, a
//! sandboxed one never hears it. `core/korvus-webapi/src/core/html/device_sensors.rs` has the Chromium sources.
//!
//! The fixtures are Chrome's: `seq` and `frames-secure` 152.0.7977.82's, headed on the Wayland desktop, the others
//! 150.0.7871.46's, headed under Xvfb; a fresh profile, NO DevTools client, three runs alike, from exactly these
//! pages (served the way this file serves them; the page POSTs its object to `/report`, here it is read off
//! `window.__r`). The pages sort what is racy in Chrome: a FRESH browser's
//! first sensor request waits ~60 ms for the platform's device enumeration, and the three types' events then land in
//! the same millisecond in an order that varies run to run.
//!
//! `web-platform.test` resolves to loopback on this box (the WPT hosts file) without being a potentially trustworthy
//! name, so `http://web-platform.test:<port>/` is an insecure origin served by the same loopback server.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const SEQ_PAGE: &str = include_str!("fixtures/device-sensors/seq-page.html");
const FRAMES_PAGE: &str = include_str!("fixtures/device-sensors/frames-page.html");
const FRAME: &str = include_str!("fixtures/device-sensors/frames-frame.html");

fn chrome() -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")
}

/// One connection: `/` is the page, `/f` and `/f?..` the frame, a POST is answered empty (the page's `/report`).
fn serve(s: std::net::TcpStream, page: String, frame: String) {
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
                if k.trim().eq_ignore_ascii_case("content-length") {
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
        let body = if method == "POST" {
            ""
        } else if target == "/" {
            page.as_str()
        } else if target == "/f" || target.starts_with("/f?") {
            frame.as_str()
        } else {
            ""
        };
        let status = if body.is_empty() && method != "POST" { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// Serve `page` (`PORT` written into it and into the frame) at `http://<host>:<port>/`, load it, and read the object
/// it leaves on `window.__r`.
async fn run(page: &str, host: &str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let page = page.replace("PORT", &port.to_string());
    let frame = FRAME.replace("PORT", &port.to_string());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (p, f) = (page.clone(), frame.clone());
            std::thread::spawn(move || serve(s, p, f));
        }
    });
    let url = format!("http://{host}:{port}/");
    let mut e = engine::Engine::new(Box::new(chrome()));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return serde_json::from_str(s).expect("the page's object");
        }
    }
}

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/device-sensors/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect("the Chrome fixture")).expect("JSON")
}

/// ONE WINDOW, STEP BY STEP: the events' shape (trusted, not bubbling, null readings, the motion event's three
/// sub-objects and `interval` 16, the absolute pump's `absolute`), that they land after a 0 ms timer and before a
/// 250 ms one, and -- 300 ms apart -- which listeners hear what each addition, removal and handler assignment starts:
/// a duplicate adds nothing, a capture registration of the same function does; the first non-null `on<type>` starts
/// it and a reassignment does not; a removal before the tick (directly, or by an aborted signal) stops it; a listener
/// added during the dispatch hears nothing; a `document` or `<body>` listener starts nothing. And the constructors:
/// `new DeviceMotionEvent(type, {acceleration: {x: 1}})` makes the sub-object, `requestPermission` is absent.
#[tokio::test]
async fn listeners_hear_the_null_events_chromes_controllers_fire() {
    let got = run(SEQ_PAGE, "127.0.0.1").await;
    let want = fixture("seq-chrome-152.json");
    assert_eq!(got["hidden"], false, "the page must stay visible");
    assert_eq!(got["shapes"], want["shapes"], "the three null events");
    assert_eq!(got["order"], want["order"], "when they land against two timers");
    assert_eq!(got["ctor"], want["ctor"], "the constructors");
    assert_eq!(got["log"], want["log"], "which listeners hear what, step by step");
    assert_eq!(got, want);
}

/// FRAMES, SECURE: a same-origin frame hears all three; a cross-origin one nothing without `allow`, `devicemotion`
/// and `deviceorientation` with `allow="accelerometer; gyroscope"` and all three with `magnetometer` added; `gyroscope
/// 'none'` silences all three, `magnetometer 'none'` the absolute one; a sandboxed frame nothing; a blank frame's
/// window, listened on by the parent, hears all three with the frame's window as `target`; a detached frame's window
/// nothing.
#[tokio::test]
async fn frames_hear_what_their_policy_allows_as_in_chrome() {
    let got = run(FRAMES_PAGE, "127.0.0.1").await;
    let want = fixture("frames-secure-chrome-152.json");
    assert_eq!(got, want);
}

/// AN INSECURE PAGE hears nothing and has no `on<type>` member and no event interface -- and nor does any frame in
/// it, a frame of the potentially trustworthy `http://127.0.0.1` included: Chrome's secure context needs every
/// ancestor's origin trustworthy (`LocalDOMWindow::HasInsecureContextInAncestors`).
///
/// The frames' own `isSecureContext` is left out: Chrome answers `false` for each of them, and this engine's
/// `isSecureContext` (and the `[SecureContext]` exposure it shares) still reads the frame's own URL alone -- a known
/// gap outside this controller, which applies the ancestor rule itself.
#[tokio::test]
async fn an_insecure_page_and_its_frames_hear_nothing_as_in_chrome() {
    let strip = |mut v: serde_json::Value| {
        if let Some(frames) = v["frames"].as_object_mut() {
            for f in frames.values_mut() {
                if let Some(o) = f.as_object_mut() {
                    o.remove("sc");
                }
            }
        }
        v
    };
    let got = strip(run(FRAMES_PAGE, "web-platform.test").await);
    let want = strip(fixture("frames-insecure-chrome-150.json"));
    assert_eq!(got, want);
}

/// AT-TARGET ORDER, which the sequence above rides on (`re-add l1 capture`): a target that is NOT a node -- the page's
/// window, a frame's, an `EventTarget`, an `AbortSignal`, an XHR -- fires every listener in registration order,
/// capturing or not (Blink's `LocalDOMWindow::DispatchEvent` / `EventTarget::DispatchEventInternal`: one
/// `FireEventListeners` pass); a document or an element, connected or not, fires its capturing listeners first (the
/// node path's `DispatchEventAtCapturing` / `AtBubbling`). Script-dispatched and engine-fired (`message`) alike.
#[tokio::test]
async fn listeners_at_the_target_fire_in_chromes_order() {
    let got = run(include_str!("fixtures/device-sensors/at-target-page.html"), "127.0.0.1").await;
    let want = fixture("at-target-chrome-150.json");
    assert_eq!(got, want);
}

/// THE PRODUCT'S PATH, not this file's harness: the probe-page shape (the probe
/// inlined in a classic script, its answer left on `window.__report`) loaded EXACTLY as `korvus fetch` loads a URL
/// (`korvus/src/main.rs` `fetch_cmd`): the navigation fetched with its `navigation_start`, the response handed to
/// `load_page_response` under the response's URL, then the event loop run in 50 ms slices for the `--wait` budget and
/// LEFT when it drains -- where [`run`] above pumps until the page answers. A controller whose reply or tick were not
/// tasks the loop waits for would pass there and hear nothing here.
///
/// The probe is the live verifier's: each way of registering a listener --
/// bare, `window.`, `self.`, `EventTarget.prototype.addEventListener.call(window)`, `ondevicemotion` -- must hear one
/// trusted event. Chrome's rows (three runs alike but for the first row's milliseconds: 79-82, the fresh browser's
/// device enumeration) are `[form, ms, true]`; the milliseconds are checked against the pump's 16.67 ms, not equality.
#[tokio::test]
async fn every_registration_form_hears_the_event_on_the_korvus_fetch_path() {
    let page = include_str!("fixtures/device-sensors/cli-probe-page.html").replace("PROBE", include_str!("fixtures/device-sensors/cli-forms-probe.js"));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let p = page.clone();
            std::thread::spawn(move || serve(s, p, String::new()));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    // `fetch_cmd`, step for step (its ledger reset left out: the ledger is process-wide and these tests run together).
    let mut e = engine::Engine::new(Box::new(chrome()));
    e.set_url(&url);
    let navigation_start = std::time::Instant::now();
    let resp = transport::fetch(transport::FetchRequest {
        navigation_start: Some(navigation_start),
        ..transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())
    })
    .await
    .expect("the navigation");
    let doc_url = if resp.url.is_empty() { url.clone() } else { resp.url.clone() };
    e.load_page_response(&resp.bytes, &doc_url, &[], &resp.headers).await.expect("load");
    let report = |e: &mut engine::Engine| -> Option<String> {
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let until = deadline.min(now + Duration::from_millis(50));
        let end = e.pump(until.saturating_duration_since(now)).await;
        if end.is_err() || matches!(end, Ok(engine::PumpEnd::Drained)) {
            break;
        }
        // (`fetch_cmd` runs out its whole budget; stopping once the page has answered changes nothing it heard.)
        if report(&mut e).is_some() {
            break;
        }
    }
    let got: serde_json::Value = serde_json::from_str(&report(&mut e).expect("the page answered before the loop drained")).expect("JSON");
    let want = fixture("cli-forms-chrome-150.json");
    let rows = |v: &serde_json::Value| v.as_array().expect("rows").iter().map(|r| (r[0].clone(), r[2].clone())).collect::<Vec<_>>();
    assert_eq!(rows(&got), rows(&want), "each registration form hears one trusted event: {got}");
    for r in got.as_array().unwrap() {
        let ms = r[1].as_f64().unwrap_or(-1.0);
        assert!((16.0..=200.0).contains(&ms), "the event lands one pump period after the add, not {r}");
    }
}

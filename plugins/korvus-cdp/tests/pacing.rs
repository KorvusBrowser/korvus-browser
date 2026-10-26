//! FIXER C11: the page runs on its own schedule while a CDP client is attached, a command is answered at the next
//! boundary between two of the page's tasks, and a navigation the page asks for is followed when it asks.
//!
//! `korvus serve` used to pump the page 25 ms out of every 50 (an idle tick), and to keep a loading document's
//! pump running to its `load` or to a 1.5 s budget whatever the page asked for meanwhile. Measured on the two
//! fixtures below with ONE raw CDP client over the page websocket (no domain enabled),
//! three runs each, Chrome 150.0.7871.46 headed vs the pre-fix `korvus serve` (`korvus-b2`):
//!
//! ```text
//!                                              chrome 150 (3 runs)          korvus serve before
//!   setTimeout(f, 10) delay, max               14.0 / 14.5 / 18.9           26.7 / 26.3 / 26.7
//!   requestAnimationFrame interval, min-max    16.5-16.8 (all three)        12.1-38.0 / 16.0-34.5 / 7.9-38.6
//!   Input.dispatchMouseEvent round trip, median   2.1 / 2.4 / 3.3            20.0 / 20.0 / 20.0
//!   reload asked -> its request at the server   3.7 / 3.6 / 2.8              1205.9 / 1219.1 / 1220.0
//! ```
//!
//! `fixtures/pacing.chrome-150.json` holds Chrome's rows. The bounds below sit between the two columns with room for
//! a loaded box (the reasons are at each assertion); `korvus fetch`, whose loop has always run continuously, holds
//! 14.0-14.1 and 15.9-20.6 on the same page.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PACING: &str = include_str!("fixtures/pacing.html");
const RELOAD: &str = include_str!("fixtures/pacing-reload.html");
const CHROME: &str = include_str!("fixtures/pacing.chrome-150.json");
const CSS_WAKE: &str = include_str!("fixtures/css-wake.html");

/// Every request the fixture server saw: when it arrived, and its path.
type Log = Arc<Mutex<Vec<(Instant, String)>>>;

/// The fixture server, on threads of its own: `/` is `page`, `/slow.png` answers after 3 s, `/asked` answers empty.
///
/// NOT a task on the test's runtime: the engine runs on that runtime's one thread, and the reload fixture's
/// synchronous XHR holds it while it waits for `/asked` -- and the stamps must be taken by a clock the page's own
/// work cannot delay.
fn serve_fixture(page: &'static str) -> (u16, Log) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                seen.lock().unwrap().push((Instant::now(), path.clone()));
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).map(|n| n == 0).unwrap_or(true) || h.trim().is_empty() {
                        break;
                    }
                }
                if path == "/slow.png" {
                    std::thread::sleep(Duration::from_secs(3));
                }
                let (status, body) = match path.as_str() {
                    "/" => ("200 OK", page),
                    "/asked" => ("200 OK", ""),
                    _ => ("404 Not Found", ""),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            });
        }
    });
    (port, log)
}

struct Client {
    ws: ClientWs,
    next: i64,
}

impl Client {
    /// One command, answered: its result and how long the answer took.
    async fn call(&mut self, method: &str, params: Value) -> (Value, f64) {
        self.next += 1;
        let id = self.next;
        let t = Instant::now();
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next()).await.expect("an answer within 30 s").unwrap().unwrap();
            if let Message::Text(txt) = msg {
                let v: Value = serde_json::from_str(&txt).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    return (v.get("result").cloned().unwrap_or(Value::Null), t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
    }
}

/// Drive `page` over a fresh `korvus serve` connection with `drive`, and hand back what it returned.
async fn with_page<T, F, Fut>(port: u16, drive: F) -> T
where
    F: FnOnce(Client, String) -> Fut,
    Fut: std::future::Future<Output = (Client, T)>,
{
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let (mut c, out) = drive(Client { ws, next: 0 }, format!("http://127.0.0.1:{port}/")).await;
        c.ws.send(Message::Close(None)).await.unwrap();
        out
    };
    let (server, out) = tokio::join!(server, client);
    server.unwrap();
    out
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// `[min, median, max]` of a page-side series, as `fixtures/pacing.html`'s `q()` writes it.
fn triple(v: &Value) -> [f64; 3] {
    let a = v.as_array().expect("a [min, median, max] triple");
    [a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_f64().unwrap()]
}

#[tokio::test]
async fn the_page_keeps_its_own_cadence_and_input_is_its_next_task() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // The recording the bounds are read against: every Chrome run inside them, with the same arithmetic.
    let chrome: Value = serde_json::from_str(CHROME).unwrap();
    for run in chrome["runs"].as_array().unwrap() {
        let t10 = triple(&run["out"]["t10"]);
        let raf = triple(&run["out"]["raf"]);
        let mut rtt: Vec<f64> = run["move_ms"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        rtt.push(run["press_ms"].as_f64().unwrap());
        rtt.push(run["release_ms"].as_f64().unwrap());
        assert!(t10[2] <= 22.0 && raf[2] <= 25.0 && raf[1] >= 15.0 && raf[1] <= 18.5 && median(rtt) <= 10.0, "Chrome's own run is outside the bounds: {run}");
    }

    let (port, _) = serve_fixture(PACING);
    let (out, rtt) = with_page(port, |mut c, url| async move {
        c.call("Page.navigate", json!({ "url": url })).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        c.call("Runtime.evaluate", json!({ "expression": "__start()", "returnByValue": true })).await;
        let mut rtt = Vec::new();
        for i in 0..16 {
            let p = json!({ "type": "mouseMoved", "x": 100 + 20 * i, "y": 100 + 10 * i, "button": "none", "buttons": 0 });
            rtt.push(c.call("Input.dispatchMouseEvent", p).await.1);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let press = json!({ "type": "mousePressed", "x": 420, "y": 260, "button": "left", "buttons": 1, "clickCount": 1 });
        rtt.push(c.call("Input.dispatchMouseEvent", press).await.1);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let release = json!({ "type": "mouseReleased", "x": 420, "y": 260, "button": "left", "buttons": 0, "clickCount": 1 });
        rtt.push(c.call("Input.dispatchMouseEvent", release).await.1);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let read = json!({ "expression": "JSON.stringify({out: window.__out, ev: ev.join(',')})", "returnByValue": true });
        let (r, _) = c.call("Runtime.evaluate", read).await;
        let out: Value = serde_json::from_str(r["result"]["value"].as_str().expect("the page's report")).unwrap();
        (c, (out, rtt))
    })
    .await;

    // Every event reached the page, in order: the moves, then the press and the release.
    let ev = out["ev"].as_str().unwrap();
    assert!(ev.ends_with("mousemove,mousedown,mouseup") && ev.matches("mousemove").count() >= 16, "events: {ev}");
    assert!(!out["out"].is_null(), "the page's measurement did not finish: {out}");
    let t10 = triple(&out["out"]["t10"]);
    let raf = triple(&out["out"]["raf"]);
    // A 10 ms timer waits at most for the 4 ms task in front of it (Chrome: max 14.0-18.9). The idle tick added the
    // 25 ms the loop was left alone (26.3-26.7).
    assert!(t10[2] <= 22.0, "setTimeout(f, 10) fired {} ms after it was set (Chrome: <= 18.9)", t10[2]);
    // A frame every ~16.7 ms (Chrome 16.5-16.8; `korvus fetch` 15.9-20.6 on this page). The idle tick's frames came
    // in bursts and gaps: 34.5-38.6 at the longest.
    assert!(raf[1] >= 15.0 && raf[1] <= 18.5 && raf[2] <= 25.0, "requestAnimationFrame intervals {raf:?} (Chrome: 16.5-16.8)");
    // A command is the page's next task: it waits for the 4 ms task already running, not for the end of a slice
    // (Chrome's median 2.1-3.3 ms; the idle tick's 20.0).
    let m = median(rtt.clone());
    assert!(m <= 10.0, "Input.dispatchMouseEvent answered in a median {m:.1} ms (Chrome: 2.1-3.3): {rtt:?}");
}

#[tokio::test]
async fn a_navigation_the_page_asks_for_during_its_load_is_followed_at_once() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (port, log) = serve_fixture(RELOAD);
    with_page(port, |mut c, url| async move {
        c.call("Page.navigate", json!({ "url": url })).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        (c, ())
    })
    .await;
    let log = log.lock().unwrap().clone();
    let paths: Vec<&str> = log.iter().map(|(_, p)| p.as_str()).collect();
    let asked = log.iter().find(|(_, p)| p == "/asked").map(|(t, _)| *t).unwrap_or_else(|| panic!("the page never asked: {paths:?}"));
    let reload = log.iter().filter(|(_, p)| p == "/").nth(1).map(|(t, _)| *t).unwrap_or_else(|| panic!("the reload never came: {paths:?}"));
    let lag = reload.saturating_duration_since(asked).as_secs_f64() * 1000.0;
    // Chrome 2.8-3.7 ms; before, the load pump ran on to its 1.5 s budget first (1205.9-1220.0).
    assert!(lag <= 100.0, "the reload's request came {lag:.1} ms after the page asked for it (Chrome: 2.8-3.7)");
}

/// A SETTLED served page sends its CSS transition's events from its own frames, not at the client's next
/// command. A 300 ms transition started by one `Runtime.evaluate` (after a style read, as the page's first render would
/// have made one), then 800 ms in which the client sends nothing: Chrome sends `transitionrun` and `transitionstart`
/// within a couple of frames and `transitionend` ~300 ms in. `korvus serve` stopped pumping a page whose loop had
/// drained -- the events' clock keeps no loop alive -- so it sent none of them in 1.1 s; woken only at boundaries
/// still to come, it sent all three together at ~420 ms.
#[tokio::test]
async fn a_settled_page_sends_its_transition_events_on_time() {
    let (port, _) = serve_fixture(CSS_WAKE);
    let events: Value = with_page(port, |mut c, url| async move {
        c.call("Page.navigate", json!({ "url": url })).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        c.call("Runtime.evaluate", json!({ "expression": "getComputedStyle(document.getElementById('t')).opacity" })).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let start = "window.__t0 = performance.now(); const t = document.getElementById('t'); t.classList.add('off'); getComputedStyle(t).opacity";
        c.call("Runtime.evaluate", json!({ "expression": start, "returnByValue": true })).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let (r, _) = c.call("Runtime.evaluate", json!({ "expression": "JSON.stringify(window.__ev)", "returnByValue": true })).await;
        (c, serde_json::from_str(r["result"]["value"].as_str().expect("the page's list")).unwrap())
    })
    .await;
    let at = |ty: &str| events.as_array().unwrap().iter().find(|e| e[0] == ty).and_then(|e| e[1].as_f64());
    assert!(at("transitionrun").is_some() && at("transitionstart").is_some(), "run and start: {events}");
    let (run, start) = (at("transitionrun").unwrap(), at("transitionstart").unwrap());
    let end = at("transitionend").unwrap_or_else(|| panic!("no transitionend in 800 ms: {events}"));
    assert!(run < 100.0 && start < 100.0, "run {run} / start {start} ms after the style change, Chrome's within two frames: {events}");
    assert!((280.0..480.0).contains(&end), "transitionend {end} ms after the style change, Chrome ~300: {events}");
    assert!(end - start >= 250.0, "the transition's events came together, not 300 ms apart: {events}");
}

/// A page served and left idle transitions on its FIRST class change, as Chrome's does: Chrome's first frame
/// styled every element, so the change starts from that style. Nothing here had styled `#t` since the page loaded --
/// no frame ran a style update, no script read a style -- so the change jumped to the new value with no transition
/// and no event at all. The page's first rendering update now styles it (`cssom::note_page_paint`).
#[tokio::test]
async fn a_page_left_idle_after_its_load_transitions_on_its_first_class_change() {
    let (port, _) = serve_fixture(CSS_WAKE);
    let events: Value = with_page(port, |mut c, url| async move {
        c.call("Page.navigate", json!({ "url": url })).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let start = "window.__t0 = performance.now(); const t = document.getElementById('t'); t.classList.add('off'); getComputedStyle(t).opacity";
        let (r, _) = c.call("Runtime.evaluate", json!({ "expression": start, "returnByValue": true })).await;
        assert_eq!(r["result"]["value"], "1", "the change transitions: opacity is still 1 when it is read");
        tokio::time::sleep(Duration::from_millis(800)).await;
        let (r, _) = c.call("Runtime.evaluate", json!({ "expression": "JSON.stringify(window.__ev)", "returnByValue": true })).await;
        (c, serde_json::from_str(r["result"]["value"].as_str().expect("the page's list")).unwrap())
    })
    .await;
    let at = |ty: &str| events.as_array().unwrap().iter().find(|e| e[0] == ty).and_then(|e| e[1].as_f64());
    let end = at("transitionend").unwrap_or_else(|| panic!("no transition on the first class change: {events}"));
    assert!((280.0..480.0).contains(&end), "transitionend {end} ms after the change, Chrome ~300: {events}");
}

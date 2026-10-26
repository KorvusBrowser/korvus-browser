//! `XMLHttpRequest` INSIDE a worker isolate actually transfers.
//!
//! It used to be a generated no-op shell. The worker realm exposed the interface with the recorded
//! Chrome-151 shape -- constructor, 23 members, right arity, `[native code]` everywhere -- and every
//! one of those members was an `op_noop_method`. `open()` recorded nothing, `send()` sent nothing, and
//! no event ever fired, so a worker test that fetched anything waited forever. Under the accounting we
//! score against, a file that never publishes a result is `0/0`: it does not fail, it LEAVES the
//! denominator, which is why 47 `xhr/*.worker` files were invisible rather than red.
//!
//! The fix is not a new implementation. A worker runtime loads `webapi_core_ext` exactly as the page
//! does (`worker.rs::spawn_real_worker`), so the native cppgc `XMLHttpRequest` class and its ops were
//! already in this isolate, unused, behind the shell. `worker-native.js` wires them up and the
//! generated shell file defers to it; the SHAPE still comes from the capture (see
//! `core/korvus-webapi/tests/worker_surface.rs`, which is the test that would catch it moving).
//!
//! Everything here is measured against a real local HTTP server on the worker's own thread -- the same
//! harness `worker_url_loading.rs` uses -- because an XHR that resolves without a socket is the exact
//! failure this file exists to prevent.
use engine::Engine;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
/// A tiny multi-route HTTP/1.1 server. `routes` maps a path to `(content-type, body)`; anything else is
/// a 404 with a short text body (which is a REAL response, so an XHR to it must still reach DONE with
/// `status === 404` rather than erroring).
async fn serve_routes(listener: TcpListener, routes: HashMap<&'static str, (&'static str, &'static str)>) {
    let routes = Arc::new(routes);
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let routes = routes.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
            let resp = match routes.get(path) {
                Some((ctype, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nX-Korvus-Probe: seen\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => {
                    let body = format!("not found: {path}");
                    format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            };
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

async fn spawn_server(routes: HashMap<&'static str, (&'static str, &'static str)>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_routes(listener, routes));
    addr
}

/// Real workers deliver cross-thread (a fetch + a spawned isolate on its own OS thread), so pump the
/// event loop repeatedly rather than a single `run()`.
async fn pump_until_result(e: &mut Engine, deadline: std::time::Duration) -> Vec<String> {
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out
}

fn result_of(out: &[String]) -> String {
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .cloned()
        .unwrap_or_else(|| format!("RESULT:NONE (output: {out:?})"))
}

/// Drive one worker script and return the `RESULT:` line the page printed from its `onmessage`.
async fn run_worker(routes: HashMap<&'static str, (&'static str, &'static str)>, path: &str) -> String {
    let addr = spawn_server(routes).await;
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "worker-driver",
        format!(
            "const w = new Worker('http://{addr}{path}'); \
             w.onmessage = (ev) => print('RESULT:' + ev.data); \
             w.onerror = (ev) => print('RESULT:WORKER-ERROR:' + ev);"
        ),
    )
    .expect("eval driver");
    let out = pump_until_result(&mut e, std::time::Duration::from_secs(15)).await;
    result_of(&out)
}

/// THE regression: an async `XMLHttpRequest` in a worker reaches readyState 4 with the server's bytes
/// and fires `load`. This is the whole no-verdict mechanism in one assertion -- before the fix the
/// worker sat in `onload` that never came and the page's `onmessage` never fired at all.
#[tokio::test]
async fn worker_xhr_async_get_delivers_the_body() {
    let mut routes = HashMap::new();
    routes.insert("/payload.txt", ("text/plain", "hello-from-worker-xhr"));
    routes.insert(
        "/w.js",
        (
            "application/javascript",
            "const x = new XMLHttpRequest(); \
             x.onload = () => self.postMessage(x.readyState + '|' + x.status + '|' + x.responseText); \
             x.onerror = () => self.postMessage('xhr-error'); \
             x.open('GET', '/payload.txt'); \
             x.send();",
        ),
    );
    let got = run_worker(routes, "/w.js").await;
    assert_eq!(
        got, "RESULT:4|200|hello-from-worker-xhr",
        "a worker XHR must actually transfer and fire load"
    );
}

/// `open()` must record what it was given and `getAllResponseHeaders()` must report what came back --
/// i.e. the object holds REAL state, not the shell's nothing. `X-Korvus-Probe` is set by the server
/// above, so seeing it proves the bytes came off a socket.
#[tokio::test]
async fn worker_xhr_reports_status_url_and_headers() {
    let mut routes = HashMap::new();
    routes.insert("/payload.txt", ("text/plain", "abc"));
    routes.insert(
        "/w.js",
        (
            "application/javascript",
            "const x = new XMLHttpRequest(); \
             x.onload = () => self.postMessage( \
               x.statusText + '|' + (x.responseURL.endsWith('/payload.txt') ? 'url-ok' : 'url-BAD:' + x.responseURL) \
               + '|' + (x.getAllResponseHeaders().includes('x-korvus-probe') ? 'hdr-ok' : 'hdr-BAD')); \
             x.onerror = () => self.postMessage('xhr-error'); \
             x.open('GET', '/payload.txt'); \
             x.send();",
        ),
    );
    let got = run_worker(routes, "/w.js").await;
    assert_eq!(got, "RESULT:OK|url-ok|hdr-ok", "worker XHR must expose real response state");
}

/// `addEventListener` on the XHR, not just the `on<type>` property: the native class has to be a real
/// EventTarget in the worker realm (its prototype chain runs
/// `XMLHttpRequest -> XMLHttpRequestEventTarget -> EventTarget`, and only the last of those has a
/// listener store). A shell instance would register a listener that nothing ever dispatches to.
#[tokio::test]
async fn worker_xhr_is_a_real_event_target() {
    let mut routes = HashMap::new();
    routes.insert("/payload.txt", ("text/plain", "xyz"));
    routes.insert(
        "/w.js",
        (
            "application/javascript",
            "const x = new XMLHttpRequest(); \
             const seen = []; \
             x.addEventListener('readystatechange', () => seen.push('rsc' + x.readyState)); \
             x.addEventListener('load', (ev) => seen.push('load:' + (ev.target === x ? 'self' : 'other'))); \
             x.addEventListener('loadend', () => self.postMessage(seen.join(','))); \
             x.open('GET', '/payload.txt'); \
             x.send();",
        ),
    );
    let got = run_worker(routes, "/w.js").await;
    // The FULL readyState walk, not just DONE. This expectation used to be `rsc4,load:self`, which
    // recorded the engine's own gap rather than a browser's behaviour: `open()` fires a
    // readystatechange for OPENED, and the response fires one for HEADERS_RECEIVED and one for
    // LOADING before DONE. Every ordered-event test in WPT's xhr/ directory records those four, and
    // Track 6.1 made the engine fire them; the assertion is tightened to the real sequence rather
    // than loosened to accept either.
    assert_eq!(
        got, "RESULT:rsc1,rsc2,rsc3,rsc4,load:self",
        "worker XHR events must reach addEventListener with the XHR as target"
    );
}

/// A 404 is a RESPONSE, not a network error: the request still completes and `status` is 404. This is
/// the shape half of the WPT `xhr/` suite depends on, and it is also the assertion that would go red if
/// `send()` ever quietly stopped talking to the transport and started resolving locally.
#[tokio::test]
async fn worker_xhr_reports_a_404_as_a_completed_response() {
    let mut routes = HashMap::new();
    routes.insert(
        "/w.js",
        (
            "application/javascript",
            "const x = new XMLHttpRequest(); \
             x.onloadend = () => self.postMessage(x.status + '|' + x.readyState); \
             x.open('GET', '/nope.txt'); \
             x.send();",
        ),
    );
    let got = run_worker(routes, "/w.js").await;
    assert_eq!(got, "RESULT:404|4", "a 404 must complete with status 404, not error out");
}

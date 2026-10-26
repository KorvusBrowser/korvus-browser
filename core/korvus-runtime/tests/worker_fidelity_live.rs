//! A worker's script, its imports, its blob URLs and its errors, as Chrome 150 shapes them (the live
//! a challenge provider challenge, 2026-10-07: the challenge frame's workers run its checks).
//!
//! Each `fixtures/worker-fidelity/<probe>.html` is loaded UNMODIFIED from a loopback server here and in
//! `google-chrome --headless=new` 150.0.7871.46 (Python's `http.server` over the same directory);
//! `<probe>.chrome-150.txt` is Chrome's `window.__r` -- sorted, except `perr`'s -- RECORDED 2.5 s after
//! load, two runs identical, with the origin written `ORIGIN` and each blob UUID `UUID`. A line that
//! differs is a finding about the engine, never a reason to edit the file.
//!
//! RE-RECORDED 2026-10-10 (werr, werr2, werr3, werr5, perr): the first recording ENABLED the DevTools Runtime
//! domain before load, which makes V8 capture stacks for uncaught exceptions -- and Blink then sites an Error
//! where it was constructed. Without it, as a user's Chrome runs, every one is sited where it was THROWN (`throw
//! new Error(...)` at the `throw`, six columns earlier). The other four fixtures re-recorded byte-identical.
//!
//! ```text
//! wstack  a worker's stack frames name its script's URL (korvus: `at f (worker:1:23)`), and its
//!         relative `import()` resolves against it
//! blob4   a CLASSIC worker's `import()` of its own blob URL, its page's, and a `data:` module
//!         (korvus: "Module loading is not supported.")
//! blob3   a URL a worker minted reads while it runs and fails once it is terminated
//! werr    eval / Function stacks, unhandledrejection, self.onerror + listener + page, per error
//! werr2   a top-level throw or an unhandled rejection does not end the worker (korvus: it did)
//! werr3   ErrorEvent at the page with the worker's message and site (korvus: "", 0, 0); a 404 is a
//!         plain Event; a syntax error is an ErrorEvent with its line
//! werr4   a module worker: load / parse / link failures are plain Events, a throw (its own or an
//!         import's) an ErrorEvent naming the module
//! werr5   an error nothing handled climbs: the Worker object, the creator's global, its creator's ...
//!         up to window.onerror
//! perr    a page script's thrown Error is reported where it was thrown
//! ```
//!
//! Re-record: serve the directory (`python3 -m http.server`), open each probe in headless Chrome over CDP
//! WITHOUT enabling the Runtime domain before the navigation, wait 2.5 s, evaluate
//! `JSON.stringify(window.__r.slice().sort(), null, 1)` (`perr`: `window.__r`), and normalize the origin and
//! the UUIDs as above.
use engine::Engine;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const FILES: &[(&str, &str)] = &[
    ("/wstack.html", include_str!("fixtures/worker-fidelity/wstack.html")),
    ("/wstack.js", include_str!("fixtures/worker-fidelity/wstack.js")),
    ("/blob3.html", include_str!("fixtures/worker-fidelity/blob3.html")),
    ("/blob4.html", include_str!("fixtures/worker-fidelity/blob4.html")),
    ("/werr.html", include_str!("fixtures/worker-fidelity/werr.html")),
    ("/werr.js", include_str!("fixtures/worker-fidelity/werr.js")),
    ("/werr2.html", include_str!("fixtures/worker-fidelity/werr2.html")),
    ("/werr3.html", include_str!("fixtures/worker-fidelity/werr3.html")),
    ("/werr4.html", include_str!("fixtures/worker-fidelity/werr4.html")),
    ("/mthrow.mjs", include_str!("fixtures/worker-fidelity/mthrow.mjs")),
    ("/werr5.html", include_str!("fixtures/worker-fidelity/werr5.html")),
    ("/perr.html", include_str!("fixtures/worker-fidelity/perr.html")),
];

/// One HTTP/1.1 exchange, `Connection: close`: a fixture by path, 404 for anything else.
fn serve_one(mut s: std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let resp = match FILES.iter().find(|(p, _)| *p == path) {
        Some((p, body)) => {
            let ty = if p.ends_with(".html") { "text/html" } else { "text/javascript" };
            format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        }
        None => "HTTP/1.1 404 File not found\r\nContent-Type: text/html\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found".to_string(),
    };
    let _ = s.write_all(resp.as_bytes());
    let _ = s.flush();
}

/// The fixtures, served on their OWN OS threads (a worker's `importScripts` blocks its isolate's thread).
fn serve() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve_one(s));
        }
    });
    origin
}

/// Load `<name>.html` and pump until its `window.__r` is the recorded Chrome list -- or 10 s, then assert
/// on what it is.
async fn probe(name: &str, chrome: &str, sorted: bool) {
    let want: Vec<String> = serde_json::from_str(chrome).expect("the recorded Chrome output is a JSON list");
    let origin = serve();
    let html = FILES.iter().find(|(p, _)| *p == format!("/{name}.html")).map(|(_, b)| *b).unwrap();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(html, &format!("{origin}/{name}.html"), &[]).await.expect("load_page");
    let list = if sorted { "window.__r.slice().sort()" } else { "window.__r" };
    let read = format!(
        "print('R:' + JSON.stringify({list}.map(s => s.split(location.origin).join('ORIGIN')\
         .replace(/[0-9a-f]{{8}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}/g, 'UUID'))))"
    );
    let start = Instant::now();
    let mut got: Vec<String> = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        e.eval("probe-read", read.clone()).unwrap();
        if let Some(l) = e.take_output().into_iter().rev().find(|l| l.starts_with("R:")) {
            got = serde_json::from_str(&l[2..]).unwrap_or_default();
        }
        if got == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(got, want, "{name}: korvus vs Chrome 150 (fixtures/worker-fidelity/{name}.chrome-150.txt)");
}

#[tokio::test]
async fn a_worker_script_is_named_by_its_url() {
    probe("wstack", include_str!("fixtures/worker-fidelity/wstack.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_classic_worker_imports_modules() {
    probe("blob4", include_str!("fixtures/worker-fidelity/blob4.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_workers_blob_urls_end_with_the_worker() {
    probe("blob3", include_str!("fixtures/worker-fidelity/blob3.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_worker_reports_its_errors_where_they_happened() {
    probe("werr", include_str!("fixtures/worker-fidelity/werr.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_worker_keeps_running_after_a_throw_or_a_rejection() {
    probe("werr2", include_str!("fixtures/worker-fidelity/werr2.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_worker_error_reaches_the_page_as_chrome_shapes_it() {
    probe("werr3", include_str!("fixtures/worker-fidelity/werr3.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_module_worker_fails_to_load_as_an_event_and_throws_as_an_error_event() {
    probe("werr4", include_str!("fixtures/worker-fidelity/werr4.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn an_unhandled_worker_error_climbs_to_window_onerror() {
    probe("werr5", include_str!("fixtures/worker-fidelity/werr5.chrome-150.txt"), true).await;
}

#[tokio::test]
async fn a_page_script_throw_is_reported_where_it_was_thrown() {
    probe("perr", include_str!("fixtures/worker-fidelity/perr.chrome-150.txt"), false).await;
}

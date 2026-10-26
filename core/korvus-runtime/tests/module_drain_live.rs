//! The module drain's deadline, and what Chrome 150 actually bounds.
//!
//! `fixtures/module-drain-probe.corpus.txt` is ONE set of bytes served to BOTH engines: to
//! `google-chrome --headless` 150.0.7871.46 over a loopback server that answers a path beginning
//! `/d<ms>/` that many milliseconds late, and here over a `TcpListener` this test binds itself and
//! delays the same way. `fixtures/module-drain-probe.chrome-150.txt` is Chrome's answer, RECORDED —
//! two runs, identical — and this test asserts korvus prints the same lines.
//!
//! A line that differs is a finding about the engine, never a reason to edit the file.
//!
//! # What was wrong (IM1 measured it, MD1 fixed it)
//!
//! `drain_modules` computed `now + MODULE_DRAIN_BUDGET` once, before its loop, and then loaded each
//! queued module script's graph IN TURN. So 2,000 ms bounded the SUM of every module script's
//! import fetches on the page, and a page with several module scripts spent the whole budget on the
//! first few while every later one timed out having waited for nothing —
//! `Uncaught Error: module <url> load module: timed out fetching imports`, 6 live rows at
//! `chrome_rows = 0`.
//!
//! **The serialization was the defect; the constant was not.** The drain now WARMS every queued
//! module's static imports before it loads any of them (see `Engine::warm_queued_module_imports`),
//! so independent graphs are fetched concurrently and the one budget bounds the SLOWEST graph
//! instead of the sum. `MODULE_DRAIN_BUDGET` is unchanged at 2,000 ms, so the worst case a drain
//! admits is exactly the one the harness watchdogs were sized against.
//!
//! # THE SIX LIVE ROWS, on the one instrument that can reach them
//!
//! The cluster is live-only — none of the six pages has a recording, so the replay tier cannot see
//! it at all. Measured instead with the release binary's own reader,
//! `korvus fetch <url> --errors --wait 12`, one host at a time, three reps per arm, 2026-09-21
//! 08:44–08:56, loadavg 0.46–8.00; the base arm is the binary built at `744b5d2a`:
//!
//! ```text
//!   host                  base (3 reps)   after (3 reps)
//!   society6.com          0   0   0        0   0   0
//!   aftonbladet.se        6   6   6        0   0   0
//!   elcorteingles.es      0   0   0        0   0   0
//!   otto.de              44  44  44       21  21  21
//!   rei.com               0   0   0        0   0   0
//!   vg.no                13  13  13        0   0   0
//! ```
//!
//! Absolute rates per arm (R40), on THIS instrument and not on the parity driver: the message fires
//! on **3 of 6 hosts before and 1 of 6 after**, 63 occurrences to 21, 3/3 reps identical in both
//! arms. `otto.de` is the one that remains, and it is not the same defect: its 16 distinct
//! timed-out modules are a micro-frontend's graphs across three origins, `date-fns` chains
//! included, whose DEPTH exceeds 2 s even with every level fetched in parallel. Closing that one
//! means changing what the bound is, not how the drain uses it.
//!
//! # WHAT CHROME 150 ACTUALLY BOUNDS — measured 2026-09-21, loadavg 1.0–3.3, over the corpus above
//!
//! Chrome 150.0.7871.46, `--headless --disable-gpu --no-sandbox --virtual-time-budget=…
//! --dump-dom`, wall clock of the whole process:
//!
//! ```text
//!   page        shape                                   loaded      wall
//!   /index      6 modules x 1 import x 700 ms           all six     0.93 s / 0.91 s
//!   /three      3 modules x 1 import x 700 ms           all three   0.94 s / 0.99 s
//!   /chain      1 module  x 3-deep chain x 300 ms       yes         1.18 s / 1.16 s
//!   /long5      1 module  x 1 import x 5000 ms          yes         5.24 s
//!   /six5       6 modules x 1 import x 5000 ms          all six     5.24 s
//!   /long30     1 module  x 1 import x 30000 ms         yes        30.24 s
//! ```
//!
//! **So Chrome bounds this NOWHERE — not per module, not per navigation.** 30 s of import fetch is
//! served and run; six of them at once cost the same 5.2 s as one, which is the second reading and
//! the important one. The server's own request log says why, to the millisecond:
//!
//! ```text
//!   REQ …804.074 /index.html
//!   REQ …804.081 /d700/dep1.mjs   REQ …804.081 /d700/dep4.mjs      <- all six within 7 ms of the
//!   REQ …804.081 /d700/dep2.mjs   REQ …804.081 /d700/dep5.mjs         document, in one round
//!   REQ …804.081 /d700/dep3.mjs   REQ …804.081 /d700/dep6.mjs
//!
//!   REQ …809.790 /d300/c1.mjs     REQ …810.133 /d300/c2.mjs        <- and a CHAIN is serial, one
//!   REQ …810.435 /d300/c3.mjs                                         hop at a time, as it must be
//! ```
//!
//! Independent graphs are fetched in parallel; a chain inside one graph is not. That is the shape
//! this engine now has, and `/chain.html` is in the compared transcript so that a "fix" which
//! flattened a real dependency chain would be caught.
//!
//! **A module whose import NEVER answers costs Chrome the whole document.** `/hang.html`'s import is
//! answered by nothing at all; Chrome was stopped at 6 s and at 20 s (`--timeout`, which issues a
//! stop and dumps whatever exists) and both times `<pre id=out>` was EMPTY and the title unchanged —
//! **`DOMContentLoaded` had not fired**, let alone `load`. With `--dump-dom` and no stop it produced
//! no output at all in 60 s. The control matters here (R41): the same instrument on `/index.html`
//! at a 6 s stop printed all eight lines, so an empty `<pre>` is a reading and not a broken probe.
//!
//! korvus cannot follow Chrome there and should not: the runtime is driven by callers that must
//! return (`pump(50 ms)` in the WPT runner, `pump_until_loaded(1500 ms)` in CDP's navigate), and a
//! drain that waits forever hands them a hang instead of a page. What it does instead is bounded:
//! after `MODULE_DRAIN_BUDGET` the module is reported as a timed-out import and the document goes on
//! to `DOMContentLoaded`. **That divergence is recorded, not asserted** — `/long5.html`,
//! `/six5.html`, `/long30.html` and `/hang.html` are in the corpus, are served by the same two
//! steps, and are deliberately NOT in the compared transcript, because matching Chrome on them
//! would mean having no bound at all.
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CORPUS: &str = include_str!("fixtures/module-drain-probe.corpus.txt");
const CHROME: &str = include_str!("fixtures/module-drain-probe.chrome-150.txt");

/// The pages whose transcripts are compared, in the order the fixture records them.
const COMPARED: [&str; 3] = ["/index.html", "/three.html", "/chain.html"];

/// The corpus file's `=== <path> <content-type>` sections, in order. The body is everything between
/// one header and the next, verbatim — the same parse that lays the corpus out as a directory for
/// Chrome (the corpus file's own header carries the script), so both engines are handed the
/// identical bytes rather than two copies that can drift.
fn corpus() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in CORPUS.lines() {
        if let Some(header) = line.strip_prefix("=== ") {
            let mut parts = header.split(' ');
            let path = parts.next().unwrap_or_default().to_string();
            let ctype = parts.next().unwrap_or("text/plain").to_string();
            out.push((path, ctype, String::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.2.push_str(line);
            last.2.push('\n');
        }
    }
    out
}

/// Every path the engine asked the fixture server for, in order. The point of recording it is the
/// one thing a prefetch could plausibly get wrong: warming an import means taking the DOCUMENT'S
/// module-map entry early, so the loader finds the fetch in flight — if it instead minted a second
/// one, the page would make two requests where a browser makes one, and the transcript would not
/// show it.
type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// THE DELAY IS IN THE PATH, and this is the half of that rule the Rust driver implements: the
/// Chrome recording's server reads the same `/d<ms>/` prefix out of the same corpus paths. A
/// `/hang/` path is never answered — the connection is held open and the request is left pending,
/// which is what "a module that never answers" has to mean on the wire.
fn delay_of(path: &str) -> Option<std::time::Duration> {
    let rest = path.strip_prefix("/d")?;
    let (ms, _) = rest.split_once('/')?;
    ms.parse::<u64>().ok().map(std::time::Duration::from_millis)
}

/// Serve the corpus until the listener is dropped. One task per connection, because the engine opens
/// several at once for a page's module graphs and a serial loop would deadlock the drain against
/// itself — and measuring concurrency on a server that cannot be concurrent measures nothing.
async fn serve(listener: TcpListener, files: std::sync::Arc<Vec<(String, String, String)>>, log: RequestLog) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let files = files.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = sock.read(&mut buf).await else { return };
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(path.clone());
            if path.starts_with("/hang/") {
                // Held, never answered, and never closed either: a closed socket is a network error,
                // which is a different measurement.
                std::future::pending::<()>().await;
                return;
            }
            if let Some(d) = delay_of(&path) {
                tokio::time::sleep(d).await;
            }
            let resp = match files.iter().find(|(p, _, _)| *p == path) {
                Some((_, ctype, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
            };
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

/// Drive the engine until the probe page has filled its `<pre id=out>`, and answer its text.
async fn pump_for_out(e: &mut Engine) -> String {
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        let v = e
            .eval_value(
                "probe",
                "(() => { const p = document.getElementById('out'); return p && p.textContent ? p.textContent : null; })()".to_string(),
            )
            .unwrap_or(serde_json::Value::Null);
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            return "<the probe page never filled #out within 30s>".to_string();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// The fixture, split into `(page, lines)` on its `== <page>` headers.
fn chrome_blocks() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in CHROME.lines() {
        if let Some(page) = line.strip_prefix("== ") {
            out.push((page.trim().to_string(), Vec::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.1.push(line.to_string());
        }
    }
    out
}

#[tokio::test]
async fn independent_module_graphs_load_the_way_chrome_150_loads_them() {
    let files = std::sync::Arc::new(corpus());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log: RequestLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    tokio::spawn(serve(listener, files.clone(), log.clone()));

    let want = chrome_blocks();
    assert_eq!(
        want.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        COMPARED,
        "the fixture's page blocks and this test's compared set must be the same list, in order"
    );

    // What each page's imports ARE, so that "exactly these, once each" can be asserted of a run
    // rather than read off it. Each navigation is a fresh document and therefore a fresh module
    // map, which is why the log is taken per page and not once at the end.
    let expected_requests: std::collections::HashMap<&str, Vec<&str>> = [
        (
            "/index.html",
            vec![
                "/d700/dep1.mjs",
                "/d700/dep2.mjs",
                "/d700/dep3.mjs",
                "/d700/dep4.mjs",
                "/d700/dep5.mjs",
                "/d700/dep6.mjs",
                // The document's own favicon, which Chrome asks for on these pages too (it is in
                // the recording server's log beside the imports). Named rather than filtered, so
                // that "exactly these and nothing else" stays exactly that.
                "/favicon.ico",
            ],
        ),
        ("/three.html", vec!["/d700/dep1.mjs", "/d700/dep2.mjs", "/d700/dep3.mjs", "/favicon.ico"]),
        ("/chain.html", vec!["/d300/c1.mjs", "/d300/c2.mjs", "/d300/c3.mjs", "/favicon.ico"]),
    ]
    .into_iter()
    .collect();

    let mut diffs: Vec<String> = Vec::new();
    let mut req_findings: Vec<String> = Vec::new();
    let mut lines = 0usize;
    for (page, want_lines) in &want {
        log.lock().unwrap().clear();
        let html = files
            .iter()
            .find(|(p, _, _)| p == page)
            .map(|(_, _, b)| b.clone())
            .unwrap_or_else(|| panic!("the corpus carries {page}"));
        let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
        e.run().await.expect("boot");
        let doc = format!("http://{addr}{page}");
        e.load_page(&html, &doc, &[]).await.expect("load_page");
        let got = pump_for_out(&mut e).await;
        let got_lines: Vec<&str> = got.lines().collect();
        for i in 0..want_lines.len().max(got_lines.len()) {
            lines += 1;
            let w = want_lines.get(i).map(|s| s.as_str()).unwrap_or("<missing>");
            let g = got_lines.get(i).copied().unwrap_or("<missing>");
            if w != g {
                diffs.push(format!("  {page} line {}\n    chrome: {w}\n    korvus: {g}", i + 1));
            }
        }
        // ONE REQUEST PER IMPORT, WARMED OR NOT. The prefetch takes the SAME module-map entry the
        // loader takes, so a warmed import is the one request issued earlier and never a second
        // one. This is what would fire if `warm_queued_module_imports` minted its own fetch instead
        // of going through `modules::module_map_entry`, and it is also what would fire if the scan
        // resolved a specifier differently from the loader and warmed a URL the page never names.
        let mut got_requests: Vec<String> = log.lock().unwrap().clone();
        got_requests.sort();
        let want_requests: Vec<String> = expected_requests[page.as_str()].iter().map(|s| s.to_string()).collect();
        if got_requests != want_requests {
            req_findings.push(format!(
                "  {page} asked for\n    {got_requests:?}\n  and the page's imports, once each, are\n    {want_requests:?}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} of {} transcript lines differ from Chrome 150:\n{}",
        diffs.len(),
        lines,
        diffs.join("\n")
    );
    assert!(req_findings.is_empty(), "{}", req_findings.join("\n"));
}

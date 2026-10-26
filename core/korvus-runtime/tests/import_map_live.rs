//! `<script type="importmap">`, and HTML's "resolve a module specifier".
//!
//! `fixtures/import-map-probe.corpus.txt` is ONE set of bytes served to BOTH engines: to
//! `google-chrome --headless` 150.0.7871.46 over a loopback `python3 -m http.server`, and here over a
//! `TcpListener` this test binds itself. `fixtures/import-map-probe.chrome-150.txt` is Chrome's
//! answer, RECORDED — two runs, identical — and this test asserts korvus prints the same 47 lines.
//!
//! A line that differs is a finding about the engine, never a reason to edit the file. The only
//! normalization is the origin: Chrome was served on a fixed port and this test binds port 0, so both
//! drivers rewrite their own `http://127.0.0.1:<port>` to `<origin>` before the comparison.
//!
//! ## What was wrong
//!
//! korvus read no import map at all. `HTMLScriptElement.supports("importmap")` answered `true`,
//! `script_type_or_language_runs` correctly declined to run the element, and nothing ever looked at
//! its text — so every bare specifier reached deno_core's `resolve_import` and came back
//!
//! ```text
//! Relative import path "@theme/section-hydration" not prefixed with / or ./ or ../
//! ```
//!
//! which is what a browser WITHOUT import maps says. Six rows of the 513-row live corpus died on it,
//! at `chrome_rows = 0` on every one. The five pages behind those rows are named in
//! a demand map and not here (M6: no corpus hostname in engine source);
//! each ships one inline import map in its initial HTML, read on 2026-09-21, one fetch apiece.
//!
//! ## What is measured here, one probe line each
//!
//! Exact keys, trailing-slash prefix keys, the longest key winning whatever order it was declared in,
//! the backtracking guard, URL-like specifiers being remapped too, `scopes` tried most-specific-first
//! with fall-through, several maps merging with the FIRST one keeping its keys, a map the page
//! INJECTS, an external `src=` map being refused, the three ways one resolution failure is worded,
//! the two JS error types a bad map is reported with (`SyntaxError` for invalid JSON, `TypeError`
//! for a JSON value of the wrong shape), and the RESOLVED MODULE SET — a late map may not re-point
//! a specifier that has already resolved, may not reach it with a prefix key either, and is
//! unaffected by a resolution that failed. The rules themselves, and the Chrome readings behind
//! each, are in `webapi`'s `core/html/import_map.rs`.
//!
//! ## Two Chrome readings this engine does NOT yet reproduce, recorded rather than asserted
//!
//! Both are about how a FAILED resolution is REPORTED, not about what resolves, and both are older
//! than this unit — they are the module drain's error wrapper, which is shared with syntax errors and
//! fetch failures and is not this lane's to move:
//!
//! ```text
//!                                    chrome 150                        korvus
//!   inline module, bare specifier    Uncaught TypeError: Failed to     Uncaught Error: module <url>:
//!                                    resolve module specifier "X".     load module: Failed to resolve
//!                                    Relative references must …        module specifier "X". Relative …
//!   external module, bare specifier  uncaught TypeError, then the      no uncaught error, and the
//!                                    element fires `load`              element fires `error`
//! ```
//!
//! Chrome treats an unresolvable graph exactly as it treats a SYNTAX error — the script loaded, it
//! simply could not be linked — and korvus's drain classifies it as a fetch failure. Measured on the
//! same day, on probe pages `p24`/`p25`; the sentence itself now matches, the wrapper and the element
//! event do not.
//!
//! And one that is unreachable rather than wrong: Chrome does NOT apply a document's import map
//! inside a WORKER (`Failed to resolve module specifier 'dep'` from a `{type:"module"}` worker whose
//! page had a map). korvus has one `JsRuntime` for every realm, so the map WOULD be visible there —
//! but a module worker in this engine posted no message at all on the same probe, so the divergence
//! has no path to a page today. Recorded, not asserted, and not invented.
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CORPUS: &str = include_str!("fixtures/import-map-probe.corpus.txt");
const CHROME: &str = include_str!("fixtures/import-map-probe.chrome-150.txt");

/// The corpus file's `=== <path> <content-type>` sections, in order. The body is everything between
/// one header and the next, verbatim — the same eight-line parse that lays the corpus out as a
/// directory for Chrome (the corpus file's own header says how), so both engines are handed the
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

/// Serve the corpus until the listener is dropped. One task per connection, because the engine opens
/// several at once for a module graph and a serial loop would deadlock the drain against itself.
async fn serve(listener: TcpListener, files: std::sync::Arc<Vec<(String, String, String)>>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let files = files.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = sock.read(&mut buf).await else { return };
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let resp = match files.iter().find(|(p, _, _)| *p == path) {
                Some((_, ctype, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
            };
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

/// Drive the engine until the probe page has appended its `<pre id=out>`, and answer its text.
async fn pump_for_out(e: &mut Engine) -> String {
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        let v = e
            .eval_value("probe", "(() => { const p = document.getElementById('out'); return p ? p.textContent : null; })()".to_string())
            .unwrap_or(serde_json::Value::Null);
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            panic!("the probe page never appended #out within 60s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn import_maps_resolve_what_chrome_150_resolves() {
    let files = std::sync::Arc::new(corpus());
    let index = files
        .iter()
        .find(|(p, _, _)| p == "/index.html")
        .map(|(_, _, b)| b.clone())
        .expect("the corpus carries /index.html");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, files));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("http://{addr}/index.html");
    e.load_page(&index, &doc, &[]).await.expect("load_page");
    let got = pump_for_out(&mut e).await.replace(&format!("http://{addr}"), "<origin>");

    let got_lines: Vec<&str> = got.lines().collect();
    let want_lines: Vec<&str> = CHROME.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..want_lines.len().max(got_lines.len()) {
        let w = want_lines.get(i).copied().unwrap_or("<missing>");
        let g = got_lines.get(i).copied().unwrap_or("<missing>");
        if w != g {
            diffs.push(format!("  chrome: {w}\n  korvus: {g}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} of {} lines differ from Chrome 150:\n{}",
        diffs.len(),
        want_lines.len(),
        diffs.join("\n")
    );
}

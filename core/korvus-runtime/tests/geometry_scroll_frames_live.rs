//! `getBoundingClientRect` and the hit test answer in client coordinates after a scroll,
//! and a frame's document is laid out into the frame.
//!
//! # The oracle
//!
//! `fixtures/p13a/probe.js` is evaluated UNMODIFIED here and in stock Chrome 150.0.7871.46 by
//! `fixtures/p13a/record.py`, which launches the binary itself (`--remote-debugging-port`, a throwaway
//! profile, headed under Xvfb at 1920x1080), attaches with `suppress_origin=True`, and runs
//! each section on a fresh browser against a loopback `page.html` (plus the `frame.html` one frame
//! loads), one `Runtime.evaluate (awaitPromise)` per phase. `chrome-150.<section>.txt` is what it wrote;
//! two consecutive recordings were byte-identical. Every box sits at a fixed position in a document
//! far larger than either engine's window, so no number depends on the window's size.
//!
//! # Before this unit
//!
//! `#a.getBoundingClientRect()` after `scrollTo(50, 900)` answered `10,1000,60,30` where Chrome answers
//! `-40,100,60,30`, and every element inside every frame answered `0,0,0,0`.

use engine::{Engine, PumpEnd};

const PROBE: &str = include_str!("fixtures/p13a/probe.js");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/p13a");
const DOC: &[u8] = b"<!doctype html><html><head></head><body></body></html>";
const FRAME: &[u8] = br#"<!doctype html><style>html,body{margin:0}</style><div id="in" style="position:absolute;left:15px;top:25px;width:40px;height:30px"></div>"#;
/// `plugins/korvus-cdp`'s bounded pump before every `Runtime.evaluate`.
const EVAL_PUMP: std::time::Duration = std::time::Duration::from_millis(100);

/// One HTTP/1.1 exchange: `page.html` and `frame.html`, everything else a 404.
fn serve_one(mut s: std::net::TcpStream) {
    use std::io::{Read, Write};
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
    let path = head.split_whitespace().nth(1).and_then(|p| p.split('?').next()).unwrap_or("").to_string();
    let body: Option<&[u8]> = match path.as_str() {
        "/page.html" => Some(DOC),
        "/frame.html" => Some(FRAME),
        _ => None,
    };
    let h = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        if body.is_some() { "200 OK" } else { "404 Not Found" },
        if body.is_some() { "text/html" } else { "text/plain" },
        body.map_or(9, |b| b.len())
    );
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body.unwrap_or(b"not found"));
    let _ = s.flush();
}

/// A loopback server on its own OS thread, so nothing it does waits on the isolate's thread.
fn start_server() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || serve_one(s));
        }
    });
    addr
}

/// A fresh engine on the empty loopback document -- the page every fixture was recorded on.
async fn served_page() -> Engine {
    let addr = start_server();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    let url = format!("http://{addr}/page.html");
    e.set_url(&url);
    e.load_page(std::str::from_utf8(DOC).unwrap(), &url, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(std::time::Duration::from_secs(5)).await;
    e
}

/// One `Runtime.evaluate (awaitPromise, returnByValue)`, as `korvus serve` performs it.
async fn evaluate(e: &mut Engine, expr: String) -> String {
    let _ = e.pump(EVAL_PUMP).await;
    let out = e.eval_remote_awaited_details(expr, true).await;
    if let Some(x) = out.exception {
        return format!("EVAL THREW {}", x.text);
    }
    match out.result {
        engine::RemoteResult::Value(serde_json::Value::String(s)) => s,
        engine::RemoteResult::Value(v) => v.to_string(),
        engine::RemoteResult::Object(_) => "<object>".to_string(),
    }
}

/// Every phase of `section`, joined the way `record.py` writes them.
async fn run_section(e: &mut Engine, section: &str, phases: u32) -> String {
    let mut lines = Vec::new();
    for phase in 0..phases {
        let expr = format!("({PROBE})({}, {phase})", serde_json::to_string(section).unwrap());
        lines.push(format!("-- phase {phase}"));
        lines.push(evaluate(e, expr).await);
    }
    lines.join("\n")
}

fn fixture(section: &str) -> String {
    let p = std::path::Path::new(FIXTURES).join(format!("chrome-150.{section}.txt"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())).trim_end().to_string()
}

/// Line-for-line equality with Chrome's recording.
fn assert_lines(section: &str, got: &str) {
    let want = fixture(section);
    let g: Vec<&str> = got.lines().collect();
    let w: Vec<&str> = want.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..g.len().max(w.len()) {
        let (a, b) = (w.get(i).copied().unwrap_or("<missing>"), g.get(i).copied().unwrap_or("<missing>"));
        if a != b {
            diffs.push(format!("  chrome: {a}\n  korvus: {b}"));
        }
    }
    assert!(diffs.is_empty(), "[{section}] {} of {} lines differ:\n{}", diffs.len(), w.len(), diffs.join("\n"));
}

/// The document scrolled to `(50, 900)` and two scroll containers scrolled: `getBoundingClientRect`,
/// `getClientRects` and a Range's rects in client coordinates (an absolutely positioned box, a fixed
/// one, a scroller's content, an absolute child of a static scroller, a wrapped inline, a sticky box
/// held at its inset); `offsetTop` still in document coordinates; `elementFromPoint` /
/// `elementsFromPoint` at client points, clipped by the scroller that hides a child; the same boxes
/// back where layout put them after `scrollTo(0, 0)`.
#[tokio::test]
async fn geometry_after_scrolling_matches_chrome() {
    let mut e = served_page().await;
    assert_lines("scroll", &run_section(&mut e, "scroll", 2).await);
}

/// Five frames -- `srcdoc` with and without its own styles, a padded and bordered one, one taller
/// than itself, a same-origin `src`, and an initial `about:blank` written into by its parent: each
/// frame window's `innerWidth`/`innerHeight`, its document element's client and scroll sizes, the
/// rects and offsets of the elements inside, and its document's hit test. Then a frame resized and its
/// content changed, and a frame scrolled by its own window while the page stays put.
#[tokio::test]
async fn frame_documents_are_laid_out_as_chrome_lays_them_out() {
    let mut e = served_page().await;
    assert_lines("frames", &run_section(&mut e, "frames", 3).await);
}

// ── the efficiency box ───────────────────────────────────────────────────────────────────────────
//
// `Engine::layout_counts` is `(page layouts, frame layouts)`: counts, not a clock.

/// A page with NO frames lays out no frame document, and lays itself out once per change it measures
/// after -- not once per question, and not once per scroll.
#[tokio::test]
async fn a_page_without_frames_lays_out_nothing_extra() {
    let mut e = served_page().await;
    let v = e
        .eval_value(
            "p13a-no-frames",
            r#"for (let i = 0; i < 40; i++) { const d = document.createElement('div'); d.style.height = (i + 1) * 10 + 'px'; document.body.appendChild(d); }
               const els = Array.from(document.body.children); let sum = 0;
               for (const d of els) sum += d.getBoundingClientRect().height;
               sum"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, 8200);
    let (page_a, frames_a) = e.layout_counts();
    assert_eq!(frames_a, 0, "a page with no frame laid out a frame document");
    // Forty more questions, and ten scrolls between them: no layout at all.
    e.eval_value(
        "p13a-scroll-and-ask",
        r#"for (let i = 0; i < 10; i++) { scrollTo(0, i * 50); for (const d of document.body.children) d.getBoundingClientRect(); document.elementFromPoint(5, 5); } 1"#.to_string(),
    )
    .unwrap();
    assert_eq!(e.layout_counts(), (page_a, 0), "scrolling and asking again laid the page out again");
    // One change, then questions: exactly one more layout.
    e.eval_value("p13a-change", r#"document.body.firstElementChild.style.height = '7px'; for (const d of document.body.children) d.getBoundingClientRect(); 1"#.to_string())
        .unwrap();
    assert_eq!(e.layout_counts(), (page_a + 1, 0));
    for _ in 0..3 {
        assert_eq!(e.pump(std::time::Duration::from_millis(500)).await.unwrap(), PumpEnd::Drained);
    }
    assert_eq!(e.layout_counts(), (page_a + 1, 0), "an idle page laid something out");
}

/// A page WITH a frame that nothing measures inside lays out no frame document either; the first
/// question inside the frame lays it out once, and asking again does not.
#[tokio::test]
async fn a_frame_is_laid_out_only_when_something_inside_it_is_measured() {
    let mut e = served_page().await;
    e.eval_value(
        "p13a-frame",
        r#"globalThis.__loaded = false; const f = document.createElement('iframe'); f.id = 'f';
           f.srcdoc = '<!doctype html><div id="in" style="width:50px;height:50px"></div>';
           f.onload = () => { globalThis.__loaded = true; }; document.body.appendChild(f); 1"#
            .to_string(),
    )
    .unwrap();
    for _ in 0..50 {
        if e.eval_value("p13a-loaded", "__loaded".to_string()).unwrap() == true {
            break;
        }
        let _ = e.pump(std::time::Duration::from_millis(100)).await;
    }
    e.eval_value("p13a-page-only", "for (let i = 0; i < 20; i++) document.getElementById('f').getBoundingClientRect(); document.elementFromPoint(10, 10); 1".to_string())
        .unwrap();
    let (page, frames) = e.layout_counts();
    assert!(page >= 1);
    assert_eq!(frames, 0, "a frame nobody measured in was laid out");
    let r = e
        .eval_value("p13a-inside", "const d = document.getElementById('f').contentDocument; const r = d.getElementById('in').getBoundingClientRect(); [r.x, r.y, r.width, r.height].join()".to_string())
        .unwrap();
    assert_eq!(r, "8,8,50,50");
    assert_eq!(e.layout_counts(), (page, 1));
    e.eval_value("p13a-again", "const d2 = document.getElementById('f').contentDocument; for (let i = 0; i < 20; i++) d2.getElementById('in').getBoundingClientRect(); 1".to_string())
        .unwrap();
    assert_eq!(e.layout_counts(), (page, 1), "asking again inside the frame laid it out again");
}

/// `<iframe scrolling=no>` lays its document out with no scrollbars: reCAPTCHA's 400x580 challenge frame holds a
/// document two pixels taller than itself, and Chrome 150 shows neither bar -- the frame's `clientWidth` is its whole
/// width -- where korvus drew both and cut the tile grid's last column. A frame without the attribute keeps its bar.
/// Chrome 150, the same two frames: `200x100` and `185x100`, both scrolling 102.
#[tokio::test]
async fn a_frame_that_says_scrolling_no_shows_no_scrollbars() {
    let mut e = served_page().await;
    let got = evaluate(
        &mut e,
        r#"(async () => {
            const doc = "<body style='margin:0'><div style='height:102px'></div></body>";
            const a = document.createElement("iframe");
            a.setAttribute("scrolling", "no");
            const b = document.createElement("iframe");
            for (const f of [a, b]) {
                f.style.cssText = "width:200px;height:100px;border:0";
                f.srcdoc = doc;
                document.body.appendChild(f);
            }
            await Promise.all([a, b].map(f => new Promise(r => f.onload = r)));
            return [a, b].map(f => { const d = f.contentDocument.documentElement; return d.clientWidth + "x" + d.clientHeight + " " + d.scrollHeight; }).join(" | ");
        })()"#
            .to_string(),
    )
    .await;
    assert_eq!(got, "200x100 102 | 185x100 102");
}

//! The modern cascade and the loads a stylesheet induces, in a live engine over a
//! loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expectation is stock Chrome 150.0.7871.46 headless over CDP on the same markup, served from
//! a loopback server that logs paths (the unit's `css.html` / `t1.html` / `t3.html`):
//!
//! ```text
//! getComputedStyle(<p>).marginTop                 16px              (korvus before: 0px)
//! @layer base { .l { color } } .l {}              rgb(1, 2, 3)      (rgb(0, 0, 0))
//! @supports (display:grid) { .s { color } }       rgb(4, 5, 6)      (rgb(0, 0, 0))
//! .n { & .c { color } }                           rgb(7, 8, 9)      (rgb(0, 0, 0))
//! margin-inline-start: 11px / padding-block-end   11px / 12px       (0px / 0px)
//! @import url(imp.css) -> .i { color }            rgb(10, 11, 12)   (rgb(0, 0, 0))
//! new CSSStyleSheet().insertRule("@keyframes …")  0                 (SyntaxError)
//! document.querySelectorAll("& > div").length     0                 (SyntaxError)
//! requests                                        /css.html /imp.css /bg-used.png /used.woff2
//!                                                 (+ /favicon.ico, which is not a stylesheet load)
//! load event with a 400 ms font and a 300 ms background image    after both
//! ```

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

const PAGE: &str = r#"<!doctype html>
<html><head>
<style>
@import url(imp.css);
@layer base { .l { color: rgb(1, 2, 3) } }
.l { }
@supports (display:grid) { .s { color: rgb(4, 5, 6) } }
.n { & .c { color: rgb(7, 8, 9) } }
.lp { margin-inline-start: 11px; padding-block-end: 12px }
@font-face { font-family: Used; src: url(used.woff2) format("woff2") }
@font-face { font-family: Unused; src: url(unused.woff2) format("woff2") }
@font-face { font-family: NoneFace; src: url(none.woff2) format("woff2") }
.fu { font-family: Used }
.fn { font-family: NoneFace; display: none }
.used { background: url(bg-used.png) }
.unused { background-image: url(bg-unused.png) }
.hid { background-image: url(bg-none.png); display: none }
</style>
</head><body>
<p id=p>para</p>
<div class=l id=l>l</div>
<div class=s id=s>s</div>
<div class=n><div class=c id=c>c</div></div>
<div class=lp id=lp>lp</div>
<div class=i id=i>i</div>
<div class=fu>font used</div>
<div class=fn>font none</div>
<div class=used>bg</div>
<div class=hid>hid</div>
</body></html>"#;

const SLOW_PAGE: &str = r#"<!doctype html>
<html><head>
<link rel=stylesheet href="sub/linked.css">
<style>
@font-face { font-family: Slow; src: url(slow.woff2) }
.f { font-family: Slow }
</style>
<script>window.__t = []; addEventListener('load', () => __t.push(Math.round(performance.now())));</script>
</head><body><div class=f>text</div><div class=b>b</div></body></html>"#;

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if let Ok(mut l) = log.lock() {
                l.push(path.clone());
            }
            let (ctype, body, delay) = match path.as_str() {
                "/page.html" => ("text/html", PAGE.to_string(), 0),
                "/slow.html" => ("text/html", SLOW_PAGE.to_string(), 0),
                "/imp.css" => ("text/css", ".i { color: rgb(10, 11, 12) }".to_string(), 0),
                "/sub/linked.css" => ("text/css", ".b { background-image: url(rel.png) }".to_string(), 0),
                "/slow.woff2" => ("font/woff2", "x".to_string(), 400),
                "/sub/rel.png" => ("image/png", "x".to_string(), 300),
                p if p.ends_with(".png") => ("image/png", "x".to_string(), 0),
                p if p.ends_with(".woff2") => ("font/woff2", "x".to_string(), 0),
                _ => ("text/plain", "no".to_string(), 0),
            };
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

async fn load(path: &str) -> (Engine, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, log.clone()));
    let doc = if path == "/slow.html" { SLOW_PAGE } else { PAGE };
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(doc, &format!("http://{addr}{path}"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(8)).await;
    (e, log)
}

async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("p10a-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    e.take_output().into_iter().next().unwrap_or_default()
}

#[tokio::test]
async fn every_row_of_the_fixture_answers_chromes_way_and_requests_what_chrome_requests() {
    let (mut e, log) = load("/page.html").await;
    let cs = |id: &str, p: &str| format!("getComputedStyle(document.getElementById('{id}')).{p}");
    assert_eq!(ask(&mut e, &cs("p", "marginTop")).await, "16px");
    assert_eq!(ask(&mut e, &cs("l", "color")).await, "rgb(1, 2, 3)");
    assert_eq!(ask(&mut e, &cs("s", "color")).await, "rgb(4, 5, 6)");
    assert_eq!(ask(&mut e, &cs("c", "color")).await, "rgb(7, 8, 9)");
    assert_eq!(ask(&mut e, &cs("lp", "marginLeft")).await, "11px");
    assert_eq!(ask(&mut e, &cs("lp", "paddingBottom")).await, "12px");
    assert_eq!(ask(&mut e, &cs("i", "color")).await, "rgb(10, 11, 12)");
    assert_eq!(
        ask(&mut e, "new CSSStyleSheet().insertRule('@keyframes x { from { opacity: 0 } to { opacity: 1 } }')").await,
        "0"
    );
    assert_eq!(ask(&mut e, "document.querySelectorAll('& > div').length").await, "0");
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules[0].constructor.name").await, "CSSImportRule");
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules[1].constructor.name").await, "CSSLayerBlockRule");

    let mut got: Vec<String> = log.lock().unwrap().clone();
    got.sort();
    // `load_page` is handed the document, so the page itself is not in this log. `/favicon.ico` is:
    // the page declares no icon, and Chrome 150 requests the default one for it after `load`
    // (`core/html/link_icons.rs`).
    assert_eq!(got, vec!["/bg-used.png", "/favicon.ico", "/imp.css", "/used.woff2"], "the access log");
}

#[tokio::test]
async fn the_load_event_waits_for_a_used_font_and_a_matched_background_resolved_against_its_sheet() {
    let (mut e, log) = load("/slow.html").await;
    let got = log.lock().unwrap().clone();
    assert!(got.contains(&"/sub/rel.png".to_string()), "url() resolves against the sheet's URL: {got:?}");
    assert!(got.contains(&"/slow.woff2".to_string()), "{got:?}");
    let t: u64 = ask(&mut e, "__t[0]").await.parse().expect("load fired");
    assert!(t >= 400, "load at {t} ms, before the 400 ms font settled");
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.querySelector('.b')).backgroundImage.includes('/sub/rel.png')").await,
        "true",
        "the computed url() is absolute, as Chrome serializes it"
    );
}

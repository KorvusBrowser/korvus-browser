//! The input a parser-blocking `document.write`'d `<script src>` holds back is scanned for its loads while
//! the script is still being fetched, as Chrome's preload scanner reads written input. korvus requested them only
//! once the script had run and the held-back nodes were inserted: WPT
//! `html/syntax/speculative-parsing/generated/document-write/link-rel-preload-as-font-crossorigin`, which Chrome 150
//! passes, writes a 1.5 s script and then a font preload, and lost to its own `load`-time read of the server's record
//! in most runs ("speculative case did not fetch").
//!
//! Here a script writes a 600 ms script, then a font preload and an image. Both are asked for before the slow
//! script's response is served, and each exactly once: the elements join the scanned requests when they are inserted.
//! And a `<meta http-equiv=Content-Security-Policy>` in the held-back input holds the whole scan, as Blink holds every
//! queued preload until the parser has applied it (R27 v2): `<meta ... img-src 'none'><img>` never asks for the image.
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

const PAGE: &str = "<!doctype html><html><head><script>\
document.write('<script src=\"/slow.js\"><\\/script><link rel=preload as=font crossorigin href=\"/f.woff2?w=1\"><img src=\"/i.gif?w=1\">');\
</script></head><body><script>addEventListener('load', function () { setTimeout(function () { window.DONE = 'done'; }, 300); });</script></body></html>";
/// The same written shape behind a written `img-src 'none'` policy (WPT speculative-parsing document-write/meta-csp-img-src-none).
const CSP_PAGE: &str = "<!doctype html><html><head><script>\
document.write('<script src=\"/slow.js\"><\\/script><meta http-equiv=\"Content-Security-Policy\" content=\"img-src \\'none\\'\"><img src=\"/i.gif?w=2\">');\
</script></head><body><script>addEventListener('load', function () { setTimeout(function () { window.DONE = 'done'; }, 300); });</script></body></html>";
/// A 1x1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(target.clone());
            let (ct, body): (&str, &[u8]) = if target.starts_with("/slow.js") {
                tokio::time::sleep(Duration::from_millis(600)).await;
                log.lock().unwrap().push("slow-done".to_string());
                ("text/javascript", &b"window.slow = 1;"[..])
            } else if target.starts_with("/f.woff2") {
                ("font/woff2", &b"wOF2"[..])
            } else if target.starts_with("/i.gif") {
                ("image/gif", GIF)
            } else {
                ("text/html", PAGE.as_bytes())
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn written_input_behind_a_blocking_script_is_scanned_for_its_loads() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(l, log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(PAGE, &format!("{origin}/page.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if e.eval_value("written-scan", "window.DONE || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).is_some_and(|d| d == "done") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the page never loaded");
    }
    let log = log.lock().unwrap().clone();
    let at = |what: &str| log.iter().position(|t| t.starts_with(what));
    let count = |what: &str| log.iter().filter(|t| t.starts_with(what)).count();
    let slow_done = at("slow-done").expect("the slow script was served");
    assert!(at("/f.woff2?w=1").is_some_and(|i| i < slow_done), "the font preload was asked for while the script was: {log:?}");
    assert!(at("/i.gif?w=1").is_some_and(|i| i < slow_done), "the image was asked for while the script was: {log:?}");
    assert_eq!((count("/f.woff2?w=1"), count("/i.gif?w=1")), (1, 1), "each once, the inserted elements joining: {log:?}");
}

#[tokio::test]
async fn a_written_csp_meta_holds_the_scan_of_written_input() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(l, log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(CSP_PAGE, &format!("{origin}/page.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if e.eval_value("written-csp", "window.DONE || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).is_some_and(|d| d == "done") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the page never loaded");
    }
    let log = log.lock().unwrap().clone();
    assert!(log.iter().any(|t| t == "slow-done"), "the slow script was served: {log:?}");
    // neither the scan (held by the policy) nor the inserted <img> (refused by it) asks for the image
    assert!(!log.iter().any(|t| t.starts_with("/i.gif?w=2")), "the image behind img-src 'none' was requested: {log:?}");
}

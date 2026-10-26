//! A font request is shared within ONE document, not across documents. An interning change kept a font's
//! response for the life of the document, so a second `@font-face` or preload of the URL joins the first request
//! (`subresource::DocumentRequests`; Blink's `ResourceFetcher`: "Avoid making multiple requests for the same URL during
//! the initial load"). But the table was the PROCESS's, so a frame document's font preload of a URL the page had already
//! asked for joined the page's request and never reached the server. Across documents Blink's memory cache serves a
//! response only while it is fresh, and one with no caching headers is not.
//!
//! WPT `html/syntax/speculative-parsing/generated/{document-write,page-load}/link-rel-preload-as-font-crossorigin`,
//! which Chrome 150 passes and korvus passed earlier, preloads one URL in two documents and expects the second request
//! ("non-speculative case did not fetch" after it). Here: the page and its same-origin frame each preload one font
//! URL with no caching headers, and the server sees two requests for it.
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

const PAGE: &str = "<!doctype html><html><head><link rel=preload as=font crossorigin href=\"/f.woff2?k=1\"></head><body>\
<script>var f = document.createElement('iframe'); f.src = '/frame.html'; document.body.appendChild(f);\
var t = setInterval(function () { try { if (f.contentWindow.slow === 1) { clearInterval(t); setTimeout(function () { window.DONE = 'done'; }, 300); } } catch (e) {} }, 50);\
</script></body></html>";
const FRAME: &str = "<!doctype html><html><head><link rel=preload as=font crossorigin href=\"/f.woff2?k=1\"><script src=\"/slow.js\"></script></head><body>f</body></html>";

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
            let (ct, body): (&str, &[u8]) = if target.starts_with("/frame.html") {
                ("text/html", FRAME.as_bytes())
            } else if target.starts_with("/slow.js") {
                tokio::time::sleep(Duration::from_millis(400)).await;
                ("text/javascript", &b"window.slow = 1;"[..])
            } else if target.starts_with("/f.woff2") {
                ("font/woff2", &b"wOF2"[..])
            } else {
                ("text/html", PAGE.as_bytes())
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_frames_font_preload_is_its_own_request() {
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
        if e.eval_value("font-doc", "window.DONE || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).is_some_and(|d| d == "done") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the frame never loaded");
    }
    let fonts = log.lock().unwrap().iter().filter(|t| t.starts_with("/f.woff2")).count();
    assert_eq!(fonts, 2, "one font request per document: {:?}", log.lock().unwrap());
}

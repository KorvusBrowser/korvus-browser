//! An image document's `<img>` is fed from the navigation's own response. Blink's `ImageDocumentParser` hands
//! the main resource's bytes to the document's image element (`HTMLImageElement::StartLoadingImageDocument`), whose
//! loader takes them instead of fetching (`ImageLoader::UpdateFromElement`: "Prevent the creation of a ResourceLoader
//! (and therefore a network request) for ImageDocument loads"). korvus built the document as markup and its `<img src>`
//! asked the network again: a frame showing an image made two GETs where Chrome 150 makes one, and a page navigated to
//! an image requested it once more after its navigation had.
//!
//! The image is answered `no-store`, so a second request cannot be a cache's. Read at the frame's `load`: the image is
//! complete with its natural size, and the image document's own timeline holds no resource entry (no request was made).
//! The element takes the response ONCE: no request means no CSP check for it, and any other load in that document is
//! an ordinary one, CSP included. And the page's preload scanner, which an image document does not have, is the next
//! ordinary page's again (`performance_entries_live::parser_subresources_leave_in_document_order_once_each`'s shape).
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

/// A 3x2 RGBA PNG.
const PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x03\x00\x00\x00\x02\x08\x06\x00\x00\x00\x9d\x74\x66\x1a\x00\x00\x00\x11\x49\x44\x41\x54\x78\x9c\x63\xf8\xcf\xc0\xf0\x1f\x86\x19\x90\x39\x00\x9b\x7e\x0b\xf5\x72\xb0\xb9\x3c\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

const PAGE: &str = "<!doctype html><html><body>\
<iframe src=\"/i.png?f\" onload=\"var d = this.contentDocument, i = d.images[0];\
window.DONE = JSON.stringify([d.images.length, i.complete, i.naturalWidth, i.naturalHeight, this.contentWindow.performance.getEntriesByType('resource').map(function (e) { return e.initiatorType + ' ' + e.name.replace(location.origin, ''); })]);\"></iframe>\
</body></html>";

/// The image document `/i.png?fc` is served with `img-src 'none'`; at its `load` a second image is put in it.
const CSP_PAGE: &str = "<!doctype html><html><body>\
<iframe src=\"/i.png?fc\" onload=\"var d = this.contentDocument, i = d.images[0], a = [i.complete, i.naturalWidth, i.naturalHeight];\
var j = d.createElement('img'); d.body.appendChild(j);\
j.onload = function () { window.DONE = JSON.stringify(a.concat(['load', j.naturalWidth])); };\
j.onerror = function () { window.DONE = JSON.stringify(a.concat(['error', j.naturalWidth])); };\
j.src = '/i.png?f2';\"></iframe>\
</body></html>";

/// The persona `performance_entries_live` reads the preload scanner's order with.
const CHROME: &str = "chrome-152-linux-20261016T1022";

/// An ordinary page: with the preload scanner its font preload and image leave in tree order with the parser's own
/// stylesheet and scripts; the element sweep would ask for them after those.
const SCANNED_PAGE: &str = "<!doctype html><html><head><link rel=preload href=f.ttf as=font crossorigin><link rel=stylesheet href=a.css>\
<script src=b.js></script><script async src=c.js></script></head><body>text<img src=n.png></body></html>";

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
            let (ct, body): (&str, &[u8]) = if target.starts_with("/i.png") {
                ("image/png", PNG)
            } else if target == "/n.png" {
                // slow, as an image a page's body names usually is (`performance_entries_live::serve_logged`)
                tokio::time::sleep(Duration::from_millis(30)).await;
                ("image/png", PNG)
            } else if target.ends_with(".css") {
                ("text/css", &b"p{color:red}"[..])
            } else if target.ends_with(".js") {
                ("text/javascript", &b"0"[..])
            } else if target.ends_with(".ttf") {
                ("application/octet-stream", &b"x"[..])
            } else {
                ("text/html", PAGE.as_bytes())
            };
            let csp = if target == "/i.png?fc" { "Content-Security-Policy: img-src 'none'\r\n" } else { "" };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\n{csp}Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body).await;
            let _ = s.flush().await;
        });
    }
}

async fn started() -> (String, Log, Engine) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(l, log.clone()));
    let p = persona::CapturedPersona::select(Some(CHROME)).expect("chrome bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("boot");
    (origin, log, e)
}

/// Pump `e` until `expr` evaluates to a non-empty string, and return it.
async fn wait_for(e: &mut Engine, expr: &str) -> String {
    let start = std::time::Instant::now();
    loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("image-doc", expr.to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if !v.is_empty() {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "never ready: {expr}");
    }
}

fn gets(log: &Log, target: &str) -> usize {
    log.lock().unwrap().iter().filter(|t| *t == target).count()
}

#[tokio::test]
async fn a_frame_showing_an_image_requests_it_once() {
    let (origin, log, mut e) = started().await;
    e.load_page(PAGE, &format!("{origin}/page.html"), &[]).await.expect("load_page");
    let got = wait_for(&mut e, "window.DONE || ''").await;
    // one image, complete at the frame's `load` with its 3x2 size, and no resource entry in the image document
    assert_eq!(got, "[1,true,3,2,[]]", "the image document at its load; server: {:?}", log.lock().unwrap());
    // let a late second request arrive if there is one
    let _ = e.pump(Duration::from_millis(300)).await;
    assert_eq!(gets(&log, "/i.png?f"), 1, "the frame's navigation is the image's one request: {:?}", log.lock().unwrap());
}

#[tokio::test]
async fn an_image_documents_own_image_is_no_csp_load_and_its_next_one_is() {
    let (origin, log, mut e) = started().await;
    e.load_page(CSP_PAGE, &format!("{origin}/page.html"), &[]).await.expect("load_page");
    let got = wait_for(&mut e, "window.DONE || ''").await;
    // the document's own image is shown under `img-src 'none'`; the image put in it after is refused, never requested
    assert_eq!(got, "[true,3,2,\"error\",0]", "the image document under img-src 'none'");
    let _ = e.pump(Duration::from_millis(300)).await;
    assert_eq!((gets(&log, "/i.png?fc"), gets(&log, "/i.png?f2")), (1, 0), "{:?}", log.lock().unwrap());
}

#[tokio::test]
async fn a_page_navigated_to_an_image_does_not_request_it_again() {
    let (origin, log, mut e) = started().await;
    let headers = vec![("content-type".to_string(), "image/png".to_string())];
    e.load_page_response(PNG, &format!("{origin}/i.png?t"), &[], &headers).await.expect("load");
    let ready = "(function () { var i = document.images[0]; return i && i.complete && document.readyState === 'complete' ? \
                 JSON.stringify([document.images.length, i.naturalWidth, i.naturalHeight, performance.getEntriesByType('resource').map(function (e) { return e.initiatorType + ' ' + e.name.replace(location.origin, ''); }).filter(function (n) { return n.indexOf('favicon') < 0; })]) : ''; })()";
    let got = wait_for(&mut e, ready).await;
    // (the page's favicon request after its `load` is the favicon handler's, as `performance_entries_live` filters it)
    assert_eq!(got, "[1,3,2,[]]", "the image document after its load; server: {:?}", log.lock().unwrap());
    let _ = e.pump(Duration::from_millis(300)).await;
    // the response was handed in: the engine itself asks the server for nothing
    assert_eq!(gets(&log, "/i.png?t"), 0, "the navigation's response is the image's: {:?}", log.lock().unwrap());
    // THE NEXT, ORDINARY PAGE HAS ITS PRELOAD SCANNER BACK, read as `performance_entries_live` reads it: the font
    // preload and the image in tree order with the stylesheet and the scripts, each asked once.
    let html = [("content-type".to_string(), "text/html".to_string())];
    e.load_page_delivered(SCANNED_PAGE, &format!("{origin}/next.html"), &[], &html).await.expect("load");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    for _ in 0..10 {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
    }
    let order = e
        .eval_value("order", "performance.getEntriesByType('resource').map(e => e.name.split('/').pop()).filter(n => n != 'favicon.ico').join(',')".into())
        .expect("eval");
    assert_eq!(order, "f.ttf,a.css,b.js,c.js,n.png", "the ordinary page after the image document lost its preload scanner");
    let once = ["/f.ttf", "/a.css", "/b.js", "/c.js", "/n.png"].iter().all(|t| gets(&log, t) == 1);
    assert!(once, "each requested once: {:?}", log.lock().unwrap());
}

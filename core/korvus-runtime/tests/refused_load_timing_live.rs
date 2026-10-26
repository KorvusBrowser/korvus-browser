//! A load the document's CSP refuses -- or whose URL has dangling markup -- is never requested and still has its
//! resource-timing entry, in the refusal shape (Blink `ResourceFetcher::RequestResource`: a blocked request's timing is
//! added with `response_end` = now, resource_fetcher.cc 1433-1446). korvus made none, and it requested a refused
//! head stylesheet (and applied it), a refused preload, a refused parser script and a refused style-sheet image.
//!
//! Chrome 150.0.7871.46 x3, headed, no DevTools (out/chrome-{1,2,3}.json; every count and field alike,
//! only the order of the preload scanner's first batch races):
//! - a refused MARKUP `<link rel=stylesheet>`, `<link rel=preload>`, `<script src>`, `<img>` and a dangling-markup `<img>`:
//!   two entries each, the preload scanner's and the element's; the head sheet's and script's read "blocking";
//! - script-inserted: an `<img>`, `<script>`, stylesheet, preload, a `FontFace` source (`css`), an XHR, a style sheet's
//!   `background-image` (`css`), a frame (`iframe`, its error page): one each; a `fetch()`: none;
//! - a frame document under its own CSP: its refused markup `<img>` twice on the FRAME's timeline, never on the page's;
//! - every refused entry: duration 0 (a frame's excepted), status 0, sizes 0, `nextHopProtocol` and `contentType` "";
//! - the server sees no refused URL.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x54, 0xa2, 0x4f, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const PAGE: &str = "<!doctype html><meta charset=utf-8>
<link rel=stylesheet href=\"/no/mc.css\"><link rel=stylesheet href=\"/ok/mc.css\"><link rel=stylesheet nonce=n1 href=\"/nonce/nc.css\">
<link rel=preload as=image href=\"/no/mp.png\">
<script src=\"/no/ms.js\"></script><script src=\"/ok/ms.js\"></script>
<body>
<img src=\"/no/m.png\"><img src=\"/ok/m.png\"><img src=\"/ok/dm.png?a
<b\">
<iframe src=\"/ok/fr.html\"></iframe>
<script>
var out = {};
var frameRep = null;
window.addEventListener('message', function (e) { frameRep = e.data; });
function el(tag, attrs, parent) {
  return new Promise(function (res) {
    var n = document.createElement(tag), fired = false;
    var done = function () { if (!fired) { fired = true; res(); } };
    n.onload = done; n.onerror = done; setTimeout(done, 3000);
    for (var k in attrs) n.setAttribute(k, attrs[k]);
    (parent || document.body).appendChild(n);
  });
}
function row(e) {
  var refusal = e.duration === 0 && e.responseStatus === 0 && e.transferSize === 0 && e.encodedBodySize === 0 && e.decodedBodySize === 0
    && e.requestStart === 0 && e.responseStart === 0 && e.nextHopProtocol === '' && e.contentType === '';
  // a refused frame navigation ends in an error page: its entry has a duration, and nothing else
  if (e.initiatorType === 'iframe') refusal = e.responseStatus === 0 && e.transferSize === 0 && e.requestStart === 0 && e.contentType === '';
  return e.name.replace(location.origin, '') + ' ' + e.initiatorType + (refusal ? ' refused' : '') + (e.renderBlockingStatus === 'blocking' ? ' blocking' : '');
}
window.addEventListener('load', async function () {
  var n = document.createElement('img'); n.src = '/no/sync.png'; document.body.appendChild(n);
  out.sync = performance.getEntriesByName(location.origin + '/no/sync.png').length;
  await Promise.resolve();
  out.micro = performance.getEntriesByName(location.origin + '/no/sync.png').length;
  await el('img', {src: '/no/i.png'});
  await el('img', {src: '/ok/di.png?a\\n<b'});
  await el('script', {src: '/no/s.js'});
  await el('link', {rel: 'stylesheet', href: '/no/c.css'}, document.head);
  await el('link', {rel: 'preload', as: 'image', href: '/no/p.png'}, document.head);
  try { await new FontFace('nf', 'url(/no/f.ttf)').load(); } catch (e) {}
  try { await fetch('/no/d.json'); } catch (e) {}
  await new Promise(function (res) { var x = new XMLHttpRequest(); x.open('GET', '/no/x.json'); x.onloadend = res; x.send(); });
  var bg = document.createElement('div'); bg.style.cssText = 'width:10px;height:10px;background-image:url(/no/bg.png)'; document.body.appendChild(bg);
  getComputedStyle(bg).backgroundImage;
  await new Promise(function (r) { setTimeout(r, 300); });
  await el('iframe', {src: '/no/f.html'});
  await new Promise(function (r) { setTimeout(r, 300); });
  out.page = performance.getEntriesByType('resource').map(row).filter(function (s) { return s.indexOf('/no/') >= 0 || s.indexOf('/nonce/') >= 0 || s.indexOf('dm.png') >= 0 || s.indexOf('di.png') >= 0; }).sort();
  out.frame = frameRep;
  window.RESULT = JSON.stringify(out);
});
</script>";

const FRAME: &str = "<!doctype html><meta charset=utf-8><body><img src=\"/no/fm.png\"><img src=\"/ok/fm.png\"><script>
window.addEventListener('load', function () {
  setTimeout(function () {
    parent.postMessage(performance.getEntriesByType('resource').map(function (e) { return e.name.replace(location.origin, '') + ' ' + e.initiatorType + ' ' + e.responseStatus; }).sort(), '*');
  }, 100);
});
</script>";

async fn serve(listener: TcpListener, hits: Arc<Mutex<Vec<String>>>) {
    let ok = format!("http://{}/ok/", listener.local_addr().unwrap());
    // `'nonce-n1'`: a nonce'd head sheet the URL list refuses loads all the same (WPT style-src-stylesheet-nonce-allowed)
    let csp = format!("default-src 'none'; script-src 'unsafe-inline' {ok}; style-src {ok} 'nonce-n1'; img-src {ok}; font-src {ok}; connect-src {ok}; frame-src {ok}");
    let frame_csp = format!("img-src {ok}; script-src 'unsafe-inline'");
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let (csp, frame_csp, hits) = (csp.clone(), frame_csp.clone(), hits.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split(' ').nth(1).unwrap_or("/").to_string();
            hits.lock().unwrap().push(path.clone());
            let bare = path.split('?').next().unwrap_or("").to_string();
            let (body, ty, policy): (Vec<u8>, &str, Option<&str>) = match bare.as_str() {
                "/" => (PAGE.as_bytes().to_vec(), "text/html; charset=utf-8", Some(&csp)),
                "/ok/fr.html" => (FRAME.as_bytes().to_vec(), "text/html; charset=utf-8", Some(&frame_csp)),
                p if p.ends_with(".png") => (PNG.to_vec(), "image/png", None),
                p if p.ends_with(".css") => (b"p{color:red}".to_vec(), "text/css", None),
                p if p.ends_with(".js") => (b"window.okjs = 1;".to_vec(), "text/javascript", None),
                p if p.ends_with(".html") => (b"<p>f".to_vec(), "text/html", None),
                _ => (Vec::new(), "text/plain", None),
            };
            let policy = policy.map(|p| format!("Content-Security-Policy: {p}\r\n")).unwrap_or_default();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\n{policy}Cache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_refused_load_is_never_requested_and_has_its_entry() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(l, hits.clone()));
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(30)).await;
        if let Some(r) = e.eval_value("r", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "no result");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    let hits = hits.lock().unwrap().clone();
    let refused_on_wire: Vec<&String> = hits.iter().filter(|h| h.starts_with("/no/") || h.contains("dm.png") || h.contains("di.png")).collect();
    assert!(refused_on_wire.is_empty(), "refused URLs were requested: {refused_on_wire:?} (entries {got})");
    assert!(hits.iter().any(|h| h == "/nonce/nc.css"), "the nonce'd sheet was not requested: {hits:?}");
    // the frame refused nothing but /no/fm.png
    assert_eq!(got["frame"], serde_json::json!(["/no/fm.png img 0", "/no/fm.png img 0", "/ok/fm.png img 200"]), "the frame's timeline (page {got})");
    // a refused iframe's entry has a duration (its error page loaded); every other refused entry is in the refusal shape
    assert_eq!(
        got["page"],
        serde_json::json!([
            "/no/bg.png css refused",
            "/no/c.css link refused",
            "/no/f.html iframe refused",
            "/no/f.ttf css refused",
            "/no/i.png img refused",
            "/no/m.png img refused",
            "/no/m.png img refused",
            "/no/mc.css link refused blocking",
            "/no/mc.css link refused blocking",
            "/no/mp.png link refused",
            "/no/mp.png link refused",
            "/no/ms.js script refused blocking",
            "/no/ms.js script refused blocking",
            "/no/p.png link refused",
            "/no/s.js script refused",
            "/no/sync.png img refused",
            "/no/x.json xmlhttprequest refused",
            "/nonce/nc.css link blocking",
            "/ok/di.png?a%3Cb img refused",
            "/ok/dm.png?a%3Cb img refused",
            "/ok/dm.png?a%3Cb img refused",
        ]),
        "the page's timeline (server hits {hits:?})"
    );
    // a script-inserted image is loaded in a microtask (Blink's ImageLoader): its refused entry is not there at once
    assert_eq!((got["sync"].as_u64(), got["micro"].as_u64()), (Some(0), Some(1)), "{got}");
}

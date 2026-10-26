//! A frame document's loads are on the FRAME's timeline: the page lists the frame's own `iframe` entry, the frame its
//! `<img>`. korvus put a frame document's entries on the page's timeline and none on the frame's -- and since a frame's
//! load waits for its image, a frame showing an image (its viewer document's `<img>`) gave the page two
//! entries by its `load` where WPT resource-timing/iframe-non-html expects one.
//!
//! Chrome 150.0.7871.46, headed on the Wayland desktop, no DevTools: a same-origin frame holding one `<img>`
//! reads `{page: [f.html:iframe], frame: [sq.png:img]}` at its `load` and after; an image, an XHTML and a text frame each
//! give the page one `iframe` entry.
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x54, 0xa2, 0x4f, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const PAGE: &str = r#"<!doctype html><body><script>
var res = {};
function names(p) { return p.getEntriesByType('resource').map(function (x) { return x.name.split('/').pop() + ':' + x.initiatorType; }).filter(function (s) { return s.indexOf('favicon') < 0; }); }
var f = document.createElement('iframe');
f.onload = function () {
  res.page = names(performance); res.frame = names(f.contentWindow.performance);
  var g = document.createElement('iframe');
  g.onload = function () { res.image = names(performance).filter(function (s) { return s.indexOf('img.png') == 0; }); window.RESULT = JSON.stringify(res); };
  g.src = 'img.png?x=1'; document.body.appendChild(g);
};
f.src = 'f.html?x=1'; document.body.appendChild(f);
</script></body>"#;

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split(' ').nth(1).unwrap_or("/").to_string();
            let (body, ty): (Vec<u8>, &str) = if path.starts_with("/f.html") {
                (b"<!doctype html><img src=\"sq.png?x=1\">".to_vec(), "text/html")
            } else if path.starts_with("/sq.png") || path.starts_with("/img.png") {
                (PNG.to_vec(), "image/png")
            } else if path == "/" {
                (PAGE.as_bytes().to_vec(), "text/html")
            } else {
                (Vec::new(), "text/plain")
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_frame_documents_loads_are_on_the_frames_timeline() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    tokio::spawn(serve(l));
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
        assert!(start.elapsed() < Duration::from_secs(20), "no result");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(
        got,
        serde_json::json!({"page": ["f.html?x=1:iframe"], "frame": ["sq.png?x=1:img"], "image": ["img.png?x=1:iframe"]})
    );
}

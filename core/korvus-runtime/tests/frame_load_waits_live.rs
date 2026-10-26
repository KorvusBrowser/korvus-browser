//! A frame document's `load` waits for the loads its own document starts -- a script its script inserted, `async =
//! false` or async -- as the page's does (HTML "delay the load event"; Blink's `CheckCompleted` on the frame
//! document's load-event delay count).
//!
//! korvus fired a frame's `load` at the end of its parse, whatever its document still had in flight: a bot-detection script's
//! sandboxed `crcldu.com/bd/sync.html` inserts `auditor.js`, which registers the frame's `message` listener, and the
//! page posts its first message from the iframe's `onload` -- so the message arrived before the listener existed and
//! was lost, the frame never sent its fst-ec / dvp requests, and the press & hold carried no result of them (traced
//! offline in both engines).
//!
//! Chrome 150.0.7871.46 on this machine, headed, no DevTools, three runs alike: a
//! same-origin frame, a cross-origin one and a cross-origin `sandbox=allow-scripts` one each insert two scripts that
//! take 400 ms; the page posts from `onload` and every frame answers with both scripts run.
use engine::Engine;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FRAME: &str = "<!doctype html><html><body><script>\
var s = document.createElement('script'); s.src = 'slow.js?' + location.search.slice(1); s.async = false; document.body.appendChild(s);\
var d = document.createElement('script'); d.src = 'slow.js?dyn-async-' + location.search.slice(1); document.body.appendChild(d);\
</script></body></html>";
const SLOW: &str = "window.ready = (window.ready || 0) + 1; if (window.ready === 1) addEventListener('message', function (e) { setTimeout(function () { e.ports[0] && e.ports[0].postMessage('got ' + e.data + ' ready=' + window.ready); }, 10); });";

/// A 1x1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

/// `img.html?<lazy|eager>&<name>`: a frame document whose parser meets one `<img>` that takes 400 ms, `loading=lazy` or
/// not, and which tells its parent the order of its image's `load` and its window's.
fn img_frame(q: &str) -> String {
    let (kind, name) = q.split_once('&').unwrap_or((q, q));
    let lazy = if kind == "lazy" { " loading=lazy" } else { "" };
    format!(
        "<!doctype html><html><body><img{lazy} src=\"/slow.gif?{name}\"><script>\
var seq = []; document.querySelector('img').onload = function () {{ seq.push('image'); }};\
addEventListener('load', function () {{ seq.push('window'); setTimeout(function () {{ parent.postMessage('{name} ' + seq.join(','), '*'); }}, 800); }});\
</script></body></html>"
    )
}

async fn serve(listener: TcpListener, page: Arc<String>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (ct, body): (&str, Vec<u8>) = if target.starts_with("/frame.html") {
                ("text/html", FRAME.into())
            } else if target.starts_with("/slow.js") {
                tokio::time::sleep(Duration::from_millis(400)).await;
                ("text/javascript", SLOW.into())
            } else if let Some(q) = target.strip_prefix("/img.html?") {
                ("text/html", img_frame(q).into_bytes())
            } else if target.starts_with("/slow.gif") {
                tokio::time::sleep(Duration::from_millis(400)).await;
                ("image/gif", GIF.to_vec())
            } else {
                ("text/html", page.as_bytes().to_vec())
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_frames_load_waits_for_the_scripts_its_document_inserted() {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (oa, ob) = (format!("http://{}", a.local_addr().unwrap()), format!("http://{}", b.local_addr().unwrap()));
    let page = Arc::new(format!(
        "<!doctype html><html><body><script>\
var R = {{}};\
function mk(name, src, sandbox, next) {{\
  var f = document.createElement('iframe'); if (sandbox) f.setAttribute('sandbox', sandbox);\
  f.src = src + '?' + name;\
  var ch = new MessageChannel(), got = false;\
  ch.port1.onmessage = function (e) {{ got = true; R[name] = e.data; next(); }};\
  f.onload = function () {{ f.contentWindow.postMessage(name, '*', [ch.port2]); setTimeout(function () {{ if (!got) {{ R[name] = 'LOST'; next(); }} }}, 1500); }};\
  document.body.appendChild(f);\
}}\
mk('same', '/frame.html', null, function () {{ mk('xsbx', '{ob}/frame.html', 'allow-scripts', function () {{ mk('xorigin', '{ob}/frame.html', null, function () {{ window.RESULT = JSON.stringify(R); }}); }}); }});\
</script></body></html>"
    ));
    tokio::spawn(serve(a, page.clone()));
    tokio::spawn(serve(b, page.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, &format!("{oa}/page.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("frame-load", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the frames never answered");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(got, serde_json::json!({"same": "got same ready=2", "xorigin": "got xorigin ready=2", "xsbx": "got xsbx ready=2"}));
}

/// A LAZY image in a frame's document does not hold the frame's `load`; an eager one still does (above). Chrome
/// requests even an in-viewport lazy image only after `load` (`lazy_load.rs`), so its `load` comes first. WPT
/// `image-loading-lazy-in-cross-origin-iframe-00{1,2}` asserts it, and Chrome passes it. korvus loads a frame's lazy image
/// at once (no layout for a frame), and that request now holds the frame's `load`: `image,window`.
#[tokio::test]
async fn a_lazy_image_does_not_hold_its_frames_load() {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (oa, ob) = (format!("http://{}", a.local_addr().unwrap()), format!("http://{}", b.local_addr().unwrap()));
    let page = Arc::new(format!(
        "<!doctype html><html><body><script>\
var R = {{}};\
addEventListener('message', function (e) {{ var p = String(e.data).split(' '); R[p[0]] = p[1]; if (Object.keys(R).length === 4) window.RESULT = JSON.stringify(R); }});\
[['same-lazy', '', 'lazy'], ['same-eager', '', 'eager'], ['x-lazy', '{ob}', 'lazy'], ['x-eager', '{ob}', 'eager']].forEach(function (f) {{\
  var i = document.createElement('iframe'); i.src = f[1] + '/img.html?' + f[2] + '&' + f[0]; document.body.appendChild(i);\
}});\
</script></body></html>"
    ));
    tokio::spawn(serve(a, page.clone()));
    tokio::spawn(serve(b, page.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, &format!("{oa}/page.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("frame-lazy", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the frames never answered");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(
        got,
        serde_json::json!({"same-lazy": "window,image", "same-eager": "image,window", "x-lazy": "window,image", "x-eager": "image,window"})
    );
}

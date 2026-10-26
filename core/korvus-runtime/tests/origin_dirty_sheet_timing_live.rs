//! What a CROSS-ORIGIN style sheet fetched without CORS requests -- its `@import`, its `@font-face` font, its
//! `background-image` -- makes no resource-timing entry. Blink asks for each with `SetFromOriginDirtyStyleSheet`
//! (css_image_value.cc, css_font_face_src_value.cc, style_rule_import.cc), and `PopulateAndAddResourceTimingInfo` adds no
//! entry for such a request (resource_fetcher.cc 3222). A same-origin sheet's own loads keep theirs.
//!
//! korvus listed all of them as `css` entries (WPT resource-timing/no-entries-for-cross-origin-css-fetched, and its
//! memory-cache twin; Chrome 150 passes both).
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

/// The page (origin A) adds a same-origin sheet and a cross-origin one (origin B, no `crossorigin`), each with an
/// `@import`, a used `@font-face` font and a `background-image`, then reads its resource entries.
fn page(b: &str) -> String {
    format!(
        r#"<!doctype html><body><p class=a>a</p><p class=b>b</p><script>
var left = 2;
function done() {{
  if (--left) return;
  setTimeout(function () {{
    window.RESULT = JSON.stringify(performance.getEntriesByType('resource').map(function (e) {{ return e.name.split('/').pop() + ':' + e.initiatorType; }}).sort());
  }}, 800);
}}
['same.css', '{b}/cross.css'].forEach(function (h) {{
  var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = h; l.onload = l.onerror = done; document.head.appendChild(l);
}});
</script></body>"#
    )
}

fn sheet(tag: &str) -> String {
    format!("@import \"{tag}-imp.css\";\n@font-face {{ font-family: F{tag}; src: url({tag}-font.ttf); }}\np.{c} {{ font-family: F{tag}; background-image: url({tag}-bg.png); }}\n", c = if tag == "s" { "a" } else { "b" })
}

async fn serve(listener: TcpListener, b: String, hits: Arc<Mutex<Vec<String>>>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let b = b.clone();
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split(' ').nth(1).unwrap_or("/").to_string();
            hits.lock().unwrap().push(path.clone());
            let (body, ty): (Vec<u8>, &str) = match path.as_str() {
                "/" => (page(&b).into_bytes(), "text/html"),
                "/same.css" => (sheet("s").into_bytes(), "text/css"),
                "/cross.css" => (sheet("x").into_bytes(), "text/css"),
                p if p.ends_with("-imp.css") => (b"i{color:red}".to_vec(), "text/css"),
                p if p.ends_with(".png") => (PNG.to_vec(), "image/png"),
                p if p.ends_with(".ttf") => (b"\0\x01\0\0".to_vec(), "font/ttf"),
                _ => (Vec::new(), "text/plain"),
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_cross_origin_sheets_own_loads_make_no_entries() {
    let la = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lb = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = format!("http://{}", la.local_addr().unwrap());
    let b = format!("http://{}", lb.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(la, b.clone(), hits.clone()));
    tokio::spawn(serve(lb, b.clone(), hits.clone()));
    let url = format!("{a}/");
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
    let got: Vec<String> = serde_json::from_str(&got).expect("json");
    let got: Vec<String> = got.into_iter().filter(|s| !s.contains("favicon")).collect();
    let hits = hits.lock().unwrap().clone();
    // every load was made: the cross-origin sheet's three went on the wire, they only have no entry
    for p in ["/x-imp.css", "/x-font.ttf", "/x-bg.png", "/s-imp.css", "/s-font.ttf", "/s-bg.png"] {
        assert!(hits.iter().any(|h| h == p), "{p} was never requested: {hits:?} (entries {got:?})");
    }
    assert_eq!(
        got,
        vec!["cross.css:link", "s-bg.png:css", "s-font.ttf:css", "s-imp.css:css", "same.css:link"],
        "server hits {hits:?}"
    );
}

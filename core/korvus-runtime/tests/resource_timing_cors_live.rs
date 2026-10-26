//! A parser-inserted `<script crossorigin>` from another origin is fetched in CORS mode, so its response is the
//! document's to read: its resource-timing entry reads `responseStatus` and `contentType`, while its sizes and its
//! protocol stay hidden without Timing-Allow-Origin. korvus put it on the timeline as an opaque load
//! -- 0 and "" -- as for a script without `crossorigin`.
//!
//! Chrome 150.0.7871.46 on this machine, headed, no DevTools, three runs alike: the
//! script and its CORS twin from 127.0.0.2, both `Access-Control-Allow-Origin: *`, no Timing-Allow-Origin.
//!
//! The same holds for a parser `<link rel=stylesheet crossorigin>` (korvus read 0 and ""), and every
//! readable entry's `contentType` is Blink's MINIMIZED type -- the JavaScript, JSON and XML types folded to one name,
//! a type Blink does not support "" (Chrome 150, three runs alike; korvus
//! read each type as sent).
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split(' ').nth(1).unwrap_or("/").to_string();
            // `/c.css` a sheet, `/r?ct=TYPE` a body sent as TYPE (`%3B` and `%20` decoded), else a script
            let (body, ty) = if path.starts_with("/c.css") {
                ("p{color:red}", "text/css".to_string())
            } else if let Some(t) = path.split("ct=").nth(1) {
                ("x", t.replace("%3B", ";").replace("%20", " "))
            } else {
                ("var x=1;", "text/javascript".to_string())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_cross_origin_cors_script_reads_its_status_and_type() {
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let ob = format!("http://{}", b.local_addr().unwrap());
    tokio::spawn(serve(b));
    let page = format!(
        "<!doctype html><html><head><script src=\"{ob}/s.js?cross\"></script><script crossorigin src=\"{ob}/s.js?crosscors\"></script></head><body><script>\
onload = function () {{ window.RESULT = JSON.stringify(performance.getEntriesByType('resource').filter(function (e) {{ return e.name.indexOf('/s.js') > 0; }}).map(function (e) {{\
  return [e.name.split('?')[1], e.responseStatus, e.contentType, e.transferSize, e.encodedBodySize, e.decodedBodySize, e.nextHopProtocol]; }})); }};\
</script></body></html>"
    );
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("rt-cors", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no entries");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(
        got,
        serde_json::json!([["cross", 0, "", 0, 0, 0, ""], ["crosscors", 200, "text/javascript", 0, 0, 0, ""]])
    );
}

#[tokio::test]
async fn a_cross_origin_cors_sheet_reads_its_status_and_every_type_reads_minimized() {
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let ob = format!("http://{}", b.local_addr().unwrap());
    tokio::spawn(serve(b));
    // [the type sent, what Chrome 150 reads]
    let types = [
        ("font/woff2", ""),
        ("application/pdf", ""),
        ("text/csv", ""),
        ("image/tiff", ""),
        ("garbage", ""),
        ("text/plain", "text/plain"),
        ("text/plain%3B%20charset=utf-8", "text/plain"),
        ("application/x-javascript", "text/javascript"),
        ("text/javascript1.5", "text/javascript"),
        ("text/json", "application/json"),
        ("application/ld+json", "application/json"),
        ("text/xml", "application/xml"),
        ("application/rss+xml", "application/xml"),
        ("application/xhtml+xml", "application/xml"),
        ("image/svg+xml%3B%20charset=utf-8", "image/svg+xml"),
        ("image/png", "image/png"),
        ("image/x-png", "image/x-png"),
        ("message/rfc822", "message/rfc822"),
    ];
    let list = types.iter().map(|(t, _)| format!("'{t}'")).collect::<Vec<_>>().join(",");
    let page = format!(
        "<!doctype html><html><head><link rel=stylesheet href=\"{ob}/c.css?cross\"><link rel=stylesheet crossorigin href=\"{ob}/c.css?crosscors\">\
<script src=\"{ob}/s.js?after\"></script></head><body><script>\
var T = [{list}];\
onload = async function () {{ for (var i = 0; i < T.length; i++) {{ await (await fetch('{ob}/r?i=' + i + '&ct=' + T[i])).text(); }}\
  var all = performance.getEntriesByType('resource');\
  var sheets = all.filter(function (e) {{ return e.name.indexOf('/c.css') > 0; }}).map(function (e) {{ return [e.name.split('?')[1], e.responseStatus, e.contentType]; }}).sort();\
  var typed = all.filter(function (e) {{ return e.name.indexOf('/r?') > 0; }}).map(function (e) {{ return [T[+e.name.split('i=')[1].split('&')[0]], e.responseStatus, e.contentType]; }});\
  window.RESULT = JSON.stringify(sheets.concat(typed)); }};\
</script></body></html>"
    );
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("rt-mime", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no entries");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    let mut want = vec![serde_json::json!(["cross", 0, ""]), serde_json::json!(["crosscors", 200, "text/css"])];
    want.extend(types.iter().map(|(t, m)| serde_json::json!([t, 200, m])));
    assert_eq!(got, serde_json::Value::Array(want));
}

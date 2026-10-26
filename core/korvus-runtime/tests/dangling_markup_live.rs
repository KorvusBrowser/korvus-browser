//! Chromium's POTENTIALLY DANGLING MARKUP refusal: an http(s) URL whose string, as the page wrote it, had a
//! tab, CR or LF that the URL parser removed AND has a `<` is never requested. `fetch()` rejects with `TypeError: Failed
//! to fetch`, an element's load fails with `error`, and a frame navigation is dropped (`url_canon_etc.cc`
//! `DoRemoveUrlWhitespace`; `base_fetch_context.cc` `CanRequest`; `frame_loader.cc` `StartNavigation`). A `Request`
//! keeps the mark through `clone()` and `new Request(request)`, as Blink's `KURL` does, but its serialized `url` does not.
//!
//! korvus refused none of it. WPT `fetch/security/dangling-markup/dangling-markup-mitigation.tentative.html`: 9 rows
//! Chrome passes (6 `fetch()`, 3 `<img>` in a srcdoc frame) failed. Once the frame rows were visible: a
//! frame's `load` now waits for its image, which loaded.
//!
//! The rows are `fixtures/dangling-markup/probe.html`'s, and every one of them is stock Chrome 150.0.7871.46's own:
//! three runs, byte-identical, over the same three paths (ac, `r19chrome/chrome.txt`; the parsed `<img>`'s row, which the
//! preload scanner would otherwise have fetched, measured with it). `Request` keeps the parsed
//! `KURL` (`FetchRequestData::url_`), and an `<img>` and a frame go through the same `CanRequest` / `StartNavigation`.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PROBE: &str = include_str!("fixtures/dangling-markup/probe.html");
/// A 1x1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (ct, body): (&str, Vec<u8>) = if target.starts_with("/g.gif") {
                ("image/gif", GIF.to_vec())
            } else if target.starts_with("/g.html") {
                ("text/html", b"<!doctype html><p>g</p>".to_vec())
            } else {
                ("text/html", PROBE.as_bytes().to_vec())
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_url_with_dangling_markup_is_never_requested() {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", a.local_addr().unwrap());
    tokio::spawn(serve(a));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(PROBE, &format!("{origin}/probe.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("dangling", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the probe never finished");
    };
    let want = [
        "fetch nl+lt -> TypeError: Failed to fetch",
        "fetch cr+lt -> TypeError: Failed to fetch",
        "fetch tab+lt -> TypeError: Failed to fetch",
        "fetch nl -> ok 200",
        "fetch lt -> ok 200",
        "fetch nl+%3C -> ok 200",
        "fetch data nl+lt -> ok 200",
        "fetch Request -> TypeError: Failed to fetch",
        "fetch Request.clone() -> TypeError: Failed to fetch",
        "fetch Request(Request) -> TypeError: Failed to fetch",
        "fetch Request.url -> ok 200",
        "img nl+lt -> error 0",
        "img nl -> load 1",
        "img lt -> load 1",
        "iframe nl+lt -> about:blank",
        "iframe %3C -> /g.html?f=%3C",
        "img parsed nl+lt -> error 0",
    ]
    .join("\n");
    assert_eq!(got, want);
}

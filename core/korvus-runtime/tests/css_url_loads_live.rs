//! FIXER C10 (A/B program, investigator `a production site` F1) -- which `url()` a style FETCHES, and how, in a live engine
//! over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Stock Chrome 150.0.7871.46, headed under Xvfb, NO DevTools attached, walking the pages of
//! `fixtures/css-url-loads/pages.json` one top-level document after another (each navigates to the next), every
//! request logged with its `Sec-Fetch-Dest`, `Sec-Fetch-Mode` and whether it carried `Origin`. Three runs gave the
//! same rows; they are `fixtures/css-url-loads/chrome-150.txt` (`page | host | path | dest | mode | origin`, one
//! per distinct request; the chain's own navigations and `/favicon.ico` left out). The recorder is
//!
//! What the rows say (Chromium's `ElementStyleResources`, `core/css/resolver/element_style_resources.cc`):
//!
//! ```text
//! url(#id) or the page's own URL in                        Chrome
//! mask-image, -webkit-mask-image, mask=, a sheet's mask     nothing (an element of the document, :374-391)
//! clip-path, filter, backdrop-filter, fill, stroke, marker  nothing (:281-289)
//! background-image, border-image-source, list-style-image,  the PAGE'S OWN URL, image, no-cors
//!   content, cursor, -webkit-mask-box-image-source, -webkit-box-reflect
//! shape-outside                                             the page's own URL, image, cors
//!
//! another document's url()
//! mask-image (svg or png), mask=, a sheet's mask            image, cors, Origin
//! clip-path, fill, stroke, marker-*                         image, cors, Origin  (also cross-origin)
//! filter, backdrop-filter                                   image, same-origin; cross-origin: never sent
//! shape-outside                                             image, cors
//! background, -webkit-mask-box-image-source, box-reflect    image, no-cors
//! ```
//!
//! korvus before: every mask variant fetched the page's own URL as a no-cors image (a production site: ~277 such
//! requests per visit, 276 answered 403), external masks went no-cors, and no clip-path / filter / fill / stroke
//! / marker / shape-outside / -webkit-mask-box-image / -webkit-box-reflect document was ever requested.

use engine::Engine;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAGES: &str = include_str!("fixtures/css-url-loads/pages.json");
const CHROME: &str = include_str!("fixtures/css-url-loads/chrome-150.txt");

/// One logged request: host, path, `Sec-Fetch-Dest`, `Sec-Fetch-Mode`, whether `Origin` was sent.
type Log = Arc<Mutex<Vec<(String, String, String, String, bool)>>>;

const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x54, 0xa2, 0x4f, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn serve(listener: TcpListener, log: Log, fixture: Arc<serde_json::Value>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        let fixture = fixture.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let host = header(&req, "host").unwrap_or("").split(':').next().unwrap_or("").to_string();
            let dest = header(&req, "sec-fetch-dest").unwrap_or("-").to_string();
            let mode = header(&req, "sec-fetch-mode").unwrap_or("-").to_string();
            let origin = header(&req, "origin").is_some();
            if let Ok(mut l) = log.lock() {
                l.push((host, path.clone(), dest, mode, origin));
            }
            let p = path.split('?').next().unwrap_or("");
            let (ctype, body): (&str, Vec<u8>) = if let Some(css) = fixture["sheets"][p].as_str() {
                ("text/css", css.as_bytes().to_vec())
            } else if p.ends_with(".svg") {
                ("image/svg+xml", fixture["svg"].as_str().unwrap_or("").as_bytes().to_vec())
            } else if p.ends_with(".png") {
                ("image/png", PNG.to_vec())
            } else {
                // The page's own URL, asked for again as an image by an engine that fetches a local url().
                ("text/html", b"<!doctype html>".to_vec())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\n\
                 Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

/// Load one page of the fixture as `/v/<name>` and answer its requests as fixture rows.
async fn rows_of(name: &str, html: &str, fixture: Arc<serde_json::Value>) -> Vec<String> {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve(listener, log.clone(), fixture));
    let doc = format!(
        "<!doctype html><html><head><meta charset=utf-8></head><body>{}</body></html>",
        html.replace("{XO}", &format!("http://localhost:{port}"))
    );
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://127.0.0.1:{port}/v/{name}"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(8)).await;
    let _ = e.pump(Duration::from_millis(400)).await;
    let got = log.lock().unwrap().clone();
    got.into_iter()
        .filter(|(_, path, ..)| path != "/favicon.ico")
        .map(|(host, path, dest, mode, origin)| {
            let host = match host.as_str() {
                "127.0.0.1" => "self",
                "localhost" => "xo",
                h => h,
            }
            .to_string();
            [name.to_string(), host, path, dest, mode, if origin { "origin" } else { "-" }.to_string()].join(" | ")
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[tokio::test]
async fn a_style_fetches_the_urls_chrome_fetches_and_no_local_one() {
    let fixture: Arc<serde_json::Value> = Arc::new(serde_json::from_str(PAGES).expect("pages.json"));
    let mut got = Vec::new();
    for page in fixture["pages"].as_array().expect("pages").clone() {
        let name = page[0].as_str().unwrap();
        let html = page[1].as_str().unwrap();
        got.extend(rows_of(name, html, fixture.clone()).await);
    }
    got.sort();
    let want: Vec<String> = CHROME.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    let missing: Vec<&String> = want.iter().filter(|r| !got.contains(r)).collect();
    let extra: Vec<&String> = got.iter().filter(|r| !want.contains(r)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "korvus's requests differ from stock Chrome 150's\n  Chrome only: {missing:#?}\n  korvus only: {extra:#?}"
    );
}

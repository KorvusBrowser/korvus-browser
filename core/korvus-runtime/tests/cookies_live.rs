//! ONE COOKIE STORE, AS CHROME 150 KEEPS IT: what a page stores and reads back
//! through `document.cookie`, its requests' responses, the `Cookie` header those requests carry, a cross-site frame's
//! `document.cookie` and `cookieStore` -- all one store now (`transport::cookies`, Chrome's `CookieMonster`).
//!
//! `fixtures/cookies/probe.js` runs on `http://a.ck.localhost:PORT/d/page.html`, served by the server below and, for
//! Chrome, by `fixtures/cookies/record.py` (the same answers): `ck.localhost` is the page's registrable domain, `b.ck.
//! localhost` the same site, `c.ck2.localhost` and `127.0.0.1` other sites; `/set?c=LINE` answers one `Set-Cookie` per
//! `c`, `/echo` the request's `Cookie` header. `fixtures/cookies/chrome-150.json` is google-chrome 150.0.7871.46's
//! answer, headed under Xvfb with no DevTools, three runs identical.
//!
//! Before, against the same record, korvus differed in 19 of 36 rows: `document.cookie` showed what a response set
//! for a `Domain` its host does not domain-match and what a `credentials: 'omit'` response set (the network jar had
//! refused both); `domain=localhost` was stored (no registry rule); `__Secure-`/`__Host-` names without their
//! attributes, a value with a tab or a control character, `Partitioned` without `Secure` were stored; a nameless
//! cookie was lost; an overwrite with a new value kept its place in the line; a cross-site `*.localhost` frame saw no
//! `Secure` cookie; `cookieStore` kept its own list (`getAll()` saw none of the page's cookies, every item `path:
//! "/"`, `sameSite: "strict"`); every `document.cookie` write read `window.cookieStore` through the page's getter.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const PROBE: &str = include_str!("fixtures/cookies/probe.js");
const CHROME: &str = include_str!("fixtures/cookies/chrome-150.json");
const FRAME: &str = "<!doctype html><html><body><script>\nvar r = [document.cookie];\ndocument.cookie = 'kf_lax=1';\n\
document.cookie = 'kf_none=1; SameSite=None; Secure';\ndocument.cookie = 'kf_part=1; SameSite=None; Secure; Partitioned';\n\
r.push(document.cookie);\nparent.postMessage(r.join(' || '), '*');\n</script></body></html>";
const GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

/// The page `record.py` serves: the probe inlined, its rows in `window.__report`.
fn page() -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8></head><body><script>\nvar __r;\ntry {{ __r = {PROBE}; }} catch (e) {{ __r = [['FATAL-OUTER', String(e)]]; }}\n\
         Promise.resolve(__r).then(function (r) {{ window.__report = JSON.stringify(r); }}, function (e) {{ window.__report = JSON.stringify([['FATAL', String(e)]]); }});\n</script></body></html>"
    )
}

/// One keep-alive connection, answered as `record.py` answers.
fn serve(s: std::net::TcpStream, page: &str) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (mut len, mut origin, mut cookie, mut image) = (0usize, None::<String>, String::new(), false);
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let Some((k, v)) = h.split_once(':') else { continue };
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "origin" => origin = Some(v.to_string()),
                "cookie" => cookie = v.to_string(),
                "accept" if v.contains("image") => image = true,
                "sec-fetch-dest" if v == "image" => image = true,
                _ => {}
            }
        }
        let mut body = vec![0u8; len];
        let _ = r.read_exact(&mut body);
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let mut extra = String::new();
        let (ctype, body): (&str, Vec<u8>) = match path {
            "/d/page.html" => ("text/html; charset=utf-8", page.as_bytes().to_vec()),
            "/frame.html" => ("text/html; charset=utf-8", FRAME.as_bytes().to_vec()),
            "/set" => {
                for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
                    if k == "c" {
                        extra.push_str(&format!("Set-Cookie: {v}\r\n"));
                    }
                }
                if image { ("image/gif", GIF.to_vec()) } else { ("text/plain", b"ok".to_vec()) }
            }
            "/echo" => ("text/plain", cookie.into_bytes()),
            _ => ("text/plain", Vec::new()),
        };
        if let Some(o) = origin {
            extra.push_str(&format!("Access-Control-Allow-Origin: {o}\r\nAccess-Control-Allow-Credentials: true\r\n"));
        }
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{extra}\r\n", body.len());
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(&body)).is_err() {
            return;
        }
    }
}

/// The server on both loopback families of one port (`*.localhost` resolves to `::1` and `127.0.0.1`).
fn server() -> u16 {
    let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = v4.local_addr().unwrap().port();
    let v6 = std::net::TcpListener::bind(("::1", port)).expect("the same port on ::1");
    let page: &'static str = Box::leak(page().into_boxed_str());
    for l in [v4, v6] {
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                std::thread::spawn(move || serve(s, page));
            }
        });
    }
    port
}

#[tokio::test]
async fn one_cookie_store_answers_as_chrome_150s() {
    let port = server();
    let url = format!("http://a.ck.localhost:{port}/d/page.html");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let report = loop {
        assert!(start.elapsed() < Duration::from_secs(60), "the probe never answered");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "typeof window.__report === 'string' ? window.__report : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break s.replace(&format!(":{port}"), ":PORT");
        }
    };
    let got: Vec<serde_json::Value> = serde_json::from_str(&report).expect("the probe's rows");
    let want: Vec<serde_json::Value> = serde_json::from_str(CHROME).expect("the recorded Chrome rows");
    assert_eq!(got.len(), want.len(), "row count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} rows differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

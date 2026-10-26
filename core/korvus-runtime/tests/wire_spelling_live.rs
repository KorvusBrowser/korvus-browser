//! WHAT CHROME WRITES ON AN HTTP/1.1 WIRE, line for line: every request's header names as
//! Chrome spells them, in Chrome's order, with Chrome's values.
//!
//! * Spelling. The network stack's own names are `Host`, `Connection`, `User-Agent`, `Sec-Fetch-Dest`, ... and the
//!   client hints `sec-ch-ua*` lowercase; a page's own header goes out as the page spelled it (`X-Foo`, `x-bar`, a
//!   lowercase `content-type`), and the `Content-Type` a body implies as Blink spells it. korvus wrote every name
//!   lowercase.
//! * A form POST navigation -- the page's or a frame's -- carries `Cache-Control: max-age=0` (Chromium validates the
//!   cache for a navigation with a body) and the initiator's `Origin`, with `Content-Type` before `User-Agent` and
//!   `Origin` before `Accept`; one into a frame goes out `Sec-Fetch-Dest: iframe`. korvus sent no `Cache-Control`,
//!   no `Origin`, `Content-Type` after `User-Agent`, and `document` for the frame.
//! * A CORS preflight carries `Connection: keep-alive` second, as every request does; korvus's had none.
//! * The headers a page sets itself (`hdr`, 2026-10-11, ShieldSquare): one line per name -- a page's `Accept`
//!   REPLACES the default (korvus sent `Accept: */*` AND jQuery's `Accept`, and ShieldSquare challenged every such
//!   XHR), a name set twice is combined, a value is trimmed, a forbidden name is dropped, a `Headers` passed to
//!   `fetch()` arrives lowercase, a `Range` brings `Accept-Encoding: identity` and goes last; and
//!   `setRequestHeader`'s exceptions in Blink's words (sent as the query of `/r/xhr-errors`).
//! * Over TLS spoken as HTTP/1.1 (`tls`) no request carries `Priority`: Chromium writes it on HTTP/2 and HTTP/3 only.
//!
//! The documents are `fixtures/wire-spelling/record.py`'s, which recorded stock Chrome 150.0.7871.46 (headed under
//! Xvfb, NO DevTools client, a fresh browser per case, three runs alike) into `chrome-150.json`: per case, the first request of each
//! request line, ports written `P` and a multipart boundary `----WebKitFormBoundaryX`.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHROME: &str = include_str!("fixtures/wire-spelling/chrome-152.json");

/// The `tls` case's certificate (signed by `tls-ca.pem`, which the engine is told to trust through `KORVUS_EXTRA_CA`).
const TLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire-spelling");

const SUB: &str = r#"<!doctype html><title>sub</title><link rel=stylesheet href="/r/css"><script src="/r/js"></script>
<img src="/r/img"><iframe src="/r/frame"></iframe>
<script>
onload = function () {
  fetch('/r/fetch-plain');
  fetch('/r/fetch-hdr', {headers: {'X-Foo': '1', 'x-bar': '2', 'X-BAZ': '3', 'Content-Type': 'text/plain'}});
  fetch('/r/fetch-post', {method: 'POST', body: 'a=1'});
  fetch('/r/fetch-json', {method: 'POST', body: '{}', headers: {'content-type': 'application/json'}});
  var x = new XMLHttpRequest(); x.open('POST', '/r/xhr'); x.setRequestHeader('X-Req', 'y'); x.send('b');
  navigator.sendBeacon('/r/beacon', 'z');
  fetch('http://localhost:PORT/r/cors', {headers: {'X-Cors': '1'}});
};
</script>"#;

const HDR: &str = r#"<!doctype html><title>hdr</title><script>
function xhr(path, hdrs, method, body) { return new Promise(function (res) { var x = new XMLHttpRequest(); x.open(method || 'GET', path);
  for (var i = 0; i < hdrs.length; i++) { try { x.setRequestHeader(hdrs[i][0], hdrs[i][1]); } catch (e) {} }
  x.onloadend = res; x.send(body === undefined ? null : body); }); }
function errs() { var out = [], x = new XMLHttpRequest();
  function t(f) { try { f(); out.push('ok'); } catch (e) { out.push(e.name + ': ' + e.message); } }
  t(function () { x.setRequestHeader('X-A', '1'); });
  x.open('GET', '/r/xhr-sent');
  t(function () { x.setRequestHeader('a b', '1'); });
  t(function () { x.setRequestHeader('', '1'); });
  t(function () { x.setRequestHeader('X-A', 'a\nb'); });
  t(function () { x.setRequestHeader('X-A', '\u0100'); });
  t(function () { x.setRequestHeader('X-E', ' \te\t '); });
  x.send();
  t(function () { x.setRequestHeader('X-A', '1'); });
  t(function () { x.send(); });
  return fetch('/r/xhr-errors?' + encodeURIComponent(out.join('|'))); }
var steps = [
  function () { return xhr('/r/xhr-jquery', [['Accept', 'application/json, text/javascript, */*; q=0.01'], ['X-Requested-With', 'XMLHttpRequest']]); },
  function () { return xhr('/r/xhr-plain', []); },
  function () { return xhr('/r/xhr-accept-twice', [['Accept', 'a/b'], ['accept', 'c/d']]); },
  function () { return xhr('/r/xhr-xa-twice', [['X-A', '1'], ['x-a', '2']]); },
  function () { return xhr('/r/xhr-ws', [['X-B', '  v  '], ['X-C', '\tw\t']]); },
  function () { return xhr('/r/xhr-forbidden', [['User-Agent', 'p'], ['Cookie', 'c=1'], ['Referer', 'http://x/'], ['Origin', 'http://x'],
    ['Sec-Foo', '1'], ['Proxy-Foo', '1'], ['Accept-Encoding', 'x'], ['Connection', 'close'], ['X-HTTP-Method-Override', 'TRACE'],
    ['Access-Control-Request-Private-Network', 'true'], ['X-Ok', '1']]); },
  function () { return xhr('/r/xhr-override-put', [['X-HTTP-Method-Override', 'PUT']]); },
  function () { return xhr('/r/xhr-accept-language', [['Accept-Language', 'fr']]); },
  function () { return xhr('/r/xhr-post-ct', [['Content-Type', 'application/json']], 'POST', '{}'); },
  function () { return fetch('/r/fetch-accept', {headers: {'Accept': 'application/json'}}); },
  function () { return fetch('/r/fetch-accept-language', {headers: {'Accept-Language': 'fr'}}); },
  function () { return fetch('/r/fetch-xa-twice', {headers: [['X-A', '1'], ['x-a', '2']]}); },
  function () { var h = new Headers(); h.append('Accept', 'a/b'); h.append('accept', 'c/d'); return fetch('/r/fetch-accept-twice', {headers: h}); },
  function () { return fetch('/r/fetch-headers-object', {headers: new Headers({'X-Mixed': '1', 'Accept': 'x/y'})}); },
  function () { return fetch('/r/fetch-ws', {headers: {'X-B': '  v  '}}); },
  function () { return fetch(new Request('/r/fetch-request', {headers: {'X-Req': '1', 'Accept': 'r/s'}})); },
  function () { return fetch('/r/fetch-range', {headers: {'Range': 'bytes=0-1'}}); },
  function () { return fetch('/r/fetch-forbidden', {headers: {'User-Agent': 'p', 'X-HTTP-Method-Override': 'TRACE', 'X-Ok': '1'}}); },
  errs];
steps.reduce(function (p, s) { return p.then(s, s); }, Promise.resolve());
</script>"#;

const TLS: &str = r#"<!doctype html><title>tls</title><script src="/r/js"></script><img src="/r/img">
<script>
onload = function () {
  fetch('/r/tls-fetch');
  fetch('/r/tls-fetch-priority', {headers: {'priority': 'u=5'}});
  var x = new XMLHttpRequest(); x.open('GET', '/r/tls-xhr'); x.setRequestHeader('Accept', 'application/json'); x.send();
};
</script>"#;

fn form(kind: &str, port: u16) -> String {
    let (mut method, mut action, mut enctype, mut target, mut extra) = ("POST", format!("/post?{kind}"), "", "", "");
    match kind {
        "multipart" => enctype = " enctype=\"multipart/form-data\"",
        "plain" => enctype = " enctype=\"text/plain\"",
        "get" => method = "GET",
        "cross" => action = format!("http://localhost:{port}/post?cross"),
        "frame" => (target, extra) = (" target=\"fr\"", "<iframe name=\"fr\" src=\"/blank\"></iframe>"),
        _ => {}
    }
    format!(
        "<!doctype html><title>form</title>{extra}<form method=\"{method}\" action=\"{action}\"{enctype}{target}><input name=\"a\" value=\"1\">\
         <input name=\"b\" value=\"x y\"></form><script>onload = function () {{ setTimeout(function () \
         {{ document.forms[0].submit(); }}, 300); }};</script>"
    )
}

type Log = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn serve<S: Read + Write>(s: S, port: u16, log: Log) {
    let mut r = BufReader::new(s);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let req = line.trim_end_matches(['\r', '\n']).to_string();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end_matches(['\r', '\n']).to_string();
            if h.is_empty() {
                break;
            }
            headers.push(h);
        }
        let n = headers
            .iter()
            .find_map(|h| h.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        let mut body = vec![0u8; n];
        if n > 0 && r.read_exact(&mut body).is_err() {
            return;
        }
        log.lock().unwrap().push((req.clone(), headers));
        if req.starts_with("OPTIONS") {
            let out = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\n\
                       Access-Control-Allow-Methods: *\r\nContent-Length: 0\r\n\r\n";
            if r.get_mut().write_all(out.as_bytes()).is_err() {
                return;
            }
            continue;
        }
        let path = req.split(' ').nth(1).unwrap_or("/").to_string();
        let (p, q) = path.split_once('?').unwrap_or((path.as_str(), ""));
        let (b, ct) = match p {
            "/" if q.starts_with("kind=") => (form(&q[5..], port), "text/html"),
            "/sub" => (SUB.replace("PORT", &port.to_string()), "text/html"),
            "/hdr" => (HDR.to_string(), "text/html"),
            "/tls" => (TLS.to_string(), "text/html"),
            "/r/css" => ("p{color:red}".to_string(), "text/css"),
            "/r/js" => ("var a=1;".to_string(), "text/javascript"),
            "/r/frame" | "/blank" | "/post" => ("<!doctype html><title>x</title>x".to_string(), "text/html"),
            "/favicon.ico" => (String::new(), "image/x-icon"),
            _ => ("ok".to_string(), "text/plain"),
        };
        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\r\n{b}", b.len());
        if r.get_mut().write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// The recording's shape of `log`: per request line its first request's header lines, the port written `P` and a
/// multipart boundary `----WebKitFormBoundaryX`. A `Cookie` line is left out: the test binary's tests share one cookie
/// jar, and another test's cookie for this host is no part of what is compared (Chrome's fresh profile sent none).
fn normalize(log: &[(String, Vec<String>)], port: u16) -> BTreeMap<String, Vec<String>> {
    let fix = |s: &str| {
        let s = s.replace(&port.to_string(), "P");
        match s.find("----WebKitFormBoundary") {
            Some(i) if s.len() >= i + 38 => format!("{}----WebKitFormBoundaryX{}", &s[..i], &s[i + 38..]),
            _ => s,
        }
    };
    let mut out = BTreeMap::new();
    for (req, headers) in log {
        out.entry(fix(req)).or_insert_with(|| headers.iter().filter(|h| !h.starts_with("Cookie:")).map(|h| fix(h)).collect());
    }
    out
}

/// A TLS server that speaks HTTP/1.1 only (ALPN `http/1.1`), as `record.py`'s `tls` case.
fn tls_acceptor() -> Arc<btls::ssl::SslAcceptor> {
    use btls::ssl::{select_next_proto, AlpnError, SslAcceptor, SslFiletype, SslMethod};
    let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).expect("an acceptor");
    b.set_certificate_chain_file(format!("{TLS_DIR}/tls-leaf.pem")).expect("the certificate");
    b.set_private_key_file(format!("{TLS_DIR}/tls-leaf.key"), SslFiletype::PEM).expect("the key");
    b.set_alpn_select_callback(|_, client| select_next_proto(b"\x08http/1.1", client).ok_or(AlpnError::NOACK));
    Arc::new(b.build())
}

/// Load case `case` in a fresh engine -- `korvus fetch`'s loop, the page's navigation followed -- until every
/// request Chrome made has arrived (and a moment more), and answer what reached the wire.
async fn wire(case: &str, want: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let tls = (case == "tls").then(tls_acceptor);
    {
        let log = log.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let log = log.clone();
                let tls = tls.clone();
                std::thread::spawn(move || match tls {
                    Some(acc) => {
                        if let Ok(t) = acc.accept(s) {
                            serve(t, port, log);
                        }
                    }
                    None => serve(s, port, log),
                });
            }
        });
    }
    let url = match case {
        "sub" | "hdr" => format!("http://127.0.0.1:{port}/{case}"),
        "tls" => format!("https://127.0.0.1:{port}/tls"),
        _ => format!("http://127.0.0.1:{port}/?kind={case}"),
    };
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let mut complete: Option<Instant> = None;
    while start.elapsed() < Duration::from_secs(20) && complete.is_none_or(|t| t.elapsed() < Duration::from_millis(500)) {
        if let Some(nav) = e.take_due_navigation() {
            e.fire_beforeunload();
            let next = transport::fetch(engine::navigation_request(&nav, e.persona().as_ref())).await.expect("the page's navigation");
            e.fire_unload();
            e = e.successor();
            e.arrive(engine::arrival_for(&nav, &next.url));
            e.load_page_response(&next.bytes, &next.url, &[], &next.headers).await.expect("load next");
            continue;
        }
        let _ = e.pump(Duration::from_millis(50)).await;
        if complete.is_none() && want.keys().all(|k| normalize(&log.lock().unwrap(), port).contains_key(k)) {
            complete = Some(Instant::now());
        }
    }
    let got = log.lock().unwrap().clone();
    normalize(&got, port)
}

async fn check(case: &str) {
    let chrome: BTreeMap<String, BTreeMap<String, Vec<String>>> = serde_json::from_str(CHROME).expect("the recording is JSON");
    let want = chrome.get(case).expect("the case is recorded");
    let got = wire(case, want).await;
    for (req, lines) in want {
        assert_eq!(got.get(req), Some(lines), "{case}: `{req}` on the wire, korvus vs Chrome 150");
    }
    let extra: Vec<&String> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "{case}: requests Chrome 150 did not make: {extra:?}");
}

#[tokio::test]
async fn an_urlencoded_form_post_is_written_as_chromes() {
    check("urlencoded").await;
}

#[tokio::test]
async fn a_multipart_form_post_is_written_as_chromes() {
    check("multipart").await;
}

#[tokio::test]
async fn a_text_plain_form_post_is_written_as_chromes() {
    check("plain").await;
}

#[tokio::test]
async fn a_form_get_is_written_as_chromes() {
    check("get").await;
}

#[tokio::test]
async fn a_cross_site_form_post_is_written_as_chromes() {
    check("cross").await;
}

#[tokio::test]
async fn a_form_post_into_a_frame_is_written_as_chromes() {
    check("frame").await;
}

#[tokio::test]
async fn a_pages_subresources_are_written_as_chromes() {
    check("sub").await;
}

#[tokio::test]
async fn the_headers_a_page_sets_are_written_as_chromes() {
    check("hdr").await;
}

/// In a CHILD process of this test binary: the transport reads `KORVUS_EXTRA_CA` when it first builds a wire's
/// client and keeps that client for the process, so the test CA must be in the environment before any test of the
/// binary has made a request -- which only a fresh process guarantees.
#[tokio::test]
async fn an_http1_tls_wire_carries_no_priority_as_chromes() {
    if std::env::var_os("WIRE_SPELLING_TLS_CHILD").is_some() {
        check("tls").await;
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["an_http1_tls_wire_carries_no_priority_as_chromes", "--test-threads=1", "--nocapture"])
        .env("WIRE_SPELLING_TLS_CHILD", "1")
        .env("KORVUS_EXTRA_CA", format!("{TLS_DIR}/tls-ca.pem"))
        .output()
        .expect("the child test process");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && text.contains("1 passed"), "the tls case in its own process:\n{text}");
}

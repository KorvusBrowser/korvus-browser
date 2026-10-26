//! A frame's scripts under the frame document's OWN policy list, as Chrome 150 runs them.
//!
//! HTML's policy container: an `about:srcdoc` or `about:blank` document holds its creator's policies, a fetched one
//! its response's, and either adds its `<head>` metas. korvus ran a frame's scripts under its metas only: a srcdoc
//! frame of a `script-src 'nonce-abc'` page ran its inline and external scripts, an `about:blank` frame its
//! `document.write`n inline one, and a frame whose response said `script-src 'none'` everything.
//!
//! The page (`script-src 'nonce-abc'`) makes four frames, each script of which posts its name to the page: a srcdoc
//! frame (inline, nonce'd, external), an `about:blank` frame written by script (inline, nonce'd), a same-origin frame
//! whose response says `script-src 'none'` (inline, external), and one whose `<meta>` says `script-src 'self'`
//! (inline, external). Chrome 150.0.7871.46, headed under Xvfb, three runs alike: only the nonce'd two and the meta
//! frame's same-origin external run.
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const CHROME_BUNDLE: &[&str] = &["blank:nonce", "meta:external", "srcdoc:nonce"];

const PAGE: &str = r#"<!doctype html><html><head><title>csp</title></head><body>
<script nonce="abc">
window.__got = [];
addEventListener('message', function (e) { window.__got.push(e.data); });
var a = document.createElement('iframe');
a.srcdoc = '<script>parent.postMessage("srcdoc:inline", "*")<\/script><script nonce="abc">parent.postMessage("srcdoc:nonce", "*")<\/script><script src="/s.js?srcdoc"><\/script>';
document.body.appendChild(a);
var b = document.createElement('iframe'); document.body.appendChild(b);
b.contentDocument.open(); b.contentDocument.write('<script>parent.postMessage("blank:inline", "*")<\/script><script nonce="abc">parent.postMessage("blank:nonce", "*")<\/script>'); b.contentDocument.close();
var c = document.createElement('iframe'); c.src = '/hdr'; document.body.appendChild(c);
var d = document.createElement('iframe'); d.src = '/meta'; document.body.appendChild(d);
</script></body></html>"#;
const HDR: &str = r#"<!doctype html><script>parent.postMessage("hdr:inline", "*")</script><script src="/s.js?hdr"></script>"#;
const META: &str = r#"<!doctype html><head><meta http-equiv="Content-Security-Policy" content="script-src 'self'"></head><script>parent.postMessage("meta:inline", "*")</script><script src="/s.js?meta"></script>"#;

fn serve(s: std::net::TcpStream) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
        }
        let script;
        let (ty, csp, body): (&str, Option<&str>, &str) = match path.as_str() {
            "/" => ("text/html", Some("script-src 'nonce-abc'"), PAGE),
            "/hdr" => ("text/html", Some("script-src 'none'"), HDR),
            "/meta" => ("text/html", None, META),
            p if p.starts_with("/s.js?") => {
                script = format!("parent.postMessage(\"{}:external\", \"*\")", &p[6..]);
                ("text/javascript", None, script.as_str())
            }
            _ => ("text/plain", None, ""),
        };
        let csp = csp.map(|c| format!("Content-Security-Policy: {c}\r\n")).unwrap_or_default();
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\n{csp}Content-Length: {}\r\n\r\n", body.len());
        if w.write_all(format!("{head}{body}").as_bytes()).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_frames_scripts_run_under_its_own_policies_as_chromes_do() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    // every frame has loaded and every script that may run has posted by then
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        let _ = e.pump(Duration::from_millis(50)).await;
    }
    let got = e.eval_value("r", "JSON.stringify(window.__got.slice().sort())".to_string()).expect("eval");
    let got: Vec<String> = serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list");
    assert_eq!(got, CHROME_BUNDLE, "the scripts that ran, korvus vs Chrome 150");
}

/// A WORKER'S policies, as Chrome 150 holds them: a `blob:` worker's are its creator's (no response delivered its
/// script), a fetched worker's its response's -- `eval` and `new Function` refused under a `script-src` without
/// `'unsafe-eval'`, with Blink's message naming the directive. korvus's workers held none: all three evaluated.
/// Chrome 150.0.7871.46, headed under Xvfb (2026-10-11).
#[tokio::test]
async fn a_workers_policies_are_its_creators_or_its_responses_as_chromes_are() {
    const BODY: &str = "var r = {};\
        try { r.eval = String(eval('1+1')); } catch (e) { r.eval = e.name + ': ' + e.message; }\
        try { r.fn = String(new Function('return 2')()); } catch (e) { r.fn = e.name + ': ' + e.message; }\
        postMessage(r);";
    let refused = |directive: &str| {
        format!("EvalError: Evaluating a string as JavaScript violates the following Content Security Policy directive because 'unsafe-eval' is not an allowed source of script: {directive}\".\n")
    };
    let page = format!(
        "<!doctype html><script nonce=\"abc\">window.__w = {{}};\
         function on(k, w) {{ w.onmessage = function (e) {{ window.__w[k] = e.data; }}; }}\
         on('blob', new Worker(URL.createObjectURL(new Blob([{body}], {{type: 'text/javascript'}}))));\
         on('plain', new Worker('/w.js')); on('strict', new Worker('/w2.js'));</script>",
        body = serde_json::to_string(BODY).unwrap()
    );
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let page = page.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 {
                            return;
                        }
                        if h.trim_end().is_empty() {
                            break;
                        }
                    }
                    let (ty, csp, body): (&str, Option<&str>, &str) = match path.as_str() {
                        "/" => ("text/html", Some("script-src 'nonce-abc' blob: 'self'"), page.as_str()),
                        "/w.js" => ("text/javascript", None, BODY),
                        "/w2.js" => ("text/javascript", Some("script-src 'self'"), BODY),
                        _ => ("text/plain", None, ""),
                    };
                    let csp = csp.map(|c| format!("Content-Security-Policy: {c}\r\n")).unwrap_or_default();
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\n{csp}Content-Length: {}\r\n\r\n", body.len());
                    if w.write_all(format!("{head}{body}").as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let p = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("w", "Object.keys(window.__w).length === 3 ? JSON.stringify(window.__w) : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str::<serde_json::Value>(s).expect("the workers' answers");
        }
        assert!(start.elapsed() < Duration::from_secs(10), "the workers never answered");
    };
    let blob = refused("script-src 'nonce-abc' blob: 'self'");
    let strict = refused("script-src 'self'");
    assert_eq!(got["blob"], serde_json::json!({"eval": blob, "fn": blob}), "the blob: worker (its creator's policy)");
    assert_eq!(got["plain"], serde_json::json!({"eval": "2", "fn": "2"}), "the fetched worker with no policy");
    assert_eq!(got["strict"], serde_json::json!({"eval": strict, "fn": strict}), "the fetched worker under its response's policy");
}

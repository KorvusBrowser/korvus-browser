//! A document in an OPAQUE origin -- a frame sandboxed without `allow-same-origin` -- and the blob worker it makes
//! request as that origin: no `Referer` (Referrer Policy "determine request's referrer": an opaque origin returns no
//! referrer), `Origin: null` wherever an `Origin` is sent, `Sec-Fetch-Site: cross-site` to every URL, the page's own
//! included; and a message it posts reads `origin` "null".
//!
//! korvus requested as the PAGE: its `Referer`, its `Origin`, `same-origin` to the page's host, and the worker's
//! fetch as the browser's own (`none`, no `Origin`); the message's `origin` was "". a bot-detection script's risk frame
//! (`about:srcdoc`, `sandbox=allow-scripts`) loads `ri.px-cloud.net/index.js` and posts to `dvp.px-cloud.net` from
//! there; Chrome's requests carry no Referer and `Origin: null` (recorded sessions).
//!
//! Chrome 150.0.7871.46 on this machine, headed, no CDP, three runs alike: the
//! page at A (127.0.0.1) makes a sandboxed srcdoc frame whose `<script src>`, `<script crossorigin>`, `<img>`,
//! `fetch` (GET to B and to A, POST to B), XHR and blob worker `fetch` each hit `/echo`.
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One line per `/echo` request: `METHOD host path | referer | origin | sec-fetch-site`.
type Log = Arc<Mutex<Vec<String>>>;

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 16384];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let mut words = req.split_whitespace();
            let method = words.next().unwrap_or("").to_string();
            let target = words.next().unwrap_or("/").to_string();
            let header = |name: &str| {
                req.lines()
                    .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_else(|| "-".to_string())
            };
            let host = s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            if target.starts_with("/echo") {
                log.lock().unwrap().push(format!("{method} {host} {target} | {} | {} | {}", header("referer"), header("origin"), header("sec-fetch-site")));
            }
            let body = "ok";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_sandboxed_frame_and_its_worker_request_as_an_opaque_origin() {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (oa, ob) = (format!("http://{}", a.local_addr().unwrap()), format!("http://{}", b.local_addr().unwrap()));
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(a, log.clone()));
    tokio::spawn(serve(b, log.clone()));
    let frame = format!(
        "<script src=\"{ob}/echo?s\"></script><script crossorigin src=\"{ob}/echo?sc\"></script><img src=\"{oa}/echo?i\"><script>\
var W = \"fetch('{ob}/echo?w').then(function(){{postMessage('w')}},function(){{postMessage('w')}})\";\
fetch('{ob}/echo?f');\
fetch('{oa}/echo?fa');\
fetch('{ob}/echo?p', {{method: 'POST', body: 'x', headers: {{'Content-Type': 'text/plain'}}}});\
var x = new XMLHttpRequest(); x.open('GET', '{ob}/echo?x'); x.send();\
var w = new Worker(URL.createObjectURL(new Blob([W], {{type: 'text/javascript'}})));\
w.onmessage = function () {{ parent.postMessage('done ' + self.origin, '*'); }};\
</script>"
    );
    let page = format!(
        "<!doctype html><html><body><script>\
var f = document.createElement('iframe'); f.setAttribute('sandbox', 'allow-scripts'); f.srcdoc = {};\
addEventListener('message', function (e) {{ setTimeout(function () {{ window.RESULT = e.origin + '|' + e.data; }}, 500); }});\
document.body.appendChild(f);\
</script></body></html>",
        serde_json::to_string(&frame).unwrap().replace("</", "<\\/")
    );
    // the persona the rows were measured on: Chrome's (the shipped one blocks third-party cookies, and its
    // `Sec-Fetch-Storage-Access: inactive` loads name an `Origin`)
    let chrome = persona::CapturedPersona::select(Some("chrome-152-linux-20261016T1022")).expect("chrome bundle");
    let mut e = Engine::new(Box::new(chrome));
    e.run().await.expect("engine boot");
    e.load_page(&page, &format!("{oa}/page.html"), &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let result = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("opaque", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the sandboxed frame never posted");
    };
    let mut got: Vec<String> = log.lock().unwrap().iter().map(|r| r.replace(&oa, "A").replace(&ob, "B")).collect();
    got.sort();
    assert_eq!(result, "null|done null");
    assert_eq!(
        got,
        [
            "GET 127.0.0.1 /echo?fa | - | null | cross-site",
            "GET 127.0.0.1 /echo?i | - | - | cross-site",
            "GET 127.0.0.2 /echo?f | - | null | cross-site",
            "GET 127.0.0.2 /echo?s | - | - | cross-site",
            "GET 127.0.0.2 /echo?sc | - | null | cross-site",
            "GET 127.0.0.2 /echo?w | - | null | cross-site",
            "GET 127.0.0.2 /echo?x | - | null | cross-site",
            "POST 127.0.0.2 /echo?p | - | null | cross-site",
        ]
    );
}

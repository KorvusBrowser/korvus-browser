//! A frame's fetch from an ASYNCHRONOUS callback is the FRAME's request.
//!
//! Measured on Chrome 150: a frame's `fetch` resolves against the frame's document and
//! carries it as the referrer; `browsing_context::frame_request_client` answers that for the running
//! frame. `fetch` asked it; `XMLHttpRequest` and `new Request` read the PAGE's base whichever realm
//! called them, so a frame's XHR went out to the embedding page's host: dev.to's YouTube embed posted
//! `/youtubei/v1/log_event` to dev.to. A frame is its own realm now (F9), its callbacks -- a timer, a
//! microtask, a listener, an animation frame, an observer -- run as it, and all three APIs ask which
//! realm is running.
use engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Routes = Arc<HashMap<String, String>>;
/// One line per `/echo` request: `host target | referer`.
type Log = Arc<Mutex<Vec<String>>>;

async fn serve(listener: TcpListener, routes: Routes, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let routes = routes.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 16384];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let path = target.split('?').next().unwrap_or("/").to_string();
            let host = s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            if path == "/echo" {
                let referer = req
                    .lines()
                    .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("referer")).map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_else(|| "-".to_string());
                log.lock().unwrap().push(format!("{host} {target} | {referer}"));
            }
            let body = routes.get(&path).cloned().unwrap_or_else(|| "ok".to_string());
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_frames_timer_microtask_and_listener_fetch_as_the_frame() {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (oa, ob) = (format!("http://{}", a.local_addr().unwrap()), format!("http://{}", b.local_addr().unwrap()));
    let frame = r#"<!doctype html><html><body><script>
        setTimeout(() => fetch('/echo?t=timer'), 0);
        Promise.resolve().then(() => fetch('/echo?t=microtask'));
        Promise.resolve().then(() => { const x = new XMLHttpRequest(); x.open('GET', '/echo?t=xhr'); x.send(); });
        requestAnimationFrame(() => fetch('/echo?t=raf'));
        new MutationObserver(() => fetch('/echo?t=mutation')).observe(document.body, { childList: true });
        document.body.appendChild(document.createElement('i'));
        addEventListener('message', (e) => {
          if (e.data !== 'go') return;
          fetch(new Request('/echo?t=request')).then(() => fetch('/echo?t=listener')).then(() => parent.postMessage('done', '*'));
        });
        </script></body></html>"#;
    let page = format!(
        r#"<!doctype html><html><body><script>
        window.R = null;
        addEventListener('message', (e) => {{ if (e.data === 'done') window.R = 'done'; }});
        const f = document.createElement('iframe');
        f.onload = () => setTimeout(() => f.contentWindow.postMessage('go', '*'), 50);
        f.src = '{ob}/f.html';
        document.body.appendChild(f);
        </script></body></html>"#
    );
    let mut routes = HashMap::new();
    routes.insert("/f.html".to_string(), frame.to_string());
    let routes: Routes = Arc::new(routes);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(serve(a, routes.clone(), log.clone()));
    tokio::spawn(serve(b, routes.clone(), log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, &format!("{oa}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        let v = e.eval_value("read", "String(window.R)".to_string()).expect("eval");
        if v.as_str() == Some("done") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the frame never reported back; log: {:?}", log.lock().unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut got = log.lock().unwrap().clone();
    got.sort();
    let frame_url = format!("{ob}/f.html");
    let want: Vec<String> =
        ["listener", "microtask", "mutation", "raf", "request", "timer", "xhr"].iter().map(|t| format!("127.0.0.2 /echo?t={t} | {frame_url}")).collect();
    assert_eq!(got, want, "each request goes to the frame's origin with the frame's document as its referrer");
}

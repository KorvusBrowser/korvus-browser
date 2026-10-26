//! A sandboxed frame runs no script unless its flags allow it (HTML "sandboxed scripts browsing context flag"):
//! the `sandbox` attribute, the parent's flags (they reach every nested frame), and the response's
//! `Content-Security-Policy: sandbox` directive, each a list whose `allow-scripts` / `allow-same-origin` must be in
//! every one that names it. korvus ran every frame's scripts it loaded -- parser, fetched, an `onerror` attribute, a
//! `data:` frame, a frame under a CSP `sandbox` header -- and kept a CSP-sandboxed frame's origin; it also never loaded
//! a nested `<iframe srcdoc>` from a frame's markup, nor compiled a frame's `<body onload>`.
//!
//! Chrome 150.0.7871.46, headed on the Wayland desktop, no DevTools, three runs alike: of
//! the fifteen frames below, B, E, E2 (origin "null"), F, G, I and M2 run script.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const POST: &str = "<script>parent.postMessage('{K}','*')</script>";

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split(' ').nth(1).unwrap_or("/").to_string();
            let q = |k: &str| path.split(&format!("{k}=")).nth(1).map(|v| v.split('&').next().unwrap_or("").replace("%20", " ")).unwrap_or_default();
            let (body, csp) = if path.starts_with("/child") {
                (POST.replace("{K}", &q("k")), None)
            } else if path.starts_with("/csp2") {
                ("<script>parent.postMessage('E2:' + self.origin, '*')</script>".to_string(), Some(q("p")))
            } else if path.starts_with("/csp") {
                (POST.replace("{K}", &q("k")), Some(q("p")))
            } else {
                (String::new(), None)
            };
            let csp = csp.map(|p| format!("Content-Security-Policy: {p}\r\n")).unwrap_or_default();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{csp}Cache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

#[tokio::test]
async fn a_sandboxed_frame_runs_only_the_scripts_its_flags_allow() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let o = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l));
    let page = format!(
        r#"<!doctype html><html><body><script>
var got = []; addEventListener('message', function (e) {{ got.push(String(e.data)); }});
function frame(attrs) {{ var f = document.createElement('iframe'); for (var k in attrs) f.setAttribute(k, attrs[k]); document.body.appendChild(f); }}
var S = '</scr' + 'ipt>';
frame({{sandbox: '', srcdoc: "<script>parent.postMessage('A','*')" + S}});
frame({{sandbox: 'allow-scripts', srcdoc: "<script>parent.postMessage('B','*')" + S}});
frame({{sandbox: '', src: '{o}/child?k=C'}});
frame({{src: '{o}/csp?k=D&p=sandbox'}});
frame({{src: '{o}/csp?k=E&p=sandbox%20allow-scripts'}});
frame({{sandbox: 'allow-scripts', srcdoc: "<iframe srcdoc=\"<script>top.postMessage('F','*')" + S + "\"></iframe>"}});
frame({{sandbox: 'allow-scripts allow-same-origin', srcdoc: "<iframe srcdoc=\"<script>top.postMessage('G','*')" + S + "\"></iframe>"}});
frame({{sandbox: '', srcdoc: "<img src=/nope onerror=\"parent.postMessage('H','*')\">"}});
frame({{sandbox: 'allow-scripts', srcdoc: "<img src=/nope onerror=\"parent.postMessage('I','*')\">"}});
frame({{sandbox: '', src: "data:text/html,<script>parent.postMessage('J','*')" + S}});
frame({{src: '{o}/csp?k=K&p=sandbox%20allow-scripts', sandbox: ''}});
frame({{sandbox: 'allow-scripts', src: '{o}/csp?k=L&p=sandbox'}});
frame({{sandbox: '', srcdoc: "<body onload=\"parent.postMessage('M','*')\">"}});
frame({{src: '{o}/csp2?k=E2&p=sandbox%20allow-scripts'}});
frame({{srcdoc: "<body onload=\"parent.postMessage('M2','*')\">"}});
setTimeout(function () {{ got.sort(); window.RESULT = JSON.stringify(got); }}, 2000);
</script></body></html>"#
    );
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    let start = std::time::Instant::now();
    let got = loop {
        let _ = e.pump(Duration::from_millis(50)).await;
        if let Some(r) = e.eval_value("sb", "window.RESULT || ''".to_string()).ok().and_then(|v| v.as_str().map(str::to_string)).filter(|r| !r.is_empty()) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no result");
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(got, serde_json::json!(["B", "E", "E2:null", "F", "G", "I", "M2"]));
}

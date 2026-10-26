//! The subresource-execution pipeline. `Engine::load_page` fetches external `<script src>`
//! over the stealth transport and executes them in document order, so a real site's JS (e.g. its
//! instrumentation) actually runs. Here a local HTTP server serves an external script; the page loads it and the
//! script's side effect is observed.
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
async fn serve(listener: TcpListener, content_type: &'static str, body: &'static str) {
    loop {
        let (mut s, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    }
}

/// An external `<script src>` (referenced by a relative URL, resolved against the base) is fetched over
/// transport and executed — its global side effect is visible.
#[tokio::test]
async fn s0_external_script_is_fetched_and_executed() {
    let js = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = js.local_addr().unwrap();
    tokio::spawn(serve(js, "application/javascript", "globalThis.__ext = 'ran'; globalThis.__extNum = 41 + 1;"));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let base = format!("http://{addr}/");
    // A RELATIVE src, resolved against the base; plus an inline script that must run AFTER it (doc order).
    let html = "<!doctype html><html><head>\
        <script src=\"/ext.js\"></script>\
        <script>globalThis.__order = (globalThis.__ext === 'ran') ? 'external-first' : 'inline-first';</script>\
        </head><body></body></html>";
    e.load_page(html, &base, &[]).await.expect("load_page");
    e.run().await.expect("run");

    let ext = e.eval_value("chk-ext", "globalThis.__ext".to_string()).expect("eval");
    let num = e.eval_value("chk-num", "globalThis.__extNum".to_string()).expect("eval");
    let order = e.eval_value("chk-order", "globalThis.__order".to_string()).expect("eval");
    assert_eq!(ext.as_str(), Some("ran"), "external <script src> must be fetched + executed");
    assert_eq!(num.as_i64(), Some(42), "external script's computed value must be present");
    assert_eq!(order.as_str(), Some("external-first"), "external script must run BEFORE the later inline (document order)");
}

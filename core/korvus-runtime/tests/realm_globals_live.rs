//! A frame's scripts run in THEIR OWN V8 CONTEXT.
//!
//! They ran in the page's context, inside `with` scopes over a frame window object, so everything the
//! language resolves against the REALM rather than the scope answered for the page:
//! `Function('return this')()` and `(0, eval)('this')` returned the page's window, and the frame's
//! `Array` was the page's. a challenge provider a challenge widget's challenge frame reads its global exactly that way,
//! looked its elements up in the page's document, and threw `null.appendChild` on seven sites of the
//! C2 live run. The first test is fixture `F-realm-globals` (`tools/realweb/seed/realm/`), Chrome's
//! answer read from the manifest; the others pin what a realm of its own means for a frame's scripts.
use engine::Engine;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Routes = Arc<HashMap<String, String>>;

async fn serve(listener: TcpListener, routes: Routes) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let routes = routes.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
            let body = routes.get(&path).cloned().unwrap_or_default();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

/// Serve `routes`, load `/index.html`, and return `window.R` once the page has set it to a string.
async fn run(routes: HashMap<String, String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let page = routes["/index.html"].clone();
    tokio::spawn(serve(listener, Arc::new(routes)));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&page, &format!("{origin}/index.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        let v = e.eval_value("read", "typeof window.R === 'string' ? window.R : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
        assert!(start.elapsed() < Duration::from_secs(20), "window.R never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn routes(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[tokio::test]
async fn a_frames_function_and_eval_return_its_own_window_as_chrome_measured() {
    let got = run(routes(&[
        ("/index.html", include_str!("../../../tools/realweb/seed/realm/index.html")),
        ("/frame.html", include_str!("../../../tools/realweb/seed/realm/frame.html")),
    ]))
    .await;
    let m: serde_json::Value = serde_json::from_str(include_str!("../../../tools/realweb/seed/manifest.json")).unwrap();
    let want = m["F-realm-globals"]["chrome_expected"].as_str().unwrap();
    assert_eq!(got, want);
}

/// What a global scope of its own means: a later script sees an earlier one's `let` and `function`,
/// a name the frame never defined is a ReferenceError even when the PAGE defines it, and a bare
/// `addEventListener` is the frame window's -- its listener runs for the frame's `message`, and the
/// page's `message` does not reach it.
#[tokio::test]
async fn a_frames_scripts_share_its_global_scope_and_only_its() {
    let frame = r#"<!doctype html><html><body><script>
        let shared = 'lex'; function named() { return 'fn'; }
        </script><script>
        var out = [typeof shared === 'undefined' ? 'none' : shared, named()];
        try { pageOnly; out.push('leaked'); } catch (e) { out.push(e.name); }
        addEventListener('message', (e) => { if (e.data === 'ping-frame') parent.postMessage('frame-heard', '*'); });
        parent.postMessage(JSON.stringify(out), '*');
        </script></body></html>"#;
    let page = r#"<!doctype html><html><body><script>
        var pageOnly = 1;
        window.R = null; var got = [];
        addEventListener('message', (e) => {
          got.push(e.data);
          if (got.length === 1) document.querySelector('iframe').contentWindow.postMessage('ping-frame', '*');
          if (got.length === 2) window.R = JSON.stringify(got);
        });
        const f = document.createElement('iframe'); f.src = 'frame.html'; document.body.appendChild(f);
        </script></body></html>"#;
    let got = run(routes(&[("/index.html", page), ("/frame.html", frame)])).await;
    assert_eq!(got, r#"["[\"lex\",\"fn\",\"ReferenceError\"]","frame-heard"]"#);
}

/// A navigation gives the frame a NEW global scope behind the SAME window proxy: the second document
/// may declare the `let` the first one declared, and `contentWindow` is still the object the parent
/// held.
#[tokio::test]
async fn a_frames_next_document_starts_a_fresh_scope_behind_the_same_window() {
    let one = r#"<!doctype html><script>let n = 1; parent.postMessage('one', '*');</script>"#;
    let two = r#"<!doctype html><script>let n = 2; parent.postMessage('two:' + n, '*');</script>"#;
    let page = r#"<!doctype html><html><body><script>
        window.R = null; var seen = [];
        const f = document.createElement('iframe'); f.src = 'one.html'; document.body.appendChild(f);
        const held = f.contentWindow;
        addEventListener('message', (e) => {
          seen.push(e.data);
          if (e.data === 'one') f.src = 'two.html';
          else window.R = JSON.stringify([seen, held === f.contentWindow]);
        });
        </script></body></html>"#;
    let got = run(routes(&[("/index.html", page), ("/one.html", one), ("/two.html", two)])).await;
    assert_eq!(got, r#"[["one","two:2"],true]"#);
}

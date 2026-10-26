//! A document's CROSS-ORIGIN ISOLATED CAPABILITY, as Chrome 150 gives it: its agent cluster is
//! isolated -- the TOP document's COOP + COEP -- AND the `cross-origin-isolated` feature is enabled in it, by its
//! container (`allow`; the feature's default allowlist is `self`) and by its own `Permissions-Policy` (HTML; Blink's
//! `LocalDOMWindow::CrossOriginIsolatedCapability`). That one answer decides `crossOriginIsolated`, the
//! `SharedArrayBuffer` global and the clock's grain (5 µs, else 100 µs).
//!
//! korvus answered the agent cluster alone: a cross-site frame without `allow="cross-origin-isolated"` read true, with
//! `SharedArrayBuffer` and a 5 µs clock (its out-of-process engine decided by the frame's own COOP and COEP), and so
//! did a frame its container or its own header denied the feature, and a page that denied it to itself.
//!
//! Each document reports `crossOriginIsolated / typeof SharedArrayBuffer / clock`, the clock "fine" when two readings
//! ever differ by less than 50 µs. Chrome 150.0.7871.46, headed under Xvfb, two runs alike. One answer is left out:
//! the clock of the same-origin frame whose OWN header denies the feature -- Chrome reads it fine, because that frame
//! reuses its initial `about:blank` window and with it the `Performance` object (and grain) made before the response
//! arrived (`DocumentLoader`'s `ShouldReuseDOMWindow`); korvus makes each document its own clock.
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const CHROME_150_PAGE: &str = r#"{"cross":"false/undefined/coarse","cross-allow":"true/function/fine","page":"true/function/fine","same":"true/function/fine","same-allow":"true/function/fine","same-none":"false/undefined/coarse","same-ppself":"false/undefined","srcdoc-none":"false/undefined/coarse"}"#;
const CHROME_150_DENYING_PAGE: &str = r#"{"page":"false/undefined/coarse","pp-same":"false/undefined/coarse","pp-same-allow":"false/undefined/coarse"}"#;

const READ: &str = "function read(clock) { var step = 1e9, a = performance.now(), n = 0;\
    for (var i = 0; i < 200000 && n < 20; i++) { var b = performance.now(); if (b > a) { step = Math.min(step, b - a); a = b; n++; } }\
    var r = [crossOriginIsolated, typeof SharedArrayBuffer]; if (clock) r.push(step < 0.05 ? 'fine' : 'coarse'); return r.join('/'); }";

fn page(frames: &[&str]) -> String {
    format!(
        "<!doctype html><title>coi</title><script>{READ}\nwindow.__got = {{page: read(true)}};\
         addEventListener('message', function (e) {{ window.__got[e.data[0]] = e.data[1]; }});</script>{}",
        frames.concat()
    )
}

fn frame() -> String {
    format!("<!doctype html><script>{READ}\nparent.postMessage([location.search.slice(1), read(location.search.indexOf('ppself') < 0)], '*')</script>")
}

fn serve(s: std::net::TcpStream, port: u16) {
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
        let srcdoc = format!(
            "<script>{} parent.postMessage([&quot;srcdoc-none&quot;, read(true)], &quot;*&quot;)</script>",
            READ.replace('"', "&quot;")
        );
        let body = match path.as_str() {
            "/" => page(&[
                "<iframe src=\"/f?same\"></iframe>",
                "<iframe src=\"/f?same-allow\" allow=\"cross-origin-isolated\"></iframe>",
                &format!("<iframe src=\"http://localhost:{port}/f?cross\"></iframe>"),
                &format!("<iframe src=\"http://localhost:{port}/f?cross-allow\" allow=\"cross-origin-isolated\"></iframe>"),
                "<iframe src=\"/f?same-ppself\"></iframe>",
                "<iframe src=\"/f?same-none\" allow=\"cross-origin-isolated 'none'\"></iframe>",
                &format!("<iframe srcdoc=\"{srcdoc}\" allow=\"cross-origin-isolated 'none'\"></iframe>"),
            ]),
            "/pp" => page(&["<iframe src=\"/f?pp-same\"></iframe>", "<iframe src=\"/f?pp-same-allow\" allow=\"cross-origin-isolated\"></iframe>"]),
            p if p.starts_with("/f?") => frame(),
            _ => String::new(),
        };
        let denied = if path == "/pp" || path.contains("ppself") { "Permissions-Policy: cross-origin-isolated=()\r\n" } else { "" };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCross-Origin-Opener-Policy: same-origin\r\n\
             Cross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: cross-origin\r\n{denied}Content-Length: {}\r\n\r\n",
            body.len()
        );
        if w.write_all(format!("{head}{body}").as_bytes()).is_err() {
            return;
        }
    }
}

/// Load `path` of the isolated server and read what every document reported, once all `want` have.
async fn reported(path: &str, want: usize) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, port));
        }
    });
    let url = format!("http://127.0.0.1:{port}{path}");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the documents never all reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "JSON.stringify(window.__got)".to_string()).expect("eval");
        let got: serde_json::Value = serde_json::from_str(v.as_str().unwrap_or("{}")).expect("the reports");
        if got.as_object().is_some_and(|o| o.len() == want) {
            return got;
        }
    }
}

/// An isolated page's frames: same-origin, granted, cross-site, denied by the container, denied by the frame itself.
#[tokio::test]
async fn a_frames_isolation_is_its_documents_capability_as_chromes_is() {
    let got = reported("/", 8).await;
    let want: serde_json::Value = serde_json::from_str(CHROME_150_PAGE).unwrap();
    assert_eq!(got, want, "crossOriginIsolated / SharedArrayBuffer / clock, korvus vs Chrome 150");
}

/// An isolated page that denies itself `cross-origin-isolated`: neither it nor any frame of it has the capability.
#[tokio::test]
async fn a_page_denying_itself_the_feature_is_not_isolated_as_in_chrome() {
    let got = reported("/pp", 3).await;
    let want: serde_json::Value = serde_json::from_str(CHROME_150_DENYING_PAGE).unwrap();
    assert_eq!(got, want, "crossOriginIsolated / SharedArrayBuffer / clock, korvus vs Chrome 150");
}

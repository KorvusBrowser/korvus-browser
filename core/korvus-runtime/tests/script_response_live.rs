//! A `<script src>` whose response is not OK is NOT executed, and its element fires
//! `error`; one whose response is OK runs and fires `load`. In a live engine, over loopback.
//!
//! # The defect, measured before the fix
//!
//! On master at `0a21edc2`, one parser-inserted `<script src>` per status, each body a line of valid
//! JavaScript that records itself, and an `onload`/`onerror` on every element:
//!
//! ```text
//!                         korvus (before)                    Chrome 150.0.7871.46
//! 404 / 500 / 400 body    RAN, no event                      not run, `error`
//! 302 -> 404              RAN, no event                      not run, `error`
//! 200 / 206 / 299         ran, no event                      ran, `load`
//! 204 / empty 200 / 304   no event                           `load`
//! image/png, text/csv     RAN                                not run, `error`  (classic MIME block)
//! module text/plain       RAN                                not run, `error`  (strict MIME)
//! inserted <script> 300   `error`                            ran, `load`
//! ```
//!
//! A 404 carrying an HTML error page was therefore handed to V8, which threw a SyntaxError the page
//! never wrote -- the uncaught error the load axis counted on every page whose script 404s.
//!
//! # The oracle
//!
//! Every expected sequence below is what stock Chrome 150.0.7871.46 (headless, attached over CDP,
//! launched by the parity driver's own `attach`) recorded for the same markup on 2026-09-17. The
//! rule itself, and the full table it came from, is `webapi`'s `script_response_ok`.

use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const HTML_404: &str = "<!doctype html><html><head><title>404 Not Found</title></head><body><h1>Not Found</h1></body></html>";

/// `(status, Content-Type or none, extra headers, body)` for one path.
fn route(path: &str) -> (u16, Option<&'static str>, Vec<(&'static str, String)>, String) {
    let js = |tag: &str| format!("L.push('ran:{tag}');");
    // `/s/<status>/<tag>`: that status, text/javascript, a body that records the tag.
    if let Some(rest) = path.strip_prefix("/s/") {
        let (status, tag) = rest.split_once('/').unwrap_or((rest, ""));
        let status: u16 = status.parse().unwrap_or(500);
        let extra = if status == 206 { vec![("Content-Range", format!("bytes 0-{}/{}", js(tag).len() - 1, js(tag).len()))] } else { vec![] };
        let body = if status == 204 || status == 304 { String::new() } else { js(tag) };
        return (status, Some("text/javascript"), extra, body);
    }
    // `/html404/<tag>`: a 404 whose body is an HTML error page -- what the live web actually serves.
    if path.starts_with("/html404/") {
        return (404, Some("text/html"), vec![], HTML_404.to_string());
    }
    // `/to/<status>/<tag>`: a 302 to `/s/<status>/<tag>`.
    if let Some(rest) = path.strip_prefix("/to/") {
        return (302, None, vec![("Location", format!("/s/{rest}"))], String::new());
    }
    // `/empty/<tag>`: a 200 with no body at all.
    if path.starts_with("/empty/") {
        return (200, Some("text/javascript"), vec![], String::new());
    }
    // `/ct/<name>/<tag>`: a 200 JavaScript body under a chosen Content-Type (and maybe nosniff).
    if let Some(rest) = path.strip_prefix("/ct/") {
        let (name, tag) = rest.split_once('/').unwrap_or((rest, ""));
        let (ct, nosniff): (Option<&'static str>, bool) = match name {
            "plain" => (Some("text/plain"), false),
            "html" => (Some("text/html"), false),
            "json" => (Some("application/json"), false),
            "png" => (Some("image/png"), false),
            "csv" => (Some("text/csv; charset=utf-8"), false),
            "audio" => (Some("audio/mpeg"), false),
            "none" => (None, false),
            "plain-nosniff" => (Some("text/plain"), true),
            "none-nosniff" => (None, true),
            "js-nosniff" => (Some("application/x-javascript"), true),
            _ => (Some("text/javascript"), false),
        };
        let extra = if nosniff { vec![("X-Content-Type-Options", "nosniff".to_string())] } else { vec![] };
        return (200, ct, extra, js(tag));
    }
    // `/syntax`: a 200 JavaScript response that is not valid JavaScript.
    if path == "/syntax" {
        return (200, Some("text/javascript"), vec![], "this is not js".into());
    }
    // `/cs/<tag>`: records the id of `document.currentScript` while it runs.
    if path.starts_with("/cs/") {
        return (200, Some("text/javascript"), vec![], "L.push('cs:' + (document.currentScript && document.currentScript.id));".into());
    }
    // `/throws`: runs, then throws.
    if path == "/throws" {
        return (200, Some("text/javascript"), vec![], "L.push('ran:throws'); throw new TypeError('boom');".into());
    }
    // `/import404`: a module whose own response is fine and whose one import is a 404.
    if path == "/import404" {
        return (200, Some("text/javascript"), vec![], "import '/s/404/imported'; L.push('ran:importer');".into());
    }
    (404, Some("text/plain"), vec![], "no".into())
}

async fn serve(listener: TcpListener, page: String) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, ctype, extra, body) =
                if path == "/page.html" { (200, Some("text/html"), vec![], page) } else { route(&path) };
            let mut head = format!("HTTP/1.1 {status} X\r\nCache-Control: no-store\r\nConnection: close\r\n");
            if status != 204 && status != 304 {
                head.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            if let Some(ct) = ctype {
                head.push_str(&format!("Content-Type: {ct}\r\n"));
            }
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

/// The recorder every page starts with: `L` collects the page's own markers, and a CAPTURING `error`
/// listener on the window sees both an element's `error` (target SCRIPT) and an uncaught exception
/// (target window) -- which is how the two are told apart. `console.error` is counted, because
/// `console_errors` is a scored field and Chrome's "Failed to load resource" line is NOT a console call.
const HEAD: &str = r#"<script>var L=[];var CE=0;var ce=console.error;console.error=function(){CE++;return ce.apply(console,arguments)};
addEventListener('error',function(e){L.push('winerr:'+(e.target===window?'win':e.target.tagName))},true);
document.addEventListener('DOMContentLoaded',function(){L.push('DCL')});addEventListener('load',function(){L.push('load')});</script>"#;

/// `<script src=URL>` with an `onload`/`onerror` that record `tag`.
fn tag_script(attrs: &str, url: &str, tag: &str) -> String {
    format!(r#"<script{attrs} src="{url}" onload="L.push('onload:{tag}')" onerror="L.push('onerror:{tag}')"></script>"#)
}

/// Load `body` (after [`HEAD`]) from a loopback origin, let it settle, and return the page's `L`, the
/// engine's uncaught errors, and the page's `console.error` count.
async fn run_page(body: &str) -> (Vec<String>, Vec<String>, i64) {
    let doc = format!("<!doctype html><html><head>{HEAD}</head><body>{body}</body></html>");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, doc.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(400);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("p9a", "print(JSON.stringify(L)); print(String(CE))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let log: Vec<String> = serde_json::from_str(&out[0]).expect("L is JSON");
    let ce: i64 = out[1].parse().unwrap();
    (log, e.uncaught_errors(), ce)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------------------------
// 1. THE GATE: status. A 4xx/5xx body is not run and fires `error`; everything below 400 is OK.
// ---------------------------------------------------------------------------------------------

/// One parser-inserted script per row, each followed by an inline marker, so the sequence also pins
/// WHEN the event fires: before the next script in document order. Chrome 150's sequence, verbatim.
#[tokio::test]
async fn a_parser_inserted_script_runs_only_on_an_ok_status() {
    let rows: &[(&str, &str)] = &[
        ("200", "/s/200/200"),
        ("204", "/s/204/204"),
        ("206", "/s/206/206"),
        ("299", "/s/299/299"),
        ("300", "/s/300/300"),
        ("304", "/s/304/304"),
        ("399", "/s/399/399"),
        ("400", "/s/400/400"),
        ("404", "/s/404/404"),
        ("500", "/s/500/500"),
        ("r404", "/to/404/r404"),
        ("r200", "/to/200/r200"),
        ("empty", "/empty/empty"),
        ("html404", "/html404/html404"),
    ];
    let mut body = String::new();
    for (tag, url) in rows {
        body.push_str(&tag_script("", url, tag));
        body.push_str(&format!("<script>L.push('after:{tag}')</script>"));
    }
    let (log, uncaught, ce) = run_page(&body).await;
    assert_eq!(
        log,
        strs(&[
            "ran:200", "onload:200", "after:200",
            "onload:204", "after:204",
            "ran:206", "onload:206", "after:206",
            "ran:299", "onload:299", "after:299",
            "ran:300", "onload:300", "after:300",
            "onload:304", "after:304",
            "ran:399", "onload:399", "after:399",
            "winerr:SCRIPT", "onerror:400", "after:400",
            "winerr:SCRIPT", "onerror:404", "after:404",
            "winerr:SCRIPT", "onerror:500", "after:500",
            "winerr:SCRIPT", "onerror:r404", "after:r404",
            "ran:r200", "onload:r200", "after:r200",
            "onload:empty", "after:empty",
            "winerr:SCRIPT", "onerror:html404", "after:html404",
            "DCL", "load",
        ])
    );
    // THE REASON THIS UNIT EXISTS: the HTML error page is not parsed as JavaScript, so there is no
    // `Unexpected token '<'` -- and a refused load is not an uncaught error or a console.error either.
    assert!(uncaught.is_empty(), "a refused script is not an uncaught error: {uncaught:?}");
    assert_eq!(ce, 0, "a refused script calls no console.error");
}

/// A script that IS run and throws still throws -- the fix must not swallow real errors -- and its
/// element still fires `load`: Chrome reports the uncaught SyntaxError, then fires `load`.
#[tokio::test]
async fn a_script_that_runs_and_throws_still_throws_and_loads() {
    let body = format!("{}{}", tag_script("", "/syntax", "syntax"), tag_script("", "/ct/html/html", "html"));
    let (log, uncaught, _) = run_page(&body).await;
    assert_eq!(log, strs(&["winerr:win", "onload:syntax", "ran:html", "onload:html", "DCL", "load"]));
    assert_eq!(uncaught.len(), 1, "a script that runs and throws is still an uncaught error: {uncaught:?}");
}

// ---------------------------------------------------------------------------------------------
// 2. Content-Type: the classic MIME block and nosniff; a module's strict MIME check.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_classic_script_is_refused_only_for_the_blocked_mime_types() {
    let names = ["plain", "html", "json", "png", "csv", "audio", "none", "plain-nosniff", "none-nosniff", "js-nosniff"];
    let body: String = names.iter().map(|n| tag_script("", &format!("/ct/{n}/{n}"), n)).collect();
    let (log, uncaught, _) = run_page(&body).await;
    assert_eq!(
        log,
        strs(&[
            "ran:plain", "onload:plain",
            "ran:html", "onload:html",
            "ran:json", "onload:json",
            "winerr:SCRIPT", "onerror:png",
            "winerr:SCRIPT", "onerror:csv",
            "winerr:SCRIPT", "onerror:audio",
            "ran:none", "onload:none",
            "winerr:SCRIPT", "onerror:plain-nosniff",
            "winerr:SCRIPT", "onerror:none-nosniff",
            "ran:js-nosniff", "onload:js-nosniff",
            "DCL", "load",
        ])
    );
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

/// External module scripts fire their events after the parse, in document order, before
/// DOMContentLoaded. A module needs a JavaScript MIME type; an import that 404s fails the importer
/// with `error`, not with an uncaught error. Chrome 150's sequence.
#[tokio::test]
async fn a_module_script_needs_an_ok_status_and_a_javascript_mime_type() {
    let rows: &[(&str, &str)] = &[
        ("plain", "/ct/plain/plain"),
        ("none", "/ct/none/none"),
        ("js", "/ct/js/js"),
        ("404", "/s/404/404"),
        ("html404", "/html404/html404"),
        ("204", "/s/204/204"),
        ("importer", "/import404"),
    ];
    let mut body: String = rows.iter().map(|(t, u)| tag_script(" type=module", u, t)).collect();
    body.push_str("<script>L.push('last-inline')</script>");
    let (log, uncaught, ce) = run_page(&body).await;
    assert_eq!(
        log,
        strs(&[
            "last-inline",
            "winerr:SCRIPT", "onerror:plain",
            "winerr:SCRIPT", "onerror:none",
            "ran:js", "onload:js",
            "winerr:SCRIPT", "onerror:404",
            "winerr:SCRIPT", "onerror:html404",
            "onload:204",
            "winerr:SCRIPT", "onerror:importer",
            "DCL", "load",
        ])
    );
    assert!(uncaught.is_empty(), "a module that failed to FETCH is not an uncaught error: {uncaught:?}");
    assert_eq!(ce, 0);
}

// ---------------------------------------------------------------------------------------------
// 3. async / defer / inserted from script: the same rule and the same event.
// ---------------------------------------------------------------------------------------------

/// Membership, not order, for async and inserted scripts: their timing is a network race in Chrome
/// too. What is pinned is that each refused one fires `error` and never runs, each OK one runs and
/// fires `load`, and none of it blocks DOMContentLoaded or `load`.
#[tokio::test]
async fn async_defer_and_inserted_scripts_follow_the_same_rule() {
    let mut body = String::new();
    body.push_str(&tag_script(" async", "/s/404/a404", "a404"));
    body.push_str(&tag_script(" async", "/s/200/a200", "a200"));
    body.push_str(&tag_script(" defer", "/s/404/d404", "d404"));
    body.push_str(&tag_script(" defer", "/s/200/d200", "d200"));
    body.push_str(
        r#"<script>
        [['i404','/s/404/i404'],['i300','/s/300/i300'],['ihtml','/html404/ihtml'],['ipng','/ct/png/ipng']].forEach(function(r){
          var s=document.createElement('script'); s.src=r[1];
          s.onload=function(){L.push('onload:'+r[0])}; s.onerror=function(){L.push('onerror:'+r[0])};
          document.head.appendChild(s);
        });
        </script>"#,
    );
    let (log, uncaught, ce) = run_page(&body).await;
    let has = |s: &str| log.iter().any(|x| x == s);
    for want in ["onerror:a404", "ran:a200", "onload:a200", "onerror:d404", "ran:d200", "onload:d200", "onerror:i404", "ran:i300", "onload:i300", "onerror:ihtml", "onerror:ipng", "DCL", "load"] {
        assert!(has(want), "missing {want} in {log:?}");
    }
    for never in ["ran:a404", "ran:d404", "ran:i404", "ran:ipng", "onload:a404", "onerror:i300"] {
        assert!(!has(never), "unexpected {never} in {log:?}");
    }
    // A defer script's event precedes DOMContentLoaded, as in Chrome.
    let pos = |s: &str| log.iter().position(|x| x == s).unwrap();
    assert!(pos("onerror:d404") < pos("DCL") && pos("onload:d200") < pos("DCL"), "{log:?}");
    assert!(uncaught.is_empty(), "{uncaught:?}");
    assert_eq!(ce, 0);
}

// ---------------------------------------------------------------------------------------------
// 4. An INSERTED script that runs: it is `document.currentScript`, and its throw is a real one.
// ---------------------------------------------------------------------------------------------

/// Chrome 150.0.7871.46, a `<script src>` created and appended by script:
///
/// ```text
/// inside the body     document.currentScript.id === "dyn1"   (korvus before: null)
/// in its onload       document.currentScript === null
/// body throws         an uncaught TypeError at the window, then `load`
///                     (korvus before: a bare Event "error" at the window, no message, and nothing
///                     in the uncaught tally)
/// ```
///
/// The first row is what a bundler reads to find its own chunk's URL; null there is an exception the
/// page never saw in Chrome. The sequence is compared with DOMContentLoaded filtered out, because
/// when an inserted script runs relative to it is a network race in Chrome too.
#[tokio::test]
async fn an_inserted_script_is_the_current_script_and_its_throw_is_reported() {
    let body = r#"<script>
      var s = document.createElement('script'); s.id = 'dyn1'; s.src = '/cs/dyn1';
      s.onload = function () { L.push('onload-cs:' + document.currentScript); };
      document.head.appendChild(s);
    </script>"#;
    let (log, uncaught, _) = run_page(body).await;
    let log: Vec<String> = log.into_iter().filter(|x| x != "DCL").collect();
    assert_eq!(log, strs(&["cs:dyn1", "onload-cs:null", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");

    let body = r#"<script>
      var s = document.createElement('script'); s.src = '/throws';
      s.onload = function () { L.push('onload:throws'); };
      document.head.appendChild(s);
    </script>"#;
    let (log, uncaught, _) = run_page(body).await;
    let log: Vec<String> = log.into_iter().filter(|x| x != "DCL").collect();
    assert_eq!(log, strs(&["ran:throws", "winerr:win", "onload:throws", "load"]));
    assert_eq!(uncaught.len(), 1, "{uncaught:?}");
    assert!(uncaught[0].contains("TypeError: boom"), "{uncaught:?}");
}

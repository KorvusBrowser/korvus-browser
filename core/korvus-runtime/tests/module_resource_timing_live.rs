//! A SCRIPT'S RESOURCE ENTRY AS CHROME 150 MAKES IT, two findings measured on stock Chrome
//! 150.0.7871.46 (headed under Xvfb) against the pages below:
//!
//! * Every fetch of a MODULE GRAPH is on the resource timeline -- a module script's static import, an inline
//!   module's, and each `import()`'s module -- as `initiatorType` "script", "non-blocking". korvus put none of them
//!   there: the module loader fetched off the V8 scope and nothing recorded it (the loader now queues the
//!   transport's measurement and the realm drains it when its timeline is read, and on each load turn).
//! * `renderBlockingStatus` is HTML's "render-blocking": a classic script with neither `async` nor `defer`, or one
//!   marked `blocking="render"`, met by the parser while the document had no body (in `<head>`). Body, async,
//!   deferred and module scripts are "non-blocking"; korvus said "blocking" for every parser-inserted script.
//! * A `PerformanceObserver` sees those entries as they are made, the page reading no timeline at all: the module
//!   fetches are drained into the timeline on every turn of the event loop, not only when a page reads it.
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const CHROME_150_MODULES: &str = r#"[["/dep.js","script","http/1.0",true,19,19,200,"non-blocking",""],["/dyn.js","script","http/1.0",true,19,19,200,"non-blocking",""],["/dyn2.js?late","script","http/1.0",true,19,19,200,"non-blocking",""],["/inline-dep.js","script","http/1.0",true,19,19,200,"non-blocking",""],["/m.js","script","http/1.0",true,39,39,200,"non-blocking",""]]"#;

const CHROME_150_OBSERVED: &str = r#"[["/dep.js","script","non-blocking"],["/dyn.js","script","non-blocking"],["/m.js","script","non-blocking"]]"#;

const CHROME_150_BLOCKING: &str = r#"[["/body-classic.js","script","non-blocking"],["/body-defer.js","script","non-blocking"],["/head-async.js","script","non-blocking"],["/head-classic.js","script","blocking"],["/head-defer.js","script","non-blocking"],["/head-module-async.js","script","non-blocking"],["/head-module-blocking.js","script","blocking"],["/head-module.js","script","non-blocking"]]"#;

const MODULES: &str = r#"<!doctype html><title>mod</title>
<script type="module" src="/m.js"></script>
<script type="module">import '/inline-dep.js'; window.inlineDone = true;</script>
<script>
addEventListener('load', function () {
  setTimeout(function () {
    import('/dyn.js').then(function () {
      import('/dyn2.js?late').then(function () {
        setTimeout(function () {
          var r = performance.getEntriesByType('resource').filter(function (e) { return e.name.indexOf('.js') > 0; }).map(function (e) {
            return [e.name.replace(location.origin, ''), e.initiatorType, e.nextHopProtocol, Math.round(e.transferSize) > 0, e.encodedBodySize, e.decodedBodySize, e.responseStatus, e.renderBlockingStatus, e.deliveryType];
          });
          r.sort(function (a, b) { return a[0] < b[0] ? -1 : 1; });
          window.__report = JSON.stringify(r);
        }, 300);
      });
    });
  }, 100);
});
</script>"#;

const OBSERVED: &str = r#"<!doctype html><title>obs</title>
<script>
var seen = [];
new PerformanceObserver(function (list) {
  list.getEntries().forEach(function (e) { if (e.name.indexOf('.js') > 0) seen.push([e.name.replace(location.origin, ''), e.initiatorType, e.renderBlockingStatus]); });
}).observe({type: 'resource'});
addEventListener('load', function () {
  setTimeout(function () {
    import('/dyn.js').then(function () {
      setTimeout(function () {
        seen.sort(function (a, b) { return a[0] < b[0] ? -1 : 1; });
        window.__report = JSON.stringify(seen);
      }, 300);
    });
  }, 100);
});
</script>
<script type="module" src="/m.js"></script>"#;

const BLOCKING: &str = r#"<!doctype html><html><head><title>rb</title>
<script src="/head-classic.js"></script>
<script defer src="/head-defer.js"></script>
<script async src="/head-async.js"></script>
<script type="module" src="/head-module.js"></script>
<script type="module" async src="/head-module-async.js"></script>
<script blocking="render" type="module" src="/head-module-blocking.js"></script>
</head><body>
<script src="/body-classic.js"></script>
<script defer src="/body-defer.js"></script>
<script>
addEventListener('load', function () { setTimeout(function () {
  var r = performance.getEntriesByType('resource').filter(function (e) { return e.name.indexOf('.js') > 0; }).map(function (e) { return [e.name.replace(location.origin, ''), e.initiatorType, e.renderBlockingStatus]; });
  r.sort(function (a, b) { return a[0] < b[0] ? -1 : 1; });
  window.__report = JSON.stringify(r);
}, 300); });
</script></body></html>"#;

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
        let bare = path.split('?').next().unwrap_or("/");
        let (body, ty) = match bare {
            "/modules" => (MODULES, "text/html"),
            "/blocking" => (BLOCKING, "text/html"),
            "/observed" => (OBSERVED, "text/html"),
            "/m.js" => ("import './dep.js'; window.mDone = true;", "text/javascript"),
            "/dep.js" => ("export const d = 1;", "text/javascript"),
            "/inline-dep.js" => ("export const i = 1;", "text/javascript"),
            "/dyn.js" => ("export const y = 1;", "text/javascript"),
            "/dyn2.js" => ("export const z = 2;", "text/javascript"),
            p if p.ends_with(".js") => ("window.x = 1;", "text/javascript"),
            _ => ("", "text/plain"),
        };
        // HTTP/1.0, as the recording's server answered (`nextHopProtocol` "http/1.0")
        let out = format!("HTTP/1.0 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
        return;
    }
}

async fn report(path: &str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
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
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return serde_json::from_str(s).expect("the report");
        }
    }
}

#[tokio::test]
async fn every_module_fetch_has_its_resource_entry_as_in_chrome_150() {
    let got = report("/modules").await;
    let want: serde_json::Value = serde_json::from_str(CHROME_150_MODULES).unwrap();
    assert_eq!(got, want, "the module graph's resource entries, korvus vs Chrome 150");
}

#[tokio::test]
async fn a_scripts_render_blocking_status_is_htmls_as_in_chrome_150() {
    let got = report("/blocking").await;
    let want: serde_json::Value = serde_json::from_str(CHROME_150_BLOCKING).unwrap();
    assert_eq!(got, want, "renderBlockingStatus, korvus vs Chrome 150");
}

#[tokio::test]
async fn an_observer_sees_every_module_fetch_as_in_chrome_150() {
    let got = report("/observed").await;
    let want: serde_json::Value = serde_json::from_str(CHROME_150_OBSERVED).unwrap();
    assert_eq!(got, want, "the module fetches a PerformanceObserver saw, korvus vs Chrome 150");
}

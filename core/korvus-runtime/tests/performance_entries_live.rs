//! The performance timeline's own entries and `supportedEntryTypes`, against Chrome 152 (and Chrome 150, Brave 151)
//! (korvus-analysis U-3 / U-4: a challenge provider an interactive-widget challenge challenge frame reads `getEntries()` and every
//! entry's `entryType`, and the list of supported types).
//!
//! Measured 2026-10-07, `google-chrome --headless=new` 150.0.7871.46 on a loopback page:
//!
//! ```text
//! getEntries() before load                [navigation, visibility-state "visible"]   (page and frame)
//! Object.getOwnPropertyNames(navEntry)    []        (fields are prototype getters)
//! navEntry.hasOwnProperty("toJSON")        false     (PerformanceNavigationTiming.prototype.toJSON)
//! Object.keys(navEntry.toJSON())          47 keys, resource order then the navigation half (Chrome 152: 48,
//!                                         `navigationId` after `duration`)
//! visibility entry toJSON                 {name:"visible", entryType:"visibility-state", startTime:0, duration:0}
//!                                         (Chrome 152: and `navigationId`, the document's random id)
//! supportedEntryTypes, window             13 types (no interaction-contentful-paint, soft-navigation);
//!                                         Chrome 152.0.7977.82 (2026-10-16): 15, with both
//! supportedEntryTypes, worker             [mark, measure, resource]
//! ```
//!
//! Brave 151's and Chrome 152's captures list 15 window types: the list follows the entry interfaces the persona's
//! surface exposes, never a list per persona.

use engine::Engine;
use std::time::Duration;

const CHROME: &str = "chrome-152-linux-20261016T1022";
const URL: &str = "http://127.0.0.1:8123/page.html";
const DOC: &str = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";

async fn engine(bundle: &str) -> Engine {
    let p = persona::CapturedPersona::select(Some(bundle)).unwrap_or_else(|e| panic!("select {bundle}: {e}"));
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("boot");
    e.load_page(DOC, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e
}

async fn value(bundle: &str, js: &str) -> serde_json::Value {
    let mut e = engine(bundle).await;
    e.eval_value("perf", js.to_string()).expect("eval")
}

/// What a worker of `bundle` answers for `PerformanceObserver.supportedEntryTypes`.
async fn worker_types(bundle: &str) -> String {
    let mut e = engine(bundle).await;
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string("postMessage(PerformanceObserver.supportedEntryTypes.join(','))").unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.eval("driver", "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into()).unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    while start.elapsed() < Duration::from_secs(20) {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("RESULT:")) {
            return l["RESULT:".len()..].to_string();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    format!("NONE (output: {out:?})")
}

#[tokio::test]
async fn supported_entry_types_follow_the_realms_entry_interfaces() {
    let types = "PerformanceObserver.supportedEntryTypes.join(',')";
    assert_eq!(
        value(CHROME, types).await,
        "element,event,first-input,interaction-contentful-paint,largest-contentful-paint,layout-shift,long-animation-frame,longtask,mark,measure,navigation,paint,resource,soft-navigation,visibility-state"
    );
    assert_eq!(worker_types(CHROME).await, "mark,measure,resource");
}

#[tokio::test]
async fn the_navigation_and_visibility_entries_are_chromes() {
    let got = value(
        CHROME,
        r#"(() => {
            const all = performance.getEntries();
            const n = performance.getEntriesByType('navigation')[0];
            const v = performance.getEntriesByType('visibility-state')[0];
            return [
                all.slice(0, 2).map(e => e.entryType + ':' + (e.entryType == 'visibility-state' ? e.name : '')).join(','),
                Object.getOwnPropertyNames(n).length,
                n.hasOwnProperty('toJSON'),
                Object.keys(n.toJSON()).join(','),
                n.toJSON().type,
                Object.getOwnPropertyNames(v).length,
                Object.prototype.toString.call(v),
                // the navigation id is the document's random one (100..10000): its type, not its value
                JSON.stringify(v, (k, x) => k === 'navigationId' ? typeof x : x),
            ];
        })()"#,
    )
    .await;
    let want = serde_json::json!([
        "navigation:,visibility-state:visible",
        0,
        false,
        "name,entryType,startTime,duration,navigationId,initiatorType,deliveryType,nextHopProtocol,renderBlockingStatus,contentType,contentEncoding,workerStart,workerRouterEvaluationStart,workerCacheLookupStart,workerMatchedSourceType,workerFinalSourceType,redirectStart,redirectEnd,fetchStart,domainLookupStart,domainLookupEnd,connectStart,secureConnectionStart,connectEnd,requestStart,responseStart,firstInterimResponseStart,finalResponseHeadersStart,responseEnd,transferSize,encodedBodySize,decodedBodySize,responseStatus,serverTiming,unloadEventStart,unloadEventEnd,domInteractive,domContentLoadedEventStart,domContentLoadedEventEnd,domComplete,loadEventStart,loadEventEnd,type,redirectCount,activationStart,criticalCHRestart,notRestoredReasons,confidence",
        "navigate",
        0,
        "[object VisibilityStateEntry]",
        "{\"name\":\"visible\",\"entryType\":\"visibility-state\",\"startTime\":0,\"duration\":0,\"navigationId\":\"number\"}",
    ]);
    assert_eq!(got, want);
}

/// A loopback server: `/frame` is a document gzip-encoded on the wire, everything else 404.
async fn serve_gzip_frame(listener: tokio::net::TcpListener, frame: Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let frame = frame.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let resp = if path.starts_with("/frame") {
                let mut r = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    frame.len()
                )
                .into_bytes();
                r.extend_from_slice(&frame);
                r
            } else {
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec()
            };
            let _ = sock.write_all(&resp).await;
        });
    }
}

/// A FRAME's navigation entry is ITS document's response, and a content-coded body reports its two
/// sizes apart (korvus-analysis U-5). Chrome 150, measured on the same shape (a page and a frame whose
/// 12577-byte document is 443 gzip bytes on the wire): the frame's entry reads transferSize 743,
/// encodedBodySize 443, decodedBodySize 12577 -- korvus reported the PAGE's response in every frame,
/// and the decoded size as the encoded one (the client stripped the coding before anything counted).
/// `fetch()` of the same URL shows the coding headers and the same three sizes in Chrome.
#[tokio::test]
async fn a_frames_navigation_entry_is_its_own_coded_response() {
    use std::io::Write;
    let doc = format!("<!doctype html><body>{}", "frame ".repeat(2000));
    let gz = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(doc.as_bytes()).unwrap();
        e.finish().unwrap()
    };
    let (encoded, decoded) = (gz.len(), doc.len());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_gzip_frame(listener, gz));

    let p = persona::CapturedPersona::select(Some(CHROME)).expect("chrome bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("boot");
    let top = "<!doctype html><html><body><iframe src=\"/frame\"></iframe></body></html>";
    let headers = [("content-type".to_string(), "text/html".to_string())];
    e.load_page_delivered(top, &format!("http://{addr}/"), &[], &headers).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval(
        "fetch",
        "fetch('/frame').then(r => r.text().then(t => { globalThis.__f = [r.headers.get('content-encoding'), r.headers.get('content-length'), t.length]; }))".into(),
    )
    .unwrap();
    for _ in 0..50 {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        if e.eval_value("done", "!!globalThis.__f".into()).ok() == Some(serde_json::json!(true)) {
            break;
        }
    }
    let got = e
        .eval_value(
            "frame-entry",
            r#"(() => {
                const n = document.querySelector('iframe').contentWindow.performance.getEntriesByType('navigation')[0];
                const r = performance.getEntriesByName(location.origin + '/frame').pop();
                return [new URL(n.name).pathname, n.transferSize, n.encodedBodySize, n.decodedBodySize,
                        globalThis.__f, r && [r.transferSize, r.encodedBodySize, r.decodedBodySize]];
            })()"#
                .into(),
        )
        .expect("eval");
    let (enc, dec) = (encoded as u64, decoded as u64);
    assert_eq!(
        got,
        serde_json::json!(["/frame", enc + 300, enc, dec, ["gzip", enc.to_string(), dec], [enc + 300, enc, dec]])
    );
}

/// `navigationEntry.confidence`, Chromium's randomized-response report (Chrome 150, measured, and its
/// source: `randomized_confidence_utils.cc`, `RenderFrameHostImpl::NotifyDocumentInteractive`): the
/// page's document has one once interactive -- `randomizedTriggerRate` 0.4994798 (2 / (1 + e^1.1)),
/// `value` "high" or "low" -- a NEW object every read, `toJSON` {randomizedTriggerRate, value}; a
/// frame's document has none (`null`, only the primary main frame's is finalized). The two
/// service-worker source types read "" with no service worker in the way (korvus answered null).
#[tokio::test]
async fn the_navigation_confidence_is_chromes_randomized_report() {
    let got = value(
        CHROME,
        r#"(() => {
            const n = performance.getEntriesByType('navigation')[0];
            const c = n.confidence;
            const f = document.createElement('iframe');
            document.body.append(f);
            const fn = f.contentWindow.performance.getEntriesByType('navigation')[0];
            return [
                Object.prototype.toString.call(c), c.randomizedTriggerRate, ['high', 'low'].includes(c.value),
                n.confidence === c, Object.getOwnPropertyNames(c).length, Object.keys(c.toJSON()).join(','),
                Object.keys(n.toJSON().confidence.toJSON()).join(','),
                fn ? fn.confidence : 'no frame entry',
                n.workerMatchedSourceType, n.workerFinalSourceType,
            ];
        })()"#,
    )
    .await;
    assert_eq!(
        got,
        serde_json::json!(["[object PerformanceTimingConfidence]", 0.4994798, true, false, 0, "randomizedTriggerRate,value", "randomizedTriggerRate,value", null, "", ""])
    );
}

/// A loopback server answering every path with a small body of a type its extension names, and
/// logging each path it was asked for.
async fn serve_logged(listener: tokio::net::TcpListener, log: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(path.clone());
            let (ty, body): (&str, &[u8]) = if path.ends_with(".css") {
                ("text/css", b"p{color:red}")
            } else if path.ends_with(".js") {
                ("text/javascript", b"0")
            } else if path.ends_with(".png") {
                // slow, as an image a page's body names usually is
                tokio::time::sleep(Duration::from_millis(30)).await;
                ("image/png", b"\\x89PNG")
            } else {
                ("application/octet-stream", b"x")
            };
            let mut r = format!("HTTP/1.1 200 OK\r\ncontent-type: {ty}\r\ncontent-length: {}\r\naccess-control-allow-origin: *\r\nconnection: close\r\n\r\n", body.len()).into_bytes();
            r.extend_from_slice(body);
            let _ = sock.write_all(&r).await;
        });
    }
}

/// THE PRELOAD SCANNER (korvus-analysis, an interactive-widget challenge flow's page timeline). Chrome 150, measured on
/// this shape (a font preload, a stylesheet and two scripts in `<head>`, an image in `<body>`): the
/// resource timeline after load lists font, stylesheet, script, async script, image -- the order the
/// requests LEFT in, which is document order -- and each is requested once. korvus asked for the
/// font and the image only after both scripts and the stylesheet had ARRIVED, and listed the
/// stylesheet after the scripts.
#[tokio::test]
async fn parser_subresources_leave_in_document_order_once_each() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_logged(listener, log.clone()));
    let p = persona::CapturedPersona::select(Some(CHROME)).expect("chrome bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("boot");
    let doc = "<!doctype html><html><head><link rel=preload href=f.ttf as=font crossorigin><link rel=stylesheet href=a.css>\
               <script src=b.js></script><script async src=c.js></script></head><body>text<img src=i.png></body></html>";
    let headers = [("content-type".to_string(), "text/html".to_string())];
    e.load_page_delivered(doc, &format!("http://{addr}/page.html"), &[], &headers).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    for _ in 0..10 {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
    }
    let got = e
        .eval_value("order", "performance.getEntriesByType('resource').map(e => e.name.split('/').pop()).filter(n => n != 'favicon.ico').join(',')".into())
        .expect("eval");
    assert_eq!(got, "f.ttf,a.css,b.js,c.js,i.png");
    let mut asked: Vec<String> = log.lock().unwrap().iter().filter(|p| !p.contains("favicon")).cloned().collect();
    asked.sort();
    assert_eq!(asked, vec!["/a.css", "/b.js", "/c.js", "/f.ttf", "/i.png"], "each requested once");
}

/// A `<link rel=preload as=style>` of a blocking stylesheet is that stylesheet's request, asked ONCE.
/// Chrome 150, measured (`<link rel=preload href=a.css as=style><link rel=stylesheet href=a.css>`, a
/// defer and a blocking script, a body image): one request each, and the timeline after load lists the
/// preloaded font, the stylesheet, both scripts, the image. With the preload scanner, korvus asked
/// for the preloaded stylesheet twice -- the scanner's request beside the fetch pass's own.
#[tokio::test]
async fn a_preload_of_the_passes_own_stylesheet_is_one_request() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_logged(listener, log.clone()));
    let p = persona::CapturedPersona::select(Some(CHROME)).expect("chrome bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("boot");
    let doc = "<!doctype html><html><head><link rel=preload href=f.ttf as=font crossorigin><link rel=preload href=a.css as=style>\
               <link rel=stylesheet href=a.css><script defer src=d.js></script><script src=b.js></script></head><body>text<img src=i.png></body></html>";
    let headers = [("content-type".to_string(), "text/html".to_string())];
    e.load_page_delivered(doc, &format!("http://{addr}/page.html"), &[], &headers).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    for _ in 0..10 {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
    }
    let mut asked: Vec<String> = log.lock().unwrap().iter().filter(|p| !p.contains("favicon")).cloned().collect();
    asked.sort();
    assert_eq!(asked, vec!["/a.css", "/b.js", "/d.js", "/f.ttf", "/i.png"], "each requested once");
    let all = e
        .eval_value("all", "performance.getEntriesByType('resource').map(e => e.name.split('/').pop()).filter(n => n != 'favicon.ico').join(',')".into())
        .expect("eval");
    let all = all.as_str().unwrap_or_default().to_string();
    assert!(all.starts_with("f.ttf,a.css,") && all.ends_with(",i.png") && all.split(',').count() == 5, "one entry each, Chrome's order: {all}");
}

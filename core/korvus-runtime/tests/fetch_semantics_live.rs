//! What a page can see of a fetch: response tainting, the CORS check and its preflight,
//! redirect modes, `Headers` iteration, and the resource-timing entries its loads leave. Live, on two
//! loopback origins (127.0.0.1 is the page, 127.0.0.2 is cross-origin).
//!
//! Every expectation was READ OFF stock `google-chrome` 150.0.7871.46 against a server answering the
//! same routes (the probe tables are in `plugins/korvus-transport/src/cors.rs` and
//! `core/korvus-webapi/src/core/timing/resource_timing.rs`). The server keeps a request log, because
//! half of what Chrome does here is decide what NOT to send.
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

/// One loopback origin. A route's behaviour is spelled in its query: `acao=<v>` (`echo` echoes the
/// Origin), `acah=<v>`, `tao=<v>`, `to=<location>` (a 302).
async fn serve(listener: TcpListener, page: String, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 16384];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let mut words = req.split_whitespace();
            let method = words.next().unwrap_or("").to_string();
            let target = words.next().unwrap_or("/").to_string();
            let origin = req.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("origin")).map(|(_, v)| v.trim().to_string()));
            let host = s.local_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            if let Ok(mut l) = log.lock() {
                l.push(format!("{method} {host} {target}"));
            }
            let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
            let q = |k: &str| query.split('&').find_map(|kv| kv.split_once('=').filter(|(kk, _)| *kk == k).map(|(_, v)| v.replace("%2F", "/").replace("%3A", ":")));
            let mut headers = vec![("Content-Type".to_string(), if path == "/page.html" { "text/html" } else { "text/plain" }.to_string())];
            if let Some(v) = q("acao") {
                headers.push(("Access-Control-Allow-Origin".into(), if v == "echo" { origin.clone().unwrap_or_default() } else { v }));
            }
            if let Some(v) = q("acah") {
                headers.push(("Access-Control-Allow-Headers".into(), v));
            }
            if let Some(v) = q("tao") {
                headers.push(("Timing-Allow-Origin".into(), v));
            }
            headers.push(("X-B".into(), "2".into()));
            headers.push(("X-A".into(), "1".into()));
            let (status, body) = if let Some(to) = q("to") {
                headers.push(("Location".into(), to));
                ("302 Found", Vec::new())
            } else if path == "/page.html" {
                ("200 OK", page.into_bytes())
            } else {
                ("200 OK", b"hello".to_vec())
            };
            let mut head = format!("HTTP/1.1 {status}\r\n");
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str(&format!("Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len()));
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

/// Boot a page on origin A, run `probe` (an async function body with `A` and `B` in scope), return its
/// JSON result and the request log.
async fn run(probe: &str) -> (serde_json::Value, Vec<String>) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.2:0").await.unwrap();
    let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
    let doc = "<!doctype html><html><head></head><body></body></html>".to_string();
    tokio::spawn(serve(a, doc.clone(), log.clone()));
    tokio::spawn(serve(b, String::new(), log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&doc, &format!("http://{aa}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    log.lock().unwrap().clear();
    e.eval(
        "p10e_probe",
        format!(
            "(async () => {{ const A = 'http://{aa}', B = 'http://{ba}'; {probe} }})().then((v) => print('P10E:' + JSON.stringify(v)), (err) => print('P10E:' + JSON.stringify('PROBE FAILED ' + err)));"
        ),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("P10E:")) {
            let v = serde_json::from_str(&line["P10E:".len()..]).expect("probe JSON");
            let l = log.lock().unwrap().clone();
            return (v, l);
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the probe never reported; output: {out:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

/// `no-cors` to another origin is opaque; CORS without `Access-Control-Allow-Origin` fails; with it the
/// headers are the safelisted ones; `redirect: 'manual'` is an opaque redirect; `same-origin` mode never
/// sends. Chrome 150, row for row.
#[tokio::test]
async fn a_cross_origin_response_is_what_its_mode_allows() {
    let (v, log) = run(
        "const t = async (u, i) => { try { const r = await fetch(u, i); return [r.type, r.status, r.ok, r.url, [...r.headers].map(h => h[0]).filter(n => n !== 'connection').join(','), r.body === null, await r.text()]; } catch (e) { return e.name + ': ' + e.message; } };
         return { nocors: await t(B + '/n', {mode: 'no-cors'}), noacao: await t(B + '/c'), acao: await t(B + '/s?acao=*'),
                  manual: await t(A + '/r?to=/t', {redirect: 'manual'}), sameorigin: await t(B + '/so', {mode: 'same-origin'}),
                  same: await t(A + '/t') };",
    )
    .await;
    assert_eq!(v["nocors"], json(r#"["opaque",0,false,"","",true,""]"#));
    assert_eq!(v["noacao"], json(r#""TypeError: Failed to fetch""#));
    assert_eq!(v["acao"][0], json(r#""cors""#));
    assert_eq!(v["acao"][4], json(r#""cache-control,content-length,content-type""#), "only the CORS-safelisted names");
    assert_eq!(v["manual"].as_array().unwrap()[0..3], json(r#"["opaqueredirect",0,false]"#).as_array().unwrap()[..]);
    assert!(v["manual"][3].as_str().unwrap().ends_with("/r?to=/t"), "an opaque redirect keeps the request url");
    assert_eq!(v["sameorigin"], json(r#""TypeError: Failed to fetch""#));
    assert!(!log.iter().any(|l| l.contains("/so")), "a same-origin-mode request to another origin is never sent: {log:?}");
    assert_eq!(v["same"][0], json(r#""basic""#));
    assert_eq!(v["same"][4], json(r#""cache-control,content-length,content-type,x-a,x-b""#), "sorted, all of them");
}

/// A non-safelisted header sends an OPTIONS first, and a refused preflight never sends the request.
#[tokio::test]
async fn a_preflight_goes_first_and_a_refused_one_stops_the_request() {
    let (v, log) = run(
        "const ok = await fetch(B + '/p1?acao=*&acah=x-q', {headers: {'x-q': '1'}}).then(r => r.type, e => e.message);
         const no = await fetch(B + '/p2?acao=*', {headers: {'x-q': '1'}}).then(r => r.type, e => e.message);
         return [ok, no];",
    )
    .await;
    assert_eq!(v, json(r#"["cors","Failed to fetch"]"#));
    let seen: Vec<&str> = log.iter().map(|l| l.as_str()).collect();
    assert!(seen.iter().position(|l| l.starts_with("OPTIONS") && l.contains("/p1")) < seen.iter().position(|l| l.starts_with("GET") && l.contains("/p1")), "{seen:?}");
    assert!(seen.iter().any(|l| l.starts_with("OPTIONS") && l.contains("/p2")), "{seen:?}");
    assert!(!seen.iter().any(|l| l.starts_with("GET") && l.contains("/p2")), "a refused preflight sends no request: {seen:?}");
}

/// `Headers` iterate sorted and combined, and a fetched response's headers are immutable.
#[tokio::test]
async fn headers_sort_combine_and_a_fetched_list_is_immutable() {
    let (v, _) = run(
        "const h = new Headers([['b', '1'], ['a', '2'], ['A', '3'], ['set-cookie', 'x'], ['set-cookie', 'y']]);
         const r = await fetch(A + '/t'); let m; try { r.headers.set('x', '1'); m = 'ok'; } catch (e) { m = e.message; }
         return [[...h], m];",
    )
    .await;
    assert_eq!(v, json(r#"[[["a","2, 3"],["b","1"],["set-cookie","x"],["set-cookie","y"]],"Failed to execute 'set' on 'Headers': Headers are immutable"]"#));
}

/// A read fetch has a `fetch` entry, an unread or opaque one has none, a CORS response without
/// Timing-Allow-Origin hides its detail, and a full buffer keeps what fits and fires the event.
#[tokio::test]
async fn resource_timing_entries_are_chromes() {
    let (v, _) = run(
        "const tick = () => new Promise(r => setTimeout(r, 50));
         await (await fetch(A + '/read')).text();
         await fetch(A + '/unread');
         await (await fetch(B + '/opaque', {mode: 'no-cors'})).text();
         await (await fetch(B + '/cors?acao=*')).text();
         await tick();
         const pick = (e) => e && [e.initiatorType, e.responseStatus, e.transferSize, e.nextHopProtocol !== '', e.requestStart > 0, e.duration >= 0, Object.getOwnPropertyNames(e).length];
         const by = (s) => performance.getEntriesByType('resource').find(e => e.name.endsWith(s));
         const out = { read: pick(by('/read')), unread: pick(by('/unread')), opaque: pick(by('/opaque')), cors: pick(by('/cors?acao=*')),
                       proto: Object.prototype.toString.call(by('/read')) };
         performance.clearResourceTimings(); performance.setResourceTimingBufferSize(1);
         let full = 0; performance.onresourcetimingbufferfull = () => { full++; };
         for (const i of [0, 1, 2]) { await (await fetch(A + '/b' + i)).text(); await tick(); }
         out.buffer = [performance.getEntriesByType('resource').map(e => e.name.split('/').pop()), full];
         return out;",
    )
    .await;
    assert_eq!(v["read"], json(r#"["fetch",200,305,true,true,true,0]"#));
    assert_eq!(v["unread"], serde_json::Value::Null, "an unread body leaves no entry yet");
    assert_eq!(v["opaque"], serde_json::Value::Null, "an opaque fetch leaves no entry");
    assert_eq!(v["cors"], json(r#"["fetch",200,0,false,false,true,0]"#), "status readable, detail hidden without TAO");
    assert_eq!(v["proto"], json(r#""[object PerformanceResourceTiming]""#));
    assert_eq!(v["buffer"], json(r#"[["b0"],2]"#));
}

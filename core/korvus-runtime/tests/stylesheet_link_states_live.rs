//! A `<link rel=stylesheet>`'s sheet and its loads in the states a page puts it in -- never fetched,
//! `disabled`, enabled again -- in a live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expected value is Google Chrome 152.0.7977.82's (`--headless=new`, this machine, 2026-10-23), read
//! off the same markup served by a loopback server answering `Cache-Control: no-store` and logging every
//! request.

use engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every path this server was asked for, in arrival order, duplicates kept.
type Log = Arc<Mutex<Vec<String>>>;

/// `(status, Content-Type, extra header line, body)`.
fn route(path: &str) -> (u16, &'static str, &'static str, &'static str) {
    match path {
        "/m.css" => (200, "text/css", "", "a { color: red }\n#x { width: 42px }"),
        "/s.css" => (200, "text/css", "", "a { color: blue }\nb { color: blue }\n#y { width: 43px }"),
        "/plain.txt" => (200, "text/plain", "", "#x { width: 44px }"),
        "/nosniff.txt" => (200, "text/plain", "X-Content-Type-Options: nosniff\r\n", "#x { width: 45px }"),
        _ => (404, "text/css", "", ""),
    }
}

async fn serve(listener: TcpListener, page: String, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if let Ok(mut l) = log.lock() {
                l.push(path.clone());
            }
            let (status, ctype, extra, body) = if path == "/page.html" { (200, "text/html", "", page.as_str()) } else { route(&path) };
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\n{extra}Cache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

/// Run the engine's event loop for `ms`.
async fn settle(e: &mut Engine, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A booted engine on `doc`, served from a loopback origin, loaded and settled; and its request log.
async fn page_of(doc: &str) -> (Engine, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, doc.to_string(), log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    settle(&mut e, 300).await;
    (e, log)
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("link-states-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

fn count(log: &Log, path: &str) -> usize {
    let c: HashMap<String, usize> = log.lock().unwrap().iter().fold(HashMap::new(), |mut m, p| {
        *m.entry(p.clone()).or_default() += 1;
        m
    });
    c.get(path).copied().unwrap_or(0)
}

/// A link that never fetched has no sheet; a `disabled` one is fetched only when the parser met it, is heard from
/// by nobody, and loads -- with its rules -- when it is enabled. Chrome 152:
///
/// ```text
///   rel=stylesheet with no href / href="" / href=" " / href="http://["   sheet null, not listed, no events,
///                                                                       no request, no resource entry
///   <link rel=stylesheet href=/m.css disabled> in the markup            requested once, no load/error, sheet null
///   script-inserted <link rel=stylesheet href=/s.css disabled>          not requested, no events, sheet null
///   then  M.disabled = false; D.removeAttribute("disabled")             each requested (again), each `load`,
///                                                                       2 and 3 rules, both listed, both apply
/// ```
///
/// Before: every never-fetched link was listed with an empty sheet and `http://[` fired `error`; both disabled links
/// fired `load` at once (the script's one requested), and enabled they showed 0 rules that never filled.
#[tokio::test]
async fn a_never_fetched_link_has_no_sheet_and_a_disabled_one_loads_when_enabled() {
    let (mut e, log) = page_of(
        r#"<!doctype html><html><head><script>var EV = [];</script>
<link id="M" rel="stylesheet" href="/m.css" disabled onload="EV.push('M:load')" onerror="EV.push('M:error')">
</head><body><div id="x"></div><div id="y"></div><script>
for (const [id, href] of [["N", null], ["E", ""], ["W", " "], ["U", "http://["], ["D", "/s.css"]]) {
  const l = document.createElement("link");
  l.id = id;
  l.rel = "stylesheet";
  if (href !== null) l.setAttribute("href", href);
  if (id === "D") l.disabled = true;
  l.onload = () => EV.push(id + ":load");
  l.onerror = () => EV.push(id + ":error");
  document.head.appendChild(l);
}
</script></body></html>"#,
    )
    .await;
    let links = r#"["M", "N", "E", "W", "U", "D"].map(i => document.getElementById(i).sheet === null).join()"#;
    assert_eq!(ask(&mut e, links).await, "true,true,true,true,true,true", "no link has a sheet");
    assert_eq!(ask(&mut e, "document.styleSheets.length").await, "0");
    assert_eq!(ask(&mut e, "EV.join()").await, "", "nothing fired at any link");
    assert_eq!(ask(&mut e, "performance.getEntriesByType('resource').filter(r => r.name.includes('[')).length").await, "0");
    assert_eq!(count(&log, "/m.css"), 1, "the parser's disabled link is requested; log {:?}", log.lock().unwrap());
    assert_eq!(count(&log, "/s.css"), 0, "the script's is not; log {:?}", log.lock().unwrap());

    e.eval("enable", "document.getElementById('M').disabled = false; document.getElementById('D').removeAttribute('disabled');".to_string()).expect("eval");
    settle(&mut e, 600).await;
    e.take_output();
    assert_eq!(ask(&mut e, "EV.slice().sort().join()").await, "D:load,M:load");
    assert_eq!(ask(&mut e, "['M', 'D'].map(i => document.getElementById(i).sheet.cssRules.length).join()").await, "2,3");
    assert_eq!(ask(&mut e, "Array.from(document.styleSheets, s => s.ownerNode.id).join()").await, "M,D");
    assert_eq!(ask(&mut e, "['x', 'y'].map(i => getComputedStyle(document.getElementById(i)).width).join()").await, "42px,43px");
    assert_eq!(count(&log, "/m.css"), 2, "enabled, the parser's link asks again; log {:?}", log.lock().unwrap());
    assert_eq!(count(&log, "/s.css"), 1, "and the script's for the first time; log {:?}", log.lock().unwrap());
}

/// A sheet whose element leaves the document is DETACHED, and the element's next sheet is a new one. Chrome 152,
/// `<style title=tt>` and `<link title=lt>` with their sheets disabled by script:
///
/// ```text
///   removed                       ownerNode null; disabled, rules, title and href kept; the element's sheet null
///   the removed link's title set  the detached sheet still answers `lt`
///   insertRule into the detached  2 rules there; the style appended again has a NEW sheet of 1 rule
///   style's old sheet
///   the link appended again       `load` again, a new sheet
/// ```
///
/// Before: `ownerNode` still named the removed element and the reinserted one answered the same sheet, edits
/// included, and the link fired nothing.
#[tokio::test]
async fn a_removed_elements_sheet_is_detached_and_its_next_sheet_is_new() {
    let (mut e, _log) = page_of(
        r#"<!doctype html><html><head><script>var EV = [];</script>
<style id="S" title="tt">#x { color: red }</style>
<link id="L" rel="stylesheet" href="/m.css" title="lt" onload="EV.push('L:load')">
</head><body><div id="x"></div></body></html>"#,
    )
    .await;
    let d = r#"(s => [s.ownerNode && s.ownerNode.id, s.disabled, s.cssRules.length, s.title, s.href && s.href.replace(location.origin, '')].join())"#;
    e.eval("detach", "var S = document.getElementById('S'), L = document.getElementById('L'); var s0 = S.sheet, l0 = L.sheet; s0.disabled = true; l0.disabled = true; S.remove(); L.remove(); L.setAttribute('title', 'changed');".to_string()).expect("eval");
    e.run().await.expect("run");
    assert_eq!(ask(&mut e, &format!("{d}(s0)")).await, ",true,1,tt,", "the style's sheet, detached");
    assert_eq!(ask(&mut e, &format!("{d}(l0)")).await, ",true,2,lt,/m.css", "the link's, detached");
    assert_eq!(ask(&mut e, "[S.sheet, L.sheet].join()").await, ",");
    e.eval("back", "s0.insertRule('#q {}', 0); document.head.append(S, L);".to_string()).expect("eval");
    settle(&mut e, 300).await;
    e.take_output();
    assert_eq!(ask(&mut e, "[S.sheet === s0, S.sheet.cssRules.length, s0.cssRules.length, L.sheet === l0, L.sheet.disabled].join()").await, "false,1,2,false,false");
    assert_eq!(ask(&mut e, &format!("{d}(L.sheet)")).await, "L,false,2,changed,/m.css");
    assert_eq!(ask(&mut e, "EV.join()").await, "L:load,L:load", "the link fires `load` again");
}

/// A `<style>` fires `load` -- `error` for a failed `@import` -- and a link whose sheet its MIME type refuses fires
/// `load` with an empty sheet unless `nosniff` refused it. Chrome 152 (no-quirks document):
///
/// ```text
///   <style> appended                      microtask, load, setTimeout(0)
///   @import of /m.css / of a 404          load once it arrived / error
///   <link> to text/plain                  load, 0 rules
///   <link> to text/plain + nosniff        error, cssRules throws SecurityError
/// ```
///
/// Before: no `<style>` ever fired anything, and both links fired `error` with an empty readable sheet.
#[tokio::test]
async fn a_style_fires_load_and_a_refused_sheet_fires_load_unless_nosniff() {
    let (mut e, _log) = page_of(r#"<!doctype html><html><head><script>var EV = [];</script></head><body><div id="x"></div></body></html>"#).await;
    e.eval(
        "insert",
        r#"
const on = (el, id) => { el.onload = () => EV.push(id + ':load'); el.onerror = () => EV.push(id + ':error'); return el; };
const st = (id, text) => { const s = on(document.createElement('style'), id); s.textContent = text; return s; };
const ln = (id, href) => { const l = on(document.createElement('link'), id); l.id = id; l.rel = 'stylesheet'; l.href = href; return l; };
document.head.appendChild(st('A', '#x { color: red }'));
Promise.resolve().then(() => EV.push('microtask'));
setTimeout(() => EV.push('timeout0'), 0);
document.head.append(st('I', '@import url(/m.css);'), st('F', '@import url(/missing.css);'), ln('P', '/plain.txt'), ln('N', '/nosniff.txt'));
"#
        .to_string(),
    )
    .expect("eval");
    settle(&mut e, 800).await;
    e.take_output();
    assert_eq!(ask(&mut e, "EV.slice(0, 3).join()").await, "microtask,A:load,timeout0");
    assert_eq!(ask(&mut e, "EV.slice(3).sort().join()").await, "F:error,I:load,N:error,P:load");
    let rules = "(() => { const P = document.getElementById('P'), N = document.getElementById('N'); try { return [P.sheet.cssRules.length, N.sheet.cssRules.length].join() } catch (x) { return P.sheet.cssRules.length + ',' + x.name } })()";
    assert_eq!(ask(&mut e, rules).await, "0,SecurityError");
}

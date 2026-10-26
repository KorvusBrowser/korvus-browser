//! A stylesheet's life after its first load -- its `@import`s, its `href` moving on, its element moved, disabled,
//! renamed -- in a live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expected value is Google Chrome 152.0.7977.82's (`--headless=new`, this machine, 2026-10-23), read off the
//! same shapes served by a loopback server answering `Cache-Control: no-store` and logging every request.

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every path this server was asked for, in arrival order, duplicates kept.
type Log = Arc<Mutex<Vec<String>>>;

/// `(status, delay in ms, body)`; every body is `text/css`.
fn route(path: &str) -> (u16, u64, &'static str) {
    match path {
        "/m.css" => (200, 0, "a { color: red }\n#x { width: 42px }"),
        "/b.css" => (200, 0, "#x { width: 47px }"),
        "/slow.css" => (200, 300, "#y { width: 46px }"),
        "/slow2.css" => (200, 300, "#x { width: 48px }"),
        "/imp404.css" => (200, 0, "@import url(/missing.css);\n#x { height: 3px }"),
        "/impslow.css" => (200, 0, "@import url(/slow.css);\n#z { height: 3px }"),
        _ => (404, 0, ""),
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
            let (status, delay, ctype, body) = if path == "/page.html" {
                (200, 0, "text/html", page.as_str())
            } else {
                let (status, delay, body) = route(&path);
                (status, delay, "text/css", body)
            };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
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

/// Run `js`, then the event loop for `ms`.
async fn act(e: &mut Engine, js: &str, ms: u64) {
    e.eval("lifecycle-act", js.to_string()).expect("eval");
    settle(e, ms).await;
    e.take_output();
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("lifecycle-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

fn count(log: &Log, path: &str) -> usize {
    log.lock().unwrap().iter().filter(|p| p.as_str() == path).count()
}

const DOC: &str = r#"<!doctype html><html><head><script>var EV = [];
function on(el, id) { el.onload = () => EV.push(id + ':load'); el.onerror = () => EV.push(id + ':error'); return el; }
function link(id, href) { const l = on(document.createElement('link'), id); l.id = id; l.rel = 'stylesheet'; l.href = href; return l; }
</script></head><body><div id="x"></div><div id="y"></div><div id="z"></div></body></html>"#;

/// A `<style>` whose text is replaced by an `@import` of ANOTHER URL at the same place requests that URL, fires its
/// `load`, and lets the window's fire. Chrome 152: `@import url(/b.css)`, then inside its `load` the text becomes
/// `@import url(/m.css)`: `/m.css` requested, the style's `load` again, the window's `load`, `#x` 42px.
///
/// Before: the import dedupe kept the place alone, so `/m.css` was never requested, the style's wait held the
/// document's `load` until the starvation watchdog (readyState `interactive` at 600 ms), and `#x` stayed unstyled.
#[tokio::test]
async fn a_style_whose_text_imports_another_url_at_the_same_place_requests_it() {
    let (mut e, log) = page_of(
        r#"<!doctype html><html><head><script>var EV = []; addEventListener('load', () => EV.push('window'));
const s = document.createElement('style'); s.textContent = '@import url(/b.css);';
let n = 0; s.onload = () => { EV.push('S:load'); if (n++ === 0) s.textContent = '@import url(/m.css);'; };
s.onerror = () => EV.push('S:error');
document.head.appendChild(s);
</script></head><body><div id="x"></div></body></html>"#,
    )
    .await;
    settle(&mut e, 300).await;
    assert_eq!(count(&log, "/m.css"), 1, "the new import is requested; log {:?}", log.lock().unwrap());
    assert_eq!(ask(&mut e, "document.readyState").await, "complete");
    assert_eq!(ask(&mut e, "EV.filter(x => x !== 'S:load').join()").await, "window");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('x')).width").await, "42px");
}

/// A script's link `disabled` while its sheet is on its way still fires `load` when the sheet arrives; enabling it
/// afterwards requests nothing, fires nothing and leaves it no sheet. Chrome 152: `appendChild(link);
/// link.disabled = true`, the sheet 300 ms away: `load`, one request; `disabled = false` 500 ms later: no event, no
/// request, `sheet` null, not in `document.styleSheets`.
///
/// Before: the completion went unheard (a `link.onload` promise never resolved), and enabling the link requested
/// the sheet again and fired `load` then.
#[tokio::test]
async fn a_link_disabled_while_its_sheet_loads_fires_load_and_enabling_it_requests_nothing() {
    let (mut e, log) = page_of(DOC).await;
    act(&mut e, "var D = link('D', '/slow.css'); document.head.appendChild(D); D.disabled = true;", 600).await;
    assert_eq!(ask(&mut e, "EV.join()").await, "D:load");
    act(&mut e, "D.disabled = false;", 500).await;
    assert_eq!(ask(&mut e, "EV.join()").await, "D:load", "enabling it fires nothing");
    assert_eq!(count(&log, "/slow.css"), 1, "and requests nothing; log {:?}", log.lock().unwrap());
    assert_eq!(ask(&mut e, "[D.sheet, document.styleSheets.length].join()").await, ",0");
}

/// A link moved, or disabled and enabled again, has a NEW sheet holding its response again -- not the edits made to
/// the sheet it had -- and the old sheet keeps them, its rule objects the same objects. Chrome 152, `/m.css` (2 rules,
/// `#x` 42px) loaded, `insertRule('#x { width: 77px }', 2)` into its sheet, then the link moved (or disabled and
/// enabled): a new sheet of 2 rules, `#x` 42px, the old one 3 rules and `ownerNode` null; a `<style>`'s first rule
/// read before its move is `===` its detached sheet's first rule after it.
///
/// Before: the new sheet shared the old one's rules, 3 of them, and `#x` stayed 77px; the rule objects were new ones.
#[tokio::test]
async fn a_moved_or_reenabled_link_holds_its_response_again_not_its_edits() {
    let (mut e, _log) = page_of(DOC).await;
    act(&mut e, "var L = link('L', '/m.css'); document.head.appendChild(L);", 300).await;
    let edit_and = |how: &str| format!("var s1 = L.sheet; s1.insertRule('#x {{ width: 77px }}', 2); {how}");
    act(&mut e, &edit_and("document.body.appendChild(L);"), 300).await;
    let state = "[L.sheet === s1, L.sheet.cssRules.length, s1.cssRules.length, s1.ownerNode, getComputedStyle(document.getElementById('x')).width].join()";
    assert_eq!(ask(&mut e, state).await, "false,2,3,,42px", "moved");
    act(&mut e, &edit_and("L.disabled = true; L.disabled = false;"), 300).await;
    assert_eq!(ask(&mut e, state).await, "false,2,3,,42px", "disabled and enabled");
    act(&mut e, "var S = document.createElement('style'); S.textContent = '#y { color: red } #z { color: red }'; document.head.appendChild(S); var t0 = S.sheet, q0 = t0.cssRules[0]; S.remove(); document.head.appendChild(S);", 100).await;
    assert_eq!(ask(&mut e, "[t0.cssRules[0] === q0, t0.cssRules.length, S.sheet === t0].join()").await, "true,2,false");
}

/// A loaded link whose `href` can no longer fetch -- removed, `""`, `"http://["` -- has no sheet from then on and
/// styles nothing; one whose `href` moves to a 404 has a new, EMPTY sheet. Chrome 152: `sheet` null at once, out of
/// `document.styleSheets`, `#x` unstyled, the old sheet's `ownerNode` null with its 2 rules, no event; the 404: `error`,
/// a new sheet of 0 rules, `#x` unstyled.
///
/// Before: the first three kept their loaded sheets, listed and styling; the 404's new sheet held the old 2 rules.
#[tokio::test]
async fn a_link_whose_href_can_fetch_nothing_has_no_sheet_and_a_404_leaves_an_empty_one() {
    let (mut e, _log) = page_of(DOC).await;
    for change in ["L.removeAttribute('href')", "L.setAttribute('href', '')", "L.href = 'http://['"] {
        act(&mut e, "EV = []; document.querySelectorAll('link').forEach(l => l.remove()); var L = link('L', '/m.css'); document.head.appendChild(L);", 300).await;
        act(&mut e, &format!("var s1 = L.sheet; {change}; var sync = [L.sheet, document.styleSheets.length].join();"), 300).await;
        assert_eq!(ask(&mut e, "sync").await, ",0", "{change}: no sheet at once");
        let after = "[L.sheet, document.styleSheets.length, getComputedStyle(document.getElementById('x')).width === '42px', s1.ownerNode, s1.cssRules.length, EV.join()].join('|')";
        assert_eq!(ask(&mut e, after).await, "|0|false||2|L:load", "{change}");
    }
    act(&mut e, "EV = []; var F = link('F', '/m.css'); document.head.appendChild(F);", 300).await;
    act(&mut e, "var f1 = F.sheet; F.href = '/missing.css';", 300).await;
    let after = "[F.sheet === f1, F.sheet.cssRules.length, f1.cssRules.length, getComputedStyle(document.getElementById('x')).width === '42px', EV.join()].join('|')";
    assert_eq!(ask(&mut e, after).await, "false|0|2|false|F:load,F:error");
}

/// A link's `load` waits for its `@import`s, and is `error` when one failed, as a `<style>`'s is. Chrome 152: a link
/// to a sheet importing a 404 fires `error`; one importing a sheet 300 ms away fires `load` once it is there, its
/// rule applying inside the handler.
///
/// Before: both fired `load` at their own response, before the slow import had arrived.
#[tokio::test]
async fn a_links_load_waits_for_its_imports() {
    let (mut e, _log) = page_of(DOC).await;
    act(
        &mut e,
        "var W = []; document.head.appendChild(link('E', '/imp404.css')); const S = link('S', '/impslow.css'); S.addEventListener('load', () => W.push(getComputedStyle(document.getElementById('y')).width)); document.head.appendChild(S);",
        800,
    )
    .await;
    assert_eq!(ask(&mut e, "EV.slice().sort().join()").await, "E:error,S:load");
    assert_eq!(ask(&mut e, "W.join()").await, "46px", "the slow import's rule applies when `load` fires");
}

/// A sheet keeps the `title` its owner had while connected -- a REMOVED `title` included. Chrome 152: `<style
/// title=a>`, its sheet read, `removeAttribute('title')` (or `toggleAttribute('title', false)`), the element removed:
/// the old sheet's `title` is null.
///
/// Before: removing the attribute was never seen, and the detached sheet answered `a`.
#[tokio::test]
async fn a_removed_title_is_not_the_detached_sheets() {
    let (mut e, _log) = page_of(DOC).await;
    let js = "var A = document.createElement('style'); A.title = 'a'; document.head.appendChild(A); var a = A.sheet; A.removeAttribute('title'); A.remove();
              var B = document.createElement('style'); B.title = 'b'; document.head.appendChild(B); var b = B.sheet; B.toggleAttribute('title', false); B.remove();";
    act(&mut e, js, 100).await;
    assert_eq!(ask(&mut e, "[a.title, b.title, a.ownerNode, b.ownerNode].map(String).join()").await, "null,null,null,null");
}

/// A stylesheet `href` Chrome's URL parser takes is requested and fails like any request, though the WHATWG parser
/// refuses it. Chrome 152: `http://a b/` (a space in the host) and `http://xn--/` (an `xn--` label that is not
/// punycode): `error`, a sheet, listed; `http://[` and `http://a^b/`: no event, no sheet.
///
/// Before: all four were refused as unparseable: no event, no sheet.
#[tokio::test]
async fn a_stylesheet_href_chromes_parser_takes_is_requested() {
    let (mut e, _log) = page_of(DOC).await;
    act(&mut e, "for (const [id, h] of [['SP', 'http://a b/'], ['XN', 'http://xn--/'], ['BR', 'http://['], ['CA', 'http://a^b/']]) { const l = link(id, 'x'); l.setAttribute('href', h); document.head.appendChild(l); }", 1500).await;
    assert_eq!(ask(&mut e, "EV.slice().sort().join()").await, "SP:error,XN:error");
    assert_eq!(ask(&mut e, "['SP', 'XN', 'BR', 'CA'].map(i => document.getElementById(i).sheet ? 'S' : '-').join('')").await, "SS--");
}

/// A fetch for an `href` the link has moved on from is CANCELLED: one `load`, the current `href`'s rules and URL.
/// Chrome 152: `href` set to a sheet 300 ms away and then, in the same task, to one with none: one `load`, the second
/// sheet's rules, its URL as the sheet's `href`; both requested.
///
/// Before: `load` twice, and the sheet of whichever fetch ended last -- the slow, stale one -- with its URL.
#[tokio::test]
async fn a_fetch_for_an_href_the_link_moved_on_from_is_cancelled() {
    let (mut e, log) = page_of(DOC).await;
    act(&mut e, "var P = link('P', '/slow2.css'); document.head.appendChild(P); P.href = '/b.css';", 700).await;
    assert_eq!(ask(&mut e, "EV.join()").await, "P:load");
    let state = "[P.sheet.href.replace(location.origin, ''), P.sheet.cssRules[0].cssText, getComputedStyle(document.getElementById('x')).width].join('|')";
    assert_eq!(ask(&mut e, state).await, "/b.css|#x { width: 47px; }|47px");
    assert_eq!((count(&log, "/slow2.css"), count(&log, "/b.css")), (1, 1), "both requested; log {:?}", log.lock().unwrap());
}

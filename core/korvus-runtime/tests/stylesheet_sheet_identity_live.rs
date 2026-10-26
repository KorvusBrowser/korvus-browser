//! A stylesheet's identity across its element's moves, its link's `href` coming back, its `@import`s' answers and its
//! `disabled` flag -- in a live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expected value is Google Chrome 152.0.7977.82's (`--headless=new`, this machine, 2026-10-23), read off the
//! same shapes served by a loopback server logging every request.

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every path this server was asked for, in arrival order, duplicates kept.
type Log = Arc<Mutex<Vec<String>>>;

/// `(status, delay in ms, Content-Type, extra header lines, body)`. `no-store` unless an extra line says otherwise.
fn route(path: &str) -> (u16, u64, &'static str, &'static str, &'static str) {
    match path {
        "/m.css" => (200, 0, "text/css", "", "a { color: red }\n#x { width: 42px }"),
        "/b.css" => (200, 0, "text/css", "", "#x { width: 47px }"),
        "/slow2.css" => (200, 300, "text/css", "", "#x { width: 48px }"),
        "/fresh.css" => (200, 0, "text/css", "Cache-Control: max-age=3600\r\n", "#y { width: 43px }"),
        "/plain.txt" => (200, 0, "text/plain", "", "#z { width: 45px }"),
        "/nosniff.txt" => (200, 0, "text/plain", "X-Content-Type-Options: nosniff\r\n", "#z { width: 46px }"),
        "/imp-plain.css" => (200, 0, "text/css", "", "@import url(/plain.txt);\n#x { height: 3px }"),
        _ => (404, 0, "text/css", "", ""),
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
            let (status, delay, ctype, extra, body) = if path == "/page.html" { (200, 0, "text/html", "", page.as_str()) } else { route(&path) };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let cache = if extra.contains("Cache-Control") { "" } else { "Cache-Control: no-store\r\n" };
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\n{extra}{cache}Connection: close\r\n\r\n", body.len());
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
    e.eval("identity-act", js.to_string()).expect("eval");
    settle(e, ms).await;
    e.take_output();
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("identity-ask", format!("print(String({expr}))")).expect("eval");
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
function width(id, p) { return getComputedStyle(document.getElementById(id))[p || 'width']; }
</script></head><body><div id="x"></div><div id="y"></div><div id="z"></div></body></html>"#;

/// A `<style>` whose sheet script has read, whose text then changed and was laid out, is styled by its text AS IT IS
/// when it is moved. Chrome 152: `S.sheet.cssRules[0]; S.textContent = '#x { width: 20px }'; getComputedStyle(x);
/// head.appendChild(S)`: `#x` 20px, `S.sheet.cssRules[0]` `#x { width: 20px; }`; an `insertRule`, then a text node
/// appended (Emotion's non-speedy shape), laid out, moved to the body: `#y` 40px, height `0px`, 2 rules.
///
/// Before: the moved style's sheet was parsed again from the text it was last read at -- 10px, `#x { width: 10px; }`
/// -- and the sheet sync, which had already parsed the new text, had nothing left to do.
#[tokio::test]
async fn a_moved_style_holds_its_text_as_it_is_now() {
    let (mut e, _log) = page_of(DOC).await;
    act(&mut e, "var S = document.createElement('style'); S.textContent = '#x { width: 10px }'; document.head.appendChild(S); S.sheet.cssRules[0]; S.textContent = '#x { width: 20px }'; width('x'); document.head.appendChild(S);", 100).await;
    assert_eq!(ask(&mut e, "[width('x'), S.sheet.cssRules[0].cssText].join('|')").await, "20px|#x { width: 20px; }");
    act(&mut e, "var T = document.createElement('style'); T.textContent = '#y { width: 10px }'; document.head.appendChild(T); T.sheet.insertRule('#y { height: 5px }', 1); T.appendChild(document.createTextNode('#y { width: 40px }')); width('y'); document.body.appendChild(T);", 100).await;
    assert_eq!(ask(&mut e, "[width('y'), width('y', 'height'), T.sheet.cssRules.length].join('|')").await, "40px|0px|2");
}

/// A link whose `href` comes back to a URL it moved away from fetches it again and fires `load`; one whose fetch of
/// that URL is still in flight asks for it once. Chrome 152, `no-store` sheets: `/m.css` loaded, its `href` set to
/// `""` (or removed) -- `sheet` null -- and written back: `load`, 2 rules, `#x` 42px, `/m.css` requested again; `/m.css`
/// to `/b.css` (loaded) and back: `load` each time, `#x` 42px; `/slow2.css` (300 ms), then `/b.css` and `/slow2.css`
/// again in one task: one `load`, `#x` 48px, `/slow2.css` requested once.
///
/// Before: the URL stayed claimed by the link, so writing it back fetched nothing and fired nothing: an empty sheet,
/// unstyled, and the toggle stayed on `/b.css`.
#[tokio::test]
async fn a_link_whose_href_comes_back_fetches_it_again() {
    let (mut e, log) = page_of(DOC).await;
    for change in ["L.setAttribute('href', '')", "L.removeAttribute('href')"] {
        act(&mut e, "document.querySelectorAll('link').forEach(l => l.remove()); var L = link('L', '/m.css'); document.head.appendChild(L);", 300).await;
        act(&mut e, &format!("EV = []; {change};"), 200).await;
        assert_eq!(ask(&mut e, "[L.sheet, EV.join()].join('|')").await, "|", "{change}: no sheet, no event");
        act(&mut e, "L.href = '/m.css';", 300).await;
        assert_eq!(ask(&mut e, "[L.sheet.cssRules.length, width('x'), EV.join()].join('|')").await, "2|42px|L:load", "{change}, written back");
    }
    assert_eq!(count(&log, "/m.css"), 4, "each link requests /m.css twice; log {:?}", log.lock().unwrap());
    act(&mut e, "document.querySelectorAll('link').forEach(l => l.remove()); var L = link('L', '/m.css'); document.head.appendChild(L);", 300).await;
    act(&mut e, "EV = []; L.href = '/b.css';", 300).await;
    act(&mut e, "L.href = '/m.css';", 300).await;
    assert_eq!(ask(&mut e, "[L.sheet.cssRules.length, width('x'), EV.join()].join('|')").await, "2|42px|L:load,L:load");
    act(&mut e, "EV = []; document.querySelectorAll('link').forEach(l => l.remove()); var L = link('L', '/slow2.css'); document.head.appendChild(L); L.href = '/b.css'; L.href = '/slow2.css';", 700).await;
    assert_eq!(ask(&mut e, "[L.sheet.cssRules.length, width('x'), EV.join()].join('|')").await, "1|48px|L:load");
    assert_eq!(count(&log, "/slow2.css"), 1, "log {:?}", log.lock().unwrap());
}

/// A fresh (`max-age`) sheet a link loads again is its memory cache's: no second request and no second resource-timing
/// entry, but its `load` again. Chrome 152: `Cache-Control: max-age=3600`, loaded, `href` cleared and written back:
/// `load` twice, one request, one `link` entry for the URL.
///
/// Before the link let go of the URL it could not load it again at all; letting go, each load made an entry.
#[tokio::test]
async fn a_fresh_sheet_loaded_again_makes_no_second_entry() {
    let (mut e, log) = page_of(DOC).await;
    act(&mut e, "var F = link('F', '/fresh.css'); document.head.appendChild(F);", 300).await;
    act(&mut e, "F.href = '';", 200).await;
    act(&mut e, "F.href = '/fresh.css';", 300).await;
    assert_eq!(ask(&mut e, "[width('y'), EV.join(), performance.getEntriesByType('resource').filter(r => r.name.endsWith('/fresh.css')).length].join('|')").await, "43px|F:load,F:load|1");
    assert_eq!(count(&log, "/fresh.css"), 1, "log {:?}", log.lock().unwrap());
}

/// An `@import` answered with another type than CSS is a LOADED, empty sheet -- `load`, not `error` -- unless `nosniff`
/// refused it; every answered import has a `styleSheet`. Chrome 152, no-quirks document, a `<style>` and a linked
/// sheet each importing:
///
/// ```text
///   /plain.txt (text/plain)            load    styleSheet.cssRules 0 rules        #z not applied
///   /nosniff.txt (text/plain, nosniff) error   SecurityError                      not applied
///   /missing.css (404)                 error   0 rules
/// ```
///
/// and in a quirks document `/plain.txt` applies: 1 rule, `#z` 45px.
///
/// Before: every import that was not CSS fired `error` -- a linked sheet's too, which now waits for its imports -- its
/// `styleSheet` was null, and no quirks document applied one.
#[tokio::test]
async fn an_import_of_another_type_is_an_empty_sheet_and_fires_load() {
    let (mut e, _log) = page_of(DOC).await;
    act(
        &mut e,
        "var I = {}; for (const [k, u] of [['P', '/plain.txt'], ['N', '/nosniff.txt'], ['M', '/missing.css']]) { const s = on(document.createElement('style'), k); s.textContent = '@import url(' + u + ');'; document.head.appendChild(s); I[k] = s; }
         document.head.appendChild(link('L', '/imp-plain.css'));",
        500,
    )
    .await;
    assert_eq!(ask(&mut e, "EV.slice().sort().join()").await, "L:load,M:error,N:error,P:load");
    let rules = "Object.keys(I).map(k => { const s = I[k].sheet.cssRules[0].styleSheet; if (!s) return k + ':null'; try { return k + ':' + s.cssRules.length; } catch (x) { return k + ':' + x.name; } }).join()";
    assert_eq!(ask(&mut e, rules).await, "P:0,N:SecurityError,M:0");
    assert_eq!(ask(&mut e, "[width('z') === '45px', document.getElementById('L').sheet.cssRules[0].styleSheet.cssRules.length].join('|')").await, "false|0");
    let (mut q, _log) = page_of("<html><head><style>@import url(/plain.txt);</style></head><body><div id=z></div></body></html>").await;
    assert_eq!(ask(&mut q, "[document.compatMode, document.styleSheets[0].cssRules[0].styleSheet.cssRules.length, getComputedStyle(document.getElementById('z')).width].join('|')").await, "BackCompat|1|45px");
}

/// The sheets a moved `<style>` leaves behind keep their own rules however many there are, and stop costing anything
/// once script lets them go. Chrome 152: a 300-rule `<style>` moved 1,000 times with its old sheet's `cssRules` read
/// after each move takes 3 ms, and 2,000 restyles after it as long as before it; 50 old sheets kept, each with an
/// `insertRule` of its own, hold 301 rules and their own edit, `ownerNode` null.
///
/// Before: every old sheet read left a copy of its rules in the page's sheet list for good, which the next matcher
/// build walked: 2,000 restyles after 1,000 such moves took 156 ms against 19 before them, and 238 MB (`mvr.html`).
#[tokio::test]
async fn the_sheets_a_moved_style_leaves_keep_their_rules_and_go_when_dropped() {
    let (mut e, _log) = page_of(DOC).await;
    act(
        &mut e,
        "var S = document.createElement('style'); S.textContent = Array.from({ length: 300 }, (_, i) => '.r' + i + ' { margin-left: ' + i + 'px }').join('\\n'); document.head.appendChild(S);
         function restyles() { const t0 = performance.now(); const x = document.getElementById('x'); for (let i = 0; i < 2000; i++) { x.className = 'r' + (i % 300); getComputedStyle(x).marginLeft; } return performance.now() - t0; }
         restyles(); var before = restyles();
         for (let i = 0; i < 1000; i++) { const old = S.sheet; document.head.appendChild(S); old.cssRules.length; }
         var after = restyles();
         var kept = []; for (let i = 0; i < 50; i++) { const old = S.sheet; document.head.appendChild(S); old.cssRules.length; old.insertRule('.k' + i + ' { color: red }', 0); kept.push(old); }",
        50,
    )
    .await;
    let ratio: f64 = ask(&mut e, "after / before").await.parse().unwrap();
    assert!(ratio < 4.0, "2,000 restyles after the moves took {ratio:.1}x as long as before them");
    // each kept sheet holds its rules and its own edit, and the element's sheet none of them
    let kept = "kept.every((s, i) => s.cssRules.length === 301 && s.cssRules[0].cssText === '.k' + i + ' { color: red; }' && s.ownerNode === null)";
    assert_eq!(ask(&mut e, &format!("[{kept}, S.sheet.cssRules.length, S.sheet.cssRules[0].cssText, getComputedStyle(document.getElementById('x')).marginLeft].join('|')")).await, "true|300|.r0 { margin-left: 0px; }|199px");
}

/// `sheet.disabled = true` takes a sheet out of the cascade, its rules still readable and writable, until it is
/// enabled -- or its element moves, which makes a new, enabled sheet. Chrome 152, a `<style>` (`#y` 10px) and a link
/// (`/m.css`, `#x` 42px) disabled -- one through `document.styleSheets[1]` -- and an `insertRule('#y { height: 5px }')`:
/// both unstyled, both listed, 2 rules each, `<style>.disabled` true; enabled: `#y` 10px and 5px high; the link moved:
/// a new sheet, enabled, `#x` 42px, the old one disabled with `ownerNode` null; a disabled `<style>` moved: the same.
/// An adopted constructed sheet disabled: unstyled, enabled: styled; one made with `{disabled: true}`: unstyled.
///
/// Before: the flag was only read back -- every disabled sheet went on styling.
#[tokio::test]
async fn a_disabled_sheet_styles_nothing_until_it_is_enabled() {
    let (mut e, _log) = page_of(DOC).await;
    act(&mut e, "var S = document.createElement('style'); S.textContent = '#y { width: 10px }'; document.head.appendChild(S); var L = link('L', '/m.css'); document.head.appendChild(L);", 300).await;
    act(&mut e, "S.sheet.disabled = true; document.styleSheets[1].disabled = true; S.sheet.insertRule('#y { height: 5px }', 1);", 50).await;
    let off = "[width('y') === '10px', width('x') === '42px', width('y', 'height'), document.styleSheets.length, S.sheet.cssRules.length, L.sheet.cssRules.length, S.disabled].join('|')";
    assert_eq!(ask(&mut e, off).await, "false|false|0px|2|2|2|true");
    act(&mut e, "S.sheet.disabled = false;", 50).await;
    assert_eq!(ask(&mut e, "[width('y'), width('y', 'height'), S.sheet.disabled, S.disabled].join('|')").await, "10px|5px|false|false");
    act(&mut e, "var l1 = L.sheet; document.body.appendChild(L);", 300).await;
    assert_eq!(ask(&mut e, "[width('x'), L.sheet === l1, L.sheet.disabled, l1.disabled, l1.ownerNode].join('|')").await, "42px|false|false|true|");
    act(&mut e, "S.disabled = true; var s1 = S.sheet; document.body.appendChild(S);", 50).await;
    assert_eq!(ask(&mut e, "[width('y'), S.sheet === s1, S.sheet.disabled, s1.disabled, s1.cssRules.length].join('|')").await, "10px|false|false|true|2");
    act(&mut e, "var k = new CSSStyleSheet(); k.replaceSync('#z { width: 7px }'); document.adoptedStyleSheets = [k]; var k0 = width('z'); k.disabled = true; var k1 = width('z'); k.disabled = false; var k2 = width('z');", 50).await;
    assert_eq!(ask(&mut e, "[k0, k1 === '7px', k2].join('|')").await, "7px|false|7px");
    act(&mut e, "var c = new CSSStyleSheet({ disabled: true }); c.replaceSync('#z { width: 8px }'); document.adoptedStyleSheets = [k, c];", 50).await;
    assert_eq!(ask(&mut e, "width('z')").await, "7px");
}

/// A PARSED stylesheet link whose `href` Chrome's URL parser refuses fetches nothing and fires nothing, but leaves the
/// preload scanner's entry, named as GURL spells the URL; an alternate or non-matching-media one leaves none. Chrome
/// 152: `href="http://["` -> an entry `http://[/` (initiator `link`, duration 0); `"http://a^b"` -> `http://a%5Eb/`;
/// a `rel=preload as=style` -> its entry; `rel="alternate stylesheet"`, `media=print` -> none; no event, no sheet.
///
/// Before: no entry at all (the build before the first fix put the URL on the wire, fired `error`, and named the
/// entry as written).
#[tokio::test]
async fn a_parsed_unparseable_sheet_leaves_the_scanners_entry() {
    let (mut e, _log) = page_of(
        r#"<!doctype html><html><head><script>var EV = [];</script>
<link id=A rel=stylesheet href="http://[" onload="EV.push('A:load')" onerror="EV.push('A:error')">
<link id=B rel=stylesheet href="http://a^b" onload="EV.push('B:load')" onerror="EV.push('B:error')">
<link id=C rel=preload as=style href="http://[/pre">
<link id=D rel="alternate stylesheet" title=t href="http://[/alt">
<link id=E rel=stylesheet media=print href="http://[/print">
</head><body></body></html>"#,
    )
    .await;
    let entries = "performance.getEntriesByType('resource').filter(r => r.initiatorType === 'link').map(r => r.name + ' ' + r.duration).join()";
    assert_eq!(ask(&mut e, entries).await, "http://[/ 0,http://a%5Eb/ 0,http://[/pre 0");
    assert_eq!(ask(&mut e, "[EV.join(), document.getElementById('A').sheet, document.styleSheets.length].join('|')").await, "||0");
}

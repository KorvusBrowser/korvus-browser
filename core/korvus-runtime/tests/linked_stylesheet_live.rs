//! A `<link rel=stylesheet>`'s bytes reach the cascade AND the CSSOM, in a live engine.
//!
//! # The defect, measured before the fix rather than argued
//!
//! P2a's interactivity sweep reported that a linked sheet's `cssRules` was empty while an inline
//! `<style>`'s was not, and read that as a CSSOM defect with "the declarations reaching the element".
//! Run against a loopback server at the base commit (`9fced3cc`), with one linked sheet and one
//! inline `<style>` on the same page:
//!
//! ```text
//! requests                                ["/s.css"]      the bytes DO arrive, exactly once
//! document.styleSheets.length             2               both sheets are in the list
//! styleSheets[0].cssRules.length          0               the linked one has no rules
//! styleSheets[1].cssRules.length          2               the inline one does
//! getComputedStyle(#lk).width             1904px          #lk { width: 123px } NEVER APPLIED
//! ```
//!
//! The fourth line is the correction: **the cascade did not see them either.** The three candidate
//! causes a diagnosis has to separate are "the bytes never arrive", "they arrive and are never
//! parsed", and "they are parsed into a store nothing reads". L2's request ledger rules out the
//! first in one line; the fifth rules out the third. What was actually happening is the second, and
//! it is one branch: `subresource.rs`'s `cb_load_ok` read the response body and used it for exactly
//! two destinations, a classic script and a frame's document. For `style` it fell off the end.
//!
//! # The oracle
//!
//! Every expectation below was measured on **stock Chrome 150.0.7871.46**, headless, driven over CDP,
//! against a pair of loopback servers on different ports (so "cross-origin" is a real origin
//! difference and not a scheme trick). Nothing here is read off a specification. The table Chrome
//! answered, one `<link rel=stylesheet>` per row, the rule in each sheet being `#<id>_t { width: 123px }`:
//!
//! ```text
//! same-origin  text/css                      sheet=obj  rules=1       cascade=123px
//! same-origin  text/plain                    sheet=obj  rules=0       cascade=764px  (refused on MIME)
//! same-origin  no Content-Type               sheet=obj  rules=1       cascade=123px
//! same-origin  404 with a text/css body      sheet=obj  rules=0       cascade=764px
//! same-origin  500 with a text/css body      sheet=obj  rules=0       cascade=764px
//! cross-origin no crossorigin, no ACAO       sheet=obj  rules=THROW SecurityError  cascade=123px
//! cross-origin no crossorigin, ACAO: *       sheet=obj  rules=THROW SecurityError  cascade=123px
//! cross-origin crossorigin,    ACAO: *       sheet=obj  rules=2       cascade=123px
//! <link href=… with NO rel>                  sheet=null
//! document.createElement("link").sheet       null
//! ```
//!
//! The `SecurityError` rows are the ones that decide whether a corpus number means anything: P2b
//! measured that 52 of 142 sampled pages carry at least one cross-origin sheet, so an engine that
//! answered `[]` there instead of throwing would report rules on pages Chrome refuses to show.

use engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every path this server was asked for, in arrival order, duplicates kept.
type Log = Arc<Mutex<Vec<String>>>;

/// `(status, Content-Type or none, extra headers, body)` for one route. The routes are named for
/// what they prove rather than for a file type, so a failure names the case.
fn route(path: &str) -> (u16, Option<&'static str>, &'static [(&'static str, &'static str)], String) {
    match path {
        "/s.css" => (200, Some("text/css"), &[], ".tile:hover { color: rgb(1, 2, 3) }\n.a:hover { color: rgb(4, 5, 6) }\n#lk { width: 123px }".into()),
        "/hover.css" => (200, Some("text/css"), &[], "#box { width: 400px; height: 400px }\n#box:hover { width: 250px; background-color: rgb(9, 9, 9) }".into()),
        "/order.css" => (200, Some("text/css"), &[], "#ord { width: 10px }".into()),
        "/order2.css" => (200, Some("text/css"), &[], "#ord2 { width: 30px }".into()),
        "/plain.css" => (200, Some("text/plain"), &[], "#mime { width: 123px }".into()),
        "/nomime.css" => (200, None, &[], "#nomime { width: 123px }".into()),
        "/e404.css" => (404, Some("text/css"), &[], "#e404 { width: 123px }".into()),
        "/xo.css" => (200, Some("text/css"), &[], "#xo { width: 200px }".into()),
        "/cors.css" => (200, Some("text/css"), &[("Access-Control-Allow-Origin", "*")], "#co { width: 300px }\n#co2 { width: 301px }".into()),
        _ => (404, Some("text/plain"), &[], "no".into()),
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
            let (status, ctype, extra, body) =
                if path == "/page.html" { (200, Some("text/html"), &[][..], page) } else { route(&path) };
            let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n", body.len());
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

/// A booted engine on `body_html` served from a loopback origin, with a SECOND loopback origin
/// available for the cross-origin rows. `build` is handed `(same_origin_base, cross_origin_base)`
/// and returns the document.
///
/// Returns the engine, the request log of the page's own origin, and the cross-origin log.
async fn page_of(build: impl FnOnce(&str, &str) -> String) -> (Engine, Log, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let xo_log: Log = Arc::new(Mutex::new(Vec::new()));
    let xo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let xo_addr = xo_listener.local_addr().unwrap();
    tokio::spawn(serve(xo_listener, String::new(), xo_log.clone()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let doc = build(&format!("http://{addr}"), &format!("http://{xo_addr}"));
    tokio::spawn(serve(listener, doc.clone(), log.clone()));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    // A stylesheet delays the document's `load`, so `pump_until_loaded` already covers the fetch;
    // the settle is for the second origin, whose connection is a separate task.
    let until = tokio::time::Instant::now() + Duration::from_millis(400);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (e, log, xo_log)
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("p3a-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

fn counts(log: &Log) -> HashMap<String, usize> {
    let mut c = HashMap::new();
    for p in log.lock().unwrap().iter() {
        *c.entry(p.clone()).or_default() += 1;
    }
    c
}

// ---------------------------------------------------------------------------------------------
// 1. THE GATE: a linked sheet's rules are in the CSSOM, and the cascade applies them.
// ---------------------------------------------------------------------------------------------

/// The whole defect in one page: one linked sheet, one inline `<style>`, both carrying a rule and a
/// `:hover` rule. Before this unit the linked half of every line below read `0` / `1904px`.
#[tokio::test]
async fn a_linked_stylesheet_has_its_rules_in_the_cssom_and_in_the_cascade() {
    let (mut e, log, _) = page_of(|_same, _xo| {
        r#"<!doctype html><html><head>
<link id="l" rel="stylesheet" href="/s.css">
<style>.inline:hover { color: rgb(7, 8, 9) } #il { width: 77px }</style>
</head><body><div id="lk" class="tile"></div><div id="il" class="inline"></div></body></html>"#
            .to_string()
    })
    .await;

    // The bytes arrive, once. (L2's ledger answers the first of the three candidate causes.)
    assert_eq!(counts(&log).get("/s.css").copied().unwrap_or(0), 1, "the linked sheet is fetched exactly once; log: {:?}", log.lock().unwrap());

    assert_eq!(ask(&mut e, "document.styleSheets.length").await, "2");
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules.length").await, "3", "the LINKED sheet's three rules");
    assert_eq!(
        ask(&mut e, r#"Array.from(document.styleSheets[0].cssRules).map(r => r.selectorText).join("|")"#).await,
        ".tile:hover|.a:hover|#lk",
    );
    assert_eq!(ask(&mut e, "document.styleSheets[1].cssRules.length").await, "2", "the inline sheet is unaffected");
    // The rule text survives the round trip, so `cssRules` is a view of the parsed sheet and not a
    // count of something else.
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules[2].cssText").await, "#lk { width: 123px; }");
    // `cssRules[i].style` is a `CSSStyleDeclaration` that reads EMPTY here -- and equally empty over
    // an inline `<style>`'s rule (`LINKED= INLINE= LEN=0` when both are asked side by side), so it is
    // a pre-existing defect of the rule-backed declaration block and not this unit's. Asserted
    // negatively rather than left out, so the day it is fixed this line says so.
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules[2].style.length === document.styleSheets[1].cssRules[1].style.length").await, "true");
    // THE CASCADE. This is the line P2a's report said already worked and did not.
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('lk')).width").await, "123px");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('il')).width").await, "77px");
    // `<link>.sheet` is the SAME object every read, and it is the sheet in the list.
    assert_eq!(ask(&mut e, "document.getElementById('l').sheet === document.getElementById('l').sheet").await, "true");
    assert_eq!(ask(&mut e, "document.getElementById('l').sheet === document.styleSheets[0]").await, "true");
    assert_eq!(ask(&mut e, "document.styleSheets[0].ownerNode.id").await, "l");
    assert_eq!(ask(&mut e, "document.styleSheets[0].href.endsWith('/s.css')").await, "true");
}

/// A `:hover` rule that lives in a LINKED sheet matches when the pointer arrives — the capability
/// P2b built the chain for and this defect was hiding. `Engine::move_mouse` is the entry point CDP's
/// `Input.dispatchMouseEvent mouseMoved` reaches, so the hit test, the invalidation and the cascade
/// are all in the loop.
#[tokio::test]
async fn a_hover_rule_from_a_linked_sheet_matches() {
    let (mut e, _, _) = page_of(|_s, _x| {
        r#"<!doctype html><html><head><style>body { margin: 0 }</style>
<link rel="stylesheet" href="/hover.css">
</head><body><div id="box"></div></body></html>"#
            .to_string()
    })
    .await;

    assert_eq!(ask(&mut e, "document.styleSheets[1].cssRules.length").await, "2", "the linked sheet parsed");
    assert_eq!(ask(&mut e, "document.querySelectorAll(':hover').length").await, "0", "nothing hovered yet");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('box')).width").await, "400px");

    e.move_mouse(200.0, 200.0, 20.0).await.expect("move");
    e.run().await.expect("run");
    e.take_output();

    assert_eq!(ask(&mut e, "document.getElementById('box').matches(':hover')").await, "true");
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.getElementById('box')).backgroundColor").await,
        "rgb(9, 9, 9)",
        "the linked sheet's :hover rule reaches computed style",
    );
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('box')).width").await, "250px");
}

/// A linked sheet sits where the MARKUP put it, not where its bytes landed. Two equally specific
/// rules for `#ord`: the linked one first in the document, the inline one second, so the inline one
/// wins — which is only true if the linked sheet took a slot in document order rather than being
/// appended whenever the network answered.
#[tokio::test]
async fn a_linked_sheet_cascades_in_document_order_not_arrival_order() {
    let (mut e, _, _) = page_of(|_s, _x| {
        r#"<!doctype html><html><head>
<link rel="stylesheet" href="/order.css">
<style>#ord { width: 20px }</style>
</head><body><div id="ord"></div></body></html>"#
            .to_string()
    })
    .await;
    assert_eq!(ask(&mut e, "document.styleSheets[0].cssRules.length").await, "1", "the linked sheet is styleSheets[0]");
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.getElementById('ord')).width").await,
        "20px",
        "the LATER sheet in the markup wins the tie, whichever arrived first",
    );
}

// ---------------------------------------------------------------------------------------------
// 2. What a page may READ: the origin-clean flag.
// ---------------------------------------------------------------------------------------------

/// A cross-origin sheet without CORS styles the page and throws on `cssRules`, exactly as Chrome
/// does. Answering `[]` here would be the quiet failure: it reads like "this page has no rules".
#[tokio::test]
async fn a_cross_origin_sheet_without_cors_throws_on_cssrules_and_still_styles_the_page() {
    let (mut e, _, xo_log) = page_of(|_s, xo| {
        format!(
            r#"<!doctype html><html><head>
<link id="x" rel="stylesheet" href="{xo}/xo.css">
<link id="c" rel="stylesheet" href="{xo}/cors.css" crossorigin>
</head><body><div id="xo"></div><div id="co"></div></body></html>"#
        )
    })
    .await;

    assert_eq!(counts(&xo_log).get("/xo.css").copied().unwrap_or(0), 1, "the cross-origin sheet is fetched; log: {:?}", xo_log.lock().unwrap());

    // Not origin-clean: every member that would expose a rule throws SecurityError.
    for member in ["cssRules", "rules"] {
        assert_eq!(
            ask(&mut e, &format!("(() => {{ try {{ document.getElementById('x').sheet.{member}; return 'no throw'; }} catch (e) {{ return e.name + '|' + (e instanceof DOMException); }} }})()")).await,
            "SecurityError|true",
            "{member} on a cross-origin sheet",
        );
    }
    for call in ["insertRule('#z{color:red}', 0)", "deleteRule(0)", "addRule('#z', 'color:red')", "removeRule(0)"] {
        assert_eq!(
            ask(&mut e, &format!("(() => {{ try {{ document.getElementById('x').sheet.{call}; return 'no throw'; }} catch (e) {{ return e.name; }} }})()")).await,
            "SecurityError",
            "{call} on a cross-origin sheet",
        );
    }
    // …and everything that is a fact about the LINK rather than about the bytes still answers.
    assert_eq!(ask(&mut e, "document.getElementById('x').sheet.href.endsWith('/xo.css')").await, "true");
    assert_eq!(ask(&mut e, "document.getElementById('x').sheet.ownerNode.id").await, "x");
    assert_eq!(ask(&mut e, "document.getElementById('x').sheet.disabled").await, "false");
    // …and the page is STYLED by it.
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('xo')).width").await, "200px");

    // The CORS one is readable, and is the control that proves the throw above is about the flag
    // and not about cross-origin bytes failing to parse.
    assert_eq!(ask(&mut e, "document.getElementById('c').sheet.cssRules.length").await, "2");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('co')).width").await, "300px");
}

// ---------------------------------------------------------------------------------------------
// 3. The responses that are NOT a stylesheet.
// ---------------------------------------------------------------------------------------------

/// A no-quirks document refuses a stylesheet whose `Content-Type` is present and is not `text/css`,
/// and accepts one with no `Content-Type` at all. A non-2xx response is not parsed either, though the
/// element still has a sheet. All four measured on Chrome; see the module header.
#[tokio::test]
async fn a_response_that_is_not_css_is_not_parsed_as_css() {
    let (mut e, _, _) = page_of(|_s, _x| {
        r#"<!doctype html><html><head>
<link id="m" rel="stylesheet" href="/plain.css">
<link id="n" rel="stylesheet" href="/nomime.css">
<link id="f" rel="stylesheet" href="/e404.css">
</head><body><div id="mime"></div><div id="nomime"></div><div id="e404"></div></body></html>"#
            .to_string()
    })
    .await;
    assert_eq!(ask(&mut e, "document.getElementById('m').sheet.cssRules.length").await, "0", "text/plain is refused");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('mime')).width").await, "1904px");
    assert_eq!(ask(&mut e, "document.getElementById('n').sheet.cssRules.length").await, "1", "no Content-Type is accepted");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('nomime')).width").await, "123px");
    assert_eq!(ask(&mut e, "document.getElementById('f').sheet.cssRules.length").await, "0", "a 404 body is not a stylesheet");
    assert_eq!(ask(&mut e, "getComputedStyle(document.getElementById('e404')).width").await, "1904px");
}

/// A `<link>` that is not a stylesheet has no sheet at all, and neither does a disconnected one.
/// `document.styleSheets` counts only the stylesheet links — which is what stops an icon or a
/// preload from taking a slot in the cascade.
#[tokio::test]
async fn only_a_stylesheet_link_has_a_sheet() {
    let (mut e, _, _) = page_of(|_s, _x| {
        r#"<!doctype html><html><head>
<link id="none" href="/s.css">
<link id="icon" rel="icon" href="/favicon.ico">
<link id="pre" rel="preload" href="/s.css" as="style">
<link id="alt" rel="alternate stylesheet" href="/s.css">
</head><body></body></html>"#
            .to_string()
    })
    .await;
    assert_eq!(ask(&mut e, "document.getElementById('none').sheet").await, "null", "a <link> with no rel");
    assert_eq!(ask(&mut e, "document.getElementById('icon').sheet").await, "null", "rel=icon");
    assert_eq!(ask(&mut e, "document.getElementById('pre').sheet").await, "null", "rel=preload");
    assert_eq!(ask(&mut e, "document.getElementById('alt').sheet === null").await, "false", "rel='alternate stylesheet' IS one");
    assert_eq!(ask(&mut e, "document.createElement('link').sheet").await, "null", "a disconnected <link>");
    assert_eq!(ask(&mut e, "document.styleSheets.length").await, "1", "only the alternate stylesheet is in the list");
}

/// READING `styleEl.sheet` MUST NOT CHANGE WHAT THE PAGE LOOKS LIKE. It used to: the getter minted a
/// SECOND `korvus-page` sheet for the element and parsed the element's text into it, so the inline
/// rules appeared twice in the cascade -- once at the element's own position and once APPENDED at the
/// end, where they outrank every sheet that follows them in the markup. Nothing could see it before
/// this unit, because a linked sheet had no rules to lose the tie.
#[tokio::test]
async fn reading_a_style_elements_sheet_does_not_move_it_in_the_cascade() {
    let (mut e, _, _) = page_of(|_s, _x| {
        r#"<!doctype html><html><head>
<style>#ord2 { width: 40px }</style>
<link rel="stylesheet" href="/order2.css">
</head><body><div id="ord2"></div></body></html>"#
            .to_string()
    })
    .await;
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.getElementById('ord2')).width").await,
        "30px",
        "the LINKED sheet is later in the markup and wins the tie",
    );
    assert_eq!(ask(&mut e, "document.querySelector('style').sheet.cssRules.length").await, "1");
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.getElementById('ord2')).width").await,
        "30px",
        "and still wins it after script has touched the <style>'s sheet",
    );
}

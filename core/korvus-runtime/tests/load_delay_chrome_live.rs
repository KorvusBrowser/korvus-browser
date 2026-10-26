//! What `load` waits for, and in what order things happen around it, as Chrome 150
//! answers it against a loopback server that REALLY sleeps.
//!
//! THE DEFECT. The document's load step waited on its "delay the load event" set, but only until a
//! host cap of 1000 ms after it parked -- after which `load` fired with the set still full. Chrome has
//! no such cap. Measured on Chrome 150.0.7871.46 headless (`--headless=new`, no other flag), each page
//! logging into an array and POSTing it back, every resource served by a Python server that sleeps
//! before answering:
//!
//! ```text
//! inserted <script src> 1.5 s + inserted <link rel=stylesheet> 1.5 s   ran slow · load; color=rgb(1, 2, 3)
//!     korvus before this unit                                          load; color=rgb(0, 0, 0) · ran slow
//! inserted <script src> 3 s (default async, and async = false)         ran slow · load 3003 ms
//! inserted <script src> 6 s                                            ran slow · load 6004 ms
//! inserted <script src> that is NEVER answered                         no load after 20 s, readyState interactive
//! parser <link rel=stylesheet> 3 s                                     link load sheet=true · load; color=rgb(1, 2, 3)
//! parser <script async> / <script defer> / <script type=module> 1.5 s  ran slow · load
//! inserted <script type=module> 1.5 s                                  ran slow · load
//! inline module with a static import 1.5 s                             ran slow · inline module ran · load
//! dynamic import() / fetch() / XMLHttpRequest 1.5 s                    load · (then the resource)
//! <style>@import 1.5 s, and a linked sheet that @imports 1.5 s         load waits; color=rgb(1, 2, 3)
//! new Image() 1.5 s, attached or not                                   img load · load
//! inserted <iframe src> 1.5 s                                          iframe load · load
//! @font-face used by an element, 1.5 s (bytes not a font)             load 1512 ms, its resource entry exists at load
//! @font-face nothing uses                                              load 17 ms, no resource entry
//! a script inserted from a script's onload / from DOMContentLoaded     ran · load
//! an inserted script removed right after insertion                     ran removed · load
//! an inserted stylesheet removed right after insertion                 load 1504 ms, no `load` at the link
//! inserted <link rel=stylesheet>: sheet right after append / at 300 ms null · null; at its load: non-null, 1 rule
//! inserted <link rel=stylesheet> answered 404                          link error sheet=true · load
//! ```
//!
//! Every case below was re-asked of Chrome in exactly the shape the test serves it (the delays, the
//! marker script, the 300 ms timer), and Chrome's log is the expected value.
//!
//! The rule is therefore the one HTML writes and Chrome keeps: `load` waits for the set to be EMPTY,
//! however long that takes. Every test below is an ORDER of events the page itself logged; none reads
//! a clock. The server's sleeps are inputs, chosen past the old 1000 ms cap so a cap that is still
//! firing puts `load` in the wrong place in the log.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Past the old 1000 ms cap: a load step released by any leftover cap lands BEFORE the resource.
const SLOW: u64 = 1500;

const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xff, 0xff, 0x03, 0x00, 0x00, 0x06, 0x00, 0x05, 0x57, 0x0b, 0x7c, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
    0x60, 0x82,
];

fn query<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// `(status, content-type, body, sleep ms)` for a request target. `/held` is answered only once the
/// test opens `gate`.
fn route(target: &str) -> (u16, &'static str, Vec<u8>, u64) {
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let ms = query(q, "ms").and_then(|v| v.parse().ok()).unwrap_or(0);
    match path {
        "/js" | "/held" => (200, "text/javascript", format!("L('ran {}');", query(q, "n").unwrap_or("?")).into_bytes(), ms),
        // a script that, when it runs, has `later()` (the case's head) insert a second one
        "/later" => {
            let how = query(q, "how").unwrap_or("?");
            (200, "text/javascript", format!("L('ran first-{how}'); later('{how}', 'second-{how}');").into_bytes(), 300)
        }
        "/css" => (200, "text/css", b"#p { color: rgb(1, 2, 3) }".to_vec(), ms),
        "/importer.css" => (200, "text/css", format!("@import '/css?ms={SLOW}';").into_bytes(), 0),
        "/png" => (200, "image/png", PNG.to_vec(), ms),
        "/html" => (200, "text/html", b"<p>frame".to_vec(), ms),
        "/font" => (200, "font/woff2", b"not a font".to_vec(), ms),
        _ => (404, "text/plain", b"no".to_vec(), 0),
    }
}

async fn serve(listener: TcpListener, gate: std::sync::Arc<tokio::sync::Semaphore>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let gate = gate.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if target.starts_with("/held") {
                let _ = gate.acquire().await;
            }
            let (status, ctype, body, ms) = route(&target);
            if ms > 0 {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

/// The rig's in-page logger, verbatim in substance: `L()` appends, and `load` appends itself plus
/// whatever the case's `PROBE` reads at that moment.
const PRE: &str = r#"<script>
var R = [];
function L(s) { R.push(s); }
addEventListener('load', function () { L('load' + (window.PROBE ? '; ' + PROBE() : '')); });
</script>"#;

const COLOR: &str = "window.PROBE = function () { return 'color=' + getComputedStyle(document.getElementById('p')).color; };";

struct Page {
    e: Engine,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
}

async fn open(head: &str, body: &str) -> Page {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    tokio::spawn(serve(listener, gate.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("<!doctype html><html><head>{PRE}{head}</head><body>{body}</body></html>");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    Page { e, gate }
}

fn log(e: &mut Engine) -> Vec<String> {
    let v = e.eval_value("p13b-log", "R.slice()".to_string()).expect("eval");
    v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
}

/// Pump until the page has logged `n` entries, or the (generous) budget runs out. The budget is a
/// ceiling on waiting, never asserted on: the assertion is the log.
async fn until_logged(e: &mut Engine, n: usize) -> Vec<String> {
    for _ in 0..400 {
        let _ = e.pump(Duration::from_millis(50)).await;
        let l = log(e);
        if l.len() >= n {
            return l;
        }
    }
    log(e)
}

async fn case(head: &str, body: &str, n: usize) -> Vec<String> {
    let mut p = open(head, body).await;
    until_logged(&mut p.e, n).await
}

// ---- the brief's fixture ----------------------------------------------------------------------

#[tokio::test]
async fn load_waits_for_a_slow_inserted_script_and_a_slow_inserted_stylesheet() {
    let got = case(
        "",
        &format!(
            r#"<div id=p>x</div><script>
            var s = document.createElement('script'); s.src = '/js?ms={SLOW}&n=slow'; document.head.appendChild(s);
            var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/css?ms={SLOW}'; document.head.appendChild(l);
            {COLOR}
            </script>"#
        ),
        2,
    )
    .await;
    assert_eq!(got, ["ran slow", "load; color=rgb(1, 2, 3)"], "Chrome 150: the script runs and the sheet applies BEFORE load");
}

// ---- scripts ----------------------------------------------------------------------------------

#[tokio::test]
async fn load_waits_for_inserted_scripts_async_or_not() {
    for set_async in ["", "s.async = false;"] {
        let got = case("", &format!("<script>var s = document.createElement('script'); s.src = '/js?ms={SLOW}&n=slow'; {set_async} document.head.appendChild(s);</script>"), 2).await;
        assert_eq!(got, ["ran slow", "load"], "inserted script [{set_async}]");
    }
}

#[tokio::test]
async fn load_waits_for_parser_async_defer_and_module_scripts() {
    for tag in [
        format!("<script async src='/js?ms={SLOW}&n=slow'></script>"),
        format!("<script defer src='/js?ms={SLOW}&n=slow'></script>"),
        format!("<script type=module src='/js?ms={SLOW}&n=slow'></script>"),
        format!("<script>var s = document.createElement('script'); s.type = 'module'; s.src = '/js?ms={SLOW}&n=slow'; document.head.appendChild(s);</script>"),
    ] {
        let got = case("", &tag, 2).await;
        assert_eq!(got, ["ran slow", "load"], "{tag}");
    }
}

#[tokio::test]
async fn load_waits_for_an_inline_module_s_static_import() {
    let got = case("", &format!("<script type=module>import '/js?ms={SLOW}&n=slow'; L('inline module ran');</script>"), 3).await;
    assert_eq!(got, ["ran slow", "inline module ran", "load"]);
}

#[tokio::test]
async fn load_does_not_wait_for_dynamic_import_fetch_or_xhr() {
    let got = case("", &format!("<script>import('/js?ms={SLOW}&n=slow').then(function () {{ L('import resolved'); }});</script>"), 3).await;
    assert_eq!(got, ["load", "ran slow", "import resolved"], "dynamic import()");
    let got = case("", &format!("<script>fetch('/js?ms={SLOW}&n=slow').then(function () {{ L('fetch resolved'); }});</script>"), 2).await;
    assert_eq!(got, ["load", "fetch resolved"], "fetch()");
    let got = case(
        "",
        &format!("<script>var x = new XMLHttpRequest(); x.open('GET', '/js?ms={SLOW}&n=slow'); x.onload = function () {{ L('xhr loaded'); }}; x.send();</script>"),
        2,
    )
    .await;
    assert_eq!(got, ["load", "xhr loaded"], "XMLHttpRequest");
}

#[tokio::test]
async fn load_waits_for_a_script_inserted_later_while_it_is_still_waiting() {
    let got = case(
        "",
        &format!(
            "<script>var s = document.createElement('script'); s.src = '/js?ms=300&n=first';
             s.onload = function () {{ var t = document.createElement('script'); t.src = '/js?ms={SLOW}&n=second'; document.head.appendChild(t); }};
             document.head.appendChild(s);</script>"
        ),
        3,
    )
    .await;
    assert_eq!(got, ["ran first", "ran second", "load"], "a script inserted from another script's onload");
    let got = case(
        "",
        &format!("<script>document.addEventListener('DOMContentLoaded', function () {{ var s = document.createElement('script'); s.src = '/js?ms={SLOW}&n=dcl'; document.head.appendChild(s); }});</script>"),
        2,
    )
    .await;
    assert_eq!(got, ["ran dcl", "load"], "a script inserted from DOMContentLoaded");
}

/// A script a running script inserts FROM A TASK IT QUEUED lands between the first script's release of
/// the last delay and the document's load step -- and Chrome asks the set again when that step's task
/// runs (Blink's zero-delay `load_event_delay_timer_` -> `CheckCompleted`), so the second script delays
/// `load` like any other. A 5 ms timer comes after the step. Chrome 150.0.7871.46, each shape alone:
///
/// ```text
/// inserted from setTimeout(0) / a microtask / a MessageChannel task   ran first · ran second · load
/// inserted from setTimeout(5)                                        ran first · load · ran second
/// ```
///
/// a challenge provider's orchestrator inserts a challenge widget's `api.js` exactly so; Chrome's `api.js` reads
/// `readyState` `interactive`, and korvus's read `complete` -- its load step fired from the task it had
/// queued when the set emptied, without asking again.
#[tokio::test]
async fn load_waits_for_a_script_inserted_from_a_task_queued_before_the_load_step() {
    let head = "<script>function later(how, n) { var go = function () { var t = document.createElement('script'); \
        t.src = '/js?ms=1500&n=' + n; document.head.appendChild(t); }; if (how === 't0') setTimeout(go, 0); \
        else if (how === 't5') setTimeout(go, 5); else if (how === 'p') Promise.resolve().then(go); \
        else { var c = new MessageChannel(); c.port1.onmessage = go; c.port2.postMessage(0); } }</script>";
    for (how, second_first) in [("t0", true), ("p", true), ("mc", true), ("t5", false)] {
        let body = format!("<script>var s = document.createElement('script'); s.src = '/later?how={how}'; document.head.appendChild(s);</script>");
        let got = case(head, &body, 3).await;
        let (first, second) = (format!("ran first-{how}"), format!("ran second-{how}"));
        let want = if second_first { [first, second, "load".to_string()] } else { [first, "load".to_string(), second] };
        assert_eq!(got, want, "a script inserted from a task ({how})");
    }
}

#[tokio::test]
async fn a_removed_inserted_script_still_runs_and_still_delays_load() {
    let got = case("", &format!("<script>var s = document.createElement('script'); s.src = '/js?ms={SLOW}&n=removed'; document.head.appendChild(s); s.remove();</script>"), 2).await;
    assert_eq!(got, ["ran removed", "load"]);
}

/// Insertion order for `async = false`, arrival order for the force-async default, `defer` before
/// `DOMContentLoaded`. The fast default script lands first, the 1000 ms one next, and the two
/// in-order scripts run together once the 1500 ms head of their list arrives.
#[tokio::test]
async fn inserted_script_order_matches_chrome() {
    let got = case(
        "",
        &format!(
            "<script>
            function ins(u, inorder) {{ var s = document.createElement('script'); s.src = u; if (inorder) s.async = false; document.head.appendChild(s); }}
            ins('/js?ms={SLOW}&n=A-slow-asyncfalse', true); ins('/js?ms=0&n=B-fast-asyncfalse', true);
            ins('/js?ms=700&n=C-slow-default'); ins('/js?ms=0&n=D-fast-default');
            </script><script defer src='/js?ms=0&n=E-defer'></script>
            <script>document.addEventListener('DOMContentLoaded', function () {{ L('DCL'); }});</script>"
        ),
        7,
    )
    .await;
    assert_eq!(got, ["ran E-defer", "DCL", "ran D-fast-default", "ran C-slow-default", "ran A-slow-asyncfalse", "ran B-fast-asyncfalse", "load"]);
}

// ---- stylesheets ------------------------------------------------------------------------------

#[tokio::test]
async fn load_waits_for_a_slow_parser_stylesheet_and_its_load_event_comes_first() {
    let got = case(
        "",
        &format!(r#"<div id=p>x</div><link rel=stylesheet href='/css?ms={SLOW}' onload="L('link load sheet=' + !!this.sheet)" onerror="L('link error')"><script>{COLOR}</script>"#),
        2,
    )
    .await;
    assert_eq!(got, ["link load sheet=true", "load; color=rgb(1, 2, 3)"]);
}

#[tokio::test]
async fn an_inserted_stylesheet_has_no_sheet_until_its_load_event() {
    let got = case(
        "",
        &format!(
            "<div id=p>x</div><script>
            var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/css?ms={SLOW}';
            l.onload = function () {{ L('link load sheet=' + !!l.sheet + ' rules=' + (l.sheet && l.sheet.cssRules.length)); }};
            l.onerror = function () {{ L('link error'); }};
            document.head.appendChild(l); L('after append sheet=' + !!l.sheet);
            setTimeout(function () {{ L('later sheet=' + !!l.sheet); }}, 300);
            </script>"
        ),
        4,
    )
    .await;
    assert_eq!(got, ["after append sheet=false", "later sheet=false", "link load sheet=true rules=1", "load"]);
}

/// Chrome 150, an inserted stylesheet answered 404: `error` at the link, with `sheet` NON-null (an
/// empty sheet), then `load`.
#[tokio::test]
async fn a_failed_stylesheet_fires_error_with_a_sheet() {
    let got = case(
        "",
        "<script>var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/missing.css';
         l.onload = function () { L('link load sheet=' + !!l.sheet); };
         l.onerror = function () { L('link error sheet=' + !!l.sheet); };
         document.head.appendChild(l);</script>",
        2,
    )
    .await;
    assert_eq!(got, ["link error sheet=true", "load"]);
}

#[tokio::test]
async fn a_removed_inserted_stylesheet_still_delays_load_but_fires_nothing() {
    let got = case(
        "",
        &format!(
            "<script>var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/css?ms={SLOW}';
             l.onload = function () {{ L('link load'); }}; document.head.appendChild(l); l.remove();
             var s = document.createElement('script'); s.src = '/js?ms=300&n=marker'; document.head.appendChild(s);</script>"
        ),
        2,
    )
    .await;
    assert_eq!(got, ["ran marker", "load"]);
}

#[tokio::test]
async fn load_waits_for_an_import_in_a_style_element_and_in_a_linked_sheet() {
    let got = case("", &format!("<div id=p>x</div><style>@import '/css?ms={SLOW}';</style><script>{COLOR}</script>"), 1).await;
    assert_eq!(got, ["load; color=rgb(1, 2, 3)"], "<style>@import");
    let got = case("", &format!(r#"<div id=p>x</div><link rel=stylesheet href='/importer.css' onload="L('link load')"><script>{COLOR}</script>"#), 2).await;
    assert_eq!(got, ["link load", "load; color=rgb(1, 2, 3)"], "a linked sheet's @import");
}

// ---- images, frames ---------------------------------------------------------------------------

#[tokio::test]
async fn load_waits_for_images_attached_or_not_and_for_an_inserted_frame() {
    for attach in ["document.body.appendChild(i);", ""] {
        let got = case("", &format!("<script>var i = new Image(); i.onload = function () {{ L('img load'); }}; i.src = '/png?ms={SLOW}'; {attach}</script>"), 2).await;
        assert_eq!(got, ["img load", "load"], "image [{attach}]");
    }
    let got = case("", &format!("<script>var f = document.createElement('iframe'); f.onload = function () {{ L('iframe load'); }}; f.src = '/html?ms={SLOW}'; document.body.appendChild(f);</script>"), 2).await;
    assert_eq!(got, ["iframe load", "load"]);
}

// ---- fonts ------------------------------------------------------------------------------------

/// Chrome 150: an `@font-face` an element USES delays `load` until the font's fetch has ended -- its
/// resource-timing entry exists inside the `load` handler; one nothing uses is never fetched and
/// `load` does not wait for it (no entry).
#[tokio::test]
async fn load_waits_for_a_used_web_font_and_not_for_an_unused_one() {
    const ENTRIES: &str = "window.PROBE = function () { return 'font entries=' + performance.getEntriesByType('resource').filter(function (e) { return e.name.indexOf('/font') >= 0; }).length; };";
    let face = format!("<style>@font-face {{ font-family: SlowF; src: url('/font?ms={SLOW}') }}</style>");
    let got = case(&face, &format!("<div style='font-family: SlowF'>hello</div><script>{ENTRIES}</script>"), 1).await;
    assert_eq!(got, ["load; font entries=1"], "a used font");
    let got = case(&face, &format!("<div>hello</div><script>{ENTRIES}</script>"), 1).await;
    assert_eq!(got, ["load; font entries=0"], "an unused font");
}

// ---- a response that has not come ------------------------------------------------------------

/// Chrome: an inserted script whose response has not arrived holds `load` for as long as it has not
/// arrived -- 20 s measured, `readyState` still `interactive`. The engine used to fire `load` at its
/// 1000 ms cap. The pump budget here is deliberately past that old cap: the page is IDLE (nothing to
/// interrupt), so this is also the pump's own path, and it must not release anything.
#[tokio::test]
async fn an_unanswered_script_withholds_load_until_it_answers() {
    let mut p = open("", "<script>var s = document.createElement('script'); s.src = '/held?n=held'; document.head.appendChild(s);</script>").await;
    let _ = p.e.pump(Duration::from_millis(2500)).await;
    assert!(log(&mut p.e).is_empty(), "nothing may happen while the response is outstanding; got {:?}", log(&mut p.e));
    assert!(!p.e.load_fired(), "load must not fire while a delaying fetch is outstanding");
    let ready = p.e.eval_value("p13b-ready", "document.readyState".to_string()).expect("eval");
    assert_eq!(ready.as_str(), Some("interactive"));
    p.gate.add_permits(1);
    let got = until_logged(&mut p.e, 2).await;
    assert_eq!(got, ["ran held", "load"], "once it answers, the script runs and then load fires");
    assert!(p.e.load_fired());
}

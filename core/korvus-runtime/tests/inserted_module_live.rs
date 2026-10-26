//! A `<script type=module>` the PAGE inserted is fetched and evaluated as a MODULE.
//! In a live engine, over loopback.
//!
//! # The defect, measured before the fix
//!
//! On master at `5875efbe`, `s = document.createElement('script'); s.type = 'module';
//! s.src = '/mod.mjs'; head.appendChild(s)` reached the classic compile, so:
//!
//! ```text
//!                                korvus (before)                       Chrome 150.0.7871.46
//! export in the body             uncaught "Unexpected token 'export'"  runs, no error
//! import in the body             uncaught "Cannot use import …"        runs; the import is fetched
//! import.meta                    uncaught "Cannot use 'import.meta' …" the module's own url
//! top-level await                uncaught "await is only valid …"      runs
//! Content-Type text/plain        RAN the body, fired `load`            not run, `error` (strict MIME)
//! no Content-Type                RAN the body, fired `load`            not run, `error`
//! document.currentScript in it   the <script> element                  null
//! the request's Sec-Fetch-Mode   no-cors                               cors
//! type="MODULE" inserted         ran as a CLASSIC script               a module
//! type="MODULE" parser-inserted  fetched, ran NOTHING, fired nothing   a module, `load`
//! an inline inserted module      ran synchronously, before DCL         deferred, after DCL
//! ```
//!
//! # The oracle
//!
//! Every expected sequence below is what stock Chrome 150.0.7871.46 (headless, attached over CDP,
//! launched by the parity driver's own `attach`) recorded for the same markup on the same loopback
//! server on 2026-09-18.
//!
//! One row of that table is NOT closed here: Chrome also sends an `Origin` header with a module's
//! request and korvus sends none, because `korvus-transport` appends a page request's Origin on the
//! response TAINTING rather than on the request MODE. That is one condition, in a file this unit does
//! not own, and it is the same for every cors-mode `fetch()` and XHR -- reported, not touched.
//!
//! `DCL` is filtered out of the comparisons that do not turn on it, exactly as
//! `script_response_live.rs` filters it: when an inserted script's fetch settles relative to the end
//! of the parse is a network race in Chrome too. The two tests that ARE about `DOMContentLoaded` keep
//! it and say so.

use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// How late the "slow" routes answer. Long enough that a fetch which has not been issued yet cannot
/// win the race by accident, short enough to keep the file's whole runtime in seconds.
const SLOW: Duration = Duration::from_millis(600);

/// `(status, Content-Type or none, extra headers, body, how late)` for one path.
fn route(path: &str) -> (u16, Option<&'static str>, Vec<(&'static str, String)>, String, Duration) {
    let js = "text/javascript";
    let now = Duration::ZERO;
    let ok = |body: &str| (200u16, Some(js), Vec::new(), body.to_string(), now);
    match path {
        // A module with an `export`: the line that used to take the page down.
        "/mod.mjs" => ok("export const x = 1;\nL.push('ran:mod');\n"),
        "/mod2.mjs" => ok("export const y = 2;\nL.push('ran:mod2');\n"),
        // A module GRAPH: this one's static import must be fetched and evaluated first.
        "/graph.mjs" => ok("import { d } from './dep.mjs';\nL.push('ran:graph:' + d);\n"),
        "/dep.mjs" => ok("L.push('ran:dep');\nexport const d = 'D';\n"),
        "/meta.mjs" => ok("L.push('meta:' + import.meta.url.split('/').pop());\n"),
        "/tla.mjs" => ok("L.push('tla:before');\nawait new Promise(r => setTimeout(r, 20));\nL.push('tla:after');\nexport {};\n"),
        // The same top-level await, held open long enough that WHERE the element's `load` falls
        // relative to it is a question of hundreds of milliseconds rather than a coin toss -- which is
        // how Chrome was asked it. See `a_top_level_await_module_loads_at_its_first_await`.
        "/tla-slow.mjs" => ok("L.push('tla:before');\nawait new Promise(r => setTimeout(r, 400));\nL.push('tla:after');\nexport {};\n"),
        "/throws.mjs" => ok("L.push('ran:throws');\nthrow new TypeError('boom');\n"),
        // A module that inserts a module.
        "/nested.mjs" => ok(
            "L.push('ran:nested-outer');\nvar s = document.createElement('script'); s.type = 'module'; s.src = '/mod2.mjs';\ns.onload = function () { L.push('onload:inner') };\ndocument.head.appendChild(s);\n",
        ),
        "/cs.mjs" => ok("L.push('cs:' + String(document.currentScript));\n"),
        "/classic.js" => ok("L.push('ran:classic');\n"),
        "/b.mjs" => ok("L.push('ran:b');\nexport {};\n"),
        // The slow ones.
        "/slow-classic.js" => (200, Some(js), Vec::new(), "L.push('ran:slow-classic');\n".into(), SLOW),
        "/slow-mod.mjs" => (200, Some(js), Vec::new(), "L.push('ran:slow-mod');\nexport {};\n".into(), SLOW),
        "/a.mjs" => (200, Some(js), Vec::new(), "L.push('ran:a');\nexport {};\n".into(), SLOW),
        // A JavaScript MIME type under `nosniff` is fine for a module -- the strict check is about the
        // TYPE, not about sniffing.
        "/nosniff.mjs" => (200, Some("application/javascript"), vec![("X-Content-Type-Options", "nosniff".into())], "export const z = 1;\nL.push('ran:nosniff');\n".into(), now),
        // The three responses a module refuses.
        "/plain.mjs" => (200, Some("text/plain"), Vec::new(), "export const x = 1;\nL.push('ran:plain');\n".into(), now),
        "/nomime.mjs" => (200, None, Vec::new(), "export const x = 1;\nL.push('ran:nomime');\n".into(), now),
        "/404.mjs" => (404, Some(js), Vec::new(), "L.push('ran:404');\n".into(), now),
        _ => (404, Some("text/plain"), Vec::new(), "no".into(), now),
    }
}

/// The server, plus the ledger of what the page actually requested and in which Fetch MODE -- which
/// is the axis-B half of this unit: a module script is a CORS request in Chrome, and korvus sent
/// `no-cors`. The third field is whether an `Origin` header came with it -- carried, not asserted;
/// see the file header.
type Ledger = std::sync::Arc<std::sync::Mutex<Vec<(String, String, bool)>>>;

async fn serve(listener: TcpListener, page: String, ledger: Ledger) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        let ledger = ledger.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let header = |name: &str| -> Option<String> {
                req.lines()
                    .find(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
                    .map(|l| l[name.len() + 1..].trim().to_string())
            };
            if path != "/page.html" {
                let mode = header("sec-fetch-mode").unwrap_or_default();
                let origin = header("origin").is_some();
                ledger.lock().unwrap().push((path.clone(), mode, origin));
            }
            let (status, ctype, extra, body, late) =
                if path == "/page.html" { (200, Some("text/html"), Vec::new(), page, Duration::ZERO) } else { route(&path) };
            if !late.is_zero() {
                tokio::time::sleep(late).await;
            }
            let mut head = format!("HTTP/1.1 {status} X\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
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

/// The recorder every page starts with. A CAPTURING `error` listener on the window sees BOTH an
/// element's `error` (target SCRIPT -- a refused module) and an uncaught exception (target window),
/// which is how a refusal and a throw are told apart; `ins` is the insertion every fixture uses.
const HEAD: &str = r#"<script>var L=[];
addEventListener('error',function(e){L.push('winerr:'+(e.target===window?'win':e.target.tagName))},true);
document.addEventListener('DOMContentLoaded',function(){L.push('DCL')});addEventListener('load',function(){L.push('load')});
function ins(tag,o){var s=document.createElement('script');
 for(var k in o){if(k==='text'){s.textContent=o[k]}else if(k==='attr'){for(var a in o.attr)s.setAttribute(a,o.attr[a])}else{s[k]=o[k]}}
 s.onload=function(){L.push('onload:'+tag)};s.onerror=function(){L.push('onerror:'+tag)};
 document.head.appendChild(s);return s}
</script>"#;

/// Load `body` (after [`HEAD`]) from a loopback origin, let it settle, and return the page's `L`, the
/// engine's uncaught errors, and the request ledger.
async fn run_page(body: &str) -> (Vec<String>, Vec<String>, Vec<(String, String, bool)>) {
    let doc = format!("<!doctype html><html><head>{HEAD}</head><body>{body}</body></html>");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ledger: Ledger = Default::default();
    tokio::spawn(serve(listener, doc.clone(), ledger.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    // A settle long enough for the slowest route (600 ms) plus its module drain.
    let until = tokio::time::Instant::now() + Duration::from_millis(1500);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(60), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("p15c", "print(JSON.stringify(L))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let log: Vec<String> = serde_json::from_str(&out[0]).expect("L is JSON");
    let reqs = ledger.lock().unwrap().clone();
    (log, e.uncaught_errors(), reqs)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The log with `DCL` removed -- see the file header.
fn no_dcl(log: Vec<String>) -> Vec<String> {
    log.into_iter().filter(|x| x != "DCL").collect()
}

// ---------------------------------------------------------------------------------------------
// 1. THE FIXTURE FROM THE BRIEF, and the module semantics that hang off it.
// ---------------------------------------------------------------------------------------------

/// L22's C02 verbatim. Chrome: the body runs and the element fires `load`, with nothing uncaught.
#[tokio::test]
async fn an_inserted_module_runs_and_fires_load() {
    let (log, uncaught, reqs) = run_page("<script>ins('mod',{type:'module',src:'/mod.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:mod", "onload:mod", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
    // Axis B: Chrome sends a module script's request in CORS MODE whether or not the element carries
    // `crossorigin`, where korvus sent `no-cors`.
    //
    // THE `Origin` HEADER IS NOT ASSERTED, and the third field is carried so the gap is visible
    // rather than forgotten: Chrome 150 sends `Origin: http://127.0.0.1:<port>` with this request
    // and korvus sends none, because `korvus-transport` appends a page request's Origin when the
    // response TAINTING is `Cors` rather than when the request MODE is -- and a same-origin CORS
    // request is `Basic`. That is one condition in `plugins/korvus-transport/src/lib.rs`, it is the
    // same for every `fetch()` and XHR in cors mode (the module loader's imports included, which is
    // the parser-inserted path), and it is outside this unit's files.
    assert_eq!(reqs.iter().find(|r| r.0 == "/mod.mjs").map(|r| r.1.as_str()), Some("cors"), "{reqs:?}");
}

/// The three module things a classic compile cannot do. Each is its own page, because each one used
/// to be a SyntaxError that ended the page's script.
#[tokio::test]
async fn an_inserted_module_has_a_graph_an_import_meta_and_top_level_await() {
    let (log, uncaught, reqs) = run_page("<script>ins('graph',{type:'module',src:'/graph.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:dep", "ran:graph:D", "onload:graph", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
    // The import is a request, and it is a CORS one too.
    assert_eq!(reqs.iter().find(|r| r.0 == "/dep.mjs").map(|r| r.1.as_str()), Some("cors"), "{reqs:?}");

    let (log, uncaught, _) = run_page("<script>ins('meta',{type:'module',src:'/meta.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["meta:meta.mjs", "onload:meta", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");

    // Top-level await: the body runs, the element loads AT THE FIRST AWAIT, and the rest of the body
    // runs after. The full ordering is [`a_top_level_await_module_loads_at_its_first_await`]; this row
    // is here because a module with top-level await is one of the three things a classic compile
    // cannot do, and this is the file's list of those three.
    let (log, uncaught, _) = run_page("<script>ins('tla',{type:'module',src:'/tla.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["tla:before", "onload:tla", "load", "tla:after"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

/// **WHERE A TOP-LEVEL AWAIT MODULE'S ELEMENT FIRES `load`.** At the FIRST AWAIT, with
/// the rest of the module still to run — not when its evaluation promise settles.
///
/// Chrome 150.0.7871.46, headless, over loopback, 2026-09-21, for a module whose body is
/// `P('tla:before'); await new Promise(r => setTimeout(r, 400)); P('tla:after');` and a page that
/// timestamps every marker with `performance.now()`:
///
/// ```text
///   inserted from script   DCL @1.8   tla:before @8.0   onload:tla @8.2   doc:load @8.2   tla:after @411.7
///   parser-inserted        tla:before @0.7   onload:ptla @1.8   DCL @1.9   doc:load @2.6   tla:after @403.2
///   inline (no element)    tla:before @3.6   DCL @3.8   doc:load @4.0   tla:after @406.6
/// ```
///
/// Three readings, one rule: both `load`s — the element's and the DOCUMENT's — land at the first
/// await, 400 ms before the module finishes, on the inserted path and the parser-inserted path
/// alike, and an inline module still fires no element event at all. korvus fired both once the
/// evaluation promise SETTLED, which put them after `tla:after`, and because the drain had already
/// taken the module off its queue a caller whose future was dropped in that window (the harness's
/// own `timeout(_, engine.run())`, `Engine::pump`'s budget, CDP's `Page.navigate`) lost the
/// element's `load` outright — the intermittent `onload:tla`, at 30–60 %, that this closes.
#[tokio::test]
async fn a_top_level_await_module_loads_at_its_first_await() {
    // Inserted from script. `DCL` is kept: Chrome's falls before the fetch settles here and after it
    // on the parser path, and both are part of the reading.
    let (log, uncaught, _) = run_page("<script>ins('tla',{type:'module',src:'/tla-slow.mjs'})</script>").await;
    assert_eq!(log, strs(&["DCL", "tla:before", "onload:tla", "load", "tla:after"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");

    // Parser-inserted: the SAME drain, so the same rule. This is the second path the old behaviour
    // was shared with, and the reason the fix is in the drain rather than beside one queue's producer.
    let body = r#"<script type=module src="/tla-slow.mjs" onload="L.push('onload:ptla')"></script><script>L.push('after-parser-tag')</script>"#;
    let (log, uncaught, _) = run_page(body).await;
    assert_eq!(log, strs(&["after-parser-tag", "tla:before", "onload:ptla", "DCL", "load", "tla:after"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

/// A module inserted BY a module, `nomodule` (ignored for a module script), `defer` (ignored too),
/// and `document.currentScript` inside a module, which is null in Chrome and was the element here.
#[tokio::test]
async fn a_module_nests_ignores_nomodule_and_defer_and_has_no_current_script() {
    let (log, uncaught, _) = run_page("<script>ins('nested',{type:'module',src:'/nested.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:nested-outer", "onload:nested", "ran:mod2", "onload:inner", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");

    let (log, _, _) = run_page("<script>ins('nm',{type:'module',src:'/mod.mjs',noModule:true})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:mod", "onload:nm", "load"]));

    let (log, _, _) = run_page("<script>ins('dm',{type:'module',src:'/mod.mjs',defer:true})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:mod", "onload:dm", "load"]));

    let (log, _, _) = run_page("<script>ins('cs',{type:'module',src:'/cs.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["cs:null", "onload:cs", "load"]));
}

/// A module that throws at top level: the throw is reported as the page error it is, and the element
/// still LOADS -- a module that threw still loaded. (Chrome reports the throw BEFORE firing `load`;
/// this drain reports it after, on the parser-inserted path as well -- pre-existing, and why the
/// order of those two is not asserted here.)
#[tokio::test]
async fn a_module_that_throws_is_reported_and_still_loads() {
    let (log, uncaught, _) = run_page("<script>ins('throws',{type:'module',src:'/throws.mjs'})</script>").await;
    let log = no_dcl(log);
    assert_eq!(log.first().map(String::as_str), Some("ran:throws"), "{log:?}");
    assert!(log.contains(&"onload:throws".to_string()), "{log:?}");
    assert!(log.contains(&"winerr:win".to_string()), "{log:?}");
    assert_eq!(uncaught.len(), 1, "{uncaught:?}");
    assert!(uncaught[0].contains("TypeError: boom"), "{uncaught:?}");
}

// ---------------------------------------------------------------------------------------------
// 2. THE RESPONSE: a module's MIME check is the strict one.
// ---------------------------------------------------------------------------------------------

/// Chrome 150, measured: an inserted module served `text/plain`, or with no `Content-Type` at all,
/// or 404, is NOT run and its element fires `error` -- and none of the three is an uncaught error,
/// only the element's event. A JavaScript MIME type under `nosniff` runs.
#[tokio::test]
async fn an_inserted_module_is_checked_by_the_module_mime_rules() {
    for (path, tag) in [("/plain.mjs", "plain"), ("/nomime.mjs", "nomime"), ("/404.mjs", "s404")] {
        let (log, uncaught, _) = run_page(&format!("<script>ins('{tag}',{{type:'module',src:'{path}'}})</script>")).await;
        assert_eq!(no_dcl(log), strs(&["winerr:SCRIPT", &format!("onerror:{tag}"), "load"]), "{path}");
        assert!(uncaught.is_empty(), "{path}: {uncaught:?}");
    }
    let (log, uncaught, _) = run_page("<script>ins('ns',{type:'module',src:'/nosniff.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:nosniff", "onload:ns", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

// ---------------------------------------------------------------------------------------------
// 3. ORDER -- against a classic script inserted beside it, and against another module.
// ---------------------------------------------------------------------------------------------

/// Chrome 150, measured, both elements inserted in one task and both force-async (`async` untouched):
///
/// ```text
/// slow classic + fast module            ran:mod, ran:slow-classic          (arrival order)
/// fast classic + slow module            ran:classic, ran:slow-mod
/// both fast, the MODULE inserted first  ran:classic, ran:mod
/// ```
///
/// The third row is the one worth having: a classic script runs INSIDE its own fetch completion, a
/// module runs in the next turn of the loop, so a classic script that arrives at the same moment
/// wins even though it was inserted second.
#[tokio::test]
async fn an_inserted_module_runs_beside_a_classic_script_in_chrome_s_order() {
    let (log, _, _) = run_page("<script>ins('sc',{src:'/slow-classic.js'});ins('m',{type:'module',src:'/mod.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:mod", "onload:m", "ran:slow-classic", "onload:sc", "load"]));

    let (log, _, _) = run_page("<script>ins('sm',{type:'module',src:'/slow-mod.mjs'});ins('c',{src:'/classic.js'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:classic", "onload:c", "ran:slow-mod", "onload:sm", "load"]));

    let (log, _, _) = run_page("<script>ins('m',{type:'module',src:'/mod.mjs'});ins('c',{src:'/classic.js'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:classic", "onload:c", "ran:mod", "onload:m", "load"]));
}

/// Two modules: force-async ones run as they ARRIVE, and ones whose `async` was set false BEFORE
/// insertion run in INSERTION order however late the first one is. Chrome 150, measured, with
/// `/a.mjs` served 600 ms late: `ran:b, ran:a` and `ran:a, ran:b` respectively.
#[tokio::test]
async fn two_inserted_modules_follow_the_async_flag_they_were_prepared_with() {
    let (log, _, _) = run_page("<script>ins('a',{type:'module',src:'/a.mjs'});ins('b',{type:'module',src:'/b.mjs'})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:b", "onload:b", "ran:a", "onload:a", "load"]));

    let body = r#"<script>
      function insno(tag, src) { var s = document.createElement('script'); s.type = 'module'; s.async = false; s.src = src;
        s.onload = function () { L.push('onload:' + tag) }; document.head.appendChild(s); return s }
      insno('a', '/a.mjs'); insno('b', '/b.mjs');
    </script>"#;
    let (log, _, _) = run_page(body).await;
    assert_eq!(no_dcl(log), strs(&["ran:a", "onload:a", "ran:b", "onload:b", "load"]));
}

// ---------------------------------------------------------------------------------------------
// 4. WHEN -- `DOMContentLoaded` and `load`. These two keep `DCL`, because they are about it.
// ---------------------------------------------------------------------------------------------

/// A module the PARSER found is deferred to the end of the parse and runs BEFORE
/// `DOMContentLoaded`; one the page inserted is a task of its own and runs AFTER it. Chrome 150,
/// measured: `ran:parser-module, DCL, ran:mod` for the two in one document, and `after-insert, DCL,
/// ran:inline` for an inline module inserted by a parser-inserted script.
#[tokio::test]
async fn a_parser_module_runs_before_dom_content_loaded_and_an_inserted_one_after() {
    let body = "<script type=module>L.push('ran:parser-module')</script><script>ins('mod',{type:'module',src:'/mod.mjs'})</script>";
    let (log, _, _) = run_page(body).await;
    assert_eq!(log, strs(&["ran:parser-module", "DCL", "ran:mod", "onload:mod", "load"]));

    let body = "<script>ins('i',{type:'module',text:\"L.push('ran:inline')\"});L.push('after-insert')</script>";
    let (log, uncaught, _) = run_page(body).await;
    // An INLINE module fires neither `load` nor `error` -- in Chrome or in the spec -- so `onload:i`
    // is absent from this sequence by design.
    assert_eq!(log, strs(&["after-insert", "DCL", "ran:inline", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

/// An inline inserted module is a real module: its `import` is fetched and evaluated. (Before, its
/// first `import` was an uncaught "Cannot use import statement outside a module".)
#[tokio::test]
async fn an_inline_inserted_module_imports() {
    let body = "<script>ins('i',{type:'module',text:\"import {d} from '/dep.mjs'; L.push('ran:inline-import:'+d);\"})</script>";
    let (log, uncaught, reqs) = run_page(body).await;
    assert_eq!(no_dcl(log), strs(&["ran:dep", "ran:inline-import:D", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
    assert!(reqs.iter().any(|r| r.0 == "/dep.mjs"), "{reqs:?}");
}

/// The document's `load` waits for the module to RUN, not for its bytes to arrive. Chrome 150 with a
/// module served 600 ms late: `ran:slow-mod`, the element's `load`, then the document's `load`.
#[tokio::test]
async fn the_document_s_load_waits_for_an_inserted_module_to_evaluate() {
    let (log, _, _) = run_page("<script>ins('sm',{type:'module',src:'/slow-mod.mjs'})</script>").await;
    assert_eq!(log, strs(&["DCL", "ran:slow-mod", "onload:sm", "load"]));
}

// ---------------------------------------------------------------------------------------------
// 5. The type attribute.
// ---------------------------------------------------------------------------------------------

/// HTML matches the module type ASCII-case-INSENSITIVELY, and Chrome 150 does: `type="MODULE"` is a
/// module both inserted from script and parser-inserted. korvus ran the inserted one as a CLASSIC
/// script ("Unexpected token 'export'") and, on the parser path, fetched the body, ran nothing and
/// fired no event at all.
#[tokio::test]
async fn the_module_type_is_matched_case_insensitively() {
    let (log, uncaught, _) = run_page("<script>ins('up',{src:'/mod.mjs',attr:{type:'MODULE'}})</script>").await;
    assert_eq!(no_dcl(log), strs(&["ran:mod", "onload:up", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");

    let body = r#"<script type="MODULE" src="/mod.mjs" onload="L.push('onload:pu')"></script>"#;
    let (log, uncaught, _) = run_page(body).await;
    assert_eq!(log, strs(&["ran:mod", "onload:pu", "DCL", "load"]));
    assert!(uncaught.is_empty(), "{uncaught:?}");
}

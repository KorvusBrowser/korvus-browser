//! The script a module-supporting browser never REQUESTS, and the task a settled
//! `<script src>` executes from.
//!
//! # Row 1: `nomodule` is decided before the fetch
//!
//! HTML "prepare the script element": *If el has a `nomodule` content attribute and its type is
//! "classic", then return. (No script is executed.)* That step is upstream of "fetch a classic
//! script", so a browser that supports module scripts does not put the legacy fallback on the wire at
//! all. korvus fetched it **and ran it** — a request no modern browser makes, visible in a site's own
//! access log: a client that fetched the module graph *and* the legacy fallback is a browser that does
//! not exist.
//!
//! ## The oracle, read off the WIRE
//!
//! Stock Chrome 150.0.7871.46, one fixture served from loopback, every external script at a URL of its
//! own so "requested" is the server's log and not an inference; the page beacons its own execution log
//! back through a second request:
//!
//! ```text
//!                                                  requested?   ran?     korvus before
//!   <script src nomodule>                              no        no      requested + ran
//!   <script src nomodule="nomodule">                   no        no      requested + ran
//!   <script src nomodule="false">                      no        no      requested + ran
//!   <script src type="text/javascript" nomodule>       no        no      requested + ran
//!   <script nomodule>inline</script>                   -         no      ran
//!   <script src>                        (the control)  YES       YES     agreed
//!   <script type=module src nomodule>                  YES       YES     agreed
//!   <script type=module nomodule>inline</script>       -         YES     agreed
//!   inserted, `s.noModule = true` BEFORE appendChild   no        no      requested + ran
//!   inserted, `s.noModule = true` AFTER  appendChild   YES       YES     agreed
//! ```
//!
//! Three rules fall out, and each has a row here that would fail without it: the attribute's
//! **presence** decides and its value is never read; it applies only to a **classic** script; and it is
//! read when the element is **prepared**, which is why the last two rows differ.
//!
//! **THE SIZE OF IT.** 85 of the 504 recorded corpus rows carry a classic `<script nomodule>`, 82 of
//! them an external one, for 100 requests korvus made that Chrome does not — counted by scanning every
//! HTML response body in every recording's flow.
//!
//! # Row 2: an inserted external script runs from a TASK
//!
//! HTML: *queue an element task on the networking task source given el to run these steps: execute the
//! script element.* korvus reached it from the fetch promise's **continuation** instead — inside
//! deno_core's own microtask drain, where V8 correctly makes a nested `PerformCheckpoint` a no-op. So
//! FP13's "clean up after running script" did nothing for this one shape and the script's own
//! microtasks ran after `document.currentScript` had been put back. Chrome 150 vs korvus, one
//! `<script src>` appended from script, the body reading `document.currentScript` twice:
//!
//! ```text
//!                    while it runs   in the microtask it queued   in a setTimeout(0) it queued
//!   chrome 150       the element     the element                  null
//!   korvus before    the element     null                         null
//! ```
//!
//! The fix is the task source, not the checkpoint: the settled script is parked and the HOST runs it
//! from Rust, between turns of the event loop, which is the one place the JavaScript execution context
//! stack is genuinely empty.
//!
//! # What makes these rules and not literals
//!
//! Every assertion is an equation between two readings that differ in ONE thing. A `nomodule` script
//! is compared to its own twin — the identical body, the identical shape, the attribute the only
//! difference — so no row can pass by naming a URL. The IDL row asserts `el.noModule ===
//! el.hasAttribute("nomodule")` over EVERY script in the document rather than four literals, which is
//! the reflection itself. And row 2 asserts `micro == sync` for the script, with the `setTimeout(0)`
//! column as the control that keeps "the checkpoint happens first" apart from "currentScript is never
//! cleared".

use engine::Engine;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every external script under `/js/` has the SAME body under a different tag, so a row that runs
/// cannot be running for a reason another row does not have.
fn body_for(tag: &str) -> String {
    format!("L.push('ran:{tag}');")
}

/// What the server was asked for, in arrival order. This is the axis-B reading: a path in here is a
/// request this engine put on the wire.
type Log = Arc<Mutex<Vec<String>>>;

async fn serve(listener: TcpListener, page: String, hits: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            hits.lock().unwrap().push(path.clone());
            let tag = path.rsplit('/').next().unwrap_or("x").to_string();
            let (ctype, body) = if path == "/page.html" {
                ("text/html", page)
            } else {
                ("text/javascript", body_for(&tag))
            };
            let head = format!(
                "HTTP/1.1 200 X\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

const HEAD: &str = r#"<script>var L=[];
var W={id:function(v){return (v===null||v===undefined)?'null':(v.id||v.tagName)}};</script>"#;

/// Load one page from loopback, let it settle, and return (the page's `L`, the server's request log).
async fn run_page(body: &str) -> (Vec<String>, Vec<String>) {
    let doc = format!("<!doctype html><html><head>{HEAD}</head><body>{body}</body></html>");
    let hits: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, doc.clone(), hits.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("fp17", "print(JSON.stringify(L))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let log: Vec<String> = serde_json::from_str(&out[0]).expect("L is JSON");
    let seen = hits.lock().unwrap().clone();
    (log, seen)
}

/// Did this engine REQUEST the script named by `tag`? Read off the server's log, never inferred from
/// whether the script ran -- the two are separate readings and row 1 needs both.
fn requested(hits: &[String], tag: &str) -> bool {
    hits.iter().any(|h| h == &format!("/js/{tag}"))
}

/// Did the script named by `tag` RUN?
fn ran(log: &[String], tag: &str) -> bool {
    log.iter().any(|l| l == &format!("ran:{tag}"))
}

/// The reading `tag` made at `when`, as the log recorded it.
fn read(log: &[String], tag: &str, when: &str) -> String {
    let want = format!("{tag}:{when}=");
    log.iter()
        .find_map(|l| l.strip_prefix(&want))
        .unwrap_or_else(|| panic!("no {tag}:{when} reading in {log:?}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// 1. A classic `nomodule` script is neither requested nor run -- and its TWIN is both.
// ---------------------------------------------------------------------------------------------

/// Four spellings of the attribute, each against a twin that differs in nothing else. The assertion
/// is the pair, so "no request went out" can never pass because the page failed to load: the twin's
/// request is the proof that the same shape, without the attribute, is fetched.
///
/// `nomodule=""`, `nomodule="nomodule"` and `nomodule="false"` are all here because the VALUE is never
/// read -- it is a boolean content attribute, and a rule that parsed it would let `="false"` through.
#[tokio::test]
async fn a_classic_nomodule_script_is_neither_requested_nor_run_and_its_twin_is_both() {
    let body = "\
        <script src=\"/js/bare\" nomodule></script>      <script src=\"/js/bareT\"></script>\
        <script src=\"/js/word\" nomodule=\"nomodule\"></script> <script src=\"/js/wordT\"></script>\
        <script src=\"/js/fals\" nomodule=\"false\"></script>    <script src=\"/js/falsT\"></script>\
        <script src=\"/js/typd\" type=\"text/javascript\" nomodule></script> \
        <script src=\"/js/typdT\" type=\"text/javascript\"></script>";
    let (log, hits) = run_page(body).await;
    for (skipped, twin) in [("bare", "bareT"), ("word", "wordT"), ("fals", "falsT"), ("typd", "typdT")] {
        assert!(requested(&hits, twin), "the twin of {skipped} must be fetched, or the pair proves nothing: {hits:?}");
        assert!(ran(&log, twin), "the twin of {skipped} must run: {log:?}");
        assert!(!requested(&hits, skipped), "{skipped} was REQUESTED; a module-supporting browser asks for no such script: {hits:?}");
        assert!(!ran(&log, skipped), "{skipped} RAN: {log:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// 2. ...only a CLASSIC one. A module script carrying the attribute is fetched and run.
// ---------------------------------------------------------------------------------------------

/// The clause that keeps the rule apart from "the attribute suppresses the script". `nomodule` is the
/// fallback FOR browsers without modules; on a module script it means nothing, and Chrome fetches and
/// runs it. Asserted as an equation between the module twins -- the attribute must make NO difference
/// there -- so a fix that keyed on the attribute alone fails this row.
#[tokio::test]
async fn the_attribute_changes_nothing_on_a_module_script() {
    let body = "\
        <script type=\"module\" src=\"/js/mnm\" nomodule></script>\
        <script type=\"module\" src=\"/js/mctl\"></script>\
        <script type=\"module\" nomodule>L.push('ran:imn');</script>\
        <script type=\"module\">L.push('ran:ictl');</script>";
    let (log, hits) = run_page(body).await;
    assert!(requested(&hits, "mctl"), "the module control must be fetched: {hits:?}");
    assert_eq!(
        requested(&hits, "mnm"),
        requested(&hits, "mctl"),
        "`nomodule` on a MODULE script must not change whether it is requested: {hits:?}"
    );
    assert!(ran(&log, "ictl"), "the inline module control must run: {log:?}");
    assert_eq!(ran(&log, "mnm"), ran(&log, "mctl"), "nor whether an external module runs: {log:?}");
    assert_eq!(ran(&log, "imn"), ran(&log, "ictl"), "nor whether an inline module runs: {log:?}");
}

// ---------------------------------------------------------------------------------------------
// 3. An inline classic script carrying it does not run -- and its twin does.
// ---------------------------------------------------------------------------------------------

/// No request is involved, so this is the execution half of the rule on its own: a `nomodule` inline
/// script is skipped by "prepare the script element" exactly as an external one is.
#[tokio::test]
async fn an_inline_classic_nomodule_script_does_not_run_and_its_twin_does() {
    let body = "<script nomodule>L.push('ran:inm');</script><script>L.push('ran:ictl');</script>";
    let (log, _) = run_page(body).await;
    assert!(ran(&log, "ictl"), "the twin must run, or this row proves nothing: {log:?}");
    assert!(!ran(&log, "inm"), "an inline `nomodule` script ran: {log:?}");
}

// ---------------------------------------------------------------------------------------------
// 4. The attribute is read when the element is PREPARED, not whenever it is set.
// ---------------------------------------------------------------------------------------------

/// The row that distinguishes "the attribute suppresses the script" from HTML's actual rule, which is
/// a step of "prepare the script element". `noModule` set BEFORE `appendChild` suppresses the request;
/// the SAME assignment made one statement after the insertion does not, because by then the element
/// has been prepared and its fetch is already away. Both elements report `noModule === true` at the
/// end, so the reading that differs is WHEN, not WHAT -- which is what the two assertions pair up.
#[tokio::test]
async fn nomodule_is_read_when_the_element_is_prepared_and_not_after() {
    let body = "<script>\
        var before=document.createElement('script'); before.noModule=true; before.src='/js/before';\
        document.head.appendChild(before);\
        var after=document.createElement('script'); after.src='/js/after';\
        document.head.appendChild(after); after.noModule=true;\
        L.push('idl:before=' + before.noModule + ',after=' + after.noModule);\
        </script>";
    let (log, hits) = run_page(body).await;
    assert_eq!(
        read(&log, "idl", "before"),
        "true,after=true",
        "both elements must end up carrying the attribute, or the pair differs in more than timing: {log:?}"
    );
    assert!(requested(&hits, "after"), "set AFTER the insertion, the fetch was already away: {hits:?}");
    assert!(ran(&log, "after"), "and the script runs: {log:?}");
    assert!(!requested(&hits, "before"), "set BEFORE the insertion, no request may go out: {hits:?}");
    assert!(!ran(&log, "before"), "and nothing may run: {log:?}");
}

// ---------------------------------------------------------------------------------------------
// 5. `noModule` is a boolean reflection -- asserted as the reflection, over every script there is.
// ---------------------------------------------------------------------------------------------

/// Not four literals: the property IS `hasAttribute("nomodule")`, so that equation is asserted for
/// EVERY `<script>` in the document, whatever spelling it carries. The setter half round-trips through
/// the same equation -- set true, the attribute exists; set false, it does not -- which is the whole
/// of "reflected boolean" and stays true if the page grows another spelling.
#[tokio::test]
async fn the_nomodule_idl_property_is_the_content_attributes_presence() {
    let body = "\
        <script nomodule></script><script nomodule=\"nomodule\"></script>\
        <script nomodule=\"false\"></script><script src=\"/js/plain\"></script>\
        <script>\
        var all=document.getElementsByTagName('script');\
        var bad=[];\
        for (var i=0;i<all.length;i++) { if (all[i].noModule !== all[i].hasAttribute('nomodule')) bad.push(i); }\
        L.push('idl:reflects=' + (bad.length===0) + ',of=' + all.length);\
        var t=document.createElement('script');\
        L.push('idl:fresh=' + (t.noModule === t.hasAttribute('nomodule')) + ',v=' + t.noModule);\
        t.noModule = true;\
        L.push('idl:on=' + (t.noModule === t.hasAttribute('nomodule')) + ',v=' + t.noModule);\
        t.noModule = false;\
        L.push('idl:off=' + (t.noModule === t.hasAttribute('nomodule')) + ',v=' + t.noModule);\
        </script>";
    let (log, _) = run_page(body).await;
    let (reflects, of) = read(&log, "idl", "reflects").split_once(",of=").map(|(a, b)| (a.to_string(), b.to_string())).unwrap();
    assert!(of.parse::<u32>().unwrap() >= 5, "the document must hold the scripts this asserts over: {log:?}");
    assert_eq!(reflects, "true", "`noModule` is not `hasAttribute(\"nomodule\")` on every script: {log:?}");
    assert_eq!(read(&log, "idl", "fresh"), "true,v=false", "{log:?}");
    assert_eq!(read(&log, "idl", "on"), "true,v=true", "setting it true must add the attribute: {log:?}");
    assert_eq!(read(&log, "idl", "off"), "true,v=false", "setting it false must remove it: {log:?}");
}

// ---------------------------------------------------------------------------------------------
// 6. The whole request set, derived from the rule rather than listed.
// ---------------------------------------------------------------------------------------------

/// The axis-B statement itself: over a page mixing every shape, the set of `/js/` paths this engine
/// asked for is exactly the set whose element the rule does NOT skip. Written as a set equation so a
/// later shape added to the fixture has to be classified rather than appended to an expected list.
#[tokio::test]
async fn the_request_set_is_exactly_the_scripts_the_rule_does_not_skip() {
    let body = "\
        <script src=\"/js/c1\" nomodule></script>\
        <script src=\"/js/c2\"></script>\
        <script src=\"/js/c3\" type=\"text/javascript\" nomodule></script>\
        <script src=\"/js/m1\" type=\"module\" nomodule></script>\
        <script src=\"/js/m2\" type=\"module\"></script>";
    let (_, hits) = run_page(body).await;
    // The rule: a CLASSIC script carrying the attribute is skipped; everything else is fetched. The
    // fixture's names carry their own classification, so this list is derived and not transcribed.
    let want: BTreeSet<&str> = ["c2", "m1", "m2"].into_iter().collect();
    let got: BTreeSet<&str> = ["c1", "c2", "c3", "m1", "m2"].into_iter().filter(|t| requested(&hits, t)).collect();
    assert_eq!(got, want, "the request set is not the one the rule describes: {hits:?}");
}

// ---------------------------------------------------------------------------------------------
// 7. ROW 2 -- an inserted EXTERNAL script's own microtask reads the script that queued it.
// ---------------------------------------------------------------------------------------------

/// FP13's equation, on the one shape it could not close: `micro == sync`. The script is fetched, and
/// is therefore reached from the fetch's completion — which is where korvus used to execute it, inside
/// deno_core's microtask drain, where a checkpoint is a no-op. Asserted against ITSELF, so no element
/// or page is named, and the request log is checked first so that an `ext` that never arrived cannot
/// pass this by leaving both readings equal and absent.
///
/// FP13's own test 5 is the other side of the same clause and must keep passing: a script inserted and
/// run from inside another performs NO checkpoint, because the outer script is still on the stack. A
/// fix that simply ran everything from a task would flatten the two and fail there.
#[tokio::test]
async fn an_inserted_external_scripts_own_microtask_reads_the_script_that_queued_it() {
    let (log, hits) = run_page_cs().await;
    assert!(requested(&hits, "cs"), "the inserted script must actually have been fetched: {hits:?}");
    let sync = read(&log, "ext", "sync");
    assert_ne!(sync, "null", "the inserted script never became document.currentScript: {log:?}");
    assert_eq!(
        read(&log, "ext", "micro"),
        sync,
        "an inserted external script's own microtask must read the script that queued it: {log:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 8. ...and the control: a TASK it queued is outside the script and reads nothing.
// ---------------------------------------------------------------------------------------------

/// Without this, "the script runs from a task, so its checkpoint is real" and "`currentScript` is
/// simply never cleared on this path" are the same test. A `setTimeout(0)` callback runs on a LATER
/// task, after the restore — Chrome 150: `null`.
#[tokio::test]
async fn a_task_queued_by_an_inserted_external_script_sees_no_running_script() {
    let (log, _) = run_page_cs().await;
    assert_eq!(read(&log, "ext", "task"), "null", "a task is outside the script: {log:?}");
}

/// The row-2 fixture: an inserted EXTERNAL script whose body reads `document.currentScript` three
/// times — while it runs, from the microtask it queues, and from the task it queues. One body, three
/// readings, so the assertions are equations between them.
async fn run_page_cs() -> (Vec<String>, Vec<String>) {
    let doc = format!(
        "<!doctype html><html><head>{HEAD}</head><body>\
         <script id=\"host\">var s=document.createElement('script');s.id='ext';s.src='/cs';\
         document.head.appendChild(s);</script></body></html>"
    );
    let hits: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let page = doc.clone();
    let h2 = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let page = page.clone();
            let h2 = h2.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let Ok(n) = s.read(&mut buf).await else { return };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                h2.lock().unwrap().push(if path == "/cs" { "/js/cs".to_string() } else { path.clone() });
                let (ctype, body) = if path == "/page.html" {
                    ("text/html", page)
                } else {
                    (
                        "text/javascript",
                        "L.push('ext:sync=' + W.id(document.currentScript));\
                         Promise.resolve().then(function(){ L.push('ext:micro=' + W.id(document.currentScript)); });\
                         setTimeout(function(){ L.push('ext:task=' + W.id(document.currentScript)); }, 0);"
                            .to_string(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 200 X\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("fp17", "print(JSON.stringify(L))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let log: Vec<String> = serde_json::from_str(&out[0]).expect("L is JSON");
    let seen = hits.lock().unwrap().clone();
    (log, seen)
}

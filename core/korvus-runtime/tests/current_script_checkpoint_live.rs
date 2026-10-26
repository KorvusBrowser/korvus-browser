//! A classic script's own microtasks read it as `document.currentScript`, because
//! HTML performs the script's microtask checkpoint BEFORE it puts `currentScript` back.
//!
//! # The rule, not the page
//!
//! "Execute the script element" saves `document.currentScript`, sets it to the element, calls **run
//! a classic script**, and only THEN restores it. "Run a classic script" ends with **clean up after
//! running script**, which performs the microtask checkpoint. The checkpoint is therefore *inside*
//! the window in which the element is `currentScript`, and every microtask the script queued reads
//! the element — while a TASK the same script queued is queued past the restore and reads null.
//!
//! A module script is the control from the other side: HTML runs it with `currentScript` already
//! null, so moving the checkpoint changes nothing there and both readings stay null.
//!
//! ...and the checkpoint is performed only **if the JavaScript execution context stack is now
//! empty**, which is the clause that decides the nested case: a script inserted and run from inside
//! another script performs no checkpoint of its own, so ITS microtasks are drained by the OUTER
//! script's checkpoint and read the outer element.
//!
//! # The oracle
//!
//! Stock Chrome 150.0.7871.46, this fixture served from loopback, attached over CDP by the parity
//! driver's own `attach` (never `chromium.launch()`), 2026-09-19:
//!
//! ```text
//!                            sync     Promise.then   queueMicrotask   await   setTimeout(0)
//!   inline classic           elem     elem           elem             elem    null
//!   external classic         elem     elem           -                -       -
//!   external classic async   elem     elem           -                -       -
//!   external classic defer   elem     elem           -                -       -
//!   inline type=module       null     null           -                -       -
//!   inserted inline (inner)  inner    THE OUTER      -                -       -
//! ```
//!
//! korvus on master at `47986ed4` read **null in every microtask column** and the element in every
//! `sync` one: it cleared `currentScript` the instant the body returned and drained the microtask
//! queue at the next turn of the event loop. Nineteen load-axis rows — every Next.js site in the
//! corpus — threw
//!
//! ```text
//! Invariant: Expected document.currentScript to be a <script> element. Received null instead.
//! ```
//!
//! from turbopack's `registerChunk`, which is an `async function`: the chunk's entry module is
//! evaluated from the continuation of an `await`, so the URL it reads its own asset prefix from is
//! read in a microtask. Nothing about that page is module-shaped — the chunk is a classic
//! `<script src async>` — and Chrome runs the identical stack, frame for frame, with the element in
//! hand.
//!
//! # What makes this a rule and not four literals
//!
//! Each row is asserted as an EQUATION between two readings of the same script — `micro == sync` for
//! a classic script, `micro == task == null` where HTML says there is no running script — so the
//! assertions do not name any particular element, shape or page. The `setTimeout(0)` column is what
//! keeps "the checkpoint happens first" apart from "currentScript is never cleared": a fix that
//! simply stopped clearing it would make the task column read the element and fail here.

use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Every body is the same two lines under a different id: read `currentScript` now, and read it
/// again from the microtask this script queues. ONE source for every shape, so a row that passes
/// cannot be passing for a reason another row does not have.
fn body_for(tag: &str) -> String {
    format!(
        "L.push('{tag}:sync=' + W.id(document.currentScript));\
         Promise.resolve().then(function () {{ L.push('{tag}:micro=' + W.id(document.currentScript)); }});"
    )
}

async fn serve(listener: TcpListener, page: String) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let tag = path.rsplit('/').next().unwrap_or("x").to_string();
            let (ctype, body) = if path == "/page.html" {
                ("text/html", page)
            } else if path.starts_with("/throw/") {
                // `/throw/<tag>`: an external classic script that runs and throws.
                ("text/javascript", format!("throw new TypeError('{tag}');"))
            } else {
                // `/js/<tag>`: the shared body, named by the tag in the URL.
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

/// `W.id(v)` names what a reading saw without naming the element: the script's own `id`, or the
/// spelling of the empty answer. `undefined` and `null` are folded to `null`, because HTML has one
/// "no running script" and this engine's store spells it both ways.
const HEAD: &str = r#"<script>var L=[];var W={id:function(v){return (v===null||v===undefined)?'null':(v.id||v.tagName)}};
addEventListener('error',function(e){if(e.target===window)L.push('err:cs=' + W.id(document.currentScript))});</script>"#;

/// Load one page from loopback, let it settle, and return the page's `L`.
async fn run_page(body: &str) -> Vec<String> {
    let doc = format!("<!doctype html><html><head>{HEAD}</head><body>{body}</body></html>");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, doc.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(400);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("fp13", "print(JSON.stringify(L))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    serde_json::from_str(&out[0]).expect("L is JSON")
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
// 1. Every classic shape: the microtask reading EQUALS the synchronous one.
// ---------------------------------------------------------------------------------------------

/// One `<script>` per shape, each running the identical body, each asserted against ITSELF: what the
/// script read while running, and what the microtask it queued read. The shapes differ only in how
/// the parser reached the script -- inline, external, `async`, `defer` -- which is exactly the thing
/// that must not matter. Every one of them runs with an empty JS stack, so every one performs its
/// own checkpoint.
#[tokio::test]
async fn a_classic_scripts_microtask_reads_the_script_that_queued_it() {
    let body = format!(
        "<script id=\"inl\">{}</script>\
         <script id=\"ext\" src=\"/js/ext\"></script>\
         <script id=\"asy\" src=\"/js/asy\" async></script>\
         <script id=\"def\" src=\"/js/def\" defer></script>",
        body_for("inl"),
    );
    let log = run_page(&body).await;
    for tag in ["inl", "ext", "asy", "def"] {
        let sync = read(&log, tag, "sync");
        let micro = read(&log, tag, "micro");
        // The script ran at all -- otherwise "null == null" would pass this test vacuously.
        assert_ne!(sync, "null", "{tag} never became document.currentScript: {log:?}");
        assert_eq!(micro, sync, "{tag}: its own microtask read {micro}, not the running script {sync}");
    }
}

// ---------------------------------------------------------------------------------------------
// 2. The control: a TASK is queued past the restore and sees no running script.
// ---------------------------------------------------------------------------------------------

/// Without this, "the checkpoint happens before the restore" and "the restore never happens" are the
/// same test. A `setTimeout(0)` callback runs on a later task, after `currentScript` is back to null
/// -- Chrome 150: `null`.
#[tokio::test]
async fn a_task_queued_by_a_classic_script_sees_no_running_script() {
    let body = "<script id=\"t\">\
        L.push('t:sync=' + W.id(document.currentScript));\
        Promise.resolve().then(function(){ L.push('t:micro=' + W.id(document.currentScript)); });\
        setTimeout(function(){ L.push('t:task=' + W.id(document.currentScript)); }, 0);\
        </script>";
    let log = run_page(body).await;
    assert_eq!(read(&log, "t", "sync"), "t");
    assert_eq!(read(&log, "t", "micro"), "t", "the microtask is inside the script: {log:?}");
    assert_eq!(read(&log, "t", "task"), "null", "a task is outside it: {log:?}");
}

// ---------------------------------------------------------------------------------------------
// 3. A module is null on both readings -- the checkpoint moved, the module's answer did not.
// ---------------------------------------------------------------------------------------------

/// `document.currentScript` is null throughout a module script in HTML, so there is nothing for the
/// checkpoint's position to reveal. Asserted so that a future "set currentScript around everything"
/// shortcut cannot pass test 1 by breaking this.
#[tokio::test]
async fn a_module_reads_null_both_while_it_runs_and_in_its_microtask() {
    let body = format!("<script id=\"mod\" type=\"module\">{}</script>", body_for("mod"));
    let log = run_page(&body).await;
    assert_eq!(read(&log, "mod", "sync"), "null", "{log:?}");
    assert_eq!(read(&log, "mod", "micro"), "null", "{log:?}");
}

// ---------------------------------------------------------------------------------------------
// 4. The same window, from the other side: a throw is REPORTED before the restore too.
// ---------------------------------------------------------------------------------------------

/// "Run a classic script" reports the exception and only then returns, so "execute the script
/// element" has not restored `currentScript` when the window's `error` listener runs. The checkpoint
/// and the report live in the same window and must not be separated by the restore.
///
/// Chrome 150.0.7871.46 (loopback, 2026-09-19) reads the throwing element in the handler for BOTH a
/// parser-inserted script and one inserted from script; korvus read the element for the first and
/// `null` for the second, because that path reported after putting `currentScript` back.
#[tokio::test]
async fn the_error_event_for_a_throwing_script_names_that_script() {
    for body in [
        // parser-inserted, inline
        "<script id=\"boom\">throw new TypeError('x');</script>",
        // parser-inserted, external
        "<script id=\"boom\" src=\"/throw/x\"></script>",
        // inserted from script, inline
        "<script id=\"host\">var s=document.createElement('script');s.id='boom';\
         s.text=\"throw new TypeError('x');\";document.head.appendChild(s);</script>",
        // inserted from script, external -- the path that reported after the restore
        "<script id=\"host\">var s=document.createElement('script');s.id='boom';\
         s.src='/throw/x';document.head.appendChild(s);</script>",
    ] {
        let log = run_page(body).await;
        let seen: Vec<&String> = log.iter().filter(|l| l.starts_with("err:cs=")).collect();
        assert_eq!(seen.len(), 1, "exactly one uncaught error per page: {log:?}");
        assert_eq!(seen[0], "err:cs=boom", "the ErrorEvent must name the throwing script: {log:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// 5. Nesting: an inner script's checkpoint does not steal the outer script's turn.
// ---------------------------------------------------------------------------------------------

/// THE CLAUSE THAT DECIDES IT: "clean up after running script" performs the checkpoint only *if the
/// JavaScript execution context stack is now empty*. An inline script inserted and run from inside
/// another script leaves the outer script on the stack, so it performs NO checkpoint and its own
/// microtask is drained by the outer script's -- reading the OUTER element.
///
/// Measured on Chrome 150.0.7871.46 (loopback, 2026-09-19): `inner` sync reads `inner`, `inner`'s
/// microtask reads `outer`, and both of `outer`'s readings read `outer`. This is the row that tells
/// the RULE ("the checkpoint's owner is the script whose stack is empty") apart from the OBSERVATION
/// ("a script's microtask reads that script"), and korvus answered it by accident, with no checkpoint
/// anywhere, until FP13 seated one in each of the four parser shapes and NOT here.
#[tokio::test]
async fn a_nested_script_is_drained_by_the_outer_scripts_checkpoint() {
    let body = "<script id=\"outer\">\
        var s=document.createElement('script');s.id='inner';\
        s.text=\"L.push('inner:sync=' + W.id(document.currentScript));\
                Promise.resolve().then(function(){ L.push('inner:micro=' + W.id(document.currentScript)); });\";\
        document.head.appendChild(s);\
        L.push('outer:sync=' + W.id(document.currentScript));\
        Promise.resolve().then(function(){ L.push('outer:micro=' + W.id(document.currentScript)); });\
        </script>";
    let log = run_page(body).await;
    assert_eq!(read(&log, "inner", "sync"), "inner", "the inner script IS currentScript: {log:?}");
    assert_eq!(read(&log, "outer", "sync"), "outer", "and the outer one is again after it: {log:?}");
    assert_eq!(read(&log, "outer", "micro"), "outer", "{log:?}");
    assert_eq!(
        read(&log, "inner", "micro"),
        read(&log, "outer", "sync"),
        "a nested script performs no checkpoint of its own, so its microtask belongs to the outer script: {log:?}"
    );
}

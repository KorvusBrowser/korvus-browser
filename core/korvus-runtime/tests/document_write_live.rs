//! `document.write()` on the PAGE document put nothing in the tree, and five rows
//! of the replay tier were exactly the elements it dropped.
//!
//! # What was wrong
//!
//! `core/dom/document.rs::cb_doc_write_family` answered three documents: a nested browsing
//! context's (written for real), a non-HTML one (`InvalidStateError`) and **every other one with a
//! silent return**. The silent return is the page's own document, so a page whose parser-reached
//! script wrote markup got no markup and no error -- `document.write` reported success and did
//! nothing, which is the failure shape this goal keeps finding.
//!
//! # The measurement that made it a row rather than a curiosity
//!
//! A counting shim over `Document.prototype.write`, installed before the page ran, over the 32
//! rows of R37's partition B (failing, within +-3 nodes of Chrome). Six call it at top level, and
//! the ELEMENT COUNT of what they wrote is the row's node deficit, to the element:
//!
//! ```text
//!   site (recording)          wrote                      korvus nodes   Chrome   after
//!   webassembly.org           1 meta + 2 link (275 ch)        65          68       68
//!   www.r-project.org         1 <a>              (58 ch)     202         203      203
//!   rxjs.dev                  1 <script src>     (48 ch)     222         223      223
//!   docs.oracle.com/en/java   1 <script src>     (48 ch)     630         631      631
//!   caniuse.com               1 <style>          (42 ch)     240         241      241
//!   www.pcbway.com            1 <iframe>        (224 ch)    2231        2233     2232
//! ```
//!
//! # The oracle: Chrome 150, same loopback server, same page
//!
//! ```text
//!                                              chrome            korvus (before)
//!   meta+2 link written from a head script     3 elements        0
//!   two writes then a writeln, in body         w1,w2,w3 in order  none
//!   a written <script src>                     fetched and ran   not in the DOM
//!   a write AFTER load                         document.open()s  nothing            <- still a gap
//! ```
//!
//! # What is asserted, and what is deliberately not
//!
//! The RULE is HTML's: a write from the script the parser reached lands **at the insertion point**,
//! which for a script-inserted write is immediately after that script's element, and successive
//! writes follow one another. Asserted as that rule -- position relative to the writing script and
//! to its siblings -- not as a node count, which would be a second copy of the fixture.
//!
//! NOT asserted, because korvus does not do it and inventing it would be worse than the hole:
//! a write of UNBALANCED markup (the real parser leaves the open tag on the stack; the fragment
//! parser here balances it), and a write after the parser finished (Chrome implies
//! `document.open()` and wipes the document; korvus still does nothing). The last one is measured
//! in `a_write_after_the_parser_has_finished_is_still_a_no_op` so the gap has a failing-if-it-moves
//! record rather than a sentence.
use engine::Engine;
use std::time::Duration;

/// Load `html` as a whole document, pump until `load`, then read one expression.
async fn run(html: &str, read: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, "http://127.0.0.1/page.html", &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    e.pump(Duration::from_millis(300)).await.expect("pump 2");
    e.eval_value("read", read.to_string()).expect("eval")
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(serde_json::Value::as_str).unwrap_or("<missing>").to_string()
}

/// WHAT A HEAD SCRIPT WRITES IS IN THE HEAD, AFTER THAT SCRIPT. webassembly.org's shape: a
/// `<script type=text/plain>` template read back and written, which is how a page ships two
/// `prefers-color-scheme` stylesheets without a flash.
#[tokio::test]
async fn markup_written_by_a_head_script_lands_in_the_head_after_it() {
    let out = run(
        r#"<!doctype html><html><head>
        <script id="tpl" type="text/plain"><meta name="color-scheme" content="light dark">
<link rel="stylesheet" href="/light.css"><link rel="stylesheet" href="/dark.css"></script>
        <script id="w">document.write(document.getElementById("tpl").textContent);</script>
        </head><body><p>x</p><script>
        window.R = function () {
          var names = [];
          var n = document.head.firstElementChild;
          while (n) { names.push(n.id || n.localName); n = n.nextElementSibling; }
          return {
            head_order: names.join(","),
            // the RULE: every written node is a later sibling of the script that wrote it
            after_writer: String(Array.prototype.every.call(
              document.querySelectorAll('head meta[name=color-scheme], head link[rel=stylesheet]'),
              function (el) {
                return document.getElementById("w").compareDocumentPosition(el)
                       & Node.DOCUMENT_POSITION_FOLLOWING;
              })),
          };
        };
        </script></body></html>"#,
        "window.R()",
    )
    .await;
    // Chrome 150, same page: "tpl,w,meta,link,link".
    assert_eq!(s(&out, "head_order"), "tpl,w,meta,link,link", "written nodes follow the writer in the head");
    assert_eq!(s(&out, "after_writer"), "true", "every written node is positioned after its writer");
}

/// SUCCESSIVE WRITES FOLLOW ONE ANOTHER, and `writeln` is `write` plus a newline. The insertion
/// point ADVANCES: a second write that re-anchored on the script element would reverse the pair,
/// which is the bug this asserts against.
#[tokio::test]
async fn successive_writes_keep_their_call_order_and_precede_the_next_element() {
    let out = run(
        r#"<!doctype html><html><body><div id="before">B</div>
        <script id="s1">
          document.write('<span id="w1">one</span>');
          document.write('<span id="w2">two</span>');
          document.writeln('<span id="w3">three</span>');
        </script>
        <div id="after">A</div>
        <script>
        window.R = function () {
          var names = [], n = document.body.firstElementChild;
          while (n) { names.push(n.id || n.localName); n = n.nextElementSibling; }
          return { order: names.join(","),
                   newline_after_writeln: String(
                     document.getElementById("w3").nextSibling &&
                     document.getElementById("w3").nextSibling.nodeType === 3 &&
                     document.getElementById("w3").nextSibling.data.indexOf("\n") === 0) };
        };
        </script></body></html>"#,
        "window.R()",
    )
    .await;
    // Chrome 150, same page: "before,s1,w1,w2,w3,after,script".
    assert_eq!(s(&out, "order"), "before,s1,w1,w2,w3,after,script",
               "three writes land between the writer and the element after it, in call order");
    assert_eq!(s(&out, "newline_after_writeln"), "true", "writeln appends the newline");
}

/// AN INLINE WRITTEN `<script>` IS IN THE DOCUMENT AND DOES NOT RUN -- the one place this change
/// leaves korvus disagreeing with Chrome, recorded as a measurement rather than as a sentence.
///
/// Measured on the loopback fixture `inline-script.html`, both engines, same page:
///
/// ```text
///                                                chrome      korvus
///   host.appendChild(inline <script>)            runs        runs
///   host.innerHTML += "<script>…"                inert       inert     <- both right
///   document.write("<script>…")                  runs        INERT     <- this row
/// ```
///
/// WHY IT IS NOT FIXED HERE. The written nodes come from the FRAGMENT PARSER, and `node.rs`'s
/// `notify_connected` already records why its batch path passes `scripts: false`: this engine sets
/// HTML's "already started" flag nowhere, so `<script>` inside `innerHTML` stays inert only because
/// the insertion steps never reach it. Running the written one from here would ALSO run it
/// SYNCHRONOUSLY, inside the `document.write()` call, where Chrome runs it after the writing script
/// returns -- a second, invisible difference bought to close a visible one. The `src` form, which
/// is the form the corpus actually writes, reaches the subresource loader by its own route and does
/// run (`which_subresources_load::what_the_page_writes_into_itself_loads`).
#[tokio::test]
async fn an_inline_written_script_is_in_the_document_and_runs() {
    let out = run(
        r#"<!doctype html><html><body>
        <script>document.write('<scr' + 'ipt id="wrote">window.RAN = "yes";</scr' + 'ipt>');</script>
        <script>
        window.R = function () {
          return { ran: String(window.RAN),
                   in_dom: String(document.getElementById("wrote") !== null) };
        };
        </script></body></html>"#,
        "window.R()",
    )
    .await;
    assert_eq!(s(&out, "in_dom"), "true", "the written script element IS in the document");
    // The fragment parser now sets "already started" on what it creates
    // (so `innerHTML` scripts stay inert by the flag, not by the walk), and a script `document.write`
    // puts into the document is prepared by the insertion steps -- it runs, as Chrome runs it.
    assert_eq!(s(&out, "ran"), "yes", "Chrome runs a written inline script, and so does korvus now");
}

/// After the parser is done there is no insertion point: HTML says the write implies
/// `document.open()`, which discards the document, and the written markup is parsed into the new one
/// -- Chrome does exactly that. (Until 2026-10-25 korvus wrote nothing here; this test recorded the gap.)
#[tokio::test]
async fn a_write_after_the_parser_has_finished_opens_the_document_again() {
    let out = run(
        r#"<!doctype html><html><body><div id="keep">K</div>
        <script>
        window.R = function () {
          document.write('<b id="late">late</b>');
          return { late_appeared: String(document.getElementById("late") !== null),
                   page_survived: String(document.getElementById("keep") !== null) };
        };
        </script></body></html>"#,
        "window.R()",
    )
    .await;
    // Chrome 150 answers late_appeared=true, page_survived=false (the open() wiped it).
    assert_eq!(s(&out, "late_appeared"), "true", "the written markup is in the reopened document");
    assert_eq!(s(&out, "page_survived"), "false", "and the implied open() discarded the old one");
}

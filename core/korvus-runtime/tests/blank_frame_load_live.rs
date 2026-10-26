//! A frame whose navigation never reaches the network still ENDS A NAVIGATION, so it
//! fires `load` AT THE ELEMENT. Live, in the engine, against Chrome's own answers.
//!
//! # What was wrong, and how a page felt it
//!
//! `core/html/subresource.rs::induced` returns `None` for a frame with no `src`, an empty one, an
//! `about:*` one or a `javascript:` one -- correctly, there is no request -- and that was the whole
//! answer, so nothing else happened either. HTML's "process the iframe attributes" runs the IFRAME
//! LOAD EVENT STEPS for exactly those URLs in place of a navigation.
//!
//! The shape that pays for it is the analytics pixel's FORM-POST TRANSPORT, and it is why this is a
//! row rather than a conformance one. The library builds
//! `<form target=NAME><iframe name=NAME src=about:blank></iframe></form>`, appends it, and does the
//! rest **in the iframe's `load` handler**: add one `<input>` per parameter, `form.submit()`, and on
//! the SECOND load remove the form from the document. With no first `load` the form is built, never
//! submitted and **never removed** -- so korvus carried a `<form>` and an `<iframe>` that Chrome's
//! DOM does not, on every page that ships that pixel and fires an event.
//!
//! # The oracle: Chrome 150.0.7871.46, driven over CDP by `tools/parity/driver.py`
//!
//! One loopback page, both engines, the count of `load` events per shape. korvus before the fix in
//! brackets:
//!
//! ```text
//!   no src attribute at all                     1   [0]
//!   src=""                                      1   [0]
//!   src="about:blank"   ·  "about:blank?a=1#f"  1   [0]
//!   src="about:srcdoc"  ·  "about:foo"          1   [0]
//!   src="javascript:void 0"                     1   [0]
//!   <frame> rather than <iframe>                1   [0]
//!   srcdoc="<p>hi"                              1   [1]   <- a real navigation, already fired
//! ```
//!
//! and the three rules that a reading of the spec gets wrong, each measured on Chrome:
//!
//! ```text
//!   ORDER      appending an <iframe> logs its `load` BEFORE the next statement -- synchronous
//!   TARGET     with a listener on a connected frame's contentWindow, a second src="about:blank"
//!              fires the ELEMENT's load once and the WINDOW's zero times (the document IS replaced)
//!   REPEAT     remove+append fires again (2); insert then src="about:blank" fires again (2);
//!              an <iframe> inside a DETACHED <div> fires 0 until the div is connected, then 1
//! ```
//!
//! # Not asserted here
//!
//! Chrome interleaves a parser-made frame's `load` with the inline scripts around it (an
//! `<iframe id=a>` has fired before the next `<script>` can add a listener). This engine parses a
//! document in one shot and sweeps it at install, so its parser-made frames all fire before the
//! page's first inline script. That is the same ordering limit `document.write` has here; the COUNT
//! is asserted, the interleaving is not.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Load `html` as a whole document and pump until `load` fires, then read one expression.
async fn run(html: &str, read: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, "http://127.0.0.1/page.html", &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    // A second turn of the loop, so anything the page queued for after `load` has run.
    e.pump(Duration::from_millis(300)).await.expect("pump 2");
    e.eval_value("read", read.to_string()).expect("eval")
}

fn n(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(serde_json::Value::as_i64).unwrap_or(-1)
}

/// EVERY NON-FETCHING FRAME SHAPE FIRES EXACTLY ONE `load`, and the counts are Chrome's.
#[tokio::test]
async fn every_non_fetching_frame_shape_fires_one_load_at_the_element() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = {};
        function shape(key, build) {
          var el = document.createElement("iframe");
          C[key] = 0;
          el.addEventListener("load", function () { C[key]++; });
          build(el);
          document.body.appendChild(el);
        }
        shape("no_src", function () {});
        shape("src_empty", function (el) { el.src = ""; });
        shape("about_blank", function (el) { el.src = "about:blank"; });
        shape("about_blank_query", function (el) { el.src = "about:blank?a=1#f"; });
        shape("about_srcdoc", function (el) { el.src = "about:srcdoc"; });
        shape("about_foo", function (el) { el.src = "about:foo"; });
        shape("javascript_url", function (el) { el.src = "javascript:void 0"; });
        shape("srcdoc", function (el) { el.srcdoc = "<p>hi</p>"; });
        var fr = document.createElement("frame");
        C.frame_element = 0;
        fr.addEventListener("load", function () { C.frame_element++; });
        document.body.appendChild(fr);
        </script></body></html>"#,
        "window.C",
    )
    .await;
    for key in [
        "no_src",
        "src_empty",
        "about_blank",
        "about_blank_query",
        "about_srcdoc",
        "about_foo",
        "javascript_url",
        "srcdoc",
        "frame_element",
    ] {
        assert_eq!(n(&out, key), 1, "{key}: Chrome 150 fires exactly one `load`; got {out}");
    }
}

/// THE ORDER IS CHROME'S: the `load` is dispatched as part of the insertion, before the statement
/// after `appendChild` -- not from a task, and not from a microtask.
#[tokio::test]
async fn the_blank_frame_load_is_dispatched_inside_the_insertion() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.SEQ = [];
        var a = document.createElement("iframe");
        a.addEventListener("load", function () { SEQ.push("load"); });
        queueMicrotask(function () { SEQ.push("micro-queued-before"); });
        document.body.appendChild(a);
        SEQ.push("after-append");
        Promise.resolve().then(function () { SEQ.push("micro-after"); });
        setTimeout(function () { SEQ.push("task"); }, 0);
        </script></body></html>"#,
        "JSON.stringify(window.SEQ)",
    )
    .await;
    assert_eq!(
        out.as_str().unwrap_or_default(),
        r#"["load","after-append","micro-queued-before","micro-after","task"]"#,
        "Chrome 150 logs the frame's `load` BEFORE the statement after appendChild"
    );
}

/// AT THE ELEMENT, NOT AT THE WINDOW -- and a repeat navigation to `about:blank` is a repeat load.
#[tokio::test]
async fn a_second_about_blank_fires_the_element_and_not_the_frame_window() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { win: 0, el: 0 };
        var d = document.createElement("iframe");
        document.body.appendChild(d);                 // the initial about:blank
        var w = d.contentWindow;
        try { w.addEventListener("load", function () { C.win++; }); } catch (e) {}
        d.addEventListener("load", function () { C.el++; });
        d.src = "about:blank";                        // a SECOND navigation to about:blank
        </script></body></html>"#,
        "window.C",
    )
    .await;
    assert_eq!(n(&out, "el"), 1, "the ELEMENT hears the second about:blank (Chrome: 1); got {out}");
    assert_eq!(n(&out, "win"), 0, "the frame's WINDOW hears nothing (Chrome: 0); got {out}");
}

/// ONLY THE INSERTION IS SYNCHRONOUS: a `src` written on a connected frame is a navigation whose
/// `load` arrives in a later task, so a frame whose `onload` re-navigates it to `about:blank` loops
/// one task at a time instead of recursing. It recursed in-line until the process died
/// (`Check failed: IsOnCentralStack()`, WPT `opening-the-input-stream/ignore-opens-during-unload`).
#[tokio::test]
async fn a_src_write_on_a_connected_frame_loads_in_a_later_task() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { loads: 0, depth: 0, deepest: 0, sync: -1 };
        var f = document.createElement("iframe");
        document.body.appendChild(f);                 // the initial about:blank: fired in-line, unheard
        f.onload = function () {
          C.depth++; C.deepest = Math.max(C.deepest, C.depth); C.loads++;
          if (C.loads < 5) f.src = "about:blank";
          C.depth--;
        };
        f.src = "about:blank";
        C.sync = C.loads;                             // nothing yet: the load is a task away
        f.src = "about:blank";                        // the same navigation replaced: still one load
        </script></body></html>"#,
        "window.C",
    )
    .await;
    assert_eq!(n(&out, "sync"), 0, "the src write's load is not in-line (Chrome: 0); got {out}");
    assert_eq!(n(&out, "deepest"), 1, "each load is its own task, never nested; got {out}");
    assert_eq!(n(&out, "loads"), 5, "one load per navigation, the two writes in one task counting once; got {out}");
}

/// A FRAME IN NO DOCUMENT FIRES NOTHING, and connecting the subtree that holds it fires once --
/// which is the case the pixel's transport depends on, since it appends the FORM and not the frame.
#[tokio::test]
async fn a_detached_frame_is_silent_until_its_subtree_is_connected() {
    let out = run(
        r#"<!doctype html><html><body><script>
        window.C = { before: -1, after: -1, moved: 0 };
        var holder = document.createElement("div");
        var e = document.createElement("iframe");
        var loads = 0;
        e.addEventListener("load", function () { loads++; });
        holder.appendChild(e);
        C.before = loads;
        document.body.appendChild(holder);
        C.after = loads;
        // and a move is a new navigation, so it fires again
        var m = document.createElement("iframe");
        m.addEventListener("load", function () { C.moved++; });
        document.body.appendChild(m);
        m.parentNode.removeChild(m);
        document.body.appendChild(m);
        </script></body></html>"#,
        "window.C",
    )
    .await;
    assert_eq!(n(&out, "before"), 0, "a frame in a DETACHED subtree fires nothing (Chrome: 0); got {out}");
    assert_eq!(n(&out, "after"), 1, "connecting the subtree fires it once (Chrome: 1); got {out}");
    assert_eq!(n(&out, "moved"), 2, "remove + append is a second navigation (Chrome: 2); got {out}");
}

/// THE WHOLE TRANSPORT, END TO END -- the shape the parity rows carry. The page builds
/// `<form target=NAME><iframe name=NAME src=about:blank>`, and does everything else from the
/// frame's events: inputs and `submit()` on the first, removal on the second. The assertion is that
/// the document is left as Chrome leaves it -- no form, no iframe -- and the server's log proves the
/// POST actually went out rather than the form being dropped on the floor.
#[tokio::test]
async fn the_form_post_transport_submits_and_takes_its_form_back_out_of_the_document() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let Ok(n) = s.read(&mut buf).await else { return };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let body = b"<!doctype html><title>ok</title>";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body).await;
                let _ = s.flush().await;
            });
        }
    });

    let html = format!(
        r#"<!doctype html><html><body><script>
        window.C = {{ first: 0, second: 0, inputs: 0 }};
        var nm = "tgt1";
        var f = document.createElement("form");
        f.method = "post"; f.action = "http://{addr}/collect"; f.target = nm;
        f.acceptCharset = "utf-8"; f.style.display = "none";
        var i = document.createElement("iframe");
        i.src = "about:blank"; i.id = nm; i.name = nm;
        f.appendChild(i);
        i.addEventListener("load", function once() {{
          i.removeEventListener("load", once);
          C.first++;
          var inp = document.createElement("input"); inp.name = "k"; inp.value = "v";
          f.appendChild(inp);
          C.inputs = f.getElementsByTagName("input").length;
          i.addEventListener("load", function () {{
            C.second++;
            if (f.parentNode) f.parentNode.removeChild(f);
          }});
          f.submit();
        }});
        document.body.appendChild(f);
        </script></body></html>"#
    );
    // The counts are read HERE rather than from a task the page queues: the submitted frame's load
    // lands after the document's own, so a page-side reading would be measuring the probe's timing
    // and not the engine's behaviour.
    let out = run(
        &html,
        "({ first: window.C.first, second: window.C.second, inputs: window.C.inputs, \
           forms: document.getElementsByTagName('form').length, \
           iframes: document.getElementsByTagName('iframe').length })",
    )
    .await;
    assert_eq!(n(&out, "first"), 1, "the initial about:blank must fire (Chrome: 1); got {out}");
    assert_eq!(n(&out, "inputs"), 1, "the handler appends its input; got {out}");
    let requests = seen.lock().unwrap().clone();
    assert!(
        requests.iter().any(|l| l.starts_with("POST /collect")),
        "the submit must reach the server as a POST; server saw {requests:?}"
    );
    assert_eq!(n(&out, "second"), 1, "the submitted frame's load must fire (Chrome: 1); got {out}");
    assert_eq!(n(&out, "forms"), 0, "the form is removed by its own handler, as in Chrome; got {out}");
    assert_eq!(n(&out, "iframes"), 0, "and the iframe goes with it; got {out}");
}

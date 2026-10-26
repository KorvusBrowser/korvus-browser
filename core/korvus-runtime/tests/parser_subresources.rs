//! The subresources the PARSER produces, on the wire.
//!
//! THE DEFECT THIS FILE EXISTS FOR. `core/korvus-webapi/src/core/html/subresource.rs` has two seams:
//! an attribute write and an arena insertion. The parser goes through NEITHER -- it builds its nodes
//! straight into the flat arena -- so every `<img src>`, `<link rel=stylesheet>`, `<object data>` and
//! SVG `<image href>` written in the MARKUP was never requested at all. Measured before this unit on
//! a loopback server that logs paths: a page carrying a stylesheet, an image and a script requested
//! exactly two things, itself and the script. Chrome 150.0.7871.46 over the same server requests all
//! four.
//!
//! Only frames were swept (`op_start_parsed_frames`), which is why `<iframe src>` worked and nothing
//! else did.
//!
//! THESE TESTS DO NOT ASSERT ON `/page.html`. `Engine::load_page` is handed the document's bytes, so
//! the page itself never crosses this wire -- which makes the log a clean measure of what the
//! ELEMENTS asked for, and an empty log a real answer rather than a setup failure.
//!
//! WHY A PASSING TEST WAS NOT EVIDENCE. W9.13 made every subresource delay the document's `load`
//! event and its `<img>` row passed -- but every fixture in `load_delay.rs` injects its element from
//! SCRIPT, which is the seam that already worked. A resource that is never requested also never
//! delays anything, so the delay tests could not have caught this. The assertions here are on the
//! REQUEST LOG, not on a timing, for exactly that reason.
//!
//! A DOUBLE FETCH IS WORSE THAN NONE, so the negatives are asserted as hard as the positives. Every
//! expected value below was measured against headless Chrome 150.0.7871.46 over this same shape of
//! loopback server (`--headless=new --dump-dom`, 2026-09-11):
//!
//! | markup | Chrome requests it | Chrome delays `load` for it |
//! |---|---|---|
//! | `<img src>`                         | yes | yes (641 ms against a 600 ms server) |
//! | `<link rel=stylesheet>`             | yes | yes (645 ms) |
//! | `<script src>`                      | yes, ONCE | yes |
//! | `<link rel=preload as=style\|image>` | yes | **no** (1 ms) |
//! | `<link rel=prefetch>`               | yes | no (0 ms) |
//! | `<link rel=icon>`                   | yes | no (0 ms) |
//! | `<link rel=manifest>`               | yes | no (1 ms) |
//! | `<link rel=preload>` with no `as`   | **no** | -- |
//! | `<link rel=author>` (unknown rel)   | **no** | -- |
//! | `<img>` with no `src`, `<img src="">` | **no** | -- |
//! | a page with nothing to fetch        | -- | no (0 ms) |
use engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A 1x1 transparent PNG, so an `<img>` has real bytes to receive.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xff, 0xff, 0x03, 0x00, 0x00, 0x06, 0x00, 0x05, 0x57, 0x0b, 0x7c, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
    0x60, 0x82,
];

/// How long a `/slow*` route sleeps before answering. Above an undelayed `load` (a few ms) and below
/// the 1000 ms load-delay cap, so "delayed" and "not delayed" are never in doubt.
const DELAY_MS: u64 = 300;
const HALF: u128 = DELAY_MS as u128 / 2;

/// The request log: every path this server was asked for, in arrival order, with duplicates kept --
/// a double fetch has to be visible.
type Log = Arc<Mutex<Vec<String>>>;

fn body_for(path: &str) -> (u16, &'static str, Vec<u8>) {
    if path.starts_with("/missing") {
        (404, "text/plain", b"no".to_vec())
    } else if path.ends_with(".png") {
        (200, "image/png", PNG.to_vec())
    } else if path.ends_with(".css") {
        (200, "text/css", b"#p { color: rgb(1, 2, 3) }".to_vec())
    } else if path.ends_with(".js") {
        (200, "text/javascript", b"globalThis.__RAN = 1;".to_vec())
    } else {
        (200, "application/octet-stream", b"{}".to_vec())
    }
}

/// A recording loopback server. `page` is answered at `/page.html`; everything else is a fixture
/// resource. A path beginning `/slow` sleeps first. One task per connection, so concurrent resources
/// are genuinely concurrent.
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
            let (status, ctype, body) = if path == "/page.html" {
                (200, "text/html", page.into_bytes())
            } else {
                if path.starts_with("/slow") {
                    tokio::time::sleep(Duration::from_millis(DELAY_MS)).await;
                }
                body_for(&path)
            };
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

/// The page's own stopwatch, so `load` timing is the page's answer -- the same number the Chrome rig
/// in the header prints.
const PROBE: &str = r#"<script>window.__t0 = Date.now();</script>
<script>addEventListener("load", function () { window.__ms = Date.now() - window.__t0; });</script>"#;

/// Load a page built from `head` + `body` and pump until `load` (or 5 s), then settle for `settle`.
///
/// THE SETTLE IS NOT `Engine::pump`. A worker runs on its OWN thread with its own event loop, and
/// nothing the main isolate can poll knows it is in flight -- so `pump` drains, answers `Drained`
/// immediately, and returns long before the worker's own `fetch` has left the machine. (That is what
/// made `new Worker(rel)` look like it never requested anything: `korvus fetch --wait 3` is one
/// `pump`, and one `pump` over an idle main loop is not three seconds.) This loop yields the THREAD
/// between slices, which is the only thing that gives another thread its turn.
///
/// Returns `(request counts, ms at load)`.
async fn run(head: &str, body: &str, settle: Duration) -> (HashMap<String, usize>, Option<u128>) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let doc = format!("<!doctype html><html><head>{PROBE}{head}</head><body>{body}</body></html>");
    tokio::spawn(serve(listener, doc.clone(), log.clone()));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + settle;
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let ms = e.eval_value("chk-ms", "window.__ms === undefined ? null : window.__ms".to_string()).expect("eval");

    let mut counts: HashMap<String, usize> = HashMap::new();
    for p in log.lock().unwrap().iter() {
        *counts.entry(p.clone()).or_default() += 1;
    }
    (counts, ms.as_u64().map(u128::from))
}

fn n(counts: &HashMap<String, usize>, path: &str) -> usize {
    counts.get(path).copied().unwrap_or(0)
}

/// A readable dump of the whole log, so a failure says what DID go out rather than only what did not.
fn dump(counts: &HashMap<String, usize>) -> String {
    let mut v: Vec<String> = counts.iter().map(|(k, c)| format!("{k} x{c}")).collect();
    v.sort();
    v.join(", ")
}

// ---- the positives ----------------------------------------------------------------------------

/// THE MEASUREMENT THIS UNIT EXISTS FOR. Before it, this page put exactly ONE thing on the wire --
/// `/script.js`, and only because `load_page` fetches external scripts itself. Chrome asks for all
/// five.
#[tokio::test]
async fn the_parser_s_own_subresources_are_requested() {
    let (c, _) = run(
        r#"<link rel="stylesheet" href="/style.css"><script src="/script.js"></script>"#,
        r#"<img src="/image.png"><object data="/thing.bin"></object><svg><image href="/svg.png"></image></svg>"#,
        Duration::ZERO,
    )
    .await;
    assert_eq!(n(&c, "/style.css"), 1, "a parser-produced <link rel=stylesheet> must be fetched exactly once; log: {}", dump(&c));
    assert_eq!(n(&c, "/image.png"), 1, "a parser-produced <img src> must be fetched exactly once; log: {}", dump(&c));
    assert_eq!(n(&c, "/script.js"), 1, "a parser-produced <script src> must be fetched exactly ONCE -- it is already fetched and executed by load_page; log: {}", dump(&c));
    assert_eq!(n(&c, "/thing.bin"), 1, "a parser-produced <object data> must be fetched exactly once; log: {}", dump(&c));
    assert_eq!(n(&c, "/svg.png"), 1, "a parser-produced SVG <image href> must be fetched exactly once; log: {}", dump(&c));
}

/// `new Worker(relativeURL)`: the worker fetches its own top-level script, on its own thread, from
/// the URL resolved against the page.
///
/// THIS ONE WAS NOT BROKEN, AND FINDING THAT OUT IS THE POINT. It was reported as "never requested"
/// alongside the parser's subresources, and it looked exactly like one on the wire -- but the cause
/// was the INSTRUMENT: the probe (and `korvus fetch --wait N`, and `Engine::pump`) returns the moment
/// the MAIN loop drains, and a worker's fetch is on another thread that the main loop knows nothing
/// about. Given a settle that actually yields the thread, the request is there. The test stays,
/// because the day this regresses for real it must fail for a real reason.
///
/// THE SETTLE IS 3 s, NOT 800 ms. This page now also puts `/favicon.ico` on the wire
/// after `load` -- Chrome requests it too -- and with that request in flight on the main loop, this
/// harness's sliced settle saw the worker's request arrive after 800 ms in 3 of 5 runs (4 of 4 inside
/// 4 s; 5 of 5 inside 800 ms with an icon that needs no request). `korvus fetch` over a Python server
/// shows no such delay: worker.js 30-50 ms after the favicon. The budget is the harness's, not the
/// engine's, and the assertion -- exactly once -- is unchanged.
#[tokio::test]
async fn a_worker_created_with_a_relative_url_fetches_its_script() {
    let (c, _) = run("", r#"<script>try { new Worker("/worker.js"); } catch (e) {}</script>"#, Duration::from_millis(3000)).await;
    assert_eq!(n(&c, "/worker.js"), 1, "new Worker(relative) must fetch its script from the page-relative URL exactly once; log: {}", dump(&c));
}

/// The `<link>` rels Chrome DOES fetch, each exactly once. `preload` needs its `as` to have a
/// destination at all -- see the negative below.
#[tokio::test]
async fn the_link_rels_chrome_fetches_are_fetched_exactly_once() {
    let (c, _) = run(
        r#"<link rel="preload" as="style" href="/preload.css">
           <link rel="preload" as="image" href="/preload.png">
           <link rel="prefetch" href="/prefetch.css">
           <link rel="icon" href="/icon.png">
           <link rel="manifest" href="/manifest.json">"#,
        "",
        Duration::ZERO,
    )
    .await;
    for p in ["/preload.css", "/preload.png", "/prefetch.css", "/icon.png", "/manifest.json"] {
        assert_eq!(n(&c, p), 1, "Chrome fetches {p} exactly once; log: {}", dump(&c));
    }
}

/// A `<picture>` IS A CHOICE, NOT A LIST, and a responsive image is on most pages of the modern web
/// -- so getting this wrong is a 3x multiplier on image requests, not a corner case. Measured on
/// Chrome: `<source a><source b><img c>` requests `a` and nothing else; a picture with no `<source>`
/// requests its `<img>`; and a `<source>` naming a type Chrome cannot decode is skipped in favour of
/// the `<img>` behind it.
#[tokio::test]
async fn a_picture_fetches_exactly_one_candidate() {
    let (c, _) = run(
        "",
        r#"<picture><source srcset="/a.png" type="image/png"><source srcset="/b.png" type="image/webp"><img src="/c.png"></picture>
           <picture><img src="/lone.png"></picture>
           <picture><source srcset="/bad.xyz" type="image/xyz"><img src="/good.png"></picture>"#,
        Duration::ZERO,
    )
    .await;
    assert_eq!(n(&c, "/a.png"), 1, "the first usable <source> is the one fetched; log: {}", dump(&c));
    assert_eq!(n(&c, "/b.png"), 0, "a later <source> is not fetched; log: {}", dump(&c));
    assert_eq!(n(&c, "/c.png"), 0, "nor is the <img> fallback once a <source> was chosen; log: {}", dump(&c));
    assert_eq!(n(&c, "/lone.png"), 1, "a <picture> with no <source> fetches its <img>; log: {}", dump(&c));
    assert_eq!(n(&c, "/bad.xyz"), 0, "an undecodable <source type> is skipped; log: {}", dump(&c));
    assert_eq!(n(&c, "/good.png"), 1, "...and the <img> behind it is taken; log: {}", dump(&c));
}

/// `srcset` BEATS `src` on the SAME element. Chrome requests only the srcset candidate; fetching both
/// would double every responsive `<img>` on the web.
#[tokio::test]
async fn srcset_beats_src_on_the_same_element() {
    let (c, _) = run("", r#"<img src="/plain.png" srcset="/responsive.png 1x">"#, Duration::ZERO).await;
    assert_eq!(n(&c, "/responsive.png"), 1, "the srcset candidate is fetched; log: {}", dump(&c));
    assert_eq!(n(&c, "/plain.png"), 0, "and the `src` it overrides is not; log: {}", dump(&c));
}

/// A PARSED MEDIA BODY IS FETCHED, once, and so is its poster. Chrome requests both; this
/// sweep used to leave the media out -- the one place it knowingly put less on the wire than Chrome --
/// because there was no media element to hand the response to. The media element's own resource
/// selection (`core/html/media_load.rs`) loads it now, from the `src` or the first playable `<source>`,
/// and the kernel's loader reads the header without handing the body to the page's realm.
#[tokio::test]
async fn a_parsed_media_body_is_fetched_once_like_its_poster() {
    let (c, _) = run(
        "",
        r#"<video src="/movie.mp4" poster="/poster.png"></video>
           <audio src="/sound.mp3"></audio>
           <video><source src="/alt.webm" type="video/webm"></video>"#,
        Duration::ZERO,
    )
    .await;
    assert_eq!(n(&c, "/poster.png"), 1, "a <video poster> is an ordinary image and is fetched; log: {}", dump(&c));
    for p in ["/movie.mp4", "/sound.mp3", "/alt.webm"] {
        assert_eq!(n(&c, p), 1, "{p} is the media element's resource, requested once; log: {}", dump(&c));
    }
}

// ---- the negatives ----------------------------------------------------------------------------

/// NOTHING SPURIOUS. Each of these is a shape a real page carries in quantity, and each one Chrome
/// leaves alone -- measured, not assumed.
#[tokio::test]
async fn the_elements_chrome_does_not_fetch_produce_no_request_at_all() {
    let (c, _) = run(
        r#"<link rel="author" href="/author.css">
           <link rel="preload" href="/preload-no-as.css">
           <link rel="stylesheet">"#,
        r#"<img><img src=""><img src="   "><input type="text" src="/input-text.png"><iframe srcdoc="<p>x"></iframe>"#,
        Duration::ZERO,
    )
    .await;
    // `/page.html` is the only thing on this page an ELEMENT may request. `/favicon.ico` is the
    // browser's own request for a page that declares no icon, after `load` -- Chrome 150 makes it for
    // this page too (`core/html/link_icons.rs`); it is asserted by `which_subresources_load`.
    let extra: Vec<String> = c.keys().filter(|k| *k != "/page.html" && *k != "/favicon.ico").cloned().collect();
    assert!(
        extra.is_empty(),
        "none of these elements induces a request in Chrome; korvus requested {extra:?} (full log: {})",
        dump(&c)
    );
}

/// A `<script src>` the parser already executed is fetched ONCE, not twice. `load_page` fetches and
/// stages external classic scripts itself (document-order execution is staged in Rust), so a sweep
/// that also handed `<script>` to the element loader would double every script request on the web.
#[tokio::test]
async fn a_parser_executed_script_is_not_fetched_a_second_time() {
    let (c, _) = run(r#"<script src="/a.js"></script>"#, r#"<script src="/b.js"></script><script src="/a.js"></script>"#, Duration::ZERO).await;
    assert_eq!(n(&c, "/a.js"), 2, "two <script src> elements naming the same url are two fetches, one each; log: {}", dump(&c));
    assert_eq!(n(&c, "/b.js"), 1, "and one element is one fetch; log: {}", dump(&c));
}

// ---- the delay accounting the new requests now feed into --------------------------------------

/// THE SECOND-ORDER POINT. Now that the parser's resources are actually requested they take a
/// load-delay token, and the document's `load` must wait for them -- which is what Chrome does
/// (641 ms for an image, 645 ms for a stylesheet, against a 600 ms server).
#[tokio::test]
async fn a_parser_produced_resource_delays_the_load_event() {
    let (c, ms) = run(r#"<link rel="stylesheet" href="/slow.css">"#, r#"<img src="/slow.png">"#, Duration::ZERO).await;
    assert_eq!(n(&c, "/slow.css"), 1, "the stylesheet must have been requested; log: {}", dump(&c));
    assert_eq!(n(&c, "/slow.png"), 1, "the image must have been requested; log: {}", dump(&c));
    let ms = ms.expect("`load` must fire at all -- no answer here is a leaked delay token");
    assert!(ms >= HALF, "the document's load event must have waited for the parser's own resources: fired {ms}ms in, each takes {DELAY_MS}ms");
}

/// AND THE RELEASE. Every parser-produced load that FAILS gives its token back, so the document
/// still loads promptly instead of sitting at the 1000 ms cap -- which in a suite reads as a
/// timeout, i.e. no verdict rather than a wrong one.
#[tokio::test]
async fn failing_parser_produced_resources_release_their_tokens() {
    let (c, ms) = run(
        r#"<link rel="stylesheet" href="/missing.css"><script src="/missing.js"></script>"#,
        r#"<img src="/missing.png"><object data="/missing.bin"></object>"#,
        Duration::ZERO,
    )
    .await;
    for p in ["/missing.css", "/missing.png", "/missing.bin"] {
        assert_eq!(n(&c, p), 1, "{p} must have been requested (and 404ed); log: {}", dump(&c));
    }
    let ms = ms.expect("`load` must fire at all -- a leaked token shows up here as no load event");
    assert!(ms < HALF * 2, "four failed parser-produced subresources must not hold the document: load fired {ms}ms in (the cap is 1000ms)");
}

/// THE LINK RELS THAT ARE FETCHED BUT DO NOT WAIT. Chrome fires `load` at ~1 ms with all four of
/// these in flight against a 600 ms server; waiting for them would make this engine slower than the
/// web is, which is its own defect.
#[tokio::test]
async fn the_fetched_but_undelaying_link_rels_do_not_hold_the_load_event() {
    let (c, ms) = run(
        r#"<link rel="preload" as="style" href="/slow-preload.css">
           <link rel="prefetch" href="/slow-prefetch.css">
           <link rel="icon" href="/slow-icon.png">
           <link rel="manifest" href="/slow-manifest.json">"#,
        "",
        Duration::ZERO,
    )
    .await;
    for p in ["/slow-preload.css", "/slow-prefetch.css", "/slow-icon.png", "/slow-manifest.json"] {
        assert_eq!(n(&c, p), 1, "{p} must still be REQUESTED -- Chrome requests all four; log: {}", dump(&c));
    }
    let ms = ms.expect("`load` must fire at all");
    assert!(ms < HALF, "none of these delays the load event in Chrome; korvus fired {ms}ms in against a {DELAY_MS}ms server");
}

// ---- the rule the new load events made reachable ----------------------------------------------

/// A `load` EVENT NEVER REACHES THE WINDOW FROM AN ELEMENT -- not even a CAPTURING window listener.
///
/// DOM §2.9, "get the parent": «A document's get the parent algorithm, given an event, returns null
/// if event's type attribute value is "load"». So the propagation path of an element's `load` stops
/// at the Document, and `addEventListener("load", f, true)` on the window is called once, for the
/// window's own load, and never for a subresource's.
///
/// MEASURED, Chrome 150.0.7871.46 headless over loopback, on the fixture below: `f` fires ONCE, with
/// `target === window` and `readyState === "complete"`. Korvus fired it SEVEN times (three images,
/// two stylesheets, and the window twice).
///
/// THIS IS THE TEST THAT KEEPS THE ENGINE USABLE ON THE REAL WEB. `web-vitals` -- inside Next.js,
/// Vercel Analytics and GA4 -- is exactly:
///
///     o4 = e => document.readyState !== "complete"
///             ? addEventListener("load", () => o4(e), true)
///             : setTimeout(e)
///
/// Every element `load` that reaches the window re-registers a NEW closure and the next one runs all
/// of them, so the listener list DOUBLES per element load: five subresources take it from 1 to 32.
/// With W9.17's requests actually going out, `grok.com` (244 subresources) passed 100,000 listeners
/// and never loaded again -- `ok` became a 60 s timeout on grok.com and genspark.ai, measured on the
/// corpus. Both are back to ~7 s with this rule in place.
#[tokio::test]
async fn an_element_s_load_event_never_reaches_a_capturing_window_listener() {
    let head = r#"<script>
        window.__seen = [];
        window.__adds = 0;
        var o4 = function (e) {
          if (document.readyState !== "complete") {
            window.__adds++;
            addEventListener("load", function () { o4(e); }, true);
          }
        };
        addEventListener("load", function (ev) {
          window.__seen.push(((ev.target && ev.target.tagName) || "window") + ":" + document.readyState);
        }, true);
        o4(function () {});
      </script>
      <link rel="stylesheet" href="/s1.css"><link rel="stylesheet" href="/s2.css">"#;
    let body = r#"<img src="/i1.png"><img src="/i2.png"><img src="/i3.png">"#;

    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let doc = format!("<!doctype html><html><head>{head}</head><body>{body}</body></html>");
    tokio::spawn(serve(listener, doc.clone(), log.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;

    let seen = e.eval_value("seen", "JSON.stringify(window.__seen)".to_string()).expect("eval");
    let adds = e.eval_value("adds", "window.__adds".to_string()).expect("eval");
    assert_eq!(
        seen.as_str(),
        Some(r#"["window:complete"]"#),
        "a capturing window `load` listener must see exactly ONE event, the window's own (Chrome 150: [\"window:complete\"])"
    );
    assert_eq!(adds.as_u64(), Some(1), "and web-vitals' re-registration must happen exactly once (Chrome 150: 1); anything above 1 doubles per subresource");
    // The subresources really were fetched, so the five load events this asserts about existed.
    let counts: HashMap<String, usize> = {
        let mut c = HashMap::new();
        for p in log.lock().unwrap().iter() {
            *c.entry(p.clone()).or_default() += 1;
        }
        c
    };
    for p in ["/s1.css", "/s2.css", "/i1.png", "/i2.png", "/i3.png"] {
        assert_eq!(n(&counts, p), 1, "{p} must have been fetched -- otherwise this test proves nothing; log: {}", dump(&counts));
    }
}

/// A PAGE WITH NOTHING TO FETCH IS UNCHANGED: NO request at all, and an immediate load. (`load_page`
/// is handed the document's bytes, so the page itself never crosses the wire here -- which makes the
/// log a clean measure of what the ELEMENTS asked for and nothing else.)
#[tokio::test]
async fn a_page_with_no_subresources_still_requests_only_itself() {
    let (c, ms) = run("", "<p>hello</p>", Duration::ZERO).await;
    // Nothing but the favicon: a page that declares no icon gets `/favicon.ico` requested for it after
    // `load`, by Chrome 150 as by this engine -- never an element load.
    let elements: Vec<&String> = c.keys().filter(|k| *k != "/favicon.ico").collect();
    assert!(elements.is_empty(), "a page with no subresources must put no element load on the wire; it requested {}", dump(&c));
    let ms = ms.expect("`load` must fire at all");
    assert!(ms < HALF, "and it loads immediately: fired {ms}ms in");
}

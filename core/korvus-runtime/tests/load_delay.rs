//! "delaying the load event" for SUBRESOURCES, not only frames.
//!
//! HTML gives every Document a set of things that delay its `load` event, and a document's `load`
//! fires only once that set is empty. Until this unit exactly ONE thing ever joined it: a frame's
//! navigation (`core/korvus-webapi/src/core/html/subresource.rs`, `if framing { delay_load(...) }`).
//! Everything else a page pulled in was fetched with nothing waiting on it, so `load` fired whenever
//! the parse and the task queue happened to empty -- typically 4 ms in, before a single subresource
//! had landed.
//!
//! WHAT THAT COST, measured: `css-tricks.com` threw `Cannot read properties of undefined (reading
//! 'callApi')` out of its own `load` handler, because TrustArc's notice script INJECTS the script that
//! defines `truste.cma`, and korvus fired `load` in the gap. Headless Chrome on the same page logs no
//! such error.
//!
//! THE ORACLE IS CHROME, not the spec prose. Chrome 150.0.7871.46 headless, over a loopback server
//! that sleeps 600 ms per resource, on a page that timestamps its own `load` (a page with nothing to
//! wait for fires at 1 ms, so 600+ ms IS the delay): parser-inserted `<script src>` that injects a
//! second one 1245 ms · `<script async>` 642 ms · `<script defer>` 643 ms · injected `<script src>`
//! 645 ms · injected with `async = false` 645 ms · `<img>` 643 ms · `<link rel=stylesheet>` 646 ms ·
//! `<object>` 643 ms · `<embed>` 646 ms · SVG `<image>` 643 ms -- all delay. And the ones that do
//! NOT: `<link rel=prefetch>` 1 ms · `<link rel=icon>` 1 ms · `<link rel=manifest>` 1 ms.
//!
//! THE FAILURE MODE THESE TESTS EXIST FOR is not the delay, it is the RELEASE. Every token is a
//! chance to hang the document to the cap: a token taken on a path that returns early without
//! releasing, a token released twice, a token for a fetch whose completion never runs. A hung
//! document does not read as "wrong" in a suite, it reads as a timeout -- no verdict at all. So for
//! every resource kind that takes a token there is a test here where the fetch FAILS and `load` still
//! fires promptly, plus one where a fetch has NOT answered yet and `load` waits for it -- as Chrome
//! waits (there is no deadline; `tests/load_delay_chrome_live.rs` carries the table).
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// How long the server sleeps before answering a `/slow*` route. Comfortably above the few
/// milliseconds an undelayed `load` takes and comfortably below the 1000 ms cap, so "delayed" and
/// "not delayed" are never in doubt.
const DELAY_MS: u64 = 300;

/// The midpoint the assertions split on.
const HALF: u128 = DELAY_MS as u128 / 2;

/// A 1x1 transparent PNG, so an `<img>` has real bytes to receive.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xff, 0xff, 0x03, 0x00, 0x00, 0x06, 0x00, 0x05, 0x57, 0x0b, 0x7c, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
    0x60, 0x82,
];

/// `(status, content-type, body, delay)` for a request path. `None` means "hold the connection open
/// and never answer". `/held*` is answered only once the test opens the server's gate.
fn route(path: &str) -> Option<(u16, &'static str, Vec<u8>, u64)> {
    Some(match path {
        // Sets a global, after a delay. The one an injected `<script src>` points at.
        "/slow.js" => (200, "text/javascript", b"window.__INJ = 1;".to_vec(), DELAY_MS),
        // Injects `/slow.js`: the css-tricks shape -- a script that arrives late and whose own job is
        // to add another script.
        "/chain.js" => (
            200,
            "text/javascript",
            b"window.__CHAIN = 1; var s = document.createElement('script'); s.src = '/slow.js'; document.head.appendChild(s);".to_vec(),
            DELAY_MS,
        ),
        "/slow.png" => (200, "image/png", PNG.to_vec(), DELAY_MS),
        "/slow.css" => (200, "text/css", b"#probe { color: rgb(1, 2, 3) }".to_vec(), DELAY_MS),
        "/held.js" => (200, "text/javascript", b"window.__INJ = 1;".to_vec(), 0),
        "/held.png" => (200, "image/png", PNG.to_vec(), 0),
        "/never" => return None,
        // Everything else is an immediate 404 -- the failure side of every path above.
        _ => (404, "text/plain", b"no".to_vec(), 0),
    })
}

/// A path-routing loopback server. One task per connection, so several delayed resources are in
/// flight at once (a serial accept loop would serialise the delays and make a 300 ms answer look
/// like 900 ms for reasons that have nothing to do with the load event).
async fn serve(listener: TcpListener) {
    serve_gated(listener, std::sync::Arc::new(tokio::sync::Semaphore::new(0))).await;
}

/// [`serve`], with a gate the test opens (`add_permits`) to let a `/held*` response go out.
async fn serve_gated(listener: TcpListener, gate: std::sync::Arc<tokio::sync::Semaphore>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let gate = gate.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if path.starts_with("/held") {
                let _ = gate.acquire().await;
            }
            let Some((status, ctype, body, delay)) = route(&path) else {
                // Never answer, and never close: this is what the cap is measured against. Park the
                // task forever; the test's Engine is dropped long before this matters.
                std::future::pending::<()>().await;
                return;
            };
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
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

/// The page's own stopwatch plus the facts its `load` handler can see. `__t0` is stamped by the first
/// inline script -- i.e. once the document exists -- and `__ms` by the `load` listener, so the number
/// asserted on is the page's own answer to "how long did my load event wait", exactly the number the
/// Chrome rig above prints.
const PROBE: &str = r#"<script>window.__t0 = Date.now(); window.__loads = 0;</script>
<script>
addEventListener("load", function () {
  window.__loads++;
  window.__ms = Date.now() - window.__t0;
  window.__at = { inj: !!window.__INJ, chain: !!window.__CHAIN,
                  imgload: !!window.__IMGL, imgerr: !!window.__IMGE,
                  cssload: !!window.__CSSL, scripterr: !!window.__SERR,
                  ready: document.readyState === "complete",
                  spinning: window.__SPINS > 0 };
});
</script>"#;

/// Load `html` and pump until the page's `load` event fires (or 5 s). Returns `(ms_at_load, facts)`;
/// `ms_at_load` is `None` when `load` never fired at all, which is the outcome a leaked token
/// produces and must never be silently read as a pass.
async fn run_page(html: &str) -> (Option<u128>, serde_json::Value) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    let base = format!("http://{addr}/page.html");

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("<!doctype html><html><head>{PROBE}</head><body>{html}</body></html>");
    e.load_page(&doc, &base, &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");

    let ms = e.eval_value("chk-ms", "window.__ms === undefined ? null : window.__ms".to_string()).expect("eval");
    let at = e.eval_value("chk-at", "window.__at || null".to_string()).expect("eval");
    (ms.as_u64().map(u128::from), at)
}

fn fact(at: &serde_json::Value, key: &str) -> bool {
    at.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

// ---- the delay side ---------------------------------------------------------------------------

/// THE MEASUREMENT THIS UNIT EXISTS FOR. A script injected by another script has RUN by the time
/// `load` fires -- Chrome 150 answers `true` here and korvus answered `false`, which is the whole of
/// the css-tricks failure.
#[tokio::test]
async fn an_injected_script_has_run_by_the_load_event() {
    let (ms, at) = run_page(r#"<script>
      var s = document.createElement("script"); s.src = "/slow.js"; document.head.appendChild(s);
    </script>"#)
    .await;
    let ms = ms.expect("`load` must fire at all");
    assert!(fact(&at, "inj"), "an injected <script src> must have RUN by the load event (Chrome: true); got {at}");
    assert!(ms >= HALF, "the load event must have WAITED for it, not raced it: fired {ms}ms in, resource takes {DELAY_MS}ms");
}

/// The css-tricks shape end to end: a late script whose own job is to add another late script. Both
/// have run by `load` (Chrome: 1245 ms for the 600 ms rig, i.e. it waited for BOTH in series).
#[tokio::test]
async fn a_chain_of_injected_scripts_is_complete_by_the_load_event() {
    let (ms, at) = run_page(r#"<script>
      var s = document.createElement("script"); s.src = "/chain.js"; document.head.appendChild(s);
    </script>"#)
    .await;
    let ms = ms.expect("`load` must fire at all");
    assert!(fact(&at, "chain"), "the first injected script must have run; got {at}");
    assert!(fact(&at, "inj"), "the script the FIRST one injected must also have run by the load event; got {at}");
    assert!(ms >= HALF * 2, "the load event must have waited for both hops in series: fired {ms}ms in");
}

/// An injected `<img>` fires its own `load` BEFORE the document's (Chrome: 643 ms).
#[tokio::test]
async fn an_injected_image_delays_the_load_event() {
    let (ms, at) = run_page(r#"<script>
      var i = document.createElement("img");
      i.onload = function () { window.__IMGL = 1; };
      i.onerror = function () { window.__IMGE = 1; };
      i.src = "/slow.png"; document.head.appendChild(i);
    </script>"#)
    .await;
    let ms = ms.expect("`load` must fire at all");
    assert!(fact(&at, "imgload"), "the image's own load event must have fired first; got {at}");
    assert!(ms >= HALF, "the document's load event must have waited for the image: fired {ms}ms in");
}

/// An injected `<link rel=stylesheet>` delays it too (Chrome: 646 ms).
#[tokio::test]
async fn an_injected_stylesheet_delays_the_load_event() {
    let (ms, at) = run_page(r#"<script>
      var l = document.createElement("link"); l.rel = "stylesheet";
      l.onload = function () { window.__CSSL = 1; };
      l.href = "/slow.css"; document.head.appendChild(l);
    </script>"#)
    .await;
    let ms = ms.expect("`load` must fire at all");
    assert!(fact(&at, "cssload"), "the stylesheet's own load event must have fired first; got {at}");
    assert!(ms >= HALF, "the document's load event must have waited for the stylesheet: fired {ms}ms in");
}

// ---- the NOT-delayed side ---------------------------------------------------------------------

/// A `<link>` that is not a stylesheet is a hint or metadata, and Chrome does not wait for one:
/// `rel=prefetch`, `rel=icon` and `rel=manifest` all fire `load` at 1 ms against the 600 ms rig.
/// Waiting for them would be a slower engine than the web is, which is its own defect.
#[tokio::test]
async fn a_link_that_is_not_a_stylesheet_does_not_delay_the_load_event() {
    for rel in ["prefetch", "icon", "manifest"] {
        let (ms, _) = run_page(&format!(
            r#"<script>
              var l = document.createElement("link"); l.rel = "{rel}";
              l.href = "/slow.css"; document.head.appendChild(l);
            </script>"#
        ))
        .await;
        let ms = ms.expect("`load` must fire at all");
        assert!(ms < HALF, "<link rel={rel}> must NOT delay the load event (Chrome does not): fired {ms}ms in, resource takes {DELAY_MS}ms");
    }
}

// ---- THE RELEASE SIDE: one failing fetch per resource kind ------------------------------------

/// EVERY resource kind that takes a token, failing. A 404 ends the fetch exactly as a 200 does, so
/// every token comes back and `load` fires promptly -- if any one of them leaked, this test would not
/// fail, it would sit at the 1000 ms cap, and that is what the millisecond bound is for.
#[tokio::test]
async fn a_failed_fetch_releases_its_token_and_the_load_event_fires_promptly() {
    let (ms, at) = run_page(r#"<script>
      var i = document.createElement("img");
      i.onload = function () { window.__IMGL = 1; };
      i.onerror = function () { window.__IMGE = 1; };
      i.src = "/missing.png"; document.head.appendChild(i);
      var l = document.createElement("link"); l.rel = "stylesheet";
      l.onload = function () { window.__CSSL = 1; };
      l.href = "/missing.css"; document.head.appendChild(l);
      var s = document.createElement("script");
      s.onerror = function () { window.__SERR = 1; };
      s.src = "/missing.js"; document.head.appendChild(s);
      var o = document.createElement("object"); o.data = "/missing.bin";
      document.documentElement.appendChild(o);
    </script>"#)
    .await;
    let ms = ms.expect("`load` must fire at all -- a leaked token shows up here as no load event");
    assert!(fact(&at, "imgerr"), "the failed image must have fired `error` before the document's load; got {at}");
    assert!(
        ms < HALF * 2,
        "four failed subresources must not hold the document: load fired {ms}ms in (the cap is 1000ms -- anything near it is a leaked token)"
    );
}

/// A FETCH THAT HAS NOT ANSWERED HOLDS `load` FOR AS LONG AS IT HAS NOT ANSWERED.
///
/// This test used to be the opposite one -- `a_fetch_that_never_settles_cannot_withhold_the_load_event`,
/// asserting `load` at a 1000 ms cap -- and Chrome disagrees: Chrome 150 still had `readyState`
/// `interactive` 20 s after inserting a script whose response never came, and ran a 6 s one before
/// `load`. The pump budget below is past the old cap; the image is the resource kind so the
/// script-shaped case in `load_delay_chrome_live.rs` is not merely repeated.
#[tokio::test]
async fn an_unanswered_image_withholds_the_load_event_until_it_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    tokio::spawn(serve_gated(listener, gate.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var i = document.createElement('img'); i.onload = function () {{ window.__IMGL = 1; }};\
         i.src = '/held.png'; document.head.appendChild(i);\
         </script></body></html>"
    );
    e.load_page(&doc, &format!("http://{addr}/held.html"), &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_millis(2500)).await.expect("pump");
    assert!(!e.load_fired(), "`load` must not fire while the image has not answered");
    let loads = e.eval_value("chk-loads", "window.__loads".to_string()).expect("eval");
    assert_eq!(loads.as_u64(), Some(0), "no load event yet");

    gate.add_permits(1);
    e.pump_until_loaded(Duration::from_secs(10)).await.expect("pump");
    let at = e.eval_value("chk-at", "window.__at || null".to_string()).expect("eval");
    assert!(e.load_fired(), "once the image answers, `load` fires");
    assert!(fact(&at, "imgload"), "and the image's own `load` came first; got {at}");
}

/// A SECOND NAVIGATION IN THE SAME ISOLATE STARTS WITH AN EMPTY SET. The delay set lives on the
/// isolate's `BrowsingContexts`, not on the document, so a load the first page started and never
/// finished used to still be holding a token when the second page parked its own load step behind it
/// -- one page's dead fetch silently capping the next page's load event. Creating a document clears
/// the set (`set_doc_readiness(Loading, notify=false)`).
#[tokio::test]
async fn a_new_document_does_not_inherit_the_previous_one_s_delays() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");

    // Page one leaves a fetch that never settles in flight, and is capped.
    let one = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var s = document.createElement('script'); s.src = '/never'; document.head.appendChild(s);\
         </script></body></html>"
    );
    e.load_page(&one, &format!("http://{addr}/one.html"), &[]).await.expect("load one");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump one");

    // Page two has nothing to wait for and must say so.
    let two = format!("<!doctype html><html><head>{PROBE}</head><body>hi</body></html>");
    e.load_page(&two, &format!("http://{addr}/two.html"), &[]).await.expect("load two");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump two");

    let ms = e.eval_value("chk", "window.__ms === undefined ? null : window.__ms".to_string()).expect("eval");
    let ms = ms.as_u64().expect("page two's `load` must fire");
    assert!(
        u128::from(ms) < HALF,
        "page two delays on nothing and must load immediately; it fired {ms}ms in, which is page one's dead fetch still holding a token"
    );
}

// ---- THE CAP IS A HOST DEADLINE, NOT A PAGE TIMER --------------------------------
//
// Everything above this line measures a cap that a `setTimeout` could enforce, because in every one
// of those pages the event loop still gets its turn. The corpus found the page where it does not:
// `rakuten.co.jp` reads `document.readyState` 1,058,730 times in four seconds waiting for
// `complete`, and `complete` arrives only from the load step, and the load step was parked behind a
// cap that was itself a page timer. A 3000 ms timer registered before that page's scripts ran never
// fired at all, so no cap VALUE could have helped -- 1000 / 500 / 300 / 150 ms were all measured and
// all ended in the same 60 s process timeout, with `rakuten.co.jp`, `oxylabs.io` and
// `kotlinlang.org` leaving the corpus as `errored`.
//
// The cap became an instant on the host's clock, and later it stopped being a
// deadline at all: Chrome waits for its subresources however long they take. What is left is the
// one thing that cannot wait -- a page holding the isolate so the loop never takes a turn. `Engine::pump`
// stamps the loop's turns, and a watchdog thread's V8 interrupt (the only thing that reaches inside a
// synchronous spin) releases the load step when the stamps stop.

/// A page that spins on `document.readyState` and never returns to the event loop. It cannot stop
/// spinning until `load` fires, and `load` cannot fire until something outside its queue makes it --
/// which is the whole of the rakuten shape, reduced to eight lines.
///
/// THE SPIN IS BOUNDED at 20 s so a broken cap FAILS this test instead of hanging the test binary
/// forever; 20 s is twenty times the cap, so the bound can never be what ends a working run.
const SPINNER: &str = r#"<script>
  var s = document.createElement("script"); s.src = "/never"; document.head.appendChild(s);
  setTimeout(function () {
    window.__SPINS = 0;
    var t = Date.now();
    while (document.readyState !== "complete" && Date.now() - t < 20000) { window.__SPINS++; }
    window.__SPUN = Date.now() - t;
  }, 0);
</script>"#;

/// THE MEASUREMENT THIS UNIT EXISTS FOR. `load` arrives on a page that never yields to a timer.
#[tokio::test]
async fn the_cap_fires_on_a_page_that_never_yields_to_a_timer() {
    let (ms, at) = run_page(SPINNER).await;
    let ms = ms.expect("the cap must fire `load` on a page that starves its own task queue");
    assert!(
        fact(&at, "spinning"),
        "the fixture must really have been spinning when `load` fired -- otherwise this test proves nothing about starvation; got {at}"
    );
    assert!(fact(&at, "ready"), "`document.readyState` must be `complete` by the time `load` fires; got {at}");
    assert!(ms >= HALF, "it did wait for the resource first: fired {ms}ms in");
    assert!(ms < 3000, "and it waited only to the cap: fired {ms}ms in (20000 means the spin's own bound ended it, i.e. no cap fired)");
}

/// The spin's own escape, measured from the page's side: it stopped because `readyState` became
/// `complete`, not because it ran out of its 20 s bound.
#[tokio::test]
async fn the_spinning_page_is_released_by_the_cap_and_runs_on() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("<!doctype html><html><head>{PROBE}</head><body>{SPINNER}</body></html>");
    e.load_page(&doc, &format!("http://{addr}/spin.html"), &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(8)).await.expect("pump");

    let spun = e.eval_value("chk-spun", "window.__SPUN === undefined ? null : window.__SPUN".to_string()).expect("eval");
    let spun = spun.as_u64().expect("the spinning callback must have RETURNED, not still be running");
    assert!(spun < 5000, "the spin ended because readyState went `complete`, not on its own 20000ms bound: it ran {spun}ms");
    let spins = e.eval_value("chk-spins", "window.__SPINS || 0".to_string()).expect("eval");
    assert!(spins.as_u64().unwrap_or(0) > 1000, "the fixture must have spun hard enough to starve a timer; it looped {spins} times");
    let ready = e.eval_value("chk-ready", "document.readyState".to_string()).expect("eval");
    assert_eq!(ready.as_str(), Some("complete"), "the released document must end `complete`, exactly as a natural drain leaves it");
}

/// AN IDLE DOCUMENT LOADS INSIDE ONE PUMP WHEN ITS LAST FETCH ANSWERS. A document whose only pending
/// work is a fetch runs NO JavaScript while it waits, so nothing but the pump itself can carry the
/// completion to the load step -- and no watchdog may fire in the meantime, because the loop is
/// taking turns the whole time (it is waiting, not starved).
///
/// It pumps with NO probe in between: `pump_until_loaded` evaluates a completion probe every 25 ms,
/// and `korvus fetch` does not -- it makes one `pump(--wait)` after `load_page` -- so this is the shape
/// of the CLI. This test used to assert the opposite (`load` at a 1000 ms cap inside the pump); Chrome
/// waits.
#[tokio::test]
async fn an_idle_document_loads_inside_one_pump_once_its_fetch_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    tokio::spawn(serve_gated(listener, gate.clone()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var s = document.createElement('script'); s.src = '/held.js'; document.head.appendChild(s);\
         </script></body></html>"
    );
    e.load_page(&doc, &format!("http://{addr}/idle.html"), &[]).await.expect("load_page");
    let end = e.pump(Duration::from_millis(2500)).await.expect("pump");
    assert_eq!(end, engine::PumpEnd::BudgetElapsed, "a fetch in flight is scheduled work, not a drained loop");
    assert!(!e.load_fired(), "an idle document waiting on a fetch must not load before the fetch answers");

    gate.add_permits(1);
    let _ = e.pump(Duration::from_secs(5)).await;
    assert!(e.load_fired(), "`load` must have fired DURING the pump, with no probe to help it");
    let at = e.eval_value("chk-at", "window.__at || null".to_string()).expect("eval");
    assert!(fact(&at, "inj"), "the script ran before `load`; got {at}");
    assert!(fact(&at, "ready"), "`readyState` was `complete` at `load`; got {at}");
}

/// A STARVATION RELEASE LEAVES THE STATE A NATURAL DRAIN LEAVES. One `load` event, not two: the
/// release takes the waiter before it calls it, so the token of a fetch that answered DURING the spin
/// -- and whose completion runs only once the spin has ended -- cannot run the load step again.
#[tokio::test]
async fn a_starvation_release_fires_exactly_one_load_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    // `/slow.js` answers 300 ms in, but the page is spinning by then and its completion cannot run:
    // only the watchdog can fire `load`, and the late release must not fire a second one.
    let doc = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var b = document.createElement('script'); b.src = '/slow.js'; document.head.appendChild(b);\
         setTimeout(function () {{\
           window.__SPINS = 0; var t = Date.now();\
           while (document.readyState !== 'complete' && Date.now() - t < 20000) {{ window.__SPINS++; }}\
           window.__SPUN = Date.now() - t;\
         }}, 0);\
         </script></body></html>"
    );
    e.load_page(&doc, &format!("http://{addr}/one-load.html"), &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(10)).await.expect("pump");
    // Keep pumping so the late completion, and any second load event, has every chance to show up.
    for _ in 0..40 {
        let _ = e.pump(Duration::from_millis(50)).await;
        if e.eval_value("chk-inj", "!!window.__INJ".to_string()).expect("eval").as_bool() == Some(true) {
            break;
        }
    }
    let _ = e.pump(Duration::from_millis(200)).await;
    let at = e.eval_value("chk-at", "window.__at || null".to_string()).expect("eval");
    assert!(fact(&at, "spinning"), "the fixture must have been spinning when `load` fired; got {at}");
    assert!(!fact(&at, "inj"), "the script's completion could not have run inside the spin; got {at}");
    let inj = e.eval_value("chk-inj", "!!window.__INJ".to_string()).expect("eval");
    assert_eq!(inj.as_bool(), Some(true), "the late script still runs once the spin has ended");
    let loads = e.eval_value("chk-loads", "window.__loads || 0".to_string()).expect("eval");
    assert_eq!(loads.as_u64(), Some(1), "a starvation release must fire exactly one `load` event; got {loads}");
    // The ENGINE's own marker, not a page eval: `__loadFired` stopped being a bridge slot,
    // and `pump_until_loaded` reads this very field.
    assert!(e.load_fired(), "the release must set the same load marker a natural drain sets");
}

/// THE CAP DOES NOT FIRE EARLY ON A HEALTHY PAGE. A 300 ms resource is released by its own fetch,
/// not by the 1000 ms cap -- and the proof is that the script RAN: a capped release clears the delay
/// set and fires `load` with the resource still in flight, so `inj` would be false.
#[tokio::test]
async fn the_cap_does_not_fire_early_on_a_healthy_page() {
    let (ms, at) = run_page(
        r#"<script>
      var s = document.createElement("script"); s.src = "/slow.js"; document.head.appendChild(s);
    </script>"#,
    )
    .await;
    let ms = ms.expect("`load` must fire at all");
    assert!(fact(&at, "inj"), "the resource must have ARRIVED before `load` -- a cap firing early would leave it in flight; got {at}");
    assert!(ms >= HALF, "the load event waited for the resource: fired {ms}ms in");
    assert!(ms < 900, "and it did NOT wait for the cap: fired {ms}ms in, which is the 1000ms cap rather than the {DELAY_MS}ms resource");
}

/// A PAGE WITH NOTHING TO WAIT FOR IS UNCHANGED. No delay set, so no deadline, so no host-side wait:
/// `load` on the first task, exactly as before any of this existed.
#[tokio::test]
async fn a_page_with_nothing_to_wait_for_arms_no_cap_at_all() {
    let (ms, _) = run_page("hello").await;
    let ms = ms.expect("`load` must fire at all");
    assert!(ms < HALF, "a page that delays on nothing must load immediately; it fired {ms}ms in");
}

/// A SECOND NAVIGATION MUST NOT INHERIT A PARKED LOAD STEP. Page one never loads -- its script is
/// never answered, so its step stays parked with the starvation watchdog armed; if page two inherited
/// either, page two's step would be run by page one's state rather than by its own resource.
///
/// This is the parked-step half of `a_new_document_does_not_inherit_the_previous_one_s_delays`, which
/// covers the token half. (It was `..._cap_deadline` while the step had a deadline.)
#[tokio::test]
async fn a_new_document_does_not_inherit_the_previous_one_s_parked_load_step() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");

    let one = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var s = document.createElement('script'); s.src = '/held.js'; document.head.appendChild(s);\
         </script></body></html>"
    );
    e.load_page(&one, &format!("http://{addr}/one.html"), &[]).await.expect("load one");
    e.pump_until_loaded(Duration::from_millis(1500)).await.expect("pump one");
    assert!(!e.load_fired(), "page one must still be waiting on its unanswered script");

    // Page two has a resource of its own and must wait for IT.
    let two = format!(
        "<!doctype html><html><head>{PROBE}</head><body><script>\
         var s = document.createElement('script'); s.src = '/slow.js'; document.head.appendChild(s);\
         </script></body></html>"
    );
    e.load_page(&two, &format!("http://{addr}/two.html"), &[]).await.expect("load two");
    e.pump_until_loaded(Duration::from_secs(10)).await.expect("pump two");

    assert!(e.load_fired(), "page two's `load` must fire");
    let at = e.eval_value("chk-two-at", "window.__at || null".to_string()).expect("eval");
    assert!(fact(&at, "inj"), "page two must have waited for its OWN resource: {at}");
}

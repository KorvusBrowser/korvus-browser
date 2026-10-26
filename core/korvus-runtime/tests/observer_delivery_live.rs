//! `IntersectionObserver` and `ResizeObserver` call back, with Chrome's entries, in
//! Chrome's order, and cost nothing when nothing is observed.
//!
//! # The oracle
//!
//! `fixtures/p12c/probe.js` is evaluated UNMODIFIED here and in stock Chrome 150.0.7871.46 by
//! `fixtures/p12c/record.py`, which launches the binary itself (`--remote-debugging-port`, a throwaway
//! profile, headed under Xvfb at 1920x1080), attaches with `suppress_origin=True`, and runs
//! each section on a fresh browser against an empty loopback document, one `Runtime.evaluate
//! (awaitPromise)` per phase. `chrome-150.<section>.txt` is what it wrote; two consecutive recordings
//! were byte-identical. The probe prints counts, orders and values, never a time, and prints
//! viewport-relative numbers symbolically (`VW`, `VH-50`) because the two engines' windows differ.
//!
//! # The same client, here
//!
//! Each phase is driven the way `korvus serve` drives a `Runtime.evaluate`: a bounded [`Engine::pump`]
//! first (`plugins/korvus-cdp`'s `EVAL_PUMP_MS_DEFAULT`, 100 ms), then the awaited evaluation. That
//! pump is the host's rendering opportunity: a scroll made by the previous phase is noticed there,
//! exactly as it is between two CDP commands.
//!
//! # Before this unit
//!
//! Every section's callbacks were absent: `io [] ro 0` where Chrome answers
//! `io ["far:false","t:true"] ro 1` (the brief's measurement, reproduced by `chrome-150.io-basic.txt`).

use engine::{Engine, PumpEnd};

const PROBE: &str = include_str!("fixtures/p12c/probe.js");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/p12c");
const DOC: &[u8] = b"<!doctype html><html><head></head><body></body></html>";
/// `plugins/korvus-cdp`'s bounded pump before every `Runtime.evaluate`.
const EVAL_PUMP: std::time::Duration = std::time::Duration::from_millis(100);

/// One HTTP/1.1 exchange: `page.html` is the empty document, everything else is a 404.
fn serve_one(mut s: std::net::TcpStream) {
    use std::io::{Read, Write};
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let ok = head.split_whitespace().nth(1).is_some_and(|p| p.split('?').next() == Some("/page.html"));
    let body: &[u8] = if ok { DOC } else { b"not found" };
    let h = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        if ok { "200 OK" } else { "404 Not Found" },
        if ok { "text/html" } else { "text/plain" },
        body.len()
    );
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

/// A loopback server on its own OS thread, so nothing it does waits on the isolate's thread.
fn start_server() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || serve_one(s));
        }
    });
    addr
}

/// A fresh engine on the empty loopback document -- the page every fixture was recorded on.
async fn served_page() -> Engine {
    let addr = start_server();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    let url = format!("http://{addr}/page.html");
    e.set_url(&url);
    e.load_page(std::str::from_utf8(DOC).unwrap(), &url, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(std::time::Duration::from_secs(5)).await;
    e
}

/// One `Runtime.evaluate (awaitPromise, returnByValue)`, as `korvus serve` performs it.
async fn evaluate(e: &mut Engine, expr: String) -> String {
    let _ = e.pump(EVAL_PUMP).await;
    let out = e.eval_remote_awaited_details(expr, true).await;
    if let Some(x) = out.exception {
        return format!("EVAL THREW {}", x.text);
    }
    match out.result {
        engine::RemoteResult::Value(serde_json::Value::String(s)) => s,
        engine::RemoteResult::Value(v) => v.to_string(),
        engine::RemoteResult::Object(_) => "<object>".to_string(),
    }
}

/// Every phase of `section`, joined the way `record.py` writes them.
async fn run_section(section: &str, phases: u32) -> String {
    let mut e = served_page().await;
    let mut lines = Vec::new();
    for phase in 0..phases {
        let expr = format!("({PROBE})({}, {phase})", serde_json::to_string(section).unwrap());
        lines.push(format!("-- phase {phase}"));
        lines.push(evaluate(&mut e, expr).await);
    }
    lines.join("\n")
}

fn fixture(section: &str) -> String {
    let p = std::path::Path::new(FIXTURES).join(format!("chrome-150.{section}.txt"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())).trim_end().to_string()
}

/// Line-for-line equality with Chrome's recording.
fn assert_lines(section: &str, got: &str) {
    let want = fixture(section);
    let g: Vec<&str> = got.lines().collect();
    let w: Vec<&str> = want.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..g.len().max(w.len()) {
        let (a, b) = (w.get(i).copied().unwrap_or("<missing>"), g.get(i).copied().unwrap_or("<missing>"));
        if a != b {
            diffs.push(format!("  chrome: {a}\n  korvus: {b}"));
        }
    }
    assert!(diffs.is_empty(), "[{section}] {} of {} lines differ:\n{}", diffs.len(), w.len(), diffs.join("\n"));
}

/// The brief's case, and what follows it: the initial notification for an in-viewport and a far target
/// (one callback, both entries, their rects and `rootBounds`), no second notification while nothing
/// moves, a scroll that swaps them, `unobserve` and `disconnect` that stop what they name.
#[tokio::test]
async fn intersection_initial_notification_and_scroll_match_chrome() {
    assert_lines("io-basic", &run_section("io-basic", 5).await);
}

/// `IntersectionObserverInit` read like Chrome: sorted thresholds and their RangeError, `rootMargin`
/// and `scrollMargin` in pixels or percent, `root` an Element or a Document, `trackVisibility` refused
/// without `delay >= 100`, `observe`/`unobserve` of a non-Element.
#[tokio::test]
async fn intersection_observer_options_match_chrome() {
    assert_lines("io-options", &run_section("io-options", 1).await);
}

/// Margins (px, negative, percent), thresholds crossed, a zero-area target, `display: none`,
/// `visibility: hidden`, a detached target, an `overflow: hidden` clip, an explicit scrolling root and
/// a target outside it, `root: document` -- and the updates when each changes.
#[tokio::test]
async fn intersection_geometry_matches_chrome() {
    assert_lines("io-geom", &run_section("io-geom", 3).await);
}

/// A frame's document as the root of a target in the parent: never intersecting, all zeros.
#[tokio::test]
async fn a_foreign_document_root_matches_chrome() {
    assert_lines("io-frame", &run_section("io-frame", 1).await);
}

/// Content, border and device-pixel boxes; the initial callback for every target including the ones
/// with no box; a resize reported and a move not; `unobserve` and `disconnect`.
#[tokio::test]
async fn resize_observer_boxes_match_chrome() {
    assert_lines("ro", &run_section("ro", 4).await);
}

/// The depth loop and "ResizeObserver loop completed with undelivered notifications.", in Chrome's
/// sequence of callbacks and error events.
#[tokio::test]
async fn resize_observer_loop_limit_matches_chrome() {
    assert_lines("ro-loop", &run_section("ro-loop", 1).await);
}

/// Where the callbacks run: after the animation frames requested in the same script, resize before
/// intersection, each intersection observer's microtasks drained before the next is called, and a
/// timeout queued from a callback after all of them.
#[tokio::test]
async fn delivery_order_matches_chrome() {
    assert_lines("order", &run_section("order", 1).await);
}

// ── the efficiency box ───────────────────────────────────────────────────────────────────────────
//
// A frame is a task on the realm's timer task source, and every timer the realm arms -- the page's or
// the engine's -- takes the next id from ONE counter (`scheduler::Scheduler::next_id`). So the gap
// between two `setTimeout` ids the page takes is the exact number of timers armed in between: a count,
// not a clock.

/// The id `setTimeout` hands back now (the timer itself does nothing).
async fn next_timer_id(e: &mut Engine) -> u64 {
    let v = e.eval_value("p12c-timer-id", "setTimeout(() => {}, 0)".to_string()).unwrap();
    let _ = e.pump(std::time::Duration::from_millis(10)).await;
    v.as_u64().expect("a timer id")
}

/// A page with NO observers, mutated and restyled: not one frame is armed, and the loop drains.
#[tokio::test]
async fn no_observer_means_no_frame() {
    let mut e = served_page().await;
    let a = next_timer_id(&mut e).await;
    e.eval_value(
        "p12c-mutate",
        "for (let i = 0; i < 50; i++) { const d = document.createElement('div'); d.style.height = i + 'px'; document.body.appendChild(d); } document.body.style.width = '300px'; 1".to_string(),
    )
    .unwrap();
    for _ in 0..3 {
        assert_eq!(e.pump(std::time::Duration::from_secs(2)).await.unwrap(), PumpEnd::Drained);
    }
    let b = next_timer_id(&mut e).await;
    assert_eq!(b, a + 1, "timers armed between two page timers on a page with no observer");
}

/// A page WITH observers that has settled arms nothing more: the loop drains and the timer counter does
/// not move. A later resize is picked up at the host's next rendering opportunity and delivered.
#[tokio::test]
async fn a_settled_observed_page_is_idle_and_wakes_on_change() {
    let mut e = served_page().await;
    e.eval_value(
        "p12c-observe",
        r#"globalThis.__ro = 0; globalThis.__io = 0;
           const t = document.createElement('div'); t.id = 't'; t.style.width = '100px'; t.style.height = '20px';
           document.body.appendChild(t);
           new ResizeObserver(() => { globalThis.__ro++; }).observe(t);
           new IntersectionObserver(() => { globalThis.__io++; }).observe(t); 1"#
            .to_string(),
    )
    .unwrap();
    // Until both initial notifications have run and the frames have stopped.
    let mut settled = false;
    for _ in 0..50 {
        if e.pump(std::time::Duration::from_millis(200)).await.unwrap() == PumpEnd::Drained {
            settled = true;
            break;
        }
    }
    assert!(settled, "an observed page that stopped changing must let the event loop drain");
    assert_eq!(e.eval_value("p12c-counts", "__ro + ',' + __io".to_string()).unwrap(), "1,1");

    let a = next_timer_id(&mut e).await;
    for _ in 0..3 {
        assert_eq!(e.pump(std::time::Duration::from_secs(2)).await.unwrap(), PumpEnd::Drained);
    }
    let b = next_timer_id(&mut e).await;
    assert_eq!(b, a + 1, "timers armed while an observed page sat idle");

    e.eval_value("p12c-resize", "document.getElementById('t').style.width = '150px'; 1".to_string()).unwrap();
    for _ in 0..50 {
        if e.pump(std::time::Duration::from_millis(200)).await.unwrap() == PumpEnd::Drained {
            break;
        }
    }
    assert_eq!(e.eval_value("p12c-counts2", "__ro + ',' + __io".to_string()).unwrap(), "2,1", "the resize is delivered, the unchanged intersection is not");
}

/// Pump until the loop drains (or ten seconds pass): every frame the last change asked for has run.
async fn drain(e: &mut Engine) {
    for _ in 0..50 {
        if e.pump(std::time::Duration::from_millis(200)).await.unwrap() == PumpEnd::Drained {
            break;
        }
    }
}

/// The entries delivered since the last call, each as `{id, got, want}`: what the entry says, and what the
/// layout members answer for the same target at delivery (`__oracle`).
fn take_entries(e: &mut Engine, label: &'static str) -> Vec<serde_json::Value> {
    let v = e.eval_value(label, "JSON.stringify(globalThis.__log.splice(0))".to_string()).unwrap();
    serde_json::from_str(v.as_str().expect("a JSON string")).unwrap()
}

/// INTERSECTIONS UNDER NESTED SCROLL CONTAINERS answer what the layout members answer at delivery: an
/// entry's `boundingClientRect` is the target's `getBoundingClientRect()`, and its `intersectionRect` is
/// that rect clipped by every ancestor that clips (its client area: `getBoundingClientRect()` moved in by
/// `clientLeft`/`clientTop`, `clientWidth` x `clientHeight`) and by the viewport. Integer-pixel geometry
/// only, so the oracle's arithmetic and the engine's agree to the bit. Frames: the initial delivery (t1
/// half-clipped by #i, t3 clipped out by #o), an inner scroll, an outer scroll plus `overflow: visible`,
/// and a wrapper going `display: none` (t2 then has no box: all zeros, `rootBounds` too -- Chrome 150's
/// no-box answer). Pins what the observers read from scroll offsets and computed overflow as those move.
#[tokio::test]
async fn nested_scrollers_answer_as_the_layout_reads() {
    let mut e = served_page().await;
    e.eval_value(
        "t5-nested-setup",
        r#"document.body.style.margin = '0';
           document.body.innerHTML = '<div id=o style="width:300px;height:200px;overflow:auto"><div style="height:50px"></div>'
             + '<div id=i style="margin-left:20px;width:200px;height:150px;overflow:auto"><div style="height:160px"></div>'
             + '<div id=t1 style="margin-left:10px;width:50px;height:40px"></div><div style="height:400px"></div></div>'
             + '<div id=w><div id=t2 style="width:100px;height:30px"></div></div><div style="height:300px"></div>'
             + '<div id=t3 style="width:100px;height:30px"></div></div>';
           const $ = id => document.getElementById(id);
           $('o').scrollTop = 50; $('i').scrollTop = 30;
           globalThis.__log = [];
           const R = r => [r.x, r.y, r.width, r.height];
           const inter = (a, b) => {
             const x1 = Math.max(a[0], b[0]), y1 = Math.max(a[1], b[1]);
             const x2 = Math.min(a[0] + a[2], b[0] + b[2]), y2 = Math.min(a[1] + a[3], b[1] + b[3]);
             return x2 >= x1 && y2 >= y1 ? [x1, y1, x2 - x1, y2 - y1] : null;
           };
           globalThis.__oracle = t => {
             const de = document.documentElement, zero = [0, 0, 0, 0];
             if (t.getClientRects().length === 0) return { b: zero, i: zero, hit: false, ratio: 0, root: zero };
             const b = R(t.getBoundingClientRect()), root = [0, 0, de.clientWidth, de.clientHeight];
             let c = b;
             for (let a = t.parentElement; c && a && a !== document.body && a !== de; a = a.parentElement) {
               const cs = getComputedStyle(a);
               if (cs.overflowX !== 'visible' || cs.overflowY !== 'visible') {
                 const ab = a.getBoundingClientRect();
                 c = inter(c, [ab.x + a.clientLeft, ab.y + a.clientTop, a.clientWidth, a.clientHeight]);
               }
             }
             const i = c && inter(c, root);
             if (!i) return { b, i: zero, hit: false, ratio: 0, root };
             const area = b[2] * b[3];
             return { b, i, hit: true, ratio: +(area > 0 ? Math.min(1, i[2] * i[3] / area) : 1).toFixed(4), root };
           };
           const io = new IntersectionObserver(es => {
             for (const x of es) {
               const got = { b: R(x.boundingClientRect), i: R(x.intersectionRect), hit: x.isIntersecting,
                             ratio: +x.intersectionRatio.toFixed(4), root: R(x.rootBounds) };
               __log.push({ id: x.target.id, got, want: __oracle(x.target) });
             }
           }, { threshold: Array.from({ length: 101 }, (_, k) => k / 100) });
           ['t1', 't2', 't3'].forEach(id => io.observe($(id))); 1"#
            .to_string(),
    )
    .unwrap();
    let phases: [(&str, Option<&str>); 4] = [
        ("initial", None),
        ("inner scroll", Some("document.getElementById('i').scrollTop = 60; 1")),
        ("outer scroll, then overflow visible", Some("const o = document.getElementById('o'); o.scrollTop = 0; o.style.overflow = 'visible'; 1")),
        ("a wrapper goes display:none", Some("document.getElementById('w').style.display = 'none'; 1")),
    ];
    for (n, (label, change)) in phases.iter().enumerate() {
        if let Some(js) = change {
            e.eval_value("t5-nested-change", js.to_string()).unwrap();
        }
        drain(&mut e).await;
        let entries = take_entries(&mut e, "t5-nested-log");
        assert!(!entries.is_empty(), "frame {n} ({label}): no entry was delivered");
        for x in &entries {
            assert_eq!(x["got"], x["want"], "frame {n} ({label}): {} answers other than the layout members", x["id"]);
        }
        let of = |id: &str| entries.iter().rev().find(|x| x["id"] == id).map(|x| x["got"].clone());
        match n {
            0 => {
                assert_eq!(of("t1").expect("t1's initial entry")["ratio"], 0.5, "t1 is half-clipped by #i");
                assert_eq!(of("t3").expect("t3's initial entry")["hit"], false, "t3 is clipped out by #o");
            }
            1 => assert_eq!(of("t1").expect("t1 moved")["ratio"], 1.0, "t1 is wholly inside #i after its scroll"),
            2 => assert_eq!(of("t3").expect("t3 moved")["hit"], true, "t3 is shown once #o stops clipping"),
            _ => {
                let t2 = of("t2").expect("t2 lost its box");
                assert_eq!(t2["hit"], false);
                assert_eq!(t2["root"], serde_json::json!([0, 0, 0, 0]), "a target with no box has all-zero rootBounds");
            }
        }
    }
}

/// `isIntersecting` IS "THE RATIO REACHED THE FIRST THRESHOLD" (Blink's `IntersectionGeometry::IsIntersecting`, the
/// threshold index past 0), not "the rectangles touch": under `threshold: 0.4` a target a third in view reads `false`
/// and one wholly in view `true` (Chrome 150.0.7871.46: GitHub's 2021px hero, 39.6 % in an 800px window, reads
/// `false`, and the page shows its "back to top" button; korvus read `true`).
#[tokio::test]
async fn is_intersecting_is_the_first_threshold_reached() {
    let mut e = served_page().await;
    e.eval_value(
        "first-threshold-setup",
        r#"document.body.style.margin = '0';
           document.body.innerHTML = '<div id=small style="height:50px"></div><div id=tall style="height:' + innerHeight * 3 + 'px"></div>';
           globalThis.__log = [];
           const io = new IntersectionObserver(es => {
             for (const x of es) __log.push({ id: x.target.id, hit: x.isIntersecting, over: x.intersectionRatio >= 0.4 });
           }, { threshold: 0.4 });
           io.observe(document.getElementById('small')); io.observe(document.getElementById('tall')); 1"#
            .to_string(),
    )
    .unwrap();
    drain(&mut e).await;
    let v = e.eval_value("first-threshold-log", "JSON.stringify(globalThis.__log)".to_string()).unwrap();
    let entries: Vec<serde_json::Value> = serde_json::from_str(v.as_str().expect("a JSON string")).unwrap();
    let of = |id: &str| entries.iter().find(|x| x["id"] == id).cloned().unwrap_or_else(|| panic!("no entry for {id}: {entries:?}"));
    assert_eq!(of("small")["hit"], true, "wholly in view: past the threshold");
    assert_eq!((of("tall")["hit"].clone(), of("tall")["over"].clone()), (serde_json::json!(false), serde_json::json!(false)), "a third in view, under 0.4");
}

/// A STYLE FLUSH IN THE MIDDLE OF AN OBSERVER PASS is seen by every observation after it. Under a scroll
/// container, the observers' walk over a target's ancestors reads the container's scroll offset, which
/// flushes style first -- and that flush is where a pending `width` change becomes a transition held at its
/// before-change value (`steps(1, end)` over 100 s: 100px throughout). The first observer of the frame may
/// have read the target's box before that flush; the second must read it after, as Chrome's single style
/// update before the observers gives both. Asserted only where Chrome and this engine agree: the second
/// observer, and the first observer's settled value.
#[tokio::test]
async fn observers_after_a_mid_pass_style_flush_read_the_flushed_layout() {
    const SETUP: &str = r#"document.body.style.margin = '0';
        document.body.innerHTML = '<div id=s style="width:300px;height:100px;overflow:auto">'
          + '<div id=t style="width:100px;height:20px;transition:width 100s steps(1,end)"></div><div style="height:500px"></div></div>';
        document.getElementById('t').offsetWidth"#;
    // A: two resize observers on the same target.
    let mut e = served_page().await;
    assert_eq!(e.eval_value("t5-flush-a-setup", SETUP.to_string()).unwrap(), 100);
    e.eval_value(
        "t5-flush-a-observe",
        r#"globalThis.__ro = [[], []];
           const t = document.getElementById('t');
           for (const i of [0, 1]) new ResizeObserver(es => { for (const x of es) __ro[i].push(x.borderBoxSize[0].inlineSize); }).observe(t, { box: 'border-box' }); 1"#
            .to_string(),
    )
    .unwrap();
    drain(&mut e).await;
    assert_eq!(e.eval_value("t5-flush-a-0", "JSON.stringify(__ro)".to_string()).unwrap(), "[[100],[100]]");
    e.eval_value("t5-flush-a-write", "document.getElementById('t').style.width = '300px'; 1".to_string()).unwrap();
    drain(&mut e).await;
    let ro: serde_json::Value = serde_json::from_str(e.eval_value("t5-flush-a-1", "JSON.stringify(__ro)".to_string()).unwrap().as_str().unwrap()).unwrap();
    assert_eq!(ro[1], serde_json::json!([100]), "the second resize observer read a box from before the frame's style flush: {ro}");
    assert_eq!(ro[0].as_array().and_then(|a| a.last()), Some(&serde_json::json!(100)), "the first resize observer did not settle on the flushed box: {ro}");

    // B: two intersection observers made in the same task as the write.
    let mut e = served_page().await;
    assert_eq!(e.eval_value("t5-flush-b-setup", SETUP.to_string()).unwrap(), 100);
    drain(&mut e).await;
    e.eval_value(
        "t5-flush-b-observe",
        r#"globalThis.__io = [[], []];
           const t = document.getElementById('t');
           t.style.width = '300px';
           for (const i of [0, 1]) new IntersectionObserver(es => { for (const x of es) __io[i].push([x.boundingClientRect.width, x.target.getBoundingClientRect().width]); }).observe(t); 1"#
            .to_string(),
    )
    .unwrap();
    drain(&mut e).await;
    let io: serde_json::Value = serde_json::from_str(e.eval_value("t5-flush-b-1", "JSON.stringify(__io)".to_string()).unwrap().as_str().unwrap()).unwrap();
    assert_eq!(io[1][0], serde_json::json!([100, 100]), "the second intersection observer read a box from before the frame's style flush: {io}");
}

/// A valid `w`x`h` RGBA PNG (stored, uncompressed deflate blocks).
fn late_png(w: u32, h: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.push(0u8);
        for _ in 0..w {
            raw.extend_from_slice(&[0, 128, 255, 255]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut s) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        s = (s + a) % 65521;
    }
    z.extend_from_slice(&((s << 16) | a).to_be_bytes());
    let chunk = |t: &[u8], d: &[u8]| {
        let mut td = t.to_vec();
        td.extend_from_slice(d);
        let mut out = (d.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&td);
        out.extend_from_slice(&crc32(&td).to_be_bytes());
        out
    };
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &z));
    out.extend(chunk(b"IEND", &[]));
    out
}

/// A DETACHED LAZY IMAGE DOES NOT HOLD BACK A SETTLED IMAGE'S RESIZE. An `<img>` whose bytes land after the
/// observers' first frames (`late.png`, served 400 ms late) is first observed at 0x0; once it has decoded,
/// Chrome delivers a second ResizeObserver entry at its natural size. Here that frame comes from the lazy-load
/// check the document's `load` runs: its layout refresh hands the page the settled image, which moves the
/// layout key the observers' frames are armed on. A deferred lazy image that no document holds (made by
/// `createElement` and never inserted) must not turn that refresh off: the 922b19df2 skip did, and the second
/// entry never came (2026-10-23, `korvus fetch` of the same markup, 3 of 3 runs on each tree).
#[tokio::test]
async fn a_detached_lazy_image_does_not_hold_back_a_settled_image_s_resize() {
    const PAGE: &str = r#"<!doctype html><html><head></head><body><img id=a src=late.png style=display:block>
<script>globalThis.__ro = [];
const l = document.createElement('img'); l.loading = 'lazy'; l.src = 'never.png'; globalThis.__keep = l;
new ResizeObserver(es => { for (const x of es) __ro.push(x.contentRect.width + 'x' + x.contentRect.height); })
  .observe(document.getElementById('a'));</script></body></html>"#;
    let png = std::sync::Arc::new(late_png(300, 200));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let png = png.clone();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut tmp) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap_or("").to_string();
                let (status, ty, body): (&str, &str, Vec<u8>) = match path.as_str() {
                    "/page.html" => ("200 OK", "text/html", PAGE.as_bytes().to_vec()),
                    "/late.png" => {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        ("200 OK", "image/png", png.to_vec())
                    }
                    _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
                };
                let h = format!("HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(h.as_bytes());
                let _ = s.write_all(&body);
                let _ = s.flush();
            });
        }
    });
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    let url = format!("http://{addr}/page.html");
    e.set_url(&url);
    e.load_page(PAGE, &url, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(std::time::Duration::from_secs(5)).await;
    drain(&mut e).await;
    let ro = e.eval_value("t5-late-image", "JSON.stringify(__ro)".to_string()).unwrap();
    assert_eq!(ro, r#"["0x0","300x200"]"#, "the settled image's resize was not delivered");
}

/// An ancestor clips a target only on the axes it clips, and only when it has a box: a `display: contents` wrapper
/// with `overflow: clip` clips nothing, and an `overflow-x: clip` one leaves its `visible` vertical axis open.
/// Chrome 150, the same document: `a` and `b` intersect whole, `c` (clipped both ways, its margin collapsed through
/// its clipper) a fifth. minea's ticker sat under both and was never told it was in view, so it never showed.
#[tokio::test]
async fn an_ancestor_clips_on_its_own_axes_and_only_with_a_box() {
    let mut e = served_page().await;
    let got = evaluate(
        &mut e,
        r#"new Promise(res => {
            document.body.style.margin = "0";
            document.body.innerHTML = '<div style="display:contents;overflow:clip"><div id=a style="height:20px">a</div></div>'
              + '<div style="overflow-x:clip;height:10px"><div id=b style="height:50px;margin-top:20px">b</div></div>'
              + '<div style="overflow:clip;height:10px"><div id=c style="height:50px;margin-top:20px">c</div></div>';
            const io = new IntersectionObserver(es => {
                res(JSON.stringify(es.map(e => [e.target.id, e.isIntersecting, Math.round(e.intersectionRatio * 100) / 100])));
                io.disconnect();
            });
            ["a", "b", "c"].forEach(i => io.observe(document.getElementById(i)));
        })"#
        .to_string(),
    )
    .await;
    assert_eq!(got, r#"[["a",true,1],["b",true,1],["c",true,0.2]]"#);
}

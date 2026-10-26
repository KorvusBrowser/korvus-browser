//! The animation timeline is sampled ONCE PER RENDERING OPPORTUNITY, not once per
//! observation, and that is what makes a second style read of an unchanged tree free.
//!
//! # The oracle
//!
//! Stock Google Chrome 150.0.7871.46, launched by us with `--remote-debugging-port` (never
//! Playwright's launcher), headless, on a loopback fixture carrying 300 elements each
//! running `animation: grow 10s linear infinite` over `width` — a LAYOUT-affecting property, which
//! is the hard case. Five reads inside one task, 3 ms of spin between them, taken after three
//! `requestAnimationFrame`s so the document is inside a live rendering loop:
//!
//! ```text
//!   document.timeline.currentTime       193, 193, 193, 193, 193        constant across the task
//!   animation.currentTime               133.4 x5                       constant
//!   getComputedStyle('.a').width        "15.1875px" x5                 the SAMPLE does not move
//!   performance.now()                   209.3 212.3 215.3 218.3 221.3  the wall clock DOES
//!   the timeline across one rAF         193 -> 243                     it moves when a frame does
//!   getComputedStyle / innerText / rect 0 ms each, all five            no work at all
//! ```
//!
//! So the rule is not "the timeline is `performance.now()`". It is: **the timeline is sampled at a
//! rendering opportunity and held for the whole task.** `performance.now()` is the clock the sample
//! is READ FROM, once.
//!
//! # What was wrong, and why it was not a micro-optimisation
//!
//! `timeline_now` read `performance.now()` on every call, so every observation was its own frame.
//! `getComputedStyle` runs the "update animations" step, so a fresh moment resampled every running
//! animation; the new values differed from the last, which dropped the whole `style_memo`, bumped
//! the layout fingerprint, and — on a document with `@container` rules — re-laid the WHOLE document
//! out. Measured on the banked `bandcamp.com` recording (2,260 nodes, 16 running `width` /
//! `padding` / `height` animations), `korvus fetch` through the replay proxy:
//!
//! ```text
//!                                     before              after
//!   getComputedStyle(el).display x5   20.4 20.2 20.5 20.2 20.5 ms      0.2 0.1 0 0.1 0 ms
//!   el.innerText x5                   20.2 20.1 20.2 20.1 20.1 ms      0.1 0 0.1 0 0.1 ms
//!   getComputedStyle over 10 elements 205 ms, then 204 ms again        1 ms, then 1 ms
//!   document.body.innerText           NEVER RETURNS (30 s probe)       answers
//!   verdict.usable() from the harness false, nodes None, 38.4 s        true, 2,070 nodes, 3.4 s
//! ```
//!
//! # Both halves are asserted here, because a cache that never invalidates is the worse bug
//!
//! The tests below assert that a repeated read lays nothing out AND that a dirtied document does:
//! a DOM mutation inside the same task, and the passage of a rendering opportunity, each make the
//! next read recompute. `Engine::document_layouts` is what they count — the layouts `korvus-page`
//! ran, not the refreshes the runtime's cache asked for, because the `@container` path lays out
//! behind that cache.

use engine::{Engine, PumpEnd};

/// 300 elements running a LAYOUT-affecting animation, under a `@container` rule — the exact shape
/// that made every style read re-lay-out the document. No hostname anywhere: the fixture is the
/// property, not a site.
const ANIMATED: &str = r#"<!doctype html><html><head><style>
@keyframes grow { from { width: 10px; } to { width: 400px; } }
.a { animation: grow 10s linear infinite; height: 4px; }
#wrap { container-type: inline-size; }
@container (min-width: 1px) { .a { color: rgb(1, 2, 3); } }
</style></head><body><div id=wrap><!--ROWS--></div><p id=t>hello world</p></body></html>"#;

fn animated_page() -> String {
    let rows: String = (0..300).map(|_| "<div class=a>x</div>").collect();
    ANIMATED.replace("<!--ROWS-->", &rows)
}

async fn booted(html: &str) -> Engine {
    let mut e = Engine::new(persona::Overridden::new().boxed());
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.run().await.unwrap();
    e
}

/// Chrome's answer, on korvus: five reads of the timeline inside one task are one value, while
/// `performance.now()` moves underneath them.
#[tokio::test]
async fn the_timeline_is_one_moment_for_the_whole_task() {
    let mut e = booted(&animated_page()).await;
    let v = e
        .eval_value(
            "c1-timeline-constant",
            r#"(() => {
                 const spin = () => { const s = performance.now(); while (performance.now() - s < 2) {} };
                 const tl = [], pn = [];
                 for (let i = 0; i < 5; i++) { tl.push(document.timeline.currentTime); pn.push(performance.now()); spin(); }
                 return JSON.stringify({ timeline_constant: tl.every(v => v === tl[0]),
                                         clock_moved: pn[4] > pn[0] });
               })()"#
                .to_string(),
        )
        .unwrap();
    let got: serde_json::Value = serde_json::from_str(v.as_str().expect("a JSON string")).unwrap();
    assert_eq!(got["timeline_constant"], true, "the timeline moved between two reads in one task");
    assert_eq!(got["clock_moved"], true, "the fixture did not actually spin, so it proves nothing");
}

/// An animated property reads the SAME value twice in one task — the sample, not the clock. This is
/// the fidelity half: korvus used to report two different widths for two synchronous reads where
/// Chrome reports one.
#[tokio::test]
async fn an_animated_value_does_not_move_between_two_reads_in_one_task() {
    let mut e = booted(&animated_page()).await;
    let v = e
        .eval_value(
            "c1-animated-value-constant",
            r#"(() => {
                 const spin = () => { const s = performance.now(); while (performance.now() - s < 2) {} };
                 const el = document.querySelector('.a'); const w = [];
                 for (let i = 0; i < 5; i++) { w.push(getComputedStyle(el).width); spin(); }
                 return JSON.stringify({ widths: w, constant: w.every(x => x === w[0]) });
               })()"#
                .to_string(),
        )
        .unwrap();
    let got: serde_json::Value = serde_json::from_str(v.as_str().expect("a JSON string")).unwrap();
    assert_eq!(got["constant"], true, "an animated width moved inside one task: {}", got["widths"]);
}

/// THE AMORTISATION PROFILE, as a count rather than a clock: the first read of a tree pays, the
/// rest do not. Chrome's shape is `first read pays, subsequent reads do not`; this asserts exactly
/// that and would fail on a merely-faster engine that still laid out per call.
#[tokio::test]
async fn a_second_read_of_an_unchanged_tree_lays_nothing_out() {
    let mut e = booted(&animated_page()).await;
    // One read to warm whatever the first read warms.
    e.eval_value("c1-warm", "getComputedStyle(document.getElementById('t')).display".to_string()).unwrap();
    let warm = e.document_layouts();
    // Twenty more reads of the SAME unchanged tree, all in one task: not one further layout.
    let v = e
        .eval_value(
            "c1-repeat-reads",
            r#"(() => { const el = document.getElementById('t'); let n = 0;
                 for (let i = 0; i < 20; i++) { if (getComputedStyle(el).display === 'block') n++; }
                 return n; })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, serde_json::json!(20));
    assert_eq!(e.document_layouts(), warm, "twenty reads of an unchanged tree laid the document out again");
    // And the same for `innerText`, which asks computed style of every element it descends through
    // and is the read a scraper makes. It warms SEPARATELY: `innerText` also wants the generated
    // boxes, which is a layout `getComputedStyle` never asks for, so the first one legitimately
    // pays where the twenty after it must not -- which is Chrome's shape, `56, 0, 0, 0, 0`.
    e.eval_value("c1-inner-text-warm", "document.getElementById('t').innerText".to_string()).unwrap();
    let before_text = e.document_layouts();
    let text = e
        .eval_value(
            "c1-repeat-inner-text",
            r#"(() => { const el = document.getElementById('t'); let s = '';
                 for (let i = 0; i < 20; i++) { s = el.innerText; } return s; })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(text, serde_json::json!("hello world"));
    assert_eq!(e.document_layouts(), before_text, "twenty innerText reads laid the document out again");
}

/// THE DIRTY HALF, first form: a document the page MUTATES recomputes. A cache that never
/// invalidates would pass the test above and serve stale text here.
#[tokio::test]
async fn a_mutated_tree_recomputes_in_the_same_task() {
    let mut e = booted(&animated_page()).await;
    let v = e
        .eval_value(
            "c1-dirty-mutation",
            r#"(() => {
                 const el = document.getElementById('t');
                 const first = el.innerText;
                 el.textContent = 'changed';
                 const second = el.innerText;
                 el.style.display = 'none';
                 const hidden = getComputedStyle(el).display;
                 el.style.display = '';
                 const back = getComputedStyle(el).display;
                 const added = document.createElement('p');
                 added.textContent = 'added';
                 document.body.appendChild(added);
                 const body = document.body.innerText.includes('added');
                 return JSON.stringify({ first, second, hidden, back, body });
               })()"#
                .to_string(),
        )
        .unwrap();
    let got: serde_json::Value = serde_json::from_str(v.as_str().expect("a JSON string")).unwrap();
    assert_eq!(got["first"], "hello world");
    assert_eq!(got["second"], "changed", "innerText served the pre-mutation text");
    assert_eq!(got["hidden"], "none", "a style set in this task was not seen");
    assert_eq!(got["back"], "block", "a style unset in this task was not seen");
    assert_eq!(got["body"], true, "an element appended in this task is missing from body.innerText");
}

/// THE DIRTY HALF, second form: across a RENDERING OPPORTUNITY the timeline moves, so a running
/// animation's value moves with it. Freezing the sample must not freeze the animation.
#[tokio::test]
async fn the_timeline_moves_across_a_rendering_opportunity() {
    let mut e = booted(&animated_page()).await;
    let before = e
        .eval_value("c1-before-frame", "JSON.stringify([document.timeline.currentTime, getComputedStyle(document.querySelector('.a')).width])".to_string())
        .unwrap();
    // The host's rendering opportunity, which `Engine::pump` opens between slices of the loop -- spaced as frames
    // are. A CSS animation starts at the first frame after the style change that made it, and the frame
    // that asks for it is a timer that keeps no loop alive, so with nothing else to run each pump returns at once:
    // four back-to-back opportunities fit in a few milliseconds, and the frame that starts the animation can be the
    // one whose timeline sample the read below sees -- its start value, as Chrome reads within that frame.
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        e.pump(std::time::Duration::from_millis(120)).await.unwrap();
    }
    let after = e
        .eval_value("c1-after-frame", "JSON.stringify([document.timeline.currentTime, getComputedStyle(document.querySelector('.a')).width])".to_string())
        .unwrap();
    let b: serde_json::Value = serde_json::from_str(before.as_str().expect("a JSON string")).unwrap();
    let a: serde_json::Value = serde_json::from_str(after.as_str().expect("a JSON string")).unwrap();
    assert!(
        a[0].as_f64().unwrap() > b[0].as_f64().unwrap(),
        "the timeline did not advance across a rendering opportunity: {b} -> {a}"
    );
    assert_ne!(a[1], b[1], "a running animation's width did not move across a rendering opportunity: {b} -> {a}");
}

/// A HOST THAT RUNS NO FRAME WHILE THE PAGE SITS IDLE -- `korvus serve` between two commands, a client sleeping --
/// must not set a CSS animation back when its first frame finally comes. Chrome's first frame after the style change
/// comes a vsync later, so 300 idle ms later the 10 s `grow` animation has run ~0.3 s: `10 + 390 * 0.3 / 10` = 21.7 px.
/// The frame that starts a pending animation took the LATE frame's time as its start, so the read
/// after it went back to ~14 px; it is started a frame after the host's loop went quiet now (`css::send_events`).
#[tokio::test]
async fn a_late_first_frame_does_not_set_an_animation_back() {
    let mut e = booted(&animated_page()).await;
    let width = |v: serde_json::Value| -> f64 { v.as_str().and_then(|s| s.trim_end_matches("px").parse().ok()).expect("a px width") };
    // the first style read makes the animation; then nothing runs for 300 ms
    let made = e.eval_value("c1-made", "getComputedStyle(document.querySelector('.a')).width".to_string()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        e.pump(std::time::Duration::from_millis(120)).await.unwrap();
    }
    let after = e.eval_value("c1-late", "getComputedStyle(document.querySelector('.a')).width".to_string()).unwrap();
    let (made, after) = (width(made), width(after));
    assert!(made < 13.0, "the animation was made by the first read, or just before it: {made}");
    assert!(after > 18.0, "a late first frame set the animation back: {made} px, then {after} px after 300 idle ms and four frames");
}

/// A page with NO animation pays nothing for any of this, and its reads amortise too. The sample is
/// taken lazily, so such a document never reads the clock at all.
#[tokio::test]
async fn a_document_with_no_animation_still_amortises() {
    let rows: String = (0..300).map(|_| "<div class=a>x</div>").collect();
    let html = format!("<!doctype html><html><body><div>{rows}</div><p id=t>hello world</p></body></html>");
    let mut e = booted(&html).await;
    // Two warming reads, for the same reason the animated case warms twice: the FIRST `innerText`
    // asks for the generated boxes and legitimately lays out. What must be zero is everything after.
    e.eval_value("c1-noanim-warm", "document.getElementById('t').innerText".to_string()).unwrap();
    e.eval_value("c1-noanim-warm2", "document.getElementById('t').innerText".to_string()).unwrap();
    let warm = e.document_layouts();
    let v = e
        .eval_value(
            "c1-noanim-repeat",
            r#"(() => { const el = document.getElementById('t'); let s = '';
                 for (let i = 0; i < 20; i++) { s = el.innerText; } return s; })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, serde_json::json!("hello world"));
    assert_eq!(e.document_layouts(), warm, "an un-animated page laid out again for a repeated read");
    assert_eq!(e.pump(std::time::Duration::from_millis(200)).await.unwrap(), PumpEnd::Drained);
}

/// A LAYOUT READ ON AN UNCHANGED DOCUMENT may skip its style flush only while the flush would change
/// nothing: a write between two reads, and a sheet inserted between two reads, are still seen by the next
/// read's flush. Read through `offsetLeft`/`offsetTop` only (`measured`, which flushes), never
/// `getAnimations` (which runs the update itself). The transition starts at the read, held at its
/// from-value as Chrome 150 holds it (`steps(1, end)` over 100 s: 0 throughout); the inserted animation
/// applies at the read that follows the insertion (37 from and to).
#[tokio::test]
async fn a_quiet_style_flush_still_sees_writes_and_sheets() {
    let html = r#"<!doctype html><html><head></head><body><div id=t style="position:absolute;left:0;top:0;width:10px;height:10px;transition:margin-left 100s steps(1,end)"></div></body></html>"#;
    let mut e = booted(html).await;
    let v = e
        .eval_value(
            "t5-quiet-flush",
            r#"(() => {
                 const t = document.getElementById('t'), out = [];
                 for (let i = 0; i < 3; i++) out.push(t.offsetLeft);
                 t.style.marginLeft = '50px';
                 out.push(t.offsetLeft);
                 out.push(t.offsetTop, t.offsetTop);
                 const s = document.createElement('style');
                 s.textContent = '@keyframes k { from, to { margin-top: 37px } } #t { animation: k 100s }';
                 document.head.appendChild(s);
                 out.push(t.offsetTop);
                 return JSON.stringify(out);
               })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, "[0,0,0,0,0,0,37]", "a read after a write or a sheet insertion answered from before it");

    // Changes that leave the DOM revision where it was: a rule inserted through the CSSOM, an animation made by
    // script, a paused one given new keyframes. Each read follows reads of the unchanged tree.
    let html = r#"<!doctype html><html><head><style id=s></style></head><body><div id=u style="position:absolute;left:0;top:0;width:10px;height:10px"></div></body></html>"#;
    let mut e = booted(html).await;
    let v = e
        .eval_value(
            "t5-quiet-flush-cssom",
            r#"(() => {
                 const u = document.getElementById('u'), out = [];
                 out.push(u.offsetTop, u.offsetTop, u.offsetTop);
                 document.getElementById('s').sheet.insertRule('#u { margin-top: 20px }');
                 out.push(u.offsetTop);
                 out.push(u.offsetLeft, u.offsetLeft);
                 u.animate([{ marginLeft: '70px' }, { marginLeft: '70px' }], 100000);
                 out.push(u.offsetLeft);
                 out.push(u.offsetLeft, u.offsetLeft);
                 const a = u.getAnimations()[0];
                 a.pause();
                 a.effect.setKeyframes([{ marginLeft: '5px' }, { marginLeft: '5px' }]);
                 out.push(u.offsetLeft);
                 return JSON.stringify(out);
               })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, "[0,0,0,20,0,0,70,70,70,5]", "a read after a CSSOM rule or a script animation answered from before it");
}

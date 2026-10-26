//! An inline element has a box, in a live engine: `<a>` and `<span>` report where they
//! actually are, a row of them lays out ACROSS a line, a wrapped one returns a rect per line, and a
//! click aimed at a rect's centre lands on the element that rect belongs to.
//!
//! # The oracle
//!
//! Every number asserted here was measured on **stock Google Chrome 150.0.7871.46**, driven over CDP
//! (launched by us with `--remote-debugging-port=0`, never by Playwright's launcher) on
//! the same markup, on 2026-09-14. None of it is read off a specification. The engine is given a
//! persona whose window inner size is Chrome's measured viewport — **1920 × 993** — and every page
//! names `font: 16px Arial`, because the persona's default family is Times New Roman and Chrome's on
//! this host is Arial: naming it is what makes the two engines measure the same text.
//!
//! ```text
//! <div><a>link</a> <span>span text</span> …</div>       a      [8,      8, 24.02, 17]
//!                                                       span   [36.47,  8, 64.94, 17]
//! <div style=width:200px>aaa <span>one … seven</span>   rect 1 [39.14, 26, 158.31, 17]
//!                                             zzz</div> rect 2 [8,     44,  66.7,  17]
//!                                                       bcr    [8,     26, 189.45, 35]
//! ```
//!
//! Before this unit both inlines were `[0, 0, 0, 0]`: `layout/style.rs` mapped `display: inline` to
//! no box at all, deliberately, rather than stack a `<span>` on its own line at x = 0 and report a
//! confidently wrong rect. The fix is not the thing it refused — `<span>` at x = 36.47 on the same
//! line as `<a>` is not a block's geometry — it is the inline formatting context that was missing.
//!
//! # What this asks that `korvus-page`'s own tests cannot
//!
//! `korvus-page` proves the box tree produces the fragments. This drives a real `Engine`, so
//! `getBoundingClientRect`, `getClientRects`, `elementFromPoint`, `offsetWidth`, `clientWidth` and
//! `scrollWidth` all answer through their WebAPI rows, from the layout cache, under the persona's
//! viewport — the same path `korvus serve` gives CDP, and the one the probe measured when it
//! found **no click target on 402 sites** because the page's clickable thing was a link.

use engine::Engine;
use spec::ScreenProfile;

/// Chrome's measured viewport, as a persona.
fn chrome_viewport() -> Box<dyn spec::Persona> {
    persona::Overridden::new()
        .screen(ScreenProfile {
            width: 1920,
            height: 1080,
            window_inner_width: 1920,
            window_inner_height: 993,
            ..Default::default()
        })
        .boxed()
}

async fn run(body: &str, script: &str) -> Vec<String> {
    let html = format!(
        "<!doctype html><html><head><style>body{{margin:8px;font:16px Arial}}\
         div{{font:16px Arial}}</style></head><body>{body}</body></html>"
    );
    let mut e = Engine::new(chrome_viewport());
    e.run().await.unwrap();
    e.load_html(&html).unwrap();
    e.eval("inline", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The inline-axis allowance, and why the block axis has none — see `layout::ifc`'s test module for
/// the long form. `x` and `width` are sums of captured glyph advances and disagree with Chrome by up
/// to 0.02px along a line; `y`, `height`, rect COUNTS and hit-test answers are exact.
const ADVANCE_PRECISION: f64 = 0.03;

#[track_caller]
fn near(got: &str, want: f64, what: &str) {
    let got: f64 = got.parse().unwrap_or_else(|_| panic!("{what}: {got:?} is not a number"));
    assert!((got - want).abs() <= ADVANCE_PRECISION, "{what}: got {got}, Chrome says {want}");
}

/// THE UNIT, in one test: a row of inlines lays out ACROSS a line, at Chrome's own x.
///
/// This is the assertion the whole lane exists for. `INTERACTIVITY.md` §3.3 measured a `<button>`
/// reporting `129 x 18` and receiving its click while the `<a>` beside it reported `0 x 0` — so on
/// every page whose clickable thing is a link there was nothing for a probe to aim at. Three
/// different x values on one y is what makes an aim possible.
#[tokio::test]
async fn a_row_of_inlines_lays_out_across_a_line_and_not_stacked_at_zero() {
    let out = run(
        "<div id='row'><a id='a' href='#'>link</a> <span id='s'>span text</span> tail</div>",
        r#"
        for (const id of ["a", "s"]) {
            const r = document.getElementById(id).getBoundingClientRect();
            print(id + " " + [r.x, r.y, r.width, r.height].join(" "));
        }
        "#,
    )
    .await;
    let a: Vec<&str> = out[0].split(' ').collect();
    let s: Vec<&str> = out[1].split(' ').collect();
    near(a[1], 8.0, "a.x");
    near(a[3], 24.02, "a.width");
    assert_eq!((a[2], a[4]), ("8", "17"), "Chrome: y = 8, height = 17 (the content area)");
    // The whole point: the second inline is NOT at x = 0, and NOT on a line of its own.
    near(s[1], 36.47, "span.x");
    near(s[3], 64.94, "span.width");
    assert_eq!((s[2], s[4]), ("8", "17"), "the same line, the same content area");
}

/// An inline wrapping two lines returns TWO rects, and the bounding rect is their union.
///
/// Chrome: `[39.14, 26, 158.31, 17]` and `[8, 44, 66.7, 17]`, bounding `[8, 26, 189.45, 35]`. This
/// is the case `getBoundingClientRect` alone cannot express, and the reason `client_rects` has
/// returned a list since design D11.
#[tokio::test]
async fn a_wrapped_inline_returns_one_rect_per_line_and_a_union_bounding_rect() {
    let out = run(
        "<div style='height:18px'></div>\
         <div id='B' style='width:200px'>aaa <span id='s'>one two three four five six seven</span> \
         zzz</div>",
        r#"
        const s = document.getElementById("s");
        const list = s.getClientRects();
        print("n=" + list.length);
        for (const r of list) print([r.x, r.y, r.width, r.height].join(" "));
        const b = s.getBoundingClientRect();
        print([b.x, b.y, b.width, b.height].join(" "));
        print("offsetWidth=" + s.offsetWidth + " client=" + s.clientWidth + " scroll=" + s.scrollWidth);
        "#,
    )
    .await;
    assert_eq!(out[0], "n=2", "one rect per line box");
    let one: Vec<&str> = out[1].split(' ').collect();
    let two: Vec<&str> = out[2].split(' ').collect();
    near(one[0], 39.14, "line 1 x");
    near(one[2], 158.31, "line 1 width");
    assert_eq!((one[1], one[3]), ("26", "17"));
    near(two[0], 8.0, "line 2 x");
    near(two[2], 66.7, "line 2 width");
    assert_eq!((two[1], two[3]), ("44", "17"), "the next line down");
    let b: Vec<&str> = out[3].split(' ').collect();
    near(b[0], 8.0, "bcr x");
    near(b[2], 189.45, "bcr width");
    assert_eq!((b[1], b[3]), ("26", "35"), "the union of the two lines");
    // Chrome answers 189 / 0 / 0 for these three on that span: an inline box has a bounding box but
    // no padding box and no scrolling area.
    assert_eq!(out[4], "offsetWidth=189 client=0 scroll=0");
}

/// Hit testing agrees with the rects — at the centre of EACH fragment.
///
/// Chrome makes the distinction itself, which is why this is per-fragment and not per-element: at
/// the centre of a two-line span's BOUNDING rect `elementFromPoint` answered the enclosing block,
/// because that point falls in the leading between the two lines. At the centre of each of its
/// `getClientRects()` entries it answered the span. A probe clicking a bounding-rect centre would
/// mis-target; one clicking a fragment centre hits.
#[tokio::test]
async fn a_click_at_each_rects_centre_hits_the_element_that_rect_belongs_to() {
    let out = run(
        "<div id='row'><a id='a' href='#'>link</a> <span id='s'>span text</span> tail</div>\
         <div id='B' style='width:200px'>aaa <span id='w'>one two three four five six seven</span> \
         zzz</div>",
        r#"
        for (const id of ["a", "s", "w"]) {
            const el = document.getElementById(id);
            const hits = [...el.getClientRects()].map((r) => {
                const h = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
                return h ? h.id : "null";
            });
            print(id + "=" + hits.join(","));
        }
        "#,
    )
    .await;
    assert_eq!(out, vec!["a=a", "s=s", "w=w,w"], "every fragment centre finds its own element");
}

/// A link is clickable: a real press at the centre of an `<a>`'s rect reaches the page's own
/// listener, which is the capability `INTERACTIVITY.md` §4 scores `click` on and the one that was
/// unreachable for want of a target.
#[tokio::test]
async fn a_press_at_a_links_rect_centre_reaches_the_pages_own_listener() {
    let out = run(
        "<div>before <a id='a' href='#'>click me</a> after</div>",
        r#"
        const a = document.getElementById("a");
        a.addEventListener("click", (e) => print("clicked target=" + e.target.id));
        const r = a.getBoundingClientRect();
        print("rect=" + (r.width > 0 && r.height > 0));
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        print("hit=" + (hit ? hit.id : "null"));
        hit.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        "#,
    )
    .await;
    assert_eq!(out, vec!["rect=true", "hit=a", "clicked target=a"]);
}

/// The fragments move with the page. A second layout — here, a resize of the containing block —
/// re-breaks the lines and the rects follow, rather than being frozen from the first pass.
#[tokio::test]
async fn the_fragments_are_relaid_out_when_the_page_changes() {
    let out = run(
        "<div id='B' style='width:200px'><span id='s'>one two three four five six seven</span></div>",
        r#"
        const s = document.getElementById("s"), b = document.getElementById("B");
        print("narrow=" + s.getClientRects().length);
        b.style.width = "1000px";
        print("wide=" + s.getClientRects().length);
        const r = s.getBoundingClientRect();
        print("oneline=" + (r.height === 17));
        "#,
    )
    .await;
    assert_eq!(out, vec!["narrow=2", "wide=1", "oneline=true"]);
}

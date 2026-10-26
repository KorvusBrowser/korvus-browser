//! Scrolling end to end, in a live engine: the document has an extent, `scrollTo`
//! lands on it, and `scrollWidth`/`scrollHeight` answer the scrolling box instead of zero.
//!
//! # The oracle
//!
//! Every number asserted here was measured on **stock Google Chrome 150.0.7871.46**, driven over
//! CDP (launched by us with `--remote-debugging-port=0`, never by Playwright's launcher),
//! on the same markup, on 2026-09-13. None of it is read off a specification. The engine is
//! given a persona whose window inner size is Chrome's measured viewport — **1920 × 993** — so the
//! expectations are Chrome's own integers and not a rescaling of them.
//!
//! ```text
//! page                                    documentElement      scrollTo(0, 2e6)
//!                                    scrollHeight  clientHeight   leaves scrollY
//! 50px of content                          993          993             0
//! 5000px of content                       5000          993          4007
//! 4000px wide, 50px tall                   978*         978*            0    (scrollX -> 2080)
//! three 100px divs + 4000px filler        4300          993          3307
//! ```
//!
//! `*` is the ONE place the two engines disagree by construction: Chrome reserves 15px for a classic
//! scrollbar and korvus-page lays out none, so on a page that overflows horizontally Chrome's
//! `clientHeight` is 993 − 15 = 978 and korvus's is 993. Every assertion below is on an axis where
//! no scrollbar is in play, and the horizontal case uses the page Chrome answers 1920 for
//! (`de_clientWidth` with no vertical scrollbar), so the numbers are comparable as written. The
//! scrollbar gap is recorded in `korvus-page`'s own tests and is not this unit's.
//!
//! # What this asks that the unit tests cannot
//!
//! `korvus-page` proves the box tree knows the scrolling box's size; this drives a real `Engine`, so
//! the member rows, the layout cache, the persona's viewport, the clamp and the one-store rule for
//! `documentElement.scrollTop` are all in the loop — the same path `korvus serve` gives CDP and the
//! same one the probe measured as `scrollY == 2000000` on a document whose `scrollHeight` was 0.

use engine::Engine;
use spec::ScreenProfile;

/// Chrome's measured viewport, as a persona. `window_inner_*` is what `window.innerWidth`/
/// `innerHeight` report and what the document is laid out into, so setting it sets both.
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

async fn run(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(chrome_viewport());
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("scroll", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// `html`/`body` margins and padding zeroed, so the page's height is the number in the markup and
/// the assertion is about scrolling rather than about the user-agent sheet.
fn page(body: &str) -> String {
    format!("<!doctype html><html><head><style>html,body{{margin:0;padding:0}}</style></head><body>{body}</body></html>")
}

/// THE UNIT, in one test: both halves of the capability `INTERACTIVITY.md` §2 scores —
/// "the document moved AND landed at its own `scrollHeight - innerHeight`".
#[tokio::test]
async fn the_document_scrolls_to_its_own_extent_and_not_to_the_number_it_was_given() {
    let out = run(
        &page("<div style='height:5000px'></div>"),
        r#"
        const d = document.documentElement;
        print("scrollHeight=" + d.scrollHeight);
        print("clientHeight=" + d.clientHeight);
        print("innerHeight=" + innerHeight);
        const bottom = d.scrollHeight - innerHeight;
        const before = scrollY;
        scrollTo(0, 2000000);
        print("moved=" + (scrollY > before));
        print("landed=" + scrollY + " bottom=" + bottom);
        print("atBottom=" + (scrollY === bottom));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // Chrome: 5000, 993, 993, and scrollY 4007 after scrollTo(0, 2e6).
            "scrollHeight=5000",
            "clientHeight=993",
            "innerHeight=993",
            "moved=true",
            "landed=4007 bottom=4007",
            "atBottom=true",
        ]
    );
}

/// A page SHORTER than the viewport: `documentElement.scrollHeight` is the viewport's height, not
/// the content's, and the document does not move at all. This is the half a "did it move?" rule
/// cannot see — the engine used to report `scrollY == 2000000` here too.
#[tokio::test]
async fn a_page_shorter_than_the_viewport_reports_the_viewport_and_does_not_move() {
    let out = run(
        &page("<div id='box' style='height:50px'></div>"),
        r#"
        const d = document.documentElement;
        print("de=" + d.scrollHeight + "," + d.clientHeight + "," + d.offsetHeight);
        print("body=" + document.body.scrollHeight + "," + document.body.clientHeight);
        scrollTo(0, 2000000);
        print("scrollY=" + scrollY);
        scrollTo(0, -5);
        print("negative=" + scrollY);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // Chrome: de_scrollHeight 993, de_clientHeight 993, de_offsetHeight 50 -- the root's own
            // box is still 50, only the two client/scroll members answer the viewport.
            "de=993,993,50",
            // ...and `body` is a different number on the same page, which is why the parity probe
            // records `body_scroll_height` beside `scroll_height`.
            "body=50,50",
            "scrollY=0",
            "negative=0",
        ]
    );
}

/// The horizontal axis, on the one page shape where Chrome reserves no vertical scrollbar and the
/// two engines' `clientWidth` therefore agree: 4000px wide, 50px tall. Chrome answers
/// `scrollWidth` 4000, `clientWidth` 1920 and lands `scrollX` on 2080.
#[tokio::test]
async fn the_horizontal_extent_is_the_content_width_less_the_viewport() {
    let out = run(
        &page("<div style='width:4000px;height:50px'></div>"),
        r#"
        const d = document.documentElement;
        print("scrollWidth=" + d.scrollWidth + " clientWidth=" + d.clientWidth);
        scrollTo(2000000, 2000000);
        print("scroll=" + scrollX + "," + scrollY);
        print("scrollLeft=" + d.scrollLeft);
        scrollTo(0, 0);
        scrollBy(40, 40);
        print("by=" + scrollX + "," + scrollY);
        scrollBy(-1000, -1000);
        print("byNeg=" + scrollX + "," + scrollY);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "scrollWidth=4000 clientWidth=1920",
            // Chrome: [2080, 0] -- 4000 - 1920 on x, nothing to scroll on y.
            "scroll=2080,0",
            "scrollLeft=2080",
            "by=40,0",
            "byNeg=0,0",
        ]
    );
}

/// `window.scrollTo` and `documentElement.scrollTop` are ONE position, readable through either.
/// Measured both directions in Chrome on this exact page: `d.scrollTop = 2e6` left `scrollY` at
/// 3307, and `scrollTo(0, 2e6)` left `d.scrollTop` at 3307. `body.scrollTop` is not that position
/// in a no-quirks document and moves neither.
#[tokio::test]
async fn the_document_element_and_the_window_share_one_scroll_position() {
    let out = run(
        &page(
            "<div id='vis'  style='height:100px;overflow:visible'><div style='height:900px'></div></div>\
             <div id='clip' style='height:100px;overflow:clip'><div style='height:900px'></div></div>\
             <div id='hid'  style='height:100px;overflow:hidden'><div style='height:900px'></div></div>\
             <div style='height:4000px'></div>",
        ),
        r#"
        const d = document.documentElement, b = document.body;
        print("de=" + d.scrollHeight + " inner=" + innerHeight);
        d.scrollTop = 2000000;
        print("viaElement=" + d.scrollTop + "," + scrollY);
        d.scrollTop = 0;
        scrollTo(0, 2000000);
        print("viaWindow=" + d.scrollTop + "," + scrollY);
        scrollTo(0, 0);
        b.scrollTop = 2000000;
        print("viaBody=" + b.scrollTop + "," + scrollY);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // Chrome: de_scrollHeight 4300 in a 993 viewport -> 3307 either way.
            "de=4300 inner=993",
            "viaElement=3307,3307",
            "viaWindow=3307,3307",
            "viaBody=0,0",
        ]
    );
}

/// An element's own scrolling box: every box REPORTS its overflow, and only a scroll container can
/// be scrolled through it. Chrome, on three 100px divs each holding a 900px child: `scrollHeight`
/// 900 for all three, and `el.scrollTop = 2e6` leaves `visible` at 0, `clip` at 0 and `hidden` at
/// 800. `overflow: hidden` reserves no scrollbar in Chrome either, so this page is exactly
/// comparable.
#[tokio::test]
async fn only_a_scroll_container_scrolls_though_every_box_reports_its_overflow() {
    let out = run(
        &page(
            "<div id='vis'  style='height:100px;overflow:visible'><div style='height:900px'></div></div>\
             <div id='clip' style='height:100px;overflow:clip'><div style='height:900px'></div></div>\
             <div id='hid'  style='height:100px;overflow:hidden'><div style='height:900px'></div></div>",
        ),
        r#"
        for (const id of ["vis", "clip", "hid"]) {
          const el = document.getElementById(id);
          el.scrollTop = 2000000;
          print(id + "=" + el.scrollHeight + "," + el.clientHeight + "," + el.scrollTop);
        }
        const det = document.createElement("div");
        det.scrollTop = 5;
        print("detached=" + det.scrollHeight + "," + det.clientHeight + "," + det.scrollTop);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "vis=900,100,0",
            "clip=900,100,0",
            "hid=900,100,800",
            // Chrome answers all three as 0 for an element that was never in a document.
            "detached=0,0,0",
        ]
    );
}

/// A scroll container's own END padding is inside its scrolling area, on both axes. Chrome, on a
/// 200×300 content box with 7px padding and a 3px border holding a 1234×2345 child: `scrollHeight`
/// 1248 = 1234 + 7 + 7, `scrollWidth` 2359 = 2345 + 7 + 7.
#[tokio::test]
async fn a_scroll_containers_own_padding_is_inside_its_scrolling_area() {
    let out = run(
        &page(
            "<div id='sc' style='height:200px;width:300px;overflow:auto;border:3px solid black;padding:7px'>\
             <div style='height:1234px;width:2345px'></div></div>",
        ),
        r#"
        const sc = document.getElementById("sc");
        print("size=" + sc.scrollWidth + "," + sc.scrollHeight);
        print("offset=" + sc.offsetWidth + "," + sc.offsetHeight);
        print("border=" + sc.clientTop + "," + sc.clientLeft);
        sc.scrollTop = 2000000; sc.scrollLeft = 2000000;
        print("at=" + sc.scrollLeft + "," + sc.scrollTop);
        print("extent=" + (sc.scrollWidth - sc.clientWidth) + "," + (sc.scrollHeight - sc.clientHeight));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // Chrome: 2359,1248 -- child plus BOTH paddings on each axis.
            "size=2359,1248",
            "offset=320,220",
            // clientTop/clientLeft are the BORDER widths. Chrome: 3,3.
            "border=3,3",
            // Both classic scrollbars show, so clientWidth/Height are Chrome's 299/199
            // and the extent -- and the landing point -- is Chrome's 2060/1049. Before P11b no
            // scrollbar was reserved and both read 2045/1034, 15px short on each axis.
            "at=2060,1049",
            "extent=2060,1049",
        ]
    );
}

/// `scrollHeight` is a `long` over a fractional layout, and Chrome rounds HALF UP — measured on a
/// deliberately fractional box rather than assumed, because truncation reads just as well.
/// Chrome: 1999.1 → 1999, 2000.4 → 2000, 2000.5 → **2001**, 2000.6 → 2001.
#[tokio::test]
async fn a_fractional_document_rounds_its_scroll_height_half_up() {
    let mut got = Vec::new();
    for css in ["1999.1", "2000.4", "2000.5", "2000.6"] {
        let out = run(
            &page(&format!("<div style='height:{css}px'></div>")),
            r#"print(document.documentElement.scrollHeight + "," + (function () {
                 scrollTo(0, 2000000); return scrollY;
               })());"#,
        )
        .await;
        got.push(out.join(""));
    }
    assert_eq!(
        got,
        vec![
            "1999,1006".to_string(),  // 1999 - 993
            "2000,1007".to_string(),
            "2001,1008".to_string(),
            "2001,1008".to_string(),
        ]
    );
}

/// The member is still a native accessor on `Element.prototype`, and still throws for a foreign
/// receiver. The zero it replaced threw too, and losing either half would trade a wrong number for
/// a surface tell.
#[tokio::test]
async fn the_scroll_size_members_are_native_and_reject_a_foreign_receiver() {
    let out = run(
        &page("<div id='d'></div>"),
        r#"
        const find = (name) => {
          for (let o = Element.prototype; o; o = Object.getPrototypeOf(o)) {
            const desc = Object.getOwnPropertyDescriptor(o, name);
            if (desc) return desc;
          }
          return null;
        };
        for (const name of ["scrollWidth", "scrollHeight"]) {
          const d = find(name);
          const native = String(d.get).includes("[native code]");
          let threw = "NO-THROW";
          try { d.get.call({}); } catch (e) { threw = e.name; }
          print(name + "=" + native + "," + (d.get.prototype === undefined) + "," + !d.set + "," + threw);
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["scrollWidth=true,true,true,TypeError", "scrollHeight=true,true,true,TypeError"]
    );
}

/// `scrollIntoView` and `scrollIntoViewIfNeeded` move every scrolling box between the element and the viewport, to
/// where Chrome puts it. Measured on stock Chrome 150.0.7871.46 (`--headless=new`, a 1920x993 viewport, Playwright's
/// `set_content` of this markup with the user-agent margins left in, 2026-10-16): each line is
/// `scrollX,scrollY,container.scrollTop` after one call from the top. korvus used to write the ELEMENT's own scroll
/// position to zero and move nothing else, so a target below the fold stayed there and Playwright refused to click it
/// ("element is outside of the viewport").
#[tokio::test]
async fn scroll_into_view_moves_every_scrolling_box_to_where_chrome_puts_the_element() {
    let out = run(
        "<!doctype html><html><body><div style='height:3000px'></div><div id='t' style='height:50px'>t</div>\
         <div style='height:3000px'></div><div id='c' style='height:200px;overflow:auto'><div style='height:1000px'></div>\
         <div id='t2' style='height:20px'>x</div><div style='height:1000px'></div></div><div style='height:3000px'></div>\
         </body></html>",
        r#"
        const t = document.getElementById('t'), c = document.getElementById('c'), t2 = document.getElementById('t2');
        const step = (name, f) => { scrollTo(0, 0); c.scrollTop = 0; f(); print(name + '=' + [scrollX, scrollY, c.scrollTop].join(',')); };
        step('default', () => t.scrollIntoView());
        step('true', () => t.scrollIntoView(true));
        step('false', () => t.scrollIntoView(false));
        step('center', () => t.scrollIntoView({block: 'center'}));
        step('end', () => t.scrollIntoView({block: 'end'}));
        step('nearest-below', () => t.scrollIntoView({block: 'nearest'}));
        step('nearest-above', () => { scrollTo(0, 5000); t.scrollIntoView({block: 'nearest'}); });
        step('nearest-visible', () => { scrollTo(0, 2900); t.scrollIntoView({block: 'nearest'}); });
        step('ifneeded-hidden', () => t.scrollIntoViewIfNeeded());
        step('ifneeded-visible', () => { scrollTo(0, 2900); t.scrollIntoViewIfNeeded(); });
        step('ifneeded-false', () => t.scrollIntoViewIfNeeded(false));
        step('nested', () => t2.scrollIntoView());
        step('nested-container-nearest', () => t2.scrollIntoView({container: 'nearest'}));
        print('returns=' + Object.prototype.toString.call(t.scrollIntoView()) + ',' + String(t.scrollIntoViewIfNeeded()));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "default=0,3008,0",
            "true=0,3008,0",
            "false=0,2065,0",
            // 3008 + 25 - 496.5 = 2536.5: Chrome lands on the whole pixel above
            "center=0,2537,0",
            "end=0,2065,0",
            "nearest-below=0,2065,0",
            "nearest-above=0,3008,0",
            "nearest-visible=0,2900,0",
            "ifneeded-hidden=0,2537,0",
            "ifneeded-visible=0,2900,0",
            "ifneeded-false=0,2065,0",
            // the container scrolls its child to its top, then the viewport scrolls the container's top to its own
            "nested=0,6058,1000",
            "nested-container-nearest=0,0,1000",
            "returns=[object Promise],undefined",
        ]
    );
}

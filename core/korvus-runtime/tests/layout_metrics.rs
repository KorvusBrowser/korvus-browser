//! Layout-metric properties + DOMRect, answered by the page pipeline.
//!
//! Every one of these used to be zero, because there was no layout.
//! `document.body.getBoundingClientRect().width === 0` is a one-line detect on every page, and it
//! is the number this file exists to keep non-zero.
use engine::Engine;
use spec::Persona;

/// The viewport is the PERSONA's screen, so every width below is derived from it rather than
/// written down. The literals here used to be a 1024-wide persona's (`1008` = 1024 - 2x8), which
/// meant switching the shipped bundle broke a test about layout metrics -- a fact about the persona
/// masquerading as a fact about the box model.
fn viewport_width() -> f64 {
    persona::CapturedPersona::default().screen().width as f64
}

async fn run(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

// The mission, in one number.
#[tokio::test]
async fn the_body_is_not_zero_wide() {
    let out = run(
        r#"<!doctype html><html><body><div id="d">x</div></body></html>"#,
        r#"
        const r = document.body.getBoundingClientRect();
        print("rect=" + [r.x, r.y, r.width].join(","));
        print("isRect=" + (r instanceof DOMRect));
        print("derived=" + [r.top, r.left, r.right, r.bottom].join(","));
        "#,
    )
    .await;
    let w = viewport_width() - 16.0;   // the user-agent sheet's 8px body margin on each side
    assert_eq!(
        out,
        vec![
            format!("rect=8,8,{w}"),
            "isRect=true".to_string(),
            // One line of the persona's default font: Brave 151's `"Noto Sans CJK KR"` line box is
            // 24px at 16px (`V-font-metrics`'s `dom16`), so the body ends at 8 + 24.
            format!("derived=8,8,{},32", w + 8.0),
        ]
    );
}

#[tokio::test]
async fn the_box_model_shows_up_in_the_metrics() {
    let out = run(
        r#"<!doctype html><html><body><div id="d"
             style="width:100px;height:40px;padding:10px;border:2px solid red;margin:5px"></div></body></html>"#,
        r#"
        const d = document.getElementById("d");
        const r = d.getBoundingClientRect();
        print("rect=" + [r.x, r.y, r.width, r.height].join(","));
        print("offset=" + [d.offsetWidth, d.offsetHeight, d.offsetLeft, d.offsetTop].join(","));
        print("client=" + [d.clientWidth, d.clientHeight, d.clientTop, d.clientLeft].join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // content 100x40 + 10px padding each side + 2px border each side.
            // y is 8 and not 5: the div's 5px top margin *collapsed* into body's 8px rather than
            // adding to it, which is what CSS says and what taffy's block layout does.
            "rect=13,8,124,64",
            "offset=124,64,13,8",
            // The padding box, and the border widths.
            "client=120,60,2,2",
        ]
    );
}

// The members have to stay `[native code]`: closing the geometry tell by installing plain JS
// functions would open a `Function.prototype.toString` one instead.
#[tokio::test]
async fn the_geometry_members_are_still_native() {
    let out = run(
        r#"<!doctype html><html><body><div id="d"></div></body></html>"#,
        r#"
        const d = document.getElementById("d");
        const native = (s) => s.indexOf("[native code]") >= 0;
        print("gbcr=" + native(d.getBoundingClientRect.toString()));
        print("gcr=" + native(d.getClientRects.toString()));
        // The accessors sit somewhere on the prototype chain; find them rather than assume a depth.
        const find = (name) => {
          for (let o = Object.getPrototypeOf(d); o; o = Object.getPrototypeOf(o)) {
            const desc = Object.getOwnPropertyDescriptor(o, name);
            if (desc) return desc;
          }
          return null;
        };
        for (const name of ["offsetWidth", "clientWidth"]) {
          const desc = find(name);
          // Native, and owning no `.prototype` -- the two halves surface_parity checks for.
          print(name + "=" + native(desc.get.toString()) + "," + (desc.get.prototype === undefined));
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["gbcr=true", "gcr=true", "offsetWidth=true,true", "clientWidth=true,true"]
    );
}

#[tokio::test]
async fn a_stylesheet_moves_the_boxes_the_metrics_report() {
    let out = run(
        r#"<!doctype html><html><head><style>
             #a { width: 200px; height: 30px } #b { height: 10px }
           </style></head><body><div id="a"></div><div id="b"></div></body></html>"#,
        r#"
        const a = document.getElementById("a"), b = document.getElementById("b");
        print("a=" + [a.offsetWidth, a.offsetHeight, a.offsetTop].join(","));
        print("b=" + [b.offsetHeight, b.offsetTop].join(","));
        "#,
    )
    .await;
    assert_eq!(out, vec!["a=200,30,8", "b=10,38"], "b sits below a");
}

// A page that measures, mutates, and measures again must not see the first answer. The layout is
// cached, so this is the case the cache key has to be right about.
#[tokio::test]
async fn a_mutation_between_two_measurements_is_not_cached_over() {
    let out = run(
        r#"<!doctype html><html><body><div id="d" style="width:100px;height:10px"></div></body></html>"#,
        r#"
        const d = document.getElementById("d");
        print("before=" + d.offsetWidth);
        d.style.width = "250px";
        print("after=" + d.offsetWidth);
        d.setAttribute("style", "width:60px;height:10px");
        print("again=" + d.offsetWidth);
        "#,
    )
    .await;
    assert_eq!(out, vec!["before=100", "after=250", "again=60"]);
}

#[tokio::test]
async fn an_element_with_no_box_still_answers_with_zeros() {
    let out = run(
        r#"<!doctype html><html><body><div id="gone" style="display:none"></div>
           <span id="inline">x</span></body></html>"#,
        r#"
        const g = document.getElementById("gone"), s = document.getElementById("inline");
        const r = g.getBoundingClientRect();
        print("gone=" + [r.x, r.width, g.offsetWidth].join(",") + " rects=" + g.getClientRects().length);
        // An inline box's geometry IS a run of line boxes, and it now has one. Chrome answers a
        // real width and one rect here, and answers `clientWidth`/`scrollWidth` of 0 for it -- an
        // inline box has no padding box and no scrolling area, however wide its rect is.
        print("inline=" + (s.offsetWidth > 0) + " rects=" + s.getClientRects().length
              + " client=" + s.clientWidth + " scroll=" + s.scrollWidth);
        "#,
    )
    .await;
    assert_eq!(out, vec!["gone=0,0,0 rects=0", "inline=true rects=1 client=0 scroll=0"]);
}

// GeometryUtils -- `getBoxQuads` and the three `convert*FromNode` -- is a CSSOM View mixin that
// webref declares and BLINK DOES NOT SHIP. The captured brave-151 surface carries none of the four
// on any of these prototypes, and a live Brave 151 on this machine agrees: `"getBoxQuads" in
// Element.prototype` is `false` and `document.body.getBoxQuads` is `undefined`.
//
// This test exists because a reasonable reading of the WebIDL corpus says to add them, and adding
// them would make the fingerprint WORSE, not better: four members no Chrome has, on four
// prototypes, is a structural diff any page can enumerate. A member that exists and lies is worse
// than one that is missing -- the grader cannot see it and a page can. So the absence is asserted,
// not merely left alone.
#[tokio::test]
async fn the_geometryutils_mixin_stays_absent_because_chrome_does_not_ship_it() {
    let out = run(
        r#"<!doctype html><html><body><div id="d">x</div></body></html>"#,
        r#"
        for (const name of ["getBoxQuads", "convertQuadFromNode", "convertRectFromNode",
                            "convertPointFromNode"]) {
          print(name + "=" + [Element.prototype, Document.prototype, Text.prototype,
                              Node.prototype].map((p) => name in p).join(",")
                + "|" + typeof document.body[name]);
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "getBoxQuads=false,false,false,false|undefined",
            "convertQuadFromNode=false,false,false,false|undefined",
            "convertRectFromNode=false,false,false,false|undefined",
            "convertPointFromNode=false,false,false,false|undefined",
        ],
        "the control Brave 151 answers exactly this"
    );
}

#[tokio::test]
async fn the_untouched_members_still_behave() {
    let out = run(
        r#"<!doctype html><html><body><div id="d">x</div></body></html>"#,
        r#"
        const d = document.getElementById("d");
        print("has=" + ("offsetWidth" in d) + "," + ("offsetParent" in d));
        print("parent=" + d.offsetParent);
        d.scrollTop = 40;
        print("scrollTop=" + d.scrollTop + " of " + (d.scrollHeight - d.clientHeight));
        const s = document.createElement("div");
        s.style.cssText = "height:100px;overflow:auto";
        s.innerHTML = "<div style='height:900px'></div>";
        document.body.appendChild(s);
        s.scrollTop = 40;
        print("scroller=" + s.scrollTop + " of " + (s.scrollHeight - s.clientHeight));
        d.scrollIntoView(); d.focus();
        print("ok");
        "#,
    )
    .await;
    assert_eq!(
        out,
        // `offsetParent` of a positioned-nowhere div in the body is the body (Chrome 151, checked
        // directly). `null` was the stub's answer, and this line pinned it.
        vec![
            "has=true,true".to_string(),
            "parent=[object HTMLBodyElement]".to_string(),
            // `scrollTop=40` was the stub's answer too, and this line pinned THAT: a 24px-tall div
            // with no overflow has nothing to scroll, so Chrome 150.0.7871.46 -- measured on this
            // exact markup over CDP, 2026-09-13 -- leaves `scrollTop` at 0 with `scrollHeight ==
            // clientHeight == 24`. The write is not refused, it is CLAMPED, which the scroll
            // container on the next line shows by taking the same 40.
            "scrollTop=0 of 0".to_string(),
            "scroller=40 of 800".to_string(),
            "ok".to_string(),
        ]
    );
}

// A page measures elements in a loop -- that is what a scraper and a page's own measurement loop both do. The
// layout is cached, so this is linear; without the cache it is a full relayout per element.
//
// The bound is deliberately loose. On this machine 6,000 elements measure in ~0.4s cached; the
// quadratic version took 15s on a page of that size, so anything in between is still a clear
// failure and anything above is a machine three times slower than this one.
//
// The bound is on this thread's CPU time, not the wall clock: the whole workspace suite runs several
// V8 engines at once (`scripts/gate.sh`, 4 threads), and under that contention the same 0.4s of work
// was starved past 5s of wall clock while passing in 0.4s alone. A quadratic cache miss is CPU work,
// which is exactly what CPU time counts and what waiting on a busy machine does not.
#[tokio::test]
async fn measuring_every_element_is_linear_and_not_quadratic() {
    let mut html = String::from("<!doctype html><html><body>");
    for i in 0..6000 {
        html.push_str(&format!("<div class='row' id='r{i}' style='height:3px'></div>"));
    }
    html.push_str("</body></html>");

    let cpu_started = thread_cpu_time();
    let started = std::time::Instant::now();
    let out = run(
        &html,
        r#"
        let nonzero = 0;
        for (const el of document.querySelectorAll("div")) {
          if (el.getBoundingClientRect().width > 0) nonzero++;
        }
        print("nonzero=" + nonzero);
        print("last=" + document.getElementById("r5999").offsetTop);
        "#,
    )
    .await;
    let elapsed = started.elapsed();
    let cpu = thread_cpu_time().saturating_sub(cpu_started);
    // 5,999 rows of 3px above it, plus body's 8px margin.
    assert_eq!(out, vec!["nonzero=6000", "last=18005"]);
    assert!(
        cpu < std::time::Duration::from_secs(5),
        "6,000 measurements took {cpu:?} of CPU time ({elapsed:?} wall); the cache is not holding"
    );
}

/// This thread's user+system CPU time, from `/proc/thread-self/stat` (fields 14 and 15, in USER_HZ =
/// 100 ticks per second). The engine under test runs on the current-thread tokio runtime, so the
/// measuring work is this thread's. Zero if the file cannot be read (a non-Linux host), which makes
/// the bound trivially pass there rather than fail on a missing file.
fn thread_cpu_time() -> std::time::Duration {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap_or_default();
    // Everything after the last ')' (the comm field can contain spaces) starts at field 3.
    let rest = stat.rsplit(')').next().unwrap_or("");
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| fields.get(i).and_then(|f| f.parse::<u64>().ok()).unwrap_or(0);
    // utime is field 14 and stime field 15; `rest` begins at field 3, so they are indices 11 and 12.
    std::time::Duration::from_millis((tick(11) + tick(12)) * 10)
}

//! Korvus without the page pipeline.
//!
//! Definition of Done 4: loading Korvus **without** the page plugin still works, degrading to the
//! zero geometry it had before the pipeline existed. The design's use-case table has embedders that
//! want `fetch` and nothing else, and they should not pay for a cascade.
//!
//! This is the test that makes every consumer's "the service is absent" branch code that actually
//! runs. Without it those branches are unexercised, and an unexercised fallback is a fallback that
//! does not work.
use engine::Engine;
/// Everything a page can ask about geometry and style, with no pipeline behind it.
///
/// One test rather than several because the switch is process-wide: `KORVUS_NO_PAGE` is read when
/// an `Engine` is built, and cargo runs a file's tests in one process by default, so two tests
/// setting and clearing it would race.
#[tokio::test]
async fn every_geometry_member_degrades_instead_of_throwing() {
    std::env::set_var("KORVUS_NO_PAGE", "1");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(
        r#"<!doctype html><html><head><style id="s">div { width: 200px }</style></head>
           <body><div id="d" style="height:40px">hello</div></body></html>"#,
    )
    .unwrap();
    e.eval(
        "t",
        r#"
        const d = document.getElementById("d");
        const r = d.getBoundingClientRect();
        print("rect=" + [r.x, r.y, r.width, r.height].join(","));
        print("metrics=" + [d.offsetWidth, d.offsetHeight, d.clientWidth].join(","));
        print("rects=" + d.getClientRects().length);
        print("point=" + document.elementFromPoint(10, 10));
        // The CSSOM is still there and still an object; it just has no rules in it.
        const sheet = document.getElementById("s").sheet;
        print("sheet=" + (sheet !== null && sheet !== undefined) + " rules=" + sheet.cssRules.length);
        // getComputedStyle answers with an empty declaration rather than throwing.
        const cs = getComputedStyle(d);
        print("computed=" + cs.length + " display=[" + cs.display + "]");
        // measureText still returns a finite number -- a page reading `.width` must get one.
        const c = document.createElement("canvas").getContext("2d");
        print("measure=" + Number.isFinite(c.measureText("hello").width));
        print("ok");
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    std::env::remove_var("KORVUS_NO_PAGE");

    assert_eq!(
        e.take_output(),
        vec![
            "rect=0,0,0,0",
            "metrics=0,0,0",
            "rects=0",
            "point=null",
            // A sheet handle is still handed out; it is simply backed by nothing.
            "sheet=true rules=0",
            "computed=0 display=[]",
            "measure=true",
            "ok",
        ],
        "nothing threw, and every answer is the zero this engine gave before the pipeline existed"
    );
}

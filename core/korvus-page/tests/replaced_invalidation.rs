//! P9d x P9b: a replaced element's size reads DOM inputs the cascade does not -- `width`/`height` on
//! `<img>`/`<canvas>`/`<video>`, `size` on `<input>`, `cols`/`rows` on `<textarea>`, an `<option>`'s
//! text, an `<svg>`'s `viewBox` -- and `Page::layout` keeps the last layout's styles, restyling only
//! what a write can reach (P9b's `MatchCache` kept matched rules across `style` writes).
//!
//! That is safe only if every one of those inputs is read again by the layout that follows a write
//! to it. It is, by construction: the box tree is rebuilt from the DOM on every `Page::layout`, and
//! only STYLES are reused. This proves it by COUNT, one layout per write: every layout after the first
//! styles what changed (the counter moves by one), and every size follows its input and equals a cold
//! page's.
use korvus_dom::{Dom, NodeId};
use page::style::media::MediaContext;
use page::Page;

fn element(dom: &Dom, id: &str) -> NodeId {
    (0..dom.len() as NodeId)
        .find(|&n| dom.get_attribute(n, "id") == Some(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

const IDS: [&str; 6] = ["c", "m", "i", "t", "s", "v"];

/// One layout of `page`; every sized node's (width, height), each checked against a cold page's, and
/// how many layouts reused matches as of this one.
fn step(page: &Page, dom: &Dom) -> (Vec<(f32, f32)>, u64) {
    let media = MediaContext::default();
    let laid = page.layout(dom, &media);
    let cold = Page::default().layout(dom, &media);
    let sizes = IDS
        .iter()
        .map(|id| {
            let n = element(dom, id);
            let (r, k) = (laid.bounding_rect(n), cold.bounding_rect(n));
            assert_eq!(r, k, "#{id} differs from a cold layout");
            (r.width, r.height)
        })
        .collect();
    (sizes, page.layouts_reusing_matches())
}

#[test]
fn a_sizing_input_write_is_laid_out_again_and_a_style_write_still_reuses_matches() {
    let page = Page::default();
    let mut dom = Dom::parse(
        "<head><style>.u { font: 16px Arial; padding: 0; border: 0 }</style></head><body>\
         <div id='d'>x</div><canvas id='c'></canvas><img id='m' width=40 height=30>\
         <input id='i' class='u'><textarea id='t' class='u'></textarea>\
         <select id='s' class='u'><option id='o'>a</option></select>\
         <svg id='v' viewBox='0 0 10 5' width=100></svg></body>",
    );
    let [c, m, i, t, _s, v] = IDS.map(|id| element(&dom, id));
    let (d, o) = (element(&dom, "d"), element(&dom, "o"));

    let (first, reused) = step(&page, &dom);
    assert_eq!(first[0], (300.0, 150.0));
    assert_eq!(first[5], (100.0, 50.0));
    assert_eq!(reused, 0);

    // A style write: a reuse, sizes unchanged and as cold.
    dom.set_attribute(d, "style", "height: 5px");
    let (sizes, count) = step(&page, &dom);
    assert_eq!(count, 1, "a style write restyles what changed");
    assert_eq!(sizes, first);

    // Each sizing input, written, then ONE layout: not a reuse, and the size follows the input.
    let writes: Vec<(&str, Box<dyn Fn(&mut Dom)>, usize, Box<dyn Fn((f32, f32)) -> bool>)> = vec![
        ("canvas width", Box::new(move |dom| dom.set_attribute(c, "width", "50")), 0, Box::new(|s| s == (50.0, 150.0))),
        ("img height", Box::new(move |dom| dom.set_attribute(m, "height", "60")), 1, Box::new(|s| s == (40.0, 60.0))),
        ("input size", Box::new(move |dom| dom.set_attribute(i, "size", "5")), 2, Box::new(|s| s.0 > 0.0)),
        ("textarea rows", Box::new(move |dom| dom.set_attribute(t, "rows", "4")), 3, Box::new(|s| s.1 == 72.0)),
        ("option text", Box::new(move |dom| dom.set_text_content(o, "a much longer label")), 4, Box::new(|s| s.0 > 0.0)),
        ("svg viewBox", Box::new(move |dom| dom.set_attribute(v, "viewBox", "0 0 10 10")), 5, Box::new(|s| s == (100.0, 100.0))),
    ];
    let (mut last, mut count) = step(&page, &dom);
    assert_eq!(count, 2, "a layout with no write in between is a reuse too");
    for (what, write, index, holds) in writes {
        write(&mut dom);
        let (sizes, now) = step(&page, &dom);
        assert_eq!(now, count + 1, "{what}: the element is styled again, the rest reused");
        assert_ne!(sizes[index], last[index], "{what}: the size did not follow the write");
        assert!(holds(sizes[index]), "{what}: got {:?}", sizes[index]);
        (last, count) = (sizes, now);
    }

    // A style write ON a replaced element: a reuse, and the 50x150 bitmap's ratio sizes the height.
    dom.set_attribute(c, "style", "width: 100px");
    let (sizes, now) = step(&page, &dom);
    assert_eq!(now, count + 1, "a style write restyles what changed");
    assert_eq!(sizes[0], (100.0, 300.0));
}

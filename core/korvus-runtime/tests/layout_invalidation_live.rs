//! A document is laid out again only when something it renders has changed.
//!
//! Nothing is laid out again for a node made or changed outside every document, an attribute rewritten
//! with its own value, or a `style` rewrite that changes only the transform family and `opacity` (which
//! korvus lays out, hit-tests and resolves without). Every change a document can see is still seen,
//! including an opacity write that a `[style*=...]` selector turns into a height. The values are Chrome
//! 150's (headless, measured 2026-09-30); the counts are `Engine::layout_counts`, not a clock.
use engine::Engine;

async fn loaded(html: &str) -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.run().await.unwrap();
    e
}

fn eval(e: &mut Engine, name: &'static str, js: &str) -> String {
    match e.eval_value(name, js.to_string()).unwrap() {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

const STYLE: &str = "<style>body{margin:0} div{height:10px} #a{height:20px} .tall{height:40px}</style>";

/// Between two reads: a subtree built off-document (an element, a clone in a fragment, an `innerHTML`
/// parse), an attribute rewritten with its own value, and `transform` / `opacity` writes. The second
/// read lays nothing out, and the computed style still answers the new transform and opacity.
#[tokio::test]
async fn nothing_outside_a_document_lays_the_page_out_again() {
    let mut e = loaded(&format!(r#"<!doctype html><html><head>{STYLE}</head><body><div id="a"></div><div id="b"></div></body></html>"#)).await;
    assert_eq!(eval(&mut e, "a6-first", "String(document.getElementById('b').offsetTop)"), "20");
    let before = e.layout_counts();
    let out = eval(
        &mut e,
        "a6-off-document",
        r#"const a = document.getElementById('a'), b = document.getElementById('b');
           const d = document.createElement('div'); d.className = 'tall'; d.setAttribute('data-x', '1'); d.textContent = 'x';
           const frag = document.createDocumentFragment(); frag.appendChild(d.cloneNode(true));
           const w = document.createElement('div'); w.innerHTML = '<div class=tall></div><p>y</p>';
           b.setAttribute('id', 'b');
           a.style.transform = 'translateX(7px)'; a.style.opacity = '0.5';
           [b.offsetTop, getComputedStyle(a).transform, getComputedStyle(a).opacity].join(' ')"#,
    );
    assert_eq!(out, "20 matrix(1, 0, 0, 1, 7, 0) 0.5");
    assert_eq!(e.layout_counts(), before, "work no document can see laid the page out again");
}

/// Each change a document CAN see, read straight after it:
/// - a built subtree inserted;
/// - a connected element moved into a detached one (the document loses it: `unlink` asks where the node
///   was);
/// - a rewrite that changes a height together with a transform;
/// - text;
/// - an element inside a frame's document.
#[tokio::test]
async fn every_change_a_document_can_see_is_seen() {
    let mut e = loaded(&format!(
        r#"<!doctype html><html><head>{STYLE}</head><body><div id="a"></div><div id="b"></div><span id="t" style="display:inline-block">xx</span><iframe id="f" srcdoc="<style>body{{margin:0}}</style><div id=in style=&quot;height:5px&quot;></div>"></iframe></body></html>"#
    ))
    .await;
    let out = eval(
        &mut e,
        "a6-seen",
        r#"const out = [], $ = (id, d) => (d || document).getElementById(id);
           const a = $('a'), b = $('b'), t = $('t');
           out.push(b.offsetTop);
           const d = document.createElement('div'); d.className = 'tall';
           const w = document.createElement('div');
           document.body.insertBefore(d, b); out.push(b.offsetTop);
           w.appendChild(a); out.push(b.offsetTop);
           d.style.cssText = 'height: 15px; transform: translateX(1px)'; out.push(b.offsetTop);
           const w0 = t.offsetWidth; t.firstChild.data = 'xxxxxxxx'; out.push(t.offsetWidth > w0);
           const inner = $('in', $('f').contentDocument);
           out.push(inner.offsetHeight); inner.style.height = '9px'; out.push(inner.offsetHeight);
           out.join(' ')"#,
    );
    assert_eq!(out, "20 60 40 15 true 5 9");
}

/// A selector that reads the `style` attribute itself makes an `opacity` write a layout input: here it
/// sets the height, so the write must be laid out.
#[tokio::test]
async fn a_selector_that_reads_the_style_attribute_keeps_those_writes_live() {
    let mut e = loaded(r#"<!doctype html><html><head><style>body{margin:0} div{height:10px} #a{height:20px} #a[style*="opacity: 0.25"]{height:30px}</style></head><body><div id="a"></div></body></html>"#).await;
    let out = eval(&mut e, "a6-style-read", r#"const a = document.getElementById('a'); const h0 = a.offsetHeight; a.style.opacity = '0.25'; h0 + ' ' + a.offsetHeight"#);
    assert_eq!(out, "20 30");
}


/// A change inside a FRAME's document is that document's, not the page's.
/// (a) The page is not laid out again for it -- neither for a page geometry read after it nor for a frame
///     style read, which resolves against the frame's viewport (its element's box in the page's layout).
///     Counting the frame's changes as the page's relaid the page out per frame mutation: highcharts.com
///     took 326 G instructions to load, and past 300 s once frame styles were resolved (F27).
/// (b) The frame's own geometry still follows the change.
#[tokio::test]
async fn a_frames_changes_do_not_lay_the_page_out_again() {
    let mut e = loaded(&format!(
        r#"<!doctype html><html><head>{STYLE}</head><body><div id="a"></div><div id="b"></div><iframe id="f" srcdoc="<style>body{{margin:0}}</style><div id=in style=&quot;height:5px&quot;></div><p id=p>x</p>"></iframe></body></html>"#
    ))
    .await;
    let warm = eval(
        &mut e,
        "f27b-warm",
        r#"(() => { const d = document.getElementById('f').contentDocument, w = document.getElementById('f').contentWindow;
           return [document.getElementById('b').offsetTop, d.getElementById('in').offsetHeight, w.getComputedStyle(d.getElementById('p')).display].join(' '); })()"#,
    );
    assert_eq!(warm, "20 5 block");
    let (pages0, frames0) = e.layout_counts();
    let out = eval(
        &mut e,
        "f27b-frame-writes",
        r#"(() => { const d = document.getElementById('f').contentDocument, w = document.getElementById('f').contentWindow;
           const inner = d.getElementById('in'), p = d.getElementById('p'), out = [];
           for (let i = 0; i < 5; i++) {
             inner.style.height = (6 + i) + 'px'; p.textContent = 'y' + i; d.body.appendChild(d.createElement('span'));
             out.push(w.getComputedStyle(p).display, document.getElementById('b').offsetTop, inner.offsetHeight);
           }
           return out.join(' '); })()"#,
    );
    assert_eq!(out, "block 20 6 block 20 7 block 20 8 block 20 9 block 20 10", "the frame's geometry follows its changes");
    let (pages1, frames1) = e.layout_counts();
    assert_eq!(pages1, pages0, "a frame's changes laid the PAGE out again");
    assert!(frames1 > frames0, "the frame was not laid out again after its own changes");
}

/// A FRAME's layout is its own document's. A page write that moves the frame without resizing it
/// lays the page out again and not the frame, whose geometry and style reads keep answering from its layout.
#[tokio::test]
async fn a_page_write_does_not_lay_a_frame_out_again() {
    let mut e = loaded(&format!(
        r#"<!doctype html><html><head>{STYLE}</head><body><div id="a"></div><div id="b"></div><iframe id="f" srcdoc="<style>body{{margin:0}}</style><div id=in style=&quot;height:5px&quot;></div><p id=p>x</p>"></iframe></body></html>"#
    ))
    .await;
    let warm = eval(
        &mut e,
        "f27c-warm",
        r#"(() => { const d = document.getElementById('f').contentDocument, w = document.getElementById('f').contentWindow;
           return [document.getElementById('f').offsetTop, d.getElementById('in').offsetHeight, w.getComputedStyle(d.getElementById('p')).display].join(' '); })()"#,
    );
    assert_eq!(warm, "30 5 block");
    let (pages0, frames0) = e.layout_counts();
    let out = eval(
        &mut e,
        "f27c-page-writes",
        r#"(() => { const d = document.getElementById('f').contentDocument, w = document.getElementById('f').contentWindow;
           const b = document.getElementById('b'), f = document.getElementById('f'), out = [];
           for (let i = 0; i < 5; i++) {
             b.className = i % 2 ? '' : 'tall';
             out.push(f.offsetTop, d.getElementById('in').offsetHeight, w.getComputedStyle(d.getElementById('p')).display);
           }
           return out.join(' '); })()"#,
    );
    assert_eq!(out, "60 5 block 30 5 block 60 5 block 30 5 block 60 5 block", "the page's geometry follows its changes");
    let (pages1, frames1) = e.layout_counts();
    assert!(pages1 > pages0, "the page was not laid out again after its own changes");
    assert_eq!(frames1, frames0, "a PAGE write that leaves the frame's box as it was laid the FRAME out again");
}

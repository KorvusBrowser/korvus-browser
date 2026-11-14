//! Text the persona's captured tables do not cover is laid out as the paint shapes it (issue #18).
//!
//! HyperFrames' `html-dir-rtl` sets a bold 48px `مرحبا بالعالم` at `right: 32px` in an `ar` document. Layout measured
//! it from the tables, which hold no Arabic: each letter at the face's zero advance, unjoined -- in `system-ui,
//! sans-serif` 336 px, while the paint shaped it in the fonts it is drawn in, 295 px, from the box's left edge, 38 px
//! short of where its box ends. Chrome 155 lays the fixture's out 230.156 wide and draws it edge to edge. The paint and a canvas shape text through the
//! same backend, so the width a word is laid out at must be the width `measureText` answers in the same font and
//! language. (A space is still measured from the persona's table, as Latin text is: here its `sans-serif` space and the
//! host face the paint draws `sans-serif` in differ by 2.55 px at 48px bold; in HyperFrames' `Inter` they agree.)

#[tokio::test]
async fn arabic_text_is_laid_out_at_the_width_it_is_drawn_at() {
    let script = r#"(() => {
        document.documentElement.lang = 'ar';
        document.documentElement.dir = 'rtl';
        const font = '700 48px sans-serif';
        const add = (text) => { const h = document.createElement('h1'); h.textContent = text; h.style.cssText = `position:absolute;right:32px;margin:0;font:${font}`; document.body.append(h); return h; };
        const ar = add('بالعالم'), latin = add('Hello');
        const canvas = document.createElement('canvas');
        document.body.append(canvas);
        const c = canvas.getContext('2d');
        c.font = font;
        const a = ar.getBoundingClientRect(), l = latin.getBoundingClientRect();
        return { laid: a.width, drawn: c.measureText('بالعالم').width, end: a.right, edge: document.documentElement.clientWidth - 32, taller: a.height >= l.height };
    })()"#;
    let got = crate::keyboard_live::answers_value(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let (laid, drawn) = (got["laid"].as_f64().unwrap(), got["drawn"].as_f64().unwrap());
    // a layout width is the advance rounded up to a 1/64 px LayoutUnit
    assert!((laid - drawn).abs() <= 1.0 / 64.0 + 1e-6, "laid out {laid}, drawn {drawn}: {got}");
    assert!(drawn > 60.0, "the word is shaped in a font that has it: {got}");
    assert_eq!(got["end"], got["edge"], "the box ends at right: 32px: {got}");
    assert_eq!(got["taller"], true, "a fallback font never shortens a line-height: normal line: {got}");
}

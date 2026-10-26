//! `innerText` asks computed style for THREE properties, and they are three different
//! rules: `display`, which the engine already consulted, plus `visibility` and `content-visibility`,
//! which it did not.
//!
//! # The oracle
//!
//! Every string asserted here is **stock Google Chrome 150.0.7871.46**'s own answer, measured on
//! 2026-09-14 over CDP on a loopback fixture (the binary launched by us with
//! `--remote-debugging-port=0`, never by Playwright's launcher). None of it is read off a
//! specification: this lane exists because `FIRST-MEASUREMENT.md` §10.3's one-line summary,
//! *"`innerText` never consults computed style"*, was too strong, and the brief that replaced it was
//! itself narrower than the truth in three places. So each row below is a browser's answer, and the
//! comment beside a row says what the browser said rather than what a rule ought to be.
//!
//! **A note on how they were measured.** `tools/parity/textprobe.py` reports through `document.title`,
//! and the `document.title` GETTER strips and collapses runs of ASCII whitespace. That is exact for a
//! single space and LOSSY for a run of two — read through it, Chrome's `<pre>  p1</pre>` answer comes
//! back with one space where the DOM has two, which looks exactly like a `<pre>` defect that is not
//! there. The answers below were re-measured through a base64'd title, which has no whitespace runs.
//!
//! # The three rules, and why one filter would have been wrong for all three
//!
//! ```text
//!   display: none            no box at all      subtree skipped, and TRANSPARENT to white-space
//!                                               collapsing: `A <span style=display:none>H</span> B`
//!                                               is "A B", one space.
//!   visibility: hidden       PER ELEMENT        it INHERITS, so a descendant can set
//!            / collapse                         `visibility: visible` and come back — which
//!                                               `display: none` can never do. The hidden element
//!                                               emits no text and no break of its own, but its
//!                                               text is still LAID OUT, so it still separates two
//!                                               white-space runs.
//!   content-visibility:      PER SUBTREE        the opposite: a `visibility: visible` descendant
//!            hidden                             does NOT come back. Inert on a non-atomic inline,
//!                                               because size containment cannot apply there.
//! ```
//!
//! # `content-visibility: auto` is deliberately NOT filtered
//!
//! Measured rather than assumed, because guessing here is how a rule ends up right on a fixture and
//! wrong on a page. Chrome skips an `auto` subtree only while it is not relevant to the user: read
//! synchronously, an in-viewport `content-visibility: auto` element reads `""`; read after one
//! rendering opportunity, the same element reads its full text and only the below-the-fold one stays
//! empty. `getComputedStyle` reports the specified `auto` in both engines, and this engine has no
//! viewport-relevance machinery to ask — so treating `auto` as `hidden` would delete the text of
//! every lazily-rendered list. `auto_is_not_hidden` is the assertion that keeps that from being
//! "fixed" later.

use engine::Engine;

/// One page, one `innerText`. The markup is wrapped so that the element under test is `#t`, and the
/// answer is printed with `JSON.stringify` so a newline, a tab and a trailing space are all visible
/// in the failure message rather than being eaten by it.
async fn inner_text(markup: &str) -> String {
    let html = format!("<!doctype html><html><body>{markup}</body></html>");
    let mut e = Engine::new(persona::Overridden::new().boxed());
    e.run().await.unwrap();
    e.load_html(&html).unwrap();
    e.eval("p7a", "print(JSON.stringify(document.getElementById('t').innerText))".to_string()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    let line = out.last().cloned().unwrap_or_default();
    serde_json::from_str::<String>(line.trim()).unwrap_or_else(|_| panic!("not a JSON string: {line:?}"))
}

/// `want` is what Chrome 150 answered on this exact markup. No `#[track_caller]`: it is ungated on an
/// async fn and would be a new warning, and the panic already prints the markup, which names the row.
async fn chrome_says(markup: &str, want: &str) {
    let got = inner_text(markup).await;
    assert_eq!(got, want, "\n  markup: {markup}\n  chrome: {want:?}\n  korvus: {got:?}");
}

/// THE UNIT, in one test: the brief's seven subtrees on one line, in the order it measured them.
///
/// Chrome 150: `"KEEP1  OPACITY0\nCOLLAPSED\nOFFSCREEN\nCLIPPED KEEP2"`. Before this unit Korvus read
/// `"KEEP1  VISHIDDEN OPACITY0 \nCONTENTVIS\n \nCOLLAPSED\n \nOFFSCREEN\n CLIPPED KEEP2"` — two
/// subtrees Chrome drops, and a stray space at every block gap.
///
/// The two spaces in `"KEEP1  OPACITY0"` are not a typo and they are the reason this row is
/// interesting: the `display:none` span is transparent, so the space before it and the space after it
/// collapse into one — and then the `visibility:hidden` span's text still occupies the line, so the
/// space on ITS far side cannot collapse into that one. One rule would not produce both.
#[tokio::test]
async fn the_seven_subtrees() {
    chrome_says(
        "<div id=t>KEEP1 <span style=\"display:none\">DISPNONE</span> \
         <span style=\"visibility:hidden\">VISHIDDEN</span> \
         <span style=\"opacity:0\">OPACITY0</span> \
         <div style=\"content-visibility:hidden\">CONTENTVIS</div> \
         <div style=\"height:0;overflow:hidden\">COLLAPSED</div> \
         <div style=\"position:absolute;left:-9999px\">OFFSCREEN</div> \
         <span style=\"clip-path:inset(50%)\">CLIPPED</span> KEEP2</div>",
        "KEEP1  OPACITY0\nCOLLAPSED\nOFFSCREEN\nCLIPPED KEEP2",
    )
    .await;
}

/// THE TRAP: `visibility` inherits, so the filter is PER ELEMENT and never per subtree.
///
/// A filter that treated `visibility: hidden` the way `display: none` is treated — skip the subtree —
/// would pass every row of `visibility_hidden_is_excluded` and lose the text Chrome keeps here. Skip
/// links, off-canvas menus and tab panels are written exactly this way.
#[tokio::test]
async fn a_visible_descendant_of_a_hidden_element_comes_back() {
    // The wrapper is hidden and the child sets itself visible: Chrome keeps only the child.
    chrome_says("<div id=t style=\"visibility:hidden\">PARENT<span style=\"visibility:visible\">CHILD</span></div>", "CHILD").await;
    // Through an intermediate element that merely inherits the hidden value.
    chrome_says("<div id=t style=\"visibility:hidden\">PARENT<span><b style=\"visibility:visible\">DEEP</b></span></div>", "DEEP").await;
    // And it is the TEXT NODE'S OWN parent that decides, not the subtree: HID and HID2 go, BACK stays.
    chrome_says("<div id=t>A<span style=\"visibility:hidden\">HID<span style=\"visibility:visible\">BACK</span>HID2</span>B</div>", "ABACKB").await;
    // A visible BLOCK inside a hidden one still contributes its OWN required line breaks...
    chrome_says("<div id=t>A<div style=\"visibility:hidden\">P<div style=\"visibility:visible\">C</div></div>B</div>", "A\nC\nB").await;
    // ...while a visible INLINE inside one contributes none, because the hidden block carries none.
    chrome_says("<div id=t>A<div style=\"visibility:hidden\">P<span style=\"visibility:visible\">C</span></div>B</div>", "ACB").await;
}

/// `visibility: hidden` and `collapse` are both excluded, and the hidden element contributes NO
/// required line break of its own — `"AB"`, not `"A\nB"`.
#[tokio::test]
async fn visibility_hidden_is_excluded() {
    chrome_says("<div id=t>A<span style=\"visibility:hidden\">HID</span>B</div>", "AB").await;
    chrome_says("<div id=t>A<div style=\"visibility:hidden\">HID</div>B</div>", "AB").await;
    chrome_says("<div id=t>A<span style=\"visibility:collapse\">COLL</span>B</div>", "AB").await;
    // Not even a `<p>`, whose break count is 2 when it is visible.
    chrome_says("<div id=t>A<p style=\"visibility:hidden\">H</p>B</div>", "AB").await;
    // Between two visible blocks it adds nothing at all: one newline, not three.
    chrome_says("<div id=t><div>M</div><div style=\"visibility:hidden\">H</div><div>N</div></div>", "M\nN").await;
    // The element innerText is READ ON: its own hidden text goes, its visible child stays.
    chrome_says("<div id=t style=\"visibility:hidden\">SELFVH<span style=\"visibility:visible\">SELFVV</span></div>", "SELFVV").await;
}

/// `content-visibility: hidden` is the OPPOSITE rule: the whole subtree goes, and a
/// `visibility: visible` descendant does not rescue it.
#[tokio::test]
async fn content_visibility_hidden_skips_the_subtree() {
    chrome_says("<div id=t>A<div style=\"content-visibility:hidden\">HID</div>B</div>", "AB").await;
    chrome_says("<div id=t>A<div style=\"content-visibility:hidden\">HID<span style=\"visibility:visible\">VV</span></div>B</div>", "AB").await;
    // Read ON the skipped element it is empty -- and NOT `textContent`, which is what an element that
    // is not being rendered at all would answer.
    chrome_says("<div id=t style=\"content-visibility:hidden\">SELFCVH</div>", "").await;
    // INERT on a non-atomic inline, because size containment cannot apply to one. Chrome keeps `H`.
    chrome_says("<div id=t>A <span style=\"content-visibility:hidden\">H</span> B</div>", "A H B").await;
    chrome_says("<div id=t>A <span style=\"display:contents;content-visibility:hidden\">H</span> B</div>", "A H B").await;
    // It DOES apply to an atomic inline -- and both spaces survive, because the box occupies the line.
    chrome_says("<div id=t>A <span style=\"display:inline-block;content-visibility:hidden\">H</span> B</div>", "A  B").await;
}

/// `content-visibility: auto` is NOT `hidden`, and this engine must not pretend otherwise.
///
/// Chrome 150, measured both ways on one page: read synchronously an in-viewport `auto` element is
/// `""`, and read after two `requestAnimationFrame`s plus a timer it is `"NEARAUTO"` — its full text.
/// Only the element parked below a 3,000px spacer stays `""` on both reads. The skip is the viewport
/// relevance check, not the property, and this engine has nothing to ask; `"AUTOVIS"` is the answer
/// that is right on a page.
#[tokio::test]
async fn auto_is_not_hidden() {
    chrome_says("<div id=t>A<div style=\"content-visibility:auto\">AUTOVIS</div>B</div>", "A\nAUTOVIS\nB").await;
}

/// The other four subtrees of the brief's table, which were ALREADY right and must stay right. A
/// filter that reached for "looks invisible" rather than for a named property would break every one:
/// screen-reader affordances are written with exactly these declarations and Chrome reads them out.
#[tokio::test]
async fn the_four_that_chrome_keeps() {
    chrome_says("<div id=t>A<span style=\"opacity:0\">OP</span>B</div>", "AOPB").await;
    chrome_says("<div id=t>A<div style=\"height:0;overflow:hidden\">CO</div>B</div>", "A\nCO\nB").await;
    chrome_says("<div id=t>A<div style=\"position:absolute;left:-9999px\">OFF</div>B</div>", "A\nOFF\nB").await;
    chrome_says("<div id=t>A<span style=\"clip-path:inset(50%)\">CL</span>B</div>", "ACLB").await;
    // The `.sronly` idiom, which is the shape that actually appears on pages — clipped to one pixel
    // and still read out in full.
    //
    // MEASURED RESIDUE, and it is NOT this file's to fix. The real idiom also carries
    // `position:absolute`, and there Chrome answers `"visible\nskip to navigation"` where this engine
    // answers `"visibleskip to navigation"`: CSS BLOCKIFIES an absolutely positioned, fixed or
    // floated box, so Chrome's computed `display` for such a `<span>` is `block` and the break falls
    // out of the rule already implemented here — `korvus-page` reports `inline` instead. The defect is
    // one property's computed value in `core/korvus-page/src/style/`, not a rule in the serialisation,
    // which is why the row below names the clip and leaves the positioning to the unit that owns it.
    chrome_says(
        "<div id=t>visible<span style=\"width:1px;height:1px;overflow:hidden;\
         clip-path:inset(50%);white-space:nowrap\">skip to navigation</span></div>",
        "visibleskip to navigation",
    )
    .await;
}

/// `<script>` and `<style>` vanish because the UA sheet gives them `display: none` and this engine
/// reports it. That is the mechanism, and a tag list would be a second, divergent one (I5) — so this
/// asserts the OUTCOME on the elements a tag list would have named, to catch anyone adding one.
#[tokio::test]
async fn script_and_style_go_by_display_not_by_name() {
    chrome_says("<div id=t>before<style>.x{border:1px solid red}</style>after</div>", "beforeafter").await;
    chrome_says("<div id=t>before<script>var zzz=1;</script>after</div>", "beforeafter").await;
    chrome_says("<div id=t>before<template>templatetext</template>after</div>", "beforeafter").await;
    chrome_says("<div id=t>before<!-- commenttext -->after</div>", "beforeafter").await;
    // `aria-hidden` is NOT a rendering property, and Chrome reads straight through it.
    chrome_says("<div id=t>before<div aria-hidden=\"true\">ariahidden</div>after</div>", "before\nariahidden\nafter").await;
}

/// The white-space rule, derived rather than patched: a collapsible space is dropped at the start of
/// the result, at the end of it, and on either side of a line break. Before this unit Korvus trimmed
/// nothing and wrote `" \n"` at every block gap.
#[tokio::test]
async fn collapsible_space_dies_at_every_line_edge() {
    chrome_says("<div id=t>   LEAD</div>", "LEAD").await;
    chrome_says("<div id=t>TRAIL   </div>", "TRAIL").await;
    chrome_says("<div id=t>   </div>", "").await;
    chrome_says("<div id=t>A <div>M</div> B</div>", "A\nM\nB").await;
    chrome_says("<div id=t>A<div> M </div>B</div>", "A\nM\nB").await;
    chrome_says("<div id=t>A<div>M</div>   </div>", "A\nM").await;
    chrome_says("<div id=t>   <div>M</div>B</div>", "M\nB").await;
    // An empty or blank block still emits ONE break, and two of them still emit one.
    chrome_says("<div id=t>A<div></div>B</div>", "A\nB").await;
    chrome_says("<div id=t>A<div>   </div>B</div>", "A\nB").await;
    chrome_says("<div id=t>A<div></div><div></div>B</div>", "A\nB").await;
    chrome_says("<div id=t>A <div></div> B</div>", "A\nB").await;
    // Inline spaces are untouched, and a run of two collapses to one.
    chrome_says("<div id=t>A <span>M</span> B</div>", "A M B").await;
    chrome_says("<div id=t>A  B</div>", "A B").await;
    // NBSP is not collapsible white space and is not trimmed.
    chrome_says("<div id=t>\u{a0}NB\u{a0}</div>", "\u{a0}NB\u{a0}").await;
}

/// Which side of a collapsed space run survives decides whether the space is emitted, and Chrome
/// takes the FIRST: a run that starts inside the hidden span is gone, a run that starts outside it
/// stays. Two rows that differ only in where the span sits, and that answer differently.
#[tokio::test]
async fn a_collapsed_space_run_takes_its_visibility_from_its_first_space() {
    chrome_says("<div id=t>A<span style=\"visibility:hidden\"> </span> B</div>", "AB").await;
    chrome_says("<div id=t>A <span style=\"visibility:hidden\"> </span>B</div>", "A B").await;
    // Hidden text still OCCUPIES the line, so the spaces either side of it cannot collapse together.
    chrome_says("<div id=t>A <span style=\"visibility:hidden\">H</span> B</div>", "A  B").await;
    // ...whereas `display:none` generates no box at all, and they do.
    chrome_says("<div id=t>A <span style=\"display:none\">H</span> B</div>", "A B").await;
    chrome_says("<div id=t>A <div style=\"display:none\">H</div> B</div>", "A B").await;
    // A hidden non-space character also ends the line's start, so the space after it survives.
    chrome_says("<div id=t>A<div>  <span style=\"visibility:hidden\">H</span> B</div></div>", "A\n B").await;
}

/// `<br>` is not a required line break and does not behave like one: it is never dropped at either
/// end of the result, and it never merges into an adjacent block gap.
#[tokio::test]
async fn br_is_not_a_block_gap() {
    chrome_says("<div id=t><br>A<br></div>", "\nA\n").await;
    chrome_says("<div id=t> <br>A</div>", "\nA").await;
    chrome_says("<div id=t>A <br> B</div>", "A\nB").await;
    chrome_says("<div id=t>A<br><div>M</div>B</div>", "A\n\nM\nB").await;
    chrome_says("<div id=t>A<div>M</div><br>B</div>", "A\nM\n\nB").await;
}

/// `<pre>` preserves its white space, including a two-space indent and a trailing run — the rows that
/// `document.title` cannot carry, and that were re-measured through base64 to state them here.
#[tokio::test]
async fn pre_preserves_its_white_space() {
    chrome_says("<div id=t>A<pre>  p1\n  p2</pre>B</div>", "A\n  p1\n  p2\nB").await;
    chrome_says("<div id=t><pre>   lead</pre></div>", "   lead").await;
}

/// The block-gap rules that were already right and that the new white-space model must not move:
/// `<p>` is worth two breaks, a block-level child one, and an inline child none.
#[tokio::test]
async fn the_block_gaps_that_were_already_right() {
    chrome_says("<div id=t><div>alpha</div><div>beta</div></div>", "alpha\nbeta").await;
    chrome_says("<div id=t><p>one</p><p>two</p></div>", "one\n\ntwo").await;
    chrome_says("<div id=t><h1>heading</h1>body</div>", "heading\nbody").await;
    chrome_says("<div id=t>visible<span>inline</span></div>", "visibleinline").await;
    chrome_says("<div id=t>visible<span style=\"display:inline-block\">inlineblock</span></div>", "visibleinlineblock").await;
    chrome_says("<div id=t>visible<span style=\"display:block\">blockspan</span></div>", "visible\nblockspan").await;
    chrome_says("<div id=t>before<div style=\"display:contents\">contentsdisp</div>after</div>", "beforecontentsdispafter").await;
    chrome_says("<div id=t>visible<span style=\"display:none\">nonetext</span></div>", "visible").await;
}


/// `innerText` is the RENDERED text, so it carries the case the box PAINTS.
///
/// The fourth property the reader asks computed style for. `text-transform` was not consulted at
/// all, so every uppercased navigation on the corpus read back in its source case. Chrome
/// 150.0.7871.46, measured on a UTF-8 loopback fixture, is every `want` below.
///
/// **`straße` is the row that matters most and it is not a curiosity.** It uppercases to TWO
/// characters, so this transform changes `text_len` — an ASCII-only uppercase would pass every
/// other row here and be wrong on it, and no ASCII fixture could have shown that.
#[tokio::test]
async fn text_transform_is_applied_because_inner_text_is_the_rendered_text() {
    chrome_says("<div id=t style=\"text-transform:uppercase\">Community Projects</div>",
                "COMMUNITY PROJECTS").await;
    chrome_says("<div id=t style=\"text-transform:lowercase\">SEE ALL PROJECTS</div>",
                "see all projects").await;
    // It INHERITS, so a child element with no rule of its own is transformed too.
    chrome_says("<div id=t style=\"text-transform:uppercase\">outer <span>inner</span> tail</div>",
                "OUTER INNER TAIL").await;
    // ... and a child that overrides it wins for its own text, which is what makes this a
    // per-element read and not one value handed down the subtree.
    chrome_says("<div id=t style=\"text-transform:uppercase\">A<span style=\"text-transform:lowercase\">BcD</span>e</div>",
                "AbcdE").await;
    // The FULL Unicode mapping, not the ASCII one: `ß` -> `SS` grows the string.
    chrome_says("<div id=t style=\"text-transform:uppercase\">straße mañana ıstanbul</div>",
                "STRASSE MAÑANA ISTANBUL").await;
    // It runs on a preserved run too, and does not disturb it.
    chrome_says("<div id=t style=\"text-transform:uppercase\"><pre>  keep  me</pre></div>",
                "  KEEP  ME").await;
    // And it COMPOSES with the other three rules rather than replacing them: the hidden span
    // still contributes nothing, and the transform does not resurrect it.
    chrome_says("<div id=t style=\"text-transform:uppercase\">a<span style=\"visibility:hidden\">hid</span>b</div>",
                "AB").await;
    chrome_says("<div id=t style=\"text-transform:none\">Left Alone</div>", "Left Alone").await;
}

/// The half of `text-transform` this engine does NOT apply, asserted as UNAPPLIED rather than left
/// to be discovered — and with Chrome's own answer beside each row, so the lane that closes it has
/// the target rather than a description.
///
/// `capitalize` needs the word-start state of the whole TEXT RUN: Chrome capitalises after a `-`
/// but not after a `'`, leaves a digit-initial word alone, and treats
/// `<span>ab</span><span>cd</span>` as ONE word (`Abcd`, not `AbCd`). A per-text-node pass is wrong
/// on every one of those — which is every case that distinguishes it from a naive one — so it is
/// absent rather than approximated. See `TextTransform`'s own docs.
#[tokio::test]
async fn capitalize_and_the_width_transforms_are_asserted_absent_not_approximated() {
    // chrome 150: "Texera Maka Xtable"
    chrome_says("<div id=t style=\"text-transform:capitalize\">texera maka xtable</div>",
                "texera maka xtable").await;
    // chrome 150: "Hello World 3rd O'neil X-Ray"
    chrome_says("<div id=t style=\"text-transform:capitalize\">hello world 3rd o'neil x-ray</div>",
                "hello world 3rd o'neil x-ray").await;
    // chrome 150: "Abcd" -- one word across two text nodes
    chrome_says("<div id=t style=\"text-transform:capitalize\"><span>ab</span><span>cd</span></div>",
                "abcd").await;
    // chrome 150: "fw123" -- Chrome answered the input unchanged, so this row agrees by ACCIDENT
    // and not by implementation. Recorded, never claimed.
    chrome_says("<div id=t style=\"text-transform:full-width\">fw123</div>", "fw123").await;
}

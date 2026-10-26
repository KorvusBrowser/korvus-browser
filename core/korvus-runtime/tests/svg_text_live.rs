//! SVG text, its fonts and the SVG styles around it, answered as stock Chrome 150 answers them.
//! Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine with the probe
//! beside it in `fixtures/svg-text/` and is replayed here in the persona's engine.
//!
//! a bot-challenge frame sets a family on an SVG `<text>` (`font: italic 150px <family>`, `transform:
//! scale(1.000998)`) and reads `getComputedTextLength()` for each: every call answered 0 here, so its font list came
//! out empty where Chrome's named `DejaVu Sans|Liberation Mono|Liberation Sans|Noto Color Emoji|...`.

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, probe).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// The challenge frame's own SVG text probe, replayed with the strings stock Chrome 150 measured in a live run: an
/// about:blank frame in a closed shadow root, a `<g>` of 19 texts under `scale(1.000998)` imported from a template
/// (four emoji whose `fontSize = "150px !important"` is no value, fifteen hex strings in italic 150px serif), the
/// group's `getBBox()` (each text's box mapped by its transform), every length, the first character's extent, and a
/// length after the frame is removed (0). Chrome's answers are the live trace's to the last bit.
#[tokio::test]
async fn the_challenges_svg_text_probe_answers_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/challenge-probe-replay-probe.js"), include_str!("fixtures/svg-text/challenge-probe-replay-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// The methods called with no argument: on the prototype "Illegal invocation" before any arity check, on a text
/// Blink's arity messages, and the no-argument members' answers for a detached text.
#[tokio::test]
async fn svg_text_methods_check_their_receiver_and_arity_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/no-argument-calls-probe.js"), include_str!("fixtures/svg-text/no-argument-calls-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `SVGTextContentElement`'s character metrics and a text's `getBBox()`, as Blink's `SvgTextQuery` answers them:
/// per family at 150px italic and under the challenge's `scale(1.000998)` (the font shaped at the scaled size,
/// every answer divided back in float), a plain run, `text-anchor`, a `tspan` with `dx` (a kerning pair's caret),
/// collapsed white space, `rotate`, hit testing, a text that is not rendered, and Blink's IndexSizeError messages.
#[tokio::test]
async fn svg_text_metrics_answer_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/metrics-probe.js"), include_str!("fixtures/svg-text/metrics-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// An SVG element's presentation attributes are declarations below every author rule (`font-size="20"` is 20px, a
/// unitless length is px, an invalid value sets nothing, an HTML element's `fill` is no hint, `xml:space`), a
/// `<text>` computes `display: block`, and the text's metrics follow its attributes' font and letter-spacing.
#[tokio::test]
async fn svg_presentation_attributes_cascade_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/presentation-probe.js"), include_str!("fixtures/svg-text/presentation-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// Italic text in a face the list names that has no italic is the upright face skewed by -1/4 -- fontconfig's own
/// 0.2 slant never reaches it -- in canvas `measureText`'s ink box and an SVG text's `getBBox()` alike; a run's
/// width is its 16.16 advances summed exactly, then a float.
#[tokio::test]
async fn italic_text_ink_is_slanted_as_chrome_150_slants_it() {
    let d = differences(include_str!("fixtures/svg-text/italic-ink-probe.js"), include_str!("fixtures/svg-text/italic-ink-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `fill` and `stroke` take a `<paint>` -- `none`, a `url()` with its fallback, `context-fill` -- and the stroke
/// properties their own grammars, in `CSS.supports` and in the `style` setters; a paint computes its colour, and a
/// fragment-only `url()` keeps its spelling.
#[tokio::test]
async fn svg_paint_values_parse_and_compute_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/paint-supports-probe.js"), include_str!("fixtures/svg-text/paint-supports-chrome-152.json")).await;
    assert!(d.is_empty(), "CSS.supports: {} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
    let d = differences(include_str!("fixtures/svg-text/paint-computed-probe.js"), include_str!("fixtures/svg-text/paint-computed-chrome-152.json")).await;
    assert!(d.is_empty(), "computed paint: {} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `SVGTransform` and an element's `transform` list as Blink keeps them: each setter's type, angle and matrix (the
/// arguments `float`, `setMatrix`'s an SVGMatrix), the list methods writing the attribute back in Blink's
/// serialization (`scale(2)` -> `scale(2 2)`, six significant digits, `1.00000e-7`), a detached transform that
/// stays the list's item once inserted, copies of an item another list holds, `consolidate` (which writes nothing
/// back), the attribute grammar (an error leaves the list empty), read-only `animVal` items, `list[i] = t`, and the
/// question WPT `svgtransformlist-replaceitem.html` asks: a text's box after `replaceItem(scale(10 1))` is the box
/// of one written so. These were stubs and text reads of the attribute here (`replaceItem` changed nothing).
#[tokio::test]
async fn svg_transforms_and_their_lists_answer_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/svg-text/transform-list-probe.js"), include_str!("fixtures/svg-text/transform-list-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// An inline declaration reads back as CSSOM serializes it (`scale(1.001)`, `%.6g` numbers: `1.23457e+06`), while
/// the cascade keeps the value the page wrote: the metrics test's scaled rows lay `scale(1.000998)` out at 1.000998.
#[tokio::test]
async fn an_inline_style_reads_back_as_cssom_serializes_it() {
    let d = differences(include_str!("fixtures/svg-text/inline-precision-probe.js"), include_str!("fixtures/svg-text/inline-precision-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

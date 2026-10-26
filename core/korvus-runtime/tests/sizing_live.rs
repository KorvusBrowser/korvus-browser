//! Box sizes, font sizes and text widths, answered as stock Chrome 150 answers them. Each list
//! was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine (its desktop session, the persona's)
//! with the probe beside it in `fixtures/sizing/` or `fixtures/font-sizes/`, and is replayed here in the persona's
//! engine. The page's origin reads `ORIGIN` in both.
//!
//! a bot challenge's font probe puts six `display:inline-block; width:fit-content; white-space:pre` spans of
//! `abababababababababababab` in 16px monospace inside a `width:0` box at `left:-10000px` and reads each one's
//! `getBoundingClientRect().width`: Chrome 192.015625 every time, this engine 0 every time -- an inline-block was
//! clamped to its line, the min-content floor of shrink-to-fit dropped, and the text's 16.16 advances summed as a
//! linear em width (192, not the 192.000366 Chrome snaps up to a layout unit).

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, probe).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// The challenge's own span, and the same span under every width keyword, in boxes 0 and 20px wide, fixed,
/// absolute, floated, flexed, hidden, under every `display` and over every installed family: an atomic inline or a
/// `fit-content` box is as wide as its content however narrow its container (it overflows), and a run of text is its
/// exact advance snapped up to a layout unit (`ShapeResult::SnappedWidth`).
#[tokio::test]
async fn the_challenges_font_probe_spans_measure_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/sizing/fit-content-probe.js"), include_str!("fixtures/sizing/fit-content-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `width`'s sizing keywords -- `min-content`, `max-content`, `fit-content` and their `-webkit-` spellings,
/// `stretch`/`-webkit-fill-available` -- parsed as Blink parses them (`fit-content(50px)` is not a width) and laid out
/// on block, inline-block, table and flex boxes in 0, 100 and 1000px containers, floats and absolutely positioned
/// boxes: shrink-to-fit is `min(max-content, max(min-content, available))` and a wrapped run takes the space on offer.
/// NOT here: `min-width: max-content` and `max-width: min-content` -- the layout's min/max sizes cannot carry a
/// keyword (taffy 0.14's `LengthPercentageAuto`), so they still read as `auto`/`none`.
#[tokio::test]
async fn width_keywords_size_boxes_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/sizing/keywords-probe.js"), include_str!("fixtures/sizing/keywords-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// Runs of 1 to 480 glyphs in `monospace`, `sans-serif` and named families, with and without `font-kerning: none`,
/// against canvas `measureText` of the same runs: Chrome's 16.16 advances (a 1000-unit face's 500 units at 16px is
/// 8.0000153), kerning pairs only when `font-kerning` allows them, and `font-family: monospace` with no size of its
/// own at 13px. NOT here: a TTC face's own family name (`"Noto Sans Mono CJK KR"`) -- the persona's font map names
/// none of them and they measure as the default face.
#[tokio::test]
async fn text_runs_measure_as_chrome_150_measures_them() {
    let d = differences(include_str!("fixtures/sizing/text-widths-probe.js"), include_str!("fixtures/sizing/text-widths-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `font-size` as Blink's `FontBuilder` computes it: the absolute-size keywords from Blink's table (`small` 13px,
/// not 8/9 of 16), a bare `monospace` family's own row (`medium` 13px) and the 13/16 rescale a family change applies
/// to a size that was not written in an absolute unit (`<pre>`, `<code>`, `<kbd>`, `<tt>`, `<samp>` 13px; under a
/// `1.25em` parent 16.25px; under `20px` 20px), `larger`/`smaller` by 1.2, a math function, and `<font size>`.
#[tokio::test]
async fn font_sizes_compute_as_chrome_150_computes_them() {
    let d = differences(include_str!("fixtures/font-sizes/keywords-and-monospace-probe.js"), include_str!("fixtures/font-sizes/keywords-and-monospace-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// The minimum LOGICAL font size of the persona's Korean UI (10px): a size not written in an absolute unit computes
/// no smaller (`0.5em`, `50%`, `xx-small`), one written in px, rem or a math function keeps its own (`5px`, `0.3rem`,
/// `calc(5px)`), and `0px` stays 0.
#[tokio::test]
async fn small_font_sizes_meet_the_korean_minimum_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/font-sizes/minimum-logical-probe.js"), include_str!("fixtures/font-sizes/minimum-logical-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `em` everywhere means the SPECIFIED size -- the one before the minimum (`font-size: 0.5em; width: 10em` is 80px
/// wide while its font computes 10px) -- and a `line-height` number or percentage is of the computed size.
#[tokio::test]
async fn em_lengths_resolve_against_the_specified_size_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/font-sizes/specified-size-probe.js"), include_str!("fixtures/font-sizes/specified-size-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

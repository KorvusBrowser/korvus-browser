//! CSS AS CHROME 150 ANSWERS IT (unit F4): what the bot-detection investigations found a page can
//! read off this engine's CSS that differs from Chrome's, each swept wider than the one row a service read.
//!
//! * MEDIA QUERIES (BrowserScan item 5, a fingerprinting library s41): `(inverted-colors: none)` and `(video-dynamic-range:
//!   standard)` matched here and are unknown in Chrome 150 (their flags are experimental); `(device-posture:
//!   continuous)`, `(horizontal-viewport-segments: 1)`, `(-webkit-transform-3d)` and `(grid: 0)` match there and did
//!   not here; `.media` was a normalization of the text. Every feature of `media_feature_names.json5` with the values
//!   its grammar takes, and the syntax around them (`page::style::media`).
//! * MATH FUNCTIONS (a fingerprinting library s203): `calc(1px * sin(0.5))` reads back `calc(0.479426px)` in Chrome -- every
//!   function Blink simplifies at parse time -- and here it was echoed (`page::style::calc`).
//! * THE INLINE DECLARATION BLOCK (a bot-detection script D6): `setAttribute('style', '... border: 0;')` reads back `border: 0px`
//!   from `cssText`, seventeen longhands from `length` (`page::style::inline`).
//! * CSS TRANSITIONS (a bot-detection service D7): a style flush through `offsetHeight` or `getBoundingClientRect` takes the
//!   before-change style a transition starts from; only `getComputedStyle` did here.
//! * WEB ANIMATIONS' DICTIONARIES (BrowserScan item 7): `getTiming()` and `getComputedTiming()` members in Blink's
//!   bindings' order, `getKeyframes()` with `computedOffset` last.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, NO
//! DevTools attached, three runs alike): each `fixtures/css-answers/<name>-probe.js` in `canvas-and-errors/
//! probe-page.html` answered `<name>-chrome-150.json`. Left out, each with its reason in the probe: `(fallback:
//! none)` (it crashes Chrome 150's renderer), a keyframe's property order (a Blink HashMap's), and the answers that
//! depend on the window's exact size or on font metrics.

use crate::chrome_canvas_and_errors_live::{answers, assert_as_chrome, Mode};

#[tokio::test]
async fn media_queries_match_and_serialize_as_chrome_150s() {
    let got = answers(Mode::Page, include_str!("fixtures/css-answers/media-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/css-answers/media-chrome-150.json"));
}

#[tokio::test]
async fn math_functions_simplify_as_chrome_150s() {
    let got = answers(Mode::Page, include_str!("fixtures/css-answers/calc-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/css-answers/calc-chrome-152.json"));
}

#[tokio::test]
async fn an_inline_declaration_block_serializes_as_chrome_150s() {
    let got = answers(Mode::Page, include_str!("fixtures/css-answers/inline-style-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/css-answers/inline-style-chrome-152.json"));
}

#[tokio::test]
async fn a_layout_read_starts_a_transition_as_chrome_150s_does() {
    let got = answers(Mode::Page, include_str!("fixtures/css-answers/transition-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/css-answers/transition-chrome-152.json"));
}

#[tokio::test]
async fn web_animations_dictionaries_in_chrome_150s_member_order() {
    let got = answers(Mode::Page, include_str!("fixtures/css-answers/effect-dicts-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/css-answers/effect-dicts-chrome-152.json"));
}

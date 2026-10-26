//! `getComputedStyle` reads as stock Chrome 150 reads. a bot challenge writes an
//! about:blank frame's document and reads its body's computed style by every name Chrome's `CSSStyleDeclaration`
//! carries -- 736 -- and 112 of them differed: 111 SHORTHANDS (`background`, `font`, `flex`, `border-block`,
//! `transition`, `grid`, `outline`, `columns` ...) read "" where Blink serializes them from their longhands
//! (`ComputedStyleUtils`, `shorthands_custom.cc`, now `core/css/computed_shorthand.rs`), `background-position-x`/`-y`
//! and `page` read "" though Chrome computes them (it only leaves them out of the enumeration), and `position-anchor`
//! read `normal`, which is Chromium 151's initial value -- Chromium 150's is `none`.
//!
//! Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine (its desktop session, the
//! persona's), three runs alike, with the probe beside it in `fixtures/computed-style/`, and is replayed here in the
//! persona's engine. The page's origin reads `ORIGIN` in both.

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, probe).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// The challenge's own read: a frame whose document the page wrote, its body's computed style by every camel-cased
/// name, sorted.
#[tokio::test]
async fn a_written_frame_bodys_computed_style_reads_as_chrome_150s() {
    let d = differences(include_str!("fixtures/computed-style/frame-body-probe.js"), include_str!("fixtures/computed-style/frame-body-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// Every shorthand Blink serializes, over its initial values and over declared ones, by `getPropertyValue` --
/// aliases (`-webkit-border-before`, `-webkit-column-break-after`) included, and the 33 declarations this cascade
/// used to DROP whole (`overscroll-behavior`, `corner-shape`, `font-synthesis`, `interest-delay`, `marker`,
/// `text-emphasis`, `-webkit-text-stroke`, the timelines, `position-try`, `text-box`, `offset`, `animation-range`,
/// `border-image`, `mask`, `grid`, `grid-template`, `page-break-after: always`, `font: caption`, a gradient).
#[tokio::test]
async fn computed_shorthands_serialize_as_chrome_150_writes_them() {
    let d = differences(include_str!("fixtures/computed-style/shorthands-probe.js"), include_str!("fixtures/computed-style/shorthands-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// The fill layers, as Blink keeps them: `background-position` and `mask-position` are shorthands over an x and a
/// y longhand, which `background` writes too -- a layer's position in one to four components (`right 5px bottom`
/// computes `calc(100% - 5px)` and `100%`), every layer (`background: url(a.png) no-repeat, url(b.png) red` was
/// dropped whole), each list as long as the image list (repeated when shorter: `no-repeat, repeat-x` over three
/// images), sizes and repeats in Blink's spelling (`16px` for `1em auto`, `repeat-x` for `repeat no-repeat`);
/// `border-spacing` and its two `-webkit-` longhands (a `<table>`'s `2px`), `page`, `position-anchor`; and a
/// sheet's rules read back -- a layer's omitted component an implicit `initial` (`initial, initial`), the
/// longhands in Blink's order (`rule.style[0]` is `background-image`).
#[tokio::test]
async fn fill_layers_answer_as_chrome_150s() {
    let d = differences(include_str!("fixtures/computed-style/fill-layers-probe.js"), include_str!("fixtures/computed-style/fill-layers-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// The declarations this cascade used to drop, and the values Blink computes to another spelling, read back shorthand
/// and longhands alike (842 answers): each shorthand's grammar as its Blink parser takes it (`text-box: cap` is
/// invalid, `border-image: 30% / / 4px` is not), the system fonts (`16px Arial`, the `-webkit-*-control` ones
/// `13.3333px`), `font-stretch` keywords as percentages, a gradient's colours, lengths and default direction,
/// `offset-rotate`'s `auto 180deg`, `text-emphasis-style`'s default shape by writing mode (`dot`, vertically
/// `sesame`), `superellipse(2)` as `squircle`, `row dense` as `dense`, a `<position-area>`'s canonical order and a
/// `path()`'s data as `SVGPathStringBuilder` writes it.
#[tokio::test]
async fn declared_shorthands_expand_and_compute_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/computed-style/declared-shorthands-probe.js"), include_str!("fixtures/computed-style/declared-shorthands-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

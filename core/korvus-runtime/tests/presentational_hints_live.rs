//! LIVE gate: the embedded and replaced elements' presentational hints, and the boxes they size, as Chrome 150 maps
//! and lays them out (ledger N18).
//!
//! `width`/`height` on `<img>`, `<iframe>`, `<object>`, `<embed>`, `<video>` and an image button are CSS `width` /
//! `height` at the presentational-hint level -- a percentage included -- with `aspect-ratio: auto w / h` on an image,
//! a video or an image button that has both (a `<canvas>` only that, from its bitmap's integers); `hspace`/`vspace`
//! are margins, `border` a solid border, `align` a float or a `vertical-align` (a `text-align` on a video or a canvas),
//! `valign` an image's `vertical-align`, `frameborder` of 0 takes an iframe's border away, and `hidden` is an
//! `<embed>`'s zero size; the table family's lengths take each element's own flags. This engine mapped none of them
//! and read the dimensions as the box's natural size, refusing a percentage: a device-check script's device-check frame
//! `<iframe width="100%" height="100%" frameborder="0">` was 304 wide where Chrome lays it out at the page's width, so
//! its `innerWidth` read 300 and the check flagged "devtools open"; `<img width="50%">` computed 0px.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs byte-identical): the script `fixtures/presentational-hints/probe.js`, served from
//! 127.0.0.1, answered `fixtures/presentational-hints/chrome-152.json`. Each row is `[case, answer]`; no answer
//! depends on the window's size or on a font.

#[tokio::test]
async fn the_embedded_elements_presentational_hints_are_chrome_150s() {
    let script = include_str!("fixtures/presentational-hints/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/presentational-hints/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "one row per case");
    let wrong: Vec<String> = got
        .iter()
        .zip(&want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  korvus {g}\n  chrome {w}"))
        .collect();
    assert!(wrong.is_empty(), "{} of {} cases differ from Chrome 150:\n{}", wrong.len(), want.len(), wrong.join("\n"));
}

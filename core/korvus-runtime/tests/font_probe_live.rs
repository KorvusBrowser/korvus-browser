//! LIVE gate: a font-detection probe answers as stock Chrome 150 does.
//!
//! Fingerprinting scripts measure installed fonts with ONE off-screen `position: absolute` span: they rewrite its
//! `font-family` and read `offsetWidth` / `offsetHeight` after each -- www.wayfair.com's search page did so ~20 times
//! a second for 30-40 s, each read a whole layout of ~7,700 elements here. A layout now carries every subtree built as
//! before over from the last one (`BoxTree::carry_from`) and lays out the probe and the boxes above it; the probe's
//! answers are a fingerprint, so they are pinned here: generic families, installed ones, a missing one alone and with a
//! fallback, beside 200 rows the carry keeps.
//!
//! `fixtures/font-probe/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-13, headless
//! at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike.

#[tokio::test]
async fn a_font_probe_answers_as_chrome_150() {
    let script = include_str!("fixtures/font-probe/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/font-probe/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

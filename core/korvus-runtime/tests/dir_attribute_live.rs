//! LIVE gate: the `dir` attribute is a presentational hint for `direction`, as Blink maps it.
//!
//! `HTMLElement::CollectStyleForPresentationAttribute`: `dir="ltr"` / `"rtl"` (any case) set
//! `direction` on every HTML element; any other value but `auto` sets `ltr` on `<body>` alone; an
//! author declaration beats the hint. This engine had only the UA sheet's `unicode-bidi: isolate`, so
//! `<div dir=rtl>` computed `direction: ltr` and a 2D context on a canvas inside it answered
//! `direction` "ltr" (found while checking `CanvasRenderingContext2D.direction`).
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/dir-attribute/probe.js`, served from 127.0.0.1, answered
//! `fixtures/dir-attribute/chrome-152.json`.

#[tokio::test]
async fn dir_maps_to_direction_as_chrome_150_maps_it() {
    let script = include_str!("fixtures/dir-attribute/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/dir-attribute/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want);
}

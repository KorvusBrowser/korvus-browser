//! LIVE gate: CSS animations and their events in and around shadow trees, as Chrome runs them.
//!
//! A light child of a shadow host that no slot takes is outside the flat tree: Chrome gives it no box and runs no
//! animation for it or below it (`getAnimations()` 0, no event), while the same child assigned to a slot animates.
//! korvus ran the unslotted one -- and, once the events existed, sent its `animationstart` / `animationend`. An
//! element of a closed shadow tree transitions, and its events reach its own listeners but not the document's: a
//! `TransitionEvent` is not composed.
//!
//! `fixtures/shadow-animations/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12,
//! headless at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike.

#[tokio::test]
async fn animations_follow_the_flat_tree_and_their_events_stay_in_their_tree() {
    let script = include_str!("fixtures/shadow-animations/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/shadow-animations/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

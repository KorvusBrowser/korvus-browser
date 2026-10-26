//! LIVE gate: a transition follows the FLAT tree, as Chrome's does.
//!
//! A slotted element inherits from its SLOT, so when the slot's `color` changes, an element that transitions `color`
//! starts a transition (Chrome 150: `CSSTransition:color`, `transitionrun` and `transitionend` at the document). The
//! animation rescan cascaded a slotted element from its light-tree parent, so nothing moved and nothing ran. An
//! element of a closed shadow tree transitions from its tree's own sheet; a light child no slot takes runs no
//! animation.
//!
//! `fixtures/slotted-transition/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12,
//! headless at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike. The slotted
//! element's settled `color` row (`getComputedStyle` inheriting from the slot, rgb(0, 0, 255) in all three runs) is in
//! since that climb was fixed. The unslotted element's computed `opacity` is left out: Chrome
//! reads its animation's first keyframe (0.25) from the style it resolves on demand for an element it does not
//! render, while `getAnimations()` holds nothing -- korvus reads the cascade's value, having no animation to apply.

#[tokio::test]
async fn a_slotted_element_transitions_from_its_slot() {
    let script = include_str!("fixtures/slotted-transition/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/slotted-transition/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

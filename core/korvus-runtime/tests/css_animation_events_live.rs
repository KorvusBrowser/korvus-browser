//! LIVE gate: CSS transitions and animations send their events, as Chrome sends them.
//!
//! No `transitionrun` / `transitionstart` / `transitionend` / `transitioncancel` or `animationstart` /
//! `animationiteration` / `animationend` / `animationcancel` was ever dispatched: the transitions and animations
//! ran, `getAnimations()` listed them, and a page waiting for `transitionend` (a modal, a carousel, a framework's
//! leave transition) waited for ever. They are sent by the rendering frame's "update animations and send events"
//! step, before its rAF callbacks (`animation::css::send_events`).
//!
//! The probe logs every event at the document as `type:target:name:elapsedTime:bubbles:cancelable:pseudoElement:
//! isTrusted:constructor` (`elapsedTime` `-` for a cancel, whose value is the moment it happened) for: one
//! transition; three starting in one frame (their order); a transition with a delay; a transition whose target moves
//! mid-run (a cancel, then a new one); `display: none` mid-transition; an animation of two iterations;
//! `display: none` mid-animation; an element shown from `display: none`, itself and through its parent, with a new
//! `opacity` (no transition); and a `transitionrun` against an rAF callback set up in the same task.
//!
//! `fixtures/css-animation-events/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12,
//! headless at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike.

#[tokio::test]
async fn css_transitions_and_animations_send_their_events() {
    let script = include_str!("fixtures/css-animation-events/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/css-animation-events/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

//! LIVE gate: a transition or an animation that follows a container -- declared inside an `@container` rule, or on
//! a `cq*` length -- runs, as in Chrome.
//!
//! The animation rescan (`Page::declarative_update`) matched with no container answers (`MatchEnv::default()`), so no
//! `@container` rule held there, and resolved `cq*` against the viewport. A rule inside a container query that
//! declared a transition or an animation never ran, and a `cq*` length that moved with its container never
//! transitioned. Because its styles were not the page style pass's, every rescan of such a page also started from
//! nothing: www.airbnb.com (45 `@container` rules, 11 with `cq*`) 52 times in 52, www.glassdoor.com 242 in 244.
//!
//! The first rows pin that an element's FIRST style has the containers laid out: an element inserted into an already
//! wide container reads the rule's value with no transition, so the rescan settles the containers before it styles.
//!
//! `fixtures/container-animations/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12,
//! headless at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike.

#[tokio::test]
async fn a_transition_or_an_animation_that_follows_a_container_runs() {
    let script = include_str!("fixtures/container-animations/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/container-animations/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

//! LIVE gate: the 2026-10-08 batch -- the last 490 of the Chrome oracle ratchet's answers and
//! the engine defects found under them -- answers as stock Chrome 150 does.
//!
//! * `typed-om-*`: the CSS Typed OM (`core/css/typed_om.rs`): the `CSS.px()` family and `CSS`'s own
//!   property order, `CSSUnitValue` / `CSSKeywordValue` / the `CSSMath*` tree (`%.6g` serialization,
//!   arithmetic folding, types with the percent hint, `to` / `toSum`, `parse` with Blink's calc
//!   simplification), transform components and `toMatrix`, `CSSUnparsedValue`'s `/**/` serialization,
//!   `CSS.registerProperty` (into the cascade) and `CSS.paintWorklet`, `StylePropertyMap` writes and
//!   reified reads, `CSSStyleRule.styleMap`, `CSSPageRule.selectorText`.
//! * `dommatrix`: `DOMMatrix` rotations as Blink computes them (`SinCosDegrees`: 90deg is exact) and its
//!   stringifier's ECMAScript numbers (`1e-17`); a rule's `style.length`; `CSS.supports` refusing an
//!   identifier a length property does not take (`width: red`).
//! * `media-shadow-wgsl`: `wgslLanguageFeatures` (Chrome 150's shipped WGSL features), a ShadowRoot's
//!   `elementsFromPoint` / `getSelection` through its document, `TextTrack.cues` / `activeCues` (null only
//!   while the track is disabled, Chrome 152), `VTTCue.getCueAsHTML()`'s WebVTT DOM, `MediaSource.addSourceBuffer`'s order.
//! * `frames`: `window.status` per Window. `fragment`: a frame's fragment navigation (no reload, a
//!   queued `hashchange`). `indexeddb`: `open` completes a task later (`databases()` in the same turn
//!   does not list it).
//!
//! THE ROWS LEFT OUT, each a residual stated in its module: `CSSPerspective.toMatrix()`'s 2D flag
//! (`typed_om.rs`), a transform's `calc()` term order, the indexed SETTERS of `CSSTransformValue` /
//! `CSSUnparsedValue` (no type check, no gap refusal), and the `open` / `setTimeout(0)` order, which is
//! Chrome's IPC latency rather than an engine rule.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: each probe in
//! `fixtures/an interactive-widget challenge_batch/`, served from 127.0.0.1, answered its `*-chrome-150.json` row for row.

use crate::keyboard_live::{answers, CHROME_BUNDLE};

async fn check(script: &str, recorded: &str) {
    let want: Vec<serde_json::Value> = serde_json::from_str(recorded).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

#[tokio::test]
async fn typed_om_answers_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_batch/typed-om-probe.js"), include_str!("fixtures/challenge_batch/typed-om-chrome-152.json")).await;
}

#[tokio::test]
async fn typed_om_math_transforms_and_registration_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_batch/typed-om-2-probe.js"), include_str!("fixtures/challenge_batch/typed-om-2-chrome-152.json")).await;
}

#[tokio::test]
async fn dommatrix_rule_styles_and_supports_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_batch/dommatrix-probe.js"), include_str!("fixtures/challenge_batch/dommatrix-chrome-152.json")).await;
}

#[tokio::test]
async fn media_shadow_roots_and_wgsl_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_batch/media-shadow-wgsl-probe.js"), include_str!("fixtures/challenge_batch/media-shadow-wgsl-chrome-152.json")).await;
}

#[tokio::test]
async fn window_status_is_per_window_as_in_chrome_150() {
    check(include_str!("fixtures/challenge_batch/frames-probe.js"), include_str!("fixtures/challenge_batch/frames-chrome-152.json")).await;
}

#[tokio::test]
async fn a_frames_fragment_navigation_answers_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_batch/fragment-probe.js"), include_str!("fixtures/challenge_batch/fragment-chrome-152.json")).await;
}

#[tokio::test]
async fn indexeddb_open_completes_a_task_later_as_in_chrome_150() {
    check(include_str!("fixtures/challenge_batch/indexeddb-probe.js"), include_str!("fixtures/challenge_batch/indexeddb-chrome-152.json")).await;
}

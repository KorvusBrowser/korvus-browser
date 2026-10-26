//! LIVE gate: WebGL 2's object state answers as Chrome 150's does (the Chrome
//! oracle ratchet).
//!
//! Samplers, queries, syncs, the indexed buffer bindings, buffer readback/copy and immutable texture
//! storage were no-ops answering `null` -- `getSamplerParameter(createSampler(), TEXTURE_MIN_FILTER)`
//! was `null` where Chrome answers `NEAREST_MIPMAP_LINEAR`, `getBufferSubData` read zeros, and no bad
//! enum ever raised an error. `core/html/webgl/gl2.rs` now keeps that state the way
//! `WebGL2RenderingContextBase` (Chromium 150) and GL behind it do: parameter validation and defaults,
//! query targets and activity (a result is available only after a task), fence parameters, indexed
//! binding ranges against the persona's limits and `UNIFORM_BUFFER_OFFSET_ALIGNMENT`, buffer contents
//! for every target, and `texStorage*`'s immutability and level checks.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/webgl2/state-probe.js` answered `fixtures/webgl2/state-chrome-152.json` row for row (each
//! row: the value, then two `getError()` reads). None of these rows depends on the GPU: they are
//! Blink's and ANGLE's validation, and the limits read are the persona's.

use crate::keyboard_live::{answers, CHROME_BUNDLE};

#[tokio::test]
async fn webgl2_object_state_answers_as_chrome_150_does() {
    let script = include_str!("fixtures/webgl2/state-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/webgl2/state-chrome-152.json")).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

//! LIVE gate: every WebGL extension object the Chrome persona lists has Chrome 150's prototype -- its
//! own names, their kinds, lengths and flags, and its `constructor` (none: the extension interfaces are
//! `[LegacyNoInterfaceObject]`, so it reads `Object`).
//!
//! It caught `WEBGL_provoking_vertex`'s method spelled `provokingVertex`; Blink's IDL and Chrome say
//! `provokingVertexWEBGL` (the challenge frame enumerates extension prototypes).
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/webgl-extensions/probe.js`, served from 127.0.0.1, answered
//! `fixtures/webgl-extensions/chrome-152.json`.
//!
//! **Re-recorded 2026-10-12**, three times alike, headed on the Wayland desktop with no DevTools:
//! the list is the GPU's, and Chrome here now draws on the RTX 4060 through ANGLE's GLES back end rather than the
//! AMD iGPU of the first recording -- `WEBGL_draw_buffers`, `OVR_multiview2` and `WEBGL_provoking_vertex` left it,
//! the ASTC / ETC / ETC1 compressed formats joined it. korvus answers the same list because it asks the same GPU.

#[tokio::test]
async fn webgl_extension_prototypes_are_chrome_150s() {
    let script = include_str!("fixtures/webgl-extensions/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/webgl-extensions/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let differ: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty() && got.len() == want.len(), "{} of {} differ ({} vs {} rows):\n{}", differ.len(), want.len(), got.len(), want.len(), differ.join("\n"));
}

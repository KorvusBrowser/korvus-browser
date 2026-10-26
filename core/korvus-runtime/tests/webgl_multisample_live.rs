//! LIVE gate: an `antialias: true` WebGL drawing buffer rasterized and resolved as the persona's GPU does it
//! (a bot-detection script's press-and-hold challenge).
//!
//! a bot-detection script's `captcha.js` draws a proof of work on every frame -- one triangle, a fragment shader that colours by
//! `mod(gl_FragCoord.x, 2.0)` -- and reads it back. Two things made korvus unlike Chrome there:
//!
//! * THE PIXELS. korvus SUPERSAMPLED: the shader ran at each of four samples with a `gl_FragCoord` of the sample's own,
//!   so the proof of work read back 17 colours where Chrome's buffer holds 5. A GPU MULTISAMPLES: coverage is decided
//!   at the four standard sample positions, the shader runs once per pixel at its centre, and the resolve averages the
//!   8-bit samples as floats; a float is stored with an exact tie going down (0.5 -> 127).
//! * THE COST. Every `readPixels` replayed every op since the context was made: on a block page the 112 readbacks took
//!   42 s of main thread (the longest 0.9 s), and the page's input waited behind them -- 0.7-2 s for each CDP mouse
//!   move where Chrome answers in under 20 ms.
//!
//! `fixtures/webgl-msaa/probe.js` draws the proof of work at three sizes, seven flat-coloured triangles chosen to pin
//! the sample positions, and the shader's own `gl_FragCoord`, with and without antialiasing; `chrome-150.json` is stock
//! Chrome 150.0.7871.46 on the persona's GPU (ANGLE, NVIDIA GeForce RTX 4060; headed on the Wayland desktop, no
//! DevTools, three runs alike).

#[tokio::test]
async fn a_multisampled_drawing_buffer_reads_back_as_the_personas_gpu_resolves_it() {
    let script = include_str!("fixtures/webgl-msaa/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/webgl-msaa/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}", got.len(), want.len());
    assert!(wrong.is_empty(), "{} buffer(s) differ from Chrome 150's:\n{}", wrong.len(), wrong.join("\n"));
}

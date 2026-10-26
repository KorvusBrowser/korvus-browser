//! Owner 2026-10-12: WebGL on this PC's own GPU, through Chrome's ANGLE, as Chrome draws it: 
//! a bot-detection script's `DM` float fingerprint (main.min.js) answers as desktop Chrome does on the same GPU.
//!
//! The probe (`fixtures/webgl-gpu/probe.js`) draws PX's denormal-halving shader at 256x160 in a `webgl2` and a
//! `webgl` context and reads one column back: the values are the GPU's arithmetic -- the RTX 4060 flushes denormals,
//! and its compiler keeps `x` in G but not in R -- which korvus's software interpreter never matched. It also prints
//! the device, because the answer belongs to that GPU: on another one this test has nothing to compare and says so.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-12**, headed on the Wayland desktop
//! with no DevTools, three times alike (and alike the C12b recording of 05:17): `fixtures/webgl-gpu/chrome-152.json`.

#[tokio::test]
async fn a_gpu_float_fingerprint_reads_as_chrome_on_the_same_gpu() {
    let script = include_str!("fixtures/webgl-gpu/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/webgl-gpu/chrome-152.json")).expect("the recorded Chrome rows");
    let recorded_on = want.last().cloned().unwrap_or_default();
    let here = render::gpu::available().map(|d| serde_json::json!(["device", d.vendor, d.renderer]));
    if here.as_ref() != Some(&recorded_on) {
        eprintln!("NOT COMPARED: recorded on {recorded_on}, this PC's GPU is {here:?}");
        return;
    }
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let differ: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty() && got.len() == want.len(), "{} of {} differ ({} vs {} rows):\n{}", differ.len(), want.len(), got.len(), want.len(), differ.join("\n"));
}

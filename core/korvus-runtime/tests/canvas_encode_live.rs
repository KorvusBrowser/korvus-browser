//! LIVE gate: `toDataURL` / `toBlob` / `convertToBlob` encode as Blink does (PHASE 3 F7, a bot-detection service D4).
//!
//! Three differences, each a byte string a page hashes:
//!
//!   * `toDataURL('image/webp')` answered a PNG -- the backend had no WebP encoder. Blink encodes with Skia's
//!     `SkWebpEncoder` over libwebp (`ImageEncoder::ComputeWebpOptions`: lossy at quality 80, `quality * 100` for a
//!     number in [0,1], lossless at 75 for exactly 1), the type ASCII-lower-cased and compared whole
//!     (`"IMAGE/WEBP"` is WebP, `" image/webp"` is PNG).
//!   * the `quality` argument is read only when it is a Number (`"0.5"`, `true` are the default), where korvus
//!     applied ToNumber; `convertToBlob`'s `quality` defaults to 1, so its WebP is lossless.
//!   * a 2D canvas Blink has made a resource provider for exports an sRGB-TAGGED snapshot, so its JPEG carries an
//!     ICC_PROFILE APP2 and its WebP an ICCP chunk (472 / 456 bytes); a canvas never drawn on exports Blink's
//!     untagged transparent image. `fixtures/canvas-encode/icc-probe.js` touches an 8x8 canvas 21 ways and reads
//!     which exports carry the profile.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs alike): `fixtures/canvas-encode/probe.js` answered `chrome-150.json` -- per
//! canvas and argument list `[canvas, args, data-URL head, length, SHA-256 prefix, the URL when short]`, then the
//! `toBlob` and `convertToBlob` blobs' type, size and hash -- and `icc-probe.js` answered `icc-chrome-150.json`
//! (`[case, WebP has ICCP, JPEG has ICC_PROFILE, PNG has iCCP/sRGB]`; the recording's three lengths are left out of
//! the fixture, since `fillText`'s depend on the font).

fn compare(got: &[serde_json::Value], want: &[serde_json::Value]) -> Vec<String> {
    let mut wrong: Vec<String> = got
        .iter()
        .zip(want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  korvus {g}\n  chrome {w}"))
        .collect();
    if got.len() != want.len() {
        wrong.push(format!("  korvus answered {} rows, chrome {}", got.len(), want.len()));
    }
    wrong
}

#[tokio::test]
async fn canvas_exports_encode_as_blink_does() {
    let script = include_str!("fixtures/canvas-encode/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/canvas-encode/chrome-150.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong = compare(&got, &want);
    assert!(wrong.is_empty(), "{} exports differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

#[tokio::test]
async fn a_drawn_canvas_exports_its_srgb_profile_and_an_untouched_one_does_not() {
    let script = include_str!("fixtures/canvas-encode/icc-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/canvas-encode/icc-chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong = compare(&got, &want);
    assert!(wrong.is_empty(), "{} canvases differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

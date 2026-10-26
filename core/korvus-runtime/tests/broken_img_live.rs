//! LIVE gate: a broken `<img>` is laid out as Blink lays out its FALLBACK content (A/B C8).
//!
//! When an image's request fails -- or its `src` names nothing -- Blink's `HTMLImageElement` stops being a
//! `LayoutImage` and becomes a `LayoutBlockFlow` over a user-agent shadow tree: a 16x16 broken-image icon
//! and the alt text (`HTMLImageFallbackHelper::CreateAltTextShadowTree`). An inline host shrinks to that
//! content with its `width`/`height` reset (`AdjustHostStyle`); one that already has both dimensions and no
//! `alt` stays a replaced box of that size (`TreatImageAsReplaced`); an empty `alt` hides the icon
//! (`ImageRepresentsNothing`). This engine laid every broken image out 0x0 -- an image with no size -- which
//! is what bot.incolumitas.com's `intoli.brokenImageDimensions` reads as a headless browser.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed, no
//! DevTools attached, three runs alike): the script `fixtures/broken-img/probe.js`, served from
//! 127.0.0.1, answered `fixtures/broken-img/chrome-152.json`. Each row is `[case, event, [width, height,
//! complete] inside the handler, [width, height, naturalWidth, naturalHeight, complete, offsetWidth,
//! offsetHeight, rect width, rect height, left and top in its wrapper, the wrapper's height, computed
//! width, computed height, computed display]]`.
//!
//! Left out because this engine has no float algorithm (`BoxLevel::of_floated`): a FLOATED broken image
//! (its wrapper is 0 tall in Chrome, a line here), alt text that wraps below the floated icon in a narrow
//! box, and an absolutely positioned one.

#[tokio::test]
async fn a_broken_image_lays_out_its_fallback_content_as_chrome_150_does() {
    let script = include_str!("fixtures/broken-img/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/broken-img/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "one row per case");
    let wrong: Vec<String> = got
        .iter()
        .zip(&want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  korvus {g}\n  chrome {w}"))
        .collect();
    assert!(wrong.is_empty(), "{} of {} cases differ from Chrome 150:\n{}", wrong.len(), want.len(), wrong.join("\n"));
}

//! LIVE gate: SVG's value types and a handful of reflected and static members answer as Chrome 150's do
//! (the Chrome oracle ratchet).
//!
//! * `points` / `animatedPoints` are SVGPointList views of the `points` attribute (they were undefined):
//!   Blink's parser (a parse error clears the list, an odd trailing number is dropped), fresh SVGPoint
//!   items that write back, the mutators, `animatedPoints` read-only (NoModificationAllowedError).
//! * every SVG list's `removeItem` / `clear` / `appendItem` / ... rewrite the attribute; out of range is
//!   IndexSizeError with Blink's "greater than [or equal to] the maximum bound" message; `list[i]` is an
//!   indexed property; an `animVal` list is read-only.
//! * an SVGAnimatedEnumeration with no (or an unknown) attribute reads its element's INITIAL value
//!   (`feBlend.mode` NORMAL, `gradientUnits` OBJECTBOUNDINGBOX), and its setter refuses 0 and values past
//!   the last constant with Blink's TypeErrors.
//! * `valueAsString` parses (SyntaxError for `"x"` and `""`) and serializes with its unit; `align` /
//!   `meetOrSlice` validate and rewrite `preserveAspectRatio`; `SVGTransform.matrix` is an SVGMatrix;
//!   `getPointAtLength` on an empty path is InvalidStateError; `currentScale` / `currentTranslate` /
//!   `suspendRedraw` are the outermost <svg>'s.
//! * `<style>.disabled` is its sheet's flag, not a reflected attribute (HTML and SVG); SVG `<style>`'s
//!   `type` / `media` default to "text/css" / "all"; SVG `<image>.crossOrigin` is the CORS enumeration
//!   and SVG `<a>.referrerPolicy` a plain string.
//! * `misc-probe.js`: `<img>.crossOrigin` reads null when absent (it read "anonymous"), `<font>.color`
//!   reflects, `Response.json` / `Response.redirect` exist with the constructor's validation.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: each probe in
//! `fixtures/svg_values/`, served from 127.0.0.1, answered its `*chrome-150.json` row for row.

use crate::keyboard_live::{answers, CHROME_BUNDLE};

async fn check(script: &str, recorded: &str) {
    let want: Vec<serde_json::Value> = serde_json::from_str(recorded).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

#[tokio::test]
async fn svg_value_types_answer_as_chrome_150_does() {
    check(include_str!("fixtures/svg_values/probe.js"), include_str!("fixtures/svg_values/chrome-152.json")).await;
}

#[tokio::test]
async fn image_font_and_response_statics_answer_as_chrome_150_does() {
    check(include_str!("fixtures/svg_values/misc-probe.js"), include_str!("fixtures/svg_values/misc-chrome-152.json")).await;
}

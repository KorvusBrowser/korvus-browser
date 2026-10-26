//! LIVE gate: `getBoundingClientRect` and `getClientRects` through CSS transforms, as Chrome maps them.
//!
//! a challenge provider's widget lays out a hidden probe of transformed boxes -- `skewY(23.1753218deg)`, a
//! `scale()` of 1e35, a `matrix()`, `perspective(12890px) translateZ(101.5px)` -- and reads their rects
//! (a live korvus trace, 2026-10-07). korvus answered every one as the untransformed box.
//!
//! Every expectation is Google Chrome 150.0.7871.46's answer on this machine on 2026-10-08, to
//! `fixtures/transforms/transforms.js` served from 127.0.0.1 at the persona's 1920x965 viewport: the
//! box's corners mapped through its own transform and every transformed ancestor's, each about its
//! `transform-origin`, as Blink's float pipeline rounds them -- the functions' arguments stored as
//! floats, a projective w as a float, a scale-and-translate one in AxisTransform2d's float arithmetic,
//! each point clamped to +-FLT_MAX/1e6 -- and a non-replaced inline left untransformed.
//!
//! `widget`: a bot-challenge frame's ClientRects probe itself, verbatim, in an 806px container -- each box
//! mapped as Blink's layout maps it (`core/geometry/gfx_transform.rs`: the transform built and applied in
//! `gfx::Transform`'s own float/double steps, the box's local corners up its containers). Mapped textbook-style, the
//! four transformed boxes read 1 to 10 float32 ULPs off Chrome's.
//!
//! `numbers`: a CSS number as Blink writes one -- six significant digits, `%g`'s exponent form below
//! 1e-4 and from 1e6 (`1e-05`, `1.23457e+06`, the `-7.75795e-05` of a `matrix3d()`); a unitless
//! `line-height` in pixels (`1.6px` for 0.1 at 16px); the opacities as clamped numbers.

const CHROME: &[(&str, &str, &str)] = &[
    ("transforms", include_str!("fixtures/transforms/transforms.js"), include_str!("fixtures/transforms/transforms.chrome-152.json")),
    ("numbers", include_str!("fixtures/transforms/numbers.js"), include_str!("fixtures/transforms/numbers.chrome-152.json")),
    // the challenge's own probe, its every rect to the float32 bit (`record.py` beside the fixtures, 2026-10-11)
    ("widget", include_str!("fixtures/transforms/widget.js"), include_str!("fixtures/transforms/widget.chrome-152.json")),
];

#[tokio::test]
async fn transformed_boxes_and_css_numbers_read_as_chrome_150_reads_them() {
    let mut differ = Vec::new();
    for (name, script, chrome) in CHROME {
        let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
        let got = crate::layout_root_live::answers(script).await;
        assert_eq!(got.len(), want.len(), "{name}: answer count");
        for (g, w) in got.iter().zip(&want) {
            if g != w {
                differ.push(format!("{name}: korvus {g}\n    Chrome {w}"));
            }
        }
    }
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

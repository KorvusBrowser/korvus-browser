//! LIVE gate: form controls on a line as Chrome lays them out, and the positioned insets
//! `getComputedStyle` answers.
//!
//! Every expectation is Google Chrome 150.0.7871.46's answer on this machine on 2026-10-08, to the same
//! script (`fixtures/form-controls/*.js`) served from 127.0.0.1 in a `--window-size=1920,1060` window
//! (the persona's 1920x965 viewport):
//!
//! * `controls`: a themed checkbox or radio button is its native theme part, 13 x 13 (it was 0 x 0),
//!   with its padding and border reset and its baseline at its border box's bottom (Blink's
//!   `AdjustCheckboxStyle`); a text field, a button-type `<input>` and a drop-down sit on their own
//!   line of text, not on their bottom margin edge (which pushed every box on the line 5px down); an
//!   atomic inline's used margins are what `getComputedStyle` reads (a checkbox's `margin` read `0px`).
//! * `insets`: a relatively positioned box answers each side as written -- a percentage or `calc()`
//!   truncated to a `LayoutUnit` -- and only an `auto` side as the negated opposite (it read the
//!   over-constrained shift, `1px -2px -1px 2px` for `inset: 1px 2px`, and 0 for any `calc()`); a sticky
//!   box resolves a side that is not `auto` against its scroll container's client box (`top: 5%`
//!   read `5%`, Chrome `47.5px`).
//! * `line-widths`: a border, outline or column-rule width computes to whole pixels (Blink's
//!   `ClampLineWidth`: 0 < w < 1 is 1, else floored; `thin`/`medium`/`thick` 1/3/5) -- `2.715px` read
//!   `2.715px` and laid out 2.715 wide; `column-rule` is expanded; and a padding or margin that is a
//!   length reads back as written (`2.715px`), not as the LayoutUnit it was laid out at.

const CHROME: &[(&str, &str, &str)] = &[
    ("controls", include_str!("fixtures/form-controls/controls.js"), include_str!("fixtures/form-controls/controls.chrome-152.json")),
    ("insets", include_str!("fixtures/form-controls/insets.js"), include_str!("fixtures/form-controls/insets.chrome-152.json")),
    ("line-widths", include_str!("fixtures/form-controls/line-widths.js"), include_str!("fixtures/form-controls/line-widths.chrome-152.json")),
];

#[tokio::test]
async fn form_controls_insets_and_line_widths_read_as_chrome_150_reads_them() {
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

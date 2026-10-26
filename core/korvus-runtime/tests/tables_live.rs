//! LIVE gate: tables laid out as Chrome lays them out, the presentational hints that style them, the
//! line metrics of text set in a fallback font, and the CSS animations of a shadow tree.
//!
//! Every expectation is Google Chrome 150.0.7871.46's answer on this machine on 2026-10-08, to
//! `fixtures/tables/*.js` served from 127.0.0.1 at the persona's 1920x965 viewport:
//!
//! * `tables`: fourteen tables -- default spacing and padding, a caption, `width: 100%` and a fixed
//!   width, a fixed-width cell, `colspan` and `rowspan`, `display: table` boxes with anonymous rows, an
//!   empty `display: table`, row groups out of order, `border`/`cellpadding`/`cellspacing` attributes,
//!   `table-layout: fixed`, `vertical-align` in a 40px row, an inline table. korvus had no table
//!   algorithm: every table was as wide as its container with its cells stacked one per line, and no
//!   cell had the 1px padding a `<table>` gives its cells.
//! * `fallback-fonts`: `line-height: normal` takes the fallback font a run is set in into account (a
//!   Korean word in `system-ui` makes a 20px line around a 17px box).
//! * `shadow-animations`: a shadow root's own `@keyframes` animate its elements (an interactive challenge widget's
//!   spinner) and a shadow-tree element does not see the document's.

const CHROME: &[(&str, &str, &str)] = &[
    ("tables", include_str!("fixtures/tables/tables.js"), include_str!("fixtures/tables/tables.chrome-152.json")),
    ("fallback-fonts", include_str!("fixtures/tables/fallback-fonts.js"), include_str!("fixtures/tables/fallback-fonts.chrome-152.json")),
    ("shadow-animations", include_str!("fixtures/tables/shadow-animations.js"), include_str!("fixtures/tables/shadow-animations.chrome-152.json")),
];

#[tokio::test]
async fn tables_hints_fallback_lines_and_shadow_animations_read_as_chrome_150_reads_them() {
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

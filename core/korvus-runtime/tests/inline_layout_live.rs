//! LIVE gate: lines laid out as Chrome lays them out -- `vertical-align`, the baselines of buttons and
//! flex containers, `inside` list markers -- and the style the widget's text depends on: `system-ui`,
//! and the prefixed aliases (`-webkit-transform` IS `transform`).
//!
//! Every expectation is Google Chrome 150.0.7871.46's answer on this machine on 2026-10-08, to
//! `fixtures/inline-layout/*.js` served from 127.0.0.1 at the persona's 1920x965 viewport. The
//! an interactive challenge widget's probe area (snapshotted from a live challenge frame) has a `<progress>` (UA
//! `vertical-align: -0.2em`), an empty `<button>`, a closed `<details>`, transformed boxes written with
//! `-webkit-transform` after `transform`, and every word in `system-ui`.

const CHROME: &[(&str, &str, &str)] = &[
    ("vertical-align", include_str!("fixtures/inline-layout/vertical-align.js"), include_str!("fixtures/inline-layout/vertical-align.chrome-152.json")),
    ("baselines", include_str!("fixtures/inline-layout/baselines.js"), include_str!("fixtures/inline-layout/baselines.chrome-152.json")),
    ("markers", include_str!("fixtures/inline-layout/markers.js"), include_str!("fixtures/inline-layout/markers.chrome-152.json")),
    ("system-ui", include_str!("fixtures/inline-layout/system-ui.js"), include_str!("fixtures/inline-layout/system-ui.chrome-152.json")),
    ("aliases", include_str!("fixtures/inline-layout/aliases.js"), include_str!("fixtures/inline-layout/aliases.chrome-152.json")),
    ("list-style", include_str!("fixtures/inline-layout/list-style.js"), include_str!("fixtures/inline-layout/list-style.chrome-152.json")),
];

#[tokio::test]
async fn lines_and_their_style_read_as_chrome_150_reads_them() {
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

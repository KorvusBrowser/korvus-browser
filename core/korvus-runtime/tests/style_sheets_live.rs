//! LIVE gate: `document.styleSheets` / `ShadowRoot.styleSheets` list what Chrome lists, and a `data:`
//! stylesheet's rules are readable.
//!
//! The list is now built off the arena (it ran `querySelectorAll` and read each
//! element's `rel` and `sheet` through page-visible getters, which the challenge frame counted); this
//! pins its membership and order -- `<style>`, `rel=stylesheet` and `rel="alternate stylesheet"`
//! links, not `rel=icon` or a rel-less link, a shadow root's own sheets, a removed element's sheet
//! gone, adopted sheets not listed -- and `[SameObject]`. A `data:` URL's response is "basic", so its
//! sheet is origin-clean: Chrome hands it its rules where this engine threw `SecurityError`.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/style-sheets/probe.js`, served from 127.0.0.1, answered
//! `fixtures/style-sheets/chrome-152.json`.

#[tokio::test]
async fn style_sheets_list_as_chrome_150_lists_them() {
    let script = include_str!("fixtures/style-sheets/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/style-sheets/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want);
}

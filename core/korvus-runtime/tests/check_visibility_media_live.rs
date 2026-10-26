//! LIVE gate: `Element.checkVisibility()` reads `display` under the document's own media, as `getComputedStyle` and
//! the layout do.
//!
//! It read it under `MediaContext::default()` -- a 1024x768 screen with no theme -- so on any wider window an `@media`
//! rule answered for another viewport there, and every call moved the page's style basis: the next style read or
//! layout restyled every element. www.ticketmaster.com alternates `checkVisibility()` with `getBoundingClientRect()`;
//! 975 of its 997 layouts were full restyles (perf counters), 18.3-19.1 s of CPU for `fetch --wait 5`, 4.1-4.7 s with
//! the document's media.
//!
//! `fixtures/check-visibility-media/probe.js` makes its breakpoints from the window's own `innerWidth`, so the answers do
//! not depend on the window's size. `chrome-150.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12,
//! headless at 1920x1080, no DevTools attached (the page posted its answers back), three runs alike.

#[tokio::test]
async fn check_visibility_reads_display_under_the_documents_media() {
    let script = include_str!("fixtures/check-visibility-media/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/check-visibility-media/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

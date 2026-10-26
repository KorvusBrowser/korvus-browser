//! EVERY `FrozenArray<T>` ATTRIBUTE IS HANDED OUT AS CHROME 150'S BINDINGS HAND IT OUT (C2, 2026-10-11).
//!
//! Fingerprint Pro scored korvus `bad / browser_automation_studio / tampering / anti_detect_browser` on one payload
//! field: `navigator.languages`' `length` and entries were writable and configurable (korvus built a new plain array
//! per read -- the shape of an anti-detect browser's JS override); splicing that field alone flipped the verdict both
//! ways, 3/3 against its sham. The rule is WebIDL's and general, so the fix is one binding step
//! (`webapi::install::frozen_array`, over every FrozenArray attribute of Blink 150's IDL and how a second read
//! answers: `[SaveSameObject]`, `[CachedAttribute=Pred]`, a getter returning a stored `FrozenArray<T>`, or a fresh
//! array) and this sweep: every such attribute a page reaches without a permission or a device, in a window and a
//! dedicated worker -- frozen or not, `length`'s writability, the same object on a second read, entry 0's descriptor
//! and identity -- the `[CachedAttribute]`s walked through what makes them dirty (`DataTransfer.types`,
//! `MessageEvent.ports`), the reflected element lists, `ClipboardItem`'s constructor (whose `types` korvus did not
//! have), and `Event.composedPath()` as the control that must stay unfrozen.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, NO
//! DevTools attached, three runs alike): `fixtures/frozen-arrays/sweep-probe.js` in `canvas-and-errors/probe-page.html`
//! answered `sweep-chrome-150.json`. Left out because their gaps are not the FrozenArray rule (reported, not fixed
//! here): `ClipboardChangeEvent` (its constructor takes the dictionary first), `CSSViewTransitionRule` (the parser
//! drops `@view-transition`), an element list read from its content attribute's ids, `Notification`'s `vibrate`
//! pattern, and `MediaImage.src` resolution.

#[tokio::test]
async fn every_frozen_array_attribute_answers_as_chromes() {
    let got = crate::chrome_canvas_and_errors_live::answers(crate::chrome_canvas_and_errors_live::Mode::Page, include_str!("fixtures/frozen-arrays/sweep-probe.js")).await;
    crate::chrome_canvas_and_errors_live::assert_as_chrome(got, include_str!("fixtures/frozen-arrays/sweep-chrome-152.json"));
}

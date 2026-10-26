//! DOM and window answers as stock Chrome 150 gives them (unit F5): what the investigators of
//! a fingerprinting library, BrowserScan and a bot-detection service found korvus answering otherwise -- `document.createEvent`, the URL parser's
//! Chromium schemes, a named frame on the window, `chrome.loadTimes`/`csi` and `chrome.app` in a frame on its way
//! to its first document, the plugin lists' named properties, and `navigator.storageBuckets`.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, NO
//! DevTools attached, three runs alike): each `fixtures/dom-window-answers/<name>-probe.js` in
//! `canvas-and-errors/probe-page.html`, served as `chrome_canvas_and_errors_live` serves it, answered
//! `<name>-chrome-150.json`. What a probe leaves out, and why, is said at its test.

use crate::chrome_canvas_and_errors_live::{answers, assert_as_chrome, Mode};

/// `document.createEvent` over Chrome 150's own Event interfaces, Blink's aliases and names Blink once had, in
/// three casings, and its argument conversions. Blink's factory (`make_event_factory.py`): `TransitionEvent` is
/// still made in 150 (deprecated in M148), `TouchEvent` is refused without touch event feature detection -- off
/// on every desktop -- and every refusal carries "Failed to execute 'createEvent' on 'Document': ". A Symbol is
/// V8's TypeError, prefixed.
#[tokio::test]
async fn create_event_answers_as_blinks_factory() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/create-event-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/create-event-chrome-152.json"));
}

/// The URL parser where Chromium's is not the WHATWG one: `filesystem:` URLs (an inner standard URL and a type;
/// `filesystem:///x` invalid; `pathname` the outer path, `origin` the inner one), the schemes Chrome registers as
/// standard (`chrome-extension:` and eight more: host canonicalized, no user info or port, a tuple origin but for
/// the no-access two), `file:`'s origin, relative resolution against both and their setters, through `URL` and
/// `<a href>`. Left out: the five schemes GURL still parses as opaque paths (`android:` and its kind), which the
/// engine's URL record cannot hold.
#[tokio::test]
async fn url_parses_chromes_schemes_as_chrome() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/url-schemes-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/url-schemes-chrome-152.json"));
}

/// A named child frame is reached through the Window's named properties object, never an own property of the
/// window: not in `getOwnPropertyNames`, `hasOwnProperty` false, `delete` a no-op, shadowed by a window property
/// and by `EventTarget.prototype`/`Object.prototype`, before an element of the same id, the first-made of two
/// frames of one name, the frame's own `window.name` (not its element's renamed attribute), a frame in a frame
/// its own window's, an index-like name reached by index.
#[tokio::test]
async fn named_frames_are_window_named_properties() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/window-named-frames-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/window-named-frames-chrome-152.json"));
}

/// `chrome.loadTimes` / `chrome.csi`: constructors with a `prototype` of their own, `new` answering the object,
/// in the page and in a frame; `chrome.app`'s four are not constructors. Left out: `fn.arguments` / `fn.caller`,
/// which read `null` off Chromium's extension-script functions and throw off this engine's API functions.
#[tokio::test]
async fn load_times_and_csi_are_constructible_as_chromes() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/chrome-fns-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/chrome-fns-chrome-152.json"));
}

/// `chrome.app` is absent from the initial about:blank window of a frame inserted with a navigation to make (a
/// `srcdoc`, parsed or set before insertion, or a `src`), and the same `chrome` object gains it when that navigation
/// commits; a frame with no navigation, or `src="about:blank"`, has it from the start and keeps it.
#[tokio::test]
async fn chrome_app_waits_for_a_frames_first_navigation() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/chrome-app-frames-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/chrome-app-frames-chrome-152.json"));
}

/// `PluginArray`, `Plugin`, `MimeTypeArray`: indices enumerable, names `[LegacyUnenumerableNamedProperties]` -- not in
/// `Object.keys`, `for-in`, `JSON`, so a plugin string lists each mime type once -- and neither redefinable nor
/// deletable.
#[tokio::test]
async fn plugin_lists_hide_their_names_as_chromes() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/plugins-named-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/plugins-named-chrome-152.json"));
}

/// `navigator.storageBuckets` and its buckets: `open`'s conversions, name rules and options, `keys()` sorted,
/// `delete()`, a bucket's `persist`/`persisted`/`expires`/`setExpires`/`estimate`, its own `indexedDB`, `caches` and
/// file system apart from the default bucket's, and the receiver checks. Left out: a deleted bucket's object (the
/// tab crashed on it in Chrome, three runs alike) and the usage IndexedDB's backing store adds to `estimate()`
/// (1236 in Chrome; this engine accounts no storage usage).
#[tokio::test]
async fn storage_buckets_answer_as_chromes() {
    let got = answers(Mode::Page, include_str!("fixtures/dom-window-answers/storage-buckets-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/dom-window-answers/storage-buckets-chrome-152.json"));
}

//! Promise-returning platform APIs a bot-challenge frame calls, answered as stock Chrome 150 answers them:
//! what they settle with and WHEN -- already settled at return, a microtask later, or a task
//! later the way a browser-process reply arrives. Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on
//! this machine with the probe beside it in `fixtures/platform-answers/` and is replayed in the persona's engine.
//!
//! Rows a probe records but these lists leave out, and why: a REMOVED frame's navigator (the detached-window
//! behaviours, a later batch); a local face's family in a canvas font (canvas text does not use web fonts yet); the
//! built-in AI APIs' `availability()` and `create()` answers -- the desktop's on-device model state, which moves under
//! a running Chrome (the persona's capture said `unavailable` for all four, a later recording `downloadable` for two
//! and pending for two); and the settle times of `requestAdapter`, `storage.estimate`, `getFileHandle` and
//! `setLocalDescription`, which land a different number of timers late in each Chrome run.

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, probe).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// `navigator.getBattery()`: one promise per navigator, settled a task later with one `BatteryManager` -- a platform
/// object with no own properties (`JSON.stringify` is `{}`), its members on the prototype, an illegal constructor,
/// `onX = 5` reading `null`, its events dispatched -- and a child frame's own manager.
#[tokio::test]
async fn get_battery_answers_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/platform-answers/battery-probe.js"), include_str!("fixtures/platform-answers/battery-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `new FontFace(name, "local(...)")`: a face fontconfig finds (by PostScript name, then full name, among scalable
/// faces with a file on disk) loads synchronously, one it does not fails with Chrome's NetworkError, and `load()` and
/// `status` agree with `document.fonts`.
#[tokio::test]
async fn local_font_faces_load_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/platform-answers/font-face-local-probe.js"), include_str!("fixtures/platform-answers/font-face-local-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
    let d = differences(include_str!("fixtures/platform-answers/font-face-local-2-probe.js"), include_str!("fixtures/platform-answers/font-face-local-2-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `Summarizer`, `LanguageModel`, `LanguageDetector` and `Translator`: their static members, and the options
/// dictionary converted as Blink's bindings convert it -- an invalid enum value, a missing required member -- the
/// error REJECTING the promise rather than throwing.
#[tokio::test]
async fn built_in_ai_statics_convert_their_options_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/platform-answers/ai-statics-probe.js"), include_str!("fixtures/platform-answers/ai-statics-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
    let d = differences(include_str!("fixtures/platform-answers/ai-create-probe.js"), include_str!("fixtures/platform-answers/ai-create-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// When a promise settles against a microtask, a posted message and timers queued right after the call:
/// `getHighEntropyValues`, `createOffer`, `permissions.query` and `keyboard.getLayoutMap` (OPFS `getDirectory` is
/// `chrome_opfs_timing_live`'s: Chrome 150's lands between different timers run to run).
#[tokio::test]
async fn platform_promises_settle_in_chrome_150s_order() {
    let d = differences(include_str!("fixtures/platform-answers/settle-order-probe.js"), include_str!("fixtures/platform-answers/settle-order-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// The ids `setTimeout` answers count the PAGE's timers only: nothing the engine queues for itself takes one --
/// `AbortSignal.timeout`, `scheduler.postTask` and `yield`, a WebGL context's `webglcontextlost`, a message, a
/// frame's navigation, a view transition, `requestIdleCallback`, `chrome.csi()`. Two `setTimeout`s around each are
/// 1 apart in Chrome 150; `loseContext()` put them 2 apart here, through the realm's own `setTimeout` (the
/// an interactive-widget challenge challenge frame loses a WebGL 1 and a WebGL 2 context, then arms its timers).
#[tokio::test]
async fn timer_ids_count_only_the_pages_timers_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/platform-answers/timer-ids-probe.js"), include_str!("fixtures/platform-answers/timer-ids-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

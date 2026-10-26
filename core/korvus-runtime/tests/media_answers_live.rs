//! LIVE gate: WebRTC, Web Audio and media-device answers as Chrome 150 gives them (PHASE 3 F7; BrowserScan
//! items 7 and 8, a bot sensor's media/audio probe).
//!
//! What each fixture pins, and the Chromium source each rule is taken from:
//!
//!   * `rtc-media` -- `createOffer()` resolves an `RTCSessionDescriptionInit` DICTIONARY (members sorted: `sdp`,
//!     `type`); `localDescription` is an `RTCSessionDescription`; `RTCIceCandidate.toJSON()` is Blink's
//!     `toJSONForBinding` (`sdpMid`/`usernameFragment` as strings, `""` for null, `sdpMLineIndex` only when set);
//!     a constructed candidate's `usernameFragment` is the dictionary's, never the line's `ufrag`
//!     (`RTCIceCandidatePlatform::PopulateFields`); a `MediaStreamAudioDestinationNode`'s stream is a real
//!     `MediaStream` whose audio track reads its WebAudio source's settings and capabilities
//!     (`media_stream_audio_destination_node.cc`, `MediaStreamSource::GetSettings`) -- it was a bare shell, and
//!     every member threw `Illegal invocation`.
//!   * `devices-audio` -- `decodeAudioData` transfers the ArrayBuffer and answers from a task posted back by its
//!     decoder thread (`DecodeOnBackgroundThread` -> `HandleDecodeAudioData`): a `DOMException` `EncodingError`,
//!     the promise first and the callback after, with the context as `this`; a detached buffer is a synchronous
//!     `DataCloneError`; PCM converts to float by `FixedSampleTypeTraits` and to the context's rate by
//!     `SincResampler`. `enumerateDevices()` answers `MediaDeviceInfo` instances from a task.
//!   * `offline-audio` -- `OfflineAudioContext.startRendering()` renders on a thread of its own: `running` after
//!     the call, the page's tasks running meanwhile, `statechange` for `running` and `closed`, the `complete`
//!     listener and `oncomplete` before the promise; a second call rejects.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs alike), from the probe beside each fixture, served from 127.0.0.1. Fresh UUIDs
//! are reported by shape; timings are not recorded (they are the machine's).

fn compare(got: &[serde_json::Value], want: &[serde_json::Value]) -> Vec<String> {
    let mut wrong: Vec<String> = got
        .iter()
        .zip(want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  korvus {g}\n  chrome {w}"))
        .collect();
    if got.len() != want.len() {
        wrong.push(format!("  korvus answered {} rows, chrome {}", got.len(), want.len()));
    }
    wrong
}

#[tokio::test]
async fn webrtc_values_and_the_destination_stream_answer_as_chrome_150() {
    let script = include_str!("fixtures/rtc-media/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/rtc-media/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong = compare(&got, &want);
    assert!(wrong.is_empty(), "{} rows differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

#[tokio::test]
async fn decode_audio_data_and_enumerate_devices_answer_as_chrome_150() {
    let script = include_str!("fixtures/devices-audio/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/devices-audio/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong = compare(&got, &want);
    assert!(wrong.is_empty(), "{} rows differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

#[tokio::test]
async fn offline_rendering_follows_chromes_render_thread_model() {
    let script = include_str!("fixtures/offline-audio/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/offline-audio/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers_value(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let got = got.as_array().cloned().unwrap_or_default();
    let wrong = compare(&got, &want);
    assert!(wrong.is_empty(), "{} rows differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

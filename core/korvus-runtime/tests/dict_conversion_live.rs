//! A WebIDL DICTIONARY IS CONVERTED AS CHROME 150 CONVERTS IT -- every hand-written binding too.
//! A sweep handed a logging Proxy to 229 dictionary arguments (every `…Event` constructor and the other
//! dictionary-taking constructors and methods) and found korvus reading 114 of them unlike stock Chrome: members in
//! its own order, members left out, a getter's throw swallowed (`new Event('e', {get bubbles() { throw 1 }})`
//! constructed), messages without Blink's context, deno's own sentences leaking ("expected f64", "serde_v8 error"),
//! and `addEventListener` with no `signal` at all. The rule now lives in one place, `install::ctor_dicts::
//! convert_dict` over the dictionaries `tools/sync/gen_ctor_dicts.py` emits from Blink 150's IDL -- members in WebIDL's
//! order (the ancestors' first, each dictionary's sorted), each read once and converted as it is read, the members of
//! features Chrome 150 has off left out, `[EnforceRange]`/`[Clamp]`/`[DeprecateAs]`/`[Exposed=Window]` honoured --
//! and the bindings read the converted dictionary in place of the page's object.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, three
//! runs alike): `fixtures/dict-conversion/sweep-probe.js` answered `sweep-chrome-150.json` row for row -- the reads,
//! in order, and what each call answered -- and `values-probe.js` answered `values-chrome-150.json`: what converted
//! members read back as, listener options (`signal`, default passive, `capture` from a number), numeric and string
//! arguments of the macro-built classes, `fetch`'s argument order, non-object dictionaries, and the scroll methods'
//! rejections. The streams, `VideoFrame`'s source-state check and `VideoEncoder.isConfigSupported` are left out of the
//! sweep: korvus's streams model has no `pull` source or queuing strategy to read, and the other two are a stub.

use crate::keyboard_live::{answers, CHROME_BUNDLE};

async fn compare(script: &str, recorded: &str) {
    let want: Vec<serde_json::Value> = serde_json::from_str(recorded).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

#[tokio::test]
async fn every_dictionary_is_read_in_chromes_order() {
    compare(include_str!("fixtures/dict-conversion/sweep-probe.js"), include_str!("fixtures/dict-conversion/sweep-chrome-152.json")).await;
}

#[tokio::test]
async fn converted_dictionaries_and_arguments_answer_as_chromes() {
    compare(include_str!("fixtures/dict-conversion/values-probe.js"), include_str!("fixtures/dict-conversion/values-chrome-152.json")).await;
}

/// `members-probe.js` (recorded the same way, three runs alike): an integer member's `[Clamp]` -- Blink clamps into
/// the range and TRUNCATES an 8-, 16- or 32-bit value (`streamErrorCode: 3.5` is 3), and rounds half to even only a
/// 64-bit one -- and `[EnforceRange]`; a Symbol or a BigInt member refused with Blink's context; Blink 150's
/// `WebTransportError(init)` and `WebSocketError(message, init)` with its close code and reason checks; and the members
/// a worker reads (`RequestInit`'s `[Exposed=Window]` ones are not).
#[tokio::test]
async fn members_convert_as_chromes_in_a_window_and_a_worker() {
    compare(include_str!("fixtures/dict-conversion/members-probe.js"), include_str!("fixtures/dict-conversion/members-chrome-152.json")).await;
}

/// `overloads-probe.js` (recorded the same way, three runs alike): what a constructor answers before it converts an
/// argument -- WebIDL's argument count ("N argument(s) required, but only M present."; korvus answered its own checks
/// first: "parameter 1 is not of type 'Function'.", "The provided double value is non-finite."), the overload the count
/// picks (`OfflineAudioContext` with two arguments: "Overload resolution failed."; `KeyframeEffect` with one: the copy
/// constructor), and `WebKitMutationObserver` being `MutationObserver` itself ([LegacyWindowAlias]; korvus built a
/// second interface).
#[tokio::test]
async fn constructors_resolve_their_overloads_as_chromes() {
    compare(include_str!("fixtures/dict-conversion/overloads-probe.js"), include_str!("fixtures/dict-conversion/overloads-chrome-152.json")).await;
}

/// `arity-sweep-probe.js` (three runs alike): every interface object that requires two or more arguments, constructed
/// with one too few -- the count is refused before any argument is converted (72 interfaces; korvus's `VTTCue` read
/// its numbers first, "The provided double value is non-finite.").
#[tokio::test]
async fn every_constructor_counts_its_arguments_first() {
    compare(include_str!("fixtures/dict-conversion/arity-sweep-probe.js"), include_str!("fixtures/dict-conversion/arity-sweep-chrome-152.json")).await;
}

/// `mediacaps-probe.js` (three runs alike): `navigator.mediaCapabilities.decodingInfo` / `encodingInfo` -- the members
/// read through a logging proxy, nested configurations included, in WebIDL's order; every refusal a REJECTION, in
/// Blink's sentences; and the `webrtc` answers from the device's RTP codecs (VP8/VP9/H264/AV1 and opus/G722/PCMU/PCMA,
/// an encoder's scalability modes, a decoder's spatial layers). korvus read `type` first and only what its own checks
/// needed, threw, refused `width: NaN` (Chrome converts it to 0) and answered every `webrtc` question unsupported.
#[tokio::test]
async fn media_capabilities_convert_and_answer_as_chromes() {
    compare(include_str!("fixtures/dict-conversion/mediacaps-probe.js"), include_str!("fixtures/dict-conversion/mediacaps-chrome-152.json")).await;
}

/// `stub-methods-probe.js` (three runs alike, no DevTools attached, in `canvas-and-errors/probe-page.html`): a second
/// sweep, over the methods the first did not reach -- the generated shells' 549 operations among them, which converted
/// no argument at all (`navigator.hid.requestDevice({})` resolved where Chrome rejects for its missing `filters`,
/// `navigator.geolocation.getCurrentPosition(5)` returned) -- and the hand-written readers that read their own way:
/// `requestFullscreen`'s `FullscreenOptions`, `setHTMLUnsafe`, `Document.parseHTMLUnsafe`, `requestStorageAccess`,
/// the media session, the gated window methods, `IdleDetector.start` (Chrome refuses it a task later), and
/// `navigation.navigate('#x', d)` -- a same-document navigation, which used to load the page again.
#[tokio::test]
async fn stub_methods_convert_their_arguments_as_chromes() {
    let got = crate::chrome_canvas_and_errors_live::answers(crate::chrome_canvas_and_errors_live::Mode::Page, include_str!("fixtures/dict-conversion/stub-methods-probe.js")).await;
    crate::chrome_canvas_and_errors_live::assert_as_chrome(got, include_str!("fixtures/dict-conversion/stub-methods-chrome-152.json"));
}

/// `callbacks-probe.js` (three runs alike, as above): a callback argument that is not one -- a callback FUNCTION is
/// "parameter N is not of type 'Function'.", a callback INTERFACE (`NodeFilter`, `EventListener`) "… 'Object'." and
/// takes any object; korvus called `createTreeWalker`'s `{}` filter not a function.
#[tokio::test]
async fn callback_arguments_convert_as_chromes() {
    let got = crate::chrome_canvas_and_errors_live::answers(crate::chrome_canvas_and_errors_live::Mode::Page, include_str!("fixtures/dict-conversion/callbacks-probe.js")).await;
    crate::chrome_canvas_and_errors_live::assert_as_chrome(got, include_str!("fixtures/dict-conversion/callbacks-chrome-152.json"));
}

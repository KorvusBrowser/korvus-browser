//! LIVE gate: `navigator.keyboard` answers as the persona's browser answers.
//!
//! The row was the constant `null` -- Brave 151's answer -- under every persona, so the Chrome persona
//! told a bot-challenge frame it had no Keyboard API. Chrome 150 hands back a `Keyboard` whose
//! layout map is the device's (48 entries on this US-layout desktop, in Blink's hash order), refuses
//! `lock()` from a subframe and unknown key codes, and supersedes an older `lock()` with a newer one.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/keyboard/probe.js`, served from 127.0.0.1, answered `fixtures/keyboard/chrome-152.json`
//! row for row. The same script also pins the three neighbours the challenge read beside it:
//! `DeprecatedStorageQuota.prototype` has no `constructor` (`[LegacyNoInterfaceObject]`), and
//! `navigator.protectedAudience` / `navigator.login` / `deprecatedRunAdAuctionEnforcesKAnonymity`
//! are a `ProtectedAudience`, a `NavigatorLogin` and `false`, not `undefined`.

use engine::Engine;

pub(crate) const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

/// The probe's answer as one JSON value (an object, say), for probes that do not answer a list.
pub(crate) async fn answers_value(bundle: &str, script: &str) -> serde_json::Value {
    let p = persona::CapturedPersona::select(Some(bundle)).expect("the bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/probe";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><html><head><meta charset=utf-8></head><body></body></html>", url, &[], &[]).await.expect("load");
    let got = e
        .eval_value_settled("r", format!("Promise.resolve({script}).then(r => JSON.stringify(r))"), std::time::Duration::from_secs(20))
        .await
        .expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("null")).expect("json")
}

pub(crate) async fn answers(bundle: &str, script: &str) -> Vec<serde_json::Value> {
    let p = persona::CapturedPersona::select(Some(bundle)).expect("the bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/keyboard";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><html><head><meta charset=utf-8></head><body></body></html>", url, &[], &[]).await.expect("load");
    let got = e
        .eval_value_settled("r", format!("Promise.resolve({script}).then(r => JSON.stringify(r))"), std::time::Duration::from_secs(10))
        .await
        .expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list")
}

#[tokio::test]
async fn the_keyboard_api_answers_as_chrome_150_answers() {
    let script = include_str!("fixtures/keyboard/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/keyboard/chrome-152.json")).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

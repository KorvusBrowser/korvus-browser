//! How a member refuses a call that is not its own, as stock Chrome 150 refuses it.
//! Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine with the probe beside it in
//! `fixtures/foreign-receivers/` (three runs alike), and is replayed here in the persona's engine.
//!
//! The SWEEP calls every getter and method of every interface prototype on the global with the receiver `{}` and
//! no arguments, and awaits what a promise member answers. 232 of its 6,108 rows answered otherwise here:
//! * the argument count before the receiver -- a stub, or a body the arity pass wrapped from outside
//!   (`AudioData.prototype.copyTo.call({})` is `Illegal invocation` in Chrome, "2 arguments required" here);
//! * a promise member typed `Any` because webref does not declare it, throwing where Chrome rejects
//!   (`FileSystemHandle.remove`, the Protected Audience methods, `SharedStorage`'s);
//! * the opposite order for an OVERLOADED promise operation, which counts first in Blink
//!   (`CookieStore.set`, `LockManager.request`, `NavigatorLogin.setStatus`);
//! * no receiver check at all on `performance.memory`'s getters and on `navigator.webkit*Storage`'s methods.
//!
//! QUOTA: the two deprecated quota methods' argument errors, their callbacks (a task later, `this` undefined,
//! `min(requested, quota)`), and a callback's throw reported rather than propagated -- they were no-ops whose
//! callbacks never ran. MEMORY: `performance.memory`'s shape, its brand included (one row the recording drops: the
//! heap numbers themselves, which differ run to run in both).

fn compare(got: Vec<serde_json::Value>, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

#[tokio::test]
async fn every_member_refuses_a_foreign_receiver_as_chrome_150s_does() {
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/foreign-receivers/sweep-probe.js")).await;
    let d = compare(got, include_str!("fixtures/foreign-receivers/sweep-chrome-152.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

#[tokio::test]
async fn the_deprecated_quota_methods_answer_as_chrome_150s() {
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/foreign-receivers/quota-probe.js")).await;
    let d = compare(got, include_str!("fixtures/foreign-receivers/quota-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

#[tokio::test]
async fn performance_memory_is_a_branded_memory_info_as_in_chrome_150() {
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/foreign-receivers/memory-info-probe.js")).await;
    let d = compare(got, include_str!("fixtures/foreign-receivers/memory-info-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

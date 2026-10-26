//! LIVE gate: a registry shell constructor converts its init dictionary as Chrome does
//! (the Chrome oracle ratchet).
//!
//! `binding::protos::cb_shell_ctor` ran no dictionary conversion: `new BlobEvent("x", {})`,
//! `new AudioData({})`, `new RTCError({})` and 42 more constructed where Chrome throws, an
//! interface-typed member was never checked, and the members a page passed were dropped --
//! `new BlobEvent("x", {data: b}).data` was `undefined`. Now `install::ctor_dicts` converts the
//! dictionary from Blink 150's IDL (`tools/sync/gen_ctor_dicts.py` over `gen/blink/idl`): required
//! members, interface / buffer / enum / callback / nested-dictionary checks with Blink's messages, IDL
//! defaults, each member read once in WebIDL's order; and the interface's own attributes read the
//! converted values behind its brand.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/ctor_dicts/probe.js`, served from 127.0.0.1, answered `fixtures/ctor_dicts/chrome-152.json`
//! row for row (messages and the dictionary's read order included).

use crate::keyboard_live::{answers, CHROME_BUNDLE};

#[tokio::test]
async fn shell_constructors_convert_their_dictionary_as_chrome_150_does() {
    let script = include_str!("fixtures/ctor_dicts/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/ctor_dicts/chrome-152.json")).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

/// The positional arguments too: an argument that NAMES one of the interface's readonly attributes is
/// what that attribute reads (`new USBOutTransferResult("ok", -1).bytesWritten` is 4294967295, the
/// `unsigned long` conversion), a sequence argument is iterated (`new Set([p])` works) and its elements
/// checked, and a `FrozenArray` attribute without Blink's `[SameObject]` is a fresh frozen array on
/// every read (`r.packets !== r.packets`). Recorded from Chrome 150 the same way
/// (`fixtures/ctor_dicts/args-probe.js` -> `args-chrome-150.json`).
#[tokio::test]
async fn shell_constructors_convert_their_positional_arguments_as_chrome_150_does() {
    let script = include_str!("fixtures/ctor_dicts/args-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/ctor_dicts/args-chrome-152.json")).expect("the recorded Chrome list");
    let got = answers(CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

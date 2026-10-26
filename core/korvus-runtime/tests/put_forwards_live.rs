//! LIVE gate: a `[PutForwards]` attribute's setter is WebIDL's -- an observable `Get` of the attribute,
//! then a `Set` of the forwarded member on what it returned.
//!
//! `iframe.sandbox = "x"` is `Q = Get(iframe, "sandbox")`, `Set(Q, "value", "x")`: a page that wraps
//! `HTMLIFrameElement.prototype.sandbox` and `DOMTokenList.prototype.value` sees each fire once, in
//! Chrome. This engine's setters wrote the content attribute directly and fired neither; a challenge provider's
//! challenge frame assigns `iframe.sandbox` and its call log, beside stock Chrome's, lacked the getter.
//! The probe covers every forwarding attribute Blink exposes here -- the token
//! lists (`classList`, `part`, `relList` x5, `sizes`, `blocking` x3, `sandbox`, `htmlFor`,
//! `controlsList`), `style` on HTML and SVG elements and on a CSS rule (`cssText`) -- and the receiver
//! checks a foreign `this` meets.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/put-forwards/probe.js`, served from 127.0.0.1, answered
//! `fixtures/put-forwards/chrome-152.json`.

#[tokio::test]
async fn put_forwards_setters_get_then_set_as_chrome_150_does() {
    let script = include_str!("fixtures/put-forwards/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/put-forwards/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let differ: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty() && got.len() == want.len(), "{} of {} differ ({} rows):\n{}", differ.len(), want.len(), got.len(), differ.join("\n"));
}

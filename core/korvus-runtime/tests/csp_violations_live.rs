//! LIVE gate: a request a document's Content-Security-Policy refuses, and the `securitypolicyviolation` event it
//! fires, as Chrome 150 (a bot-detection script D1).
//!
//! korvus asked a document's policies two questions -- may an image load, may a script element run -- and reported
//! nothing: a `fetch()` under `connect-src 'none'` went out, a stylesheet under `style-src 'none'` loaded, and no
//! `SecurityPolicyViolationEvent` ever fired, where a bot-detection script's block page reads them. Every request is now checked
//! against the directive CSP3 names for its destination (`csp::effective_directive`), and each policy it violates
//! reports at the element that made it or at the document, from a task.
//!
//! `fixtures/csp-violations/probe.js` runs each request in a srcdoc frame under its own `<head>` policy and logs the
//! call's outcome and every field of each event, in order; `chrome-150.json` is stock Chrome 150.0.7871.46 (headed on
//! the Wayland desktop, no DevTools, three runs alike).

/// Rows korvus cannot answer as Chrome yet, each with its reason. A row leaves this list when its feature lands.
const KNOWN_GAPS: &[(&str, &str)] = &[
    // `EventSource` is a stub (no event stream, no connection at all); Chrome refuses the connection, reports, and
    // fires `error` with readyState CLOSED. Server-sent events are the ledger's next item.
    ("connect-src none, EventSource", "EventSource is not implemented"),
];

#[tokio::test]
async fn a_refused_request_fails_and_reports_as_chrome_150() {
    let script = include_str!("fixtures/csp-violations/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/csp-violations/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers_value(crate::keyboard_live::CHROME_BUNDLE, script).await;
    // the recorder serves Chrome's page on a port it writes as `:PORT`; this one is on the default port
    let got: serde_json::Value = serde_json::from_str(&got.to_string().replace("//127.0.0.1/", "//127.0.0.1:PORT/")).expect("json");
    let known = |r: &serde_json::Value| KNOWN_GAPS.iter().any(|(name, _)| r.get(0).and_then(|n| n.as_str()) == Some(name));
    let got: Vec<serde_json::Value> = got.as_array().cloned().unwrap_or_default().into_iter().filter(|r| !known(r)).collect();
    let want: Vec<serde_json::Value> = want.into_iter().filter(|r| !known(r)).collect();
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}", got.len(), want.len());
    assert!(wrong.is_empty(), "{} case(s) differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

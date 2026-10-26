//! LIVE gate: a `<time>` property's computed value is in seconds, as Blink writes it.
//!
//! `transition-duration`, `transition-delay`, `animation-duration` and `animation-delay` computed to the value as
//! written: `transition: color 80ms` read `80ms` from `getComputedStyle`, where Chrome reads `0.08s`. Blink makes each
//! a `CSSNumericLiteralValue` in seconds (`ComputedStyleUtils::ValueForAnimationDuration` / `ValueForAnimationDelay`),
//! written as `CustomCSSText` writes any number: an integer up to six digits as an integer, else `%.6g`. The specified
//! value keeps the author's unit. Vue's `<Transition>` reads the computed duration as `Number(s.slice(0, -1)) * 1000`,
//! so `80ms` was `NaN` there and its fallback timer fired at once.
//!
//! `fixtures/css-time/chrome-152.json` is stock Google Chrome 150.0.7871.46 on this machine, 2026-10-12, headless at
//! 1920x1080, no DevTools attached (the page posted its answers back), three runs alike. Each row is the property and
//! value set, the computed value, and the specified value.

#[tokio::test]
async fn a_time_computes_to_seconds_as_blink_writes_it() {
    let script = include_str!("fixtures/css-time/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/css-time/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}: {got:?}", got.len(), want.len());
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert!(wrong.is_empty(), "{} row(s) unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

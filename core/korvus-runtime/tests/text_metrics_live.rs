//! Canvas `measureText` as Chrome 150 answers it on Linux. a challenge provider's managed
//! challenge measures ten emoji at `16px sans-serif` in its second stage and reads every `TextMetrics`
//! member; korvus answered `actualBoundingBox*` 0 (stub rows), `hangingBaseline` in double instead of
//! float, `ideographicBaseline` as the descent where Chrome reads the font's `BASE` table (-1.92 for
//! Noto Sans CJK at 16px, not -5), the `textBaseline` offsets from the wrong origin, and widths in
//! 1000-unit faces off in the fifth significant figure.
//!
//! `fixtures/text-metrics/text-metrics.js` measures nine fonts (generic and named families, a family
//! that does not exist, fractional sizes, bold, italic) over seven strings (Latin, kerning,
//! descenders, emoji with and without U+FE0F, CJK, accented capitals), each `textBaseline` and the
//! shifted `textAlign`s, and every answer is compared with `text-metrics.chrome-150.json`,
//! google-chrome 150.0.7871.46's own (headed, this machine's fonts -- the persona's). The ink box is
//! the font FILES' (glyph outlines through FreeType), so the test needs the persona's fonts installed;
//! it says so instead of failing when they are not. Before this change 71 of its 71 answers differed.
use engine::Engine;

const SCRIPT: &str = include_str!("fixtures/text-metrics/text-metrics.js");
const CHROME: &str = include_str!("fixtures/text-metrics/text-metrics.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

#[tokio::test]
async fn measure_text_answers_as_chrome_150_does() {
    let fonts = std::process::Command::new("fc-list").arg(":family").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    for need in ["Noto Sans CJK KR", "Noto Color Emoji", "Liberation Sans", "Liberation Serif"] {
        if !fonts.contains(need) {
            eprintln!("SKIPPED: the persona's font {need:?} is not installed here; the ink box is the font file's");
            return;
        }
    }
    let want: Vec<serde_json::Value> = serde_json::from_str(CHROME).expect("the recorded Chrome list");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/text-metrics";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><meta charset=utf-8><body></body>", url, &[], &[]).await.expect("load");
    let got = e.eval_value("r", format!("JSON.stringify({SCRIPT})")).expect("eval");
    let got: Vec<serde_json::Value> = serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list");
    assert_eq!(got.len(), want.len(), "answer count");
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("korvus {g}\n    Chrome {w}"))
        .collect();
    assert!(differ.is_empty(), "{} answer(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

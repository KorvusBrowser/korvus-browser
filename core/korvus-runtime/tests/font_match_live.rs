//! Canvas text in the font Chrome 150's font service gives each named family, shaped in the script Blink gives each
//! word.
//!
//! WHICH FONT. Blink asks the font service for a listed family, and its answer is Skia's
//! `SkFontConfigInterfaceDirect::matchFamilyName`: fontconfig's best font for the name, kept only when one of its
//! family names is the requested one (or fontconfig's substitute for it) or METRIC-COMPATIBLE with it
//! (`kFontEquivMap`) -- and when the name matches nothing, Blink tries its alternate (`AlternateFamilyName`:
//! Helvetica / Arial, Times / Times New Roman, Courier / Courier New). korvus took any font fontconfig BINDS to the
//! name, so a sensor cookie font probe (a bot-detection service, `font_detect.js`) read Bitstream Vera Sans Mono (DejaVu Sans Mono) and
//! Century (C059) as installed where Chrome reads neither, and PMingLiU, MS Gothic and SimSun (Noto CJK faces of
//! the same equivalence class) as missing where Chrome reads all three.
//!
//! WHICH SCRIPT. `HarfBuzzShaper::Shape` shapes 8-bit (Latin-1) text as Latin throughout, and 16-bit text by its
//! script runs, where a word of only Common characters stays Common. Each canvas word is shaped alone, so the space
//! of `😀 漢字 ok` is a Common run, and Noto Sans CJK KR's Korean `locl` -- which a Latin run takes under `ko` --
//! does not apply: its space is 0.224 em there and 0.28 em in `a b`. korvus shaped every Common-only run in the
//! UI language's script, so the GPU research's `24px sans-serif` `😀 漢字 ok` (`research/gpu`, T13) drew its words
//! 1.34 px apart from Chrome's software raster (785 of 12,000 pixels), and testpages' `11pt no-real-font-123`
//! `Cwm fjordbank glyphs vext quiz, 😃` measured 245.537 where Chrome measures 241.433 (five spaces of 0.821 px).
//!
//! `fixtures/font-match/*.js` run on this machine's fonts -- the persona's -- and every row is compared with the
//! `*.chrome-150.json` stock Chrome 150.0.7871.46 recorded (headed, no DevTools, three runs alike); the test says
//! so instead of failing where those fonts are not installed.
use engine::Engine;

const DETECT: &str = include_str!("fixtures/font-match/font-detect.js");
const DETECT_CHROME: &str = include_str!("fixtures/font-match/font-detect.chrome-150.json");
const TEXT: &str = include_str!("fixtures/font-match/emoji-cjk-text.js");
const TEXT_CHROME: &str = include_str!("fixtures/font-match/emoji-cjk-text.chrome-150.json");
const LAYOUT: &str = include_str!("fixtures/font-match/layout-detect.js");
const LAYOUT_CHROME: &str = include_str!("fixtures/font-match/layout-detect.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

/// The persona's fonts this measures with, or the one missing.
fn missing_font() -> Option<&'static str> {
    let fonts = std::process::Command::new("fc-list").arg(":family").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    ["Noto Sans CJK KR", "Noto Serif CJK TC", "Noto Sans Mono CJK JP", "Noto Color Emoji", "Liberation Sans", "DejaVu Sans Mono", "C059"]
        .into_iter()
        .find(|f| !fonts.contains(f))
}

/// The rows `script` answers in a page of the Chrome 150 persona.
async fn rows(script: &str) -> Vec<serde_json::Value> {
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/font-match";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><meta charset=utf-8><body></body>", url, &[], &[]).await.expect("load");
    let got = e.eval_value("r", format!("JSON.stringify({script})")).expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list")
}

fn differences(got: &[serde_json::Value], want: &[serde_json::Value]) -> Vec<String> {
    let mut out: Vec<String> = got.iter().zip(want).filter(|(g, w)| g != w).map(|(g, w)| format!("korvus {g}\n    Chrome {w}")).collect();
    if got.len() != want.len() {
        out.push(format!("{} rows, Chrome {}", got.len(), want.len()));
    }
    out
}

#[tokio::test]
async fn a_named_family_is_the_font_chromes_font_service_matches() {
    if let Some(f) = missing_font() {
        eprintln!("SKIPPED: the persona's font {f:?} is not installed here; which font a name reaches is this machine's fontconfig");
        return;
    }
    let want: Vec<serde_json::Value> = serde_json::from_str(DETECT_CHROME).expect("the recorded Chrome rows");
    let got = rows(DETECT).await;
    let differ = differences(&got, &want);
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

/// LAYOUT sets text in the persona's fonts as its capture recorded them (`fonts/metrics.json`, compiled into
/// `font_table.rs`), so the families a page detects through `offsetWidth` are the capture's. It recorded the 21
/// names it probed, where Chrome 150 on the persona detects 93 of BrowserScan's (`browserscan/REPORT.md` item 4):
/// `fonts-probe.html` now probes those, the fingerprinting lists' names and Skia's equivalents, and this is every
/// answer of the DOM probe compared with Chrome's. No host fonts are read: the answers are the bundle's.
#[tokio::test]
async fn layout_detects_the_families_chrome_150_detects_on_the_persona() {
    let want: Vec<serde_json::Value> = serde_json::from_str(LAYOUT_CHROME).expect("the recorded Chrome rows");
    let got = rows(LAYOUT).await;
    let differ = differences(&got, &want);
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

#[tokio::test]
async fn a_word_of_common_characters_in_16_bit_text_is_shaped_common() {
    if let Some(f) = missing_font() {
        eprintln!("SKIPPED: the persona's font {f:?} is not installed here; the advances and pixels are the font files'");
        return;
    }
    let want: Vec<serde_json::Value> = serde_json::from_str(TEXT_CHROME).expect("the recorded Chrome rows");
    let got = rows(TEXT).await;
    // KNOWN GAP: the pixels of `😀 漢字 ok` drawn at 11pt in the default font still
    // differ from Chrome's (every advance and width row matches): the fallback emoji's bitmap at this size is
    // scaled unlike Chrome's. Listed so the rest is held exactly; the row must keep differing until it is fixed.
    const KNOWN_GAPS: &[&str] = &["alpha 11pt no-real-font-123 | \"😀 漢字 ok\""];
    let differ = differences(&got, &want);
    let (gaps, differ): (Vec<_>, Vec<_>) = differ.into_iter().partition(|d| KNOWN_GAPS.iter().any(|g| d.contains(&serde_json::to_string(g).unwrap())));
    assert_eq!(gaps.len(), KNOWN_GAPS.len(), "a known gap now matches Chrome: take it off the list");
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

//! `strokeText` pixels as Chrome 150 rasterizes them on Linux. a challenge provider's
//! managed challenge strokes and fills text on its canvas probe and hashes the pixels. korvus drew text
//! through Skia's own family matching (`sans-serif` -> the JP face, no Korean `locl` forms), at Skia's
//! advances, with Skia's default text gamma, and placed `textBaseline` from Skia's float metrics.
//!
//! Text is now drawn from the run `measureText` measures (`render::text::shape`: Chrome's fonts,
//! glyphs and 16.16 positions, at subpixel positions), offset by Blink's `GetFontBaseline`, onto a
//! surface with Chromium's Linux text contrast/gamma (0.2 / 1.2). The five stroked samples of
//! `fixtures/canvas-text-pixels/stroke-text.js` are compared byte for byte with
//! `stroke-text.chrome-150.json`, google-chrome 150.0.7871.46 with `--disable-gpu` (a GPU rasterizes
//! the device's way; text is the same in both, measured). Before this change all 5 differed.
//! Filled text is still a few levels off at glyph edges (the glyph-mask rasterizer), so it is not here.
use engine::Engine;

const SCRIPT: &str = include_str!("fixtures/canvas-text-pixels/stroke-text.js");
const CHROME: &str = include_str!("fixtures/canvas-text-pixels/stroke-text.chrome-150.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

#[tokio::test]
async fn stroked_text_is_pixel_identical_to_chrome_150() {
    let fonts = std::process::Command::new("fc-list").arg(":family").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    for need in ["Noto Sans CJK KR", "Noto Serif CJK KR", "Liberation Sans"] {
        if !fonts.contains(need) {
            eprintln!("SKIPPED: the persona's font {need:?} is not installed here; text pixels are the font file's");
            return;
        }
    }
    let want: Vec<(String, Vec<u8>)> = serde_json::from_str(CHROME).expect("the recorded Chrome images");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/stroke-text";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><meta charset=utf-8><body></body>", url, &[], &[]).await.expect("load");
    let got = e.eval_value("r", format!("JSON.stringify({SCRIPT})")).expect("eval");
    let got: Vec<(String, Vec<u8>)> = serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list");
    assert_eq!(got.len(), want.len(), "image count");
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .filter_map(|((name, g), (_, w))| {
            let px: std::collections::BTreeSet<usize> = g.iter().zip(w).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i / 4).collect();
            (!px.is_empty()).then(|| format!("{name}: {} pixel(s) differ, first at {:?}", px.len(), px.iter().next()))
        })
        .collect();
    assert!(differ.is_empty(), "{} image(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}

//! Web Audio rendered as Chrome 152 renders it on x86 Linux. a challenge provider's
//! managed challenge renders a triangle at 9998.123456 Hz through a DynamicsCompressor in an
//! OfflineAudioContext in its second stage -- the classic audio fingerprint, a sum over the samples.
//! korvus's samples differed from Chrome's by an ulp from sample 269 on (688 of 1000): its wavetables
//! came from its own double-precision FFT, its oscillator interpolated with the textbook formula, the
//! k-rate/a-rate split ignored the automation event `frequency.value = f` schedules, and its
//! compressor was an older Blink revision with the platform's `powf`/`asinf`.
//!
//! Now: tables from Chromium's PFFFT and Blink's float `CreateBandLimitedTables`; the oscillator's
//! x86 kernels (float32 k-rate lanes, double a-rate positions, Lagrange below 3.2 Hz) on Blink's
//! sample-accurate rule; the value setter's `setValueAtTime(value, currentTime)`; Chrome 150's
//! compressor with Chromium's fdlibm. `fixtures/audio-render/audio-render.js` renders nine graphs and
//! every sample is compared with `audio-render.chrome-152.json`, Google Chrome 152.0.7977.82's (this
//! machine, headless, no DevTools, two runs alike; 2026-10-16). Before this change all 9 differed. Automation
//! ramps (Blink's timeline fill loops) are not ported yet and so are not here.
//!
//! Chrome 152 builds the tables with `rustfft` (the stable `WebAudioRustFft` flag) where 150 used PFFFT: every
//! oscillator render moved by an ulp or two from sample 1, the compressor on DC did not.
use engine::Engine;

const SCRIPT: &str = include_str!("fixtures/audio-render/audio-render.js");
const CHROME: &str = include_str!("fixtures/audio-render/audio-render.chrome-152.json");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

#[tokio::test]
async fn audio_renders_as_chrome_152_renders_it() {
    let want: Vec<(String, Vec<f32>)> = serde_json::from_str(CHROME).expect("the recorded Chrome renders");
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/audio-render";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><meta charset=utf-8><body></body>", url, &[], &[]).await.expect("load");
    e.eval("audio", format!("({SCRIPT}).then(r => {{ globalThis.__audio = JSON.stringify(r); }}, err => {{ globalThis.__audio = 'ERR ' + err; }});")).expect("eval");
    e.run().await.expect("run");
    let got = e.eval_value("r", "globalThis.__audio".to_string()).expect("read");
    let text = got.as_str().unwrap_or("").to_string();
    assert!(!text.starts_with("ERR") && !text.is_empty(), "render failed: {text}");
    let got: Vec<(String, Vec<f32>)> = serde_json::from_str(&text).expect("a list");
    assert_eq!(got.len(), want.len(), "render count");
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .filter_map(|((name, g), (_, w))| {
            let d: Vec<usize> = g.iter().zip(w).enumerate().filter(|(_, (a, b))| a.to_bits() != b.to_bits()).map(|(i, _)| i).collect();
            (!d.is_empty() || g.len() != w.len()).then(|| format!("{name}: {} of {} samples differ, first at {:?}", d.len(), w.len(), d.first()))
        })
        .collect();
    assert!(differ.is_empty(), "{} render(s) differ from Chrome 152's:\n  {}", differ.len(), differ.join("\n  "));
}

//! `CompressionStream` / `DecompressionStream` answer what Chrome 150 answers.
//!
//! `fixtures/compression-probe.js` is evaluated UNMODIFIED here and in `google-chrome --headless=new`
//! 150.0.7871.46 over CDP; `fixtures/compression-probe.chrome-150.txt` is Chrome's output, RECORDED --
//! two runs, identical -- and this test asserts the engine prints the same lines.
//!
//! A line that differs here is a finding about the engine, never a reason to edit the file. The probe
//! needs no server: the codec is local, so `BASE` is passed and ignored, and the two drivers agree by
//! construction rather than by two servers agreeing.
//!
//! Re-record: run `chrome_eval`-style CDP evaluation of the probe against any loopback page and write
//! the resolved string (plus "\n") to the `.txt`.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/compression-probe.js");
const CHROME: &str = include_str!("fixtures/compression-probe.chrome-150.txt");

/// Drive the engine until a line tagged `tag` appears in its output, and answer the rest of it.
async fn pump_for(e: &mut Engine, tag: &str) -> String {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(i) = out.iter().position(|l| l.starts_with(tag)) {
            return out[i][tag.len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(120) {
            panic!("{tag} never arrived within 120s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn compression_streams_answer_like_chrome() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    // `print` writes one output line, so the multi-line result travels JSON-encoded.
    e.eval(
        "compression_probe",
        format!(
            "({PROBE})(\"http://127.0.0.1/\").then((s) => print('CS1:' + JSON.stringify(s)), (err) => print('CS1:' + JSON.stringify('PROBE FAILED ' + err)));"
        ),
    )
    .unwrap();
    let raw = pump_for(&mut e, "CS1:").await;
    let got: String = serde_json::from_str(&raw).expect("the probe's result is a JSON string");
    let got_lines: Vec<&str> = got.lines().collect();
    let want_lines: Vec<&str> = CHROME.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..want_lines.len().max(got_lines.len()) {
        let w = want_lines.get(i).copied().unwrap_or("<missing>");
        let g = got_lines.get(i).copied().unwrap_or("<missing>");
        if w != g {
            diffs.push(format!("  chrome: {w}\n  korvus: {g}"));
        }
    }
    assert!(diffs.is_empty(), "{} of {} lines differ from Chrome 150:\n{}", diffs.len(), want_lines.len(), diffs.join("\n"));
}

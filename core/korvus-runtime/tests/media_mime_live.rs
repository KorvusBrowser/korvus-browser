//! LIVE gate: `canPlayType` and `MediaSource.isTypeSupported` answer as Chromium's `MimeUtil` does (PHASE 3 F7,
//! ledger N20).
//!
//! The codec grid a bundle records (`codec_grid_live.rs`) covers the 37 containers its sweep asked about; every
//! other container answered `""` -- `audio/x-m4a`, `video/x-m4v`, `audio/matroska`, `video/matroska`,
//! `audio/x-mpegurl`, all `"maybe"` in Chrome 150 because `AddSupportedMediaFormats` registers them (a device-check script's
//! `acma` signal reads `canPlayType('audio/x-m4a;')`). `core/dom/media_codecs.rs` now runs Chromium's algorithm
//! itself -- the container table, the codec-string parsers, the ambiguity rules, the platform check, and Blink's
//! `ContentType` / `MediaSource::IsTypeSupportedInternal` glue -- with the worn device supplying its decoders and
//! build flags.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs alike): `fixtures/media-mime/probe.js`, served from 127.0.0.1, answered
//! `fixtures/media-mime/chrome-152.json`. A row per container -- `[container, canPlayType, isTypeSupported,
//! canPlayType per codec spelling (p/m/-), isTypeSupported per spelling (T/F)]` over Chromium's 25 media types and
//! eight it does not register, times 72 spellings -- then `[string, canPlayType, isTypeSupported]` for the
//! `ContentType` grammar's edge cases. HEVC is left out on purpose: it is the GPU's answer, and this Chrome had
//! none (the bundle's own grid gates it).

/// The codec spellings, in the probe's order, read off the probe itself (one list, never restated here).
fn codecs() -> Vec<String> {
    let probe = include_str!("fixtures/media-mime/probe.js");
    let start = probe.find("var codecs = [").expect("the probe's codec list") + "var codecs = [".len();
    let end = start + probe[start..].find("];").expect("the list ends");
    probe[start..end].split(',').map(|s| s.trim().trim_matches('\'').to_string()).collect()
}

#[tokio::test]
async fn can_play_type_and_mse_answer_as_chromium_mime_util() {
    let script = include_str!("fixtures/media-mime/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/media-mime/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "one row per container and spelling case");
    let spellings = codecs();
    let mut wrong = Vec::new();
    for (g, w) in got.iter().zip(&want) {
        if g == w {
            continue;
        }
        let (Some(ga), Some(wa)) = (g.as_array(), w.as_array()) else {
            wrong.push(format!("  korvus {g}\n  chrome {w}"));
            continue;
        };
        if ga.len() != 5 || wa.len() != 5 {
            wrong.push(format!("  korvus {g}\n  chrome {w}"));
            continue;
        }
        let container = wa[0].as_str().unwrap_or("");
        if ga[1] != wa[1] || ga[2] != wa[2] {
            wrong.push(format!("  {container}: korvus {} {} / chrome {} {}", ga[1], ga[2], wa[1], wa[2]));
        }
        for (col, member) in [(3usize, "canPlayType"), (4, "isTypeSupported")] {
            let (gs, ws) = (ga[col].as_str().unwrap_or(""), wa[col].as_str().unwrap_or(""));
            for (i, (a, b)) in gs.chars().zip(ws.chars()).enumerate() {
                if a != b {
                    let k = spellings.get(i).map(String::as_str).unwrap_or("?");
                    wrong.push(format!("  {member}('{container}; codecs=\"{k}\"'): korvus {a}, chrome {b}"));
                }
            }
            if gs.len() != ws.len() {
                wrong.push(format!("  {container} {member}: korvus answered {} spellings, chrome {}", gs.len(), ws.len()));
            }
        }
    }
    assert!(wrong.is_empty(), "{} answers differ from Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

//! `crypto.subtle` answers what Chrome 150 answers, on 339 lines.
//!
//! `fixtures/webcrypto-probe.js` is evaluated UNMODIFIED here and in `google-chrome --headless=new`
//! 150.0.7871.46 over CDP; `fixtures/webcrypto-probe.chrome-150.txt` is Chrome's output, RECORDED --
//! two runs, identical -- and this test asserts the engine prints the same lines.
//!
//! A line that differs here is a finding about the engine, never a reason to edit the file. The
//! probe needs no server: WebCrypto is local, so `BASE` is passed and ignored.
//!
//! WHY THE CIPHERTEXT LINES MATTER MORE THAN THE ERROR LINES. This lane widened two guards, and a
//! guard widened without the cipher behind it is the worst outcome available: a rejection is
//! visible, a wrong ciphertext is not. Every key and IV in the probe is fixed, so the AES-CBC,
//! AES-CTR, AES-GCM and AES-KW outputs are KNOWN-ANSWER TESTS against Chrome's own bytes (and the
//! AES-KW vector is RFC 3394 §4.1 besides). A GHASH with the wrong reduction polynomial, a CBC with
//! the wrong padding rule or a KW with the wrong round count fails them on the first line.
//!
//! WHAT RSA1 ADDED, and why the four lines that used to stand here as "not answered" are gone.
//! `src/asym.rs` and `modules/crypto/asym_keys.rs` implement RSA and the three P-curves on the
//! BoringSSL this binary ALREADY LINKS -- the same library Chrome's own WebCrypto is built on, at
//! the cost of zero new crates (`btls`/`btls-sys` arrive under `transport`, which `korvus-webapi`
//! already depended on). So `spki`, `pkcs8`, `jwk` and `raw` all carry keys now, and section 12 of
//! the probe is 126 lines of Chrome's answers about them: which moduli and curves import, what each
//! round-trip returns, and the exact type and sentence of every refusal.
//!
//! THREE PLACES CHROME AND THE SPEC PART COMPANY, followed here because the browser is the standard
//! (§5), each measured rather than reasoned:
//!
//! ```text
//! importKey spki RSA 512       = 512      no modulus floor on import; 1024 imports too
//! generateKey RSA-OAEP 512     = 512      nor on generate
//! importKey spki trailing byte = THREW DataError:   `d2i_PUBKEY` ACCEPTS this; Chrome does not
//! ```
//!
//! The third cost a red test to find: `btls`'s `PKey::public_key_from_der` is `d2i_PUBKEY`, which
//! parses a prefix and leaves trailing bytes unread. `binding/btls_ffi.rs` uses BoringSSL's own
//! `EVP_parse_public_key` over a `CBS` and checks it was exhausted, which is what Blink does.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/webcrypto-probe.js");
const CHROME: &str = include_str!("fixtures/webcrypto-probe.chrome-150.txt");

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
async fn subtle_crypto_answers_like_chrome() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    // `print` writes one output line, so the multi-line result travels JSON-encoded.
    e.eval(
        "webcrypto_probe",
        format!(
            "({PROBE})(\"http://127.0.0.1/\").then((s) => print('WC1:' + JSON.stringify(s)), (err) => print('WC1:' + JSON.stringify('PROBE FAILED ' + err)));"
        ),
    )
    .unwrap();
    let raw = pump_for(&mut e, "WC1:").await;
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

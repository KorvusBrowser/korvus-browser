//! **HTTP/3 AS STOCK CHROME 152 WRITES IT** (H3, owner decision 2026-10-11; re-recorded for Chrome 152 2026-10-16).
//!
//! The Chrome side is `fixtures/h3/chrome-152.json`: the rows `fixtures/h3/rows.py` read off three
//! recordings of Google Chrome 152.0.7977.82 (headed, Xvfb, no DevTools, a fresh profile each,
//! `--origin-to-force-quic-on` at a loopback aioquic server, `h3rec.py`), identical in all three. The korvus side is
//! the SAME rows, computed by `wreq::http3::testing::Recorder` from what korvus put on a loopback QUIC server: the
//! client's Initial datagrams (decrypted with the RFC 9001 initial keys), the ClientHello and transport parameters
//! inside them, the HTTP/3 control, QPACK and request streams. Randomized fields are read as Chrome randomizes them:
//! the extension and parameter SETS (BoringSSL and QUICHE shuffle the order), GREASE by shape.
//!
//! **WHY A CHILD PROCESS.** The forced-QUIC origin and the fixture's CA are process-wide (`KORVUS_ORIGIN_TO_FORCE_QUIC_ON`,
//! `KORVUS_EXTRA_CA`, both read when the client is built), so the requests run in a re-run of this test binary with
//! them set, as `https_upgrade.rs` does with its proxy.

use std::time::{Duration, Instant};

const CHILD_ENV: &str = "KORVUS_H3_WIRE_PORT";
const CHROME: &str = include_str!("fixtures/h3/chrome-152.json");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/h3");
/// The bundle whose manifest names the measured QUIC preset (`[transport] quic = "chrome_150"`).
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

/// THE CHILD: a navigation and one subresource over HTTP/3, as the page Chrome was recorded on began.
#[test]
fn the_child_that_speaks_h3() {
    let Ok(port) = std::env::var(CHILD_ENV) else { return };
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let base = format!("https://127.0.0.1:{port}");
    let out = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(async move {
                let nav = transport::FetchRequest::navigation(format!("{base}/")).worn_by(&p);
                let first = transport::fetch(nav).await?;
                let sub = transport::FetchRequest::subresource(format!("{base}/a.js"), "script", transport::Mode::NoCors, format!("{base}/"))
                    .worn_by(&p);
                let second = transport::fetch(sub).await?;
                Ok::<_, anyhow::Error>((first, second))
            })
    })
    .join()
    .expect("driver thread");
    let (first, second) = out.expect("both requests must be answered over HTTP/3");
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    assert_eq!(first.timing.protocol, "h3", "the navigation must have gone over HTTP/3");
}

#[test]
fn an_h3_connection_is_written_as_chromes() {
    let cert = std::fs::read(format!("{FIXTURES}/leaf.pem")).expect("the fixture certificate");
    let key = std::fs::read(format!("{FIXTURES}/leaf.key")).expect("the fixture key");
    let rec = wreq::http3::testing::Recorder::start(&cert, &key).expect("the QUIC recorder");
    let port = rec.port();
    let out = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args(["--exact", "h3_wire::the_child_that_speaks_h3", "--nocapture"])
        .env(CHILD_ENV, port.to_string())
        .env("KORVUS_EXTRA_CA", format!("{FIXTURES}/ca.pem"))
        .env("KORVUS_ORIGIN_TO_FORCE_QUIC_ON", format!("127.0.0.1:{port}"))
        .env_remove("ALL_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("all_proxy")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .output()
        .expect("re-run this test binary for the child");
    assert!(
        out.status.success(),
        "the child's HTTP/3 requests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while rec.answered() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let korvus = rec.rows();
    let chrome: Vec<String> = serde_json::from_str(CHROME).expect("the Chrome rows");
    let missing: Vec<&String> = chrome.iter().filter(|r| !korvus.contains(r)).collect();
    let extra: Vec<&String> = korvus.iter().filter(|r| !chrome.contains(r)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "korvus's HTTP/3 wire differs from Chrome 152's.\nChrome has, korvus not: {missing:#?}\nkorvus has, Chrome not: {extra:#?}"
    );
}

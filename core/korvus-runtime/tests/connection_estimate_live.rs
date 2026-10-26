//! `navigator.connection.downlink` / `.rtt`: readings of the estimate Chrome 150 reads them of.
//!
//! A persona records ONE reading -- Blink's per-host noise over its renderer's network estimate -- and this engine
//! used that reading as the estimate, noising it again: the Chrome 152 bundle's `1.3` (Chrome's 1456 kbit/s under
//! the lowest multiplier) read 1.15..1.45 where Chrome reads 1.3..1.6 (korvus 1.4,
//! Chrome 1.5). The estimate is now the NQE default the recorded pair is a reading of
//! (`network_information.rs` `connection_estimate`).
//!
//! `fixtures/connection/chrome-150.json`: stock Chrome 150.0.7871.46, headed under Xvfb, no DevTools client, a fresh
//! profile per run (`record.py`): the pair 21 cross-site frames on 127.0.0.2..22 read, each in its own renderer. Every
//! reading is noise, so no reading is compared to one of Chrome's: what is compared is the ESTIMATES each engine's
//! readings allow -- every downlink a reading `round(E * m / 50 kbit/s)` of the same estimate `E` under one of
//! Blink's 21 multipliers -- and the two sets must meet; and every `rtt` must be one Chrome read.
use std::io::{BufRead, BufReader, Write};

const CHROME: &str = include_str!("fixtures/connection/chrome-150.json");

/// Blink's `RoundMbps` of `kbps` under multiplier `m`, in kbit/s (`network_information.rs`).
fn reading(kbps: f64, m: f64) -> f64 {
    ((kbps * m).min(10_000.0) / 50.0).round() * 50.0
}

/// Every estimate on a 0.5 kbit/s grid that each of `readings` (Mbit/s) is a reading of.
fn estimates(readings: &[f64]) -> Vec<f64> {
    let ms: Vec<f64> = (0..21).map(|i| 0.9 + i as f64 * 0.01).collect();
    (1000..20_000)
        .map(|i| i as f64 * 0.5)
        .filter(|&e| readings.iter().all(|&r| ms.iter().any(|&m| (reading(e, m) - r * 1000.0).abs() < 1e-6)))
        .collect()
}

/// A page answering on `host`, for one engine to read its connection on.
fn serve(host: &str) -> u16 {
    let l = std::net::TcpListener::bind((host, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let body = "<!doctype html><title>connection</title>";
            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    port
}

#[tokio::test]
async fn the_connection_reads_chromes_estimate() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    let mut chrome_dl = Vec::new();
    let mut chrome_rtt = Vec::new();
    for run in chrome["runs"].as_array().expect("runs") {
        for (_, pair) in run.as_object().expect("a run") {
            chrome_rtt.push(pair[0].as_f64().unwrap());
            chrome_dl.push(pair[1].as_f64().unwrap());
        }
    }
    let mut dl = Vec::new();
    for k in 2..23 {
        let host = format!("127.0.0.{k}");
        let url = format!("http://{host}:{}/", serve(&host));
        let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
        let mut e = engine::Engine::new(Box::new(p));
        e.set_url(&url);
        let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
        e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
        let v = e.eval_value("c", "JSON.stringify([navigator.connection.rtt, navigator.connection.downlink])".to_string()).expect("eval");
        let pair: Vec<f64> = serde_json::from_str(v.as_str().unwrap()).unwrap();
        assert!(chrome_rtt.contains(&pair[0]), "{host}: rtt {} is no rtt Chrome 150 read ({chrome_rtt:?})", pair[0]);
        dl.push(pair[1]);
    }
    let theirs = estimates(&chrome_dl);
    let ours = estimates(&dl);
    assert!(!theirs.is_empty(), "Chrome's own readings are of one estimate: {chrome_dl:?}");
    assert!(
        ours.iter().any(|e| theirs.contains(e)),
        "korvus's downlinks {dl:?} are readings of {:?}..{:?} kbit/s, Chrome 150's {chrome_dl:?} of {:?}..{:?}",
        ours.first(),
        ours.last(),
        theirs.first(),
        theirs.last()
    );
}

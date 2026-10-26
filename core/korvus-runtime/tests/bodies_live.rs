//! Response bodies are bytes, and the byte streams are real. Live, on a loopback server.
//!
//! `fixtures/bodies-probe.js` is evaluated UNMODIFIED here and in `google-chrome --headless=new`
//! 150.0.7871.46 (against `fixtures/bodies-probe-server.py`, which answers every route the server below
//! answers, with the same status, content headers and bytes). `fixtures/bodies-probe.chrome-150.txt` is Chrome's output, RECORDED -- two
//! runs, identical -- and this test asserts the engine prints the same lines. The first eight lines are
//! the brief's table (L14's C05 + C06 rows plus the sync-XHR row); the rest are the edges the change
//! touched (a body read twice, `bodyUsed`, a stream-backed `Response`, `clone()` then both read, the
//! controller's errored state, pipe/tee liveness, XHR's decoders, synchronous XHR).
//!
//! Re-record: run `bodies-probe-server.py <port>`, open any page on it in a headless Chrome over CDP,
//! evaluate `(<bodies-probe.js>)(location.origin + "/")`, and write the resolved string (plus "\n") to
//! the `.txt`. A line that differs here is a finding about the engine, never a reason to edit the file.
//!
//! The server runs on its OWN OS thread (std::net), not on the test's tokio runtime: a synchronous XHR
//! blocks the isolate's thread until the response arrives, so a server polled by that same thread
//! could never answer it.
use engine::Engine;
use std::io::{Read, Write};

const PROBE: &str = include_str!("fixtures/bodies-probe.js");
const CHROME: &str = include_str!("fixtures/bodies-probe.chrome-150.txt");

/// One HTTP/1.1 exchange, `Connection: close` -- the routes `bodies-probe-server.py` serves.
fn serve_one(mut s: std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    let mut content_type: Option<String> = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("content-type") {
                content_type = Some(v.trim().to_string());
            }
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    let respond = |s: &mut std::net::TcpStream, status: &str, ctype: Option<&str>, extra: &[(&str, String)], payload: &[u8]| {
        let mut out = format!("HTTP/1.1 {status}\r\n");
        if let Some(c) = ctype {
            out.push_str(&format!("Content-Type: {c}\r\n"));
        }
        for (k, v) in extra {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        if !status.starts_with("204") {
            out.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        out.push_str("Connection: close\r\n\r\n");
        let _ = s.write_all(out.as_bytes());
        if !status.starts_with("204") {
            let _ = s.write_all(payload);
        }
        let _ = s.flush();
    };
    if method == "POST" {
        if path == "/echo" {
            let ct = content_type.unwrap_or_else(|| "none".into());
            respond(&mut s, "200 OK", Some("application/octet-stream"), &[("x-ct", ct)], &body);
        } else {
            respond(&mut s, "404 Not Found", Some("text/plain"), &[], b"not found");
        }
        return;
    }
    let all: Vec<u8> = (0..=255u8).collect();
    match path.as_str() {
        "/b.bin" => respond(&mut s, "200 OK", Some("application/octet-stream"), &[], &all),
        "/j.json" => respond(&mut s, "200 OK", Some("application/json"), &[], "{\"k\":\"vé\",\"n\":[1,2]}".as_bytes()),
        "/latin.txt" => respond(&mut s, "200 OK", Some("text/plain"), &[], b"caf\xe9"),
        "/bom.json" => respond(&mut s, "200 OK", Some("application/json"), &[], b"\xef\xbb\xbf{\"a\":1}"),
        "/u16.txt" => respond(&mut s, "200 OK", Some("text/plain"), &[], b"\xff\xfeh\x00i\x00"),
        "/empty204" => respond(&mut s, "204 No Content", None, &[], b""),
        _ => respond(&mut s, "404 Not Found", Some("text/plain"), &[], b"not found"),
    }
}

fn start_server() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    std::thread::spawn(move || serve_one(s));
                }
                Err(_) => break,
            }
        }
    });
    addr
}

async fn pump_for(e: &mut Engine, tag: &str) -> String {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(i) = out.iter().position(|l| l.starts_with(tag)) {
            return out[i][tag.len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            panic!("{tag} never arrived within 60s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn bodies_and_streams_answer_like_chrome() {
    let addr = start_server();
    let base = format!("http://{addr}/");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    // `print` writes one output line, so the multi-line result travels JSON-encoded.
    e.eval(
        "bodies_probe",
        format!("({PROBE})({}).then((s) => print('BODIES:' + JSON.stringify(s)), (err) => print('BODIES:' + JSON.stringify('PROBE FAILED ' + err)));", serde_json::to_string(&base).unwrap()),
    )
    .unwrap();
    let raw = pump_for(&mut e, "BODIES:").await;
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

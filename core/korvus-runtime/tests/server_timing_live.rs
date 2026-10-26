//! `Server-Timing` on the performance timeline, as Chrome 150 exposes it (a challenge provider's answers carry
//! `Server-Timing: cfL4;desc="?proto=TCP&rtt=..."`, and its challenge reads it back off the navigation entry).
//!
//! `fixtures/server-timing/server-timing.html` is loaded UNMODIFIED here and in `google-chrome --headless=new`
//! 150.0.7871.46, both from a loopback server answering what `fixtures/server-timing/server.py` answers: the
//! page with three `Server-Timing` fields, a same-origin script with one, and two scripts from the OTHER
//! loopback origin (`localhost` for `127.0.0.1`), one of them with `Timing-Allow-Origin: *`.
//! `server-timing.chrome-150.txt` is Chrome's `window.__r`, two runs identical. A line that differs is a
//! finding about the engine, never a reason to edit the file.
use engine::Engine;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("fixtures/server-timing/server-timing.html");
const CHROME: &str = include_str!("fixtures/server-timing/server-timing.chrome-150.txt");

fn route(path: &str) -> Option<(&'static str, &'static str, Vec<&'static str>, bool)> {
    Some(match path {
        "/server-timing.html" => (
            PAGE,
            "text/html",
            vec![
                "cfL4;desc=\"?proto=TCP&rtt=13092&min_rtt=12801&cwnd=242\"",
                "cfExtPri, db;dur=53.2;desc=\"Database\", miss",
                "a;DUR=2;dur=3;desc=tok;desc=ignored, \"q\";dur=1",
            ],
            false,
        ),
        "/sub.js" => ("window.sub=1", "text/javascript", vec!["sub;dur=1.5;desc=same"], false),
        "/xo.js" => ("window.xo=1", "text/javascript", vec!["xo;dur=2"], false),
        "/xo-tao.js" => ("window.xotao=1", "text/javascript", vec!["xotao;dur=3;desc=\"tao\""], true),
        _ => return None,
    })
}

fn serve_one(mut s: std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    let resp = match route(&path) {
        Some((body, ty, timing, tao)) => {
            let mut h = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
            for t in timing {
                h.push_str(&format!("Server-Timing: {t}\r\n"));
            }
            if tao {
                h.push_str("Timing-Allow-Origin: *\r\n");
            }
            format!("{h}\r\n{body}")
        }
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    };
    let _ = s.write_all(resp.as_bytes());
}

#[tokio::test]
async fn the_timeline_exposes_server_timing_as_chromes_does() {
    let want: Vec<String> = serde_json::from_str(CHROME).expect("the recorded Chrome output is a JSON list");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve_one(s));
        }
    });
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // The page is a FRAME of a blank top document: a frame's document comes through the transport, as a
    // navigation does, so its entry carries its own response's timing (tests/performance_entries_live.rs).
    let top = "<!doctype html><html><body><iframe src=\"/server-timing.html\"></iframe></body></html>";
    e.load_page(top, &format!("http://127.0.0.1:{port}/top.html"), &[]).await.expect("load_page");
    let start = Instant::now();
    let mut got: Vec<String> = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        e.eval("read", "print('R:' + JSON.stringify(document.querySelector('iframe').contentWindow.__r || []))".to_string()).unwrap();
        if let Some(l) = e.take_output().into_iter().rev().find(|l| l.starts_with("R:")) {
            got = serde_json::from_str(&l[2..]).unwrap_or_default();
        }
        if got.last().is_some_and(|l| l == "done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(g, w, "line #{i}: korvus vs Chrome 150 (fixtures/server-timing/server-timing.chrome-150.txt)");
    }
    assert_eq!(got.len(), want.len(), "the page's lines");
}

//! A fetched classic script compiled off the page's thread (`core/html/script_stream.rs`) runs as the same script
//! compiled on it would. Live, on a loopback server.
//!
//! The byte order mark: V8's streaming UTF-8 decoder drops a leading BOM, and the script is finished with its text,
//! which keeps it -- every function the stream compiled sat one character off, and a lazily compiled one was parsed
//! from the wrong offset: `SyntaxError: Unexpected number` here, and on nzherald.co.nz V8's
//! `Check failed: start_position == start_position_from_data`, which killed the process. Stock Chrome 152 runs the
//! page below to `[602,42]`.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

fn chrome() -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")
}

/// Six hundred functions V8 compiles lazily, called after the script's top level ran, in a body well past the
/// streaming threshold.
fn script_body() -> String {
    let mut s = String::from("var res = [];\n");
    for i in 0..600 {
        s.push_str(&format!("function f{i}(a) {{ return a + {i}; }}\n"));
    }
    s.push_str("res.push(f1(1) + f599(1));\nfunction late(x) { return x * 2; }\nres.push(late(21));\n");
    s
}

fn serve(s: std::net::TcpStream, script: Vec<u8>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
        }
        let page = b"<!doctype html><body><script src=\"/s.js\"></script><script>window.__r = JSON.stringify(res);</script>";
        let (ctype, body): (&str, &[u8]) = match target.as_str() {
            "/" => ("text/html; charset=utf-8", page),
            "/s.js" => ("text/javascript", &script),
            _ => ("text/plain", b""),
        };
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n", body.len());
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(body)).is_err() {
            return;
        }
    }
}

async fn run(script: Vec<u8>) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let script = script.clone();
            std::thread::spawn(move || serve(s, script));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let mut e = engine::Engine::new(Box::new(chrome()));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
}

#[tokio::test]
async fn a_streamed_script_runs_its_lazy_functions() {
    assert_eq!(run(script_body().into_bytes()).await, "[602,42]");
}

#[tokio::test]
async fn a_script_that_starts_with_a_byte_order_mark_runs_as_in_chrome() {
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(script_body().as_bytes());
    assert_eq!(run(bytes).await, "[602,42]");
}

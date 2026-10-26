//! The editing commands, `navigator.sendBeacon`, DOMParser's type and `text-align`, answered as stock Chrome 150
//! answers them. Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this
//! machine with the probe beside it in `fixtures/editing/` and is replayed here in the persona's engine.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

fn compare(got: Vec<serde_json::Value>, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    compare(crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, probe).await, chrome)
}

/// One connection of the probe's server: `GET /` is the page running `probe`, anything else -- the beacons -- a
/// `200` with no body once its own body is read.
fn serve(s: std::net::TcpStream, probe: &'static str) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = r.read_exact(&mut body);
    if path == "/" {
        let page = format!(
            "<!doctype html><html><head><meta charset=utf-8></head><body><script>\n\
             Promise.resolve({probe}).then(function (r) {{ window.__r = r; }});\n</script></body></html>"
        );
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", page.len());
        let _ = w.write_all(format!("{head}{page}").as_bytes());
    } else {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

/// The probe's answers in a page a local server serves, its beacons answered `200` as the recording's server did.
async fn served_answers(probe: &'static str) -> Vec<serde_json::Value> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, probe));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "JSON.stringify(window.__r || null)".to_string()).expect("eval");
        let v: serde_json::Value = serde_json::from_str(v.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Array(list) = v {
            return list;
        }
    }
    panic!("the page never reported");
}

/// `queryCommandEnabled` / `State` / `Value` / `Indeterm` for every name Blink knows, in the states a page reaches:
/// a range and a caret in non-editable text, carets in a `contenteditable` host (bold, a list item, a heading, a
/// styled span), ranges across it, a focused textarea, no selection; what `execCommand` does that needs no editing
/// (`selectAll`, `unselect`, `styleWithCSS`, `useCSS`, `defaultParagraphSeparator`, `findString`); an XML document's
/// InvalidStateError and a frameless document's empty command. These were all false / "" here.
#[tokio::test]
async fn editing_commands_answer_from_the_selection_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/editing/commands-in-state-probe.js"), include_str!("fixtures/editing/commands-in-state-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// Every command name's five answers on a page with no selection and nothing editable (48 supported from the DOM),
/// `execCommand`'s on the same page, then sendBeacon's timeline entry (`beacon`, not `fetch`) and the reads it makes
/// of the page (none), and DOMParser's `SupportedType` (a TypeError in Blink's words for any other string).
#[tokio::test]
async fn default_commands_beacon_entries_and_parser_types_answer_as_chrome_150_does() {
    let got = served_answers(include_str!("fixtures/editing/commands-default-beacon-domparser-probe.js")).await;
    let d = compare(got, include_str!("fixtures/editing/commands-default-beacon-domparser-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `navigator.sendBeacon` for each URL and body form: the arguments converted first, an ill-formed or non-HTTP(S) URL
/// and a ReadableStream body TypeErrors in Blink's words, `false` past the 64 KiB keepalive quota, nothing of the
/// page read, and each queued beacon's `beacon` entry.
#[tokio::test]
async fn send_beacon_checks_queues_and_times_as_chrome_150_does() {
    let got = served_answers(include_str!("fixtures/editing/send-beacon-probe.js")).await;
    let d = compare(got, include_str!("fixtures/editing/send-beacon-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `text-align: match-parent` is no value in Chrome 150 (an `<li>` computes its parent's alignment, a page's own
/// declaration is dropped), and a `<th>` is `center` unless its parent aligns otherwise. They read `match-parent` and
/// `start` here.
#[tokio::test]
async fn text_align_computes_as_chrome_150_does() {
    let d = differences(include_str!("fixtures/editing/text-align-probe.js"), include_str!("fixtures/editing/text-align-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

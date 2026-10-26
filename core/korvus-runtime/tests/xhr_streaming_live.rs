//! An XMLHttpRequest's events as its response arrives, and IndexedDB's request queue, answered as stock Chrome 150
//! answers them. Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this
//! machine with the probe beside it in `fixtures/xhr-streaming/` and `fixtures/indexeddb/`, served by a local server
//! that trickles each response the way the probe's URL asks (`serve` below; the recording's was its Python twin), and
//! is replayed here in the persona's engine.
//!
//! a bot challenge's XHR `onreadystatechange` ran ten times over 370 ms in Chrome -- HEADERS_RECEIVED, then
//! LOADING once per 50 ms as its 120 KB body arrived -- and three times in one millisecond here, after the last byte.
//! Rows a probe records but these lists leave out, and why: where a new database's creation, a delete or a
//! `databases()` answer lands against page timers longer than 0 ms (Chrome: 0.5-9 ms, a different spot each run --
//! the engine models it as the latencies measured, `idb_factory.rs`); a `deleteDatabase` issued in the same task as
//! the `close()` of the connection that blocked it (Chrome answers it either way from run to run).

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

fn compare(got: Vec<serde_json::Value>, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// The named parts a probe's plan can ask for (anything else is that many bytes of `x`).
fn part(token: &str) -> Vec<u8> {
    let u8 = "한글".as_bytes();
    match token {
        "html" => [b"<html><body>".as_slice(), &[b'x'; 88]].concat(),
        "xmldecl" => [b"<?xml version='1.0'?>".as_slice(), &[b' '; 79]].concat(),
        "xmlroot" => [b"<?xml version='1.0'?><root>".as_slice(), &[b'x'; 73]].concat(),
        "bin" => [b"\x00".as_slice(), &[b'x'; 99]].concat(),
        "bom" => [b"\xEF\xBB\xBF".as_slice(), &[b'x'; 97]].concat(),
        "u8a" => u8[..4].to_vec(),
        "u8b" => u8[4..].to_vec(),
        "ja" => b"{\"a\":".to_vec(),
        "jb" => b"1}".to_vec(),
        "rnd" => (0..1000usize).map(|i| ((i * 7919 + (i * i) % 251) % 256) as u8).collect(),
        n => vec![b'x'; n.parse().unwrap_or(0)],
    }
}

/// `/p?ct=TYPE&cl=0|1&nosniff=1&gz=1&gz0=1&d=MS:PART,MS:PART,...`: the headers at once, then each part after its
/// delay -- with a `Content-Length`, or chunked (`cl=0`); `gz=1` sends the whole body gzip'd (stored with `gz0=1`)
/// in as many parts.
fn plan(w: &mut std::net::TcpStream, query: &str) {
    let q: std::collections::HashMap<&str, &str> = query.split('&').filter_map(|kv| kv.split_once('=')).collect();
    let mut parts: Vec<(u64, Vec<u8>)> = q
        .get("d")
        .map(|d| d.split(',').filter(|s| !s.is_empty()).map(|s| s.split_once(':').map(|(ms, t)| (ms.parse().unwrap_or(0), part(t))).unwrap()).collect())
        .unwrap_or_default();
    let cl = q.get("cl") != Some(&"0");
    let gz = q.get("gz") == Some(&"1");
    if gz {
        use std::io::Write as _;
        let level = if q.get("gz0") == Some(&"1") { flate2::Compression::none() } else { flate2::Compression::best() };
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), level);
        enc.write_all(&parts.iter().flat_map(|(_, b)| b.clone()).collect::<Vec<u8>>()).unwrap();
        let whole = enc.finish().unwrap();
        let step = whole.len().div_ceil(parts.len().max(1));
        parts = parts.iter().enumerate().map(|(i, (ms, _))| (*ms, whole[(i * step).min(whole.len())..((i + 1) * step).min(whole.len())].to_vec())).collect();
    }
    let mut head = String::from("HTTP/1.1 200 OK\r\n");
    if let Some(ct) = q.get("ct") {
        head += &format!("Content-Type: {ct}\r\n");
    }
    head += "Cache-Control: no-store\r\n";
    if q.get("nosniff") == Some(&"1") {
        head += "X-Content-Type-Options: nosniff\r\n";
    }
    if gz {
        head += "Content-Encoding: gzip\r\n";
    }
    if cl {
        head += &format!("Content-Length: {}\r\n", parts.iter().map(|(_, b)| b.len()).sum::<usize>());
    } else {
        head += "Transfer-Encoding: chunked\r\n";
    }
    head += "Connection: close\r\n\r\n";
    if w.write_all(head.as_bytes()).is_err() {
        return;
    }
    for (ms, b) in parts {
        std::thread::sleep(Duration::from_millis(ms));
        let out = if cl { b } else { [format!("{:x}\r\n", b.len()).into_bytes(), b, b"\r\n".to_vec()].concat() };
        if w.write_all(&out).is_err() {
            return;
        }
    }
    if !cl {
        let _ = w.write_all(b"0\r\n\r\n");
    }
}

/// One connection of the probes' server: `GET /` is the page running `probe`, `/frame` a bare document, `/p` a
/// planned response, `/nocontent` a 204.
fn serve(s: std::net::TcpStream, probe: &'static str) {
    let _ = s.set_nodelay(true);
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
            break;
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let page = |w: &mut std::net::TcpStream, body: &str| {
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = w.write_all(format!("{head}{body}").as_bytes());
    };
    match path {
        "/" => page(
            &mut w,
            &format!("<!doctype html><html><head><meta charset=utf-8></head><body><script>\nPromise.resolve({probe}).then(function (r) {{ window.__r = r; }});\n</script></body></html>"),
        ),
        "/frame" => page(&mut w, "<!doctype html><title>f</title>"),
        // a document whose markup is the `h` parameter (`frame_documents_live`'s frames, of this origin or of
        // `localhost`'s)
        "/doc" => {
            let h = url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "h").map(|(_, v)| v.into_owned()).unwrap_or_default();
            page(&mut w, &h);
        }
        "/nocontent" => {
            let _ = w.write_all(b"HTTP/1.1 204 No Content\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
        }
        "/p" => plan(&mut w, query),
        _ => {
            let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
}

/// The probe's answers in a page the local server serves (also `frame_documents_live`'s, for a frame of a real
/// origin).
pub(crate) async fn served_answers(probe: &'static str) -> Vec<serde_json::Value> {
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
    while start.elapsed() < Duration::from_secs(90) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "JSON.stringify(window.__r || null)".to_string()).expect("eval");
        let v: serde_json::Value = serde_json::from_str(v.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Array(list) = v {
            return list;
        }
    }
    panic!("the page never reported");
}

/// Every event of an XHR whose response arrives in parts 300 ms apart: HEADERS_RECEIVED and LOADING when the first
/// bytes are delivered, not when the headers arrive (a timer between the two reads `readyState 1`, `status 0`, no
/// headers); a `readystatechange(3)` + `progress` per delivery; `responseText` growing, without a character whose
/// bytes are still arriving; `response` null until DONE for the other types; a body sent inside 50 ms of the last
/// `progress` flushed in DONE; no `progress` for an empty body; `total` 0 for a `Content-Length: 0`, chunked or
/// gzip'd body; the network service holding a sniffable type's first bytes until it can decide (1024 for
/// `text/plain`, a tag for XML, an HTML tag for none, 8 for `application/octet-stream`); a timeout mid-body or
/// mid-sniff; and a body trickled for a second -- `progress` at most every 50 ms, each after the first preceded by a
/// `readystatechange`. Here the three transitions ran back to back after the last byte.
#[tokio::test]
async fn xhr_events_follow_the_body_as_it_arrives_as_in_chrome_150() {
    let got = served_answers(include_str!("fixtures/xhr-streaming/events-probe.js")).await;
    let d = compare(got, include_str!("fixtures/xhr-streaming/events-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// What a handler does mid-flight, as Blink's state machine answers it: `responseURL` readable once the head is in
/// (and not while a sniffed body is held); `open()` in the HEADERS_RECEIVED handler puts the bytes in the new
/// request's LOADING (and its `send()` throws); `abort()` there with an empty body fires a second `readystatechange`;
/// `open()` there fires DONE, `load` and `loadend` at the re-opened request; `abort()` in a later `progress`;
/// `open()`+`send()` in the LOADING handler gives the old request's empty `progress` and holds the new one's.
#[tokio::test]
async fn xhr_handlers_that_abort_or_reopen_mid_flight_get_chrome_150s_events() {
    let got = served_answers(include_str!("fixtures/xhr-streaming/handler-quirks-probe.js")).await;
    let d = compare(got, include_str!("fixtures/xhr-streaming/handler-quirks-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// IndexedDB's connection queue -- an `open` or `deleteDatabase` waits until every request before it on the same name
/// is processed -- and the backend's answers: `databases()` after a 0 ms timer, after the requests made before it, with
/// the COMMITTED state (no database whose first upgrade runs, the old version during an upgrade); an upgrade's
/// `complete` before the open's `success`; and a removed frame's connections closed as it goes, the delete they
/// blocked carried out at once and reported to nobody.
#[tokio::test]
async fn indexeddb_requests_queue_and_answer_as_chrome_150s_do() {
    let got = served_answers(include_str!("fixtures/indexeddb/connection-queue-probe.js")).await;
    let d = compare(got, include_str!("fixtures/indexeddb/connection-queue-chrome-150.json"));
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

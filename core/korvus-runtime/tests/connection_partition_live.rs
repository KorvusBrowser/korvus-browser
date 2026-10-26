//! WHICH CONNECTION EACH REQUEST RIDES, as stock Chrome 150 partitions them: a pooled
//! connection belongs to one privacy mode and one network anonymization key -- the top-level site, and whether the
//! frame that asked is cross-site to it (`transport::partition`). a bot-challenge frame's navigation rode
//! the page's `api.js` connection here (its navigation entry's `requestStart` 0.08 ms; Chrome's 433, a connection of
//! its own) and every request the challenge made after rode it too.
//!
//! The probe (`fixtures/connection-partition/record.py`, which recorded `chrome-150.json` from
//! `/usr/bin/google-chrome` 150.0.7871.46, three runs alike): a page on `127.0.0.1` and a cross-site frame on
//! `localhost` each load a third server twice with credentials (an `<img>`) and twice without (a CORS `fetch()`,
//! whose `same-origin` credentials are omitted for another origin), one after another; that server names the
//! connection each request arrived on by its client port. Chrome: four connections, one per (frame site, privacy
//! mode), each reused by its second request. Before: one connection for nearly all of them.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SEQ: &str = r#"
const B = 'http://127.0.0.1:%PB%/hit?tag=';
const img = (t) => new Promise((r) => { const i = new Image(); i.onload = i.onerror = () => r(); i.src = B + t; });
async function run(who) {
  await img(who + '-img1');
  await fetch(B + who + '-fetch1').then((r) => r.text()).catch(() => {});
  await img(who + '-img2');
  await fetch(B + who + '-fetch2').then((r) => r.text()).catch(() => {});
}
"#;

const GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

/// Serve one keep-alive connection: each request's target to `answer`, which gives `(content type, body)`.
fn keep_alive(s: std::net::TcpStream, answer: &(dyn Fn(&str, u16) -> (&'static str, Vec<u8>) + Send + Sync)) {
    let peer = s.peer_addr().map(|a| a.port()).unwrap_or(0);
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
        let (ct, body) = answer(&target, peer);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        if w.write_all(head.as_bytes()).and_then(|_| w.write_all(&body)).is_err() {
            return;
        }
    }
}

fn listen(answer: Arc<dyn Fn(&str, u16) -> (&'static str, Vec<u8>) + Send + Sync>) -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let a = answer.clone();
            std::thread::spawn(move || keep_alive(s, &*a));
        }
    });
    port
}

#[tokio::test]
async fn requests_ride_connections_partitioned_as_chrome_150s_are() {
    let hits: Arc<Mutex<Vec<(String, u16)>>> = Arc::default();
    let rec = hits.clone();
    let pb = listen(Arc::new(move |target: &str, peer: u16| {
        let tag = target.rsplit("tag=").next().unwrap_or("").to_string();
        let img = tag.ends_with("img1") || tag.ends_with("img2");
        rec.lock().unwrap().push((tag, peer));
        if img {
            ("image/gif", GIF.to_vec())
        } else {
            ("text/plain", b"ok".to_vec())
        }
    }));
    let pa_cell: Arc<Mutex<u16>> = Arc::default();
    let pa_read = pa_cell.clone();
    let pa = listen(Arc::new(move |target: &str, _| {
        let seq = SEQ.replace("%PB%", &pb.to_string());
        let pa = *pa_read.lock().unwrap();
        let body = match target {
            "/" => format!(
                "<!doctype html><body><script>{seq}run('page').then(() => new Promise((r) => {{ addEventListener('message', r); const f = document.createElement('iframe'); f.src = 'http://localhost:{pa}/frame'; document.body.appendChild(f); }})).then(() => {{ window.__r = 'done'; }});</script></body>"
            ),
            "/frame" => format!("<!doctype html><body><script>{seq}run('frame').then(() => parent.postMessage('ok', '*'));</script></body>"),
            _ => String::new(),
        };
        ("text/html", body.into_bytes())
    }));
    *pa_cell.lock().unwrap() = pa;

    let url = format!("http://127.0.0.1:{pa}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "the page never finished: {:?}", hits.lock().unwrap());
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r)".to_string()).expect("eval");
        if v.as_str() == Some("done") {
            break;
        }
    }
    // each connection named in order of first use, as the recorder names them
    let mut classes: Vec<u16> = Vec::new();
    let mut got = BTreeMap::new();
    for (tag, port) in hits.lock().unwrap().iter() {
        if !classes.contains(port) {
            classes.push(*port);
        }
        let n = classes.iter().position(|p| p == port).unwrap() + 1;
        got.insert(tag.clone(), format!("conn{n}"));
    }
    let want: BTreeMap<String, String> = serde_json::from_str(include_str!("fixtures/connection-partition/chrome-150.json")).unwrap();
    assert_eq!(got, want, "connections differ from Chrome 150's");
}

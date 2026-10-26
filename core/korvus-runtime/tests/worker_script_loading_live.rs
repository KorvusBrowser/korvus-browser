//! WHERE A WORKER'S SCRIPTS MAY COME FROM, AND WHEN AND AS WHOM THEY ARE FETCHED, as stock Chrome 150 does it
//! (ledger N23/N28, N11, N8). Three probes in `fixtures/worker-script-loading/`, each recorded by `record.sh` from
//! `/usr/bin/google-chrome` 150.0.7871.46 (headed, no DevTools, three runs alike) into `<probe>.chrome-150.json`:
//!
//! * `schemes.js` -- `importScripts()` of every scheme and response in a blob, a URL-loaded and a shared worker (and a
//!   module worker), and `new Worker` / `new SharedWorker` of the same from the page and from a worker. korvus sent every
//!   import to the HTTP transport, so a `blob:` or `data:` one threw "NetworkError: ... builder error: invalid
//!   authority" (on marriott.com a bot sensor sensor's worker lost 23 of its 120 signals); it ran a text/plain import
//!   Chrome refuses; it threw its own words for every failure; it named an import by its redirect's target and kept
//!   its fragment (Chrome: the URL asked for, fragment cut); it checked no constructor URL at all (a cross-origin
//!   script started a worker of the other origin) and no top-level script's type; a redirected worker kept the
//!   request's URL; a shared worker had a `Worker` (Chrome: `[Exposed=(Window,DedicatedWorker)]`). 133 of 186 rows
//!   differed (`korvus-b3`, the pre-fix build).
//! * `cache.js` -- a cross-site frame's worker: its script and its imports ride the frame's HTTP-cache partition and say
//!   they come from the frame (`Sec-Fetch-Site: same-origin`, the frame's `Referer`). korvus asked as the page, keyed
//!   them (frame site, frame site), and downloaded both again (a cross-site widget's worker script, twice more). The
//!   frame's own parser `<script src>` is the frame's request too: korvus sent it as the PAGE's (`cross-site`, the
//!   page's Referer, keyed (page site, page site)), so even keyed right the worker's import missed it.
//! * `order.js` -- a URL worker's script is requested AT `new Worker`, while the creating task still runs, and the
//!   worker's realm (its `performance.timeOrigin`) is made once the response is in. korvus requested it 175 ms later
//!   (after the 150 ms task and its realm build) and made the realm first. The probe's third column -- the first
//!   statement within 50 ms of the time origin -- is the realm build's cost, which this test does not judge (N8: a
//!   worker realm takes korvus ~11 ms to install, Chrome ~1 ms).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SCHEMES: &str = include_str!("fixtures/worker-script-loading/schemes.js");
const SCHEMES_CHROME: &str = include_str!("fixtures/worker-script-loading/schemes.chrome-150.json");
const CACHE: &str = include_str!("fixtures/worker-script-loading/cache.js");
const CACHE_CHROME: &str = include_str!("fixtures/worker-script-loading/cache.chrome-150.json");
const ORDER: &str = include_str!("fixtures/worker-script-loading/order.js");
const ORDER_CHROME: &str = include_str!("fixtures/worker-script-loading/order.chrome-150.json");

/// What the server saw: `host + target` -> each request's kept headers and arrival time (`srv.py`'s `/count`).
type Seen = Arc<Mutex<BTreeMap<String, Vec<serde_json::Value>>>>;

/// `srv.py`'s page: the probe inlined, its rows on `window.__report`.
fn page(probe: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8></head><body><script>\nvar __r;\ntry {{ __r = {probe}; }} catch (e) {{ __r = [[\"FATAL-OUTER\", String(e)]]; }}\n\
         Promise.resolve(__r).then(function (r) {{ window.__report = JSON.stringify(r); fetch('/report', {{method: 'POST', body: window.__report}}); }});\n</script></body></html>"
    )
}

/// `record.sh`'s normalisation: every `:` and 2-5 digits not followed by a word character becomes `:PORT`.
fn ports(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            let next = b.get(i + 1 + n).copied();
            if (2..=5).contains(&n) && !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                out.push_str(":PORT");
                i += 1 + n;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// One keep-alive connection, answered as `srv.py` answers.
fn serve(s: std::net::TcpStream, page: Arc<String>, seen: Seen) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
            if let Some((k, v)) = h.trim_end().split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        if method == "POST" {
            let n: usize = get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut body = vec![0u8; n];
            if r.read_exact(&mut body).is_err() || w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").is_err() {
                return;
            }
            continue;
        }
        let host = get("host").unwrap_or_default().split(':').next().unwrap_or("").to_string();
        let mut entry = serde_json::Map::new();
        for k in ["sec-fetch-dest", "sec-fetch-mode", "sec-fetch-site", "cache-control", "pragma", "referer", "origin"] {
            if let Some(v) = get(k) {
                entry.insert(k.to_string(), v.into());
            }
        }
        entry.insert("t".into(), now_ms().into());
        seen.lock().unwrap().entry(format!("{host}{target}")).or_default().push(entry.into());
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let q: BTreeMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        let mut status = 200u16;
        let mut hs: Vec<(String, String)> = Vec::new();
        let mut body: Vec<u8> = Vec::new();
        match path {
            "/" => {
                body = page.as_bytes().to_vec();
                hs = vec![("Content-Type".into(), "text/html; charset=utf-8".into()), ("Cache-Control".into(), "no-store".into())];
            }
            "/js" => {
                if let Some(d) = q.get("d").and_then(|d| d.parse::<u64>().ok()) {
                    std::thread::sleep(Duration::from_millis(d));
                }
                body = q.get("b").cloned().unwrap_or_default().into_bytes();
                status = q.get("st").and_then(|s| s.parse().ok()).unwrap_or(200);
                let ct = q.get("ct").cloned().unwrap_or_else(|| "text/javascript".into());
                if ct != "none" {
                    hs.push(("Content-Type".into(), ct));
                }
                if q.contains_key("xcto") {
                    hs.push(("X-Content-Type-Options".into(), "nosniff".into()));
                }
                hs.push(("Cache-Control".into(), q.get("cc").cloned().unwrap_or_else(|| "no-store".into())));
                if q.contains_key("acao") {
                    hs.push(("Access-Control-Allow-Origin".into(), "*".into()));
                }
            }
            "/redir" => {
                status = 302;
                hs = vec![("Location".into(), q.get("to").cloned().unwrap_or_else(|| "/".into())), ("Cache-Control".into(), "no-store".into())];
            }
            "/html" => {
                body = q.get("b").cloned().unwrap_or_default().into_bytes();
                hs = vec![("Content-Type".into(), "text/html; charset=utf-8".into()), ("Cache-Control".into(), "no-store".into())];
            }
            "/count" => {
                body = serde_json::to_vec(&*seen.lock().unwrap()).unwrap();
                hs = vec![("Content-Type".into(), "application/json".into()), ("Cache-Control".into(), "no-store".into()), ("Access-Control-Allow-Origin".into(), "*".into())];
            }
            _ => {
                status = 404;
                hs = vec![("Content-Type".into(), "text/plain".into()), ("Cache-Control".into(), "no-store".into())];
            }
        }
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            302 => "Found",
            404 => "Not Found",
            _ => "Internal Server Error",
        };
        let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
        for (k, v) in &hs {
            out.extend(format!("{k}: {v}\r\n").into_bytes());
        }
        out.extend(format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes());
        out.extend(body);
        if w.write_all(&out).is_err() {
            return;
        }
    }
}

/// Load `probe` on the server (127.0.0.1, and `localhost` as the cross-site origin) and return its rows, ports
/// normalised as `record.sh` writes Chrome's, and what the server saw.
async fn run(probe: &str) -> (serde_json::Value, Seen) {
    let seen: Seen = Arc::default();
    let page = Arc::new(page(probe));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (pg, sn) = (page.clone(), seen.clone());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (pg, sn) = (pg.clone(), sn.clone());
            std::thread::spawn(move || serve(s, pg, sn));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let rows = loop {
        assert!(start.elapsed() < Duration::from_secs(120), "the probe never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(&ports(s)).expect("the report");
        }
    };
    (rows, seen)
}

/// `[label, answer...]` rows by label.
fn by_label(rows: &serde_json::Value) -> BTreeMap<String, Vec<serde_json::Value>> {
    let mut out = BTreeMap::new();
    for r in rows.as_array().expect("rows") {
        let r = r.as_array().expect("a row");
        out.insert(r[0].as_str().unwrap_or("").to_string(), r[1..].to_vec());
    }
    out
}

/// Every label whose answers differ from Chrome's, both sides shown.
fn differences(chrome: &serde_json::Value, korvus: &serde_json::Value) -> Vec<String> {
    let (c, k) = (by_label(chrome), by_label(korvus));
    let mut out = Vec::new();
    for label in c.keys().chain(k.keys().filter(|l| !c.contains_key(*l))) {
        if c.get(label) != k.get(label) {
            out.push(format!("{label}\n    Chrome 150: {:?}\n    korvus:     {:?}", c.get(label), k.get(label)));
        }
    }
    out
}

#[tokio::test]
async fn worker_scripts_load_from_the_schemes_chrome_150_loads_them_from() {
    let chrome: serde_json::Value = serde_json::from_str(SCHEMES_CHROME).expect("the fixture");
    let (korvus, _) = run(SCHEMES).await;
    let diff = differences(&chrome, &korvus);
    assert!(diff.is_empty(), "{} of {} rows differ from Chrome 150's:\n{}", diff.len(), by_label(&chrome).len(), diff.join("\n"));
}

#[tokio::test]
async fn a_cross_site_frames_worker_fetches_as_the_frame_in_its_cache_partition() {
    let chrome: serde_json::Value = serde_json::from_str(CACHE_CHROME).expect("the fixture");
    let (korvus, _) = run(CACHE).await;
    let diff = differences(&chrome, &korvus);
    assert!(diff.is_empty(), "{} of {} rows differ from Chrome 150's:\n{}", diff.len(), by_label(&chrome).len(), diff.join("\n"));
}

/// A row's `request +N` / `origin +N` (ms after `new Worker`, rounded to 25 ms); `None` for `request none`.
fn ms(row: &[serde_json::Value], what: &str) -> Option<i64> {
    row.iter().filter_map(|v| v.as_str()).find_map(|s| s.strip_prefix(what)).and_then(|s| s.trim().strip_prefix('+')).and_then(|n| n.parse().ok())
}

#[tokio::test]
async fn a_url_workers_script_is_asked_for_at_construction_and_its_realm_made_when_it_arrives() {
    let chrome = by_label(&serde_json::from_str(ORDER_CHROME).expect("the fixture"));
    let (korvus, _) = run(ORDER).await;
    let korvus = by_label(&korvus);
    for (label, c) in &chrome {
        let k = korvus.get(label).unwrap_or_else(|| panic!("{label}: no row\n{korvus:?}"));
        // the request leaves while the 150 ms creating task runs (Chrome: +0; korvus asked at +175)
        assert_eq!(ms(k, "request").is_some(), ms(c, "request").is_some(), "{label}: {k:?} (Chrome 150: {c:?})");
        if let (Some(kr), Some(cr)) = (ms(k, "request"), ms(c, "request")) {
            assert!(kr <= cr + 25, "{label}: script requested at +{kr} ms, Chrome 150 at +{cr} -- inside the creating task\n{k:?}");
        }
        // the realm is made once the response (300 ms) is in and the task (150 ms) is over (Chrome: +300, +150 for a blob)
        let (ko, co) = (ms(k, "origin").expect("an origin"), ms(c, "origin").expect("an origin"));
        assert!(ko >= co - 25 && ko <= co + 50, "{label}: time origin +{ko} ms after new Worker, Chrome 150 +{co}\n{k:?}");
    }
}

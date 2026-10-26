//! When the origin private file system answers, as Chrome 150's storage service answers.
//! a bot challenge times its OPFS step -- `getDirectory()` in its cross-site frame, then a worker that opens
//! a file's sync access handle, writes and flushes -- and posts the time: Chrome's took 20 ms, korvus's 69. korvus
//! answered every operation a 1 ms task later and every `getDirectory()` 20 ms later, wrote and synced the file to
//! disk on the worker's thread inside `createSyncAccessHandle()`, rewrote and synced it on a `flush()` with nothing
//! new, and spent a task on each step of a directory listing.
//!
//! The fixture is Chrome's, `/usr/bin/google-chrome` 150.0.7871.46 headed under Xvfb, a fresh profile, three runs of
//! the probe in a cross-site frame. Times are not answers: no two runs agree to the tenth of a millisecond, so each
//! row is held to a bound every Chrome run met -- a floor where Chrome pays a cost (a storage key's opening, its
//! first operation), a ceiling where Chrome's is fast (the fastest of three tries: load only ever adds time; ten
//! operations in a row, where one answer's tenth of a millisecond is below what a loaded machine can resolve). The
//! ten sync access handles are the fastest of three tens too (2026-10-11, re-recorded: one ten under the whole
//! suite's load read 6.8 ms against Chrome's 0.7-0.9).

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const PROBE: &str = include_str!("fixtures/opfs-timing/frame-probe.js");
const CHROME: &str = include_str!("fixtures/opfs-timing/frame-chrome-150.json");

/// (row, floor, ceiling) in ms; every Chrome run meets each.
const BOUNDS: &[(&str, f64, f64)] = &[
    ("getDirectory", 3.0, 15.0),
    ("firstOperation", 3.0, 30.0),
    ("getDirectoryAgain", 0.0, 5.0),
    ("keysMin", 0.0, 1.5),
    ("tenLookupsMin", 0.0, 5.0),
    ("syncHandleMin", 0.0, 1.5),
    ("flushNothingNewMin", 0.0, 0.6),
    ("tenSyncHandlesMin", 0.0, 5.0),
];

fn serve(s: std::net::TcpStream, page: String, frame: String) {
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
        let body = if target == "/" { page.as_str() } else if target == "/f" { frame.as_str() } else { "" };
        let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

fn violations(got: &serde_json::Value) -> Vec<String> {
    BOUNDS
        .iter()
        .filter_map(|&(k, lo, hi)| match got.get(k).and_then(|v| v.as_f64()) {
            Some(v) if (lo..=hi).contains(&v) => None,
            v => Some(format!("{k} = {v:?}, Chrome 150's within {lo}..={hi} ms")),
        })
        .collect()
}

/// THE BOUNDS ARE CHROME'S: every recorded run meets every one.
#[test]
fn every_chrome_run_meets_the_bounds() {
    let runs: Vec<serde_json::Value> = serde_json::from_str(CHROME).unwrap();
    assert_eq!(runs.len(), 3);
    for r in &runs {
        let v = violations(r);
        assert!(v.is_empty(), "{v:?}");
    }
}

/// THE CHALLENGE'S OPFS STEP, TIMED AS CHROME'S: a cross-site frame's first `getDirectory()` and first operation pay
/// the opening of its storage key; a second `getDirectory()`, a listing, a lookup, a worker's sync access handle and
/// a flush with nothing new are each a fraction of a millisecond.
///
/// THE FASTEST OF THREE TRIES, as the bounds themselves are taken (module note: load only ever adds time). One try under
/// the gate's eight test threads read `getDirectory` 61.5 ms and a process's first try 1052.8 ms where it reads 3-15 alone
/// (ledger N48); each try is a fresh engine and a fresh frame origin (its own port, so its own storage key), and the
/// test passes when one try meets every Chrome bound. A korvus that answers outside them on every try still fails.
#[tokio::test]
async fn opfs_answers_when_chromes_storage_service_answers() {
    let mut tries = Vec::new();
    for _ in 0..3 {
        let got = one_try().await;
        let v = violations(&got);
        if v.is_empty() {
            return;
        }
        tries.push(format!("{v:?} (got {got})"));
    }
    panic!("OPFS timing outside Chrome 150's on all three tries: {tries:#?}");
}

/// One try: a page on 127.0.0.1 whose cross-site frame (`localhost`) runs the probe and posts its object back.
async fn one_try() -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let page = format!(
        "<!doctype html><body><script>addEventListener('message', e => {{ window.__r = JSON.stringify(e.data); }});\
         const f = document.createElement('iframe'); f.src = 'http://localhost:{port}/f'; document.body.appendChild(f);</script>"
    );
    let frame = format!("<!doctype html><script>{}.then(r => parent.postMessage(r, '*'));</script>", PROBE.trim().trim_end_matches(';'));
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (p, f) = (page.clone(), frame.clone());
            std::thread::spawn(move || serve(s, p, f));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the frame never reported");
        let _ = e.pump(Duration::from_millis(20)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str::<serde_json::Value>(s).expect("the frame's object");
        }
    }
}

/// A FILE MADE IS ON DISK WHEN THE ANSWER COMES: the storage service records a new file before `getFileHandle({create:
/// true})` answers, as Chrome's browser process does, so the worker's first `flush()` syncs the file's data only --
/// it paid the file's creation and the device waking (korvus 1.5-12.6 ms, Chrome 150 1.0-1.2 ms; the challenge's
/// worker times that flush). The process's backing directory gains the file before the promise settles.
#[tokio::test]
async fn a_made_file_is_on_disk_when_get_file_handle_answers() {
    let dir = std::env::temp_dir().join(format!("korvus-opfs-{}", std::process::id()));
    let listing = || std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.file_name()).collect::<std::collections::HashSet<_>>()).unwrap_or_default();
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page("<!doctype html><title>t</title>", "http://127.0.0.1:8124/made.html", &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    // the bucket is open first, so the directory exists and the listing before the creation is the right one
    let _ = e.eval_value_settled("root", "navigator.storage.getDirectory().then(() => 'open')".to_string(), Duration::from_secs(10)).await.expect("eval");
    let before = listing();
    let got = e
        .eval_value_settled(
            "made",
            "navigator.storage.getDirectory().then(r => r.getFileHandle('made-' + Math.random().toString(36).slice(2), {create: true})).then(() => 'made')".to_string(),
            Duration::from_secs(10),
        )
        .await
        .expect("eval");
    assert_eq!(got.as_str(), Some("made"));
    let after = listing();
    assert!(after.difference(&before).count() >= 1, "no backing file appeared before getFileHandle() answered: {before:?} -> {after:?}");
}

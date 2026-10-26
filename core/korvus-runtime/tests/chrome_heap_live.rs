//! `performance.memory` read as stock Chrome 150 reads it, and V8's heap sized as Chrome sizes it.
//! a bot-challenge frame reads the three numbers into its payload: Chrome's read 20 MB, this
//! engine's 65 MB -- its own Web API, built into the heap before any page script runs (an empty page read 10,035,014
//! bytes used here, 515,778 in Chrome), and the page's heap beside the frame's in the one isolate.
//!
//! Chrome's answers, from `/usr/bin/google-chrome` 150.0.7871.46 on this machine, a fresh profile, each page's first
//! script: an empty document reads 515,778 used and 1,052,410 total (two runs alike: the persona's
//! `memory.emptyDocument`); a page that holds a 4 MiB `Uint8Array` and then makes two frames -- its same-origin frame
//! reads 5,042,412 used, the page's renderer and so the page's heap, and its cross-site frame 515,778, an empty
//! document's, in a renderer of its own.
//!
//! Not exact, and not asserted as if it were: Chrome's own readings move with its collector, and this engine's isolate
//! is one for every realm. What is asserted is where each reading starts and whose allocations it counts.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use spec::Persona;

const MIB: f64 = 1024.0 * 1024.0;

fn chrome() -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")
}

/// The persona's empty document, `(total, used)`: what a reading starts from.
fn empty_document() -> (f64, f64) {
    chrome().memory().empty_document.expect("the Chrome 152 bundle measured an empty document's heap")
}

/// THE HEAP V8 IS GIVEN IS CHROME'S. Chrome makes every isolate with `ResourceConstraints::ConfigureDefaults(physical
/// memory, virtual memory)` (`gin::IsolateHolder::getDefaultIsolateParams`); this engine passed nothing, and V8 then
/// limits the heap to 1,568,669,696 bytes on any machine. Chrome's limit on a machine of 8 GB or more is V8's cap,
/// 4 GB of old generation and three 32 MB semi-spaces -- the `jsHeapSizeLimit` Chrome 150 reported on this machine,
/// which the persona carries.
#[test]
fn every_isolate_gets_chromes_heap() {
    let mut rt = engine::new_runtime(deno_core::RuntimeOptions::default());
    let limit = rt.v8_isolate().get_heap_statistics().heap_size_limit() as f64;
    let meminfo = std::fs::read_to_string("/proc/meminfo").expect("/proc/meminfo");
    let kb: f64 = meminfo.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse().ok()).expect("MemTotal");
    let physical = kb * 1024.0;
    if physical >= 8.0 * 1024.0 * MIB {
        assert_eq!(limit, chrome().memory().js_heap_size_limit, "the isolate's heap limit is not Chrome 150's on this machine");
    } else {
        // below 8 GB Chrome's old generation is half the machine's memory (V8 `OldGenerationSizeFromPhysicalMemory`)
        assert!(limit >= (physical / 2.0).max(256.0 * MIB), "the isolate's heap limit {limit} is under Chrome's for {physical} bytes");
    }
}

/// AN EMPTY DOCUMENT READS CHROME'S EMPTY DOCUMENT, plus what its own script has allocated: not the ~10 MB this engine
/// builds into the heap before the page runs.
#[tokio::test]
async fn an_empty_document_reads_chromes_empty_heap() {
    let (total0, used0) = empty_document();
    let mut e = engine::Engine::new(Box::new(chrome()));
    let url = "http://127.0.0.1/";
    e.set_url(url);
    let page = b"<!doctype html><html><head><meta charset=utf-8></head><body><script>\
        const m = performance.memory; window.__m = JSON.stringify([m.usedJSHeapSize, m.totalJSHeapSize]);\
        </script></body></html>";
    e.load_page_response(page, url, &[], &[]).await.expect("load");
    let got = e.eval_value("m", "window.__m".to_string()).expect("eval");
    let [used, total]: [f64; 2] = serde_json::from_str(got.as_str().expect("the first script's reading")).expect("two numbers");
    assert!(used >= used0 && used < used0 + 0.25 * MIB, "usedJSHeapSize {used}: Chrome's empty document reads {used0}");
    assert!(total >= total0 && total < total0 + MIB, "totalJSHeapSize {total}: Chrome's empty document reads {total0}");
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset=utf-8></head><body><script>
globalThis.__k = new Uint8Array(4 << 20).fill(1);
const got = {};
addEventListener('message', (e) => {
  const r = JSON.parse(e.data);
  got[r[0]] = r[1];
  if (Object.keys(got).length === 2) window.__r = JSON.stringify(got);
});
for (const src of ['/f?same', 'http://localhost:' + location.port + '/f?cross']) {
  const f = document.createElement('iframe');
  f.src = src;
  document.body.appendChild(f);
}
</script></body></html>"#;

const FRAME: &str = r#"<!doctype html><html><head><meta charset=utf-8></head><body><script>
parent.postMessage(JSON.stringify([location.search.slice(1), performance.memory.usedJSHeapSize]), '*');
</script></body></html>"#;

fn serve(s: std::net::TcpStream) {
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
        let body = if target == "/" {
            PAGE
        } else if target.starts_with("/f?") {
            FRAME
        } else {
            ""
        };
        let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// A FRAME READS ITS RENDERER'S HEAP. A same-site frame is the page's renderer in Chrome, and reads the page's heap --
/// the 4 MiB the page holds included; a cross-site frame is a renderer of its own, which started as an empty document
/// when the frame was made, and does not count the page's buffer. Before: each frame realm read from its own start,
/// so the same-site frame missed the page's 4 MiB as well.
#[tokio::test]
async fn a_frame_reads_its_renderers_heap() {
    let (_, used0) = empty_document();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let mut e = engine::Engine::new(Box::new(chrome()));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got: serde_json::Value = loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the frames never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(s).expect("the frames' readings");
        }
    };
    let same = got["same"].as_f64().expect("the same-site frame's reading");
    let cross = got["cross"].as_f64().expect("the cross-site frame's reading");
    assert!(same >= used0 + 4.0 * MIB, "the same-site frame reads {same}: the page's renderer holds 4 MiB more than {used0}");
    assert!(cross >= used0 && cross < used0 + 2.0 * MIB, "the cross-site frame reads {cross}: its renderer started at {used0} and holds none of the page's 4 MiB");
}

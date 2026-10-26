//! THE HTTP CACHE, at the wire, as stock Chrome 150 keeps it (`transport::http_cache`). This
//! engine had none, so every second fetch of a resource went to the network unconditional: on a bot challenge
//! page Chrome's second `/favicon.ico` went out with `if-modified-since` and came back `304` (its first answer said
//! `max-age=300` with `age: 3178`), where this engine downloaded it again.
//!
//! The probe (`fixtures/http-cache/record.py`, which recorded `chrome-150.json` from `/usr/bin/google-chrome`
//! 150.0.7871.46, three runs alike): a page fetches each of twelve resources twice -- fresh, stale on arrival with a
//! `Last-Modified`, `no-cache` with an `ETag`, `no-store`, `max-age=0` with both validators, heuristically fresh, no
//! caching headers, `Vary: Origin`, and the second fetch under `cache: no-cache / reload / force-cache / no-store` --
//! and reports the second answer's status, body, `transferSize` and `deliveryType`; the server counts the requests and
//! records the second one's validators and cache headers. Here, before: two plain requests for every one.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LM: &str = "Wed, 07 Oct 2026 12:10:03 GMT";

/// Each case's answer headers, as the recorder's server sends them.
fn case_headers(case: &str) -> Vec<(&'static str, &'static str)> {
    match case {
        "fresh" => vec![("Cache-Control", "max-age=300")],
        "stale_lm" => vec![("Cache-Control", "max-age=300"), ("Age", "3178"), ("Last-Modified", LM)],
        "nocache_etag" => vec![("Cache-Control", "no-cache"), ("ETag", "\"v1\"")],
        "nostore" => vec![("Cache-Control", "no-store"), ("ETag", "\"v1\"")],
        "maxage0_both" => vec![("Cache-Control", "max-age=0"), ("ETag", "\"v2\""), ("Last-Modified", LM)],
        "heuristic" => vec![("Last-Modified", "Wed, 01 Oct 2026 00:00:00 GMT")],
        "none" => vec![],
        "mode_nocache" => vec![("Cache-Control", "max-age=300"), ("ETag", "\"v3\"")],
        "mode_reload" => vec![("Cache-Control", "max-age=300"), ("ETag", "\"v4\"")],
        "mode_force" => vec![("Cache-Control", "max-age=0"), ("ETag", "\"v5\"")],
        "mode_nostore" => vec![("Cache-Control", "max-age=300")],
        "vary_origin" => vec![("Cache-Control", "max-age=300"), ("Vary", "Origin")],
        _ => vec![],
    }
}

const CASES: &[&str] = &[
    "fresh", "stale_lm", "nocache_etag", "nostore", "maxage0_both", "heuristic", "none", "mode_nocache", "mode_reload", "mode_force", "mode_nostore",
    "vary_origin",
];

const PAGE: &str = r#"<!doctype html><body><script>
(async () => {
  const R = {};
  const cases = %CASES%; const modes = {"mode_nocache": "no-cache", "mode_reload": "reload", "mode_force": "force-cache", "mode_nostore": "no-store"};
  for (const c of cases) {
    const u = '/r/' + c;
    await fetch(u).then((r) => r.text());
    const init = modes[c] ? { cache: modes[c] } : {};
    const r2 = await fetch(u, init);
    const b = await r2.text();
    await new Promise((r) => setTimeout(r, 30));
    const es = performance.getEntriesByName(location.origin + u);
    const e = es[es.length - 1];
    R[c] = [r2.status, b, e ? e.transferSize : null, e ? e.deliveryType : null, es.length];
  }
  window.__r = JSON.stringify(R);
})();
</script></body>"#;

type Log = Arc<Mutex<Vec<(String, [Option<String>; 4])>>>;

fn serve(s: std::net::TcpStream, log: Log) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
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
        let mut out = Vec::new();
        if target == "/" {
            let list = format!("[{}]", CASES.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(","));
            let b = PAGE.replace("%CASES%", &list);
            out.extend(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{b}", b.len()).into_bytes());
        } else if let Some(case) = target.strip_prefix("/r/") {
            log.lock().unwrap().push((case.to_string(), [get("if-none-match"), get("if-modified-since"), get("cache-control"), get("pragma")]));
            let hs = case_headers(case);
            let etag = hs.iter().find(|(k, _)| *k == "ETag").map(|(_, v)| *v);
            let lm = hs.iter().find(|(k, _)| *k == "Last-Modified").map(|(_, v)| *v);
            let not_modified = match etag {
                Some(e) => get("if-none-match").as_deref() == Some(e),
                None => lm.is_some() && get("if-modified-since").as_deref() == lm,
            };
            let extra: String = hs.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            if not_modified {
                out.extend(format!("HTTP/1.1 304 Not Modified\r\n{extra}Content-Length: 0\r\n\r\n").into_bytes());
            } else {
                let b = format!("body-{case}");
                out.extend(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n{extra}Content-Length: {}\r\n\r\n{b}", b.len()).into_bytes());
            }
        } else {
            out.extend(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
        if w.write_all(&out).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn the_http_cache_answers_at_the_wire_as_chrome_150s_does() {
    let log: Log = Arc::default();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let lg = log.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let lg = lg.clone();
            std::thread::spawn(move || serve(s, lg));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let page: serde_json::Value = loop {
        assert!(start.elapsed() < Duration::from_secs(60), "the page never finished");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(s).expect("the page's report");
        }
    };
    let log = log.lock().unwrap().clone();
    let mut got = BTreeMap::new();
    for c in CASES {
        let reqs: Vec<&[Option<String>; 4]> = log.iter().filter(|(k, _)| k == c).map(|(_, v)| v).collect();
        got.insert(
            c.to_string(),
            serde_json::json!({"page": page[*c], "requests": reqs.len(), "second": reqs.get(1).map(|v| serde_json::json!(v))}),
        );
    }
    let want: BTreeMap<String, serde_json::Value> = serde_json::from_str(include_str!("fixtures/http-cache/chrome-150.json")).unwrap();
    let diffs: Vec<String> = want.iter().filter(|(k, v)| got.get(*k) != Some(*v)).map(|(k, v)| format!("{k}: korvus {}\n    Chrome {v}", got[k])).collect();
    assert!(diffs.is_empty(), "{} cases differ from Chrome 150:\n{}", diffs.len(), diffs.join("\n"));
}

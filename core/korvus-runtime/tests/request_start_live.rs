//! A REQUEST LEAVES WHEN THE PAGE ASKS, NOT WHEN ITS TASK ENDS. A page starts a `fetch()`, an
//! XMLHttpRequest, a script, a stylesheet, a frame and an image, then stays busy for 200 ms; the server answers each
//! after 50 ms. Stock Chrome 150.0.7871.46 (headed under Xvfb, three runs): each resource entry's `fetchStart` is
//! 0-4 ms after the first call and its `responseStart` 52-61 ms -- the network service had the request at once and
//! the answer while the task still ran -- and the page's callbacks run as the task ends; the IMAGE alone is asked
//! for at 201-204 ms, because "update the image data" waits for a microtask checkpoint, which comes when the script
//! is done. korvus's request ops were polled on the event loop's next turn: `fetchStart` 201 ms for every one,
//! `responseStart` 253-260. (The test reads times from the busy stretch's start, past the calls themselves, which
//! a debug build makes slow.)
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><title>start</title><script>
var busy = function (ms) { var t = performance.now(); while (performance.now() - t < ms); };
onload = function () { setTimeout(function () {
  var t0 = performance.now();
  fetch('/x?fetch').then(function (r) { return r.text(); }).then(function () { window.f_done = performance.now() - t0; });
  var im = new Image(); im.src = '/x?img'; document.body.appendChild(im);
  var sc = document.createElement('script'); sc.src = '/x?script'; document.head.appendChild(sc);
  var ln = document.createElement('link'); ln.rel = 'stylesheet'; ln.href = '/x?css'; document.head.appendChild(ln);
  var fr = document.createElement('iframe'); fr.src = '/x?frame'; document.body.appendChild(fr);
  var x = new XMLHttpRequest(); x.open('GET', '/x?xhr'); x.onload = function () { window.x_done = performance.now() - t0; }; x.send();
  var tb = performance.now();
  busy(200);
  setTimeout(function () {
    var r = {};
    performance.getEntriesByType('resource').forEach(function (e) {
      if (e.name.indexOf('/x?') < 0) return;
      r[e.name.split('?')[1]] = [e.fetchStart - tb, e.responseStart - tb];
    });
    r.done = [window.f_done - (tb - t0), window.x_done - (tb - t0)];
    window.__report = JSON.stringify(r);
  }, 400);
}, 50); };
</script>"#;

fn serve(s: std::net::TcpStream) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
        }
        let (body, ty) = if path.starts_with("/x?") {
            std::thread::sleep(Duration::from_millis(50));
            match &path[3..] {
                "script" => ("var ok = 1;", "text/javascript"),
                "css" => ("p {}", "text/css"),
                "frame" => ("<!doctype html><p>f", "text/html"),
                _ => ("ok", "text/plain"),
            }
        } else if path == "/" {
            (PAGE, "text/html")
        } else {
            ("", "text/plain")
        };
        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_request_leaves_when_the_page_asks_as_in_chrome_150() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got: serde_json::Value = loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(s).expect("the report");
        }
    };
    // Times from the start of the 200 ms busy stretch that follows the calls. Chrome 150: every request but the
    // image has left before it (`fetchStart` at or below 0) and is answered during it; korvus's left after it (200).
    for k in ["fetch", "xhr", "script", "css", "frame"] {
        let row = got.get(k).and_then(|r| r.as_array()).unwrap_or_else(|| panic!("no resource entry for {k}: {got}"));
        let (fetch_start, response_start) = (row[0].as_f64().unwrap(), row[1].as_f64().unwrap());
        assert!(fetch_start < 50.0, "{k}: fetchStart {fetch_start} ms into the busy task -- the request waited for the task to end\n{got}");
        assert!(response_start < 150.0, "{k}: responseStart {response_start} ms into the busy task (a 50 ms server)\n{got}");
    }
    // the image is asked for at the microtask checkpoint after the script (Chrome 150: as the task ends)
    let img = got.get("img").and_then(|r| r.as_array()).unwrap_or_else(|| panic!("no resource entry for the image: {got}"));
    let img_start = img[0].as_f64().unwrap();
    assert!(img_start >= 195.0, "the image: fetchStart {img_start} ms into the busy task, before its script was done\n{got}");
    // the page's callbacks still wait for its task: no answer settles inside the call or the task that asked
    let done = got["done"].as_array().expect("the callbacks' times");
    for d in done {
        assert!(d.as_f64().is_some_and(|d| d >= 200.0), "a callback ran inside the 200 ms task: {got}");
    }
}

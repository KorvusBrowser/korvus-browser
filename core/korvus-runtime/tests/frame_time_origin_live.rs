//! A FRAME DOCUMENT'S TIME ORIGIN IS ITS NAVIGATION'S START -- the moment the navigation was initiated, not the
//! moment its response arrived. Stock Chrome 150.0.7871.46, headed under Xvfb, three runs:
//! each frame's `performance.timeOrigin` is 0.1-1.3 ms after the page inserted its element (or set its `src`)
//! -- a same-origin and a cross-site frame whose server answers after 50 ms, a `srcdoc` frame, a frame left on its
//! initial `about:blank`, and a frame's second navigation alike -- and its `performance.timing.navigationStart` is
//! that origin to the integer millisecond.
//!
//! korvus dated a fetched frame's document from the request it sent on the event loop's next turn (13-56 ms after
//! the insertion), and a `srcdoc` frame's from after its realm's build (7-8 ms).
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><title>origin</title><script>
var got = {}, want = 6;
function done(k, v) { got[k] = v; if (Object.keys(got).length === want) window.__report = JSON.stringify(got); }
addEventListener('message', function (e) {
  var k = e.data[0], o = e.data[1], ns = e.data[2], ins = window['ins_' + k];
  done(k, [o - (performance.timeOrigin + ins), ns - o]);
});
var REPORT = "parent.postMessage([KEY, performance.timeOrigin, performance.timing.navigationStart], '*')";
function frame(k, set) { var f = document.createElement('iframe'); set(f); window['ins_' + k] = performance.now(); document.body.appendChild(f); return f; }
onload = function () {
  frame('same', function (f) { f.src = '/f?same'; });
  frame('cross', function (f) { f.src = 'http://localhost:PORT/f?cross'; });
  frame('srcdoc', function (f) { f.srcdoc = '<script>' + REPORT.replace('KEY', '"srcdoc"') + '<\/script>'; });
  var b = frame('blank', function (f) {});
  b.contentWindow.eval(REPORT.replace('KEY', '"blank"'));
  frame('same2', function (f) { f.src = '/f?same2'; });
  var n2 = frame('renav', function (f) { f.src = '/f?first'; });
  n2.onload = function () { n2.onload = null; window.ins_renav = performance.now(); n2.src = '/f?renav'; };
};
</script>"#;

const FRAME: &str = "<!doctype html><script>var k = location.search.slice(1); \
    if (k !== 'first') parent.postMessage([k, performance.timeOrigin, performance.timing.navigationStart], '*');</script>";

fn serve(s: std::net::TcpStream, port: u16) {
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
        let body = if path == "/" {
            PAGE.replace("PORT", &port.to_string())
        } else if path.starts_with("/f?") {
            // a server that takes its time: the response arrives 50 ms after the request
            if path != "/f?first" {
                std::thread::sleep(Duration::from_millis(50));
            }
            FRAME.to_string()
        } else {
            String::new()
        };
        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_frames_time_origin_is_its_navigations_start_as_in_chrome_150() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, port));
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
        assert!(start.elapsed() < Duration::from_secs(30), "the frames never all reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(s).expect("the report");
        }
    };
    for k in ["same", "cross", "srcdoc", "blank", "same2", "renav"] {
        let row = got.get(k).and_then(|r| r.as_array()).unwrap_or_else(|| panic!("no report for {k}: {got}"));
        let (after_insert, nav_start) = (row[0].as_f64().unwrap(), row[1].as_f64().unwrap());
        // Chrome 150: 0.1-1.3 ms (once 3.7); korvus was 7-56
        assert!((-0.5..5.0).contains(&after_insert), "{k}: timeOrigin {after_insert} ms after the navigation began (Chrome 150: 0.1-1.3)\n{got}");
        // `navigationStart` is that origin floored to the millisecond (Chrome 150: -0.8..1.0 when rounded to 0.1)
        assert!((-1.5..1.5).contains(&nav_start), "{k}: navigationStart {nav_start} ms from timeOrigin\n{got}");
    }
}

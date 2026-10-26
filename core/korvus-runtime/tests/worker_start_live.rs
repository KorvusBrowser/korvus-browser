//! A WORKER BEGINS WHEN ITS CREATOR'S TASK IS OVER, AND IS NAMED AS ITS OPTIONS SAY. Stock
//! Chrome 150.0.7871.46 (headed under Xvfb, three runs alike): a `blob:` and a URL worker made at the head of a
//! 200 ms task both have `performance.timeOrigin` 201 ms after `new Worker` -- the worker's global scope is made,
//! and its script fetched, once that task has ended -- and `self.name` is `WorkerOptions.name`. korvus's worker
//! thread made its realm at once (1 ms after `new Worker`), and every dedicated worker's `name` was "".
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><title>ws</title><script>
var busy = function (ms) { var t = performance.now(); while (performance.now() - t < ms); };
onload = function () { setTimeout(function () {
  var got = {};
  var src = "postMessage([self.name, performance.timeOrigin])";
  var t0 = performance.now();
  var wb = new Worker(URL.createObjectURL(new Blob([src])), {name: 'blob'});
  var wu = new Worker('/w.js', {name: 'url'});
  var wn = new Worker(URL.createObjectURL(new Blob([src])));
  [['blob', wb], ['url', wu], ['unnamed', wn]].forEach(function (kw) {
    kw[1].onmessage = function (e) {
      got[kw[0]] = [e.data[0], e.data[1] - (performance.timeOrigin + t0)];
      if (Object.keys(got).length === 3) window.__report = JSON.stringify(got);
    };
  });
  busy(200);
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
        let (body, ty) = if path.starts_with("/w.js") {
            ("postMessage([self.name, performance.timeOrigin])", "text/javascript")
        } else {
            (PAGE, "text/html")
        };
        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_worker_begins_after_its_creators_task_as_in_chrome_150() {
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
        assert!(start.elapsed() < Duration::from_secs(30), "the workers never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break serde_json::from_str(s).expect("the report");
        }
    };
    for (k, name) in [("blob", "blob"), ("url", "url"), ("unnamed", "")] {
        let row = got.get(k).and_then(|r| r.as_array()).unwrap_or_else(|| panic!("no report for {k}: {got}"));
        assert_eq!(row[0].as_str(), Some(name), "{k}: self.name (Chrome 150: WorkerOptions.name)\n{got}");
        let after = row[1].as_f64().unwrap();
        // Chrome 150: 201 ms -- the worker's realm is made once the 200 ms task that created it is over
        assert!(after >= 195.0, "{k}: timeOrigin {after} ms after new Worker, inside the creating task (Chrome 150: 201)\n{got}");
    }
}

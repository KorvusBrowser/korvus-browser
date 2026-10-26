//! Paint holding, paint timing and a page's first animation frame as stock Chrome 150 keeps them
//! -- what a page's timeline holds while its body draws nothing, then a box, then text.
//!
//! Chrome defers an outermost main frame's compositor commits once its lifecycle updates begin, until it paints
//! contentful content or 500 ms pass (`LocalFrameView::BeginLifecycleUpdates`): its rendering updates run, nothing
//! is presented, so neither a `paint` entry nor a rendered `long-animation-frame` reaches the timeline meanwhile.
//! `first-paint` is the first update that DRAWS something (a box's border or background, text, an image; the
//! default document background aside), `first-contentful-paint` the first that draws text or an image. And the
//! page's first frame runs from the navigation's first task -- before the response -- to its first rendering
//! update, so a page whose response took 300 ms has a long one.
//!
//! a bot challenge page draws nothing until its challenge does (its only markup is a `<noscript>`): when
//! the challenge first reads the timeline, Chrome's holds `navigation, visibility-state, resource,
//! long-animation-frame`; this engine's held `navigation, visibility-state, resource, paint, paint` -- it recorded
//! both paints at the first update of any page with a body, called an inline script's source contentful text, and
//! started the first frame at the response.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-09**: the probe
//! `fixtures/an interactive-widget challenge_live/paint-hold-probe.js` in a page served 300 ms late, its body empty
//! (`paint-hold-empty-chrome-150.json`) or the challenge page's markup (`paint-hold-cf-chrome-150.json`).
use engine::Engine;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";
const PROBE: &str = include_str!("fixtures/challenge_probes/paint-hold-probe.js");
/// The challenge page's body as a challenge provider serves it (recorded 2026-10-08, nih.gov): nothing but a `<noscript>`.
const CF_BODY: &str = "<div class=main-wrapper role=main><div class=main-content><noscript><div class=h2><span>Enable JavaScript and cookies to continue</span></div></noscript></div></div>";

/// The `<select>` pages of [`a_select_paints_as_chrome_150_paints_it`], as the probe served them to Chrome 150.
const SELECT_CASES: &[(&str, &str)] = &[
    ("A", "<select><option></option><option></option></select>"),
    ("B", "<select><option>abc</option></select>"),
    ("C", "<select multiple><option></option></select>"),
    ("D", "<select multiple><option>abc</option></select>"),
    ("E", "<select size=3><option>abc</option></select>"),
    ("F", "<select><option></option></select><script>document.querySelector(\"select\").options.length = 100000;</script>"),
    ("G", "<select style=\"border:none;background:transparent\"><option></option></select>"),
    ("H", "<select><optgroup label=\"grp\"><option></option></optgroup></select>"),
    ("I", "<div style=\"display:none\">hidden text</div><select><option>x</option></select>"),
    ("J", "<datalist><option>abc</option></datalist>"),
];

fn serve(s: std::net::TcpStream) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
            break;
        }
    }
    // A `<select>` page (`/select/X`), served at once: its paint entries once they settle after `load` -- the first
    // contentful paint, or a second after `load` with anything painted, or four seconds with nothing.
    if let Some(case) = path.strip_prefix("/select/") {
        let Some(markup) = SELECT_CASES.iter().find(|(c, _)| *c == case).map(|(_, m)| *m) else {
            let _ = w.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        };
        let page = format!(
            "<!doctype html><html><head><meta charset=utf-8></head><body>{markup}<script>addEventListener('load', function () {{ \
             var t0 = performance.now(); (function poll() {{ var n = performance.getEntriesByType('paint').map(function (e) {{ return e.name; }}); \
             var dt = performance.now() - t0; if ((n.length && (n.indexOf('first-contentful-paint') >= 0 || dt >= 1000)) || dt >= 4000) {{ \
             window.__r = n; return; }} setTimeout(poll, 100); }})(); }});</script></body></html>"
        );
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", page.len());
        let _ = w.write_all(format!("{head}{page}").as_bytes());
        return;
    }
    let body = match path.as_str() {
        "/empty" => "",
        "/cf" => CF_BODY,
        _ => {
            let _ = w.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    };
    // the response is 300 ms late: the page's first frame waits it out
    std::thread::sleep(Duration::from_millis(300));
    let page = format!(
        "<!doctype html><html><head><meta charset=utf-8></head><body>{body}<script>\
         Promise.resolve({PROBE}).then(function (r) {{ window.__r = r; }});</script></body></html>"
    );
    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", page.len());
    let _ = w.write_all(format!("{head}{page}").as_bytes());
}

fn server() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    port
}

/// Navigate to `url` as `korvus fetch` does and return the page's `__r`.
async fn report(url: &str) -> serde_json::Value {
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    e.set_url(url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.to_string()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        // `pump`: the event loop's rendering steps -- the paint timing among them -- run only there
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "JSON.stringify(window.__r || null)".to_string()).expect("eval");
        let v: serde_json::Value = serde_json::from_str(v.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        if !v.is_null() {
            return v;
        }
    }
    panic!("{url}: the page never reported");
}

async fn check(path: &str, recorded: &str) {
    let port = server();
    let got = report(&format!("http://127.0.0.1:{port}{path}")).await;
    let want: serde_json::Value = serde_json::from_str(recorded).expect("the recorded Chrome list");
    assert_eq!(got, want, "[moment, the timeline's entry types (paint entries by name)] on {path}");
}

#[tokio::test]
async fn a_page_that_draws_nothing_holds_its_paint_and_its_first_frame_as_chrome_150_does() {
    check("/empty", include_str!("fixtures/challenge_probes/paint-hold-empty-chrome-150.json")).await;
}

#[tokio::test]
async fn the_challenge_page_holds_its_paint_and_its_first_frame_as_chrome_150_does() {
    check("/cf", include_str!("fixtures/challenge_probes/paint-hold-cf-chrome-150.json")).await;
}

/// A `<select>` page's paint entries (recorded from Chrome 150 on this machine, 2026-10-09,
/// `fixtures/an interactive-widget challenge_live/select-paint-chrome-150.json`): a drop-down draws its box and arrow -- `first-paint` even
/// borderless and transparent -- and is contentful only by its selected option's label; a list box by its options'
/// text; a `<datalist>` draws nothing. The paint walk re-walked a document that drew nothing at every rendering
/// opportunity: a drop-down of 100 000 empty options (WPT `html/select/options-length-too-large.html`) never let its
/// page's timers or load event run.
#[tokio::test]
async fn a_select_paints_as_chrome_150_paints_it() {
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/challenge_probes/select-paint-chrome-150.json")).expect("the recorded Chrome entries");
    let port = server();
    let mut differ = Vec::new();
    for (case, _) in SELECT_CASES {
        let got = report(&format!("http://127.0.0.1:{port}/select/{case}")).await;
        if got != want[*case] {
            differ.push(format!("{case}: korvus {got}, Chrome {}", want[*case]));
        }
    }
    assert!(differ.is_empty(), "paint entries differ from Chrome 150:\n{}", differ.join("\n"));
}

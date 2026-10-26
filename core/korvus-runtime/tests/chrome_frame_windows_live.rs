//! A frame's window as stock Chrome 150 keeps it. Found by the pinned-session replay -- one
//! recorded challenge served to Chrome and to this engine with Math.random seeded alike, so the two runs could be read
//! side by side call for call:
//!
//! * the WebRTC host candidates' mDNS name is the DOCUMENT's: two connections of one document share it, and every
//!   other document -- a same-site frame, each of two frames of another site -- has its own. The challenge reads its
//!   page's candidates and its frame's; here both carried one name, the process's.
//! * `window[i]` on a FRAME's window: a frame whose document holds frames answers `w[0] ===` its first frame's window,
//!   `'0' in w`, and lists `"0"` in `Object.keys(w)` -- whether the parent's script or the frame's own made the inner
//!   frame. Here only the page's window held its frames; a frame's counted them (`length`) and held none.
//! * a cross-origin-isolated FRAME's `SharedArrayBuffer` is the LAST of its window's own names, as on the page --
//!   V8 installs the per-context constructor once the embedder's global is built. Here a frame kept it where
//!   V8's bootstrap leaves it, 61st of 1,225, and the challenge frame posts its window's names in order.
//! * the named properties object answers a named element's descriptor NOT enumerable: Window is
//!   [LegacyUnenumerableNamedProperties]. Here it was enumerable.
//!
//! The fixtures are Chrome's, `/usr/bin/google-chrome` 150.0.7871.46 headed under Xvfb, a fresh profile, three runs
//! alike, from these pages (the page posted the object; here it is left on `window.__r`).

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const NAME: &str = r#"
function hostName() {
  return new Promise(res => {
    const pc = new RTCPeerConnection();
    pc.createDataChannel('x');
    pc.onicecandidate = e => {
      if (!e.candidate) return;
      const m = / ([0-9a-f-]{36}\.local) [0-9]+ typ host /.exec(e.candidate.candidate);
      res(m ? m[1] : 'other:' + e.candidate.candidate.replace(/[0-9]+/g, 'N')); pc.close();
    };
    pc.createOffer().then(o => pc.setLocalDescription(o));
    setTimeout(() => res('timeout'), 4000);
  });
}"#;

const MDNS_PAGE: &str = r#"
const got = {};
addEventListener('message', e => { got[e.data.who] = e.data.name; done(); });
(async () => {
  got.page1 = await hostName();
  got.page2 = await hostName();
  for (const [who, src] of [['same', '/f?same'], ['crossA', 'http://localhost:PORT/f?crossA'], ['crossB', 'http://localhost:PORT/f?crossB']]) {
    const f = document.createElement('iframe'); f.src = src; document.body.appendChild(f);
  }
  done();
})();
let sent = false;
function done() {
  if (sent || !['page1', 'page2', 'same', 'crossA', 'crossB'].every(k => k in got)) return;
  sent = true;
  const fmt = v => /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.local$/.test(v) ? 'uuid4.local' : v;
  window.__r = JSON.stringify({
    format: [...new Set(Object.values(got).map(fmt))],
    page1_eq_page2: got.page1 === got.page2,
    same_eq_page: got.same === got.page1,
    crossA_eq_page: got.crossA === got.page1,
    crossB_eq_page: got.crossB === got.page1,
    crossA_eq_crossB: got.crossA === got.crossB,
  });
}"#;

const MDNS_FRAME: &str = r#"
hostName().then(n => parent.postMessage({who: location.search.slice(1), name: n}, '*'));"#;

const INDEX_PAGE: &str = r#"
const r = {};
const a = document.createElement('iframe'); document.body.appendChild(a);
const b = document.createElement('iframe'); document.body.appendChild(b);
// a: its child made by THIS (the parent's) script; b: its child made by b's own script
const ga = a.contentDocument.createElement('iframe'); a.contentDocument.body.appendChild(ga);
b.contentWindow.eval("document.body.appendChild(document.createElement('iframe'))");
for (const [n, f] of [['a', a], ['b', b]]) {
  const w = f.contentWindow;
  r[n] = {length: w.length, has0: Object.prototype.hasOwnProperty.call(w, '0'), zeroIsChild: w[0] === w.document.querySelector('iframe').contentWindow,
          keys0: Object.keys(w).includes('0'), desc: JSON.stringify(Object.getOwnPropertyDescriptor(w, '0') ? Object.keys(Object.getOwnPropertyDescriptor(w, '0')) : null),
          enumerable: (Object.getOwnPropertyDescriptor(w, '0') || {}).enumerable, frames: w.frames.length, inSelf: '0' in w};
}
r.top = {length: window.length, keys: Object.keys(window).filter(k => /^\d+$/.test(k))};
window.__r = JSON.stringify(r);"#;


const NAMED_PAGE: &str = r#"<!doctype html><body>
<div id="addEventListener"></div><div id="foo"></div><div id="length"></div><div id="toString"></div><div id="document"></div>
<div id="alert"></div><div id="kQOwR1"></div><form name="fooForm"></form><img name="imgName">
<div id="valueOf"></div><div id="hasOwnProperty"></div><div id="onload"></div><div id="name"></div><div id="status"></div><div id="close"></div>
<script>
const r = {};
for (const n of ['addEventListener', 'foo', 'length', 'toString', 'document', 'alert', 'kQOwR1', 'fooForm', 'imgName', 'valueOf', 'hasOwnProperty', 'onload', 'name', 'status', 'close', 'nonexistentX']) {
  const v = window[n];
  r[n] = [typeof v, v instanceof Element ? v.tagName : (String(v).slice(0, 30)), n in window, Object.prototype.hasOwnProperty.call(window, n)];
}
const wp = Object.getPrototypeOf(Window.prototype);
r.wpOwn = Object.getOwnPropertyNames(wp).sort().join(',');
r.wpDesc = JSON.stringify(Object.getOwnPropertyDescriptor(wp, 'foo'));
r.globalThisFoo = typeof globalThis.foo;
const d = document.createElement('div'); d.id = 'lateId'; document.body.appendChild(d);
r.late = typeof window.lateId; d.remove(); r.lateAfterRemove = typeof window.lateId;
const fi = []; for (const k in window) fi.push(k);
r.forinIds = fi.filter(k => ['foo', 'kQOwR1', 'fooForm', 'imgName', 'addEventListener', 'document', 'valueOf', 'lateId'].includes(k)).join(',');
r.forinLen = fi.length;
r.keysIds = Object.keys(window).filter(k => ['foo', 'kQOwR1', 'fooForm', 'imgName'].includes(k)).join(',');
r.gopnIds = Object.getOwnPropertyNames(window).filter(k => ['foo', 'kQOwR1', 'fooForm', 'imgName'].includes(k)).join(',');
r.wpKeys = Object.keys(Object.getPrototypeOf(Window.prototype)).join(',');
r.wpGopn = Object.getOwnPropertyNames(Object.getPrototypeOf(Window.prototype)).join(',');
r.wpForin = (() => { const a = []; for (const k in Object.getPrototypeOf(Window.prototype)) a.push(k); return a.slice(0, 12).join(','); })();
window.__r = JSON.stringify(r);
</script>"#;

const SAB_READ: &str = r#"function where(w) { const g = Object.getOwnPropertyNames(w); const i = g.indexOf('SharedArrayBuffer'); return {len: g.length, at: i, before: g.slice(Math.max(0, i - 3), i), after: g.slice(i + 1, i + 6), last: g.slice(-6), coi: w.crossOriginIsolated, keysAt: Object.keys(w).indexOf('SharedArrayBuffer'), desc: JSON.stringify(Object.getOwnPropertyDescriptor(w, 'SharedArrayBuffer') ? Object.keys(Object.getOwnPropertyDescriptor(w, 'SharedArrayBuffer')).concat([Object.getOwnPropertyDescriptor(w, 'SharedArrayBuffer').enumerable]) : null)}; }"#;

const SAB_PAGE: &str = r#"
const r = {page: where(window)};
const f = document.createElement('iframe'); f.allow = 'cross-origin-isolated'; f.src = 'http://localhost:PORT/f'; document.body.appendChild(f);
addEventListener('message', e => { r.frame = e.data; window.__r = JSON.stringify(r); });"#;

const ISOLATED: &str = "Cross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: cross-origin\r\n";

fn chrome() -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")
}

fn serve(s: std::net::TcpStream, page: String, frame: String, headers: &'static str) {
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
            page.as_str()
        } else if target == "/f" || target.starts_with("/f?") {
            frame.as_str()
        } else {
            ""
        };
        let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// Serve `page` (and `frame` at `/f` and `/f?..`, `PORT` written into the page), load it, and read the object it leaves on
/// `window.__r`.
async fn run(page: &str, frame: &str) -> serde_json::Value {
    let page = format!("<!doctype html><body><script>{NAME}{page}</script>");
    let frame = format!("<!doctype html><script>{NAME}{frame}</script>");
    run_html(&page, &frame, "").await
}

/// [`run`] with whole documents, every response carrying `headers` (CRLF-terminated lines).
async fn run_html(page: &str, frame: &str, headers: &'static str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let page = page.replace("PORT", &port.to_string());
    let frame = frame.to_string();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (p, f) = (page.clone(), frame.clone());
            std::thread::spawn(move || serve(s, p, f, headers));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let mut e = engine::Engine::new(Box::new(chrome()));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return serde_json::from_str(s).expect("the page's object");
        }
    }
}

/// THE mDNS NAME IS THE DOCUMENT'S: two connections of the page share one; the same-site frame and each of the two
/// frames of another site has its own.
#[tokio::test]
async fn each_document_names_its_host_candidates_as_chromes_does() {
    let got = run(MDNS_PAGE, MDNS_FRAME).await;
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/frame-windows/mdns-names-chrome-150.json")).unwrap();
    assert_eq!(got, want, "which documents' host candidates share an mDNS name");
}

/// A FRAME'S WINDOW HOLDS ITS OWN FRAMES as `window[i]`, as the page's does.
#[tokio::test]
async fn a_frames_window_holds_its_own_frames_as_chromes_does() {
    let got = run(INDEX_PAGE, "").await;
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/frame-windows/frame-index-chrome-150.json")).unwrap();
    assert_eq!(got, want, "a frame window's window[0] and its key list");
}

/// THE NAMED PROPERTIES OBJECT as Chrome's: an element's id and a form's or image's name resolve on the window
/// (`foo`, `kQOwR1`, `fooForm`, `imgName`), never over a member the chain already has (`addEventListener`,
/// `toString`, `alert`), never as the window's own; and the descriptor it gives is NOT enumerable.
#[tokio::test]
async fn named_elements_resolve_on_the_window_as_chromes_do() {
    let got = run_html(NAMED_PAGE, "", "").await;
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/frame-windows/named-access-chrome-150.json")).unwrap();
    assert_eq!(got, want, "named access on the window");
}

/// AN ISOLATED FRAME'S `SharedArrayBuffer` IS ITS LAST OWN NAME, as the page's is: a COOP + COEP page and its
/// cross-site frame (delegated `cross-origin-isolated`) each list it after every Web API member.
#[tokio::test]
async fn an_isolated_frames_shared_array_buffer_is_last_as_in_chrome() {
    let page = format!("<!doctype html><body><script>{SAB_READ}{SAB_PAGE}</script>");
    let frame = format!("<!doctype html><script>{SAB_READ}parent.postMessage(where(window), \"*\");</script>");
    let got = run_html(&page, &frame, ISOLATED).await;
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/frame-windows/sab-order-chrome-152.json")).unwrap();
    assert_eq!(got, want, "where SharedArrayBuffer sits among an isolated page's and frame's own names");
}

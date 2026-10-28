//! The document a navigation ends on -- asked of Chrome, answered here over CDP.
//!
//! Every expectation below is stock Chrome 150.0.7871.46's answer to the same fixture over loopback
//! (`google-chrome --headless=new`, attached the way `tools/parity/driver.py` attaches: `Page.enable`,
//! `Runtime.enable`, `Network.enable`, `Page.navigate`, then events for a few seconds, then one
//! `Runtime.evaluate`). The fixtures are served from 127.0.0.1 by this file; nothing leaves the box.
//!
//! THE EVENT SEQUENCES are Chrome's with three things left out, each on purpose:
//! * `Page.lifecycleEvent` -- Chrome sends none unless `Page.setLifecycleEventsEnabled`, which the driver
//!   never sends; this server always has, and that is not this unit's question.
//! * `Network.requestWillBeSent` for anything but a `Document` -- Chrome's are `favicon.ico`, which a
//!   browser UI asks for and a page does not.
//! * the per-hop `requestWillBeSent` with `redirectResponse` -- graded by `redirect_hops.rs` (N27),
//! not here.
//!
//! A navigation is compared as a sequence of short labels: `FSN` frameStartedNavigating, `FSL`
//! frameStartedLoading, `REQ` a document request, `ECC` executionContextsCleared, `FN` frameNavigated,
//! `CTX` executionContextCreated, `DCL` domContentEventFired, `LOAD` loadEventFired, `STOP`
//! frameStoppedLoading, `SCHED` frameScheduledNavigation, `ASK` frameRequestedNavigation, `CLR`
//! frameClearedScheduledNavigation. Each carries the file name and the loader (L0, L1 in order of
//! appearance) where Chrome's does.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const TARGET: &str = "<!doctype html><html><head><title>landed</title></head><body><p>arrived at target</p>\n<script>document.body.insertAdjacentHTML('beforeend','<p id=log>'+(localStorage.getItem('log')||'')+'</p>');localStorage.removeItem('log')</script></body></html>";

/// Every page that navigates away logs its own lifecycle into `localStorage`, and the target prints the
/// log: that is how the order of `beforeunload`/`pagehide`/`visibilitychange`/`unload` -- which run on a
/// document nobody can evaluate in any more -- reaches the probe.
const LOGGER: &str = "<script>
function L(s){localStorage.setItem('log',(localStorage.getItem('log')||'')+s+' ')}
addEventListener('beforeunload',()=>L('beforeunload'));
addEventListener('pagehide',e=>L('pagehide:'+e.persisted));
addEventListener('unload',()=>L('unload'));
document.addEventListener('visibilitychange',()=>L('vis:'+document.visibilityState));
addEventListener('load',()=>L('load'));
addEventListener('DOMContentLoaded',()=>L('dcl'));
</script>";

fn page(title: &str, head: &str, body: &str) -> Vec<u8> {
    format!("<!doctype html><html><head><title>{title}</title>{head}{LOGGER}</head><body><p>{title} body</p>{body}</body></html>").into_bytes()
}

fn enc(s: &str, e: &'static encoding_rs::Encoding) -> Vec<u8> {
    e.encode(s).0.into_owned()
}

struct Fixture {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

const HTML_UTF8: &str = "text/html; charset=utf-8";

fn fixture(path: &str, other_port: u16) -> Option<Fixture> {
    use encoding_rs::{EUC_KR, GBK, SHIFT_JIS, WINDOWS_1252};
    let ok = |ct: &str, body: Vec<u8>| Some(Fixture { status: 200, headers: vec![("Content-Type", ct.to_string())], body });
    let html = |body: Vec<u8>| ok(HTML_UTF8, body);
    let bare = |body: Vec<u8>| ok("text/html", body);
    let redirect = |status: u16, to: String| Some(Fixture { status, headers: vec![("Location", to), ("Content-Type", "text/html".into())], body: vec![] });
    match path {
        "/target.html" => html(TARGET.as_bytes().to_vec()),
        "/order.html" => html(format!("{ORDER_LOG}<script async src=slow-a.js></script><script defer src=d.js></script><script>log('inline-end')</script>").into_bytes()),
        "/d.js" => ok("text/javascript", b"log('defer')".to_vec()),
        "/slow-a.js" => ok("text/javascript", b"log('async-slow')".to_vec()),
        "/quiet.html" => html(b"<!doctype html><title>quiet</title><p>nothing scheduled</p>".to_vec()),
        "/leave-a.html" => html(LEAVE_A.as_bytes().to_vec()),
        "/posts.html" => html(POSTS.as_bytes().to_vec()),
        "/leave-b.html" => html(b"<!doctype html><title>leave-b</title><script>console.log('b-loaded')</script>".to_vec()),
        "/idletimer.html" => html(b"<!doctype html><title>start</title><script>setTimeout(()=>{document.title='fired'},300)</script>".to_vec()),
        "/mr0.html" => html(page("mr0", r#"<meta http-equiv=refresh content="0; url=target.html">"#, "")),
        "/mr1.html" => html(page("mr1", r#"<meta http-equiv=refresh content="1; url=target.html">"#, "")),
        "/mrbody.html" => html(page("mrbody", "", r#"<meta http-equiv=refresh content="0;URL='target.html'">"#)),
        "/mrjunk.html" => html(page("mrjunk", r#"<meta http-equiv=refresh content="x; url=target.html">"#, "")),
        "/refreshhdr.html" => Some(Fixture {
            status: 200,
            headers: vec![("Content-Type", HTML_UTF8.into()), ("Refresh", "0; url=target.html".into())],
            body: b"<!doctype html><title>rh</title><p>refresh header</p>".to_vec(),
        }),
        "/href.html" => html(page("href", "", "<script>location.href='target.html'</script><p>after</p>")),
        "/replace.html" => html(page("replace", "", "<script>location.replace('target.html')</script>")),
        "/assignto.html" => html(page("assignto", "", "<script>setTimeout(()=>location.assign('target.html'),50)</script>")),
        "/wloc.html" => html(page("wloc", "", "<script>window.location='target.html'</script>")),
        "/onload.html" => html(page("onload", "", "<script>onload=()=>{location.href='target.html'}</script>")),
        "/r301.html" => redirect(301, format!("http://127.0.0.1:{other_port}/target.html")),
        "/r302chain.html" => redirect(302, "/r301.html".into()),
        "/rbase.html" => redirect(302, "/sub/based.html".into()),
        "/sub/based.html" => html(b"<!doctype html><title>based</title><a id=a href=x.html>x</a><script>document.title=[location.href,document.URL,document.baseURI,document.getElementById('a').href].join('|')</script>".to_vec()),
        "/euckr.html" => bare(enc(r#"<!doctype html><html><head><meta http-equiv="Content-Type" content="text/html; charset=EUC-KR"><title>문서</title></head><body><p>문서 모듈</p></body></html>"#, EUC_KR)),
        "/latin1.html" => bare(enc("<!doctype html><html><head><title>pt</title></head><body><p>português</p></body></html>", WINDOWS_1252)),
        "/metacharset.html" => bare(enc(r#"<!doctype html><meta charset="iso-8859-1"><title>mc</title><p>português</p>"#, WINDOWS_1252)),
        "/hdrsjis.html" => ok("text/html; charset=Shift_JIS", enc("<!doctype html><title>sj</title><p>日本語テキスト</p>", SHIFT_JIS)),
        "/hdrwins.html" => ok("text/html; charset=gbk", enc(r#"<!doctype html><meta charset="utf-8"><title>gw</title><p>中文字符</p>"#, GBK)),
        "/bomwins.html" => {
            let mut b = b"\xef\xbb\xbf".to_vec();
            b.extend("<!doctype html><title>bw</title><p>português 한</p>".as_bytes());
            ok("text/html; charset=windows-1252", b)
        }
        "/utf16.html" => {
            let mut b = b"\xff\xfe".to_vec();
            for u in "<!doctype html><title>u16</title><p>한국어</p>".encode_utf16() {
                b.extend(u.to_le_bytes());
            }
            bare(b)
        }
        "/latemeta.html" => {
            let mut b = format!("<!doctype html><title>lm</title><!--{}--><meta charset=\"euc-kr\"><p>", "x".repeat(1100)).into_bytes();
            b.extend(enc("문서", EUC_KR));
            b.extend(b"</p>");
            bare(b)
        }
        "/bodylate.html" => {
            let mut b = format!("<!doctype html><title>bl</title><body><!--{}--><meta charset=\"euc-kr\"><p>", "x".repeat(1100)).into_bytes();
            b.extend(enc("문서", EUC_KR));
            b.extend(b"</p>");
            bare(b)
        }
        "/scriptlate.html" => {
            let mut b = format!("<!doctype html><head><title>sl</title><script>var a=\"{}\"</script><meta charset=\"euc-kr\"></head><p>", "x".repeat(3000)).into_bytes();
            b.extend(enc("문서", EUC_KR));
            b.extend(b"</p>");
            bare(b)
        }
        "/bodyearly.html" => {
            let mut b = b"<!doctype html><title>be</title><body><p>hi</p><meta charset=\"euc-kr\"><p>".to_vec();
            b.extend(enc("문서", EUC_KR));
            b.extend(b"</p>");
            bare(b)
        }
        "/ascii.html" => bare(b"<!doctype html><title>as</title><p>plain ascii</p>".to_vec()),
        "/utf8latin.html" => bare("<!doctype html><title>ul</title><p>português café</p>".as_bytes().to_vec()),
        "/utf8nometa.html" => bare("<!doctype html><title>u8</title><p>한국어 português</p>".as_bytes().to_vec()),
        "/euckrnometa.html" => {
            let mut b = b"<!doctype html><title>en</title><p>".to_vec();
            b.extend(enc("대한민국 문서 모듈 한국어 텍스트입니다", EUC_KR));
            b.extend(b"</p>");
            bare(b)
        }
        "/sjisnometa.html" => {
            let mut b = b"<!doctype html><title>sn</title><p>".to_vec();
            b.extend(enc("日本語のテキストです。これはテストです。", SHIFT_JIS));
            b.extend(b"</p>");
            bare(b)
        }
        "/bogushdr.html" => {
            let mut b = b"<!doctype html><meta charset=\"euc-kr\"><title>bh</title><p>".to_vec();
            b.extend(enc("문서", EUC_KR));
            b.extend(b"</p>");
            ok("text/html; charset=bogus", b)
        }
        "/metautf16.html" => bare(r#"<!doctype html><meta charset="utf-16le"><title>m16</title><p>한국어</p>"#.as_bytes().to_vec()),
        "/hdrutf8latin1.html" => ok("text/html; charset=utf-8", enc("<!doctype html><title>hl</title><p>português</p>", WINDOWS_1252)),
        "/httpequivsjis.html" => bare(enc(r#"<!doctype html><head><title>hq</title><meta http-equiv="Content-Type" content="text/html; charset=shift_jis"></head><p>日本語</p>"#, SHIFT_JIS)),
        p if p.starts_with("/p13c-") => html(p13c_fixture(p)?),
        _ => None,
    }
}

// ── `window.open(url, target)` and form submission ──────────────────────────────────
//
// Every expectation in this half is stock Chrome 150.0.7871.46's answer to the same fixture over
// loopback, driven exactly as the P12d half above is. `/p13c-echo` answers with the request it
// received written into its body, which is how a POST's method, `Content-Type` and BODY reach the
// probe: there is no other way to see a request body from inside the page it landed on.

/// A page whose `<title>` is `src` and whose body is `body`, with the unload logger of the P12d half.
fn src_page(body: &str) -> Vec<u8> {
    page("src", "", body)
}

const P13C_FORM: &str = r#"<form id=f action="p13c-echo" method=get><input name=q value=1><input name=r value="a b"></form>"#;

/// Run `js` from the page's `load` handler.
fn on_load(js: &str) -> String {
    format!("<script>onload=function(){{{js}}}</script>")
}

fn p13c_fixture(path: &str) -> Option<Vec<u8>> {
    let b = match path {
        "/p13c-landed.html" => page("landed2", "", ""),
        "/p13c-blank.html" => page("blank", "", ""),
        // -- window.open(url, target)
        "/p13c-open-self.html" => src_page("<script>window.open('p13c-landed.html','_self')</script>"),
        "/p13c-open-self-onload.html" => src_page(&on_load("window.open('p13c-landed.html','_self')")),
        "/p13c-open-top.html" => src_page("<script>window.open('p13c-landed.html','_top')</script>"),
        "/p13c-open-parent.html" => src_page("<script>window.open('p13c-landed.html','_parent')</script>"),
        "/p13c-open-frame.html" => src_page(&format!(
            "<iframe name=frame1 src=p13c-blank.html></iframe>{}",
            on_load("var w=window.open('p13c-landed.html','frame1');document.title='src w='+(w===window?'self':'obj')")
        )),
        // -- the form submission itself
        "/p13c-form-get.html" => src_page(&format!("{P13C_FORM}{}", on_load("document.getElementById('f').submit()"))),
        "/p13c-form-parse.html" => {
            src_page(&format!("{P13C_FORM}<script>document.getElementById('f').submit()</script><p>after</p>"))
        }
        "/p13c-form-post.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=post><input name=q value=1><input name=r value="a b"></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-form-multipart.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=post enctype="multipart/form-data"><input name=q value=1><input type=file name=fi></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-form-textplain.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=post enctype="text/plain"><input name=q value=1><input name=r value="a b"></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-form-requestsubmit.html" => {
            src_page(&format!("{P13C_FORM}{}", on_load("document.getElementById('f').requestSubmit()")))
        }
        "/p13c-form-click.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get><input name=q value=1><button type=submit name=b value=bv>go</button></form>{}"#,
            on_load("document.querySelector('button').click()")
        )),
        "/p13c-form-formaction.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get><input name=q value=1><button type=submit formaction="p13c-echo2" formmethod=post name=b value=bv>go</button></form>{}"#,
            on_load("document.querySelector('button').click()")
        )),
        // -- where it goes
        "/p13c-form-empty-action.html" => src_page(&format!(
            r#"<form id=f action="" method=get><input name=q value=1></form>{}"#,
            on_load("if(!location.search){document.getElementById('f').submit()}")
        )),
        "/p13c-form-fragment.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo?keep=1#h" method=get><input name=q value=1></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-form-blank.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get target=_blank><input name=q value=1></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-frame-top.html" => page("parent", "", "<iframe src=p13c-inner-top.html></iframe>"),
        "/p13c-inner-top.html" => page(
            "inner",
            "",
            &format!(
                r#"<form id=f action="p13c-echo" method=get target=_top><input name=q value=1></form>{}"#,
                on_load("document.getElementById('f').submit()")
            ),
        ),
        "/p13c-frame-self.html" => page("parent", "", "<iframe src=p13c-inner-self.html></iframe>"),
        "/p13c-inner-self.html" => page(
            "inner",
            "",
            &format!(
                r#"<form id=f action="p13c-echo" method=get><input name=q value=1></form>{}"#,
                on_load("document.getElementById('f').submit()")
            ),
        ),
        "/p13c-form-named-frame.html" => src_page(&format!(
            r#"<iframe name=frame1 src=p13c-blank.html></iframe><form id=f action="p13c-echo" method=get target=frame1><input name=q value=1></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        // -- the events (target=_blank, so Chrome's popup blocker leaves the page where it is and the
        //    events are readable from it)
        "/p13c-events-requestsubmit.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get target=_blank><input name=q value=1><input name=r value="a b"><button type=submit name=b value=bv>go</button></form>{}"#,
            on_load(
                "var ev=[];var f=document.getElementById('f');                 f.addEventListener('submit',function(e){ev.push('submit c='+e.cancelable+' b='+e.bubbles+' s='+(e.submitter===null?'null':e.submitter.tagName)+' t='+(e.target===f))});                 f.addEventListener('formdata',function(e){ev.push('formdata c='+e.cancelable+' b='+e.bubbles+' k='+[...e.formData.entries()].map(function(x){return x[0]+'='+x[1]}).join(','))});                 f.requestSubmit();document.title='['+ev.join('] [')+']'"
            )
        )),
        "/p13c-events-click.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get target=_blank><input name=q value=1><button type=submit name=b value=bv>go</button></form>{}"#,
            on_load(
                "var ev=[];var f=document.getElementById('f');                 f.addEventListener('submit',function(e){ev.push('submit s='+e.submitter.tagName+'/'+e.submitter.value)});                 f.addEventListener('formdata',function(e){ev.push('formdata k='+[...e.formData.keys()].join(','))});                 document.querySelector('button').click();document.title='['+ev.join('] [')+']'"
            )
        )),
        "/p13c-events-submit.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get target=_blank><input name=q value=1></form>{}"#,
            on_load(
                "var ev=[];var f=document.getElementById('f');                 f.addEventListener('submit',function(){ev.push('submit')});                 f.addEventListener('formdata',function(){ev.push('formdata')});                 f.submit();document.title='['+ev.join('] [')+']'"
            )
        )),
        "/p13c-events-prevent.html" => src_page(&format!(
            "{P13C_FORM}{}",
            on_load(
                "var f=document.getElementById('f');                 f.addEventListener('submit',function(e){e.preventDefault()});f.requestSubmit();document.title='prevented'"
            )
        )),
        "/p13c-events-prevent-submit.html" => src_page(&format!(
            "{P13C_FORM}{}",
            on_load(
                "var got=0;var f=document.getElementById('f');                 f.addEventListener('submit',function(e){got=1;e.preventDefault()});f.submit();document.title='got='+got"
            )
        )),
        "/p13c-form-dialog.html" => src_page(&format!(
            r#"<dialog id=d open><form id=f method=dialog><input name=q value=1><button type=submit value=bv>go</button></form></dialog>{}"#,
            on_load(
                "document.querySelector('button').click();                 document.title='open='+document.getElementById('d').open+' rv='+document.getElementById('d').returnValue"
            )
        )),
        // -- constraint validation
        "/p13c-required-submit.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get><input name=q required value=""></form>{}"#,
            on_load("document.getElementById('f').submit()")
        )),
        "/p13c-required-requestsubmit.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get><input id=i name=q required value=""></form>{}"#,
            on_load(
                "var iv=0;document.getElementById('i').addEventListener('invalid',function(){iv++});                 document.getElementById('f').requestSubmit();document.title='invalid='+iv"
            )
        )),
        "/p13c-required-click.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get><input id=i name=q required value=""><button type=submit>go</button></form>{}"#,
            on_load(
                "var iv=0;document.getElementById('i').addEventListener('invalid',function(){iv++});                 document.querySelector('button').click();document.title='invalid='+iv"
            )
        )),
        "/p13c-novalidate.html" => src_page(&format!(
            r#"<form id=f action="p13c-echo" method=get novalidate><input name=q required value=""></form>{}"#,
            on_load("document.getElementById('f').requestSubmit()")
        )),
        _ => return None,
    };
    Some(b)
}

/// The `/p13c-echo` response: a page titled `landed` whose body is the request it answered, written
/// `METHOD|query|content-type|body`. A multipart boundary is replaced with `B` so the body is a
/// fixed string -- the boundary itself is random by construction and is asserted separately.
fn p13c_echo(method: &str, query: &str, content_type: &str, body: &str) -> Vec<u8> {
    let mut ct = content_type.to_string();
    let mut b = body.to_string();
    if let Some(boundary) = content_type.split("boundary=").nth(1) {
        ct = format!("multipart/form-data; boundary={}", boundary_shape(boundary));
        b = b.replace(boundary, "B");
    }
    let line = format!("{method}|{query}|{ct}|{}", b.replace('\r', "").replace('&', "&amp;").replace('<', "&lt;"));
    page("landed", "", &format!("<pre id=e>{line}</pre>"))
}

/// `----WebKitFormBoundary` and 16 characters of Chromium's alphabet, as a SHAPE: the 16 are random,
/// so what is asserted is that they are 16 of that alphabet.
fn boundary_shape(b: &str) -> String {
    const PREFIX: &str = "----WebKitFormBoundary";
    let tail = b.strip_prefix(PREFIX).unwrap_or("");
    let ok = tail.len() == 16 && tail.bytes().all(|c| c.is_ascii_alphanumeric());
    if ok { format!("{PREFIX}<16>") } else { b.to_string() }
}

/// Serve the fixture table on `listener`, one request per connection. `other` is the second origin's
/// port, which the cross-origin redirect points at.
async fn serve_fixtures(listener: TcpListener, other: u16) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p.to_string(), q.to_string()),
                None => (target.clone(), String::new()),
            };
            let method = head.split_whitespace().next().unwrap_or("GET").to_string();
            if path.starts_with("/slow") {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            // A form submission's POST has a BODY, and reading it is the only way its
            // bytes reach the page the request lands on. Read exactly `Content-Length` of it.
            let header = |name: &str| {
                head.lines()
                    .find(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            };
            let want = header("content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let head_len = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
            let mut body = buf[head_len.min(buf.len())..].to_vec();
            while body.len() < want {
                match s.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                }
            }
            let echo = path.starts_with("/p13c-echo").then(|| Fixture {
                status: 200,
                headers: vec![("Content-Type", HTML_UTF8.to_string())],
                body: p13c_echo(&method, &query, &header("content-type").unwrap_or_default(), &String::from_utf8_lossy(&body)),
            });
            let resp = match echo.or_else(|| fixture(&path, other)) {
                Some(f) => {
                    let reason = match f.status { 200 => "OK", 301 => "Moved Permanently", 302 => "Found", _ => "X" };
                    let mut h = format!("HTTP/1.1 {} {reason}\r\n", f.status);
                    for (k, v) in &f.headers {
                        h.push_str(&format!("{k}: {v}\r\n"));
                    }
                    h.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", f.body.len()));
                    let mut out = h.into_bytes();
                    out.extend(f.body);
                    out
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            };
            let _ = s.write_all(&resp).await;
            let _ = s.flush().await;
        });
    }
}

struct Client {
    ws: ClientWs,
    next: i64,
    events: Vec<(Instant, Value)>,
}

impl Client {
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next()).await.expect("an answer within 30 s").unwrap().unwrap();
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    return v.get("result").cloned().unwrap_or(Value::Null);
                }
                if v.get("method").is_some() {
                    self.events.push((Instant::now(), v));
                }
            }
        }
    }

    /// Collect events until the socket has been QUIET for `d` -- not until `d` has elapsed.
    ///
    /// The fixed window this used to be is a wall-clock assertion in disguise: it says "everything this
    /// navigation will do, it does within 200 ms", which is true on an idle box and false on a loaded
    /// one. It went red exactly once in `gate.sh` at load 29 (P12a's merge,
    /// `a_document_is_decoded_from_its_bytes_as_chrome_decodes_it`) and passed 2/2 alone, which is the
    /// signature. A quiet window ends at the same moment on an idle box and simply waits longer when the
    /// box is busy, so the test measures the ENGINE's sequence rather than the machine's speed. The cap
    /// exists so a page that never goes quiet still ends the test instead of hanging the suite.
    async fn pump(&mut self, d: Duration) {
        let cap = tokio::time::Instant::now() + (d * 40).max(Duration::from_secs(20));
        loop {
            let quiet_until = (tokio::time::Instant::now() + d).min(cap);
            match tokio::time::timeout_at(quiet_until, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v.get("method").is_some() {
                        self.events.push((Instant::now(), v));
                    }
                }
                Ok(_) => return,
                Err(_) => return,
            }
            if tokio::time::Instant::now() >= cap {
                return;
            }
        }
    }
}

const PROBE: &str = "JSON.stringify({href:location.href,url:document.URL,base:document.baseURI,title:document.title,cs:document.characterSet,text:document.body?document.body.innerText:null,lang:navigator.language,frames:[...document.querySelectorAll('iframe')].map(f=>{try{return f.contentWindow.location.href}catch(e){return 'X'}})})";

/// The transport's request ledger is PROCESS-global (one page per process is the production shape), and
/// every test in this binary shares the process. Every test that NAVIGATES takes this lock, so they
/// take turns; a request row another test's page made is filtered out by origin in [`labels`].
///
/// `pub(crate)`: it used to be this file's, and `server.rs`'s and `conformance.rs`'s
/// navigations ran beside these -- which is not a filtering problem but a RESET one. A navigation
/// calls `transport::ledger_reset()` before its document request, so a second navigation starting in
/// another test drops the rows this one has not reported yet, and a `REQ` label goes missing from a
/// sequence that is otherwise exactly Chrome's. It showed up as one of these tests failing under
/// `cargo test --workspace` at eight threads and passing alone, three runs each way.
pub(crate) static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One fixture, driven the way the parity driver drives a page: `(events after navigate, the probe)`.
async fn drive(path: &str, settle: Duration) -> (Vec<(Instant, Value)>, Value, u16, u16) {
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (pa, pb) = (a.local_addr().unwrap().port(), b.local_addr().unwrap().port());
    tokio::spawn(serve_fixtures(a, pb));
    tokio::spawn(serve_fixtures(b, pa));

    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let server = cdp::serve_connection(cdp_listener, Engine::new(Box::new(persona::CapturedPersona::default())));
    let url = format!("http://127.0.0.1:{pa}{path}");
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{cdp_addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Network.enable", json!({})).await;
        let mark = c.events.len();
        c.call("Page.navigate", json!({ "url": url })).await;
        c.pump(settle).await;
        let probe = c.call("Runtime.evaluate", json!({ "expression": PROBE, "returnByValue": true })).await;
        let page: Value = serde_json::from_str(probe["result"]["value"].as_str().expect("the probe answers a string")).unwrap();
        c.ws.send(Message::Close(None)).await.unwrap();
        (c.events.split_off(mark), page)
    };
    let (server, (events, page)) = tokio::join!(server, client);
    server.unwrap();
    (events, page, pa, pb)
}

/// The label sequence of `events` (see the module header).
fn labels(events: &[(Instant, Value)], ports: (u16, u16)) -> Vec<String> {
    let ours = |u: &str| [ports.0, ports.1].iter().any(|p| u.starts_with(&format!("http://127.0.0.1:{p}/")));
    let mut loaders: Vec<String> = Vec::new();
    let mut lid = |id: &str| -> String {
        if !loaders.iter().any(|l| l == id) {
            loaders.push(id.to_string());
        }
        format!("L{}", loaders.iter().position(|l| l == id).unwrap())
    };
    let file = |u: &str| u.rsplit('/').next().unwrap_or(u).to_string();
    let mut out = Vec::new();
    for (_, ev) in events {
        let p = &ev["params"];
        let s = match ev["method"].as_str().unwrap_or("") {
            "Page.frameStartedNavigating" => format!("FSN {} {}", file(p["url"].as_str().unwrap()), lid(p["loaderId"].as_str().unwrap())),
            "Page.frameStartedLoading" => "FSL".into(),
            "Network.requestWillBeSent"
                if p["type"] == "Document" && p.get("redirectResponse").is_none() && ours(p["request"]["url"].as_str().unwrap_or("")) =>
            {
                {
                    // The METHOD, when it is not the `GET` every other navigation is.
                    let m = match p["request"]["method"].as_str().unwrap_or("GET") {
                        "GET" => String::new(),
                        other => format!("{other} "),
                    };
                    format!("REQ {m}{} {}", file(p["request"]["url"].as_str().unwrap()), lid(p["loaderId"].as_str().unwrap()))
                }
            }
            "Runtime.executionContextsCleared" => "ECC".into(),
            "Page.frameNavigated" => format!("FN {} {}", file(p["frame"]["url"].as_str().unwrap()), lid(p["frame"]["loaderId"].as_str().unwrap())),
            "Runtime.executionContextCreated" => "CTX".into(),
            "Page.domContentEventFired" => "DCL".into(),
            "Page.loadEventFired" => "LOAD".into(),
            "Page.frameStoppedLoading" => "STOP".into(),
            "Page.frameScheduledNavigation" => format!("SCHED {} {}", p["reason"].as_str().unwrap(), p["delay"]),
            "Page.frameRequestedNavigation" => format!("ASK {} {} {}", p["reason"].as_str().unwrap(), p["disposition"].as_str().unwrap(), file(p["url"].as_str().unwrap())),
            "Page.frameClearedScheduledNavigation" => "CLR".into(),
            _ => continue,
        };
        out.push(s);
    }
    out
}

fn seq(s: &str) -> Vec<String> {
    s.split(" | ").map(str::to_string).collect()
}

/// A document that loads and stays, as Chrome reports it.
fn one_document(file: &str) -> Vec<String> {
    seq(&format!("FSN {file} L0 | FSL | REQ {file} L0 | ECC | ECC | FN {file} L0 | CTX | DCL | LOAD | STOP"))
}

/// Chrome's `(title, characterSet, innerText)` and the page's own `href`/`URL`/`baseURI`, with the
/// fixture origins written `A` and `B`.
fn check_page(path: &str, page: &Value, pa: u16, pb: u16, href: &str, title: &str, cs: &str, text: &str) {
    let unport = |s: &str| s.replace(&format!("127.0.0.1:{pa}"), "A").replace(&format!("127.0.0.1:{pb}"), "B");
    let got = (
        unport(page["href"].as_str().unwrap()),
        unport(page["url"].as_str().unwrap()),
        unport(page["base"].as_str().unwrap()),
        unport(page["title"].as_str().unwrap()),
        page["cs"].as_str().unwrap().to_string(),
        page["text"].as_str().unwrap_or("").to_string(),
    );
    let want = (href.to_string(), href.to_string(), href.to_string(), title.to_string(), cs.to_string(), text.to_string());
    assert_eq!(got, want, "{path}: (href, URL, baseURI, title, characterSet, innerText) differs from Chrome 150's");
}

const ORDER_LOG: &str = "<script>var L=[];function log(s){L.push(s+':'+document.readyState)};document.addEventListener('DOMContentLoaded',()=>log('DCL'));addEventListener('load',()=>log('load'))</script>";

const LOG_AFTER_LOAD: &str = "arrived at target\n\ndcl load beforeunload pagehide:false vis:hidden unload";
const LOG_IN_PARSE: &str = "arrived at target\n\nbeforeunload pagehide:false vis:hidden unload";

#[tokio::test]
async fn a_meta_refresh_or_refresh_header_navigates_after_the_load() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let after = |file: &str, reason: &str, delay: u32| {
        seq(&format!(
            "FSN {file} L0 | FSL | REQ {file} L0 | ECC | ECC | FN {file} L0 | CTX | DCL | LOAD | SCHED {reason} {delay} | STOP | \
             ASK {reason} currentTab target.html | FSN target.html L1 | FSL | REQ target.html L1 | CLR | ECC | ECC | \
             FN target.html L1 | CTX | DCL | LOAD | STOP"
        ))
    };
    for (path, want) in [
        ("/mr0.html", after("mr0.html", "metaTagRefresh", 0)),
        ("/mrbody.html", after("mrbody.html", "metaTagRefresh", 0)),
        ("/mr1.html", after("mr1.html", "metaTagRefresh", 1)),
    ] {
        let (events, page, pa, pb) = drive(path, Duration::from_millis(2500)).await;
        assert_eq!(labels(&events, (pa, pb)), want, "{path}: CDP events differ from Chrome 150's");
        check_page(path, &page, pa, pb, "http://A/target.html", "landed", "UTF-8", LOG_AFTER_LOAD);
        if path == "/mr1.html" {
            // The one-second delay counts from the load, and the navigation is not asked before it.
            let at = |label: &str| events.iter().find(|(_, e)| e["method"] == label).map(|(t, _)| *t).unwrap();
            let waited = at("Page.frameRequestedNavigation") - at("Page.loadEventFired");
            assert!(waited >= Duration::from_millis(900), "{path}: the refresh fired {waited:?} after load, not ~1 s");
        }
    }
    let (events, page, pa, pb) = drive("/refreshhdr.html", Duration::from_millis(1500)).await;
    assert_eq!(labels(&events, (pa, pb)), after("refreshhdr.html", "httpHeaderRefresh", 0), "a Refresh header: CDP events differ from Chrome 150's");
    check_page("/refreshhdr.html", &page, pa, pb, "http://A/target.html", "landed", "UTF-8", "arrived at target");

    // Not a refresh: no digits before the `;`. Chrome stays.
    let (events, page, pa, pb) = drive("/mrjunk.html", Duration::from_millis(1000)).await;
    assert_eq!(labels(&events, (pa, pb)), one_document("mrjunk.html"));
    check_page("/mrjunk.html", &page, pa, pb, "http://A/mrjunk.html", "mrjunk", "UTF-8", "mrjunk body");
}

#[tokio::test]
async fn a_script_navigation_lands_on_its_target() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // During the parse: the first document never reaches DOMContentLoaded or load, and a second
    // `frameStartedLoading` falls between the two context clears.
    let in_parse = |file: &str| {
        seq(&format!(
            "FSN {file} L0 | FSL | REQ {file} L0 | ECC | ECC | FN {file} L0 | CTX | SCHED scriptInitiated 0 | \
             ASK scriptInitiated currentTab target.html | FSN target.html L1 | FSL | REQ target.html L1 | CLR | ECC | FSL | ECC | \
             FN target.html L1 | CTX | DCL | LOAD | STOP"
        ))
    };
    for path in ["/href.html", "/replace.html", "/wloc.html"] {
        let (events, page, pa, pb) = drive(path, Duration::from_millis(1500)).await;
        assert_eq!(labels(&events, (pa, pb)), in_parse(&path[1..]), "{path}: CDP events differ from Chrome 150's");
        check_page(path, &page, pa, pb, "http://A/target.html", "landed", "UTF-8", LOG_IN_PARSE);
    }

    // A timer after the load: the first document finishes loading, then navigates.
    let (events, page, pa, pb) = drive("/assignto.html", Duration::from_millis(1500)).await;
    assert_eq!(
        labels(&events, (pa, pb)),
        seq("FSN assignto.html L0 | FSL | REQ assignto.html L0 | ECC | ECC | FN assignto.html L0 | CTX | DCL | LOAD | STOP | \
             SCHED scriptInitiated 0 | ASK scriptInitiated currentTab target.html | FSN target.html L1 | FSL | REQ target.html L1 | CLR | \
             ECC | ECC | FN target.html L1 | CTX | DCL | LOAD | STOP"),
        "setTimeout(location.assign): CDP events differ from Chrome 150's"
    );
    check_page("/assignto.html", &page, pa, pb, "http://A/target.html", "landed", "UTF-8", LOG_AFTER_LOAD);

    // A `load` handler: the load event ran (the log says so) but Chrome reports no loadEventFired.
    let (events, page, pa, pb) = drive("/onload.html", Duration::from_millis(1500)).await;
    assert_eq!(
        labels(&events, (pa, pb)),
        seq("FSN onload.html L0 | FSL | REQ onload.html L0 | ECC | ECC | FN onload.html L0 | CTX | DCL | SCHED scriptInitiated 0 | \
             ASK scriptInitiated currentTab target.html | FSN target.html L1 | FSL | REQ target.html L1 | CLR | ECC | FSL | ECC | \
             FN target.html L1 | CTX | DCL | LOAD | STOP"),
        "onload navigation: CDP events differ from Chrome 150's"
    );
    check_page("/onload.html", &page, pa, pb, "http://A/target.html", "landed", "UTF-8", LOG_AFTER_LOAD);
}

#[tokio::test]
async fn a_redirected_document_is_the_response_url() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (events, page, pa, pb) = drive("/r301.html", Duration::from_millis(500)).await;
    assert_eq!(labels(&events, (pa, pb)), seq("FSN r301.html L0 | FSL | REQ r301.html L0 | ECC | ECC | FN target.html L0 | CTX | DCL | LOAD | STOP"));
    check_page("/r301.html", &page, pa, pb, "http://B/target.html", "landed", "UTF-8", "arrived at target");
    let origin = events.iter().find(|(_, e)| e["method"] == "Runtime.executionContextCreated").unwrap().1["params"]["context"]["origin"].clone();
    assert_eq!(origin, json!(format!("http://127.0.0.1:{pb}")), "the context's origin is the response's origin, without a path");

    let (events, page, pa, pb) = drive("/r302chain.html", Duration::from_millis(500)).await;
    assert_eq!(labels(&events, (pa, pb)), seq("FSN r302chain.html L0 | FSL | REQ r302chain.html L0 | ECC | ECC | FN target.html L0 | CTX | DCL | LOAD | STOP"));
    check_page("/r302chain.html", &page, pa, pb, "http://B/target.html", "landed", "UTF-8", "arrived at target");

    let (_, page, pa, pb) = drive("/rbase.html", Duration::from_millis(500)).await;
    check_page(
        "/rbase.html",
        &page,
        pa,
        pb,
        "http://A/sub/based.html",
        "http://A/sub/based.html|http://A/sub/based.html|http://A/sub/based.html|http://A/sub/x.html",
        "UTF-8",
        "x",
    );
}

#[tokio::test]
async fn a_document_is_decoded_from_its_bytes_as_chrome_decodes_it() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let rows: &[(&str, &str, &str, &str)] = &[
        ("/euckr.html", "문서", "EUC-KR", "문서 모듈"),
        ("/latin1.html", "pt", "windows-1252", "português"),
        ("/metacharset.html", "mc", "windows-1252", "português"),
        ("/hdrsjis.html", "sj", "Shift_JIS", "日本語テキスト"),
        ("/hdrwins.html", "gw", "GBK", "中文字符"),
        ("/bomwins.html", "bw", "UTF-8", "português 한"),
        ("/utf16.html", "u16", "UTF-16LE", "한국어"),
        ("/latemeta.html", "lm", "EUC-KR", "문서"),
        ("/bodylate.html", "bl", "EUC-KR", "문서"),
        ("/scriptlate.html", "sl", "EUC-KR", "문서"),
        ("/bodyearly.html", "be", "EUC-KR", "hi\n\n문서"),
        ("/ascii.html", "as", "windows-1252", "plain ascii"),
        ("/euckrnometa.html", "en", "EUC-KR", "대한민국 문서 모듈 한국어 텍스트입니다"),
        ("/sjisnometa.html", "sn", "Shift_JIS", "日本語のテキストです。これはテストです。"),
        ("/bogushdr.html", "bh", "EUC-KR", "문서"),
        ("/metautf16.html", "m16", "UTF-8", "한국어"),
        ("/hdrutf8latin1.html", "hl", "UTF-8", "portugu\u{fffd}s"),
        ("/httpequivsjis.html", "hq", "Shift_JIS", "日本語"),
    ];
    for (path, title, cs, text) in rows {
        let (events, page, pa, pb) = drive(path, Duration::from_millis(200)).await;
        assert_eq!(labels(&events, (pa, pb)), one_document(&path[1..]), "{path}");
        check_page(path, &page, pa, pb, &format!("http://A{path}"), title, cs, text);
    }
    // UTF-8 bytes that declare nothing are the detector's ambiguous case, which Chrome resolves to the
    // UI locale's default -- measured under both locales, and the device's locale is the persona's.
    let (_, page, ..) = drive("/utf8nometa.html", Duration::from_millis(200)).await;
    let lang = page["lang"].as_str().unwrap().to_string();
    let locale_rows: &[(&str, &str, &str, &str, &str)] = &[
        ("ko", "/utf8nometa.html", "u8", "EUC-KR", "\u{fffd}\u{c493}\u{b385}\u{fffd}\u{fffd} portugu\u{cc57}s"),
        ("ko", "/utf8latin.html", "ul", "EUC-KR", "portugu\u{cc57}s caf\u{cc55}"),
        ("en", "/utf8nometa.html", "u8", "windows-1252", "\u{ed}\u{2022}\u{153}\u{ea}\u{b5}\u{ad}\u{ec}\u{2013}\u{b4} portugu\u{c3}\u{aa}s"),
        ("en", "/utf8latin.html", "ul", "windows-1252", "portugu\u{c3}\u{aa}s caf\u{c3}\u{a9}"),
    ];
    let primary = lang.split('-').next().unwrap_or("").to_ascii_lowercase();
    let mine: Vec<_> = locale_rows.iter().filter(|r| r.0 == primary).collect();
    assert!(!mine.is_empty(), "no Chrome measurement for the persona's locale {lang:?}; ask Chrome under that locale");
    for (_, path, title, cs, text) in mine {
        let (_, page, pa, pb) = drive(path, Duration::from_millis(200)).await;
        check_page(path, &page, pa, pb, &format!("http://A{path}"), title, cs, text);
    }
}

/// The page keeps running while the client sends nothing, as Chrome's renderer does -- and a page with
/// nothing scheduled costs this server nothing while it waits.
///
/// Chrome 150, loopback, `setTimeout(() => document.title = 'fired', 300)`: one `Runtime.evaluate`
/// answers "start", then one second with nothing sent, then a second evaluate answers "fired"; a fetch
/// the timer makes reaches the server 281 ms after the first answer. Korvus `fd6473b8` answered "fired"
/// too, but its fetch arrived 1004 ms after -- WITH the second evaluate, because the page only ran inside
/// a command. (P10c's report; measured again for P12d with `idle_probe.py`, after: 126 ms.)
#[tokio::test]
async fn the_page_runs_between_commands_and_a_settled_page_costs_nothing() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    async fn idle(path: &str, idle_for: Duration) -> (String, String, cdp::ServeStats) {
        let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pa = a.local_addr().unwrap().port();
        tokio::spawn(serve_fixtures(a, pa));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let server = cdp::serve_connection_counted(l, Engine::new(Box::new(persona::CapturedPersona::default())));
        let url = format!("http://127.0.0.1:{pa}{path}");
        let client = async move {
            let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
            let mut c = Client { ws, next: 0, events: Vec::new() };
            c.call("Page.enable", json!({})).await;
            c.call("Page.navigate", json!({ "url": url })).await;
            let title = json!({ "expression": "document.title", "returnByValue": true });
            let first = c.call("Runtime.evaluate", title.clone()).await["result"]["value"].as_str().unwrap().to_string();
            // Send nothing and READ nothing: the socket is left alone for the whole wait.
            tokio::time::sleep(idle_for).await;
            let second = c.call("Runtime.evaluate", title).await["result"]["value"].as_str().unwrap().to_string();
            c.ws.send(Message::Close(None)).await.unwrap();
            (first, second)
        };
        let (server, (first, second)) = tokio::join!(server, client);
        let (_, stats) = server.unwrap();
        (first, second, stats)
    }

    let (first, second, _) = idle("/idletimer.html", Duration::from_millis(1000)).await;
    assert_eq!((first.as_str(), second.as_str()), ("start", "fired"), "Chrome 150 answers start, then fired after 1 s idle");

    // Two seconds idle is ~40 ticks. A settled page is pumped until its loop first reports drained and
    // never again: the count is a handful, and every other tick is a skip.
    let (_, _, stats) = idle("/quiet.html", Duration::from_millis(2000)).await;
    assert!(stats.idle_pumps <= 3, "a page with nothing scheduled was pumped {} times while idle: {stats:?}", stats.idle_pumps);
    assert!(stats.idle_skips >= 20, "the idle ticks did not run: {stats:?}");
}

/// `defer` runs at `interactive` before `DOMContentLoaded`; an `async` body that arrives after the parse
/// runs after it. Chrome 150, loopback, three runs, the async script served 800 ms late -- the same
/// order all three times:
///
/// ```text
/// inline-end:loading defer:interactive DCL:interactive async-slow:interactive load:complete
/// ```
#[tokio::test]
async fn defer_and_async_scripts_run_where_chrome_runs_them() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (path, want) in [
        ("/order.html", "inline-end:loading defer:interactive DCL:interactive async-slow:interactive load:complete"),
    ] {
        let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pa = a.local_addr().unwrap().port();
        tokio::spawn(serve_fixtures(a, pa));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
        let url = format!("http://127.0.0.1:{pa}{path}");
        let client = async move {
            let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
            let mut c = Client { ws, next: 0, events: Vec::new() };
            c.call("Page.enable", json!({})).await;
            c.call("Page.navigate", json!({ "url": url })).await;
            c.pump(Duration::from_millis(1500)).await;
            let got = c.call("Runtime.evaluate", json!({ "expression": "L.join(' ')", "returnByValue": true })).await["result"]["value"]
                .as_str()
                .unwrap()
                .to_string();
            c.ws.send(Message::Close(None)).await.unwrap();
            got
        };
        let (server, got) = tokio::join!(server, client);
        server.unwrap();
        assert_eq!(got, want, "{path}: script order differs from Chrome 150's");
    }
}

// ── The navigations `window.open` and a form submission ask for ─────────────────────
//
// Chrome 150.0.7871.46, `--headless=new`, one page-target WebSocket per fixture, three runs each --
// every sequence below was identical all three times, so they are the protocol's order and not a
// race. What this half does NOT compare, and why:
//
//   * a NESTED context's navigation events. Chrome reports `frameStartedNavigating` /
//     `frameNavigated` / `frameStoppedLoading` for every iframe; this server reports none, for any
//     frame, and has since before P12d. The frame fixtures below therefore assert the OUTCOME -- the
//     document each context ends on, read through `contentWindow.location` -- which is the thing the
//     load axis scores. The missing frame events are recorded in the P13c report, not graded here.
//   * `window.open(url, "_blank")`. Chrome's popup blocker answers `null` with no user activation and
//     fetches nothing; this engine has no user-activation model at all and opens one. Recorded in
//     `core/html/html_form_element.rs`'s header and in the P13c report; a fixture that asserted
//     either answer would be asserting the engine's, not Chrome's.

/// The QUIET window every fixture below is driven with -- the same 1500 ms the P12d half uses for a
/// fixture that navigates twice. It is not a deadline and not an assertion: `Client::pump` returns as
/// soon as the socket has been silent for this long, so an idle box finishes in a fraction of it and
/// a box running the whole gate at eight threads simply waits. 300 ms was enough alone and truncated
/// one sequence under `gate.sh --full`, which is exactly the signature the `pump` comment describes.
const P13C_QUIET: Duration = Duration::from_millis(1500);

/// `... FN <file> L0 | CTX` -- the head every one of these sequences starts with.
fn head_of(file: &str) -> String {
    format!("FSN {file} L0 | FSL | REQ {file} L0 | ECC | ECC | FN {file} L0 | CTX")
}

/// The tail Chrome sends once the SECOND document has committed: `FN … | CTX | DCL | LOAD | STOP`.
/// `restarted` is the second `frameStartedLoading` Chrome sends when it never reported the leaving
/// document's `loadEventFired`.
fn commit_of(file: &str, restarted: bool) -> String {
    let ecc = if restarted { "ECC | FSL | ECC" } else { "ECC | ECC" };
    format!("{ecc} | FN {file} L1 | CTX | DCL | LOAD | STOP")
}

/// `window.open(url, "_self" | "_top" | "_parent")` navigates the PAGE, with reason `other` -- and
/// `other` is the one reason Chrome never announces as scheduled and never clears.
///
/// ```text
/// in the parse   … FN src L0 | CTX | ASK other currentTab t | FSN t L1 | FSL | REQ t L1 | ECC | FSL | ECC | FN t L1 …
/// in `load`      … FN src L0 | CTX | DCL | ASK other currentTab t | FSN t L1 | FSL | REQ t L1 | LOAD | ECC | ECC | FN t L1 …
/// ```
///
/// The `load` row is the shape no other reason has: the leaving document's `Page.loadEventFired`
/// comes AFTER the next navigation's request, and its `frameStoppedLoading` never comes at all.
#[tokio::test]
async fn window_open_self_navigates_the_page_as_chrome_navigates_it() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let target = "p13c-landed.html";
    for file in ["p13c-open-self.html", "p13c-open-top.html", "p13c-open-parent.html"] {
        let (events, page, pa, pb) = drive(&format!("/{file}"), P13C_QUIET).await;
        assert_eq!(
            labels(&events, (pa, pb)),
            seq(&format!(
                "{} | ASK other currentTab {target} | FSN {target} L1 | FSL | REQ {target} L1 | {}",
                head_of(file),
                commit_of(target, true)
            )),
            "{file}: the event sequence differs from Chrome 150's"
        );
        check_page(file, &page, pa, pb, &format!("http://A/{target}"), "landed2", "UTF-8", "landed2 body");
    }

    let file = "p13c-open-self-onload.html";
    let (events, page, pa, pb) = drive(&format!("/{file}"), P13C_QUIET).await;
    assert_eq!(
        labels(&events, (pa, pb)),
        seq(&format!(
            "{} | DCL | ASK other currentTab {target} | FSN {target} L1 | FSL | REQ {target} L1 | LOAD | {}",
            head_of(file),
            commit_of(target, false)
        )),
        "{file}: the event sequence differs from Chrome 150's"
    );
    check_page(file, &page, pa, pb, &format!("http://A/{target}"), "landed2", "UTF-8", "landed2 body");
}

/// `window.open(url, name)` navigates the FRAME that name belongs to, and hands back its window.
/// Chrome: the iframe ends on the opened document, the page stays where it was, and `w` is an object
/// that is not `window`.
#[tokio::test]
async fn window_open_navigates_the_frame_its_target_names() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (_events, page, pa, _pb) = drive("/p13c-open-frame.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-open-frame.html"), "the page must not move");
    assert_eq!(page["title"].as_str().unwrap(), "src w=obj", "open() answers the named frame's window");
    assert_eq!(
        page["frames"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>(),
        vec![format!("http://127.0.0.1:{pa}/p13c-landed.html")],
        "the named frame is the context that navigated"
    );
}

/// A form submission is a navigation, and Chrome reports it with the reason its METHOD names.
///
/// The three orders in this table are the unit's whole finding about form submission and CDP:
/// `frameRequestedNavigation` is emitted where the SCRIPT ran, the submission's
/// `frameScheduledNavigation` follows the leaving document's `frameStoppedLoading` (the submission is
/// a queued task, so that document reaches its own `load` first), and a submission during the parse
/// abandons the document exactly as `location.href` does.
#[tokio::test]
async fn a_form_submission_is_the_navigation_chrome_reports() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (fixture, reason, the target as it appears in the events, the REQ label's method prefix)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("p13c-form-get.html", "formSubmissionGet", "p13c-echo?q=1&r=a+b", ""),
        ("p13c-form-post.html", "formSubmissionPost", "p13c-echo", "POST "),
        ("p13c-form-requestsubmit.html", "formSubmissionGet", "p13c-echo?q=1&r=a+b", ""),
        ("p13c-form-click.html", "formSubmissionGet", "p13c-echo?q=1&b=bv", ""),
        ("p13c-form-formaction.html", "formSubmissionPost", "p13c-echo2", "POST "),
    ];
    for (file, reason, target, method) in cases {
        let (events, _page, pa, pb) = drive(&format!("/{file}"), P13C_QUIET).await;
        assert_eq!(
            labels(&events, (pa, pb)),
            seq(&format!(
                "{} | DCL | ASK {reason} currentTab {target} | LOAD | STOP | SCHED {reason} 0 | \
                 FSN {target} L1 | FSL | REQ {method}{target} L1 | CLR | {}",
                head_of(file),
                commit_of(target, false)
            )),
            "{file}: the event sequence differs from Chrome 150's"
        );
    }

    // Submitted DURING THE PARSE: the document is abandoned -- no `DOMContentLoaded`, no `load`, no
    // `frameStoppedLoading` -- and the announcement is all it has left to say.
    let (file, target) = ("p13c-form-parse.html", "p13c-echo?q=1&r=a+b");
    let (events, _page, pa, pb) = drive(&format!("/{file}"), P13C_QUIET).await;
    assert_eq!(
        labels(&events, (pa, pb)),
        seq(&format!(
            "{} | ASK formSubmissionGet currentTab {target} | SCHED formSubmissionGet 0 | \
             FSN {target} L1 | FSL | REQ {target} L1 | CLR | {}",
            head_of(file),
            commit_of(target, true)
        )),
        "{file}: the event sequence differs from Chrome 150's"
    );
}

/// What each `enctype` puts on the wire, read back off the request that landed.
///
/// ```text
/// (default)             application/x-www-form-urlencoded   q=1&r=a+b
/// text/plain            text/plain                          "q=1\r\nr=a b\r\n"
/// multipart/form-data   multipart/form-data; boundary=…     the block below, with an EMPTY file part
/// ```
#[tokio::test]
async fn a_post_carries_the_body_its_enctype_serializes() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let body_of = |page: &Value| page["text"].as_str().unwrap_or("").to_string();

    let (_e, page, _pa, _pb) = drive("/p13c-form-post.html", P13C_QUIET).await;
    assert_eq!(body_of(&page), "landed body\n\nPOST||application/x-www-form-urlencoded|q=1&r=a+b");

    let (_e, page, _pa, _pb) = drive("/p13c-form-textplain.html", P13C_QUIET).await;
    assert_eq!(body_of(&page), "landed body\n\nPOST||text/plain|q=1\nr=a b\n");

    // The boundary is `----WebKitFormBoundary` and 16 characters of Chromium's alphabet; the server
    // rewrote it to `<16>` in the header and to `B` in the body, so what is left is the block itself.
    let (_e, page, _pa, _pb) = drive("/p13c-form-multipart.html", P13C_QUIET).await;
    assert_eq!(
        body_of(&page),
        "landed body\n\nPOST||multipart/form-data; boundary=----WebKitFormBoundary<16>|\
         --B\nContent-Disposition: form-data; name=\"q\"\n\n1\n\
         --B\nContent-Disposition: form-data; name=\"fi\"; filename=\"\"\nContent-Type: application/octet-stream\n\n\n\
         --B--\n"
    );
}

/// WHERE a submission goes: an empty action is the document's own URL with the query REPLACED, an
/// action's fragment survives onto the document while the request asks without it, a `_blank` target
/// navigates nothing, and a form in a frame submits the FRAME unless it says `_top`.
#[tokio::test]
async fn a_form_submits_to_the_url_and_the_context_chrome_chooses() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    // `action=""`: the document's URL, query replaced. (Chrome resubmits for ever without the guard
    // this fixture carries -- measured, and the reason the guard is there.)
    let (_e, page, pa, _pb) = drive("/p13c-form-empty-action.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-form-empty-action.html?q=1"));

    // `action="p13c-echo?keep=1#h"`: the query is replaced whole, the fragment is kept on the
    // DOCUMENT, and the request went out without it.
    let (events, page, pa, pb) = drive("/p13c-form-fragment.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-echo?q=1#h"));
    assert_eq!(page["text"].as_str().unwrap(), "landed body\n\nGET|q=1||");
    let ls = labels(&events, (pa, pb));
    assert!(ls.contains(&"REQ p13c-echo?q=1 L1".to_string()), "the request carries no fragment: {ls:?}");
    assert!(ls.contains(&"FN p13c-echo?q=1 L1".to_string()), "frame.url carries no fragment: {ls:?}");

    // `target=_blank`: nothing navigates and nothing is requested.
    let (events, page, pa, pb) = drive("/p13c-form-blank.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-form-blank.html"));
    assert_eq!(labels(&events, (pa, pb)), one_document("p13c-form-blank.html"));

    // A form in a frame, `target=_top`: the PAGE navigates.
    let (_e, page, pa, _pb) = drive("/p13c-frame-top.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-echo?q=1"));

    // The same form with no target: the FRAME navigates and the page stays.
    let (_e, page, pa, _pb) = drive("/p13c-frame-self.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-frame-self.html"));
    assert_eq!(
        page["frames"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>(),
        vec![format!("http://127.0.0.1:{pa}/p13c-echo?q=1")]
    );

    // `target=<a frame's name>`: that frame navigates.
    let (_e, page, pa, _pb) = drive("/p13c-form-named-frame.html", P13C_QUIET).await;
    assert_eq!(page["href"].as_str().unwrap(), format!("http://127.0.0.1:{pa}/p13c-form-named-frame.html"));
    assert_eq!(
        page["frames"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>(),
        vec![format!("http://127.0.0.1:{pa}/p13c-echo?q=1")]
    );
}

/// The `submit` and `formdata` events, measured with the navigation blocked (`target=_blank` with no
/// user activation) so the page that fired them is still there to be asked:
///
/// ```text
/// requestSubmit()   [submit c=true b=true s=null t=true] [formdata c=false b=true k=q=1,r=a b]
/// button.click()    [submit s=BUTTON/bv]                 [formdata k=q,b]
/// submit()          [formdata]                           -- and NO submit event at all
/// ```
///
/// `submit()`'s silence is the half that is easy to get wrong: a `preventDefault()`ing listener does
/// not stop it, because it is never called.
#[tokio::test]
async fn the_submit_and_formdata_events_are_chromes() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (file, want) in [
        (
            "p13c-events-requestsubmit.html",
            "[submit c=true b=true s=null t=true] [formdata c=false b=true k=q=1,r=a b]",
        ),
        ("p13c-events-click.html", "[submit s=BUTTON/bv] [formdata k=q,b]"),
        ("p13c-events-submit.html", "[formdata]"),
    ] {
        let (_e, page, _pa, _pb) = drive(&format!("/{file}"), P13C_QUIET).await;
        assert_eq!(page["title"].as_str().unwrap(), want, "{file}: the events differ from Chrome 150's");
    }

    // A `preventDefault()`ed `submit` stops `requestSubmit()` and does not stop `submit()`.
    let (events, page, pa, pb) = drive("/p13c-events-prevent.html", P13C_QUIET).await;
    assert_eq!(page["title"].as_str().unwrap(), "prevented");
    assert_eq!(labels(&events, (pa, pb)), one_document("p13c-events-prevent.html"));

    let (events, page, pa, pb) = drive("/p13c-events-prevent-submit.html", P13C_QUIET).await;
    assert_eq!(page["title"].as_str().unwrap(), "landed", "submit() ignores a cancelled submit event");
    assert_ne!(labels(&events, (pa, pb)), one_document("p13c-events-prevent-submit.html"));

    // `method=dialog` closes the nearest ancestor dialog with the submitter's value, and navigates
    // nothing.
    let (events, page, pa, pb) = drive("/p13c-form-dialog.html", P13C_QUIET).await;
    assert_eq!(page["title"].as_str().unwrap(), "open=false rv=bv");
    assert_eq!(labels(&events, (pa, pb)), one_document("p13c-form-dialog.html"));
}

/// Constraint validation gates `requestSubmit()` and a button's activation, and NOT `submit()`.
///
/// ```text
/// <input required value="">  form.submit()        navigates to ?q=   no `invalid` event
///                            form.requestSubmit() stays              one `invalid` event
///                            button.click()       stays              one `invalid` event
///                            + novalidate         navigates to ?q=   no `invalid` event
/// ```
#[tokio::test]
async fn constraint_validation_gates_every_submission_but_the_submit_method() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for file in ["p13c-required-submit.html", "p13c-novalidate.html"] {
        let (_e, page, pa, _pb) = drive(&format!("/{file}"), P13C_QUIET).await;
        assert_eq!(
            page["href"].as_str().unwrap(),
            format!("http://127.0.0.1:{pa}/p13c-echo?q="),
            "{file}: an unvalidated submission navigates"
        );
    }
    for file in ["p13c-required-requestsubmit.html", "p13c-required-click.html"] {
        let (events, page, pa, pb) = drive(&format!("/{file}"), P13C_QUIET).await;
        assert_eq!(page["title"].as_str().unwrap(), "invalid=1", "{file}: the invalid event fires once");
        assert_eq!(labels(&events, (pa, pb)), one_document(file), "{file}: an invalid form navigates nothing");
    }
}

/// A history entry a client sends the tab to is where the tab is for the very next command, as Chrome's
/// browser-side traversal is: Chrome 152.0.7977.82, loopback, 2026-10-28, two documents then back and forward, the
/// history and the document read at once after each:
///
/// ```text
/// after the two navigations    currentIndex 2   about:blank /dropdown /resistor
/// back                         currentIndex 1   /dropdown complete     (from the back/forward cache or not)
/// forward                      currentIndex 2   /resistor complete
/// ```
///
/// Stagehand reads both without waiting for a load (its core eval `navigation/back_forward`): korvus answered the
/// leaving document and its index until the traversal's round trip had passed, so going straight forward found
/// nothing ahead, and the URL read right after going back was the one it had left.
#[tokio::test]
async fn a_history_entry_the_client_goes_to_is_where_the_next_command_finds_the_tab() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pa = a.local_addr().unwrap().port();
    tokio::spawn(serve_fixtures(a, pa));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let base = format!("http://127.0.0.1:{pa}");
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        for path in ["/quiet.html", "/target.html"] {
            c.call("Page.navigate", json!({ "url": format!("{base}{path}") })).await;
            c.pump(Duration::from_millis(300)).await;
        }
        let h = c.call("Page.getNavigationHistory", json!({})).await;
        let at = h["currentIndex"].as_u64().unwrap() as usize;
        let entries = h["entries"].as_array().unwrap().clone();
        assert_eq!(entries[at]["url"], format!("{base}/target.html"), "{h}");
        assert_eq!(entries[at - 1]["url"], format!("{base}/quiet.html"), "{h}");
        let mut seen = Vec::new();
        for (to, path) in [(at - 1, "/quiet.html"), (at, "/target.html")] {
            c.call("Page.navigateToHistoryEntry", json!({ "entryId": entries[to]["id"] })).await;
            let index = c.call("Page.getNavigationHistory", json!({})).await["currentIndex"].as_u64().unwrap() as usize;
            let probe = json!({ "expression": "location.pathname + ' ' + document.readyState", "returnByValue": true });
            let here = c.call("Runtime.evaluate", probe).await["result"]["value"].as_str().unwrap().to_string();
            seen.push((to, index, here, format!("{path} complete")));
            c.pump(Duration::from_millis(300)).await;
        }
        c.ws.send(Message::Close(None)).await.unwrap();
        seen
    };
    let (server, seen) = tokio::join!(server, client);
    server.unwrap();
    for (to, index, here, there) in seen {
        assert_eq!((index, here), (to, there), "the next command finds the tab at the entry it was sent to");
    }
}

/// A `data:` URL a client navigates to is the document: its markup, its type, its opaque origin, the charset it names.
///
/// Every scheme but http(s) loaded an empty document, so `page.goto('data:text/html,...')` -- Stagehand's
/// `observe_main_frame_element_ids` builds its page so -- showed a blank page. Chrome 152 answers each probe below
/// with exactly these values (headless, raw CDP, 2026-10-28).
#[tokio::test]
async fn a_data_url_the_client_navigates_to_is_the_document() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let cases = [
        ("data:text/html,<p>hi</p>", r#"["<p>hi</p>","BackCompat","text/html","null","windows-1252"]"#),
        ("data:text/html,%3C!doctype%20html%3E%3Cp%3Ea%23b%3C%2Fp%3E", r#"["<p>a#b</p>","CSS1Compat","text/html","null","windows-1252"]"#),
        ("data:text/html;base64,PHA+aGk8L3A+", r#"["<p>hi</p>","BackCompat","text/html","null","windows-1252"]"#),
        ("data:text/html;charset=utf-8,%3Cp%3Ehi%3C%2Fp%3E", r#"["<p>hi</p>","BackCompat","text/html","null","UTF-8"]"#),
        ("data:text/html;charset=iso-8859-1,%E9t%E9", r#"["été","BackCompat","text/html","null","windows-1252"]"#),
        ("data:,hello%20there", r#"["<pre style=\"word-wrap: break-word; white-space: pre-wrap;\">hello there</pre>","CSS1Compat","text/plain","null","windows-1252"]"#),
    ];
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        let mut seen = Vec::new();
        for (url, chrome) in cases {
            c.call("Page.navigate", json!({ "url": url })).await;
            c.pump(Duration::from_millis(300)).await;
            let probe = "JSON.stringify([document.body.innerHTML, document.compatMode, document.contentType, origin, document.characterSet])";
            let here = c.call("Runtime.evaluate", json!({ "expression": probe, "returnByValue": true })).await;
            seen.push((url, here["result"]["value"].as_str().unwrap_or_default().to_string(), chrome));
        }
        c.ws.send(Message::Close(None)).await.unwrap();
        seen
    };
    let (server, seen) = tokio::join!(server, client);
    server.unwrap();
    for (url, here, chrome) in seen {
        assert_eq!(here, chrome, "{url}");
    }
}

/// What a page requests once its `load` has fired is reported after `Page.loadEventFired`, as Chrome reports it.
///
/// The load's events went out when the load had returned, after what the `load` handler's timers had requested (and
/// the favicon). Chrome 152 over loopback (2026-10-28), this page: `/load-sync`, the `load` handler's own request,
/// before `Page.loadEventFired`; the two handlers' `setTimeout(0)` requests after it. (What this does not give a
/// client that counts quiet from `load` on, Stagehand's `networkidle`, is in `reporting_network`.)
#[tokio::test]
async fn what_a_page_requests_after_its_load_is_reported_after_the_load() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let site = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = site.local_addr().unwrap().port();
    tokio::spawn(async move {
        const PAGE: &str = "<!doctype html><title>lo</title><body><p>x</p><script src=/slow.js></script><script>
fetch('/parse');
document.addEventListener('DOMContentLoaded', () => { fetch('/dcl-sync'); setTimeout(() => fetch('/dcl-timeout0'), 0); });
addEventListener('load', () => { fetch('/load-sync'); setTimeout(() => fetch('/load-timeout0'), 0); });
</script><img src=/img.png></body>";
        loop {
            let Ok((mut s, _)) = site.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let path = String::from_utf8_lossy(&buf[..n]).split_whitespace().nth(1).unwrap_or("/").to_string();
                // what the load waits for comes late, so the parse's and the handlers' requests fall on either side
                if path == "/slow.js" || path == "/img.png" {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                let (ty, body) = if path == "/" { ("text/html", PAGE) } else { ("text/plain", "1") };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Network.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/") })).await;
        c.pump(Duration::from_millis(500)).await;
        c.ws.send(Message::Close(None)).await.unwrap();
        c.events
    };
    let (server, events) = tokio::join!(server, client);
    server.unwrap();
    let order: Vec<String> = events
        .iter()
        .filter_map(|(_, e)| match e["method"].as_str()? {
            "Page.loadEventFired" => Some("load".to_string()),
            "Network.requestWillBeSent" => {
                let url = e["params"]["request"]["url"].as_str()?;
                let path = url.split_once(&format!(":{port}")).map(|(_, p)| p)?;
                matches!(path, "/parse" | "/dcl-sync" | "/load-sync" | "/dcl-timeout0" | "/load-timeout0").then(|| path.to_string())
            }
            _ => None,
        })
        .collect();
    let at = |what: &str| order.iter().position(|o| o == what).unwrap_or_else(|| panic!("{what} in {order:?}"));
    // the page's own `load`: the first one is the blank document's the tab started at
    let load = order.iter().rposition(|o| o == "load").expect("the page's load");
    for before in ["/parse", "/dcl-sync", "/load-sync"] {
        assert!(at(before) < load, "{before} before the load's event: {order:?}");
    }
    for after in ["/dcl-timeout0", "/load-timeout0"] {
        assert!(at(after) > load, "{after} after the load's event, as Chrome reports it: {order:?}");
    }
}

/// A document that writes to the console as it is left: `beforeunload`, `pagehide` and `unload` each log, and
/// `unload` throws too.
const LEAVE_A: &str = "<!doctype html><title>leave-a</title><script>
console.log('a-loaded');
addEventListener('beforeunload',()=>console.log('a-beforeunload'));
addEventListener('pagehide',()=>console.log('a-pagehide'));
addEventListener('unload',()=>{console.log('a-unload');throw new Error('a-unload-throw')});
</script>";

/// What a document logs as it is LEFT. A console call's values are handles into the realm that made them, and that
/// realm goes with the isolate the navigation drops: what `pagehide` logged was drained in the NEXT document's isolate
/// and the page process died ("attempt to access Handle hosted by disposed Isolate" -- every eBay run of a minter
/// that navigated a second time, 2026-10-27).
///
/// Stock Chrome 150.0.7871.46 (`--headless=new`, these two fixtures, `Page.enable` + `Runtime.enable`):
///
/// ```text
/// FSN leave-b.html | FSL | LOG a-beforeunload @A | ECC | ECC | FN leave-b.html | CTX @B | LOG b-loaded @B
/// ```
///
/// `beforeunload`'s message in the leaving document's own context, before its contexts are cleared; nothing of what
/// `pagehide` and `unload` logged or threw.
#[tokio::test]
async fn what_a_document_logs_as_it_is_left_is_what_chrome_reports() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pa = a.local_addr().unwrap().port();
    tokio::spawn(serve_fixtures(a, pa));
    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let server = cdp::serve_connection(cdp_listener, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{cdp_addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{pa}/leave-a.html") })).await;
        c.pump(Duration::from_millis(300)).await;
        let mark = c.events.len();
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{pa}/leave-b.html") })).await;
        c.pump(Duration::from_millis(300)).await;
        // the page is alive in its new document
        let title = c.call("Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true })).await;
        c.ws.send(Message::Close(None)).await.unwrap();
        (c.events, mark, title)
    };
    let (server, (events, mark, title)) = tokio::join!(server, client);
    server.unwrap();
    assert_eq!(title["result"]["value"], "leave-b");
    let ctx_a = events[..mark].iter().rev().find(|(_, e)| e["method"] == "Runtime.executionContextCreated").map(|(_, e)| e["params"]["context"]["id"].clone());
    let ctx_b = events[mark..].iter().find(|(_, e)| e["method"] == "Runtime.executionContextCreated").map(|(_, e)| e["params"]["context"]["id"].clone());
    let (ctx_a, ctx_b) = (ctx_a.expect("leave-a's context"), ctx_b.expect("leave-b's context"));
    let at = |id: &Value| if *id == ctx_a { "@A" } else if *id == ctx_b { "@B" } else { "@?" };
    let got: Vec<String> = events[mark..]
        .iter()
        .filter_map(|(_, e)| {
            let p = &e["params"];
            Some(match e["method"].as_str()? {
                "Page.frameStartedNavigating" => format!("FSN {}", p["url"].as_str()?.rsplit('/').next()?),
                "Page.frameStartedLoading" => "FSL".into(),
                "Runtime.consoleAPICalled" => format!("LOG {} {}", p["args"][0]["value"].as_str()?, at(&p["executionContextId"])),
                "Runtime.exceptionThrown" => format!("THROW {}", p["exceptionDetails"]["text"].as_str()?),
                "Runtime.executionContextsCleared" => "ECC".into(),
                "Page.frameNavigated" => format!("FN {}", p["frame"]["url"].as_str()?.rsplit('/').next()?),
                "Runtime.executionContextCreated" => format!("CTX {}", at(&p["context"]["id"])),
                _ => return None,
            })
        })
        .collect();
    assert_eq!(got, seq("FSN leave-b.html | FSL | LOG a-beforeunload @A | ECC | ECC | FN leave-b.html | CTX @B | LOG b-loaded @B"));
}

/// A page's `fetch()`es with bodies of each kind, and one without.
const POSTS: &str = "<!doctype html><title>posts</title><script>
fetch('/sink-json',{method:'POST',headers:{'Content-Type':'application/json'},body:'{\"a\":1}'});
fetch('/sink-bin',{method:'POST',body:new Uint8Array([0,1,2,255])});
fetch('/sink-form',{method:'POST',body:new URLSearchParams({q:'x y'})});
fetch('/sink-get');
</script>";

/// The body a request was sent with, over CDP `Network`: a client that records what a page signed reads it back, and
/// a signature that covers the body is checked against it. Stock Chrome 150.0.7871.46 (`--headless=new`, `POSTS`,
/// `Network.enable`): each POST's `requestWillBeSent` carries `hasPostData`, `postData` (UTF-8 text, else a byte a
/// character) and one `postDataEntries` entry of its bytes; `Network.getRequestPostData` answers the text, or base64 for
/// bytes that are not UTF-8; a GET carries none of the three and has no post data to ask for.
#[tokio::test]
async fn a_request_body_is_reported_as_chrome_reports_it() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pa = a.local_addr().unwrap().port();
    tokio::spawn(serve_fixtures(a, pa));
    let cdp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdp_addr = cdp_listener.local_addr().unwrap();
    let server = cdp::serve_connection(cdp_listener, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{cdp_addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Network.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{pa}/posts.html") })).await;
        c.pump(Duration::from_millis(300)).await;
        let sent: Vec<(String, Value)> = c
            .events
            .iter()
            .filter(|(_, e)| e["method"] == "Network.requestWillBeSent")
            .filter_map(|(_, e)| {
                let file = e["params"]["request"]["url"].as_str()?.rsplit('/').next()?.to_string();
                file.starts_with("sink-").then(|| (file, e["params"].clone()))
            })
            .collect();
        let mut out = Vec::new();
        for (file, p) in sent {
            c.next += 1;
            let id = c.next;
            let ask = json!({ "id": id, "method": "Network.getRequestPostData", "params": { "requestId": p["requestId"] } });
            c.ws.send(Message::Text(ask.to_string())).await.unwrap();
            let answer = loop {
                let Some(Ok(Message::Text(t))) = c.ws.next().await else { panic!("the socket closed") };
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["id"] == id {
                    break v;
                }
            };
            out.push((file, p["request"].clone(), answer));
        }
        c.ws.send(Message::Close(None)).await.unwrap();
        out
    };
    let (server, mut seen) = tokio::join!(server, client);
    server.unwrap();
    seen.sort_by(|a, b| a.0.cmp(&b.0));
    let files: Vec<&str> = seen.iter().map(|(f, _, _)| f.as_str()).collect();
    assert_eq!(files, ["sink-bin", "sink-form", "sink-get", "sink-json"]);
    for (file, request, answer) in &seen {
        let body = |k: &str| request.get(k).cloned().unwrap_or(Value::Null);
        let (want_text, want_bytes, want_answer) = match file.as_str() {
            "sink-json" => (json!("{\"a\":1}"), json!([{ "bytes": "eyJhIjoxfQ==" }]), json!({ "postData": "{\"a\":1}", "base64Encoded": false })),
            "sink-bin" => (json!("\u{0}\u{1}\u{2}\u{ff}"), json!([{ "bytes": "AAEC/w==" }]), json!({ "postData": "AAEC/w==", "base64Encoded": true })),
            "sink-form" => (json!("q=x+y"), json!([{ "bytes": "cT14K3k=" }]), json!({ "postData": "q=x+y", "base64Encoded": false })),
            _ => {
                assert_eq!((body("hasPostData"), body("postData"), body("postDataEntries")), (Value::Null, Value::Null, Value::Null), "{file}");
                assert_eq!(answer["error"]["message"], "No post data available for the request", "{file}");
                continue;
            }
        };
        assert_eq!((body("hasPostData"), body("postData"), body("postDataEntries")), (json!(true), want_text, want_bytes), "{file}");
        assert_eq!(answer["result"], want_answer, "{file}");
    }
}

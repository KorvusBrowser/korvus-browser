//! THE STREAMING PARSE: the parser's scripts run as their responses come in, and the parser waits --
//! with the event loop running -- only where Chrome's does.
//!
//! korvus fetched every parser `<script src>` and blocking sheet before the first script ran, so an inline script ran
//! behind the slowest later one: WPT `html/browsers/history/the-history-interface/007.html`'s inline `history.go(-1)`
//! waited 1.5 s for a later script and its `popstate` came after `load`. Three documents, each measured on
//! /usr/bin/google-chrome 150.0.7871.46 (headed, no DevTools, three runs each; the rig is
//! these exact documents plus a reporter):
//!
//! * `wait` -- the traversal and a 1 ms timer run while the parser waits for a slow script.
//! * `rejection` -- the parser waited for two scripts, so its end is a task of its own
//!   (`HTMLDocumentParser::ScheduleEndIfDelayed`), after the notification of the rejection the inline script made.
//! * `nowait` -- a parse that never waited ends in the task that parsed it, and with nothing delaying it `load` fires
//!   there (`FrameLoader::FinishedParsing` -> `ImplicitClose`), before the tasks the scripts queued -- the rejection's
//!   notification among them, queued at the end of the first script's microtask checkpoint.

use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn query<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (path, q) = target.split_once('?').unwrap_or((&target, ""));
            let (status, body) = match path {
                "/js" => (200, format!("L('ran {}');", query(q, "n").unwrap_or("?"))),
                _ => (404, String::new()),
            };
            if let Some(ms) = query(q, "ms").and_then(|v| v.parse().ok()) {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

const PRE: &str = r#"<script>
var R = [];
function L(s) { R.push(s); }
addEventListener('load', function () { L('load'); });
</script>"#;

fn log(e: &mut Engine) -> Vec<String> {
    let v = e.eval_value("streaming-log", "R.slice()".to_string()).expect("eval");
    v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
}

/// Load `head` + `body` from a loopback server and pump until the page logged `n` entries (a ceiling on waiting,
/// never asserted on), then a little longer so a late extra entry would show.
async fn case(head: &str, body: &str, n: usize) -> Vec<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("<!doctype html><html><head>{PRE}{head}</head><body>{body}</body></html>");
    e.load_page(&doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    for _ in 0..200 {
        let _ = e.pump(Duration::from_millis(50)).await;
        if log(&mut e).len() >= n {
            break;
        }
    }
    let _ = e.pump(Duration::from_millis(100)).await;
    log(&mut e)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn an_inline_script_runs_while_a_later_script_is_on_the_wire() {
    let body = r#"<script>
addEventListener('popstate', function (e) { L('popstate ' + e.state); });
history.pushState('state1', ''); history.pushState('state2', '');
history.go(-1); L('go called');
setTimeout(function () { L('timer'); }, 1);
onload = function () { L('onload state=' + history.state); };
</script><script src="/js?n=slow&ms=1500"></script>"#;
    let got = case("", body, 6).await;
    // Chrome 150: the 1 ms timer and the traversal's `popstate` race (2 runs of 3 the timer first), both while the
    // parser waits; everything else is the same every run
    assert_eq!(got.len(), 6, "{got:?}");
    assert_eq!(got[0], "go called", "{got:?}");
    let mut waited = got[1..3].to_vec();
    waited.sort();
    assert_eq!(waited, strs(&["popstate state1", "timer"]), "{got:?}");
    assert_eq!(got[3..], strs(&["ran slow", "load", "onload state=state1"]), "{got:?}");
}

#[tokio::test]
async fn a_parse_that_waited_ends_in_a_task_after_the_rejection_notification() {
    let head = r#"<script src="/js?n=a"></script><script src="/js?n=b"></script>"#;
    let body = r#"<p>x</p><script>
document.addEventListener('readystatechange', function () { L('rsc:' + document.readyState); });
addEventListener('unhandledrejection', function () { L('unhandledrejection'); });
Promise.reject(new Error('intentional'));
</script>"#;
    let got = case(head, body, 6).await;
    assert_eq!(got, strs(&["ran a", "ran b", "unhandledrejection", "rsc:interactive", "rsc:complete", "load"]));
}

#[tokio::test]
async fn a_parse_that_never_waited_fires_load_before_the_tasks_its_scripts_queued() {
    let head = r#"<script>
document.addEventListener('readystatechange', function () { L('rsc:' + document.readyState); });
document.addEventListener('DOMContentLoaded', function () { L('DCL'); });
addEventListener('unhandledrejection', function () { L('unhandledrejection'); });
var mc = new MessageChannel(); mc.port1.onmessage = function () { L('mc'); };
setTimeout(function () { L('t0'); }, 0);
mc.port2.postMessage(1);
Promise.reject(new Error('intentional'));
L('s1');
</script><script>L('s2'); setTimeout(function () { L('t0b'); }, 0);</script>"#;
    let body = "<p>x</p><script>L('s3');</script><p>y</p>";
    let got = case(head, body, 11).await;
    assert_eq!(got, strs(&["s1", "s2", "s3", "rsc:interactive", "DCL", "rsc:complete", "load", "t0", "mc", "unhandledrejection", "t0b"]));
}

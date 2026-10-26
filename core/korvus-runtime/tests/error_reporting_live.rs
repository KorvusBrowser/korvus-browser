//! An uncaught error reaches the page's `ErrorEvent` and the engine's log, NEVER the
//! page's `console`; and a nested browsing context's window carries the realm's whole global surface
//! and receives its own errors. In a live engine, over loopback.
//!
//! # The defects, measured before the fix (master `8f5aee08`, release binary)
//!
//! ```text
//! page wraps console.error; classic/string/syntax/external/timer throws    korvus 4 calls      chrome 0
//! page replaces `console` with its own object; a classic throw             korvus 1 call       chrome 0
//! `preventDefault()` on the ErrorEvent                                      korvus 1 call       chrome 0
//! ErrorEvent.message for `undefined.foo`       korvus "Cannot read properties of undefined (reading 'foo')"
//!                                              chrome "Uncaught TypeError: Cannot read properties of ..."
//! ErrorEvent lineno/colno for thrower.js:3:6   korvus 0/0, filename the DOCUMENT   chrome 3/6, thrower.js
//! <script type=module>throw new Error("mod")   korvus `unhandledrejection`         chrome ErrorEvent
//! <iframe src=fr.html> whose script throws     korvus parent console.error +1,     chrome parent untouched,
//!                                                     frame's `error` never fired         frame's `error` fires
//! typeof contentWindow.{JSON, matchMedia, getComputedStyle, performance}
//!                                              korvus undefined x4                 chrome object,function,function,object
//! cross-origin iframe.contentDocument          korvus the document                 chrome null
//! ```
//!
//! # The oracle
//!
//! Every expectation below is what stock Chrome 150.0.7871.46 (`--headless=new`, attached over CDP)
//! answered for the same markup, over 127.0.0.1, on 2026-09-17. Where this engine's answer is NOT
//! Chrome's, the test says so in a comment and asserts nothing about it -- line numbers of INLINE
//! scripts (Chrome counts from the top of the HTML file; this engine's parser does not record where a
//! script's text began, so it counts from the script's first line).

use engine::Engine;
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The recorder every page starts with: a wrapped `console.error` (what every error-monitoring SDK
/// does, and what the parity collector does), and an `error` / `unhandledrejection` listener that
/// keeps each event's fields.
const HEAD: &str = r#"<script>
var R={ce:0,ceArgs:[],ev:[],rej:[]};
var oce=console.error; console.error=function(){R.ce++;R.ceArgs.push(String(arguments[0]).slice(0,60));return oce.apply(console,arguments)};
addEventListener("error",function(e){R.ev.push({m:e.message,f:String(e.filename).replace(location.origin,""),l:e.lineno,c:e.colno,err:String(e.error),tr:e.isTrusted,ca:e.cancelable,win:e.target===window,ee:e instanceof ErrorEvent})});
addEventListener("unhandledrejection",function(e){R.rej.push(String(e.reason))});
</script>"#;

type Routes = HashMap<String, (&'static str, String)>;

async fn serve(listener: TcpListener, routes: Routes) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let routes = routes.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, ctype, body) = match routes.get(&path) {
                Some((ct, b)) => (200, *ct, b.clone()),
                None => (404, "text/plain", "no".to_string()),
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

async fn listen(routes: Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, routes));
    format!("http://{addr}")
}

fn html(body: &str) -> String {
    format!("<!doctype html><html><head>{HEAD}</head><body>{body}</body></html>")
}

/// Load `/page.html` from `origin`, let timers and modules settle, and return the page's `R` (as
/// JSON), the engine's uncaught log and its unhandled rejections.
async fn run(origin: &str, routes: &Routes) -> (serde_json::Value, Vec<String>, Vec<String>) {
    let doc = routes["/page.html"].1.clone();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(&doc, &format!("{origin}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.take_output();
    e.eval("p10b", "print(JSON.stringify(R))".into()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let r: serde_json::Value = serde_json::from_str(&out[0]).expect("R is JSON");
    (r, e.uncaught_errors(), e.unhandled_rejections())
}

fn page(body: &str) -> Routes {
    let mut r = Routes::new();
    r.insert("/page.html".into(), ("text/html", html(body)));
    r
}

fn ev(r: &serde_json::Value) -> Vec<serde_json::Value> {
    r["ev"].as_array().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// C03: the page's console is the page's
// ---------------------------------------------------------------------------------------------

/// Every kind of uncaught error a page can have reaches its `ErrorEvent` listener with Chrome's
/// message, and NONE of them calls the page's (wrapped) `console.error`.
#[tokio::test]
async fn no_uncaught_error_calls_the_pages_console_error() {
    let mut routes = page(
        r#"<script>undefined.foo</script>
<script>
  // line 2
  throw "str";
</script>
<script>var x = ;</script>
<script>setTimeout(function(){ throw new Error("in timeout") },0)</script>
<script>Promise.reject(new Error("rej"))</script>
<script>reportError(new Error("rep"))</script>
<script src="/thrower.js"></script>"#,
    );
    routes.insert("/thrower.js".into(), ("text/javascript", "// a\n// b\nnull.bar;\n".into()));
    let origin = listen(routes.clone()).await;
    let (r, uncaught, rejections) = run(&origin, &routes).await;

    assert_eq!(r["ce"], 0, "Chrome calls the page's console.error for NO uncaught error: {:?}", r["ceArgs"]);

    let evs = ev(&r);
    let mut messages: Vec<String> = evs.iter().map(|e| e["m"].as_str().unwrap_or("").to_string()).collect();
    messages.sort();
    let mut want = vec![
        "Uncaught TypeError: Cannot read properties of undefined (reading 'foo')",
        "Uncaught str",
        "Uncaught SyntaxError: Unexpected token ';'",
        "Uncaught Error: in timeout",
        "Uncaught Error: rep",
        "Uncaught TypeError: Cannot read properties of null (reading 'bar')",
    ];
    want.sort();
    assert_eq!(messages, want, "Chrome 150's ErrorEvent messages: {evs:?}");
    for e in &evs {
        assert_eq!((e["tr"].as_bool(), e["ca"].as_bool(), e["win"].as_bool(), e["ee"].as_bool()), (Some(true), Some(true), Some(true), Some(true)), "a trusted, cancelable ErrorEvent at window: {e}");
    }

    // An EXTERNAL script's site is exact: its own URL, its own line and column (Chrome: 3 / 6).
    let ext = evs.iter().find(|e| e["m"].as_str().unwrap_or("").contains("reading 'bar'")).unwrap();
    assert_eq!((ext["f"].as_str(), ext["l"].as_u64(), ext["c"].as_u64()), (Some("/thrower.js"), Some(3), Some(6)), "{ext}");
    // A thrown PRIMITIVE still has a site (the TryCatch's), and `error` is the primitive itself.
    let s = evs.iter().find(|e| e["m"] == "Uncaught str").unwrap();
    assert_eq!(s["err"], "str");
    assert!(s["l"].as_u64().unwrap_or(0) > 0, "a thrown string has a line: {s}");
    // An Error handed to reportError is sited where it was CONSTRUCTED -- nothing threw it -- and one THROWN from
    // a timer where the `throw` ran: Chrome answers the column of `new Error` for the first (21) and of `throw` for
    // the second (32), counted in the HTML. The 38 this asserted for the timer was the construction too, measured
    // through a DevTools client that enabled the Runtime domain before load, which makes V8 capture stacks for
    // uncaught exceptions and Blink prefer them (stock Chrome 150 without it reports the throw).
    let rep = evs.iter().find(|e| e["m"] == "Uncaught Error: rep").unwrap();
    assert_eq!((rep["f"].as_str(), rep["c"].as_u64()), (Some("/page.html"), Some(21)), "{rep}");
    let tmo = evs.iter().find(|e| e["m"] == "Uncaught Error: in timeout").unwrap();
    assert_eq!((tmo["f"].as_str(), tmo["c"].as_u64()), (Some("/page.html"), Some(32)), "{tmo}");

    // The engine's log -- `korvus fetch --errors`, this engine's DevTools -- still has every one.
    assert_eq!(uncaught.len(), 6, "each uncaught error is still logged: {uncaught:?}");
    // And a rejection is a rejection, not an error.
    assert_eq!(r["rej"], serde_json::json!(["Error: rej"]));
    assert_eq!(rejections.len(), 1, "{rejections:?}");
}

/// A page that REPLACES `console` with its own object: Chrome calls nothing on it.
#[tokio::test]
async fn a_replaced_console_object_is_never_called() {
    let routes = page(
        r#"<script>window.console={error:function(){R.ce++;R.ceArgs.push("replaced:"+arguments[0])},log:function(){},warn:function(){},info:function(){},debug:function(){}};</script>
<script>undefined.foo</script>
<script>setTimeout(function(){ throw new Error("t2") },0)</script>"#,
    );
    let origin = listen(routes.clone()).await;
    let (r, uncaught, _) = run(&origin, &routes).await;
    assert_eq!(r["ce"], 0, "{:?}", r["ceArgs"]);
    assert_eq!(ev(&r).len(), 2);
    assert_eq!(uncaught.len(), 2);
}

/// `preventDefault()` on the ErrorEvent: `window.onerror` still sees Chrome's five arguments, the page's
/// console is untouched, and the error leaves the log (Chrome: no `Runtime.exceptionThrown`).
#[tokio::test]
async fn a_cancelled_error_event_leaves_the_log_and_never_the_console() {
    let routes = page(
        r#"<script>addEventListener("error",function(e){e.preventDefault()});window.onerror=function(m,f,l,c,e){R.onerr=[m,String(f).replace(location.origin,""),String(e)];};</script>
<script>undefined.foo</script>"#,
    );
    let origin = listen(routes.clone()).await;
    let (r, uncaught, _) = run(&origin, &routes).await;
    assert_eq!(r["ce"], 0, "{:?}", r["ceArgs"]);
    assert_eq!(
        r["onerr"],
        serde_json::json!([
            "Uncaught TypeError: Cannot read properties of undefined (reading 'foo')",
            "/page.html",
            "TypeError: Cannot read properties of undefined (reading 'foo')"
        ])
    );
    assert!(uncaught.is_empty(), "a cancelled error is not logged: {uncaught:?}");
}

/// A module script that throws is an UNCAUGHT EXCEPTION -- an `ErrorEvent` at window with its own
/// site -- and not an unhandled rejection, which is what deno_core's re-raise made it.
#[tokio::test]
async fn a_throwing_module_is_an_error_event_not_a_rejection() {
    let mut routes = page(
        r#"<script type=module>
throw new Error("mod")
</script>
<script type=module src="/mod.js"></script>
<script type=module>
Promise.reject(new Error("own rejection"));
await 0;
throw new Error("after await")
</script>"#,
    );
    routes.insert("/mod.js".into(), ("text/javascript", "export const a = 1;\nnull.baz;\n".into()));
    let origin = listen(routes.clone()).await;
    let (r, uncaught, rejections) = run(&origin, &routes).await;
    assert_eq!(r["ce"], 0, "{:?}", r["ceArgs"]);
    let evs = ev(&r);
    let mut messages: Vec<String> = evs.iter().map(|e| e["m"].as_str().unwrap_or("").to_string()).collect();
    messages.sort();
    assert_eq!(
        messages,
        vec![
            "Uncaught Error: after await".to_string(),
            "Uncaught Error: mod".to_string(),
            "Uncaught TypeError: Cannot read properties of null (reading 'baz')".to_string(),
        ],
        "{evs:?}"
    );
    let ext = evs.iter().find(|e| e["m"].as_str().unwrap_or("").contains("'baz'")).unwrap();
    assert_eq!((ext["f"].as_str(), ext["l"].as_u64(), ext["c"].as_u64()), (Some("/mod.js"), Some(2), Some(6)), "{ext}");
    // The page's OWN rejection inside a module is still a rejection -- the re-raise is told apart by
    // identity, not by timing.
    assert_eq!(r["rej"], serde_json::json!(["Error: own rejection"]));
    assert_eq!(rejections.len(), 1, "{rejections:?}");
    assert_eq!(uncaught.len(), 3, "{uncaught:?}");
}

// ---------------------------------------------------------------------------------------------
// C04: a frame's window is a realm's window, and its errors are its own
// ---------------------------------------------------------------------------------------------

const FRAME: &str = r#"<!doctype html><html><body><script>
var R2={ev:[],ce:0};
var oce=console.error;console.error=function(){R2.ce++;return oce.apply(console,arguments)};
addEventListener("error",function(e){R2.ev.push([e.message,String(e.filename).replace(location.origin,"")])});
</script>
<script>undefined.foo</script>
<script>setTimeout(function(){ throw new Error("frame timer") },0)</script>
<script>R2.types=[typeof JSON,typeof matchMedia,typeof getComputedStyle,typeof performance];R2.self=[window.parent===parent,top===parent,self===window]</script>
</body></html>"#;

#[tokio::test]
async fn a_frames_errors_are_its_own_and_its_window_has_the_realms_globals() {
    // The cross-origin frame is served from a SECOND listener: another port is another origin.
    let mut other = Routes::new();
    other.insert("/fr2.html".into(), ("text/html", FRAME.to_string()));
    let other_origin = listen(other).await;

    let mut routes = page(&format!(
        r#"<iframe id=a name=fa src="/fr.html"></iframe>
<iframe id=b srcdoc="<script>var S=[typeof JSON,typeof matchMedia,typeof getComputedStyle,typeof performance]</script>"></iframe>
<iframe id=c></iframe>
<iframe id=x src="{other_origin}/fr2.html"></iframe>
<script>
function types(w){{return [typeof w.JSON,typeof w.matchMedia,typeof w.getComputedStyle,typeof w.performance,typeof w.Math,typeof w.Array,typeof w.fetch,typeof w.localStorage,typeof w.document,typeof w.customElements,typeof w.requestAnimationFrame,typeof w.atob]}}
addEventListener("load",function(){{
  var a=document.getElementById("a"), b=document.getElementById("b"), c=document.getElementById("c"), x=document.getElementById("x");
  R.frameTypes=types(a.contentWindow);
  R.blankTypes=types(c.contentWindow);
  R.srcdocTypes=types(b.contentWindow);
  R.srcdocInner=b.contentWindow.S;
  R.ident=[a.contentWindow===a.contentWindow, frames[0]===a.contentWindow, frames.length, a.contentWindow.parent===window, a.contentWindow.top===window, a.contentWindow.frameElement===a, a.contentWindow.window===a.contentWindow, a.contentWindow!==window, a.contentWindow.console!==console, a.contentWindow.opener, a.contentWindow.name, c.contentWindow.name, window.name];
  R.xorigDoc=x.contentDocument===null;
  R.inner=a.contentWindow.R2;
  R.pageVar=typeof a.contentWindow.R;
  R.gcs=typeof a.contentWindow.getComputedStyle(a.contentDocument.body).display;
  R.mm=typeof a.contentWindow.matchMedia("(min-width: 1px)").matches;
  R.perf=typeof a.contentWindow.performance.now();
}});
</script>"#
    ));
    routes.insert("/fr.html".into(), ("text/html", FRAME.to_string()));
    let origin = listen(routes.clone()).await;
    let (r, uncaught, _) = run(&origin, &routes).await;

    // The PARENT is untouched: no console call, no error event.
    assert_eq!(r["ce"], 0, "a frame's error never reaches the parent's console: {:?}", r["ceArgs"]);
    assert_eq!(ev(&r).len(), 0, "a frame's error is not the parent's error event: {:?}", r["ev"]);

    // The FRAME got both of its errors, at its own window -- and its own console was not called either.
    let inner = &r["inner"];
    assert_eq!(inner["ce"], 0, "{inner}");
    let mut got: Vec<String> = inner["ev"].as_array().unwrap().iter().map(|e| e[0].as_str().unwrap().to_string()).collect();
    got.sort();
    assert_eq!(got, vec!["Uncaught Error: frame timer", "Uncaught TypeError: Cannot read properties of undefined (reading 'foo')"], "{inner}");
    for e in inner["ev"].as_array().unwrap() {
        assert_eq!(e[1], "/fr.html", "a frame script's error names the frame's document: {e}");
    }
    assert!(uncaught.iter().any(|u| u.contains("reading 'foo'")), "the engine's log keeps a frame's error: {uncaught:?}");

    // The window surface, Chrome 150's answer for a src frame, an empty frame and a srcdoc frame alike.
    let want = serde_json::json!(["object", "function", "function", "object", "object", "function", "function", "object", "object", "object", "function", "function"]);
    assert_eq!(r["frameTypes"], want);
    assert_eq!(r["blankTypes"], want);
    assert_eq!(r["srcdocTypes"], want);
    assert_eq!(r["srcdocInner"], serde_json::json!(["object", "function", "function", "object"]));
    assert_eq!(inner["types"], serde_json::json!(["object", "function", "function", "object"]));
    assert_eq!(inner["self"], serde_json::json!([true, true, true]));
    assert_eq!(r["ident"], serde_json::json!([true, true, 4, true, true, true, true, true, true, null, "fa", "", ""]));
    // A page's own global is the PAGE's: Chrome's frame does not see `var R`.
    assert_eq!(r["pageVar"], "undefined");
    // The members answer when called on the frame's window.
    assert_eq!((r["gcs"].as_str(), r["mm"].as_str(), r["perf"].as_str()), (Some("string"), Some("boolean"), Some("number")));
    // Another port is another origin: `contentDocument` is null.
    assert_eq!(r["xorigDoc"], true);
}

/// `iframe.src = URL.createObjectURL(blob)` is ONE navigation with ONE `load`, and the Blob's document
/// is same-origin to the page that minted it.
///
/// ```text
///                                        korvus (8f5aee08)                     chrome 150
/// load events after the src write        2: "<p id="x">hello</p>", then ""     1: "<p id="x">hello</p>"
/// ```
///
/// The second was the subresource loader's own fetch of the `blob:` URL, which fails and ended in an
/// empty document. WPT's `html5lib_url.html` reuses one iframe per test and sets the next test's
/// `onload` straight after, so the stale `load` decided `?file=scripted_adoption01` and
/// `?file=pending-spec-changes-plain-text-unsafe` by timing. (Chrome also fires a `load` when the
/// empty frame is APPENDED; this engine does not, and this test does not count that one.)
#[tokio::test]
async fn a_blob_src_navigates_a_frame_once() {
    let routes = page(
        r#"<script>
var f=document.createElement("iframe");
document.body.appendChild(f);
R.loads=[];
setTimeout(function(){
  var url=URL.createObjectURL(new Blob(['<p id=x>hello</p>'],{type:"text/html"}));
  f.addEventListener("load",function(){ R.loads.push(f.contentDocument===null?"NULLDOC":f.contentDocument.body.innerHTML); });
  f.src=url;
},50);
</script>"#,
    );
    let origin = listen(routes.clone()).await;
    let (r, _, _) = run(&origin, &routes).await;
    assert_eq!(r["loads"], serde_json::json!(["<p id=\"x\">hello</p>"]), "one navigation, one load, a readable document");
}

/// THE PAGE'S VIEW OF A CROSS-ORIGIN FRAME (korvus-analysis U-7, an interactive challenge widget posting into its
/// challenge frame). Chrome 150, measured with a frame on another origin and one on the page's own:
///
/// - `frames[0] === iframe.contentWindow`, and `frames[0].location.href` is a SecurityError -- korvus's
///   `window[0]` held the frame's RAW window: another object, and its URL readable;
/// - the cross-origin methods and the `location` setter are functions of the CALLER's realm
///   (`instanceof Function`, the page's `Function.prototype`) and keep their identity -- HTML's
///   cross-origin property map; korvus minted them in whatever realm asked first;
/// - a same-origin frame's own `postMessage` is the frame's (`instanceof Function` false).
#[tokio::test]
async fn a_cross_origin_frame_is_the_pages_cross_origin_window_proxy() {
    let mut other = Routes::new();
    other.insert("/x.html".into(), ("text/html", "<!doctype html><title>x</title>".to_string()));
    let other_origin = listen(other).await;
    let mut routes = page(&format!(
        r#"<iframe id=x src="{other_origin}/x.html"></iframe><iframe id=s src="/s.html"></iframe>
<script>
addEventListener("load",function(){{
  var x=document.getElementById("x").contentWindow, s=document.getElementById("s").contentWindow;
  var d=Object.getOwnPropertyDescriptor(x,"location");
  R.xo=[frames[0]===x, frames[1]===s, (function(){{try{{return frames[0].location.href}}catch(e){{return e.name}}}})(),
        x.postMessage instanceof Function, Object.getPrototypeOf(x.close)===Function.prototype,
        x.focus instanceof Function, d.set instanceof Function, x.postMessage===frames[0].postMessage,
        s.postMessage instanceof Function];
}});
</script>"#
    ));
    routes.insert("/s.html".into(), ("text/html", "<!doctype html><title>s</title>".to_string()));
    let origin = listen(routes.clone()).await;
    let (r, _, _) = run(&origin, &routes).await;
    assert_eq!(r["xo"], serde_json::json!([true, true, "SecurityError", true, true, true, true, true, false]));
}

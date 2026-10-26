//! Request interception (`Fetch.*`) and the DevTools cookie commands, as Chrome 150 answers them.
//!
//! Puppeteer's `setRequestInterception` + `respond`, Playwright's `route` + `fulfill`, and Puppeteer's `page.cookies()`
//! all ran into `-32601` here. `fixtures/fetch-domain.chrome-150.json` holds stock Chrome 150.0.7871.46's answers
//! (headless, a fresh profile, a raw CDP client: `~/bench-lean/cdp/probe/a1s3*_record.mjs`, 2026-10-12); each test
//! below pins what it names from there.
//!
//! What makes these work at all is graded first: a paused request is answered while the page's loop is busy -- the
//! document request while `Page.navigate` is waiting for it, and a `fetch()` while `Runtime.evaluate` is awaiting its
//! promise -- and each pause comes after its own `Network.requestWillBeSent`, under the same id (`networkId`), which is
//! how both clients pair the two.
//!
//! KNOWN, not graded: the header VALUES of a pause (they are the persona's, not a headless Chrome's) and the order of
//! `Network.getCookies` (Chrome's is a hash map's).
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const CHROME: &str = include_str!("fixtures/fetch-domain.chrome-150.json");

const PAGE: &str = "<!doctype html><title>fetch domain</title><div id=out></div>\
<script>fetch('/data.json').then(r => r.text()).then(t => { out.textContent = JSON.parse(t).v; out.dataset.done = 1; });</script>";

/// `/page.html` fetches `/data.json`; `/cookies` sets an HttpOnly cookie; the JSON answers say which one answered.
async fn fixture_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (ctype, extra, body) = match path.as_str() {
                    "/page.html" => ("text/html", "", PAGE.to_string()),
                    "/cookies" => ("text/html", "Set-Cookie: http_c=2; Path=/; HttpOnly\r\n", "<!doctype html><title>cookies</title>".to_string()),
                    "/data.json" => ("application/json", "", r#"{"v":"net"}"#.to_string()),
                    "/other.json" => ("application/json", "", r#"{"v":"other"}"#.to_string()),
                    _ => ("text/plain", "", "plain".to_string()),
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n{extra}Cache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    port
}

/// How the client answers a pause: the commands to send for it (none: leave it paused).
type Rule = Box<dyn FnMut(&Value) -> Vec<(String, Value)>>;

struct Client {
    ws: ClientWs,
    next: i64,
    events: Vec<Value>,
    answers: HashMap<i64, Value>,
    /// The answers to the commands the rule sent, in the order they arrived.
    rule_answers: Vec<Value>,
    rule_ids: Vec<i64>,
    rule: Rule,
}

impl Client {
    async fn send(&mut self, method: &str, params: Value) -> i64 {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        id
    }

    /// Read one message: an answer is kept, an event recorded, and a pause answered by the rule.
    async fn pump(&mut self, wait: Duration) {
        let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(wait, self.ws.next()).await else { return };
        let v: Value = serde_json::from_str(&t).unwrap();
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            if self.rule_ids.contains(&id) {
                self.rule_answers.push(v);
            } else {
                self.answers.insert(id, v);
            }
            return;
        }
        if v["method"] == "Fetch.requestPaused" {
            for (method, params) in (self.rule)(&v["params"]) {
                let id = self.send(&method, params).await;
                self.rule_ids.push(id);
            }
        }
        self.events.push(v);
    }

    /// One command, its whole answer; pauses are answered meanwhile.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Some(v) = self.answers.remove(&id) {
                return v;
            }
            self.pump(Duration::from_millis(200)).await;
        }
        panic!("no answer to {method} within 30 s; events: {:?}", self.events.iter().map(|e| e["method"].clone()).collect::<Vec<_>>());
    }

    async fn wait_for(&mut self, pred: impl Fn(&Value) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && !self.events.iter().any(&pred) {
            self.pump(Duration::from_millis(200)).await;
        }
    }

    async fn eval(&mut self, expr: &str) -> Value {
        let r = self.call("Runtime.evaluate", json!({ "expression": expr, "awaitPromise": true, "returnByValue": true })).await;
        r["result"]["result"]["value"].clone()
    }
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

fn continue_rest(p: &Value) -> Vec<(String, Value)> {
    vec![("Fetch.continueRequest".to_string(), json!({ "requestId": p["requestId"] }))]
}

/// Serve one CDP connection on a fresh engine while `client` runs against it.
async fn with_page<F, Fut>(rule: Rule, client: F)
where
    F: FnOnce(Client, u16) -> Fut,
    Fut: std::future::Future<Output = Client>,
{
    let port = fixture_server().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let run = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let c = Client { ws, next: 0, events: Vec::new(), answers: HashMap::new(), rule_answers: Vec::new(), rule_ids: Vec::new(), rule };
        let mut c = client(c, port).await;
        let _ = c.ws.close(None).await;
    };
    let (served, ()) = tokio::join!(server, run);
    served.expect("the connection is served");
}

#[tokio::test]
async fn a_paused_navigation_and_fetch_are_answered_while_the_page_waits() {
    // The ledger, the cookie store and the interceptor are process-global: a navigating test takes its turn.
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    transport::cookies::clear();
    let chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    let rule: Rule = Box::new(|p: &Value| {
        if p["request"]["url"].as_str().unwrap_or("").ends_with("/data.json") {
            let body = "eyJ2IjoibW9ja2VkIn0="; // {"v":"mocked"}
            vec![(
                "Fetch.fulfillRequest".to_string(),
                json!({ "requestId": p["requestId"], "responseCode": 200, "body": body, "responseHeaders": [
                    { "name": "Content-Type", "value": "application/json" },
                    { "name": "Set-Cookie", "value": "from_fulfill=1; Path=/" } ] }),
            )]
        } else {
            continue_rest(p)
        }
    });
    with_page(rule, |mut c, port| async move {
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Network.enable", json!({})).await;
        assert_eq!(c.call("Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] })).await["result"], json!({}));
        // `Page.navigate` is answered only once its paused document request has been continued.
        let nav = c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/page.html") })).await;
        let loader = nav["result"]["loaderId"].as_str().expect("the navigation committed").to_string();
        c.wait_for(|e| e["method"] == "Page.loadEventFired").await;
        // ... and the page's `fetch()` was fulfilled while this client waited on an evaluate.
        let shown = c.eval("new Promise(r => { const t = () => out.dataset.done ? r(out.textContent) : setTimeout(t, 20); t(); })").await;
        assert_eq!(shown, json!("mocked"), "the page read the fulfilled body");

        let paused: Vec<&Value> = c.events.iter().filter(|e| e["method"] == "Fetch.requestPaused").collect();
        assert!(paused.len() >= 2, "the document and the fetch were paused: {paused:?}");
        let doc = &paused[0]["params"];
        let d = &chrome["paused_document"];
        assert_eq!(json!(keys(doc)), d["keys"], "the pause's keys, in Chrome's order");
        assert_eq!(json!(keys(&doc["request"])), d["request_keys"], "its request's keys");
        assert_eq!(doc["resourceType"], d["resourceType"]);
        assert_eq!(doc["requestId"], d["requestId"], "the first job");
        assert_eq!(doc["networkId"], json!(loader), "a navigation's networkId is its loader id, as Chrome's");
        let names = keys(&doc["request"]["headers"]);
        let mut sorted = names.clone();
        sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(names, sorted, "the pause lists its headers sorted, as DevTools' dictionary does");
        for n in ["Accept", "Upgrade-Insecure-Requests", "User-Agent"] {
            assert!(names.iter().any(|h| h == n), "a document's pause shows {n}, as Chrome's: {names:?}");
        }
        for n in ["Host", "Connection", "Accept-Encoding", "Accept-Language", "Sec-Fetch-Site", "Sec-Fetch-Mode", "Sec-Fetch-Dest", "Cookie"] {
            assert!(!names.iter().any(|h| h.eq_ignore_ascii_case(n)), "{n} is added after the pause, as in Chrome: {names:?}");
        }

        let fetch = paused.iter().map(|e| &e["params"]).find(|p| p["request"]["url"].as_str().unwrap_or("").ends_with("/data.json")).expect("the fetch's pause");
        assert_eq!(json!(keys(fetch)), chrome["paused_fetch"]["keys"]);
        assert_eq!(fetch["resourceType"], chrome["paused_fetch"]["resourceType"], "a fetch() pauses as XHR, as Chrome's");
        // each pause pairs with its own requestWillBeSent, which went out first
        for p in &paused {
            let id = p["params"]["networkId"].clone();
            let at = c.events.iter().position(|e| e == *p).unwrap();
            let sent = c.events.iter().position(|e| e["method"] == "Network.requestWillBeSent" && e["params"]["requestId"] == id);
            assert!(sent.is_some_and(|s| s < at), "requestWillBeSent {id} precedes its pause");
        }

        // a fulfilled response's Set-Cookie reaches the store and the document, as Chrome's did
        assert_eq!(chrome["fulfill_set_cookie"]["storeHasIt"], json!(true));
        let cookie = c.eval("document.cookie").await;
        assert!(cookie.as_str().unwrap_or("").contains("from_fulfill=1"), "document.cookie {cookie}");
        let listed = c.call("Network.getCookies", json!({})).await;
        assert!(listed["result"]["cookies"].as_array().unwrap().iter().any(|k| k["name"] == "from_fulfill"));
        c.call("Fetch.disable", json!({})).await;
        c
    })
    .await;
}

#[tokio::test]
async fn fail_overrides_and_refusals_are_chromes() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    let rule: Rule = Box::new(|p: &Value| {
        let url = p["request"]["url"].as_str().unwrap_or("").to_string();
        let id = p["requestId"].clone();
        if url.ends_with("/fail.json") {
            vec![("Fetch.failRequest".into(), json!({ "requestId": id, "errorReason": "BlockedByClient" }))]
        } else if url.ends_with("/same.json") {
            vec![("Fetch.continueRequest".into(), json!({ "requestId": id, "url": url.replace("/same.json", "/data.json") }))]
        } else if url.ends_with("/cross.json") {
            vec![("Fetch.continueRequest".into(), json!({ "requestId": id, "url": "ftp://example.com/x" }))]
        } else if url.ends_with("/twice.json") {
            vec![("Fetch.continueRequest".into(), json!({ "requestId": id.clone() })), ("Fetch.continueRequest".into(), json!({ "requestId": id }))]
        } else if url.ends_with("/f201.json") {
            vec![("Fetch.fulfillRequest".into(), json!({ "requestId": id, "responseCode": 201, "responseHeaders": [{ "name": "X-A", "value": "1" }], "body": "b25l" }))]
        } else if url.ends_with("/f299.json") {
            vec![
                ("Fetch.fulfillRequest".into(), json!({ "requestId": id.clone(), "responseCode": 299, "body": "" })),
                ("Fetch.continueRequest".into(), json!({ "requestId": id })),
            ]
        } else {
            continue_rest(p)
        }
    });
    with_page(rule, |mut c, port| async move {
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Network.enable", json!({})).await;
        let r = &chrome["refusals"];
        let not_enabled = c.call("Fetch.continueRequest", json!({ "requestId": "interception-job-99.0" })).await;
        assert_eq!(not_enabled["error"], r["continue_when_disabled"]);
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/cookies") })).await;
        c.wait_for(|e| e["method"] == "Page.loadEventFired").await;
        // the document is not paused: the patterns name JSON only
        c.call("Fetch.enable", json!({ "patterns": [{ "urlPattern": "*.json" }] })).await;
        let unknown = c.call("Fetch.continueRequest", json!({ "requestId": "interception-job-99.0" })).await;
        assert_eq!(unknown["error"], r["continue_unknown_id"]);
        let bad_reason = c.call("Fetch.failRequest", json!({ "requestId": "interception-job-99.0", "errorReason": "Nope" })).await;
        assert_eq!(bad_reason["error"], r["fail_unknown_reason"]);

        let failed = c.eval("fetch('/fail.json').then(r => 'status ' + r.status).catch(e => 'rejected ' + e)").await;
        assert_eq!(failed, chrome["fail_blocked_by_client"]["page"]);
        c.wait_for(|e| e["method"] == "Network.loadingFailed").await;
        let lf = c.events.iter().find(|e| e["method"] == "Network.loadingFailed").unwrap();
        let want = &chrome["fail_blocked_by_client"]["loadingFailed"];
        assert_eq!((&lf["params"]["errorText"], &lf["params"]["blockedReason"]), (&want["errorText"], &want["blockedReason"]));

        let same = c.eval("fetch('/same.json').then(r => r.text().then(t => r.url.endsWith('/same.json') + ' ' + r.redirected + ' ' + t))").await;
        // Chrome 150 (`override_same_scheme`): the page read the url it asked for, `redirected` false, the new url's body
        assert!(chrome["override_same_scheme"].as_str().unwrap().contains("/other.json false"));
        assert_eq!(same, json!(r#"true false {"v":"net"}"#), "a same-scheme override is invisible to the page");
        let cross = c.eval("fetch('/cross.json').then(r => 'status ' + r.status).catch(e => 'rejected ' + e)").await;
        assert_eq!(cross, chrome["override_cross_scheme"]["page"]);

        c.eval("fetch('/twice.json').then(r => r.status)").await;
        let f201 = c.eval("fetch('/f201.json').then(r => r.status + '|' + r.statusText + '|' + r.headers.get('x-a') + '|' + r.headers.get('content-type'))").await;
        assert_eq!(f201, json!(chrome["fulfill_page"]["code_201_no_phrase"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>().join("|")));
        c.eval("fetch('/f299.json').then(r => r.status)").await;
        // the rule's answers: cross-scheme continue `{}`; the second continue of /twice.json; the 299 refusal
        let errors: Vec<&Value> = c.rule_answers.iter().filter(|a| a.get("error").is_some()).map(|a| &a["error"]).collect();
        assert!(errors.contains(&&r["second_answer"]), "a second answer: {errors:?}");
        assert!(errors.contains(&&r["fulfill_code_without_phrase"]), "a code with no phrase: {errors:?}");
        assert_eq!(chrome["override_cross_scheme"]["answer"], json!({}));

        c.call("Fetch.disable", json!({})).await;
        let after = c.call("Fetch.continueRequest", json!({ "requestId": "interception-job-1.0" })).await;
        assert_eq!(after["error"], r["continue_after_disable"]);
        c
    })
    .await;
}

#[tokio::test]
async fn cookies_read_and_write_as_chromes() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    transport::cookies::clear();
    let chrome: Value = serde_json::from_str(CHROME).expect("the recording is JSON");
    with_page(Box::new(continue_rest), |mut c, port| async move {
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/cookies") })).await;
        c.wait_for(|e| e["method"] == "Page.loadEventFired").await;
        c.eval("document.cookie = 'script_c=1; path=/'").await;
        let listed = c.call("Network.getCookies", json!({})).await;
        let cookies = listed["result"]["cookies"].as_array().expect("cookies").clone();
        let mut names: Vec<&str> = cookies.iter().map(|k| k["name"].as_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["http_c", "script_c"], "DevTools sees the HttpOnly cookie too");
        for k in &cookies {
            assert_eq!(json!(keys(k)), chrome["cookie_keys"], "a Network.Cookie's keys, in Chrome's order");
            assert_eq!((&k["sourceScheme"], &k["sourcePort"]), (&json!("NonSecure"), &json!(port)), "set from this page");
        }
        let http = cookies.iter().find(|k| k["name"] == "http_c").unwrap();
        let want = chrome["cookies_page"]["networkGetCookies"]["cookies"].as_array().unwrap().iter().find(|k| k["name"] == "http_c").unwrap().clone();
        for f in ["value", "path", "expires", "size", "httpOnly", "secure", "session", "priority"] {
            assert_eq!(http[f], want[f], "http_c.{f}");
        }

        // writes: Chrome's answers, and a cookie set by URL lands on `/` with the scheme's port
        let set = &chrome["set_cookie"];
        let by_url = c.call("Network.setCookie", json!({ "name": "u1", "value": "v1", "url": format!("http://127.0.0.1:{port}/deep/path/x.html") })).await;
        assert_eq!(by_url["result"], set["byUrl"]);
        let none = c.call("Network.setCookie", json!({ "name": "n", "value": "v" })).await;
        assert_eq!(none["error"], set["none"]["error"]);
        let bad_scheme = c.call("Network.setCookie", json!({ "name": "n", "value": "v", "url": "ftp://x/" })).await;
        assert_eq!(bad_scheme["error"], set["badScheme"]["error"]);
        let bad_name = c.call("Network.setCookie", json!({ "name": "a;b", "value": "v", "url": format!("http://127.0.0.1:{port}/") })).await;
        assert_eq!(bad_name["error"], set["badName"]["error"]);
        let many_bad = c.call("Network.setCookies", json!({ "cookies": [{ "name": "m3", "value": "3", "url": format!("http://127.0.0.1:{port}/") }, { "name": "m4", "value": "4" }] })).await;
        assert_eq!(many_bad["error"], set["manyBad"]["error"]);
        let all = c.call("Storage.getCookies", json!({})).await;
        let all = all["result"]["cookies"].as_array().unwrap().clone();
        let u1 = all.iter().find(|k| k["name"] == "u1").expect("u1 stored");
        assert_eq!((&u1["path"], &u1["sourcePort"]), (&json!("/"), &json!(80)), "the URL's path is not the cookie's, its port the scheme's (Chrome: `/`, 80)");
        assert!(!all.iter().any(|k| k["name"] == "m3"), "a refused list stores none of its cookies, as Chrome's");

        let no_target = c.call("Network.deleteCookies", json!({ "name": "u1" })).await;
        assert_eq!(no_target["error"], chrome["delete_cookies"]["noTarget"]["error"]);
        c.call("Network.deleteCookies", json!({ "name": "u1", "url": format!("http://127.0.0.1:{port}/other") })).await;
        let all = c.call("Storage.getCookies", json!({})).await;
        assert!(!all["result"]["cookies"].as_array().unwrap().iter().any(|k| k["name"] == "u1"), "deleted by its URL's host");
        c.call("Storage.clearCookies", json!({})).await;
        let all = c.call("Storage.getCookies", json!({})).await;
        assert_eq!(all["result"]["cookies"], json!([]));
        c
    })
    .await;
}

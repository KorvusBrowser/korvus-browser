//! THE `Fetch` DOMAIN: request interception, served BESIDE the page.
//!
//! Chrome serves `Fetch` in the browser process, in front of the network service, so a client can answer a paused
//! request while the page is busy -- Puppeteer's `page.goto` waits for `Page.navigate` while its request handler
//! answers the paused document request, and `page.waitForSelector` waits for a `Runtime.callFunctionOn` whose promise
//! only settles once a paused `fetch()` has been fulfilled. This server answers commands one at a time, inside the
//! page's own loop, so a paused request would wait for an answer that loop is not reading.
//!
//! So the connection is split as Chrome's is:
//! - [`Out`]: the write half, shared. The page's loop and this domain both write, a frame at a time, in the order
//!   they sent.
//! - [`read_socket`]: the read half, in a task of its own. It answers every `Fetch.*` command itself and hands
//!   everything else to the page's loop ([`Inbox`]).
//! - [`announce`]: a task that reports each paused request. It sends the request's own `Network.*` events first, so a
//!   client that pairs `Fetch.requestPaused` with `Network.requestWillBeSent` by `networkId` (Playwright pairs them
//!   and continues an unpaired pause untouched) always has both.
//!
//! The transport side is `transport::intercept`; its module comment says what each answer does to the request.
//! Shapes, answers and messages are stock Chrome 150's, recorded (`~/bench-lean/cdp/probe/chrome150-a1s3*.json`).
//!
//! Authentication (`handleAuthRequests`): a `401`/`407` challenge is held for the client as `Fetch.authRequired`,
//! and `Fetch.continueWithAuth` answers it -- credentials send the request again with them (Playwright's
//! `httpCredentials`), anything else lets the challenge's own response through, as a headless browser with no dialog
//! does.
//!
//! NOT DONE: the response stage (`requestStage: "Response"`, `Fetch.continueResponse`, `Fetch.getResponseBody`): no
//! request is ever paused there, so those commands answer as Chrome does for a request that was not.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use transport::intercept::{AuthAnswer, AuthChallenge, AuthFuture, Fulfilment, Overrides, PauseFuture, Paused, PausedRequest};

use crate::network;

type Sink = futures_util::stream::SplitSink<WebSocketStream<TcpStream>, Message>;
type Source = futures_util::stream::SplitStream<WebSocketStream<TcpStream>>;
type Incoming = Result<Message, tokio_tungstenite::tungstenite::Error>;

/// The write half of a connection, shared by the page's loop and the `Fetch` domain. A frame is written whole before
/// the next sender's, and [`Out::send`] returns once it is on the socket -- as a write on the sink itself did, which
/// is what lets `Page.navigate`'s answer leave before a long page script runs.
#[derive(Clone)]
pub(crate) struct Out(Arc<tokio::sync::Mutex<Sink>>);

impl Out {
    pub(crate) fn new(sink: Sink) -> Out {
        Out(Arc::new(tokio::sync::Mutex::new(sink)))
    }

    pub(crate) async fn send(&mut self, m: Message) -> anyhow::Result<()> {
        self.0.lock().await.send(m).await?;
        Ok(())
    }

    /// Several frames, with nothing of another sender's between them.
    pub(crate) async fn send_all(&self, ms: Vec<Message>) -> anyhow::Result<()> {
        let mut sink = self.0.lock().await;
        for m in ms {
            sink.send(m).await?;
        }
        Ok(())
    }
}

/// The page loop's side of the read half: every message [`read_socket`] did not answer itself, in order.
pub(crate) struct Inbox(tokio::sync::mpsc::UnboundedReceiver<Incoming>);

impl futures_util::Stream for Inbox {
    type Item = Incoming;
    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Incoming>> {
        self.0.poll_recv(cx)
    }
}

/// The connection's reporting (`network::Reporting`), shared: the page's loop reports what its navigations and
/// commands caused, and [`announce`] reports a paused request's events ahead of its pause.
pub(crate) type Rep = Arc<Mutex<network::Reporting>>;

/// Lock the reporting. Never held across an `await`.
pub(crate) fn rep_lock(rep: &Rep) -> std::sync::MutexGuard<'_, network::Reporting> {
    rep.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every `Network.*` event `rep` has not reported, each with the session it goes to (the page's: `page_session`).
pub(crate) fn flushed_messages(rep: &Rep, page_session: Option<&str>) -> Vec<Message> {
    let rows = transport::ledger().records();
    let targets = transport::ledger().targets();
    let events = rep_lock(rep).flush(&rows, &targets, crate::ids::hex32);
    events
        .into_iter()
        .map(|(stamp, ev)| {
            let session = match &stamp {
                network::Stamp::Page => page_session,
                network::Stamp::Session(s) => Some(s.as_str()),
            };
            stamped(ev, session)
        })
        .collect()
}

/// What each worker whose session enabled `Runtime` has logged since the last call, as `Runtime.consoleAPICalled` on
/// that session, in the worker realm's context.
pub(crate) fn worker_console_messages(rep: &Rep) -> Vec<Message> {
    let children = rep_lock(rep).runtime_children();
    let mut out = Vec::new();
    // a worker that has run its script says so on its session, as Chrome's does: Playwright evaluates in it only then
    for session in rep_lock(rep).take_script_loaded(engine::worker_devtools::script_loaded) {
        out.push(stamped(json!({ "method": "Inspector.workerScriptLoaded", "params": {} }), Some(&session)));
    }
    for (idx, session, target) in children {
        for mut params in webapi::devtools_log::take_worker_console(&target) {
            params["executionContextId"] = json!(crate::worker_context_id(idx));
            out.push(stamped(json!({ "method": "Runtime.consoleAPICalled", "params": params }), Some(&session)));
        }
    }
    out
}

/// `ev` as a frame, stamped with `session` when there is one.
pub(crate) fn stamped(mut ev: Value, session: Option<&str>) -> Message {
    if let (Some(obj), Some(s)) = (ev.as_object_mut(), session) {
        obj.insert("sessionId".into(), json!(s));
    }
    if std::env::var("KORVUS_CDP_LOG").is_ok() {
        eprintln!("CDP -> {ev}");
    }
    Message::Text(ev.to_string())
}

// ── the domain ───────────────────────────────────────────────────────────────────────────────────────

/// One `Fetch.RequestPattern`.
#[derive(Debug, Clone)]
struct Pattern {
    /// `urlPattern`: `*` any run, `?` one character, `\` escapes the next.
    url: String,
    /// `resourceType`; `None` matches every type.
    resource_type: Option<String>,
    /// `requestStage` is `Request` (the only stage a request is paused at here).
    request_stage: bool,
}

impl Pattern {
    fn matches(&self, url: &str, resource_type: &str) -> bool {
        self.request_stage && self.resource_type.as_deref().is_none_or(|t| t == resource_type) && glob(self.url.as_bytes(), url.as_bytes())
    }
}

/// `base::MatchPattern`: `*` matches any run, `?` one character, `\` makes the next one literal.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => (0..=s.len()).any(|i| glob(&p[1..], &s[i..])),
        Some(b'?') => !s.is_empty() && glob(&p[1..], &s[1..]),
        Some(b'\\') if p.len() > 1 => s.first() == Some(&p[1]) && glob(&p[2..], &s[1..]),
        Some(c) => s.first() == Some(c) && glob(&p[1..], &s[1..]),
    }
}

/// The protocol's `Network.ResourceType` values, for checking a pattern's (`AddInterceptedResourceType`).
const RESOURCE_TYPES: &[&str] = &[
    "Document", "Stylesheet", "Image", "Media", "Font", "Script", "TextTrack", "XHR", "Fetch", "Prefetch", "EventSource",
    "WebSocket", "Manifest", "SignedExchange", "Ping", "CSPViolationReport", "Preflight", "FedCM", "Other",
];

#[derive(Default)]
struct State {
    enabled: bool,
    patterns: Vec<Pattern>,
    /// The session `Fetch.enable` arrived on: its events go there.
    session: Option<String>,
    next_job: u64,
    /// Each paused ledger row's job and how many times it has been paused (a redirect hop is paused again, as
    /// `interception-job-8.1` after `interception-job-8.0`).
    jobs: HashMap<u64, (u64, u32)>,
    /// The pauses waiting for an answer.
    pending: HashMap<String, tokio::sync::oneshot::Sender<Paused>>,
    /// The pauses already answered: a second answer is Chrome's "Invalid state", not "Invalid InterceptionId".
    answered: HashSet<String>,
    /// `handleAuthRequests`: authentication challenges are the client's.
    handle_auth: bool,
    /// The challenges waiting for `Fetch.continueWithAuth`.
    pending_auth: HashMap<String, tokio::sync::oneshot::Sender<AuthAnswer>>,
}

/// A pause to report.
pub(crate) struct Announce {
    id: String,
    redirected_from: Option<String>,
    req: PausedRequest,
    session: Option<String>,
    /// An authentication challenge (`Fetch.authRequired`), not a request-stage pause: `(source, origin, scheme, realm)`.
    auth: Option<(&'static str, String, String, String)>,
}

/// The `Fetch` domain of one connection, and the interceptor the transport consults.
pub(crate) struct FetchDomain {
    st: Mutex<State>,
    announce: tokio::sync::mpsc::UnboundedSender<Announce>,
}

impl FetchDomain {
    pub(crate) fn new() -> (Arc<FetchDomain>, tokio::sync::mpsc::UnboundedReceiver<Announce>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(FetchDomain { st: Mutex::new(State::default()), announce: tx }), rx)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The connection is gone: nothing is intercepted any more, and every paused request goes on.
    pub(crate) fn close(&self) {
        transport::intercept::set_interceptor(None);
        let mut st = self.state();
        st.enabled = false;
        st.pending.clear();
        st.pending_auth.clear();
    }

    /// A `Fetch.*` command, answered: `Ok(result)` or `Err((code, message))`.
    fn command(self: &Arc<Self>, method: &str, params: &Value, session: Option<&str>) -> Result<Value, (i64, String)> {
        let invalid = |m: &str| -> Result<Value, (i64, String)> { Err(bad(m)) };
        match method {
            "Fetch.enable" => {
                let mut patterns = Vec::new();
                match params.get("patterns").and_then(Value::as_array) {
                    None => patterns.push(Pattern { url: "*".into(), resource_type: None, request_stage: true }),
                    Some(list) => {
                        for p in list {
                            let resource_type = p.get("resourceType").and_then(Value::as_str).filter(|t| !t.is_empty());
                            if let Some(t) = resource_type {
                                if !RESOURCE_TYPES.contains(&t) {
                                    return invalid(&format!("Unknown resource type in fetch filter: '{t}'"));
                                }
                            }
                            patterns.push(Pattern {
                                url: p.get("urlPattern").and_then(Value::as_str).unwrap_or("*").to_string(),
                                resource_type: resource_type.map(str::to_string),
                                request_stage: p.get("requestStage").and_then(Value::as_str).unwrap_or("Request") == "Request",
                            });
                        }
                    }
                }
                if patterns.is_empty() && params.get("handleAuthRequests").and_then(Value::as_bool).unwrap_or(false) {
                    return invalid("Can't specify empty patterns with handleAuth set");
                }
                {
                    let mut st = self.state();
                    st.enabled = true;
                    st.patterns = patterns;
                    st.session = session.map(str::to_string);
                    st.handle_auth = params.get("handleAuthRequests").and_then(Value::as_bool).unwrap_or(false);
                }
                transport::intercept::set_interceptor(Some(self.clone() as Arc<dyn transport::intercept::Interceptor>));
                Ok(json!({}))
            }
            "Fetch.disable" => {
                transport::intercept::set_interceptor(None);
                let mut st = self.state();
                st.enabled = false;
                st.patterns.clear();
                st.handle_auth = false;
                // every request still paused goes on (its sender dropped: a plain continue), and every challenge
                // gets its own response
                st.pending.clear();
                st.pending_auth.clear();
                Ok(json!({}))
            }
            "Fetch.continueRequest" => {
                self.enabled()?;
                let mut o = Overrides::default();
                if let Some(list) = params.get("headers").and_then(Value::as_array) {
                    let mut headers = Vec::new();
                    for h in list {
                        let (name, value) = header_entry(h)?;
                        if !request_header_safe(&name, &value) {
                            return invalid(&format!("Unsafe header: {name}"));
                        }
                        headers.push((name, value));
                    }
                    o.headers = Some(headers);
                }
                o.url = params.get("url").and_then(Value::as_str).map(str::to_string);
                o.method = params.get("method").and_then(Value::as_str).map(str::to_string);
                if let Some(b) = params.get("postData").and_then(Value::as_str) {
                    o.body = Some(base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| bad("Invalid postData"))?);
                }
                self.answer(params, Paused::Continue(o))
            }
            "Fetch.fulfillRequest" => {
                self.enabled()?;
                let status = params.get("responseCode").and_then(Value::as_i64).unwrap_or(0);
                let phrase = match params.get("responsePhrase").and_then(Value::as_str) {
                    Some(p) => p.to_string(),
                    None => reason_phrase(status).to_string(),
                };
                if phrase.is_empty() || !(100..=999).contains(&status) {
                    return invalid("Invalid http status code or phrase");
                }
                let mut headers = Vec::new();
                match (params.get("responseHeaders").and_then(Value::as_array), params.get("binaryResponseHeaders").and_then(Value::as_str)) {
                    (Some(_), Some(_)) => return invalid("Only one of responseHeaders or binaryHeaders may be present"),
                    (Some(list), None) => {
                        for h in list {
                            headers.push(header_entry(h)?);
                        }
                    }
                    (None, Some(b)) => {
                        let raw = base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| bad("Invalid binaryResponseHeaders"))?;
                        for line in raw.split(|c| *c == 0).filter(|l| !l.is_empty()) {
                            let line = String::from_utf8_lossy(line);
                            if let Some((k, v)) = line.split_once(':') {
                                headers.push((k.trim().to_string(), v.trim().to_string()));
                            }
                        }
                    }
                    (None, None) => {}
                }
                let body = match params.get("body").and_then(Value::as_str) {
                    Some(b) => base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| bad("Invalid body"))?,
                    None => Vec::new(),
                };
                self.answer(params, Paused::Fulfill(Fulfilment { status: status as u16, phrase, headers, body }))
            }
            "Fetch.failRequest" => {
                self.enabled()?;
                let reason = params.get("errorReason").and_then(Value::as_str).unwrap_or("");
                let Some(net) = net_error(reason) else { return invalid("Unknown errorReason") };
                self.answer(params, Paused::Fail(net.to_string()))
            }
            // Nothing is ever paused at the response stage or for credentials here (module comment).
            "Fetch.getResponseBody" | "Fetch.takeResponseBodyAsStream" => {
                self.enabled()?;
                self.known(params)?;
                Err((-32000, "Can only get response body on HeadersReceived pattern matched requests.".to_string()))
            }
            // The client's answer to a challenge it was told of (`Fetch.authRequired`).
            "Fetch.continueWithAuth" => {
                self.enabled()?;
                let id = params.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
                let r = params.get("authChallengeResponse").cloned().unwrap_or(Value::Null);
                let answer = match r.get("response").and_then(Value::as_str) {
                    Some("ProvideCredentials") => AuthAnswer::Provide {
                        username: r.get("username").and_then(Value::as_str).unwrap_or("").to_string(),
                        password: r.get("password").and_then(Value::as_str).unwrap_or("").to_string(),
                    },
                    Some("Default" | "CancelAuth") => AuthAnswer::Cancel,
                    _ => return invalid("Unrecognized authChallengeResponse"),
                };
                let mut st = self.state();
                match st.pending_auth.remove(&id) {
                    Some(tx) => {
                        let _ = tx.send(answer);
                        Ok(json!({}))
                    }
                    None if st.pending.contains_key(&id) || st.answered.contains(&id) => {
                        Err((-32000, "Invalid state for continueInterceptedRequest".to_string()))
                    }
                    None => Err((-32602, "Invalid InterceptionId.".to_string())),
                }
            }
            "Fetch.continueResponse" => {
                self.enabled()?;
                self.known(params)?;
                Err((-32000, "Invalid state for continueInterceptedRequest".to_string()))
            }
            m => Err((-32601, format!("'{m}' wasn't found"))),
        }
    }

    fn enabled(&self) -> Result<(), (i64, String)> {
        if self.state().enabled {
            Ok(())
        } else {
            Err((-32000, "Fetch domain is not enabled".to_string()))
        }
    }

    /// Is `params.requestId` a pause this domain made?
    fn known(&self, params: &Value) -> Result<(), (i64, String)> {
        let id = params.get("requestId").and_then(Value::as_str).unwrap_or("");
        let st = self.state();
        if st.pending.contains_key(id) || st.answered.contains(id) {
            Ok(())
        } else {
            Err((-32602, "Invalid InterceptionId.".to_string()))
        }
    }

    /// Hand `answer` to the pause `params.requestId` names.
    fn answer(&self, params: &Value, answer: Paused) -> Result<Value, (i64, String)> {
        let id = params.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        let mut st = self.state();
        match st.pending.remove(&id) {
            Some(tx) => {
                st.answered.insert(id);
                // a request the page gave up meanwhile has nobody to tell; the command still succeeded
                let _ = tx.send(answer);
                Ok(json!({}))
            }
            None if st.answered.contains(&id) => Err((-32000, "Invalid state for continueInterceptedRequest".to_string())),
            None => Err((-32602, "Invalid InterceptionId.".to_string())),
        }
    }
}

impl transport::intercept::Interceptor for FetchDomain {
    fn pause(&self, r: &PausedRequest) -> Option<PauseFuture> {
        let resource_type = network::paused_resource_type(&r.destination);
        let mut st = self.state();
        if !st.enabled || !st.patterns.iter().any(|p| p.matches(&r.url, resource_type)) {
            return None;
        }
        let (job, hop) = match st.jobs.get_mut(&r.row) {
            Some((job, hop)) => {
                *hop += 1;
                (*job, *hop)
            }
            None => {
                st.next_job += 1;
                let job = st.next_job;
                st.jobs.insert(r.row, (job, 0));
                (job, 0)
            }
        };
        let id = format!("interception-job-{job}.{hop}");
        let redirected_from = (hop > 0).then(|| format!("interception-job-{job}.{}", hop - 1));
        let (tx, rx) = tokio::sync::oneshot::channel();
        st.pending.insert(id.clone(), tx);
        let session = st.session.clone();
        drop(st);
        if self.announce.send(Announce { id, redirected_from, req: r.clone(), session, auth: None }).is_err() {
            // the connection is gone: nobody will answer
            return None;
        }
        Some(Box::pin(async move { rx.await.unwrap_or(Paused::Continue(Overrides::default())) }))
    }

    /// A challenge on a request the client's patterns cover, while it handles them: told as `Fetch.authRequired` under
    /// the request's interception id (a new job when it was never paused), and held for `Fetch.continueWithAuth`.
    fn auth(&self, c: &AuthChallenge) -> Option<AuthFuture> {
        let r = &c.req;
        let resource_type = network::paused_resource_type(&r.destination);
        let mut st = self.state();
        if !st.enabled || !st.handle_auth || !st.patterns.iter().any(|p| p.matches(&r.url, resource_type)) {
            return None;
        }
        let (job, hop) = match st.jobs.get(&r.row) {
            Some((job, hop)) => (*job, *hop),
            None => {
                st.next_job += 1;
                let job = st.next_job;
                st.jobs.insert(r.row, (job, 0));
                (job, 0)
            }
        };
        let id = format!("interception-job-{job}.{hop}");
        let (tx, rx) = tokio::sync::oneshot::channel();
        st.pending_auth.insert(id.clone(), tx);
        let session = st.session.clone();
        drop(st);
        let auth = Some((c.source, c.origin.clone(), c.scheme.clone(), c.realm.clone()));
        if self.announce.send(Announce { id, redirected_from: None, req: r.clone(), session, auth }).is_err() {
            return None;
        }
        Some(Box::pin(async move { rx.await.unwrap_or(AuthAnswer::Cancel) }))
    }
}

/// Chrome's `InvalidParams` refusal.
fn bad(message: &str) -> (i64, String) {
    (-32602, message.to_string())
}

/// One `Fetch.HeaderEntry`, checked as Chrome checks it (`ValidateHeaders`).
fn header_entry(h: &Value) -> Result<(String, String), (i64, String)> {
    let name = h.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let value = h.get("value").and_then(Value::as_str).unwrap_or("").to_string();
    let name_ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b));
    let value_ok = !value.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n');
    if name_ok && value_ok {
        Ok((name, value))
    } else {
        Err(bad(&format!("Invalid header: {name}")))
    }
}

/// `network::IsRequestHeaderSafe`: what a client may not set on a request (`Cookie` it may -- Chrome 150 sent a
/// `continueRequest`'s `Cookie: x=1`).
fn request_header_safe(name: &str, value: &str) -> bool {
    const UNSAFE: &[&str] = &["content-length", "host", "trailer", "te", "upgrade", "cookie2", "keep-alive", "transfer-encoding", "set-cookie"];
    let lower = name.to_ascii_lowercase();
    !(UNSAFE.contains(&lower.as_str())
        || (lower == "connection" && value.eq_ignore_ascii_case("upgrade"))
        || lower.starts_with("proxy-")
        || (matches!(lower.as_str(), "x-http-method" | "x-http-method-override" | "x-method-override")
            && value.split(',').any(|m| matches!(m.trim().to_ascii_uppercase().as_str(), "CONNECT" | "TRACE" | "TRACK"))))
}

/// `NetworkHandler::NetErrorFromString`, as the request's error reads (`Network.loadingFailed.errorText`): a request a
/// client blocked is Blink's `ERR_BLOCKED_BY_CLIENT` with the `Inspector` reason (Chrome 150 recorded
/// `net::ERR_BLOCKED_BY_CLIENT.Inspector`, `blockedReason: "inspector"`).
fn net_error(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "Failed" => "net::ERR_FAILED",
        "Aborted" => "net::ERR_ABORTED",
        "TimedOut" => "net::ERR_TIMED_OUT",
        "AccessDenied" => "net::ERR_ACCESS_DENIED",
        "ConnectionClosed" => "net::ERR_CONNECTION_CLOSED",
        "ConnectionReset" => "net::ERR_CONNECTION_RESET",
        "ConnectionRefused" => "net::ERR_CONNECTION_REFUSED",
        "ConnectionAborted" => "net::ERR_CONNECTION_ABORTED",
        "ConnectionFailed" => "net::ERR_CONNECTION_FAILED",
        "NameNotResolved" => "net::ERR_NAME_NOT_RESOLVED",
        "InternetDisconnected" => "net::ERR_INTERNET_DISCONNECTED",
        "AddressUnreachable" => "net::ERR_ADDRESS_UNREACHABLE",
        "BlockedByClient" => network::BLOCKED_BY_INSPECTOR,
        "BlockedByResponse" => "net::ERR_BLOCKED_BY_RESPONSE",
        _ => return None,
    })
}

/// `net::GetHttpReasonPhrase` (`http_status_code_list.h`): the phrase of a fulfilled response that named none, and
/// `""` -- refused -- for a code Chrome has none for (a `299` was, recorded).
fn reason_phrase(code: i64) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        414 => "Request-URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Requested Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "Invalid XPrivet Token",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended (Obsoleted)",
        511 => "Network Authentication Required",
        _ => "",
    }
}

// ── the two tasks ────────────────────────────────────────────────────────────────────────────────────

/// The read half: every `Fetch.*` command is answered here, at once, whatever the page is doing; the rest go to the
/// page's loop in order. Ends with the socket, or when the loop has gone.
pub(crate) async fn read_socket(mut source: Source, to_loop: tokio::sync::mpsc::UnboundedSender<Incoming>, fetch: Arc<FetchDomain>, mut out: Out) {
    while let Some(m) = source.next().await {
        if let Ok(Message::Text(txt)) = &m {
            // cheap test first: a page command can be large (Playwright's utility script)
            if txt.contains("\"Fetch.") {
                let req: Value = serde_json::from_str(txt).unwrap_or(Value::Null);
                if let Some(method) = req.get("method").and_then(Value::as_str).filter(|m| m.starts_with("Fetch.")) {
                    let id = req.get("id").and_then(Value::as_i64).unwrap_or(0);
                    let session = req.get("sessionId").and_then(Value::as_str);
                    let params = req.get("params").cloned().unwrap_or(Value::Null);
                    if std::env::var("KORVUS_CDP_LOG").is_ok() {
                        eprintln!("CDP <- {method} (session={}, beside the page)", session.is_some());
                    }
                    let mut resp = match fetch.command(method, &params, session) {
                        Ok(r) => json!({ "id": id, "result": r }),
                        Err((code, message)) => json!({ "id": id, "error": { "code": code, "message": message } }),
                    };
                    if let (Some(obj), Some(s)) = (resp.as_object_mut(), session) {
                        obj.insert("sessionId".into(), json!(s));
                    }
                    if out.send(Message::Text(resp.to_string())).await.is_err() {
                        break;
                    }
                    continue;
                }
            }
            // A plugin's CDP domain answered HERE, off the page loop, when that domain's plugin can PARK requests
            // (`cdp_domain_can_hold`, ABI 3 / D7). This is what releases a held INITIAL-DOCUMENT navigation: the
            // page loop is then blocked inside `begin_navigate` awaiting the very document the plugin holds
            // (lib.rs), so it cannot answer the `Korvus_hold.*` call that would resume it -- but this socket-reader
            // task runs on the same thread while that loop is parked and answers it. Only a holding plugin can
            // block the loop this way, so only its domains are taken off-path; a non-holding page-level domain
            // (e.g. virtual-time `Korvus_time`) stays in order on the page loop and is never reordered. The
            // `continue` suppresses forwarding, so the in-order plugin block in lib.rs never double-answers. The
            // cheap prefix test runs first so engine traffic (never `Korvus_`) is untouched, and the gate is false
            // for every no-plugin / stealth-only session, so this path is invisible then (I9/J2).
            if txt.contains("\"Korvus_") {
                let req: Value = serde_json::from_str(txt).unwrap_or(Value::Null);
                if let Some(method) = req.get("method").and_then(Value::as_str).filter(|m| m.starts_with("Korvus_")) {
                    if plugin_host::cdp_domain_can_hold(method.split('.').next().unwrap_or("")) {
                        let id = req.get("id").and_then(Value::as_i64).unwrap_or(0);
                        let session = req.get("sessionId").and_then(Value::as_str);
                        let params = req.get("params").cloned().unwrap_or(Value::Null);
                        let params_json = if params.is_null() { String::new() } else { params.to_string() };
                        let mut resp = match plugin_host::cdp_call(method, &params_json) {
                            Some(r) => json!({ "id": id, "result": serde_json::from_str::<Value>(&r).unwrap_or(Value::Null) }),
                            None => json!({ "id": id, "error": { "code": -32000, "message": format!("the plugin did not answer {method}") } }),
                        };
                        if let (Some(obj), Some(s)) = (resp.as_object_mut(), session) {
                            obj.insert("sessionId".into(), json!(s));
                        }
                        if out.send(Message::Text(resp.to_string())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                }
            }
        }
        if to_loop.send(m).is_err() {
            break;
        }
    }
}

/// Report each pause: the request's `Network.*` events not yet sent, then `Fetch.requestPaused` -- Chrome 150's order
/// for every request but a preflight (whose pause it sent first; either order is one a client handles).
pub(crate) async fn announce(mut rx: tokio::sync::mpsc::UnboundedReceiver<Announce>, rep: Rep, out: Out) {
    while let Some(a) = rx.recv().await {
        let mut frames = flushed_messages(&rep, a.session.as_deref());
        let (network_id, frame_id) = {
            let r = rep_lock(&rep);
            (r.network_id(a.req.row, &a.req.kind), r.frame_id())
        };
        let ev = match &a.auth {
            Some(challenge) => auth_event(&a, challenge, frame_id),
            None => paused_event(&a, network_id, frame_id),
        };
        frames.push(stamped(ev, a.session.as_deref()));
        if out.send_all(frames).await.is_err() {
            break;
        }
    }
}

/// `Fetch.requestPaused`, its keys in Chrome 150's order (`requestId, request, frameId, resourceType, networkId,
/// redirectedRequestId`), its `request` a `Network.Request` as the renderer made it.
fn paused_event(a: &Announce, network_id: Option<String>, frame_id: Option<String>) -> Value {
    let r = &a.req;
    let mut headers = serde_json::Map::new();
    for (k, v) in &r.headers {
        headers.insert(k.clone(), json!(v));
    }
    let mut request = json!({ "url": r.url, "method": r.method, "headers": Value::Object(headers) });
    if let Some(body) = r.body.as_ref().filter(|b| !b.is_empty()) {
        request["postData"] = json!(String::from_utf8_lossy(body));
        request["hasPostData"] = json!(true);
        request["postDataEntries"] = json!([{ "bytes": base64::engine::general_purpose::STANDARD.encode(body) }]);
    }
    request["initialPriority"] = json!(network::initial_priority(r.priority.as_deref()));
    request["referrerPolicy"] = json!("strict-origin-when-cross-origin");
    let mut params = json!({ "requestId": a.id, "request": request });
    if let Some(f) = frame_id {
        params["frameId"] = json!(f);
    }
    params["resourceType"] = json!(network::paused_resource_type(&r.destination));
    if let Some(n) = network_id {
        params["networkId"] = json!(n);
    }
    if let Some(from) = &a.redirected_from {
        params["redirectedRequestId"] = json!(from);
    }
    json!({ "method": "Fetch.requestPaused", "params": params })
}

/// `Fetch.authRequired`: the request as `Fetch.requestPaused` shows it, and the challenge (`Fetch.AuthChallenge`).
fn auth_event(a: &Announce, challenge: &(&'static str, String, String, String), frame_id: Option<String>) -> Value {
    let mut ev = paused_event(a, None, frame_id);
    let (source, origin, scheme, realm) = challenge;
    ev["method"] = json!("Fetch.authRequired");
    if let Some(p) = ev["params"].as_object_mut() {
        p.remove("networkId");
        p.remove("redirectedRequestId");
        p.insert("authChallenge".into(), json!({ "source": source, "origin": origin, "scheme": scheme, "realm": realm }));
    }
    ev
}

/// Start a connection's halves: the shared writer, the reader task, the announcer, and the domain. The returned guard
/// stops both tasks and lets every paused request go when the connection ends.
pub(crate) fn split(ws: WebSocketStream<TcpStream>, rep: &Rep) -> (Out, futures_util::stream::Peekable<Inbox>, Halves) {
    let (sink, source) = ws.split();
    let out = Out::new(sink);
    let (fetch, announced) = FetchDomain::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(read_socket(source, tx, fetch.clone(), out.clone()));
    let announcer = tokio::spawn(announce(announced, rep.clone(), out.clone()));
    (out, Inbox(rx).peekable(), Halves { fetch, reader, announcer })
}

/// The connection's tasks; dropping it ends them (see [`split`]).
pub(crate) struct Halves {
    fetch: Arc<FetchDomain>,
    reader: tokio::task::JoinHandle<()>,
    announcer: tokio::task::JoinHandle<()>,
}

impl Drop for Halves {
    fn drop(&mut self) {
        self.fetch.close();
        self.reader.abort();
        self.announcer.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_patterns_match_as_base_match_pattern() {
        assert!(glob(b"*", b"http://a/b"));
        assert!(glob(b"*/data.json", b"http://localhost:1/data.json"));
        assert!(!glob(b"*/data.json", b"http://localhost:1/data.jsonx"));
        assert!(glob(b"http://?/x", b"http://a/x"));
        assert!(!glob(b"http://?/x", b"http://ab/x"));
        assert!(glob(b"*\\*", b"a*"));
        assert!(!glob(b"*\\*", b"ab"));
    }

    #[test]
    fn a_pattern_names_its_resource_type_and_stage() {
        let p = Pattern { url: "*".into(), resource_type: Some("XHR".into()), request_stage: true };
        assert!(p.matches("http://a/", "XHR"));
        assert!(!p.matches("http://a/", "Document"));
        let r = Pattern { url: "*".into(), resource_type: None, request_stage: false };
        assert!(!r.matches("http://a/", "Document"), "nothing is paused at the response stage");
    }

    // Chrome 150's answers, recorded (`chrome150-a1s3b.json`): its messages and codes for each refusal.
    #[test]
    fn commands_refuse_as_chrome_does() {
        let (fetch, _rx) = FetchDomain::new();
        let cont = json!({ "requestId": "interception-job-99.0" });
        assert_eq!(fetch.command("Fetch.continueRequest", &cont, None), Err((-32000, "Fetch domain is not enabled".to_string())));
        // enabling installs the process's interceptor; this test removes it again below
        assert_eq!(fetch.command("Fetch.enable", &json!({ "patterns": [{ "urlPattern": "*" }] }), None), Ok(json!({})));
        assert_eq!(fetch.command("Fetch.continueRequest", &cont, None), Err((-32602, "Invalid InterceptionId.".to_string())));
        assert_eq!(
            fetch.command("Fetch.failRequest", &json!({ "requestId": "interception-job-99.0", "errorReason": "Nope" }), None),
            Err((-32602, "Unknown errorReason".to_string()))
        );
        assert_eq!(
            fetch.command("Fetch.fulfillRequest", &json!({ "requestId": "interception-job-99.0", "responseCode": 299 }), None),
            Err((-32602, "Invalid http status code or phrase".to_string()))
        );
        assert_eq!(fetch.command("Fetch.disable", &json!({}), None), Ok(json!({})));
        assert_eq!(fetch.command("Fetch.continueRequest", &cont, None), Err((-32000, "Fetch domain is not enabled".to_string())));
    }

    #[test]
    fn a_second_answer_is_an_invalid_state() {
        let (fetch, mut rx) = FetchDomain::new();
        fetch.command("Fetch.enable", &json!({}), Some("S")).unwrap();
        let req = PausedRequest {
            row: 7,
            kind: "empty".into(),
            destination: "empty".into(),
            target: None,
            url: "http://localhost/redir".into(),
            method: "GET".into(),
            headers: vec![],
            body: None,
            priority: None,
        };
        let first = transport::intercept::Interceptor::pause(&*fetch, &req);
        assert!(first.is_some());
        let a = rx.try_recv().unwrap();
        assert_eq!(a.id, "interception-job-1.0");
        assert_eq!(a.session.as_deref(), Some("S"));
        let id = json!({ "requestId": "interception-job-1.0" });
        assert_eq!(fetch.command("Fetch.continueRequest", &id, None), Ok(json!({})));
        assert_eq!(fetch.command("Fetch.continueRequest", &id, None), Err((-32000, "Invalid state for continueInterceptedRequest".to_string())));
        // the same row paused again (its redirect hop): the job's next number, and where it came from
        let _second = transport::intercept::Interceptor::pause(&*fetch, &req);
        let b = rx.try_recv().unwrap();
        assert_eq!((b.id.as_str(), b.redirected_from.as_deref()), ("interception-job-1.1", Some("interception-job-1.0")));
        fetch.close();
    }

    #[test]
    fn a_failed_request_reads_as_chromes_error() {
        assert_eq!(net_error("BlockedByClient"), Some("net::ERR_BLOCKED_BY_CLIENT.Inspector"));
        assert_eq!(net_error("Failed"), Some("net::ERR_FAILED"));
        assert_eq!(net_error("nope"), None);
    }
}

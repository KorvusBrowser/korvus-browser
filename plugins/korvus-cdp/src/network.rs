//! The `Network` domain: the engine's [request ledger](transport::ledger) rendered as the three CDP
//! events a client joins into one request.
//!
//! WHY THIS EXISTS. `Network.enable` was on `ACCEPTED_NOOPS` — accepted, and followed by silence. A
//! Playwright script that does `page.on('request', …)` against `korvus serve` therefore saw nothing,
//! which is worse than an error: the harness reports a page that fetched zero resources exactly the
//! way it reports a page that fetched none. The parity goal's cheapest fidelity check is the
//! resource-set diff, and it is run through this wire for both engines, so a silent
//! `Network.enable` was the single thing standing between "one driver, one protocol" and two.
//!
//! WHAT A CLIENT NEEDS, AND WHY THE ID DISCIPLINE IS NOT COSMETIC. Playwright joins
//! `requestWillBeSent` → `responseReceived` → `loadingFinished`/`loadingFailed` **by `requestId`**. An
//! id that does not survive all three events produces a request that never completes, which its
//! `page.goto` then waits on. So the id is minted from the ledger row's own monotonic id, once, and
//! every event for that row carries it.
//!
//! WHEN THEY ARE SENT. The document request's own at commit, just before `Page.navigate` is answered;
//! every other request's as the ledger records it -- flushed every 50 ms while the document loads
//! (`reporting_network` in `lib.rs`, at the points the load yields) and after it -- each row's
//! `requestWillBeSent` when it is first seen and its completion once it has finished. They used to go
//! out in one batch once the load was over, and a client waiting for network quiet read the load as
//! silence. What a client still cannot see is a request a running script made before that script
//! returns. The ORDER is the order the requests started, and the `timestamp`s are the real per-request
//! times from the ledger; `Network.requestWillBeSent`'s `wallTime` is likewise the real clock. Nothing
//! here reports a number the ledger did not measure.

use serde_json::{json, Value};

/// Chrome's `Network.ResourceType` for a Fetch request destination.
///
/// The left column is Fetch's destination token (what the ledger stores, because it is what the
/// engine's own `Sec-Fetch-Dest` is computed from); the right is the protocol's enum, which is what a
/// CDP client's `request.resourceType()` reads. Two vocabularies for one fact, each in the place that
/// defines it — not two facts.
pub fn resource_type(destination: &str) -> &'static str {
    match destination {
        "document" => "Document",
        // A subframe's document is `Document` in Chrome's taxonomy too; the frame-ness is carried by
        // `frameId`, not by the resource type.
        "iframe" | "frame" | "fencedframe" => "Document",
        "style" => "Stylesheet",
        "image" => "Image",
        "font" => "Font",
        "script" | "worker" | "sharedworker" | "serviceworker" | "xslt" => "Script",
        "audio" | "video" | "track" | "audioworklet" | "paintworklet" => "Media",
        "manifest" => "Manifest",
        "object" | "embed" => "Other",
        "report" => "CSPViolationReport",
        // A CORS preflight the transport sent for a page request.
        "preflight" => "Preflight",
        // The EMPTY destination is a page `fetch()` or XHR. Chrome distinguishes the two
        // (`Fetch` vs `XHR`) from the API that started it, which this transport is not told, so it
        // reports the one it can stand behind.
        "empty" | "" => "Fetch",
        _ => "Other",
    }
}

/// The `resourceType` of a `Fetch.requestPaused`: Chrome's from the network service's request type
/// (`NetworkHandler::ResourceTypeToString` over `RequestContextToResourceType`), which is NOT the `Network.*` events'
/// type -- Chrome 150 paused a `fetch()` as `XHR` and a worker's script as `Other` (`chrome150-a1s3.json`). A beacon is
/// Chrome's `Ping`, which the destination (`empty`, as a `fetch()`'s) cannot tell apart.
pub fn paused_resource_type(destination: &str) -> &'static str {
    match destination {
        "document" | "iframe" | "frame" | "fencedframe" => "Document",
        "style" | "xslt" => "Stylesheet",
        "script" | "audioworklet" | "paintworklet" | "sharedstorageworklet" => "Script",
        "image" => "Image",
        "font" => "Font",
        "audio" | "video" | "track" => "Media",
        "report" => "CSPViolationReport",
        "empty" | "" => "XHR",
        _ => "Other",
    }
}

/// The error a request a DevTools client blocked fails with (`Fetch.failRequest`'s `BlockedByClient`): Chrome 150's
/// `Network.loadingFailed` read `errorText: "net::ERR_BLOCKED_BY_CLIENT.Inspector"`, `blockedReason: "inspector"`.
pub const BLOCKED_BY_INSPECTOR: &str = "net::ERR_BLOCKED_BY_CLIENT.Inspector";

/// One `Network.Cookie`, keys in stock Chrome 150's order (`chrome150-a1s3.json`): `sameSite` only
/// when the cookie set one, `partitionKey` only on a partitioned cookie and as an object, every priority `Medium` (the
/// store keeps none other).
pub fn cookie_json(c: &transport::cookies::DevtoolsCookie) -> Value {
    let mut v = json!({
        "name": c.name,
        "value": c.value,
        "domain": c.domain,
        "path": c.path,
        // a double, written as Chrome's protocol writer writes one: `-1`, not `-1.0`, for a session cookie
        "expires": if c.expires.fract() == 0.0 && c.expires.abs() < 9_007_199_254_740_992.0 { json!(c.expires as i64) } else { json!(c.expires) },
        "size": c.size,
        "httpOnly": c.http_only,
        "secure": c.secure,
        "session": c.session,
    });
    if let Some(s) = c.same_site {
        v["sameSite"] = json!(s);
    }
    v["priority"] = json!("Medium");
    v["sourceScheme"] = json!(c.source_scheme);
    v["sourcePort"] = json!(c.source_port);
    if let Some((site, cross)) = &c.partition_key {
        v["partitionKey"] = json!({ "topLevelSite": site, "hasCrossSiteAncestor": cross });
    }
    v
}

/// A `Network.CookieParam` as the store takes it.
pub fn cookie_init(p: &Value) -> transport::cookies::DevtoolsCookieInit {
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    transport::cookies::DevtoolsCookieInit {
        name: s("name").unwrap_or_default(),
        value: s("value").unwrap_or_default(),
        url: s("url"),
        domain: s("domain"),
        path: s("path"),
        secure: p.get("secure").and_then(Value::as_bool).unwrap_or(false),
        http_only: p.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
        same_site: s("sameSite"),
        expires: p.get("expires").and_then(Value::as_f64).unwrap_or(-1.0),
        partition_key: p.get("partitionKey").and_then(|k| {
            let site = k.get("topLevelSite").and_then(Value::as_str).filter(|s| !s.is_empty())?;
            Some((site.to_string(), k.get("hasCrossSiteAncestor").and_then(Value::as_bool).unwrap_or(false)))
        }),
    }
}

/// The `requestId` for a ledger row.
///
/// Chrome sets the MAIN DOCUMENT request's id equal to the navigation's `loaderId` (measured, and
/// Playwright relies on it to recognise a navigation request); every other request gets
/// `<prefix>.<n>`, where Chrome's prefix is the renderer process id. `prefix` here is per-connection
/// and stable for its life, so ids never collide across a connection's navigations.
pub fn request_id(row_id: u64, destination: &str, loader_id: &str, prefix: i64) -> String {
    if destination == "document" {
        loader_id.to_string()
    } else {
        format!("{prefix}.{row_id}")
    }
}

/// `response.mimeType`: the response head's `mime_type` -- the network stack's reading of every `Content-Type`
/// value, no parameters (`transport::mime`, A/B C5). Empty when the server sent none — Chrome reports an empty
/// string there rather than guessing.
fn mime_type(headers: &[(String, String)]) -> String {
    transport::mime::mime_type_and_charset(headers).mime
}

/// The response headers no renderer and no `Network.Response` ever sees: Chromium serializes a response head for
/// IPC without them (`HttpResponseHeaders::SerializeForMojoIpc` -> `PERSIST_SANS_COOKIES`, `kCookieResponseHeaders`
/// in `http_response_headers.cc:73`). Chrome 150 reports them only in `Network.responseReceivedExtraInfo`.
const COOKIE_RESPONSE_HEADERS: [&str; 3] = ["set-cookie", "set-cookie2", "clear-site-data"];

/// Is a row of this destination a NAVIGATION -- a frame's document, reported by the browser process?
fn is_navigation(kind: &str) -> bool {
    matches!(kind, "document" | "iframe" | "frame" | "fencedframe")
}

/// Headers as the protocol's `Network.Headers` (a flat object), as Chrome 150 reports a RESPONSE's
/// (three runs alike).
///
/// Every value of a repeated header is kept: this used to insert each line over the last, so of two `Set-Cookie`
/// lines one survived and of two `X-Dup` lines the second. Chrome joins them, and HOW depends on who reports the
/// response: a navigation's comes from the browser process (`BuildResponseHeaders`, `network_handler.cc:3000`, joined
/// with `"\n"`), a subresource's from the renderer's `HTTPHeaderMap` (`", "`, as `ResourceResponse::AddHttpHeaderField`
/// combines a field). Neither carries [`COOKIE_RESPONSE_HEADERS`].
fn headers_object(headers: &[(String, String)], navigation: bool) -> Value {
    let sep = if navigation { "\n" } else { ", " };
    let mut m = serde_json::Map::new();
    for (k, v) in headers {
        if COOKIE_RESPONSE_HEADERS.iter().any(|c| k.eq_ignore_ascii_case(c)) {
            continue;
        }
        match m.get_mut(k) {
            Some(Value::String(prev)) => {
                prev.push_str(sep);
                prev.push_str(v);
            }
            _ => {
                m.insert(k.clone(), json!(v));
            }
        }
    }
    Value::Object(m)
}

/// One `Network.Response`: a final response, or a redirect response (`redirectResponse`).
struct ResponseView<'a> {
    url: &'a str,
    status: u16,
    status_text: &'a str,
    headers: &'a [(String, String)],
    /// Who reports it (see [`headers_object`]).
    navigation: bool,
    from_cache: bool,
    /// The body length the ledger measured (decoded; see `encodedDataLength` below).
    bytes: usize,
    /// When it arrived, Unix milliseconds.
    response_time_ms: f64,
}

impl ResponseView<'_> {
    fn to_json(&self) -> Value {
        json!({
            "url": self.url,
            "status": self.status,
            "statusText": self.status_text,
            "headers": headers_object(self.headers, self.navigation),
            "mimeType": mime_type(self.headers),
            "charset": "",
            "connectionReused": false,
            "connectionId": 0,
            "fromDiskCache": self.from_cache,
            "fromServiceWorker": false,
            "fromPrefetchCache": false,
            // The DECODED body length. This transport hands its callers a decoded
            // string and never sees the compressed count, so `encodedDataLength` and
            // `encodedBodyLength` carry the one length it measured rather than a
            // compression ratio nobody computed.
            "encodedDataLength": self.bytes,
            "responseTime": self.response_time_ms,
            "protocol": "",
            "securityState": "unknown"
        })
    }
}

/// The navigation a request's events belong to: what every event of it names, and the two clock bases
/// its ledger times are offset from.
#[derive(Debug, Clone)]
pub struct NavContext {
    pub frame_id: String,
    pub loader_id: String,
    pub document_url: String,
    /// This connection's `requestId` prefix (see [`request_id`]).
    pub prefix: i64,
    /// The monotonic `Network.MonotonicTime` base the `timestamp`s are offset from.
    pub epoch_s: f64,
    /// The Unix time matching `epoch_s`, so `wallTime` and `timestamp` describe the same instants.
    pub wall_s: f64,
}

/// `Network.requestWillBeSent` for `r`. `worker` is the target whose session carries it, when that is not
/// the page.
///
/// A worker's request is not a frame's (measured, Chrome 150 auto-attach): it carries no `frameId` and
/// an empty `loaderId`, its `documentURL` is the worker's script, and its initiator is `script`. The
/// worker's `importScripts` request is typed `Other`, not `Script`.
pub fn request_event(r: &transport::RequestRecord, ctx: &NavContext, worker: Option<&transport::ChildTarget>) -> Value {
    hop_request_event(r, None, ctx, worker)
}

/// `Network.requestWillBeSent` for hop `hop` of `r`'s redirect chain: `None` is the request as first sent, `Some(k)`
/// the request that followed the `k`-th redirect, carrying that redirect's response as `redirectResponse` (N27; see
/// `transport::RedirectHop`). Every hop names its OWN url and method, and a navigation's `documentURL` is the hop's
/// url (Chrome 150: `/nav-r1`, then `/nav-r2`, then `/landing`), where a subresource's stays its document's.
pub fn hop_request_event(r: &transport::RequestRecord, hop: Option<usize>, ctx: &NavContext, worker: Option<&transport::ChildTarget>) -> Value {
    let id = request_id(r.id, &r.kind, &ctx.loader_id, ctx.prefix);
    let ty = match worker {
        Some(_) if r.kind == "script" => "Other",
        _ => resource_type(&r.kind),
    };
    let led = hop.and_then(|k| r.hops.get(k));
    let (url, method, at_ms) = match led {
        Some(h) => (h.next_url.as_str(), h.next_method.as_str(), h.at_ms),
        None => (r.url.as_str(), r.method.as_str(), r.started_ms),
    };
    let start_s = ctx.epoch_s + at_ms / 1000.0;
    let document_url = match worker {
        Some(w) => w.url.as_str(),
        None if is_navigation(&r.kind) => url,
        None => ctx.document_url.as_str(),
    };
    let initiator = match (worker, &r.initiator) {
        // A CORS preflight is the network stack's own request for the one it precedes (Chrome's `preflight`
        // initiator; Playwright recognises an intercepted preflight by it and answers it itself).
        _ if r.kind == transport::PREFLIGHT_KIND => json!({ "type": "preflight", "url": r.url }),
        // A worker's own script, whichever session requests it: Chrome's initiator is `other`.
        _ if matches!(r.kind.as_str(), "worker" | "sharedworker" | "serviceworker") => json!({ "type": "other" }),
        (Some(_), _) => json!({ "type": "script" }),
        // Chrome's `parser` initiator carries the URL that induced the load, which is what every
        // element-induced subresource here is.
        (None, Some(u)) => json!({ "type": "parser", "url": u }),
        (None, None) => json!({ "type": "other" }),
    };
    let mut params = json!({
        "requestId": id,
        "loaderId": if worker.is_some() { "" } else { ctx.loader_id.as_str() },
        "documentURL": document_url,
        "request": {
            "url": url,
            "method": method,
            // The REQUEST headers are not in the ledger: it records what the transport did,
            // and the per-hop request headers are rebuilt inside the redirect loop rather
            // than stored. An empty object is what the protocol permits here; inventing the
            // persona's header block would be a value this event did not measure.
            "headers": {},
            "initialPriority": initial_priority(r.priority.as_deref()),
            "referrerPolicy": "strict-origin-when-cross-origin"
        },
        "timestamp": start_s,
        "wallTime": ctx.wall_s + at_ms / 1000.0,
        "initiator": initiator,
        "redirectHasExtraInfo": false,
        "type": ty,
        "hasUserGesture": false
    });
    if let Some(h) = led {
        // The redirect response, as the hop that got it reports it. Its body is not read, so its measured length
        // is none.
        let view = ResponseView {
            url: &h.url,
            status: h.status,
            status_text: &h.status_text,
            headers: &h.headers,
            navigation: is_navigation(&r.kind),
            from_cache: false,
            bytes: 0,
            response_time_ms: (ctx.wall_s + h.at_ms / 1000.0) * 1000.0,
        };
        params["redirectResponse"] = view.to_json();
    }
    if worker.is_none() {
        params["frameId"] = json!(ctx.frame_id);
    }
    json!({ "method": "Network.requestWillBeSent", "params": params })
}

/// `Network.Request.initialPriority`: Blink's load priority, read back off the h2 `priority` field the
/// request carries -- its urgency is the priority one for one (`u=0` VeryHigh ... `u=4` VeryLow; no field,
/// or a bare `i`, is the default urgency 3, Low).
pub(crate) fn initial_priority(field: Option<&str>) -> &'static str {
    let urgency = field.and_then(|f| f.split(',').map(str::trim).find_map(|p| p.strip_prefix("u=")).and_then(|u| u.parse::<u8>().ok()));
    match urgency {
        Some(0) => "VeryHigh",
        Some(1) => "High",
        Some(2) => "Medium",
        Some(4) | Some(5) => "VeryLow",
        _ => "Low",
    }
}

/// Is `r` finished -- answered, failed or refused? Only a finished row has completion events.
pub fn is_finished(r: &transport::RequestRecord) -> bool {
    r.status.is_some() || r.error.is_some()
}

/// The completion of `r`: `responseReceived` + `loadingFinished`, or `loadingFailed`. `on_worker` drops
/// the frame, as [`request_event`] does.
pub fn completion_events(r: &transport::RequestRecord, ctx: &NavContext, on_worker: bool) -> Vec<Value> {
    let id = request_id(r.id, &r.kind, &ctx.loader_id, ctx.prefix);
    let ty = resource_type(&r.kind);
    let start_s = ctx.epoch_s + r.started_ms / 1000.0;
    let end_s = start_s + r.ms / 1000.0;
    let mut out = Vec::with_capacity(2);
    match r.status {
        Some(status) => {
            // `final_url`, not `url`: after a redirect chain this is where the bytes came from, which is what
            // Chrome reports.
            let view = ResponseView {
                url: &r.final_url,
                status,
                // the status line's own text (`"OK"`, `"Moved Permanently"`; none over h2), where this said `""`
                status_text: &r.status_text,
                headers: &r.headers,
                navigation: is_navigation(&r.kind),
                from_cache: r.from_cache,
                bytes: r.bytes,
                response_time_ms: (ctx.wall_s + (r.started_ms + r.ms) / 1000.0) * 1000.0,
            };
            let mut params = json!({
                "requestId": id,
                "loaderId": if on_worker { "" } else { ctx.loader_id.as_str() },
                "timestamp": end_s,
                "type": ty,
                "response": view.to_json(),
                "hasExtraInfo": false
            });
            if !on_worker {
                params["frameId"] = json!(ctx.frame_id);
            }
            out.push(json!({ "method": "Network.responseReceived", "params": params }));
            out.push(json!({
                "method": "Network.loadingFinished",
                "params": {
                    "requestId": id,
                    "timestamp": end_s,
                    "encodedDataLength": r.bytes
                }
            }));
        }
        None => {
            let mut params = json!({
                "requestId": id,
                "timestamp": end_s,
                "type": ty,
                // The transport's own message, not a net:: code invented to look like
                // Chrome's. A client that prints it learns what actually happened.
                "errorText": r.error.clone().unwrap_or_else(|| "net::ERR_FAILED".to_string()),
                "canceled": false
            });
            // A request the engine REFUSED (mixed content) is Chrome's
            // `errorText: ""` + `blockedReason` -- measured on an https page's http script.
            if let Some(reason) = &r.blocked_reason {
                params["errorText"] = json!("");
                params["blockedReason"] = json!(reason);
            }
            // A request a DevTools client failed as `BlockedByClient` keeps its error text and says why.
            if r.error.as_deref() == Some(BLOCKED_BY_INSPECTOR) {
                params["blockedReason"] = json!("inspector");
            }
            out.push(json!({ "method": "Network.loadingFailed", "params": params }));
        }
    }
    out
}

#[cfg(test)]
/// Every `Network.*` event for `rows` as the PAGE reports them, in the order the requests started: each
/// row's `requestWillBeSent`, then its completion when it has one.
pub fn events(
    rows: &[transport::RequestRecord],
    frame_id: &str,
    loader_id: &str,
    document_url: &str,
    prefix: i64,
    epoch_s: f64,
    wall_s: f64,
) -> Vec<Value> {
    let ctx = NavContext {
        frame_id: frame_id.to_string(),
        loader_id: loader_id.to_string(),
        document_url: document_url.to_string(),
        prefix,
        epoch_s,
        wall_s,
    };
    let mut out = Vec::with_capacity(rows.len() * 3);
    for r in rows {
        out.push(request_event(r, &ctx, None));
        for k in 0..r.hops.len() {
            out.push(hop_request_event(r, Some(k), &ctx, None));
        }
        if is_finished(r) {
            out.extend(completion_events(r, &ctx, false));
        }
    }
    out
}

// ── streaming, and the worker targets ────────────────────────────────────────
//
// THE DEFECT. Every `Network.*` event went out in two batches -- the document's at commit, everything
// else once the load step returned -- and never again. A request a page made after its load (a
// dedicated worker's own fetch, a service worker's install-time fetch, a timer's beacon) was in the
// ledger and never on the wire. Measured with `tools/parity`'s M1 fixture on binary sha256:6f7cdbb5:
// korvus SERVED sw.js, sw-import.js, sw-fetched.txt, w.js and w-fetched.txt and reported none of them.
//
// Chrome reports them on the TARGET that made them (see `transport::Reporter` for the measured table),
// and only to a client that auto-attached. So this is two things: a flush that runs whenever the
// ledger has moved (the message loop calls it on every command and on a short tick while idle), and
// the child targets -- announced with `Target.attachedToTarget` once a client arms auto-attach on the
// page, each with a session of its own on which its requests are reported once that session has
// `Network.enable`d.

/// Where a flushed event goes: the page's session, or a child target's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stamp {
    Page,
    Session(String),
}

/// One auto-attached worker target.
#[derive(Debug, Clone)]
pub struct Child {
    pub target: transport::ChildTarget,
    pub session_id: String,
    /// Has the client sent `Network.enable` on this child's session?
    pub network: bool,
    /// Has the client sent `Runtime.enable` there (its console is reported from then on)?
    pub runtime: bool,
    /// `Inspector.workerScriptLoaded` has gone out on its session.
    pub script_loaded_sent: bool,
}

/// Where one row's event can be reported right now.
enum Place {
    Page,
    Child(usize),
    /// Not yet: the target is not attached, or its session has not enabled `Network`.
    Later,
}

/// A connection's network reporting: which rows have been announced and to whom.
#[derive(Default)]
pub struct Reporting {
    /// `Network.enable` on the page.
    pub page_network: bool,
    nav: Option<NavContext>,
    requested: std::collections::HashSet<u64>,
    /// How many of a requested row's redirect hops have had their `requestWillBeSent` (N27).
    hops_sent: std::collections::HashMap<u64, usize>,
    completed: std::collections::HashSet<u64>,
    /// Has a client asked the page to auto-attach its children?
    auto_attach: bool,
    children: Vec<Child>,
}

impl Reporting {
    /// A new document: its predecessor's children are detached, and nothing of it has been reported.
    pub fn begin_navigation(&mut self, ctx: NavContext) -> Vec<(Stamp, Value)> {
        let out = self
            .children
            .drain(..)
            .map(|c| {
                (Stamp::Page, json!({ "method": "Target.detachedFromTarget",
                    "params": { "sessionId": c.session_id, "targetId": c.target.id } }))
            })
            .collect();
        self.requested.clear();
        self.hops_sent.clear();
        self.completed.clear();
        self.nav = Some(ctx);
        out
    }

    /// The `requestId` the `Network.*` events of ledger row `row` (of destination `kind`) carry -- a
    /// paused request's `networkId`. `None` before a navigation has given this connection its context.
    pub fn network_id(&self, row: u64, kind: &str) -> Option<String> {
        let ctx = self.nav.as_ref()?;
        Some(request_id(row, kind, &ctx.loader_id, ctx.prefix))
    }

    /// The frame the current document's requests name.
    pub fn frame_id(&self) -> Option<String> {
        self.nav.as_ref().map(|c| c.frame_id.clone())
    }

    /// The document's URL once its request has followed its redirects: the context is made before the request is
    /// sent (so a pause of the document request already has it), with the URL it was asked for.
    pub fn set_document_url(&mut self, url: &str) {
        if let Some(c) = self.nav.as_mut() {
            c.document_url = url.to_string();
        }
    }

    /// `Target.setAutoAttach` on the page: `autoAttach` arms or disarms child announcement.
    pub fn set_auto_attach(&mut self, on: bool) {
        self.auto_attach = on;
    }

    /// The index of the child whose session is `session_id`.
    pub fn child(&self, session_id: &str) -> Option<usize> {
        self.children.iter().position(|c| c.session_id == session_id)
    }

    /// The target id of the child whose index is `idx`.
    pub fn child_target(&self, idx: usize) -> Option<String> {
        self.children.get(idx).map(|c| c.target.id.clone())
    }

    /// `Runtime.enable` on a child's session.
    pub fn enable_child_runtime(&mut self, idx: usize) {
        if let Some(c) = self.children.get_mut(idx) {
            c.runtime = true;
        }
    }

    /// The children whose `Inspector.workerScriptLoaded` is still owed (their session enabled `Runtime`): marked sent,
    /// as `(session, target)`, when `loaded` says the worker ran its script.
    pub fn take_script_loaded(&mut self, loaded: impl Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.children.iter_mut().filter(|c| c.runtime && !c.script_loaded_sent) {
            if loaded(&c.target.id) {
                c.script_loaded_sent = true;
                out.push(c.session_id.clone());
            }
        }
        out
    }

    /// Every child whose session enabled `Runtime`: its index, session and target id.
    pub fn runtime_children(&self) -> Vec<(usize, String, String)> {
        self.children.iter().enumerate().filter(|(_, c)| c.runtime).map(|(i, c)| (i, c.session_id.clone(), c.target.id.clone())).collect()
    }

    /// `Network.enable` on a child's session.
    pub fn enable_child_network(&mut self, idx: usize) {
        if let Some(c) = self.children.get_mut(idx) {
            c.network = true;
        }
    }

    fn place(&self, target: Option<&str>) -> Place {
        let Some(id) = target else {
            return if self.page_network { Place::Page } else { Place::Later };
        };
        match self.children.iter().position(|c| c.target.id == id) {
            Some(i) if self.children[i].network => Place::Child(i),
            Some(_) => Place::Later,
            // A target no page client attaches to (a shared worker) or one not announced yet.
            None => Place::Later,
        }
    }

    /// Everything the ledger now holds that this connection has not reported, in start order: child
    /// announcements first, then each row's `requestWillBeSent` and completion on the session that
    /// reports it. `new_session` mints a child session id.
    pub fn flush(
        &mut self,
        rows: &[transport::RequestRecord],
        targets: &[transport::ChildTarget],
        mut new_session: impl FnMut() -> String,
    ) -> Vec<(Stamp, Value)> {
        let Some(ctx) = self.nav.clone() else { return Vec::new() };
        let mut out = Vec::new();
        if self.auto_attach {
            for t in targets {
                // Chrome's page-level auto-attach announces dedicated and service workers, and not a
                // shared worker (measured).
                if !matches!(t.kind.as_str(), "worker" | "service_worker") || self.children.iter().any(|c| c.target.id == t.id) {
                    continue;
                }
                let session_id = new_session();
                out.push((
                    Stamp::Page,
                    json!({ "method": "Target.attachedToTarget", "params": {
                        "sessionId": session_id,
                        "targetInfo": { "targetId": t.id, "type": t.kind, "title": t.url, "url": t.url,
                                        "attached": true, "canAccessOpener": false },
                        // The worker is already running: this engine starts a worker when the page
                        // asks, not when a debugger releases it, and says so rather than claiming a
                        // pause it did not take.
                        "waitingForDebugger": false
                    } }),
                ));
                self.children.push(Child { target: t.clone(), session_id, network: false, runtime: false, script_loaded_sent: false });
            }
        }
        for r in rows {
            // The request as first sent, then every redirect it has followed since -- each on the session that
            // reports the request (N27).
            let sent = self.hops_sent.get(&r.id).copied().unwrap_or(0);
            if !self.requested.contains(&r.id) || sent < r.hops.len() {
                let (stamp, worker) = match self.place(r.reporter.target.as_deref()) {
                    Place::Page => (Stamp::Page, None),
                    Place::Child(i) => {
                        let c = &self.children[i];
                        (Stamp::Session(c.session_id.clone()), Some(c.target.clone()))
                    }
                    Place::Later => continue,
                };
                if !self.requested.contains(&r.id) {
                    out.push((stamp.clone(), request_event(r, &ctx, worker.as_ref())));
                    self.requested.insert(r.id);
                }
                for k in sent..r.hops.len() {
                    out.push((stamp.clone(), hop_request_event(r, Some(k), &ctx, worker.as_ref())));
                }
                self.hops_sent.insert(r.id, r.hops.len());
            }
            if is_finished(r) && !self.completed.contains(&r.id) {
                let target = r.reporter.response_target.as_deref().or(r.reporter.target.as_deref());
                let (stamp, on_worker) = match self.place(target) {
                    Place::Page => (Stamp::Page, false),
                    Place::Child(i) => (Stamp::Session(self.children[i].session_id.clone()), true),
                    Place::Later => continue,
                };
                for ev in completion_events(r, &ctx, on_worker) {
                    out.push((stamp.clone(), ev));
                }
                self.completed.insert(r.id);
            }
        }
        // the page's WebSockets, as they went (kept only while the page's `Network` is on)
        if self.page_network {
            for (id, at, ev) in transport::websocket::take_devtools_events() {
                out.push((Stamp::Page, websocket_event(id, at, &ev, &ctx)));
            }
        }
        out
    }
}

/// One WebSocket event as Chrome's `Network.webSocket*` reports it. The socket's `requestId` is of the page's request
/// ids, numbered apart from the ledger's rows.
fn websocket_event(id: u64, at: std::time::Instant, ev: &transport::websocket::DevtoolsWs, ctx: &NavContext) -> Value {
    use transport::websocket::DevtoolsWs;
    let request_id = format!("{}.{}", ctx.prefix, 1_000_000 + id);
    let timestamp = crate::monotonic_s_at(at);
    let headers = |h: &[(String, String)]| {
        let mut m = serde_json::Map::new();
        for (k, v) in h {
            m.insert(k.clone(), json!(v));
        }
        Value::Object(m)
    };
    match ev {
        DevtoolsWs::Created { url } => json!({ "method": "Network.webSocketCreated", "params": {
            "requestId": request_id, "url": url, "initiator": { "type": "script" } } }),
        DevtoolsWs::WillSendHandshake { headers: h } => {
            let wall = std::time::SystemTime::now()
                .checked_sub(at.elapsed())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            json!({ "method": "Network.webSocketWillSendHandshakeRequest", "params": {
                "requestId": request_id, "timestamp": timestamp, "wallTime": wall, "request": { "headers": headers(h) } } })
        }
        DevtoolsWs::HandshakeResponse { status, status_text, headers: h } => json!({ "method": "Network.webSocketHandshakeResponseReceived", "params": {
            "requestId": request_id, "timestamp": timestamp,
            "response": { "status": status, "statusText": status_text, "headers": headers(h) } } }),
        DevtoolsWs::Frame { sent, opcode, payload } => {
            // a text frame's payload as itself, a binary one's in base64 (`WebSocketFrame.payloadData`)
            let data = if *opcode == 1 {
                String::from_utf8_lossy(payload).into_owned()
            } else {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(payload)
            };
            let method = if *sent { "Network.webSocketFrameSent" } else { "Network.webSocketFrameReceived" };
            json!({ "method": method, "params": { "requestId": request_id, "timestamp": timestamp,
                "response": { "opcode": opcode, "mask": sent, "payloadData": data } } })
        }
        DevtoolsWs::Error { message } => json!({ "method": "Network.webSocketFrameError", "params": {
            "requestId": request_id, "timestamp": timestamp, "errorMessage": message } }),
        DevtoolsWs::Closed => json!({ "method": "Network.webSocketClosed", "params": { "requestId": request_id, "timestamp": timestamp } }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, url: &str, kind: &str, status: Option<u16>) -> transport::RequestRecord {
        transport::RequestRecord {
            id,
            url: url.to_string(),
            final_url: url.to_string(),
            kind: kind.to_string(),
            method: "GET".into(),
            status,
            status_text: if status.is_some() { "OK".into() } else { String::new() },
            hops: Vec::new(),
            bytes: 11,
            from_cache: false,
            ms: 5.0,
            initiator: None,
            error: status.is_none().then(|| "connect timed out".to_string()),
            headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
            started_ms: 1.0,
            blocked_reason: None,
            reporter: Default::default(),
            priority: None,
        }
    }

    // THE ONE PROPERTY A CLIENT CANNOT WORK WITHOUT: the id is the same in all three events of one
    // request. Playwright joins them by it, so an id that changes between events produces a request
    // that never completes and a `page.goto` that waits on it.
    #[test]
    fn one_request_id_survives_all_three_events() {
        let rows = [row(1, "http://h/a.js", "script", Some(200))];
        let ev = events(&rows, "FRAME", "LOADER", "http://h/", 42, 100.0, 1_700_000_000.0);
        assert_eq!(ev.len(), 3, "will-be-sent, response-received, loading-finished");
        let ids: Vec<&str> =
            ev.iter().map(|e| e["params"]["requestId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["42.1", "42.1", "42.1"]);
        assert_eq!(
            ev.iter().map(|e| e["method"].as_str().unwrap()).collect::<Vec<_>>(),
            ["Network.requestWillBeSent", "Network.responseReceived", "Network.loadingFinished"]
        );
    }

    // The main document's requestId IS the loaderId, as Chrome mints it -- that equality is how a
    // client tells the navigation request from every other request of the navigation.
    #[test]
    fn the_document_request_carries_the_loader_id() {
        let rows = [row(1, "http://h/", "document", Some(200))];
        let ev = events(&rows, "FRAME", "LOADER", "http://h/", 42, 0.0, 0.0);
        assert_eq!(ev[0]["params"]["requestId"], json!("LOADER"));
        assert_eq!(ev[0]["params"]["type"], json!("Document"));
    }

    // A failed request ends in `loadingFailed`, never in a `responseReceived` with a made-up status.
    #[test]
    fn a_failed_request_ends_in_loading_failed() {
        let rows = [row(1, "http://h/gone.css", "style", None)];
        let ev = events(&rows, "FRAME", "LOADER", "http://h/", 42, 0.0, 0.0);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1]["method"], json!("Network.loadingFailed"));
        assert_eq!(ev[1]["params"]["errorText"], json!("connect timed out"));
        assert_eq!(ev[1]["params"]["type"], json!("Stylesheet"));
    }

    // A refused request is `errorText: ""` + `blockedReason`, as Chrome reports an https page's http
    // script -- not a transport message, because nothing reached the transport.
    #[test]
    fn a_blocked_request_carries_its_blocked_reason() {
        let mut r = row(1, "http://h/s.js", "script", None);
        r.blocked_reason = Some("mixed-content".into());
        r.error = Some(String::new());
        let ev = events(&[r], "FRAME", "LOADER", "https://h/", 42, 0.0, 0.0);
        assert_eq!(ev[1]["params"]["blockedReason"], json!("mixed-content"));
        assert_eq!(ev[1]["params"]["errorText"], json!(""));
    }

    fn nav() -> NavContext {
        NavContext { frame_id: "FRAME".into(), loader_id: "LOADER".into(), document_url: "http://h/".into(), prefix: 42, epoch_s: 0.0, wall_s: 0.0 }
    }

    fn worker(id: &str, kind: &str) -> transport::ChildTarget {
        transport::ChildTarget { id: id.into(), kind: kind.into(), url: format!("http://h/{id}.js") }
    }

    // A request made after the load is reported when it happens, not never: the flush reports what the
    // ledger has and nothing twice, and a row still in flight gets its completion on a later flush.
    #[test]
    fn a_later_request_is_reported_by_a_later_flush_and_nothing_twice() {
        let mut rep = Reporting { page_network: true, ..Default::default() };
        let _ = rep.begin_navigation(nav());
        let mut inflight = row(1, "http://h/a.txt", "empty", None);
        inflight.error = None;
        let ev = rep.flush(&[inflight.clone()], &[], || "S".into());
        assert_eq!(ev.len(), 1, "requestWillBeSent only: the request has not finished");
        assert!(rep.flush(&[inflight.clone()], &[], || "S".into()).is_empty(), "nothing is reported twice");
        let done = row(1, "http://h/a.txt", "empty", Some(200));
        let ev = rep.flush(&[done.clone(), row(2, "http://h/b.txt", "empty", Some(200))], &[], || "S".into());
        let methods: Vec<&str> = ev.iter().map(|(_, e)| e["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["Network.responseReceived", "Network.loadingFinished", "Network.requestWillBeSent",
                             "Network.responseReceived", "Network.loadingFinished"]);
    }

    // The measured Chrome shape: a dedicated worker's script is requested on the page and answered on
    // the worker; the worker's own fetch is the worker's, start to end; a shared worker is not attached.
    #[test]
    fn worker_requests_are_reported_on_the_worker_session_once_it_enables_network() {
        let mut rep = Reporting { page_network: true, ..Default::default() };
        let _ = rep.begin_navigation(nav());
        rep.set_auto_attach(true);
        let targets = [worker("W", "worker"), worker("SH", "shared_worker")];
        let mut script = row(1, "http://h/w.js", "worker", Some(200));
        script.reporter.response_target = Some("W".into());
        let mut own = row(2, "http://h/w-fetched.txt", "empty", Some(200));
        own.reporter.target = Some("W".into());
        let mut shared = row(3, "http://h/shw-fetched.txt", "empty", Some(200));
        shared.reporter.target = Some("SH".into());
        let rows = [script, own, shared];
        let ev = rep.flush(&rows, &targets, || "CHILD".into());
        assert_eq!(ev[0].1["method"], json!("Target.attachedToTarget"));
        assert_eq!(ev[0].1["params"]["targetInfo"]["type"], json!("worker"));
        assert_eq!(ev.len(), 2, "attach + the page's requestWillBeSent for w.js; the rest waits for Network.enable");
        assert_eq!(ev[1], (Stamp::Page, ev[1].1.clone()));
        let idx = rep.child("CHILD").unwrap();
        rep.enable_child_network(idx);
        let ev = rep.flush(&rows, &targets, || "X".into());
        let got: Vec<(Stamp, &str, &str)> = ev.iter().map(|(s, e)| (s.clone(), e["method"].as_str().unwrap(), e["params"]["requestId"].as_str().unwrap())).collect();
        let child = Stamp::Session("CHILD".into());
        assert_eq!(got, [
            (child.clone(), "Network.responseReceived", "42.1"), (child.clone(), "Network.loadingFinished", "42.1"),
            (child.clone(), "Network.requestWillBeSent", "42.2"), (child.clone(), "Network.responseReceived", "42.2"),
            (child.clone(), "Network.loadingFinished", "42.2"),
        ]);
        assert!(ev[2].1["params"].get("frameId").is_none(), "a worker's request names no frame");
        assert_eq!(ev[2].1["params"]["initiator"]["type"], json!("script"));
    }

    // Without auto-attach a worker's requests are not the page's to report (Chrome's page session does
    // not see them), and a navigation detaches the previous document's children.
    #[test]
    fn without_auto_attach_worker_requests_stay_off_the_page_and_navigation_detaches() {
        let mut rep = Reporting { page_network: true, ..Default::default() };
        let _ = rep.begin_navigation(nav());
        let mut own = row(1, "http://h/w-fetched.txt", "empty", Some(200));
        own.reporter.target = Some("W".into());
        assert!(rep.flush(&[own], &[worker("W", "worker")], || "S".into()).is_empty());
        rep.set_auto_attach(true);
        let _ = rep.flush(&[], &[worker("W", "worker")], || "S".into());
        let ev = rep.begin_navigation(nav());
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1["method"], json!("Target.detachedFromTarget"));
    }

    // `type` is Chrome's ResourceType vocabulary, mapped from Fetch's destination -- the two are
    // different alphabets for one fact, and a client reads the protocol's.
    #[test]
    fn destinations_map_onto_chromes_resource_types() {
        for (dest, want) in [
            ("document", "Document"),
            ("iframe", "Document"),
            ("style", "Stylesheet"),
            ("image", "Image"),
            ("font", "Font"),
            ("script", "Script"),
            ("video", "Media"),
            ("empty", "Fetch"),
            ("manifest", "Manifest"),
            ("speculationrules", "Other"),
        ] {
            assert_eq!(resource_type(dest), want, "destination {dest}");
        }
    }

    // The response's headers and mime type are the SERVER's, not a shape: a `responseReceived` whose
    // headers are `{}` when the engine holds them would be a fixed value standing in for real data.
    #[test]
    fn the_response_carries_the_servers_headers_and_mime_type() {
        let rows = [row(1, "http://h/", "document", Some(200))];
        let ev = events(&rows, "FRAME", "LOADER", "http://h/", 42, 0.0, 0.0);
        let resp = &ev[1]["params"]["response"];
        assert_eq!(resp["mimeType"], json!("text/html"), "parameters are stripped from content-type");
        assert_eq!(resp["headers"]["content-type"], json!("text/html; charset=utf-8"));
        assert_eq!(resp["encodedDataLength"], json!(11));
        assert_eq!(resp["statusText"], json!("OK"), "the status line's own text");
    }

    fn hop(url: &str, status: u16, text: &str, next: &str, at_ms: f64) -> transport::RedirectHop {
        transport::RedirectHop {
            url: url.into(),
            status,
            status_text: text.into(),
            headers: vec![
                ("location".into(), next.into()),
                ("set-cookie".into(), "a=1".into()),
                ("set-cookie".into(), "b=2".into()),
                ("x-dup".into(), "one".into()),
                ("x-dup".into(), "two".into()),
                ("content-type".into(), "text/plain".into()),
            ],
            next_url: next.into(),
            next_method: "GET".into(),
            at_ms,
        }
    }

    // N27, Chrome 150's shape (three runs alike): a navigation `302 -> 301 -> 200` is
    // one requestId and three `requestWillBeSent`, each naming its own url as `request.url` and `documentURL`, the
    // second and third carrying the previous hop's response; one `responseReceived`, for the last.
    #[test]
    fn a_redirect_chain_is_one_request_with_a_request_will_be_sent_per_hop() {
        let mut r = row(1, "http://h/nav-r1", "document", Some(200));
        r.final_url = "http://h/landing".into();
        r.hops = vec![hop("http://h/nav-r1", 302, "Found", "http://h/nav-r2", 2.0), hop("http://h/nav-r2", 301, "Moved Permanently", "http://h/landing", 3.0)];
        let ev = events(&[r], "FRAME", "LOADER", "http://h/landing", 42, 100.0, 0.0);
        let methods: Vec<&str> = ev.iter().map(|e| e["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["Network.requestWillBeSent", "Network.requestWillBeSent", "Network.requestWillBeSent", "Network.responseReceived", "Network.loadingFinished"]);
        let urls: Vec<(&str, &str)> = ev[..3].iter().map(|e| (e["params"]["request"]["url"].as_str().unwrap(), e["params"]["documentURL"].as_str().unwrap())).collect();
        assert_eq!(urls, [("http://h/nav-r1", "http://h/nav-r1"), ("http://h/nav-r2", "http://h/nav-r2"), ("http://h/landing", "http://h/landing")]);
        assert!(ev[0]["params"].get("redirectResponse").is_none(), "the first hop follows no redirect");
        let rr = &ev[1]["params"]["redirectResponse"];
        assert_eq!((rr["url"].as_str(), rr["status"].as_u64(), rr["statusText"].as_str()), (Some("http://h/nav-r1"), Some(302), Some("Found")));
        assert_eq!(ev[2]["params"]["redirectResponse"]["status"], json!(301));
        assert!(ev.iter().all(|e| e["params"]["requestId"] == json!("LOADER")), "one request, one id");
        let t = ev[1]["params"]["timestamp"].as_f64().unwrap();
        assert!((t - 100.002).abs() < 1e-9, "a hop starts when the redirect arrived: {t}");
    }

    // Repeated response headers are JOINED -- with "\n" for a navigation, ", " for a subresource -- and the cookie
    // headers are not reported at all (Chrome 150, both measured on the same fixture; see `headers_object`).
    #[test]
    fn repeated_headers_are_joined_as_chrome_reports_them_and_cookies_are_not() {
        let nav = hop("http://h/a", 302, "Found", "http://h/b", 1.0);
        let h = headers_object(&nav.headers, true);
        assert_eq!(h["x-dup"], json!("one\ntwo"));
        assert!(h.get("set-cookie").is_none(), "Set-Cookie never reaches a Network.Response: {h}");
        let h = headers_object(&nav.headers, false);
        assert_eq!(h["x-dup"], json!("one, two"));
        assert!(h.get("set-cookie").is_none());
        let mut r = row(1, "http://h/f-r1", "empty", Some(200));
        r.hops = vec![hop("http://h/f-r1", 307, "Temporary Redirect", "http://h/f-r2", 5.0)];
        let ev = events(&[r], "FRAME", "LOADER", "http://h/landing", 42, 0.0, 0.0);
        assert_eq!(ev[1]["params"]["documentURL"], json!("http://h/landing"), "a subresource names its document");
        assert_eq!(ev[1]["params"]["redirectResponse"]["headers"]["x-dup"], json!("one, two"));
    }

    // A hop recorded after the request was first reported is reported by a later flush, once.
    #[test]
    fn a_later_hop_is_reported_by_a_later_flush() {
        let mut rep = Reporting { page_network: true, ..Default::default() };
        let _ = rep.begin_navigation(nav());
        let mut inflight = row(1, "http://h/f-r1", "empty", None);
        inflight.error = None;
        assert_eq!(rep.flush(&[inflight.clone()], &[], || "S".into()).len(), 1);
        inflight.hops = vec![hop("http://h/f-r1", 307, "Temporary Redirect", "http://h/f-r2", 5.0)];
        let ev = rep.flush(&[inflight.clone()], &[], || "S".into());
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1["params"]["request"]["url"], json!("http://h/f-r2"));
        assert!(rep.flush(&[inflight], &[], || "S".into()).is_empty(), "nothing is reported twice");
    }
}

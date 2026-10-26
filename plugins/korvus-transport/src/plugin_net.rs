//! The `network` plugin slot (goal 1020 step 2): a request hop before it is sent and a response as it arrives, as
//! [`plugin_host`] hands them to the loaded plugins.
//!
//! Every HTTP request of the process -- a navigation, a subresource, `fetch`/XHR, a module, a worker's script, a
//! CDP-initiated load, whichever path called [`crate::fetch`] -- passes [`crate::fetch`]'s hop loop, which asks the
//! plugins at each hop, before DevTools' pause; a WebSocket asks them before its handshake (`websocket.rs`). With no
//! network plugin loaded neither is reached (`plugin_host::has_network`).
use crate::intercept::Fulfilment;
use crate::FetchResponse;

/// One hop as the plugins see and change it.
pub(crate) struct Hop {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub destination: String,
    pub top_level: String,
    /// What the plugins changed: a url or method restarts the hop with it.
    pub new_url: Option<String>,
    pub new_method: Option<String>,
    pub headers_changed: bool,
    pub body: Option<Vec<u8>>,
    pub proxy: Option<String>,
    pub fulfilment: Option<Fulfilment>,
}

impl Hop {
    pub(crate) fn new(url: &str, method: &str, headers: &[(String, String)], destination: &str, top_level: Option<&str>) -> Hop {
        Hop {
            url: url.to_string(),
            method: method.to_string(),
            headers: headers.to_vec(),
            destination: destination.to_string(),
            top_level: top_level.unwrap_or("").to_string(),
            new_url: None,
            new_method: None,
            headers_changed: false,
            body: None,
            proxy: None,
            fulfilment: None,
        }
    }

    /// ABI 3 step 14 (D7): the edits a `modify_and_resume` carried, applied to this held hop just before it is
    /// released -- each through the same [`plugin_host::RequestCtx`] setter (and so the same validation) a plugin's
    /// synchronous `on_request` edit takes, so a resumed hop's url / method / headers / body land exactly as they
    /// would have inline. A `None` field leaves that part unchanged; a `Some(headers)` REPLACES the hop's header set
    /// whole (an empty list clears it), as the resume means it to.
    pub(crate) fn apply_hold_modify(&mut self, m: plugin_host::HoldModify) {
        use plugin_host::RequestCtx;
        if let Some(u) = m.url {
            self.set_url(&u);
        }
        if let Some(meth) = m.method {
            self.set_method(&meth);
        }
        if let Some(hs) = m.headers {
            self.headers.clear();
            self.headers_changed = true;
            for (k, v) in hs {
                self.set_header(k.as_str(), Some(v.as_str()));
            }
        }
        if let Some(b) = m.body {
            self.set_body(&b);
        }
    }
}

impl plugin_host::RequestCtx for Hop {
    fn url(&self) -> String {
        self.new_url.clone().unwrap_or_else(|| self.url.clone())
    }
    fn method(&self) -> String {
        self.new_method.clone().unwrap_or_else(|| self.method.clone())
    }
    fn headers(&self) -> Vec<(String, String)> {
        self.headers.clone()
    }
    fn destination(&self) -> String {
        self.destination.clone()
    }
    fn top_level_url(&self) -> String {
        self.top_level.clone()
    }
    fn set_url(&mut self, url: &str) {
        if url::Url::parse(url).is_ok() {
            self.new_url = Some(url.to_string());
        }
    }
    fn set_method(&mut self, method: &str) {
        if wreq::Method::from_bytes(method.as_bytes()).is_ok() {
            self.new_method = Some(method.to_string());
        }
    }
    fn set_header(&mut self, name: &str, value: Option<&str>) {
        if wreq::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return;
        }
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        if let Some(v) = value {
            if wreq::header::HeaderValue::from_str(v).is_err() {
                return;
            }
            self.headers.push((name.to_string(), v.to_string()));
        }
        self.headers_changed = true;
    }
    fn set_body(&mut self, body: &[u8]) {
        self.body = Some(body.to_vec());
    }
    fn set_proxy(&mut self, proxy: &str) {
        self.proxy = Some(proxy.to_string());
    }
    fn fulfill(&mut self, status: u16, headers: Vec<(String, String)>, body: Vec<u8>) {
        let phrase = wreq::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("").to_string();
        self.fulfilment = Some(Fulfilment { status, phrase, headers, body });
    }
}

/// A plugin's proxy for one hop: a proxy URL (`http://`, `https://`, `socks5://`, with a login in it), or `direct`.
pub(crate) fn with_proxy(rb: wreq::RequestBuilder, proxy: &str) -> wreq::RequestBuilder {
    if proxy == "direct" {
        // every host bypasses the stand-in proxy: no proxy at all for this hop
        return match wreq::Proxy::all("http://127.0.0.1:9") {
            Ok(p) => rb.proxy(p.no_proxy(wreq::NoProxy::from_string("*"))),
            Err(_) => rb,
        };
    }
    match wreq::Proxy::all(proxy) {
        Ok(p) => rb.proxy(p),
        Err(_) => rb,
    }
}

/// ABI 3 (step 12, D3, `on_proxy_auth`): a proxy answered a hop with `407 Proxy Authentication Required`. Ask the
/// loaded plugins for `proxy`'s credentials ([`plugin_host::on_proxy_auth`] -- last loaded first, the first non-empty
/// `"user:pass"` wins) and splice them into the proxy URL's userinfo, returning the authenticated proxy URL to retry
/// the hop through. [`with_proxy`] already honours a proxy URL with a login in it, and wreq then sends
/// `Proxy-Authorization` for a forward request and on a tunnel's `CONNECT` alike -- so the request authenticates
/// with no other change. Called when the request is built (the credentials ride the first hop, pre-empting the
/// 407); `None` -- the proxy's 407 then stands, exactly as before -- when no plugin supplies credentials, or
/// `proxy` carries no authority to hold a login (`""` / `direct`, or not a URL). `proxy` is the URL this hop is
/// exiting through (what a plugin set with `set_proxy`, else the context's own configured proxy).
pub(crate) fn proxy_auth(proxy: &str) -> Option<String> {
    if proxy.is_empty() || proxy == "direct" {
        return None;
    }
    let creds = plugin_host::on_proxy_auth(proxy)?;
    let mut u = url::Url::parse(proxy).ok()?;
    let (user, pass) = creds.split_once(':').unwrap_or((creds.as_str(), ""));
    // A proxy URL that cannot carry a login (no authority) leaves the 407 to stand.
    u.set_username(user).ok()?;
    u.set_password((!pass.is_empty()).then_some(pass)).ok()?;
    Some(u.into())
}

/// A response as the plugins see and change it.
struct Answer<'a> {
    resp: &'a mut FetchResponse,
}

impl plugin_host::ResponseCtx for Answer<'_> {
    fn status(&self) -> u16 {
        self.resp.status
    }
    fn url(&self) -> String {
        self.resp.url.clone()
    }
    fn headers(&self) -> Vec<(String, String)> {
        self.resp.headers.clone()
    }
    fn body(&self) -> Option<&[u8]> {
        (!self.resp.bytes.is_empty()).then_some(self.resp.bytes.as_slice())
    }
    fn set_header(&mut self, name: &str, value: Option<&str>) {
        self.resp.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        if let Some(v) = value {
            self.resp.headers.push((name.to_string(), v.to_string()));
        }
    }
    fn set_body(&mut self, body: &[u8]) {
        // The bytes are the body of record; the text view follows them, and Content-Length matches (the page is
        // about to read these, so the two must agree).
        self.resp.bytes = body.to_vec();
        self.resp.body = String::from_utf8_lossy(body).into_owned();
        self.set_header("content-length", Some(&body.len().to_string()));
    }
    fn remote_ip(&self) -> String {
        self.resp.remote_ip.clone()
    }
    fn http_version(&self) -> String {
        self.resp.http_version.clone()
    }
}

/// The plugins' turn at a response, before the page sees it.
pub(crate) fn answered(resp: &mut FetchResponse) {
    plugin_host::on_response(&mut Answer { resp });
}

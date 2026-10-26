//! REQUEST INTERCEPTION: the hook DevTools' `Fetch` domain pauses a request on.
//!
//! Chrome pauses a request in the browser, in front of the network service (`DevToolsURLLoaderInterceptor`): every
//! request a frame or its workers send, each redirect hop again, a CORS preflight on its own -- and never a `data:`
//! or `blob:` URL or a WebSocket handshake, which no URL loader carries (Chrome 150, `chrome150-a1s3.json`). The
//! client then answers each pause with one of three things, which are [`Paused`]'s variants.
//!
//! Here the hook is one process-global [`Interceptor`], installed by the CDP layer of the one page this process
//! serves, and consulted by the transport at each hop, after the hop's headers are known and before anything is
//! sent. Nothing is consulted while none is installed, so a run without DevTools pays one atomic load per hop.
//!
//! What the pause SHOWS is what Chrome's shows: the request as the renderer made it, before the network stack adds
//! its own headers ([`paused_headers`]). What an answer CHANGES follows Chrome too:
//! - `Continue` with a URL keeps the scheme (another scheme fails the request) and is invisible to the page -- its
//!   `Response.url` stays the original and `redirected` false;
//! - `Continue` with headers replaces the renderer's headers, and the network stack still adds its own;
//! - `Fulfill` answers without the network, through the same response path a network answer takes: its
//!   `Set-Cookie`s are stored, CORS checks it, a redirect status with a `Location` is followed;
//! - `Fail` ends the request with the network error the client named.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// One request, paused before it is sent.
#[derive(Debug, Clone)]
pub struct PausedRequest {
    /// The request's ledger row ([`crate::RequestRecord::id`]): the id its `Network.*` events carry.
    pub row: u64,
    /// The row's Fetch destination token, as the ledger records it (`"preflight"` for a CORS preflight).
    pub kind: String,
    /// The destination of the request a preflight precedes; the request's own otherwise. Chrome types a preflight's
    /// pause as its request (`XHR` for a `fetch()`'s).
    pub destination: String,
    /// The worker target that made it, when one did (see [`crate::Reporter`]).
    pub target: Option<String>,
    pub url: String,
    pub method: String,
    /// The renderer's headers, sorted as DevTools lists them ([`paused_headers`]).
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// The request's `priority` field, for `initialPriority`.
    pub priority: Option<String>,
}

/// What `Fetch.continueRequest` may change. Every field `None` is a plain continue.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub url: Option<String>,
    pub method: Option<String>,
    pub body: Option<Vec<u8>>,
    /// Replaces the renderer's headers (see the module comment).
    pub headers: Option<Vec<(String, String)>>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.url.is_none() && self.method.is_none() && self.body.is_none() && self.headers.is_none()
    }
}

/// `Fetch.fulfillRequest`: the response the client made up.
#[derive(Debug, Clone)]
pub struct Fulfilment {
    pub status: u16,
    /// The status line's phrase: the client's `responsePhrase`, or the code's standard one.
    pub phrase: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A client's answer to a pause.
#[derive(Debug, Clone)]
pub enum Paused {
    Continue(Overrides),
    Fulfill(Fulfilment),
    /// The network error the request ends with, as its ledger row and the page's error read it (`net::ERR_…`).
    Fail(String),
}

/// The future a pause resolves through: the client's answer, or a plain continue when the client went away.
pub type PauseFuture = Pin<Box<dyn Future<Output = Paused> + Send>>;

/// The DevTools side of interception.
pub trait Interceptor: Send + Sync {
    /// `None`: the request is not intercepted (no pattern matches it). Called on the net runtime before the request
    /// is sent; the future resolves when the client has answered.
    fn pause(&self, req: &PausedRequest) -> Option<PauseFuture>;

    /// An HTTP authentication challenge on a request (`Fetch.authRequired`, when the client enabled `Fetch` with
    /// `handleAuthRequests`): `None` when the client takes no challenges, else its answer. The response that
    /// challenged is held meanwhile.
    fn auth(&self, _challenge: &AuthChallenge) -> Option<AuthFuture> {
        None
    }
}

/// A `401` (or a proxy's `407`) and what it asked for, as `Fetch.AuthChallenge` names it.
#[derive(Debug, Clone)]
pub struct AuthChallenge {
    /// The paused request this challenge belongs to (as [`PausedRequest`] describes the hop).
    pub req: PausedRequest,
    /// `Server` or `Proxy`.
    pub source: &'static str,
    /// The origin of the challenger.
    pub origin: String,
    /// The scheme, as the challenge spelled it, lowercased (`basic`, `digest`).
    pub scheme: String,
    pub realm: String,
}

/// `Fetch.continueWithAuth`'s answer.
#[derive(Debug, Clone)]
pub enum AuthAnswer {
    /// `Default` and `CancelAuth`: the challenge's own response is the answer (a headless browser shows no dialog).
    Cancel,
    /// `ProvideCredentials`: the request is sent again with them.
    Provide { username: String, password: String },
}

/// The future an auth challenge resolves through.
pub type AuthFuture = Pin<Box<dyn Future<Output = AuthAnswer> + Send>>;

/// Ask the installed interceptor about an auth challenge.
pub(crate) fn auth(challenge: &AuthChallenge) -> Option<AuthFuture> {
    if !INSTALLED.load(Ordering::Acquire) {
        return None;
    }
    let i = INTERCEPTOR.read().ok()?.clone()?;
    i.auth(challenge)
}

/// The scheme and realm of a `WWW-Authenticate` / `Proxy-Authenticate` value: `Basic realm="x"` gives
/// `("basic", "x")`. Only the first challenge is read.
pub(crate) fn parse_challenge(v: &str) -> (String, String) {
    let v = v.trim();
    let (scheme, rest) = v.split_once(char::is_whitespace).unwrap_or((v, ""));
    let realm = rest
        .split(',')
        .find_map(|p| {
            let (k, val) = p.trim().split_once('=')?;
            k.trim().eq_ignore_ascii_case("realm").then(|| val.trim().trim_matches('"').to_string())
        })
        .unwrap_or_default();
    (scheme.to_ascii_lowercase(), realm)
}

static INSTALLED: AtomicBool = AtomicBool::new(false);
static INTERCEPTOR: RwLock<Option<Arc<dyn Interceptor>>> = RwLock::new(None);

/// Install (or, with `None`, remove) the process's interceptor.
pub fn set_interceptor(i: Option<Arc<dyn Interceptor>>) {
    let on = i.is_some();
    if let Ok(mut g) = INTERCEPTOR.write() {
        *g = i;
    }
    INSTALLED.store(on, Ordering::Release);
}

/// Ask the installed interceptor about `req`.
pub(crate) fn pause(req: &PausedRequest) -> Option<PauseFuture> {
    if !INSTALLED.load(Ordering::Acquire) {
        return None;
    }
    let i = INTERCEPTOR.read().ok()?.clone()?;
    i.pause(req)
}

/// Is any interceptor installed? A hop builds its [`PausedRequest`] only then.
pub(crate) fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// The headers the network stack adds after the pause, so DevTools never sees them on one (Chrome 150,
/// `chrome150-a1s3b.json`: a document's pause lists `Accept`, `Upgrade-Insecure-Requests`, `User-Agent` and the
/// three low-entropy client hints; a `fetch()`'s `Accept`, `Referer`, `User-Agent`, its own headers and the hints; the
/// same requests on the wire also carried `Host`, `Connection`, `Sec-Fetch-*`, `Accept-Encoding`,
/// `Accept-Language`). A preflight's pause is the exception: it lists `Sec-Fetch-Mode`.
const ADDED_AFTER_THE_PAUSE: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "cache-control",
    "pragma",
    "cookie",
    "priority",
    "accept-encoding",
    "accept-language",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-user",
    "sec-fetch-dest",
    "sec-fetch-storage-access",
];

/// What the network stack puts back after a `Fetch.continueRequest` replaced the renderer's headers: its own
/// additions, and the two request FIELDS it writes as headers whenever the request has none -- Chrome 150 sent
/// `Referer` and `User-Agent` on a request whose headers a client had replaced with one `X-Added`
/// (`chrome150-a1s3b.json`, `page.h`).
const KEPT_UNDER_AN_OVERRIDE: &[&str] = &["referer", "user-agent"];

/// The headers a pause shows, from the hop's full set: without the ones the stack adds later, each spelled as the
/// page wrote it or as Chrome spells it, sorted by name as DevTools' header dictionary lists them (`Accept` ...
/// `X-Probe`, then `sec-ch-ua` -- a byte order).
pub(crate) fn paused_headers(hop: &[(String, String)], spell: impl Fn(&str) -> String, preflight: bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = hop
        .iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            (preflight && k == "sec-fetch-mode") || !ADDED_AFTER_THE_PAUSE.contains(&k.as_str())
        })
        .map(|(k, v)| (spell(k), v.clone()))
        .collect();
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    out
}

/// The hop's headers once a client replaced the renderer's with `given`: the stack's own additions and
/// [`KEPT_UNDER_AN_OVERRIDE`] stay unless `given` names them, and everything else the renderer had is gone.
pub(crate) fn overridden_headers(hop: &[(String, String)], given: &[(String, String)]) -> Vec<(String, String)> {
    let named = |k: &str| given.iter().any(|(g, _)| g.eq_ignore_ascii_case(k));
    let mut out: Vec<(String, String)> = given.to_vec();
    for (k, v) in hop {
        let lower = k.to_ascii_lowercase();
        let stack = ADDED_AFTER_THE_PAUSE.contains(&lower.as_str()) || KEPT_UNDER_AN_OVERRIDE.contains(&lower.as_str());
        if stack && !named(k) {
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

/// A `Fetch.continueRequest` URL may not change the scheme: Chrome answers the command `{}` and the request fails
/// (`chrome150-a1s3.json`: an `ftp:` override, and the page's `fetch` rejected `TypeError: Failed to fetch`).
pub(crate) fn same_scheme(current: &url::Url, to: &str) -> Option<url::Url> {
    url::Url::parse(to).ok().filter(|u| u.scheme() == current.scheme())
}

/// A [`Fulfilment`] as the response the hop's response path reads: HTTP/1.1, as Chrome builds a fulfilled head
/// (`HTTP/1.1 <code> <phrase>`, `FetchHandler::FulfillRequest`), its phrase carried as a [`FulfilledPhrase`].
pub(crate) fn fulfilled_response(f: &Fulfilment) -> anyhow::Result<wreq::Response> {
    let mut b = http::Response::builder().status(f.status).version(http::Version::HTTP_11);
    for (k, v) in &f.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    let mut resp = b.body(f.body.clone())?;
    resp.extensions_mut().insert(FulfilledPhrase(f.phrase.clone()));
    Ok(wreq::Response::from(resp))
}

/// The status phrase of a fulfilled response (wreq's own `ReasonPhrase` cannot be made outside it).
#[derive(Debug, Clone)]
pub(crate) struct FulfilledPhrase(pub String);

#[cfg(test)]
mod tests {
    use super::parse_challenge;

    #[test]
    fn a_challenge_names_its_scheme_and_realm() {
        assert_eq!(parse_challenge("Basic realm=\"compat\""), ("basic".to_string(), "compat".to_string()));
        assert_eq!(parse_challenge("Digest realm=\"r\", qop=\"auth\", nonce=\"n\""), ("digest".to_string(), "r".to_string()));
        assert_eq!(parse_challenge("Bearer"), ("bearer".to_string(), String::new()));
    }
}

//! `transport` — the HTTP client, on `wreq`/`wreq-util` (BoringSSL) so the wire fingerprint (JA3/JA4/
//! HTTP/2 SETTINGS, header order) matches the persona bundle's declared preset. `fetch` is the one API
//! callers see; the cookie store (`cookies`) and the Fetch-Metadata derivation live here too.

use serde::{Deserialize, Serialize};

mod blink_headers;
mod client_hints;
mod content_coding;
/// The one cookie store (Chrome's `CookieMonster`) behind the network, `document.cookie` and `cookieStore`.
pub mod context_store;
pub mod cookies;
mod http_cache;
pub use http_cache::set_bypass as set_cache_disabled;
mod partition;
// H3 (2026-10-11): the HTTP/3 presets a bundle can name.
mod quic_presets;
pub use quic_presets::options_for as quic_options_for;
pub mod cors;
pub mod ech;
pub mod mime; // A/B C5: a response's MIME type, the three ways Chromium reads `Content-Type`.
pub use cors::{CredentialsMode, PageRequest, RedirectMode, ResponseType};
/// The WebSocket client (RFC 6455 / 7692) on the persona's wire -- its own module so it shares this
/// crate's client, cookie store and runtime without touching the fetch path. See its header.
pub mod websocket;
/// The hook DevTools' `Fetch` domain pauses a request on. See its header.
pub mod intercept;
// The `network` plugin slot: a hop and a response, as the loaded plugins see them.
mod plugin_net;

/// A fetch request as handed across the JS boundary.
///
/// The four fields after `body` are the REQUEST's own properties (Fetch §request), not the identity's:
/// they are what `Sec-Fetch-Dest` / `-Mode` / `-Site` / `-User` are computed from, per request, in
/// [`fetch`]. A caller that leaves them at their defaults gets the shape of a page-initiated
/// `fetch()` with no known initiator, which is the conservative reading (`dest: empty`, `mode: cors`,
/// `site: none`) rather than the navigation preset every request used to claim.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The request body as BYTES. A page hands `fetch`/XHR a string (sent as its UTF-8 encoding) or a
    /// `BufferSource`/`Blob` (sent byte for byte); both deserialise into this one field
    /// ([`string_or_bytes`]). It was `Option<String>`, so `fetch(u, {method: "POST", body: new
    /// Uint8Array([1, 2])})` put the seven bytes `1,2` on the wire -- a protobuf or wasm upload was
    /// corrupted before it left.
    #[serde(default, deserialize_with = "string_or_bytes")]
    pub body: Option<Vec<u8>>,
    /// Fetch's *request destination*: `"script"`, `"style"`, `"image"`, `"document"`, `"iframe"`,
    /// `"font"`, `"audio"`, `"video"`, `"track"`, `"embed"`, `"object"`, `"worker"`,
    /// `"sharedworker"`, `"manifest"`, `"report"`, ... The empty string is the *empty* destination
    /// (a page `fetch()` / XHR), which `Sec-Fetch-Dest` serialises as `empty`.
    ///
    /// NOT deserialised from JS: `destination` is not a `RequestInit` member, so a page that passed
    /// one would be dictating its own Sec-Fetch-Dest — the one header a Fetch Metadata check exists
    /// to trust. It is set by the Rust call site that knows what it is loading.
    #[serde(skip)]
    pub destination: Destination,
    /// Fetch's *request mode*. `mode` IS a `RequestInit` member, so it is read from JS — a page
    /// asking for `{mode: "no-cors"}` really does change the request, and the header must follow it.
    #[serde(default)]
    pub mode: Mode,
    /// The URL of the environment that initiated this request (the page for a subresource; `None`
    /// for a user-initiated navigation, which is what makes `Sec-Fetch-Site: none` correct there).
    #[serde(skip)]
    pub initiator: Option<String>,
    /// Navigation triggered by user activation → `Sec-Fetch-User: ?1`. Only ever set on navigations.
    #[serde(skip)]
    pub user_activated: bool,
    /// The persona's `Accept` for navigations (see `NavigatorProfile::accept_navigation`); `None` is Chromium's.
    #[serde(skip)]
    pub accept_navigation: Option<String>,
    /// The HTTP/2 `priority` value the page's loader states for this request (`""` = send none). `None`
    /// derives it from destination and mode the way Chromium does -- see [`chromium_priority`].
    #[serde(default)]
    pub priority: Option<String>,
    /// The page's `RequestInit.cache` mode (`"default"`, `"no-store"`, `"reload"`, `"no-cache"`,
    /// `"force-cache"`, `"only-if-cached"`), read from JS: it is a `RequestInit` member.
    #[serde(default)]
    pub cache: Option<String>,
    /// When the NAVIGATION this request fetches began -- before the driver set it up and the old
    /// document's `beforeunload` ran -- which HTML makes the new document's time origin (`FetchTiming::
    /// start`). `None` takes the moment the transport was asked. Chrome's `navigationStart` likewise
    /// precedes its `fetchStart` (`fetchStart > 0` on every Chrome 150 load).
    #[serde(skip)]
    pub navigation_start: Option<std::time::Instant>,
    /// **THE WIRE THIS REQUEST GOES OUT ON** — the ClientHello, the HTTP/2 settings and the claimed
    /// OS of the PERSONA THAT MADE IT. `None` means "no persona has claimed this request", and
    /// [`fetch`] refuses it rather than guessing.
    ///
    /// It lives on the REQUEST and not in a process global because the wire is a property of the
    /// identity that is fetching, and a process can hold more than one identity: `persona_surface`
    /// and `persona_diff` boot four bundles in one binary. The old shape was a `OnceLock` set at the
    /// first `Engine::assemble`, so the second bundle's assembly was refused (and moving the refusal
    /// to the socket instead poisoned every later fetch in the binary — measured and reverted at
    /// L7f.9). Neither branch could answer "which wire does THIS request carry", because the code had
    /// nowhere to put the answer. This is that place.
    ///
    /// `#[serde(skip)]`, like `destination`, and for the same reason with more at stake: a page that
    /// could name its own TLS profile through a `RequestInit` member would be choosing the browser
    /// its own request impersonates.
    ///
    /// Written by exactly one function, [`apply_persona_identity`] — the same one that writes the
    /// identity HEADERS — so a request whose `user-agent` says one browser cannot carry another's
    /// ClientHello. That is the property the old process-wide refusal existed to protect, held by
    /// construction instead of by promise.
    #[serde(skip)]
    pub wire: Option<spec::TlsProfile>,
    /// Which DevTools target reports this request. `#[serde(skip)]`: set by the Rust call site that
    /// knows which realm is fetching (a worker's isolate carries its own), never by a page.
    #[serde(skip)]
    pub reporter: Reporter,
    /// Fetch's `redirect` mode (a `RequestInit` member, read from JS).
    #[serde(default)]
    pub redirect: RedirectMode,
    /// Fetch's `credentials` mode (a `RequestInit` member, read from JS). Decides the CORS check; it
    /// does not yet decide whether the client jar attaches cookies.
    #[serde(default)]
    pub credentials: CredentialsMode,
    /// Set for a request a PAGE made (`fetch()`, XHR) -- and only then is the request tainted and
    /// CORS-checked (see `cors.rs`). `#[serde(skip)]`: the engine's page-request path sets it.
    #[serde(skip)]
    pub page: Option<PageRequest>,
    /// The request comes from a CROSS-SITE context: its URL is not same-site with the site for cookies of
    /// the document that asked, so it sends and stores only `SameSite=None` cookies, of its cross-site partition (see
    /// [`cookies::HopContext`], [`cookie_cross_site`]). `#[serde(skip)]`: the engine decides it from the requesting
    /// document, never a page.
    #[serde(skip)]
    pub cookie_cross_site: bool,
    /// The persona's browser blocks third-party cookies (Brave does, Chrome/Edge/Opera do
    /// not), which is what a cross-site context's `Sec-Fetch-Storage-Access` reports. Written by
    /// [`apply_persona_identity`] from the same persona as the identity headers.
    #[serde(skip)]
    pub third_party_cookies_blocked: bool,
    /// The top-level document's URL: a request back to ITS site from a cross-site context
    /// is the one a blocking browser reports `inactive` (with an `Origin`) rather than `none`.
    #[serde(skip)]
    pub top_level_url: Option<String>,
    /// The requesting realm sends no low-entropy Client Hints: a dedicated worker's own
    /// requests and the request for its script carry `user-agent` and no `sec-ch-ua*` -- Chrome 150 and
    /// Brave 151, over http and https.
    #[serde(skip)]
    pub no_client_hints: bool,
    /// The persona's high-entropy User-Agent hints as header values (`sec-ch-ua-full-version-list`, `-arch`,
    /// sent only where an origin's `Accept-CH` turned them on ([`client_hints`]). Written
    /// by [`apply_persona_identity`] with the rest of the identity.
    #[serde(skip)]
    pub high_entropy_hints: Option<std::sync::Arc<Vec<(String, String)>>>,
    /// The year Google Chrome's build reports in its `x-browser-year` and `x-browser-copyright` -- set when the
    /// persona is Google Chrome ([`chrome_build_year`]), `None` for another browser. Written by
    /// [`apply_persona_identity`].
    #[serde(skip)]
    pub chrome_build_year: Option<u16>,
    /// The requesting environment's URL is not one a referrer may name (a `blob:` worker):
    /// no `Referer`, while `initiator` -- the blob's ORIGIN -- still decides `Sec-Fetch-Site`.
    #[serde(skip)]
    pub no_referrer: bool,
    /// The requesting environment's origin is OPAQUE -- a sandboxed document without `allow-same-origin`, or a blob
    /// worker one made: it is same-origin with nothing, so every URL is `Sec-Fetch-Site:
    /// cross-site`, an `Origin` it carries is `null`, and only `include` sends credentials (Chrome 150, measured).
    #[serde(skip)]
    pub opaque_origin: bool,
    /// The request's referrer policy (Referrer Policy sec. 3): the element's `referrerpolicy`, the
    /// `RequestInit`'s, or the client's -- a `Referrer-Policy` header or `<meta name=referrer>` --
    /// resolved by the caller. `None` is the empty policy, which is Chrome's default
    /// `strict-origin-when-cross-origin`. A redirect response's own `Referrer-Policy` replaces it for
    /// the hops after it (Fetch "set request's referrer policy on redirect").
    #[serde(default, rename = "referrerPolicy", deserialize_with = "referrer_policy_token")]
    pub referrer_policy: Option<ReferrerPolicy>,
    /// Where the final response's HEAD goes the moment its headers are in -- status, headers, url,
    /// type, an empty body -- while the body is still being read (a page's `fetch()`
    /// resolves at the headers, as Chrome's does). `None` for a caller that only wants the whole
    /// response. `#[serde(skip)]`: the engine's, never a page's.
    #[serde(skip)]
    pub on_head: Option<HeadSink>,
    /// Stops reading the final response's body: the page aborted the request after its headers, or
    /// cancelled the body. The request then ends with an error.
    #[serde(skip)]
    pub cancel: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// Where each chunk of the final response's body goes as it arrives (Chrome streams a
    /// body -- a reader's first `read()` gets the first bytes the server flushed). The whole body is
    /// still assembled for the response the request answers.
    #[serde(skip)]
    pub on_chunk: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
}

/// The one-shot channel a request's response head is sent down ([`FetchRequest::on_head`]).
pub type HeadSink = std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<FetchResponse>>>>;

/// WHICH TARGET REPORTS A REQUEST.
///
/// Measured on stock Chrome 150.0.7871.46 with `Target.setAutoAttach {autoAttach, flatten,
/// waitForDebuggerOnStart}` on a page that starts a dedicated worker, a shared worker and a service
/// worker:
///
/// ```text
/// request            requestWillBeSent on     response events on      type     initiator
/// w.js               the page                 the WORKER's session    Script   other
/// w-fetched.txt      the worker               the worker              Fetch    script
/// w-import.js        the worker               the worker              Other    script
/// sw.js              the service worker       the service worker      Script   other
/// sw-fetched.txt     the service worker       the service worker      Fetch    script
/// shw.js             the page                 (none)                  Script   other
/// shw-fetched.txt    (none: a shared worker is not auto-attached from a page)
/// ```
///
/// So a request carries the target that ASKED for it and, for a dedicated worker's own script, the
/// target its response is handed to. `None` is the page. The ids are the ones announced with
/// [`RequestLedger::announce_target`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Reporter {
    pub target: Option<String>,
    pub response_target: Option<String>,
}

/// A worker target the engine started, as a DevTools client is told about it (`Target.TargetInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChildTarget {
    /// 32 uppercase hex, the shape of every Chrome target id.
    pub id: String,
    /// `worker`, `service_worker` or `shared_worker` -- Chrome's `TargetInfo.type` tokens.
    pub kind: String,
    pub url: String,
}

impl Default for FetchRequest {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: default_method(),
            headers: Vec::new(),
            body: None,
            destination: Destination::default(),
            mode: Mode::default(),
            initiator: None,
            user_activated: false,
            wire: None,
            reporter: Reporter::default(),
            redirect: RedirectMode::default(),
            credentials: CredentialsMode::default(),
            page: None,
            accept_navigation: None,
            priority: None,
            cache: None,
            navigation_start: None,
            cookie_cross_site: false,
            third_party_cookies_blocked: false,
            top_level_url: None,
            no_client_hints: false,
            high_entropy_hints: None,
            chrome_build_year: None,
            no_referrer: false,
            opaque_origin: false,
            referrer_policy: None,
            on_head: None,
            cancel: None,
            on_chunk: None,
        }
    }
}

impl FetchRequest {
    /// A TOP-LEVEL NAVIGATION to `url`: destination `document`, mode `navigate`, no initiator (so
    /// `Sec-Fetch-Site: none`), and `Sec-Fetch-User: ?1`.
    ///
    /// The user-activation flag is deliberate. A navigation this engine issues stands in for the one a
    /// person makes from the address bar or a bookmark, and Chrome marks those user-activated; a
    /// navigation that arrives with `Sec-Fetch-Site: none` and NO `Sec-Fetch-User` is the shape of a
    /// programmatic redirect, which is the narrower and more suspicious claim of the two.
    pub fn navigation(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            destination: Destination::from("document"),
            mode: Mode::Navigate,
            initiator: None,
            user_activated: true,
            ..Self::default()
        }
    }

    /// A SUBRESOURCE load of `url` for `destination`/`mode`, initiated by the document at `initiator`.
    pub fn subresource(url: impl Into<String>, destination: &str, mode: Mode, initiator: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            destination: Destination::from(destination),
            mode,
            initiator: Some(initiator.into()),
            ..Self::default()
        }
    }

    /// This request, WORN BY `p`: its identity headers and its wire, both read from that one persona.
    ///
    /// The builder form exists so the common call site is `FetchRequest::navigation(url).worn_by(p)`
    /// — one expression that cannot produce a request with a UA and no ClientHello, which is what
    /// `FetchRequest { headers: persona_headers(p), ..navigation(url) }` did at five call sites.
    pub fn worn_by(mut self, p: &dyn spec::Persona) -> Self {
        apply_persona_headers(p, &mut self);
        self
    }
}

/// Fetch's *request destination*, as the token that goes into `Sec-Fetch-Dest`. Kept as a newtype
/// over `String` rather than an enum because the destination list is open (`fencedframe`, `webidentity`,
/// `speculationrules`, … keep being added) and this type only ever serialises it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Destination(pub String);

impl Destination {
    /// The `Sec-Fetch-Dest` token. The *empty* destination serialises as the literal `empty`.
    pub fn token(&self) -> &str {
        if self.0.is_empty() { "empty" } else { &self.0 }
    }
}

impl From<&str> for Destination {
    fn from(s: &str) -> Self {
        Destination(s.to_string())
    }
}

/// Fetch's *request mode*. `navigate` is reserved for actual navigations; everything a page starts
/// itself is one of the other three.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Cors,
    NoCors,
    SameOrigin,
    Navigate,
    Websocket,
}

impl Mode {
    /// The `Sec-Fetch-Mode` token.
    pub fn token(self) -> &'static str {
        match self {
            Mode::Cors => "cors",
            Mode::NoCors => "no-cors",
            Mode::SameOrigin => "same-origin",
            Mode::Navigate => "navigate",
            Mode::Websocket => "websocket",
        }
    }
}

fn default_method() -> String {
    "GET".to_string()
}

/// Deserialise a request body from either a JS string (its UTF-8 encoding) or a buffer (serde_v8's
/// `deserialize_any` hands an `ArrayBuffer`/view over as `visit_byte_buf`), or null/undefined.
fn string_or_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<Vec<u8>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string, a buffer, or null")
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.as_bytes().to_vec()))
        }
        fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v.into_bytes()))
        }
        fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(Some(v.to_vec()))
        }
        fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2: serde::Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(V)
        }
    }
    d.deserialize_any(V)
}

/// A fetch response handed back to its caller.
///
/// **`bytes` IS THE BODY.** It used to be `body: String`, filled by `resp.text()`, so every body was
/// charset-decoded (U+FFFD for anything that was not valid text) before any caller saw it: a wasm
/// module, a font, a PNG or a protobuf reached the page as a string whose UTF-8 re-encoding was 512
/// bytes for a 256-byte `0..=255` file. Measured on `google-chrome` 150.0.7871.46: `fetch` of that file
/// answers `arrayBuffer().byteLength` 256 with `u[128] === 128`; this engine answered 512 and 239.
///
/// `body` is kept as the TEXT VIEW of the same bytes -- `String::from_utf8_lossy`, exactly what
/// `resp.text()` produced (this crate builds wreq WITHOUT its `charset` feature, so `text()` never read
/// a Content-Type charset and never sniffed a BOM) -- for the callers that consume a script, a
/// stylesheet or a document as text and belong to other lanes (`korvus-cdp`'s navigation,
/// `korvus-runtime`'s module loader / importScripts / subresource loader). Build one with
/// [`FetchResponse::new`], which derives it, so the two can never disagree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    pub status: u16,
    pub status_text: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The text view of [`FetchResponse::bytes`] (see the type's docs). Not the body of record.
    pub body: String,
    /// The body, byte for byte, after content-coding (gzip/br/zstd) is removed.
    #[serde(skip)]
    pub bytes: Vec<u8>,
    /// Fetch's response tainting (`basic` for every request that is not a page's).
    pub response_type: ResponseType,
    /// Did a redirect happen on the way (`Response.redirected`)?
    pub redirected: bool,
    /// The request's credentials mode, which the CORS header filter needs.
    #[serde(skip)]
    pub credentials: CredentialsMode,
    /// When each phase of the request happened, for Resource Timing.
    #[serde(skip)]
    pub timing: spec::FetchTiming,
    /// The address the terminal hop connected to (`ip:port`), for a plugin's `res_remote_ip`; "" for a cache hit
    /// or a fulfilled/non-network response (plugin ABI 3, D1).
    #[serde(skip)]
    pub remote_ip: String,
    /// The HTTP version the terminal hop spoke (`http/1.1`, `h2`, `h3`), for a plugin's `res_http_version`.
    #[serde(skip)]
    pub http_version: String,
}

impl FetchResponse {
    /// A response over `bytes`, with the text view derived from them the way `wreq`'s `text()` did.
    pub fn new(status: u16, status_text: String, url: String, headers: Vec<(String, String)>, bytes: Vec<u8>) -> Self {
        let body = String::from_utf8_lossy(&bytes).into_owned();
        FetchResponse {
            status,
            status_text,
            url,
            headers,
            body,
            bytes,
            response_type: ResponseType::Basic,
            redirected: false,
            credentials: CredentialsMode::default(),
            timing: spec::FetchTiming::default(),
            remote_ip: String::new(),
            http_version: String::new(),
        }
    }
}

/// The ALPN-style token for an HTTP version, as a plugin reads it (`res_http_version`).
fn http_version_token(v: wreq::Version) -> &'static str {
    match v {
        wreq::Version::HTTP_09 => "http/0.9",
        wreq::Version::HTTP_10 => "http/1.0",
        wreq::Version::HTTP_11 => "http/1.1",
        wreq::Version::HTTP_2 => "h2",
        wreq::Version::HTTP_3 => "h3",
        _ => "",
    }
}

/// The phase instants of one request, collected hop by hop and turned into a [`spec::FetchTiming`].
struct PhaseClock {
    asked: std::time::Instant,
    /// The navigation's own start, when the engine gave one (`FetchRequest::navigation_start`).
    navigation_start: Option<std::time::Instant>,
    redirect_start: Option<std::time::Instant>,
    redirect_end: Option<std::time::Instant>,
    /// Redirects followed, and a `Critical-CH` restart, which Chrome counts as one.
    redirect_count: u32,
    critical_ch_restart: Option<std::time::Instant>,
    hop_start: std::time::Instant,
    /// The phases of the connection THIS hop opened (`None`: it rode one already open, or none at all).
    conn: Option<wreq::ConnectTiming>,
    request_start: std::time::Instant,
    response_start: std::time::Instant,
}

impl PhaseClock {
    fn new(asked: std::time::Instant) -> Self {
        PhaseClock {
            asked,
            navigation_start: None,
            redirect_start: None,
            redirect_end: None,
            redirect_count: 0,
            critical_ch_restart: None,
            hop_start: asked,
            conn: None,
            request_start: asked,
            response_start: asked,
        }
    }

    /// The connection the response came over: its phases (`wreq::ConnectTiming`) count for this hop only when the hop
    /// OPENED it -- a request that rode an idle HTTP/1 connection or another request's HTTP/2 session reports none
    /// (`wreq::ConnectionReused`; Chromium's `socket_reused`, `stream_socket_handle.cc:27`, `spdy_session.cc:1513`).
    /// The request went out once it was usable (`requestStart` after the connect: the challenge frame's own
    /// connection, 433 ms in Chrome 150).
    ///
    /// As `URLRequest` turns the network stack's moments into the times it was BLOCKED on each (Chromium's
    /// `ConvertRealLoadTimesToBlockingTimes`, `url_request.cc:82`), a phase that began before the hop did is moved
    /// to the hop's start.
    fn connection(&mut self, timing: Option<wreq::ConnectTiming>, reused: bool) {
        self.conn = None;
        if reused {
            return;
        }
        if let Some(mut t) = timing {
            let floor = self.hop_start;
            let clamp = |at: &mut Option<std::time::Instant>| {
                if let Some(v) = at.as_mut() {
                    *v = (*v).max(floor);
                }
            };
            clamp(&mut t.domain_lookup_start);
            clamp(&mut t.domain_lookup_end);
            clamp(&mut t.connect_start);
            clamp(&mut t.ssl_start);
            t.connect_end = t.connect_end.max(floor);
            self.request_start = self.request_start.max(t.connect_end);
            self.conn = Some(t);
        }
    }

    fn finish(&self, url_list: &[url::Url], status: u16, version: wreq::Version, headers: &[(String, String)], body_size: usize, encoded_size: u64) -> spec::FetchTiming {
        let ms = spec::process_ms;
        let redirected = self.redirect_start.is_some();
        let protocol = match version {
            wreq::Version::HTTP_09 => "http/0.9",
            wreq::Version::HTTP_10 => "http/1.0",
            wreq::Version::HTTP_11 => "http/1.1",
            wreq::Version::HTTP_2 => "h2",
            wreq::Version::HTTP_3 => "h3",
            _ => "",
        };
        let header = |name: &str| headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        spec::FetchTiming {
            url_list: url_list.iter().map(|u| u.to_string()).collect(),
            start: ms(self.navigation_start.unwrap_or(self.asked)),
            redirect_start: self.redirect_start.map(ms).unwrap_or(0.0),
            redirect_end: self.redirect_end.map(ms).unwrap_or(0.0),
            fetch_start: if redirected { ms(self.hop_start) } else { ms(self.asked) },
            request_start: ms(self.request_start),
            response_start: ms(self.response_start),
            response_end: ms(std::time::Instant::now()),
            protocol: protocol.to_string(),
            body_size: body_size as u64,
            encoded_size,
            status,
            // Blink's `HttpContentType()` (`resource_timing_utils.cc:122`): the first value of the joined field (A/B C5).
            content_type: crate::mime::http_content_type(headers).unwrap_or_default(),
            timing_allow_origin: header("timing-allow-origin").iter().flat_map(|v| v.split(',')).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            server_timing: header("server-timing").join(", "),
            redirect_count: self.redirect_count,
            critical_ch_restart: self.critical_ch_restart.map(ms).unwrap_or(0.0),
            domain_lookup_start: self.conn.and_then(|c| c.domain_lookup_start).map(ms).unwrap_or(0.0),
            domain_lookup_end: self.conn.and_then(|c| c.domain_lookup_end).map(ms).unwrap_or(0.0),
            connect_start: self.conn.and_then(|c| c.connect_start).map(ms).unwrap_or(0.0),
            ssl_start: self.conn.and_then(|c| c.ssl_start).map(ms).unwrap_or(0.0),
            connect_end: self.conn.map(|c| ms(c.connect_end)).unwrap_or(0.0),
            cache: String::new(),
        }
    }
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

// ---- Fetch Metadata (`Sec-Fetch-*`) --------------------------------------------------------------
//
// These headers are a property of the REQUEST, not of the worn identity, which is why they are computed
// here and not in the persona plugin. Every one of them used to come from `wreq-util`'s static
// Chrome-137 preset, so a `<script src>` subresource and a page `fetch()` both announced
// `dest: document, mode: navigate, site: none` — a combination no browser ever sends for a subresource,
// and a first-tier tell for anything that reads the request instead of the JS surface.

/// Is `u` a *potentially trustworthy* URL (W3C secure-contexts §3.1)? Fetch Metadata is only ever
/// attached to a request whose URL is one — which is why WPT's plain-`http://web-platform.test:8000`
/// tests assert every `Sec-Fetch-*` header is ABSENT, and why sending them unconditionally is both
/// wrong and detectable.
///
/// Only the cases this engine can actually issue a request for are decided here: `https`/`wss` are
/// trustworthy by scheme, a loopback host is trustworthy by address, `file` is trustworthy by scheme.
/// Everything else (notably plain `http` to a named host) is not.
fn is_potentially_trustworthy(u: &url::Url) -> bool {
    match u.scheme() {
        "https" | "wss" | "file" => return true,
        "http" | "ws" => {}
        _ => return false,
    }
    match u.host() {
        // 127.0.0.0/8 and ::1 are loopback; nothing else is.
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

// ---- Mixed content ---------------------------------------------------------
//
// An `http:` subresource on an `https:` page. Measured on stock `google-chrome` 150.0.7871.46 through a
// loopback proxy that answers `https://mixed.test/page.html` (the replay CA, no insecure flag):
//
// ```text
// <img src=http://img.test/a.png>            requested as https://img.test/a.png     (upgraded)
// <img src=http://img.test:8080/port.png>    requested as https://img.test:8080/...  (port kept)
// new Image().src = http://...               upgraded
// <audio src=http://...> / <video src=...>   upgraded
// <script src=http://...>                    never sent; Network.loadingFailed blockedReason "mixed-content"
// <link rel=stylesheet href=http://...>      never sent; blocked
// <iframe src=http://...>                    never sent
// fetch / fetch no-cors / XHR  http://...    never sent; TypeError: Failed to fetch / XHR error
// fetch https://mixed.test/redir -> 302 http blocked at the redirect hop
// <img srcset="http://... 1x">               never sent (blockable, NOT upgraded)
// the same page served from http://127.0.0.1  everything sent as http: a loopback page is not
//                                             an https page, whatever `isSecureContext` says
// ```
//
// So the decision needs the INITIATOR's scheme (https/wss -- not "potentially trustworthy", which would
// include loopback), the target (a potentially trustworthy target, e.g. `http://127.0.0.1`, is never
// mixed) and the destination (image/audio/video are upgradeable; everything else is blockable). A
// document navigation is not mixed content. It lives here because this is the one entry every request
// takes, the same reason the ledger does.
//
// NOT DISTINGUISHED: an `<img srcset>` candidate is blockable in Chrome and upgraded here. The
// transport is told the destination, not which attribute chose the URL; the ruling is in P10e.1.

/// What mixed-content checking does to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixedContent {
    /// Not mixed content: send it as it is.
    Allowed,
    /// Upgradeable content: send it to this `https:` URL instead.
    Upgrade(url::Url),
    /// Blockable content: never sent.
    Blocked,
}

/// The mixed-content decision for a request to `target` for `dest`/`mode`, initiated by `initiator`.
pub fn mixed_content(initiator: Option<&url::Url>, target: &url::Url, dest: &Destination, mode: Mode) -> MixedContent {
    let Some(init) = initiator else { return MixedContent::Allowed };
    if !matches!(init.scheme(), "https" | "wss") {
        return MixedContent::Allowed;
    }
    // A top-level document is navigated, not embedded.
    if dest.0 == "document" && mode == Mode::Navigate {
        return MixedContent::Allowed;
    }
    if !matches!(target.scheme(), "http" | "ws") || is_potentially_trustworthy(target) {
        return MixedContent::Allowed;
    }
    if matches!(dest.0.as_str(), "image" | "audio" | "video") && target.scheme() == "http" {
        let mut up = target.clone();
        if up.set_scheme("https").is_ok() {
            return MixedContent::Upgrade(up);
        }
    }
    MixedContent::Blocked
}

/// The error a blocked request fails with. The ledger records the reason separately (see
/// [`RequestRecord::blocked_reason`]), which is what `Network.loadingFailed` reports.
pub const MIXED_CONTENT_BLOCKED: &str = "mixed-content";

/// The `Network.loadingFailed` `blockedReason` for a request a `--block` rule drops: `inspector`, the reason Chrome
/// reports for a URL dropped by DevTools' `Network.setBlockedURLs`. A client-configured blocklist is that same kind
/// of drop, so a page sees exactly what it sees when DevTools blocks a URL.
pub const RULE_BLOCKED: &str = "inspector";

/// `korvus serve --block=…` (and a one-shot run's `KORVUS_BLOCK`): the request kinds and hosts this process drops
/// before the network, to spend no bandwidth or memory on what a scrape or a mint does not need. A rule is either a
/// Fetch destination (`image`, `font`, `media`, `script`, `style`, `stylesheet`, `xhr`, `fetch`, …) or `host:<substr>`
/// (dropped when the request URL's host contains `<substr>`). A top-level navigation (`document`) is never dropped by
/// a kind rule, so the page itself always loads; a `host:` rule still applies to it. Read ONCE from `KORVUS_BLOCK`.
struct BlockRules {
    kinds: Vec<String>,
    hosts: Vec<String>,
}

impl BlockRules {
    fn parse(spec: &str) -> Option<Self> {
        let mut kinds = Vec::new();
        let mut hosts = Vec::new();
        for rule in spec.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
            match rule.strip_prefix("host:") {
                Some(h) => hosts.push(h.to_ascii_lowercase()),
                None => kinds.push(rule.to_ascii_lowercase()),
            }
        }
        (!kinds.is_empty() || !hosts.is_empty()).then_some(BlockRules { kinds, hosts })
    }

    /// Whether this request is dropped: its destination matches a kind rule (never for a top-level navigation), or its
    /// URL host contains a `host:` rule.
    fn blocks(&self, url: &str, destination: &Destination, top_navigation: bool) -> bool {
        if !top_navigation {
            let dest = destination.token();
            let hit = self.kinds.iter().any(|k| match k.as_str() {
                "media" => matches!(dest, "audio" | "video" | "track"),
                "stylesheet" | "css" => dest == "style",
                "xhr" | "fetch" | "empty" => dest.is_empty() || dest == "empty",
                other => other == dest,
            });
            if hit {
                return true;
            }
        }
        if !self.hosts.is_empty() {
            if let Some(host) = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) {
                if self.hosts.iter().any(|h| host.contains(h.as_str())) {
                    return true;
                }
            }
        }
        false
    }
}

/// The process's block rules, read once from `KORVUS_BLOCK`. `None` when unset (the common case) -- one atomic load
/// per request and nothing more.
fn block_rules() -> Option<&'static BlockRules> {
    static RULES: std::sync::OnceLock<Option<BlockRules>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| std::env::var("KORVUS_BLOCK").ok().as_deref().and_then(BlockRules::parse)).as_ref()
}

#[cfg(test)]
mod block_rules_tests {
    use super::{BlockRules, Destination};

    #[test]
    fn a_kind_rule_drops_that_destination_but_never_the_top_navigation() {
        let r = BlockRules::parse("image,font,media").unwrap();
        let img = Destination::from("image");
        assert!(r.blocks("https://site/x.png", &img, false), "an image subresource is dropped");
        assert!(!r.blocks("https://site/", &img, true), "a top-level navigation is never dropped by a kind rule");
        assert!(r.blocks("https://site/clip.mp4", &Destination::from("video"), false), "media covers audio/video/track");
        assert!(!r.blocks("https://site/app.js", &Destination::from("script"), false), "a kind not listed is kept");
    }

    #[test]
    fn aliases_and_host_rules() {
        let r = BlockRules::parse("stylesheet, host:ads.example , xhr").unwrap();
        assert!(r.blocks("https://site/a.css", &Destination::from("style"), false), "stylesheet aliases style");
        assert!(r.blocks("https://ads.example/t.gif", &Destination::from("image"), false), "a host rule drops any kind");
        assert!(r.blocks("https://ads.example/", &Destination::from("document"), true), "a host rule drops even the navigation");
        assert!(r.blocks("https://site/api", &Destination::default(), false), "xhr/fetch is the empty destination");
        assert!(!r.blocks("https://other/x.css", &Destination::from("image"), false), "no rule matches");
    }

    #[test]
    fn an_empty_or_whitespace_spec_is_no_rules() {
        assert!(BlockRules::parse("").is_none());
        assert!(BlockRules::parse("  , ; ").is_none());
    }
}

#[cfg(test)]
mod cache_budget_tests {
    #[test]
    fn the_budget_lowers_but_never_rises_above_chromes() {
        // `KORVUS_HTTP_CACHE_MAX` is read inside `http_cache::budget`; exercise the arithmetic it uses.
        let clamp = |mb: usize| (mb * 1024 * 1024).min(50 * 1024 * 1024);
        assert_eq!(clamp(10), 10 * 1024 * 1024, "N MB is honoured when below Chrome's 50");
        assert_eq!(clamp(0), 0, "0 turns the cache off");
        assert_eq!(clamp(500), 50 * 1024 * 1024, "a request above Chrome's budget is capped at it");
    }
}

// ---- HTTPS-Upgrades ------------------------------------------------------------------
//
// A top-level `http:` navigation is tried over `https:` first, and falls back to `http:` if that
// does not work. This is NOT HSTS and the difference matters, because the two have different
// observable consequences: HSTS is stateful (a header the site sent, or a shipped preload list),
// per-host, and NEVER falls back; HTTPS-Upgrades is stateless, applies to hosts with no HSTS at all,
// and falls back.
//
// WHICH ONE CHROME IS USING HERE IS MEASURED, not inferred. Chrome names its own internal redirects
// in the `Non-Authoritative-Reason` header of the 307 it reports to `Network.requestWillBeSent`, and
// on stock `google-chrome` 150.0.7871.46, fresh profile, it says:
//
// ```text
// http://home.unicode.org/   -> 307 Non-Authoritative-Reason: HttpsUpgrades -> https://home.unicode.org/
// http://jwpconnatix.com/    -> 307 Non-Authoritative-Reason: HttpsUpgrades -> https://jwpconnatix.com/
// ```
//
// Not `HSTS`. And the recordings agree that no `http:` request for either host was ever put on the
// wire — which is what an internal redirect looks like from the outside, and is the reason korvus,
// which asked for `http://`, got an unrecorded-request failure on both rows.
//
// THE SHAPE OF THE RULE, measured offline against a logging proxy that answers every plain `http:`
// GET and refuses (or hangs on) every `CONNECT`, so the upgrade attempt and the fallback are both
// visible as wire events:
//
// ```text
// start url                             CONNECT :443 ?   fallback GET http: ?
// http://c4probe-xyz.com/                    yes              yes  (+0.000 s, CONNECT refused)
// http://c4probe-xyz.com:80/                 yes              yes  (:80 is the default port)
// http://sub.deep.c4probe-xyz.co.uk/         yes              yes  (multi-label public suffix)
// http://8.8.8.8/                            yes              yes  (a PUBLIC ip literal is upgraded)
// http://c4probe-slow.com/  (CONNECT hangs)  yes              yes  at +3.000 s -- a fallback TIMER
// http://c4probe-xyz.com:8080/               NO               --   (non-default port)
// http://c4probe.test/                       NO               --   (no registry-known suffix)
// http://c4probe.example/                    NO               --   (no registry-known suffix)
// http://c4probe/                            NO               --   (single label)
// http://192.168.77.250/                     NO               --   (private ip)
// http://192.0.2.77/                         NO               --   (reserved ip, TEST-NET-1)
// ```
//
// So the host condition is Chromium's *non-unique hostname* test — a name a public CA could never
// be issued for is never tried over TLS — and the port condition is "the default one".
//
// AND THE FALLBACK IS THE HALF THAT MATTERS MORE THAN THE UPGRADE. An upgrade with no fallback
// breaks every plain-http site there is. Measured live on `www.slackware.com`, which serves over
// `http:` and 301s `https:` straight back:
//
// ```text
// http://www.slackware.com/   -> 307 HttpsUpgrades -> https://www.slackware.com/
// https://www.slackware.com/  -> 307 HttpsUpgrades -> http://www.slackware.com/    and it loads
// ```
//
// Chrome reports the RETURN hop as an `HttpsUpgrades` redirect too, and does not re-upgrade. Here
// that comes out of the loop guard rather than a second mechanism: an upgrade target already in the
// request's url list is declined, so the 301 back to `http:` is simply followed. Same wire.
//
// WHAT THE UPGRADED REQUEST LOOKS LIKE, read through a proxy that tunnels `CONNECT` to a local TLS
// origin (Chrome under `--ignore-certificate-errors`, a property of the measuring rig and of nothing
// else): `Sec-Fetch-Site: none`, `Sec-Fetch-Mode: navigate`, `Sec-Fetch-Dest: document`,
// `Upgrade-Insecure-Requests: 1`. `none`, not `cross-site` — so the internal redirect is NOT a step
// in the chain that `Sec-Fetch-Site` is computed over, which is why the upgrade REPLACES the url
// list's last entry here instead of pushing a hop onto it.

/// How long the upgraded `https:` attempt gets before the navigation falls back to `http:`.
/// Measured at 3.000 s (CONNECT hung at +0.089 s, fallback GET at +3.089 s).
const HTTPS_UPGRADE_FALLBACK: std::time::Duration = std::time::Duration::from_secs(3);

/// Is `host` *unique* — a name or address a public certificate authority could be issued for, and so
/// one it is worth trying over TLS? Chromium's `net::IsHostnameNonUnique`, which is what gates the
/// upgrade (and several other platform features) on the same question.
///
/// A domain is unique when it has a registry-known public suffix AND at least one label of its own
/// under it; `foo.test`, `foo.example` and a single-label intranet name have no known suffix, and a
/// bare suffix (`com`) has no label of its own. An address is unique when it is publicly routable:
/// the IANA special-purpose ranges (RFC 6890) are not.
fn is_unique_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            psl::domain(d.as_bytes()).is_some_and(|dom| dom.suffix().is_known())
        }
        url::Host::Ipv4(ip) => is_publicly_routable_v4(*ip),
        url::Host::Ipv6(ip) => is_publicly_routable_v6(*ip),
    }
}

/// The IANA IPv4 Special-Purpose Address Registry (RFC 6890 and its successors): everything here is
/// **not** publicly routable, so it is not a host a public CA could vouch for.
fn is_publicly_routable_v4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    let n = u32::from(ip);
    let net = |a: u8, b: u8, c: u8, d: u8, bits: u32| -> bool {
        let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
        (n & mask) == (u32::from(std::net::Ipv4Addr::new(a, b, c, d)) & mask)
    };
    !(net(0, 0, 0, 0, 8)              // "this network"
        || net(10, 0, 0, 0, 8)        // private
        || net(100, 64, 0, 0, 10)     // carrier-grade NAT
        || net(127, 0, 0, 0, 8)       // loopback
        || net(169, 254, 0, 0, 16)    // link-local
        || net(172, 16, 0, 0, 12)     // private
        || net(192, 0, 0, 0, 24)      // IETF protocol assignments
        || net(192, 0, 2, 0, 24)      // TEST-NET-1
        || net(192, 88, 99, 0, 24)    // 6to4 relay anycast (deprecated)
        || net(192, 168, 0, 0, 16)    // private
        || net(198, 18, 0, 0, 15)     // benchmarking
        || net(198, 51, 100, 0, 24)   // TEST-NET-2
        || net(203, 0, 113, 0, 24)    // TEST-NET-3
        || net(224, 0, 0, 0, 4)       // multicast
        || net(240, 0, 0, 0, 4)       // reserved, and 255.255.255.255 with it
        || o == [255, 255, 255, 255])
}

/// The same question for IPv6: loopback, unspecified, unique-local (`fc00::/7`), link-local
/// (`fe80::/10`), the documentation prefix (`2001:db8::/32`) and multicast are not publicly routable.
/// An IPv4-mapped address is decided by its IPv4 half.
fn is_publicly_routable_v6(ip: std::net::Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_publicly_routable_v4(v4);
    }
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] == 0x2001 && s[1] == 0x0db8))
}

/// The `https:` URL a top-level `http:` navigation is optimistically tried at, or `None` when the
/// rule does not apply to this request. See the measured table above for every condition.
pub fn https_upgrade(target: &url::Url, dest: &Destination, mode: Mode) -> Option<url::Url> {
    if mode != Mode::Navigate || dest.0 != "document" {
        return None;
    }
    if target.scheme() != "http" {
        return None;
    }
    // `url` already normalises an explicit `:80` away, so `port()` is `None` for the default port.
    if target.port().is_some() {
        return None;
    }
    if !is_unique_host(&target.host()?) {
        return None;
    }
    let mut up = target.clone();
    up.set_scheme("https").ok()?;
    Some(up)
}

/// What a request decides BEFORE its fetch starts -- Chrome's `NavigationRequest::WillStartRequest` throttles, which
/// run before `OnStartChecksComplete` marks `fetch_start`: the first hop's HTTPS-Upgrades URL. Made in
/// [`fetch`] before it takes `asked` (the request's fetchStart), and handed to the net runtime with the request.
struct StartChecks {
    /// [`https_upgrade`] of the request's URL: the first hop's.
    first_upgrade: Option<url::Url>,
    /// When the checks were done: never after `asked` (asserted on the net runtime in a debug build).
    at: std::time::Instant,
}

fn start_checks(req: &FetchRequest) -> StartChecks {
    let first_upgrade = url::Url::parse(&req.url).ok().and_then(|u| https_upgrade(&u, &req.destination, req.mode));
    StartChecks { first_upgrade, at: std::time::Instant::now() }
}

/// The *registrable domain* of `u`'s host (`www.example.co.uk` → `example.co.uk`), or `None` when the
/// host is an IP literal or has no registrable domain of its own (a bare public suffix). Backed by the
/// compiled-in Public Suffix List so `foo.co.uk` and `bar.co.uk` come out CROSS-site, which a
/// last-two-labels approximation would get wrong.
fn registrable_domain(u: &url::Url) -> Option<String> {
    let host = match u.host() {
        Some(url::Host::Domain(d)) => d.trim_end_matches('.').to_ascii_lowercase(),
        _ => return None,
    };
    let d = psl::domain_str(host.as_str())?;
    Some(d.to_string())
}

/// A URL's SITE (`net::SchemefulSite`), serialized `scheme://registrable-domain`: the host itself when it has
/// no registrable domain (an IP literal, `localhost`); `None` for a URL with no host (`about:blank`,
/// `data:`, a `blob:` URL -- whose document's site is its creator's, which only the caller knows).
pub fn schemeful_site(u: &url::Url) -> Option<String> {
    let host = registrable_domain(u).or_else(|| u.host_str().map(|h| h.to_ascii_lowercase()))?;
    Some(format!("{}://{host}", u.scheme()))
}

/// Are `a` and `b` *same site* (HTML §"same site": schemelessly same site AND same scheme)? Two hosts
/// that are IP literals are same site only when identical.
pub fn same_site(a: &url::Url, b: &url::Url) -> bool {
    if a.scheme() != b.scheme() {
        return false;
    }
    match (registrable_domain(a), registrable_domain(b)) {
        (Some(x), Some(y)) => x == y,
        // No registrable domain on either side (IP literal, `localhost`, a bare public suffix): the
        // host itself is the site.
        _ => a.host_str().map(|h| h.to_ascii_lowercase()) == b.host_str().map(|h| h.to_ascii_lowercase()),
    }
}

fn same_origin(a: &url::Url, b: &url::Url) -> bool {
    a.origin() == b.origin()
}

/// `Sec-Fetch-Site` for a request initiated by `initiator` (`None` = the user agent itself — a typed
/// URL, a bookmark, the first navigation of a session) whose URL LIST is `url_list`: the initial URL
/// followed by every redirect hop, in order.
///
/// The url-list walk is the whole point: Chrome computes this over the redirect chain, not over the
/// final hop, so an `http → https` redirect on the same host reports `cross-site` (the scheme changed
/// mid-chain) even though the last hop looks same-origin-ish. Mirrors Fetch Metadata §"set the
/// Sec-Fetch-Site header".
fn sec_fetch_site(initiator: Option<&url::Url>, url_list: &[url::Url]) -> &'static str {
    let Some(origin) = initiator else { return "none" };
    let mut site = "same-origin";
    for u in url_list {
        if same_origin(origin, u) {
            continue;
        }
        site = "cross-site";
        if !same_site(origin, u) {
            break;
        }
        site = "same-site";
    }
    site
}

/// The navigation `Accept` when the worn persona names none: the Fetch Standard's own default for a
/// document request, which identifies no browser. Every shipped bundle names its browser's value
/// (`nav.acceptNavigation`) -- Chromium's, with `application/signed-exchange`, was a constant here, a
/// browser-identifying literal compiled into the engine, which `scripts/freeze.sh`'s persona-literal scan
/// refuses; it is in the Chrome, Edge and Opera bundles beside Brave's measured one.
/// A subresource does NOT get the navigation `Accept`; sending the document one for a script was part of
/// the same preset tell.
const FETCH_NAVIGATION_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

fn is_navigation_like(dest: &Destination, mode: Mode) -> bool {
    mode == Mode::Navigate || matches!(dest.0.as_str(), "document" | "iframe" | "frame" | "object" | "embed")
}

fn accept_for(dest: &Destination, mode: Mode, persona_navigation: Option<&str>) -> String {
    if is_navigation_like(dest, mode) {
        return persona_navigation.unwrap_or(FETCH_NAVIGATION_ACCEPT).to_string();
    }
    accept_for_subresource(dest).to_string()
}

/// A subresource's `Accept`, Blink's `network_utils::GetAcceptHeaderForDestination`
/// (`third_party/blink/common/loader/network_utils.cc:74-91`; the image one is `ImageAcceptHeader()` as Chrome 150 is
/// built, AVIF on and JPEG XL off).
fn accept_for_subresource(dest: &Destination) -> &'static str {
    match dest.0.as_str() {
        "style" | "xslt" => "text/css,*/*;q=0.1",
        "image" => "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        "webbundle" => "application/webbundle;v=b2",
        "json" => "application/json,*/*;q=0.5",
        _ => "*/*",
    }
}

/// Does `dest`'s `Accept` REPLACE one the request already carries? Blink's `network_utils::SetAcceptHeader`
/// (`network_utils.cc:57-72`) `SetHeader`s it for a stylesheet, an XSLT, a web bundle and a JSON module, and for every
/// other destination calls `SetHeaderIfMissing` -- "because JS can manually set an accept header on an XHR".
fn accept_overwrites(dest: &Destination) -> bool {
    matches!(dest.0.as_str(), "style" | "xslt" | "webbundle" | "json")
}

/// A request's header list at one hop as Chromium's network stack holds it (`net::HttpRequestHeaders`): ONE entry per
/// name, compared case-insensitively. The caller's headers go in first -- Blink hands the network service the page's
/// own (and its `Content-Type`, client hints and `User-Agent`) -- and every header the stack adds after them follows
/// the rule its source states: [`set`](Self::set) (`SetHeader`: the entry of that name, if any, keeps its place and
/// its spelling and takes the new value) or [`set_if_missing`](Self::set_if_missing) (`SetHeaderIfMissing`).
///
/// Measured 2026-10-11 (ShieldSquare / Radware): korvus put its own `Accept: */*` on every request and then APPENDED the
/// caller's headers, so a page's `Accept` -- jQuery's `application/json, text/javascript, */*; q=0.01`, a `fetch()`'s
/// `{Accept: 'application/json'}` -- went out as a SECOND `Accept` line beside korvus's. ShieldSquare answered every
/// such XHR with its captcha: the extra line alone, taken out of korvus's request, passed 3/3 on two sites, and put
/// into Chrome's request failed 3/3.
#[derive(Debug, Default)]
struct HopHeaders(Vec<(String, String)>);

impl HopHeaders {
    fn has(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.0.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
            Some(entry) => entry.1 = value,
            None => self.0.push((name.to_string(), value)),
        }
    }

    fn set_if_missing(&mut self, name: &str, value: impl Into<String>) {
        if !self.has(name) {
            self.0.push((name.to_string(), value.into()));
        }
    }

    /// A caller's header: a name it already gave is combined with `", "`, as Blink's `HTTPHeaderMap` combines a
    /// repeated name (`ResourceRequestHead::AddHttpHeaderField`) -- one line, never two.
    fn combine(&mut self, name: &str, value: &str) {
        match self.0.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
            Some(entry) => {
                entry.1.push_str(", ");
                entry.1.push_str(value);
            }
            None => self.0.push((name.to_string(), value.to_string())),
        }
    }
}

/// `Accept-Encoding` as Chromium's network stack writes it on a request that carries none
/// (`net::HttpRequestHeaders::SetAcceptEncodingIfMissing`, `net/http/http_request_headers.cc:232-280`, called from
/// `URLRequestHttpJob::AddExtraHeaders`): `identity` when the request asks for a `Range`; otherwise the codings the
/// browser decodes -- the persona's preset (`preset`) -- with `br` and `zstd` advertised only to a URL that is
/// cryptographic or localhost ("only if transferred data is opaque to proxy"). So Chrome 150 sends `gzip, deflate` to
/// a plain-http site and `gzip, deflate, br, zstd` to https and to `http://127.0.0.1`; korvus sent all four
/// everywhere, and its client default to a page's `Range` request.
fn advertised_encodings(preset: &str, url: &url::Url, has_range: bool) -> String {
    if has_range {
        return "identity".to_string();
    }
    if is_potentially_trustworthy(url) {
        return preset.to_string();
    }
    preset
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("br") && !c.eq_ignore_ascii_case("zstd"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each wire's preset `Accept-Encoding` -- the codings its browser advertises -- recorded when [`client_for`] builds
/// that wire's client, for [`accept_encoding_for`].
static PRESET_ACCEPT_ENCODING: OnceLock<Mutex<HashMap<spec::TlsProfile, String>>> = OnceLock::new();

/// [`advertised_encodings`] for a request on `wire` to `url`; `None` when the preset advertises nothing (the client
/// then sends what it always did).
fn accept_encoding_for(wire: &spec::TlsProfile, url: &url::Url, has_range: bool) -> Option<String> {
    let preset = PRESET_ACCEPT_ENCODING.get()?.lock().ok()?.get(wire).cloned()?;
    let v = advertised_encodings(&preset, url, has_range);
    (!v.is_empty()).then_some(v)
}

/// The request-header field Chromium writes on HTTP/2 and HTTP/3 only: the RFC 9218 `priority`
/// (`net/spdy/spdy_http_utils.cc:221-235`, `CreateSpdyHeadersFromHttpRequest` -- appended after every other header,
/// and only when the request carries none). A request on an HTTP/1.1 connection never has it from the network stack;
/// a page's own `priority` header is an ordinary header and goes out on either. korvus sent `Priority: u=0, i` /
/// `u=1, i` on every https request it made over HTTP/1.1 (TLS echo against Chrome 150). The
/// protocol is ALPN's choice, made after the request is built, so the request names the field and the client drops
/// it on an HTTP/1 connection (`wreq::Http2OnlyHeaders`, a patch of the vendored `wreq`).
fn h2_only_priority() -> wreq::Http2OnlyHeaders {
    wreq::Http2OnlyHeaders(vec![wreq::header::HeaderName::from_static("priority")])
}

/// THE HEADERS FRAME'S PRIORITY OVER HTTP/2: Chromium's, from the
/// request's own priority. Every stream used to carry the connection's one fixed dependency -- weight byte 255, the
/// root -- where Chrome 150 gives a `u=1` favicon 219 and an image 146, each depending on the newest open stream of
/// the same or a higher priority (the vendored `http2`'s `ChromiumPriority`). The request's SPDY priority is the
/// urgency of the `priority` field it carries (`ConvertRequestPriorityToSpdyPriority` and `...ToQuicPriority` are
/// one map); a request with no field is urgency 3, which Chromium writes no field for. An HTTP/1 connection ignores it.
fn chromium_h2_priority(request: &wreq::Request) -> wreq::http2::ChromiumPriority {
    let field = request.headers().get("priority").and_then(|v| v.to_str().ok());
    let urgency = field.and_then(|f| f.split(',').map(str::trim).find_map(|p| p.strip_prefix("u=")).and_then(|u| u.parse::<u8>().ok()));
    wreq::http2::ChromiumPriority(urgency.unwrap_or(3).min(7))
}

/// `Sec-GPC`'s value when the worn identity claims Global Privacy Control.
///
/// The JS surface answers `navigator.globalPrivacyControl === true`, and a browser that claims GPC in
/// JS while sending no `Sec-GPC` on the wire is contradicting itself in the one place a server can
/// check both halves. So the header is CONDITIONAL on the same persona field the JS member reads
/// ([`spec::NavigatorProfile::global_privacy_control`]) — one place, both halves.
///
/// It used to be pushed unconditionally. Measured 2026-09-18 on a loopback socket, one navigation
/// plus nineteen subresources each: real Brave 151.1.93.138 sends `Sec-GPC: 1`; real Chrome 150,
/// Edge 151 and Opera 135 send the header on NOTHING. Under the Chrome, Edge and Opera bundles this
/// engine was sending a header those browsers never send, on every request, while its own
/// `navigator` said the member did not exist — the cheapest kind of contradiction to read.
pub const GPC_HEADER_VALUE: &str = "1";

/// The header names the persona OWNS. A caller may not set these: whatever identity this process
/// wears decides them, so [`apply_persona_headers`] strips any that arrived and re-states the
/// persona's.
///
/// NOT `accept-language`, deliberately: it is a CORS-safelisted request header, so a page really may
/// set it on a `fetch()` and Chrome really does send the page's value. The persona states it only
/// when the caller has not — see [`apply_persona_identity`].
const PERSONA_OWNED_HEADERS: &[&str] = &[
    "user-agent",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "sec-gpc",
    // the high-entropy hints, which the transport adds where an origin's `Accept-CH` asked for them
    "sec-ch-ua-full-version",
    "sec-ch-ua-arch",
    "sec-ch-ua-platform-version",
    "sec-ch-ua-model",
    "sec-ch-ua-bitness",
    "sec-ch-ua-wow64",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-form-factors",
];

/// **The** builder of the identity headers — UA + low-entropy Client Hints + `Sec-GPC` — so the HTTP
/// `User-Agent` / `sec-ch-ua` cannot differ from `navigator.userAgent` / `navigator.userAgentData`.
///
/// There used to be three of these (`korvus`'s CLI fetch, the runtime's `op_fetch`, the runtime's
/// subresource loader), each a hand copy of the same seven pushes. Three copies of one fact is three
/// chances for the wire identity of a page fetch, a `fetch()` and a `<script src>` to disagree — and a
/// server that sees two of those requests can read the disagreement. One builder, no drift.
///
/// wreq's emulation sets Chrome-137 defaults for the TLS/H2 fingerprint (shared with 151), but its
/// default `user-agent` (137) would then contradict the JS UA (151) — a header-vs-JS coherence tell a
/// bot/human gate checks. `fetch` applies `req.headers` over wreq's defaults, so the persona's wins.
///
/// NOT here: `Accept`, `Referer` and the `Sec-Fetch-*` quartet. Those are properties of the REQUEST
/// (its destination, mode and initiator), not of the identity, and this crate derives them per
/// request. Emitting them alongside the identity is how a `<script src>` once inherited a
/// navigation's header set.
pub fn persona_headers(p: &dyn spec::Persona) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = Vec::new();
    let nav = p.navigator();
    let ua = nav.user_agent;
    if !ua.is_empty() {
        h.push(("user-agent".into(), ua));
    }
    let ch = p.client_hints();
    if !ch.brands.is_empty() {
        let sec = ch.brands.iter().map(|b| format!("\"{}\";v=\"{}\"", b.brand, b.version)).collect::<Vec<_>>().join(", ");
        h.push(("sec-ch-ua".into(), sec));
        h.push(("sec-ch-ua-mobile".into(), if ch.mobile { "?1".into() } else { "?0".into() }));
        if !ch.platform.is_empty() {
            h.push(("sec-ch-ua-platform".into(), format!("\"{}\"", ch.platform)));
        }
    }
    if nav.global_privacy_control == Some(true) {
        h.push(("sec-gpc".into(), GPC_HEADER_VALUE.into()));
    }
    // The wire half of `navigator.languages`, verbatim from the capture. Absent rather than guessed:
    // the emulation preset's own `en-US,en;q=0.9` is what this replaces, and it contradicted the JS
    // answer in the same page load (FP0 §L1). `client_for` removes the preset's copy so this is the
    // only thing that can put the header on the wire.
    if !nav.accept_language.is_empty() {
        h.push(("accept-language".into(), nav.accept_language));
    }
    h
}

/// Everything a request wears that comes from the persona that made it: the identity HEADERS and the
/// WIRE they must travel on.
///
/// **ONE VALUE, BECAUSE THEY ARE ONE CLAIM.** `user-agent: …Chrome/151…` and an Edge ClientHello are
/// not two facts about a request that happen to be nearby; they are one device, and a server that
/// reads both sees the contradiction immediately. The two used to be carried separately — headers on
/// the request, wire in a process `OnceLock` — and every call site that built a request by hand took
/// the headers and could not have taken the wire, because there was nothing to take. Handing them
/// over together is what makes "a request goes out on its own persona's wire" a compile-time shape
/// rather than a rule somebody has to remember.
#[derive(Debug, Clone, Default)]
pub struct PersonaIdentity {
    /// The persona's navigation `Accept`, when its browser is not Chromium's default.
    pub accept_navigation: Option<String>,
    /// UA + low-entropy Client Hints + `Sec-GPC` — [`persona_headers`]'s output.
    pub headers: Vec<(String, String)>,
    /// The TLS/H2 preset and claimed OS — [`spec::Persona::transport_profile`]'s output.
    pub wire: spec::TlsProfile,
    /// The browser blocks third-party cookies ([`spec::NavigatorProfile::third_party_cookies_blocked`]).
    pub third_party_cookies_blocked: bool,
    /// The high-entropy User-Agent hints' values ([`client_hints::high_entropy_values`]), for the requests
    /// an origin's `Accept-CH` asks them on.
    pub high_entropy_hints: std::sync::Arc<Vec<(String, String)>>,
    /// [`chrome_build_year`]: the persona is Google Chrome, built that year.
    pub chrome_build_year: Option<u16>,
}

/// Read a persona's whole request identity out of it, once, so the two halves cannot be read from
/// two different personas.
pub fn persona_identity(p: &dyn spec::Persona) -> PersonaIdentity {
    let nav = p.navigator();
    PersonaIdentity {
        headers: persona_headers(p),
        wire: p.transport_profile(),
        accept_navigation: nav.accept_navigation,
        third_party_cookies_blocked: nav.third_party_cookies_blocked,
        high_entropy_hints: std::sync::Arc::new(client_hints::high_entropy_values(&p.client_hints())),
        chrome_build_year: chrome_build_year(&p.client_hints()),
    }
}

/// The year a Google Chrome persona's build carries, from its major version; `None` when the persona is not
/// Google Chrome (its brands name no `Google Chrome`: Chromium, Edge, Brave, Opera send none of these headers).
/// Chrome ships a major every four weeks, and the year is its build's: Chrome 150.0.7871.46 sends `2026`
/// (measured), 144 was the year's first stable, so 144 to 156 are 2026 and every 13 majors a year more or less.
pub fn chrome_build_year(ch: &spec::ClientHints) -> Option<u16> {
    let chrome = ch.brands.iter().find(|b| b.brand == "Google Chrome")?;
    let major: i32 = chrome.version.split('.').next()?.trim().parse().ok()?;
    Some((2026 + (major - 144).div_euclid(13)).clamp(2020, 2100) as u16)
}

/// Is `host` one Chrome sends its Google-only request headers to (Chromium's
/// `google_util::IsGoogleAssociatedDomainUrl`): a `google.<TLD>` or `youtube.<TLD>` host, its subdomains
/// included, or one under the fixed suffixes Google serves from -- `.googleapis.com`, `.gstatic.com`,
/// `.doubleclick.net` and the rest. `google.com`, `www.google.co.kr`, `play.google.com`, `www.gstatic.com`: yes;
/// `google.example.com`, `notgoogle.com`: no.
pub fn google_associated_host(host: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        ".android.com", ".doubleclick.com", ".doubleclick.net", ".ggpht.com", ".googleadservices.com",
        ".googleapis.com", ".googlesyndication.com", ".googleusercontent.com", ".googlevideo.com", ".gstatic.com",
        ".litepages.googlezip.net", ".youtubekids.com", ".ytimg.com",
    ];
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if SUFFIXES.iter().any(|s| host.ends_with(s)) || host == "googleweblight.com" {
        return true;
    }
    // `<sub>.google.<tld>` / `youtube.<tld>`: the label before a one- or two-label country or generic TLD
    // (`com`, `co.kr`, `com.au`, `de`) -- GOOGLE_TLD_LIST's shape: a short last label, and a short one before it
    let labels: Vec<&str> = host.split('.').collect();
    let named = |i: usize| matches!(labels.get(i), Some(&"google") | Some(&"youtube"));
    let tld = |part: &[&str]| !part.is_empty() && part.iter().all(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()));
    let n = labels.len();
    (n >= 2 && named(n - 2) && tld(&labels[n - 1..])) || (n >= 3 && named(n - 3) && tld(&labels[n - 2..]))
}

/// A persona's identity applied ONTO a request: any header the persona owns that the caller already
/// set is dropped first, so a page that passed `{headers: {"user-agent": …}}` to `fetch()` cannot
/// wear a different identity than the same process's navigation; and the wire is stamped from the
/// same persona in the same call.
///
/// Push ORDER here does not decide wire order — [`CHROME_HEADER_ORDER`] does, via wreq's
/// `headers_order` — so appending is safe.
pub fn apply_persona_identity(id: &PersonaIdentity, req: &mut FetchRequest) {
    req.headers.retain(|(k, _)| !PERSONA_OWNED_HEADERS.iter().any(|n| k.eq_ignore_ascii_case(n)));
    // `accept-language` is the one identity header a PAGE may legitimately state: it is
    // CORS-safelisted, and Chrome sends the author's value when `fetch()` carries one. So the
    // persona states it only when the request does not already, and a doubled header is impossible
    // either way.
    let authored_language =
        req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-language"));
    req.headers.extend(
        id.headers
            .iter()
            .filter(|(k, _)| !(authored_language && k.eq_ignore_ascii_case("accept-language")))
            .cloned(),
    );
    req.wire = Some(id.wire.clone());
    req.accept_navigation = id.accept_navigation.clone();
    req.third_party_cookies_blocked = id.third_party_cookies_blocked;
    req.high_entropy_hints = Some(id.high_entropy_hints.clone());
    req.chrome_build_year = id.chrome_build_year;
    apply_devtools_overrides(req, authored_language);
}

/// What a DevTools client set on this page's requests (`spec::devtools`), over the persona's headers: its user agent
/// and the client hints that go with it (none, for a user agent it gave no metadata for -- Chrome sends no `sec-ch-ua`
/// then), its `Accept-Language` unless the request states its own, and its extra headers over any of the same name
/// (`Network.setExtraHTTPHeaders`, which Chrome adds as the request leaves). Nothing set, nothing changed.
fn apply_devtools_overrides(req: &mut FetchRequest, authored_language: bool) {
    let set = |req: &mut FetchRequest, name: &str, value: String| {
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        req.headers.push((name.to_string(), value));
    };
    if let Some(o) = spec::devtools::user_agent() {
        if !o.user_agent.is_empty() {
            set(req, "user-agent", o.user_agent.clone());
            // the client hints change only with the client's metadata; without it they stay the persona's, as
            // Chrome's stay its own (`spec::devtools::client_hints`)
            if let Some(m) = &o.metadata {
                req.headers.retain(|(k, _)| !k.to_ascii_lowercase().starts_with("sec-ch-ua"));
                if !m.brands.is_empty() {
                    let sec = m.brands.iter().map(|b| format!("\"{}\";v=\"{}\"", b.brand, b.version)).collect::<Vec<_>>().join(", ");
                    req.headers.push(("sec-ch-ua".into(), sec));
                    req.headers.push(("sec-ch-ua-mobile".into(), if m.mobile { "?1".into() } else { "?0".into() }));
                    if !m.platform.is_empty() {
                        req.headers.push(("sec-ch-ua-platform".into(), format!("\"{}\"", m.platform)));
                    }
                }
                req.high_entropy_hints = None;
            }
        }
        if !o.accept_language.is_empty() && !authored_language {
            let langs = spec::devtools::languages_of(&o.accept_language);
            set(req, "accept-language", spec::devtools::accept_language_header(&langs));
        }
    }
    for (k, v) in spec::devtools::extra_headers() {
        set(req, &k.to_ascii_lowercase(), v);
    }
}

/// [`apply_persona_identity`] straight off a persona, for the call sites that hold one.
pub fn apply_persona_headers(p: &dyn spec::Persona, req: &mut FetchRequest) {
    apply_persona_identity(&persona_identity(p), req);
}

/// The header names this engine emits, in the order Chrome puts them on the wire. Handed to wreq as
/// `headers_order`, which sorts exactly these to the front (in this order) and leaves anything else
/// after them — so a page-supplied header cannot reshuffle the fingerprinted prefix.
///
/// Two facts fix the shape. (a) `sec-ch-ua*` precede `user-agent`: that is wreq-util's own Chrome
/// insertion order, and it is what the old code broke by pushing the persona's `user-agent` onto the
/// front of `req.headers`. (b) the `Sec-Fetch-*` quartet is emitted Site → Mode → User → Dest, which is
/// the order the Fetch Metadata spec appends them in and therefore the order Chromium's implementation
/// of that algorithm produces.
const CHROME_HEADER_ORDER: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "pragma",
    "cache-control",
    // the User-Agent hints in Chromium's navigation order (`client_hints::UA_NAVIGATION_ORDER`): the
    // low-entropy three on every navigation, the rest where an origin's `Accept-CH` asked for them
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-full-version",
    "sec-ch-ua-arch",
    "sec-ch-ua-platform",
    "sec-ch-ua-platform-version",
    "sec-ch-ua-model",
    "sec-ch-ua-bitness",
    "sec-ch-ua-wow64",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-form-factors",
    "upgrade-insecure-requests",
    // a form POST's body type and origin around `user-agent` (Chrome 150, 2026-10-11, HTTP/1.1 echo of
    // a page's and a frame's urlencoded, multipart and text/plain submissions: `upgrade-insecure-requests,
    // content-type, user-agent, origin, accept`; korvus put `content-type` after `user-agent` and sent no `origin`)
    "content-type",
    "user-agent",
    "origin",
    "accept",
    // Google Chrome's own, to Google's hosts (the network stack's, after `Accept`; Chrome 150 over HTTP/2:
    // `... user-agent, accept, x-browser-channel, x-browser-year, x-browser-validation, x-browser-copyright,
    // x-client-data, sec-fetch-site ...`)
    "x-browser-channel",
    "x-browser-year",
    "x-browser-validation",
    "x-browser-copyright",
    "x-client-data",
    "sec-gpc",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-user",
    "sec-fetch-dest",
    "sec-fetch-storage-access",
    "referer",
    "accept-encoding",
    "accept-language",
    "cookie",
    "if-none-match",
    "if-modified-since",
    "priority",
];

/// A SUBRESOURCE's header order (script, style, image, fetch/XHR, beacon, media) is three runs, measured
/// on the wire against Chrome 150 and Brave 151 (HTTP/1.1 echo, 2026-09-25 and 2026-10-02):
///
/// * the connection's own headers ([`SUBRESOURCE_HEAD`]);
/// * Blink's header MAP -- the page's own headers, `Content-Type`, the client hints and `User-Agent`
///   (and a media request's `Accept-Encoding`) -- in its hash-table order ([`blink_headers`]):
/// a function of the names, not of the order a page set them;
/// * what the network stack appends ([`SUBRESOURCE_TAIL`]), `Range` last.
///
/// A navigation keeps `CHROME_HEADER_ORDER`.
const SUBRESOURCE_HEAD: &[&str] = &["host", "connection", "content-length", "pragma", "cache-control"];
const SUBRESOURCE_TAIL: &[&str] = &[
    "accept",
    "sec-gpc",
    "origin",
    "x-browser-channel",
    "x-browser-year",
    "x-browser-validation",
    "x-browser-copyright",
    "x-client-data",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
    "sec-fetch-storage-access",
    "referer",
    "accept-encoding",
    "accept-language",
    "cookie",
    "if-none-match",
    "if-modified-since",
    "priority",
    "range",
];

/// HOW CHROME SPELLS A HEADER NAME on an HTTP/1.1 wire (HTTP/2 lowercases every name): the network stack's own
/// spelling (`net::HttpRequestHeaders`'s constants, `services/network`'s Fetch Metadata, the browser's
/// `Upgrade-Insecure-Requests`) for what the browser writes; the client hints lowercase, as Blink names them.
/// Chrome 150, 2026-10-11 (HTTP/1.1 echo of a navigation, a frame, a stylesheet, a script, an image,
/// fetch, XHR, a beacon and a CORS preflight): `Host`, `Connection`, `User-Agent`, `Sec-Fetch-Dest`, ... beside
/// `sec-ch-ua`; korvus wrote every name lowercase. A name not here goes out as the request spells it.
const CHROME_SPELLING: &[(&str, &str)] = &[
    ("host", "Host"),
    ("connection", "Connection"),
    ("content-length", "Content-Length"),
    ("pragma", "Pragma"),
    ("cache-control", "Cache-Control"),
    ("upgrade-insecure-requests", "Upgrade-Insecure-Requests"),
    ("user-agent", "User-Agent"),
    ("content-type", "Content-Type"),
    ("accept", "Accept"),
    ("origin", "Origin"),
    ("sec-gpc", "Sec-GPC"),
    ("sec-fetch-site", "Sec-Fetch-Site"),
    ("sec-fetch-mode", "Sec-Fetch-Mode"),
    ("sec-fetch-user", "Sec-Fetch-User"),
    ("sec-fetch-dest", "Sec-Fetch-Dest"),
    ("sec-fetch-storage-access", "Sec-Fetch-Storage-Access"),
    ("referer", "Referer"),
    ("accept-encoding", "Accept-Encoding"),
    ("accept-language", "Accept-Language"),
    ("cookie", "Cookie"),
    ("if-none-match", "If-None-Match"),
    ("if-modified-since", "If-Modified-Since"),
    ("range", "Range"),
    ("priority", "Priority"),
    ("access-control-request-method", "Access-Control-Request-Method"),
    ("access-control-request-headers", "Access-Control-Request-Headers"),
];

/// Put `name` (lowercase) into `order` as the PAGE spelled it, when the page set it (`authored`: a header list keeps
/// each name's spelling -- Chrome 150 sends a page's `content-type` lowercase and its `X-Foo` as `X-Foo`), else as
/// Chrome writes it ([`CHROME_SPELLING`]), else as given.
fn insert_spelled(order: &mut wreq::header::OrigHeaderMap, name: &str, authored: Option<&str>) {
    if let Some(a) = authored {
        order.insert(a.to_string());
        return;
    }
    match CHROME_SPELLING.iter().find(|(n, _)| *n == name) {
        Some((_, chrome)) => {
            order.insert(*chrome);
        }
        None => {
            order.insert(name.to_string());
        }
    }
}

/// A header name as a DevTools pause lists it (`intercept`): as the caller spelled it when that spelling is its own
/// (Chrome 150 listed a page's `X-Probe` and `Content-Type` as written), else as Chrome spells it on the wire, else as
/// it is (`sec-ch-ua` stays lowercase).
fn devtools_spelling(authored: &[(String, String)], name: &str) -> String {
    if let Some((k, _)) = authored.iter().find(|(k, _)| k.eq_ignore_ascii_case(name) && k.bytes().any(|b| b.is_ascii_uppercase())) {
        return k.clone();
    }
    let lower = name.to_ascii_lowercase();
    match CHROME_SPELLING.iter().find(|(n, _)| *n == lower) {
        Some((_, chrome)) => chrome.to_string(),
        None => name.to_string(),
    }
}

/// `headers` in the order a navigation sends them ([`CHROME_HEADER_ORDER`]; a name it does not list last, as given):
/// for a client that replays a page's identity with an HTTP client of its own (`Korvus.getIdentity`).
pub fn wire_order(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = headers.to_vec();
    out.sort_by_key(|(k, _)| CHROME_HEADER_ORDER.iter().position(|n| k.eq_ignore_ascii_case(n)).unwrap_or(usize::MAX));
    out
}

fn navigation_order() -> wreq::header::OrigHeaderMap {
    let mut order = wreq::header::OrigHeaderMap::new();
    for n in CHROME_HEADER_ORDER {
        insert_spelled(&mut order, n, None);
    }
    order
}

/// A navigation RESTARTED for `Critical-CH` ([`client_hints`]): Chromium takes the client hints off the
/// request and adds them back, so they follow `Accept` -- measured on Chrome 150 (loopback fixture and a
/// live challenge): `upgrade-insecure-requests, user-agent, accept, sec-ch-ua, ..., sec-fetch-site`.
fn restart_order() -> wreq::header::OrigHeaderMap {
    let mut order = wreq::header::OrigHeaderMap::new();
    let hint = |n: &&str| n.starts_with("sec-ch-ua");
    for n in CHROME_HEADER_ORDER.iter().filter(|n| !hint(n)) {
        insert_spelled(&mut order, n, None);
        if *n == "accept" {
            for h in client_hints::UA_NAVIGATION_ORDER {
                insert_spelled(&mut order, h, None);
            }
        }
    }
    order
}

/// A navigation's hop AFTER A REDIRECT: Chromium takes the client hints off the request and adds them back
/// (`NavigationRequest::OnRequestRedirected`), behind the Fetch Metadata headers the network service updated
/// in place -- not behind `Accept` as after a `Critical-CH` restart. Chrome 150
/// (`korvus-runtime/tests/request_shape_live.rs`): `upgrade-insecure-requests, user-agent, accept,
/// sec-fetch-site, sec-fetch-mode, sec-fetch-user, sec-fetch-dest, sec-ch-ua, sec-ch-ua-mobile,
/// sec-ch-ua-platform, accept-encoding, accept-language, cookie`; korvus kept the first hop's order.
fn redirect_order() -> wreq::header::OrigHeaderMap {
    let mut order = wreq::header::OrigHeaderMap::new();
    let hint = |n: &&str| n.starts_with("sec-ch-ua");
    for n in CHROME_HEADER_ORDER.iter().filter(|n| !hint(n)) {
        insert_spelled(&mut order, n, None);
        if *n == "sec-fetch-dest" {
            for h in client_hints::UA_NAVIGATION_ORDER {
                insert_spelled(&mut order, h, None);
            }
        }
    }
    order
}

/// A CORS preflight's order, as Chromium builds it (`preflight_controller.cc` `CreatePreflightRequest`):
/// `Accept`, `Access-Control-Request-Method`, `-Headers`, `Origin`, the request's `User-Agent`, its own
/// `Sec-Fetch-Mode: cors`; then the network stack's `Sec-Fetch-Site`, `Sec-Fetch-Dest`, `Referer`,
/// `Accept-Encoding`, `Accept-Language`; on HTTP/2 the `priority` of the request it precedes. Chrome 150
/// sent exactly that; korvus sent the client hints first and `Sec-Fetch-Site`
/// before `-Mode`.
const PREFLIGHT_ORDER: &[&str] = &[
    "host",
    "connection",
    "accept",
    "access-control-request-method",
    "access-control-request-headers",
    "origin",
    "user-agent",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-fetch-dest",
    "referer",
    "accept-encoding",
    "accept-language",
    "priority",
];

/// `req`'s subresource order (see [`SUBRESOURCE_HEAD`]): the map holds what the PAGE set
/// (`PageRequest::author_headers`, `Origin` aside -- the network stack writes that), a media
/// request's `Accept-Encoding`, then the identity headers the request carries -- the client hints in
/// Blink's insertion order (the low-entropy three and the `extra` an origin's `Accept-CH` added), then
/// `User-Agent`.
fn subresource_order(req: &FetchRequest, extra: &[&str]) -> wreq::header::OrigHeaderMap {
    let mut keys: Vec<String> = Vec::new();
    if origin_in_blink_map(req) {
        keys.push("origin".into());
    }
    // A GET's `Range` is not where the page put it: the HTTP cache takes it off the request and sets it back
    // (`HttpCache::Transaction`, `http_cache_transaction.cc:2884-2889`, `PartialData::SetHeaders`), so it goes
    // out LAST -- Chrome 150, `fetch(u, {headers: {Range: 'bytes=0-1'}})`: `..., Accept-Encoding: identity,
    // Accept-Language: ..., Range: bytes=0-1`.
    let cache_moves_range = req.method.eq_ignore_ascii_case("GET");
    keys.extend(
        req.page
            .as_ref()
            .map(|p| {
                p.author_headers
                    .iter()
                    .map(|(k, _)| k.to_ascii_lowercase())
                    .filter(|k| k != "origin" && !(cache_moves_range && k == "range"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    if is_media(&req.destination) {
        keys.push("accept-encoding".into());
    }
    let identity = client_hints::UA_BLINK_ORDER.iter().chain(std::iter::once(&"user-agent"));
    for n in identity {
        let carried = req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n)) || extra.contains(n);
        if carried && !(req.no_client_hints && is_client_hint(n)) {
            keys.push((*n).into());
        }
    }
    let map = blink_headers::map_order(&keys);
    let mut order = wreq::header::OrigHeaderMap::new();
    for n in SUBRESOURCE_HEAD.iter().filter(|n| !map.iter().any(|m| m == *n)) {
        insert_spelled(&mut order, n, None);
    }
    let authored = req.page.as_ref().map(|p| p.author_headers.as_slice()).unwrap_or_default();
    for n in &map {
        if wreq::header::HeaderName::from_bytes(n.as_bytes()).is_ok() {
            let spelled = authored.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(k, _)| k.as_str());
            insert_spelled(&mut order, n, spelled);
        }
    }
    for n in SUBRESOURCE_TAIL.iter().filter(|n| !map.iter().any(|m| m == *n)) {
        insert_spelled(&mut order, n, None);
    }
    order
}

/// Is `req`'s `Origin` one BLINK put in the request's header map -- the map's first key -- rather than one
/// the network service adds after it? `FetchParameters::SetCrossOriginAccessControl` calls `SetHTTPOrigin`
/// before anything else is in the map, for every load that is CORS because its LOADER says so: a module
/// script, an element carrying `crossorigin`, a CSS font -- same-origin ones included. A page's `fetch()`
/// gets its `Origin` from the network service, at the tail.
///
/// Measured (Chrome 150 over h2, `origin sec-ch-ua-platform user-agent sec-ch-ua
/// sec-ch-ua-mobile accept ...` on every one of them): a same- and a cross-origin module script, parser
/// `<script crossorigin>` same and cross, a script-inserted one, `<img crossorigin>`, `<link rel=stylesheet
/// crossorigin>`, a font, a `<track>` of a `crossorigin` video; and a challenge provider's `api.js` on nih.gov. korvus
/// put it after `accept`.
fn origin_in_blink_map(req: &FetchRequest) -> bool {
    req.mode == Mode::Cors
        && match &req.page {
            Some(p) => p.cors_preconfigured,
            // The element loader's own: it states `Origin` itself (see `korvus-runtime`'s loader).
            None => req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")),
        }
}

/// A low-entropy User-Agent Client Hint (`sec-ch-ua`, `-mobile`, `-platform`).
fn is_client_hint(name: &str) -> bool {
    name.len() >= 9 && name[..9].eq_ignore_ascii_case("sec-ch-ua")
}

/// A media element's request (`video`, `audio`): Blink's media loader asks for `bytes=0-` with
/// `Accept-Encoding: identity;q=1, *;q=0` (Brave 151 and Chrome 150).
fn is_media(dest: &Destination) -> bool {
    matches!(dest.token(), "video" | "audio")
}

/// Does `req` carry credentials at this hop -- Fetch's *includeCredentials*, "request's credentials mode is
/// `include`, or `same-origin` and its response tainting is `basic`" -- given its URL LIST so far? Tainting
/// is `basic` only while every URL the request has been at is same-origin with its origin; one hop away
/// and it stays tainted, even back home.
///
/// A navigation and a WebSocket handshake are `include`. So is an ELEMENT's no-cors load (a `page`-less
/// request): HTML's "No CORS" state of a CORS settings attribute is credentials mode `include`. A PAGE's
/// `fetch(u, {mode: "no-cors"})` is not -- its credentials mode is the `RequestInit`'s, `same-origin` by
/// default. A request with no initiator is the user agent's own, and keeps its cookies.
///
/// The cookie jar used to be consulted for every request whatever its credentials
/// mode said. Chrome 150, two ports of `localhost` (one site, two origins), a cookie set on the first:
///
/// ```text
///                                                   Chrome 150   korvus (before)
///   fetch(cross)                  same-origin       no cookie    k=1
///   fetch(same, {credentials: 'omit'})              no cookie    k=1
///   XHR cross, withCredentials false                no cookie    k=1
///   <img crossorigin>, <script crossorigin> cross   no cookie    k=1
///   <script type=module> cross                      no cookie    k=1
///   fetch(cross, {credentials: 'include'}), <script crossorigin=use-credentials>, no-cors loads: k=1 in both
/// ```
fn credentials_included(req: &FetchRequest, initiator: Option<&url::Url>, url_list: &[url::Url]) -> bool {
    match req.mode {
        Mode::Navigate | Mode::Websocket => true,
        Mode::NoCors if req.page.is_none() => true,
        // A request from an OPAQUE origin (a sandboxed frame's, its blob worker's) is same-origin with nothing: only
        // `include` sends credentials.
        _ if req.opaque_origin => req.credentials == CredentialsMode::Include,
        _ => match (req.credentials, initiator) {
            (_, None) => true,
            (CredentialsMode::Include, _) => true,
            (CredentialsMode::Omit, _) => false,
            (CredentialsMode::SameOrigin, Some(i)) => url_list.iter().all(|u| same_origin(i, u)),
        },
    }
}

/// The cookie jar a request whose credentials are not included sees: none to send, and a `Set-Cookie` on
/// its response is not stored (Fetch "HTTP-network fetch": cookies are set only when includeCredentials).
struct NoCredentials;

impl wreq::cookie::CookieStore for NoCredentials {
    fn set_cookies(&self, _: &mut dyn Iterator<Item = &wreq::header::HeaderValue>, _: &wreq::Uri) {}

    fn cookies(&self, _: &wreq::Uri, _: wreq::Version) -> wreq::cookie::Cookies {
        wreq::cookie::Cookies::Empty
    }
}

/// `Sec-Fetch-Storage-Access` for a credentialed request from a cross-site context, or
/// `None` when it carries none. Measured 2026-10-02 on a localhost frame under a 127.0.0.1 page: Chrome
/// 150, Edge 151 and Opera 135 `active` on every one; Brave 151, which blocks third-party cookies,
/// `none` -- and `inactive` (with an `Origin`) on the request back to the top-level site.
fn storage_access(req: &FetchRequest, initiator: Option<&url::Url>, url_list: &[url::Url], target: &url::Url) -> Option<&'static str> {
    if !req.cookie_cross_site || !credentials_included(req, initiator, url_list) {
        return None;
    }
    if !req.third_party_cookies_blocked {
        return Some("active");
    }
    let top = req.top_level_url.as_deref().and_then(|u| url::Url::parse(u).ok());
    Some(if top.is_some_and(|t| same_site(&t, target)) { "inactive" } else { "none" })
}

/// The HTTP/2 `priority` header (RFC 9218) Chromium sends for `req`, or `None` for none. Measured on
/// 2026-09-25 over h2 against Chrome 150 and Brave 151, which agree on every row: a navigation (document
/// or iframe) `u=0, i`; a stylesheet `u=0`; a parser-blocking or module script `u=1` and an async/defer
/// one none; a parser-inserted image `u=2, i` and a script-created or lazy one `i`; fetch/XHR `u=1, i`;
/// a beacon `u=4, i`; media `i`. This is Blink's `TypeToPriority` per destination; what moves a request
/// off it -- `async`, a script after the document's first image, the preload scanner's image boost,
/// `fetchpriority`, a non-matching `media` -- only its loader knows, and it states the result in
/// `FetchRequest::priority` (`korvus-webapi`'s `load_priority`).
fn chromium_priority(req: &FetchRequest) -> Option<String> {
    if let Some(p) = &req.priority {
        return if p.is_empty() { None } else { Some(p.clone()) };
    }
    if is_navigation_like(&req.destination, req.mode) {
        return Some("u=0, i".into());
    }
    let p = match req.destination.token() {
        "style" | "font" => "u=0",
        "script" | "worker" | "sharedworker" | "serviceworker" => "u=1",
        // Blink's `TypeToPriority` for an image is Low; the first five sizeable images the preload
        // scanner finds in a document are boosted (`u=2, i`), which only that loader knows to state.
        "image" => "i",
        "empty" | "json" => "u=1, i",
        "video" | "audio" | "track" => "i",
        _ => return None,
    };
    Some(p.into())
}

/// A request record's `referrerPolicy` member: a policy token, `""` (the default policy) or absent.
fn referrer_policy_token<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<ReferrerPolicy>, D::Error> {
    let s: Option<String> = serde::Deserialize::deserialize(d)?;
    Ok(s.as_deref().and_then(ReferrerPolicy::from_token))
}

/// A referrer policy (Referrer Policy sec. 3). The default is Chrome's for the empty policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReferrerPolicy {
    NoReferrer,
    NoReferrerWhenDowngrade,
    SameOrigin,
    Origin,
    StrictOrigin,
    OriginWhenCrossOrigin,
    #[default]
    StrictOriginWhenCrossOrigin,
    UnsafeUrl,
}

impl ReferrerPolicy {
    /// One policy token, ASCII case-insensitively (the `referrerpolicy` attribute's and the
    /// `RequestInit` member's grammar). The empty string and an unknown token are `None`: they set
    /// nothing, so the next policy down the chain decides.
    pub fn from_token(tok: &str) -> Option<Self> {
        Some(match tok.trim().to_ascii_lowercase().as_str() {
            "no-referrer" => Self::NoReferrer,
            "no-referrer-when-downgrade" => Self::NoReferrerWhenDowngrade,
            "same-origin" => Self::SameOrigin,
            "origin" => Self::Origin,
            "strict-origin" => Self::StrictOrigin,
            "origin-when-cross-origin" => Self::OriginWhenCrossOrigin,
            "strict-origin-when-cross-origin" => Self::StrictOriginWhenCrossOrigin,
            "unsafe-url" => Self::UnsafeUrl,
            _ => return None,
        })
    }

    /// A `Referrer-Policy` header value: a comma-separated list whose LAST recognised token wins, so a
    /// site can name a new policy with a fallback before it (Referrer Policy sec. 8.1, "parse a
    /// referrer policy from a Referrer-Policy header").
    pub fn from_header(value: &str) -> Option<Self> {
        value.split(',').filter_map(Self::from_token).last()
    }

    /// The last `Referrer-Policy` among response headers, if any names a policy.
    pub fn from_headers(headers: &[(String, String)]) -> Option<Self> {
        headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("referrer-policy")).filter_map(|(_, v)| Self::from_header(v)).last()
    }

    /// The token, as `Request.referrerPolicy` and `HTMLElement.referrerPolicy` spell it.
    pub fn token(self) -> &'static str {
        match self {
            Self::NoReferrer => "no-referrer",
            Self::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
            Self::SameOrigin => "same-origin",
            Self::Origin => "origin",
            Self::StrictOrigin => "strict-origin",
            Self::OriginWhenCrossOrigin => "origin-when-cross-origin",
            Self::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
            Self::UnsafeUrl => "unsafe-url",
        }
    }
}

/// `Referer` for a request from `initiator` to `target` under Chrome's default referrer policy
/// (`strict-origin-when-cross-origin`): the full URL (minus credentials and fragment) same-origin, the
/// bare origin cross-origin, and NOTHING on an https → http downgrade.
pub fn referer_for(initiator: &url::Url, target: &url::Url) -> Option<String> {
    referer_with_policy(initiator, target, ReferrerPolicy::default())
}

/// `Referer` under `policy` (Referrer Policy sec. 8.3, "determine request's referrer"). Measured on a
/// loopback pair against stock Chrome 150 (`tools/realweb/seed/refperm`): a `same-origin` document
/// sends nothing cross-origin from an image, a script, a `fetch`, or a frame, and an element's
/// `referrerpolicy="unsafe-url"` or a `fetch` init's sends the full URL.
pub fn referer_with_policy(initiator: &url::Url, target: &url::Url, policy: ReferrerPolicy) -> Option<String> {
    if !matches!(initiator.scheme(), "http" | "https") || !matches!(target.scheme(), "http" | "https") {
        return None;
    }
    let full = || {
        let mut v = initiator.clone();
        let _ = v.set_username("");
        let _ = v.set_password(None);
        v.set_fragment(None);
        v.to_string()
    };
    let origin = || format!("{}/", initiator.origin().ascii_serialization());
    let downgrade = initiator.scheme() == "https" && !is_potentially_trustworthy(target);
    let same = same_origin(initiator, target);
    match policy {
        ReferrerPolicy::NoReferrer => None,
        ReferrerPolicy::UnsafeUrl => Some(full()),
        ReferrerPolicy::Origin => Some(origin()),
        ReferrerPolicy::SameOrigin => same.then(full),
        ReferrerPolicy::OriginWhenCrossOrigin => Some(if same { full() } else { origin() }),
        ReferrerPolicy::StrictOrigin => (!downgrade).then(origin),
        ReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then(full),
        ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same {
                Some(full())
            } else if downgrade {
                None
            } else {
                Some(origin())
            }
        }
    }
}

/// The reused HTTP clients, **one per wire**, built on first use and keyed by the profile that
/// decided their bytes. Reuse keeps the connection pool alive across requests (the cookies live in
/// [`cookies`], which every client shares) --
/// both required for a real site's request sequence (a per-request client also churns connections,
/// which surfaced as spurious "Connection closed" mid-sequence).
///
/// KEYED, NOT SINGLE, and the key is the point. One client per wire means two personas in one
/// process never share a connection pool -- which is the thing "one process, one wire fingerprint"
/// was really protecting: a reused h2 connection carries whatever ClientHello opened it, so a second
/// persona borrowing the first's pool would be wearing the first's handshake no matter what it
/// declared. Separate clients make that impossible instead of forbidden.
///
/// A `Mutex<HashMap>` rather than a `OnceLock`: the set is not known until requests arrive, and it is
/// written at most once per distinct profile. `wreq::Client` is an `Arc` inside, so the clone handed
/// out shares the pool with every other holder of the same key.
/// The KEY IS THE WHOLE `TlsProfile`, not its name: a bundle that corrects one measured field of
/// the preset (`h2_priority`) is a different wire from the same preset uncorrected, and two
/// personas that differ only there must not share a pool whose connections carry the other's
/// HEADERS frame.
static CLIENTS: OnceLock<Mutex<HashMap<spec::TlsProfile, wreq::Client>>> = OnceLock::new();
/// ABI 3 (wave 1, 4.5 `verify_cert`): the certificate-verification-OFF clients ([`insecure_client_for`]), kept apart
/// from [`CLIENTS`] so a verifying request and a plugin-decided one never share a connection pool — a reused
/// connection carries the verification mode it was opened under, exactly as it carries its ClientHello.
static INSECURE_CLIENTS: OnceLock<Mutex<HashMap<spec::TlsProfile, wreq::Client>>> = OnceLock::new();

/// Whether a request to `request_url` from a document whose SITE FOR COOKIES is `site_for_cookies` is cross-site:
/// `""` names the page's own (`page_url`), `"null"` a document under a cross-site ancestor (always
/// cross-site), anything else is that top-level URL. A URL that does not parse decides nothing (same-site).
pub fn cookie_cross_site(site_for_cookies: &str, page_url: &str, request_url: &str) -> bool {
    let site = if site_for_cookies.is_empty() { page_url } else { site_for_cookies };
    if site == "null" {
        return true;
    }
    match (url::Url::parse(site), url::Url::parse(request_url)) {
        (Ok(s), Ok(r)) => (r.scheme() == "http" || r.scheme() == "https") && !same_site(&s, &r),
        _ => false,
    }
}

/// Every preset name `wreq-util` itself declares, paired with the variant it names.
///
/// **DERIVED FROM THE LIBRARY, NEVER TYPED HERE.** `Profile::VARIANTS` is the library's own list and
/// the `emulation-serde` derive carries each variant's declared `serde(rename)` -- which IS the name
/// the enum's own macro wrote next to it (`Chrome149 => ("chrome_149", ...)`). Serializing a variant
/// therefore returns the library's spelling, not ours.
///
/// WHY THIS EXISTS AT ALL. The mapping used to be one hand-written match arm per version, and a hand
/// list rots silently: this one had 40 Chrome arms for the library's 41 and was missing `chrome_110`,
/// so a bundle naming it would have been refused by a transport that could in fact emit it. That is
/// the same failure as a silent fallback wearing a different mask -- a preset the library ships and
/// this function cannot reach. Asking the library removes the possibility rather than fixing the
/// instance, and it is why the resolution test enumerates THIS function's output instead of a list a
/// test author typed.
pub fn profile_names() -> Vec<(String, wreq_util::Profile)> {
    wreq_util::Profile::VARIANTS
        .iter()
        .filter_map(|&p| match serde_json::to_value(p) {
            Ok(serde_json::Value::String(name)) => Some((name, p)),
            _ => None,
        })
        .collect()
}

/// Resolve a `TlsProfile` id to a preset. **Fails closed**: an id with no mapping is an error, never
/// a fallback, because falling back means emitting some other browser's ClientHello under this
/// persona's UA -- the exact incoherence this crate exists to prevent.
///
/// Resolution asks `wreq-util` for its own name (see [`profile_names`]), so the set this accepts is
/// exactly the set the pinned library can emit -- 133 presets at wreq-util 0.2.0, the whole Chromium
/// family (41 `chrome_*`, 19 `edge_*`, 16 `opera_*`) included, and whatever a version bump adds
/// without an edit here.
///
/// IT DOES NOT FILTER BY BROWSER FAMILY, deliberately. The manifest names a preset and the transport
/// resolves it; no code here asks which browser a name belongs to. Whether a Chromium persona may
/// wear a Firefox ClientHello is a COHERENCE question and `korvus-kernel`'s `coherence::audit` is
/// where it is answered -- and today it is not: that check parses the trailing integer out of the id
/// and compares it to the UA's Chrome major, so `firefox_151` under a Chrome/151 UA reads as
/// agreement. Nothing can reach that hole at present (`promote.py` stamps one constant, `chrome_149`,
/// into every bundle it writes), and plugging it by teaching THIS function a family allowlist would
/// put the browser-name branch in the engine that P2 exists to keep out.
pub fn emulation_for(id: &str) -> anyhow::Result<wreq_util::Profile> {
    let known = profile_names();
    if let Some((_, p)) = known.iter().find(|(name, _)| name == id) {
        return Ok(*p);
    }
    // The list is part of the error ON PURPOSE. This function refuses rather than falls back, so the
    // refusal is the only thing the operator gets to read, and "that name is wrong" without "these
    // are the names" costs a round.
    anyhow::bail!(
        "no TLS emulation preset for {id:?}. The pinned wreq-util ships {} presets and this is not \
         one of them: {}. A bundle asking for anything else must either change its declared preset \
         or the pin. Refusing to start rather than emitting a different browser's ClientHello.",
        known.len(),
        known.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
    )
}

/// Check that a persona's declared profile is one this build can actually emit, WITHOUT committing
/// the process to it.
///
/// `Engine::assemble` calls this so a bundle naming a preset the pinned `wreq-util` does not ship
/// refuses to start, which is the fail-closed property `configure` used to carry. What it no longer
/// does is claim the preset for the whole process: the wire travels on the request now
/// ([`FetchRequest::wire`]), so a second engine wearing a different browser is not a contradiction to
/// be refused -- it is a second wire, and both are correct for their own requests.
///
/// It builds nothing. A process that assembles four personas and fetches under none (the persona
/// surface gates) opens no socket and constructs no client, which is what made the old eager
/// `configure` the wrong shape for a differential in the first place.
pub fn check_wire(profile: &spec::TlsProfile) -> anyhow::Result<()> {
    emulation_for(&profile.id)?;
    // H3: a named QUIC preset must exist, or the bundle claims a wire nothing here can produce.
    if let Some(q) = profile.quic.as_deref() {
        if quic_presets::options_for(q).is_none() {
            return Err(anyhow::anyhow!("the persona's QUIC preset {q:?} is not one this transport carries"));
        }
    }
    Ok(())
}

/// The OS the emulation should claim. Unknown names fall back to the library default rather than
/// failing: a platform this build cannot name is a smaller problem than refusing to start, and the
/// coherence audit already compares the persona's platform against its UA.
fn platform_for(name: &str) -> wreq_util::Platform {
    use wreq_util::Platform;
    use wreq_util::Platform::*;
    match name {
        "macos" | "mac" => MacOS,
        "windows" | "win" => Windows,
        "linux" => Linux,
        "android" => Android,
        "ios" => IOS,
        _ => Platform::default(),
    }
}

/// The cookie-aware, browser-emulating client **for one wire** (built once per wire, then reused).
///
/// The returned clone shares its connection pool (and the one cookie store) with every other holder of the same
/// `wire`; a different `wire` gets a different pool, because a pooled connection carries the
/// handshake that opened it.
// ── A missing intermediate, completed from the certificate's AIA URL ─────────────────
//
// A server that sends only its leaf fails verification ("unable to get local issuer") where Chrome
// loads it: Chrome fetches the missing issuer from the certificate's Authority Information Access
// "CA Issuers" URL and completes the chain. 7-11.com.tw sends only `*.7-11.com.tw`, issued by
// `GlobalSign RSA OV SSL CA 2018` (openssl: verify code 21, 2026-09-27). The vendored wreq connector
// reports the certificate whose issuer was missing ([`record_issuer_miss`]); [`fetch`] then fetches
// that issuer once, adds it to the store the clients are built with, and sends the hop again.

static ISSUER_MISSES: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
static INTERMEDIATES: OnceLock<Mutex<Vec<Vec<u8>>>> = OnceLock::new();

fn record_issuer_miss(host: &str, der: &[u8]) {
    if let Ok(mut m) = ISSUER_MISSES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        m.insert(host.to_ascii_lowercase(), der.to_vec());
    }
}

fn learned_intermediates() -> Vec<Vec<u8>> {
    INTERMEDIATES.get().and_then(|m| m.lock().ok().map(|v| v.clone())).unwrap_or_default()
}

/// Every `caIssuers` URI (OID 1.3.6.1.5.5.7.48.2) in a DER certificate's AIA extension, at most
/// [`AIA_URLS_PER_CERT`] (Chromium's `kMaxFetchesPerCert`): each the OID's encoding, then an `[6] IA5String` (tag 0x86)
/// with a short- or one-byte long-form length; only http(s) ones are fetched, as Chromium's `FetchCaIssuers`.
fn ca_issuers_urls(der: &[u8]) -> Vec<String> {
    const OID: [u8; 10] = [0x06, 0x08, 0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = der.get(from..).and_then(|d| d.windows(OID.len()).position(|w| w == OID)) {
        let at = from + at + OID.len();
        from = at;
        let Some(rest) = der.get(at..) else { break };
        if rest.first() != Some(&0x86) {
            continue;
        }
        let (len, start) = match rest.get(1) {
            Some(&n) if n < 0x80 => (n as usize, 2),
            Some(&0x81) => match rest.get(2) {
                Some(&n) => (n as usize, 3),
                None => break,
            },
            _ => continue,
        };
        let Some(url) = rest.get(start..start + len).and_then(|b| std::str::from_utf8(b).ok()) else { continue };
        if (url.starts_with("http://") || url.starts_with("https://")) && out.len() < AIA_URLS_PER_CERT {
            out.push(url.to_string());
        }
    }
    out
}

/// One DER element at the front of `d`: its tag, its contents and what follows. Definite lengths only (DER).
fn der_element(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *d.first()?;
    let first = *d.get(1)?;
    let (len, head) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for b in d.get(2..2 + n)? {
            len = (len << 8) | *b as usize;
        }
        (len, 2 + n)
    };
    let body = d.get(head..head.checked_add(len)?)?;
    Some((tag, body, &d[head + len..]))
}

/// The certificates an AIA response carries, as Chromium's `CertIssuerSourceAia` reads one: a DER certificate, else a
/// PKCS#7 SignedData's `certificates` (`CreateCertBuffersFromPKCS7Bytes`, a `.p7c`). Each one returned is a whole
/// DER `Certificate`; PEM text is not read (nor is it by Chromium).
fn aia_certificates(bytes: &[u8]) -> Vec<Vec<u8>> {
    const SIGNED_DATA: [u8; 9] = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
    let Some((0x30, body, _)) = der_element(bytes) else { return Vec::new() };
    // ContentInfo { OID signedData, [0] EXPLICIT SignedData } -- or else it is a certificate itself
    let pkcs7 = || -> Option<Vec<Vec<u8>>> {
        let (tag, oid, rest) = der_element(body)?;
        if tag != 0x06 || oid != SIGNED_DATA {
            return None;
        }
        let (0xA0, explicit, _) = der_element(rest)? else { return None };
        let (0x30, mut signed, _) = der_element(explicit)? else { return None };
        // version, digestAlgorithms, contentInfo, then [0] IMPLICIT certificates
        for _ in 0..3 {
            signed = der_element(signed)?.2;
        }
        let (0xA0, mut certs, _) = der_element(signed)? else { return Some(Vec::new()) };
        let mut out = Vec::new();
        while !certs.is_empty() {
            let (tag, _, rest) = der_element(certs)?;
            let whole = &certs[..certs.len() - rest.len()];
            if tag == 0x30 {
                out.push(whole.to_vec());
            }
            certs = rest;
        }
        Some(out)
    };
    match pkcs7() {
        Some(list) => list,
        None => vec![bytes.to_vec()],
    }
}

fn is_cert_verify_failure(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("CERTIFICATE_VERIFY_FAILED")
}

/// ABI 3 (wave 1, 4.5 `verify_cert`): is any loaded plugin filling the `verify_cert` slot (the host reports it as the
/// `"tls"` slot)? Only then is it worth retrying a certificate the trust store rejected on the insecure client to ask
/// the plugins about it -- with none, the rejection stands as it did before, with no extra round-trip.
fn has_tls_plugin() -> bool {
    plugin_host::any() && plugin_host::loaded().iter().any(|p| p.slots().contains(&"tls"))
}

/// Chromium's AIA bounds (`cert_issuer_source_aia.cc`): at most 5 `caIssuers` URLs fetched per certificate, each in
/// 10 s and at most 64 KiB; and the path builder's 20 iterations (`kPathBuilderIterationLimit`), which bound how many
/// missing issuers one request chases.
const AIA_URLS_PER_CERT: usize = 5;
const AIA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const AIA_MAX_BYTES: usize = 65536;
const AIA_CHASES_PER_REQUEST: u32 = 20;
/// The most intermediates one process learns. korvus's OWN bound: Chromium keeps no such list (its AIA answers are
/// inputs to one verification), and korvus's is process-wide, so without a bound a page sending requests at hosts with
/// junk chains grew it by up to [`AIA_CHASES_PER_REQUEST`] certificates -- and a client rebuild -- per request (a
/// review). A full list learns nothing more: no eviction, so no rebuild churn; a host whose chain needs a
/// new intermediate then fails as it did before T6.
const AIA_LEARNED_MAX: usize = 64;

/// Add an AIA answer's certificates to the learned list `v`: never a root -- a server could name its own -- only
/// issued intermediates, as Chromium's path builder takes AIA answers as issuers and roots only from its trust store;
/// none already held; none once `v` holds [`AIA_LEARNED_MAX`]. `true` when one was added.
fn learn_into(v: &mut Vec<Vec<u8>>, certs: Vec<Vec<u8>>) -> bool {
    let mut learned = false;
    for cert in certs.into_iter().filter(|c| !wreq::tls::trust::is_self_issued_der(c)) {
        if v.len() >= AIA_LEARNED_MAX {
            break;
        }
        if !v.iter().any(|c| *c == cert) {
            v.push(cert);
            learned = true;
        }
    }
    learned
}

/// Fetch the issuer `host`'s handshake reported missing, and learn it. `true` when a new certificate
/// was learned (the clients are rebuilt with it); `false` when there is nothing to try.
///
/// A chain can lack more than one certificate -- Let's Encrypt's `YR1` names `ISRG Root YR`, itself cross-signed by
/// `ISRG Root X1`, and `incomplete-chain.badssl.com` serves neither -- so the caller chases again after each one it
/// learned, as Chromium's path builder asks AIA for each issuer it misses (it loads in Chrome 150; korvus refused it
/// at the first missing issuer, ledger N41).
async fn chase_aia(host: &str, wire: &spec::TlsProfile) -> bool {
    let der = ISSUER_MISSES
        .get()
        .and_then(|m| m.lock().ok().and_then(|mut m| m.remove(&host.to_ascii_lowercase())));
    let Some(der) = der else { return false };
    let Ok(client) = client_for(wire) else { return false };
    let mut learned = false;
    for url in ca_issuers_urls(&der) {
        let Ok(resp) = client.get(url.as_str()).header("accept", "*/*").timeout(AIA_TIMEOUT).send().await else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(bytes) = resp.bytes().await else { continue };
        if bytes.len() > AIA_MAX_BYTES {
            continue;
        }
        let Ok(mut v) = INTERMEDIATES.get_or_init(|| Mutex::new(Vec::new())).lock() else { return false };
        if learn_into(&mut v, aia_certificates(&bytes)) {
            learned = true;
            break;
        }
    }
    if learned {
        // Every client is rebuilt with the new store on its next use -- the WebSocket ones too.
        if let Ok(mut m) = CLIENTS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
            m.clear();
        }
        websocket::forget_clients();
    }
    learned
}

/// Hex to bytes, or `None` for an odd length or a non-hex digit.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn client_for(wire: &spec::TlsProfile) -> anyhow::Result<wreq::Client> {
    build_or_cached_client(CLIENTS.get_or_init(|| Mutex::new(HashMap::new())), wire, false, None)
}

/// ABI 3 (wave 1, 4.5 `verify_cert`): the SAME stealth client -- byte-for-byte the persona's ClientHello, so an
/// intercepting proxy (Burp/ZAP) still sees the real browser -- but with certificate verification OFF and `TlsInfo`
/// added to every response. [`fetch`] falls onto it only after a normal verify failed and a `verify_cert` plugin is
/// loaded, and then shows each https hop's leaf certificate to [`plugin_host::verify_cert`] and lets only an explicit
/// allow through (a deny or a defer fails the hop). Cached in its own map ([`INSECURE_CLIENTS`]) so a connection never
/// carries a verification mode other than the one it was opened under.
fn insecure_client_for(wire: &spec::TlsProfile) -> anyhow::Result<wreq::Client> {
    build_or_cached_client(INSECURE_CLIENTS.get_or_init(|| Mutex::new(HashMap::new())), wire, true, None)
}

/// ABI 3 (step 13, D4 `resolve`): [`client_for`]'s stealth client for `wire`, but with `host` pinned to `addrs` by
/// a resolver plugin -- the one name this client answers from the plugin's addresses, every other host normally.
/// Built in the hop's current verification mode (`insecure`), so the ClientHello and the trust-store verdict are
/// exactly [`client_for`]'s / [`insecure_client_for`]'s: the ONLY change is the name-to-address step. Deliberately
/// NOT cached and NOT shared with [`CLIENTS`]/[`INSECURE_CLIENTS`] -- the pin is this request's, never the wire's,
/// so no other host or request can inherit it.
fn resolved_client_for(
    wire: &spec::TlsProfile,
    host: &str,
    addrs: &[std::net::SocketAddr],
    insecure: bool,
) -> anyhow::Result<wreq::Client> {
    let clients = if insecure {
        INSECURE_CLIENTS.get_or_init(|| Mutex::new(HashMap::new()))
    } else {
        CLIENTS.get_or_init(|| Mutex::new(HashMap::new()))
    };
    build_or_cached_client(clients, wire, insecure, Some((host, addrs)))
}

/// ABI 3 (step 13, D4 `resolve`): the host of `url`, resolved through the loaded plugins before a hop connects.
/// Called only once [`plugin_host::has_resolver`] has fired (with no resolver plugin the host resolves normally and
/// this is never reached -- the engine is byte-for-byte unchanged, I9/J2). Returns:
///   * `Ok(None)` -- no plugin answered for this host, or `url` names no host or an IP literal (a literal is never
///     looked up, as Chromium's resolver hands it straight to connect): the host resolves normally.
///   * `Ok(Some(client))` -- a plugin pinned the host to IPs: [`resolved_client_for`]'s client, which connects this
///     one name to those addresses (the request's own port kept) and every other host normally.
///   * `Err(..)` -- the plugin blocked the name (`DnsAnswer::Block`), or pinned it to addresses none of which
///     parsed: the hop fails as a DNS failure (`net::ERR_NAME_NOT_RESOLVED`). Fail-closed on a pin that cannot be
///     honoured, so a blocked or unparseable answer never falls back to the real resolver and leaks the name.
fn dns_pinned_client(url: &url::Url, wire: &spec::TlsProfile, insecure: bool) -> anyhow::Result<Option<wreq::Client>> {
    // Only a registrable NAME is resolved; an IP literal (and a request with no host) is handed to connect as-is.
    let Some(url::Host::Domain(host)) = url.host() else {
        return Ok(None);
    };
    match plugin_host::resolve(host) {
        None => Ok(None),
        Some(plugin_host::DnsAnswer::Block) => Err(anyhow::anyhow!("net::ERR_NAME_NOT_RESOLVED: a plugin blocked DNS for {host}")),
        Some(plugin_host::DnsAnswer::Ips(ips)) => {
            // Each answer is an `IP` (the request's own port is kept) or an explicit `IP:port`; an entry that is
            // neither is dropped. A pin whose addresses all fail to parse fails the hop fail-closed rather than
            // leak the name to the real resolver.
            let addrs: Vec<std::net::SocketAddr> = ips
                .iter()
                .filter_map(|ip| {
                    ip.parse::<std::net::SocketAddr>()
                        .or_else(|_| ip.parse::<std::net::IpAddr>().map(|a| std::net::SocketAddr::new(a, 0)))
                        .ok()
                })
                .collect();
            if addrs.is_empty() {
                return Err(anyhow::anyhow!("net::ERR_NAME_NOT_RESOLVED: a plugin pinned {host} to {ips:?}, no address of which parsed"));
            }
            Ok(Some(resolved_client_for(wire, host, &addrs, insecure)?))
        }
    }
}

/// Build (or reuse) the `wreq::Client` for `wire`. With `insecure` (ABI 3, 4.5 `verify_cert`) certificate
/// verification is OFF and responses carry their `TlsInfo`, so [`insecure_client_for`]'s caller can read the leaf
/// certificate and let a plugin decide; otherwise the trust store's verdict stands as always. `clients` is the cache
/// map for the mode, so the two modes never collide.
fn build_or_cached_client(
    clients: &Mutex<HashMap<spec::TlsProfile, wreq::Client>>,
    wire: &spec::TlsProfile,
    insecure: bool,
    resolve: Option<(&str, &[std::net::SocketAddr])>,
) -> anyhow::Result<wreq::Client> {
    let key = wire.clone();
    // The lock is held across the build on purpose: two threads racing the same wire would otherwise
    // build two clients with two pools, and the loser's would be dropped with its connections open.
    // A DNS-pinned client (`resolve`, ABI 3 step 13 D4) is this request's own and is never cached or shared, so
    // it neither reads nor writes the table -- and takes no lock, leaving the cached path uncontended.
    let map = match resolve {
        None => Some(clients.lock().map_err(|_| anyhow::anyhow!("the transport's client table is poisoned"))?),
        Some(_) => None,
    };
    if let Some(c) = map.as_ref().and_then(|m| m.get(&key)) {
        return Ok(c.clone());
    }
    // GS.1: wreq + emulation -> the ClientHello (JA3/JA4) + HTTP/2 SETTINGS/frame fingerprint match a
    // real browser. WHICH browser is the persona bundle's declaration, not this file's opinion.
    let emulation = emulation_for(&wire.id)?;
    let platform = platform_for(&wire.platform);
    // The preset, resolved into its parts so four of its default headers can be dropped BEFORE the
    // client is built rather than removed after.
    //
    // The platform is a separate axis in wreq-util 0.2 and worth declaring: it steers the preset's own
    // `sec-ch-ua-platform` and neighbours to the persona's OS instead of a default that can disagree
    // with the UA in the same request.
    //
    // WHY FIVE HEADERS COME OUT. `sec-fetch-{dest,mode,site}` and `accept` are per-REQUEST facts, and
    // as client defaults they are merged into every request that does not carry its own -- so a
    // subresource inherited `dest: document, mode: navigate, site: none`, and a plain-http request
    // still carried Fetch Metadata after `fetch` deliberately withheld it (it is only ever sent to a
    // potentially-trustworthy URL). `fetch` computes all four per request, so the defaults have
    // nothing left to contribute but the wrong answer. This replaces a `Client::update().apply()`
    // that wreq 0.16 removed, and it is better: the headers are never installed at all.
    //
    // AND THE FIFTH IS `accept-language`, which is a fact about the DEVICE and belongs to the persona
    // (FP2). The preset's copy is `en-US,en;q=0.9` for every Chromium preset the library ships, so
    // every bundle — three of whose captures answer `navigator.languages` in Korean — was sending a
    // header that contradicted its own JS in the same page load. `persona_headers` states this one
    // now, from the capture; removing it here is what makes that the ONLY source, rather than a value
    // layered over a default that would still show through on any path that forgot to apply it.
    use wreq::IntoEmulation;
    let em = wreq_util::Emulation::builder()
        .profile(emulation)
        .platform(platform)
        .build()
        .into_emulation();
    let mut headers = em.headers;
    // The codings the preset advertises, for each request's own `Accept-Encoding` (`accept_encoding_for`, Chromium's
    // rule per URL). The client default stays behind it for a request that sets none.
    if let Some(ae) = headers.get("accept-encoding").and_then(|v| v.to_str().ok()) {
        if let Ok(mut m) = PRESET_ACCEPT_ENCODING.get_or_init(|| Mutex::new(HashMap::new())).lock() {
            m.insert(wire.clone(), ae.to_string());
        }
    }
    // ALSO the three navigation-only headers the preset carries as client DEFAULTS -- left in, they rode
    // EVERY request: `sec-fetch-user: ?1`, `upgrade-insecure-requests: 1` and `priority: u=0, i` on every
    // script, image, XHR and sensor POST (measured 2026-09-24 against Chrome 150 and Brave 151, which send
    // none of the three on a subresource). The request loop states each one for the requests that carry it.
    //
    // AND THE PRESET'S CLIENT HINTS: the persona states its own on every request it wears
    // ([`persona_headers`]), and a request that must carry none -- a dedicated worker's -- dropped the
    // persona's copy while the preset's default still went out.
    for h in [
        "sec-fetch-dest",
        "sec-fetch-mode",
        "sec-fetch-site",
        "accept",
        "accept-language",
        "sec-fetch-user",
        "upgrade-insecure-requests",
        "priority",
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
    ] {
        headers.remove(h);
    }
    // ONE MEASURED FIELD OF THE PRESET'S HTTP/2, CORRECTED BY THE PERSONA.
    //
    // `wreq-util`'s `chrome_*` and `opera_*` presets open a stream with
    // `StreamDependency::new(StreamId::zero(), 219, true)` -- wire byte 219, i.e. weight 220 -- and
    // real Chrome 150, Brave 151, Edge 151 and Opera 135 all send byte 255, weight 256. Measured off
    // a loopback TLS/h2 sink on 2026-09-18, all four, same page, and korvus measured at 220 under all
    // four bundles on the same sink. a bot sensor h2 fingerprint already matches; this field sits
    // beside it and was a constant on every connection this engine has ever opened.
    //
    // The bundle declares it, the engine never asks which browser it is wearing, and a bundle that
    // declares nothing keeps the preset's value rather than inheriting some other device's
    // measurement. `Http2Options` is `#[non_exhaustive]` with public fields, so the ONE field is
    // overwritten in place -- rebuilding the options from a builder would silently drop every other
    // field of the preset, which is the whole fingerprint.
    let mut http2 = em.http2_options;
    if let (Some(opts), Some(p)) = (http2.as_mut(), wire.h2_priority) {
        let weight = p.weight.clamp(1, 256) - 1;
        opts.headers_stream_dependency = Some(wreq::http2::StreamDependency::new(
            wreq::http2::StreamId::from(p.dep_stream),
            weight as u8,
            p.exclusive,
        ));
    }
    // TWO MEASURED FIELDS OF THE PRESET'S TLS, CORRECTED TO WHAT EVERY BROWSER ACTUALLY SENDS.
    // Both are constants of the Chromium family, not facts about one device, which is
    // why neither reads the bundle: this function is not allowed to know which browser it is wearing
    // and does not need to.
    //
    // (1) SIGNATURE ALGORITHMS. Chrome 150, Brave 151, Edge 151 and Opera 135 all lead
    //     `signature_algorithms` with 0x0904/0x0905/0x0906 (ML-DSA-44/65/87) and then the classical
    //     eight; no `wreq-util` preset does. Measured off a loopback TLS sink on 2026-09-18/19, all
    //     four browsers, and korvus measured without them under all four bundles on the same sink.
    //     JA4 hashes the sigalgs list, so this was the WHOLE of korvus's JA4 difference from its own
    //     persona's browser -- the first thing a connection is judged on, before a byte of JS runs.
    //
    //     The PRESET'S OWN TAIL IS KEPT, in the preset's own order, and the three codepoints are
    //     prepended to it. That is deliberate: the tail is the preset's measurement of its browser
    //     and this function has no better one. A preset that carries no `sigalgs_list` is left
    //     alone -- prepending onto a default we cannot see would be inventing a list, not
    //     correcting one.
    //
    //     WHY NAMES AND NOT CODEPOINTS: `wreq`'s only sigalgs spelling is a string routed to
    //     BoringSSL's `SSL_CTX_set1_sigalgs_list`, and `kSignatureAlgorithmNames` spells these three
    //     "mldsa44"/"mldsa65"/"mldsa87". The raw-codepoint route funnels into the same validator, so
    //     there is no second spelling to prefer. A BoringSSL that does not register them refuses
    //     the list and `Client::build` fails outright -- which is why this hunk and the workspace
    //     `[patch.crates-io]` rev pin are ONE change: the pin without this moves nothing (measured),
    //     and this without the pin breaks every fetch.
    //
    //     WHAT IT DOES TO A NON-CHROMIUM PRESET, said rather than hidden: this applies to whatever
    //     preset is worn, so `firefox_*` would gain the three codepoints too, and real Firefox does
    //     not lead with them. Measured, not guessed -- `firefox_133` on the sink moves to
    //     `t13i1715h2_5b57614c22b0_698a9c4632b2`. It is accepted because no bundle in this tree
    //     declares a Firefox preset and the alternative is a browser-family branch in engine code,
    //     which P2 exists to keep out. The right fix, when a non-Chromium persona exists, is for the
    //     BUNDLE to carry its browser's sigalgs as a captured fact (R19) and for this function to
    //     read it, the way `h2_priority` below is already read.
    //
    // (2) ALPS CODEPOINT. `0x4469` was the draft spelling of `application_settings`; `0x44cd`
    //     superseded it. Every browser on the sink sends `0x44cd`, real Opera 135 included -- its
    //     ClientHello is byte-for-byte Chrome's. `opera_131` alone still sends `0x4469`, and because
    //     JA4 hashes the extension list that was the Opera bundle's last wire difference: with the
    //     sigalgs closed it sat at `c8687cd82e98` where the browsers sit at `806a8c22fdea`.
    //     Setting it true is a no-op for `chrome_*`/`edge_*`, which already do.
    //
    // (3) TRUST ANCHORS -- see [`correct_preset_tls`], which applies all three, for this client and the WebSocket one.
    let mut tls_options = em.tls_options;
    if let Some(tls) = tls_options.as_mut() {
        correct_preset_tls(tls, wire)?;
    }
    // The connect path asks `ech` for each host's config (vendored wreq's one patch).
    wreq::tls::set_ech_config_provider(ech::cached);
    let mut b = wreq::Client::builder()
        .tls_options(tls_options)
        .http1_options(em.http1_options)
        .http2_options(http2)
        .default_headers(headers)
        .orig_headers(em.orig_headers)
        // NO CLIENT-WIDE COOKIE STORE: each request states the cookies it may send (`cookies::SendCookies`, per hop),
        // and one that states none sends and stores none -- the issuer fetch `chase_aia` makes, which Chrome's
        // `CertNetFetcherURLRequest` sends with `set_disallow_credentials()`.
        // TWO BOUNDED PHASES, so no single stalled connection can hang the engine indefinitely. There
        // was no timeout at all: a page fetch or one `<script src>` that never connected, or connected
        // and then went silent, blocked the whole load until the OS gave up (a ~30s connect wall) or,
        // on an open-but-idle socket, forever -- the corpus only survived it because it kills the
        // process at 60s, which yields an `errored` row with zero output instead of a rendered page.
        // Measured 2026-09-07: gnu.org fails TCP connect from this host (os error 110) and sat ~30s;
        // github.com intermittently stalled to the 60s kill. A browser bounds each load, so:
        //
        //   connect_timeout  DNS + the TCP address race + TLS. 15s is past any healthy handshake and
        //                    well inside the OS SYN wall, so an unreachable host fails fast as one
        //                    failed request, not a whole dead load.
        //   read_timeout     the READ phase only -- it fires when no byte arrives for the interval, and
        //                    every byte resets it, so a slow-but-progressing download is untouched while
        //                    a connection that goes silent is abandoned. NOT a total timeout, which
        //                    would wrongly abort a large legitimate response.
        //
        // A tripped timeout surfaces as a fetch Err: the navigation errors (caller decides), and a
        // subresource stages an empty body and the page renders what it has -- the join_all over
        // `<script src>` bodies no longer waits on the slowest dead socket.
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(30));

    // AN EXTRA ROOT CA, FOR A TEST SERVER ONLY (`$KORVUS_EXTRA_CA`, a PEM file).
    //
    // wptserve presents a certificate signed by WPT's own CA (`tools/certs/cacert.pem`), which nothing
    // trusts by default -- so every `.https.` test failed at the TLS handshake and produced no verdict
    // at all. Measured 2026-08-28: 372 of the 3,175 no-verdict files in the Core tier, failing for a
    // reason that has nothing to do with the engine.
    //
    // Trusting that CA is what the reference methodology does, not a departure from it: wptrunner
    // installs the same file into the browser profile it runs the suite with, so a real Chrome scoring
    // WPT trusts it too. Comparing ourselves against a reference engine's numbers while refusing the CA their
    // runner accepts would be measuring a different thing.
    //
    // It is opt-in and additive on purpose. The variable is set by the WPT harness and by nothing else,
    // so the client a real page gets is byte-for-byte the one it got before; and `set_default_paths()`
    // keeps the system roots, so this ADDS one CA rather than replacing the trust store with it.
    // What it is NOT is `cert_verification(false)` -- that would disable verification for every origin,
    // which is both a different (much larger) claim and one a server could detect.
    // The intermediates learned from AIA join the store (see [`chase_aia`]); with none
    // learned and no extra CA, the client keeps wreq's default store exactly as before.
    if let Some(store) = trust_store()? {
        b = b.tls_cert_store(store);
    }
    let extra_ca = std::env::var_os("KORVUS_EXTRA_CA");

    // HTTP/3 (H3, owner decision 2026-10-11): the persona's measured QUIC preset, when its bundle names one. Chrome
    // races QUIC beside TCP for an origin that advertised `h3` (Alt-Svc) or whose DNS `HTTPS` record lists it (the
    // record `ech` already fetches), and a challenge provider challenge records which transport carried it -- korvus's h2
    // tokens were 794 characters where Chrome's h3 ones are 730. The vendored wreq
    // speaks it (`vendor/wreq/src/http3`); a bundle without `quic` keeps its client exactly as it was.
    //
    // Two switches mirror Chrome's: `KORVUS_ORIGIN_TO_FORCE_QUIC_ON` is `--origin-to-force-quic-on` (comma-separated
    // `host:port`, QUIC as the only job), and with `KORVUS_EXTRA_CA` -- a root the system does not carry -- QUIC is
    // used for forced origins only, as Chrome's QUIC refuses a certificate that does not chain to a known root
    // (`ERR_QUIC_CERT_ROOT_NOT_KNOWN`, `net/quic/crypto/proof_verifier_chromium.cc:383-389`).
    if let Some(name) = wire.quic.as_deref() {
        let mut h3 = quic_presets::options_for(name)
            .ok_or_else(|| anyhow::anyhow!("the persona's QUIC preset {name:?} is not one this transport carries"))?;
        h3.force_quic_origins = std::env::var("KORVUS_ORIGIN_TO_FORCE_QUIC_ON")
            .map(|v| v.split(',').map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect())
            .unwrap_or_default();
        h3.only_forced_origins = extra_ca.is_some();
        wreq::http3::set_https_alpn_provider(ech::cached_alpn);
        b = b.http3_options(h3);
    }

    // Chrome's wire ORDER for the headers this engine emits. Without it the order is whatever the
    // request builder happened to insert in, which put the persona's `user-agent` ahead of `sec-ch-ua`
    // — backwards from every real Chrome, and visible to anything that reads the raw header list.
    // wreq 0.16 replaced `headers_order(Vec<HeaderName>)` with an `OrigHeaderMap`, which carries the
    // ORDER and the original CASING together -- a strictly better fit for this, since Chrome's header
    // case is as much a fingerprint as its order.
    let mut order = wreq::header::OrigHeaderMap::new();
    for n in CHROME_HEADER_ORDER {
        insert_spelled(&mut order, n, None);
    }
    // ABI 3 (wave 1, 4.5 `verify_cert`): the insecure variant trusts the trust store for nothing -- verification is
    // OFF -- and tags each response with its `TlsInfo`, so [`fetch`] can read the leaf certificate it just rode and
    // hand it to the plugins. Applied last, after the fingerprint is fully built, so the ClientHello is identical to
    // the verifying client's: the ONLY difference is whether BoringSSL's verdict is enforced.
    if insecure {
        b = b.tls_cert_verification(false).tls_info(true);
    }
    // ABI 3 (step 13, D4 `resolve`): a resolver plugin pinned this request's host to these addresses, so the
    // client answers that one name from them and resolves every other host normally. `resolve_to_addrs` keeps the
    // request's own port (the plugin named addresses, not ports: a `0` port is filled with the URL's). Only ever
    // set for a pinned, uncached client, so the shared pools never carry an override.
    if let Some((host, addrs)) = resolve {
        b = b.resolve_to_addrs(host.to_string(), addrs.iter().copied());
    }
    let c = b
        .orig_headers(order)
        // Redirects are followed BY US ([`fetch`]), one hop at a time, because `Sec-Fetch-Site` and
        // `Referer` are recomputed at every hop against the growing URL list — a client-internal
        // redirect would replay the first hop's headers for the whole chain and get both wrong.
        .redirect(wreq::redirect::Policy::none())
        .build()?;
    if let Some(mut m) = map {
        m.insert(key, c.clone());
    }
    Ok(c)
}

/// The three measured corrections every Chromium-family preset needs over its `wreq-util` TLS options (the comment
/// above in [`build_or_cached_client`] has the measurements): ML-DSA leading the preset's own `signature_algorithms`,
/// the current ALPS codepoint, and the bundle's recorded `trust_anchors` body.
///
/// ONE FUNCTION FOR EVERY CLIENT THAT OPENS A TLS CONNECTION FOR A PAGE, because each is a fact about the browser and
/// none about the request. The WebSocket client ([`websocket`]) once built its options straight from the preset and
/// so went out without the first and third -- a `wss://` ClientHello whose JA4 matched neither the page's own fetches
/// nor any Chrome, the sigalgs gap FP1 closed for fetches left open on the one path that copied the preset by hand.
pub(crate) fn correct_preset_tls(tls: &mut wreq::tls::TlsOptions, wire: &spec::TlsProfile) -> anyhow::Result<()> {
    if let Some(preset_tail) = tls.sigalgs_list.take() {
        tls.sigalgs_list = Some(format!("mldsa44:mldsa65:mldsa87:{preset_tail}").into());
    }
    tls.alps_use_new_codepoint = true;
    // GREASE IN `signature_algorithms`. Chrome 152's list leads with a GREASE value (`GREASE-2308-2309-2310-1027-...`,
    // tls.peet.ws's peetprint, a reference host-OS policy 152 measured 2026-10-22); korvus's started at 2308. JA4 drops GREASE, so the
    // two hashed alike while the raw ClientHello told them apart -- the one difference left on a request a device-check script
    // answered 429 for korvus and 200 for Chrome (expedia.com, 6 of 6 each way, same IP, same headers).
    tls.grease_sigalgs_enabled = Some(true);
    // TRUST ANCHORS. Chrome 152 added the `trust_anchors` extension (0xca34, draft-ietf-tls-trust-anchor-ids) to its
    // ClientHello, TCP and QUIC alike: 206 bytes, the same on every connection (measured 2026-10-16 off a loopback
    // sink; Chrome 150 sends none). No preset knows it, so the bundle records the body it was measured sending and
    // BoringSSL writes it (`SSL_set1_requested_trust_anchors`, vendored wreq's patch), which also moves the extension
    // count JA4 hashes. A bundle that recorded none sends none.
    if let Some(hex) = wire.trust_anchors.as_deref() {
        let body = decode_hex(hex).ok_or_else(|| anyhow::anyhow!("the bundle's trust_anchors is not hex"))?;
        anyhow::ensure!(
            body.len() >= 2 && usize::from(u16::from_be_bytes([body[0], body[1]])) == body.len() - 2,
            "the bundle's trust_anchors is not an extension body (a two-byte list length, then the IDs)"
        );
        tls.requested_trust_anchors = Some(body[2..].to_vec().into());
    }
    Ok(())
}

/// The trust store a page's TLS connections verify against, when it is not wreq's default: the webpki roots plus the
/// test-only `$KORVUS_EXTRA_CA` and the intermediates AIA has taught this process ([`chase_aia`]). `None` -- nothing
/// learned, no extra CA -- keeps wreq's default store exactly. Shared by the page client and the WebSocket client so a
/// host whose chain the page's fetches completed also verifies for its `wss://`.
fn trust_store() -> anyhow::Result<Option<wreq::tls::trust::CertStore>> {
    wreq::tls::set_issuer_miss_hook(record_issuer_miss);
    let learned = learned_intermediates();
    let extra_ca = std::env::var_os("KORVUS_EXTRA_CA");
    if extra_ca.is_none() && learned.is_empty() {
        return Ok(None);
    }
    // `add_pem_certs` takes an ITERATOR of certificates in 0.16, not a path, so the bundle is read here and handed
    // over as bytes.
    let ca = extra_ca.clone().unwrap_or_default();
    let pem = extra_ca.as_ref().map(|c| std::fs::read(c).unwrap_or_default()).unwrap_or_default();
    // the default store's own anchors (the webpki roots), never the build's default path; the AIA-learned
    // certificates only as intermediates (N41, security: a learned root was a trust anchor)
    let mut builder = wreq::tls::trust::CertStore::builder().add_webpki_roots();
    if extra_ca.is_some() {
        builder = builder.add_stack_pem_certs(pem.as_slice());
    }
    builder = builder.add_der_intermediates(learned.iter());
    match builder.build() {
        Ok(store) => Ok(Some(store)),
        // A bad path must not silently downgrade to "no extra CA": the run would then look like an engine failure on
        // 372 files again, with nothing saying why.
        Err(e) => Err(anyhow::anyhow!("KORVUS_EXTRA_CA {:?}: {e}", ca)),
    }
}

/// `document.cookie` as a top-level document at `url` reads it: the one store's cookies its script may read
/// ([`cookies::document_cookie`]; `HttpOnly` ones never). For the CLI's and the panel's reports; a page's own
/// `document.cookie` goes through [`cookies`] with its document's context.
pub fn document_cookie(url: &str) -> String {
    match cookies::ScriptContext::new(url, false, None) {
        Some(ctx) => cookies::document_cookie(&ctx, &mut cookies::CookieMonster::default()),
        None => String::new(),
    }
}

/// A top-level document at `url` writes `document.cookie = cookie` ([`cookies::set_document_cookie`]), Chrome's
/// rules deciding whether it is stored.
pub fn set_document_cookie(url: &str, cookie: &str) {
    if let Some(ctx) = cookies::ScriptContext::new(url, false, None) {
        cookies::set_document_cookie(&ctx, cookie, &mut cookies::CookieMonster::default());
    }
}

/// A response's `Set-Cookie`, for a document the engine was HANDED rather than fetched through this transport
/// (`Engine::load_page_delivered`): stored as the navigation that fetched it would have stored it.
pub fn store_response_cookie(url: &str, header: &str) {
    cookies::store_delivered_response(url, header);
}

/// Drop every cookie of the store. For test isolation.
pub fn reset_cookies() {
    cookies::clear();
}

/// Perform an HTTP request over the shared cookie-aware, Chrome-emulating client.
/// The transport **service** on the kernel `Context`: a swappable HTTP transport the `fetch` op calls
/// through, so a plugin can provide proxy-routing or an alternate transport. The returned future is not
/// required to be `Send` (deno_core async ops run single-threaded), so the trait object stays
/// `Send + Sync` — storable on the `Context` — while its futures need not be.
pub type FetchFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<FetchResponse>>>>;

/// A swappable HTTP transport. The default is [`DefaultTransport`] (the stealth `fetch` below).
/// Whether a loaded plugin reads or rewrites response bodies (`want_bodies` + `on_response`). When it does, a
/// page fetch is served whole rather than streamed, so the plugin sees and can rewrite the complete body before
/// the page reads it (plugin ABI 3, D6). Lets the runtime gate the streaming path without a plugin-host dep.
pub fn plugins_want_bodies() -> bool {
    plugin_host::wants_bodies()
}

pub trait TransportService: Send + Sync {
    fn fetch(&self, req: FetchRequest) -> FetchFuture;
}

/// The transport service handle placed on the kernel `Context` (mirrors `RenderHandle`/`PersonaHandle`).
pub struct TransportHandle(pub std::sync::Arc<dyn TransportService>);

/// The built-in stealth transport (BoringSSL/wreq impersonation) — the default provider.
pub struct DefaultTransport;
impl TransportService for DefaultTransport {
    fn fetch(&self, req: FetchRequest) -> FetchFuture {
        Box::pin(fetch(req))
    }
}

/// The default transport plugin: `provide`s a `TransportHandle(DefaultTransport)` on the `Context`.
/// Loading a different transport plugin (e.g. proxy-routing) swaps it without touching the fetch op.
pub struct TransportPlugin;
impl spec::kernel::Plugin for TransportPlugin {
    fn name(&self) -> &str {
        "transport"
    }
    fn provides(&self) -> &[&str] {
        &["transport"]
    }
    fn apply(&self, ctx: &spec::kernel::Context, _c: &spec::kernel::PluginConfig) -> spec::kernel::Result<()> {
        ctx.provide(TransportHandle(std::sync::Arc::new(DefaultTransport)));
        Ok(())
    }
}

/// Chrome's redirect ceiling. Chromium's `net::URLRequest` gives up after 20 hops
/// (`kMaxRedirects`), and a chain that long is already pathological.
const MAX_REDIRECTS: usize = 20;

/// The runtime every HTTP request actually runs on.
///
/// WHY THE HOP. `wreq`/hyper keep-alive works by spawning a background task per pooled CONNECTION,
/// on whatever runtime issued the request that opened it, and the response body cannot be read unless
/// that task keeps being polled. This engine has many runtimes: the page's (driven only inside short
/// `pump()` slices), one CURRENT-THREAD runtime per worker isolate, and whatever a caller happens to
/// be on. A connection opened on one and reused from another therefore depends on a runtime nobody is
/// driving, and the request simply never finishes.
///
/// Measured, on this tree, before this hop existed: a `new Worker(url)` whose script came from a
/// keep-alive server (a real `wpt serve`) never ran at all -- the worker's own boot fetch hung
/// forever, no error, no timeout -- while the identical worker served by a `Connection: close` server
/// ran fine, and so did one whose fetch happened to open the connection itself. That is the whole
/// reason `.any.worker.html` files reach no verdict.
///
/// Pinning every request to ONE always-running multi-threaded runtime removes the dependency: the
/// caller only awaits a channel, and the connection pool lives with a runtime that is always being
/// polled, whichever isolate asked.
fn net_runtime() -> &'static tokio::runtime::Runtime {
    static NET: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    NET.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("korvus-net")
            .enable_all()
            .build()
            .expect("build the transport runtime")
    })
}

// ---- the request ledger --------------------------------------------------------------
//
// WHAT IT IS FOR. The cheapest high-signal fidelity check the parity goal has is the RESOURCE-SET
// comparison: line up the URLs Chrome asked for against the URLs Korvus asked for. A script that
// silently failed, a lazy image that never triggered, a `@font-face` never fetched — each of them is
// invisible in a DOM diff and obvious in a request diff. That comparison needs one thing this engine
// did not have: a record of what the transport actually did.
//
// WHY IT IS HERE AND NOT IN `OpState`. The `__loadFired` migration put host state in
// `OpState` because the host knew the answer and the page should not. The same instinct applies, and
// the same destination does not: `OpState` is `!Send` and lives on the isolate thread, while every
// request is performed on [`net_runtime`] — a separate multi-threaded tokio runtime whose worker
// threads never see an `OpState` and cannot be handed one. The store therefore lives with the writer,
// in this crate, and `Engine::requests()` reads it through the re-export. That is the brief's own
// second branch ("otherwise in `plugins/korvus-transport` re-exported to the runtime"), chosen after
// testing the mechanism rather than assuming it.
//
// WHY IT IS A PROCESS GLOBAL. The other stores of this crate (`CLIENTS`, the cookie store in `cookies`)
// are process globals for the same reason: `fetch` is a free function with no engine to belong to.
// One process runs one page at a time here — `korvus fetch` is one navigation, and `cdp::serve` is
// documented as "one page per WS connection (sequential)" — so a global ledger is correct by
// construction today. [`ledger_reset`] is what keeps it honest across navigations: a navigation
// clears the ledger the way `reset_load_fired` clears the load flag, so a row always belongs to the
// navigation being measured.

/// One request the transport made, as recorded by [`fetch`].
///
/// The field set is the parity snapshot's `requests[]` (`url`, `type`, `method`, `status`,
/// `bytes`, `from_cache`, `ms`) plus the three the engine knows and the snapshot does not carry:
/// `id` (what the CDP `requestId` is minted from), `final_url` (after redirects) and `initiator`.
#[derive(Debug, Clone, Serialize)]
pub struct RequestRecord {
    /// Monotonic within a ledger generation, from 1. The CDP `requestId` is derived from it, which is
    /// what lets `requestWillBeSent` / `responseReceived` / `loadingFinished` be joined by a client.
    pub id: u64,
    /// The request's INITIAL url — the one the page asked for, which is the one a resource-set diff
    /// compares. A redirect chain is one row, as it is one REQUEST in Chrome's `Network.*` (one
    /// `requestId`); each redirect it followed is in [`RequestRecord::hops`].
    pub url: String,
    /// The url the response actually came from (`url` when nothing redirected).
    pub final_url: String,
    /// Fetch's request destination, as the `Sec-Fetch-Dest` token: `document`, `script`, `style`,
    /// `image`, `font`, `iframe`, `empty`, … . `empty` is a page `fetch()`/XHR.
    #[serde(rename = "type")]
    pub kind: String,
    pub method: String,
    /// `None` when the request never produced a response (DNS failure, TLS failure, timeout).
    pub status: Option<u16>,
    /// The final response's status text as Chromium reads it ([`status_text_of`]): an HTTP/1.x reason phrase as
    /// the server wrote it, none over HTTP/2 and HTTP/3. `Network.Response.statusText`.
    pub status_text: String,
    /// The redirects the request followed, in order (N27: the chain was one row with the FINAL response's headers
    /// only, so a client never saw a redirect response -- which hid a pass for a whole investigation). Chrome
    /// reports each as the `redirectResponse` of the next hop's `Network.requestWillBeSent`, same `requestId`.
    pub hops: Vec<RedirectHop>,
    /// Body bytes, AFTER content decoding (gzip/br/zstd removed) — the length of
    /// `FetchResponse::bytes`, so a binary body counts its real bytes rather than the UTF-8 length of
    /// its lossy text view. There is no encoded length to report and inventing one would be a fake. Chrome's
    /// `encodedDataLength` is the compressed count; the two are not comparable and the snapshot
    /// schema does not ask them to be (`bytes (body)`).
    pub bytes: usize,
    /// The answer was served from the HTTP cache as stored (`http_cache`; DevTools' "from disk cache"), not
    /// fetched; a stored answer a `304` confirmed is a network row with status 304.
    pub from_cache: bool,
    /// Milliseconds ON THE WIRE: the time the request spent inside [`fetch_on_net_runtime`],
    /// measured on the net runtime itself.
    ///
    /// NOT the time the caller waited, and the difference is not academic. [`fetch`] hands the work
    /// to [`net_runtime`] and awaits a oneshot; a caller whose own runtime is only polled in slices
    /// (the page's event loop is pumped in `pump()` windows) does not observe the answer until its
    /// next slice. Measuring entry-to-return therefore charged the NETWORK for the ENGINE's
    /// scheduling. Measured on the local fixture before this was moved: one request per run reported
    /// ~1030 ms against a loopback server answering in ~2 ms, and which request it landed on moved
    /// from run to run. Chrome's `Network.loadingFinished - requestWillBeSent` is wire time, so a
    /// ledger that reported queueing time here could not be compared with it at all — which is the
    /// one thing this field exists for.
    ///
    /// The engine's own pacing is not lost; it is reported where it belongs, as §6's `load_ms`.
    pub ms: f64,
    /// The URL of the environment that started this request; `None` for a top-level navigation.
    pub initiator: Option<String>,
    /// The transport error, when `status` is `None`.
    pub error: Option<String>,
    /// The response's headers, as received. Recorded because `Network.responseReceived` carries them
    /// and this engine HAS them: emitting `{}` there would be a fixed value standing in for data that
    /// exists, which the unit contract calls stubbing. Empty when the request failed.
    pub headers: Vec<(String, String)>,
    /// Milliseconds from the ledger generation's epoch to the moment this request started. What the
    /// driver's `settle_ms` (500 ms with no new request) is computed from.
    pub started_ms: f64,
    /// Why the request was never sent, when the engine refused it (`"mixed-content"`). Chrome reports
    /// such a request as `requestWillBeSent` + `loadingFailed { errorText: "", blockedReason }`.
    pub blocked_reason: Option<String>,
    /// The target that asked for this request and the one its response is handed to (see [`Reporter`]).
    pub reporter: Reporter,
    /// The RFC 9218 `priority` field the request carries over HTTP/2 -- Chromium writes it on h2 only --
    /// whatever protocol it actually went out on; `None` is no field (urgency 3, not incremental). The
    /// load priority Blink gave the request, in the one spelling that reaches the wire;
    /// `Network.requestWillBeSent`'s `initialPriority` is read from it.
    pub priority: Option<String>,
    /// The request was sent with a body (`Network.Request.hasPostData`); the bytes, while kept, are [`request_body`]'s.
    /// Not in the `--requests` line, which predates it.
    #[serde(skip)]
    pub has_post_data: bool,
}

/// One redirect a request followed: the hop that got a redirect response, and the hop it led to.
///
/// Chrome 150 over loopback (three runs alike): a navigation `302 -> 301 -> 200`
/// and a `fetch()` `307 -> 200` are each ONE `requestId` with one `Network.requestWillBeSent` per hop; every hop
/// after the first carries `redirectResponse` -- the previous hop's url, status, status text and headers -- and
/// names its own url and method. Only the final response gets `Network.responseReceived`.
#[derive(Debug, Clone, Serialize)]
pub struct RedirectHop {
    /// The url the hop requested, which answered with the redirect.
    pub url: String,
    /// The redirect response's status, status text ([`status_text_of`]) and headers, as received.
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// Where it led -- after an upgrade to `https:` where mixed content upgrades it -- and with which method (a
    /// `303`, and a `301`/`302` of anything but GET/HEAD, continue as GET).
    pub next_url: String,
    pub next_method: String,
    /// Milliseconds from the ledger generation's epoch to the redirect response's arrival, when the next hop starts.
    pub at_ms: f64,
}

/// The error a request fails with while a DevTools client has the page offline.
pub const INTERNET_DISCONNECTED: &str = "net::ERR_INTERNET_DISCONNECTED";

/// Chrome's `net::ErrorToString` name for a failed request, read off the transport's error: a `net::ERR_*` the
/// transport already named (a refused certificate, a blocked or offline request), else what the connection's failure
/// was (refused, reset, a name that did not resolve, a TLS failure), else `net::ERR_FAILED`.
pub fn net_error_name(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    if let Some(i) = text.find("net::ERR_") {
        let name: String = text[i..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':' || *c == '.').collect();
        return name.trim_end_matches(':').to_string();
    }
    if text.contains("CERTIFICATE_VERIFY_FAILED") || text.to_ascii_lowercase().contains("certificate") {
        return "net::ERR_CERT_AUTHORITY_INVALID".into();
    }
    for cause in e.chain() {
        if let Some(w) = cause.downcast_ref::<wreq::Error>() {
            return websocket::net_error_name(w).to_string();
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            match io.kind() {
                ConnectionRefused => return "net::ERR_CONNECTION_REFUSED".into(),
                ConnectionReset => return "net::ERR_CONNECTION_RESET".into(),
                TimedOut => return "net::ERR_CONNECTION_TIMED_OUT".into(),
                _ => {}
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("dns error") || lower.contains("failed to lookup") {
        return "net::ERR_NAME_NOT_RESOLVED".into();
    }
    if lower.contains("connection refused") {
        return "net::ERR_CONNECTION_REFUSED".into();
    }
    "net::ERR_FAILED".into()
}

/// The bodies a DevTools client may still ask for, by ledger row -- the responses' (`Network.getResponseBody`) and the
/// requests' (`Network.requestWillBeSent`'s `postData`, `Network.getRequestPostData`), one store each: kept only while
/// a client has the `Network` domain on ([`keep_bodies`]), the oldest dropped past [`BODY_CAP`] bytes.
#[derive(Default)]
struct Bodies {
    rows: std::collections::VecDeque<(u64, std::sync::Arc<Vec<u8>>)>,
    total: usize,
}

impl Bodies {
    fn keep(&mut self, id: u64, bytes: &[u8]) {
        self.rows.retain(|(r, _)| *r != id);
        self.total += bytes.len();
        self.rows.push_back((id, std::sync::Arc::new(bytes.to_vec())));
        while self.total > BODY_CAP {
            match self.rows.pop_front() {
                Some((_, old)) => self.total -= old.len(),
                None => break,
            }
        }
    }

    fn get(&self, id: u64) -> Option<std::sync::Arc<Vec<u8>>> {
        self.rows.iter().find(|(r, _)| *r == id).map(|(_, b)| b.clone())
    }
}

/// How many body bytes each store keeps at most.
const BODY_CAP: usize = 64 * 1024 * 1024;

static KEEP_BODIES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn bodies() -> &'static Mutex<Bodies> {
    static B: OnceLock<Mutex<Bodies>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn request_bodies() -> &'static Mutex<Bodies> {
    static B: OnceLock<Mutex<Bodies>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// Keep each request's and response's body for a DevTools client (it enabled `Network`), or stop and drop them.
pub fn keep_bodies(on: bool) {
    KEEP_BODIES.store(on, std::sync::atomic::Ordering::Relaxed);
    if !on {
        *bodies().lock().unwrap_or_else(|e| e.into_inner()) = Bodies::default();
        *request_bodies().lock().unwrap_or_else(|e| e.into_inner()) = Bodies::default();
    }
}

fn store_body(store: &Mutex<Bodies>, id: u64, bytes: &[u8]) {
    if !KEEP_BODIES.load(std::sync::atomic::Ordering::Relaxed) || bytes.len() > BODY_CAP {
        return;
    }
    store.lock().unwrap_or_else(|e| e.into_inner()).keep(id, bytes);
}

/// The body ledger row `id`'s response came with, when it is still kept.
pub fn response_body(id: u64) -> Option<std::sync::Arc<Vec<u8>>> {
    bodies().lock().unwrap_or_else(|e| e.into_inner()).get(id)
}

/// The body ledger row `id`'s request was sent with (the page's `fetch`/XHR body, a form's), when it is still kept:
/// what a client that records what a page signed reads back (`postData`, `Network.getRequestPostData`). A request
/// with a body says so in [`RequestRecord::has_post_data`] whether or not it is kept.
pub fn request_body(id: u64) -> Option<std::sync::Arc<Vec<u8>>> {
    request_bodies().lock().unwrap_or_else(|e| e.into_inner()).get(id)
}

/// The transport's record of every request it performed since the last [`ledger_reset`].
pub struct RequestLedger {
    inner: Mutex<LedgerInner>,
}

struct LedgerInner {
    epoch: std::time::Instant,
    next_id: u64,
    rows: Vec<RequestRecord>,
    /// The worker targets started in this generation, in the order they were started.
    targets: Vec<ChildTarget>,
    /// Bumped by every change to `rows` or `targets`, so a reader that only needs to know whether there
    /// is anything new (the CDP server's idle flush) does not clone the ledger to find out.
    revision: u64,
    /// Rows dropped because the ledger hit [`LEDGER_CAP`]. A number, never a silent truncation: a
    /// resource-set diff that is missing rows must say so rather than read as "Korvus asked for less".
    dropped: u64,
}

/// How many rows one generation keeps. A real page is tens to a few hundred requests; a page with a
/// perpetual polling loop driven for minutes is unbounded, and an unbounded `Vec` in a long-lived
/// `korvus serve` is a leak.
const LEDGER_CAP: usize = 8192;

impl RequestLedger {
    /// Open a row for `req` and return its id. Called before the request is spawned, so the row
    /// exists even if the request never comes back, and `started_ms` is the moment the CALLER asked
    /// (which is what Chrome's `requestWillBeSent.timestamp` is too).
    fn begin(&self, req: &FetchRequest) -> u64 {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        st.next_id += 1;
        let id = st.next_id;
        let started_ms = st.epoch.elapsed().as_secs_f64() * 1000.0;
        if st.rows.len() >= LEDGER_CAP {
            st.dropped += 1;
        } else {
            st.rows.push(RequestRecord {
                id,
                url: req.url.clone(),
                final_url: req.url.clone(),
                kind: req.destination.token().to_string(),
                method: req.method.clone(),
                status: None,
                status_text: String::new(),
                hops: Vec::new(),
                bytes: 0,
                from_cache: false,
                ms: 0.0,
                initiator: req.initiator.clone(),
                error: None,
                headers: Vec::new(),
                started_ms,
                blocked_reason: None,
                reporter: req.reporter.clone(),
                priority: chromium_priority(req),
                has_post_data: req.body.as_ref().is_some_and(|b| !b.is_empty()),
            });
        }
        if let Some(body) = req.body.as_deref().filter(|b| !b.is_empty()) {
            store_body(request_bodies(), id, body);
        }
        id
    }

    /// Close the row `id` for a request the engine refused to send.
    fn block(&self, id: u64, reason: &str) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == id) {
            r.blocked_reason = Some(reason.to_string());
            r.error = Some(String::new());
        }
    }

    /// Close the row `id` with what the request produced, and the wire time it took.
    fn finish(&self, id: u64, ms: f64, out: &anyhow::Result<FetchResponse>) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        let Some(r) = st.rows.iter_mut().find(|r| r.id == id) else { return };
        r.ms = ms;
        match out {
            Ok(resp) => {
                store_body(bodies(), id, &resp.bytes);
                // a 304 confirmed a stored answer: the wire said 304 (DevTools does), the page reads the stored one
                r.status = Some(if resp.timing.cache == "validated" { 304 } else { resp.status });
                r.status_text = resp.status_text.clone();
                r.from_cache = resp.timing.cache == "local";
                r.bytes = resp.bytes.len();
                r.final_url = resp.url.clone();
                r.headers = resp.headers.clone();
            }
            Err(e) => {
                let msg = e.to_string();
                // A request refused at a REDIRECT hop was sent, and then its next hop was not: the
                // row carries the reason the chain stopped, exactly as the pre-send refusal does.
                if msg.starts_with(MIXED_CONTENT_BLOCKED) {
                    r.blocked_reason = Some(MIXED_CONTENT_BLOCKED.to_string());
                    r.error = Some(String::new());
                } else {
                    r.error = Some(msg);
                }
            }
        }
    }

    /// Record a redirect row `id` followed (see [`RedirectHop`]); `hop.at_ms` is stamped here, on the ledger's clock.
    fn redirect(&self, id: u64, mut hop: RedirectHop) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        hop.at_ms = st.epoch.elapsed().as_secs_f64() * 1000.0;
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == id) {
            r.hops.push(hop);
        }
    }

    /// Close the row `id` for a request whose task never ran to completion (the runtime dropped it).
    /// The row stays, with the reason — a hole in the ledger would read as a request never made.
    fn abandon(&self, id: u64) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == id) {
            if r.status.is_none() && r.error.is_none() {
                r.error = Some("transport task dropped".to_string());
            }
        }
    }

    /// When each of the PAGE's requests started and ended (`None` while in flight), on the `Instant` clock --
    /// what `engine`'s replay of Blink's `IdlenessDetector` reads (`core/korvus-runtime/src/idleness.rs`). A worker's
    /// requests (`Reporter::target` set) are its own, as in Chrome, and a refused request was never in flight. Light:
    /// only the times are copied, never a row, because it is read on every 50 ms slice of a `--wait-until` wait.
    pub fn page_spans(&self) -> Vec<(std::time::Instant, Option<std::time::Instant>)> {
        let st = self.inner.lock().unwrap();
        let at = |ms: f64| st.epoch + std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0);
        st.rows
            .iter()
            .filter(|r| r.reporter.target.is_none() && r.blocked_reason.is_none())
            .map(|r| {
                let start = at(r.started_ms);
                let ended = r.status.is_some() || r.error.is_some();
                (start, ended.then(|| at(r.started_ms + r.ms)))
            })
            .collect()
    }

    /// Every row of the current generation, in the order the requests STARTED.
    pub fn records(&self) -> Vec<RequestRecord> {
        self.inner.lock().unwrap().rows.clone()
    }

    /// Close row `id` as failed with `error` after all -- a navigation response that became a download, which Chrome
    /// reports as the navigation request's `net::ERR_ABORTED`.
    pub fn mark_failed(&self, id: u64, error: &str) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == id) {
            r.status = None;
            r.error = Some(error.to_string());
        }
    }

    /// The row `id` of the current generation.
    pub fn record(&self, id: u64) -> Option<RequestRecord> {
        self.inner.lock().unwrap().rows.iter().find(|r| r.id == id).cloned()
    }

    /// Rows lost to [`LEDGER_CAP`] in this generation.
    pub fn dropped(&self) -> u64 {
        self.inner.lock().unwrap().dropped
    }

    /// Record that a worker target exists. Called by the realm that starts it, BEFORE the worker issues
    /// any request, so a client can be told about the target before it is told about its requests.
    pub fn announce_target(&self, target: ChildTarget) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        if !st.targets.iter().any(|t| t.id == target.id) {
            st.targets.push(target);
        }
    }

    /// A number that changes whenever a row or a target is added or closed. Never repeats in a process.
    pub fn revision(&self) -> u64 {
        self.inner.lock().unwrap().revision
    }

    /// Something a reporter flushes at the ledger's revisions happened outside it (a WebSocket's event,
    /// [`websocket::devtools_event`]): the revision moves, so the next flush looks.
    pub fn touch(&self) {
        self.inner.lock().unwrap().revision += 1;
    }

    /// Every worker target of the current generation, in start order.
    pub fn targets(&self) -> Vec<ChildTarget> {
        self.inner.lock().unwrap().targets.clone()
    }

    /// Start a new generation: drop every row and restart the clock `started_ms` is measured from.
    pub fn reset(&self) {
        let mut st = self.inner.lock().unwrap();
        st.revision += 1;
        st.epoch = std::time::Instant::now();
        st.next_id = 0;
        st.rows.clear();
        st.targets.clear();
        st.dropped = 0;
    }
}

static LEDGER: OnceLock<RequestLedger> = OnceLock::new();

/// The process's request ledger.
pub fn ledger() -> &'static RequestLedger {
    LEDGER.get_or_init(|| RequestLedger {
        inner: Mutex::new(LedgerInner {
            epoch: std::time::Instant::now(),
            next_id: 0,
            rows: Vec::new(),
            targets: Vec::new(),
            revision: 0,
            dropped: 0,
        }),
    })
}

/// Begin a new ledger generation. Called at the start of a navigation, so the rows a caller reads are
/// the rows of the navigation it is measuring — the same discipline `reset_load_fired` applies to the
/// load flag.
pub fn ledger_reset() {
    ledger().reset();
}

/// A proxy URL as Chrome means it: `socks5://` resolves names at the proxy (Chrome's SOCKS5 always does), which wreq
/// spells `socks5h://`. A login in the URL (`socks5://user:pass@host:1080`, `http://user:pass@host:3128`) is sent
/// to the proxy as its scheme sends one.
pub fn proxy_url(url: &str) -> String {
    match url.strip_prefix("socks5://") {
        Some(rest) => format!("socks5h://{rest}"),
        None => url.to_string(),
    }
}

/// One HTTP request korvus makes for itself, not for a page -- an IP echo through a context's proxy (goal 1020 step
/// 4): `method` to `url` with `headers` and `body` (its content type and bytes), through `proxy` when given, its
/// status and body. Blocking, on a thread of its own, at most
/// `within`, so a caller in any runtime (or none) can make it.
pub fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, Vec<u8>)>,
    proxy: Option<&str>,
    within: std::time::Duration,
) -> Result<(u16, Vec<u8>), String> {
    let (method, url, asked_of) = (method.to_string(), url.to_string(), url.to_string());
    let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let body = body.map(|(t, b)| (t.to_string(), b));
    let proxy = proxy.map(proxy_url);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let asked = async {
            let mut builder = wreq::Client::builder();
            if let Some(p) = &proxy {
                builder = builder.proxy(wreq::Proxy::all(p.as_str()).map_err(|e| format!("the proxy {p}: {e}"))?);
            }
            let client = builder.build().map_err(|e| e.to_string())?;
            let method = wreq::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
            let mut req = client.request(method, &url);
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            if let Some((content_type, bytes)) = body {
                req = req.header("content-type", content_type).body(bytes);
            }
            let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
            Ok((status, bytes.to_vec()))
        };
        let r = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt.block_on(async { tokio::time::timeout(within, asked).await.unwrap_or_else(|_| Err(format!("{url}: no answer"))) }),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(r);
    });
    rx.recv_timeout(within + std::time::Duration::from_secs(1)).map_err(|_| format!("{asked_of}: no answer"))?
}

/// Where `proxy` exits: the address an IP echo (`echo`, which answers a request with the address it came from, e.g.
/// `https://api.ipify.org`) sees a request through it come from. Blocking, at most `within`.
pub fn exit_ip(proxy: &str, echo: &str, within: std::time::Duration) -> Result<std::net::IpAddr, String> {
    let (status, body) = http_request("GET", echo, &[], None, Some(proxy), within).map_err(|e| format!("{e} (through the proxy)"))?;
    let body = String::from_utf8_lossy(&body);
    if status != 200 {
        return Err(format!("{echo}: answered {status} through the proxy"));
    }
    body.trim().parse::<std::net::IpAddr>().map_err(|_| format!("{echo}: not an IP ({})", body.trim()))
}

/// Perform `req`, on [`net_runtime`]. The caller awaits only a oneshot channel, so it does not matter
/// which runtime the caller is on or whether that runtime is being driven between polls.
///
/// THIS IS THE ONE ENTRY EVERY REQUEST PASSES THROUGH, so it is where the [request ledger](ledger) is
/// written: one row opened here before the task is spawned, closed here when the response (or the
/// error) comes back. Recording at this level — rather than at each of the four call sites that
/// build a `FetchRequest` — is what makes the ledger *the transport's own record* instead of a tally
/// somebody remembered to keep, and it is why the `ms` it reports is the whole wall-clock cost the
/// caller paid, oneshot hop included.
/// The HTTP credentials a DevTools client gave per origin (Chrome's HTTP auth cache): what a later request to the origin
/// sends up front.
static AUTH_CACHE: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn remember_auth(url: &url::Url, header: &str) {
    let origin = url.origin().ascii_serialization();
    let mut c = AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|(o, _)| *o != origin);
    c.push((origin, header.to_string()));
}

fn cached_auth(url: &url::Url) -> Option<String> {
    let origin = url.origin().ascii_serialization();
    AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(o, _)| *o == origin).map(|(_, h)| h.clone())
}

pub async fn fetch(mut req: FetchRequest) -> anyhow::Result<FetchResponse> {
    // MIXED CONTENT, before the row is opened: an upgraded request is recorded under the url it is
    // actually sent to (Chrome's `requestWillBeSent` names the https url), and a blocked one gets a row
    // that says so and never reaches the network.
    let initiator = req.initiator.as_deref().and_then(|s| url::Url::parse(s).ok());
    let mut blocked = false;
    if let Ok(target) = url::Url::parse(&req.url) {
        match mixed_content(initiator.as_ref(), &target, &req.destination, req.mode) {
            MixedContent::Allowed => {}
            MixedContent::Upgrade(u) => req.url = u.to_string(),
            MixedContent::Blocked => blocked = true,
        }
    }
    // Opened HERE, so `started_ms` is the moment the caller asked; CLOSED on the net runtime, so
    // `ms` is time on the wire and not time the caller spent un-polled. See [`RequestRecord::ms`].
    let id = ledger().begin(&req);
    let top_navigation = req.mode == Mode::Navigate && req.destination.token() == "document";
    // A DevTools client took the page offline (`Network.emulateNetworkConditions`): nothing reaches the network, and
    // the request fails as Chrome's does with no connection.
    if spec::devtools::offline() {
        let out: anyhow::Result<FetchResponse> = Err(anyhow::anyhow!("{INTERNET_DISCONNECTED}"));
        ledger().finish(id, 0.0, &out);
        return out;
    }
    if blocked {
        ledger().block(id, MIXED_CONTENT_BLOCKED);
        return Err(anyhow::anyhow!("{MIXED_CONTENT_BLOCKED}: {} was not sent from a secure page", req.url));
    }
    // `--block=…`: a request a client rule drops never reaches the network, and the page sees exactly what DevTools'
    // `Network.setBlockedURLs` makes it see -- a `loadingFailed` with `blockedReason: inspector`.
    if let Some(rules) = block_rules() {
        if rules.blocks(&req.url, &req.destination, top_navigation) {
            ledger().block(id, RULE_BLOCKED);
            return Err(anyhow::anyhow!("net::ERR_BLOCKED_BY_CLIENT: {} was dropped by a --block rule", req.url));
        }
    }
    if plugin_host::has_observer() {
        let ev = serde_json::json!({ "url": req.url, "method": req.method, "destination": req.destination.token() });
        plugin_host::event("request", &ev.to_string());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    // the start checks, then fetchStart (Chrome's order: the throttles run before `fetch_start` is marked)
    let checks = start_checks(&req);
    let asked = std::time::Instant::now();
    net_runtime().spawn(async move {
        let started = std::time::Instant::now();
        let out = fetch_on_net_runtime(req, checks, asked, id).await;
        ledger().finish(id, started.elapsed().as_secs_f64() * 1000.0, &out);
        if let (true, Ok(r)) = (top_navigation, &out) {
            note_navigation_timing(r);
        }
        let _ = tx.send(out);
    });
    match rx.await {
        Ok(mut inner) => {
            if let (true, Ok(r)) = (plugin_host::has_network(), &mut inner) {
                plugin_net::answered(r);
            }
            if plugin_host::has_observer() {
                let ev = match &inner {
                    Ok(r) => serde_json::json!({ "url": r.url, "status": r.status }),
                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                };
                plugin_host::event("response", &ev.to_string());
            }
            inner
        }
        // A dropped task is a failed request, not a silent hole in the ledger: the row is closed
        // with the reason either way, which is the difference between "Korvus did not fetch it" and
        // "Korvus tried and failed" when the two ledgers are diffed.
        Err(_) => {
            ledger().abandon(id);
            Err(anyhow::anyhow!("transport task dropped"))
        }
    }
}

/// The timing of the NAVIGATION responses not yet read, keyed by the URL each came from: the
/// engine reads one when it installs that document, for `PerformanceNavigationTiming` and `timeOrigin`,
/// whichever driver fetched the document (`korvus fetch`, the CDP server, a page's own navigation).
///
/// SEVERAL, not the newest one: one process runs a page and its cross-site frames' child engines on their own
/// threads (OOPIF), and any navigation between a document's fetch and its install replaced the one slot this was --
/// the document then had no navigation entry phases at all (its `performance.timing` network fields 0; caught by
/// `performance_timing_live` beside the other engines of the test binary, 2026-10-11).
static NAVIGATION_TIMING: OnceLock<Mutex<Vec<(String, spec::FetchTiming)>>> = OnceLock::new();

/// How many unread navigation timings are kept; the oldest goes first.
const NAVIGATION_TIMINGS_KEPT: usize = 32;

fn note_navigation_timing(r: &FetchResponse) {
    let final_url = r.timing.url_list.last().cloned().unwrap_or_else(|| r.url.clone());
    if let Ok(mut g) = NAVIGATION_TIMING.get_or_init(|| Mutex::new(Vec::new())).lock() {
        if g.len() >= NAVIGATION_TIMINGS_KEPT {
            g.remove(0);
        }
        g.push((final_url, r.timing.clone()));
    }
}

/// The timing of the newest navigation that delivered `url`, taken (a document reads it once). `None` when no
/// navigation response for that URL is waiting.
pub fn take_navigation_timing(url: &str) -> Option<spec::FetchTiming> {
    let strip = |u: &str| u.split('#').next().unwrap_or(u).to_string();
    let mut g = NAVIGATION_TIMING.get_or_init(|| Mutex::new(Vec::new())).lock().ok()?;
    let want = strip(url);
    let i = g.iter().rposition(|(u, _)| strip(u) == want)?;
    Some(g.remove(i).1)
}

/// The persona-owned headers of `req`, for a request the engine issues on the page's behalf (a
/// preflight): the same identity as the request it precedes, and nothing the page wrote.
///
/// `accept-language` rides along even though it is not in [`PERSONA_OWNED_HEADERS`] (a page may
/// state its own on a `fetch()`, so the persona does not OWN it) -- because the preflight is a
/// request the browser makes, and a browser puts its language preference on it. Measured
/// 2026-09-18: all four installed Chromium-family browsers send `Accept-Language` on the OPTIONS.
/// Before FP2 it arrived from the emulation preset's client defaults; those are gone, so it is
/// carried explicitly or the preflight goes out without it.
///
/// `sec-gpc` does NOT ride along, and that is measured rather than reasoned: on the same run, real
/// Brave 151 put `Sec-GPC: 1` on the navigation and on all nineteen subresources and on NONE of the
/// preflights. It is the only installed browser that sends the header at all, so it is the only
/// evidence there is; korvus was sending it on the OPTIONS.
///
/// Nor do the client hints: Chromium copies the request's `User-Agent` onto the preflight and nothing else of
/// its identity ("TODO: do the same for client hints", `preflight_controller.cc`). Chrome 150 sent none on the
/// OPTIONS; korvus sent the three.
fn identity_headers(req: &FetchRequest) -> Vec<(String, String)> {
    req.headers
        .iter()
        .filter(|(k, _)| {
            (PERSONA_OWNED_HEADERS.iter().any(|p| k.eq_ignore_ascii_case(p))
                || k.eq_ignore_ascii_case("accept-language"))
                && !k.eq_ignore_ascii_case("sec-gpc")
                && !is_client_hint(k)
        })
        .cloned()
        .collect()
}

/// A CORS-preflight for `req` to `url`, answered from the cache or the network (see `cors.rs`).
async fn preflight(client: &wreq::Client, req: &FetchRequest, page: &PageRequest, url: &url::Url, initiator: Option<&url::Url>) -> anyhow::Result<()> {
    let unsafe_headers = cors::unsafe_header_names(&page.author_headers);
    if cors::cached_preflight_allows(&page.origin, url.as_str(), req.credentials, &req.method, &unsafe_headers) {
        return Ok(());
    }
    // The preflight is a request of its own in the ledger, as in Chrome's (measured: `Network.*` reports
    // it with type `Preflight`, and the fixture server's access log has the OPTIONS line).
    let row = FetchRequest {
        url: url.to_string(),
        method: "OPTIONS".to_string(),
        destination: Destination::from(PREFLIGHT_KIND),
        mode: Mode::Cors,
        initiator: req.initiator.clone(),
        reporter: req.reporter.clone(),
        // the request's own priority (`preflight_request->priority = request.priority`)
        priority: Some(chromium_priority(req).unwrap_or_default()),
        ..Default::default()
    };
    let id = ledger().begin(&row);
    let started = std::time::Instant::now();
    let out = preflight_on_wire(client, req, page, url, initiator, &unsafe_headers, id).await;
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    ledger().finish(id, ms, &out.as_ref().map(|(r, _)| r.clone()).map_err(|e| anyhow::anyhow!("{e}")));
    let (resp, grant) = out?;
    match grant {
        Some(grant) => {
            cors::cache_preflight(&page.origin, url.as_str(), grant);
            Ok(())
        }
        None => Err(anyhow::anyhow!("CORS: the preflight for {} {url} was not allowed (status {})", req.method, resp.status)),
    }
}

/// The ledger `type` a CORS preflight is recorded under (`Network.ResourceType` `Preflight`). Not a
/// `Sec-Fetch-Dest` token: a preflight's is `empty`.
pub const PREFLIGHT_KIND: &str = "preflight";

async fn preflight_on_wire(
    client: &wreq::Client,
    req: &FetchRequest,
    page: &PageRequest,
    url: &url::Url,
    initiator: Option<&url::Url>,
    unsafe_headers: &[String],
    row: u64,
) -> anyhow::Result<(FetchResponse, Option<cors::PreflightGrant>)> {
    let mut rb = client.request(wreq::Method::OPTIONS, url.as_str());
    // A preflight never carries credentials (Fetch "CORS-preflight fetch": its response tainting is `cors`)...
    rb = rb.cookie_provider(Arc::new(NoCredentials));
    // ... and so rides a connection of the no-credentials group, under the request's own key (`partition.rs`).
    let top_level = req.top_level_url.as_deref().and_then(|u| url::Url::parse(u).ok());
    rb = rb.group(wreq::Group::new(partition::preflight_group(req, url, top_level.as_ref(), initiator)));
    rb = rb.header("accept", "*/*");
    // `Connection: keep-alive` second, as on every request (Chrome 150 HTTP/1.1; korvus's
    // preflight had none)
    rb = rb.header("connection", "keep-alive");
    rb = rb.header("access-control-request-method", req.method.as_str());
    if !unsafe_headers.is_empty() {
        rb = rb.header("access-control-request-headers", unsafe_headers.join(","));
    }
    rb = rb.header("origin", page.origin.as_str());
    if is_potentially_trustworthy(url) {
        // an opaque origin (a sandboxed frame's) is same-origin with nothing (Chrome 150: cross-site)
        let site = if req.opaque_origin { "cross-site" } else { sec_fetch_site(initiator, std::slice::from_ref(url)) };
        rb = rb.header("sec-fetch-site", site);
        rb = rb.header("sec-fetch-mode", "cors");
        rb = rb.header("sec-fetch-dest", req.destination.token());
    }
    if let Some(init) = initiator.filter(|_| !req.no_referrer) {
        if let Some(r) = referer_with_policy(init, url, req.referrer_policy.unwrap_or_default()) {
            rb = rb.header("referer", r);
        }
    }
    for (k, v) in identity_headers(req) {
        rb = rb.header(k.as_str(), v.as_str());
    }
    // the network stack's own `Accept-Encoding` for this URL (`advertised_encodings`), as on every request
    if let Some(ae) = req.wire.as_ref().and_then(|w| accept_encoding_for(w, url, false)) {
        rb = rb.header("accept-encoding", ae);
    }
    // on HTTP/2 only ([`h2_only_priority`])
    let mut own_priority = false;
    if url.scheme() == "https" {
        if let Some(p) = chromium_priority(req) {
            rb = rb.header("priority", p);
            own_priority = true;
        }
    }
    let mut order = wreq::header::OrigHeaderMap::new();
    for n in PREFLIGHT_ORDER {
        insert_spelled(&mut order, n, None);
    }
    rb = rb.orig_headers(order);
    let (cl, built) = rb.build_split();
    let mut request = built?;
    if own_priority {
        request.extensions_mut().insert(h2_only_priority());
    }
    let h2_priority = chromium_h2_priority(&request);
    request.extensions_mut().insert(h2_priority);
    // DevTools pauses the preflight as a request of its own -- Chrome 150 pauses the OPTIONS, typed as
    // the request it precedes, its pause listing `Sec-Fetch-Mode` (`chrome150-a1s3b.json`). A `continueRequest`'s
    // overrides are not applied to it.
    let mut fulfilled = None;
    if intercept::installed() {
        let sent: Vec<(String, String)> =
            request.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let paused = intercept::PausedRequest {
            row,
            kind: PREFLIGHT_KIND.to_string(),
            destination: req.destination.token().to_string(),
            target: req.reporter.target.clone(),
            url: url.to_string(),
            method: "OPTIONS".to_string(),
            headers: intercept::paused_headers(&sent, |k| devtools_spelling(&[], k), true),
            body: None,
            priority: chromium_priority(req),
        };
        if let Some(answer) = intercept::pause(&paused) {
            match answer.await {
                intercept::Paused::Fail(reason) => return Err(anyhow::anyhow!("{reason}")),
                intercept::Paused::Fulfill(f) => fulfilled = Some(f),
                intercept::Paused::Continue(_) => {}
            }
        }
    }
    let resp = match fulfilled {
        Some(f) => intercept::fulfilled_response(&f)?,
        None => cl.execute(request).await?,
    };
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
    let bytes = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    let grant = cors::preflight_grant(status, &headers, &page.origin, req.credentials, &req.method, unsafe_headers);
    Ok((FetchResponse::new(status, String::new(), url.to_string(), headers, bytes), grant))
}

/// Fetch's redirect statuses. A 300 or 304 carrying `Location` is not a redirect.
fn is_redirect_status(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

/// A response's status text as Chromium reads it (`HttpResponseHeaders::GetStatusText`): the reason phrase of an
/// HTTP/1.x status line as the server wrote it, its surrounding spaces trimmed (`ParseStatus`), and none for HTTP/2
/// and HTTP/3, which carry no status line -- Chromium writes their head as `HTTP/1.1 <status>` with no text
/// (`spdy_http_utils.cc:85`), so `Response.statusText` is `""` there. It was the CANONICAL phrase for every response
/// (`"OK"` over h2, and a server's own `200 Awesome` read `"OK"`). `wreq` keeps the wire's phrase only when it is
/// not the canonical one (`ReasonPhrase`), so the canonical one stands for the rest.
fn status_text_of(resp: &wreq::Response) -> String {
    // a response DevTools made up (`Fetch.fulfillRequest`) carries its own phrase
    if let Some(p) = resp.extensions().get::<intercept::FulfilledPhrase>() {
        return p.0.clone();
    }
    match resp.version() {
        wreq::Version::HTTP_2 | wreq::Version::HTTP_3 => String::new(),
        _ => match resp.extensions().get::<wreq::ReasonPhrase>() {
            Some(r) => String::from_utf8_lossy(r.as_ref()).trim_matches(' ').to_string(),
            None => resp.status().canonical_reason().unwrap_or("").to_string(),
        },
    }
}

async fn fetch_on_net_runtime(req: FetchRequest, checks: StartChecks, asked: std::time::Instant, row: u64) -> anyhow::Result<FetchResponse> {
    debug_assert!(checks.at <= asked, "the start checks ran after fetchStart");
    // the first hop's upgrade, decided before fetchStart; taken on the loop's first pass only
    let mut first_upgrade = Some(checks.first_upgrade);
    let mut clock = PhaseClock::new(asked);
    clock.navigation_start = req.navigation_start.filter(|n| *n <= asked);
    // THE ONE PLACE THE WIRE IS CHOSEN, and it is chosen from the request rather than from a process
    // global. A request that no persona claimed is refused here, with the same reasoning the old
    // unconfigured-transport error carried: a guessed ClientHello is an incoherent device, and the
    // caller that could not name a persona is the bug.
    let wire = req.wire.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "this request names no persona, so it has no wire: {} {}. Every request carries the TLS \
             profile of the identity that made it -- set it with transport::apply_persona_identity \
             (or apply_persona_headers) at the call site that knows which persona is fetching. \
             Refusing to guess.",
            req.method,
            req.url
        )
    })?;
    let mut client = client_for(&wire)?;
    // A retry after each missing intermediate learned, up to the path builder's bound (N41: a chain can
    // lack two).
    let mut aia_chases = 0u32;
    // ABI 3 (wave 1, 4.5 `verify_cert`): once a certificate the trust store rejected (and AIA could not complete) is
    // retried on the insecure client so its leaf can be read and shown to the plugins, this request rides that client
    // for the rest of its hops, and every https hop is then gated by `plugin_host::verify_cert` below.
    let mut on_insecure_client = false;
    let initiator = req.initiator.as_deref().and_then(|s| url::Url::parse(s).ok());
    // The request's URL LIST, in Fetch's sense: the initial URL plus one entry per redirect followed.
    // `Sec-Fetch-Site` is computed over the WHOLE list at each hop, which is why it is accumulated
    // rather than replaced.
    let mut url_list: Vec<url::Url> = vec![url::Url::parse(&req.url)?];
    let mut method = wreq::Method::from_bytes(req.method.as_bytes())?;
    let mut body = req.body.clone();

    // RESPONSE TAINTING AND CORS, for a page's request only (`cors.rs`). Decided before anything is
    // sent: a same-origin request to another origin, and a no-cors request that will not follow its
    // redirects, never reach the network (both measured).
    let page = req.page.clone();
    let mut tainting = ResponseType::Basic;
    // Fetch's "tainted origin flag": a redirect chain that went cross-origin and on to a third origin
    // (or back) sends `Origin: null` from then on.
    let mut tainted_origin = false;
    if let Some(p) = &page {
        tainting = cors::taint(tainting, &p.origin, &url_list[0], req.mode).map_err(|e| anyhow::anyhow!("{e}: {}", req.url))?;
        if tainting == ResponseType::Opaque && req.redirect != RedirectMode::Follow {
            return Err(anyhow::anyhow!("mode no-cors with redirect mode {:?}: {}", req.redirect, req.url));
        }
        if tainting == ResponseType::Cors && cors::needs_preflight(&req.method, &p.author_headers) {
            preflight(&client, &req, p, &url_list[0], initiator.as_ref()).await?;
        }
    }

    // HTTPS-Upgrades state (see the measured table above `https_upgrade`). `upgrade_fallback` holds
    // the `http:` URL an in-flight upgraded attempt stands in for, so a transport failure or a slow
    // origin can fall back to it; `upgrade_tried` holds every `https:` URL this request has already
    // reached for, so a site that redirects `https:` back to `http:` is followed rather than looped.
    let mut upgrade_fallback: Option<url::Url> = None;
    let mut upgrade_tried: Vec<url::Url> = Vec::new();

    // The referrer policy the next hop is sent under; a redirect response may replace it.
    let mut policy = req.referrer_policy.unwrap_or_default();

    // CLIENT HINTS (see `client_hints`): the top-level document a request belongs to, and a
    // top-level navigation's `Critical-CH` restart -- the origins it restarted for, and whether the request
    // now goes out in the restarted order.
    let top_navigation = req.mode == Mode::Navigate && req.destination.token() == "document";
    let top_level = req.top_level_url.as_deref().and_then(|u| url::Url::parse(u).ok());
    let first = (url_list[0].clone(), method.clone(), body.clone());
    // A POST NAVIGATION VALIDATES THE CACHE: Chromium's `UpdateLoadFlagsWithCacheFlags` gives a different-document
    // navigation with a body `LOAD_VALIDATE_CACHE`, which the network transaction writes as `Cache-Control:
    // max-age=0` on every hop of the request (Chrome 150, 2026-10-11: each form POST, a page's or a
    // frame's; korvus sent none).
    let validate_cache = req.mode == Mode::Navigate
        && first.1 == wreq::Method::POST
        && !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cache-control"));
    // What this request may do with the HTTP cache (`http_cache`): its `RequestInit.cache`, its own `If-*` headers.
    let cache_mode = http_cache::mode(req.cache.as_deref(), &req.headers);
    let mut restarted_for: Vec<String> = Vec::new();
    let mut restarted = false;

    let mut hop = 0usize;
    // This hop follows a redirect (a navigation's then takes `redirect_order`); a restart starts afresh.
    let mut redirected = false;
    // `intercept`: where a `Fetch.continueRequest` sent this hop instead (the page still sees the
    // url it asked for), the headers it gave in place of the renderer's (kept for the hops after, as Chrome's
    // modified request keeps them), and whether this pass is the one such an answer restarted (not paused again).
    let mut rerouted: Option<url::Url> = None;
    let mut header_override: Option<Vec<(String, String)>> = None;
    let mut resumed = false;
    // The network plugins' (`plugin_net`): a pass their new url or method restarted (not asked again), the headers they
    // gave it, and the proxy they chose, kept for the request's later hops.
    let mut plugin_resumed = false;
    let mut plugin_headers: Option<Vec<(String, String)>> = None;
    let mut plugin_proxy: Option<String> = None;
    // HTTP authentication a DevTools client answered (`intercept::auth`): the credential header this request carries
    // from then on (`Authorization`, or a proxy's `Proxy-Authorization`), and how many challenges it has met.
    let mut auth_header: Option<(&'static str, String)> = None;
    let mut auth_tries = 0u32;
    while hop <= MAX_REDIRECTS {
        let mut current = url_list.last().expect("url list is never empty").clone();
        if let Some(to) = rerouted.take() {
            current = to;
        } else if upgrade_fallback.is_none() {
            // the first hop's decision was made before fetchStart ([`start_checks`]); a later hop decides here, as
            // Chrome's throttle does on a redirect
            let up = match first_upgrade.take() {
                Some(decided) => decided,
                None => https_upgrade(&current, &req.destination, req.mode),
            };
            if let Some(up) = up {
                if !upgrade_tried.contains(&up) && !url_list.contains(&up) {
                    upgrade_tried.push(up.clone());
                    upgrade_fallback = Some(current.clone());
                    current = up.clone();
                    // REPLACED, not pushed: Chrome's internal 307 is not a step the url list (and so
                    // `Sec-Fetch-Site`) is computed over -- measured `Sec-Fetch-Site: none` on the
                    // upgraded navigation, where a pushed hop would have made it `cross-site`.
                    *url_list.last_mut().expect("url list is never empty") = up;
                }
            }
        }
        clock.hop_start = std::time::Instant::now();
        // D4 DNS (ABI 3 step 13, `resolve`): before this hop connects, a resolver plugin may pin its host to
        // specific IPs or refuse the name. Gated on `plugin_host::has_resolver()`, so with no such plugin loaded
        // the host resolves exactly as before and the engine is byte-for-byte unchanged (I9/J2). A pin builds a
        // one-off client ([`dns_pinned_client`]) that answers THIS host from the plugin's addresses and every
        // other host normally, in the hop's current verification mode (so the ClientHello is unchanged); a `Block`
        // (or a pin that cannot be parsed) returns an error here and fails the hop as a DNS failure. The pinned
        // client is this hop's only -- `client` (and its pool) is untouched, so the next hop to another host, or
        // a request after a resolver plugin is dropped, resolves normally again.
        let dns_pin = if plugin_host::has_resolver() {
            dns_pinned_client(&current, &wire, on_insecure_client)?
        } else {
            None
        };
        let mut rb = dns_pin.as_ref().unwrap_or(&client).request(method.clone(), current.as_str());
        // The connection this hop may ride is one of its own pool group (`partition.rs`): its privacy mode and
        // its network anonymization key, as Chrome's socket pools and HTTP/2 sessions are keyed.
        rb = rb.group(wreq::Group::new(partition::group(&req, &current, top_level.as_ref(), initiator.as_ref(), &url_list)));
        // The client never decodes: the body's content coding is removed after the wire bytes are
        // counted (`content_coding`). The persona's `Accept-Encoding` is unaffected -- the client's
        // layer only adds one to a request that has none.
        rb = rb.gzip(false).deflate(false).brotli(false).zstd(false);
        // THE COOKIES OF THIS HOP (`cookies`): a request whose credentials are not included at this hop sends none
        // and stores none; any other sends what its SameSite context and partition let it, and its response's are
        // stored below, once its `Date` is known.
        let hop_cookies = credentials_included(&req, initiator.as_ref(), &url_list)
            .then(|| cookies::HopContext::for_hop(&req, initiator.as_ref(), top_level.as_ref(), &current, method.as_str(), url_list.len() > 1));
        match &hop_cookies {
            Some(ctx) => rb = rb.cookie_provider(Arc::new(cookies::SendCookies(ctx.clone()))),
            None => rb = rb.cookie_provider(Arc::new(NoCredentials)),
        }

        // --- the request's headers at this hop: ONE entry per name ([`HopHeaders`]) ----------------
        // The caller's go in first -- the page's own headers, its identity, a loader's `Origin`, a form's
        // `Content-Type` -- then what the network stack adds, each by its own Chromium rule (`set` /
        // `set_if_missing`). Order of insertion does not decide the wire order (the `orig_headers` map below
        // does); which names exist, once each, and their values do. They used to be appended, the caller's
        // last "so they win" -- which put two `Accept` lines on every page request that named its own.
        let mut hop_headers = HopHeaders::default();
        for (k, v) in &req.headers {
            // A caller-supplied Referer is only right for the first hop; after a redirect the chain's
            // own referrer policy owns it.
            if hop > 0 && k.eq_ignore_ascii_case("referer") {
                continue;
            }
            // A page request's Origin is the per-hop one below.
            if page.is_some() && k.eq_ignore_ascii_case("origin") {
                continue;
            }
            // A realm that sends no Client Hints and no Referer.
            if (req.no_client_hints && is_client_hint(k)) || (req.no_referrer && k.eq_ignore_ascii_case("referer")) {
                continue;
            }
            hop_headers.combine(k, v);
        }
        let mut storage: Option<&'static str> = None;
        // `Accept` (Blink's `network_utils::SetAcceptHeader`): the destination's, only when the request carries
        // none -- except where the destination's replaces it ([`accept_overwrites`]).
        let accept = accept_for(&req.destination, req.mode, req.accept_navigation.as_deref());
        if accept_overwrites(&req.destination) {
            hop_headers.set("accept", accept);
        } else {
            hop_headers.set_if_missing("accept", accept);
        }
        // `Connection: keep-alive`, second on the wire over HTTP/1.1 as Chrome sends it (the
        // header fixture's every request lacked it). Over HTTP/2 a connection-specific header is
        // illegal and the h2 layer strips it, so one insertion serves both.
        hop_headers.set_if_missing("connection", "keep-alive");
        // The client hints beyond the low-entropy three that an origin's `Accept-CH` turned on, at this hop.
        let extra_hints: Vec<&'static str> = match req.high_entropy_hints.as_ref() {
            Some(values) if !req.no_client_hints => client_hints::extra_for(&current, top_level.as_ref(), initiator.as_ref(), top_navigation)
                .into_iter()
                .filter(|h| values.iter().any(|(k, _)| k == h))
                .collect(),
            _ => Vec::new(),
        };
        if let Some(values) = req.high_entropy_hints.as_ref() {
            for h in &extra_hints {
                if let Some((_, v)) = values.iter().find(|(k, _)| k == h) {
                    hop_headers.set_if_missing(h, v.as_str());
                }
            }
        }
        // Google Chrome's own headers to Google's hosts over https ([`google_associated_host`]), in every
        // browser context: the build's channel, year and copyright. Measured on Chrome 150 through a header-rewriting
        // proxy: with korvus's Windows 152 identity, Google serves its "choose Chrome" promo -- the page it gives a
        // browser that is not Chrome -- unless these three come with it (either one alone does not do; the
        // `x-browser-validation` hash and `x-client-data` are not needed, and are not sent: the hash is keyed by
        // the build's private API key, which nothing here can reproduce).
        if let Some(year) = req.chrome_build_year.filter(|_| current.scheme() == "https" && current.host_str().is_some_and(google_associated_host)) {
            hop_headers.set("x-browser-channel", "stable");
            hop_headers.set("x-browser-year", &year.to_string());
            hop_headers.set("x-browser-copyright", &format!("Copyright {year} Google LLC. All Rights Reserved."));
        }
        // Chromium orders a subresource's headers differently from a navigation's; one order for both put
        // `sec-ch-ua` ahead of `user-agent` on every script and XHR.
        rb = rb.orig_headers(if redirected && is_navigation_like(&req.destination, req.mode) {
            redirect_order()
        } else if restarted {
            restart_order()
        } else if is_navigation_like(&req.destination, req.mode) {
            navigation_order()
        } else {
            subresource_order(&req, &extra_hints)
        });
        // A media element's request.
        if is_media(&req.destination) {
            hop_headers.set_if_missing("range", "bytes=0-");
            hop_headers.set_if_missing("accept-encoding", "identity;q=1, *;q=0");
        }
        // `Accept-Encoding` when the request carries none: [`advertised_encodings`].
        if !hop_headers.has("accept-encoding") {
            if let Some(ae) = accept_encoding_for(&wire, &current, hop_headers.has("range")) {
                hop_headers.set("accept-encoding", ae);
            }
        }
        // The RFC 9218 `priority` field, when the request carries none -- an HTTP/2-only one ([`h2_only_priority`]).
        let mut own_priority = false;
        if current.scheme() == "https" && !hop_headers.has("priority") {
            if let Some(p) = chromium_priority(&req) {
                hop_headers.set("priority", p);
                own_priority = true;
            }
        }
        // Fetch Metadata is attached ONLY to a request for a potentially-trustworthy URL. Plain http
        // to a named host gets none of it — which is exactly what WPT's fetch/metadata suite asserts,
        // and what emitting the wreq preset unconditionally was getting wrong on every request.
        if is_potentially_trustworthy(&current) {
            // an opaque request origin (a sandboxed frame's `fetch` or element load) is same-origin with nothing
            hop_headers.set("sec-fetch-site", if req.opaque_origin { "cross-site" } else { sec_fetch_site(initiator.as_ref(), &url_list) });
            hop_headers.set("sec-fetch-mode", req.mode.token());
            if req.mode == Mode::Navigate && req.user_activated {
                hop_headers.set("sec-fetch-user", "?1");
            }
            hop_headers.set("sec-fetch-dest", req.destination.token());
            storage = storage_access(&req, initiator.as_ref(), &url_list, &current);
            if let Some(v) = storage {
                hop_headers.set("sec-fetch-storage-access", v);
            }
        }
        if req.mode == Mode::Navigate {
            // Chrome sends this on every navigation (it is how a server knows it may upgrade), and on
            // nothing else.
            hop_headers.set("upgrade-insecure-requests", "1");
        }
        if let Some(init) = initiator.as_ref().filter(|_| !req.no_referrer) {
            if let Some(r) = referer_with_policy(init, &current, policy) {
                // a caller's own first-hop Referer stands (it is skipped after a redirect, above)
                hop_headers.set_if_missing("referer", r);
            }
        }
        // A page request's `Origin`, per hop (Fetch "append a request Origin header"): for a CORS-tainted
        // request, and for any method other than GET/HEAD; `null` once the origin is tainted.
        //
        // AND FOR A REQUEST THE LOADER CONFIGURED FOR CORS ITSELF. Fetch's two
        // rules do not cover the third case Chromium actually has: a module script, or a
        // `crossorigin`-attributed subresource, carries `Origin` even when it is same-origin and a
        // GET. Measured against Brave 151, Chrome 150, Edge 151 and Opera 135 -- all four send it --
        // and korvus sent it on NEITHER the same-origin nor the cross-origin module import, because
        // a module request carried no `page` at all and so was never tainted, never CORS-checked and
        // never given an origin. See `PageRequest::cors_preconfigured` for the whole measured table.
        let mut origin_sent = false;
        if let Some(p) = &page {
            let non_get = method != wreq::Method::GET && method != wreq::Method::HEAD;
            let loader_stated = p.cors_preconfigured && req.mode == Mode::Cors;
            if tainting == ResponseType::Cors || (non_get && req.mode != Mode::Navigate) || loader_stated {
                hop_headers.set("origin", if tainted_origin { "null" } else { p.origin.as_str() });
                origin_sent = true;
            }
        }
        // A NAVIGATION's `Origin` (Fetch "append a request Origin header", a mode that is not `cors`): any method but
        // GET and HEAD -- a form POST -- names its initiator's origin, `null` where the referrer policy withholds it
        // or a redirect tainted it (Chrome 150, 2026-10-11: a same-origin and a cross-site form POST each
        // carry the page's origin; korvus sent none).
        if page.is_none() && req.mode == Mode::Navigate && method != wreq::Method::GET && method != wreq::Method::HEAD {
            if let Some(init) = initiator.as_ref() {
                let o = init.origin();
                let downgrade = init.scheme() == "https" && current.scheme() != "https";
                let withheld = !o.is_tuple()
                    || tainted_origin
                    || match policy {
                        ReferrerPolicy::NoReferrer => true,
                        ReferrerPolicy::NoReferrerWhenDowngrade | ReferrerPolicy::StrictOrigin | ReferrerPolicy::StrictOriginWhenCrossOrigin => downgrade,
                        ReferrerPolicy::SameOrigin => o != current.origin(),
                        _ => false,
                    };
                hop_headers.set("origin", if withheld { "null".to_string() } else { o.ascii_serialization() });
                origin_sent = true;
            }
        }
        if validate_cache {
            hop_headers.set_if_missing("cache-control", "max-age=0");
        }
        // DevTools' cache bypass (`http_cache::BYPASS`)
        if http_cache::bypassed() {
            hop_headers.set_if_missing("pragma", "no-cache");
            hop_headers.set_if_missing("cache-control", "no-cache");
        }
        // An `inactive` storage-access request names its origin, so the server may ask to activate it
        // (Storage Access Headers; Brave 151 measured).
        if storage == Some("inactive") && !origin_sent && !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")) {
            if req.opaque_origin {
                // the request's origin, serialized: an opaque one is `null`
                hop_headers.set_if_missing("origin", "null");
            } else if let Some(o) = initiator.as_ref().map(|i| i.origin()).filter(|o| o.is_tuple()) {
                hop_headers.set_if_missing("origin", o.ascii_serialization());
            }
        }
        // The credentials a client gave for this request's challenge, else those an earlier challenge on this origin
        // was answered with (Chrome sends them up front from its auth cache) -- never over the page's own.
        if !hop_headers.has("authorization") {
            match &auth_header {
                Some((name, value)) => hop_headers.set(name, value.as_str()),
                None => {
                    if let Some(v) = cached_auth(&current) {
                        hop_headers.set("authorization", v.as_str());
                    }
                }
            }
        }
        // DevTools' pause (`intercept`), once the hop's headers are known and before anything is
        // sent -- at every hop, as Chrome pauses each request it sends, but not on the pass a `continueRequest` with
        // overrides restarted.
        let mut fulfilled: Option<intercept::Fulfilment> = None;
        // THE NETWORK PLUGINS (`plugin_net`, goal 1020 step 2), before DevTools' pause, at every hop: each may rewrite
        // the hop's url, method, headers or body, send it through a proxy of its own, answer it, or refuse it. A new
        // url or method restarts the hop with it, and that pass does not ask them again.
        if plugin_resumed {
            plugin_resumed = false;
            if let Some(h) = plugin_headers.take() {
                hop_headers.0 = h;
            }
        } else if plugin_host::has_network() && matches!(current.scheme(), "http" | "https") {
            let mut asked = plugin_net::Hop::new(current.as_str(), method.as_str(), &hop_headers.0, req.destination.token(), req.top_level_url.as_deref());
            let mut verdict = plugin_host::on_request(&mut asked);
            // D7 async hold (ABI 3 step 14, `KORVUS_HOLD`): a plugin PARKED this hop. It returns here only once the
            // plugin releases it with `host->resume_request` / `modify_and_resume`. The wait is a plain `.await` on
            // the net runtime -- this one hop's future parks, while the engine's thread and every other page keep
            // running (D7); a cancel aborts it exactly as it does DevTools' pause below. The resume's verdict stands
            // in for the plugin's (`Continue` sends the hop, `Block` fails it), and its `modify`, when set, edits the
            // hop first -- through the same `RequestCtx` setters (and validation) a synchronous `on_request` edit
            // uses, so the downstream header/body/proxy/restart handling treats a resumed hop like any other. Off
            // entirely when nothing holds: `hold_gate` returns `None` from one relaxed atomic load on every run whose
            // plugins never park a request -- every no-plugin run too (I9/J2).
            if let plugin_host::Verdict::Hold(id) = verdict {
                verdict = match plugin_host::hold_gate(id) {
                    Some(token) => {
                        let resolution = match req.cancel.clone() {
                            Some(cancel) => tokio::select! {
                                r = token => r,
                                _ = cancel.notified() => return Err(anyhow::anyhow!("aborted: {} was cancelled while a plugin held it", req.url)),
                            },
                            None => token.await,
                        };
                        if let Some(m) = resolution.modify {
                            asked.apply_hold_modify(m);
                        }
                        resolution.verdict
                    }
                    // The slot was resolved and reclaimed before we could await it, or the gate is down -- release
                    // the hop as if it had continued (never silently drop a hop a plugin meant to let through).
                    None => plugin_host::Verdict::Continue,
                };
            }
            if verdict == plugin_host::Verdict::Block {
                return Err(anyhow::anyhow!("net::ERR_BLOCKED_BY_CLIENT: a plugin refused {current}"));
            }
            if asked.headers_changed {
                hop_headers.0 = asked.headers.clone();
            }
            if let Some(b) = asked.body.take() {
                body = Some(b);
            }
            if let Some(p) = asked.proxy.take() {
                plugin_proxy = Some(p);
            }
            if verdict == plugin_host::Verdict::Fulfill {
                fulfilled = asked.fulfilment.take();
            } else {
                let mut restart = false;
                if let Some(m) = asked.new_method.take() {
                    method = wreq::Method::from_bytes(m.as_bytes())?;
                    restart = true;
                }
                if let Some(u) = asked.new_url.take().and_then(|u| url::Url::parse(&u).ok()).filter(|u| matches!(u.scheme(), "http" | "https")) {
                    rerouted = Some(u);
                    restart = true;
                }
                if restart {
                    plugin_resumed = true;
                    plugin_headers = asked.headers_changed.then(|| asked.headers.clone());
                    continue;
                }
            }
        }
        if resumed {
            resumed = false;
        } else if intercept::installed() && matches!(current.scheme(), "http" | "https") {
            let paused = intercept::PausedRequest {
                row,
                kind: req.destination.token().to_string(),
                destination: req.destination.token().to_string(),
                target: req.reporter.target.clone(),
                url: current.to_string(),
                method: method.to_string(),
                headers: intercept::paused_headers(&hop_headers.0, |k| devtools_spelling(&req.headers, k), false),
                body: body.clone(),
                priority: chromium_priority(&req),
            };
            if let Some(answer) = intercept::pause(&paused) {
                let answer = match req.cancel.clone() {
                    Some(cancel) => tokio::select! {
                        a = answer => a,
                        _ = cancel.notified() => return Err(anyhow::anyhow!("aborted: {} was cancelled while DevTools held it", req.url)),
                    },
                    None => answer.await,
                };
                match answer {
                    intercept::Paused::Fail(reason) => return Err(anyhow::anyhow!("{reason}")),
                    intercept::Paused::Fulfill(f) => fulfilled = Some(f),
                    intercept::Paused::Continue(o) => {
                        if let Some(h) = o.headers {
                            header_override = Some(h);
                        }
                        if let Some(b) = o.body {
                            body = Some(b);
                        }
                        // A new URL or method changes what this hop's request is built from: the hop starts again
                        // with it, and is not paused a second time.
                        let mut restart = false;
                        if let Some(m) = o.method {
                            method = wreq::Method::from_bytes(m.as_bytes())?;
                            restart = true;
                        }
                        if let Some(u) = o.url {
                            match intercept::same_scheme(&current, &u) {
                                Some(to) => {
                                    rerouted = Some(to);
                                    restart = true;
                                }
                                None => return Err(anyhow::anyhow!("net::ERR_FAILED: DevTools sent {} to {u}, another scheme", req.url)),
                            }
                        }
                        if restart {
                            resumed = true;
                            continue;
                        }
                    }
                }
            }
        }
        if let Some(given) = &header_override {
            hop_headers.0 = intercept::overridden_headers(&hop_headers.0, given);
        }
        for (k, v) in &hop_headers.0 {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(b) = body.clone() {
            rb = rb.body(b);
        }

        // The host's ECH config, if its DNS `HTTPS` record publishes one (see `ech`) -- not for a response
        // DevTools made up, which never reaches the host.
        if current.scheme() == "https" && fulfilled.is_none() {
            if let Some(host) = current.host_str() {
                ech::prepare(host).await;
            }
        }
        // THE HTTP CACHE (`http_cache`): a GET asks it first -- answered from it as stored, or sent with the stored
        // answer's validators. The request is built here so the cache sees the headers it will carry (`Vary`).
        if let Some(p) = &plugin_proxy {
            // D3 (ABI 3 step 12): if a plugin supplies this proxy's credentials (`on_proxy_auth`), splice them in
            // so the request carries `Proxy-Authorization` and an authenticating proxy admits it -- answering its
            // 407 challenge up front. No plugin, or no credentials, leaves the proxy URL exactly as it was (I9/J2).
            let authed = plugin_net::proxy_auth(p);
            rb = plugin_net::with_proxy(rb, authed.as_deref().unwrap_or(p));
        }
        let (cl, built) = rb.build_split();
        let mut request = built?;
        if own_priority {
            request.extensions_mut().insert(h2_only_priority());
        }
        let h2_priority = chromium_h2_priority(&request);
        request.extensions_mut().insert(h2_priority);
        // a response DevTools made up is neither looked up nor stored (Chrome serves it beside the network service)
        let cacheable = fulfilled.is_none() && method == wreq::Method::GET && !is_media(&req.destination);
        let partition = partition::cache_partition(&req, &current, top_level.as_ref(), initiator.as_ref());
        let cache_key = http_cache::key(&partition, &current);
        // D10 (step 14, Frontier L): the key korvus hands a shared-cache plugin -- the request method, the same
        // partition, and the URL without its fragment (the browser cache key folded into one string; `Vary` is
        // enforced from the stored blob in `serve_match`). Built only when a cache plugin is loaded
        // (`has_cache`), so a run with none computes nothing new and behaves exactly as before (I9/J2).
        let shared_key = if plugin_host::has_cache() { http_cache::shared_key(method.as_str(), &partition, &current) } else { String::new() };
        let request_headers: Vec<(String, String)> =
            request.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let mut validating = false;
        if cacheable {
            // THE SHARED CACHE (D10): the cache a plugin backs, consulted ahead of the in-session one and only when
            // a cache plugin is loaded (`shared_key` non-empty; OFF by default, I9/J2). It honours the request's
            // cache mode like the in-session cache -- a `reload` / `no-store` request (`!cache_mode.read`) still
            // bypasses it and goes to the network. A hit -- a stored blob whose recorded `Vary` matches this request
            // -- is served as the network's answer would have been; the plugin owns the blob's freshness and
            // eviction (a shared, cross-process store), so a hit is served as stored. A miss (NULL, an undecodable
            // or malformed blob, or a `Vary` mismatch) falls through to the in-session cache and the network below.
            if !shared_key.is_empty() && cache_mode.read {
                if let Some(bytes) = plugin_host::cache_get(&shared_key) {
                    if let Some(stored) = http_cache::serve_match(&bytes, &request_headers) {
                        clock.request_start = std::time::Instant::now();
                        clock.response_start = clock.request_start;
                        clock.conn = None;
                        let version = stored.version;
                        return deliver_stored(&req, page.as_ref(), tainting, tainted_origin, &url_list, &clock, &stored, version, "plugin");
                    }
                }
            }
            match http_cache::lookup(&cache_key, &request_headers, cache_mode, http_cache::now()) {
                http_cache::Use::Fresh(stored) => {
                    clock.request_start = std::time::Instant::now();
                    clock.response_start = clock.request_start;
                    clock.conn = None;
                    let version = stored.version;
                    return deliver_stored(&req, page.as_ref(), tainting, tainted_origin, &url_list, &clock, &stored, version, "local");
                }
                http_cache::Use::Validate(validators) => {
                    for (name, value) in validators {
                        if let Ok(v) = wreq::header::HeaderValue::from_str(&value) {
                            request.headers_mut().insert(name, v);
                        }
                    }
                    validating = true;
                }
                http_cache::Use::Miss if cache_mode.only_if_cached => {
                    return Err(anyhow::anyhow!("only-if-cached: {current} is not in the HTTP cache"));
                }
                http_cache::Use::Miss => {}
            }
        }
        let request_time = http_cache::now();
        clock.request_start = std::time::Instant::now();
        // An upgraded attempt is on a clock and has somewhere to go when it fails; every other
        // request is sent exactly as before.
        let sent = match (fulfilled.take(), &upgrade_fallback) {
            (Some(f), _) => intercept::fulfilled_response(&f),
            (None, Some(_)) => match tokio::time::timeout(HTTPS_UPGRADE_FALLBACK, cl.execute(request)).await {
                Ok(r) => r.map_err(anyhow::Error::from),
                Err(_) => Err(anyhow::anyhow!(
                    "the https upgrade of {current} did not answer within {:?}",
                    HTTPS_UPGRADE_FALLBACK
                )),
            },
            (None, None) => cl.execute(request).await.map_err(anyhow::Error::from),
        };
        let resp = match sent {
            Ok(r) => r,
            Err(e) => match upgrade_fallback.take() {
                // The upgrade never reached the page, so this is not a redirect and does not spend
                // a hop: the original `http:` URL is put back and the same hop is sent again.
                Some(original) => {
                    *url_list.last_mut().expect("url list is never empty") = original;
                    continue;
                }
                None => {
                    if aia_chases < AIA_CHASES_PER_REQUEST && is_cert_verify_failure(&e) {
                        aia_chases += 1;
                        if let Some(host) = current.host_str().map(str::to_string) {
                            if chase_aia(&host, &wire).await {
                                client = client_for(&wire)?;
                                continue;
                            }
                        }
                    }
                    // ABI 3 (wave 1, 4.5 `verify_cert`): a certificate the trust store rejected and AIA could not
                    // complete is the plugins' to decide (an intercepting proxy's CA, a scanner's pinned leaf). Retry
                    // the hop once on the insecure client; its handshake succeeds, so the leaf rides the response and
                    // is shown to `plugin_host::verify_cert` before the response is used (just after `sent`, below).
                    //
                    // TODO(1021-verify): this decides AFTER the trust-store verdict, by retrying on a
                    // verification-off client to read the leaf. The REAL verify decision is BoringSSL's
                    // `set_verify_callback` in vendored wreq (korvus/vendor/wreq/src/tls/conn.rs, the `issuer_miss_hook`
                    // blocks at the `set_verify_callback` calls in `setup_ssl`/`setup_ssl2`), which returns `ok`
                    // unchanged. The fully-correct hook would call `plugin_host::verify_cert(host, leaf_der)` there and
                    // return its verdict as `ok` -- avoiding the extra insecure round-trip and the brief window where
                    // the request rides an unverified connection before the plugin approves it. That needs a new
                    // consumer-facing hook in wreq (like `set_issuer_miss_hook`), which is outside this crate (the
                    // vendored wreq and the plugin-host ABI are owned by other passes).
                    if !on_insecure_client && is_cert_verify_failure(&e) && (has_tls_plugin() || spec::devtools::ignore_cert_errors()) {
                        on_insecure_client = true;
                        client = insecure_client_for(&wire)?;
                        continue;
                    }
                    // A certificate the trust store refused fails as Chrome names it.
                    if is_cert_verify_failure(&e) {
                        return Err(anyhow::anyhow!("net::ERR_CERT_AUTHORITY_INVALID: {e:#}"));
                    }
                    return Err(e);
                }
            },
        };
        upgrade_fallback = None;
        // ABI 3 (wave 1, 4.5 `verify_cert`): while this request rides the insecure client (a normal cert-verify
        // failed and a `verify_cert` plugin opted to decide), BoringSSL enforced no verdict, so every https hop's
        // server certificate is shown to the plugins and only an explicit allow (`Some(true)`) is let through -- a
        // deny (`Some(false)`) or a defer (`None`, which includes "no plugin decided") fails the hop, exactly as the
        // rejected certificate would have. The leaf's DER rides the response's `TlsInfo` (the insecure client sets
        // `tls_info(true)`), base64-encoded as `verify_cert` takes it; a hop whose leaf cannot be read fails closed.
        if on_insecure_client && current.scheme() == "https" {
            use base64::Engine as _;
            let leaf = resp
                .extensions()
                .get::<wreq::tls::TlsInfo>()
                .and_then(|i| i.peer_certificate())
                .map(|der| base64::engine::general_purpose::STANDARD.encode(der));
            let host = current.host_str().unwrap_or_default();
            // a DevTools client said to use it anyway (`Security.setIgnoreCertificateErrors`)
            let allowed = spec::devtools::ignore_cert_errors()
                || leaf.as_deref().is_some_and(|b64| plugin_host::verify_cert(host, b64) == Some(true));
            if !allowed {
                return Err(anyhow::anyhow!(
                    "net::ERR_CERT_AUTHORITY_INVALID: {current} -- no plugin vouched for the server certificate the \
                     trust store rejected"
                ));
            }
        }
        clock.response_start = std::time::Instant::now();
        let response_time = http_cache::now();
        // The connection it came over: opened for this hop (its connect ends, and the request goes out, when it
        // became usable), or ridden.
        clock.connection(
            resp.extensions().get::<wreq::ConnectTiming>().copied(),
            resp.extensions().get::<wreq::ConnectionReused>().is_some_and(|r| r.0),
        );
        let status = resp.status();
        let version = resp.version();
        // The terminal hop's connection, read before the body consumes `resp` (plugin ABI 3, D1). Re-read each hop,
        // like `version`, so a redirect chain leaves the final hop's address here.
        let remote_ip = resp.remote_addr().map(|a| a.to_string()).unwrap_or_default();
        let status_text = status_text_of(&resp);
        // The hop's `Set-Cookie`s go into the store before anything reads the response -- per hop: a redirect that
        // sets a session cookie is how most login flows work. Each one's `Expires` is moved by the server's clock
        // skew, its `Date` (`URLRequestHttpJob::SaveCookiesAndNotifyHeadersComplete`).
        if let Some(ctx) = &hop_cookies {
            let date = resp.headers().get("date").and_then(|v| v.to_str().ok());
            cookies::store_response(&current, resp.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()), cookies::server_time(date), ctx);
        }
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        // A `304` to the cache's own validators: the stored answer, its headers updated by this one, is the response.
        if validating && status.as_u16() == 304 {
            if let Some(stored) = http_cache::refresh(&cache_key, &headers, request_time, response_time) {
                let _ = resp.bytes().await;
                return deliver_stored(&req, page.as_ref(), tainting, tainted_origin, &url_list, &clock, &stored, version, "validated");
            }
        }
        // AN AUTHENTICATION CHALLENGE, while a DevTools client takes them (`Fetch.authRequired`): held for its answer --
        // credentials send this hop again with them, anything else lets the challenge's own response through. A
        // client answering the same challenge again and again is cut short, as Chrome stops at its retry limit.
        let challenged = match status.as_u16() {
            401 => Some(("Server", "www-authenticate", "authorization")),
            407 => Some(("Proxy", "proxy-authenticate", "proxy-authorization")),
            _ => None,
        };
        if let (Some((source, asked, answer_header)), true) = (challenged, auth_tries < 3 && intercept::installed()) {
            if let Some(v) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(asked)).map(|(_, v)| v.clone()) {
                let (scheme, realm) = intercept::parse_challenge(&v);
                let challenge = intercept::AuthChallenge {
                    req: intercept::PausedRequest {
                        row,
                        kind: req.destination.token().to_string(),
                        destination: req.destination.token().to_string(),
                        target: req.reporter.target.clone(),
                        url: current.to_string(),
                        method: method.to_string(),
                        headers: intercept::paused_headers(&hop_headers.0, |k| devtools_spelling(&req.headers, k), false),
                        body: body.clone(),
                        priority: chromium_priority(&req),
                    },
                    source,
                    origin: current.origin().ascii_serialization(),
                    scheme: scheme.clone(),
                    realm,
                };
                if let Some(answer) = intercept::auth(&challenge) {
                    let answer = match req.cancel.clone() {
                        Some(cancel) => tokio::select! {
                            a = answer => a,
                            _ = cancel.notified() => return Err(anyhow::anyhow!("aborted: {} was cancelled while DevTools held its challenge", req.url)),
                        },
                        None => answer.await,
                    };
                    if let intercept::AuthAnswer::Provide { username, password } = answer {
                        if scheme == "basic" {
                            use base64::Engine as _;
                            let token = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")));
                            auth_tries += 1;
                            if source == "Server" {
                                remember_auth(&current, &token);
                            }
                            auth_header = Some((answer_header, token));
                            let _ = resp.bytes().await;
                            // the same hop again, not paused a second time
                            resumed = true;
                            continue;
                        }
                    }
                }
            }
        }
        // An unsafe method's success makes the stored answer for its URL stale for good (RFC 9111 4.4).
        if !matches!(method, wreq::Method::GET | wreq::Method::HEAD | wreq::Method::OPTIONS) && status.as_u16() < 400 {
            http_cache::invalidate(&cache_key);
        }

        // CLIENT HINTS: a top-level navigation's answer persists its origin's `Accept-CH`, and its
        // `Critical-CH` restarts the navigation -- once per origin, from the first URL, with the cookies
        // this answer set -- when a hint it names that `Accept-CH` turned on went out missing (Chromium's
        // `CriticalClientHintsThrottle`; DevTools shows the restart as a `307 Internal Redirect`).
        if top_navigation && is_potentially_trustworthy(&current) {
            if let Some(accept) = client_hints::combined(&headers, "accept-ch").and_then(|v| client_hints::parse(&v)) {
                client_hints::persist(&current.origin(), &accept);
                let mut sent: Vec<&str> = extra_hints.clone();
                if !req.no_client_hints {
                    sent.extend(client_hints::LOW_ENTROPY.iter().filter(|n| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n))));
                }
                let critical = client_hints::combined(&headers, "critical-ch");
                if req.high_entropy_hints.is_some()
                    && client_hints::critical_restart(&current, &accept, critical.as_deref(), &sent, &mut restarted_for)
                {
                    let _ = resp.bytes().await;
                    // Chrome's navigation entry counts the restart as a redirect, its window from the
                    // first request to here, and stamps `criticalCHRestart` (measured, Chrome 150).
                    let now = std::time::Instant::now();
                    if clock.redirect_start.is_none() {
                        clock.redirect_start = Some(clock.hop_start);
                    }
                    clock.redirect_end = Some(now);
                    clock.redirect_count += 1;
                    clock.critical_ch_restart = Some(now);
                    url_list = vec![first.0.clone()];
                    method = first.1.clone();
                    body = first.2.clone();
                    restarted = true;
                    redirected = false;
                    continue;
                }
            }
        }

        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        if is_redirect_status(status.as_u16()) && hop < MAX_REDIRECTS {
            if let Some(next) = location.and_then(|loc| current.join(&loc).ok()) {
                if let Some(p) = ReferrerPolicy::from_headers(&headers) {
                    policy = p;
                }
                if let Some(p) = &page {
                    // A CORS-tainted redirect must itself pass the CORS check, or the next hop is not
                    // requested (measured: B -> 302 without ACAO -> B?acao=* sends ONE request).
                    if tainting == ResponseType::Cors && !cors::cors_check(&headers, if tainted_origin { "null" } else { &p.origin }, req.credentials) {
                        return Err(anyhow::anyhow!("CORS: redirect response from {current} is not allowed"));
                    }
                    match req.redirect {
                        RedirectMode::Error => return Err(anyhow::anyhow!("redirect mode error: {current} redirected")),
                        RedirectMode::Manual => {
                            let _ = resp.bytes().await;
                            let timing = clock.finish(&url_list, status.as_u16(), version, &headers, 0, 0);
                            let mut out = FetchResponse::new(status.as_u16(), String::new(), url_list[0].to_string(), headers, Vec::new());
                            out.timing = timing;
                            out.response_type = ResponseType::OpaqueRedirect;
                            out.credentials = req.credentials;
                            return Ok(out);
                        }
                        RedirectMode::Follow => {}
                    }
                }
                // 303 always becomes GET; 301/302 become GET for anything that is not GET/HEAD
                // (what every browser does, over the letter of RFC 7231). 307/308 preserve both
                // method and body.
                let code = status.as_u16();
                let redirected_from = current.to_string();
                if code == 303 || ((code == 301 || code == 302) && method != wreq::Method::GET && method != wreq::Method::HEAD) {
                    method = wreq::Method::GET;
                    body = None;
                }
                // Every hop is checked, not just the first: an https page's fetch that redirects
                // to http is blocked at that hop (measured, see "Mixed content" above), and an
                // upgradeable load is upgraded there too.
                let next = match mixed_content(initiator.as_ref(), &next, &req.destination, req.mode) {
                    MixedContent::Allowed => next,
                    MixedContent::Upgrade(u) => u,
                    MixedContent::Blocked => {
                        return Err(anyhow::anyhow!("{MIXED_CONTENT_BLOCKED}: redirect to {next} was not followed from a secure page"));
                    }
                };
                // a navigation's origin is its initiator's: tainted the same way (Fetch "redirect-tainted origin")
                if let (None, Mode::Navigate, Some(init)) = (&page, req.mode, initiator.as_ref()) {
                    if current.origin() != next.origin() && current.origin() != init.origin() {
                        tainted_origin = true;
                    }
                }
                if let Some(p) = &page {
                    if current.origin() != next.origin() && current.origin().ascii_serialization() != p.origin {
                        tainted_origin = true;
                    }
                    tainting = cors::taint(tainting, &p.origin, &next, req.mode).map_err(|e| anyhow::anyhow!("{e}: {next}"))?;
                }
                if clock.redirect_start.is_none() {
                    clock.redirect_start = Some(clock.hop_start);
                }
                clock.redirect_end = Some(std::time::Instant::now());
                clock.redirect_count += 1;
                // The hop as the request log reports it (N27): its redirect response, and where it led.
                ledger().redirect(
                    row,
                    RedirectHop {
                        url: redirected_from,
                        status: code,
                        status_text: status_text.clone(),
                        headers: headers.clone(),
                        next_url: next.to_string(),
                        next_method: method.to_string(),
                        at_ms: 0.0,
                    },
                );
                url_list.push(next);
                hop += 1;
                redirected = true;
                continue;
            }
        }

        let final_url = url_list.last().expect("url list is never empty").to_string();
        if let Some(p) = &page {
            if tainting == ResponseType::Cors && !cors::cors_check(&headers, if tainted_origin { "null" } else { &p.origin }, req.credentials) {
                return Err(anyhow::anyhow!("CORS: the response from {final_url} is not allowed for origin {}", p.origin));
            }
        }
        // THE HEAD FIRST: whoever asked for it gets the response as soon as its headers
        // are in, and the body is read after -- unless the request is cancelled meanwhile.
        if let Some(sink) = req.on_head.as_ref().and_then(|s| s.lock().ok().and_then(|mut g| g.take())) {
            let mut head = FetchResponse::new(
                status.as_u16(),
                status_text.clone(),
                url_list.last().expect("url list is never empty").to_string(),
                headers.clone(),
                Vec::new(),
            );
            head.response_type = tainting;
            head.redirected = url_list.len() > 1;
            head.credentials = req.credentials;
            let _ = sink.send(head);
        }
        // The content coding comes off HERE, so the wire bytes are counted (`content_coding`).
        let mut decoder = content_coding::BodyDecoder::for_headers(&headers);
        let bytes = match (req.cancel.clone(), req.on_chunk.clone()) {
            // Streamed: chunk by chunk, each one forwarded as it lands (H47), the whole kept.
            (cancel, Some(sink)) => {
                let mut resp = resp;
                let mut all = Vec::new();
                let cancel = cancel.unwrap_or_default();
                loop {
                    let next = tokio::select! {
                        c = resp.chunk() => c,
                        _ = cancel.notified() => return Err(anyhow::anyhow!("aborted: the body of {} was cancelled", req.url)),
                    };
                    match next {
                        Ok(Some(c)) => {
                            let d = decoder.feed(&c)?;
                            if !d.is_empty() {
                                all.extend_from_slice(&d);
                                let _ = sink.send(d);
                            }
                        }
                        Ok(None) => break,
                        Err(e) => return Err(anyhow::anyhow!("the body of {} failed: {e}", req.url)),
                    }
                }
                all
            }
            // A body that failed mid-read is a failed fetch, as in the streamed branch above -- never an empty body
            // under the head's status, which the page would run (an empty script "loads") and `put` would cache.
            (Some(cancel), None) => tokio::select! {
                b = resp.bytes() => decoder.feed(&b.map_err(|e| anyhow::anyhow!("the body of {} failed: {e}", req.url))?)?,
                _ = cancel.notified() => return Err(anyhow::anyhow!("aborted: the body of {} was cancelled", req.url)),
            },
            (None, None) => decoder.feed(&resp.bytes().await.map_err(|e| anyhow::anyhow!("the body of {} failed: {e}", req.url))?)?,
        };
        let (tail, encoded_size) = decoder.finish()?;
        let bytes = if tail.is_empty() {
            bytes
        } else {
            if let Some(sink) = req.on_chunk.as_ref() {
                let _ = sink.send(tail.clone());
            }
            [bytes, tail].concat()
        };
        let timing = clock.finish(&url_list, status.as_u16(), version, &headers, bytes.len(), encoded_size);
        if cacheable {
            http_cache::put(&cache_key, cache_mode, &request_headers, status.as_u16(), version, &headers, &bytes, encoded_size, request_time, response_time);
            // D10: offer the same answer to the shared cache a plugin backs. korvus serializes the storable answer
            // (the `put` storability rules) and hands the plugin the blob under `shared_key`; the plugin stores it
            // and owns its lifetime. A no-op when no cache plugin is loaded (`shared_key` empty) or the answer is
            // not storable.
            if !shared_key.is_empty() {
                if let Some(blob) =
                    http_cache::shared_blob(cache_mode, &request_headers, status.as_u16(), version, &headers, &bytes, encoded_size, request_time, response_time)
                {
                    plugin_host::cache_put(&shared_key, &blob);
                }
            }
        }
        let mut out = FetchResponse::new(status.as_u16(), status_text, final_url, headers, bytes);
        out.timing = timing;
        out.remote_ip = remote_ip;
        out.http_version = http_version_token(version).to_string();
        out.response_type = tainting;
        out.redirected = url_list.len() > 1;
        out.credentials = req.credentials;
        return Ok(out);
    }
    Err(anyhow::anyhow!("too many redirects ({MAX_REDIRECTS}) for {}", req.url))
}

/// An answer the HTTP cache holds, delivered as the network's would have been: the CORS check, the head first, the
/// body as one chunk, and the timing marked with where it came from (`"local"`: served as stored; `"validated"`: a
/// `304` confirmed it, over `version`).
#[allow(clippy::too_many_arguments)]
fn deliver_stored(
    req: &FetchRequest,
    page: Option<&PageRequest>,
    tainting: ResponseType,
    tainted_origin: bool,
    url_list: &[url::Url],
    clock: &PhaseClock,
    stored: &http_cache::Entry,
    version: wreq::Version,
    cache: &str,
) -> anyhow::Result<FetchResponse> {
    let final_url = url_list.last().expect("url list is never empty").to_string();
    if let Some(p) = page {
        if tainting == ResponseType::Cors && !cors::cors_check(&stored.headers, if tainted_origin { "null" } else { &p.origin }, req.credentials) {
            return Err(anyhow::anyhow!("CORS: the response from {final_url} is not allowed for origin {}", p.origin));
        }
    }
    // The stored answer's status text: none for one that came over HTTP/2 or HTTP/3 ([`status_text_of`]); the cache
    // does not keep an HTTP/1.x phrase, so the canonical one stands for it.
    let reason = match stored.version {
        wreq::Version::HTTP_2 | wreq::Version::HTTP_3 => String::new(),
        _ => wreq::StatusCode::from_u16(stored.status).ok().and_then(|s| s.canonical_reason()).unwrap_or("").to_string(),
    };
    if let Some(sink) = req.on_head.as_ref().and_then(|s| s.lock().ok().and_then(|mut g| g.take())) {
        let mut head = FetchResponse::new(stored.status, reason.clone(), final_url.clone(), stored.headers.clone(), Vec::new());
        head.response_type = tainting;
        head.redirected = url_list.len() > 1;
        head.credentials = req.credentials;
        let _ = sink.send(head);
    }
    if let Some(sink) = req.on_chunk.as_ref().filter(|_| !stored.body.is_empty()) {
        let _ = sink.send(stored.body.clone());
    }
    let mut timing = clock.finish(url_list, stored.status, version, &stored.headers, stored.body.len(), stored.encoded_size);
    timing.cache = cache.to_string();
    let mut out = FetchResponse::new(stored.status, reason, final_url, stored.headers.clone(), stored.body.clone());
    out.timing = timing;
    out.response_type = tainting;
    out.redirected = url_list.len() > 1;
    out.credentials = req.credentials;
    Ok(out)
}

/// Banner string used by the binary.
pub fn transport_name() -> &'static str {
    "korvus-transport (wreq/BoringSSL Chrome-emulation, GS.1)"
}

#[cfg(test)]
mod tests {
    // Chromium's `IsGoogleAssociatedDomainUrl`: google.<TLD> / youtube.<TLD> and their subdomains, and the fixed
    // suffixes; nothing that merely contains the word.
    #[test]
    fn googles_hosts_are_chromiums_list() {
        for host in ["google.com", "www.google.com", "www.google.co.kr", "play.google.com", "ogads-pa.clients6.google.com",
            "www.gstatic.com", "fonts.googleapis.com", "www.youtube.com", "i.ytimg.com", "ad.doubleclick.net", "google.de"]
        {
            assert!(super::google_associated_host(host), "{host}");
        }
        for host in ["notgoogle.com", "google.example.com", "example.com", "gstatic.com.evil.org", "googleapis.com.example", "www.naver.com"] {
            assert!(!super::google_associated_host(host), "{host}");
        }
    }

    // Google Chrome's build year from its major (Chrome 150.0.7871.46 sends 2026); no year for a browser that is not
    // Google Chrome.
    #[test]
    fn a_chrome_personas_build_year() {
        let ch = |brand: &str, v: &str| spec::ClientHints {
            brands: vec![spec::Brand { brand: brand.into(), version: v.into() }],
            ..Default::default()
        };
        assert_eq!(super::chrome_build_year(&ch("Google Chrome", "150")), Some(2026));
        assert_eq!(super::chrome_build_year(&ch("Google Chrome", "152")), Some(2026));
        assert_eq!(super::chrome_build_year(&ch("Google Chrome", "143")), Some(2025));
        assert_eq!(super::chrome_build_year(&ch("Microsoft Edge", "152")), None);
        assert_eq!(super::chrome_build_year(&ch("Brave", "151")), None);
    }

    use super::*;

    // N41, the certificates `incomplete-chain.badssl.com` and its AIA serve (2026-10-14): the leaf names `YR1` at
    // `http://yr1.i.lencr.org/`, which names `Root YR` (cross-signed by ISRG Root X1) at `http://yr.i.lencr.org/`.
    // Each AIA answer is DER; `bundle.p7c` is the two as one PKCS#7, the `.p7c` form CAs also serve.
    #[test]
    fn aia_urls_and_bodies_are_read_as_chromium_reads_them() {
        let fx = |n: &str| std::fs::read(format!("{}/tests/fixtures/aia/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(ca_issuers_urls(&fx("leaf.der")), vec!["http://yr1.i.lencr.org/".to_string()]);
        assert_eq!(ca_issuers_urls(&fx("yr1.der")), vec!["http://yr.i.lencr.org/".to_string()]);
        // a DER certificate is itself; a PKCS#7 bundle is its certificates, each whole
        assert_eq!(aia_certificates(&fx("yr1.der")), vec![fx("yr1.der")]);
        assert_eq!(aia_certificates(&fx("bundle.p7c")), vec![fx("yr1.der"), fx("root-yr-x1.der")]);
        // text is nothing (Chromium reads no PEM from AIA)
        assert!(aia_certificates(b"-----BEGIN CERTIFICATE-----").is_empty());
        // a root is never learned: a self-signed CA a server's AIA names is self-issued; an intermediate, and a root
        // CROSS-SIGNED by another (Root YR under ISRG Root X1), are not
        assert!(wreq::tls::trust::is_self_issued_der(&fx("selfsigned-ca.der")));
        assert!(!wreq::tls::trust::is_self_issued_der(&fx("yr1.der")));
        assert!(!wreq::tls::trust::is_self_issued_der(&fx("root-yr-x1.der")));
        assert!(!wreq::tls::trust::is_self_issued_der(b"not a certificate"));
    }

    // The process-wide learned list is bounded -- a root is never added, a duplicate never,
    // and nothing once it holds AIA_LEARNED_MAX (a page aiming requests at junk chains cannot grow it without end)
    #[test]
    fn the_learned_list_takes_no_root_no_duplicate_and_stops_at_its_bound() {
        let fx = |n: &str| std::fs::read(format!("{}/tests/fixtures/aia/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut v = Vec::new();
        assert!(!learn_into(&mut v, vec![fx("selfsigned-ca.der")]), "a root is not learned");
        assert!(learn_into(&mut v, vec![fx("yr1.der"), fx("yr1.der")]));
        assert_eq!(v.len(), 1, "a duplicate is not learned");
        assert!(!learn_into(&mut v, vec![fx("yr1.der")]));
        // fill to the bound with stand-ins (the bound is a count; the list holds bytes)
        while v.len() < AIA_LEARNED_MAX {
            let n = v.len();
            v.push(vec![0x30, n as u8, (n >> 8) as u8]);
        }
        assert!(!learn_into(&mut v, vec![fx("root-yr-x1.der")]), "a full list learns nothing more");
        assert_eq!(v.len(), AIA_LEARNED_MAX);
    }

    // A body is BYTES, and its text view is derived from them. The `0..=255` file is the case L14
    // measured: Chrome's arrayBuffer() is 256 bytes with u[128] === 128.
    #[test]
    fn a_response_keeps_its_bytes_and_derives_its_text() {
        let all: Vec<u8> = (0..=255u8).collect();
        let r = FetchResponse::new(200, "OK".into(), "http://x/".into(), vec![], all.clone());
        assert_eq!(r.bytes, all, "the body of record is byte-exact");
        assert_eq!(r.bytes[128], 128);
        assert_eq!(r.body.chars().count(), 256, "the text view is the UTF-8 decode: one char per byte here");
        // The text view is the lossy UTF-8 decode wreq's text() did WITHOUT its `charset` feature: a
        // Content-Type charset is not read and a UTF-16 BOM is not sniffed. Changing that would move
        // every script and module this engine loads (WPT's json-module/charset-bom caught exactly that).
        let latin = FetchResponse::new(
            200,
            "OK".into(),
            "http://x/".into(),
            vec![("Content-Type".into(), "text/plain; charset=ISO-8859-1".into())],
            b"caf\xe9".to_vec(),
        );
        assert_eq!(latin.body, "caf\u{fffd}");
        assert_eq!(latin.bytes, b"caf\xe9");
        let bom16 = FetchResponse::new(200, "OK".into(), "http://x/".into(), vec![], b"\xff\xfeh\x00".to_vec());
        assert_eq!(bom16.body, "\u{fffd}\u{fffd}h\u{0}");
    }

    // Mixed content, row for row the Chrome 150 measurement in the "Mixed content" section.
    #[test]
    fn mixed_content_upgrades_images_and_media_and_blocks_the_rest() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let https_page = u("https://mixed.test/page.html");
        let d = |s: &str| Destination::from(s);
        let up = |s: &str| MixedContent::Upgrade(u(s));
        assert_eq!(mixed_content(Some(&https_page), &u("http://img.test/a.png"), &d("image"), Mode::NoCors), up("https://img.test/a.png"));
        assert_eq!(mixed_content(Some(&https_page), &u("http://img.test:8080/p.png"), &d("image"), Mode::NoCors), up("https://img.test:8080/p.png"));
        assert_eq!(mixed_content(Some(&https_page), &u("http://img.test/v.mp4"), &d("video"), Mode::NoCors), up("https://img.test/v.mp4"));
        assert_eq!(mixed_content(Some(&https_page), &u("http://img.test/au.mp3"), &d("audio"), Mode::NoCors), up("https://img.test/au.mp3"));
        for (dest, mode) in [("script", Mode::NoCors), ("style", Mode::NoCors), ("", Mode::Cors), ("", Mode::NoCors), ("iframe", Mode::Navigate), ("font", Mode::Cors), ("worker", Mode::SameOrigin)] {
            assert_eq!(mixed_content(Some(&https_page), &u("http://img.test/x"), &d(dest), mode), MixedContent::Blocked, "{dest}");
        }
        // Not mixed: a loopback (potentially trustworthy) target, an https target, a plain-http page
        // (even a loopback one), a top-level navigation, and a request no document initiated.
        assert_eq!(mixed_content(Some(&https_page), &u("http://127.0.0.1:8000/s.js"), &d("script"), Mode::NoCors), MixedContent::Allowed);
        assert_eq!(mixed_content(Some(&https_page), &u("https://img.test/s.js"), &d("script"), Mode::NoCors), MixedContent::Allowed);
        assert_eq!(mixed_content(Some(&u("http://127.0.0.1:81/page.html")), &u("http://img.test/s.js"), &d("script"), Mode::NoCors), MixedContent::Allowed);
        assert_eq!(mixed_content(Some(&https_page), &u("http://img.test/"), &d("document"), Mode::Navigate), MixedContent::Allowed);
        assert_eq!(mixed_content(None, &u("http://img.test/s.js"), &d("script"), Mode::NoCors), MixedContent::Allowed);
    }

    // The first hop's upgrade is decided before fetchStart, and it is the decision the hop loop made there:
    // the same rule on the same URL (so the wire is unchanged), for every row of the measurement below.
    #[test]
    fn the_start_checks_decide_the_first_hop_upgrade_before_fetch_start() {
        for (url, dest, mode) in [
            ("http://c4probe-xyz.com/", "document", Mode::Navigate),
            ("http://c4probe-xyz.com:8080/", "document", Mode::Navigate),
            ("http://localhost/", "document", Mode::Navigate),
            ("http://8.8.8.8/", "document", Mode::Navigate),
            ("https://c4probe-xyz.com/", "document", Mode::Navigate),
            ("http://c4probe-xyz.com/a.png", "image", Mode::NoCors),
            ("http://c4probe-xyz.com/", "iframe", Mode::Navigate),
            ("http://c4probe-xyz.com/", "document", Mode::NoCors),
        ] {
            let mut req = FetchRequest::navigation(url);
            req.destination = Destination::from(dest);
            req.mode = mode;
            let before = std::time::Instant::now();
            let checks = start_checks(&req);
            let asked = std::time::Instant::now();
            assert!(before <= checks.at && checks.at <= asked, "{url}: the checks are not before fetchStart");
            let in_loop = https_upgrade(&url::Url::parse(url).unwrap(), &Destination::from(dest), mode);
            assert_eq!(checks.first_upgrade, in_loop, "{url} ({dest}): not the decision the hop loop made");
        }
    }

    // HTTPS-Upgrades, row for row the Chrome 150 measurement in the "HTTPS-Upgrades" section. Each
    // row here was a wire event: a `CONNECT host:443` the logging proxy saw, or its absence.
    #[test]
    fn a_top_level_http_navigation_is_tried_over_https_when_the_host_could_hold_a_certificate() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let nav = |s: &str| https_upgrade(&u(s), &Destination::from("document"), Mode::Navigate);
        // Upgraded: a default-port `http:` navigation to a host a public CA could vouch for.
        assert_eq!(nav("http://c4probe-xyz.com/"), Some(u("https://c4probe-xyz.com/")));
        assert_eq!(nav("http://c4probe-xyz.com:80/"), Some(u("https://c4probe-xyz.com/")));
        assert_eq!(nav("http://sub.deep.c4probe-xyz.co.uk/a?b=c"), Some(u("https://sub.deep.c4probe-xyz.co.uk/a?b=c")));
        assert_eq!(nav("http://8.8.8.8/"), Some(u("https://8.8.8.8/")), "a PUBLIC ip literal is upgraded");
        // Not upgraded: a non-default port, and every flavour of non-unique host.
        assert_eq!(nav("http://c4probe-xyz.com:8080/"), None, "non-default port");
        assert_eq!(nav("http://c4probe.test/"), None, "no registry-known suffix");
        assert_eq!(nav("http://c4probe.example/"), None, "no registry-known suffix");
        assert_eq!(nav("http://c4probe/"), None, "single label");
        assert_eq!(nav("http://com/"), None, "a bare public suffix has no label of its own");
        assert_eq!(nav("http://localhost/"), None);
        assert_eq!(nav("http://127.0.0.1/"), None);
        assert_eq!(nav("http://192.168.77.250/"), None, "private ip");
        assert_eq!(nav("http://192.0.2.77/"), None, "reserved ip (TEST-NET-1)");
        assert_eq!(nav("http://10.1.2.3/"), None);
        assert_eq!(nav("http://169.254.1.1/"), None);
        assert_eq!(nav("http://[::1]/"), None);
        assert_eq!(nav("http://[fe80::1]/"), None);
        assert_eq!(nav("http://[2606:4700:4700::1111]/"), Some(u("https://[2606:4700:4700::1111]/")));
        // Already secure, and every request that is not a top-level document navigation. The
        // asymmetry with mixed content is the point: THAT rule upgrades images on an https page and
        // explicitly exempts a navigation; THIS one upgrades only the navigation.
        assert_eq!(nav("https://c4probe-xyz.com/"), None);
        assert_eq!(https_upgrade(&u("http://c4probe-xyz.com/a.png"), &Destination::from("image"), Mode::NoCors), None);
        assert_eq!(https_upgrade(&u("http://c4probe-xyz.com/s.js"), &Destination::from("script"), Mode::NoCors), None);
        assert_eq!(https_upgrade(&u("http://c4probe-xyz.com/"), &Destination::from("iframe"), Mode::Navigate), None);
        assert_eq!(https_upgrade(&u("http://c4probe-xyz.com/"), &Destination::from("document"), Mode::NoCors), None);
    }

    // One entry per name, Chromium's two rules, a caller's repeated name combined.
    #[test]
    fn a_request_header_list_holds_one_entry_per_name() {
        let mut h = HopHeaders::default();
        h.combine("Accept", "application/json, text/javascript, */*; q=0.01");
        h.combine("X-A", "1");
        h.combine("x-a", "2");
        h.set_if_missing("accept", "*/*");
        h.set("ACCEPT-language", "fr");
        h.set("accept-language", "ko");
        assert_eq!(
            h.0,
            vec![
                ("Accept".to_string(), "application/json, text/javascript, */*; q=0.01".to_string()),
                ("X-A".to_string(), "1, 2".to_string()),
                ("ACCEPT-language".to_string(), "ko".to_string()),
            ],
            "a default never doubles a caller's header; `set` keeps the entry's place and spelling"
        );
        let d = |s: &str| Destination::from(s);
        assert!(!accept_overwrites(&d("")) && !accept_overwrites(&d("script")) && !accept_overwrites(&d("image")));
        assert!(accept_overwrites(&d("style")) && accept_overwrites(&d("json")) && accept_overwrites(&d("xslt")) && accept_overwrites(&d("webbundle")));
        assert_eq!(accept_for_subresource(&d("json")), "application/json,*/*;q=0.5");
        assert_eq!(accept_for_subresource(&d("xslt")), "text/css,*/*;q=0.1");
        assert_eq!(accept_for_subresource(&d("")), "*/*");
    }

    // `SetAcceptEncodingIfMissing`: br and zstd only where the bytes are opaque to a proxy; identity for a Range.
    #[test]
    fn accept_encoding_follows_the_url_and_a_range() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let p = "gzip, deflate, br, zstd";
        assert_eq!(advertised_encodings(p, &u("https://example.com/"), false), p);
        assert_eq!(advertised_encodings(p, &u("http://127.0.0.1:8000/"), false), p);
        assert_eq!(advertised_encodings(p, &u("http://localhost/"), false), p);
        assert_eq!(advertised_encodings(p, &u("http://a.localhost/"), false), p);
        assert_eq!(advertised_encodings(p, &u("http://example.com/"), false), "gzip, deflate");
        assert_eq!(advertised_encodings(p, &u("http://192.168.1.2/"), false), "gzip, deflate");
        assert_eq!(advertised_encodings(p, &u("https://example.com/"), true), "identity");
    }

    // A request body arrives from JS as a string OR a buffer; both land in the one byte field.
    #[test]
    fn a_request_body_is_a_string_or_bytes() {
        let s: FetchRequest = serde_json::from_str(r#"{"url":"http://x/","body":"hé"}"#).unwrap();
        assert_eq!(s.body.as_deref(), Some("h\u{e9}".as_bytes()));
        let n: FetchRequest = serde_json::from_str(r#"{"url":"http://x/","body":null}"#).unwrap();
        assert_eq!(n.body, None);
        let absent: FetchRequest = serde_json::from_str(r#"{"url":"http://x/"}"#).unwrap();
        assert_eq!(absent.body, None);
        let bytes =
            string_or_bytes(serde::de::value::BytesDeserializer::<serde::de::value::Error>::new(&[0, 255])).unwrap();
        assert_eq!(bytes, Some(vec![0, 255]));
    }

    // document.cookie: NON-HttpOnly cookies are readable and scoped to the host; an HttpOnly cookie a
    // response set rides requests but must NOT appear in document.cookie -- a sensor that sets one and
    // finds it in document.cookie has caught a non-browser -- and a script cannot set one at all.
    #[test]
    fn document_cookie_excludes_httponly_and_scopes_by_host() {
        reset_cookies();
        let url = "https://www.example-korvus.test/path";
        set_document_cookie(url, "visible=1; Path=/");
        store_response_cookie(url, "secret=2; Path=/; HttpOnly");
        set_document_cookie(url, "scripted=3; Path=/; HttpOnly");
        let dc = document_cookie(url);
        assert_eq!(dc, "visible=1", "the HttpOnly cookie is hidden, the script's refused");

        // A cookie for a different site is not visible here.
        set_document_cookie("https://other-korvus.test/", "elsewhere=3");
        assert!(!document_cookie(url).contains("elsewhere=3"), "cross-site cookie must not leak");

        // Domain= cookie covers subdomains.
        reset_cookies();
        set_document_cookie("https://example-korvus.test/", "shared=9; Domain=example-korvus.test");
        assert!(document_cookie("https://www.example-korvus.test/").contains("shared=9"), "Domain cookie covers subdomain");
        reset_cookies();
    }

    // ONE test, not three, ON PURPOSE: the ledger is a process global (like the cookie jar above) and
    // `cargo test` runs test functions on parallel threads, so three functions resetting one store
    // would interleave and fail each other intermittently -- the worst kind of red.
    #[test]
    fn the_request_ledger_records_entry_and_completion() {
        // (1) A request that FAILED is a row with no status and an error -- not a missing row. This
        // is the distinction the whole resource-set comparison rests on: "Korvus never asked for it"
        // and "Korvus asked and the fetch blew up" are different findings, and a ledger that only
        // records successes reports the second as the first.
        let l = ledger();
        l.reset();
        let row = l.begin(&FetchRequest::subresource(
            "https://example-korvus.test/x.js",
            "script",
            Mode::NoCors,
            "https://example-korvus.test/",
        ));
        l.finish(row, 7.5, &Err(anyhow::anyhow!("connect timed out")));
        let rows = l.records();
        assert_eq!(rows.len(), 1, "the row exists even though the request failed");
        assert_eq!(rows[0].url, "https://example-korvus.test/x.js");
        assert_eq!(rows[0].kind, "script", "the destination token is the ledger's `type`");
        assert_eq!(rows[0].status, None, "no response means no status, not status 0");
        assert_eq!(rows[0].error.as_deref(), Some("connect timed out"));
        assert_eq!(rows[0].initiator.as_deref(), Some("https://example-korvus.test/"));

        // (2) A successful row carries what §6's `requests[]` compares, and `reset` starts a
        // generation -- so a second navigation's ledger is that navigation's, not both.
        l.reset();
        let row = l.begin(&FetchRequest::navigation("https://example-korvus.test/"));
        // A redirect it followed is on the row, in order, with its own response (N27).
        l.redirect(
            row,
            RedirectHop {
                url: "https://example-korvus.test/".into(),
                status: 302,
                status_text: "Found".into(),
                headers: vec![("location".into(), "/moved".into())],
                next_url: "https://example-korvus.test/moved".into(),
                next_method: "GET".into(),
                at_ms: 0.0,
            },
        );
        l.finish(
            row,
            12.5,
            &Ok(FetchResponse::new(
                200,
                "OK".into(),
                "https://example-korvus.test/moved".into(),
                vec![("content-type".into(), "text/html".into())],
                b"hello".to_vec(),
            )),
        );
        let rows = l.records();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, Some(200));
        assert_eq!(rows[0].bytes, 5, "bytes is the body's byte length");
        assert_eq!(rows[0].kind, "document");
        assert_eq!(rows[0].final_url, "https://example-korvus.test/moved", "a redirect moves final_url, not url");
        assert_eq!(rows[0].url, "https://example-korvus.test/");
        assert_eq!(rows[0].hops.len(), 1, "the redirect is on the row");
        assert_eq!((rows[0].hops[0].status, rows[0].hops[0].next_url.as_str()), (302, "https://example-korvus.test/moved"));
        assert_eq!(rows[0].status_text, "OK");
        assert!(!rows[0].from_cache, "a fetched answer is a network row, not a cache one");
        assert_eq!(rows[0].headers.len(), 1);
        assert_eq!(rows[0].ms, 12.5, "`ms` is the wire time the caller passed in, not entry-to-return");

        l.reset();
        assert!(l.records().is_empty(), "a new generation starts empty");
        assert_eq!(l.dropped(), 0);

        // (3) Ids are monotonic from 1 within a generation: the CDP `requestId` is minted from this,
        // and a client joins `requestWillBeSent`/`responseReceived`/`loadingFinished` by it.
        for _ in 0..3 {
            let row = l.begin(&FetchRequest::navigation("https://example-korvus.test/"));
            l.finish(row, 0.0, &Err(anyhow::anyhow!("x")));
        }
        assert_eq!(l.records().iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        l.reset();
        let row = l.begin(&FetchRequest::navigation("https://example-korvus.test/"));
        l.finish(row, 0.0, &Err(anyhow::anyhow!("x")));
        assert_eq!(l.records()[0].id, 1, "reset restarts the ids with the generation");
        l.reset();
    }
}

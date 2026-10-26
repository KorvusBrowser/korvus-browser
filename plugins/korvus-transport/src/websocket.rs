//! The WebSocket client -- Chromium 150's `net/websockets` + `services/network/websocket*.cc`, on the
//! SAME wire as every other request this engine makes.
//!
//! WHY IT LIVES HERE. A WebSocket handshake is an HTTP/1.1 request, and a `wss://` one opens with a
//! ClientHello. Both are fingerprints, so both must be the persona's: the handshake goes out through a
//! `wreq` client built from the persona's own preset ([`ws_client_for`]: BoringSSL, the same TLS
//! options but Chrome's WebSocket ALPN, the shared cookie store, a pool of its own as Chrome's WebSocket
//! socket pool is) and only the bytes AFTER `101 Switching Protocols` are this file's framing. Measured on stock
//! Chrome 150.0.7871.46 against `core/korvus-runtime/tests/fixtures/websocket/ws_server.py` (three
//! recordings, identical): the request is
//!
//! ```text
//! GET /echo HTTP/1.1
//! Host: 127.0.0.1:PORT
//! Connection: Upgrade
//! Pragma: no-cache
//! Cache-Control: no-cache
//! User-Agent: Mozilla/5.0 (X11; Linux x86_64) ... Chrome/150.0.0.0 Safari/537.36
//! Upgrade: websocket
//! Origin: http://127.0.0.1:PORT
//! Sec-WebSocket-Version: 13
//! Accept-Encoding: gzip, deflate, br, zstd
//! Accept-Language: ...
//! Cookie: ...                                   (when the jar has one)
//! Sec-WebSocket-Key: <16 random bytes, base64>
//! Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits
//! Sec-WebSocket-Protocol: p1, p2                (when protocols were given)
//! ```
//!
//! -- no `sec-ch-ua*`, no `Sec-Fetch-*`, no `Priority`. That is the order `net::WebSocketStream`
//! (`websocket_stream.cc:161`) + `HttpNetworkTransaction`'s defaults + `WebSocketBasicHandshakeStream::
//! SendRequest` (`websocket_basic_handshake_stream.cc:278`, the key and the two extension headers last)
//! produce, and [`WS_HEADER_ORDER`] states it once.
//!
//! THE ALPN. A FRESH `wss://` connection offers `http/1.1` ONLY: `ClientSocketPool` builds websocket
//! connect jobs with `AlpnMode::kHttp11Only` (`net/socket/client_socket_pool.cc:250`), because "not all
//! HTTP/2 servers implement RFC 8441" (`http_stream_factory_job.cc:1036`). wreq does the same thing for
//! a request pinned to `Version::HTTP_11` (`wreq tls/conn.rs:156`, ALPN from the request's version), and
//! the version is also part of its pool key, so a WebSocket never rides a pooled h2 connection either.
//! RFC 8441 (WebSocket over an EXISTING h2 session that advertised SETTINGS_ENABLE_CONNECT_PROTOCOL) is
//! the one Chrome path this file does not take; see the report's risk list.
//!
//! WHAT IS CHROME'S, ONE PLACE EACH:
//!   * the per-renderer throttle (`services/network/websocket_throttler.cc`) -- [`Throttler`]
//!   * the restricted ports (`net/base/port_util.cc` kRestrictedPorts) -- [`port_allowed`]
//!   * handshake validation and its failure sentences (`websocket_basic_handshake_stream.cc`,
//!     `websocket_handshake_stream_base.cc`, `websocket_deflate_parameters.cc`) -- [`validate_response`]
//!   * the channel state machine: ping/pong, the closing handshake and its 60 s / 2 s timers, protocol
//!     failures and their close codes (`net/websockets/websocket_channel.cc`) -- [`Channel`]
//!   * permessage-deflate (`websocket_deflate_stream.cc`, `websocket_deflater.cc`) -- [`Deflate`]

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// RFC 6455's magic GUID for `Sec-WebSocket-Accept`.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `kClosingHandshakeTimeoutSeconds` (websocket_channel.cc:61): how long we wait for the server's
/// Close after sending ours.
const CLOSING_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// `kUnderlyingConnectionCloseTimeoutSeconds` (websocket_channel.cc:69): how long we wait for the
/// server to close TCP once both Close frames have crossed.
const UNDERLYING_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// `kHandshakeTimeoutIntervalInSeconds` (websocket_stream.cc): the opening handshake's bound.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(240);
/// `kSmallMessageThreshold` (services/network/websocket.cc:64): a message up to this size goes out as
/// ONE frame; a larger one is framed as the data pipe hands it over.
const SMALL_MESSAGE_THRESHOLD: usize = 1 << 16;
/// `kReceiveDataPipeCapacity` (services/network/websocket.cc:69, non-Android): the size of the data
/// pipe both directions use, and so the largest chunk a large message is framed in.
pub const DATA_PIPE_CAPACITY: usize = 131_000;

/// The handshake headers Chrome emits, in its wire order and its wire CASE (HTTP/1.1 keeps both).
/// `Cookie` is added by the shared jar; everything else by [`connect`].
const WS_HEADER_ORDER: &[&str] = &[
    "Host",
    "Connection",
    "Pragma",
    "Cache-Control",
    "User-Agent",
    "Upgrade",
    "Origin",
    "Sec-WebSocket-Version",
    "Accept-Encoding",
    "Accept-Language",
    "Cookie",
    "Sec-WebSocket-Key",
    "Sec-WebSocket-Extensions",
    "Sec-WebSocket-Protocol",
];

/// The extension offer, verbatim (`WebSocketStream::CreateAndConnectStream` builds it from
/// `WebSocketDeflateParameters` with `client_max_window_bits` and no value).
const EXTENSION_OFFER: &str = "permessage-deflate; client_max_window_bits";

// ---- the request and what comes back ------------------------------------------------------------

/// A WebSocket the page asked for, already through Blink's checks (`WebSocketCommon::Connect`): the
/// URL is a valid `ws:`/`wss:` URL with no fragment and the protocols are valid, distinct tokens.
#[derive(Debug, Clone)]
pub struct WsRequest {
    pub url: String,
    pub protocols: Vec<String>,
    /// The serialised origin of the document (or worker) that made the socket -- the `Origin` header.
    pub origin: String,
    /// The persona's identity headers ([`super::persona_headers`]); only `user-agent` and
    /// `accept-language` reach the handshake, as in Chrome.
    pub headers: Vec<(String, String)>,
    /// The persona's wire. A request without one is refused, exactly as [`super::fetch`] refuses.
    pub wire: Option<spec::TlsProfile>,
}

impl WsRequest {
    /// Stamp a persona's identity onto this request -- headers and wire from ONE persona, as
    /// [`super::apply_persona_identity`] does for a fetch.
    pub fn worn_by(mut self, id: &super::PersonaIdentity) -> Self {
        self.headers = id.headers.clone();
        self.wire = Some(id.wire.clone());
        self
    }
}

/// One thing the network side tells the page side, in order. These are the calls
/// `network::mojom::WebSocketClient` / `WebSocketHandshakeClient` make into Blink.
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    /// `OnConnectionEstablished`: the selected subprotocol and the accepted extensions string.
    Open { protocol: String, extensions: String },
    /// A complete text message (already validated as UTF-8).
    Text(String),
    /// A complete binary message.
    Binary(Vec<u8>),
    /// The network took this many bytes of queued message data off the send pipe (the trigger of
    /// Blink's `DidConsumeBufferedAmount` for data that did not fit the pipe at `send()` time).
    Drained(u64),
    /// `OnClosingHandshake`: the server's Close frame arrived and ours went back.
    ClosingHandshake,
    /// `OnDropChannel`: the connection is gone.
    Closed { was_clean: bool, code: u16, reason: String },
    /// `OnFailure` + the mojo disconnect: the connection FAILED. The string is the console message's
    /// tail (`WebSocket connection to '<url>' failed: <this>`).
    Failed(String),
}

/// One thing the page side asks of the network side.
#[derive(Debug)]
pub enum WsCommand {
    /// `WebSocket::SendMessage` -- a whole message.
    Send { text: bool, data: Vec<u8> },
    /// `StartClosingHandshake(code, reason)`. `1005` means "a Close frame with no payload", which is
    /// how Blink asks for `close()` without a code (`websocket_channel_impl.cc` Close()).
    Close { code: u16, reason: String },
}

/// The page side's end of one socket. Dropping `commands` (the realm went away, the context was
/// destroyed, `close()` was called during CONNECTING) tears the connection down without a Close
/// frame -- what `WebSocketChannelImpl::Disconnect` does to its mojo pipes.
pub struct WsHandle {
    pub commands: mpsc::UnboundedSender<WsCommand>,
    pub events: mpsc::UnboundedReceiver<WsEvent>,
}

/// Open a WebSocket. Returns at once; the handshake runs on the transport runtime and its outcome
/// arrives as the first [`WsEvent`] (`Open` or `Failed`).
pub fn open(req: WsRequest) -> WsHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let id = NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    devtools_event(id, DevtoolsWs::Created { url: req.url.clone() });
    super::net_runtime().spawn(async move {
        run(req, id, cmd_rx, evt_tx).await;
    });
    WsHandle { commands: cmd_tx, events: evt_rx }
}

// ---- what DevTools is told (`Network.webSocket*`) ----------------------------------------------------

/// Each socket's number, for DevTools to name it by.
static NEXT_SOCKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One thing a socket did, as Chrome's `Network.webSocket*` events report it.
#[derive(Debug, Clone)]
pub enum DevtoolsWs {
    /// `webSocketCreated`.
    Created { url: String },
    /// `webSocketWillSendHandshakeRequest`: the handshake's headers, as sent.
    WillSendHandshake { headers: Vec<(String, String)> },
    /// `webSocketHandshakeResponseReceived`.
    HandshakeResponse { status: u16, status_text: String, headers: Vec<(String, String)> },
    /// `webSocketFrameSent` / `webSocketFrameReceived`: a whole message, `opcode` 1 (text) or 2 (binary).
    Frame { sent: bool, opcode: u8, payload: Vec<u8> },
    /// `webSocketFrameError`.
    Error { message: String },
    /// `webSocketClosed`.
    Closed,
}

/// Are the sockets' events kept for a DevTools client ([`capture_devtools`])? Off: nothing is kept.
static CAPTURE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn devtools_log() -> &'static Mutex<Vec<(u64, Instant, DevtoolsWs)>> {
    static L: OnceLock<Mutex<Vec<(u64, Instant, DevtoolsWs)>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// A client's `Network` domain on or off: the sockets' events are kept for it, or dropped.
pub fn capture_devtools(on: bool) {
    CAPTURE.store(on, std::sync::atomic::Ordering::Relaxed);
    if !on {
        devtools_log().lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Socket `id` did `ev`, now. Kept only while a client captures, and at most a bounded number wait.
fn devtools_event(id: u64, ev: DevtoolsWs) {
    if !CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut l = devtools_log().lock().unwrap_or_else(|e| e.into_inner());
    if l.len() < 10_000 {
        l.push((id, Instant::now(), ev));
    }
    drop(l);
    super::ledger().touch();
}

/// Every socket event since the last call: the socket's number, when, and what.
pub fn take_devtools_events() -> Vec<(u64, Instant, DevtoolsWs)> {
    std::mem::take(&mut *devtools_log().lock().unwrap_or_else(|e| e.into_inner()))
}

// ---- the throttle (services/network/websocket_throttler.cc) --------------------------------------

/// `WebSocketPerProcessThrottler`. Chrome keeps one per renderer process; this engine is one renderer,
/// so there is one per process.
struct Throttler {
    pending: i64,
    current_succeeded: i64,
    previous_succeeded: i64,
    current_failed: i64,
    previous_failed: i64,
    /// When the 2-minute period (`throttling_period_timer_`) last rolled.
    rolled_at: Instant,
}

/// `kMaxPendingWebSocketConnections`.
const MAX_PENDING: i64 = 255;
/// The throttling period (`throttling_period_timer_.Start(FROM_HERE, base::Minutes(2), ...)`).
const THROTTLE_PERIOD: Duration = Duration::from_secs(120);

fn throttler() -> &'static Mutex<Throttler> {
    static T: OnceLock<Mutex<Throttler>> = OnceLock::new();
    T.get_or_init(|| {
        Mutex::new(Throttler {
            pending: 0,
            current_succeeded: 0,
            previous_succeeded: 0,
            current_failed: 0,
            previous_failed: 0,
            rolled_at: Instant::now(),
        })
    })
}

impl Throttler {
    /// `Roll()`, as many times as whole periods have passed since the last one.
    fn roll_if_due(&mut self) {
        while self.rolled_at.elapsed() >= THROTTLE_PERIOD {
            self.previous_succeeded = self.current_succeeded;
            self.previous_failed = self.current_failed;
            self.current_succeeded = 0;
            self.current_failed = 0;
            self.rolled_at += THROTTLE_PERIOD;
        }
    }

    /// `CalculateDelay()`: `RandInt(1000, 5000) * 2^min(p + f / (s + 1), 16) / 65536` milliseconds.
    fn delay(&self) -> Duration {
        let f = self.previous_failed + self.current_failed;
        let s = self.previous_succeeded + self.current_succeeded;
        let p = self.pending;
        let exp = (p + f / (s + 1)).min(16).max(0) as u32;
        let r = 1000 + (random_u32() % 4001) as u64;
        Duration::from_micros(r * (1u64 << exp) * 1000 / 65536)
    }
}

/// `PendingConnection`: counts as pending until the handshake completes; dropped without completing,
/// it counts as a failure.
struct PendingConnection {
    completed: bool,
}

impl PendingConnection {
    fn issue() -> Option<(PendingConnection, Duration)> {
        let mut t = throttler().lock().ok()?;
        t.roll_if_due();
        if t.pending >= MAX_PENDING {
            return None;
        }
        // The delay is computed BEFORE this connection is counted (`WebSocketFactory::CreateWebSocket`
        // passes `CalculateDelay(process_id)` after `IssuePendingConnectionTracker`, whose constructor
        // has already incremented -- so the new connection IS in `p`).
        t.pending += 1;
        let d = t.delay();
        Some((PendingConnection { completed: false }, d))
    }

    fn complete(&mut self) {
        if self.completed {
            return;
        }
        self.completed = true;
        if let Ok(mut t) = throttler().lock() {
            t.roll_if_due();
            t.pending -= 1;
            t.current_succeeded += 1;
        }
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Ok(mut t) = throttler().lock() {
            t.roll_if_due();
            t.pending -= 1;
            t.current_failed += 1;
        }
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    // An entropy failure is not survivable for a handshake key or a mask, but it is also not
    // something a page can provoke; fall back to the clock rather than panic on the net runtime.
    if getrandom::getrandom(&mut b).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        for (i, x) in b.iter_mut().enumerate() {
            *x = (t >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(151);
        }
    }
    b
}

fn random_u32() -> u32 {
    u32::from_le_bytes(random_bytes::<4>())
}

// ---- ports (net/base/port_util.cc) ---------------------------------------------------------------

/// `kRestrictedPorts`, Chromium 150. A request to one of these fails with `net::ERR_UNSAFE_PORT`
/// before any socket is opened (`IsPortAllowedForScheme`).
const RESTRICTED_PORTS: &[u16] = &[
    0, 1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101, 102, 103,
    104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427, 465, 512, 513, 514,
    515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990, 993, 995, 1719, 1720, 1723,
    2049, 3659, 4045, 5060, 5061, 6000, 6566, 6665, 6666, 6667, 6668, 6669, 6697, 10080,
];

/// Chrome's `IsPortAllowedForScheme` for the http(s) schemes a WebSocket is fetched as.
pub fn port_allowed(port: u16) -> bool {
    !RESTRICTED_PORTS.contains(&port)
}

// ---- the handshake --------------------------------------------------------------------------------

/// `base64(sha1(key + GUID))`.
fn accept_for(key: &str) -> String {
    use base64::Engine as _;
    use sha1::Digest as _;
    let mut h = sha1::Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

/// `ws:` -> `http:`, `wss:` -> `https:` (`net::ChangeWebSocketSchemeToHttpScheme`).
fn http_url_for(ws: &url::Url) -> Option<url::Url> {
    let mut u = ws.clone();
    let scheme = match ws.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    // `set_scheme` refuses special<->special changes only when the port/host shape would break; ws and
    // http are both special schemes, so this succeeds.
    u.set_scheme(scheme).ok()?;
    Some(u)
}

/// Chrome's `net::ErrorToString` for the connection failures a page can provoke, read off the error
/// chain wreq hands back. The fallback is the generic `net::ERR_FAILED`, which is also what Chrome
/// reports for a failure it has no better name for.
pub(crate) fn net_error_name(e: &wreq::Error) -> &'static str {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            match io.kind() {
                ConnectionRefused => return "net::ERR_CONNECTION_REFUSED",
                ConnectionReset => return "net::ERR_CONNECTION_RESET",
                ConnectionAborted => return "net::ERR_CONNECTION_ABORTED",
                TimedOut => return "net::ERR_CONNECTION_TIMED_OUT",
                AddrNotAvailable => return "net::ERR_ADDRESS_INVALID",
                UnexpectedEof => return "net::ERR_CONNECTION_CLOSED",
                _ => {}
            }
        }
        src = s.source();
    }
    let text = format!("{e:?}").to_ascii_lowercase();
    if e.is_dns() || text.contains("dns error") || text.contains("failed to lookup") {
        return "net::ERR_NAME_NOT_RESOLVED";
    }
    if text.contains("certificate") || text.contains("cert verify") {
        return "net::ERR_CERT_AUTHORITY_INVALID";
    }
    if e.is_tls() || text.contains("ssl") || text.contains("handshake") {
        return "net::ERR_SSL_PROTOCOL_ERROR";
    }
    if e.is_timeout() {
        return "net::ERR_TIMED_OUT";
    }
    if e.is_connect() {
        return "net::ERR_CONNECTION_FAILED";
    }
    "net::ERR_FAILED"
}

/// What the handshake response settled.
struct Accepted {
    protocol: String,
    extensions: String,
    deflate: Option<DeflateParams>,
}

/// The values of every `name` header, split on commas the way `HttpResponseHeaders::EnumerateHeader`
/// splits them.
fn header_values(headers: &wreq::header::HeaderMap, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for v in headers.get_all(name) {
        let s = String::from_utf8_lossy(v.as_bytes()).into_owned();
        for part in s.split(',') {
            let p = part.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// The whole header lines (not comma-split) -- `EnumerateHeaderLines`-shaped, for the extensions
/// header whose value is itself a comma list Chrome parses as one.
fn header_lines(headers: &wreq::header::HeaderMap, name: &str) -> Vec<String> {
    headers.get_all(name).iter().map(|v| String::from_utf8_lossy(v.as_bytes()).trim().to_string()).collect()
}

/// `WebSocketBasicHandshakeStream::ValidateUpgradeResponse`, in Chrome's order, with Chrome's
/// sentences. `Err` is the tail after "Error during WebSocket handshake: ".
fn validate_response(
    headers: &wreq::header::HeaderMap,
    expected_accept: &str,
    requested: &[String],
) -> Result<Accepted, String> {
    let single = |name: &str| -> Result<String, String> {
        let v = header_values(headers, name);
        match v.len() {
            0 => Err(format!("'{name}' header is missing")),
            1 => Ok(v[0].clone()),
            _ => Err(format!("'{name}' header must not appear more than once in a response")),
        }
    };
    let upgrade = single("Upgrade")?;
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(format!("'Upgrade' header value is not 'WebSocket': {upgrade}"));
    }
    let accept = single("Sec-WebSocket-Accept")?;
    if accept != expected_accept {
        return Err("Incorrect 'Sec-WebSocket-Accept' header value".to_string());
    }
    let connection = header_values(headers, "Connection");
    if connection.is_empty() {
        return Err("'Connection' header is missing".to_string());
    }
    if !connection.iter().any(|t| t.eq_ignore_ascii_case("upgrade")) {
        return Err("'Connection' header value must contain 'Upgrade'".to_string());
    }
    // `ValidateSubProtocol`.
    let offered = header_values(headers, "Sec-WebSocket-Protocol");
    let mut protocol: Option<String> = None;
    for v in offered {
        if protocol.is_some() {
            return Err("'Sec-WebSocket-Protocol' header must not appear more than once in a response".to_string());
        }
        if requested.is_empty() {
            return Err(format!("Response must not include 'Sec-WebSocket-Protocol' header if not present in request: {v}"));
        }
        if !requested.iter().any(|r| *r == v) {
            return Err(format!("'Sec-WebSocket-Protocol' header value '{v}' in response does not match any of sent values"));
        }
        protocol = Some(v);
    }
    if !requested.is_empty() && protocol.is_none() {
        return Err("Sent non-empty 'Sec-WebSocket-Protocol' header but no response was received".to_string());
    }
    // `ValidateExtensions`.
    let mut accepted_lines: Vec<String> = Vec::new();
    let mut deflate: Option<DeflateParams> = None;
    for line in header_lines(headers, "Sec-WebSocket-Extensions") {
        let Some(exts) = parse_extensions(&line) else {
            return Err(format!("'Sec-WebSocket-Extensions' header value is rejected by the parser: {line}"));
        };
        for (name, params) in exts {
            if name != "permessage-deflate" {
                return Err(format!("Found an unsupported extension '{name}' in 'Sec-WebSocket-Extensions' header"));
            }
            if deflate.is_some() {
                return Err("Received duplicate permessage-deflate response".to_string());
            }
            deflate = Some(DeflateParams::from_response(&params).map_err(|m| format!("Error in permessage-deflate: {m}"))?);
        }
        accepted_lines.push(line);
    }
    Ok(Accepted { protocol: protocol.unwrap_or_default(), extensions: accepted_lines.join(", "), deflate })
}

/// `ParseWebSocketExtensions`: `ext *( "," ext )`, `ext = token *( ";" param )`,
/// `param = token [ "=" ( token / quoted-string ) ]`. `None` when the parser rejects the value.
fn parse_extensions(v: &str) -> Option<Vec<(String, Vec<(String, Option<String>)>)>> {
    fn is_token(s: &str) -> bool {
        !s.is_empty()
            && s.bytes().all(|c| c > 0x20 && c < 0x7f && !b"()<>@,;:\\\"/[]?={}".contains(&c))
    }
    let mut out = Vec::new();
    for ext in v.split(',') {
        let mut parts = ext.split(';');
        let name = parts.next()?.trim();
        if !is_token(name) {
            return None;
        }
        let mut params = Vec::new();
        for p in parts {
            let p = p.trim();
            match p.split_once('=') {
                None => {
                    if !is_token(p) {
                        return None;
                    }
                    params.push((p.to_string(), None));
                }
                Some((k, val)) => {
                    let k = k.trim();
                    let mut val = val.trim().to_string();
                    if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                        val = val[1..val.len() - 1].to_string();
                    }
                    if !is_token(k) || !is_token(&val) {
                        return None;
                    }
                    params.push((k.to_string(), Some(val)));
                }
            }
        }
        out.push((name.to_string(), params));
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// The negotiated permessage-deflate parameters (`WebSocketDeflateParameters`, response side).
#[derive(Debug, Clone, Copy)]
struct DeflateParams {
    server_no_context_takeover: bool,
    client_no_context_takeover: bool,
    /// `client_max_window_bits`'s value when the server set one (8..=15); 15 otherwise.
    client_max_window_bits: u8,
}

impl DeflateParams {
    /// `Initialize` + `IsValidAsResponse`, with their failure sentences.
    fn from_response(params: &[(String, Option<String>)]) -> Result<DeflateParams, String> {
        let window = |v: &Option<String>| -> Option<u8> {
            let v = v.as_deref()?;
            if v.is_empty() || v.starts_with('0') || !v.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n: u32 = v.parse().ok()?;
            (8..=15).contains(&n).then_some(n as u8)
        };
        let (mut snct, mut cnct, mut smwb, mut cmwb) = (false, false, false, None::<Option<u8>>);
        for (k, v) in params {
            match k.as_str() {
                "server_no_context_takeover" => {
                    if snct {
                        return Err(format!("Received duplicate permessage-deflate extension parameter {k}"));
                    }
                    if v.is_some() {
                        return Err(format!("Received invalid {k} parameter"));
                    }
                    snct = true;
                }
                "client_no_context_takeover" => {
                    if cnct {
                        return Err(format!("Received duplicate permessage-deflate extension parameter {k}"));
                    }
                    if v.is_some() {
                        return Err(format!("Received invalid {k} parameter"));
                    }
                    cnct = true;
                }
                "server_max_window_bits" => {
                    if smwb {
                        return Err(format!("Received duplicate permessage-deflate extension parameter {k}"));
                    }
                    if window(v).is_none() {
                        return Err(format!("Received invalid {k} parameter"));
                    }
                    smwb = true;
                }
                "client_max_window_bits" => {
                    if cmwb.is_some() {
                        return Err(format!("Received duplicate permessage-deflate extension parameter {k}"));
                    }
                    match v {
                        None => cmwb = Some(None),
                        Some(_) => match window(v) {
                            Some(b) => cmwb = Some(Some(b)),
                            None => return Err(format!("Received invalid {k} parameter")),
                        },
                    }
                }
                _ => return Err("Received an unexpected permessage-deflate extension parameter".to_string()),
            }
        }
        // `IsValidAsResponse`: a response may not echo the valueless form.
        if let Some(None) = cmwb {
            return Err("client_max_window_bits must have value".to_string());
        }
        Ok(DeflateParams {
            server_no_context_takeover: snct,
            client_no_context_takeover: cnct,
            client_max_window_bits: cmwb.flatten().unwrap_or(15),
        })
    }
}

/// The OrigHeaderMap that puts [`WS_HEADER_ORDER`] on the wire in Chrome's order and case.
fn ws_order() -> wreq::header::OrigHeaderMap {
    let mut m = wreq::header::OrigHeaderMap::new();
    for n in WS_HEADER_ORDER {
        m.insert(*n);
    }
    m
}

/// The WebSocket client for one wire, and the preset header value the handshake carries.
#[derive(Clone)]
struct WsClient {
    client: wreq::Client,
    /// The preset's own `Accept-Encoding` -- the value every other request on this wire sends, so a
    /// WebSocket cannot advertise a different coding set than the page's fetches. (`Accept-Language`
    /// is the persona's, not the preset's: it rides in on [`WsRequest::headers`], as on every fetch.)
    accept_encoding: Option<String>,
}

type WsClients = Mutex<std::collections::HashMap<(String, String), WsClient>>;
static WS_CLIENTS: OnceLock<WsClients> = OnceLock::new();

/// Drop every cached WebSocket client, so the next handshake builds one on the current trust store -- called when
/// AIA taught the process an intermediate ([`super::chase_aia`]), as the page clients are.
pub(crate) fn forget_clients() {
    if let Some(m) = WS_CLIENTS.get() {
        if let Ok(mut m) = m.lock() {
            m.clear();
        }
    }
}

/// The WebSocket clients, one per wire, built on first use (the shape of [`super::client_for`]'s
/// table, and keyed the same way).
///
/// A CLIENT OF ITS OWN, for two of Chrome's facts:
///   * the WEBSOCKET SOCKET POOL is separate (`HttpNetworkSession::SocketPoolType::kWebSocket`): a
///     WebSocket never rides, and never leaves behind, a connection the page's fetches use;
///   * its ClientHello offers ALPN `http/1.1` ONLY (`AlpnMode::kHttp11Only`) and therefore NO ALPS:
///     Chrome adds an `application_settings` entry only for a protocol in its ALPN list
///     (`SSLClientSocketImpl`), and BoringSSL sends whatever ALPS it is configured with
///     (`ext_alps_add_clienthello_impl` checks only that SOME ALPN list exists) -- so the page
///     client's preset, which configures ALPS for `h2`, would put an `h2` ALPS extension beside an
///     `http/1.1`-only ALPN, a ClientHello no Chrome sends.
/// Everything else is the page client's: the same preset's TLS options WITH the same corrections
/// ([`super::correct_preset_tls`] -- ML-DSA sigalgs, ALPS codepoint, the bundle's trust anchors), the same
/// trust store ([`super::trust_store`]), the same HTTP/1 options, the same cookie store (each handshake
/// states its cookies, [`handshake`]) and the same connect bound.
fn ws_client_for(wire: &spec::TlsProfile) -> anyhow::Result<WsClient> {
    let key = (wire.id.clone(), wire.platform.clone());
    let clients = WS_CLIENTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut map = clients.lock().map_err(|_| anyhow::anyhow!("the WebSocket client table is poisoned"))?;
    if let Some(c) = map.get(&key) {
        return Ok(c.clone());
    }
    use wreq::IntoEmulation;
    let em = wreq_util::Emulation::builder()
        .profile(super::emulation_for(&wire.id)?)
        .platform(super::platform_for(&wire.platform))
        .build()
        .into_emulation();
    let accept_encoding = em.headers.get("accept-encoding").and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut tls = em.tls_options;
    if let Some(t) = tls.as_mut() {
        super::correct_preset_tls(t, wire)?;
        t.alpn_protocols = Some(std::borrow::Cow::Borrowed(&[wreq::tls::AlpnProtocol::HTTP1]));
        t.alps_protocols = None;
    }
    // The connect path asks `ech` for each host's config, as the page client's does.
    wreq::tls::set_ech_config_provider(super::ech::cached);
    let mut b = wreq::Client::builder()
        .tls_options(tls)
        .http1_options(em.http1_options)
        .connect_timeout(Duration::from_secs(15))
        .redirect(wreq::redirect::Policy::none());
    if let Some(store) = super::trust_store()? {
        b = b.tls_cert_store(store);
    }
    let c = WsClient { client: b.build()?, accept_encoding };
    map.insert(key, c.clone());
    Ok(c)
}

/// The opening handshake. `Ok` carries the upgraded stream; `Err` the failure message tail.
async fn handshake(req: &WsRequest, ws_url: &url::Url, id: u64) -> Result<(wreq::Upgraded, Accepted), String> {
    let wire = req.wire.clone().ok_or_else(|| "Error in connection establishment: net::ERR_FAILED".to_string())?;
    let ws = ws_client_for(&wire).map_err(|_| "Error in connection establishment: net::ERR_FAILED".to_string())?;
    let http = http_url_for(ws_url).ok_or_else(|| "Error in connection establishment: net::ERR_FAILED".to_string())?;

    use base64::Engine as _;
    let key = base64::engine::general_purpose::STANDARD.encode(random_bytes::<16>());
    let expected = accept_for(&key);

    let mut rb = ws
        .client
        .request(wreq::Method::GET, http.as_str())
        .version(wreq::Version::HTTP_11)
        // The preset's defaults are a NAVIGATION's header set (sec-ch-ua*, accept, the fetch
        // metadata); a WebSocket handshake carries none of them.
        .default_headers(false)
        .orig_headers(ws_order())
        .timeout(HANDSHAKE_TIMEOUT)
        .read_timeout(HANDSHAKE_TIMEOUT)
        .header("Connection", "Upgrade")
        .header("Pragma", "no-cache")
        .header("Cache-Control", "no-cache");
    // The store's cookies for the handshake, and the store for its response's: in the SameSite context and partition
    // of the document that opened the socket, as far as its origin tells them.
    let origin = url::Url::parse(&req.origin).ok();
    rb = rb.cookie_provider(std::sync::Arc::new(super::cookies::ExchangeCookies(super::cookies::HopContext::for_websocket(origin.as_ref(), &http))));
    if let Some((_, ua)) = req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        rb = rb.header("User-Agent", ua.as_str());
    }
    rb = rb.header("Upgrade", "websocket").header("Origin", req.origin.as_str()).header("Sec-WebSocket-Version", "13");
    if let Some(v) = ws.accept_encoding.as_deref() {
        rb = rb.header("Accept-Encoding", v);
    }
    // The persona's, as on every fetch (`persona_headers`): the preset's copy is `en-US,en;q=0.9` for every
    // Chromium preset and contradicted a Korean persona's `navigator.languages` on the one request that read it.
    if let Some((_, v)) = req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("accept-language")) {
        rb = rb.header("Accept-Language", v.as_str());
    }
    rb = rb.header("Sec-WebSocket-Key", key.as_str()).header("Sec-WebSocket-Extensions", EXTENSION_OFFER);
    if !req.protocols.is_empty() {
        rb = rb.header("Sec-WebSocket-Protocol", req.protocols.join(", "));
    }
    if CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
        // what DevTools shows of the request: the headers this handshake set (the jar's `Cookie` is added on the way)
        let mut shown: Vec<(String, String)> = vec![
            ("Connection".into(), "Upgrade".into()),
            ("Pragma".into(), "no-cache".into()),
            ("Cache-Control".into(), "no-cache".into()),
        ];
        if let Some((_, ua)) = req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
            shown.push(("User-Agent".into(), ua.clone()));
        }
        shown.push(("Upgrade".into(), "websocket".into()));
        shown.push(("Origin".into(), req.origin.clone()));
        shown.push(("Sec-WebSocket-Version".into(), "13".into()));
        shown.push(("Sec-WebSocket-Key".into(), key.clone()));
        shown.push(("Sec-WebSocket-Extensions".into(), EXTENSION_OFFER.into()));
        if !req.protocols.is_empty() {
            shown.push(("Sec-WebSocket-Protocol".into(), req.protocols.join(", ")));
        }
        devtools_event(id, DevtoolsWs::WillSendHandshake { headers: shown });
    }

    let resp = rb
        .send()
        .await
        .map_err(|e| format!("Error in connection establishment: {}", net_error_name(&e)))?;

    // Any `Set-Cookie` is in the store already (the handshake's `ExchangeCookies`).
    let status = resp.status().as_u16();
    if CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
        let headers = resp.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
        devtools_event(id, DevtoolsWs::HandshakeResponse { status, status_text, headers });
    }
    if status != 101 {
        // `websocket_basic_handshake_stream.cc:479`: anything but 101 (a redirect included -- a
        // WebSocket does not follow one).
        return Err(format!("Error during WebSocket handshake: Unexpected response code: {status}"));
    }
    let accepted = validate_response(resp.headers(), &expected, &req.protocols)
        .map_err(|m| format!("Error during WebSocket handshake: {m}"))?;
    let up = resp
        .upgrade()
        .await
        .map_err(|_| "Connection closed before receiving a handshake response".to_string())?;
    Ok((up, accepted))
}

/// The socket's whole life on the transport runtime.
async fn run(mut req: WsRequest, id: u64, mut cmd_rx: mpsc::UnboundedReceiver<WsCommand>, evt_tx: mpsc::UnboundedSender<WsEvent>) {
    let fail = |m: String| {
        devtools_event(id, DevtoolsWs::Error { message: m.clone() });
        devtools_event(id, DevtoolsWs::Closed);
        let _ = evt_tx.send(WsEvent::Failed(m));
    };
    // The network plugins (`plugin_net`, goal 1020 step 2) see the handshake as a `websocket` request: they may refuse
    // it, send it to another url, or change its headers.
    if plugin_host::has_network() {
        let mut asked = super::plugin_net::Hop::new(&req.url, "GET", &req.headers, "websocket", None);
        if plugin_host::on_request(&mut asked) == plugin_host::Verdict::Block {
            fail("Error in connection establishment: net::ERR_BLOCKED_BY_CLIENT".to_string());
            return;
        }
        if asked.headers_changed {
            req.headers = asked.headers.clone();
        }
        if let Some(u) = asked.new_url.take() {
            req.url = u;
        }
    }
    // `WebSocketFactory::CreateWebSocket`: too many pending -> "Insufficient resources".
    let Some((mut pending, delay)) = PendingConnection::issue() else {
        fail("Insufficient resources".to_string());
        return;
    };
    let Ok(ws_url) = url::Url::parse(&req.url) else {
        fail("Error in connection establishment: net::ERR_INVALID_URL".to_string());
        return;
    };
    let port = ws_url.port_or_known_default().unwrap_or(0);

    // `WebSocket::WebSocket`: a positive throttle delay posts `AddChannel` that far in the future. The
    // page may give up meanwhile (a dropped command sender), and then nothing is sent at all.
    let connect = async {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if !port_allowed(port) {
            return Err("Error in connection establishment: net::ERR_UNSAFE_PORT".to_string());
        }
        handshake(&req, &ws_url, id).await
    };
    let outcome = tokio::select! {
        r = connect => Some(r),
        // `recv` answering `None` is the page dropping the socket; a command during CONNECTING cannot
        // happen (Blink throws for `send`, and `close` drops the sender), so either way: abandon.
        _ = cmd_rx.recv() => None,
    };
    let Some(outcome) = outcome else { return };
    let (upgraded, accepted) = match outcome {
        Ok(v) => v,
        Err(m) => {
            fail(m);
            return;
        }
    };
    pending.complete();
    drop(pending);
    if evt_tx
        .send(WsEvent::Open { protocol: accepted.protocol.clone(), extensions: accepted.extensions.clone() })
        .is_err()
    {
        return;
    }
    let deflate = accepted.deflate.map(Deflate::new);
    Channel::new(upgraded, deflate, evt_tx, req.url.clone(), id).run(cmd_rx).await;
    devtools_event(id, DevtoolsWs::Closed);
}

// ---- framing (RFC 6455 section 5) -------------------------------------------------------------------

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// A frame as the server sent it.
#[derive(Debug)]
struct Frame {
    fin: bool,
    rsv1: bool,
    rsv2: bool,
    rsv3: bool,
    masked: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// What the reader task hands the channel: a frame, a framing violation, or the end of the stream.
enum Read {
    Frame(Frame),
    /// `ERR_WS_PROTOCOL_ERROR` from the frame parser.
    ProtocolError,
    /// The server closed TCP (`ERR_CONNECTION_CLOSED`).
    Eof,
    /// Any other read failure (a reset).
    Error,
}

/// The largest message this client assembles. Chrome's bound is `wtf_size_t`'s maximum; this one is
/// the same order and keeps a hostile server from making the process allocate without limit.
const MAX_MESSAGE: usize = u32::MAX as usize / 2;

/// `WebSocketFrameParser`: header, extended length (minimal encoding required), mask, payload.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Read {
    let mut h = [0u8; 2];
    match r.read_exact(&mut h).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Read::Eof,
        Err(_) => return Read::Error,
    }
    let fin = h[0] & 0x80 != 0;
    let rsv1 = h[0] & 0x40 != 0;
    let rsv2 = h[0] & 0x20 != 0;
    let rsv3 = h[0] & 0x10 != 0;
    let opcode = h[0] & 0x0F;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        if r.read_exact(&mut b).await.is_err() {
            return Read::Eof;
        }
        len = u16::from_be_bytes(b) as u64;
        if len <= 125 {
            return Read::ProtocolError;
        }
    } else if len == 127 {
        let mut b = [0u8; 8];
        if r.read_exact(&mut b).await.is_err() {
            return Read::Eof;
        }
        len = u64::from_be_bytes(b);
        if len <= 0xFFFF || len >> 63 != 0 {
            return Read::ProtocolError;
        }
    }
    // Control frames: at most 125 bytes and never fragmented (RFC 6455 5.5).
    if opcode >= 0x8 && (len > 125 || !fin) {
        return Read::ProtocolError;
    }
    if len as usize > MAX_MESSAGE {
        return Read::ProtocolError;
    }
    let mut key = [0u8; 4];
    if masked && r.read_exact(&mut key).await.is_err() {
        return Read::Eof;
    }
    let mut payload = vec![0u8; len as usize];
    if r.read_exact(&mut payload).await.is_err() {
        return Read::Eof;
    }
    if masked {
        apply_mask(&mut payload, key);
    }
    Read::Frame(Frame { fin, rsv1, rsv2, rsv3, masked, opcode, payload })
}

fn apply_mask(data: &mut [u8], key: [u8; 4]) {
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i & 3];
    }
}

/// One client frame, masked with a fresh key (`GenerateWebSocketMaskingKey`, crypto-random).
fn encode_frame(fin: bool, rsv1: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push((if fin { 0x80 } else { 0 }) | (if rsv1 { 0x40 } else { 0 }) | opcode);
    let n = payload.len();
    if n <= 125 {
        out.push(0x80 | n as u8);
    } else if n <= 0xFFFF {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let key = random_bytes::<4>();
    out.extend_from_slice(&key);
    let start = out.len();
    out.extend_from_slice(payload);
    apply_mask(&mut out[start..], key);
    out
}

// ---- permessage-deflate (RFC 7692) ----------------------------------------------------------------

/// The four bytes a sync flush ends with, which RFC 7692 7.2.1 strips and 7.2.2 puts back.
const SYNC_TAIL: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF];

struct Deflate {
    params: DeflateParams,
    /// `None` when the server limited our window below 15: this build's deflate backend (miniz_oxide)
    /// always uses a 32 KiB window, so a smaller negotiated one cannot be honoured -- messages then go
    /// out uncompressed, which RFC 7692 permits (RSV1 is per message).
    compress: Option<flate2::Compress>,
    decompress: flate2::Decompress,
}

impl Deflate {
    fn new(params: DeflateParams) -> Deflate {
        let compress = (params.client_max_window_bits == 15).then(|| flate2::Compress::new(flate2::Compression::default(), false));
        Deflate { params, compress, decompress: flate2::Decompress::new(false) }
    }

    /// `WebSocketDeflater`: raw deflate, sync flush, the tail stripped. An EMPTY message is one 0x00
    /// byte (`WebSocketDeflater::Finish` with no bytes added), not an empty payload.
    fn deflate(&mut self, input: &[u8]) -> Option<Vec<u8>> {
        let c = self.compress.as_mut()?;
        if input.is_empty() {
            if self.params.client_no_context_takeover {
                c.reset();
            }
            return Some(vec![0x00]);
        }
        let mut out: Vec<u8> = Vec::with_capacity(input.len() / 2 + 64);
        let mut consumed = 0usize;
        loop {
            if out.capacity() - out.len() < 64 {
                out.reserve(out.capacity().max(1024));
            }
            let before = c.total_in();
            c.compress_vec(&input[consumed..], &mut out, flate2::FlushCompress::Sync).ok()?;
            consumed += (c.total_in() - before) as usize;
            // The flush is complete when all input is in and the call left output space unused.
            if consumed >= input.len() && out.len() < out.capacity() {
                break;
            }
        }
        if out.ends_with(&SYNC_TAIL) {
            out.truncate(out.len() - 4);
        }
        if self.params.client_no_context_takeover {
            c.reset();
        }
        Some(out)
    }

    /// `WebSocketInflater`: the tail put back, raw inflate to completion. `None` is a protocol error.
    fn inflate(&mut self, compressed: &[u8]) -> Option<Vec<u8>> {
        let mut input = Vec::with_capacity(compressed.len() + 4);
        input.extend_from_slice(compressed);
        input.extend_from_slice(&SYNC_TAIL);
        let d = &mut self.decompress;
        let mut out: Vec<u8> = Vec::with_capacity(compressed.len() * 3 + 64);
        let mut consumed = 0usize;
        loop {
            if out.capacity() - out.len() < 1024 {
                out.reserve(out.capacity().max(4096));
            }
            if out.len() > MAX_MESSAGE {
                return None;
            }
            let before = d.total_in();
            let produced_before = out.len();
            let st = d.decompress_vec(&input[consumed..], &mut out, flate2::FlushDecompress::Sync).ok()?;
            consumed += (d.total_in() - before) as usize;
            if st == flate2::Status::StreamEnd {
                break;
            }
            if consumed >= input.len() && out.len() < out.capacity() {
                break;
            }
            if consumed >= input.len() && out.len() == produced_before {
                break;
            }
            // No input taken and no output made with room to spare: the stream cannot advance
            // (corrupt data the backend did not report as an error). Fail rather than spin.
            if d.total_in() == before && out.len() == produced_before && out.len() < out.capacity() {
                return None;
            }
        }
        if self.params.server_no_context_takeover {
            self.decompress.reset(false);
        }
        Some(out)
    }
}

// ---- the channel (net/websockets/websocket_channel.cc) ---------------------------------------------

/// `WebSocketChannel::State`, the subset after the handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Connected,
    /// We sent Close, waiting for the server's.
    SendClosed,
    /// The server sent Close and we have answered; waiting for TCP to close.
    CloseWait,
    Closed,
}

struct Channel<W> {
    writer: W,
    reader: tokio::task::JoinHandle<()>,
    frames: mpsc::Receiver<Read>,
    deflate: Option<Deflate>,
    events: mpsc::UnboundedSender<WsEvent>,
    /// The socket's URL, for the observer event a plugin reads (plugin ABI 3, D8).
    url: String,
    state: State,
    /// The data message being assembled: its opcode, whether it is compressed, and its bytes.
    assembling: Option<(u8, bool, Vec<u8>)>,
    received_close: Option<(u16, String)>,
    deadline: Option<tokio::time::Instant>,
    /// The socket's number, for what DevTools is told ([`devtools_event`]).
    id: u64,
}

impl Channel<tokio::io::WriteHalf<wreq::Upgraded>> {
    fn new(up: wreq::Upgraded, deflate: Option<Deflate>, events: mpsc::UnboundedSender<WsEvent>, url: String, id: u64) -> Self {
        let (mut rd, wr) = tokio::io::split(up);
        // The reader is its own task so a frame half-read is never lost to a `select!` branch: the
        // channel only ever sees whole frames. A bounded queue is the network service's back-pressure
        // (`kReceiveDataPipeCapacity`): the reader stops reading the socket when the page lags.
        let (tx, rx) = mpsc::channel::<Read>(64);
        let reader = tokio::spawn(async move {
            loop {
                let r = read_frame(&mut rd).await;
                let last = !matches!(r, Read::Frame(_));
                if tx.send(r).await.is_err() || last {
                    return;
                }
            }
        });
        Channel {
            writer: wr,
            reader,
            frames: rx,
            deflate,
            events,
            state: State::Connected,
            assembling: None,
            received_close: None,
            deadline: None,
            url,
            id,
        }
    }
}

impl<W: AsyncWrite + Unpin> Channel<W> {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<WsCommand>) {
        let mut commands_open = true;
        while self.state != State::Closed {
            let sleep_until = self.deadline;
            tokio::select! {
                c = commands.recv(), if commands_open => match c {
                    Some(WsCommand::Send { text, data }) => self.send_message(text, data).await,
                    Some(WsCommand::Close { code, reason }) => self.start_closing_handshake(code, reason).await,
                    // The page side went away without a closing handshake: drop the connection.
                    None => { commands_open = false; self.abandon().await; }
                },
                r = self.frames.recv() => match r {
                    Some(Read::Frame(f)) => self.handle_frame(f).await,
                    Some(Read::ProtocolError) => self.fail_channel("Invalid frame header", 1002, "WebSocket Protocol Error").await,
                    Some(Read::Eof) | None => self.on_read_end(true).await,
                    Some(Read::Error) => self.on_read_end(false).await,
                },
                _ = async { match sleep_until { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending::<()>().await } } => {
                    self.close_timeout().await;
                }
            }
        }
        self.reader.abort();
        let _ = self.writer.shutdown().await;
    }

    fn emit(&self, e: WsEvent) {
        let _ = self.events.send(e);
    }

    async fn write(&mut self, bytes: &[u8]) -> bool {
        self.writer.write_all(bytes).await.is_ok() && self.writer.flush().await.is_ok()
    }

    /// `WebSocket::SendMessage` -> `WebSocketChannel::SendFrame` -> the deflate stream: one message.
    /// A message up to [`SMALL_MESSAGE_THRESHOLD`] is one frame; a larger one goes out in
    /// [`DATA_PIPE_CAPACITY`] pieces, which is how the network service reads it off the pipe.
    async fn send_message(&mut self, text: bool, data: Vec<u8>) {
        // `SendFrame` in a closing state is silently dropped (`InClosingState()`).
        if self.state != State::Connected {
            return;
        }
        let len = data.len() as u64;
        let opcode = if text { OP_TEXT } else { OP_BINARY };
        if CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
            devtools_event(self.id, DevtoolsWs::Frame { sent: true, opcode, payload: data.clone() });
        }
        let compressed = self.deflate.as_mut().and_then(|d| d.deflate(&data));
        let (rsv1, payload) = match compressed {
            Some(c) => (true, c),
            None => (false, data),
        };
        let mut ok = true;
        if payload.len() <= SMALL_MESSAGE_THRESHOLD {
            ok = self.write(&encode_frame(true, rsv1, opcode, &payload)).await;
        } else {
            let mut first = true;
            let chunks: Vec<&[u8]> = payload.chunks(DATA_PIPE_CAPACITY).collect();
            let last = chunks.len() - 1;
            for (i, chunk) in chunks.iter().enumerate() {
                let op = if first { opcode } else { OP_CONT };
                let frame = encode_frame(i == last, rsv1 && first, op, chunk);
                first = false;
                if !self.write(&frame).await {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            self.emit(WsEvent::Drained(len));
        } else {
            self.on_read_end(false).await;
        }
    }

    /// `WebSocketChannel::StartClosingHandshake`.
    async fn start_closing_handshake(&mut self, code: u16, reason: String) {
        if self.state != State::Connected {
            return;
        }
        if let Some((rc, rr)) = self.received_close.clone() {
            // The server's Close is already in; answering it IS our closing handshake.
            return self.respond_to_closing_handshake(rc, rr).await;
        }
        self.deadline = Some(tokio::time::Instant::now() + CLOSING_HANDSHAKE_TIMEOUT);
        // Javascript permits only 1000 and 3000-4999 and Blink has checked the reason's length; an
        // invalid pair here means a broken renderer and is sent as 1011 (`IsStrictlyValidCloseStatusCode`).
        let (code, reason) = if code != 1005 && !strictly_valid_close_code(code) || reason.len() > 123 {
            (1011, String::new())
        } else {
            (code, reason)
        };
        if !self.send_close(code, &reason).await {
            return self.on_read_end(false).await;
        }
        self.state = State::SendClosed;
    }

    /// `SendClose`: 1005 is "no status", an empty payload.
    async fn send_close(&mut self, code: u16, reason: &str) -> bool {
        let mut p = Vec::new();
        if code != 1005 {
            p.extend_from_slice(&code.to_be_bytes());
            p.extend_from_slice(reason.as_bytes());
        }
        self.write(&encode_frame(true, false, OP_CLOSE, &p)).await
    }

    async fn handle_frame(&mut self, f: Frame) {
        if f.masked {
            return self.fail_channel("A server must not mask any frames that it sends to the client.", 1002, "Masked frame from server").await;
        }
        // RSV1 on the FIRST frame of a data message is permessage-deflate's; anywhere else it is the
        // channel's "reserved bit" error (the deflate stream only clears it where it owns it).
        let rsv1_owned = self.deflate.is_some() && (f.opcode == OP_TEXT || f.opcode == OP_BINARY);
        if (f.rsv1 && !rsv1_owned) || f.rsv2 || f.rsv3 {
            let m = format!(
                "One or more reserved bits are on: reserved1 = {}, reserved2 = {}, reserved3 = {}",
                f.rsv1 as u8, f.rsv2 as u8, f.rsv3 as u8
            );
            return self.fail_channel(&m, 1002, "Invalid reserved bit").await;
        }
        if self.state == State::CloseWait {
            let name = match f.opcode {
                OP_CONT | OP_TEXT | OP_BINARY => "Data frame",
                OP_PING => "Ping",
                OP_PONG => "Pong",
                OP_CLOSE => "Close",
                _ => "Unknown frame type",
            };
            return self.fail_channel(&format!("{name} received after close"), 1002, "").await;
        }
        match f.opcode {
            OP_CONT | OP_TEXT | OP_BINARY => self.handle_data_frame(f).await,
            OP_PING => {
                if self.state == State::Connected && !self.write(&encode_frame(true, false, OP_PONG, &f.payload)).await {
                    self.on_read_end(false).await;
                }
            }
            OP_PONG => {}
            OP_CLOSE => match parse_close(&f.payload) {
                Ok((code, reason)) => self.handle_close_frame(code, reason).await,
                Err((code, reason, message)) => self.fail_channel(&message, code, &reason).await,
            },
            op => self.fail_channel(&format!("Unrecognized frame opcode: {op}"), 1002, "Unknown opcode").await,
        }
    }

    /// `HandleDataFrame`: continuation bookkeeping, UTF-8 validation, assembly.
    async fn handle_data_frame(&mut self, f: Frame) {
        if self.state != State::Connected || self.received_close.is_some() {
            return; // "Ignored data packet received in state ..."
        }
        let continuation = f.opcode == OP_CONT;
        if continuation != self.assembling.is_some() {
            let (log, reason) = if continuation {
                ("Received unexpected continuation frame.", "Unexpected continuation")
            } else {
                ("Received start of new message but previous message is unfinished.", "Previous data frame unfinished")
            };
            return self.fail_channel(log, 1002, reason).await;
        }
        if !continuation {
            self.assembling = Some((f.opcode, f.rsv1, Vec::new()));
        } else if f.rsv1 {
            // RSV1 on a continuation frame (`WebSocketDeflateStream::Inflate`).
            return self.fail_channel("Invalid frame header", 1002, "WebSocket Protocol Error").await;
        }
        if let Some((_, _, buf)) = self.assembling.as_mut() {
            if buf.len() + f.payload.len() > MAX_MESSAGE {
                return self.fail_channel("Message size is too large.", 1009, "").await;
            }
            buf.extend_from_slice(&f.payload);
        }
        if !f.fin {
            return;
        }
        let Some((opcode, compressed, mut data)) = self.assembling.take() else { return };
        if compressed {
            match self.deflate.as_mut().and_then(|d| d.inflate(&data)) {
                Some(d) => data = d,
                None => return self.fail_channel("Invalid frame header", 1002, "WebSocket Protocol Error").await,
            }
        }
        if opcode == OP_TEXT {
            match String::from_utf8(data) {
                Ok(s) => {
                    self.observe_message("text", s.len(), Some(&s));
                    if CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
                        devtools_event(self.id, DevtoolsWs::Frame { sent: false, opcode: OP_TEXT, payload: s.as_bytes().to_vec() });
                    }
                    self.emit(WsEvent::Text(s))
                }
                Err(_) => {
                    // 1002, not 1007: `HandleDataFrame` fails with kWebSocketErrorProtocolError, and the
                    // fixture's server log shows Chrome's Close frame carrying exactly 1002 + this reason.
                    return self.fail_channel("Could not decode a text frame as UTF-8.", 1002, "Invalid UTF-8 in text frame").await
                }
            }
        } else {
            self.observe_message("binary", data.len(), None);
            if CAPTURE.load(std::sync::atomic::Ordering::Relaxed) {
                devtools_event(self.id, DevtoolsWs::Frame { sent: false, opcode: OP_BINARY, payload: data.clone() });
            }
            self.emit(WsEvent::Binary(data));
        }
    }

    /// A complete incoming message, to any observer plugin (plugin ABI 3, D8): read-only -- the message reaches the
    /// page unchanged. Costs nothing when no plugin observes.
    fn observe_message(&self, kind: &str, len: usize, text: Option<&str>) {
        if !plugin_host::has_observer() {
            return;
        }
        let payload = match text {
            Some(s) => serde_json::json!({ "url": self.url, "op": kind, "len": len, "text": s }),
            None => serde_json::json!({ "url": self.url, "op": kind, "len": len }),
        };
        plugin_host::event("websocket.message", &payload.to_string());
    }

    /// `HandleCloseFrame`.
    async fn handle_close_frame(&mut self, code: u16, reason: String) {
        match self.state {
            State::Connected => {
                self.received_close = Some((code, reason.clone()));
                self.respond_to_closing_handshake(code, reason).await;
            }
            State::SendClosed => {
                self.received_close = Some((code, reason));
                self.state = State::CloseWait;
                self.deadline = Some(tokio::time::Instant::now() + UNDERLYING_CLOSE_TIMEOUT);
            }
            _ => {}
        }
    }

    /// `RespondToClosingHandshake`: echo the server's code and reason, then wait (2 s) for TCP to close.
    async fn respond_to_closing_handshake(&mut self, code: u16, reason: String) {
        // The renderer hears of the closing handshake first (`OnClosingHandshake` -> readyState CLOSING).
        let sent = self.send_close(code, &reason).await;
        self.state = State::CloseWait;
        self.deadline = Some(tokio::time::Instant::now() + UNDERLYING_CLOSE_TIMEOUT);
        self.emit(WsEvent::ClosingHandshake);
        if !sent {
            self.on_read_end(false).await;
        }
    }

    /// `OnReadDone` with an error: `eof` is `ERR_CONNECTION_CLOSED`, anything else a reset.
    async fn on_read_end(&mut self, eof: bool) {
        if self.state == State::Closed {
            return;
        }
        self.state = State::Closed;
        match self.received_close.clone() {
            Some((code, reason)) => self.emit(WsEvent::Closed { was_clean: eof, code, reason }),
            None => self.emit(WsEvent::Closed { was_clean: false, code: 1006, reason: String::new() }),
        }
    }

    /// `CloseTimeout`.
    async fn close_timeout(&mut self) {
        self.deadline = None;
        if self.state == State::Closed {
            return;
        }
        self.state = State::Closed;
        match self.received_close.clone() {
            Some((code, reason)) => self.emit(WsEvent::Closed { was_clean: true, code, reason }),
            None => self.emit(WsEvent::Closed { was_clean: false, code: 1006, reason: String::new() }),
        }
    }

    /// `FailChannel`: a Close with `code`/`reason` when we are still CONNECTED, then the connection
    /// is dropped and the page is told it FAILED (error + close 1006 on its side).
    async fn fail_channel(&mut self, message: &str, code: u16, reason: &str) {
        if self.state == State::Connected {
            let _ = self.send_close(code, reason).await;
        }
        self.state = State::Closed;
        self.emit(WsEvent::Failed(message.to_string()));
    }

    /// The page side is gone (the socket was torn down, the realm went away). An ESTABLISHED connection
    /// still says goodbye: `network::WebSocket::~WebSocket` starts a closing handshake with 1001
    /// (Going Away) when the handshake had succeeded, and the connection is dropped right after it.
    /// Measured: Chrome 150's `new WebSocket(u); ws.close()` whose handshake won the race against the
    /// renderer's teardown task sent exactly `Close 1001 ""` (fixture log, all three recordings).
    async fn abandon(&mut self) {
        if self.state == State::Connected {
            let _ = self.send_close(1001, "").await;
        }
        self.state = State::Closed;
    }
}

/// `IsStrictlyValidCloseStatusCode` (websocket_channel.cc:86).
fn strictly_valid_close_code(code: u16) -> bool {
    let c = code as u32;
    !(c < 1000 || c == 1006 || (1014..3000).contains(&c) || c >= 5000)
}

/// `ParseCloseFrame` (websocket_frame.cc:261). `Err` = (code, reason, console message) to fail with.
fn parse_close(p: &[u8]) -> Result<(u16, String), (u16, String, String)> {
    if p.is_empty() {
        return Ok((1005, String::new()));
    }
    if p.len() == 1 {
        return Err((1002, String::new(), "Received a broken close frame with an invalid size of 1 byte.".to_string()));
    }
    let code = u16::from_be_bytes([p[0], p[1]]);
    if code == 1005 || code == 1006 || code == 1015 {
        return Err((1002, String::new(), "Received a broken close frame containing a reserved status code.".to_string()));
    }
    match std::str::from_utf8(&p[2..]) {
        Ok(r) => Ok((code, r.to_string())),
        Err(_) => Err((
            1002,
            "Invalid UTF-8 in Close frame".to_string(),
            "Received a broken close frame containing invalid UTF-8.".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_is_rfc_6455s_example() {
        assert_eq!(accept_for("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_are_masked_and_minimally_encoded() {
        let f = encode_frame(true, false, OP_TEXT, b"hi");
        assert_eq!(f[0], 0x81);
        assert_eq!(f[1], 0x80 | 2);
        let mut p = f[6..].to_vec();
        apply_mask(&mut p, [f[2], f[3], f[4], f[5]]);
        assert_eq!(p, b"hi");
        assert_eq!(encode_frame(true, false, OP_BINARY, &[0u8; 126])[1], 0x80 | 126);
        assert_eq!(encode_frame(true, false, OP_BINARY, &[0u8; 65536])[1], 0x80 | 127);
    }

    #[test]
    fn deflate_round_trips_and_strips_the_tail() {
        let p = DeflateParams { server_no_context_takeover: false, client_no_context_takeover: false, client_max_window_bits: 15 };
        let mut a = Deflate::new(p);
        let mut b = Deflate::new(p);
        for msg in [&b"compress me compress me compress me"[..], b"", b"second message, context kept"] {
            let c = a.deflate(msg).expect("window 15 compresses");
            assert!(!c.ends_with(&SYNC_TAIL));
            assert_eq!(b.inflate(&c).expect("inflates"), msg);
        }
        let small = DeflateParams { client_max_window_bits: 10, ..p };
        assert!(Deflate::new(small).deflate(b"x").is_none(), "a narrowed window sends uncompressed");
    }

    #[test]
    fn deflate_parameters_follow_chrome() {
        let ok = parse_extensions("permessage-deflate; client_max_window_bits=15").unwrap();
        assert_eq!(DeflateParams::from_response(&ok[0].1).unwrap().client_max_window_bits, 15);
        let bare = parse_extensions("permessage-deflate; client_max_window_bits").unwrap();
        assert_eq!(DeflateParams::from_response(&bare[0].1).unwrap_err(), "client_max_window_bits must have value");
        let bad = parse_extensions("permessage-deflate; server_max_window_bits=7").unwrap();
        assert_eq!(DeflateParams::from_response(&bad[0].1).unwrap_err(), "Received invalid server_max_window_bits parameter");
        assert!(parse_extensions("permessage-deflate;;").is_none());
    }

    #[test]
    fn close_codes_and_frames() {
        assert!(strictly_valid_close_code(1000) && strictly_valid_close_code(4999) && strictly_valid_close_code(1013));
        assert!(!strictly_valid_close_code(1006) && !strictly_valid_close_code(999) && !strictly_valid_close_code(2000));
        assert_eq!(parse_close(&[]).unwrap(), (1005, String::new()));
        assert_eq!(parse_close(&[0x0f, 0xa1, b's']).unwrap(), (4001, "s".to_string()));
        assert!(parse_close(&[0x03, 0xed]).is_err(), "1005 on the wire is reserved");
    }

    #[test]
    fn restricted_ports_are_chromes() {
        assert!(!port_allowed(1) && !port_allowed(22) && !port_allowed(6000) && !port_allowed(10080));
        assert!(port_allowed(47) && port_allowed(80) && port_allowed(443) && port_allowed(8080));
    }
}

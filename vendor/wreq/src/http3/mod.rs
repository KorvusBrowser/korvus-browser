//! KORVUS PATCH (H3, owner decision 2026-10-11): HTTP/3 over QUIC v1, decided and spoken as Chrome 150 does.
//!
//! WHY. Chrome talks to a Cloudflare challenge over h3 from its first request (the host's DNS `HTTPS` record says
//! `alpn=h3`), and the a challenge provider token records the transport: korvus's h2 tokens were 794 characters where Chrome's
//! are 730. Measured 2026-10-11 off stock Chrome 150.0.7871.46 against a local
//! recorder, and read in Chromium 150's `net/` and QUICHE.
//!
//! WHEN (`HttpStreamFactory::JobController`, the default path: `kHappyEyeballsV3` is off, `net/base/features.cc:92`):
//!  * an origin's cached Alt-Svc `h3` entry starts an ALTERNATIVE job; with none, a host whose DNS `HTTPS` record
//!    lists `h3` starts a DNS_ALPN_H3 job to the origin itself (`http_stream_factory_job_controller.cc:917-930,
//!    1063-1128`); `--origin-to-force-quic-on` (here [`Http3Options::force_quic_origins`]) makes QUIC the only job;
//!  * the TCP job waits while QUIC is pending: 0 until QUIC has worked on this network, else 1.5 x the server's cached
//!    srtt, 300 ms without one, at most 3 s (`QuicSessionPool::GetTimeDelayForWaitingJob`, `quic_session_pool.cc:
//!    1548-1586`; `http_stream_factory_job.cc:143, 739-750`); a QUIC failure resumes it at once;
//!  * the first job to be ready wins; QUIC winning cancels TCP, TCP winning leaves the QUIC job running to completion,
//!    and a QUIC failure next to a TCP success marks the alternative broken (`:1155-1330`); a request that fails over
//!    QUIC before its response is sent again over TCP, and QUIC is marked broken if that works
//!    (`retry_without_alt_svc_on_quic_errors`, `http_network_transaction.cc:2181-2215`).
//!
//! WHAT (see `crypto.rs`, `tp.rs`, `driver.rs`, `qpack.rs`, and quinn-proto's `korvus_chaos.rs`): QUIC v1, an 8-byte
//! random destination CID and an empty source CID, packet numbers from 1, Initial datagrams of 1250 bytes with QUICHE's
//! chaos protection, Chrome's transport parameters shuffled with GREASE, a TLS 1.3 ClientHello from the persona's
//! BoringSSL configuration with ALPN `h3` and ALPS, the HTTP/3 control stream with sorted SETTINGS + GREASE, QUICHE's
//! QPACK encoder, `:method :authority :scheme :path`, the `priority` header and a PRIORITY_UPDATE.
//!
//! NOT (yet): 0-RTT (Chrome sends safe requests in 0-RTT on a resumed session); connection migration; pooling one
//! session across hosts that share an IP and a certificate; QUIC through a proxy (Chrome has none either).

// The crate denies `missing_docs` and `unused`; this module's public timing / options fields are documented where they
// are filled (`driver.rs`, `quic_presets.rs` in korvus-transport), and `HandshakeData::protocol` is kept for the
// handshake's debug output.
#![allow(missing_docs, dead_code)]

mod altsvc;
mod body;
// BoringSSL's QUIC API is FFI: the one module of the crate allowed `unsafe` (denied everywhere else).
#[allow(unsafe_code)]
pub(crate) mod crypto;
mod driver;
mod huffman;
mod huffman_table;
mod qpack;
#[doc(hidden)]
pub mod testing;
mod tp;

use std::{
    borrow::Cow,
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::Ordering,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, Uri, Version};
use quinn_proto::{ConnectError, ConnectionIdGenerator, EndpointConfig, RandomConnectionIdGenerator, TransportConfig, VarInt};
use tokio::sync::watch;

pub use self::body::ResBody;
pub(crate) use self::driver::Conn;
pub use self::driver::QuicTiming;
use self::{
    altsvc::{AltSvcCache, Alternative},
    driver::{H3Settings, NetworkStats, RequestHead},
};
use crate::{
    conn::descriptor::{ConnectionDescriptor, ConnectionId},
    dns::{Name, Resolve},
    group::Group,
    header::OrigHeaderMap,
    tls::{AlpnProtocol, AlpsProtocol, TlsOptions, TlsVersion, conn::TlsConnector},
};

/// What a persona's HTTP/3 looks like. Every field is a measured fact about the browser the persona wears; the
/// transport fills it from the persona's preset. The comments give Chrome 150's values and where they come from.
#[derive(Debug, Clone)]
pub struct Http3Options {
    /// Every datagram carrying an Initial packet, and the path MTU: 1250 (`kDefaultMaxPacketSize`).
    pub max_packet_size: u16,
    /// `max_udp_payload_size`: 1472 (`kMaxIncomingPacketSize`, `quic_config.cc:1057`).
    pub max_udp_payload_size: u64,
    /// The client's first destination CID: 8 random bytes (`quic_session_pool.cc:2058-2059`).
    pub dcid_len: usize,
    /// The client's source CID: empty (`quic_connection.cc:211-212`).
    pub scid_len: usize,
    /// 30 s (`quic_context.h:69-75`).
    pub max_idle_timeout_ms: u64,
    /// 15 MiB (`quic_context.cc:21-22`).
    pub initial_max_data: u64,
    /// 6 MiB, for each of the three stream windows.
    pub initial_max_stream_data: u64,
    /// 100.
    pub initial_max_streams_bidi: u64,
    /// 103: 100 plus the three static HTTP/3 streams (`quic_session.cc:217-221`).
    pub initial_max_streams_uni: u64,
    /// 65536 (`quic_config.cc:1058`).
    pub max_datagram_frame_size: Option<u64>,
    /// `google_connection_options`: `ORIG` (`kTryQuicByDefault`, `quic_session_pool.cc:2106-2109`).
    pub connection_options: Vec<[u8; 4]>,
    /// `version_information` with a GREASE version (`tls_client_handshaker.cc:301-304`).
    pub version_information: bool,
    /// One reserved transport parameter (`transport_parameters.cc:865-882`).
    pub grease_transport_parameter: bool,
    /// A fresh order every handshake (`transport_parameters.cc:890-895`).
    pub shuffle_transport_parameters: bool,
    /// `initial_round_trip_time` from the server's cached srtt (`quic_session_pool.cc:2229-2262`).
    pub send_initial_rtt: bool,
    /// QUICHE's chaos protection of the Initial packets (`quiche_protocol_flags_list.h:247-249`).
    pub chaos_protection: bool,
    /// The first packet number: 1 (`quic_constants.cc:25-27`).
    pub first_packet_number: u64,
    /// The RTT assumed before any is measured: 100 ms (QUICHE `kInitialRttMs`).
    pub default_initial_rtt_ms: u64,
    /// The handshake's deadline: 10 s (`kMaxTimeForCryptoHandshakeSecs`).
    pub handshake_timeout_ms: u64,
    /// TLS GREASE inside QUIC: off (QUICHE never calls `SSL_set_grease_enabled`; measured: no GREASE value).
    pub tls_grease: bool,
    /// `status_request` and `signed_certificate_timestamp` inside QUIC: neither (measured).
    pub tls_ocsp_stapling: bool,
    pub tls_signed_cert_timestamps: bool,
    /// The extension order permuted per handshake (`SSL_set_permute_extensions`, `tls_client_handshaker.cc:117`).
    pub tls_permute_extensions: bool,
    /// The signature algorithms: BoringSSL's default verify list (QUIC does not override it), spelled out.
    pub tls_sigalgs: Option<String>,
    /// ALPS for `h3` with empty settings (`tls_client_handshaker.cc:278-291`).
    pub alps: bool,
    /// This side's SETTINGS (id, value): QPACK_MAX_TABLE_CAPACITY 65536, MAX_FIELD_SECTION_SIZE 262144,
    /// QPACK_BLOCKED_STREAMS 100, H3_DATAGRAM 1 (`quic_spdy_session.cc:618-658`).
    pub settings: Vec<(u64, u64)>,
    /// A GREASE setting and a GREASE frame on the control stream (`quic_enable_http3_grease_randomness`).
    pub grease_setting: bool,
    pub grease_frame: bool,
    /// The cap on the peer's QPACK table this encoder uses: 64 KiB.
    pub qpack_encoder_max_capacity: u64,
    /// Huffman-coded QPACK strings when shorter, and `cookie` crumbled (`quic_spdy_session.h:659-664`).
    pub huffman: bool,
    pub cookie_crumbling: bool,
    /// A PRIORITY_UPDATE before the HEADERS of a request with a non-default priority (`quic_http_stream.cc:499-510`).
    pub priority_update: bool,
    /// The pseudo-header order: `:method :authority :scheme :path` (`spdy_http_utils.cc:198-237`).
    pub pseudo_header_order: Vec<String>,
    /// Use a DNS `HTTPS` record's `alpn` to race QUIC on the first request (`use_dns_https_svcb_alpn = true`).
    pub use_dns_https_alpn: bool,
    /// The TCP job's wait with no srtt cached (300 ms), the multiplier on a cached one (1.5), the cap (3 s).
    pub main_job_default_delay_ms: u64,
    pub main_job_srtt_multiplier: f64,
    pub main_job_max_delay_ms: u64,
    /// `--origin-to-force-quic-on` (`host:port`, or `*`): QUIC is the only job, and the certificate need not chain to a
    /// known root (`proof_verifier_chromium.cc:383-389`).
    pub force_quic_origins: Vec<String>,
    /// QUIC only for forced origins: set when the trust store carries a root the system does not (Chrome's QUIC
    /// refuses a certificate that does not chain to a known root, `ERR_QUIC_CERT_ROOT_NOT_KNOWN`).
    pub only_forced_origins: bool,
}

/// Where a host's DNS `HTTPS` record's `alpn` list comes from (the transport's resolver; `None` = no record).
pub type HttpsAlpnProvider = fn(&str) -> Option<Vec<String>>;

static HTTPS_ALPN: OnceLock<HttpsAlpnProvider> = OnceLock::new();

/// Install the `HTTPS`-record ALPN provider (once per process; a second call is ignored).
pub fn set_https_alpn_provider(provider: HttpsAlpnProvider) {
    let _ = HTTPS_ALPN.set(provider);
}

/// The request's header order, kept beside the one handed to the HTTP/1 and HTTP/2 encoders (which keep theirs in
/// a private extension).
#[derive(Clone)]
pub(crate) struct HeaderOrder(pub(crate) OrigHeaderMap);

/// The connection a response rode, when it was HTTP/3: its phases, and whether it was already open (every stream
/// but the session's first).
#[derive(Debug, Clone, Copy)]
pub struct QuicConnection {
    pub timing: QuicTiming,
    pub reused: bool,
}

/// One request's plan: the QUIC destination to race and how long the TCP job waits.
#[derive(Clone)]
pub(crate) struct Plan {
    origin: String,
    partition: String,
    dest: Alternative,
    sni_host: String,
    descriptor: ConnectionDescriptor,
    key: ConnectionId,
    pub(crate) main_delay: Duration,
    pub(crate) forced: bool,
}

pub(crate) enum SendError {
    /// Nothing of a response arrived: the request may go again over TCP.
    BeforeResponse(String),
    Other(String),
}

type ConnResult = Result<Arc<Conn>, String>;

/// The client's HTTP/3 side: the Alt-Svc cache, the QUIC sessions and what Chrome remembers about servers.
pub(crate) struct H3Client {
    opts: Http3Options,
    tls: TlsConnector,
    resolver: Arc<dyn Resolve>,
    /// The client's proxies: a request one of them intercepts never goes over QUIC.
    proxies: Vec<crate::proxy::Matcher>,
    altsvc: AltSvcCache,
    sessions: Mutex<HashMap<ConnectionId, Arc<Conn>>>,
    pending: Mutex<HashMap<ConnectionId, watch::Receiver<Option<ConnResult>>>>,
    stats: NetworkStats,
    endpoint_config: Arc<EndpointConfig>,
    token_store: Arc<dyn quinn_proto::TokenStore>,
}

impl Http3Options {
    /// The persona's TCP TLS options, as QUICHE configures the same BoringSSL for QUIC.
    pub(crate) fn quic_tls_options(&self, base: Option<&TlsOptions>) -> TlsOptions {
        let mut t = base.cloned().unwrap_or_default();
        t.alpn_protocols = Some(Cow::Borrowed(&[AlpnProtocol::HTTP3]));
        t.alps_protocols = self.alps.then_some(Cow::Borrowed(&[AlpsProtocol::HTTP3][..]));
        t.min_tls_version = Some(TlsVersion::TLS_1_3);
        t.max_tls_version = Some(TlsVersion::TLS_1_3);
        t.grease_enabled = Some(self.tls_grease);
        t.enable_ocsp_stapling = self.tls_ocsp_stapling;
        t.enable_signed_cert_timestamps = self.tls_signed_cert_timestamps;
        t.permute_extensions = Some(self.tls_permute_extensions);
        if let Some(s) = &self.tls_sigalgs {
            t.sigalgs_list = Some(Cow::Owned(s.clone()));
        }
        t.extension_permutation = None;
        // Chrome 152's QUIC ClientHello leads its signature_algorithms with no GREASE, unlike its TCP one
        // (`tests/h3_wire.rs`'s capture)
        t.grease_sigalgs_enabled = None;
        t.record_size_limit = None;
        t.delegated_credentials = None;
        t
    }

    fn h3_settings(&self) -> H3Settings {
        H3Settings {
            settings: self.settings.clone(),
            grease_setting: self.grease_setting,
            grease_frame: self.grease_frame,
            qpack_encoder_max_capacity: self.qpack_encoder_max_capacity,
            huffman: self.huffman,
            cookie_crumbling: self.cookie_crumbling,
            priority_update: self.priority_update,
        }
    }
}

fn is_forced(list: &[String], host: &str, port: u16) -> bool {
    let hp = format!("{host}:{port}");
    list.iter().any(|o| o == "*" || o.eq_ignore_ascii_case(&hp))
}

impl H3Client {
    pub(crate) fn new(opts: Http3Options, tls: TlsConnector, resolver: Arc<dyn Resolve>, proxies: Vec<crate::proxy::Matcher>) -> H3Client {
        let mut ec = EndpointConfig::new(Arc::new(crypto::HmacKey::random()));
        let scid_len = opts.scid_len;
        ec.cid_generator(move || Box::new(RandomConnectionIdGenerator::new(scid_len)));
        ec.grease_quic_bit(false);
        let _ = ec.max_udp_payload_size(opts.max_udp_payload_size.min(u64::from(u16::MAX)) as u16);
        ec.supported_versions(vec![crypto::QUIC_V1]);
        H3Client {
            opts,
            tls,
            resolver,
            proxies,
            altsvc: AltSvcCache::default(),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            stats: NetworkStats::default(),
            endpoint_config: Arc::new(ec),
            token_store: Arc::new(quinn_proto::TokenMemoryCache::default()),
        }
    }

    /// The QUIC job a request to `uri` would race, if any (`JobController::DoCreateJobs`).
    pub(crate) fn plan(&self, uri: &Uri, group: &Group) -> Option<Plan> {
        if uri.scheme_str() != Some("https") || self.proxies.iter().any(|p| p.intercept(uri).is_some()) {
            return None;
        }
        let host = uri.host()?.trim_start_matches('[').trim_end_matches(']').to_string();
        let port = uri.port_u16().unwrap_or(443);
        let forced = is_forced(&self.opts.force_quic_origins, &host, port);
        if self.opts.only_forced_origins && !forced {
            return None;
        }
        let partition = format!("{group:?}");
        let origin = format!("https://{}:{port}", uri.host()?);
        let now = Instant::now();
        let dest = if forced {
            Alternative { host: host.clone(), port }
        } else if let Some(alt) = self.altsvc.quic_for(&origin, &host, port, &partition, now) {
            alt
        } else if self.opts.use_dns_https_alpn
            && HTTPS_ALPN.get().and_then(|p| p(&host)).is_some_and(|alpn| alpn.iter().any(|a| a == "h3"))
        {
            let alt = Alternative { host: host.clone(), port };
            if self.altsvc.broken.lock().ok()?.is_broken(&alt, &partition, now) {
                return None;
            }
            alt
        } else {
            return None;
        };
        let origin_uri: Uri = origin.parse().ok()?;
        let descriptor = ConnectionDescriptor::new(origin_uri, group.clone(), None, Some(Version::HTTP_3), None, None);
        let key = descriptor.id();
        let main_delay = if forced || !self.stats.quic_worked.load(Ordering::Acquire) {
            Duration::ZERO
        } else {
            let cached = self.stats.srtt.lock().ok().and_then(|m| m.get(&(origin.clone(), partition.clone())).copied());
            let d = match cached {
                Some(srtt) => srtt.mul_f64(self.opts.main_job_srtt_multiplier),
                None => Duration::from_millis(self.opts.main_job_default_delay_ms),
            };
            d.min(Duration::from_millis(self.opts.main_job_max_delay_ms))
        };
        Some(Plan { origin, partition, dest, sni_host: host, descriptor, key, main_delay, forced })
    }

    /// A usable session for the plan, already open.
    pub(crate) fn ready_session(&self, plan: &Plan) -> Option<Arc<Conn>> {
        let mut s = self.sessions.lock().ok()?;
        match s.get(&plan.key) {
            Some(c) if c.is_usable() => Some(c.clone()),
            Some(_) => {
                s.remove(&plan.key);
                None
            }
            None => None,
        }
    }

    /// The plan's session: an open one, the connect already under way, or a new connect. The connect runs on its own
    /// task, so a request that stops waiting does not stop it (an orphaned job runs to completion).
    pub(crate) fn session(self: &Arc<Self>, plan: &Plan) -> futures_util::future::BoxFuture<'static, ConnResult> {
        if let Some(c) = self.ready_session(plan) {
            return Box::pin(async move { Ok(c) });
        }
        let rx = {
            let Ok(mut pending) = self.pending.lock() else {
                return Box::pin(async { Err("the HTTP/3 session table is poisoned".to_string()) });
            };
            match pending.get(&plan.key) {
                Some(rx) => rx.clone(),
                None => {
                    let (tx, rx) = watch::channel(None);
                    pending.insert(plan.key.clone(), rx.clone());
                    let me = self.clone();
                    let p = plan.clone();
                    tokio::spawn(async move {
                        let r = me.connect(&p).await;
                        // The session is in the table before the job leaves it, so no request in between starts a
                        // second connect.
                        if let Ok(c) = &r {
                            if let Ok(mut s) = me.sessions.lock() {
                                s.insert(p.key.clone(), c.clone());
                            }
                            // A confirmed handshake clears the broken record (`ConfirmAlternativeService`).
                            if let Ok(mut b) = me.altsvc.broken.lock() {
                                b.confirm(&p.dest, &p.partition);
                            }
                        }
                        if let Ok(mut pending) = me.pending.lock() {
                            pending.remove(&p.key);
                        }
                        let _ = tx.send(Some(r));
                    });
                    rx
                }
            }
        };
        Box::pin(async move {
            let mut rx = rx;
            match rx.wait_for(Option::is_some).await {
                Ok(v) => v.clone().unwrap_or_else(|| Err("the QUIC job ended without a result".into())),
                Err(_) => Err("the QUIC job was dropped".into()),
            }
        })
    }

    async fn connect(&self, plan: &Plan) -> ConnResult {
        let o = &self.opts;
        let lookup_start = Instant::now();
        let addrs: Vec<SocketAddr> = match plan.dest.host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, plan.dest.port)],
            Err(_) => self
                .resolver
                .resolve(Name::from(plan.dest.host.as_str()))
                .await
                .map_err(|e| format!("QUIC job lookup of {}: {e}", plan.dest.host))?
                .map(|mut a| {
                    a.set_port(plan.dest.port);
                    a
                })
                .collect(),
        };
        let lookup_end = Instant::now();
        let remote = *addrs.first().ok_or_else(|| format!("no address for {}", plan.dest.host))?;

        let srtt = self.stats.srtt.lock().ok().and_then(|m| m.get(&(plan.origin.clone(), plan.partition.clone())).copied());
        let initial_rtt_us = srtt.map(|d| d.as_micros() as u64);
        let tls = self.tls.clone();
        let opts = o.clone();
        let descriptor = plan.descriptor.clone();
        let make = move |params: &quinn_proto::transport_parameters::TransportParameters| -> Result<(btls::ssl::Ssl, Vec<u8>), ConnectError> {
            let mut quinn_tp = Vec::new();
            params.write(&mut quinn_tp);
            let scid = tp::initial_source_cid(&quinn_tp);
            let ssl = tls.setup_ssl2(descriptor.clone()).map_err(|_| ConnectError::EndpointStopping)?;
            Ok((ssl, tp::encode(&opts, &scid, initial_rtt_us)))
        };
        let mut cc = quinn_proto::ClientConfig::new(Arc::new(crypto::ClientConfig { make: Box::new(make) }));
        cc.transport_config(Arc::new(self.transport_config(srtt)));
        let dcid_len = o.dcid_len;
        cc.initial_dst_cid_provider(Arc::new(move || RandomConnectionIdGenerator::new(dcid_len).generate_cid()));
        cc.version(crypto::QUIC_V1);
        cc.token_store(self.token_store.clone());

        let params = driver::ConnectParams {
            remote,
            server_name: plan.sni_host.clone(),
            client_config: cc,
            endpoint_config: self.endpoint_config.clone(),
            h3: o.h3_settings(),
            lookup: (Some(lookup_start), Some(lookup_end)),
        };
        let conn = tokio::time::timeout(Duration::from_millis(o.handshake_timeout_ms), driver::connect(params))
            .await
            .map_err(|_| "the QUIC handshake timed out".to_string())??;
        self.stats.quic_worked.store(true, Ordering::Release);
        if let Ok(mut m) = self.stats.srtt.lock() {
            m.insert((plan.origin.clone(), plan.partition.clone()), conn.srtt);
        }
        Ok(conn)
    }

    fn transport_config(&self, srtt: Option<Duration>) -> TransportConfig {
        let o = &self.opts;
        let v = |x: u64| VarInt::from_u64(x).unwrap_or(VarInt::MAX);
        let mut t = TransportConfig::default();
        t.max_concurrent_bidi_streams(v(o.initial_max_streams_bidi))
            .max_concurrent_uni_streams(v(o.initial_max_streams_uni))
            .max_idle_timeout(Some(v(o.max_idle_timeout_ms).into()))
            .stream_receive_window(v(o.initial_max_stream_data))
            .receive_window(v(o.initial_max_data))
            .initial_rtt(srtt.unwrap_or(Duration::from_millis(o.default_initial_rtt_ms)))
            .initial_mtu(o.max_packet_size)
            .min_mtu(o.max_packet_size)
            .mtu_discovery_config(None)
            .allow_spin(false)
            .datagram_receive_buffer_size(o.max_datagram_frame_size.map(|x| x as usize))
            .keep_alive_interval(None)
            .enable_segmentation_offload(false)
            .korvus_initial_datagram_size(o.max_packet_size)
            .korvus_chaos_protection(o.chaos_protection)
            .korvus_first_packet_number(o.first_packet_number)
            .korvus_initiate_key_updates(false);
        t
    }

    /// A response from `uri` carried these headers: its `Alt-Svc` is stored (`ProcessAlternativeServices`), for an
    /// `https` origin only.
    pub(crate) fn record_alt_svc(&self, uri: &Uri, group: &Group, headers: &HeaderMap) {
        if uri.scheme_str() != Some("https") {
            return;
        }
        let values: Vec<&str> = headers.get_all("alt-svc").iter().filter_map(|v| v.to_str().ok()).collect();
        if values.is_empty() {
            return;
        }
        let Some(host) = uri.host() else { return };
        let origin = format!("https://{host}:{}", uri.port_u16().unwrap_or(443));
        self.altsvc.store(&origin, &format!("{group:?}"), &values.join(", "), Instant::now());
    }

    /// TCP answered and QUIC did not: the alternative is broken (`MaybeReportBrokenAlternativeService`).
    pub(crate) fn mark_broken(&self, plan: &Plan) {
        if plan.forced {
            return;
        }
        if let Ok(mut b) = self.altsvc.broken.lock() {
            b.mark(&plan.dest, &plan.partition, Instant::now());
        }
    }

    /// Send one request over `conn`.
    pub(crate) async fn send(
        &self,
        conn: &Arc<Conn>,
        group: &Group,
        parts: &http::request::Parts,
        body: Bytes,
        #[cfg(feature = "cookies")] cookie_store: Option<Arc<dyn crate::cookie::CookieStore>>,
    ) -> Result<Response<ResBody>, SendError> {
        let mut headers = parts.headers.clone();
        #[cfg(feature = "cookies")]
        if let Some(store) = cookie_store.as_ref() {
            if !headers.contains_key(http::header::COOKIE) {
                // One field value; the QPACK encoder crumbles it (`ValueSplittingHeaderList`).
                match store.cookies(&parts.uri, Version::HTTP_11) {
                    crate::cookie::Cookies::Compressed(v) => {
                        headers.insert(http::header::COOKIE, v);
                    }
                    crate::cookie::Cookies::Uncompressed(values) => {
                        let joined: Vec<&str> = values.iter().filter_map(|v| v.to_str().ok()).collect();
                        if let Ok(v) = HeaderValue::from_str(&joined.join("; ")) {
                            headers.insert(http::header::COOKIE, v);
                        }
                    }
                    crate::cookie::Cookies::Empty => {}
                }
            }
        }
        let has_upload = matches!(parts.method, Method::POST | Method::PUT | Method::PATCH);
        if (!body.is_empty() || has_upload) && !headers.contains_key(http::header::CONTENT_LENGTH) {
            headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from(body.len()));
        }
        if let Some(order) = parts.extensions.get::<HeaderOrder>() {
            use wreq_proto::ext::OnPreserveHeaderCallback;
            order.0.call(&mut headers);
        }

        let authority = parts.uri.authority().map(|a| a.as_str().to_string()).unwrap_or_default();
        let path = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
        let mut fields: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for p in &self.opts.pseudo_header_order {
            let v = match p.as_str() {
                ":method" => parts.method.as_str().to_string(),
                ":authority" => authority.clone(),
                ":scheme" => "https".to_string(),
                ":path" => path.clone(),
                _ => continue,
            };
            fields.push((p.as_bytes().to_vec(), v.into_bytes()));
        }
        // The fields HTTP/2 and HTTP/3 forbid, as `CreateSpdyHeadersFromHttpRequest` drops them.
        const DROPPED: [&str; 4] = ["connection", "proxy-connection", "transfer-encoding", "host"];
        for (name, value) in headers.iter() {
            if DROPPED.contains(&name.as_str()) {
                continue;
            }
            fields.push((name.as_str().as_bytes().to_vec(), value.as_bytes().to_vec()));
        }
        let priority = headers.get("priority").and_then(|v| v.to_str().ok()).map(str::to_string);

        let (head, rx, fresh) = conn
            .send(RequestHead { fields, priority, body })
            .await
            .map_err(SendError::BeforeResponse)?;

        let mut res = Response::new(ResBody::h3(rx));
        *res.status_mut() = StatusCode::from_u16(head.status).map_err(|e| SendError::Other(e.to_string()))?;
        *res.version_mut() = Version::HTTP_3;
        for (n, v) in head.headers {
            if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(&n), HeaderValue::from_bytes(&v)) {
                res.headers_mut().append(n, v);
            }
        }
        #[cfg(feature = "cookies")]
        if let Some(store) = cookie_store {
            let mut cookies = res.headers().get_all(http::header::SET_COOKIE).iter().peekable();
            if cookies.peek().is_some() {
                store.set_cookies(&mut cookies, &parts.uri);
            }
        }
        self.record_alt_svc(&parts.uri, group, res.headers());
        // When this response's connection became usable (`ConnectionEstablished`), and its QUIC phases.
        res.extensions_mut().insert(crate::conn::ConnectionEstablished(conn.timing.connect_end));
        // C4: the phases as a connect job reports them, and whether the stream rode an open session.
        if fresh {
            res.extensions_mut().insert(crate::conn::ConnectTiming {
                domain_lookup_start: conn.timing.domain_lookup_start,
                domain_lookup_end: conn.timing.domain_lookup_end,
                connect_start: Some(conn.timing.connect_start),
                ssl_start: Some(conn.timing.connect_start),
                connect_end: conn.timing.connect_end,
            });
        }
        res.extensions_mut().insert(crate::conn::ConnectionReused(!fresh));
        res.extensions_mut().insert(QuicConnection { timing: conn.timing, reused: !fresh });
        Ok(res)
    }
}

/// The client initial keys' opener, for a fixture that reads a client's first flight off the wire.
#[doc(hidden)]
pub use self::crypto::client_initial_open;

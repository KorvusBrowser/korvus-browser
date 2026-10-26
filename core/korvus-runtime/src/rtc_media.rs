//! `rtc_media.rs` -- the WebRTC SEND path's DTLS-SRTP handshake, run over the UDP socket an
//! `RTCPeerConnection` already gathered on (`ice.rs`). A kernel service, like `ice.rs`:
//! `core/webrtc/peer_connection.rs` in korvus-webapi decides WHEN to run one (only when a transceiver is
//! sending a fed media track, so its offer carried a real certificate fingerprint and an answer came
//! back) and hands this op the certificate it put in that offer; this file only does the network and the
//! crypto.
//!
//! What this is, and is NOT (Slice 0 of goal A). Chrome, once an answer applies and ICE connects, runs a
//! DTLS 1.2 handshake with the use_srtp extension (RFC 5764) over the selected candidate pair and derives
//! the SRTP keys from it; korvus did none of this -- its `a=fingerprint` was `SHA-256(random)` and no
//! handshake ever ran. This op makes the handshake REAL: a DTLS context from the connection's own
//! self-signed ECDSA P-256 certificate (minted in korvus-webapi, passed in as DER), `use_srtp`
//! negotiated, the handshake driven to completion over the shared socket, and the SRTP keying material
//! exported. It does NOT yet encrypt or send media (VP8/RTP/SRTP is Slice 1); the exported keys are
//! proven present, then dropped.
//!
//! Concurrency. The handshake shares `ice.rs`'s one socket and its one reader: `dtls_bind` registers an
//! inbox so the reader forwards every non-STUN datagram here, leaving STUN consent/keepalive (which
//! carries the magic cookie) untouched. btls's `SslStream` is synchronous (`Read + Write`), and the
//! socket is async non-blocking, so the IO shim WRITES with `try_send_to` (non-blocking, atomic per
//! datagram -- safe to interleave with the keepalive's sends on the same `Arc<UdpSocket>`) and READS from
//! a queue the select loop fills from the inbox. The whole op future is `!Send` (it holds
//! `Rc<RefCell<OpState>>`, like `op_ice_stun`), so it runs on the page's own current-thread runtime with
//! no extra OS thread and no lock held across an `.await`.

use btls::pkey::PKey;
use btls::ssl::{HandshakeError, Ssl, SslMethod, SslStream, SslStreamBuilder, SslVerifyMode, SslVersion};
use btls::x509::X509;
use deno_core::{op2, OpState};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// The SRTP profiles Chrome offers in the use_srtp extension, in its order.
const SRTP_PROFILES: &str = "SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32";
/// The whole handshake is abandoned after this long with no completion -- far longer than a direct,
/// loopback exchange needs, short enough that a dead responder resolves the promise rather than hang.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// A DTLS record never exceeds this on the send path; keeps BoringSSL from fragmenting past the MTU.
const DTLS_MTU: u32 = 1200;

/// The synchronous `Read + Write` face btls's `SslStream` drives. The socket is async, and these calls
/// run inside `SSL_do_handshake` with no `.await` available, so neither call touches the socket: `read`
/// hands btls one datagram off `inbound`, `write` queues a record onto `outbound`, and the op's async
/// loop is what actually fills `inbound` from the socket reader's inbox and drains `outbound` to the
/// socket with `send_to().await`. (`tokio`'s `try_send_to` can't be used here -- it is gated on
/// reactor-tracked write-readiness a freshly bound socket has not yet been told it has, so the first
/// `ClientHello` would spuriously `WouldBlock`.) A datagram protocol: each `read` returns exactly one
/// datagram, never a splice of two, which DTLS requires.
#[derive(Debug, Default)]
struct DtlsIo {
    inbound: VecDeque<Vec<u8>>,
    outbound: VecDeque<Vec<u8>>,
}

impl Read for DtlsIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inbound.pop_front() {
            Some(dg) => {
                let n = dg.len().min(buf.len());
                buf[..n].copy_from_slice(&dg[..n]);
                Ok(n)
            }
            None => Err(io::Error::new(io::ErrorKind::WouldBlock, "no datagram ready")),
        }
    }
}

impl Write for DtlsIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.outbound.push_back(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Send every record btls queued out to the peer. A send to the same socket the keepalive uses is
/// per-datagram atomic; on loopback a small handshake record never blocks.
async fn drain(sock: &UdpSocket, remote: SocketAddr, io: &mut DtlsIo) {
    for dg in io.outbound.drain(..) {
        let _ = sock.send_to(&dg, remote).await;
    }
}

/// A completed handshake's outcome: `(ok, dtls_cipher, srtp_profile, sender)`. `ok` is true when the
/// handshake completed and SRTP keys were exported; the two names (the DTLS cipher's IANA name and the
/// SRTP profile) are read off the live stream for `getStats`; `sender` is the RTP relay's abort handle
/// when a fed sending track is present (so it can be tied to the socket for teardown), else `None`.
type Established = (bool, String, String, Option<tokio::task::AbortHandle>);

/// Read the ciphers + SRTP keys off the live stream and, for a fed sending track, start the media sender.
#[allow(clippy::too_many_arguments)]
fn finish(stream: &SslStream<DtlsIo>, is_server: bool, sock: &Arc<UdpSocket>, remote: SocketAddr, track_id: &str, ssrc: u32, pt: u8, counters: crate::ice::OutboundCounters) -> Established {
    let ssl = stream.ssl();
    let Some(profile) = ssl.selected_srtp_profile() else {
        return (false, String::new(), String::new(), None);
    };
    let srtp = profile.name().to_string();
    // RFC 5764 4.2: 2*(key + salt) for the chosen profile fits in 60 bytes. Exported only here, off the
    // live stream, then either kept by the sender or dropped.
    let mut keying = [0u8; 60];
    if ssl.export_keying_material(&mut keying, "EXTRACTOR-dtls_srtp", None).is_err() {
        return (false, String::new(), srtp, None);
    }
    let dtls = ssl.current_cipher().and_then(|c| c.standard_name()).unwrap_or_default().to_string();
    // Only a fed sending track (non-empty track_id) starts a sender; a track-less connection (e.g. the
    // handshake unit test, or a receive-only one) just reports the ciphers.
    let sender = if track_id.is_empty() { None } else { spawn_sender(&keying, is_server, sock, remote, track_id, ssrc, pt, counters) };
    (true, dtls, srtp, sender)
}

/// Start the media sender for a fed sending track: a std::thread that encodes/packetizes/protects off the
/// fed frames and an async relay that sends each packet on the gathered socket. Returns the relay's abort
/// handle (tied to the socket by the caller). The encoder runs on its OWN OS thread -- the multi-ms VP8
/// encode never touches the page event loop, and the `!Send` encoder/keys never enter an async future, so
/// there is no `Send` bound to satisfy and no `unsafe`.
#[cfg(feature = "webrtc-media")]
#[allow(clippy::too_many_arguments)]
fn spawn_sender(keying: &[u8; 60], is_server: bool, sock: &Arc<UdpSocket>, remote: SocketAddr, track_id: &str, ssrc: u32, pt: u8, counters: crate::ice::OutboundCounters) -> Option<tokio::task::AbortHandle> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let keying = *keying;
    let track = track_id.to_string();
    std::thread::spawn(move || sender_encode_loop(keying, is_server, ssrc, pt, &track, &tx, &counters));
    let sock = sock.clone();
    // The relay holds only Send state (socket + receiver), so tokio::spawn's bound is met; a detached
    // background task like `op_ice_stun`'s keepalive does not hold the page event loop open.
    let relay = tokio::spawn(async move {
        while let Some(pkt) = rx.recv().await {
            let _ = sock.send_to(&pkt, remote).await;
        }
    });
    Some(relay.abort_handle())
}

#[cfg(not(feature = "webrtc-media"))]
#[allow(clippy::too_many_arguments)]
fn spawn_sender(_keying: &[u8; 60], _is_server: bool, _sock: &Arc<UdpSocket>, _remote: SocketAddr, _track_id: &str, _ssrc: u32, _pt: u8, _counters: crate::ice::OutboundCounters) -> Option<tokio::task::AbortHandle> {
    None
}

/// The encoder OS thread: on each `FED_FRAME_GEN` advance, take the latest fed frame, VP8-encode,
/// RTP-packetize, SRTP-protect, and hand each packet to the relay. Exits (dropping the libvpx context and
/// the keys) as soon as the relay's receiver is gone -- the connection/socket closed.
#[cfg(feature = "webrtc-media")]
#[allow(clippy::too_many_arguments)]
fn sender_encode_loop(keying: [u8; 60], is_server: bool, ssrc: u32, pt: u8, track_id: &str, tx: &mpsc::UnboundedSender<Vec<u8>>, counters: &crate::ice::OutboundCounters) {
    use crate::rtc_rtp::RtpSendState;
    use crate::rtc_srtp::SrtpSendCtx;
    use crate::rtc_vp8::Vp8Encoder;
    use std::sync::atomic::Ordering;
    const SRTP_TAG: usize = 10;
    // The RTP header carries the abs-send-time + transport-wide-cc extension block, so SRTP encrypts from
    // after the 12-byte header + the block.
    let header_len = crate::rtc_rtp::HEADER_WITH_EXT;
    let mut srtp = SrtpSendCtx::new(&keying, is_server, ssrc);
    let mut seed = [0u8; 4];
    let _ = getrandom::getrandom(&mut seed);
    let mut rtp = RtpSendState::new(ssrc, pt, u16::from_be_bytes([seed[0], seed[1]]), u16::from_be_bytes([seed[2], seed[3]]));
    let mut enc: Option<Vp8Encoder> = None;
    let mut last_gen = plugin_host::fed_frame_generation();
    let start = Instant::now();
    loop {
        if tx.is_closed() {
            return;
        }
        let gen = plugin_host::fed_frame_generation();
        if gen != last_gen {
            last_gen = gen;
            if let Some(frame) = plugin_host::fed_video_frame(track_id) {
                let (w, h) = (frame.width as usize & !1, frame.height as usize & !1);
                if w == 0 || h == 0 {
                    continue;
                }
                if enc.as_ref().map(|e| e.dims()) != Some((w, h)) {
                    enc = Vp8Encoder::new(w, h, 30, 1000);
                }
                let Some(e) = enc.as_mut() else { continue };
                if let Some(frames) = e.encode(&frame.rgba) {
                    let elapsed = start.elapsed();
                    let ts = (elapsed.as_micros() as u64 * 90_000 / 1_000_000) as u32;
                    // abs-send-time: NTP seconds in 6.18 fixed-point, low 24 bits (libwebrtc's form).
                    let ast = (((elapsed.as_millis() as u64) << 18) / 1000) as u32 & 0x00FF_FFFF;
                    for ef in frames {
                        counters[2].fetch_add(1, Ordering::Relaxed); // framesEncoded
                        for mut pkt in rtp.packetize(&ef.data, ts, SRTP_TAG, Some(ast)) {
                            srtp.protect(&mut pkt, header_len);
                            counters[0].fetch_add(pkt.len() as u64, Ordering::Relaxed); // bytesSent
                            counters[1].fetch_add(1, Ordering::Relaxed); // packetsSent
                            if tx.send(pkt).is_err() {
                                return; // the relay (and the connection) is gone
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Build this side's DTLS context from the connection's certificate (DER) and private key (PKCS#8 DER),
/// with use_srtp on and peer-certificate verification accepted unconditionally (Slice 0: the handshake
/// is real; matching the peer cert against the answer's `a=fingerprint` is a deferred hardening).
fn dtls_stream(cert_der: &[u8], key_pkcs8: &[u8], io: DtlsIo, is_server: bool) -> Result<Result<SslStream<DtlsIo>, HandshakeError<DtlsIo>>, ()> {
    let cert = X509::from_der(cert_der).map_err(|_| ())?;
    let key = PKey::private_key_from_pkcs8(key_pkcs8).map_err(|_| ())?;
    let mut ctx = btls::ssl::SslContext::builder(SslMethod::dtls()).map_err(|_| ())?;
    ctx.set_min_proto_version(Some(SslVersion::DTLS1_2)).map_err(|_| ())?;
    ctx.set_max_proto_version(Some(SslVersion::DTLS1_2)).map_err(|_| ())?;
    ctx.set_certificate(&cert).map_err(|_| ())?;
    ctx.set_private_key(&key).map_err(|_| ())?;
    ctx.set_tlsext_use_srtp(SRTP_PROFILES).map_err(|_| ())?;
    ctx.set_verify_callback(SslVerifyMode::PEER, |_preverify, _store| true);
    let ctx = ctx.build();
    let mut ssl = Ssl::new(&ctx).map_err(|_| ())?;
    ssl.set_mtu(DTLS_MTU).map_err(|_| ())?;
    let builder = SslStreamBuilder::new(ssl, io);
    Ok(if is_server { builder.accept() } else { builder.connect() })
}

/// Drive a DTLS-SRTP handshake to completion over `sock` to `remote`, reading the peer's datagrams off
/// `inbox`. `true` iff it completed and SRTP keys were exported. The first flight (a ClientHello, or the
/// server's first response once a ClientHello arrives) is written inside `dtls_stream`; each later flight
/// is driven by a datagram off the inbox.
#[allow(clippy::too_many_arguments)]
async fn run_handshake(sock: Arc<UdpSocket>, remote: SocketAddr, is_server: bool, cert_der: Vec<u8>, key_pkcs8: Vec<u8>, mut inbox: mpsc::UnboundedReceiver<Vec<u8>>, track_id: String, ssrc: u32, pt: u8, counters: crate::ice::OutboundCounters) -> Established {
    let fail = || (false, String::new(), String::new(), None);
    let mut mid = match dtls_stream(&cert_der, &key_pkcs8, DtlsIo::default(), is_server) {
        Ok(Ok(mut stream)) => {
            drain(&sock, remote, stream.get_mut()).await;
            return finish(&stream, is_server, &sock, remote, &track_id, ssrc, pt, counters);
        }
        Ok(Err(HandshakeError::WouldBlock(m))) => m,
        _ => return fail(),
    };
    // The first flight btls wrote during setup (the client's ClientHello; the server writes nothing yet).
    drain(&sock, remote, mid.get_mut()).await;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        tokio::select! {
            d = inbox.recv() => match d {
                // Take this datagram and any already queued, so a multi-datagram flight is read in one go.
                Some(dg) => {
                    mid.get_mut().inbound.push_back(dg);
                    while let Ok(more) = inbox.try_recv() {
                        mid.get_mut().inbound.push_back(more);
                    }
                }
                None => return fail(), // the socket closed
            },
            _ = tokio::time::sleep_until(deadline) => return fail(),
        }
        match mid.handshake() {
            Ok(mut stream) => {
                // The final flight (e.g. the client's Finished) must reach the peer for it to complete.
                drain(&sock, remote, stream.get_mut()).await;
                return finish(&stream, is_server, &sock, remote, &track_id, ssrc, pt, counters);
            }
            Err(HandshakeError::WouldBlock(mut m)) => {
                drain(&sock, remote, m.get_mut()).await;
                mid = m;
            }
            Err(_) => return fail(),
        }
    }
}

/// Run the DTLS-SRTP handshake for the connection's sending transport over gathered socket `id`, to the
/// answer's default address `remote_addr`, as the DTLS client (`is_server=false`, the answer's
/// `a=setup:passive`) or server (`a=setup:active`). `cert_hex`/`key_hex` are the connection's certificate
/// (DER) and private key (PKCS#8 DER) as hex -- the SAME certificate whose SHA-256 is this offer's
/// `a=fingerprint`. `remote_fp` is the answer's fingerprint, kept for the deferred strict-match hardening.
/// `(ok, dtls_cipher, srtp_profile)` -- `ok` iff the handshake completed and SRTP keys were exported, and
/// the DTLS cipher's IANA name and the SRTP profile name for `getStats` (empty on failure).
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_rtc_dtls_connect(
    state: Rc<RefCell<OpState>>,
    id: u32,
    is_server: bool,
    #[string] params: String,
    ssrc: u32,
    pt: u32,
) -> (bool, String, String) {
    // deno_core caps an async op's argument count, so the four strings DTLS needs ride one newline-joined
    // `params`: cert (DER hex), key (PKCS#8 DER hex), remote address, fed track id. (The answer's
    // fingerprint, unused until the strict-match hardening, is no longer passed.)
    let fail = (false, String::new(), String::new());
    let mut it = params.split('\n');
    let (Some(cert_hex), Some(key_hex), Some(remote_addr), Some(track_id)) = (it.next(), it.next(), it.next(), it.next()) else {
        return fail;
    };
    let (Some(cert_der), Some(key_pkcs8)) = (unhex(cert_hex), unhex(key_hex)) else { return fail };
    let Ok(remote) = remote_addr.parse::<SocketAddr>() else { return fail };
    let Some((sock, inbox)) = crate::ice::dtls_bind(&state, id) else { return fail };
    let Some(counters) = crate::ice::outbound_counters(&state, id) else { return fail };
    let (ok, dtls, srtp, sender) = run_handshake(sock, remote, is_server, cert_der, key_pkcs8, inbox, track_id.to_string(), ssrc, pt as u8, counters).await;
    // Tie the sender's relay to the socket so op_ice_close / realm drop stops it (the encoder thread then
    // exits when its channel closes).
    if let Some(handle) = sender {
        crate::ice::attach_task(&state, id, handle);
    }
    (ok, dtls, srtp)
}

/// A snapshot of the sending connection's outbound counters for `getStats`' `outbound-rtp`:
/// `(bytesSent, packetsSent, framesEncoded)` for socket `id`, all 0 before anything is sent (so a
/// non-sending or feature-off connection reads zeros). Synchronous, for the synchronous `getStats` call.
#[op2]
#[serde]
pub(crate) fn op_rtc_outbound(state: &mut OpState, id: u32) -> (f64, f64, f64) {
    let c = crate::ice::outbound_snapshot(state, id);
    (c[0] as f64, c[1] as f64, c[2] as f64)
}

/// Decode an even-length ASCII-hex string to bytes; `None` on any non-hex character or odd length.
fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    b.chunks(2).map(|p| Some(val(p[0])? << 4 | val(p[1])?)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed ECDSA P-256 certificate and its PKCS#8 key, as the send path mints -- here only to
    /// drive both roles of the handshake in-process.
    fn mint_cert() -> (Vec<u8>, Vec<u8>) {
        use btls::asn1::{Asn1Integer, Asn1Time};
        use btls::bn::BigNum;
        use btls::ec::{EcGroup, EcKey};
        use btls::hash::MessageDigest;
        use btls::nid::Nid;
        use btls::x509::{X509Builder, X509NameBuilder};
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let ec = EcKey::generate(&group).unwrap();
        let key = PKey::from_ec_key(ec).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "WebRTC").unwrap();
        let name = name.build();
        let mut b = X509Builder::new().unwrap();
        b.set_version(2).unwrap();
        let serial = Asn1Integer::from_bn(&BigNum::from_u32(1).unwrap()).unwrap();
        b.set_serial_number(&serial).unwrap();
        b.set_subject_name(&name).unwrap();
        b.set_issuer_name(&name).unwrap();
        b.set_pubkey(&key).unwrap();
        b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
        b.set_not_after(&Asn1Time::days_from_now(30).unwrap()).unwrap();
        b.sign(&key, MessageDigest::sha256()).unwrap();
        (b.build().to_der().unwrap(), key.private_key_to_der_pkcs8().unwrap())
    }

    /// A real localhost DTLS-SRTP handshake, client against server, each over its own UDP socket, with a
    /// pump forwarding datagrams between them (standing in for `ice.rs`'s reader + inbox). Proves the
    /// handshake completes and SRTP keys export on both sides.
    #[test]
    fn a_dtls_srtp_handshake_completes_both_roles() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
            let (atx, arx) = mpsc::unbounded_channel();
            let (btx, brx) = mpsc::unbounded_channel();
            // Each pump reads a socket and feeds the role bound to it.
            let ap = { let a = a.clone(); tokio::task::spawn_local(async move { let mut buf = [0u8; 1500]; while let Ok((n, _)) = a.recv_from(&mut buf).await { if atx.send(buf[..n].to_vec()).is_err() { break; } } }) };
            let bp = { let b = b.clone(); tokio::task::spawn_local(async move { let mut buf = [0u8; 1500]; while let Ok((n, _)) = b.recv_from(&mut buf).await { if btx.send(buf[..n].to_vec()).is_err() { break; } } }) };
            let (ca, ka) = mint_cert();
            let (cb, kb) = mint_cert();
            // a = client (sends to b), b = server (sends to a). Empty track ids -> no media sender spawns.
            let mk = || -> crate::ice::OutboundCounters { std::sync::Arc::new([std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0)]) };
            let client = run_handshake(a.clone(), ba, false, ca, ka, arx, String::new(), 1, 96, mk());
            let server = run_handshake(b.clone(), aa, true, cb, kb, brx, String::new(), 2, 96, mk());
            let (rc, rs) = tokio::join!(client, server);
            ap.abort();
            bp.abort();
            assert!(rc.0, "client handshake + SRTP export");
            assert!(rs.0, "server handshake + SRTP export");
            // Both offered SRTP_AES128_CM_SHA1_80 first, so it is the negotiated profile; the DTLS cipher
            // name is non-empty (what getStats reports).
            assert_eq!(rc.2, "SRTP_AES128_CM_SHA1_80", "negotiated SRTP profile (client)");
            assert_eq!(rs.2, "SRTP_AES128_CM_SHA1_80", "negotiated SRTP profile (server)");
            assert!(!rc.1.is_empty(), "DTLS cipher name present");
        });
    }

    #[test]
    fn unhex_round_trips() {
        assert_eq!(unhex("00ff10Ab"), Some(vec![0x00, 0xff, 0x10, 0xab]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }

    // Slice 1b: prove the VENDORED libvpx (VP8) actually LINKS -- an rlib does not link, but this test
    // binary does, so this is what exercises libvpx.a built by scripts/vpx-source.sh. The encoder itself
    // is Slice 1c; here we only confirm the symbols resolve and VP8 is configured in. Runs only under the
    // `webrtc-media` feature (the default build never links libvpx).
    #[cfg(feature = "webrtc-media")]
    #[test]
    fn libvpx_links_and_has_the_vp8_encoder() {
        let ver = unsafe { std::ffi::CStr::from_ptr(libvpx_sys::vpx_codec_version_str()) };
        assert!(!ver.to_bytes().is_empty(), "libvpx version string resolves");
        assert!(!unsafe { libvpx_sys::vpx_codec_vp8_cx() }.is_null(), "the VP8 encoder interface is present");
    }

    /// The whole send pipeline in process: a fed RGBA frame -> VP8 encode -> RTP packetize -> SRTP protect,
    /// then a receiver with the sender's write keys unprotects, depacketizes, and reassembles the EXACT VP8
    /// frame -- proving the encode/packetize/protect wiring and that SRTP recovers the bytes. (The real
    /// socket send + a libvpx DECODE of the reassembled frame is 1c-send-ext.)
    #[cfg(feature = "webrtc-media")]
    #[test]
    fn media_pipeline_encode_protect_recovers_the_vp8_frame() {
        use crate::rtc_rtp::RtpSendState;
        use crate::rtc_srtp::SrtpSendCtx;
        use crate::rtc_vp8::Vp8Encoder;
        let keying = [0x22u8; 60];
        let ssrc = 0x1122_3344u32;
        let (w, h) = (64usize, 48usize);
        let mut rgba = vec![0u8; w * h * 4];
        for px in rgba.chunks_mut(4) {
            px[1] = 180;
            px[3] = 255;
        }
        let hl = crate::rtc_rtp::HEADER_WITH_EXT; // RTP header + the abs-send-time/transport-cc ext block
        let mut enc = Vp8Encoder::new(w, h, 30, 300).unwrap();
        let mut rtp = RtpSendState::new(ssrc, 96, 1000, 7);
        let mut tx = SrtpSendCtx::new(&keying, false, ssrc);
        let frames = enc.encode(&rgba).unwrap();
        assert!(frames[0].is_keyframe, "first frame is a keyframe");
        let mut protected = Vec::new();
        for mut p in rtp.packetize(&frames[0].data, 90_000, 10, Some(0x0A_BCDE)) {
            tx.protect(&mut p, hl);
            protected.push(p);
        }
        assert!(!protected.is_empty(), "at least one RTP packet");
        // the sender's write keys are the receiver's read keys (client role on both sides here).
        let mut rx = SrtpSendCtx::new(&keying, false, ssrc);
        let mut reassembled = Vec::new();
        for mut p in protected {
            assert!(rx.unprotect(&mut p, hl), "SRTP tag verifies and decrypts (encrypted from after header+ext)");
            reassembled.extend_from_slice(&p[hl + 4..]); // strip the RTP header + ext block + 4-byte VP8 descriptor
        }
        assert_eq!(reassembled, frames[0].data, "the recovered VP8 frame equals the encoded one");
        assert_eq!(&reassembled[3..6], &[0x9d, 0x01, 0x2a], "a valid VP8 keyframe survived the round trip");

        // decode-verify: the reassembled bytes are a real, DECODABLE VP8 frame -- feed them to libvpx's
        // own VP8 decoder and confirm the dimensions, closing the loop end to end.
        unsafe {
            let iface = libvpx_sys::vpx_codec_vp8_dx();
            assert!(!iface.is_null(), "VP8 decoder interface present");
            let mut dctx: libvpx_sys::vpx_codec_ctx_t = std::mem::zeroed();
            assert_eq!(
                libvpx_sys::vpx_codec_dec_init_ver(&mut dctx, iface, std::ptr::null(), 0, libvpx_sys::VPX_DECODER_ABI_VERSION as std::os::raw::c_int),
                libvpx_sys::vpx_codec_err_t_VPX_CODEC_OK,
                "decoder init"
            );
            assert_eq!(
                libvpx_sys::vpx_codec_decode(&mut dctx, reassembled.as_ptr(), reassembled.len() as std::os::raw::c_uint, std::ptr::null_mut(), 0),
                libvpx_sys::vpx_codec_err_t_VPX_CODEC_OK,
                "decode"
            );
            let mut iter: libvpx_sys::vpx_codec_iter_t = std::ptr::null();
            let img = libvpx_sys::vpx_codec_get_frame(&mut dctx, &mut iter);
            assert!(!img.is_null(), "libvpx decoded a frame");
            assert_eq!((*img).d_w, w as u32, "decoded width matches input");
            assert_eq!((*img).d_h, h as u32, "decoded height matches input");
            libvpx_sys::vpx_codec_destroy(&mut dctx);
        }
    }
}

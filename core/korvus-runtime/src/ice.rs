//! `ice.rs` -- ICE gathering's network half: the UDP sockets an `RTCPeerConnection` gathers on, and the
//! STUN Binding Requests sent from them (a bot challenge, in the page and again in its
//! frame, gathers candidates against its STUN server and reads them -- Chrome's include the public address
//! that server saw, korvus's had none). A kernel service (invariant I9): `core/webrtc/peer_connection.rs`
//! in korvus-webapi decides WHAT to gather and what the page sees; this file only does the network.
//!
//! As libwebrtc in Chrome 150 sends them -- measured on this box with tcpdump against stun.a challenge provider.com
//! and with a fixture STUN server that logs every request:
//!
//! * one UDP socket per transport and address family, bound to the any-address; its port is the host
//!   candidate's port, and the server-reflexive one the server reports for it;
//! * a Binding Request is 20 bytes -- type 0x0001, no attributes, the magic cookie -- with a transaction id
//!   of 12 characters from the base64 alphabet (libwebrtc's `CreateRandomString`), which a STUN server can
//!   tell from 12 random bytes;
//! * it is retransmitted 250 ms after the first send, then at doubling intervals capped at 8 s, until
//!   answered, and abandoned 39 750 ms after the first send (libwebrtc's `STUN_TOTAL_TIMEOUT`). A send that
//!   FAILS keeps the same schedule -- which is why, on a box with an IPv6 socket but no IPv6 route, a STUN
//!   server named by a host with an AAAA record holds `iceGatheringState` at "gathering" for ~40 s
//!   (measured: stun.a challenge provider.com and stun.l.google.com 39.9 s, the same servers' IPv4 literals 118 ms);
//! * once answered, the socket sends that server a fresh Binding Request every 10 s for as long as it
//!   lives (measured 10.01-10.02 s apart, every socket of the connection together).

use deno_core::{op2, OpState};
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant};

const MAGIC: u32 = 0x2112_A442;
/// When each send of one Binding Request goes, from the first (libwebrtc: an initial RTO of 250 ms,
/// doubled per retransmission and capped at 8 s, eight retransmissions).
const SEND_AT_MS: [u64; 9] = [0, 250, 750, 1750, 3750, 7750, 15750, 23750, 31750];
const TOTAL_TIMEOUT_MS: u64 = 39_750;
const KEEPALIVE_MS: u64 = 10_000;

/// The transactions waiting for an answer, by transaction id: each is handed the raw response.
type Waiters = Arc<Mutex<HashMap<[u8; 12], oneshot::Sender<Vec<u8>>>>>;

/// A DTLS handshake reusing this socket (the WebRTC send path, `rtc_media.rs`): the reader forwards
/// every non-STUN datagram here. `None` until [`dtls_bind`] registers one, so a gathering-only
/// connection never touches it.
type DtlsInbox = Arc<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>>;

/// The WebRTC send path's outbound counters for `getStats`'s `outbound-rtp` (bytesSent, packetsSent,
/// framesEncoded), bumped by the sender thread and snapshotted by the synchronous `getStats` call. Live
/// for every socket; stays all-zero until a sender runs.
pub(crate) type OutboundCounters = Arc<[std::sync::atomic::AtomicU64; 3]>;

struct IceSocket {
    sock: Arc<UdpSocket>,
    waiters: Waiters,
    dtls_inbox: DtlsInbox,
    outbound: OutboundCounters,
    /// The reader and every keepalive: aborted with the socket.
    tasks: Vec<tokio::task::AbortHandle>,
}

/// This realm's ICE sockets, by id.
#[derive(Default)]
pub(crate) struct IceSockets {
    next: u32,
    map: HashMap<u32, IceSocket>,
}

impl Drop for IceSockets {
    fn drop(&mut self) {
        for s in self.map.values() {
            for t in &s.tasks {
                t.abort();
            }
        }
    }
}

/// libwebrtc's transaction id: 12 characters of the base64 alphabet.
fn transaction_id() -> [u8; 12] {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b = [0u8; 12];
    let _ = getrandom::getrandom(&mut b);
    b.map(|x| ALPHABET[(x as usize) % ALPHABET.len()])
}

fn binding_request(tid: &[u8; 12]) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    m[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    m[8..20].copy_from_slice(tid);
    m
}

/// A Binding Success Response: its transaction id and the address it reports (XOR-MAPPED-ADDRESS, or the
/// legacy MAPPED-ADDRESS).
fn binding_success(m: &[u8]) -> Option<([u8; 12], SocketAddr)> {
    if m.len() < 20 || u16::from_be_bytes([m[0], m[1]]) != 0x0101 || u32::from_be_bytes([m[4], m[5], m[6], m[7]]) != MAGIC {
        return None;
    }
    let tid: [u8; 12] = m[8..20].try_into().ok()?;
    let end = (20 + u16::from_be_bytes([m[2], m[3]]) as usize).min(m.len());
    let (mut at, mut mapped) = (20, None);
    while at + 4 <= end {
        let ty = u16::from_be_bytes([m[at], m[at + 1]]);
        let len = u16::from_be_bytes([m[at + 2], m[at + 3]]) as usize;
        let v = m.get(at + 4..at + 4 + len)?;
        let xor = ty == 0x0020;
        if (xor || ty == 0x0001) && len >= 8 {
            let mut port = u16::from_be_bytes([v[2], v[3]]);
            let mut ip: Vec<u8> = v[4..].to_vec();
            if xor {
                port ^= (MAGIC >> 16) as u16;
                let key: Vec<u8> = MAGIC.to_be_bytes().iter().chain(tid.iter()).copied().collect();
                for (i, b) in ip.iter_mut().enumerate() {
                    *b ^= key[i % key.len()];
                }
            }
            let addr = match (v[1], ip.len()) {
                (1, 4) => Some(IpAddr::from(<[u8; 4]>::try_from(&ip[..4]).ok()?)),
                (2, 16) => Some(IpAddr::from(<[u8; 16]>::try_from(&ip[..16]).ok()?)),
                _ => None,
            };
            if let Some(a) = addr {
                // XOR-MAPPED-ADDRESS wins over MAPPED-ADDRESS when a response carries both.
                if xor || mapped.is_none() {
                    mapped = Some(SocketAddr::new(a, port));
                }
            }
        }
        at += 4 + len.div_ceil(4) * 4;
    }
    mapped.map(|a| (tid, a))
}

/// Every datagram the socket receives: a STUN message goes to the transaction waiting for its id; any
/// other datagram, only when a DTLS handshake has registered an inbox ([`dtls_bind`]), goes to it.
fn spawn_reader(sock: Arc<UdpSocket>, waiters: Waiters, inbox: DtlsInbox) -> tokio::task::AbortHandle {
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            match sock.recv_from(&mut buf).await {
                Ok((n, _)) => {
                    let m = &buf[..n];
                    if n >= 20 && u32::from_be_bytes([m[4], m[5], m[6], m[7]]) == MAGIC {
                        // a STUN message: the transaction with its id decides what it is
                        let tid: [u8; 12] = m[8..20].try_into().unwrap_or_default();
                        if let Some(tx) = waiters.lock().ok().and_then(|mut w| w.remove(&tid)) {
                            let _ = tx.send(m.to_vec());
                        }
                    } else if let Some(tx) = inbox.lock().ok().and_then(|g| g.clone()) {
                        // Not STUN (RFC 7983: DTLS is the first byte 20..=63) and a send path is up: hand it
                        // to the handshake. STUN keepalive/consent is untouched (it carries the magic cookie
                        // and takes the branch above). With no send path the inbox is `None` and this is dead.
                        let _ = tx.send(m.to_vec());
                    }
                }
                // An ICMP error for an earlier send; the transaction's own schedule decides what follows.
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .abort_handle()
}

/// Register a DTLS consumer on socket `id` (the WebRTC send path): the reader forwards every non-STUN
/// datagram to the returned receiver from now on. `(the shared socket, the inbound receiver)`, or
/// `None` when the socket is gone. Only `rtc_media::op_rtc_dtls_connect` calls this.
pub(crate) fn dtls_bind(state: &Rc<RefCell<OpState>>, id: u32) -> Option<(Arc<UdpSocket>, mpsc::UnboundedReceiver<Vec<u8>>)> {
    let st = state.borrow();
    let s = st.try_borrow::<IceSockets>()?.map.get(&id)?;
    let (tx, rx) = mpsc::unbounded_channel();
    *s.dtls_inbox.lock().ok()? = Some(tx);
    Some((s.sock.clone(), rx))
}

/// Tie a background task (the WebRTC send path's RTP relay) to socket `id`, so `op_ice_close` and
/// `IceSockets::drop` abort it with the socket. Aborts it immediately if the socket is already gone --
/// the same push-or-abort as `op_ice_stun`'s keepalive.
pub(crate) fn attach_task(state: &Rc<RefCell<OpState>>, id: u32, handle: tokio::task::AbortHandle) {
    match state.borrow_mut().try_borrow_mut::<IceSockets>().and_then(|r| r.map.get_mut(&id)) {
        Some(s) => s.tasks.push(handle),
        None => handle.abort(),
    }
}

/// The send path's outbound counters for socket `id` (the sender clones this and bumps it), or `None`
/// when the socket is gone.
pub(crate) fn outbound_counters(state: &Rc<RefCell<OpState>>, id: u32) -> Option<OutboundCounters> {
    let st = state.borrow();
    Some(st.try_borrow::<IceSockets>()?.map.get(&id)?.outbound.clone())
}

/// A snapshot of socket `id`'s outbound counters `[bytesSent, packetsSent, framesEncoded]`, for the
/// synchronous `getStats` call; all-zero when the socket is gone or nothing has sent.
pub(crate) fn outbound_snapshot(state: &OpState, id: u32) -> [u64; 3] {
    match state.try_borrow::<IceSockets>().and_then(|r| r.map.get(&id)) {
        Some(s) => [s.outbound[0].load(Ordering::Relaxed), s.outbound[1].load(Ordering::Relaxed), s.outbound[2].load(Ordering::Relaxed)],
        None => [0, 0, 0],
    }
}

/// This process's requests go through a proxy (a context's, which the transport takes from these variables): then no
/// UDP leaves this machine for WebRTC -- Chrome's `disable_non_proxied_udp` policy, under which a proxied browser
/// gathers no candidate of its own -- so no address a page gathers is one the proxy did not give (goal 1020 item 4.5).
fn proxied() -> bool {
    ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"].iter().any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

/// `[id, port]` of a fresh UDP socket on the family's any-address; `[0, 0]` when the box has none, or when the
/// context goes through a proxy ([`proxied`]).
#[op2]
#[serde]
pub(crate) fn op_ice_open(state: &mut OpState, v6: bool) -> (u32, u16) {
    if proxied() {
        return (0, 0);
    }
    let bound = std::net::UdpSocket::bind(if v6 { "[::]:0" } else { "0.0.0.0:0" })
        .and_then(|s| s.set_nonblocking(true).map(|_| s))
        .and_then(UdpSocket::from_std);
    let Ok(sock) = bound else { return (0, 0) };
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let sock = Arc::new(sock);
    let waiters: Waiters = Arc::default();
    let dtls_inbox: DtlsInbox = Arc::default();
    let outbound: OutboundCounters = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
    let reader = spawn_reader(sock.clone(), waiters.clone(), dtls_inbox.clone());
    if !state.has::<IceSockets>() {
        state.put(IceSockets::default());
    }
    let reg = state.borrow_mut::<IceSockets>();
    reg.next += 1;
    let id = reg.next;
    reg.map.insert(id, IceSocket { sock, waiters, dtls_inbox, outbound, tasks: vec![reader] });
    (id, port)
}

/// Whether the box routes the family off this machine -- libwebrtc's default-local-address probe, a UDP
/// `connect` that sends nothing. A family it does not route gets no host candidate, and nor does a proxied context.
#[op2(fast)]
pub(crate) fn op_ice_routable(v6: bool) -> bool {
    if proxied() {
        return false;
    }
    let (bind, peer) = if v6 { ("[::]:0", "[2001:4860:4860::8888]:53") } else { ("0.0.0.0:0", "8.8.8.8:53") };
    std::net::UdpSocket::bind(bind).and_then(|s| s.connect(peer)).is_ok()
}

/// One Binding Request from socket `id` to the STUN server `host:port`: `[address, port]` the server
/// reported, or `null` -- the server has no address of the socket's family, or no answer came in time.
/// An answered server is kept alive from then on.
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_ice_stun(state: Rc<RefCell<OpState>>, id: u32, #[string] host: String, port: u16) -> Option<(String, u16)> {
    let (sock, waiters) = {
        let st = state.borrow();
        let s = st.try_borrow::<IceSockets>()?.map.get(&id)?;
        (s.sock.clone(), s.waiters.clone())
    };
    let to = server_address(&sock, &host, port).await?;
    let tid = transaction_id();
    let answer = transact(&sock, &waiters, to, &binding_request(&tid), tid, |m| binding_success(m).is_some()).await?;
    let (_, mapped) = binding_success(&answer)?;
    let keepalive = tokio::spawn({
        let sock = sock.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(KEEPALIVE_MS)).await;
                let _ = sock.send_to(&binding_request(&transaction_id()), to).await;
            }
        }
    })
    .abort_handle();
    match state.borrow_mut().try_borrow_mut::<IceSockets>().and_then(|r| r.map.get_mut(&id)) {
        Some(s) => s.tasks.push(keepalive),
        // closed while the request was out: nothing to keep alive
        None => keepalive.abort(),
    }
    Some((mapped.ip().to_string(), mapped.port()))
}

/// The server `host:port` in the socket's family: `None` when it has no address of that family.
async fn server_address(sock: &UdpSocket, host: &str, port: u16) -> Option<SocketAddr> {
    let v6 = sock.local_addr().ok()?.is_ipv6();
    let bare = host.trim_start_matches('[').trim_end_matches(']').to_string();
    match bare.parse::<IpAddr>() {
        Ok(ip) => (ip.is_ipv6() == v6).then(|| SocketAddr::new(ip, port)),
        Err(_) => tokio::net::lookup_host((bare.as_str(), port)).await.ok()?.find(|a| a.is_ipv6() == v6),
    }
}

/// One transaction: `request` sent on libwebrtc's schedule until a response with its id that `accept` takes
/// arrives (a response it refuses -- one whose MESSAGE-INTEGRITY does not verify -- is dropped and the schedule
/// runs on, as libwebrtc drops it), or [`TOTAL_TIMEOUT_MS`] passes.
async fn transact(sock: &UdpSocket, waiters: &Waiters, to: SocketAddr, request: &[u8], tid: [u8; 12], accept: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let (tx, mut rx) = oneshot::channel();
    waiters.lock().ok()?.insert(tid, tx);
    let start = Instant::now();
    let deadline = start + Duration::from_millis(TOTAL_TIMEOUT_MS);
    let mut sent = 0;
    let answer = loop {
        if sent < SEND_AT_MS.len() && Instant::now() >= start + Duration::from_millis(SEND_AT_MS[sent]) {
            // A failed send (no route) is not the end: the schedule runs on, as libwebrtc's does.
            let _ = sock.send_to(request, to).await;
            sent += 1;
        }
        let wake = SEND_AT_MS.get(sent).map(|ms| start + Duration::from_millis(*ms)).unwrap_or(deadline).min(deadline);
        tokio::select! {
            r = &mut rx => match r {
                Ok(m) if accept(&m) => break Some(m),
                Ok(_) => {
                    // dropped: wait for the next answer to this transaction
                    let (tx, again) = oneshot::channel();
                    rx = again;
                    waiters.lock().ok()?.insert(tid, tx);
                }
                Err(_) => break None,
            },
            _ = tokio::time::sleep_until(wake) => {
                if Instant::now() >= deadline {
                    break None;
                }
            }
        }
    };
    if let Ok(mut w) = waiters.lock() {
        w.remove(&tid);
    }
    answer
}

// ── TURN (BrowserScan N3) ───────────────────────────────────────────────────────────
//
// A page's `turn:` server gave korvus no candidate at all: BrowserScan reads Chrome's relay candidate and calls a
// browser without one "WebRTC: disabled". libwebrtc allocates a relay over UDP (RFC 5766) under the long-term
// credential mechanism (RFC 5389 sec. 10.2): an Allocate, answered 401 with the server's REALM and NONCE, then the
// same Allocate signed with MESSAGE-INTEGRITY under MD5(username ":" realm ":" password). The answer's
// XOR-RELAYED-ADDRESS is the relay candidate's address and its XOR-MAPPED-ADDRESS the related one. An answer whose
// MESSAGE-INTEGRITY does not verify (a wrong password) is dropped, and the request runs out as an unanswered one.

const ALLOCATE: u16 = 0x0003;
const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_REALM: u16 = 0x0014;
const ATTR_NONCE: u16 = 0x0015;
const ATTR_XOR_RELAYED_ADDRESS: u16 = 0x0016;
const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// A STUN message's type and attributes, in order.
fn attributes(m: &[u8]) -> Option<(u16, Vec<(u16, &[u8])>)> {
    if m.len() < 20 || u32::from_be_bytes([m[4], m[5], m[6], m[7]]) != MAGIC {
        return None;
    }
    let ty = u16::from_be_bytes([m[0], m[1]]);
    let end = (20 + u16::from_be_bytes([m[2], m[3]]) as usize).min(m.len());
    let (mut at, mut out) = (20, Vec::new());
    while at + 4 <= end {
        let t = u16::from_be_bytes([m[at], m[at + 1]]);
        let len = u16::from_be_bytes([m[at + 2], m[at + 3]]) as usize;
        out.push((t, m.get(at + 4..at + 4 + len)?));
        at += 4 + len.div_ceil(4) * 4;
    }
    Some((ty, out))
}

/// An XOR-*-ADDRESS attribute's address (IPv4 or IPv6), under the message's transaction id.
fn xor_address(v: &[u8], tid: &[u8; 12]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC >> 16) as u16;
    let key: Vec<u8> = MAGIC.to_be_bytes().iter().chain(tid.iter()).copied().collect();
    let ip: Vec<u8> = v[4..].iter().enumerate().map(|(i, b)| b ^ key[i % key.len()]).collect();
    let addr = match (v[1], ip.len()) {
        (1, 4) => IpAddr::from(<[u8; 4]>::try_from(&ip[..4]).ok()?),
        (2, 16) => IpAddr::from(<[u8; 16]>::try_from(&ip[..16]).ok()?),
        _ => return None,
    };
    Some(SocketAddr::new(addr, port))
}

/// A STUN request of type `ty` with `attrs`, signed with MESSAGE-INTEGRITY under `key` when there is one.
fn stun_message(ty: u16, tid: &[u8; 12], attrs: &[(u16, Vec<u8>)], key: Option<&[u8; 16]>) -> Vec<u8> {
    let mut body = Vec::new();
    for (t, v) in attrs {
        body.extend_from_slice(&t.to_be_bytes());
        body.extend_from_slice(&(v.len() as u16).to_be_bytes());
        body.extend_from_slice(v);
        body.resize(body.len() + (4 - v.len() % 4) % 4, 0);
    }
    let head = |len: usize| {
        let mut h = Vec::with_capacity(20);
        h.extend_from_slice(&ty.to_be_bytes());
        h.extend_from_slice(&(len as u16).to_be_bytes());
        h.extend_from_slice(&MAGIC.to_be_bytes());
        h.extend_from_slice(tid);
        h
    };
    if let Some(k) = key {
        // over the message as it stands, its length counting the 24-byte attribute
        let mut signed = head(body.len() + 24);
        signed.extend_from_slice(&body);
        let mac = hmac_sha1(k, &signed);
        body.extend_from_slice(&ATTR_MESSAGE_INTEGRITY.to_be_bytes());
        body.extend_from_slice(&20u16.to_be_bytes());
        body.extend_from_slice(&mac);
    }
    let mut m = head(body.len());
    m.extend_from_slice(&body);
    m
}

/// Does `m`'s MESSAGE-INTEGRITY verify under `key` (RFC 5389 sec. 15.4: the HMAC of the message before the
/// attribute, its length field counting the attribute and nothing after it)?
fn integrity_verifies(m: &[u8], key: &[u8; 16]) -> bool {
    let Some(len) = m.get(2..4).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else { return false };
    let end = (20 + len).min(m.len());
    let mut at = 20;
    while at + 4 <= end {
        let t = u16::from_be_bytes([m[at], m[at + 1]]);
        let n = u16::from_be_bytes([m[at + 2], m[at + 3]]) as usize;
        if t == ATTR_MESSAGE_INTEGRITY && n == 20 && at + 24 <= m.len() {
            let mut signed = m[..at].to_vec();
            signed[2..4].copy_from_slice(&((at + 24 - 20) as u16).to_be_bytes());
            return hmac_sha1(key, &signed)[..] == m[at + 4..at + 24];
        }
        at += 4 + n.div_ceil(4) * 4;
    }
    false
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    use hmac::Mac;
    let mut mac = <hmac::Hmac<sha1::Sha1> as Mac>::new_from_slice(key).expect("any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// MD5 (RFC 1321), for the long-term credential key -- the one place this engine needs it.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (0x6745_2301u32, 0xefcd_ab89u32, 0x98ba_dcfeu32, 0x1032_5476u32);
    for chunk in msg.chunks(64) {
        let w: Vec<u32> = chunk.chunks(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(w[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// A relay allocated on TURN server `host:port` from socket `id` with the page's `username` and `credential`:
/// `[relay address, relay port, mapped address, mapped port]`, or `null` -- no server of the socket's family, a
/// refusal, or no answer that verified in time.
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_ice_turn(state: Rc<RefCell<OpState>>, id: u32, #[string] host: String, port: u16, #[string] username: String, #[string] credential: String) -> Option<(String, u16, String, u16)> {
    let (sock, waiters) = {
        let st = state.borrow();
        let s = st.try_borrow::<IceSockets>()?.map.get(&id)?;
        (s.sock.clone(), s.waiters.clone())
    };
    let to = server_address(&sock, &host, port).await?;
    let transport = (ATTR_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]);
    // the first Allocate carries no credentials: the server answers 401 with its realm and nonce
    let tid = transaction_id();
    let first = transact(&sock, &waiters, to, &stun_message(ALLOCATE, &tid, std::slice::from_ref(&transport), None), tid, |m| {
        attributes(m).is_some_and(|(t, _)| t == 0x0103 || t == 0x0113)
    })
    .await?;
    let (ty, attrs) = attributes(&first)?;
    let find = |want: u16| attrs.iter().find(|(t, _)| *t == want).map(|(_, v)| v.to_vec());
    let code = find(ATTR_ERROR_CODE).filter(|v| v.len() >= 4).map(|v| u16::from(v[2] & 7) * 100 + u16::from(v[3]));
    if ty != 0x0113 || code != Some(401) {
        return None;
    }
    let (realm, nonce) = (find(ATTR_REALM)?, find(ATTR_NONCE)?);
    let mut material = username.as_bytes().to_vec();
    material.push(b':');
    material.extend_from_slice(&realm);
    material.push(b':');
    material.extend_from_slice(credential.as_bytes());
    let key = md5(&material);
    let tid = transaction_id();
    let attrs = [transport, (ATTR_USERNAME, username.into_bytes()), (ATTR_REALM, realm), (ATTR_NONCE, nonce)];
    let request = stun_message(ALLOCATE, &tid, &attrs, Some(&key));
    let answer = transact(&sock, &waiters, to, &request, tid, |m| {
        attributes(m).is_some_and(|(t, _)| t == 0x0113) || (attributes(m).is_some_and(|(t, _)| t == 0x0103) && integrity_verifies(m, &key))
    })
    .await?;
    let (ty, attrs) = attributes(&answer)?;
    if ty != 0x0103 {
        return None;
    }
    let relayed = attrs.iter().find(|(t, _)| *t == ATTR_XOR_RELAYED_ADDRESS).and_then(|(_, v)| xor_address(v, &tid))?;
    let mapped = attrs.iter().find(|(t, _)| *t == ATTR_XOR_MAPPED_ADDRESS).and_then(|(_, v)| xor_address(v, &tid))?;
    Some((relayed.ip().to_string(), relayed.port(), mapped.ip().to_string(), mapped.port()))
}

/// Close socket `id`: its reader and keepalives stop with it.
#[op2(fast)]
pub(crate) fn op_ice_close(state: &mut OpState, id: u32) {
    if let Some(s) = state.try_borrow_mut::<IceSockets>().and_then(|r| r.map.remove(&id)) {
        for t in s.tasks {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_request_is_libwebrtcs_twenty_bytes() {
        let tid = transaction_id();
        assert!(tid.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/'), "base64-alphabet characters");
        let m = binding_request(&tid);
        assert_eq!(&m[..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42]);
        assert_eq!(&m[8..], &tid);
    }

    #[test]
    fn md5_is_rfc_1321s() {
        let hex = |d: [u8; 16]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")), "57edf4a22be3c955ac49da2e2107b67a");
    }

    #[test]
    fn a_signed_request_verifies_under_its_key_only() {
        let tid = transaction_id();
        let key = md5(b"u:korvus:p");
        let m = stun_message(ALLOCATE, &tid, &[(ATTR_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]), (ATTR_USERNAME, b"u".to_vec())], Some(&key));
        assert!(integrity_verifies(&m, &key));
        assert!(!integrity_verifies(&m, &md5(b"u:korvus:wrong")));
        let (ty, attrs) = attributes(&m).expect("a STUN message");
        assert_eq!(ty, ALLOCATE);
        assert_eq!(attrs.iter().map(|(t, _)| *t).collect::<Vec<_>>(), vec![ATTR_REQUESTED_TRANSPORT, ATTR_USERNAME, ATTR_MESSAGE_INTEGRITY]);
    }

    #[test]
    fn a_success_response_yields_its_xor_mapped_address() {
        // Built the way the captured answers from stun.a challenge provider.com are: XOR-MAPPED-ADDRESS only.
        let tid: [u8; 12] = *b"ByByrzAbPNz+";
        let mut m = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        m.extend_from_slice(&tid);
        let port = 52690u16 ^ 0x2112;
        let ip = [210u8 ^ 0x21, 126 ^ 0x12, 14 ^ 0xa4, 238 ^ 0x42];
        m.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        m.extend_from_slice(&port.to_be_bytes());
        m.extend_from_slice(&ip);
        assert_eq!(binding_success(&m), Some((tid, "210.126.14.238:52690".parse().unwrap())));
        assert_eq!(binding_success(&m[..19]), None, "a truncated header is not a response");
    }
}

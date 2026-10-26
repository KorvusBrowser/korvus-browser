//! KORVUS PATCH (H3): one QUIC connection and its HTTP/3 session, driven by one task.
//!
//! quinn-proto owns the QUIC state machine; this task owns the UDP socket (one plain datagram per transmit: no GSO,
//! no ECN marking -- Chrome sets neither) and the HTTP/3 layer, which follows QUICHE's `QuicSpdySession` /
//! `QuicSpdyClientSession`:
//!
//!  * once 1-RTT keys are in (`quic_spdy_session.cc:983-994`), the control stream (id 2) opens with SETTINGS sorted by
//!    id plus one GREASE setting `0x1f*N+0x21` (`quic_send_control_stream.cc:60-72`), then a GREASE frame with 0..=3
//!    random bytes (`http_encoder.cc:229-249`); the QPACK decoder (6) and encoder (10) streams are allocated in that
//!    order and write their type byte with their first instruction (`qpack_send_stream.cc:30-49`);
//!  * a request whose priority is not the default (`u=3`, not incremental) first puts a PRIORITY_UPDATE on the control
//!    stream (`quic_spdy_stream.cc:633-647`), then its HEADERS (QPACK, `qpack.rs`) and its body as one DATA frame;
//!  * the server's control stream SETTINGS size the QPACK encoder (`OnSetting`, `:1215-1275`); its encoder stream
//!    feeds the decoder, its decoder stream acknowledges the encoder; GOAWAY stops new requests.

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::{Buf, Bytes, BytesMut};
use quinn_proto::{
    ClientConfig, ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig, Event, ReadError, StreamEvent,
    StreamId, VarInt, WriteError,
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
};

use super::{
    qpack::{Decoded, Decoder, Encoder},
    tp::{get_varint, put_varint},
};

/// HTTP/3 frame and stream types (RFC 9114 7.2, 6.2; RFC 9218 7.1).
const FRAME_DATA: u64 = 0x00;
const FRAME_HEADERS: u64 = 0x01;
const FRAME_SETTINGS: u64 = 0x04;
const FRAME_GOAWAY: u64 = 0x07;
const FRAME_PRIORITY_UPDATE_REQUEST: u64 = 0xf0700;
const STREAM_CONTROL: u64 = 0x00;
const STREAM_PUSH: u64 = 0x01;
const STREAM_QPACK_ENCODER: u64 = 0x02;
const STREAM_QPACK_DECODER: u64 = 0x03;
const SETTINGS_QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
const SETTINGS_QPACK_BLOCKED_STREAMS: u64 = 0x07;
const H3_NO_ERROR: u32 = 0x100;
const H3_STREAM_CREATION_ERROR: u32 = 0x103;
const QPACK_ENCODER_STREAM_ERROR: u32 = 0x201;

/// The HTTP/3 half of [`super::Http3Options`] the driver needs.
#[derive(Debug, Clone)]
pub(crate) struct H3Settings {
    /// This side's SETTINGS, unsorted; written sorted by id with the GREASE one.
    pub(crate) settings: Vec<(u64, u64)>,
    pub(crate) grease_setting: bool,
    pub(crate) grease_frame: bool,
    /// The cap this encoder puts on the peer's table (`qpack_maximum_dynamic_table_capacity_`, 64 KiB).
    pub(crate) qpack_encoder_max_capacity: u64,
    pub(crate) huffman: bool,
    pub(crate) cookie_crumbling: bool,
    pub(crate) priority_update: bool,
}

/// A request as the session writes it: its field list in wire order (pseudo-headers first), its priority field value
/// (the `priority` header's), and its whole body.
pub(crate) struct RequestHead {
    pub(crate) fields: Vec<(Vec<u8>, Vec<u8>)>,
    pub(crate) priority: Option<String>,
    pub(crate) body: Bytes,
}

pub(crate) struct ResponseHead {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(Vec<u8>, Vec<u8>)>,
}

pub(crate) type BodyRx = mpsc::UnboundedReceiver<Result<Bytes, String>>;
type Reply = oneshot::Sender<Result<(ResponseHead, BodyRx), String>>;

enum Cmd {
    Request { head: RequestHead, reply: Reply },
}

/// The phases of a QUIC connection, as `QuicChromiumClientSession::GetConnectTiming` reports them
/// (`quic_chromium_client_session.cc:1124-1125, 1533, 4001-4004, 4326-4330`): the lookup of its job, `connect_start`
/// when `CryptoConnect` began (`ssl_start` is the same moment), `connect_end` when the session became usable.
#[derive(Debug, Clone, Copy)]
pub struct QuicTiming {
    pub domain_lookup_start: Option<Instant>,
    pub domain_lookup_end: Option<Instant>,
    pub connect_start: Instant,
    pub connect_end: Instant,
}

/// A handle on a live connection; requests go to its task.
pub(crate) struct Conn {
    tx: mpsc::UnboundedSender<Cmd>,
    usable: Arc<AtomicBool>,
    first_taken: AtomicBool,
    pub(crate) timing: QuicTiming,
    /// The smoothed RTT when the handshake completed (`ServerNetworkStats`).
    pub(crate) srtt: Duration,
}

impl Conn {
    pub(crate) fn is_usable(&self) -> bool {
        self.usable.load(Ordering::Acquire) && !self.tx.is_closed()
    }

    /// Send one request; `true` when it is the session's first request -- the one whose load timing carries the
    /// connect phases (`quic_http_stream.cc:279-300`, `quic_chromium_client_stream.cc:929-931`).
    pub(crate) async fn send(&self, head: RequestHead) -> Result<(ResponseHead, BodyRx, bool), String> {
        let fresh = !self.first_taken.swap(true, Ordering::AcqRel);
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Request { head, reply }).map_err(|_| "the QUIC connection is closed".to_string())?;
        let (h, body) = rx.await.map_err(|_| "the QUIC connection closed before the response".to_string())??;
        Ok((h, body, fresh))
    }
}

pub(crate) struct ConnectParams {
    pub(crate) remote: SocketAddr,
    pub(crate) server_name: String,
    pub(crate) client_config: ClientConfig,
    pub(crate) endpoint_config: Arc<EndpointConfig>,
    pub(crate) h3: H3Settings,
    pub(crate) lookup: (Option<Instant>, Option<Instant>),
}

/// Open a QUIC connection and its HTTP/3 session; resolves once the session is usable.
pub(crate) async fn connect(p: ConnectParams) -> Result<Arc<Conn>, String> {
    let bind: SocketAddr = if p.remote.is_ipv4() { "0.0.0.0:0".parse().expect("addr") } else { "[::]:0".parse().expect("addr") };
    let socket = UdpSocket::bind(bind).await.map_err(|e| format!("UDP bind: {e}"))?;
    socket.connect(p.remote).await.map_err(|e| format!("UDP connect: {e}"))?;
    let mut endpoint = Endpoint::new(p.endpoint_config, None, false, None);
    let connect_start = Instant::now();
    let (ch, conn) = endpoint
        .connect(connect_start, p.client_config, p.remote, &p.server_name)
        .map_err(|e| format!("QUIC connect: {e}"))?;
    let (ready_tx, ready_rx) = oneshot::channel();
    let (tx, rx) = mpsc::unbounded_channel();
    let usable = Arc::new(AtomicBool::new(true));
    let driver = Driver {
        socket,
        remote: p.remote,
        endpoint,
        ch,
        conn,
        rx,
        ready: Some(ready_tx),
        usable: usable.clone(),
        connected: false,
        h3: H3::new(p.h3),
    };
    tokio::spawn(driver.run());
    let (connect_end, srtt) = ready_rx.await.map_err(|_| "the QUIC handshake was abandoned".to_string())??;
    Ok(Arc::new(Conn {
        tx,
        usable,
        first_taken: AtomicBool::new(false),
        timing: QuicTiming {
            domain_lookup_start: p.lookup.0,
            domain_lookup_end: p.lookup.1,
            connect_start,
            connect_end,
        },
        srtt,
    }))
}

fn frame(ty: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    put_varint(&mut out, ty);
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

#[derive(Default)]
struct Outgoing {
    chunks: VecDeque<Bytes>,
    fin: bool,
}

struct Request {
    reply: Option<Reply>,
    body: Option<mpsc::UnboundedSender<Result<Bytes, String>>>,
    buf: Vec<u8>,
    /// Bytes of the current DATA frame still to come.
    data_left: u64,
    /// A HEADERS block waiting on the QPACK encoder stream.
    blocked: Option<Vec<u8>>,
    got_final: bool,
    fin: bool,
}

struct PeerUni {
    kind: Option<u64>,
    buf: Vec<u8>,
}

struct H3 {
    settings: H3Settings,
    control: Option<StreamId>,
    decoder_stream: Option<StreamId>,
    encoder_stream: Option<StreamId>,
    decoder_type_sent: bool,
    encoder_type_sent: bool,
    out: HashMap<StreamId, Outgoing>,
    encoder: Encoder,
    decoder: Decoder,
    requests: HashMap<StreamId, Request>,
    queued: VecDeque<Cmd>,
    peer_uni: HashMap<StreamId, PeerUni>,
    goaway: bool,
}

impl H3 {
    fn new(settings: H3Settings) -> H3 {
        let max_table = settings.settings.iter().find(|(id, _)| *id == SETTINGS_QPACK_MAX_TABLE_CAPACITY).map(|(_, v)| *v).unwrap_or(0);
        H3 {
            encoder: Encoder::new(settings.huffman, settings.cookie_crumbling),
            decoder: Decoder::new(max_table),
            settings,
            control: None,
            decoder_stream: None,
            encoder_stream: None,
            decoder_type_sent: false,
            encoder_type_sent: false,
            out: HashMap::new(),
            requests: HashMap::new(),
            queued: VecDeque::new(),
            peer_uni: HashMap::new(),
            goaway: false,
        }
    }
}

struct Driver {
    socket: UdpSocket,
    remote: SocketAddr,
    endpoint: Endpoint,
    ch: ConnectionHandle,
    conn: quinn_proto::Connection,
    rx: mpsc::UnboundedReceiver<Cmd>,
    ready: Option<oneshot::Sender<Result<(Instant, Duration), String>>>,
    usable: Arc<AtomicBool>,
    connected: bool,
    h3: H3,
}

impl Driver {
    async fn run(mut self) {
        let mut recv = vec![0u8; 65536];
        let mut handles_gone = false;
        loop {
            self.transmit().await;
            if self.conn.is_drained() {
                break;
            }
            if handles_gone && self.h3.requests.is_empty() && !self.conn.is_closed() && self.connected {
                // Nobody can send on this session again: close it as an idle QUICHE session closes.
                self.conn.close(Instant::now(), VarInt::from_u32(H3_NO_ERROR), Bytes::new());
                continue;
            }
            let deadline = self.conn.poll_timeout();
            let sleep = async move {
                match deadline {
                    Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                r = self.socket.recv(&mut recv) => {
                    if let Ok(n) = r {
                        self.on_datagram(&recv[..n]);
                    }
                }
                _ = sleep => self.conn.handle_timeout(Instant::now()),
                cmd = self.rx.recv(), if !handles_gone => match cmd {
                    Some(c) => self.on_cmd(c),
                    None => handles_gone = true,
                },
            }
            self.events();
        }
        self.usable.store(false, Ordering::Release);
        self.fail_all("the QUIC connection closed");
    }

    async fn transmit(&mut self) {
        let mut buf = Vec::with_capacity(1500);
        loop {
            buf.clear();
            match self.conn.poll_transmit(Instant::now(), 1, &mut buf) {
                Some(t) => {
                    let _ = self.socket.send(&buf[..t.size]).await;
                }
                None => break,
            }
        }
    }

    fn on_datagram(&mut self, data: &[u8]) {
        let mut resp = Vec::new();
        match self.endpoint.handle(Instant::now(), self.remote, None, None, BytesMut::from(data), &mut resp) {
            Some(DatagramEvent::ConnectionEvent(ch, ev)) if ch == self.ch => self.conn.handle_event(ev),
            Some(DatagramEvent::Response(t)) => {
                let _ = self.socket.try_send(&resp[..t.size]);
            }
            _ => {}
        }
    }

    fn events(&mut self) {
        while let Some(ev) = self.conn.poll_endpoint_events() {
            if let Some(cev) = self.endpoint.handle_event(self.ch, ev) {
                self.conn.handle_event(cev);
            }
        }
        while let Some(ev) = self.conn.poll() {
            match ev {
                Event::Connected => self.on_connected(),
                Event::ConnectionLost { reason } => {
                    self.usable.store(false, Ordering::Release);
                    if let Some(r) = self.ready.take() {
                        let _ = r.send(Err(format!("QUIC handshake failed: {reason}")));
                    }
                    self.fail_all(&format!("QUIC connection lost: {reason}"));
                }
                Event::Stream(StreamEvent::Opened { dir: Dir::Uni }) => {
                    while let Some(id) = self.conn.streams().accept(Dir::Uni) {
                        self.h3.peer_uni.insert(id, PeerUni { kind: None, buf: Vec::new() });
                        self.read(id);
                    }
                }
                Event::Stream(StreamEvent::Opened { dir: Dir::Bi }) => {
                    // A server never opens a bidirectional stream in HTTP/3.
                    while let Some(id) = self.conn.streams().accept(Dir::Bi) {
                        let _ = self.conn.recv_stream(id).stop(VarInt::from_u32(H3_STREAM_CREATION_ERROR));
                    }
                }
                Event::Stream(StreamEvent::Readable { id }) => self.read(id),
                Event::Stream(StreamEvent::Writable { id }) => self.flush(id),
                Event::Stream(StreamEvent::Available { dir: Dir::Bi }) => self.start_queued(),
                Event::Stream(StreamEvent::Stopped { id, .. }) => {
                    self.h3.out.remove(&id);
                }
                _ => {}
            }
        }
        self.start_queued();
    }

    fn on_connected(&mut self) {
        self.connected = true;
        let srtt = self.conn.rtt();
        if let Some(r) = self.ready.take() {
            let _ = r.send(Ok((Instant::now(), srtt)));
        }
        // Control, QPACK decoder, QPACK encoder: ids 2, 6, 10 (`MaybeInitializeHttp3UnidirectionalStreams`).
        let mut streams = self.conn.streams();
        self.h3.control = streams.open(Dir::Uni);
        self.h3.decoder_stream = streams.open(Dir::Uni);
        self.h3.encoder_stream = streams.open(Dir::Uni);
        let Some(control) = self.h3.control else { return };
        let mut settings = self.h3.settings.settings.clone();
        if self.h3.settings.grease_setting {
            let n = u64::from(super::crypto::random_u32());
            settings.push((0x1f * n + 0x21, u64::from(super::crypto::random_u32())));
        }
        settings.sort_by_key(|(id, _)| *id);
        let mut payload = Vec::new();
        for (id, v) in settings {
            put_varint(&mut payload, id);
            put_varint(&mut payload, v);
        }
        let mut bytes = Vec::new();
        put_varint(&mut bytes, STREAM_CONTROL);
        bytes.extend_from_slice(&frame(FRAME_SETTINGS, &payload));
        if self.h3.settings.grease_frame {
            let r = super::crypto::random_u32();
            let mut grease = vec![0u8; (r % 4) as usize];
            super::crypto::random_bytes(&mut grease);
            bytes.extend_from_slice(&frame(0x1f * u64::from(r) + 0x21, &grease));
        }
        self.push(control, bytes.into(), false);
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        self.h3.queued.push_back(cmd);
        self.start_queued();
    }

    fn start_queued(&mut self) {
        if !self.connected {
            return;
        }
        while let Some(cmd) = self.h3.queued.pop_front() {
            if self.h3.goaway || self.conn.is_closed() {
                let Cmd::Request { reply, .. } = cmd;
                let _ = reply.send(Err("the QUIC session is going away".into()));
                continue;
            }
            let Some(id) = self.conn.streams().open(Dir::Bi) else {
                self.h3.queued.push_front(cmd);
                return;
            };
            let Cmd::Request { head, reply } = cmd;
            self.start_request(id, head, reply);
        }
    }

    fn start_request(&mut self, id: StreamId, head: RequestHead, reply: Reply) {
        let sid = u64::from(id);
        if let (Some(control), Some(p)) = (self.h3.control, head.priority.as_ref()) {
            if self.h3.settings.priority_update && !is_default_priority(p) {
                let mut payload = Vec::new();
                put_varint(&mut payload, sid);
                payload.extend_from_slice(p.as_bytes());
                self.push(control, frame(FRAME_PRIORITY_UPDATE_REQUEST, &payload).into(), false);
            }
        }
        let block = self.h3.encoder.encode(sid, &head.fields);
        let instructions = self.h3.encoder.take_encoder_stream();
        if !instructions.is_empty() {
            if let Some(enc) = self.h3.encoder_stream {
                let mut bytes = Vec::new();
                if !self.h3.encoder_type_sent {
                    put_varint(&mut bytes, STREAM_QPACK_ENCODER);
                    self.h3.encoder_type_sent = true;
                }
                bytes.extend_from_slice(&instructions);
                self.push(enc, bytes.into(), false);
            }
        }
        let mut bytes = frame(FRAME_HEADERS, &block);
        if !head.body.is_empty() {
            bytes.extend_from_slice(&frame(FRAME_DATA, &head.body));
        }
        self.h3.requests.insert(
            id,
            Request { reply: Some(reply), body: None, buf: Vec::new(), data_left: 0, blocked: None, got_final: false, fin: false },
        );
        self.push(id, bytes.into(), true);
    }

    fn push(&mut self, id: StreamId, data: Bytes, fin: bool) {
        let o = self.h3.out.entry(id).or_default();
        if !data.is_empty() {
            o.chunks.push_back(data);
        }
        o.fin |= fin;
        self.flush(id);
    }

    fn flush(&mut self, id: StreamId) {
        let Some(o) = self.h3.out.get_mut(&id) else { return };
        let mut s = self.conn.send_stream(id);
        while let Some(front) = o.chunks.front_mut() {
            match s.write(&front[..]) {
                Ok(n) if n == front.len() => {
                    o.chunks.pop_front();
                }
                Ok(n) => {
                    front.advance(n);
                    break;
                }
                Err(WriteError::Blocked) => break,
                Err(_) => {
                    o.chunks.clear();
                    o.fin = false;
                    break;
                }
            }
        }
        if o.chunks.is_empty() && o.fin {
            let _ = s.finish();
            self.h3.out.remove(&id);
        }
    }

    fn read(&mut self, id: StreamId) {
        let mut data = Vec::new();
        let mut fin = false;
        let mut failed = None;
        {
            let mut rs = self.conn.recv_stream(id);
            let Ok(mut chunks) = rs.read(true) else { return };
            loop {
                match chunks.next(usize::MAX) {
                    Ok(Some(c)) => data.extend_from_slice(&c.bytes),
                    Ok(None) => {
                        fin = true;
                        break;
                    }
                    Err(ReadError::Blocked) => break,
                    Err(e) => {
                        failed = Some(e.to_string());
                        break;
                    }
                }
            }
            let _ = chunks.finalize();
        }
        if id.dir() == Dir::Bi {
            self.on_request_data(id, data, fin, failed);
        } else {
            self.on_uni_data(id, data);
        }
    }

    fn on_uni_data(&mut self, id: StreamId, data: Vec<u8>) {
        let Some(u) = self.h3.peer_uni.get_mut(&id) else { return };
        u.buf.extend_from_slice(&data);
        if u.kind.is_none() {
            let mut at = 0;
            let Some(kind) = get_varint(&u.buf, &mut at) else { return };
            u.kind = Some(kind);
            u.buf.drain(..at);
        }
        let kind = u.kind.unwrap_or(u64::MAX);
        match kind {
            STREAM_CONTROL => {
                let mut buf = std::mem::take(&mut u.buf);
                let mut at = 0;
                loop {
                    let start = at;
                    let (Some(ty), Some(len)) = (get_varint(&buf, &mut at), get_varint(&buf, &mut at)) else {
                        at = start;
                        break;
                    };
                    let end = at + len as usize;
                    if end > buf.len() {
                        at = start;
                        break;
                    }
                    let payload = buf[at..end].to_vec();
                    at = end;
                    match ty {
                        FRAME_SETTINGS => self.on_peer_settings(&payload),
                        FRAME_GOAWAY => {
                            self.h3.goaway = true;
                            self.usable.store(false, Ordering::Release);
                        }
                        _ => {}
                    }
                }
                buf.drain(..at);
                if let Some(u) = self.h3.peer_uni.get_mut(&id) {
                    u.buf = buf;
                }
            }
            STREAM_QPACK_ENCODER => {
                let mut buf = std::mem::take(&mut u.buf);
                if self.h3.decoder.on_encoder_stream(&mut buf).is_err() {
                    self.conn.close(Instant::now(), VarInt::from_u32(QPACK_ENCODER_STREAM_ERROR), Bytes::new());
                    return;
                }
                if let Some(u) = self.h3.peer_uni.get_mut(&id) {
                    u.buf = buf;
                }
                // Blocked field sections may decode now.
                let blocked: Vec<StreamId> = self.h3.requests.iter().filter(|(_, r)| r.blocked.is_some()).map(|(k, _)| *k).collect();
                for sid in blocked {
                    self.on_request_data(sid, Vec::new(), false, None);
                }
            }
            STREAM_QPACK_DECODER => {
                let mut buf = std::mem::take(&mut u.buf);
                let _ = self.h3.encoder.on_decoder_stream(&mut buf);
                if let Some(u) = self.h3.peer_uni.get_mut(&id) {
                    u.buf = buf;
                }
            }
            STREAM_PUSH => {
                // No MAX_PUSH_ID was ever sent, so no push is accepted.
                u.buf.clear();
                let _ = self.conn.recv_stream(id).stop(VarInt::from_u32(H3_STREAM_CREATION_ERROR));
            }
            _ => {
                // A reserved or unknown stream type is read and ignored (`PendingStream::StopReading`).
                u.buf.clear();
            }
        }
    }

    fn on_peer_settings(&mut self, payload: &[u8]) {
        let mut at = 0;
        let mut capacity = None;
        let mut blocked = None;
        while at < payload.len() {
            let (Some(id), Some(v)) = (get_varint(payload, &mut at), get_varint(payload, &mut at)) else { break };
            match id {
                SETTINGS_QPACK_MAX_TABLE_CAPACITY => capacity = Some(v),
                SETTINGS_QPACK_BLOCKED_STREAMS => blocked = Some(v),
                _ => {}
            }
        }
        if let Some(c) = capacity {
            self.h3.encoder.set_peer_max_table_capacity(c, self.h3.settings.qpack_encoder_max_capacity);
        }
        if let Some(b) = blocked {
            self.h3.encoder.set_peer_blocked_streams(b);
        }
    }

    fn on_request_data(&mut self, id: StreamId, data: Vec<u8>, fin: bool, failed: Option<String>) {
        let sid = u64::from(id);
        let Some(r) = self.h3.requests.get_mut(&id) else { return };
        r.buf.extend_from_slice(&data);
        r.fin |= fin;
        if let Some(e) = failed {
            let msg = format!("the HTTP/3 stream failed: {e}");
            if let Some(reply) = r.reply.take() {
                let _ = reply.send(Err(msg));
            } else if let Some(body) = r.body.take() {
                let _ = body.send(Err(msg));
            }
            self.h3.requests.remove(&id);
            return;
        }
        let mut decoder_bytes = Vec::new();
        loop {
            // A HEADERS block that waited on the encoder stream goes first.
            if let Some(block) = r.blocked.take() {
                match self.h3.decoder.decode(sid, &block) {
                    Decoded::Blocked => {
                        r.blocked = Some(block);
                        break;
                    }
                    Decoded::Error => {
                        if let Some(reply) = r.reply.take() {
                            let _ = reply.send(Err("QPACK decompression failed".into()));
                        }
                        r.body = None;
                        break;
                    }
                    Decoded::Headers(fields) => {
                        decoder_bytes.extend_from_slice(&self.h3.decoder.take_decoder_stream());
                        on_headers(r, fields);
                    }
                }
                continue;
            }
            if r.data_left > 0 {
                if r.buf.is_empty() {
                    break;
                }
                let n = (r.data_left as usize).min(r.buf.len());
                let chunk: Vec<u8> = r.buf.drain(..n).collect();
                r.data_left -= n as u64;
                if let Some(body) = r.body.as_ref() {
                    let _ = body.send(Ok(Bytes::from(chunk)));
                }
                continue;
            }
            let mut at = 0;
            let (Some(ty), Some(len)) = (get_varint(&r.buf, &mut at), get_varint(&r.buf, &mut at)) else { break };
            if ty == FRAME_DATA {
                r.buf.drain(..at);
                r.data_left = len;
                if len == 0 {
                    continue;
                }
                continue;
            }
            let end = at + len as usize;
            if end > r.buf.len() {
                break;
            }
            let payload: Vec<u8> = r.buf[at..end].to_vec();
            r.buf.drain(..end);
            if ty == FRAME_HEADERS {
                r.blocked = Some(payload);
            }
            // Any other frame (reserved, unknown) is skipped.
        }
        let finished = r.fin && r.buf.is_empty() && r.data_left == 0 && r.blocked.is_none();
        if finished {
            if let Some(reply) = r.reply.take() {
                let _ = reply.send(Err("the HTTP/3 stream ended without a response".into()));
            }
            r.body = None;
        }
        let done = finished || (r.reply.is_none() && r.body.is_none());
        if done {
            self.h3.requests.remove(&id);
        }
        if !decoder_bytes.is_empty() {
            if let Some(dec) = self.h3.decoder_stream {
                let mut bytes = Vec::new();
                if !self.h3.decoder_type_sent {
                    put_varint(&mut bytes, STREAM_QPACK_DECODER);
                    self.h3.decoder_type_sent = true;
                }
                bytes.extend_from_slice(&decoder_bytes);
                self.push(dec, bytes.into(), false);
            }
        }
    }

    fn fail_all(&mut self, why: &str) {
        for (_, mut r) in self.h3.requests.drain() {
            if let Some(reply) = r.reply.take() {
                let _ = reply.send(Err(why.to_string()));
            } else if let Some(body) = r.body.take() {
                let _ = body.send(Err(why.to_string()));
            }
        }
        for cmd in self.h3.queued.drain(..) {
            let Cmd::Request { reply, .. } = cmd;
            let _ = reply.send(Err(why.to_string()));
        }
    }
}

/// A decoded HEADERS section on a request stream: an interim (1xx) response is skipped, the final one answers the
/// request, a later one (trailers) is ignored.
fn on_headers(r: &mut Request, fields: Vec<(Vec<u8>, Vec<u8>)>) {
    if r.got_final {
        return;
    }
    let status = fields
        .iter()
        .find(|(n, _)| n.as_slice() == b":status")
        .and_then(|(_, v)| std::str::from_utf8(v).ok()?.parse::<u16>().ok());
    let Some(status) = status else {
        if let Some(reply) = r.reply.take() {
            let _ = reply.send(Err("an HTTP/3 response without :status".into()));
        }
        return;
    };
    if (100..200).contains(&status) {
        return;
    }
    r.got_final = true;
    let headers = fields.into_iter().filter(|(n, _)| !n.starts_with(b":")).collect();
    let (tx, rx) = mpsc::unbounded_channel();
    r.body = Some(tx);
    if let Some(reply) = r.reply.take() {
        let _ = reply.send(Ok((ResponseHead { status, headers }, rx)));
    }
}

/// `u=3` without `i` is the default priority (RFC 9218 4), for which QUICHE sends no PRIORITY_UPDATE.
fn is_default_priority(v: &str) -> bool {
    let mut urgency = 3u8;
    let mut incremental = false;
    for item in v.split(',') {
        let item = item.trim();
        if let Some(u) = item.strip_prefix("u=") {
            urgency = u.parse().unwrap_or(3);
        } else if item == "i" || item == "i=?1" {
            incremental = true;
        }
    }
    urgency == 3 && !incremental
}

/// The process-wide QUIC state Chrome keeps across sessions: srtt per server (`ServerNetworkStats`) and whether QUIC
/// has worked on this network yet (`QuicSessionPool::GetTimeDelayForWaitingJob`).
#[derive(Default)]
pub(crate) struct NetworkStats {
    pub(crate) srtt: Mutex<HashMap<(String, String), Duration>>,
    pub(crate) quic_worked: AtomicBool,
}

//! KORVUS PATCH (H3): a loopback HTTP/3 server that RECORDS what a client puts on the wire, for the fixture that
//! compares korvus with stock Chrome 150 (`plugins/korvus-transport/tests/h3_wire.rs`). Its rows are the ones
//! `fixtures/h3/rows.py` (korvus-transport) derives from the Chrome recordings, so the two sides are read the same way:
//!
//!  * `initial.*`: every client datagram carrying an Initial (its size), the first Initial packet's header fields,
//!    and the chaos-protection shape of the first flight -- read off the bytes with the RFC 9001 initial keys;
//!  * `ch.*`: the ClientHello in those packets, its extension SET (BoringSSL permutes the order) and values;
//!  * `tp.*`: the QUIC transport parameters (their SET; QUICHE shuffles the order);
//!  * `h3.*`: the client's unidirectional streams, its control stream's frames, its QPACK encoder stream's first
//!    instruction, and each request's field names in order.
//!
//! The server answers like the aioquic recorder Chrome was measured against: SETTINGS QPACK_MAX_TABLE_CAPACITY 4096
//! and QPACK_BLOCKED_STREAMS 16, a `200` with `alt-svc`, Section Acknowledgements for the client's field sections.

#![doc(hidden)]

use std::{
    collections::{BTreeMap, HashMap},
    net::UdpSocket,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::BytesMut;
use quinn_proto::{
    ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig, Event, ServerConfig, StreamEvent, StreamId,
};

use super::{
    crypto,
    qpack::{Decoded, Decoder, Encoder},
    tp::{get_varint, put_varint},
};

#[derive(Default)]
struct Conn {
    /// Client-initiated stream id -> every byte it carried.
    streams: BTreeMap<u64, Vec<u8>>,
    fin: BTreeMap<u64, bool>,
    /// Request stream -> decoded field list.
    requests: BTreeMap<u64, Vec<(String, String)>>,
    answered: Vec<u64>,
}

#[derive(Default)]
struct State {
    /// Client datagrams, in arrival order: (source port, bytes).
    datagrams: Vec<(u16, Vec<u8>)>,
    conns: Vec<Conn>,
}

/// The recording server. Dropping it stops nothing; the test process ends.
pub struct Recorder {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Recorder {
    /// Listen on `127.0.0.1:0` with this certificate chain and key.
    pub fn start(cert_chain_pem: &[u8], key_pem: &[u8]) -> std::io::Result<Recorder> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let port = socket.local_addr()?.port();
        socket.set_read_timeout(Some(Duration::from_millis(5)))?;
        let crypto_cfg = crypto::ServerConfig::new(cert_chain_pem, key_pem).map_err(std::io::Error::other)?;
        let server = ServerConfig::new(Arc::new(crypto_cfg), Arc::new(crypto::TokenKey::random()));
        let mut ec = EndpointConfig::new(Arc::new(crypto::HmacKey::random()));
        ec.supported_versions(vec![crypto::QUIC_V1]);
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        std::thread::spawn(move || serve(socket, Endpoint::new(Arc::new(ec), Some(Arc::new(server)), false, None), st));
        Ok(Recorder { port, state })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Requests answered so far, over every connection.
    pub fn answered(&self) -> usize {
        self.state.lock().map(|s| s.conns.iter().map(|c| c.answered.len()).sum()).unwrap_or(0)
    }

    /// The rows the fixture compares (see the module note).
    pub fn rows(&self) -> Vec<String> {
        let s = self.state.lock().expect("recorder state");
        let mut rows = initial_rows(&s.datagrams);
        // The first connection that carried a request is the one the H3 rows describe.
        if let Some(c) = s.conns.iter().find(|c| !c.requests.is_empty()) {
            rows.extend(h3_rows(c));
        }
        rows
    }
}

struct ServerConn {
    conn: quinn_proto::Connection,
    index: usize,
    decoder: Decoder,
    encoder: Encoder,
    control_sent: bool,
    decoder_stream: Option<StreamId>,
    /// This side's QPACK encoder stream: responses use the dynamic table once the client's SETTINGS allow it, as
    /// aioquic's (pylsqpack) did against Chrome -- which is what makes the client acknowledge on its decoder stream.
    encoder_stream: Option<StreamId>,
    encoder_type_sent: bool,
    client_settings_seen: bool,
    decoder_type_sent: bool,
    blocked: HashMap<StreamId, Vec<u8>>,
    /// How much of the client's encoder stream the decoder has been given, and what it has not consumed yet.
    encoder_fed: usize,
    encoder_buf: Vec<u8>,
}

fn serve(socket: UdpSocket, mut endpoint: Endpoint, state: Arc<Mutex<State>>) {
    let mut conns: HashMap<ConnectionHandle, ServerConn> = HashMap::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let now = Instant::now();
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Ok(mut s) = state.lock() {
                    if s.datagrams.len() < 64 {
                        s.datagrams.push((from.port(), buf[..n].to_vec()));
                    }
                }
                let mut resp = Vec::new();
                match endpoint.handle(now, from, None, None, BytesMut::from(&buf[..n]), &mut resp) {
                    Some(DatagramEvent::NewConnection(incoming)) => {
                        let mut out = Vec::new();
                        if let Ok((ch, conn)) = endpoint.accept(incoming, now, &mut out, None) {
                            let index = state.lock().map(|mut s| {
                                s.conns.push(Conn::default());
                                s.conns.len() - 1
                            });
                            conns.insert(
                                ch,
                                ServerConn {
                                    conn,
                                    index: index.unwrap_or(0),
                                    decoder: Decoder::new(4096),
                                    encoder: Encoder::new(true, true),
                                    control_sent: false,
                                    decoder_stream: None,
                                    encoder_stream: None,
                                    encoder_type_sent: false,
                                    client_settings_seen: false,
                                    decoder_type_sent: false,
                                    blocked: HashMap::new(),
                                    encoder_fed: 0,
                                    encoder_buf: Vec::new(),
                                },
                            );
                        }
                    }
                    Some(DatagramEvent::ConnectionEvent(ch, ev)) => {
                        if let Some(c) = conns.get_mut(&ch) {
                            c.conn.handle_event(ev);
                        }
                    }
                    Some(DatagramEvent::Response(t)) => {
                        let _ = socket.send_to(&resp[..t.size], from);
                    }
                    None => {}
                }
            }
            Err(_) => {}
        }
        let now = Instant::now();
        let mut drained = Vec::new();
        for (ch, c) in conns.iter_mut() {
            if c.conn.poll_timeout().is_some_and(|t| t <= now) {
                c.conn.handle_timeout(now);
            }
            while let Some(ev) = c.conn.poll_endpoint_events() {
                if let Some(cev) = endpoint.handle_event(*ch, ev) {
                    c.conn.handle_event(cev);
                }
            }
            drive(c, &state);
            let mut out = Vec::new();
            while let Some(t) = c.conn.poll_transmit(now, 1, &mut out) {
                let _ = socket.send_to(&out[..t.size], t.destination);
                out.clear();
            }
            if c.conn.is_drained() {
                drained.push(*ch);
            }
        }
        for ch in drained {
            conns.remove(&ch);
        }
    }
}

fn write(conn: &mut quinn_proto::Connection, id: StreamId, data: &[u8], fin: bool) {
    let mut s = conn.send_stream(id);
    let _ = s.write(data);
    if fin {
        let _ = s.finish();
    }
}

fn frame(ty: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, ty);
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn drive(c: &mut ServerConn, state: &Arc<Mutex<State>>) {
    while let Some(ev) = c.conn.poll() {
        match ev {
            Event::Connected => {
                if !c.control_sent {
                    c.control_sent = true;
                    if let Some(id) = c.conn.streams().open(Dir::Uni) {
                        let mut settings = Vec::new();
                        for (k, v) in [(0x01u64, 4096u64), (0x07, 16)] {
                            put_varint(&mut settings, k);
                            put_varint(&mut settings, v);
                        }
                        let mut bytes = vec![0x00];
                        bytes.extend_from_slice(&frame(0x04, &settings));
                        write(&mut c.conn, id, &bytes, false);
                    }
                    c.decoder_stream = c.conn.streams().open(Dir::Uni);
                    c.encoder_stream = c.conn.streams().open(Dir::Uni);
                }
            }
            Event::Stream(StreamEvent::Opened { .. }) => {
                while let Some(id) = c.conn.streams().accept(Dir::Uni) {
                    read(c, id, state);
                }
                while let Some(id) = c.conn.streams().accept(Dir::Bi) {
                    read(c, id, state);
                }
            }
            Event::Stream(StreamEvent::Readable { id }) => read(c, id, state),
            _ => {}
        }
    }
}

fn read(c: &mut ServerConn, id: StreamId, state: &Arc<Mutex<State>>) {
    let mut data = Vec::new();
    let mut fin = false;
    {
        let mut rs = c.conn.recv_stream(id);
        let Ok(mut chunks) = rs.read(true) else { return };
        loop {
            match chunks.next(usize::MAX) {
                Ok(Some(ch)) => data.extend_from_slice(&ch.bytes),
                Ok(None) => {
                    fin = true;
                    break;
                }
                Err(_) => break,
            }
        }
        let _ = chunks.finalize();
    }
    let sid = u64::from(id);
    let all = {
        let Ok(mut s) = state.lock() else { return };
        let conn = &mut s.conns[c.index];
        let e = conn.streams.entry(sid).or_default();
        e.extend_from_slice(&data);
        if fin {
            conn.fin.insert(sid, true);
        }
        e.clone()
    };
    if id.dir() == Dir::Uni {
        // The client's QPACK encoder stream feeds this side's decoder.
        let mut at = 0;
        if get_varint(&all, &mut at) == Some(0x02) {
            let body = &all[at..];
            c.encoder_buf.extend_from_slice(&body[c.encoder_fed.min(body.len())..]);
            c.encoder_fed = body.len();
            let _ = c.decoder.on_encoder_stream(&mut c.encoder_buf);
            let pending: Vec<(StreamId, Vec<u8>)> = c.blocked.drain().collect();
            for (rid, block) in pending {
                decode_request(c, rid, block, state);
            }
        }
        // The client's control stream: its SETTINGS size this side's QPACK encoder.
        let mut at = 0;
        if !c.client_settings_seen && get_varint(&all, &mut at) == Some(0x00) {
            let (Some(ty), Some(len)) = (get_varint(&all, &mut at), get_varint(&all, &mut at)) else { return };
            if ty == 0x04 && all.len() >= at + len as usize {
                c.client_settings_seen = true;
                let p = &all[at..at + len as usize];
                let mut k = 0;
                while k < p.len() {
                    let (Some(id), Some(v)) = (get_varint(p, &mut k), get_varint(p, &mut k)) else { break };
                    match id {
                        0x01 => c.encoder.set_peer_max_table_capacity(v, 4096),
                        0x07 => c.encoder.set_peer_blocked_streams(v),
                        _ => {}
                    }
                }
            }
        }
        return;
    }
    // A request stream: its first HEADERS frame, once whole.
    let answered = state.lock().map(|s| s.conns[c.index].answered.contains(&sid)).unwrap_or(true);
    if answered || c.blocked.contains_key(&id) {
        return;
    }
    let mut at = 0;
    let (Some(ty), Some(len)) = (get_varint(&all, &mut at), get_varint(&all, &mut at)) else { return };
    if ty != 0x01 || all.len() < at + len as usize {
        return;
    }
    decode_request(c, id, all[at..at + len as usize].to_vec(), state);
}

fn decode_request(c: &mut ServerConn, id: StreamId, block: Vec<u8>, state: &Arc<Mutex<State>>) {
    let sid = u64::from(id);
    match c.decoder.decode(sid, &block) {
        Decoded::Blocked => {
            c.blocked.insert(id, block);
        }
        Decoded::Error => {}
        Decoded::Headers(fields) => {
            let ack = c.decoder.take_decoder_stream();
            if let (Some(d), false) = (c.decoder_stream, ack.is_empty()) {
                let mut bytes = Vec::new();
                if !c.decoder_type_sent {
                    bytes.push(0x03);
                    c.decoder_type_sent = true;
                }
                bytes.extend_from_slice(&ack);
                write(&mut c.conn, d, &bytes, false);
            }
            let list: Vec<(String, String)> = fields
                .into_iter()
                .map(|(n, v)| (String::from_utf8_lossy(&n).into_owned(), String::from_utf8_lossy(&v).into_owned()))
                .collect();
            if let Ok(mut s) = state.lock() {
                let conn = &mut s.conns[c.index];
                conn.requests.insert(sid, list);
                conn.answered.push(sid);
            }
            let head = c.encoder.encode(
                sid,
                &[
                    (b":status".to_vec(), b"200".to_vec()),
                    (b"content-type".to_vec(), b"text/plain".to_vec()),
                    (b"alt-svc".to_vec(), b"h3=\":443\"; ma=86400".to_vec()),
                    (b"cache-control".to_vec(), b"no-store".to_vec()),
                ],
            );
            let instructions = c.encoder.take_encoder_stream();
            if let (Some(e), false) = (c.encoder_stream, instructions.is_empty()) {
                let mut bytes = Vec::new();
                if !c.encoder_type_sent {
                    bytes.push(0x02);
                    c.encoder_type_sent = true;
                }
                bytes.extend_from_slice(&instructions);
                write(&mut c.conn, e, &bytes, false);
            }
            let mut bytes = frame(0x01, &head);
            bytes.extend_from_slice(&frame(0x00, b"ok"));
            write(&mut c.conn, id, &bytes, true);
        }
    }
}

// ===== rows =====

fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    get_varint(b, at)
}

struct InitialPacket {
    version: u32,
    dcid: Vec<u8>,
    scid_len: usize,
    token_len: u64,
    pn: u64,
    frames: Vec<(u8, u64, Vec<u8>)>,
}

/// Decrypt the client Initial packets of one datagram (RFC 9001 5.2, 5.4).
fn initial_packets(d: &[u8], odcid: &mut Option<Vec<u8>>) -> Vec<InitialPacket> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < d.len() && d[i] & 0x80 != 0 {
        let first = d[i];
        let Some(vb) = d.get(i + 1..i + 5) else { break };
        let version = u32::from_be_bytes([vb[0], vb[1], vb[2], vb[3]]);
        let mut j = i + 5;
        let Some(&dl) = d.get(j) else { break };
        let dcid = d.get(j + 1..j + 1 + dl as usize).unwrap_or_default().to_vec();
        j += 1 + dl as usize;
        let Some(&sl) = d.get(j) else { break };
        j += 1 + sl as usize;
        let ty = (first >> 4) & 3;
        let mut token_len = 0;
        if ty == 0 {
            let Some(t) = varint(d, &mut j) else { break };
            token_len = t;
            j += t as usize;
        }
        let Some(len) = varint(d, &mut j) else { break };
        let end = (j + len as usize).min(d.len());
        if ty == 0 {
            let key = odcid.get_or_insert_with(|| dcid.clone()).clone();
            let (hk, pk) = crypto::client_initial_open(&key);
            let mut pkt = d[i..end].to_vec();
            let pn_offset = j - i;
            if pkt.len() >= pn_offset + 20 {
                hk.decrypt(pn_offset, &mut pkt);
                let pn_len = usize::from(pkt[0] & 3) + 1;
                let mut pn = 0u64;
                for b in &pkt[pn_offset..pn_offset + pn_len] {
                    pn = (pn << 8) | u64::from(*b);
                }
                let header = pkt[..pn_offset + pn_len].to_vec();
                let mut payload = BytesMut::from(&pkt[pn_offset + pn_len..]);
                if pk.decrypt(pn, &header, &mut payload).is_ok() {
                    out.push(InitialPacket {
                        version,
                        dcid: dcid.clone(),
                        scid_len: sl as usize,
                        token_len,
                        pn,
                        frames: frames(&payload),
                    });
                }
            }
        }
        i = end;
    }
    out
}

/// (type, offset, data) per frame: 0x06 CRYPTO with its data, 0x01 PING, 0x00 a PADDING run, 0x02 ACK.
fn frames(p: &[u8]) -> Vec<(u8, u64, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            0x00 => {
                let s = i;
                while i < p.len() && p[i] == 0 {
                    i += 1;
                }
                out.push((0x00, (i - s) as u64, Vec::new()));
            }
            0x01 => {
                out.push((0x01, 0, Vec::new()));
                i += 1;
            }
            0x06 => {
                i += 1;
                let (Some(off), Some(len)) = (varint(p, &mut i), varint(p, &mut i)) else { break };
                let data = p.get(i..i + len as usize).unwrap_or_default().to_vec();
                i += len as usize;
                out.push((0x06, off, data));
            }
            0x02 | 0x03 => {
                let ty = p[i];
                i += 1;
                let _ = varint(p, &mut i);
                let _ = varint(p, &mut i);
                let Some(n) = varint(p, &mut i) else { break };
                let _ = varint(p, &mut i);
                for _ in 0..n {
                    let _ = varint(p, &mut i);
                    let _ = varint(p, &mut i);
                }
                if ty == 0x03 {
                    for _ in 0..3 {
                        let _ = varint(p, &mut i);
                    }
                }
                out.push((0x02, 0, Vec::new()));
            }
            _ => break,
        }
    }
    out
}

fn is_grease16(v: u16) -> bool {
    v & 0x0f0f == 0x0a0a && v >> 8 == v & 0xff
}

fn hex16(v: u16) -> String {
    if is_grease16(v) { "GREASE".into() } else { format!("{v:04x}") }
}

fn list16(b: &[u8]) -> String {
    b.chunks(2).filter(|c| c.len() == 2).map(|c| hex16(u16::from_be_bytes([c[0], c[1]]))).collect::<Vec<_>>().join(",")
}

fn names(b: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let n = b[i] as usize;
        out.push(String::from_utf8_lossy(b.get(i + 1..i + 1 + n).unwrap_or_default()).into_owned());
        i += 1 + n;
    }
    out.join(",")
}

fn initial_rows(datagrams: &[(u16, Vec<u8>)]) -> Vec<String> {
    let mut rows = Vec::new();
    // The first client connection (source port) only.
    let Some(port) = datagrams.first().map(|(p, _)| *p) else { return rows };
    let mut odcid = None;
    let mut first: Option<InitialPacket> = None;
    let mut crypto: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut sizes = Vec::new();
    let mut packets_with_crypto = 0;
    for (_, d) in datagrams.iter().filter(|(p, _)| *p == port) {
        let pkts = initial_packets(d, &mut odcid);
        if pkts.is_empty() {
            continue;
        }
        sizes.push(d.len());
        for pk in pkts {
            let has_crypto = pk.frames.iter().any(|f| f.0 == 0x06);
            if has_crypto && pk.frames.iter().all(|f| f.0 != 0x02) {
                packets_with_crypto += 1;
            }
            for (ty, off, data) in &pk.frames {
                if *ty == 0x06 {
                    crypto.insert(*off, data.clone());
                }
            }
            if first.is_none() {
                first = Some(pk);
            }
        }
    }
    let Some(f) = first else { return rows };
    rows.push(format!("initial.version={:08x}", f.version));
    rows.push(format!("initial.dcid_len={}", f.dcid.len()));
    rows.push(format!("initial.scid_len={}", f.scid_len));
    rows.push(format!("initial.token_len={}", f.token_len));
    rows.push(format!("initial.first_pn={}", f.pn));
    let distinct: std::collections::BTreeSet<usize> = sizes.iter().copied().collect();
    rows.push(format!("initial.datagram_sizes={}", distinct.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",")));
    // The ClientHello.
    let mut ch = Vec::new();
    let mut off = 0u64;
    while let Some(d) = crypto.get(&off) {
        ch.extend_from_slice(d);
        off += d.len() as u64;
    }
    // Chaos protection: the first packet carries the ClientHello's head and its tail, PINGs, and several CRYPTO frames.
    let crypto_frames = f.frames.iter().filter(|x| x.0 == 0x06).count();
    let has_head = f.frames.iter().any(|x| x.0 == 0x06 && x.1 == 0);
    let has_tail = f.frames.iter().any(|x| x.0 == 0x06 && x.1 + x.2.len() as u64 == ch.len() as u64);
    rows.push(format!("initial.first_packet.crypto_frames_ge_3={}", crypto_frames >= 3));
    rows.push(format!("initial.first_packet.has_ping={}", f.frames.iter().any(|x| x.0 == 0x01)));
    rows.push(format!("initial.first_packet.head_and_tail={}", has_head && has_tail));
    rows.push(format!("initial.client_hello_packets={packets_with_crypto}"));
    rows.extend(client_hello_rows(&ch));
    rows
}

fn client_hello_rows(ch: &[u8]) -> Vec<String> {
    let mut rows = Vec::new();
    if ch.len() < 4 || ch[0] != 1 {
        rows.push("ch=missing".into());
        return rows;
    }
    let mut i = 4;
    rows.push(format!("ch.legacy_version={:02x}{:02x}", ch[i], ch[i + 1]));
    i += 2 + 32;
    let sid = ch[i] as usize;
    rows.push(format!("ch.session_id_len={sid}"));
    i += 1 + sid;
    let cl = u16::from_be_bytes([ch[i], ch[i + 1]]) as usize;
    rows.push(format!("ch.ciphers={}", list16(&ch[i + 2..i + 2 + cl])));
    i += 2 + cl;
    let ml = ch[i] as usize;
    i += 1 + ml;
    let el = u16::from_be_bytes([ch[i], ch[i + 1]]) as usize;
    i += 2;
    let end = (i + el).min(ch.len());
    let mut types = Vec::new();
    while i + 4 <= end {
        let t = u16::from_be_bytes([ch[i], ch[i + 1]]);
        let l = u16::from_be_bytes([ch[i + 2], ch[i + 3]]) as usize;
        let d = &ch[i + 4..(i + 4 + l).min(end)];
        i += 4 + l;
        types.push(hex16(t));
        match t {
            0x0000 => rows.push(format!("ch.ext.server_name={}", String::from_utf8_lossy(d.get(5..).unwrap_or_default()))),
            0x000a => rows.push(format!("ch.ext.supported_groups={}", list16(d.get(2..).unwrap_or_default()))),
            0x000d => rows.push(format!("ch.ext.signature_algorithms={}", list16(d.get(2..).unwrap_or_default()))),
            0x0010 => rows.push(format!("ch.ext.alpn={}", names(d.get(2..).unwrap_or_default()))),
            0x0033 => {
                let mut k = 2;
                let mut shares = Vec::new();
                while k + 4 <= d.len() {
                    let g = u16::from_be_bytes([d[k], d[k + 1]]);
                    let n = u16::from_be_bytes([d[k + 2], d[k + 3]]) as usize;
                    shares.push(format!("{}:{n}", hex16(g)));
                    k += 4 + n;
                }
                rows.push(format!("ch.ext.key_share={}", shares.join(",")));
            }
            0x002b => rows.push(format!("ch.ext.supported_versions={}", list16(d.get(1..).unwrap_or_default()))),
            0x002d => rows.push(format!("ch.ext.psk_key_exchange_modes={}", d.get(1..).map(hex).unwrap_or_default())),
            0x001b => rows.push(format!("ch.ext.compress_certificate={}", list16(d.get(1..).unwrap_or_default()))),
            0x44cd | 0x4469 => rows.push(format!("ch.ext.application_settings={}", names(d.get(2..).unwrap_or_default()))),
            0xfe0d => rows.push(format!("ch.ext.encrypted_client_hello.outer={}", d.first().copied().unwrap_or(9))),
            0x0039 => rows.extend(tp_rows(d)),
            _ => {}
        }
    }
    let mut set = types.clone();
    set.sort();
    rows.push(format!("ch.extension_set={}", set.join(",")));
    rows.push(format!("ch.grease={}", types.iter().any(|t| t == "GREASE")));
    rows
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn tp_rows(d: &[u8]) -> Vec<String> {
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let (Some(id), Some(len)) = (varint(d, &mut i), varint(d, &mut i)) else { break };
        let v = d.get(i..i + len as usize).unwrap_or_default();
        i += len as usize;
        if id % 31 == 27 && id > 0x3f {
            ids.push("GREASE".to_string());
            rows.push(format!("tp.grease_len_le_15={}", len <= 15));
            continue;
        }
        ids.push(format!("{id:#x}"));
        let value = match id {
            0x01 | 0x03..=0x0b | 0x0e | 0x20 | 0x3127 => {
                let mut k = 0;
                varint(v, &mut k).map(|x| x.to_string()).unwrap_or_default()
            }
            0x0f => format!("len {}", v.len()),
            0x11 => {
                let words: Vec<String> = v
                    .chunks(4)
                    .map(|c| {
                        let w = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                        if w & 0x0f0f_0f0f == 0x0a0a_0a0a { "GREASE".into() } else { format!("{w:08x}") }
                    })
                    .collect();
                // The GREASE version's position is random: report the multiset.
                let (chosen, rest) = words.split_first().map(|(a, b)| (a.clone(), b.to_vec())).unwrap_or_default();
                let mut rest = rest;
                rest.sort();
                format!("{chosen}|{}", rest.join(","))
            }
            0x3128 => String::from_utf8_lossy(v).into_owned(),
            _ => hex(v),
        };
        rows.push(format!("tp.{id:#x}={value}"));
    }
    ids.sort();
    rows.push(format!("tp.ids={}", ids.join(",")));
    rows
}

fn h3_rows(c: &Conn) -> Vec<String> {
    let mut rows = Vec::new();
    let mut updates: Vec<(u64, String)> = Vec::new();
    let uni: Vec<(u64, u64)> = c
        .streams
        .iter()
        .filter(|(id, b)| *id % 4 == 2 && !b.is_empty())
        .filter_map(|(id, b)| {
            let mut at = 0;
            get_varint(b, &mut at).map(|t| (*id, t))
        })
        .collect();
    rows.push(format!("h3.uni_streams={}", uni.iter().map(|(id, t)| format!("{id}:{t}")).collect::<Vec<_>>().join(",")));
    if let Some((id, _)) = uni.iter().find(|(_, t)| *t == 0) {
        let b = &c.streams[id];
        let mut at = 0;
        let _ = get_varint(b, &mut at);
        let mut kinds = Vec::new();
        while at < b.len() {
            let (Some(ty), Some(len)) = (get_varint(b, &mut at), get_varint(b, &mut at)) else { break };
            let p = b.get(at..at + len as usize).unwrap_or_default();
            at += len as usize;
            if ty == 0x04 {
                let mut k = 0;
                let mut s = Vec::new();
                while k < p.len() {
                    let (Some(id), Some(v)) = (get_varint(p, &mut k), get_varint(p, &mut k)) else { break };
                    if id >= 0x21 && (id - 0x21) % 0x1f == 0 {
                        s.push("GREASE".to_string());
                    } else {
                        s.push(format!("{id:#x}={v}"));
                    }
                }
                rows.push(format!("h3.settings={}", s.join(",")));
                kinds.push("SETTINGS".to_string());
            } else if ty == 0xf0700 {
                let mut k = 0;
                let elem = get_varint(p, &mut k).unwrap_or(0);
                updates.push((elem, String::from_utf8_lossy(&p[k..]).into_owned()));
                kinds.push("PRIORITY_UPDATE".to_string());
            } else if ty >= 0x21 && (ty - 0x21) % 0x1f == 0 {
                rows.push(format!("h3.grease_frame_len_le_3={}", len <= 3));
                kinds.push("GREASE".to_string());
            } else {
                kinds.push(format!("{ty:#x}"));
            }
        }
        // The order: SETTINGS, the GREASE frame, then the PRIORITY_UPDATEs.
        kinds.dedup();
        rows.push(format!("h3.control_frames={}", kinds.join(",")));
    }
    if let Some((id, _)) = uni.iter().find(|(_, t)| *t == 2) {
        let b = &c.streams[id];
        let mut at = 0;
        let _ = get_varint(b, &mut at);
        // The first instruction: Set Dynamic Table Capacity (001xxxxx).
        if let Some(&x) = b.get(at) {
            if x & 0xe0 == 0x20 {
                let mut cap = u64::from(x & 0x1f);
                if cap == 0x1f {
                    let mut shift = 0;
                    let mut k = at + 1;
                    while let Some(&y) = b.get(k) {
                        cap += u64::from(y & 0x7f) << shift;
                        shift += 7;
                        k += 1;
                        if y & 0x80 == 0 {
                            break;
                        }
                    }
                }
                rows.push(format!("h3.qpack_encoder.first=set_capacity:{cap}"));
            } else {
                rows.push(format!("h3.qpack_encoder.first={x:#04x}"));
            }
        }
    }
    for (i, fields) in c.requests.values().enumerate().take(1) {
        let pseudo: Vec<&str> = fields.iter().filter(|(n, _)| n.starts_with(':')).map(|(n, _)| n.as_str()).collect();
        rows.push(format!("h3.request{i}.pseudo={}", pseudo.join(",")));
        let names: Vec<&str> = fields.iter().filter(|(n, _)| !n.starts_with(':')).map(|(n, _)| n.as_str()).collect();
        rows.push(format!("h3.request{i}.names={}", names.join(",")));
        if let Some((_, p)) = fields.iter().find(|(n, _)| n == "priority") {
            rows.push(format!("h3.request{i}.priority={p}"));
        }
    }
    rows.push(format!("h3.first_request_stream={}", c.requests.keys().next().copied().unwrap_or(u64::MAX)));
    if let Some((elem, v)) = updates.first() {
        rows.push(format!("h3.priority_update.first={elem}:{v}"));
    }
    // Every request whose `priority` is not the default had a PRIORITY_UPDATE carrying the same value, and no other.
    let mut expected: Vec<(u64, String)> = c
        .requests
        .iter()
        .filter_map(|(sid, f)| f.iter().find(|(n, _)| n == "priority").map(|(_, v)| (*sid, v.clone())))
        .filter(|(_, v)| v != "u=3")
        .collect();
    expected.sort();
    let mut seen = updates.clone();
    seen.sort();
    rows.push(format!("h3.priority_updates_match_headers={}", expected == seen));
    rows
}

// ===== pieces, for unit tests that need no network =====

/// The Alt-Svc parser's reading of `value`: (protocol, host, port, max-age) per entry; `None` if Chrome rejects it.
pub fn alt_svc(value: &str) -> Option<Vec<(String, String, u16, u32)>> {
    super::altsvc::parse(value).map(|v| v.into_iter().map(|e| (e.protocol, e.host, e.port, e.max_age)).collect())
}

/// Encode `fields` on stream 0 with a peer table of `peer_capacity` (blocked streams 100), feed the encoder stream
/// to a decoder, decode: the decoded list, the encoder-stream bytes, and the field section.
pub fn qpack_roundtrip(fields: &[(&str, &str)], peer_capacity: u64) -> (Vec<(String, String)>, Vec<u8>, Vec<u8>) {
    let mut enc = Encoder::new(true, true);
    if peer_capacity > 0 {
        enc.set_peer_max_table_capacity(peer_capacity, 65_536);
        enc.set_peer_blocked_streams(100);
    }
    let list: Vec<(Vec<u8>, Vec<u8>)> = fields.iter().map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
    let block = enc.encode(0, &list);
    let stream = enc.take_encoder_stream();
    let mut dec = Decoder::new(peer_capacity);
    let mut buf = stream.clone();
    let _ = dec.on_encoder_stream(&mut buf);
    let decoded = match dec.decode(0, &block) {
        Decoded::Headers(h) => h
            .into_iter()
            .map(|(n, v)| (String::from_utf8_lossy(&n).into_owned(), String::from_utf8_lossy(&v).into_owned()))
            .collect(),
        _ => Vec::new(),
    };
    (decoded, stream, block)
}

/// The transport-parameter rows (`tp.*`, as `rows` reads them) of one encoding with `opts`.
pub fn transport_parameter_rows(opts: &super::Http3Options) -> Vec<String> {
    tp_rows(&super::tp::encode(opts, &[], None))
}
